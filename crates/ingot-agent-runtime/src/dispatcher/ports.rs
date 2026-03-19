use super::*;
use ingot_usecases::convergence::{CheckoutFinalizationReadiness, FinalizeTargetRefResult};

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
}

impl PreparedConvergenceFinalizePort for RuntimeFinalizePort {
    fn find_or_create_finalize_operation(
        &self,
        operation: &GitOperation,
    ) -> impl Future<Output = Result<GitOperation, ingot_usecases::UseCaseError>> + Send {
        let dispatcher = self.dispatcher.clone();
        let operation = operation.clone();
        async move {
            let convergence_id = operation
                .entity_id
                .parse::<ingot_domain::ids::ConvergenceId>()
                .map_err(|error| ingot_usecases::UseCaseError::Internal(error.to_string()))?;
            if let Some(existing) = dispatcher
                .db
                .find_unresolved_finalize_for_convergence(convergence_id)
                .await
                .map_err(ingot_usecases::UseCaseError::Repository)?
            {
                return Ok(existing);
            }

            match dispatcher.db.create_git_operation(&operation).await {
                Ok(()) => {
                    dispatcher
                        .append_activity(
                            operation.project_id,
                            ActivityEventType::GitOperationPlanned,
                            "git_operation",
                            operation.id.to_string(),
                            serde_json::json!({
                                "operation_kind": operation.operation_kind(),
                                "entity_id": operation.entity_id,
                            }),
                        )
                        .await
                        .map_err(usecase_from_runtime_error)?;
                    Ok(operation)
                }
                Err(RepositoryError::Conflict(_)) => dispatcher
                    .db
                    .find_unresolved_finalize_for_convergence(convergence_id)
                    .await
                    .map_err(ingot_usecases::UseCaseError::Repository)?
                    .ok_or_else(|| {
                        ingot_usecases::UseCaseError::Internal(
                            "finalize git operation conflict without existing row".into(),
                        )
                    }),
                Err(other) => Err(ingot_usecases::UseCaseError::Repository(other)),
            }
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
        prepared_commit_oid: &str,
    ) -> impl Future<Output = Result<CheckoutFinalizationReadiness, ingot_usecases::UseCaseError>> + Send
    {
        let dispatcher = self.dispatcher.clone();
        let project = project.clone();
        let revision = revision.clone();
        let prepared_commit_oid = prepared_commit_oid.to_string();
        async move {
            match dispatcher
                .reconcile_checkout_sync_state(&project, item.id, &revision)
                .await
                .map_err(usecase_from_runtime_error)?
            {
                CheckoutSyncStatus::Blocked { message, .. } => {
                    Ok(CheckoutFinalizationReadiness::Blocked { message })
                }
                CheckoutSyncStatus::Ready => match checkout_finalization_status(
                    Path::new(&project.path),
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
        prepared_commit_oid: &str,
    ) -> impl Future<Output = Result<(), ingot_usecases::UseCaseError>> + Send {
        let dispatcher = self.dispatcher.clone();
        let project = project.clone();
        let revision = revision.clone();
        let prepared_commit_oid = prepared_commit_oid.to_string();
        async move {
            let paths = dispatcher
                .refresh_project_mirror(&project)
                .await
                .map_err(usecase_from_runtime_error)?;
            sync_checkout_to_commit(
                Path::new(&project.path),
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

    fn apply_successful_finalization(
        &self,
        _trigger: FinalizePreparedTrigger,
        project: &Project,
        item: &ingot_domain::item::Item,
        _revision: &ItemRevision,
        convergence: &Convergence,
        _queue_entry: &ConvergenceQueueEntry,
        operation: &GitOperation,
    ) -> impl Future<Output = Result<(), ingot_usecases::UseCaseError>> + Send {
        let dispatcher = self.dispatcher.clone();
        let convergence = convergence.clone();
        let operation = operation.clone();
        async move {
            dispatcher
                .adopt_finalized_target_ref(&operation)
                .await
                .map_err(usecase_from_runtime_error)?;
            dispatcher
                .append_activity(
                    project.id,
                    ActivityEventType::ConvergenceFinalized,
                    "convergence",
                    convergence.id.to_string(),
                    serde_json::json!({ "item_id": item.id }),
                )
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
