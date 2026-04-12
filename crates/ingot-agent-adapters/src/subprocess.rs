//! Shared CLI subprocess lifecycle: spawn, stdin pipe, stream collection,
//! wait, and process-group cancellation.

use std::future::Future;
use std::path::{Path, PathBuf};

use ingot_agent_protocol::adapter::AgentError;
use ingot_agent_protocol::request::AgentRequest;
use ingot_agent_protocol::response::{
    AgentOutputChunk, AgentOutputSegmentDraft, AgentResponse, OutputStream,
};
use ingot_domain::agent_model::AgentModel;
use tokio::fs;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::Command;
use tokio::sync::mpsc;
use tracing::{debug, info, warn};

use crate::output_schema;

#[derive(Debug, Clone)]
pub(crate) struct AdapterCommandConfig {
    cli_path: PathBuf,
    model: AgentModel,
}

impl AdapterCommandConfig {
    pub(crate) fn new(cli_path: impl Into<PathBuf>, model: impl Into<AgentModel>) -> Self {
        Self {
            cli_path: cli_path.into(),
            model: model.into(),
        }
    }

    pub(crate) fn cli_path(&self) -> &Path {
        &self.cli_path
    }

    pub(crate) fn model(&self) -> &AgentModel {
        &self.model
    }
}

#[derive(Debug, Clone)]
pub(crate) struct TempFile {
    path: PathBuf,
}

impl TempFile {
    pub(crate) fn new(working_dir: &Path, stem: &str, extension: &str) -> Self {
        Self::from_path(working_dir.join(format!("{stem}-{}.{}", uuid::Uuid::now_v7(), extension)))
    }

    pub(crate) fn from_path(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    pub(crate) fn path(&self) -> &Path {
        &self.path
    }

    pub(crate) fn cli_arg(&self, label: &str) -> Result<String, AgentError> {
        self.path
            .to_str()
            .map(ToOwned::to_owned)
            .ok_or_else(|| AgentError::LaunchFailed(format!("invalid {label} path")))
    }

    pub(crate) async fn write_json(&self, value: &serde_json::Value) -> Result<(), AgentError> {
        fs::write(
            &self.path,
            serde_json::to_vec_pretty(value)
                .map_err(|err| AgentError::LaunchFailed(err.to_string()))?,
        )
        .await
        .map_err(|err| AgentError::LaunchFailed(err.to_string()))
    }

    pub(crate) async fn read_to_string(&self) -> Result<String, AgentError> {
        fs::read_to_string(&self.path)
            .await
            .map_err(|err| AgentError::ProcessError(err.to_string()))
    }

    pub(crate) async fn cleanup(&self) {
        let _ = fs::remove_file(&self.path).await;
    }
}

pub(crate) async fn output_schema_file(
    request: &AgentRequest,
    working_dir: &Path,
    adapter_name: &str,
) -> Result<TempFile, AgentError> {
    let file = TempFile::new(
        working_dir,
        &format!(".ingot-{adapter_name}-schema"),
        "json",
    );
    file.write_json(&output_schema(request)).await?;
    Ok(file)
}

pub(crate) fn inline_output_schema(request: &AgentRequest) -> Result<String, AgentError> {
    serde_json::to_string(&output_schema(request))
        .map_err(|err| AgentError::LaunchFailed(err.to_string()))
}

pub(crate) struct SubprocessOutput {
    pub exit_code: i32,
    pub stdout: String,
    pub stderr: String,
}

pub(crate) struct SchemaResultFiles<'a> {
    pub adapter_name: &'static str,
    pub result_file_stem: &'a str,
    pub result_file_extension: &'a str,
}

pub(crate) struct AdapterLaunch<'a, Model: ?Sized> {
    pub cli_path: &'a Path,
    pub model: &'a Model,
    pub request: &'a AgentRequest,
    pub working_dir: &'a Path,
    pub args: Vec<String>,
    pub adapter_name: &'static str,
    pub output_tx: Option<mpsc::Sender<AgentOutputChunk>>,
    pub stdout_parser: Option<Box<dyn StdoutOutputParser>>,
}

pub(crate) trait StdoutOutputParser: Send + 'static {
    fn parse_line(&mut self, line: &str) -> Vec<AgentOutputSegmentDraft>;
}

/// Spawn a CLI subprocess, pipe the prompt to stdin, collect stdout/stderr,
/// and wait for exit. The child is spawned in its own process group so
/// `cancel_process_group` can tear down the entire tree.
pub(crate) async fn run_cli_subprocess(
    cli_path: &Path,
    args: &[String],
    working_dir: &Path,
    prompt: &str,
    adapter_name: &'static str,
    output_tx: Option<mpsc::Sender<AgentOutputChunk>>,
    stdout_parser: Option<Box<dyn StdoutOutputParser>>,
) -> Result<SubprocessOutput, AgentError> {
    let mut command = Command::new(cli_path);
    command
        .args(args)
        .current_dir(working_dir)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);
    #[cfg(unix)]
    command.process_group(0);
    let mut child = command
        .spawn()
        .map_err(|err| AgentError::LaunchFailed(err.to_string()))?;
    debug!(adapter = adapter_name, "spawned subprocess");

    if let Some(mut stdin) = child.stdin.take() {
        stdin
            .write_all(prompt.as_bytes())
            .await
            .map_err(|err| AgentError::ProcessError(err.to_string()))?;
        stdin
            .shutdown()
            .await
            .map_err(|err| AgentError::ProcessError(err.to_string()))?;
    }
    debug!(
        adapter = adapter_name,
        "wrote prompt to stdin and closed input"
    );

    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| AgentError::ProcessError(format!("missing {adapter_name} stdout pipe")))?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| AgentError::ProcessError(format!("missing {adapter_name} stderr pipe")))?;
    let stdout_task = tokio::spawn(collect_stream(
        stdout,
        "stdout",
        adapter_name,
        output_tx.clone(),
        stdout_parser,
    ));
    let stderr_task = tokio::spawn(collect_stream(
        stderr,
        "stderr",
        adapter_name,
        output_tx,
        None,
    ));

    let status = child
        .wait()
        .await
        .map_err(|err| AgentError::ProcessError(err.to_string()))?;

    let stdout = stdout_task
        .await
        .map_err(|err| AgentError::ProcessError(err.to_string()))??;
    let stderr = stderr_task
        .await
        .map_err(|err| AgentError::ProcessError(err.to_string()))??;

    Ok(SubprocessOutput {
        exit_code: status.code().unwrap_or(-1),
        stdout,
        stderr,
    })
}

pub(crate) async fn launch_adapter<Model, Parse, ParseFuture>(
    launch: AdapterLaunch<'_, Model>,
    parse_result: Parse,
) -> Result<AgentResponse, AgentError>
where
    Model: std::fmt::Display + ?Sized,
    Parse: FnOnce(&SubprocessOutput) -> ParseFuture,
    ParseFuture: Future<Output = Result<Option<serde_json::Value>, AgentError>>,
{
    let AdapterLaunch {
        cli_path,
        model,
        request,
        working_dir,
        args,
        adapter_name,
        output_tx,
        stdout_parser,
    } = launch;

    info!(
        adapter = adapter_name,
        cli_path = %cli_path.display(),
        model = %model,
        working_dir = %working_dir.display(),
        may_mutate = request.may_mutate,
        args = ?args,
        "launching adapter subprocess"
    );

    let output = run_cli_subprocess(
        cli_path,
        &args,
        working_dir,
        &request.prompt,
        adapter_name,
        output_tx,
        stdout_parser,
    )
    .await?;
    info!(
        adapter = adapter_name,
        exit_code = output.exit_code,
        "adapter subprocess finished"
    );

    let result = parse_result(&output).await?;

    Ok(AgentResponse {
        exit_code: output.exit_code,
        stdout: output.stdout,
        stderr: output.stderr,
        result,
    })
}

pub(crate) async fn launch_adapter_with_schema_and_result_files<BuildArgs, Parse, ParseFuture>(
    command: &AdapterCommandConfig,
    request: &AgentRequest,
    working_dir: &Path,
    files: SchemaResultFiles<'_>,
    build_args: BuildArgs,
    output_tx: Option<mpsc::Sender<AgentOutputChunk>>,
    stdout_parser: Option<Box<dyn StdoutOutputParser>>,
    parse_result: Parse,
) -> Result<AgentResponse, AgentError>
where
    BuildArgs: FnOnce(&TempFile, &TempFile) -> Result<Vec<String>, AgentError>,
    Parse: FnOnce(&SubprocessOutput, TempFile) -> ParseFuture,
    ParseFuture: Future<Output = Result<Option<serde_json::Value>, AgentError>>,
{
    let result_file = TempFile::new(
        working_dir,
        files.result_file_stem,
        files.result_file_extension,
    );
    let schema_file = output_schema_file(request, working_dir, files.adapter_name).await?;
    let parse_file = result_file.clone();

    let launch_result = launch_adapter(
        AdapterLaunch {
            cli_path: command.cli_path(),
            model: command.model(),
            request,
            working_dir,
            args: build_args(&schema_file, &result_file)?,
            adapter_name: files.adapter_name,
            output_tx,
            stdout_parser,
        },
        move |output| parse_result(output, parse_file),
    )
    .await;

    result_file.cleanup().await;
    schema_file.cleanup().await;
    launch_result
}

async fn collect_stream(
    reader: impl tokio::io::AsyncRead + Unpin,
    stream_name: &'static str,
    adapter_name: &'static str,
    mut output_tx: Option<mpsc::Sender<AgentOutputChunk>>,
    mut stdout_parser: Option<Box<dyn StdoutOutputParser>>,
) -> Result<String, AgentError> {
    let mut reader = BufReader::new(reader).lines();
    let mut lines = Vec::new();

    while let Some(line) = reader
        .next_line()
        .await
        .map_err(|err| AgentError::ProcessError(err.to_string()))?
    {
        match stream_name {
            "stderr" => warn!(adapter = adapter_name, stream = stream_name, line, "stderr"),
            _ => debug!(adapter = adapter_name, stream = stream_name, line, "event"),
        }
        if let Some(tx) = output_tx.as_mut() {
            let stream = match stream_name {
                "stderr" => OutputStream::Stderr,
                _ => OutputStream::Stdout,
            };
            let chunk = format!("{line}\n");
            let segments = match stream {
                OutputStream::Stdout => stdout_parser
                    .as_mut()
                    .map(|parser| parser.parse_line(&line))
                    .unwrap_or_default(),
                OutputStream::Stderr => {
                    vec![AgentOutputSegmentDraft::diagnostic_text(line.clone())]
                }
            };
            if tx
                .send(AgentOutputChunk {
                    stream,
                    chunk,
                    segments,
                })
                .await
                .is_err()
            {
                output_tx = None;
            }
        }
        lines.push(line);
    }

    Ok(lines.join("\n"))
}

/// Cancel a subprocess by sending SIGKILL to its process group (Unix only).
pub(crate) async fn cancel_process_group(pid: u32) -> Result<(), AgentError> {
    #[cfg(unix)]
    {
        let result = unsafe { libc::killpg(pid as i32, libc::SIGKILL) };
        if result == 0 {
            return Ok(());
        }
        let error = std::io::Error::last_os_error();
        if error.raw_os_error() == Some(libc::ESRCH) {
            return Ok(());
        }
        Err(AgentError::ProcessError(format!(
            "killpg({pid}) failed: {error}"
        )))
    }
    #[cfg(not(unix))]
    {
        let _ = pid;
        Err(AgentError::ProcessError(
            "subprocess cancellation is only supported on unix".into(),
        ))
    }
}
