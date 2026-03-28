use super::deps::*;
use super::infra_ports::HttpInfraAdapter;
use super::item_projection::{
    ItemRuntimeSnapshot, hydrate_convergence_validity, load_item_runtime_snapshot,
};
use super::support::{
    activity::append_activity,
    errors::{api_to_usecase_error, repo_to_item, repo_to_project},
};
use chrono::Utc;
use ingot_domain::convergence::ConvergenceStatus;
use ingot_domain::ids::ItemRevisionId;
use ingot_git::commands::FinalizeTargetRefOutcome;
use ingot_git::project_repo::{CheckoutFinalizationStatus, CheckoutSyncStatus};
use ingot_usecases::convergence::{
    ApprovalFinalizeReadiness, CheckoutFinalizationReadiness, ConvergenceQueuePrepareContext,
    FinalizationTarget, FinalizePreparedTrigger, FinalizeTargetRefResult,
    PreparedConvergenceFinalizePort,
};

#[derive(Clone)]
pub(super) struct HttpConvergencePort {
    pub(super) state: AppState,
}

impl HttpConvergencePort {
    pub(super) fn new(state: &AppState) -> Self {
        Self {
            state: state.clone(),
        }
    }
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

fn ensure_item_in_project(item: &Item, project_id: ProjectId) -> Result<(), UseCaseError> {
    if item.project_id == project_id {
        Ok(())
    } else {
        Err(UseCaseError::ItemNotFound)
    }
}

fn has_active_job_for_revision(jobs: &[Job], revision_id: ItemRevisionId) -> bool {
    jobs.iter()
        .any(|job| job.item_revision_id == revision_id && job.state.is_active())
}

fn has_active_convergence_for_revision(
    convergences: &[Convergence],
    revision_id: ItemRevisionId,
) -> bool {
    convergences.iter().any(|convergence| {
        convergence.item_revision_id == revision_id
            && matches!(
                convergence.state.status(),
                ConvergenceStatus::Queued | ConvergenceStatus::Running
            )
    })
}

fn prepared_convergence_for_revision(
    convergences: &[Convergence],
    revision_id: ItemRevisionId,
) -> Option<Convergence> {
    convergences
        .iter()
        .find(|convergence| {
            convergence.item_revision_id == revision_id
                && convergence.state.status() == ConvergenceStatus::Prepared
        })
        .cloned()
}

fn unsupported_http_system_action<T>(message: &'static str) -> Result<T, UseCaseError> {
    Err(UseCaseError::Internal(message.into()))
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
    let status = HttpInfraAdapter::new(state)
        .checkout_sync_status(project, &revision.target_ref)
        .await
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
    ) -> impl std::future::Future<Output = Result<ConvergenceQueuePrepareContext, UseCaseError>> + Send
    {
        let state = self.state.clone();
        async move {
            let project = state
                .db
                .get_project(project_id)
                .await
                .map_err(repo_to_project)
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
            } = load_item_runtime_snapshot(&state, project.id, &item)
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

            Ok(ConvergenceQueuePrepareContext {
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
        let db = self.state.db.clone();
        let queue_entry = queue_entry.clone();
        async move {
            db.create_queue_entry(&queue_entry)
                .await
                .map_err(UseCaseError::Repository)
        }
    }

    fn update_queue_entry(
        &self,
        queue_entry: &ConvergenceQueueEntry,
    ) -> impl std::future::Future<Output = Result<(), UseCaseError>> + Send {
        let db = self.state.db.clone();
        let queue_entry = queue_entry.clone();
        async move {
            db.update_queue_entry(&queue_entry)
                .await
                .map_err(UseCaseError::Repository)
        }
    }

    fn append_activity(
        &self,
        activity: &Activity,
    ) -> impl std::future::Future<Output = Result<(), UseCaseError>> + Send {
        let db = self.state.db.clone();
        let activity = activity.clone();
        async move {
            db.append_activity(&activity)
                .await
                .map_err(UseCaseError::Repository)
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
            let item = state
                .db
                .get_item(item_id)
                .await
                .map_err(repo_to_item)
                .map_err(api_to_usecase_error)?;
            ensure_item_in_project(&item, project_id)?;
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
                &state,
                project.id,
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
            let prepared_convergence =
                prepared_convergence_for_revision(&convergences, revision_id);
            let resolved_target_oid = HttpInfraAdapter::new(&state)
                .resolve_project_ref_oid(project.id, &revision.target_ref)
                .await
                .map_err(api_to_usecase_error)?;

            Ok(ingot_usecases::convergence::ConvergenceApprovalContext {
                project,
                item,
                revision,
                has_active_job: has_active_job_for_revision(&jobs, revision_id),
                has_active_convergence: has_active_convergence_for_revision(
                    &convergences,
                    revision_id,
                ),
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
        let db = self.state.db.clone();
        let item = item.clone();
        async move {
            db.update_item(&item)
                .await
                .map_err(UseCaseError::Repository)
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
            ensure_item_in_project(&item, project_id)?;
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
            let convergences = state
                .db
                .list_convergences_by_item(item.id)
                .await
                .map_err(UseCaseError::Repository)?;

            Ok(ingot_usecases::convergence::RejectApprovalContext {
                item,
                has_active_job: has_active_job_for_revision(&jobs, revision.id),
                has_active_convergence: has_active_convergence_for_revision(
                    &convergences,
                    revision.id,
                ),
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
        let db = self.state.db.clone();
        let item = item.clone();
        let next_revision = next_revision.clone();
        async move {
            db.create_revision(&next_revision)
                .await
                .map_err(UseCaseError::Repository)?;
            db.update_item(&item)
                .await
                .map_err(UseCaseError::Repository)
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
            let prepared_commit_oid = convergence
                .state
                .prepared_commit_oid()
                .ok_or(UseCaseError::PreparedConvergenceMissing)?;
            let input_target_commit_oid = convergence
                .state
                .input_target_commit_oid()
                .ok_or(UseCaseError::PreparedConvergenceMissing)?;

            match HttpInfraAdapter::new(&state)
                .finalize_target_ref(
                    project.id,
                    &convergence.target_ref,
                    prepared_commit_oid,
                    input_target_commit_oid,
                )
                .await
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
            let readiness = match HttpInfraAdapter::new(&state)
                .checkout_finalization_status(&project, &revision.target_ref, &prepared_commit_oid)
                .await
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
            HttpInfraAdapter::new(&state)
                .sync_checkout_to_prepared_commit(
                    &project,
                    &revision.target_ref,
                    &prepared_commit_oid,
                )
                .await
                .map_err(api_to_usecase_error)?;
            Ok(())
        }
    }

    fn update_git_operation(
        &self,
        operation: &GitOperation,
    ) -> impl std::future::Future<Output = Result<(), UseCaseError>> + Send {
        let db = self.state.db.clone();
        let operation = operation.clone();
        async move {
            db.update_git_operation(&operation)
                .await
                .map_err(UseCaseError::Repository)
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
                    HttpInfraAdapter::new(&state)
                        .remove_workspace_path(project.id, &workspace.path)
                        .await
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
        unsupported_http_system_action("http convergence port does not load system actions")
    }

    async fn promote_queue_heads(&self, _project_id: ProjectId) -> Result<(), UseCaseError> {
        unsupported_http_system_action("http convergence port does not promote queue heads")
    }

    async fn prepare_queue_head_convergence(
        &self,
        _project: &Project,
        _state: &ingot_usecases::convergence::SystemActionItemState,
        _queue_entry: &ConvergenceQueueEntry,
    ) -> Result<(), UseCaseError> {
        unsupported_http_system_action("http convergence port does not prepare queue heads")
    }

    async fn invalidate_prepared_convergence(
        &self,
        _project_id: ProjectId,
        _item_id: ItemId,
    ) -> Result<(), UseCaseError> {
        unsupported_http_system_action(
            "http convergence port does not invalidate prepared convergence",
        )
    }

    async fn auto_finalize_prepared_convergence(
        &self,
        _project_id: ProjectId,
        _item_id: ItemId,
    ) -> Result<bool, UseCaseError> {
        unsupported_http_system_action("http convergence port does not auto-finalize convergence")
    }

    async fn auto_queue_convergence(
        &self,
        _project_id: ProjectId,
        _item_id: ItemId,
    ) -> Result<bool, UseCaseError> {
        unsupported_http_system_action("http convergence port does not auto-queue convergence")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    use chrono::Utc;
    use ingot_domain::ids::{ItemId, ItemRevisionId, ProjectId};
    use ingot_domain::item::ApprovalState;
    use ingot_domain::test_support::{ItemBuilder, ProjectBuilder, RevisionBuilder};
    use ingot_test_support::git::{
        git_output as support_git_output, temp_git_repo as support_temp_git_repo,
    };
    use ingot_usecases::convergence::ConvergenceCommandPort;

    use crate::router::test_helpers::test_app_state;

    fn temp_git_repo() -> PathBuf {
        support_temp_git_repo("ingot-http-api")
    }

    fn git_output(path: &std::path::Path, args: &[&str]) -> String {
        support_git_output(path, args)
    }

    #[tokio::test]
    async fn convergence_port_maps_missing_project_to_project_not_found() {
        let state = test_app_state().await;
        let error = HttpConvergencePort::new(&state)
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

        let error = HttpConvergencePort::new(&state)
            .load_approval_context(project_a.id, item.id)
            .await
            .expect_err("cross-project item should fail");

        assert!(matches!(error, UseCaseError::ItemNotFound));
    }
}
