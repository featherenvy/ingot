use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::ids::{ConvergenceId, ItemId, ItemRevisionId, ProjectId, WorkspaceId};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConvergenceStrategy {
    RebaseThenFastForward,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConvergenceStatus {
    Queued,
    Running,
    Conflicted,
    Prepared,
    Finalized,
    Failed,
    Cancelled,
}

impl ConvergenceStatus {
    pub fn is_active(self) -> bool {
        matches!(self, Self::Queued | Self::Running | Self::Prepared)
    }

    pub fn is_terminal(self) -> bool {
        matches!(self, Self::Finalized | Self::Failed | Self::Cancelled)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Convergence {
    pub id: ConvergenceId,
    pub project_id: ProjectId,
    pub item_id: ItemId,
    pub item_revision_id: ItemRevisionId,
    pub source_workspace_id: WorkspaceId,
    pub integration_workspace_id: Option<WorkspaceId>,
    pub source_head_commit_oid: String,
    pub target_ref: String,
    pub strategy: ConvergenceStrategy,
    pub status: ConvergenceStatus,
    pub input_target_commit_oid: Option<String>,
    pub prepared_commit_oid: Option<String>,
    pub final_target_commit_oid: Option<String>,
    pub target_head_valid: Option<bool>,
    pub conflict_summary: Option<String>,
    pub created_at: DateTime<Utc>,
    pub completed_at: Option<DateTime<Utc>>,
}
