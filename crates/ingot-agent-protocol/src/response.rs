use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentOutputChannel {
    Primary,
    Diagnostic,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentOutputKind {
    Text,
    Progress,
    ToolCall,
    ToolResult,
    Lifecycle,
    RawFallback,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentOutputStatus {
    InProgress,
    Completed,
    Failed,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AgentOutputSegment {
    pub sequence: u64,
    pub channel: AgentOutputChannel,
    pub kind: AgentOutputKind,
    pub status: Option<AgentOutputStatus>,
    pub title: Option<String>,
    pub text: Option<String>,
    pub data: Option<serde_json::Value>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AgentOutputSegmentDraft {
    pub channel: AgentOutputChannel,
    pub kind: AgentOutputKind,
    pub status: Option<AgentOutputStatus>,
    pub title: Option<String>,
    pub text: Option<String>,
    pub data: Option<serde_json::Value>,
}

impl AgentOutputSegmentDraft {
    pub fn new(channel: AgentOutputChannel, kind: AgentOutputKind) -> Self {
        Self {
            channel,
            kind,
            status: None,
            title: None,
            text: None,
            data: None,
        }
    }

    pub fn text(kind: AgentOutputKind, text: impl Into<String>) -> Self {
        Self {
            channel: AgentOutputChannel::Primary,
            kind,
            status: None,
            title: None,
            text: Some(text.into()),
            data: None,
        }
    }

    pub fn diagnostic_text(text: impl Into<String>) -> Self {
        Self {
            channel: AgentOutputChannel::Diagnostic,
            kind: AgentOutputKind::Text,
            status: None,
            title: None,
            text: Some(text.into()),
            data: None,
        }
    }

    pub fn into_segment(self, sequence: u64) -> AgentOutputSegment {
        AgentOutputSegment {
            sequence,
            channel: self.channel,
            kind: self.kind,
            status: self.status,
            title: self.title,
            text: self.text,
            data: self.data,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AgentOutputDocument {
    pub schema_version: String,
    pub segments: Vec<AgentOutputSegment>,
}

impl AgentOutputDocument {
    pub const SCHEMA_VERSION: &str = "agent_output:v1";

    pub fn empty() -> Self {
        Self {
            schema_version: Self::SCHEMA_VERSION.into(),
            segments: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct JobStructuredResult {
    pub schema_version: Option<String>,
    pub payload: serde_json::Value,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OutputStream {
    Stdout,
    Stderr,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AgentOutputChunk {
    pub stream: OutputStream,
    pub chunk: String,
    pub segments: Vec<AgentOutputSegmentDraft>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentResponse {
    pub exit_code: i32,
    pub stdout: String,
    pub stderr: String,
    pub result: Option<serde_json::Value>,
}
