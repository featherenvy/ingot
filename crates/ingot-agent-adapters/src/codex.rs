use std::path::{Path, PathBuf};

use ingot_agent_protocol::adapter::{AgentAdapter, AgentError};
use ingot_agent_protocol::request::AgentRequest;
use ingot_agent_protocol::response::AgentResponse;
use ingot_domain::agent_model::AgentModel;
use tokio::fs;
use tracing::{info, warn};

use crate::{output_schema, subprocess};

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

        let schema = output_schema(request);
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

        let output = subprocess::run_cli_subprocess(
            &self.cli_path,
            &args,
            working_dir,
            &request.prompt,
            "codex",
        )
        .await?;
        info!(exit_code = output.exit_code, "codex exec finished");

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
            exit_code: output.exit_code,
            stdout: output.stdout,
            stderr: output.stderr,
            result,
        })
    }

    async fn cancel(&self, pid: u32) -> Result<(), AgentError> {
        subprocess::cancel_process_group(pid).await
    }
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
}
