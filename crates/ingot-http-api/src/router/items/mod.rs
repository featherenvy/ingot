mod convergence_prep;
mod revisions;

#[allow(unused_imports)]
pub(super) use convergence_prep::prepare_convergence_workspace;
pub(super) use revisions::{
    build_superseding_revision, resolve_seed_target_commit_oid, validate_seed_commit_oid,
};

use super::dispatch::auto_dispatch_projected_review_job_locked;
use super::infra_ports::HttpInfraAdapter;
use super::item_projection::{
    evaluate_item_snapshot, load_item_detail, load_item_runtime_snapshot,
};
use super::support::*;
use super::types::*;
use super::*;

pub(super) fn routes() -> Router<AppState> {
    Router::new()
        .route(
            "/api/projects/{project_id}/items",
            get(list_items).post(create_item),
        )
        .route(
            "/api/projects/{project_id}/items/{item_id}",
            get(get_item).patch(update_item),
        )
        .route(
            "/api/projects/{project_id}/items/{item_id}/revise",
            post(revise_item),
        )
        .route(
            "/api/projects/{project_id}/items/{item_id}/defer",
            post(defer_item),
        )
        .route(
            "/api/projects/{project_id}/items/{item_id}/resume",
            post(resume_item),
        )
        .route(
            "/api/projects/{project_id}/items/{item_id}/dismiss",
            post(dismiss_item),
        )
        .route(
            "/api/projects/{project_id}/items/{item_id}/invalidate",
            post(invalidate_item),
        )
        .route(
            "/api/projects/{project_id}/items/{item_id}/reopen",
            post(reopen_item),
        )
        .route(
            "/api/projects/{project_id}/items/{item_id}/findings",
            get(list_item_findings),
        )
}

pub(super) async fn create_item(
    State(state): State<AppState>,
    ApiPath(ProjectPathParams { project_id }): ApiPath<ProjectPathParams>,
    Json(request): Json<CreateItemRequest>,
) -> Result<(StatusCode, Json<ItemDetailResponse>), ApiError> {
    let project = state
        .db
        .get_project(project_id)
        .await
        .map_err(repo_to_project)?;
    let _guard = state
        .project_locks
        .acquire_project_mutation(project_id)
        .await;
    let paths = refresh_project_mirror(&state, &project).await?;
    let config = load_effective_config(Some(&project))?;
    let configured_approval_policy = config.defaults.approval_policy;

    let target_ref = normalize_target_ref(
        request
            .target_ref
            .as_ref()
            .map(GitRef::as_str)
            .unwrap_or(project.default_branch.as_str()),
    )?;
    ensure_git_valid_target_ref(target_ref.as_str()).await?;
    let infra = HttpInfraAdapter::new(&state);
    let repo_path = paths.mirror_git_dir.as_path();
    let resolved_target_head = infra
        .resolve_project_ref_oid(project.id, &target_ref)
        .await?
        .ok_or_else(|| UseCaseError::TargetRefUnresolved(target_ref.to_string()))?;

    let seed_commit_oid = validate_seed_commit_oid(repo_path, request.seed_commit_oid).await?;
    let seed_target_commit_oid = resolve_seed_target_commit_oid(
        repo_path,
        request.seed_target_commit_oid,
        resolved_target_head,
    )
    .await?;
    let seed = AuthoringBaseSeed::from_parts(seed_commit_oid, seed_target_commit_oid);

    let sort_key = next_project_sort_key(&state, project_id).await?;

    let (item, revision) = create_manual_item(
        &project,
        CreateItemInput {
            classification: request.classification.unwrap_or(Classification::Change),
            priority: request.priority.unwrap_or(Priority::Major),
            labels: request.labels.unwrap_or_default(),
            operator_notes: request.operator_notes,
            title: request.title,
            description: request.description,
            acceptance_criteria: request.acceptance_criteria,
            target_ref,
            approval_policy: request
                .approval_policy
                .unwrap_or(configured_approval_policy),
            candidate_rework_budget: config.defaults.candidate_rework_budget,
            integration_rework_budget: config.defaults.integration_rework_budget,
            seed,
        },
        sort_key,
        Utc::now(),
    );

    state
        .db
        .create_item_with_revision(&item, &revision)
        .await
        .map_err(repo_to_internal)?;
    append_activity(
        &state,
        project_id,
        ActivityEventType::ItemCreated,
        ActivitySubject::Item(item.id),
        serde_json::json!({ "revision_id": revision.id }),
    )
    .await?;

    let detail = load_item_detail(&state, project_id, item.id).await?;
    Ok((StatusCode::CREATED, Json(detail)))
}

pub(super) async fn list_items(
    State(state): State<AppState>,
    ApiPath(ProjectPathParams { project_id }): ApiPath<ProjectPathParams>,
) -> Result<Json<Vec<ItemSummaryResponse>>, ApiError> {
    let project = state
        .db
        .get_project(project_id)
        .await
        .map_err(repo_to_project)?;
    let paths = refresh_project_mirror(&state, &project).await?;
    let items = state
        .db
        .list_items_by_project(project_id)
        .await
        .map_err(repo_to_internal)?;
    let evaluator = Evaluator::new();
    let mut summaries = Vec::with_capacity(items.len());

    for item in items {
        let snapshot =
            load_item_runtime_snapshot(&state, paths.mirror_git_dir.as_path(), &item).await?;
        let (evaluation, queue) =
            evaluate_item_snapshot(&state, &project, &item, &snapshot, &evaluator).await?;

        let title = snapshot.current_revision.title.clone();
        summaries.push(ItemSummaryResponse {
            item,
            title,
            evaluation,
            queue,
        });
    }

    Ok(Json(summaries))
}

pub(super) async fn update_item(
    State(state): State<AppState>,
    ApiPath(ProjectItemPathParams {
        project_id,
        item_id,
    }): ApiPath<ProjectItemPathParams>,
    Json(request): Json<UpdateItemRequest>,
) -> Result<Json<ItemDetailResponse>, ApiError> {
    let _project = state
        .db
        .get_project(project_id)
        .await
        .map_err(repo_to_project)?;
    let _guard = state
        .project_locks
        .acquire_project_mutation(project_id)
        .await;
    let mut item = state.db.get_item(item_id).await.map_err(repo_to_item)?;
    if item.project_id != project_id {
        return Err(UseCaseError::ItemNotFound.into());
    }
    if let Some(classification) = request.classification {
        item.classification = classification;
    }
    if let Some(priority) = request.priority {
        item.priority = priority;
    }
    if let Some(labels) = request.labels {
        item.labels = labels;
    }
    if request.operator_notes.is_some() {
        item.operator_notes = request.operator_notes;
    }
    item.updated_at = Utc::now();
    state
        .db
        .update_item(&item)
        .await
        .map_err(repo_to_internal)?;
    append_activity(
        &state,
        project_id,
        ActivityEventType::ItemUpdated,
        ActivitySubject::Item(item.id),
        serde_json::json!({}),
    )
    .await?;
    let detail = load_item_detail(&state, project_id, item.id).await?;
    Ok(Json(detail))
}

pub(super) async fn get_item(
    State(state): State<AppState>,
    ApiPath(ProjectItemPathParams {
        project_id,
        item_id,
    }): ApiPath<ProjectItemPathParams>,
) -> Result<Json<ItemDetailResponse>, ApiError> {
    state
        .db
        .get_project(project_id)
        .await
        .map_err(repo_to_project)?;
    let response = load_item_detail(&state, project_id, item_id).await?;
    Ok(Json(response))
}

pub(super) async fn revise_item(
    State(state): State<AppState>,
    ApiPath(ProjectItemPathParams {
        project_id,
        item_id,
    }): ApiPath<ProjectItemPathParams>,
    maybe_request: Option<Json<ReviseItemRequest>>,
) -> Result<Json<ItemDetailResponse>, ApiError> {
    let request: ReviseItemRequest = maybe_request
        .map(|Json(request)| request)
        .unwrap_or_default();
    let project = state
        .db
        .get_project(project_id)
        .await
        .map_err(repo_to_project)?;
    let _guard = state
        .project_locks
        .acquire_project_mutation(project_id)
        .await;
    let mut item = state.db.get_item(item_id).await.map_err(repo_to_item)?;
    if item.project_id != project_id {
        return Err(UseCaseError::ItemNotFound.into());
    }
    ensure_item_open_idle(&item)?;
    let current_revision = state
        .db
        .get_revision(item.current_revision_id)
        .await
        .map_err(repo_to_internal)?;
    let _ = teardown_revision_lane_state(&state, &project, item.id, &current_revision).await?;
    let jobs = state
        .db
        .list_jobs_by_item(item.id)
        .await
        .map_err(repo_to_internal)?;
    let next_revision =
        build_superseding_revision(&state, &project, &item, &current_revision, &jobs, request)
            .await?;
    state
        .db
        .create_revision(&next_revision)
        .await
        .map_err(repo_to_internal)?;
    item.current_revision_id = next_revision.id;
    let cleared_escalation = item.escalation.is_escalated();
    item.approval_state = approval_state_for_policy(next_revision.approval_policy);
    item.escalation = Escalation::None;
    item.updated_at = Utc::now();
    state
        .db
        .update_item(&item)
        .await
        .map_err(repo_to_internal)?;
    append_activity(
        &state,
        project_id,
        ActivityEventType::ItemRevisionCreated,
        ActivitySubject::Item(item.id),
        serde_json::json!({ "revision_id": next_revision.id, "kind": "revise" }),
    )
    .await?;
    if cleared_escalation {
        append_activity(
            &state,
            project_id,
            ActivityEventType::ItemEscalationCleared,
            ActivitySubject::Item(item.id),
            serde_json::json!({ "reason": "revise" }),
        )
        .await?;
    }
    let detail = load_item_detail(&state, project_id, item.id).await?;
    Ok(Json(detail))
}

pub(super) async fn defer_item(
    State(state): State<AppState>,
    ApiPath(ProjectItemPathParams {
        project_id,
        item_id,
    }): ApiPath<ProjectItemPathParams>,
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
    let mut item = state.db.get_item(item_id).await.map_err(repo_to_item)?;
    if item.project_id != project_id {
        return Err(UseCaseError::ItemNotFound.into());
    }
    ensure_item_open_idle(&item)?;
    if item.approval_state == ApprovalState::Pending {
        return Err(ApiError::Conflict {
            code: "item_pending_approval",
            message: "Pending approval items cannot be deferred".into(),
        });
    }
    let current_revision = state
        .db
        .get_revision(item.current_revision_id)
        .await
        .map_err(repo_to_internal)?;
    let _ = teardown_revision_lane_state(&state, &project, item.id, &current_revision).await?;
    item.parking_state = ingot_domain::item::ParkingState::Deferred;
    item.approval_state = approval_state_for_policy(current_revision.approval_policy);
    item.escalation = Escalation::None;
    item.updated_at = Utc::now();
    state
        .db
        .update_item(&item)
        .await
        .map_err(repo_to_internal)?;
    append_activity(
        &state,
        project_id,
        ActivityEventType::ItemDeferred,
        ActivitySubject::Item(item.id),
        serde_json::json!({}),
    )
    .await?;
    let detail = load_item_detail(&state, project_id, item.id).await?;
    Ok(Json(detail))
}

pub(super) async fn resume_item(
    State(state): State<AppState>,
    ApiPath(ProjectItemPathParams {
        project_id,
        item_id,
    }): ApiPath<ProjectItemPathParams>,
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
    let mut item = state.db.get_item(item_id).await.map_err(repo_to_item)?;
    if item.project_id != project_id {
        return Err(UseCaseError::ItemNotFound.into());
    }
    if item.parking_state != ingot_domain::item::ParkingState::Deferred {
        return Err(ApiError::Conflict {
            code: "item_not_deferred",
            message: "Item is not deferred".into(),
        });
    }
    item.parking_state = ingot_domain::item::ParkingState::Active;
    item.updated_at = Utc::now();
    state
        .db
        .update_item(&item)
        .await
        .map_err(repo_to_internal)?;
    append_activity(
        &state,
        project_id,
        ActivityEventType::ItemResumed,
        ActivitySubject::Item(item.id),
        serde_json::json!({}),
    )
    .await?;
    if let Err(error) = auto_dispatch_projected_review_job_locked(&state, &project, item.id).await {
        warn!(
            ?error,
            project_id = %project_id,
            item_id = %item.id,
            "projected review auto-dispatch failed after resume"
        );
    }
    let detail = load_item_detail(&state, project_id, item.id).await?;
    Ok(Json(detail))
}

pub(super) async fn dismiss_item(
    State(state): State<AppState>,
    ApiPath(ProjectItemPathParams {
        project_id,
        item_id,
    }): ApiPath<ProjectItemPathParams>,
) -> Result<Json<ItemDetailResponse>, ApiError> {
    finish_item_manually(
        state,
        project_id,
        item_id,
        DoneReason::Dismissed,
        ActivityEventType::ItemDismissed,
    )
    .await
}

pub(super) async fn invalidate_item(
    State(state): State<AppState>,
    ApiPath(ProjectItemPathParams {
        project_id,
        item_id,
    }): ApiPath<ProjectItemPathParams>,
) -> Result<Json<ItemDetailResponse>, ApiError> {
    finish_item_manually(
        state,
        project_id,
        item_id,
        DoneReason::Invalidated,
        ActivityEventType::ItemInvalidated,
    )
    .await
}

pub(super) async fn reopen_item(
    State(state): State<AppState>,
    ApiPath(ProjectItemPathParams {
        project_id,
        item_id,
    }): ApiPath<ProjectItemPathParams>,
    maybe_request: Option<Json<ReviseItemRequest>>,
) -> Result<Json<ItemDetailResponse>, ApiError> {
    let request: ReviseItemRequest = maybe_request
        .map(|Json(request)| request)
        .unwrap_or_default();
    let project = state
        .db
        .get_project(project_id)
        .await
        .map_err(repo_to_project)?;
    let _guard = state
        .project_locks
        .acquire_project_mutation(project_id)
        .await;
    let mut item = state.db.get_item(item_id).await.map_err(repo_to_item)?;
    if item.project_id != project_id {
        return Err(UseCaseError::ItemNotFound.into());
    }
    match item.lifecycle {
        Lifecycle::Done {
            reason: DoneReason::Dismissed | DoneReason::Invalidated,
            ..
        } => {}
        Lifecycle::Done {
            reason: DoneReason::Completed,
            ..
        } => return Err(UseCaseError::CompletedItemCannotReopen.into()),
        Lifecycle::Open => {
            return Err(ApiError::Conflict {
                code: "item_not_reopenable",
                message: "Only dismissed or invalidated items can be reopened".into(),
            });
        }
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
    let next_revision =
        build_superseding_revision(&state, &project, &item, &current_revision, &jobs, request)
            .await?;
    state
        .db
        .create_revision(&next_revision)
        .await
        .map_err(repo_to_internal)?;
    let cleared_escalation = item.escalation.is_escalated();
    item.current_revision_id = next_revision.id;
    item.lifecycle = Lifecycle::Open;
    item.parking_state = ingot_domain::item::ParkingState::Active;
    item.approval_state = approval_state_for_policy(next_revision.approval_policy);
    item.escalation = Escalation::None;
    item.updated_at = Utc::now();
    state
        .db
        .update_item(&item)
        .await
        .map_err(repo_to_internal)?;
    append_activity(
        &state,
        project_id,
        ActivityEventType::ItemReopened,
        ActivitySubject::Item(item.id),
        serde_json::json!({ "revision_id": next_revision.id }),
    )
    .await?;
    if cleared_escalation {
        append_activity(
            &state,
            project_id,
            ActivityEventType::ItemEscalationCleared,
            ActivitySubject::Item(item.id),
            serde_json::json!({ "reason": "reopen" }),
        )
        .await?;
    }
    let detail = load_item_detail(&state, project_id, item.id).await?;
    Ok(Json(detail))
}

pub(super) async fn list_item_findings(
    State(state): State<AppState>,
    ApiPath(ProjectItemPathParams {
        project_id,
        item_id,
    }): ApiPath<ProjectItemPathParams>,
) -> Result<Json<Vec<Finding>>, ApiError> {
    state
        .db
        .get_project(project_id)
        .await
        .map_err(repo_to_project)?;
    let item = state.db.get_item(item_id).await.map_err(repo_to_item)?;
    if item.project_id != project_id {
        return Err(UseCaseError::ItemNotFound.into());
    }

    let findings = state
        .db
        .list_findings_by_item(item_id)
        .await
        .map_err(repo_to_internal)?;

    Ok(Json(findings))
}

pub(super) fn ensure_item_open_idle(item: &Item) -> Result<(), ApiError> {
    if !item.lifecycle.is_open() {
        return Err(UseCaseError::ItemNotOpen.into());
    }
    if item.parking_state != ingot_domain::item::ParkingState::Active {
        return Err(UseCaseError::ItemNotIdle.into());
    }
    Ok(())
}

#[derive(Default)]
#[allow(dead_code)]
pub(crate) struct RevisionLaneTeardown {
    pub(super) cancelled_job_ids: Vec<String>,
    pub(super) cancelled_convergence_ids: Vec<String>,
    pub(super) cancelled_queue_entry_ids: Vec<String>,
    pub(super) reconciled_prepare_operation_ids: Vec<String>,
    pub(super) failed_finalize_operation_ids: Vec<String>,
}

impl RevisionLaneTeardown {
    pub(super) fn has_cancelled_convergence(&self) -> bool {
        !self.cancelled_convergence_ids.is_empty()
    }

    pub(super) fn has_cancelled_queue_entry(&self) -> bool {
        !self.cancelled_queue_entry_ids.is_empty()
    }

    pub(super) fn first_cancelled_convergence_id(&self) -> Option<&str> {
        self.cancelled_convergence_ids.first().map(String::as_str)
    }

    pub(super) fn first_cancelled_queue_entry_id(&self) -> Option<&str> {
        self.cancelled_queue_entry_ids.first().map(String::as_str)
    }
}

pub(super) async fn finish_item_manually(
    state: AppState,
    project_id: ProjectId,
    item_id: ItemId,
    done_reason: DoneReason,
    event_type: ActivityEventType,
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
    let mut item = state.db.get_item(item_id).await.map_err(repo_to_item)?;
    if item.project_id != project_id {
        return Err(UseCaseError::ItemNotFound.into());
    }
    ensure_item_open_idle(&item)?;
    let revision = state
        .db
        .get_revision(item.current_revision_id)
        .await
        .map_err(repo_to_internal)?;
    let _ = teardown_revision_lane_state(&state, &project, item.id, &revision).await?;
    item.lifecycle = Lifecycle::Done {
        reason: done_reason,
        source: ResolutionSource::ManualCommand,
        closed_at: Utc::now(),
    };
    item.approval_state = approval_state_for_policy(revision.approval_policy);
    item.escalation = Escalation::None;
    item.updated_at = Utc::now();
    state
        .db
        .update_item(&item)
        .await
        .map_err(repo_to_internal)?;
    append_activity(
        &state,
        project_id,
        event_type,
        ActivitySubject::Item(item.id),
        serde_json::json!({ "done_reason": item.lifecycle.done_reason() }),
    )
    .await?;
    let detail = load_item_detail(&state, project_id, item.id).await?;
    Ok(Json(detail))
}

pub(super) async fn ensure_authoring_workspace(
    state: &AppState,
    project: &Project,
    revision: &ItemRevision,
    job: &Job,
) -> Result<Workspace, ApiError> {
    let infra = HttpInfraAdapter::new(state);
    let existing = state
        .db
        .find_authoring_workspace_for_revision(revision.id)
        .await
        .map_err(repo_to_internal)?;
    let workspace_exists = existing.is_some();
    let workspace = infra
        .ensure_authoring_workspace(project.id, revision, job, existing)
        .await?;

    if workspace_exists {
        state
            .db
            .update_workspace(&workspace)
            .await
            .map_err(repo_to_internal)?;
    } else {
        state
            .db
            .create_workspace(&workspace)
            .await
            .map_err(repo_to_internal)?;
    }

    Ok(workspace)
}

pub(super) async fn current_authoring_head_for_revision_with_workspace(
    state: &AppState,
    revision: &ItemRevision,
    jobs: &[Job],
) -> Result<Option<CommitOid>, ApiError> {
    let workspace = state
        .db
        .find_authoring_workspace_for_revision(revision.id)
        .await
        .map_err(repo_to_internal)?;
    Ok(
        ingot_usecases::dispatch::current_authoring_head_for_revision_with_workspace(
            revision,
            jobs,
            workspace.as_ref(),
        ),
    )
}

pub(super) async fn effective_authoring_base_commit_oid(
    state: &AppState,
    revision: &ItemRevision,
) -> Result<Option<CommitOid>, ApiError> {
    let workspace = state
        .db
        .find_authoring_workspace_for_revision(revision.id)
        .await
        .map_err(repo_to_internal)?;
    Ok(ingot_usecases::dispatch::effective_authoring_base_commit_oid(revision, workspace.as_ref()))
}
