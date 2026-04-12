use super::deps::*;
use super::dispatch::auto_dispatch_projected_review_job;
use super::items::{
    current_authoring_head_for_revision_with_workspace, effective_authoring_base_commit_oid,
};
use super::support::{
    activity::append_activity,
    errors::{complete_job_error_to_api_error, repo_to_internal, repo_to_item, repo_to_project},
    io::{read_optional_json, read_optional_json_lines, read_optional_text},
    path::ApiPath,
};
use super::types::*;
use ingot_usecases::dispatch::failure_status;
use ingot_usecases::job_lifecycle;

pub(super) fn routes() -> Router<AppState> {
    Router::new()
        .route(
            "/api/projects/{project_id}/items/{item_id}/jobs/{job_id}/cancel",
            post(cancel_item_job),
        )
        .route("/api/jobs/{job_id}/logs", get(get_job_logs))
        .route("/api/jobs/{job_id}/logs/raw", get(get_job_logs_raw))
        .route("/api/jobs/{job_id}/complete", post(complete_job))
        .route("/api/jobs/{job_id}/fail", post(fail_job))
        .route("/api/jobs/{job_id}/expire", post(expire_job))
}

pub(super) async fn cancel_item_job(
    State(state): State<AppState>,
    ApiPath(ProjectItemJobPathParams {
        project_id,
        item_id,
        job_id,
    }): ApiPath<ProjectItemJobPathParams>,
) -> Result<Json<()>, ApiError> {
    let _project = state
        .db
        .get_project(project_id)
        .await
        .map_err(repo_to_project)?;
    let _guard = state
        .project_locks
        .acquire_project_mutation(project_id)
        .await;

    let item = state.db.get_item(item_id).await.map_err(repo_to_item)?;
    if item.project_id != project_id {
        return Err(UseCaseError::ItemNotFound.into());
    }
    let job = state.db.get_job(job_id).await.map_err(repo_to_internal)?;
    if job.item_id != item.id {
        return Err(ApiError::NotFound {
            code: "job_not_found",
            message: "Job not found".into(),
        });
    }
    if job.item_revision_id != item.current_revision_id {
        return Err(UseCaseError::ProtocolViolation(
            "job cancellation does not match the current item revision".into(),
        )
        .into());
    }

    job_lifecycle::cancel_job(
        &state.db,
        &state.db,
        &state.db,
        &job,
        &item,
        "operator_cancelled",
        WorkspaceStatus::Ready,
    )
    .await?;
    refresh_revision_context_for_job(&state, job.id).await?;

    Ok(Json(()))
}

pub(super) async fn get_job_logs(
    State(state): State<AppState>,
    ApiPath(JobPathParams { job_id }): ApiPath<JobPathParams>,
) -> Result<Json<JobLogsResponse>, ApiError> {
    let job = state.db.get_job(job_id).await.map_err(repo_to_internal)?;
    let logs_dir = state.job_logs_dir(job_id);

    let prompt = read_optional_text(logs_dir.join("prompt.txt")).await?;
    let output = read_optional_json_lines(logs_dir.join("output.jsonl")).await?;
    let stdout = read_optional_text(logs_dir.join("stdout.log")).await?;
    let stderr = read_optional_text(logs_dir.join("stderr.log")).await?;
    let result = read_optional_json(logs_dir.join("result.json")).await?;

    Ok(Json(JobLogsResponse {
        prompt,
        output: job_output_document(output, stdout, stderr),
        result: result.map(|payload| ingot_agent_protocol::JobStructuredResult {
            schema_version: job.state.result_schema_version().map(ToOwned::to_owned),
            payload,
        }),
    }))
}

fn job_output_document(
    output: Option<Vec<ingot_agent_protocol::AgentOutputSegment>>,
    stdout: Option<String>,
    stderr: Option<String>,
) -> ingot_agent_protocol::AgentOutputDocument {
    let segments = match output {
        // An existing but empty output.jsonl means the live transcript artifact has been
        // initialized, but no normalized segments have been written yet. Falling back to
        // synthetic raw segments in that state can collide with the first real live updates.
        Some(segments) => segments,
        None => raw_log_fallback_segments(stdout, stderr),
    };

    ingot_agent_protocol::AgentOutputDocument {
        schema_version: ingot_agent_protocol::AgentOutputDocument::SCHEMA_VERSION.into(),
        segments,
    }
}

fn raw_log_fallback_segments(
    stdout: Option<String>,
    stderr: Option<String>,
) -> Vec<ingot_agent_protocol::AgentOutputSegment> {
    let mut segments = Vec::new();
    let mut sequence = 1;

    for (channel, artifact_name, text) in [
        (
            ingot_agent_protocol::AgentOutputChannel::Primary,
            "stdout.log",
            stdout,
        ),
        (
            ingot_agent_protocol::AgentOutputChannel::Diagnostic,
            "stderr.log",
            stderr,
        ),
    ] {
        let Some(text) = non_empty_log_artifact(text) else {
            continue;
        };

        segments.push(ingot_agent_protocol::AgentOutputSegment {
            sequence,
            channel,
            kind: ingot_agent_protocol::AgentOutputKind::RawFallback,
            status: None,
            title: Some(artifact_name.trim_end_matches(".log").into()),
            text: Some(text),
            data: Some(serde_json::json!({
                "source_artifact": artifact_name
            })),
        });
        sequence += 1;
    }

    segments
}

fn non_empty_log_artifact(text: Option<String>) -> Option<String> {
    text.and_then(|text| {
        if text.trim().is_empty() {
            None
        } else {
            Some(text)
        }
    })
}

pub(super) async fn get_job_logs_raw(
    State(state): State<AppState>,
    ApiPath(JobPathParams { job_id }): ApiPath<JobPathParams>,
) -> Result<Json<JobRawLogsResponse>, ApiError> {
    state.db.get_job(job_id).await.map_err(repo_to_internal)?;
    let logs_dir = state.job_logs_dir(job_id);

    let prompt = read_optional_text(logs_dir.join("prompt.txt")).await?;
    let stdout = read_optional_text(logs_dir.join("stdout.log")).await?;
    let stderr = read_optional_text(logs_dir.join("stderr.log")).await?;
    let result = read_optional_json(logs_dir.join("result.json")).await?;

    Ok(Json(JobRawLogsResponse {
        prompt,
        stdout,
        stderr,
        result,
    }))
}

pub(super) async fn complete_job(
    State(state): State<AppState>,
    ApiPath(JobPathParams { job_id }): ApiPath<JobPathParams>,
    Json(request): Json<CompleteJobRequest>,
) -> Result<Json<CompleteJobResponse>, ApiError> {
    let prior_job = state.db.get_job(job_id).await.map_err(repo_to_internal)?;
    let prior_item = state
        .db
        .get_item(prior_job.item_id)
        .await
        .map_err(repo_to_item)?;
    let project = state
        .db
        .get_project(prior_job.project_id)
        .await
        .map_err(repo_to_project)?;
    state.infra().refresh_project_mirror(&project).await?;
    let result = state
        .complete_job_service
        .execute(CompleteJobCommand {
            job_id,
            outcome_class: request.outcome_class,
            result_schema_version: request.result_schema_version,
            result_payload: request.result_payload,
            output_commit_oid: request.output_commit_oid,
        })
        .await
        .map_err(complete_job_error_to_api_error)?;
    refresh_revision_context_for_job(&state, job_id).await?;
    let job = state.db.get_job(job_id).await.map_err(repo_to_internal)?;
    let item = state.db.get_item(job.item_id).await.map_err(repo_to_item)?;
    append_activity(
        &state,
        job.project_id,
        ActivityEventType::JobCompleted,
        ActivitySubject::Job(job.id),
        serde_json::json!({ "item_id": job.item_id, "outcome": job.state.outcome_class() }),
    )
    .await?;
    if prior_item.escalation.is_escalated()
        && item.current_revision_id == job.item_revision_id
        && !item.escalation.is_escalated()
    {
        append_activity(
            &state,
            job.project_id,
            ActivityEventType::ItemEscalationCleared,
            ActivitySubject::Item(item.id),
            serde_json::json!({ "reason": "successful_retry", "job_id": job.id }),
        )
        .await?;
    }
    if job.step_id == ingot_domain::step_id::StepId::ValidateIntegrated
        && job.state.outcome_class() == Some(OutcomeClass::Clean)
        && item.approval_state == ApprovalState::Pending
    {
        append_activity(
            &state,
            job.project_id,
            ActivityEventType::ApprovalRequested,
            ActivitySubject::Item(item.id),
            serde_json::json!({ "job_id": job.id }),
        )
        .await?;
    }
    if let Err(error) = auto_dispatch_projected_review_job(&state, &project, item.id).await {
        warn!(
            ?error,
            project_id = %project.id,
            item_id = %item.id,
            job_id = %job.id,
            "projected review auto-dispatch failed after job completion"
        );
    }

    Ok(Json(CompleteJobResponse {
        finding_count: result.finding_count,
    }))
}

pub(super) async fn fail_job(
    State(state): State<AppState>,
    ApiPath(JobPathParams { job_id }): ApiPath<JobPathParams>,
    Json(request): Json<FailJobRequest>,
) -> Result<Json<()>, ApiError> {
    // Validate outcome_class is valid for failure endpoint before proceeding
    let _status = map_failure_status(request.outcome_class)?;
    let job = state.db.get_job(job_id).await.map_err(repo_to_internal)?;
    let item = state.db.get_item(job.item_id).await.map_err(repo_to_item)?;
    if job.item_revision_id != item.current_revision_id {
        return Err(UseCaseError::ProtocolViolation(
            "job failure does not match the current item revision".into(),
        )
        .into());
    }

    job_lifecycle::fail_job(
        &state.db,
        &state.db,
        &state.db,
        &job,
        &item,
        request.outcome_class,
        request.error_code,
        request.error_message,
        WorkspaceStatus::Ready,
    )
    .await?;
    refresh_revision_context_for_job(&state, job.id).await?;

    Ok(Json(()))
}

pub(super) async fn expire_job(
    State(state): State<AppState>,
    ApiPath(JobPathParams { job_id }): ApiPath<JobPathParams>,
) -> Result<Json<()>, ApiError> {
    let job = state.db.get_job(job_id).await.map_err(repo_to_internal)?;
    let item = state.db.get_item(job.item_id).await.map_err(repo_to_item)?;
    if job.item_revision_id != item.current_revision_id {
        return Err(UseCaseError::ProtocolViolation(
            "job expiration does not match the current item revision".into(),
        )
        .into());
    }

    job_lifecycle::expire_job(
        &state.db,
        &state.db,
        &state.db,
        &job,
        &item,
        WorkspaceStatus::Ready,
    )
    .await?;
    refresh_revision_context_for_job(&state, job.id).await?;

    Ok(Json(()))
}

pub(super) async fn refresh_revision_context_for_job(
    state: &AppState,
    job_id: JobId,
) -> Result<(), ApiError> {
    let job = state.db.get_job(job_id).await.map_err(repo_to_internal)?;
    let item = state.db.get_item(job.item_id).await.map_err(repo_to_item)?;
    let revision = state
        .db
        .get_revision(job.item_revision_id)
        .await
        .map_err(repo_to_internal)?;
    let project = state
        .db
        .get_project(job.project_id)
        .await
        .map_err(repo_to_project)?;
    state.infra().refresh_project_mirror(&project).await?;
    refresh_revision_context_for_job_like(state, &item, &revision).await
}

pub(super) async fn refresh_revision_context_for_job_like(
    state: &AppState,
    item: &Item,
    revision: &ItemRevision,
) -> Result<(), ApiError> {
    let jobs = state
        .db
        .list_jobs_by_item(item.id)
        .await
        .map_err(repo_to_internal)?;
    let authoring_head_commit_oid =
        current_authoring_head_for_revision_with_workspace(state, revision, &jobs).await?;
    let authoring_base_commit_oid = effective_authoring_base_commit_oid(state, revision).await?;
    let changed_paths = if let (Some(base_commit_oid), Some(head_commit_oid)) = (
        authoring_base_commit_oid.as_ref(),
        authoring_head_commit_oid.as_ref(),
    ) {
        state
            .infra()
            .changed_paths_between(item.project_id, base_commit_oid, head_commit_oid)
            .await?
    } else {
        Vec::new()
    };
    let context = rebuild_revision_context(
        item,
        revision,
        &jobs,
        authoring_head_commit_oid,
        changed_paths,
        jobs.first().map(|job| job.id),
        Utc::now(),
    );
    state
        .db
        .upsert_revision_context(&context)
        .await
        .map_err(repo_to_internal)?;
    Ok(())
}

fn map_failure_status(outcome_class: OutcomeClass) -> Result<JobStatus, ApiError> {
    failure_status(outcome_class).ok_or_else(|| ApiError::BadRequest {
        code: "invalid_outcome_class",
        message:
            "Failure endpoints only accept transient_failure, terminal_failure, protocol_violation, or cancelled"
                .into(),
    })
}

#[cfg(test)]
mod tests {
    use ingot_domain::job::{JobStatus, OutcomeClass};

    use ingot_usecases::dispatch::failure_status;

    #[test]
    fn failure_status_maps_cancelled_to_cancelled_and_failures_to_failed() {
        assert_eq!(
            failure_status(OutcomeClass::Cancelled),
            Some(JobStatus::Cancelled)
        );
        assert_eq!(
            failure_status(OutcomeClass::TransientFailure),
            Some(JobStatus::Failed)
        );
        assert_eq!(
            failure_status(OutcomeClass::TerminalFailure),
            Some(JobStatus::Failed)
        );
        assert_eq!(
            failure_status(OutcomeClass::ProtocolViolation),
            Some(JobStatus::Failed)
        );
        assert_eq!(failure_status(OutcomeClass::Clean), None);
        assert_eq!(failure_status(OutcomeClass::Findings), None);
    }
}
