use std::path::{Path, PathBuf};

use ingot_agent_protocol::adapter::{AgentAdapter, AgentError};
use ingot_agent_protocol::request::AgentRequest;
use ingot_domain::agent_model::AgentModel;
use tracing::warn;

use crate::{result_from_text, subprocess};

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

    fn build_exec_args(
        &self,
        request: &AgentRequest,
        schema_file: &subprocess::TempFile,
        response_file: &subprocess::TempFile,
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
            schema_file.cli_arg("schema")?,
            "--output-last-message".into(),
            response_file.cli_arg("response")?,
            "-".into(),
        ])
    }
}

impl AgentAdapter for CodexCliAdapter {
    async fn launch(
        &self,
        request: &AgentRequest,
        working_dir: &Path,
    ) -> Result<ingot_agent_protocol::response::AgentResponse, AgentError> {
        let response_file =
            subprocess::TempFile::new(working_dir, ".ingot-codex-last-message", "txt");
        let schema_file = subprocess::output_schema_file(request, working_dir, "codex").await?;
        let launch_result = subprocess::launch_adapter(
            &self.cli_path,
            &self.model,
            request,
            working_dir,
            self.build_exec_args(request, &schema_file, &response_file)?,
            "codex",
            |_output| async {
                let final_message = response_file.read_to_string().await.ok();
                if final_message
                    .as_deref()
                    .unwrap_or_default()
                    .trim()
                    .is_empty()
                {
                    warn!(
                        response_path = %response_file.path().display(),
                        "codex did not produce a last-message payload"
                    );
                }
                Ok(final_message.as_deref().map(result_from_text))
            },
        )
        .await;
        response_file.cleanup().await;
        schema_file.cleanup().await;
        launch_result
    }

    async fn cancel(&self, pid: u32) -> Result<(), AgentError> {
        subprocess::cancel_process_group(pid).await
    }
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
        let schema_file = subprocess::TempFile::from_path("/tmp/schema.json");
        let response_file = subprocess::TempFile::from_path("/tmp/last-message.json");
        let args = adapter
            .build_exec_args(&request(true), &schema_file, &response_file)
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
        let schema_file = subprocess::TempFile::from_path("/tmp/schema.json");
        let response_file = subprocess::TempFile::from_path("/tmp/last-message.json");
        let args = adapter
            .build_exec_args(&request(false), &schema_file, &response_file)
            .expect("build args");

        let sandbox_idx = args.iter().position(|arg| arg == "--sandbox").unwrap();
        assert_eq!(args[sandbox_idx + 1], "read-only");
    }
}
