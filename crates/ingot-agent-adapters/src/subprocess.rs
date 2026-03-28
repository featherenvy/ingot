//! Shared CLI subprocess lifecycle: spawn, stdin pipe, stream collection,
//! wait, and process-group cancellation.

use std::path::Path;

use ingot_agent_protocol::adapter::AgentError;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::Command;
use tracing::{debug, warn};

pub(crate) struct SubprocessOutput {
    pub exit_code: i32,
    pub stdout: String,
    pub stderr: String,
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
    let stdout_task = tokio::spawn(collect_stream(stdout, "stdout", adapter_name));
    let stderr_task = tokio::spawn(collect_stream(stderr, "stderr", adapter_name));

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

async fn collect_stream(
    reader: impl tokio::io::AsyncRead + Unpin,
    stream_name: &'static str,
    adapter_name: &'static str,
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
