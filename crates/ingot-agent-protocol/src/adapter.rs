use std::future::Future;
use std::path::Path;

use crate::request::AgentRequest;
use crate::response::AgentResponse;

/// Trait for agent adapter implementations.
pub trait AgentAdapter: Send + Sync {
    fn launch(
        &self,
        request: &AgentRequest,
        working_dir: &Path,
    ) -> impl Future<Output = Result<AgentResponse, AgentError>> + Send;

    fn cancel(&self, pid: u32) -> impl Future<Output = Result<(), AgentError>> + Send;
}

#[derive(Debug, thiserror::Error)]
pub enum AgentError {
    #[error("launch failed: {0}")]
    LaunchFailed(String),
    #[error("process error: {0}")]
    ProcessError(String),
    #[error("timeout")]
    Timeout,
    #[error("protocol violation: {0}")]
    ProtocolViolation(String),
}
