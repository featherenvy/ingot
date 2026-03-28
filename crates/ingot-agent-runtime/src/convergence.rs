// Convergence preparation, finalization, and invalidation.

use std::path::PathBuf;

use chrono::Utc;
use ingot_domain::activity::{ActivityEventType, ActivitySubject};
use ingot_domain::commit_oid::CommitOid;
use ingot_domain::convergence::{Convergence, ConvergenceStatus, PrepareFailureKind};
use ingot_domain::convergence_queue::{ConvergenceQueueEntry, ConvergenceQueueEntryStatus};
use ingot_domain::git_operation::{
    ConvergenceReplayMetadata, GitOperation, GitOperationEntityRef, GitOperationStatus,
    OperationPayload,
};
use ingot_domain::git_ref::GitRef;
use ingot_domain::ids::{GitOperationId, WorkspaceId};
use ingot_domain::item::{ApprovalState, Escalation, EscalationReason};
use ingot_domain::job::Job;
use ingot_domain::ports::ProjectMutationLockPort;
use ingot_domain::project::Project;
use ingot_domain::revision::ItemRevision;
use ingot_domain::step_id::StepId;
use ingot_domain::workspace::{
    RetentionPolicy, Workspace, WorkspaceCommitState, WorkspaceKind, WorkspaceState,
    WorkspaceStrategy,
};
use ingot_git::commands::{git, resolve_ref_oid};
use ingot_git::commit::{
    ConvergenceCommitTrailers, abort_cherry_pick, cherry_pick_no_commit, commit_message,
    list_commits_oldest_first, working_tree_has_changes,
};
use ingot_git::project_repo::{CheckoutSyncStatus, checkout_sync_status};
use ingot_usecases::convergence::{FinalizePreparedTrigger, finalize_prepared_convergence};
use ingot_usecases::job::{DispatchJobCommand, dispatch_job};
use ingot_workspace::provision_integration_workspace;
use tracing::info;

use crate::{JobDispatcher, RuntimeError, RuntimeFinalizePort, usecase_to_runtime_error};

impl JobDispatcher {
    pub(crate) async fn auto_finalize_prepared_convergence(
        &self,
        project_id: ingot_domain::ids::ProjectId,
        item_id: ingot_domain::ids::ItemId,
    ) -> Result<bool, RuntimeError> {
        let _guard = self
            .project_locks
            .acquire_project_mutation(project_id)
            .await;
        let project = self.db.get_project(project_id).await?;
        let paths = self.refresh_project_mirror(&project).await?;
        let item = self.db.get_item(item_id).await?;
        let revision = self.db.get_revision(item.current_revision_id).await?;
        let jobs = self.db.list_jobs_by_item(item.id).await?;
        let findings = self.db.list_findings_by_item(item.id).await?;
        let convergences = self
            .hydrate_convergences(&project, self.db.list_convergences_by_item(item.id).await?)
            .await?;
        let queue_entry = self
            .db
            .find_active_queue_entry_for_revision(revision.id)
            .await?;
        if !ingot_usecases::convergence::should_auto_finalize_prepared_convergence(
            &item,
            &revision,
            &jobs,
            &findings,
            &convergences,
            queue_entry.as_ref(),
        ) {
            return Ok(false);
        }

        let convergence = convergences
            .into_iter()
            .find(|convergence| {
                convergence.item_revision_id == revision.id
                    && convergence.state.status() == ConvergenceStatus::Prepared
            })
            .ok_or_else(|| RuntimeError::InvalidState("prepared convergence missing".into()))?;
        let prepared_commit_oid = convergence
            .state
            .prepared_commit_oid()
            .map(ToOwned::to_owned)
            .ok_or_else(|| RuntimeError::InvalidState("prepared commit missing".into()))?;
        let input_target_commit_oid = convergence
            .state
            .input_target_commit_oid()
            .map(ToOwned::to_owned)
            .ok_or_else(|| RuntimeError::InvalidState("input target commit missing".into()))?;
        let current_target_oid =
            resolve_ref_oid(paths.mirror_git_dir.as_path(), &convergence.target_ref).await?;
        let target_valid = current_target_oid.as_ref() == Some(&prepared_commit_oid)
            || current_target_oid.as_ref() == Some(&input_target_commit_oid);
        if !target_valid {
            return Ok(false);
        }

        match finalize_prepared_convergence(
            &RuntimeFinalizePort {
                dispatcher: self.clone(),
            },
            FinalizePreparedTrigger::SystemCommand,
            &project,
            &item,
            &revision,
            &convergence,
            queue_entry
                .as_ref()
                .expect("queue head already validated for auto-finalize"),
        )
        .await
        {
            Ok(()) => {}
            Err(ingot_usecases::UseCaseError::ProtocolViolation(_)) => return Ok(false),
            Err(error) => return Err(usecase_to_runtime_error(error)),
        }

        info!(item_id = %item.id, convergence_id = %convergence.id, "auto-finalized prepared convergence");
        Ok(true)
    }

    pub(crate) async fn invalidate_prepared_convergence(
        &self,
        project_id: ingot_domain::ids::ProjectId,
        item_id: ingot_domain::ids::ItemId,
    ) -> Result<(), RuntimeError> {
        let _guard = self
            .project_locks
            .acquire_project_mutation(project_id)
            .await;
        let project = self.db.get_project(project_id).await?;
        let mut item = self.db.get_item(item_id).await?;
        let revision = self.db.get_revision(item.current_revision_id).await?;
        let jobs = self.db.list_jobs_by_item(item.id).await?;
        let findings = self.db.list_findings_by_item(item.id).await?;
        let convergences = self
            .hydrate_convergences(&project, self.db.list_convergences_by_item(item.id).await?)
            .await?;
        if !ingot_usecases::convergence::should_invalidate_prepared_convergence(
            &item,
            &revision,
            &jobs,
            &findings,
            &convergences,
        ) {
            return Ok(());
        }

        let invalidated = ingot_usecases::convergence::invalidate_prepared_convergence(
            &self.db,
            &self.db,
            &mut item,
            &revision,
            &convergences,
        )
        .await
        .map_err(|e| RuntimeError::InvalidState(e.to_string()))?;

        if invalidated {
            info!(item_id = %item.id, "invalidated stale prepared convergence");
        }
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn fail_prepare_convergence_attempt(
        &self,
        project: &Project,
        item: &ingot_domain::item::Item,
        revision: &ItemRevision,
        queue_entry: &ConvergenceQueueEntry,
        integration_workspace: &mut Workspace,
        convergence: &mut Convergence,
        operation: &mut GitOperation,
        source_commit_oids: &[CommitOid],
        prepared_commit_oids: &[CommitOid],
        summary: String,
        failure_kind: PrepareFailureKind,
    ) -> Result<(), RuntimeError> {
        integration_workspace.mark_error(Utc::now());
        self.db.update_workspace(integration_workspace).await?;

        match failure_kind {
            PrepareFailureKind::Conflicted => {
                convergence
                    .transition_to_conflicted(summary.clone(), Utc::now())
                    .map_err(|error| RuntimeError::InvalidState(error.to_string()))?;
            }
            PrepareFailureKind::Failed => {
                convergence.transition_to_failed(Some(summary.clone()), Utc::now());
            }
        }
        self.db.update_convergence(convergence).await?;

        let escalation_reason = match failure_kind {
            PrepareFailureKind::Conflicted => EscalationReason::ConvergenceConflict,
            PrepareFailureKind::Failed => EscalationReason::StepFailed,
        };
        let mut escalated_item = self.db.get_item(item.id).await?;
        escalated_item.approval_state = match revision.approval_policy {
            ingot_domain::revision::ApprovalPolicy::Required => ApprovalState::NotRequested,
            ingot_domain::revision::ApprovalPolicy::NotRequired => ApprovalState::NotRequired,
        };
        escalated_item.escalation = Escalation::OperatorRequired {
            reason: escalation_reason,
        };
        escalated_item.updated_at = Utc::now();
        self.db.update_item(&escalated_item).await?;

        let mut released_queue = queue_entry.clone();
        released_queue.status = ConvergenceQueueEntryStatus::Released;
        released_queue.released_at = Some(Utc::now());
        released_queue.updated_at = Utc::now();
        self.db.update_queue_entry(&released_queue).await?;

        operation.status = GitOperationStatus::Failed;
        operation.completed_at = Some(Utc::now());
        operation
            .payload
            .set_replay_metadata(ConvergenceReplayMetadata {
                source_commit_oids: source_commit_oids.to_vec(),
                prepared_commit_oids: prepared_commit_oids.to_vec(),
            })
            .map_err(|error| RuntimeError::InvalidState(error.to_string()))?;
        self.db.update_git_operation(operation).await?;

        let event_type = match failure_kind {
            PrepareFailureKind::Conflicted => ActivityEventType::ConvergenceConflicted,
            PrepareFailureKind::Failed => ActivityEventType::ConvergenceFailed,
        };
        self.append_activity(
            project.id,
            event_type,
            ActivitySubject::Convergence(convergence.id),
            serde_json::json!({ "item_id": item.id, "summary": summary }),
        )
        .await?;
        self.append_activity(
            project.id,
            ActivityEventType::ItemEscalated,
            ActivitySubject::Item(item.id),
            serde_json::json!({ "reason": escalation_reason }),
        )
        .await?;

        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn prepare_queue_head_convergence(
        &self,
        project: &Project,
        item: &ingot_domain::item::Item,
        revision: &ItemRevision,
        jobs: &[Job],
        findings: &[ingot_domain::finding::Finding],
        convergences: &[Convergence],
        queue_entry: &ConvergenceQueueEntry,
    ) -> Result<(), RuntimeError> {
        let _guard = self
            .project_locks
            .acquire_project_mutation(project.id)
            .await;

        let current_item = self.db.get_item(item.id).await?;
        if current_item.current_revision_id != revision.id {
            return Ok(());
        }
        let current_queue = self
            .db
            .find_active_queue_entry_for_revision(revision.id)
            .await?;
        if current_queue
            .as_ref()
            .map(|entry| {
                entry.id != queue_entry.id || entry.status != ConvergenceQueueEntryStatus::Head
            })
            .unwrap_or(true)
        {
            return Ok(());
        }

        if convergences.iter().any(|convergence| {
            convergence.item_revision_id == revision.id && convergence.state.is_active()
        }) {
            return Ok(());
        }

        let source_workspace = self
            .db
            .find_authoring_workspace_for_revision(revision.id)
            .await?
            .ok_or_else(|| RuntimeError::InvalidState("authoring workspace missing".into()))?;
        let source_head_commit_oid = self
            .current_authoring_head_for_revision_with_workspace(revision, jobs)
            .await?
            .ok_or_else(|| RuntimeError::InvalidState("authoring head commit missing".into()))?;
        let paths = self.refresh_project_mirror(project).await?;
        let repo_path = paths.mirror_git_dir.as_path();
        let input_target_commit_oid = resolve_ref_oid(repo_path, &revision.target_ref)
            .await?
            .ok_or_else(|| RuntimeError::InvalidState("target ref unresolved".into()))?;

        let integration_workspace_id = WorkspaceId::new();
        let integration_workspace_path = paths
            .worktree_root
            .join(integration_workspace_id.to_string());
        let integration_workspace_ref =
            GitRef::new(format!("refs/ingot/workspaces/{integration_workspace_id}"));
        let now = Utc::now();
        let mut integration_workspace = Workspace {
            id: integration_workspace_id,
            project_id: project.id,
            kind: WorkspaceKind::Integration,
            strategy: WorkspaceStrategy::Worktree,
            path: integration_workspace_path.clone(),
            created_for_revision_id: Some(revision.id),
            parent_workspace_id: Some(source_workspace.id),
            target_ref: Some(revision.target_ref.clone()),
            workspace_ref: Some(integration_workspace_ref.clone()),
            retention_policy: RetentionPolicy::Persistent,
            created_at: now,
            updated_at: now,
            state: WorkspaceState::Provisioning {
                commits: Some(WorkspaceCommitState::new(
                    input_target_commit_oid.clone(),
                    input_target_commit_oid.clone(),
                )),
            },
        };
        self.db.create_workspace(&integration_workspace).await?;

        let provisioned = provision_integration_workspace(
            repo_path,
            &integration_workspace_path,
            &integration_workspace_ref,
            &input_target_commit_oid,
        )
        .await?;
        integration_workspace.path = provisioned.workspace_path.clone();
        integration_workspace.workspace_ref = Some(provisioned.workspace_ref);
        integration_workspace.set_head_commit_oid(provisioned.head_commit_oid, Utc::now());
        self.db.update_workspace(&integration_workspace).await?;

        let mut convergence = Convergence {
            id: ingot_domain::ids::ConvergenceId::new(),
            project_id: project.id,
            item_id: item.id,
            item_revision_id: revision.id,
            source_workspace_id: source_workspace.id,
            source_head_commit_oid: source_head_commit_oid.clone(),
            target_ref: revision.target_ref.clone(),
            strategy: ingot_domain::convergence::ConvergenceStrategy::RebaseThenFastForward,
            target_head_valid: Some(true),
            created_at: now,
            state: ingot_domain::convergence::ConvergenceState::Running {
                integration_workspace_id: integration_workspace.id,
                input_target_commit_oid: input_target_commit_oid.clone(),
            },
        };
        self.db.create_convergence(&convergence).await?;
        self.append_activity(
            project.id,
            ActivityEventType::ConvergenceStarted,
            ActivitySubject::Convergence(convergence.id),
            serde_json::json!({ "item_id": item.id, "queue_entry_id": queue_entry.id }),
        )
        .await?;

        let source_base_commit_oid = self
            .effective_authoring_base_commit_oid(revision)
            .await?
            .ok_or_else(|| RuntimeError::InvalidState("authoring base commit missing".into()))?;
        let source_commit_oids =
            list_commits_oldest_first(repo_path, &source_base_commit_oid, &source_head_commit_oid)
                .await?;
        let mut operation = GitOperation {
            id: GitOperationId::new(),
            project_id: project.id,
            entity: GitOperationEntityRef::Convergence(convergence.id),
            payload: OperationPayload::PrepareConvergenceCommit {
                workspace_id: integration_workspace.id,
                ref_name: integration_workspace.workspace_ref.clone(),
                expected_old_oid: input_target_commit_oid.clone(),
                new_oid: None,
                commit_oid: None,
                replay_metadata: Some(ConvergenceReplayMetadata {
                    source_commit_oids: source_commit_oids.clone(),
                    prepared_commit_oids: vec![],
                }),
            },
            status: GitOperationStatus::Planned,
            created_at: now,
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

        let integration_workspace_dir = PathBuf::from(&integration_workspace.path);
        let mut prepared_tip = input_target_commit_oid.clone();
        let mut prepared_commit_oids = Vec::with_capacity(source_commit_oids.len());

        for source_commit_oid in &source_commit_oids {
            if let Err(error) =
                cherry_pick_no_commit(&integration_workspace_dir, source_commit_oid).await
            {
                let _ = abort_cherry_pick(&integration_workspace_dir).await;
                self.fail_prepare_convergence_attempt(
                    project,
                    item,
                    revision,
                    queue_entry,
                    &mut integration_workspace,
                    &mut convergence,
                    &mut operation,
                    &source_commit_oids,
                    &prepared_commit_oids,
                    error.to_string(),
                    PrepareFailureKind::Conflicted,
                )
                .await?;
                return Ok(());
            }

            let has_replay_changes =
                match working_tree_has_changes(&integration_workspace_dir).await {
                    Ok(has_changes) => has_changes,
                    Err(error) => {
                        self.fail_prepare_convergence_attempt(
                            project,
                            item,
                            revision,
                            queue_entry,
                            &mut integration_workspace,
                            &mut convergence,
                            &mut operation,
                            &source_commit_oids,
                            &prepared_commit_oids,
                            error.to_string(),
                            PrepareFailureKind::Failed,
                        )
                        .await?;
                        return Ok(());
                    }
                };
            if !has_replay_changes {
                continue;
            }

            let original_message = match commit_message(repo_path, source_commit_oid).await {
                Ok(message) => message,
                Err(error) => {
                    self.fail_prepare_convergence_attempt(
                        project,
                        item,
                        revision,
                        queue_entry,
                        &mut integration_workspace,
                        &mut convergence,
                        &mut operation,
                        &source_commit_oids,
                        &prepared_commit_oids,
                        error.to_string(),
                        PrepareFailureKind::Failed,
                    )
                    .await?;
                    return Ok(());
                }
            };
            let next_prepared_tip = match ingot_git::commit::create_daemon_convergence_commit(
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
                    self.fail_prepare_convergence_attempt(
                        project,
                        item,
                        revision,
                        queue_entry,
                        &mut integration_workspace,
                        &mut convergence,
                        &mut operation,
                        &source_commit_oids,
                        &prepared_commit_oids,
                        error.to_string(),
                        PrepareFailureKind::Failed,
                    )
                    .await?;
                    return Ok(());
                }
            };
            if let Some(workspace_ref) = integration_workspace.workspace_ref.as_ref() {
                if let Err(error) = git(
                    repo_path,
                    &[
                        "update-ref",
                        workspace_ref.as_str(),
                        next_prepared_tip.as_str(),
                    ],
                )
                .await
                {
                    self.fail_prepare_convergence_attempt(
                        project,
                        item,
                        revision,
                        queue_entry,
                        &mut integration_workspace,
                        &mut convergence,
                        &mut operation,
                        &source_commit_oids,
                        &prepared_commit_oids,
                        error.to_string(),
                        PrepareFailureKind::Failed,
                    )
                    .await?;
                    return Ok(());
                }
            }
            prepared_tip = next_prepared_tip;
            prepared_commit_oids.push(prepared_tip.clone());
        }

        integration_workspace.mark_ready_with_head(prepared_tip.clone(), Utc::now());
        self.db.update_workspace(&integration_workspace).await?;

        convergence
            .transition_to_prepared(prepared_tip.clone(), Some(Utc::now()))
            .map_err(|error| RuntimeError::InvalidState(error.to_string()))?;
        self.db.update_convergence(&convergence).await?;

        operation
            .payload
            .set_convergence_commit_result(prepared_tip.clone())
            .map_err(|error| RuntimeError::InvalidState(error.to_string()))?;
        operation
            .payload
            .set_replay_metadata(ConvergenceReplayMetadata {
                source_commit_oids,
                prepared_commit_oids,
            })
            .map_err(|error| RuntimeError::InvalidState(error.to_string()))?;
        self.mark_git_operation_reconciled(&mut operation).await?;

        let mut all_convergences = convergences.to_vec();
        all_convergences.push(convergence.clone());
        let validation_job = dispatch_job(
            &current_item,
            revision,
            jobs,
            findings,
            &all_convergences,
            DispatchJobCommand {
                step_id: Some(StepId::ValidateIntegrated),
            },
        )
        .map_err(|error| RuntimeError::InvalidState(error.to_string()))?;
        self.db.create_job(&validation_job).await?;
        self.append_activity(
            project.id,
            ActivityEventType::ConvergencePrepared,
            ActivitySubject::Convergence(convergence.id),
            serde_json::json!({ "item_id": item.id, "validation_job_id": validation_job.id }),
        )
        .await?;
        self.append_activity(
            project.id,
            ActivityEventType::JobDispatched,
            ActivitySubject::Job(validation_job.id),
            serde_json::json!({ "item_id": item.id, "step_id": validation_job.step_id }),
        )
        .await?;

        Ok(())
    }

    pub(crate) async fn reconcile_checkout_sync_state(
        &self,
        project: &Project,
        item_id: ingot_domain::ids::ItemId,
        revision: &ItemRevision,
    ) -> Result<CheckoutSyncStatus, RuntimeError> {
        let mut item = self.db.get_item(item_id).await?;
        let status = checkout_sync_status(&project.path, &revision.target_ref).await?;
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
                    self.db.update_item(&item).await?;
                    self.append_activity(
                        project.id,
                        ActivityEventType::CheckoutSyncCleared,
                        ActivitySubject::Item(item.id),
                        serde_json::json!({}),
                    )
                    .await?;
                    self.append_activity(
                        project.id,
                        ActivityEventType::ItemEscalationCleared,
                        ActivitySubject::Item(item.id),
                        serde_json::json!({ "reason": "checkout_sync_ready" }),
                    )
                    .await?;
                }
            }
            CheckoutSyncStatus::Blocked { message, .. } => {
                if !checkout_sync_blocked {
                    item.escalation = Escalation::OperatorRequired {
                        reason: EscalationReason::CheckoutSyncBlocked,
                    };
                    item.updated_at = Utc::now();
                    self.db.update_item(&item).await?;
                    self.append_activity(
                        project.id,
                        ActivityEventType::CheckoutSyncBlocked,
                        ActivitySubject::Item(item.id),
                        serde_json::json!({ "message": message }),
                    )
                    .await?;
                    self.append_activity(
                        project.id,
                        ActivityEventType::ItemEscalated,
                        ActivitySubject::Item(item.id),
                        serde_json::json!({ "reason": EscalationReason::CheckoutSyncBlocked }),
                    )
                    .await?;
                }
            }
        }

        Ok(status)
    }
}
