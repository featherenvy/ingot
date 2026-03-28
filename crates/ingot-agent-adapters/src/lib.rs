pub mod claude_code;
pub mod codex;
pub mod registry;
mod subprocess;

use std::path::Path;

use ingot_agent_protocol::adapter::{AgentAdapter, AgentError};
use ingot_agent_protocol::request::AgentRequest;
use ingot_agent_protocol::response::AgentResponse;
use ingot_domain::agent::{AdapterKind, Agent};

/// Launch an agent job by dispatching to the correct CLI adapter based on `AdapterKind`.
pub async fn launch_agent(
    agent: &Agent,
    request: &AgentRequest,
    working_dir: &Path,
) -> Result<AgentResponse, AgentError> {
    match agent.adapter_kind {
        AdapterKind::Codex => {
            codex::CodexCliAdapter::new(agent.cli_path.clone(), agent.model.clone())
                .launch(request, working_dir)
                .await
        }
        AdapterKind::ClaudeCode => {
            claude_code::ClaudeCodeCliAdapter::new(agent.cli_path.clone(), agent.model.clone())
                .launch(request, working_dir)
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
        .unwrap_or_else(structured_output_schema)
}

/// Default structured-output JSON schema shared by all CLI adapters.
pub(crate) fn structured_output_schema() -> serde_json::Value {
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
