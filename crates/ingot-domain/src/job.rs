use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::ids::{AgentId, ItemId, ItemRevisionId, JobId, ProjectId, WorkspaceId};
use crate::workspace::WorkspaceKind;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JobStatus {
    Queued,
    Assigned,
    Running,
    Completed,
    Failed,
    Cancelled,
    Expired,
    Superseded,
}

impl JobStatus {
    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            Self::Completed | Self::Failed | Self::Cancelled | Self::Expired | Self::Superseded
        )
    }

    pub fn is_active(self) -> bool {
        matches!(self, Self::Queued | Self::Assigned | Self::Running)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OutcomeClass {
    Clean,
    Findings,
    TransientFailure,
    TerminalFailure,
    ProtocolViolation,
    Cancelled,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PhaseKind {
    Author,
    Validate,
    Review,
    Investigate,
    System,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionPermission {
    MayMutate,
    MustNotMutate,
    DaemonOnly,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContextPolicy {
    Fresh,
    ResumeContext,
    None,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OutputArtifactKind {
    Commit,
    ReviewReport,
    ValidationReport,
    FindingReport,
    None,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Job {
    pub id: JobId,
    pub project_id: ProjectId,
    pub item_id: ItemId,
    pub item_revision_id: ItemRevisionId,
    pub step_id: String,
    pub semantic_attempt_no: u32,
    pub retry_no: u32,
    pub supersedes_job_id: Option<JobId>,
    pub status: JobStatus,
    pub outcome_class: Option<OutcomeClass>,
    pub phase_kind: PhaseKind,
    pub workspace_id: Option<WorkspaceId>,
    pub workspace_kind: WorkspaceKind,
    pub execution_permission: ExecutionPermission,
    pub context_policy: ContextPolicy,
    pub phase_template_slug: String,
    pub phase_template_digest: Option<String>,
    pub prompt_snapshot: Option<String>,
    pub input_base_commit_oid: Option<String>,
    pub input_head_commit_oid: Option<String>,
    pub output_artifact_kind: OutputArtifactKind,
    pub output_commit_oid: Option<String>,
    pub result_schema_version: Option<String>,
    pub result_payload: Option<serde_json::Value>,
    pub agent_id: Option<AgentId>,
    pub process_pid: Option<u32>,
    pub lease_owner_id: Option<String>,
    pub heartbeat_at: Option<DateTime<Utc>>,
    pub lease_expires_at: Option<DateTime<Utc>>,
    pub error_code: Option<String>,
    pub error_message: Option<String>,
    pub created_at: DateTime<Utc>,
    pub started_at: Option<DateTime<Utc>>,
    pub ended_at: Option<DateTime<Utc>>,
}
