use chrono::Utc;
use ingot_domain::ids::{ItemId, ItemRevisionId, JobId, ProjectId};
use ingot_domain::item::EscalationState;
use ingot_domain::job::{Job, JobInput};
use ingot_domain::ports::{
    FinishJobNonSuccessParams, JobRepository, RepositoryError, StartJobExecutionParams,
};
use sqlx::Row;
use sqlx::sqlite::SqliteRow;

use super::helpers::{
    StoreDecodeError, db_err, db_write_err, encode_enum, item_revision_is_stale, parse_enum,
    parse_id, parse_json, serialize_optional_json,
};
use crate::db::Database;

fn encode_job_input(job_input: &JobInput) -> (&'static str, Option<&str>, Option<&str>) {
    match job_input {
        JobInput::None => ("none", None, None),
        JobInput::AuthoringHead { head_commit_oid } => {
            ("authoring_head", None, Some(head_commit_oid.as_str()))
        }
        JobInput::CandidateSubject {
            base_commit_oid,
            head_commit_oid,
        } => (
            "candidate_subject",
            Some(base_commit_oid.as_str()),
            Some(head_commit_oid.as_str()),
        ),
        JobInput::IntegratedSubject {
            base_commit_oid,
            head_commit_oid,
        } => (
            "integrated_subject",
            Some(base_commit_oid.as_str()),
            Some(head_commit_oid.as_str()),
        ),
    }
}

fn decode_job_input(
    kind: String,
    base_commit_oid: Option<String>,
    head_commit_oid: Option<String>,
) -> Result<JobInput, StoreDecodeError> {
    match kind.as_str() {
        "none" => Ok(JobInput::None),
        "authoring_head" => head_commit_oid
            .map(JobInput::authoring_head)
            .ok_or_else(|| StoreDecodeError::Json("authoring_head job_input missing head".into())),
        "candidate_subject" => match (base_commit_oid, head_commit_oid) {
            (Some(base_commit_oid), Some(head_commit_oid)) => Ok(JobInput::candidate_subject(
                base_commit_oid,
                head_commit_oid,
            )),
            _ => Err(StoreDecodeError::Json(
                "candidate_subject job_input missing base or head".into(),
            )),
        },
        "integrated_subject" => match (base_commit_oid, head_commit_oid) {
            (Some(base_commit_oid), Some(head_commit_oid)) => Ok(JobInput::integrated_subject(
                base_commit_oid,
                head_commit_oid,
            )),
            _ => Err(StoreDecodeError::Json(
                "integrated_subject job_input missing base or head".into(),
            )),
        },
        _ => Err(StoreDecodeError::Json(format!(
            "unknown job_input_kind: {kind}"
        ))),
    }
}

impl Database {
    pub async fn list_jobs_by_item(&self, item_id: ItemId) -> Result<Vec<Job>, RepositoryError> {
        let rows = sqlx::query("SELECT * FROM jobs WHERE item_id = ? ORDER BY created_at DESC")
            .bind(item_id.to_string())
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
        .bind(project_id.to_string())
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
               AND EXISTS (
                   SELECT 1
                   FROM items
                   WHERE id = ?
                     AND current_revision_id = ?
               )",
        )
        .bind(workspace_id.map(|id| id.to_string()))
        .bind(agent_id.map(|id| id.to_string()))
        .bind(process_pid.map(i64::from))
        .bind(lease_owner_id)
        .bind(Utc::now())
        .bind(lease_expires_at)
        .bind(Utc::now())
        .bind(job_id.to_string())
        .bind(item_id.to_string())
        .bind(expected_item_revision_id.to_string())
        .execute(&self.pool)
        .await
        .map_err(db_err)?;

        if result.rows_affected() != 1 {
            return Err(classify_running_job_conflict(
                &self.pool,
                job_id,
                item_id,
                expected_item_revision_id,
                &["queued", "assigned"],
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
        lease_owner_id: &str,
        lease_expires_at: chrono::DateTime<Utc>,
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
        .bind(job_id.to_string())
        .bind(lease_owner_id)
        .bind(item_id.to_string())
        .bind(expected_item_revision_id.to_string())
        .execute(&self.pool)
        .await
        .map_err(db_err)?;

        if result.rows_affected() != 1 {
            return Err(classify_running_job_conflict(
                &self.pool,
                job_id,
                item_id,
                expected_item_revision_id,
                &["running"],
            )
            .await?);
        }

        Ok(())
    }

    pub async fn get_job(&self, job_id: JobId) -> Result<Job, RepositoryError> {
        let row = sqlx::query("SELECT * FROM jobs WHERE id = ?")
            .bind(job_id.to_string())
            .fetch_optional(&self.pool)
            .await
            .map_err(db_err)?;

        row.as_ref()
            .map(map_job)
            .transpose()?
            .ok_or(RepositoryError::NotFound)
    }

    pub async fn create_job(&self, job: &Job) -> Result<(), RepositoryError> {
        let (job_input_kind, input_base_commit_oid, input_head_commit_oid) =
            encode_job_input(&job.job_input);
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
        .bind(job.id.to_string())
        .bind(job.project_id.to_string())
        .bind(job.item_id.to_string())
        .bind(job.item_revision_id.to_string())
        .bind(&job.step_id)
        .bind(job.semantic_attempt_no as i64)
        .bind(job.retry_no as i64)
        .bind(job.supersedes_job_id.map(|id| id.to_string()))
        .bind(encode_enum(&job.status)?)
        .bind(job.outcome_class.as_ref().map(encode_enum).transpose()?)
        .bind(encode_enum(&job.phase_kind)?)
        .bind(job.workspace_id.map(|id| id.to_string()))
        .bind(encode_enum(&job.workspace_kind)?)
        .bind(encode_enum(&job.execution_permission)?)
        .bind(encode_enum(&job.context_policy)?)
        .bind(&job.phase_template_slug)
        .bind(job.phase_template_digest.as_deref())
        .bind(job.prompt_snapshot.as_deref())
        .bind(job_input_kind)
        .bind(input_base_commit_oid)
        .bind(input_head_commit_oid)
        .bind(encode_enum(&job.output_artifact_kind)?)
        .bind(job.output_commit_oid.as_deref())
        .bind(job.result_schema_version.as_deref())
        .bind(serialize_optional_json(job.result_payload.as_ref())?)
        .bind(job.agent_id.map(|id| id.to_string()))
        .bind(job.process_pid.map(i64::from))
        .bind(job.lease_owner_id.as_deref())
        .bind(job.heartbeat_at)
        .bind(job.lease_expires_at)
        .bind(job.error_code.as_deref())
        .bind(job.error_message.as_deref())
        .bind(job.created_at)
        .bind(job.started_at)
        .bind(job.ended_at)
        .execute(&self.pool)
        .await
        .map_err(db_err)?;

        Ok(())
    }

    pub async fn update_job(&self, job: &Job) -> Result<(), RepositoryError> {
        let (job_input_kind, input_base_commit_oid, input_head_commit_oid) =
            encode_job_input(&job.job_input);
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
        .bind(&job.step_id)
        .bind(job.semantic_attempt_no as i64)
        .bind(job.retry_no as i64)
        .bind(job.supersedes_job_id.map(|id| id.to_string()))
        .bind(encode_enum(&job.status)?)
        .bind(job.outcome_class.as_ref().map(encode_enum).transpose()?)
        .bind(encode_enum(&job.phase_kind)?)
        .bind(job.workspace_id.map(|id| id.to_string()))
        .bind(encode_enum(&job.workspace_kind)?)
        .bind(encode_enum(&job.execution_permission)?)
        .bind(encode_enum(&job.context_policy)?)
        .bind(&job.phase_template_slug)
        .bind(job.phase_template_digest.as_deref())
        .bind(job.prompt_snapshot.as_deref())
        .bind(job_input_kind)
        .bind(input_base_commit_oid)
        .bind(input_head_commit_oid)
        .bind(encode_enum(&job.output_artifact_kind)?)
        .bind(job.output_commit_oid.as_deref())
        .bind(job.result_schema_version.as_deref())
        .bind(serialize_optional_json(job.result_payload.as_ref())?)
        .bind(job.agent_id.map(|id| id.to_string()))
        .bind(job.process_pid.map(i64::from))
        .bind(job.lease_owner_id.as_deref())
        .bind(job.heartbeat_at)
        .bind(job.lease_expires_at)
        .bind(job.error_code.as_deref())
        .bind(job.error_message.as_deref())
        .bind(job.created_at)
        .bind(job.started_at)
        .bind(job.ended_at)
        .bind(job.id.to_string())
        .execute(&self.pool)
        .await
        .map_err(db_write_err)?;

        if result.rows_affected() == 0 {
            return Err(RepositoryError::NotFound);
        }

        Ok(())
    }

    pub async fn list_jobs_by_revision(
        &self,
        revision_id: ItemRevisionId,
    ) -> Result<Vec<Job>, RepositoryError> {
        let rows =
            sqlx::query("SELECT * FROM jobs WHERE item_revision_id = ? ORDER BY created_at DESC")
                .bind(revision_id.to_string())
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
        .bind(revision_id.to_string())
        .fetch_optional(&self.pool)
        .await
        .map_err(db_err)?;

        row.as_ref().map(map_job).transpose()
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
        .bind(encode_enum(&status)?)
        .bind(outcome_class.as_ref().map(encode_enum).transpose()?)
        .bind(error_code)
        .bind(error_message)
        .bind(Utc::now())
        .bind(job_id.to_string())
        .bind(item_id.to_string())
        .bind(expected_item_revision_id.to_string())
        .execute(&mut *tx)
        .await
        .map_err(db_err)?;

        if result.rows_affected() != 1 {
            return Err(classify_terminal_job_conflict(
                &mut tx,
                job_id,
                item_id,
                expected_item_revision_id,
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
            .bind(encode_enum(&EscalationState::OperatorRequired)?)
            .bind(encode_enum(&escalation_reason)?)
            .bind(Utc::now())
            .bind(item_id.to_string())
            .bind(expected_item_revision_id.to_string())
            .execute(&mut *tx)
            .await
            .map_err(db_err)?;

            if escalation.rows_affected() != 1 {
                return Err(RepositoryError::Conflict("job_revision_stale".into()));
            }
        }

        tx.commit().await.map_err(db_err)?;

        Ok(())
    }
}

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
        lease_owner_id: &str,
        lease_expires_at: chrono::DateTime<Utc>,
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
}

async fn classify_running_job_conflict(
    pool: &sqlx::SqlitePool,
    job_id: JobId,
    item_id: ItemId,
    expected_item_revision_id: ItemRevisionId,
    expected_statuses: &[&str],
) -> Result<RepositoryError, RepositoryError> {
    let mut tx = pool.begin().await.map_err(db_err)?;

    if item_revision_is_stale(&mut tx, item_id, expected_item_revision_id).await? {
        return Ok(RepositoryError::Conflict("job_revision_stale".into()));
    }

    let query = format!(
        "SELECT id
         FROM jobs
         WHERE id = ?
           AND status IN ({})",
        expected_statuses
            .iter()
            .map(|_| "?")
            .collect::<Vec<_>>()
            .join(", ")
    );
    let mut query = sqlx::query_scalar::<_, String>(&query).bind(job_id.to_string());
    for status in expected_statuses {
        query = query.bind(*status);
    }

    let job_matches = query.fetch_optional(&mut *tx).await.map_err(db_err)?;
    if job_matches.is_none() {
        return Ok(RepositoryError::Conflict("job_not_active".into()));
    }

    Ok(RepositoryError::Conflict("job_update_conflict".into()))
}

async fn classify_terminal_job_conflict(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    job_id: JobId,
    item_id: ItemId,
    expected_item_revision_id: ItemRevisionId,
) -> Result<RepositoryError, RepositoryError> {
    if item_revision_is_stale(tx, item_id, expected_item_revision_id).await? {
        return Ok(RepositoryError::Conflict("job_revision_stale".into()));
    }

    let job_is_active: Option<String> = sqlx::query_scalar(
        "SELECT id
         FROM jobs
         WHERE id = ?
           AND status IN ('queued', 'assigned', 'running')",
    )
    .bind(job_id.to_string())
    .fetch_optional(&mut **tx)
    .await
    .map_err(db_err)?;

    if job_is_active.is_none() {
        return Ok(RepositoryError::Conflict("job_not_active".into()));
    }

    Ok(RepositoryError::Conflict("job_update_conflict".into()))
}

fn map_job(row: &SqliteRow) -> Result<Job, RepositoryError> {
    Ok(Job {
        id: parse_id(row.try_get("id").map_err(db_err)?)?,
        project_id: parse_id(row.try_get("project_id").map_err(db_err)?)?,
        item_id: parse_id(row.try_get("item_id").map_err(db_err)?)?,
        item_revision_id: parse_id(row.try_get("item_revision_id").map_err(db_err)?)?,
        step_id: row.try_get("step_id").map_err(db_err)?,
        semantic_attempt_no: row
            .try_get::<i64, _>("semantic_attempt_no")
            .map_err(db_err)? as u32,
        retry_no: row.try_get::<i64, _>("retry_no").map_err(db_err)? as u32,
        supersedes_job_id: row
            .try_get::<Option<String>, _>("supersedes_job_id")
            .map_err(db_err)?
            .map(parse_id)
            .transpose()?,
        status: parse_enum(row.try_get("status").map_err(db_err)?)?,
        outcome_class: row
            .try_get::<Option<String>, _>("outcome_class")
            .map_err(db_err)?
            .map(parse_enum)
            .transpose()?,
        phase_kind: parse_enum(row.try_get("phase_kind").map_err(db_err)?)?,
        workspace_id: row
            .try_get::<Option<String>, _>("workspace_id")
            .map_err(db_err)?
            .map(parse_id)
            .transpose()?,
        workspace_kind: parse_enum(row.try_get("workspace_kind").map_err(db_err)?)?,
        execution_permission: parse_enum(row.try_get("execution_permission").map_err(db_err)?)?,
        context_policy: parse_enum(row.try_get("context_policy").map_err(db_err)?)?,
        phase_template_slug: row.try_get("phase_template_slug").map_err(db_err)?,
        phase_template_digest: row.try_get("phase_template_digest").map_err(db_err)?,
        prompt_snapshot: row.try_get("prompt_snapshot").map_err(db_err)?,
        job_input: decode_job_input(
            row.try_get("job_input_kind").map_err(db_err)?,
            row.try_get("input_base_commit_oid").map_err(db_err)?,
            row.try_get("input_head_commit_oid").map_err(db_err)?,
        )
        .map_err(|error| RepositoryError::Database(Box::new(error)))?,
        output_artifact_kind: parse_enum(row.try_get("output_artifact_kind").map_err(db_err)?)?,
        output_commit_oid: row.try_get("output_commit_oid").map_err(db_err)?,
        result_schema_version: row.try_get("result_schema_version").map_err(db_err)?,
        result_payload: row
            .try_get::<Option<String>, _>("result_payload")
            .map_err(db_err)?
            .map(parse_json)
            .transpose()?,
        agent_id: row
            .try_get::<Option<String>, _>("agent_id")
            .map_err(db_err)?
            .map(parse_id)
            .transpose()?,
        process_pid: row
            .try_get::<Option<i64>, _>("process_pid")
            .map_err(db_err)?
            .map(|value| value as u32),
        lease_owner_id: row.try_get("lease_owner_id").map_err(db_err)?,
        heartbeat_at: row.try_get("heartbeat_at").map_err(db_err)?,
        lease_expires_at: row.try_get("lease_expires_at").map_err(db_err)?,
        error_code: row.try_get("error_code").map_err(db_err)?,
        error_message: row.try_get("error_message").map_err(db_err)?,
        created_at: row.try_get("created_at").map_err(db_err)?,
        started_at: row.try_get("started_at").map_err(db_err)?,
        ended_at: row.try_get("ended_at").map_err(db_err)?,
    })
}

#[cfg(test)]
mod tests {
    use ingot_domain::ids::{ItemId, ItemRevisionId};
    use ingot_domain::item::EscalationReason;
    use ingot_domain::job::{
        ContextPolicy, ExecutionPermission, JobStatus, OutcomeClass, OutputArtifactKind, PhaseKind,
    };
    use ingot_domain::ports::RepositoryError;
    use ingot_domain::workspace::WorkspaceKind;
    use ingot_test_support::fixtures::{
        ItemBuilder, JobBuilder, ProjectBuilder, RevisionBuilder, parse_timestamp,
    };
    use ingot_test_support::sqlite::temp_db_path;

    use crate::Database;
    use crate::FinishJobNonSuccessParams;
    use crate::store::test_fixtures::PersistFixture;

    async fn migrated_test_db(prefix: &str) -> Database {
        let path = temp_db_path(prefix);
        let db = Database::connect(&path).await.expect("connect db");
        db.migrate().await.expect("migrate db");
        db
    }

    #[tokio::test]
    async fn finish_job_non_success_rolls_back_when_item_revision_changes_before_commit() {
        let db = migrated_test_db("ingot-store").await;

        let project = ProjectBuilder::new("/tmp/test")
            .name("Test")
            .build()
            .persist(&db)
            .await
            .expect("create project");

        let item_id = ItemId::new();
        let revision = RevisionBuilder::new(item_id)
            .seed_commit_oid(Some("abc"))
            .seed_target_commit_oid(Some("def"))
            .build();
        let mut next_revision = RevisionBuilder::new(item_id)
            .id(ItemRevisionId::new())
            .revision_no(2)
            .seed_commit_oid(Some("ghi"))
            .seed_target_commit_oid(Some("jkl"))
            .created_at(parse_timestamp("2026-03-13T00:00:00Z"))
            .build();
        next_revision.supersedes_revision_id = Some(revision.id);

        let item = ItemBuilder::new(project.id, next_revision.id)
            .id(item_id)
            .build();
        let (item, revision) = (item, revision)
            .persist(&db)
            .await
            .expect("create item with source revision");
        let next_revision = next_revision
            .persist(&db)
            .await
            .expect("create next revision");

        let job = JobBuilder::new(project.id, item.id, revision.id, "repair_candidate")
            .status(JobStatus::Running)
            .phase_kind(PhaseKind::Author)
            .workspace_kind(WorkspaceKind::Authoring)
            .execution_permission(ExecutionPermission::MayMutate)
            .context_policy(ContextPolicy::ResumeContext)
            .phase_template_slug("repair-candidate")
            .output_artifact_kind(OutputArtifactKind::Commit)
            .build()
            .persist(&db)
            .await
            .expect("create job");

        let error = db
            .finish_job_non_success(FinishJobNonSuccessParams {
                job_id: job.id,
                item_id: item.id,
                expected_item_revision_id: revision.id,
                status: JobStatus::Failed,
                outcome_class: Some(OutcomeClass::TerminalFailure),
                error_code: Some("worker_failed".into()),
                error_message: Some("boom".into()),
                escalation_reason: Some(EscalationReason::StepFailed),
            })
            .await
            .expect_err("revision drift should fail");

        assert!(matches!(
            error,
            RepositoryError::Conflict(message) if message == "job_revision_stale"
        ));

        let persisted_job = db.get_job(job.id).await.expect("load job after rollback");
        let persisted_item = db
            .get_item(item.id)
            .await
            .expect("load item after rollback");

        assert_eq!(next_revision.id, persisted_item.current_revision_id);
        assert_eq!(persisted_job.status, JobStatus::Running);
        assert_eq!(
            persisted_item.escalation_state,
            ingot_domain::item::EscalationState::None
        );
    }
}
