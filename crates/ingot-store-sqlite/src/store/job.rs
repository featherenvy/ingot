use chrono::{DateTime, Utc};
use ingot_domain::commit_oid::CommitOid;
use ingot_domain::ids::{AgentId, ItemId, ItemRevisionId, JobId, ProjectId, WorkspaceId};
use ingot_domain::item::Escalation;
use ingot_domain::job::{Job, JobAssignment, JobInput, JobLease, JobState, TerminalStatus};
use ingot_domain::lease_owner_id::LeaseOwnerId;
use ingot_domain::ports::{
    FinishJobNonSuccessParams, JobRepository, RepositoryError, StartJobExecutionParams,
};
use sqlx::Row;
use sqlx::sqlite::SqliteRow;

use super::helpers::{
    StoreDecodeError, db_err, db_write_err, item_revision_is_stale, parse_json,
    serialize_optional_json,
};
use crate::db::Database;

#[derive(Debug, Clone)]
pub struct ClaimQueuedAgentJobExecutionParams {
    pub job_id: JobId,
    pub item_id: ItemId,
    pub expected_item_revision_id: ItemRevisionId,
    pub assignment: JobAssignment,
    pub lease_owner_id: LeaseOwnerId,
    pub lease_expires_at: DateTime<Utc>,
}

fn encode_job_input(job_input: &JobInput) -> (&'static str, Option<CommitOid>, Option<CommitOid>) {
    match job_input {
        JobInput::None => ("none", None, None),
        JobInput::AuthoringHead { head_commit_oid } => {
            ("authoring_head", None, Some(head_commit_oid.clone()))
        }
        JobInput::CandidateSubject {
            base_commit_oid,
            head_commit_oid,
        } => (
            "candidate_subject",
            Some(base_commit_oid.clone()),
            Some(head_commit_oid.clone()),
        ),
        JobInput::IntegratedSubject {
            base_commit_oid,
            head_commit_oid,
        } => (
            "integrated_subject",
            Some(base_commit_oid.clone()),
            Some(head_commit_oid.clone()),
        ),
    }
}

fn decode_job_input(
    kind: String,
    base_commit_oid: Option<CommitOid>,
    head_commit_oid: Option<CommitOid>,
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
            return Err(classify_running_job_conflict(
                &self.pool,
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
            return Err(classify_running_job_conflict(
                &self.pool,
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
        .bind(job_id)
        .bind(lease_owner_id)
        .bind(item_id)
        .bind(expected_item_revision_id)
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
                true,
            )
            .await?);
        }

        Ok(())
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
        lease_owner_id: &LeaseOwnerId,
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
    require_workspace_binding: bool,
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
    let mut query = sqlx::query_scalar::<_, JobId>(&query).bind(job_id);
    for status in expected_statuses {
        query = query.bind(*status);
    }

    let job_matches = query.fetch_optional(&mut *tx).await.map_err(db_err)?;
    if job_matches.is_none() {
        return Ok(RepositoryError::Conflict("job_not_active".into()));
    }

    if require_workspace_binding {
        let workspace_id: Option<ingot_domain::ids::WorkspaceId> = sqlx::query_scalar(
            "SELECT workspace_id
             FROM jobs
             WHERE id = ?",
        )
        .bind(job_id)
        .fetch_optional(&mut *tx)
        .await
        .map_err(db_err)?
        .flatten();
        if workspace_id.is_none() {
            return Ok(RepositoryError::Conflict("job_missing_workspace".into()));
        }
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

    let job_is_active: Option<JobId> = sqlx::query_scalar(
        "SELECT id
         FROM jobs
         WHERE id = ?
           AND status IN ('queued', 'assigned', 'running')",
    )
    .bind(job_id)
    .fetch_optional(&mut **tx)
    .await
    .map_err(db_err)?;

    if job_is_active.is_none() {
        return Ok(RepositoryError::Conflict("job_not_active".into()));
    }

    Ok(RepositoryError::Conflict("job_update_conflict".into()))
}

fn map_job(row: &SqliteRow) -> Result<Job, RepositoryError> {
    use ingot_domain::job::{JobStatus, OutcomeClass};

    // Extract flat columns
    let status: JobStatus = row.try_get("status").map_err(db_err)?;
    let outcome_class: Option<OutcomeClass> = row.try_get("outcome_class").map_err(db_err)?;
    let workspace_id: Option<WorkspaceId> = row.try_get("workspace_id").map_err(db_err)?;
    let agent_id: Option<AgentId> = row.try_get("agent_id").map_err(db_err)?;
    let prompt_snapshot: Option<String> = row.try_get("prompt_snapshot").map_err(db_err)?;
    let phase_template_digest: Option<String> =
        row.try_get("phase_template_digest").map_err(db_err)?;
    let output_commit_oid: Option<CommitOid> = row.try_get("output_commit_oid").map_err(db_err)?;
    let result_schema_version: Option<String> =
        row.try_get("result_schema_version").map_err(db_err)?;
    let result_payload: Option<serde_json::Value> = row
        .try_get::<Option<String>, _>("result_payload")
        .map_err(db_err)?
        .map(parse_json)
        .transpose()?;
    let process_pid: Option<u32> = row
        .try_get::<Option<i64>, _>("process_pid")
        .map_err(db_err)?
        .map(|value| value as u32);
    let lease_owner_id: Option<LeaseOwnerId> = row.try_get("lease_owner_id").map_err(db_err)?;
    let heartbeat_at: Option<chrono::DateTime<chrono::Utc>> =
        row.try_get("heartbeat_at").map_err(db_err)?;
    let lease_expires_at: Option<chrono::DateTime<chrono::Utc>> =
        row.try_get("lease_expires_at").map_err(db_err)?;
    let error_code: Option<String> = row.try_get("error_code").map_err(db_err)?;
    let error_message: Option<String> = row.try_get("error_message").map_err(db_err)?;
    let started_at: Option<chrono::DateTime<chrono::Utc>> =
        row.try_get("started_at").map_err(db_err)?;
    let ended_at: Option<chrono::DateTime<chrono::Utc>> =
        row.try_get("ended_at").map_err(db_err)?;

    // Build assignment from flat fields (if workspace_id present)
    let assignment = workspace_id.map(|wid| JobAssignment {
        workspace_id: wid,
        agent_id,
        prompt_snapshot,
        phase_template_digest,
    });

    let state = match status {
        JobStatus::Queued => JobState::Queued,
        JobStatus::Assigned => JobState::Assigned(required_job_field("workspace_id", assignment)?),
        JobStatus::Running => JobState::Running {
            assignment: required_job_field("workspace_id", assignment)?,
            lease: JobLease {
                process_pid,
                lease_owner_id: required_job_field("lease_owner_id", lease_owner_id)?,
                heartbeat_at: required_job_field("heartbeat_at", heartbeat_at)?,
                lease_expires_at: required_job_field("lease_expires_at", lease_expires_at)?,
                started_at: required_job_field("started_at", started_at)?,
            },
        },
        JobStatus::Completed => JobState::Completed {
            assignment,
            started_at,
            outcome_class: required_job_field("outcome_class", outcome_class)?,
            ended_at: required_job_field("ended_at", ended_at)?,
            output_commit_oid,
            result_schema_version,
            result_payload,
        },
        JobStatus::Failed => JobState::Terminated {
            terminal_status: TerminalStatus::Failed,
            assignment,
            started_at,
            outcome_class,
            ended_at: required_job_field("ended_at", ended_at)?,
            error_code,
            error_message,
        },
        JobStatus::Cancelled => JobState::Terminated {
            terminal_status: TerminalStatus::Cancelled,
            assignment,
            started_at,
            outcome_class,
            ended_at: required_job_field("ended_at", ended_at)?,
            error_code,
            error_message,
        },
        JobStatus::Expired => JobState::Terminated {
            terminal_status: TerminalStatus::Expired,
            assignment,
            started_at,
            outcome_class,
            ended_at: required_job_field("ended_at", ended_at)?,
            error_code,
            error_message,
        },
        JobStatus::Superseded => JobState::Terminated {
            terminal_status: TerminalStatus::Superseded,
            assignment,
            started_at,
            outcome_class,
            ended_at: required_job_field("ended_at", ended_at)?,
            error_code,
            error_message,
        },
    };

    Ok(Job {
        id: row.try_get("id").map_err(db_err)?,
        project_id: row.try_get("project_id").map_err(db_err)?,
        item_id: row.try_get("item_id").map_err(db_err)?,
        item_revision_id: row.try_get("item_revision_id").map_err(db_err)?,
        step_id: row.try_get("step_id").map_err(db_err)?,
        semantic_attempt_no: row
            .try_get::<i64, _>("semantic_attempt_no")
            .map_err(db_err)? as u32,
        retry_no: row.try_get::<i64, _>("retry_no").map_err(db_err)? as u32,
        supersedes_job_id: row.try_get("supersedes_job_id").map_err(db_err)?,
        phase_kind: row.try_get("phase_kind").map_err(db_err)?,
        workspace_kind: row.try_get("workspace_kind").map_err(db_err)?,
        execution_permission: row.try_get("execution_permission").map_err(db_err)?,
        context_policy: row.try_get("context_policy").map_err(db_err)?,
        phase_template_slug: row.try_get("phase_template_slug").map_err(db_err)?,
        job_input: decode_job_input(
            row.try_get("job_input_kind").map_err(db_err)?,
            row.try_get("input_base_commit_oid").map_err(db_err)?,
            row.try_get("input_head_commit_oid").map_err(db_err)?,
        )
        .map_err(|error| RepositoryError::Database(Box::new(error)))?,
        output_artifact_kind: row.try_get("output_artifact_kind").map_err(db_err)?,
        created_at: row.try_get("created_at").map_err(db_err)?,
        state,
    })
}

fn required_job_field<T>(field: &'static str, value: Option<T>) -> Result<T, RepositoryError> {
    value.ok_or_else(|| {
        RepositoryError::Database(format!("job {field} is required for this status").into())
    })
}

#[cfg(test)]
mod tests {
    use chrono::Utc;
    use ingot_domain::ids::{ItemId, JobId};
    use ingot_domain::ports::RepositoryError;
    use ingot_test_support::fixtures::{ItemBuilder, ProjectBuilder, RevisionBuilder};
    use ingot_test_support::sqlite::temp_db_path;

    use crate::db::Database;
    use crate::store::PersistFixture;

    #[tokio::test]
    async fn get_job_rejects_assigned_rows_without_workspace_id() {
        let path = temp_db_path("ingot-store-job");
        let db = Database::connect(&path).await.expect("connect db");
        db.migrate().await.expect("migrate db");

        let project = ProjectBuilder::new("/tmp/test")
            .name("Test")
            .build()
            .persist(&db)
            .await
            .expect("create project");
        let revision = RevisionBuilder::new(ItemId::new()).build();
        let item = ItemBuilder::new(project.id, revision.id)
            .id(revision.item_id)
            .build();
        let (item, revision) = (item, revision)
            .persist(&db)
            .await
            .expect("create item with revision");

        let job_id = JobId::new();
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
        .bind(job_id)
        .bind(project.id)
        .bind(item.id)
        .bind(revision.id)
        .bind("author_initial")
        .bind(1_i64)
        .bind(0_i64)
        .bind(Option::<String>::None)
        .bind("assigned")
        .bind(Option::<String>::None)
        .bind("author")
        .bind(Option::<String>::None)
        .bind("authoring")
        .bind("may_mutate")
        .bind("fresh")
        .bind("template")
        .bind(Option::<String>::None)
        .bind(Option::<String>::None)
        .bind("none")
        .bind(Option::<String>::None)
        .bind(Option::<String>::None)
        .bind("none")
        .bind(Option::<String>::None)
        .bind(Option::<String>::None)
        .bind(Option::<String>::None)
        .bind(Option::<String>::None)
        .bind(Option::<i64>::None)
        .bind(Option::<String>::None)
        .bind(Option::<chrono::DateTime<Utc>>::None)
        .bind(Option::<chrono::DateTime<Utc>>::None)
        .bind(Option::<String>::None)
        .bind(Option::<String>::None)
        .bind(Utc::now())
        .bind(Option::<chrono::DateTime<Utc>>::None)
        .bind(Option::<chrono::DateTime<Utc>>::None)
        .execute(&db.pool)
        .await
        .expect("insert malformed assigned job");

        let error = db.get_job(job_id).await.expect_err("missing workspace_id");
        assert!(matches!(error, RepositoryError::Database(_)));
    }
}
