use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::git_ref::GitRef;
use crate::ids::{ConvergenceQueueEntryId, ItemId, ItemRevisionId, ProjectId};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConvergenceQueueEntryStatus {
    Queued,
    Head,
    Released,
    Cancelled,
}

impl ConvergenceQueueEntryStatus {
    pub fn is_active(self) -> bool {
        matches!(self, Self::Queued | Self::Head)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConvergenceQueueEntry {
    pub id: ConvergenceQueueEntryId,
    pub project_id: ProjectId,
    pub item_id: ItemId,
    pub item_revision_id: ItemRevisionId,
    pub target_ref: GitRef,
    pub status: ConvergenceQueueEntryStatus,
    pub head_acquired_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub released_at: Option<DateTime<Utc>>,
}
