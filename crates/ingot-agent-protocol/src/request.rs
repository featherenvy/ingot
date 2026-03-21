use std::path::PathBuf;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentRequest {
    pub prompt: String,
    pub working_dir: PathBuf,
    pub may_mutate: bool,
    pub timeout_seconds: Option<u64>,
    pub output_schema: Option<serde_json::Value>,
}
