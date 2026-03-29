pub mod claude_code;
pub mod codex;
pub mod registry;
mod subprocess;

use std::path::Path;

use ingot_agent_protocol::adapter::AgentError;
use ingot_agent_protocol::report;
use ingot_agent_protocol::request::AgentRequest;
use ingot_agent_protocol::response::{AgentOutputChunk, AgentResponse};
use ingot_domain::agent::{AdapterKind, Agent};
use tokio::sync::mpsc;

/// Launch an agent job by dispatching to the correct CLI adapter based on `AdapterKind`.
pub async fn launch_agent(
    agent: &Agent,
    request: &AgentRequest,
    working_dir: &Path,
    output_tx: Option<mpsc::Sender<AgentOutputChunk>>,
) -> Result<AgentResponse, AgentError> {
    match agent.adapter_kind {
        AdapterKind::Codex => {
            codex::CodexCliAdapter::new(agent.cli_path.clone(), agent.model.clone())
                .launch_with_output(request, working_dir, output_tx)
                .await
        }
        AdapterKind::ClaudeCode => {
            claude_code::ClaudeCodeCliAdapter::new(agent.cli_path.clone(), agent.model.clone())
                .launch_with_output(request, working_dir, output_tx)
                .await
        }
    }
}

/// Cancel a running agent subprocess by process-group ID.
pub async fn cancel_agent(pid: u32) -> Result<(), AgentError> {
    subprocess::cancel_process_group(pid).await
}

/// Resolve the structured-output schema for a request, falling back to the
/// shared default schema when the caller does not provide one.
pub(crate) fn output_schema(request: &AgentRequest) -> serde_json::Value {
    request
        .output_schema
        .clone()
        .unwrap_or_else(report::commit_summary_schema)
}

/// Parse a textual adapter payload as JSON when possible, otherwise wrap it in
/// the default summary envelope.
pub(crate) fn result_from_text(payload: &str) -> serde_json::Value {
    serde_json::from_str(payload)
        .unwrap_or_else(|_| report::commit_summary_payload(payload.trim(), None))
}

/// Default structured-output JSON schema shared by all CLI adapters.
#[cfg(test)]
pub(crate) fn structured_output_schema() -> serde_json::Value {
    report::commit_summary_schema()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(output_schema: Option<serde_json::Value>) -> AgentRequest {
        AgentRequest {
            prompt: "Implement the change".into(),
            working_dir: "/tmp/repo".into(),
            may_mutate: true,
            timeout_seconds: Some(60),
            output_schema,
        }
    }

    #[test]
    fn output_schema_falls_back_to_shared_default_schema() {
        assert_eq!(output_schema(&request(None)), structured_output_schema());
    }

    #[test]
    fn output_schema_preserves_custom_schema() {
        let custom_schema =
            serde_json::json!({"type": "object", "properties": {"outcome": {"type": "string"}}});

        assert_eq!(
            output_schema(&request(Some(custom_schema.clone()))),
            custom_schema
        );
    }

    #[test]
    fn structured_output_schema_requires_nullable_validation() {
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

    #[test]
    fn result_from_text_parses_json_when_present() {
        assert_eq!(
            result_from_text(r#"{"summary":"done","validation":null}"#),
            serde_json::json!({"summary":"done","validation":null})
        );
    }

    #[test]
    fn result_from_text_wraps_plain_text_summary() {
        assert_eq!(
            result_from_text("  completed work  "),
            serde_json::json!({"summary":"completed work","validation":null})
        );
    }
}
