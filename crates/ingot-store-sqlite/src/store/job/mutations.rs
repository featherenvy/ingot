use chrono::{DateTime, Utc};
use ingot_domain::ids::{ItemId, ItemRevisionId, JobId};
use ingot_domain::item::Escalation;
use ingot_domain::job::Job;
use ingot_domain::lease_owner_id::LeaseOwnerId;
use ingot_domain::ports::{
    ConflictKind, FinishJobNonSuccessParams, RepositoryError, StartJobExecutionParams,
};

use super::ClaimQueuedAgentJobExecutionParams;
use super::conflict::classify_job_conflict;
use super::mapping::encode_job_input;
use crate::db::Database;
use crate::store::helpers::{db_err, db_write_err, serialize_optional_json};

impl Database {
    pub async fn start_job_execution(
        &self,
        params: StartJobExecutionParams,
    ) -> Result<(), RepositoryError> {
        let StartJobExecutionParams {
            job_id,
            item_id,
            expected_item_revision_id,
            workspace_id,
            agent_id,
            lease_owner_id,
            process_pid,
            lease_expires_at,
        } = params;
        let result = sqlx::query(
            "UPDATE jobs
             SET status = 'running',
                 workspace_id = COALESCE(?, workspace_id),
                 agent_id = COALESCE(?, agent_id),
                 process_pid = ?,
                 lease_owner_id = ?,
                 heartbeat_at = ?,
                 lease_expires_at = ?,
                 started_at = COALESCE(started_at, ?)
             WHERE id = ?
               AND status IN ('queued', 'assigned')
               AND COALESCE(?, workspace_id) IS NOT NULL
               AND EXISTS (
                   SELECT 1
                   FROM items
                   WHERE id = ?
                     AND current_revision_id = ?
               )",
        )
        .bind(workspace_id)
        .bind(agent_id)
        .bind(process_pid.map(i64::from))
        .bind(lease_owner_id)
        .bind(Utc::now())
        .bind(lease_expires_at)
        .bind(Utc::now())
        .bind(job_id)
        .bind(workspace_id)
        .bind(item_id)
        .bind(expected_item_revision_id)
        .execute(&self.pool)
        .await
        .map_err(db_err)?;

        if result.rows_affected() != 1 {
            let mut tx = self.pool.begin().await.map_err(db_err)?;
            return Err(classify_job_conflict(
                &mut tx,
                job_id,
                item_id,
                expected_item_revision_id,
                &["queued", "assigned"],
                true,
            )
            .await?);
        }

        Ok(())
    }

    pub async fn claim_queued_agent_job_execution(
        &self,
        params: ClaimQueuedAgentJobExecutionParams,
    ) -> Result<(), RepositoryError> {
        let ClaimQueuedAgentJobExecutionParams {
            job_id,
            item_id,
            expected_item_revision_id,
            assignment,
            lease_owner_id,
            lease_expires_at,
        } = params;
        let now = Utc::now();
        let result = sqlx::query(
            "UPDATE jobs
             SET status = 'running',
                 workspace_id = ?,
                 agent_id = ?,
                 prompt_snapshot = ?,
                 phase_template_digest = ?,
                 process_pid = NULL,
                 lease_owner_id = ?,
                 heartbeat_at = ?,
                 lease_expires_at = ?,
                 started_at = COALESCE(started_at, ?)
             WHERE id = ?
               AND status = 'queued'
               AND EXISTS (
                   SELECT 1
                   FROM items
                   WHERE id = ?
                     AND current_revision_id = ?
               )",
        )
        .bind(assignment.workspace_id)
        .bind(assignment.agent_id)
        .bind(assignment.prompt_snapshot)
        .bind(assignment.phase_template_digest)
        .bind(lease_owner_id)
        .bind(now)
        .bind(lease_expires_at)
        .bind(now)
        .bind(job_id)
        .bind(item_id)
        .bind(expected_item_revision_id)
        .execute(&self.pool)
        .await
        .map_err(db_err)?;

        if result.rows_affected() != 1 {
            let mut tx = self.pool.begin().await.map_err(db_err)?;
            return Err(classify_job_conflict(
                &mut tx,
                job_id,
                item_id,
                expected_item_revision_id,
                &["queued"],
                false,
            )
            .await?);
        }

        Ok(())
    }

    pub async fn heartbeat_job_execution(
        &self,
        job_id: JobId,
        item_id: ItemId,
        expected_item_revision_id: ItemRevisionId,
        lease_owner_id: &LeaseOwnerId,
        lease_expires_at: DateTime<Utc>,
    ) -> Result<(), RepositoryError> {
        let result = sqlx::query(
            "UPDATE jobs
             SET heartbeat_at = ?, lease_expires_at = ?
             WHERE id = ?
               AND status = 'running'
               AND lease_owner_id = ?
               AND EXISTS (
                   SELECT 1
                   FROM items
                   WHERE id = ?
                     AND current_revision_id = ?
               )",
        )
        .bind(Utc::now())
        .bind(lease_expires_at)
        .bind(job_id)
        .bind(lease_owner_id)
        .bind(item_id)
        .bind(expected_item_revision_id)
        .execute(&self.pool)
        .await
        .map_err(db_err)?;

        if result.rows_affected() != 1 {
            let mut tx = self.pool.begin().await.map_err(db_err)?;
            return Err(classify_job_conflict(
                &mut tx,
                job_id,
                item_id,
                expected_item_revision_id,
                &["running"],
                true,
            )
            .await?);
        }

        Ok(())
    }

    pub async fn create_job(&self, job: &Job) -> Result<(), RepositoryError> {
        let (job_input_kind, input_base_commit_oid, input_head_commit_oid) =
            encode_job_input(&job.job_input);
        let status = job.state.status();
        sqlx::query(
            "INSERT INTO jobs (
                id, project_id, item_id, item_revision_id, step_id, semantic_attempt_no, retry_no,
                supersedes_job_id, status, outcome_class, phase_kind, workspace_id, workspace_kind,
                execution_permission, context_policy, phase_template_slug, phase_template_digest,
                prompt_snapshot, job_input_kind, input_base_commit_oid, input_head_commit_oid,
                output_artifact_kind, output_commit_oid, result_schema_version, result_payload,
                agent_id, process_pid, lease_owner_id, heartbeat_at, lease_expires_at, error_code,
                error_message, created_at, started_at, ended_at
             ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(job.id)
        .bind(job.project_id)
        .bind(job.item_id)
        .bind(job.item_revision_id)
        .bind(job.step_id)
        .bind(job.semantic_attempt_no as i64)
        .bind(job.retry_no as i64)
        .bind(job.supersedes_job_id)
        .bind(status)
        .bind(job.state.outcome_class())
        .bind(job.phase_kind)
        .bind(job.state.workspace_id())
        .bind(job.workspace_kind)
        .bind(job.execution_permission)
        .bind(job.context_policy)
        .bind(&job.phase_template_slug)
        .bind(job.state.phase_template_digest())
        .bind(job.state.prompt_snapshot())
        .bind(job_input_kind)
        .bind(input_base_commit_oid)
        .bind(input_head_commit_oid)
        .bind(job.output_artifact_kind)
        .bind(job.state.output_commit_oid().cloned())
        .bind(job.state.result_schema_version())
        .bind(serialize_optional_json(job.state.result_payload())?)
        .bind(job.state.agent_id())
        .bind(job.state.process_pid().map(i64::from))
        .bind(job.state.lease_owner_id())
        .bind(job.state.heartbeat_at())
        .bind(job.state.lease_expires_at())
        .bind(job.state.error_code())
        .bind(job.state.error_message())
        .bind(job.created_at)
        .bind(job.state.started_at())
        .bind(job.state.ended_at())
        .execute(&self.pool)
        .await
        .map_err(db_err)?;

        Ok(())
    }

    pub async fn update_job(&self, job: &Job) -> Result<(), RepositoryError> {
        let (job_input_kind, input_base_commit_oid, input_head_commit_oid) =
            encode_job_input(&job.job_input);
        let status = job.state.status();
        let result = sqlx::query(
            "UPDATE jobs
             SET step_id = ?, semantic_attempt_no = ?, retry_no = ?, supersedes_job_id = ?, status = ?,
                 outcome_class = ?, phase_kind = ?, workspace_id = ?, workspace_kind = ?,
                 execution_permission = ?, context_policy = ?, phase_template_slug = ?,
                 phase_template_digest = ?, prompt_snapshot = ?, job_input_kind = ?,
                 input_base_commit_oid = ?, input_head_commit_oid = ?,
                 output_artifact_kind = ?, output_commit_oid = ?, result_schema_version = ?,
                 result_payload = ?, agent_id = ?, process_pid = ?, lease_owner_id = ?,
                 heartbeat_at = ?, lease_expires_at = ?, error_code = ?, error_message = ?,
                 created_at = ?, started_at = ?, ended_at = ?
             WHERE id = ?",
        )
        .bind(job.step_id)
        .bind(job.semantic_attempt_no as i64)
        .bind(job.retry_no as i64)
        .bind(job.supersedes_job_id)
        .bind(status)
        .bind(job.state.outcome_class())
        .bind(job.phase_kind)
        .bind(job.state.workspace_id())
        .bind(job.workspace_kind)
        .bind(job.execution_permission)
        .bind(job.context_policy)
        .bind(&job.phase_template_slug)
        .bind(job.state.phase_template_digest())
        .bind(job.state.prompt_snapshot())
        .bind(job_input_kind)
        .bind(input_base_commit_oid)
        .bind(input_head_commit_oid)
        .bind(job.output_artifact_kind)
        .bind(job.state.output_commit_oid().cloned())
        .bind(job.state.result_schema_version())
        .bind(serialize_optional_json(job.state.result_payload())?)
        .bind(job.state.agent_id())
        .bind(job.state.process_pid().map(i64::from))
        .bind(job.state.lease_owner_id())
        .bind(job.state.heartbeat_at())
        .bind(job.state.lease_expires_at())
        .bind(job.state.error_code())
        .bind(job.state.error_message())
        .bind(job.created_at)
        .bind(job.state.started_at())
        .bind(job.state.ended_at())
        .bind(job.id)
        .execute(&self.pool)
        .await
        .map_err(db_write_err)?;

        if result.rows_affected() == 0 {
            return Err(RepositoryError::NotFound);
        }

        Ok(())
    }

    pub async fn finish_job_non_success(
        &self,
        params: FinishJobNonSuccessParams,
    ) -> Result<(), RepositoryError> {
        let FinishJobNonSuccessParams {
            job_id,
            item_id,
            expected_item_revision_id,
            status,
            outcome_class,
            error_code,
            error_message,
            escalation_reason,
        } = params;
        let mut tx = self.pool.begin().await.map_err(db_err)?;

        let result = sqlx::query(
            "UPDATE jobs
             SET status = ?,
                 outcome_class = ?,
                 result_schema_version = NULL,
                 result_payload = NULL,
                 output_commit_oid = NULL,
                 error_code = ?,
                 error_message = ?,
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
        .bind(status)
        .bind(outcome_class)
        .bind(error_code)
        .bind(error_message)
        .bind(Utc::now())
        .bind(job_id)
        .bind(item_id)
        .bind(expected_item_revision_id)
        .execute(&mut *tx)
        .await
        .map_err(db_err)?;

        if result.rows_affected() != 1 {
            return Err(classify_job_conflict(
                &mut tx,
                job_id,
                item_id,
                expected_item_revision_id,
                &["queued", "assigned", "running"],
                false,
            )
            .await?);
        }

        if let Some(escalation_reason) = escalation_reason {
            let escalation = sqlx::query(
                "UPDATE items
                 SET escalation_state = ?, escalation_reason = ?, updated_at = ?
                 WHERE id = ?
                   AND current_revision_id = ?",
            )
            .bind(
                Escalation::OperatorRequired {
                    reason: escalation_reason,
                }
                .as_db_str(),
            )
            .bind(escalation_reason)
            .bind(Utc::now())
            .bind(item_id)
            .bind(expected_item_revision_id)
            .execute(&mut *tx)
            .await
            .map_err(db_err)?;

            if escalation.rows_affected() != 1 {
                return Err(RepositoryError::Conflict(ConflictKind::JobRevisionStale));
            }
        }

        tx.commit().await.map_err(db_err)?;

        Ok(())
    }

    pub async fn delete_job(&self, job_id: JobId) -> Result<(), RepositoryError> {
        let result = sqlx::query("DELETE FROM jobs WHERE id = ?")
            .bind(job_id)
            .execute(&self.pool)
            .await
            .map_err(db_write_err)?;

        if result.rows_affected() == 0 {
            return Err(RepositoryError::NotFound);
        }

        Ok(())
    }
}
