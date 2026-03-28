use chrono::{DateTime, Utc};
use ingot_domain::ids::{ItemId, ItemRevisionId, JobId, ProjectId};
use ingot_domain::job::Job;
use ingot_domain::lease_owner_id::LeaseOwnerId;
use ingot_domain::ports::{
    FinishJobNonSuccessParams, JobRepository, RepositoryError, StartJobExecutionParams,
};

use crate::db::Database;

impl JobRepository for Database {
    async fn list_by_project(&self, project_id: ProjectId) -> Result<Vec<Job>, RepositoryError> {
        self.list_jobs_by_project(project_id).await
    }
    async fn list_by_revision(
        &self,
        revision_id: ItemRevisionId,
    ) -> Result<Vec<Job>, RepositoryError> {
        self.list_jobs_by_revision(revision_id).await
    }
    async fn get(&self, id: JobId) -> Result<Job, RepositoryError> {
        self.get_job(id).await
    }
    async fn create(&self, job: &Job) -> Result<(), RepositoryError> {
        self.create_job(job).await
    }
    async fn update(&self, job: &Job) -> Result<(), RepositoryError> {
        self.update_job(job).await
    }
    async fn find_active_for_revision(
        &self,
        revision_id: ItemRevisionId,
    ) -> Result<Option<Job>, RepositoryError> {
        self.find_active_job_for_revision(revision_id).await
    }
    async fn list_by_item(&self, item_id: ItemId) -> Result<Vec<Job>, RepositoryError> {
        self.list_jobs_by_item(item_id).await
    }
    async fn list_queued(&self, limit: u32) -> Result<Vec<Job>, RepositoryError> {
        self.list_queued_jobs(limit).await
    }
    async fn list_active(&self) -> Result<Vec<Job>, RepositoryError> {
        self.list_active_jobs().await
    }
    async fn start_execution(
        &self,
        params: StartJobExecutionParams,
    ) -> Result<(), RepositoryError> {
        self.start_job_execution(params).await
    }
    async fn heartbeat_execution(
        &self,
        job_id: JobId,
        item_id: ItemId,
        revision_id: ItemRevisionId,
        lease_owner_id: &LeaseOwnerId,
        lease_expires_at: DateTime<Utc>,
    ) -> Result<(), RepositoryError> {
        self.heartbeat_job_execution(
            job_id,
            item_id,
            revision_id,
            lease_owner_id,
            lease_expires_at,
        )
        .await
    }
    async fn finish_non_success(
        &self,
        params: FinishJobNonSuccessParams,
    ) -> Result<(), RepositoryError> {
        self.finish_job_non_success(params).await
    }
    async fn delete(&self, id: JobId) -> Result<(), RepositoryError> {
        self.delete_job(id).await
    }
}
