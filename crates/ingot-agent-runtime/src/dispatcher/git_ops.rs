use super::*;

impl JobDispatcher {
    pub async fn refresh_project_mirror(
        &self,
        project: &Project,
    ) -> Result<ingot_git::project_repo::ProjectRepoPaths, RuntimeError> {
        let paths = self.project_paths(project);
        let has_unresolved_finalize = self
            .db
            .list_unresolved_git_operations()
            .await?
            .into_iter()
            .any(|operation| {
                operation.project_id == project.id
                    && operation.operation_kind() == OperationKind::FinalizeTargetRef
            });
        if !(has_unresolved_finalize && paths.mirror_git_dir.exists()) {
            ensure_mirror(&paths).await?;
        }
        Ok(paths)
    }

    pub(super) async fn reconcile_git_operations(&self) -> Result<bool, RuntimeError> {
        let operations = self.db.list_unresolved_git_operations().await?;
        let mut made_progress = false;
        for mut operation in operations {
            let project = self.db.get_project(operation.project_id).await?;
            let paths = self.refresh_project_mirror(&project).await?;
            let repo_path = paths.mirror_git_dir.as_path();
            if operation.operation_kind() == OperationKind::FinalizeTargetRef {
                made_progress |= self
                    .reconcile_finalize_target_ref_operation(&project, &mut operation, &paths)
                    .await?;
                continue;
            }
            let reconciled = match &operation.payload {
                OperationPayload::FinalizeTargetRef { .. } => unreachable!("handled above"),
                OperationPayload::CreateJobCommit { .. }
                | OperationPayload::PrepareConvergenceCommit { .. } => {
                    if let Some(commit_oid) = operation.effective_commit_oid() {
                        ingot_git::commands::commit_exists(repo_path, commit_oid).await?
                    } else {
                        false
                    }
                }
                OperationPayload::CreateInvestigationRef {
                    ref_name, new_oid, ..
                } => {
                    resolve_ref_oid(repo_path, ref_name).await?.as_deref() == Some(new_oid.as_str())
                }
                OperationPayload::RemoveWorkspaceRef { ref_name, .. } => {
                    resolve_ref_oid(repo_path, ref_name).await?.is_none()
                }
                OperationPayload::RemoveInvestigationRef { ref_name, .. } => {
                    resolve_ref_oid(repo_path, ref_name).await?.is_none()
                }
                OperationPayload::ResetWorkspace {
                    workspace_id,
                    new_oid,
                    ..
                } => {
                    let workspace = self.db.get_workspace(*workspace_id).await?;
                    match head_oid(Path::new(&workspace.path)).await {
                        Ok(actual_head) => actual_head == new_oid.as_str(),
                        Err(_) => false,
                    }
                }
            };

            if reconciled {
                self.adopt_reconciled_git_operation(&operation).await?;
                self.mark_git_operation_reconciled(&mut operation).await?;
                made_progress = true;
            } else {
                operation.status = GitOperationStatus::Failed;
                operation.completed_at = Some(Utc::now());
                self.db.update_git_operation(&operation).await?;
                made_progress = true;
            }
        }
        Ok(made_progress)
    }

    pub(super) async fn mark_git_operation_reconciled(
        &self,
        operation: &mut GitOperation,
    ) -> Result<(), RuntimeError> {
        operation.status = GitOperationStatus::Reconciled;
        operation.completed_at = Some(Utc::now());
        self.db.update_git_operation(operation).await?;
        self.append_activity(
            operation.project_id,
            ActivityEventType::GitOperationReconciled,
            "git_operation",
            operation.id.to_string(),
            serde_json::json!({ "operation_kind": operation.operation_kind() }),
        )
        .await?;
        Ok(())
    }

    async fn complete_finalize_target_ref_operation(
        &self,
        context: FinalizeOperationContext<'_>,
        operation: &mut GitOperation,
    ) -> Result<FinalizeCompletionOutcome, RuntimeError> {
        let current_target_oid = resolve_ref_oid(
            context.paths.mirror_git_dir.as_path(),
            context.mirror_target_ref,
        )
        .await?;
        if current_target_oid.as_deref() != Some(context.prepared_commit_oid) {
            operation.status = GitOperationStatus::Failed;
            operation.completed_at = Some(Utc::now());
            self.db.update_git_operation(operation).await?;
            return Ok(FinalizeCompletionOutcome::Failed);
        }

        if operation.status == GitOperationStatus::Planned {
            operation.status = GitOperationStatus::Applied;
            operation.completed_at = Some(Utc::now());
            self.db.update_git_operation(operation).await?;
        }

        match checkout_finalization_status(
            Path::new(&context.project.path),
            &context.revision.target_ref,
            context.prepared_commit_oid,
        )
        .await?
        {
            CheckoutFinalizationStatus::Blocked { .. } => {
                self.reconcile_checkout_sync_state(
                    context.project,
                    context.item_id,
                    context.revision,
                )
                .await?;
                Ok(FinalizeCompletionOutcome::Blocked)
            }
            CheckoutFinalizationStatus::NeedsSync => {
                self.reconcile_checkout_sync_state(
                    context.project,
                    context.item_id,
                    context.revision,
                )
                .await?;
                sync_checkout_to_commit(
                    Path::new(&context.project.path),
                    context.paths.mirror_git_dir.as_path(),
                    &context.revision.target_ref,
                    context.prepared_commit_oid,
                )
                .await?;
                self.adopt_finalized_target_ref(operation).await?;
                self.mark_git_operation_reconciled(operation).await?;
                Ok(FinalizeCompletionOutcome::Completed)
            }
            CheckoutFinalizationStatus::Synced => {
                self.reconcile_checkout_sync_state(
                    context.project,
                    context.item_id,
                    context.revision,
                )
                .await?;
                self.adopt_finalized_target_ref(operation).await?;
                self.mark_git_operation_reconciled(operation).await?;
                Ok(FinalizeCompletionOutcome::Completed)
            }
        }
    }

    async fn adopt_reconciled_git_operation(
        &self,
        operation: &GitOperation,
    ) -> Result<(), RuntimeError> {
        match operation.operation_kind() {
            OperationKind::CreateJobCommit => self.adopt_create_job_commit(operation).await,
            OperationKind::FinalizeTargetRef => self.adopt_finalized_target_ref(operation).await,
            OperationKind::PrepareConvergenceCommit => {
                self.adopt_prepared_convergence(operation).await
            }
            OperationKind::CreateInvestigationRef => Ok(()),
            OperationKind::ResetWorkspace => self.adopt_reset_workspace(operation).await,
            OperationKind::RemoveWorkspaceRef => self.adopt_removed_workspace_ref(operation).await,
            OperationKind::RemoveInvestigationRef => Ok(()),
        }
    }

    async fn adopt_create_job_commit(&self, operation: &GitOperation) -> Result<(), RuntimeError> {
        let job_id = operation
            .entity_id
            .parse::<ingot_domain::ids::JobId>()
            .map_err(|error| RuntimeError::InvalidState(error.to_string()))?;
        let mut job = self.db.get_job(job_id).await?;
        let commit_oid = operation
            .effective_commit_oid()
            .map(ToOwned::to_owned)
            .ok_or_else(|| {
                RuntimeError::InvalidState("reconciled create_job_commit missing commit oid".into())
            })?;

        if !job.state.is_active() {
            return Ok(());
        }

        let ended_at = job.state.ended_at().unwrap_or_else(Utc::now);
        job.complete(
            OutcomeClass::Clean,
            ended_at,
            Some(commit_oid.clone()),
            None,
            None,
        );
        self.db.update_job(&job).await?;

        if let Some(workspace_id) = operation.workspace_id().or(job.state.workspace_id()) {
            let mut workspace = self.db.get_workspace(workspace_id).await?;
            let now = Utc::now();
            workspace.set_head_commit_oid(commit_oid, now);
            if workspace.state.status() == WorkspaceStatus::Busy {
                workspace.release_to(WorkspaceStatus::Ready, now);
            }
            self.db.update_workspace(&workspace).await?;
        }

        self.append_activity(
            job.project_id,
            ActivityEventType::JobCompleted,
            "job",
            job.id.to_string(),
            serde_json::json!({ "item_id": job.item_id, "outcome": "clean", "reconciled": true }),
        )
        .await?;
        self.refresh_revision_context_for_ids(
            job.project_id,
            job.item_id,
            job.item_revision_id,
            Some(job.id),
        )
        .await?;
        self.auto_dispatch_projected_review(job.project_id, job.item_id)
            .await?;

        Ok(())
    }

    pub(super) async fn adopt_finalized_target_ref(
        &self,
        operation: &GitOperation,
    ) -> Result<(), RuntimeError> {
        let convergence_id = operation
            .entity_id
            .parse::<ingot_domain::ids::ConvergenceId>()
            .map_err(|error| RuntimeError::InvalidState(error.to_string()))?;
        let mut convergence = self.db.get_convergence(convergence_id).await?;
        if convergence.state.status() != ConvergenceStatus::Finalized {
            let final_oid = operation
                .new_oid()
                .or(operation.commit_oid())
                .map(ToOwned::to_owned)
                .ok_or_else(|| {
                    RuntimeError::InvalidState(
                        "reconciled finalize_target_ref missing commit oid".into(),
                    )
                })?;
            convergence.transition_to_finalized(final_oid, Utc::now());
            self.db.update_convergence(&convergence).await?;
        }

        let project = self.db.get_project(convergence.project_id).await?;
        if let Some(workspace_id) = convergence.state.integration_workspace_id() {
            let workspace = self.db.get_workspace(workspace_id).await?;
            if workspace.state.status() != WorkspaceStatus::Abandoned {
                self.finalize_integration_workspace_after_close(&project, &workspace)
                    .await?;
            }
        }

        if let Some(mut queue_entry) = self
            .db
            .find_active_queue_entry_for_revision(convergence.item_revision_id)
            .await?
        {
            queue_entry.status = ConvergenceQueueEntryStatus::Released;
            queue_entry.released_at.get_or_insert_with(Utc::now);
            queue_entry.updated_at = Utc::now();
            self.db.update_queue_entry(&queue_entry).await?;
        }

        let mut item = self.db.get_item(convergence.item_id).await?;
        if item.current_revision_id == convergence.item_revision_id {
            if !item.lifecycle.is_done() {
                let revision = self.db.get_revision(item.current_revision_id).await?;
                let (resolution_source, approval_state) = match revision.approval_policy {
                    ingot_domain::revision::ApprovalPolicy::Required => {
                        (ResolutionSource::ApprovalCommand, ApprovalState::Approved)
                    }
                    ingot_domain::revision::ApprovalPolicy::NotRequired => {
                        (ResolutionSource::SystemCommand, ApprovalState::NotRequired)
                    }
                };
                item.lifecycle = Lifecycle::Done {
                    reason: DoneReason::Completed,
                    source: resolution_source,
                    closed_at: Utc::now(),
                };
                item.approval_state = approval_state;
            }
            item.escalation = Escalation::None;
            item.updated_at = Utc::now();
            self.db.update_item(&item).await?;
        }

        Ok(())
    }

    async fn adopt_prepared_convergence(
        &self,
        operation: &GitOperation,
    ) -> Result<(), RuntimeError> {
        let convergence_id = operation
            .entity_id
            .parse::<ingot_domain::ids::ConvergenceId>()
            .map_err(|error| RuntimeError::InvalidState(error.to_string()))?;
        let mut convergence = self.db.get_convergence(convergence_id).await?;
        if matches!(
            convergence.state.status(),
            ConvergenceStatus::Cancelled | ConvergenceStatus::Failed | ConvergenceStatus::Finalized
        ) {
            return Ok(());
        }
        if convergence.state.status() != ConvergenceStatus::Prepared {
            let prepared_oid = operation
                .effective_commit_oid()
                .map(ToOwned::to_owned)
                .ok_or_else(|| {
                    RuntimeError::InvalidState(
                        "reconciled prepare_convergence_commit missing commit oid".into(),
                    )
                })?;
            convergence.transition_to_prepared(prepared_oid, Some(Utc::now()));
            self.db.update_convergence(&convergence).await?;
        }

        if let Some(workspace_id) = convergence.state.integration_workspace_id() {
            let mut workspace = self.db.get_workspace(workspace_id).await?;
            let now = Utc::now();
            let head_commit_oid = operation.effective_commit_oid().map(ToOwned::to_owned);
            workspace.mark_ready_with_head(
                head_commit_oid.unwrap_or_else(|| {
                    workspace
                        .state
                        .head_commit_oid()
                        .unwrap_or_default()
                        .to_owned()
                }),
                now,
            );
            self.db.update_workspace(&workspace).await?;
        }

        Ok(())
    }

    async fn reconcile_finalize_target_ref_operation(
        &self,
        project: &Project,
        operation: &mut GitOperation,
        paths: &ingot_git::project_repo::ProjectRepoPaths,
    ) -> Result<bool, RuntimeError> {
        let convergence_id = operation
            .entity_id
            .parse::<ingot_domain::ids::ConvergenceId>()
            .map_err(|error| RuntimeError::InvalidState(error.to_string()))?;
        let convergence = self.db.get_convergence(convergence_id).await?;
        let item = self.db.get_item(convergence.item_id).await?;
        let revision = self.db.get_revision(convergence.item_revision_id).await?;
        let target_ref = operation
            .ref_name()
            .unwrap_or(convergence.target_ref.as_str())
            .to_string();
        let prepared_commit_oid = operation
            .new_oid()
            .or(operation.commit_oid())
            .ok_or_else(|| RuntimeError::InvalidState("finalize operation missing new oid".into()))?
            .to_string();
        Ok(!matches!(
            self.complete_finalize_target_ref_operation(
                FinalizeOperationContext {
                    project,
                    item_id: item.id,
                    revision: &revision,
                    mirror_target_ref: &target_ref,
                    prepared_commit_oid: &prepared_commit_oid,
                    paths,
                },
                operation,
            )
            .await?,
            FinalizeCompletionOutcome::Blocked
        ))
    }

    async fn adopt_reset_workspace(&self, operation: &GitOperation) -> Result<(), RuntimeError> {
        let Some(workspace_id) = operation.workspace_id() else {
            return Ok(());
        };
        let mut workspace = self.db.get_workspace(workspace_id).await?;
        let now = Utc::now();
        workspace.mark_ready_with_head(
            operation
                .new_oid()
                .unwrap_or_else(|| workspace.state.head_commit_oid().unwrap_or_default())
                .to_owned(),
            now,
        );
        self.db.update_workspace(&workspace).await?;
        Ok(())
    }

    async fn adopt_removed_workspace_ref(
        &self,
        operation: &GitOperation,
    ) -> Result<(), RuntimeError> {
        let Some(workspace_id) = operation.workspace_id() else {
            return Ok(());
        };
        let mut workspace = self.db.get_workspace(workspace_id).await?;
        let now = Utc::now();
        workspace.mark_abandoned(now);
        if operation.ref_name().is_some() {
            workspace.workspace_ref = None;
        }
        workspace.updated_at = now;
        self.db.update_workspace(&workspace).await?;
        Ok(())
    }

    pub async fn reconcile_active_jobs(&self) -> Result<bool, RuntimeError> {
        let active_jobs = self.db.list_active_jobs().await?;
        let mut made_progress = false;
        for job in active_jobs {
            match job.state.status() {
                JobStatus::Assigned => {
                    self.reconcile_assigned_job(job).await?;
                    made_progress = true;
                }
                JobStatus::Running => {
                    made_progress |= self.reconcile_running_job(job).await?;
                }
                _ => {}
            }
        }
        Ok(made_progress)
    }

    pub(super) async fn reconcile_assigned_job(&self, job: Job) -> Result<(), RuntimeError> {
        let _guard = self
            .project_locks
            .acquire_project_mutation(job.project_id)
            .await;
        let mut job = self.db.get_job(job.id).await?;
        if job.state.status() != JobStatus::Assigned {
            return Ok(());
        }

        let workspace_id = job.state.workspace_id();
        job.state = JobState::Queued;
        self.db.update_job(&job).await?;

        if let Some(workspace_id) = workspace_id {
            let mut workspace = self.db.get_workspace(workspace_id).await?;
            workspace.release_to(WorkspaceStatus::Ready, Utc::now());
            self.db.update_workspace(&workspace).await?;
        }

        Ok(())
    }

    async fn reconcile_running_job(&self, job: Job) -> Result<bool, RuntimeError> {
        let expired = job
            .state
            .lease_expires_at()
            .is_none_or(|lease| lease <= Utc::now());
        let foreign_owner = job.state.lease_owner_id() != Some(self.lease_owner_id.as_str());
        if !expired && !foreign_owner {
            return Ok(false);
        }

        let _guard = self
            .project_locks
            .acquire_project_mutation(job.project_id)
            .await;
        let job = self.db.get_job(job.id).await?;
        if job.state.status() != JobStatus::Running {
            return Ok(false);
        }
        let item = self.db.get_item(job.item_id).await?;
        self.db
            .finish_job_non_success(FinishJobNonSuccessParams {
                job_id: job.id,
                item_id: item.id,
                expected_item_revision_id: job.item_revision_id,
                status: JobStatus::Expired,
                outcome_class: Some(OutcomeClass::TransientFailure),
                error_code: Some("heartbeat_expired".into()),
                error_message: None,
                escalation_reason: None,
            })
            .await?;

        if let Some(workspace_id) = job.state.workspace_id() {
            let mut workspace = self.db.get_workspace(workspace_id).await?;
            workspace.mark_stale(Utc::now());
            self.db.update_workspace(&workspace).await?;
        }

        self.append_activity(
            job.project_id,
            ActivityEventType::JobFailed,
            "job",
            job.id.to_string(),
            serde_json::json!({ "item_id": job.item_id, "error_code": "heartbeat_expired" }),
        )
        .await?;

        Ok(true)
    }

    pub(super) async fn reconcile_active_convergences(&self) -> Result<bool, RuntimeError> {
        let active_convergences = self.db.list_active_convergences().await?;
        let mut made_progress = false;
        for convergence in active_convergences {
            let _guard = self
                .project_locks
                .acquire_project_mutation(convergence.project_id)
                .await;
            let mut convergence = convergence;
            if !matches!(
                convergence.state.status(),
                ConvergenceStatus::Queued | ConvergenceStatus::Running
            ) {
                continue;
            }
            convergence.transition_to_failed(Some("startup_recovery_required".into()), Utc::now());
            self.db.update_convergence(&convergence).await?;

            if let Some(workspace_id) = convergence.state.integration_workspace_id() {
                let mut workspace = self.db.get_workspace(workspace_id).await?;
                workspace.mark_stale(Utc::now());
                self.db.update_workspace(&workspace).await?;
            }

            self.append_activity(
                convergence.project_id,
                ActivityEventType::ConvergenceFailed,
                "convergence",
                convergence.id.to_string(),
                serde_json::json!({ "item_id": convergence.item_id, "reason": "startup_recovery_required" }),
            )
            .await?;
            made_progress = true;
        }
        Ok(made_progress)
    }

    pub(super) async fn reconcile_workspace_retention(&self) -> Result<bool, RuntimeError> {
        let mut made_progress = false;
        for project in self.db.list_projects().await? {
            let workspaces = self.db.list_workspaces_by_project(project.id).await?;
            for workspace in workspaces {
                if workspace.state.status() != WorkspaceStatus::Abandoned
                    || workspace.retention_policy == RetentionPolicy::RetainUntilDebug
                {
                    continue;
                }
                if !self.workspace_can_be_removed(&project, &workspace).await? {
                    continue;
                }
                self.remove_abandoned_workspace(&project, &workspace)
                    .await?;
                made_progress = true;
            }
        }
        Ok(made_progress)
    }
}
