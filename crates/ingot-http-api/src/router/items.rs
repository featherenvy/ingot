use super::dispatch::auto_dispatch_projected_review_job_locked;
use super::support::*;
use super::types::*;
use super::*;

pub(super) async fn create_item(
    State(state): State<AppState>,
    Path(project_id): Path<String>,
    Json(request): Json<CreateItemRequest>,
) -> Result<(StatusCode, Json<ItemDetailResponse>), ApiError> {
    let project_id = parse_id::<ProjectId>(&project_id, "project")?;
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
    let configured_approval_policy = parse_config_approval_policy(&config)?;

    let target_ref = normalize_target_ref(
        request
            .target_ref
            .as_deref()
            .unwrap_or(project.default_branch.as_str()),
    )?;
    ensure_git_valid_target_ref(&target_ref).await?;
    let repo_path = paths.mirror_git_dir.as_path();
    let resolved_target_head = resolve_ref_oid(repo_path, &target_ref)
        .await
        .map_err(git_to_internal)?
        .ok_or_else(|| UseCaseError::TargetRefUnresolved(target_ref.clone()))?;

    let seed_commit_oid = if let Some(seed_commit_oid) = request.seed_commit_oid {
        ensure_reachable_seed(repo_path, "seed_commit_oid", &seed_commit_oid).await?;
        Some(seed_commit_oid)
    } else {
        None
    };

    let seed_target_commit_oid = if let Some(seed_target_commit_oid) =
        request.seed_target_commit_oid
    {
        ensure_reachable_seed(repo_path, "seed_target_commit_oid", &seed_target_commit_oid).await?;
        Some(seed_target_commit_oid)
    } else {
        Some(resolved_target_head)
    };

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
            seed_commit_oid,
            seed_target_commit_oid,
        },
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
        "item",
        item.id,
        serde_json::json!({ "revision_id": revision.id }),
    )
    .await?;

    let detail = load_item_detail(&state, project_id, item.id).await?;
    Ok((StatusCode::CREATED, Json(detail)))
}

pub(super) async fn list_items(
    State(state): State<AppState>,
    Path(project_id): Path<String>,
) -> Result<Json<Vec<ItemSummaryResponse>>, ApiError> {
    let project_id = parse_id::<ProjectId>(&project_id, "project")?;
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
        let findings = state
            .db
            .list_findings_by_item(item.id)
            .await
            .map_err(repo_to_internal)?;
        let convergences = state
            .db
            .list_convergences_by_item(item.id)
            .await
            .map_err(repo_to_internal)?;
        let convergences =
            hydrate_convergence_validity(paths.mirror_git_dir.as_path(), convergences).await?;
        let evaluation =
            evaluator.evaluate(&item, &current_revision, &jobs, &findings, &convergences);
        let queue =
            load_queue_status(&state, &item, &current_revision, &project, &evaluation).await?;
        let evaluation = overlay_evaluation_with_queue_state(
            &item,
            &current_revision,
            &convergences,
            evaluation,
            &queue,
        );

        let title = current_revision.title.clone();
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
    Path((project_id, item_id)): Path<(String, String)>,
    Json(request): Json<UpdateItemRequest>,
) -> Result<Json<ItemDetailResponse>, ApiError> {
    let project_id = parse_id::<ProjectId>(&project_id, "project")?;
    let item_id = parse_id::<ItemId>(&item_id, "item")?;
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
        "item",
        item.id,
        serde_json::json!({}),
    )
    .await?;
    let detail = load_item_detail(&state, project_id, item.id).await?;
    Ok(Json(detail))
}

pub(super) async fn get_item(
    State(state): State<AppState>,
    Path((project_id, item_id)): Path<(String, String)>,
) -> Result<Json<ItemDetailResponse>, ApiError> {
    let project_id = parse_id::<ProjectId>(&project_id, "project")?;
    let item_id = parse_id::<ItemId>(&item_id, "item")?;
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
    Path((project_id, item_id)): Path<(String, String)>,
    maybe_request: Option<Json<ReviseItemRequest>>,
) -> Result<Json<ItemDetailResponse>, ApiError> {
    let request: ReviseItemRequest = maybe_request
        .map(|Json(request)| request)
        .unwrap_or_default();
    let project_id = parse_id::<ProjectId>(&project_id, "project")?;
    let item_id = parse_id::<ItemId>(&item_id, "item")?;
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
        "item",
        item.id,
        serde_json::json!({ "revision_id": next_revision.id, "kind": "revise" }),
    )
    .await?;
    if cleared_escalation {
        append_activity(
            &state,
            project_id,
            ActivityEventType::ItemEscalationCleared,
            "item",
            item.id,
            serde_json::json!({ "reason": "revise" }),
        )
        .await?;
    }
    let detail = load_item_detail(&state, project_id, item.id).await?;
    Ok(Json(detail))
}

pub(super) async fn defer_item(
    State(state): State<AppState>,
    Path((project_id, item_id)): Path<(String, String)>,
) -> Result<Json<ItemDetailResponse>, ApiError> {
    let project_id = parse_id::<ProjectId>(&project_id, "project")?;
    let item_id = parse_id::<ItemId>(&item_id, "item")?;
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
        "item",
        item.id,
        serde_json::json!({}),
    )
    .await?;
    let detail = load_item_detail(&state, project_id, item.id).await?;
    Ok(Json(detail))
}

pub(super) async fn resume_item(
    State(state): State<AppState>,
    Path((project_id, item_id)): Path<(String, String)>,
) -> Result<Json<ItemDetailResponse>, ApiError> {
    let project_id = parse_id::<ProjectId>(&project_id, "project")?;
    let item_id = parse_id::<ItemId>(&item_id, "item")?;
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
        "item",
        item.id,
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
    Path((project_id, item_id)): Path<(String, String)>,
) -> Result<Json<ItemDetailResponse>, ApiError> {
    finish_item_manually(
        state,
        parse_id::<ProjectId>(&project_id, "project")?,
        parse_id::<ItemId>(&item_id, "item")?,
        DoneReason::Dismissed,
        ActivityEventType::ItemDismissed,
    )
    .await
}

pub(super) async fn invalidate_item(
    State(state): State<AppState>,
    Path((project_id, item_id)): Path<(String, String)>,
) -> Result<Json<ItemDetailResponse>, ApiError> {
    finish_item_manually(
        state,
        parse_id::<ProjectId>(&project_id, "project")?,
        parse_id::<ItemId>(&item_id, "item")?,
        DoneReason::Invalidated,
        ActivityEventType::ItemInvalidated,
    )
    .await
}

pub(super) async fn reopen_item(
    State(state): State<AppState>,
    Path((project_id, item_id)): Path<(String, String)>,
    maybe_request: Option<Json<ReviseItemRequest>>,
) -> Result<Json<ItemDetailResponse>, ApiError> {
    let request: ReviseItemRequest = maybe_request
        .map(|Json(request)| request)
        .unwrap_or_default();
    let project_id = parse_id::<ProjectId>(&project_id, "project")?;
    let item_id = parse_id::<ItemId>(&item_id, "item")?;
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
        "item",
        item.id,
        serde_json::json!({ "revision_id": next_revision.id }),
    )
    .await?;
    if cleared_escalation {
        append_activity(
            &state,
            project_id,
            ActivityEventType::ItemEscalationCleared,
            "item",
            item.id,
            serde_json::json!({ "reason": "reopen" }),
        )
        .await?;
    }
    let detail = load_item_detail(&state, project_id, item.id).await?;
    Ok(Json(detail))
}

pub(super) async fn list_item_findings(
    State(state): State<AppState>,
    Path((project_id, item_id)): Path<(String, String)>,
) -> Result<Json<Vec<Finding>>, ApiError> {
    let project_id = parse_id::<ProjectId>(&project_id, "project")?;
    let item_id = parse_id::<ItemId>(&item_id, "item")?;
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

pub(super) async fn load_item_detail(
    state: &AppState,
    project_id: ProjectId,
    item_id: ItemId,
) -> Result<ItemDetailResponse, ApiError> {
    let item = state.db.get_item(item_id).await.map_err(repo_to_item)?;
    if item.project_id != project_id {
        return Err(UseCaseError::ItemNotFound.into());
    }
    let project = state
        .db
        .get_project(item.project_id)
        .await
        .map_err(repo_to_project)?;
    let paths = refresh_project_mirror(state, &project).await?;

    let current_revision = state
        .db
        .get_revision(item.current_revision_id)
        .await
        .map_err(repo_to_internal)?;
    let revision_history = state
        .db
        .list_revisions_by_item(item.id)
        .await
        .map_err(repo_to_internal)?;
    let jobs = state
        .db
        .list_jobs_by_item(item.id)
        .await
        .map_err(repo_to_internal)?;
    let findings = state
        .db
        .list_findings_by_item(item.id)
        .await
        .map_err(repo_to_internal)?;
    let workspaces = state
        .db
        .list_workspaces_by_item(item.id)
        .await
        .map_err(repo_to_internal)?;
    let convergences = state
        .db
        .list_convergences_by_item(item.id)
        .await
        .map_err(repo_to_internal)?;
    let convergences =
        hydrate_convergence_validity(paths.mirror_git_dir.as_path(), convergences).await?;
    let revision_context = state
        .db
        .get_revision_context(item.current_revision_id)
        .await
        .map_err(repo_to_internal)?;
    let revision_context_summary = parse_revision_context_summary(revision_context.as_ref())?;
    let evaluation =
        Evaluator::new().evaluate(&item, &current_revision, &jobs, &findings, &convergences);
    let queue = load_queue_status(state, &item, &current_revision, &project, &evaluation).await?;
    let evaluation = overlay_evaluation_with_queue_state(
        &item,
        &current_revision,
        &convergences,
        evaluation,
        &queue,
    );
    let diagnostics = evaluation.diagnostics.clone();

    Ok(ItemDetailResponse {
        item,
        current_revision,
        evaluation,
        queue,
        revision_history,
        jobs,
        findings,
        workspaces,
        convergences: convergences.into_iter().map(convergence_response).collect(),
        revision_context_summary,
        diagnostics,
    })
}

pub(crate) async fn append_activity(
    state: &AppState,
    project_id: ProjectId,
    event_type: ActivityEventType,
    entity_type: &'static str,
    entity_id: impl ToString,
    payload: serde_json::Value,
) -> Result<(), ApiError> {
    state
        .db
        .append_activity(&Activity {
            id: ingot_domain::ids::ActivityId::new(),
            project_id,
            event_type,
            entity_type: entity_type.into(),
            entity_id: entity_id.to_string(),
            payload,
            created_at: Utc::now(),
        })
        .await
        .map_err(repo_to_internal)
}

pub(super) async fn read_optional_text(path: PathBuf) -> Result<Option<String>, ApiError> {
    match tokio::fs::read_to_string(path).await {
        Ok(contents) => Ok(Some(contents)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(ApiError::from(UseCaseError::Internal(error.to_string()))),
    }
}

pub(super) async fn read_optional_json(
    path: PathBuf,
) -> Result<Option<serde_json::Value>, ApiError> {
    let Some(contents) = read_optional_text(path).await? else {
        return Ok(None);
    };

    serde_json::from_str(&contents)
        .map(Some)
        .map_err(|error| ApiError::from(UseCaseError::Internal(error.to_string())))
}

pub(super) fn convergence_response(convergence: Convergence) -> ConvergenceResponse {
    ConvergenceResponse {
        id: convergence.id.to_string(),
        status: serde_json::to_value(convergence.state.status())
            .ok()
            .and_then(|value| value.as_str().map(ToOwned::to_owned))
            .unwrap_or_else(|| "unknown".into()),
        input_target_commit_oid: convergence
            .state
            .input_target_commit_oid()
            .map(ToOwned::to_owned),
        prepared_commit_oid: convergence
            .state
            .prepared_commit_oid()
            .map(ToOwned::to_owned),
        final_target_commit_oid: convergence
            .state
            .final_target_commit_oid()
            .map(ToOwned::to_owned),
        target_head_valid: convergence.target_head_valid.unwrap_or(true),
    }
}

pub(super) fn empty_queue_status() -> QueueStatusResponse {
    QueueStatusResponse {
        state: None,
        position: None,
        lane_owner_item_id: None,
        lane_target_ref: None,
        checkout_sync_blocked: false,
        checkout_sync_message: None,
    }
}

pub(super) fn overlay_evaluation_with_queue_state(
    item: &Item,
    revision: &ItemRevision,
    convergences: &[Convergence],
    mut evaluation: Evaluation,
    queue: &QueueStatusResponse,
) -> Evaluation {
    let has_prepared_convergence = convergences.iter().any(|convergence| {
        convergence.item_revision_id == revision.id
            && convergence.state.status() == ingot_domain::convergence::ConvergenceStatus::Prepared
    });

    if queue.state.is_some()
        && evaluation.next_recommended_action == RecommendedAction::PrepareConvergence
    {
        set_awaiting_convergence_lane(&mut evaluation);
    }

    if queue.state.as_deref() == Some("queued") {
        set_awaiting_convergence_lane(&mut evaluation);
    }

    if item.approval_state == ApprovalState::Granted && has_prepared_convergence {
        evaluation.next_recommended_action = if queue.checkout_sync_blocked {
            RecommendedAction::ResolveCheckoutSync
        } else {
            RecommendedAction::FinalizePreparedConvergence
        };
        evaluation.dispatchable_step_id = None;
        evaluation.allowed_actions = vec![];
        evaluation.phase_status = Some(if queue.checkout_sync_blocked {
            PhaseStatus::AwaitingConvergence
        } else {
            PhaseStatus::FinalizationReady
        });
    }

    if queue.checkout_sync_blocked
        && revision.approval_policy == ApprovalPolicy::NotRequired
        && has_prepared_convergence
        && evaluation.next_recommended_action == RecommendedAction::FinalizePreparedConvergence
    {
        evaluation.next_recommended_action = RecommendedAction::ResolveCheckoutSync;
        evaluation.dispatchable_step_id = None;
        evaluation.allowed_actions = vec![];
        evaluation.phase_status = Some(PhaseStatus::AwaitingConvergence);
    }

    evaluation
}

fn set_awaiting_convergence_lane(evaluation: &mut Evaluation) {
    evaluation.next_recommended_action = RecommendedAction::AwaitConvergenceLane;
    evaluation.dispatchable_step_id = None;
    evaluation
        .allowed_actions
        .retain(|action| *action != AllowedAction::PrepareConvergence);
    evaluation.phase_status = Some(PhaseStatus::AwaitingConvergence);
}

pub(super) async fn load_queue_status(
    state: &AppState,
    item: &Item,
    revision: &ItemRevision,
    project: &Project,
    evaluation: &Evaluation,
) -> Result<QueueStatusResponse, ApiError> {
    let Some(active_entry) = state
        .db
        .find_active_queue_entry_for_revision(revision.id)
        .await
        .map_err(repo_to_internal)?
    else {
        return Ok(empty_queue_status());
    };

    let lane_entries = state
        .db
        .list_active_queue_entries_for_lane(project.id, &revision.target_ref)
        .await
        .map_err(repo_to_internal)?;
    let lane_owner_item_id = lane_entries
        .iter()
        .find(|entry| entry.status == ConvergenceQueueEntryStatus::Head)
        .map(|entry| entry.item_id.to_string());
    let position = lane_entries
        .iter()
        .position(|entry| entry.id == active_entry.id)
        .map(|index| index as u32 + 1);

    let mut queue = QueueStatusResponse {
        state: Some(
            serde_json::to_value(active_entry.status)
                .ok()
                .and_then(|value| value.as_str().map(ToOwned::to_owned))
                .unwrap_or_else(|| "unknown".into()),
        ),
        position,
        lane_owner_item_id,
        lane_target_ref: Some(active_entry.target_ref),
        checkout_sync_blocked: false,
        checkout_sync_message: None,
    };

    let should_check_checkout = active_entry.status == ConvergenceQueueEntryStatus::Head
        && (item.approval_state == ApprovalState::Granted
            || evaluation.next_recommended_action
                == RecommendedAction::FinalizePreparedConvergence);
    if should_check_checkout {
        match checkout_sync_status(FsPath::new(&project.path), &revision.target_ref)
            .await
            .map_err(git_to_internal)?
        {
            CheckoutSyncStatus::Ready => {}
            CheckoutSyncStatus::Blocked { message, .. } => {
                queue.checkout_sync_blocked = true;
                queue.checkout_sync_message = Some(message);
            }
        }
    }

    Ok(queue)
}

pub(super) async fn hydrate_convergence_validity(
    repo_path: &FsPath,
    convergences: Vec<Convergence>,
) -> Result<Vec<Convergence>, ApiError> {
    let mut hydrated = Vec::with_capacity(convergences.len());

    for mut convergence in convergences {
        convergence.target_head_valid = compute_target_head_valid(repo_path, &convergence).await?;
        hydrated.push(convergence);
    }

    Ok(hydrated)
}

pub(super) async fn compute_target_head_valid(
    repo_path: &FsPath,
    convergence: &Convergence,
) -> Result<Option<bool>, ApiError> {
    let resolved = resolve_ref_oid(repo_path, &convergence.target_ref)
        .await
        .map_err(|err| ApiError::from(UseCaseError::Internal(err.to_string())))?;

    Ok(convergence.target_head_valid_for_resolved_oid(resolved.as_deref()))
}

pub(super) async fn ensure_reachable_seed(
    repo_path: &FsPath,
    seed_name: &str,
    commit_oid: &str,
) -> Result<(), ApiError> {
    let reachable = is_commit_reachable_from_any_ref(repo_path, commit_oid)
        .await
        .map_err(git_to_internal)?;

    if !reachable {
        return Err(UseCaseError::RevisionSeedUnreachable(seed_name.into()).into());
    }

    Ok(())
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
        "item",
        item.id,
        serde_json::json!({ "done_reason": item.lifecycle.done_reason() }),
    )
    .await?;
    let detail = load_item_detail(&state, project_id, item.id).await?;
    Ok(Json(detail))
}

pub(super) async fn build_superseding_revision(
    state: &AppState,
    project: &Project,
    item: &Item,
    current_revision: &ItemRevision,
    jobs: &[Job],
    request: ReviseItemRequest,
) -> Result<ItemRevision, ApiError> {
    let target_ref = normalize_target_ref(
        request
            .target_ref
            .as_deref()
            .unwrap_or(current_revision.target_ref.as_str()),
    )?;
    ensure_git_valid_target_ref(&target_ref).await?;
    let paths = refresh_project_mirror(state, project).await?;
    let repo_path = paths.mirror_git_dir.as_path();
    let derived_target_head = resolve_ref_oid(repo_path, &target_ref)
        .await
        .map_err(git_to_internal)?
        .ok_or_else(|| UseCaseError::TargetRefUnresolved(target_ref.clone()))?;

    let seed_commit_oid = if let Some(seed_commit_oid) = request.seed_commit_oid {
        ensure_reachable_seed(repo_path, "seed_commit_oid", &seed_commit_oid).await?;
        Some(seed_commit_oid)
    } else {
        current_authoring_head_for_revision_with_workspace(state, current_revision, jobs)
            .await?
            .or_else(|| current_revision.seed_commit_oid.clone())
    };
    let seed_target_commit_oid = if let Some(seed_target_commit_oid) =
        request.seed_target_commit_oid
    {
        ensure_reachable_seed(repo_path, "seed_target_commit_oid", &seed_target_commit_oid).await?;
        Some(seed_target_commit_oid)
    } else {
        Some(derived_target_head)
    };
    let approval_policy = request
        .approval_policy
        .unwrap_or(current_revision.approval_policy);
    let policy_snapshot = build_superseding_policy_snapshot(current_revision, approval_policy);

    Ok(ItemRevision {
        id: ingot_domain::ids::ItemRevisionId::new(),
        item_id: item.id,
        revision_no: current_revision.revision_no + 1,
        title: request.title.unwrap_or(current_revision.title.clone()),
        description: request
            .description
            .unwrap_or(current_revision.description.clone()),
        acceptance_criteria: request
            .acceptance_criteria
            .unwrap_or(current_revision.acceptance_criteria.clone()),
        target_ref,
        approval_policy,
        policy_snapshot,
        template_map_snapshot: default_template_map_snapshot(),
        seed_commit_oid,
        seed_target_commit_oid,
        supersedes_revision_id: Some(current_revision.id),
        created_at: Utc::now(),
    })
}

pub(super) fn build_superseding_policy_snapshot(
    current_revision: &ItemRevision,
    approval_policy: ApprovalPolicy,
) -> serde_json::Value {
    match rework_budgets_from_policy_snapshot(&current_revision.policy_snapshot) {
        Some((candidate_rework_budget, integration_rework_budget)) => default_policy_snapshot(
            approval_policy,
            candidate_rework_budget,
            integration_rework_budget,
        ),
        None => {
            let mut policy_snapshot = current_revision.policy_snapshot.clone();
            if let Some(object) = policy_snapshot.as_object_mut() {
                object.insert(
                    "approval_policy".into(),
                    serde_json::to_value(approval_policy)
                        .expect("approval policy should serialize into JSON"),
                );
            }
            policy_snapshot
        }
    }
}

pub(super) async fn ensure_authoring_workspace(
    state: &AppState,
    project: &Project,
    revision: &ItemRevision,
    job: &Job,
) -> Result<Workspace, ApiError> {
    let now = Utc::now();
    let paths = refresh_project_mirror(state, project).await?;
    let existing = state
        .db
        .find_authoring_workspace_for_revision(revision.id)
        .await
        .map_err(repo_to_internal)?;
    let workspace_exists = existing.is_some();
    let workspace = ensure_authoring_workspace_state(
        existing,
        project.id,
        paths.mirror_git_dir.as_path(),
        paths.worktree_root.as_path(),
        revision,
        job,
        now,
    )
    .await
    .map_err(workspace_to_api_error)?;

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

#[allow(dead_code)]
pub(super) async fn prepare_convergence_workspace(
    state: &AppState,
    project: &Project,
    item: &Item,
    revision: &ItemRevision,
    source_workspace: &Workspace,
    source_head_commit_oid: &str,
) -> Result<Convergence, ApiError> {
    let paths = refresh_project_mirror(state, project).await?;
    let repo_path = paths.mirror_git_dir.as_path();
    let input_target_commit_oid = resolve_ref_oid(repo_path, &revision.target_ref)
        .await
        .map_err(git_to_internal)?
        .ok_or_else(|| UseCaseError::TargetRefUnresolved(revision.target_ref.clone()))?;

    let integration_workspace_id = WorkspaceId::new();
    let integration_workspace_path = paths
        .worktree_root
        .join(integration_workspace_id.to_string());
    let integration_workspace_ref = format!("refs/ingot/workspaces/{integration_workspace_id}");
    let now = Utc::now();
    let mut integration_workspace = Workspace {
        id: integration_workspace_id,
        project_id: project.id,
        kind: WorkspaceKind::Integration,
        strategy: ingot_domain::workspace::WorkspaceStrategy::Worktree,
        path: integration_workspace_path.display().to_string(),
        created_for_revision_id: Some(revision.id),
        parent_workspace_id: Some(source_workspace.id),
        target_ref: Some(revision.target_ref.clone()),
        workspace_ref: Some(integration_workspace_ref.clone()),
        base_commit_oid: Some(input_target_commit_oid.clone()),
        head_commit_oid: Some(input_target_commit_oid.clone()),
        retention_policy: ingot_domain::workspace::RetentionPolicy::Persistent,
        status: ingot_domain::workspace::WorkspaceStatus::Provisioning,
        current_job_id: None,
        created_at: now,
        updated_at: now,
    };
    state
        .db
        .create_workspace(&integration_workspace)
        .await
        .map_err(repo_to_internal)?;

    let provisioned = provision_integration_workspace(
        repo_path,
        &integration_workspace_path,
        &integration_workspace_ref,
        &input_target_commit_oid,
    )
    .await
    .map_err(workspace_to_api_error)?;
    integration_workspace.path = provisioned.workspace_path.display().to_string();
    integration_workspace.workspace_ref = Some(provisioned.workspace_ref);
    integration_workspace.head_commit_oid = Some(provisioned.head_commit_oid);
    integration_workspace.status = ingot_domain::workspace::WorkspaceStatus::Busy;
    integration_workspace.updated_at = Utc::now();
    state
        .db
        .update_workspace(&integration_workspace)
        .await
        .map_err(repo_to_internal)?;

    let mut convergence = Convergence {
        id: ingot_domain::ids::ConvergenceId::new(),
        project_id: project.id,
        item_id: item.id,
        item_revision_id: revision.id,
        source_workspace_id: source_workspace.id,
        source_head_commit_oid: source_head_commit_oid.into(),
        target_ref: revision.target_ref.clone(),
        strategy: ingot_domain::convergence::ConvergenceStrategy::RebaseThenFastForward,
        target_head_valid: Some(true),
        created_at: now,
        state: ingot_domain::convergence::ConvergenceState::Running {
            integration_workspace_id: integration_workspace.id,
            input_target_commit_oid: input_target_commit_oid.clone(),
        },
    };
    state
        .db
        .create_convergence(&convergence)
        .await
        .map_err(repo_to_internal)?;
    append_activity(
        state,
        project.id,
        ActivityEventType::ConvergenceStarted,
        "convergence",
        convergence.id,
        serde_json::json!({ "item_id": item.id }),
    )
    .await?;

    let mut operation = GitOperation {
        id: ingot_domain::ids::GitOperationId::new(),
        project_id: project.id,
        operation_kind: OperationKind::PrepareConvergenceCommit,
        entity_type: GitEntityType::Convergence,
        entity_id: convergence.id.to_string(),
        workspace_id: Some(integration_workspace.id),
        ref_name: integration_workspace.workspace_ref.clone(),
        expected_old_oid: Some(input_target_commit_oid.clone()),
        new_oid: None,
        commit_oid: None,
        status: GitOperationStatus::Planned,
        metadata: None,
        created_at: now,
        completed_at: None,
    };
    state
        .db
        .create_git_operation(&operation)
        .await
        .map_err(repo_to_internal)?;
    append_activity(
        state,
        project.id,
        ActivityEventType::GitOperationPlanned,
        "git_operation",
        operation.id,
        serde_json::json!({ "operation_kind": operation.operation_kind, "entity_id": operation.entity_id }),
    )
    .await?;

    let source_base_commit_oid = effective_authoring_base_commit_oid(state, revision)
        .await?
        .ok_or_else(|| {
            ApiError::UseCase(UseCaseError::Internal(
                "convergence requires a bound authoring base commit".into(),
            ))
        })?;
    let source_commit_oids =
        list_commits_oldest_first(repo_path, &source_base_commit_oid, source_head_commit_oid)
            .await
            .map_err(git_to_internal)?;
    operation.metadata = Some(serde_json::json!({
        "source_commit_oids": source_commit_oids,
        "prepared_commit_oids": [],
    }));
    state
        .db
        .update_git_operation(&operation)
        .await
        .map_err(repo_to_internal)?;

    let integration_workspace_dir = PathBuf::from(&integration_workspace.path);
    let mut prepared_tip = input_target_commit_oid.clone();
    let mut prepared_commit_oids = Vec::with_capacity(source_commit_oids.len());

    for source_commit_oid in &source_commit_oids {
        if let Err(error) =
            cherry_pick_no_commit(&integration_workspace_dir, source_commit_oid).await
        {
            let _ = abort_cherry_pick(&integration_workspace_dir).await;
            integration_workspace.status = ingot_domain::workspace::WorkspaceStatus::Error;
            integration_workspace.updated_at = Utc::now();
            let _ = state.db.update_workspace(&integration_workspace).await;

            convergence.transition_to_conflicted(error.to_string(), Utc::now());
            let _ = state.db.update_convergence(&convergence).await;
            let mut escalated_item = item.clone();
            escalated_item.escalation = Escalation::OperatorRequired {
                reason: EscalationReason::ConvergenceConflict,
            };
            escalated_item.updated_at = Utc::now();
            let _ = state.db.update_item(&escalated_item).await;
            let _ = append_activity(
                state,
                project.id,
                ActivityEventType::ConvergenceConflicted,
                "convergence",
                convergence.id,
                serde_json::json!({ "item_id": item.id }),
            )
            .await;
            let _ = append_activity(
                state,
                project.id,
                ActivityEventType::ItemEscalated,
                "item",
                item.id,
                serde_json::json!({ "reason": EscalationReason::ConvergenceConflict }),
            )
            .await;

            operation.status = GitOperationStatus::Failed;
            operation.completed_at = Some(Utc::now());
            operation.metadata = Some(serde_json::json!({
                "source_commit_oids": source_commit_oids,
                "prepared_commit_oids": prepared_commit_oids,
            }));
            let _ = state.db.update_git_operation(&operation).await;

            return Err(ApiError::Conflict {
                code: "convergence_conflicted",
                message: "Convergence replay conflicted".into(),
            });
        }

        let has_replay_changes = working_tree_has_changes(&integration_workspace_dir)
            .await
            .map_err(git_to_internal)?;
        if !has_replay_changes {
            continue;
        }

        let original_message = match commit_message(repo_path, source_commit_oid).await {
            Ok(message) => message,
            Err(error) => {
                integration_workspace.status = ingot_domain::workspace::WorkspaceStatus::Error;
                integration_workspace.updated_at = Utc::now();
                let _ = state.db.update_workspace(&integration_workspace).await;

                convergence.transition_to_failed(Some(error.to_string()), Utc::now());
                let _ = state.db.update_convergence(&convergence).await;

                let mut escalated_item = item.clone();
                escalated_item.escalation = Escalation::OperatorRequired {
                    reason: EscalationReason::StepFailed,
                };
                escalated_item.updated_at = Utc::now();
                let _ = state.db.update_item(&escalated_item).await;

                operation.status = GitOperationStatus::Failed;
                operation.completed_at = Some(Utc::now());
                operation.metadata = Some(serde_json::json!({
                    "source_commit_oids": source_commit_oids,
                    "prepared_commit_oids": prepared_commit_oids,
                }));
                let _ = state.db.update_git_operation(&operation).await;

                let _ = append_activity(
                    state,
                    project.id,
                    ActivityEventType::ConvergenceFailed,
                    "convergence",
                    convergence.id,
                    serde_json::json!({ "item_id": item.id, "summary": error.to_string() }),
                )
                .await;
                let _ = append_activity(
                    state,
                    project.id,
                    ActivityEventType::ItemEscalated,
                    "item",
                    item.id,
                    serde_json::json!({ "reason": EscalationReason::StepFailed }),
                )
                .await;

                return Err(git_to_internal(error));
            }
        };
        let next_prepared_tip = match create_daemon_convergence_commit(
            &integration_workspace_dir,
            &original_message,
            &ConvergenceCommitTrailers {
                operation_id: operation.id,
                item_id: item.id,
                revision_no: revision.revision_no,
                convergence_id: convergence.id,
                source_commit_oid: source_commit_oid.clone(),
            },
        )
        .await
        {
            Ok(prepared_tip) => prepared_tip,
            Err(error) => {
                integration_workspace.status = ingot_domain::workspace::WorkspaceStatus::Error;
                integration_workspace.updated_at = Utc::now();
                let _ = state.db.update_workspace(&integration_workspace).await;

                convergence.transition_to_failed(Some(error.to_string()), Utc::now());
                let _ = state.db.update_convergence(&convergence).await;

                let mut escalated_item = item.clone();
                escalated_item.escalation = Escalation::OperatorRequired {
                    reason: EscalationReason::StepFailed,
                };
                escalated_item.updated_at = Utc::now();
                let _ = state.db.update_item(&escalated_item).await;

                operation.status = GitOperationStatus::Failed;
                operation.completed_at = Some(Utc::now());
                operation.metadata = Some(serde_json::json!({
                    "source_commit_oids": source_commit_oids,
                    "prepared_commit_oids": prepared_commit_oids,
                }));
                let _ = state.db.update_git_operation(&operation).await;

                let _ = append_activity(
                    state,
                    project.id,
                    ActivityEventType::ConvergenceFailed,
                    "convergence",
                    convergence.id,
                    serde_json::json!({ "item_id": item.id, "summary": error.to_string() }),
                )
                .await;
                let _ = append_activity(
                    state,
                    project.id,
                    ActivityEventType::ItemEscalated,
                    "item",
                    item.id,
                    serde_json::json!({ "reason": EscalationReason::StepFailed }),
                )
                .await;

                return Err(git_to_internal(error));
            }
        };
        if let Some(workspace_ref) = integration_workspace.workspace_ref.as_deref() {
            if let Err(error) = ingot_git::commands::git(
                repo_path,
                &["update-ref", workspace_ref, &next_prepared_tip],
            )
            .await
            {
                integration_workspace.status = ingot_domain::workspace::WorkspaceStatus::Error;
                integration_workspace.updated_at = Utc::now();
                let _ = state.db.update_workspace(&integration_workspace).await;

                convergence.transition_to_failed(Some(error.to_string()), Utc::now());
                let _ = state.db.update_convergence(&convergence).await;

                let mut escalated_item = item.clone();
                escalated_item.escalation = Escalation::OperatorRequired {
                    reason: EscalationReason::StepFailed,
                };
                escalated_item.updated_at = Utc::now();
                let _ = state.db.update_item(&escalated_item).await;

                operation.status = GitOperationStatus::Failed;
                operation.completed_at = Some(Utc::now());
                operation.metadata = Some(serde_json::json!({
                    "source_commit_oids": source_commit_oids,
                    "prepared_commit_oids": prepared_commit_oids,
                }));
                let _ = state.db.update_git_operation(&operation).await;

                let _ = append_activity(
                    state,
                    project.id,
                    ActivityEventType::ConvergenceFailed,
                    "convergence",
                    convergence.id,
                    serde_json::json!({ "item_id": item.id, "summary": error.to_string() }),
                )
                .await;
                let _ = append_activity(
                    state,
                    project.id,
                    ActivityEventType::ItemEscalated,
                    "item",
                    item.id,
                    serde_json::json!({ "reason": EscalationReason::StepFailed }),
                )
                .await;

                return Err(git_to_internal(error));
            }
        }
        prepared_tip = next_prepared_tip;
        prepared_commit_oids.push(prepared_tip.clone());
    }

    integration_workspace.head_commit_oid = Some(prepared_tip.clone());
    integration_workspace.status = ingot_domain::workspace::WorkspaceStatus::Ready;
    integration_workspace.updated_at = Utc::now();
    state
        .db
        .update_workspace(&integration_workspace)
        .await
        .map_err(repo_to_internal)?;

    convergence.transition_to_prepared(prepared_tip.clone(), Some(Utc::now()));
    state
        .db
        .update_convergence(&convergence)
        .await
        .map_err(repo_to_internal)?;

    operation.new_oid = Some(prepared_tip.clone());
    operation.commit_oid = Some(prepared_tip);
    operation.status = GitOperationStatus::Applied;
    operation.completed_at = Some(Utc::now());
    operation.metadata = Some(serde_json::json!({
        "source_commit_oids": source_commit_oids,
        "prepared_commit_oids": prepared_commit_oids,
    }));
    state
        .db
        .update_git_operation(&operation)
        .await
        .map_err(repo_to_internal)?;

    Ok(convergence)
}

pub(crate) fn load_effective_config(project: Option<&Project>) -> Result<IngotConfig, ApiError> {
    let project_path = project.map(project_config_path);
    load_config(global_config_path().as_path(), project_path.as_deref()).map_err(|error| {
        ApiError::BadRequest {
            code: "config_invalid",
            message: error.to_string(),
        }
    })
}

pub(super) async fn current_authoring_head_for_revision_with_workspace(
    state: &AppState,
    revision: &ItemRevision,
    jobs: &[Job],
) -> Result<Option<String>, ApiError> {
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
) -> Result<Option<String>, ApiError> {
    let workspace = state
        .db
        .find_authoring_workspace_for_revision(revision.id)
        .await
        .map_err(repo_to_internal)?;
    Ok(ingot_usecases::dispatch::effective_authoring_base_commit_oid(revision, workspace.as_ref()))
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::compute_target_head_valid;
    use chrono::Utc;
    use ingot_domain::ids::{ItemId, ItemRevisionId, ProjectId};
    use ingot_test_support::fixtures::ConvergenceBuilder;
    use ingot_test_support::git::{
        git_output as support_git_output, run_git as support_git,
        temp_git_repo as support_temp_git_repo, write_file as support_write_file,
    };
    use uuid::Uuid;

    fn temp_git_repo() -> PathBuf {
        support_temp_git_repo("ingot-http-api")
    }

    fn git(path: &std::path::Path, args: &[&str]) {
        support_git(path, args);
    }

    fn git_output(path: &std::path::Path, args: &[&str]) -> String {
        support_git_output(path, args)
    }

    fn write_file(path: &std::path::Path, contents: &str) {
        support_write_file(path, contents);
    }

    #[tokio::test]
    async fn target_head_valid_tracks_ref_movement() {
        let repo = temp_git_repo();
        let first = git_output(&repo, &["rev-parse", "HEAD"]);
        let mut convergence = ConvergenceBuilder::new(
            ProjectId::from_uuid(Uuid::nil()),
            ItemId::from_uuid(Uuid::nil()),
            ItemRevisionId::from_uuid(Uuid::nil()),
        )
        .target_head_valid(true)
        .created_at(Utc::now())
        .input_target_commit_oid(first.clone())
        .build();
        convergence.target_ref = "refs/heads/main".into();

        let valid = compute_target_head_valid(&repo, &convergence)
            .await
            .expect("compute validity");
        assert_eq!(valid, Some(true));

        write_file(&repo.join("tracked.txt"), "next");
        git(&repo, &["add", "tracked.txt"]);
        git(&repo, &["commit", "-m", "next"]);

        let stale = compute_target_head_valid(&repo, &convergence)
            .await
            .expect("compute stale validity");
        assert_eq!(stale, Some(false));
    }
}
