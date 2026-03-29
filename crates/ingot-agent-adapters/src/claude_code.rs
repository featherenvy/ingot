use std::path::{Path, PathBuf};

use ingot_agent_protocol::adapter::{AgentAdapter, AgentError};
use ingot_agent_protocol::request::AgentRequest;

use crate::{result_from_text, subprocess};

#[derive(Debug, Clone)]
pub struct ClaudeCodeCliAdapter {
    command: subprocess::AdapterCommandConfig,
}

impl ClaudeCodeCliAdapter {
    pub fn new(
        cli_path: impl Into<PathBuf>,
        model: impl Into<ingot_domain::agent_model::AgentModel>,
    ) -> Self {
        Self {
            command: subprocess::AdapterCommandConfig::new(cli_path, model),
        }
    }

    fn build_print_args(&self, request: &AgentRequest) -> Result<Vec<String>, AgentError> {
        let mut args = vec![
            "--print".into(),
            "--output-format".into(),
            "json".into(),
            "--model".into(),
            self.command.model().to_string(),
            "--no-session-persistence".into(),
            "--dangerously-skip-permissions".into(),
            "--json-schema".into(),
            subprocess::inline_output_schema(request)?,
        ];

        if !request.may_mutate {
            args.push("--disallowedTools".into());
            args.push("Edit,Write,NotebookEdit".into());
        }

        Ok(args)
    }
}

impl AgentAdapter for ClaudeCodeCliAdapter {
    async fn launch(
        &self,
        request: &AgentRequest,
        working_dir: &Path,
    ) -> Result<ingot_agent_protocol::response::AgentResponse, AgentError> {
        subprocess::launch_adapter(
            self.command.cli_path(),
            self.command.model(),
            request,
            working_dir,
            self.build_print_args(request)?,
            "claude",
            |output| {
                let stdout = output.stdout.clone();
                async move { Ok(parse_print_output(&stdout)) }
            },
        )
        .await
    }

    async fn cancel(&self, pid: u32) -> Result<(), AgentError> {
        subprocess::cancel_process_group(pid).await
    }
}

/// Parse Claude's `--print --output-format json` envelope.
///
/// Envelope shape:
/// ```json
/// { "type": "result", "subtype": "success", "is_error": false, "result": "...",
///   "structured_output": { ... }, ... }
/// ```
///
/// If `is_error` is true, returns `None`. Prefers `structured_output` (populated
/// when `--json-schema` is used) over `result`. Falls back to parsing `result`
/// as JSON or wrapping it as `{"summary": "<text>"}`.
fn parse_print_output(stdout: &str) -> Option<serde_json::Value> {
    let envelope: serde_json::Value = serde_json::from_str(stdout).ok()?;

    if envelope.get("is_error").and_then(|v| v.as_bool()) == Some(true) {
        return None;
    }

    // Prefer structured_output (set when --json-schema is used)
    if let Some(structured) = envelope.get("structured_output") {
        if structured.is_object() || structured.is_array() {
            return Some(structured.clone());
        }
    }

    let result = envelope.get("result")?;

    match result {
        serde_json::Value::String(s) => {
            // Try to parse as JSON first
            match serde_json::from_str::<serde_json::Value>(s) {
                Ok(parsed) if parsed.is_object() || parsed.is_array() => Some(parsed),
                _ => Some(result_from_text(s)),
            }
        }
        serde_json::Value::Object(_) | serde_json::Value::Array(_) => Some(result.clone()),
        _ => None,
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
    fn build_print_args_for_mutating_job() {
        let adapter = ClaudeCodeCliAdapter::new("claude", "claude-sonnet-4-6");
        let args = adapter
            .build_print_args(&request(true))
            .expect("build args");

        assert!(args.contains(&"--print".into()));
        assert!(args.contains(&"--dangerously-skip-permissions".into()));
        assert!(args.contains(&"--no-session-persistence".into()));
        assert!(!args.contains(&"--bare".into()));
        assert!(!args.contains(&"--disallowedTools".into()));

        // Verify model position
        let model_idx = args.iter().position(|a| a == "--model").unwrap();
        assert_eq!(args[model_idx + 1], "claude-sonnet-4-6");
    }

    #[test]
    fn build_print_args_for_read_only_job() {
        let adapter = ClaudeCodeCliAdapter::new("claude", "claude-sonnet-4-6");
        let args = adapter
            .build_print_args(&request(false))
            .expect("build args");

        assert!(args.contains(&"--disallowedTools".into()));
        let idx = args.iter().position(|a| a == "--disallowedTools").unwrap();
        assert_eq!(args[idx + 1], "Edit,Write,NotebookEdit");
        assert!(args.contains(&"--dangerously-skip-permissions".into()));
    }

    #[test]
    fn build_print_args_include_json_schema_inline() {
        let custom_schema =
            serde_json::json!({"type": "object", "properties": {"outcome": {"type": "string"}}});
        let req = AgentRequest {
            output_schema: Some(custom_schema.clone()),
            ..request(true)
        };
        let adapter = ClaudeCodeCliAdapter::new("claude", "claude-sonnet-4-6");
        let args = adapter.build_print_args(&req).expect("build args");

        let schema_idx = args.iter().position(|a| a == "--json-schema").unwrap();
        let schema_str = &args[schema_idx + 1];
        let parsed: serde_json::Value = serde_json::from_str(schema_str).expect("valid JSON");
        assert_eq!(parsed, custom_schema);
    }

    #[test]
    fn parse_print_output_prefers_structured_output_over_result() {
        let envelope = serde_json::json!({
            "type": "result",
            "subtype": "success",
            "is_error": false,
            "result": "",
            "structured_output": {"outcome": "findings", "summary": "All good", "findings": []}
        });
        let result = parse_print_output(&serde_json::to_string(&envelope).unwrap());
        assert_eq!(
            result,
            Some(serde_json::json!({"outcome": "findings", "summary": "All good", "findings": []}))
        );
    }

    #[test]
    fn parse_print_output_falls_back_to_result_when_no_structured_output() {
        let envelope = serde_json::json!({
            "type": "result",
            "subtype": "success",
            "is_error": false,
            "result": r#"{"summary":"done","validation":null}"#,
            "duration_ms": 1000
        });
        let result = parse_print_output(&serde_json::to_string(&envelope).unwrap());
        assert_eq!(
            result,
            Some(serde_json::json!({"summary": "done", "validation": null}))
        );
    }

    #[test]
    fn parse_print_output_ignores_null_structured_output() {
        let envelope = serde_json::json!({
            "type": "result",
            "subtype": "success",
            "is_error": false,
            "result": {"summary": "done", "validation": null},
            "structured_output": null
        });
        let result = parse_print_output(&serde_json::to_string(&envelope).unwrap());
        assert_eq!(
            result,
            Some(serde_json::json!({"summary": "done", "validation": null}))
        );
    }

    #[test]
    fn parse_print_output_extracts_structured_json_result() {
        let envelope = serde_json::json!({
            "type": "result",
            "subtype": "success",
            "is_error": false,
            "result": r#"{"summary":"done","validation":null}"#,
            "duration_ms": 1000
        });
        let result = parse_print_output(&serde_json::to_string(&envelope).unwrap());
        assert_eq!(
            result,
            Some(serde_json::json!({"summary": "done", "validation": null}))
        );
    }

    #[test]
    fn parse_print_output_extracts_object_result() {
        let envelope = serde_json::json!({
            "type": "result",
            "subtype": "success",
            "is_error": false,
            "result": {"summary": "done", "validation": null}
        });
        let result = parse_print_output(&serde_json::to_string(&envelope).unwrap());
        assert_eq!(
            result,
            Some(serde_json::json!({"summary": "done", "validation": null}))
        );
    }

    #[test]
    fn parse_print_output_returns_none_when_is_error() {
        let envelope = serde_json::json!({
            "type": "result",
            "subtype": "error",
            "is_error": true,
            "result": "something went wrong"
        });
        let result = parse_print_output(&serde_json::to_string(&envelope).unwrap());
        assert_eq!(result, None);
    }

    #[test]
    fn parse_print_output_handles_non_json_gracefully() {
        let envelope = serde_json::json!({
            "type": "result",
            "subtype": "success",
            "is_error": false,
            "result": "I completed the task successfully."
        });
        let result = parse_print_output(&serde_json::to_string(&envelope).unwrap());
        assert_eq!(
            result,
            Some(serde_json::json!({"summary": "I completed the task successfully."}))
        );
    }

    #[test]
    fn parse_print_output_returns_none_for_non_json_stdout() {
        let result = parse_print_output("not json at all");
        assert_eq!(result, None);
    }
}
