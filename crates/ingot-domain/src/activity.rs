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
    ApprovalRequested,
    ApprovalApproved,
    ApprovalRejected,
    ConvergenceStarted,
    ConvergenceConflicted,
    ConvergencePrepared,
    ConvergenceFinalized,
    ConvergenceFailed,
    GitOperationPlanned,
    GitOperationReconciled,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Activity {
    pub id: ActivityId,
    pub project_id: ProjectId,
    pub event_type: ActivityEventType,
    pub entity_type: String,
    pub entity_id: String,
    pub payload: serde_json::Value,
    pub created_at: DateTime<Utc>,
}
