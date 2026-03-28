use std::future::Future;

use chrono::{DateTime, Utc};

use crate::ids::*;
use crate::item::EscalationReason;
use crate::job::{Job, JobStatus, OutcomeClass};
use crate::lease_owner_id::LeaseOwnerId;

use super::super::errors::RepositoryError;

pub trait JobRepository: Send + Sync {
    fn list_by_project(
        &self,
        project_id: ProjectId,
    ) -> impl Future<Output = Result<Vec<Job>, RepositoryError>> + Send;
    fn list_by_revision(
        &self,
        revision_id: ItemRevisionId,
    ) -> impl Future<Output = Result<Vec<Job>, RepositoryError>> + Send;
    fn get(&self, id: JobId) -> impl Future<Output = Result<Job, RepositoryError>> + Send;
    fn create(&self, job: &Job) -> impl Future<Output = Result<(), RepositoryError>> + Send;
    fn update(&self, job: &Job) -> impl Future<Output = Result<(), RepositoryError>> + Send;
    fn find_active_for_revision(
        &self,
        revision_id: ItemRevisionId,
    ) -> impl Future<Output = Result<Option<Job>, RepositoryError>> + Send;
    fn list_by_item(
        &self,
        item_id: ItemId,
    ) -> impl Future<Output = Result<Vec<Job>, RepositoryError>> + Send;
    fn list_queued(
        &self,
        limit: u32,
    ) -> impl Future<Output = Result<Vec<Job>, RepositoryError>> + Send;
    fn list_active(&self) -> impl Future<Output = Result<Vec<Job>, RepositoryError>> + Send;
    fn start_execution(
        &self,
        params: StartJobExecutionParams,
    ) -> impl Future<Output = Result<(), RepositoryError>> + Send;
    fn heartbeat_execution(
        &self,
        job_id: JobId,
        item_id: ItemId,
        revision_id: ItemRevisionId,
        lease_owner_id: &LeaseOwnerId,
        lease_expires_at: DateTime<Utc>,
    ) -> impl Future<Output = Result<(), RepositoryError>> + Send;
    fn finish_non_success(
        &self,
        params: FinishJobNonSuccessParams,
    ) -> impl Future<Output = Result<(), RepositoryError>> + Send;
    fn delete(&self, id: JobId) -> impl Future<Output = Result<(), RepositoryError>> + Send;
}

pub struct StartJobExecutionParams {
    pub job_id: JobId,
    pub item_id: ItemId,
    pub expected_item_revision_id: ItemRevisionId,
    pub workspace_id: Option<WorkspaceId>,
    pub agent_id: Option<AgentId>,
    pub lease_owner_id: LeaseOwnerId,
    pub process_pid: Option<u32>,
    pub lease_expires_at: DateTime<Utc>,
}

#[derive(Debug, Clone)]
pub struct FinishJobNonSuccessParams {
    pub job_id: JobId,
    pub item_id: ItemId,
    pub expected_item_revision_id: ItemRevisionId,
    pub status: JobStatus,
    pub outcome_class: Option<OutcomeClass>,
    pub error_code: Option<String>,
    pub error_message: Option<String>,
    pub escalation_reason: Option<EscalationReason>,
}
