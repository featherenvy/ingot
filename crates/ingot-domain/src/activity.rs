use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::ids::{ActivityId, ProjectId};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ActivityEventType {
    ItemCreated,
    ItemRevisionCreated,
    ItemUpdated,
    ItemDeferred,
    ItemResumed,
    ItemDismissed,
    ItemInvalidated,
    ItemReopened,
    ItemEscalated,
    ItemEscalationCleared,
    JobDispatched,
    JobCompleted,
    JobFailed,
    JobCancelled,
    FindingPromoted,
    FindingDismissed,
    FindingTriaged,
    ApprovalRequested,
    ApprovalApproved,
    ApprovalRejected,
    ConvergenceQueued,
    ConvergenceLaneAcquired,
    ConvergenceStarted,
    ConvergenceConflicted,
    ConvergencePrepared,
    ConvergenceFinalized,
    ConvergenceFailed,
    CheckoutSyncBlocked,
    CheckoutSyncCleared,
    GitOperationPlanned,
    GitOperationReconciled,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ActivityEntityType {
    Job,
    Item,
    QueueEntry,
    Convergence,
    GitOperation,
    Finding,
    Workspace,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Activity {
    pub id: ActivityId,
    pub project_id: ProjectId,
    pub event_type: ActivityEventType,
    pub entity_type: ActivityEntityType,
    pub entity_id: String,
    pub payload: serde_json::Value,
    pub created_at: DateTime<Utc>,
}
