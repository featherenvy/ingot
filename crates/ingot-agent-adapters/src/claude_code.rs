use std::path::Path;

use ingot_agent_protocol::adapter::{AgentAdapter, AgentError};
use ingot_agent_protocol::request::AgentRequest;
use ingot_agent_protocol::response::AgentResponse;

#[derive(Debug, Clone, Default)]
pub struct ClaudeCodeCliAdapter;

impl AgentAdapter for ClaudeCodeCliAdapter {
    async fn launch(
        &self,
        _request: &AgentRequest,
        _working_dir: &Path,
    ) -> Result<AgentResponse, AgentError> {
        Err(AgentError::LaunchFailed(
            "claude_code runtime execution is not implemented in the MVP yet".into(),
        ))
    }

    async fn cancel(&self, _pid: u32) -> Result<(), AgentError> {
        Err(AgentError::ProcessError(
            "claude_code subprocess cancellation is not implemented yet".into(),
        ))
    }
}
