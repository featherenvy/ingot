// Active job, git-operation, convergence, and workspace reconciliation.

use std::path::Path;

use chrono::Utc;
use ingot_domain::activity::{ActivityEventType, ActivitySubject};
use ingot_domain::commit_oid::CommitOid;
use ingot_domain::convergence::{ConvergenceStatus, FinalizedCheckoutAdoption};
use ingot_domain::git_operation::{
    GitOperation, GitOperationEntityRef, GitOperationStatus, OperationKind, OperationPayload,
};
use ingot_domain::git_ref::GitRef;
use ingot_domain::ids::GitOperationId;
use ingot_domain::item::{ApprovalState, ResolutionSource};
use ingot_domain::job::{Job, JobState, JobStatus, OutcomeClass};
use ingot_domain::ports::{
    FinalizationCheckoutAdoptionSucceededMutation, FinalizationMutation,
    FinalizationTargetRefAdvancedMutation, FinishJobNonSuccessParams, ProjectMutationLockPort,
};
use ingot_domain::project::Project;
use ingot_domain::revision::ItemRevision;
use ingot_domain::workspace::{RetentionPolicy, Workspace, WorkspaceKind, WorkspaceStatus};
use ingot_git::commands::{delete_ref, head_oid, resolve_ref_oid};
use ingot_git::project_repo::{
    CheckoutFinalizationStatus, checkout_finalization_status, sync_checkout_to_commit,
};
use ingot_workspace::remove_workspace;

use crate::{JobDispatcher, RuntimeError, is_inert_assigned_authoring_dispatch_residue};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FinalizeCompletionOutcome {
    Blocked,
    Failed,
    Completed,
}

struct FinalizeOperationContext<'a> {
    project: &'a Project,
    item_id: ingot_domain::ids::ItemId,
    revision: &'a ItemRevision,
    mirror_target_ref: &'a GitRef,
    prepared_commit_oid: &'a CommitOid,
    paths: &'a ingot_git::project_repo::ProjectRepoPaths,
}

impl JobDispatcher {
    pub async fn reconcile_active_jobs(&self) -> Result<bool, RuntimeError> {
        let active_jobs = self.db.list_active_jobs().await?;
        let mut made_progress = false;
        for job in active_jobs {
            match job.state.status() {
                JobStatus::Running => {
                    made_progress |= self.reconcile_running_job(job).await?;
                }
                JobStatus::Assigned => {
                    made_progress |= self.reconcile_inert_assigned_dispatch_job(job).await?;
                }
                _ => {}
            }
        }
        Ok(made_progress)
    }

    pub(crate) async fn reconcile_startup_assigned_jobs(&self) -> Result<bool, RuntimeError> {
        let active_jobs = self.db.list_active_jobs().await?;
        let mut made_progress = false;
        for job in active_jobs {
            if job.state.status() == JobStatus::Assigned {
                self.reconcile_assigned_job(job).await?;
                made_progress = true;
            }
        }
        Ok(made_progress)
    }

    pub(crate) async fn reconcile_git_operations(&self) -> Result<bool, RuntimeError> {
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
                } => resolve_ref_oid(repo_path, ref_name).await?.as_ref() == Some(new_oid),
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
                    match head_oid(&workspace.path).await {
                        Ok(actual_head) => actual_head == *new_oid,
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

    pub(crate) async fn mark_git_operation_reconciled(
        &self,
        operation: &mut GitOperation,
    ) -> Result<(), RuntimeError> {
        operation.status = GitOperationStatus::Reconciled;
        operation.completed_at = Some(Utc::now());
        self.db.update_git_operation(operation).await?;
        self.append_activity(
            operation.project_id,
            ActivityEventType::GitOperationReconciled,
            ActivitySubject::GitOperation(operation.id),
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
        if !current_target_oid
            .as_ref()
            .is_some_and(|oid| oid == context.prepared_commit_oid)
        {
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

        let checkout_status = checkout_finalization_status(
            Path::new(&context.project.path),
            context.paths.mirror_git_dir.as_path(),
            &context.revision.target_ref,
            context.prepared_commit_oid,
        )
        .await?;
        let checkout_adoption = match &checkout_status {
            CheckoutFinalizationStatus::Blocked { message, .. } => {
                FinalizedCheckoutAdoption::blocked(message.clone(), Utc::now())
            }
            CheckoutFinalizationStatus::NeedsSync => FinalizedCheckoutAdoption::pending(Utc::now()),
            CheckoutFinalizationStatus::Synced => FinalizedCheckoutAdoption::synced(Utc::now()),
        };
        self.db
            .apply_finalization_mutation(FinalizationMutation::TargetRefAdvanced(
                FinalizationTargetRefAdvancedMutation {
                    project_id: operation.project_id,
                    item_id: context.item_id,
                    expected_item_revision_id: context.revision.id,
                    convergence_id: *match &operation.entity {
                        GitOperationEntityRef::Convergence(id) => id,
                        _ => unreachable!("checked above"),
                    },
                    git_operation_id: operation.id,
                    final_target_commit_oid: context.prepared_commit_oid.clone(),
                    checkout_adoption,
                },
            ))
            .await?;

        match checkout_status {
            CheckoutFinalizationStatus::Blocked { .. } => Ok(FinalizeCompletionOutcome::Blocked),
            CheckoutFinalizationStatus::NeedsSync => {
                if sync_checkout_to_commit(
                    Path::new(&context.project.path),
                    context.paths.mirror_git_dir.as_path(),
                    &context.revision.target_ref,
                    context.prepared_commit_oid,
                )
                .await
                .is_err()
                {
                    if let CheckoutFinalizationStatus::Blocked { message, .. } =
                        checkout_finalization_status(
                            Path::new(&context.project.path),
                            context.paths.mirror_git_dir.as_path(),
                            &context.revision.target_ref,
                            context.prepared_commit_oid,
                        )
                        .await?
                    {
                        self.db
                            .apply_finalization_mutation(FinalizationMutation::TargetRefAdvanced(
                                FinalizationTargetRefAdvancedMutation {
                                    project_id: operation.project_id,
                                    item_id: context.item_id,
                                    expected_item_revision_id: context.revision.id,
                                    convergence_id: *match &operation.entity {
                                        GitOperationEntityRef::Convergence(id) => id,
                                        _ => unreachable!("checked above"),
                                    },
                                    git_operation_id: operation.id,
                                    final_target_commit_oid: context.prepared_commit_oid.clone(),
                                    checkout_adoption: FinalizedCheckoutAdoption::blocked(
                                        message,
                                        Utc::now(),
                                    ),
                                },
                            ))
                            .await?;
                    }
                    return Ok(FinalizeCompletionOutcome::Blocked);
                }
                let resolution_source = match context.revision.approval_policy {
                    ingot_domain::revision::ApprovalPolicy::Required => {
                        ResolutionSource::ApprovalCommand
                    }
                    ingot_domain::revision::ApprovalPolicy::NotRequired => {
                        ResolutionSource::SystemCommand
                    }
                };
                let approval_state = match context.revision.approval_policy {
                    ingot_domain::revision::ApprovalPolicy::Required => ApprovalState::Approved,
                    ingot_domain::revision::ApprovalPolicy::NotRequired => {
                        ApprovalState::NotRequired
                    }
                };
                self.db
                    .apply_finalization_mutation(FinalizationMutation::CheckoutAdoptionSucceeded(
                        FinalizationCheckoutAdoptionSucceededMutation {
                            project_id: operation.project_id,
                            item_id: context.item_id,
                            expected_item_revision_id: context.revision.id,
                            convergence_id: *match &operation.entity {
                                GitOperationEntityRef::Convergence(id) => id,
                                _ => unreachable!("checked above"),
                            },
                            git_operation_id: operation.id,
                            resolution_source,
                            approval_state,
                            synced_at: Utc::now(),
                        },
                    ))
                    .await?;
                Ok(FinalizeCompletionOutcome::Completed)
            }
            CheckoutFinalizationStatus::Synced => {
                let resolution_source = match context.revision.approval_policy {
                    ingot_domain::revision::ApprovalPolicy::Required => {
                        ResolutionSource::ApprovalCommand
                    }
                    ingot_domain::revision::ApprovalPolicy::NotRequired => {
                        ResolutionSource::SystemCommand
                    }
                };
                let approval_state = match context.revision.approval_policy {
                    ingot_domain::revision::ApprovalPolicy::Required => ApprovalState::Approved,
                    ingot_domain::revision::ApprovalPolicy::NotRequired => {
                        ApprovalState::NotRequired
                    }
                };
                self.db
                    .apply_finalization_mutation(FinalizationMutation::CheckoutAdoptionSucceeded(
                        FinalizationCheckoutAdoptionSucceededMutation {
                            project_id: operation.project_id,
                            item_id: context.item_id,
                            expected_item_revision_id: context.revision.id,
                            convergence_id: *match &operation.entity {
                                GitOperationEntityRef::Convergence(id) => id,
                                _ => unreachable!("checked above"),
                            },
                            git_operation_id: operation.id,
                            resolution_source,
                            approval_state,
                            synced_at: Utc::now(),
                        },
                    ))
                    .await?;
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
            OperationKind::FinalizeTargetRef => {
                let convergence_id = match &operation.entity {
                    GitOperationEntityRef::Convergence(id) => *id,
                    _ => unreachable!("checked above"),
                };
                let convergence = self.db.get_convergence(convergence_id).await?;
                let item = self.db.get_item(convergence.item_id).await?;
                let revision = self.db.get_revision(convergence.item_revision_id).await?;
                let resolution_source = match revision.approval_policy {
                    ingot_domain::revision::ApprovalPolicy::Required => {
                        ResolutionSource::ApprovalCommand
                    }
                    ingot_domain::revision::ApprovalPolicy::NotRequired => {
                        ResolutionSource::SystemCommand
                    }
                };
                let approval_state = match revision.approval_policy {
                    ingot_domain::revision::ApprovalPolicy::Required => ApprovalState::Approved,
                    ingot_domain::revision::ApprovalPolicy::NotRequired => {
                        ApprovalState::NotRequired
                    }
                };
                self.db
                    .apply_finalization_mutation(FinalizationMutation::TargetRefAdvanced(
                        FinalizationTargetRefAdvancedMutation {
                            project_id: operation.project_id,
                            item_id: item.id,
                            expected_item_revision_id: revision.id,
                            convergence_id,
                            git_operation_id: operation.id,
                            final_target_commit_oid: operation
                                .new_oid()
                                .or(operation.commit_oid())
                                .cloned()
                                .ok_or_else(|| {
                                    RuntimeError::InvalidState(
                                        "reconciled finalize_target_ref missing commit oid".into(),
                                    )
                                })?,
                            checkout_adoption: FinalizedCheckoutAdoption::synced(Utc::now()),
                        },
                    ))
                    .await?;
                self.db
                    .apply_finalization_mutation(FinalizationMutation::CheckoutAdoptionSucceeded(
                        FinalizationCheckoutAdoptionSucceededMutation {
                            project_id: operation.project_id,
                            item_id: item.id,
                            expected_item_revision_id: revision.id,
                            convergence_id,
                            git_operation_id: operation.id,
                            resolution_source,
                            approval_state,
                            synced_at: Utc::now(),
                        },
                    ))
                    .await
                    .map_err(RuntimeError::Repository)
            }
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
        let GitOperationEntityRef::Job(job_id) = &operation.entity else {
            return Err(RuntimeError::InvalidState(format!(
                "expected job entity, got {:?}",
                operation.entity.entity_type()
            )));
        };
        let job_id = *job_id;
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
            ActivitySubject::Job(job.id),
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

    async fn adopt_prepared_convergence(
        &self,
        operation: &GitOperation,
    ) -> Result<(), RuntimeError> {
        let GitOperationEntityRef::Convergence(convergence_id) = &operation.entity else {
            return Err(RuntimeError::InvalidState(format!(
                "expected convergence entity, got {:?}",
                operation.entity.entity_type()
            )));
        };
        let convergence_id = *convergence_id;
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
            convergence
                .transition_to_prepared(prepared_oid, Some(Utc::now()))
                .map_err(|error| RuntimeError::InvalidState(error.to_string()))?;
            self.db.update_convergence(&convergence).await?;
        }

        if let Some(workspace_id) = convergence.state.integration_workspace_id() {
            let mut workspace = self.db.get_workspace(workspace_id).await?;
            let now = Utc::now();
            if let Some(head_commit_oid) = operation
                .effective_commit_oid()
                .cloned()
                .or_else(|| workspace.state.head_commit_oid().cloned())
            {
                workspace.mark_ready_with_head(head_commit_oid, now);
            } else {
                workspace.release_to(WorkspaceStatus::Ready, now);
            }
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
        let GitOperationEntityRef::Convergence(convergence_id) = &operation.entity else {
            return Err(RuntimeError::InvalidState(format!(
                "expected convergence entity, got {:?}",
                operation.entity.entity_type()
            )));
        };
        let convergence_id = *convergence_id;
        let convergence = self.db.get_convergence(convergence_id).await?;
        let item = self.db.get_item(convergence.item_id).await?;
        let revision = self.db.get_revision(convergence.item_revision_id).await?;
        let target_ref = operation
            .ref_name()
            .cloned()
            .unwrap_or(convergence.target_ref.clone());
        let prepared_commit_oid = operation
            .new_oid()
            .or(operation.commit_oid())
            .ok_or_else(|| RuntimeError::InvalidState("finalize operation missing new oid".into()))?
            .clone();
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
        if let Some(head_commit_oid) = operation
            .new_oid()
            .cloned()
            .or_else(|| workspace.state.head_commit_oid().cloned())
        {
            workspace.mark_ready_with_head(head_commit_oid, now);
        } else {
            workspace.release_to(WorkspaceStatus::Ready, now);
        }
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

    pub(crate) async fn reconcile_assigned_job(&self, job: Job) -> Result<(), RuntimeError> {
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

    async fn reconcile_inert_assigned_dispatch_job(&self, job: Job) -> Result<bool, RuntimeError> {
        if !is_inert_assigned_authoring_dispatch_residue(&job) {
            return Ok(false);
        }

        let _guard = self
            .project_locks
            .acquire_project_mutation(job.project_id)
            .await;
        let mut job = self.db.get_job(job.id).await?;
        if !is_inert_assigned_authoring_dispatch_residue(&job) {
            return Ok(false);
        }

        let item = self.db.get_item(job.item_id).await?;
        if item.current_revision_id != job.item_revision_id {
            return Ok(false);
        }

        let Some(workspace_id) = job.state.workspace_id() else {
            return Ok(false);
        };
        let workspace = self.db.get_workspace(workspace_id).await?;
        if workspace.kind != WorkspaceKind::Authoring
            || workspace.project_id != job.project_id
            || workspace.created_for_revision_id != Some(job.item_revision_id)
            || workspace.state.status() != WorkspaceStatus::Ready
            || workspace.state.current_job_id().is_some()
        {
            return Ok(false);
        }

        job.state = JobState::Queued;
        self.db.update_job(&job).await?;

        Ok(true)
    }

    async fn reconcile_running_job(&self, job: Job) -> Result<bool, RuntimeError> {
        let expired = job
            .state
            .lease_expires_at()
            .is_none_or(|lease| lease <= Utc::now());
        let foreign_owner = job.state.lease_owner_id() != Some(&self.lease_owner_id);
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
            ActivitySubject::Job(job.id),
            serde_json::json!({ "item_id": job.item_id, "error_code": "heartbeat_expired" }),
        )
        .await?;

        Ok(true)
    }

    pub(crate) async fn reconcile_active_convergences(&self) -> Result<bool, RuntimeError> {
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
                ActivitySubject::Convergence(convergence.id),
                serde_json::json!({ "item_id": convergence.item_id, "reason": "startup_recovery_required" }),
            )
            .await?;
            made_progress = true;
        }
        Ok(made_progress)
    }

    pub(crate) async fn reconcile_workspace_retention(&self) -> Result<bool, RuntimeError> {
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

    async fn workspace_can_be_removed(
        &self,
        _project: &Project,
        workspace: &Workspace,
    ) -> Result<bool, RuntimeError> {
        if workspace.kind == WorkspaceKind::Review {
            return Ok(true);
        }
        let Some(revision_id) = workspace.created_for_revision_id else {
            return Ok(true);
        };
        let revision = self.db.get_revision(revision_id).await?;
        let item = self.db.get_item(revision.item_id).await?;
        if matches!(
            workspace.kind,
            WorkspaceKind::Authoring | WorkspaceKind::Integration
        ) && item.current_revision_id == revision.id
            && item.lifecycle.is_open()
        {
            return Ok(false);
        }

        let findings = self.db.list_findings_by_item(item.id).await?;
        let head_commit_oid = workspace.state.head_commit_oid();
        let blocked = findings.iter().any(|finding| {
            finding.source_item_revision_id == revision.id
                && finding.triage.is_unresolved()
                && head_commit_oid.is_some_and(|oid| finding.source_subject_head_commit_oid == *oid)
                && match workspace.kind {
                    WorkspaceKind::Authoring => {
                        finding.source_subject_kind
                            == ingot_domain::finding::FindingSubjectKind::Candidate
                    }
                    WorkspaceKind::Integration => {
                        finding.source_subject_kind
                            == ingot_domain::finding::FindingSubjectKind::Integrated
                    }
                    WorkspaceKind::Review => false,
                }
        });

        Ok(!blocked)
    }

    async fn remove_abandoned_workspace(
        &self,
        project: &Project,
        workspace: &Workspace,
    ) -> Result<(), RuntimeError> {
        let repo_path = self.project_paths(project).mirror_git_dir;
        let path = &workspace.path;
        if path.exists() {
            remove_workspace(repo_path.as_path(), path).await?;
        }

        if let Some(workspace_ref) = workspace.workspace_ref.as_ref()
            && let Some(current_oid) = resolve_ref_oid(repo_path.as_path(), workspace_ref).await?
        {
            let mut operation = GitOperation {
                id: GitOperationId::new(),
                project_id: project.id,
                entity: GitOperationEntityRef::Workspace(workspace.id),
                payload: OperationPayload::RemoveWorkspaceRef {
                    workspace_id: workspace.id,
                    ref_name: workspace_ref.clone(),
                    expected_old_oid: current_oid,
                },
                status: GitOperationStatus::Planned,
                created_at: Utc::now(),
                completed_at: None,
            };
            self.db.create_git_operation(&operation).await?;
            self.append_activity(
                project.id,
                ActivityEventType::GitOperationPlanned,
                ActivitySubject::GitOperation(operation.id),
                serde_json::json!({ "operation_kind": operation.operation_kind(), "entity_id": operation.entity.entity_id_string() }),
            )
            .await?;
            delete_ref(repo_path.as_path(), workspace_ref).await?;
            operation.status = GitOperationStatus::Applied;
            operation.completed_at = Some(Utc::now());
            self.db.update_git_operation(&operation).await?;
        }

        Ok(())
    }
}
