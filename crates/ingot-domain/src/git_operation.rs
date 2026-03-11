use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::ids::{GitOperationId, ProjectId, WorkspaceId};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OperationKind {
    CreateJobCommit,
    PrepareConvergenceCommit,
    FinalizeTargetRef,
    ResetWorkspace,
    RemoveWorkspaceRef,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GitEntityType {
    Job,
    Convergence,
    Workspace,
    ItemRevision,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GitOperationStatus {
    Planned,
    Applied,
    Reconciled,
    Failed,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GitOperation {
    pub id: GitOperationId,
    pub project_id: ProjectId,
    pub operation_kind: OperationKind,
    pub entity_type: GitEntityType,
    pub entity_id: String,
    pub workspace_id: Option<WorkspaceId>,
    pub ref_name: Option<String>,
    pub expected_old_oid: Option<String>,
    pub new_oid: Option<String>,
    pub commit_oid: Option<String>,
    pub status: GitOperationStatus,
    pub metadata: Option<serde_json::Value>,
    pub created_at: DateTime<Utc>,
    pub completed_at: Option<DateTime<Utc>>,
}
