use std::future::Future;

use ingot_domain::activity::{ActivityEventType, ActivitySubject};
use ingot_domain::convergence::{Convergence, FinalizedCheckoutAdoption};
use ingot_domain::convergence_queue::ConvergenceQueueEntry;
use ingot_domain::git_operation::GitOperation;
use ingot_domain::ports::ProjectMutationLockPort;
use ingot_domain::project::Project;
use ingot_domain::revision::ItemRevision;
use ingot_git::commands::{
    FinalizeTargetRefOutcome, finalize_target_ref as finalize_target_ref_in_repo,
};
use ingot_git::project_repo::{
    CheckoutFinalizationStatus, CheckoutSyncStatus, checkout_finalization_status,
    sync_checkout_to_commit,
};
use ingot_usecases::convergence::{
    CheckoutFinalizationReadiness, ConvergenceSystemActionPort, FinalizationTarget,
    FinalizePreparedTrigger, FinalizeTargetRefResult, PreparedConvergenceFinalizePort,
    SystemActionItemState, SystemActionProjectState,
};
use ingot_usecases::reconciliation::ReconciliationPort;
use tracing::warn;

use crate::{JobDispatcher, RuntimeError};

#[derive(Clone)]
pub(crate) struct RuntimeConvergencePort {
    pub(crate) dispatcher: JobDispatcher,
}

#[derive(Clone)]
pub(crate) struct RuntimeFinalizePort {
    pub(crate) dispatcher: JobDispatcher,
}

#[derive(Clone)]
pub(crate) struct RuntimeReconciliationPort {
    pub(crate) dispatcher: JobDispatcher,
}

pub(crate) fn usecase_to_runtime_error(error: ingot_usecases::UseCaseError) -> RuntimeError {
    match error {
        ingot_usecases::UseCaseError::Repository(error) => RuntimeError::Repository(error),
        other => RuntimeError::InvalidState(other.to_string()),
    }
}

pub(crate) fn usecase_from_runtime_error(error: RuntimeError) -> ingot_usecases::UseCaseError {
    match error {
        RuntimeError::Repository(error) => ingot_usecases::UseCaseError::Repository(error),
        other => ingot_usecases::UseCaseError::Internal(other.to_string()),
    }
}

pub(crate) async fn drain_until_idle<F, Fut>(mut step: F) -> Result<(), RuntimeError>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = Result<bool, RuntimeError>>,
{
    while step().await? {}
    Ok(())
}

impl ConvergenceSystemActionPort for RuntimeConvergencePort {
    fn load_system_action_projects(
        &self,
    ) -> impl Future<Output = Result<Vec<SystemActionProjectState>, ingot_usecases::UseCaseError>> + Send
    {
        let dispatcher = self.dispatcher.clone();
        async move {
            let mut projects = Vec::new();
            for project in dispatcher
                .db
                .list_projects()
                .await
                .map_err(ingot_usecases::UseCaseError::Repository)?
            {
                let mut items = Vec::new();
                for item in dispatcher
                    .db
                    .list_items_by_project(project.id)
                    .await
                    .map_err(ingot_usecases::UseCaseError::Repository)?
                {
                    let revision = dispatcher
                        .db
                        .get_revision(item.current_revision_id)
                        .await
                        .map_err(ingot_usecases::UseCaseError::Repository)?;
                    let jobs = dispatcher
                        .db
                        .list_jobs_by_item(item.id)
                        .await
                        .map_err(ingot_usecases::UseCaseError::Repository)?;
                    let findings = dispatcher
                        .db
                        .list_findings_by_item(item.id)
                        .await
                        .map_err(ingot_usecases::UseCaseError::Repository)?;
                    let convergences = match dispatcher
                        .hydrate_convergences(
                            &project,
                            dispatcher
                                .db
                                .list_convergences_by_item(item.id)
                                .await
                                .map_err(ingot_usecases::UseCaseError::Repository)?,
                        )
                        .await
                    {
                        Ok(convergences) => convergences,
                        Err(error) => {
                            warn!(
                                ?error,
                                project_id = %project.id,
                                item_id = %item.id,
                                "skipping system-action item because convergence hydration failed"
                            );
                            continue;
                        }
                    };
                    let queue_entry = dispatcher
                        .db
                        .find_active_queue_entry_for_revision(revision.id)
                        .await
                        .map_err(ingot_usecases::UseCaseError::Repository)?;
                    items.push(SystemActionItemState {
                        item_id: item.id,
                        item,
                        revision,
                        jobs,
                        findings,
                        convergences,
                        queue_entry,
                    });
                }
                projects.push(SystemActionProjectState { project, items });
            }

            Ok(projects)
        }
    }

    fn promote_queue_heads(
        &self,
        project_id: ingot_domain::ids::ProjectId,
    ) -> impl Future<Output = Result<(), ingot_usecases::UseCaseError>> + Send {
        let dispatcher = self.dispatcher.clone();
        async move {
            dispatcher
                .promote_queue_heads(project_id)
                .await
                .map_err(usecase_from_runtime_error)
        }
    }

    fn prepare_queue_head_convergence(
        &self,
        project: &Project,
        state: &SystemActionItemState,
        queue_entry: &ConvergenceQueueEntry,
    ) -> impl Future<Output = Result<(), ingot_usecases::UseCaseError>> + Send {
        let dispatcher = self.dispatcher.clone();
        let project = project.clone();
        let state = state.clone();
        let queue_entry = queue_entry.clone();
        async move {
            dispatcher
                .prepare_queue_head_convergence(
                    &project,
                    &state.item,
                    &state.revision,
                    &state.jobs,
                    &state.findings,
                    &state.convergences,
                    &queue_entry,
                )
                .await
                .map_err(usecase_from_runtime_error)
        }
    }

    fn invalidate_prepared_convergence(
        &self,
        project_id: ingot_domain::ids::ProjectId,
        item_id: ingot_domain::ids::ItemId,
    ) -> impl Future<Output = Result<(), ingot_usecases::UseCaseError>> + Send {
        let dispatcher = self.dispatcher.clone();
        async move {
            dispatcher
                .invalidate_prepared_convergence(project_id, item_id)
                .await
                .map_err(usecase_from_runtime_error)
        }
    }

    fn auto_finalize_prepared_convergence(
        &self,
        project_id: ingot_domain::ids::ProjectId,
        item_id: ingot_domain::ids::ItemId,
    ) -> impl Future<Output = Result<bool, ingot_usecases::UseCaseError>> + Send {
        let dispatcher = self.dispatcher.clone();
        async move {
            dispatcher
                .auto_finalize_prepared_convergence(project_id, item_id)
                .await
                .map_err(usecase_from_runtime_error)
        }
    }

    fn auto_queue_convergence(
        &self,
        project_id: ingot_domain::ids::ProjectId,
        item_id: ingot_domain::ids::ItemId,
    ) -> impl Future<Output = Result<bool, ingot_usecases::UseCaseError>> + Send {
        let dispatcher = self.dispatcher.clone();
        async move {
            #[cfg(test)]
            dispatcher.pause_before_auto_queue_guard().await;
            let _guard = dispatcher
                .project_locks
                .acquire_project_mutation(project_id)
                .await;
            let project = dispatcher
                .db
                .get_project(project_id)
                .await
                .map_err(ingot_usecases::UseCaseError::Repository)?;
            if project.execution_mode != ingot_domain::project::ExecutionMode::Autopilot {
                return Ok(false);
            }
            dispatcher
                .auto_queue_convergence_inner(project_id, item_id, &project)
                .await
        }
    }
}

impl PreparedConvergenceFinalizePort for RuntimeFinalizePort {
    fn find_or_create_finalize_operation(
        &self,
        operation: &GitOperation,
    ) -> impl Future<Output = Result<GitOperation, ingot_usecases::UseCaseError>> + Send {
        let db = self.dispatcher.db.clone();
        let operation = operation.clone();
        async move {
            ingot_usecases::convergence::find_or_create_finalize_operation(&db, &operation).await
        }
    }

    fn finalize_target_ref(
        &self,
        project: &Project,
        convergence: &Convergence,
    ) -> impl Future<Output = Result<FinalizeTargetRefResult, ingot_usecases::UseCaseError>> + Send
    {
        let dispatcher = self.dispatcher.clone();
        let project = project.clone();
        let convergence = convergence.clone();
        async move {
            let paths = dispatcher
                .refresh_project_mirror(&project)
                .await
                .map_err(usecase_from_runtime_error)?;
            let prepared_commit_oid = convergence
                .state
                .prepared_commit_oid()
                .map(ToOwned::to_owned)
                .ok_or_else(|| {
                    ingot_usecases::UseCaseError::Internal("prepared commit missing".into())
                })?;
            let input_target_commit_oid = convergence
                .state
                .input_target_commit_oid()
                .map(ToOwned::to_owned)
                .ok_or_else(|| {
                    ingot_usecases::UseCaseError::Internal("input target commit missing".into())
                })?;
            match finalize_target_ref_in_repo(
                paths.mirror_git_dir.as_path(),
                &convergence.target_ref,
                &prepared_commit_oid,
                &input_target_commit_oid,
            )
            .await
            .map_err(|error| usecase_from_runtime_error(RuntimeError::from(error)))?
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
        item: &ingot_domain::item::Item,
        revision: &ItemRevision,
        prepared_commit_oid: &ingot_domain::commit_oid::CommitOid,
    ) -> impl Future<Output = Result<CheckoutFinalizationReadiness, ingot_usecases::UseCaseError>> + Send
    {
        let dispatcher = self.dispatcher.clone();
        let project = project.clone();
        let revision = revision.clone();
        let prepared_commit_oid = prepared_commit_oid.clone();
        async move {
            let paths = dispatcher
                .refresh_project_mirror(&project)
                .await
                .map_err(usecase_from_runtime_error)?;
            match dispatcher
                .reconcile_checkout_sync_state(
                    &project,
                    item.id,
                    &revision,
                    Some(&prepared_commit_oid),
                )
                .await
                .map_err(usecase_from_runtime_error)?
            {
                CheckoutSyncStatus::Blocked { message, .. } => {
                    Ok(CheckoutFinalizationReadiness::Blocked { message })
                }
                CheckoutSyncStatus::Ready => match checkout_finalization_status(
                    &project.path,
                    paths.mirror_git_dir.as_path(),
                    &revision.target_ref,
                    &prepared_commit_oid,
                )
                .await
                .map_err(|error| usecase_from_runtime_error(RuntimeError::from(error)))?
                {
                    CheckoutFinalizationStatus::Blocked { message, .. } => {
                        Ok(CheckoutFinalizationReadiness::Blocked { message })
                    }
                    CheckoutFinalizationStatus::NeedsSync => {
                        Ok(CheckoutFinalizationReadiness::NeedsSync)
                    }
                    CheckoutFinalizationStatus::Synced => Ok(CheckoutFinalizationReadiness::Synced),
                },
            }
        }
    }

    fn sync_checkout_to_prepared_commit(
        &self,
        project: &Project,
        revision: &ItemRevision,
        prepared_commit_oid: &ingot_domain::commit_oid::CommitOid,
    ) -> impl Future<Output = Result<(), ingot_usecases::UseCaseError>> + Send {
        let dispatcher = self.dispatcher.clone();
        let project = project.clone();
        let revision = revision.clone();
        let prepared_commit_oid = prepared_commit_oid.clone();
        async move {
            let paths = dispatcher
                .refresh_project_mirror(&project)
                .await
                .map_err(usecase_from_runtime_error)?;
            sync_checkout_to_commit(
                &project.path,
                paths.mirror_git_dir.as_path(),
                &revision.target_ref,
                &prepared_commit_oid,
            )
            .await
            .map_err(|error| usecase_from_runtime_error(RuntimeError::from(error)))?;
            Ok(())
        }
    }

    fn update_git_operation(
        &self,
        operation: &GitOperation,
    ) -> impl Future<Output = Result<(), ingot_usecases::UseCaseError>> + Send {
        let dispatcher = self.dispatcher.clone();
        let operation = operation.clone();
        async move {
            dispatcher
                .db
                .update_git_operation(&operation)
                .await
                .map_err(ingot_usecases::UseCaseError::Repository)?;
            Ok(())
        }
    }

    fn persist_target_ref_advance(
        &self,
        _trigger: FinalizePreparedTrigger,
        project: &Project,
        item: &ingot_domain::item::Item,
        _revision: &ItemRevision,
        target: FinalizationTarget<'_>,
        operation: &GitOperation,
        checkout_adoption: &FinalizedCheckoutAdoption,
    ) -> impl Future<Output = Result<(), ingot_usecases::UseCaseError>> + Send {
        let dispatcher = self.dispatcher.clone();
        let convergence = target.convergence.clone();
        let operation = operation.clone();
        let checkout_adoption = checkout_adoption.clone();
        async move {
            if dispatcher
                .persist_target_ref_advance(&operation, checkout_adoption)
                .await
                .map_err(usecase_from_runtime_error)?
            {
                dispatcher
                    .append_activity(
                        project.id,
                        ActivityEventType::ConvergenceFinalized,
                        ActivitySubject::Convergence(convergence.id),
                        serde_json::json!({ "item_id": item.id }),
                    )
                    .await
                    .map_err(usecase_from_runtime_error)?;
            }
            Ok(())
        }
    }

    fn persist_checkout_adoption_success(
        &self,
        _trigger: FinalizePreparedTrigger,
        _project: &Project,
        _item: &ingot_domain::item::Item,
        _revision: &ItemRevision,
        _target: FinalizationTarget<'_>,
        operation: &GitOperation,
    ) -> impl Future<Output = Result<(), ingot_usecases::UseCaseError>> + Send {
        let dispatcher = self.dispatcher.clone();
        let operation = operation.clone();
        async move {
            if operation.status != ingot_domain::git_operation::GitOperationStatus::Reconciled {
                return Err(ingot_usecases::UseCaseError::Internal(
                    "cannot close finalized item before reconcile".into(),
                ));
            }
            dispatcher
                .persist_checkout_adoption_success(&operation)
                .await
                .map_err(usecase_from_runtime_error)?;
            Ok(())
        }
    }
}

impl ReconciliationPort for RuntimeReconciliationPort {
    fn reconcile_git_operations(
        &self,
    ) -> impl Future<Output = Result<bool, ingot_usecases::UseCaseError>> + Send {
        let dispatcher = self.dispatcher.clone();
        async move {
            dispatcher
                .reconcile_git_operations()
                .await
                .map_err(usecase_from_runtime_error)
        }
    }

    fn reconcile_active_jobs(
        &self,
    ) -> impl Future<Output = Result<bool, ingot_usecases::UseCaseError>> + Send {
        let dispatcher = self.dispatcher.clone();
        async move {
            dispatcher
                .reconcile_active_jobs()
                .await
                .map_err(usecase_from_runtime_error)
        }
    }

    fn reconcile_active_convergences(
        &self,
    ) -> impl Future<Output = Result<bool, ingot_usecases::UseCaseError>> + Send {
        let dispatcher = self.dispatcher.clone();
        async move {
            dispatcher
                .reconcile_active_convergences()
                .await
                .map_err(usecase_from_runtime_error)
        }
    }

    fn reconcile_workspace_retention(
        &self,
    ) -> impl Future<Output = Result<bool, ingot_usecases::UseCaseError>> + Send {
        let dispatcher = self.dispatcher.clone();
        async move {
            dispatcher
                .reconcile_workspace_retention()
                .await
                .map_err(usecase_from_runtime_error)
        }
    }
}
