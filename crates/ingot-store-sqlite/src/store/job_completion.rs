use chrono::Utc;
use ingot_domain::commit_oid::CommitOid;
use ingot_domain::git_ref::GitRef;
use ingot_domain::ids::JobId;
use ingot_domain::item::Escalation;
use ingot_domain::job::JobStatus;
use ingot_domain::ports::{
    CompletedJobCompletion, ConflictKind, JobCompletionContext, JobCompletionMutation,
    JobCompletionRepository, RepositoryError,
};
use sqlx::Row;
use sqlx::{Sqlite, Transaction};

use super::finding::upsert_finding;
use super::helpers::{db_err, item_revision_is_stale, serialize_optional_json};
use crate::db::Database;

impl Database {
    pub async fn load_job_completion_context(
        &self,
        job_id: JobId,
    ) -> Result<JobCompletionContext, RepositoryError> {
        let job = self.get_job(job_id).await?;
        let item = self.get_item(job.item_id).await?;
        let project = self.get_project(item.project_id).await?;
        let revision = self.get_revision(item.current_revision_id).await?;
        let convergences = self.list_convergences_by_item(item.id).await?;

        Ok(JobCompletionContext {
            job,
            item,
            project,
            revision,
            convergences,
        })
    }

    pub async fn load_completed_job_completion(
        &self,
        job_id: JobId,
    ) -> Result<Option<CompletedJobCompletion>, RepositoryError> {
        let job = self.get_job(job_id).await?;
        if job.state.status() != JobStatus::Completed {
            return Ok(None);
        }

        let finding_count: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM findings WHERE source_job_id = ?")
                .bind(job_id)
                .fetch_one(&self.pool)
                .await
                .map_err(db_err)?;

        Ok(Some(CompletedJobCompletion {
            job,
            finding_count: finding_count
                .try_into()
                .expect("finding count should fit into usize"),
        }))
    }

    pub async fn apply_job_completion(
        &self,
        mutation: JobCompletionMutation,
    ) -> Result<(), RepositoryError> {
        let mut tx = self.pool.begin().await.map_err(db_err)?;
        let serialized_result_payload = serialize_optional_json(mutation.result_payload.as_ref())?;

        let result = if let Some(prepared_convergence_guard) =
            mutation.prepared_convergence_guard.as_ref()
        {
            sqlx::query(
                "UPDATE jobs
                 SET status = 'completed',
                     outcome_class = ?,
                     result_schema_version = ?,
                     result_payload = ?,
                     output_commit_oid = ?,
                     ended_at = ?
                 WHERE id = ?
                   AND status IN ('queued', 'assigned', 'running')
                   AND EXISTS (
                       SELECT 1
                       FROM items
                       WHERE id = ?
                         AND current_revision_id = ?
                   )
                   AND EXISTS (
                       SELECT 1
                       FROM convergences
                       WHERE id = ?
                         AND item_revision_id = ?
                         AND status = 'prepared'
                         AND target_ref = ?
                         AND input_target_commit_oid = ?
                   )",
            )
            .bind(mutation.outcome_class)
            .bind(mutation.result_schema_version.as_deref())
            .bind(&serialized_result_payload)
            .bind(mutation.output_commit_oid.clone())
            .bind(Utc::now())
            .bind(mutation.job_id)
            .bind(mutation.item_id)
            .bind(mutation.expected_item_revision_id)
            .bind(prepared_convergence_guard.convergence_id)
            .bind(prepared_convergence_guard.item_revision_id)
            .bind(&prepared_convergence_guard.target_ref)
            .bind(prepared_convergence_guard.expected_target_head_oid.clone())
            .execute(&mut *tx)
            .await
            .map_err(db_err)?
        } else {
            sqlx::query(
                "UPDATE jobs
                 SET status = 'completed',
                     outcome_class = ?,
                     result_schema_version = ?,
                     result_payload = ?,
                     output_commit_oid = ?,
                     ended_at = ?
                 WHERE id = ?
                   AND status IN ('queued', 'assigned', 'running')
                   AND EXISTS (
                       SELECT 1
                       FROM items
                       WHERE id = ?
                         AND current_revision_id = ?
                   )",
            )
            .bind(mutation.outcome_class)
            .bind(mutation.result_schema_version.as_deref())
            .bind(&serialized_result_payload)
            .bind(mutation.output_commit_oid.clone())
            .bind(Utc::now())
            .bind(mutation.job_id)
            .bind(mutation.item_id)
            .bind(mutation.expected_item_revision_id)
            .execute(&mut *tx)
            .await
            .map_err(db_err)?
        };

        if result.rows_affected() != 1 {
            return Err(classify_job_completion_conflict(&mut tx, &mutation).await?);
        }

        for finding in &mutation.findings {
            upsert_finding(&mut tx, finding).await?;
        }

        if mutation.clear_item_escalation {
            let escalation = sqlx::query(
                "UPDATE items
                 SET escalation_state = ?, escalation_reason = NULL, updated_at = ?
                 WHERE id = ?
                   AND current_revision_id = ?",
            )
            .bind(Escalation::None.as_db_str())
            .bind(Utc::now())
            .bind(mutation.item_id)
            .bind(mutation.expected_item_revision_id)
            .execute(&mut *tx)
            .await
            .map_err(db_err)?;

            if escalation.rows_affected() != 1 {
                return Err(RepositoryError::Conflict(ConflictKind::JobRevisionStale));
            }
        }

        if let Some(prepared_convergence_guard) = mutation.prepared_convergence_guard.as_ref() {
            if let Some(approval_state) = prepared_convergence_guard.next_approval_state.as_ref() {
                let approval = sqlx::query(
                    "UPDATE items
                     SET approval_state = ?, updated_at = ?
                     WHERE id = ?
                       AND current_revision_id = ?",
                )
                .bind(*approval_state)
                .bind(Utc::now())
                .bind(mutation.item_id)
                .bind(mutation.expected_item_revision_id)
                .execute(&mut *tx)
                .await
                .map_err(db_err)?;

                if approval.rows_affected() != 1 {
                    return Err(RepositoryError::Conflict(ConflictKind::JobRevisionStale));
                }
            }
        }

        tx.commit().await.map_err(db_err)?;
        Ok(())
    }
}

impl JobCompletionRepository for Database {
    async fn load_job_completion_context(
        &self,
        job_id: JobId,
    ) -> Result<JobCompletionContext, RepositoryError> {
        Database::load_job_completion_context(self, job_id).await
    }

    async fn load_completed_job_completion(
        &self,
        job_id: JobId,
    ) -> Result<Option<CompletedJobCompletion>, RepositoryError> {
        Database::load_completed_job_completion(self, job_id).await
    }

    async fn apply_job_completion(
        &self,
        mutation: JobCompletionMutation,
    ) -> Result<(), RepositoryError> {
        Database::apply_job_completion(self, mutation).await
    }
}

async fn classify_job_completion_conflict(
    tx: &mut Transaction<'_, Sqlite>,
    mutation: &JobCompletionMutation,
) -> Result<RepositoryError, RepositoryError> {
    if item_revision_is_stale(tx, mutation.item_id, mutation.expected_item_revision_id).await? {
        return Ok(RepositoryError::Conflict(ConflictKind::JobRevisionStale));
    }

    if let Some(prepared_convergence_guard) = mutation.prepared_convergence_guard.as_ref() {
        let prepared_convergence = sqlx::query(
            "SELECT id, target_ref, input_target_commit_oid
             FROM convergences
             WHERE item_revision_id = ?
               AND status = 'prepared'
             ORDER BY created_at DESC
             LIMIT 1",
        )
        .bind(prepared_convergence_guard.item_revision_id)
        .fetch_optional(&mut **tx)
        .await
        .map_err(db_err)?;

        let Some(prepared_convergence) = prepared_convergence else {
            return Ok(RepositoryError::Conflict(
                ConflictKind::PreparedConvergenceMissing,
            ));
        };

        let prepared_convergence_id: ingot_domain::ids::ConvergenceId =
            prepared_convergence.try_get("id").map_err(db_err)?;
        let prepared_target_ref: GitRef =
            prepared_convergence.try_get("target_ref").map_err(db_err)?;
        let input_target_commit_oid: Option<CommitOid> = prepared_convergence
            .try_get("input_target_commit_oid")
            .map_err(db_err)?;
        if prepared_convergence_id != prepared_convergence_guard.convergence_id
            || prepared_target_ref != prepared_convergence_guard.target_ref
            || input_target_commit_oid.as_ref()
                != Some(&prepared_convergence_guard.expected_target_head_oid)
        {
            return Ok(RepositoryError::Conflict(
                ConflictKind::PreparedConvergenceStale,
            ));
        }
    }

    Ok(RepositoryError::Conflict(ConflictKind::JobNotActive))
}
