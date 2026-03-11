use serde::{Deserialize, Serialize};

use crate::ids::AgentId;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AdapterKind {
    ClaudeCode,
    Codex,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentCapability {
    ReadOnlyJobs,
    MutatingJobs,
    StructuredOutput,
    StreamingProgress,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentStatus {
    Available,
    Unavailable,
    Probing,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Agent {
    pub id: AgentId,
    pub slug: String,
    pub name: String,
    pub adapter_kind: AdapterKind,
    pub provider: String,
    pub model: String,
    pub cli_path: String,
    pub capabilities: Vec<AgentCapability>,
    pub health_check: Option<String>,
    pub status: AgentStatus,
}
