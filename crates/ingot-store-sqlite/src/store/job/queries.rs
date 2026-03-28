use ingot_domain::ids::{ItemId, ItemRevisionId, JobId, ProjectId};
use ingot_domain::job::Job;
use ingot_domain::ports::RepositoryError;

use super::mapping::map_job;
use crate::db::Database;
use crate::store::helpers::db_err;

impl Database {
    pub async fn list_jobs_by_item(&self, item_id: ItemId) -> Result<Vec<Job>, RepositoryError> {
        let rows = sqlx::query("SELECT * FROM jobs WHERE item_id = ? ORDER BY created_at DESC")
            .bind(item_id)
            .fetch_all(&self.pool)
            .await
            .map_err(db_err)?;

        rows.iter().map(map_job).collect()
    }

    pub async fn list_queued_jobs(&self, limit: u32) -> Result<Vec<Job>, RepositoryError> {
        let rows = sqlx::query(
            "SELECT *
             FROM jobs
             WHERE status = 'queued'
             ORDER BY created_at ASC
             LIMIT ?",
        )
        .bind(limit as i64)
        .fetch_all(&self.pool)
        .await
        .map_err(db_err)?;

        rows.iter().map(map_job).collect()
    }

    pub async fn list_jobs_by_project(
        &self,
        project_id: ProjectId,
    ) -> Result<Vec<Job>, RepositoryError> {
        let rows = sqlx::query(
            "SELECT *
             FROM jobs
             WHERE project_id = ?
             ORDER BY created_at DESC",
        )
        .bind(project_id)
        .fetch_all(&self.pool)
        .await
        .map_err(db_err)?;

        rows.iter().map(map_job).collect()
    }

    pub async fn list_active_jobs(&self) -> Result<Vec<Job>, RepositoryError> {
        let rows = sqlx::query(
            "SELECT *
             FROM jobs
             WHERE status IN ('queued', 'assigned', 'running')
             ORDER BY created_at ASC",
        )
        .fetch_all(&self.pool)
        .await
        .map_err(db_err)?;

        rows.iter().map(map_job).collect()
    }

    pub async fn get_job(&self, job_id: JobId) -> Result<Job, RepositoryError> {
        let row = sqlx::query("SELECT * FROM jobs WHERE id = ?")
            .bind(job_id)
            .fetch_optional(&self.pool)
            .await
            .map_err(db_err)?;

        row.as_ref()
            .map(map_job)
            .transpose()?
            .ok_or(RepositoryError::NotFound)
    }

    pub async fn list_jobs_by_revision(
        &self,
        revision_id: ItemRevisionId,
    ) -> Result<Vec<Job>, RepositoryError> {
        let rows =
            sqlx::query("SELECT * FROM jobs WHERE item_revision_id = ? ORDER BY created_at DESC")
                .bind(revision_id)
                .fetch_all(&self.pool)
                .await
                .map_err(db_err)?;

        rows.iter().map(map_job).collect()
    }

    pub async fn find_active_job_for_revision(
        &self,
        revision_id: ItemRevisionId,
    ) -> Result<Option<Job>, RepositoryError> {
        let row = sqlx::query(
            "SELECT *
             FROM jobs
             WHERE item_revision_id = ?
               AND status IN ('queued', 'assigned', 'running')
             ORDER BY created_at DESC
             LIMIT 1",
        )
        .bind(revision_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(db_err)?;

        row.as_ref().map(map_job).transpose()
    }
}
