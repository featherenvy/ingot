use std::path::{Path, PathBuf};

use ingot_agent_protocol::adapter::{AgentAdapter, AgentError};
use ingot_agent_protocol::request::AgentRequest;
use ingot_agent_protocol::response::AgentResponse;
use ingot_domain::agent_model::AgentModel;
use tokio::fs;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::Command;
use tracing::{debug, info, warn};

#[derive(Debug, Clone)]
pub struct CodexCliAdapter {
    cli_path: PathBuf,
    model: AgentModel,
}

impl CodexCliAdapter {
    pub fn new(cli_path: impl Into<PathBuf>, model: impl Into<AgentModel>) -> Self {
        Self {
            cli_path: cli_path.into(),
            model: model.into(),
        }
    }

    fn response_path(&self, working_dir: &Path) -> PathBuf {
        working_dir.join(format!(
            ".ingot-codex-last-message-{}.txt",
            uuid::Uuid::now_v7()
        ))
    }

    fn schema_path(&self, working_dir: &Path) -> PathBuf {
        working_dir.join(format!(".ingot-codex-schema-{}.json", uuid::Uuid::now_v7()))
    }

    fn build_exec_args(
        &self,
        request: &AgentRequest,
        schema_path: &Path,
        response_path: &Path,
    ) -> Result<Vec<String>, AgentError> {
        let sandbox = if request.may_mutate {
            "danger-full-access"
        } else {
            "read-only"
        };

        Ok(vec![
            "exec".into(),
            "--sandbox".into(),
            sandbox.into(),
            "-C".into(),
            request.working_dir.to_string_lossy().into_owned(),
            "-m".into(),
            self.model.to_string(),
            "--json".into(),
            "--output-schema".into(),
            schema_path
                .to_str()
                .ok_or_else(|| AgentError::LaunchFailed("invalid schema path".into()))?
                .into(),
            "--output-last-message".into(),
            response_path
                .to_str()
                .ok_or_else(|| AgentError::LaunchFailed("invalid response path".into()))?
                .into(),
            "-".into(),
        ])
    }

    async fn collect_stream(
        reader: impl tokio::io::AsyncRead + Unpin,
        stream_name: &'static str,
    ) -> Result<String, AgentError> {
        let mut reader = BufReader::new(reader).lines();
        let mut lines = Vec::new();

        while let Some(line) = reader
            .next_line()
            .await
            .map_err(|err| AgentError::ProcessError(err.to_string()))?
        {
            match stream_name {
                "stdout" => info!(stream = stream_name, line, "codex event"),
                "stderr" => warn!(stream = stream_name, line, "codex stderr"),
                _ => debug!(stream = stream_name, line, "codex stream"),
            }
            lines.push(line);
        }

        Ok(lines.join("\n"))
    }
}

impl AgentAdapter for CodexCliAdapter {
    async fn launch(
        &self,
        request: &AgentRequest,
        working_dir: &Path,
    ) -> Result<AgentResponse, AgentError> {
        let response_path = self.response_path(working_dir);
        let schema_path = self.schema_path(working_dir);
        let _ = fs::remove_file(&response_path).await;
        let _ = fs::remove_file(&schema_path).await;

        let schema = request
            .output_schema
            .clone()
            .unwrap_or_else(structured_output_schema);
        fs::write(
            &schema_path,
            serde_json::to_vec_pretty(&schema)
                .map_err(|err| AgentError::LaunchFailed(err.to_string()))?,
        )
        .await
        .map_err(|err| AgentError::LaunchFailed(err.to_string()))?;

        let args = self.build_exec_args(request, &schema_path, &response_path)?;
        info!(
            cli_path = %self.cli_path.display(),
            model = %self.model,
            working_dir = %request.working_dir.display(),
            may_mutate = request.may_mutate,
            args = ?args,
            "launching codex exec"
        );

        let mut command = Command::new(&self.cli_path);
        command
            .args(&args)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true);
        #[cfg(unix)]
        command.process_group(0);
        let mut child = command
            .spawn()
            .map_err(|err| AgentError::LaunchFailed(err.to_string()))?;
        debug!("spawned codex subprocess");

        if let Some(mut stdin) = child.stdin.take() {
            stdin
                .write_all(request.prompt.as_bytes())
                .await
                .map_err(|err| AgentError::ProcessError(err.to_string()))?;
            stdin
                .shutdown()
                .await
                .map_err(|err| AgentError::ProcessError(err.to_string()))?;
        }
        debug!("wrote prompt to codex stdin and closed input");

        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| AgentError::ProcessError("missing codex stdout pipe".into()))?;
        let stderr = child
            .stderr
            .take()
            .ok_or_else(|| AgentError::ProcessError("missing codex stderr pipe".into()))?;
        let stdout_task = tokio::spawn(Self::collect_stream(stdout, "stdout"));
        let stderr_task = tokio::spawn(Self::collect_stream(stderr, "stderr"));

        let status = child
            .wait()
            .await
            .map_err(|err| AgentError::ProcessError(err.to_string()))?;
        info!(
            exit_code = status.code().unwrap_or(-1),
            "codex exec finished"
        );

        let stdout = stdout_task
            .await
            .map_err(|err| AgentError::ProcessError(err.to_string()))??;
        let stderr = stderr_task
            .await
            .map_err(|err| AgentError::ProcessError(err.to_string()))??;

        let final_message = fs::read_to_string(&response_path).await.ok();
        if final_message
            .as_deref()
            .unwrap_or_default()
            .trim()
            .is_empty()
        {
            warn!(response_path = %response_path.display(), "codex did not produce a last-message payload");
        }
        let _ = fs::remove_file(&response_path).await;
        let _ = fs::remove_file(&schema_path).await;

        let result = final_message.as_deref().map(parse_last_message);

        Ok(AgentResponse {
            exit_code: status.code().unwrap_or(-1),
            stdout,
            stderr,
            result,
        })
    }

    async fn cancel(&self, pid: u32) -> Result<(), AgentError> {
        #[cfg(unix)]
        {
            // The child was spawned with process_group(0), so its pid is also
            // its process-group id. killpg sends the signal to the entire tree.
            let result = unsafe { libc::killpg(pid as i32, libc::SIGKILL) };
            if result == 0 {
                return Ok(());
            }
            let error = std::io::Error::last_os_error();
            if error.raw_os_error() == Some(libc::ESRCH) {
                // Already gone — not an error.
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
                "codex subprocess cancellation is only supported on unix".into(),
            ))
        }
    }
}

fn structured_output_schema() -> serde_json::Value {
    serde_json::json!({
        "type": "object",
        "properties": {
            "summary": {
                "type": "string",
                "description": "Short summary of the completed work."
            },
            "validation": {
                "type": ["string", "null"],
                "description": "Short note describing validation that was run, if any."
            }
        },
        "required": ["summary", "validation"],
        "additionalProperties": false
    })
}

fn parse_last_message(message: &str) -> serde_json::Value {
    serde_json::from_str(message)
        .unwrap_or_else(|_| serde_json::json!({ "summary": message.trim() }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(may_mutate: bool) -> AgentRequest {
        AgentRequest {
            prompt: "Implement the change".into(),
            working_dir: "/tmp/repo".into(),
            may_mutate,
            timeout_seconds: Some(60),
            output_schema: None,
        }
    }

    #[test]
    fn build_exec_args_match_current_codex_exec_flags() {
        let adapter = CodexCliAdapter::new("codex", "gpt-5");
        let args = adapter
            .build_exec_args(
                &request(true),
                Path::new("/tmp/schema.json"),
                Path::new("/tmp/last-message.json"),
            )
            .expect("build args");

        assert!(args.iter().all(|arg| arg != "--ask-for-approval"));
        assert!(args.iter().all(|arg| arg != "-o"));
        assert!(args.iter().all(|arg| arg != "--config"));
        assert!(args.iter().any(|arg| arg == "--output-last-message"));
        assert_eq!(
            args,
            vec![
                "exec",
                "--sandbox",
                "danger-full-access",
                "-C",
                "/tmp/repo",
                "-m",
                "gpt-5",
                "--json",
                "--output-schema",
                "/tmp/schema.json",
                "--output-last-message",
                "/tmp/last-message.json",
                "-",
            ]
        );
    }

    #[test]
    fn build_exec_args_use_read_only_sandbox_for_non_mutating_jobs() {
        let adapter = CodexCliAdapter::new("codex", "gpt-5");
        let args = adapter
            .build_exec_args(
                &request(false),
                Path::new("/tmp/schema.json"),
                Path::new("/tmp/last-message.json"),
            )
            .expect("build args");

        let sandbox_idx = args.iter().position(|arg| arg == "--sandbox").unwrap();
        assert_eq!(args[sandbox_idx + 1], "read-only");
    }

    #[test]
    fn fallback_structured_output_schema_requires_nullable_validation() {
        let schema = structured_output_schema();
        assert_eq!(
            schema["required"],
            serde_json::json!(["summary", "validation"])
        );
        assert_eq!(
            schema["properties"]["validation"]["type"],
            serde_json::json!(["string", "null"])
        );
    }
}
