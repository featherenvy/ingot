use super::convergence_port::HttpConvergencePort;
use super::item_projection::load_item_detail;
use super::items::build_superseding_revision;
use super::support::*;
use super::types::*;
use super::*;

pub(super) fn routes() -> Router<AppState> {
    Router::new()
        .route(
            "/api/projects/{project_id}/items/{item_id}/convergence/prepare",
            post(prepare_item_convergence),
        )
        .route(
            "/api/projects/{project_id}/items/{item_id}/approval/approve",
            post(approve_item),
        )
        .route(
            "/api/projects/{project_id}/items/{item_id}/approval/reject",
            post(reject_item_approval),
        )
}

pub(super) async fn prepare_item_convergence(
    State(state): State<AppState>,
    ApiPath(ProjectItemPathParams {
        project_id,
        item_id,
    }): ApiPath<ProjectItemPathParams>,
) -> Result<Json<ItemDetailResponse>, ApiError> {
    let _guard = state
        .project_locks
        .acquire_project_mutation(project_id)
        .await;
    ConvergenceService::new(HttpConvergencePort::new(&state))
        .queue_prepare(project_id, item_id)
        .await
        .map_err(ApiError::from)?;
    let detail = load_item_detail(&state, project_id, item_id).await?;
    Ok(Json(detail))
}

pub(super) async fn approve_item(
    State(state): State<AppState>,
    ApiPath(ProjectItemPathParams {
        project_id,
        item_id,
    }): ApiPath<ProjectItemPathParams>,
) -> Result<Json<ItemDetailResponse>, ApiError> {
    let _guard = state
        .project_locks
        .acquire_project_mutation(project_id)
        .await;
    ConvergenceService::new(HttpConvergencePort::new(&state))
        .approve_item(project_id, item_id)
        .await
        .map_err(ApiError::from)?;
    let detail = load_item_detail(&state, project_id, item_id).await?;
    Ok(Json(detail))
}

pub(super) async fn reject_item_approval(
    State(state): State<AppState>,
    ApiPath(ProjectItemPathParams {
        project_id,
        item_id,
    }): ApiPath<ProjectItemPathParams>,
    maybe_request: Option<Json<RejectApprovalRequest>>,
) -> Result<Json<ItemDetailResponse>, ApiError> {
    let project = state
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
    let current_revision = state
        .db
        .get_revision(item.current_revision_id)
        .await
        .map_err(repo_to_internal)?;
    let jobs = state
        .db
        .list_jobs_by_item(item.id)
        .await
        .map_err(repo_to_internal)?;
    let request: ReviseItemRequest = maybe_request
        .map(|Json(request)| request)
        .unwrap_or_default()
        .into();
    let next_revision =
        build_superseding_revision(&state, &project, &item, &current_revision, &jobs, request)
            .await?;
    let cleared_escalation = item.escalation.is_escalated();
    let teardown = ConvergenceService::new(HttpConvergencePort::new(&state))
        .reject_item_approval(project_id, item.id, &next_revision)
        .await
        .map_err(ApiError::from)?;
    append_activity(
        &state,
        project_id,
        ActivityEventType::ApprovalRejected,
        ActivitySubject::Item(item.id),
        serde_json::json!({
            "new_revision_id": next_revision.id,
            "cancelled_convergence_id": teardown.first_cancelled_convergence_id,
            "cancelled_queue_entry_id": teardown.first_cancelled_queue_entry_id
        }),
    )
    .await?;
    if cleared_escalation {
        append_activity(
            &state,
            project_id,
            ActivityEventType::ItemEscalationCleared,
            ActivitySubject::Item(item.id),
            serde_json::json!({ "reason": "approval_reject" }),
        )
        .await?;
    }

    let detail = load_item_detail(&state, project_id, item.id).await?;
    Ok(Json(detail))
}
