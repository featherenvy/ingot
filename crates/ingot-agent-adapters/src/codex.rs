use std::path::{Path, PathBuf};

use ingot_agent_protocol::adapter::{AgentAdapter, AgentError};
use ingot_agent_protocol::request::AgentRequest;
use ingot_agent_protocol::response::AgentResponse;
use tokio::fs;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::Command;
use tracing::{debug, info, warn};

#[derive(Debug, Clone)]
pub struct CodexCliAdapter {
    cli_path: String,
    model: String,
}

impl CodexCliAdapter {
    pub fn new(cli_path: impl Into<String>, model: impl Into<String>) -> Self {
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
            request.working_dir.clone(),
            "-m".into(),
            self.model.clone(),
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
            cli_path = %self.cli_path,
            model = %self.model,
            working_dir = %request.working_dir,
            may_mutate = request.may_mutate,
            args = ?args,
            "launching codex exec"
        );

        let mut child = Command::new(&self.cli_path)
            .args(&args)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .map_err(|err| AgentError::LaunchFailed(err.to_string()))?;
        debug!("spawned codex subprocess");

        if let Some(stdin) = child.stdin.as_mut() {
            stdin
                .write_all(request.prompt.as_bytes())
                .await
                .map_err(|err| AgentError::ProcessError(err.to_string()))?;
        }
        debug!("wrote prompt to codex stdin");

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

    async fn cancel(&self, _pid: u32) -> Result<(), AgentError> {
        Err(AgentError::ProcessError(
            "codex subprocess cancellation is not implemented yet".into(),
        ))
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

        assert_eq!(args[2], "read-only");
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
