mod conflict;
mod mapping;
mod mutations;
mod queries;
mod repository;

#[cfg(test)]
mod tests;

use chrono::{DateTime, Utc};
use ingot_domain::ids::{ItemId, ItemRevisionId, JobId};
use ingot_domain::job::JobAssignment;
use ingot_domain::lease_owner_id::LeaseOwnerId;

#[derive(Debug, Clone)]
pub struct ClaimQueuedAgentJobExecutionParams {
    pub job_id: JobId,
    pub item_id: ItemId,
    pub expected_item_revision_id: ItemRevisionId,
    pub assignment: JobAssignment,
    pub lease_owner_id: LeaseOwnerId,
    pub lease_expires_at: DateTime<Utc>,
}
