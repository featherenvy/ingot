use std::path::{Path, PathBuf};

use ingot_agent_protocol::adapter::{AgentAdapter, AgentError};
use ingot_agent_protocol::request::AgentRequest;
use tracing::warn;

use crate::{result_from_text, subprocess};

#[derive(Debug, Clone)]
pub struct CodexCliAdapter {
    command: subprocess::AdapterCommandConfig,
}

impl CodexCliAdapter {
    pub fn new(
        cli_path: impl Into<PathBuf>,
        model: impl Into<ingot_domain::agent_model::AgentModel>,
    ) -> Self {
        Self {
            command: subprocess::AdapterCommandConfig::new(cli_path, model),
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
            self.command.model().to_string(),
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
        subprocess::launch_adapter_with_schema_and_result_files(
            &self.command,
            request,
            working_dir,
            subprocess::SchemaResultFiles {
                adapter_name: "codex",
                result_file_stem: ".ingot-codex-last-message",
                result_file_extension: "txt",
            },
            |schema_file, response_file| self.build_exec_args(request, schema_file, response_file),
            |_output, response_file| async move {
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
        .await
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
