use super::item_projection::{
    ItemRuntimeSnapshot, hydrate_convergence_validity, load_item_detail, load_item_runtime_snapshot,
};
use super::items::build_superseding_revision;
use super::support::*;
use super::types::*;
use super::*;
use ingot_git::commands::{
    FinalizeTargetRefOutcome, finalize_target_ref as finalize_target_ref_in_repo,
};
use ingot_git::project_repo::{
    CheckoutFinalizationStatus, CheckoutSyncStatus, checkout_finalization_status,
    checkout_sync_status, sync_checkout_to_commit,
};
use ingot_usecases::convergence::{
    ApprovalFinalizeReadiness, CheckoutFinalizationReadiness, FinalizationTarget,
    FinalizePreparedTrigger, FinalizeTargetRefResult, PreparedConvergenceFinalizePort,
};

#[derive(Clone)]
pub(super) struct HttpConvergencePort {
    pub(super) state: AppState,
}

fn approval_finalize_readiness(
    prepared_convergence: Option<Convergence>,
    queue_entry: Option<ConvergenceQueueEntry>,
    resolved_target_oid: Option<&CommitOid>,
) -> ApprovalFinalizeReadiness {
    let Some(convergence) = prepared_convergence else {
        return ApprovalFinalizeReadiness::MissingPreparedConvergence;
    };

    let target_valid = convergence
        .state
        .input_target_commit_oid()
        .zip(convergence.state.prepared_commit_oid())
        .is_some_and(|(input_target_commit_oid, prepared_commit_oid)| {
            resolved_target_oid == Some(input_target_commit_oid)
                || resolved_target_oid == Some(prepared_commit_oid)
        });
    if !target_valid {
        return ApprovalFinalizeReadiness::PreparedConvergenceStale;
    }

    let Some(queue_entry) = queue_entry else {
        return ApprovalFinalizeReadiness::ConvergenceNotQueued;
    };
    if queue_entry.status != ConvergenceQueueEntryStatus::Head {
        return ApprovalFinalizeReadiness::ConvergenceNotLaneHead;
    }

    ApprovalFinalizeReadiness::Ready {
        convergence: Box::new(convergence),
        queue_entry,
    }
}

async fn reconcile_checkout_sync_state_http(
    state: &AppState,
    project: &Project,
    item_id: ItemId,
    revision: &ItemRevision,
) -> Result<CheckoutSyncStatus, UseCaseError> {
    let mut item = state
        .db
        .get_item(item_id)
        .await
        .map_err(UseCaseError::Repository)?;
    let status = checkout_sync_status(&project.path, &revision.target_ref)
        .await
        .map_err(git_to_internal)
        .map_err(api_to_usecase_error)?;
    let checkout_sync_blocked = matches!(
        item.escalation,
        Escalation::OperatorRequired {
            reason: EscalationReason::CheckoutSyncBlocked
        }
    );
    match &status {
        CheckoutSyncStatus::Ready => {
            if checkout_sync_blocked {
                item.escalation = Escalation::None;
                item.updated_at = Utc::now();
                state
                    .db
                    .update_item(&item)
                    .await
                    .map_err(UseCaseError::Repository)?;
                append_activity(
                    state,
                    project.id,
                    ActivityEventType::CheckoutSyncCleared,
                    ActivitySubject::Item(item.id),
                    serde_json::json!({}),
                )
                .await
                .map_err(api_to_usecase_error)?;
                append_activity(
                    state,
                    project.id,
                    ActivityEventType::ItemEscalationCleared,
                    ActivitySubject::Item(item.id),
                    serde_json::json!({ "reason": "checkout_sync_ready" }),
                )
                .await
                .map_err(api_to_usecase_error)?;
            }
        }
        CheckoutSyncStatus::Blocked { message, .. } => {
            if !checkout_sync_blocked {
                item.escalation = Escalation::OperatorRequired {
                    reason: EscalationReason::CheckoutSyncBlocked,
                };
                item.updated_at = Utc::now();
                state
                    .db
                    .update_item(&item)
                    .await
                    .map_err(UseCaseError::Repository)?;
                append_activity(
                    state,
                    project.id,
                    ActivityEventType::CheckoutSyncBlocked,
                    ActivitySubject::Item(item.id),
                    serde_json::json!({ "message": message }),
                )
                .await
                .map_err(api_to_usecase_error)?;
            }
        }
    }
    Ok(status)
}

impl ConvergenceCommandPort for HttpConvergencePort {
    fn load_queue_prepare_context(
        &self,
        project_id: ProjectId,
        item_id: ItemId,
    ) -> impl std::future::Future<
        Output = Result<ingot_domain::ports::ConvergenceQueuePrepareContext, UseCaseError>,
    > + Send {
        let state = self.state.clone();
        async move {
            let project = state
                .db
                .get_project(project_id)
                .await
                .map_err(repo_to_project)
                .map_err(api_to_usecase_error)?;
            let paths = refresh_project_mirror(&state, &project)
                .await
                .map_err(api_to_usecase_error)?;
            let item = state
                .db
                .get_item(item_id)
                .await
                .map_err(repo_to_item)
                .map_err(api_to_usecase_error)?;
            let ItemRuntimeSnapshot {
                current_revision,
                jobs,
                findings,
                convergences,
            } = load_item_runtime_snapshot(&state, paths.mirror_git_dir.as_path(), &item)
                .await
                .map_err(api_to_usecase_error)?;
            let active_queue_entry = state
                .db
                .find_active_queue_entry_for_revision(current_revision.id)
                .await
                .map_err(UseCaseError::Repository)?;
            let lane_head = state
                .db
                .find_queue_head(project.id, &current_revision.target_ref)
                .await
                .map_err(UseCaseError::Repository)?;

            Ok(ingot_domain::ports::ConvergenceQueuePrepareContext {
                project,
                item,
                revision: current_revision,
                jobs,
                findings,
                convergences,
                active_queue_entry,
                lane_head,
            })
        }
    }

    fn create_queue_entry(
        &self,
        queue_entry: &ConvergenceQueueEntry,
    ) -> impl std::future::Future<Output = Result<(), UseCaseError>> + Send {
        let state = self.state.clone();
        let queue_entry = queue_entry.clone();
        async move {
            state
                .db
                .create_queue_entry(&queue_entry)
                .await
                .map_err(UseCaseError::Repository)?;
            Ok(())
        }
    }

    fn update_queue_entry(
        &self,
        queue_entry: &ConvergenceQueueEntry,
    ) -> impl std::future::Future<Output = Result<(), UseCaseError>> + Send {
        let state = self.state.clone();
        let queue_entry = queue_entry.clone();
        async move {
            state
                .db
                .update_queue_entry(&queue_entry)
                .await
                .map_err(UseCaseError::Repository)?;
            Ok(())
        }
    }

    fn append_activity(
        &self,
        activity: &Activity,
    ) -> impl std::future::Future<Output = Result<(), UseCaseError>> + Send {
        let state = self.state.clone();
        let activity = activity.clone();
        async move {
            state
                .db
                .append_activity(&activity)
                .await
                .map_err(UseCaseError::Repository)?;
            Ok(())
        }
    }

    fn load_approval_context(
        &self,
        project_id: ProjectId,
        item_id: ItemId,
    ) -> impl std::future::Future<
        Output = Result<ingot_usecases::convergence::ConvergenceApprovalContext, UseCaseError>,
    > + Send {
        let state = self.state.clone();
        async move {
            let project = state
                .db
                .get_project(project_id)
                .await
                .map_err(repo_to_project)
                .map_err(api_to_usecase_error)?;
            let paths = refresh_project_mirror(&state, &project)
                .await
                .map_err(api_to_usecase_error)?;
            let item = state
                .db
                .get_item(item_id)
                .await
                .map_err(repo_to_item)
                .map_err(api_to_usecase_error)?;
            if item.project_id != project_id {
                return Err(UseCaseError::ItemNotFound);
            }
            let revision = state
                .db
                .get_revision(item.current_revision_id)
                .await
                .map_err(UseCaseError::Repository)?;
            let jobs = state
                .db
                .list_jobs_by_item(item.id)
                .await
                .map_err(UseCaseError::Repository)?;
            let convergences = hydrate_convergence_validity(
                paths.mirror_git_dir.as_path(),
                state
                    .db
                    .list_convergences_by_item(item.id)
                    .await
                    .map_err(UseCaseError::Repository)?,
            )
            .await
            .map_err(api_to_usecase_error)?;
            let queue_entry = state
                .db
                .find_active_queue_entry_for_revision(revision.id)
                .await
                .map_err(UseCaseError::Repository)?;
            let revision_id = revision.id;
            let prepared_convergence = convergences
                .iter()
                .find(|convergence| {
                    convergence.item_revision_id == revision_id
                        && convergence.state.status()
                            == ingot_domain::convergence::ConvergenceStatus::Prepared
                })
                .cloned();
            let resolved_target_oid =
                resolve_ref_oid(paths.mirror_git_dir.as_path(), &revision.target_ref)
                    .await
                    .map_err(git_to_internal)
                    .map_err(api_to_usecase_error)?;
            let has_active_job = jobs
                .iter()
                .any(|job| job.item_revision_id == revision_id && job.state.is_active());
            let has_active_convergence = convergences.iter().any(|convergence| {
                convergence.item_revision_id == revision_id
                    && matches!(
                        convergence.state.status(),
                        ingot_domain::convergence::ConvergenceStatus::Queued
                            | ingot_domain::convergence::ConvergenceStatus::Running
                    )
            });

            Ok(ingot_usecases::convergence::ConvergenceApprovalContext {
                project,
                item,
                revision,
                has_active_job,
                has_active_convergence,
                finalize_readiness: approval_finalize_readiness(
                    prepared_convergence,
                    queue_entry,
                    resolved_target_oid.as_ref(),
                ),
            })
        }
    }

    fn update_item(
        &self,
        item: &Item,
    ) -> impl std::future::Future<Output = Result<(), UseCaseError>> + Send {
        let state = self.state.clone();
        let item = item.clone();
        async move {
            state
                .db
                .update_item(&item)
                .await
                .map_err(UseCaseError::Repository)?;
            Ok(())
        }
    }

    fn load_reject_approval_context(
        &self,
        project_id: ProjectId,
        item_id: ItemId,
    ) -> impl std::future::Future<
        Output = Result<ingot_usecases::convergence::RejectApprovalContext, UseCaseError>,
    > + Send {
        let state = self.state.clone();
        async move {
            let item = state
                .db
                .get_item(item_id)
                .await
                .map_err(repo_to_item)
                .map_err(api_to_usecase_error)?;
            if item.project_id != project_id {
                return Err(UseCaseError::ItemNotFound);
            }
            let revision = state
                .db
                .get_revision(item.current_revision_id)
                .await
                .map_err(UseCaseError::Repository)?;
            let jobs = state
                .db
                .list_jobs_by_item(item.id)
                .await
                .map_err(UseCaseError::Repository)?;
            let has_active_job = jobs
                .iter()
                .any(|job| job.item_revision_id == revision.id && job.state.is_active());
            let convergences = state
                .db
                .list_convergences_by_item(item.id)
                .await
                .map_err(UseCaseError::Repository)?;
            let has_active_convergence = convergences.iter().any(|convergence| {
                convergence.item_revision_id == revision.id
                    && matches!(
                        convergence.state.status(),
                        ingot_domain::convergence::ConvergenceStatus::Queued
                            | ingot_domain::convergence::ConvergenceStatus::Running
                    )
            });

            Ok(ingot_usecases::convergence::RejectApprovalContext {
                item,
                has_active_job,
                has_active_convergence,
            })
        }
    }

    fn teardown_reject_approval(
        &self,
        project_id: ProjectId,
        item_id: ItemId,
    ) -> impl std::future::Future<
        Output = Result<ingot_usecases::convergence::RejectApprovalTeardown, UseCaseError>,
    > + Send {
        let state = self.state.clone();
        async move {
            let project = state
                .db
                .get_project(project_id)
                .await
                .map_err(UseCaseError::Repository)?;
            let item = state
                .db
                .get_item(item_id)
                .await
                .map_err(UseCaseError::Repository)?;
            let revision = state
                .db
                .get_revision(item.current_revision_id)
                .await
                .map_err(UseCaseError::Repository)?;
            let teardown = teardown_revision_lane_state(&state, &project, item.id, &revision)
                .await
                .map_err(api_to_usecase_error)?;
            Ok(ingot_usecases::convergence::RejectApprovalTeardown {
                has_cancelled_convergence: teardown.has_cancelled_convergence(),
                has_cancelled_queue_entry: teardown.has_cancelled_queue_entry(),
                first_cancelled_convergence_id: teardown
                    .first_cancelled_convergence_id()
                    .map(ToOwned::to_owned),
                first_cancelled_queue_entry_id: teardown
                    .first_cancelled_queue_entry_id()
                    .map(ToOwned::to_owned),
            })
        }
    }

    fn apply_rejected_approval(
        &self,
        item: &Item,
        next_revision: &ItemRevision,
    ) -> impl std::future::Future<Output = Result<(), UseCaseError>> + Send {
        let state = self.state.clone();
        let item = item.clone();
        let next_revision = next_revision.clone();
        async move {
            state
                .db
                .create_revision(&next_revision)
                .await
                .map_err(UseCaseError::Repository)?;
            state
                .db
                .update_item(&item)
                .await
                .map_err(UseCaseError::Repository)?;
            Ok(())
        }
    }
}

impl PreparedConvergenceFinalizePort for HttpConvergencePort {
    fn find_or_create_finalize_operation(
        &self,
        operation: &GitOperation,
    ) -> impl std::future::Future<Output = Result<GitOperation, UseCaseError>> + Send {
        let db = self.state.db.clone();
        let operation = operation.clone();
        async move {
            ingot_usecases::convergence::find_or_create_finalize_operation(&db, &operation).await
        }
    }

    fn finalize_target_ref(
        &self,
        project: &Project,
        convergence: &Convergence,
    ) -> impl std::future::Future<Output = Result<FinalizeTargetRefResult, UseCaseError>> + Send
    {
        let state = self.state.clone();
        let project = project.clone();
        let convergence = convergence.clone();
        async move {
            let paths = refresh_project_mirror(&state, &project)
                .await
                .map_err(api_to_usecase_error)?;
            let prepared_commit_oid = convergence
                .state
                .prepared_commit_oid()
                .ok_or(UseCaseError::PreparedConvergenceMissing)?;
            let input_target_commit_oid = convergence
                .state
                .input_target_commit_oid()
                .ok_or(UseCaseError::PreparedConvergenceMissing)?;

            match finalize_target_ref_in_repo(
                paths.mirror_git_dir.as_path(),
                &convergence.target_ref,
                prepared_commit_oid,
                input_target_commit_oid,
            )
            .await
            .map_err(git_to_internal)
            .map_err(api_to_usecase_error)?
            {
                FinalizeTargetRefOutcome::AlreadyFinalized => {
                    Ok(FinalizeTargetRefResult::AlreadyFinalized)
                }
                FinalizeTargetRefOutcome::UpdatedNow => Ok(FinalizeTargetRefResult::UpdatedNow),
                FinalizeTargetRefOutcome::Stale => Ok(FinalizeTargetRefResult::Stale),
            }
        }
    }

    fn checkout_finalization_readiness(
        &self,
        project: &Project,
        item: &Item,
        revision: &ItemRevision,
        prepared_commit_oid: &CommitOid,
    ) -> impl std::future::Future<Output = Result<CheckoutFinalizationReadiness, UseCaseError>> + Send
    {
        let state = self.state.clone();
        let project = project.clone();
        let item = item.clone();
        let revision = revision.clone();
        let prepared_commit_oid = prepared_commit_oid.clone();
        async move {
            let readiness = match checkout_finalization_status(
                &project.path,
                &revision.target_ref,
                &prepared_commit_oid,
            )
            .await
            .map_err(git_to_internal)
            .map_err(api_to_usecase_error)?
            {
                CheckoutFinalizationStatus::Blocked { message, .. } => {
                    CheckoutFinalizationReadiness::Blocked { message }
                }
                CheckoutFinalizationStatus::NeedsSync => CheckoutFinalizationReadiness::NeedsSync,
                CheckoutFinalizationStatus::Synced => CheckoutFinalizationReadiness::Synced,
            };
            reconcile_checkout_sync_state_http(&state, &project, item.id, &revision).await?;
            Ok(readiness)
        }
    }

    fn sync_checkout_to_prepared_commit(
        &self,
        project: &Project,
        revision: &ItemRevision,
        prepared_commit_oid: &CommitOid,
    ) -> impl std::future::Future<Output = Result<(), UseCaseError>> + Send {
        let state = self.state.clone();
        let project = project.clone();
        let revision = revision.clone();
        let prepared_commit_oid = prepared_commit_oid.clone();
        async move {
            let paths = refresh_project_mirror(&state, &project)
                .await
                .map_err(api_to_usecase_error)?;
            sync_checkout_to_commit(
                &project.path,
                paths.mirror_git_dir.as_path(),
                &revision.target_ref,
                &prepared_commit_oid,
            )
            .await
            .map_err(git_to_internal)
            .map_err(api_to_usecase_error)?;
            Ok(())
        }
    }

    fn update_git_operation(
        &self,
        operation: &GitOperation,
    ) -> impl std::future::Future<Output = Result<(), UseCaseError>> + Send {
        let state = self.state.clone();
        let operation = operation.clone();
        async move {
            state
                .db
                .update_git_operation(&operation)
                .await
                .map_err(UseCaseError::Repository)?;
            Ok(())
        }
    }

    fn apply_successful_finalization(
        &self,
        trigger: FinalizePreparedTrigger,
        project: &Project,
        item: &Item,
        _revision: &ItemRevision,
        target: FinalizationTarget<'_>,
        operation: &GitOperation,
    ) -> impl std::future::Future<Output = Result<(), UseCaseError>> + Send {
        let state = self.state.clone();
        let project = project.clone();
        let mut item = item.clone();
        let mut convergence = target.convergence.clone();
        let mut queue_entry = target.queue_entry.clone();
        let operation = operation.clone();
        async move {
            let now = Utc::now();
            let final_commit_oid = operation
                .new_oid()
                .or(operation.commit_oid())
                .map(ToOwned::to_owned)
                .ok_or_else(|| {
                    UseCaseError::Internal(
                        "reconciled finalize_target_ref missing final commit oid".into(),
                    )
                })?;

            convergence
                .transition_to_finalized(final_commit_oid, now)
                .map_err(|error| UseCaseError::Internal(error.to_string()))?;
            state
                .db
                .update_convergence(&convergence)
                .await
                .map_err(UseCaseError::Repository)?;

            queue_entry.status = ConvergenceQueueEntryStatus::Released;
            queue_entry.released_at = Some(now);
            queue_entry.updated_at = now;
            state
                .db
                .update_queue_entry(&queue_entry)
                .await
                .map_err(UseCaseError::Repository)?;

            let (resolution_source, approval_state) = match trigger {
                FinalizePreparedTrigger::ApprovalCommand => {
                    (ResolutionSource::ApprovalCommand, ApprovalState::Approved)
                }
                FinalizePreparedTrigger::SystemCommand => {
                    (ResolutionSource::SystemCommand, ApprovalState::NotRequired)
                }
            };
            item.lifecycle = Lifecycle::Done {
                reason: DoneReason::Completed,
                source: resolution_source,
                closed_at: now,
            };
            item.approval_state = approval_state;
            item.escalation = Escalation::None;
            item.updated_at = now;
            state
                .db
                .update_item(&item)
                .await
                .map_err(UseCaseError::Repository)?;

            if let Some(workspace_id) = convergence.state.integration_workspace_id() {
                let mut workspace = state
                    .db
                    .get_workspace(workspace_id)
                    .await
                    .map_err(UseCaseError::Repository)?;
                if workspace.state.status() != WorkspaceStatus::Abandoned {
                    let mirror_git_dir = project_paths(&state, &project).mirror_git_dir;
                    remove_workspace(mirror_git_dir.as_path(), &workspace.path)
                        .await
                        .map_err(workspace_to_api_error)
                        .map_err(api_to_usecase_error)?;
                    workspace.mark_abandoned(now);
                    state
                        .db
                        .update_workspace(&workspace)
                        .await
                        .map_err(UseCaseError::Repository)?;
                }
            }

            append_activity(
                &state,
                project.id,
                ActivityEventType::ConvergenceFinalized,
                ActivitySubject::Convergence(convergence.id),
                serde_json::json!({ "item_id": item.id }),
            )
            .await
            .map_err(api_to_usecase_error)?;

            Ok(())
        }
    }
}

impl ConvergenceSystemActionPort for HttpConvergencePort {
    async fn load_system_action_projects(
        &self,
    ) -> Result<Vec<ingot_usecases::convergence::SystemActionProjectState>, UseCaseError> {
        Err(UseCaseError::Internal(
            "http convergence port does not load system actions".into(),
        ))
    }

    async fn promote_queue_heads(&self, _project_id: ProjectId) -> Result<(), UseCaseError> {
        Err(UseCaseError::Internal(
            "http convergence port does not promote queue heads".into(),
        ))
    }

    async fn prepare_queue_head_convergence(
        &self,
        _project: &Project,
        _state: &ingot_usecases::convergence::SystemActionItemState,
        _queue_entry: &ConvergenceQueueEntry,
    ) -> Result<(), UseCaseError> {
        Err(UseCaseError::Internal(
            "http convergence port does not prepare queue heads".into(),
        ))
    }

    async fn invalidate_prepared_convergence(
        &self,
        _project_id: ProjectId,
        _item_id: ItemId,
    ) -> Result<(), UseCaseError> {
        Err(UseCaseError::Internal(
            "http convergence port does not invalidate prepared convergence".into(),
        ))
    }

    async fn auto_finalize_prepared_convergence(
        &self,
        _project_id: ProjectId,
        _item_id: ItemId,
    ) -> Result<bool, UseCaseError> {
        Err(UseCaseError::Internal(
            "http convergence port does not auto-finalize convergence".into(),
        ))
    }

    async fn auto_queue_convergence(
        &self,
        _project_id: ProjectId,
        _item_id: ItemId,
    ) -> Result<bool, UseCaseError> {
        Err(UseCaseError::Internal(
            "http convergence port does not auto-queue convergence".into(),
        ))
    }
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
    ConvergenceService::new(HttpConvergencePort {
        state: state.clone(),
    })
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
    ConvergenceService::new(HttpConvergencePort {
        state: state.clone(),
    })
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
    let teardown = ConvergenceService::new(HttpConvergencePort {
        state: state.clone(),
    })
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    use chrono::Utc;
    use ingot_domain::ids::{ItemId, ItemRevisionId, ProjectId};
    use ingot_domain::item::ApprovalState;
    use ingot_test_support::fixtures::{ItemBuilder, ProjectBuilder, RevisionBuilder};
    use ingot_test_support::git::{
        git_output as support_git_output, temp_git_repo as support_temp_git_repo,
    };
    use ingot_usecases::UseCaseError;
    use ingot_usecases::convergence::ConvergenceCommandPort;

    use super::super::test_helpers::test_app_state;

    fn temp_git_repo() -> PathBuf {
        support_temp_git_repo("ingot-http-api")
    }

    fn git_output(path: &std::path::Path, args: &[&str]) -> String {
        support_git_output(path, args)
    }

    #[tokio::test]
    async fn convergence_port_maps_missing_project_to_project_not_found() {
        let state = test_app_state().await;
        let error = HttpConvergencePort {
            state: state.clone(),
        }
        .load_queue_prepare_context(ProjectId::new(), ItemId::new())
        .await
        .expect_err("missing project should fail");

        assert!(matches!(error, UseCaseError::ProjectNotFound));
    }

    #[tokio::test]
    async fn convergence_port_rejects_cross_project_approval_context() {
        let state = test_app_state().await;
        let repo_a = temp_git_repo();
        let repo_b = temp_git_repo();
        let project_a = ProjectBuilder::new(&repo_a)
            .id(ProjectId::new())
            .name("A")
            .created_at(Utc::now())
            .build();
        let mut project_b = ProjectBuilder::new(&repo_b)
            .id(ProjectId::new())
            .name("B")
            .created_at(Utc::now())
            .build();
        project_b.color = "#111".into();
        state
            .db
            .create_project(&project_a)
            .await
            .expect("project a");
        state
            .db
            .create_project(&project_b)
            .await
            .expect("project b");

        let head = git_output(&repo_b, &["rev-parse", "HEAD"]);
        let item = ItemBuilder::new(project_b.id, ItemRevisionId::new())
            .approval_state(ApprovalState::Pending)
            .build();
        let revision = RevisionBuilder::new(item.id)
            .id(item.current_revision_id)
            .explicit_seed(&head)
            .build();
        state
            .db
            .create_item_with_revision(&item, &revision)
            .await
            .expect("item b");

        let error = HttpConvergencePort {
            state: state.clone(),
        }
        .load_approval_context(project_a.id, item.id)
        .await
        .expect_err("cross-project item should fail");

        assert!(matches!(error, UseCaseError::ItemNotFound));
    }
}
