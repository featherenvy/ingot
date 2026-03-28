use ingot_domain::commit_oid::CommitOid;
use ingot_domain::ids::{AgentId, WorkspaceId};
use ingot_domain::job::{Job, JobAssignment, JobInput, JobLease, JobState, TerminalStatus};
use ingot_domain::lease_owner_id::LeaseOwnerId;
use ingot_domain::ports::RepositoryError;
use sqlx::Row;
use sqlx::sqlite::SqliteRow;

use crate::store::helpers::{StoreDecodeError, db_err, parse_json};

pub(super) fn encode_job_input(
    job_input: &JobInput,
) -> (&'static str, Option<CommitOid>, Option<CommitOid>) {
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

pub(super) fn map_job(row: &SqliteRow) -> Result<Job, RepositoryError> {
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
