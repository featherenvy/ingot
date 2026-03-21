use crate::convergence_queue::{ConvergenceQueueEntry, ConvergenceQueueEntryStatus};
use crate::git_ref::GitRef;
use crate::ids;
use chrono::{DateTime, Utc};

use super::timestamps::default_timestamp;

pub struct ConvergenceQueueEntryBuilder {
    id: ids::ConvergenceQueueEntryId,
    project_id: ids::ProjectId,
    item_id: ids::ItemId,
    item_revision_id: ids::ItemRevisionId,
    target_ref: GitRef,
    status: ConvergenceQueueEntryStatus,
    head_acquired_at: Option<DateTime<Utc>>,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
    released_at: Option<DateTime<Utc>>,
}

impl ConvergenceQueueEntryBuilder {
    pub fn new(
        project_id: ids::ProjectId,
        item_id: ids::ItemId,
        item_revision_id: ids::ItemRevisionId,
    ) -> Self {
        let now = default_timestamp();
        Self {
            id: ids::ConvergenceQueueEntryId::new(),
            project_id,
            item_id,
            item_revision_id,
            target_ref: GitRef::new("refs/heads/main"),
            status: ConvergenceQueueEntryStatus::Head,
            head_acquired_at: Some(now),
            created_at: now,
            updated_at: now,
            released_at: None,
        }
    }

    pub fn status(mut self, status: ConvergenceQueueEntryStatus) -> Self {
        self.status = status;
        self
    }

    pub fn created_at(mut self, created_at: DateTime<Utc>) -> Self {
        self.created_at = created_at;
        self.updated_at = created_at;
        self.head_acquired_at = Some(created_at);
        self
    }

    pub fn build(self) -> ConvergenceQueueEntry {
        ConvergenceQueueEntry {
            id: self.id,
            project_id: self.project_id,
            item_id: self.item_id,
            item_revision_id: self.item_revision_id,
            target_ref: self.target_ref,
            status: self.status,
            head_acquired_at: self.head_acquired_at,
            created_at: self.created_at,
            updated_at: self.updated_at,
            released_at: self.released_at,
        }
    }
}
