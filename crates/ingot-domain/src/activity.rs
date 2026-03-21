use chrono::{DateTime, Utc};
use serde::ser::SerializeMap;
use serde::{Deserialize, Serialize};

use crate::ids::{
    ActivityId, ConvergenceId, ConvergenceQueueEntryId, FindingId, GitOperationId, ItemId, JobId,
    ProjectId, WorkspaceId,
};

#[cfg_attr(feature = "sqlx", derive(sqlx::Type))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "sqlx", sqlx(rename_all = "snake_case"))]
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

#[cfg_attr(feature = "sqlx", derive(sqlx::Type))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "sqlx", sqlx(rename_all = "snake_case"))]
pub enum ActivityEntityType {
    Job,
    Item,
    QueueEntry,
    Convergence,
    GitOperation,
    Finding,
    Workspace,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ActivitySubject {
    Job(JobId),
    Item(ItemId),
    QueueEntry(ConvergenceQueueEntryId),
    Convergence(ConvergenceId),
    GitOperation(GitOperationId),
    Finding(FindingId),
    Workspace(WorkspaceId),
}

impl ActivitySubject {
    #[must_use]
    pub fn entity_type(&self) -> ActivityEntityType {
        match self {
            Self::Job(_) => ActivityEntityType::Job,
            Self::Item(_) => ActivityEntityType::Item,
            Self::QueueEntry(_) => ActivityEntityType::QueueEntry,
            Self::Convergence(_) => ActivityEntityType::Convergence,
            Self::GitOperation(_) => ActivityEntityType::GitOperation,
            Self::Finding(_) => ActivityEntityType::Finding,
            Self::Workspace(_) => ActivityEntityType::Workspace,
        }
    }

    #[must_use]
    pub fn entity_id_string(&self) -> String {
        match self {
            Self::Job(id) => id.to_string(),
            Self::Item(id) => id.to_string(),
            Self::QueueEntry(id) => id.to_string(),
            Self::Convergence(id) => id.to_string(),
            Self::GitOperation(id) => id.to_string(),
            Self::Finding(id) => id.to_string(),
            Self::Workspace(id) => id.to_string(),
        }
    }

    pub fn from_parts(entity_type: ActivityEntityType, entity_id: &str) -> Result<Self, String> {
        match entity_type {
            ActivityEntityType::Job => entity_id
                .parse::<JobId>()
                .map(Self::Job)
                .map_err(|e| e.to_string()),
            ActivityEntityType::Item => entity_id
                .parse::<ItemId>()
                .map(Self::Item)
                .map_err(|e| e.to_string()),
            ActivityEntityType::QueueEntry => entity_id
                .parse::<ConvergenceQueueEntryId>()
                .map(Self::QueueEntry)
                .map_err(|e| e.to_string()),
            ActivityEntityType::Convergence => entity_id
                .parse::<ConvergenceId>()
                .map(Self::Convergence)
                .map_err(|e| e.to_string()),
            ActivityEntityType::GitOperation => entity_id
                .parse::<GitOperationId>()
                .map(Self::GitOperation)
                .map_err(|e| e.to_string()),
            ActivityEntityType::Finding => entity_id
                .parse::<FindingId>()
                .map(Self::Finding)
                .map_err(|e| e.to_string()),
            ActivityEntityType::Workspace => entity_id
                .parse::<WorkspaceId>()
                .map(Self::Workspace)
                .map_err(|e| e.to_string()),
        }
    }
}

impl Serialize for ActivitySubject {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut map = serializer.serialize_map(Some(2))?;
        map.serialize_entry("entity_type", &self.entity_type())?;
        map.serialize_entry("entity_id", &self.entity_id_string())?;
        map.end()
    }
}

impl<'de> Deserialize<'de> for ActivitySubject {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        struct Helper {
            entity_type: ActivityEntityType,
            entity_id: String,
        }
        let helper = Helper::deserialize(deserializer)?;
        Self::from_parts(helper.entity_type, &helper.entity_id)
            .map_err(serde::de::Error::custom)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Activity {
    pub id: ActivityId,
    pub project_id: ProjectId,
    pub event_type: ActivityEventType,
    #[serde(flatten)]
    pub subject: ActivitySubject,
    pub payload: serde_json::Value,
    pub created_at: DateTime<Utc>,
}
