use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::ids::{ItemRevisionId, JobId, ProjectId, WorkspaceId};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkspaceKind {
    Authoring,
    Review,
    Integration,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkspaceStrategy {
    Worktree,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RetentionPolicy {
    Ephemeral,
    RetainUntilDebug,
    Persistent,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkspaceStatus {
    Provisioning,
    Ready,
    Busy,
    Stale,
    RetainedForDebug,
    Abandoned,
    Error,
    Removing,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Workspace {
    pub id: WorkspaceId,
    pub project_id: ProjectId,
    pub kind: WorkspaceKind,
    pub strategy: WorkspaceStrategy,
    pub path: String,
    pub created_for_revision_id: Option<ItemRevisionId>,
    pub parent_workspace_id: Option<WorkspaceId>,
    pub target_ref: Option<String>,
    pub workspace_ref: Option<String>,
    pub base_commit_oid: Option<String>,
    pub head_commit_oid: Option<String>,
    pub retention_policy: RetentionPolicy,
    pub status: WorkspaceStatus,
    pub current_job_id: Option<JobId>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}
