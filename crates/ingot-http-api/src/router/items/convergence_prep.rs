use std::path::PathBuf;

use chrono::Utc;
use ingot_domain::activity::{ActivityEventType, ActivitySubject};
use ingot_domain::commit_oid::CommitOid;
use ingot_domain::convergence::Convergence;
use ingot_domain::git_operation::{
    ConvergenceReplayMetadata, GitOperation, GitOperationEntityRef, GitOperationStatus,
    OperationPayload,
};
use ingot_domain::git_ref::GitRef;
use ingot_domain::ids::WorkspaceId;
use ingot_domain::item::{Escalation, EscalationReason, Item};
use ingot_domain::project::Project;
use ingot_domain::revision::ItemRevision;
use ingot_domain::workspace::{Workspace, WorkspaceKind};
use ingot_git::commit::ConvergenceCommitTrailers;
use ingot_usecases::UseCaseError;

use crate::error::ApiError;
use crate::router::AppState;
use crate::router::infra_ports::HttpInfraAdapter;
use crate::router::support::{activity::append_activity, errors::repo_to_internal};

use super::effective_authoring_base_commit_oid;

enum ReplayFailureKind {
    Conflict,
    StepFailed,
}

struct ReplayContext<'a> {
    state: &'a AppState,
    project: &'a Project,
    item: &'a Item,
    convergence: &'a mut Convergence,
    integration_workspace: &'a mut Workspace,
    operation: &'a mut GitOperation,
    source_commit_oids: &'a [CommitOid],
    prepared_commit_oids: &'a [CommitOid],
}

async fn record_replay_failure(
    ctx: &mut ReplayContext<'_>,
    kind: ReplayFailureKind,
    error_summary: String,
) -> ApiError {
    let now = Utc::now();

    ctx.integration_workspace.mark_error(now);
    let _ = ctx
        .state
        .db
        .update_workspace(ctx.integration_workspace)
        .await;

    let (convergence_event, escalation_reason) = match kind {
        ReplayFailureKind::Conflict => {
            let _ = ctx
                .convergence
                .transition_to_conflicted(error_summary.clone(), now);
            (
                ActivityEventType::ConvergenceConflicted,
                EscalationReason::ConvergenceConflict,
            )
        }
        ReplayFailureKind::StepFailed => {
            ctx.convergence
                .transition_to_failed(Some(error_summary.clone()), now);
            (
                ActivityEventType::ConvergenceFailed,
                EscalationReason::StepFailed,
            )
        }
    };
    let _ = ctx.state.db.update_convergence(ctx.convergence).await;

    let mut escalated_item = ctx.item.clone();
    escalated_item.escalation = Escalation::OperatorRequired {
        reason: escalation_reason,
    };
    escalated_item.updated_at = now;
    let _ = ctx.state.db.update_item(&escalated_item).await;

    ctx.operation.status = GitOperationStatus::Failed;
    ctx.operation.completed_at = Some(now);
    let _ = ctx
        .operation
        .payload
        .set_replay_metadata(ConvergenceReplayMetadata {
            source_commit_oids: ctx.source_commit_oids.to_vec(),
            prepared_commit_oids: ctx.prepared_commit_oids.to_vec(),
        });
    let _ = ctx.state.db.update_git_operation(ctx.operation).await;

    let _ = append_activity(
        ctx.state,
        ctx.project.id,
        convergence_event,
        ActivitySubject::Convergence(ctx.convergence.id),
        serde_json::json!({ "item_id": ctx.item.id, "summary": error_summary }),
    )
    .await;
    let _ = append_activity(
        ctx.state,
        ctx.project.id,
        ActivityEventType::ItemEscalated,
        ActivitySubject::Item(ctx.item.id),
        serde_json::json!({ "reason": escalation_reason }),
    )
    .await;

    match kind {
        ReplayFailureKind::Conflict => ApiError::Conflict {
            code: "convergence_conflicted",
            message: "Convergence replay conflicted".into(),
        },
        ReplayFailureKind::StepFailed => ApiError::from(UseCaseError::Internal(error_summary)),
    }
}

#[allow(dead_code)]
pub(in crate::router) async fn prepare_convergence_workspace(
    state: &AppState,
    project: &Project,
    item: &Item,
    revision: &ItemRevision,
    source_workspace: &Workspace,
    source_head_commit_oid: &CommitOid,
) -> Result<Convergence, ApiError> {
    let infra = HttpInfraAdapter::new(state);
    let paths = infra.mirror_paths(project.id).await?;
    let input_target_commit_oid = infra
        .resolve_project_ref_oid(project.id, &revision.target_ref)
        .await?
        .ok_or_else(|| UseCaseError::TargetRefUnresolved(revision.target_ref.to_string()))?;

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
        strategy: ingot_domain::workspace::WorkspaceStrategy::Worktree,
        path: integration_workspace_path.clone(),
        created_for_revision_id: Some(revision.id),
        parent_workspace_id: Some(source_workspace.id),
        target_ref: Some(revision.target_ref.clone()),
        workspace_ref: Some(integration_workspace_ref.clone()),
        retention_policy: ingot_domain::workspace::RetentionPolicy::Persistent,
        state: ingot_domain::workspace::WorkspaceState::Provisioning {
            commits: Some(ingot_domain::workspace::WorkspaceCommitState::new(
                input_target_commit_oid.clone(),
                input_target_commit_oid.clone(),
            )),
        },
        created_at: now,
        updated_at: now,
    };
    state
        .db
        .create_workspace(&integration_workspace)
        .await
        .map_err(repo_to_internal)?;

    let provisioned = infra
        .provision_integration_workspace(
            project.id,
            &integration_workspace_path,
            &integration_workspace_ref,
            &input_target_commit_oid,
        )
        .await?;
    integration_workspace.path = provisioned.workspace_path.clone();
    integration_workspace.workspace_ref = Some(provisioned.workspace_ref);
    integration_workspace.mark_ready_with_head(provisioned.head_commit_oid, Utc::now());
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
    state
        .db
        .create_convergence(&convergence)
        .await
        .map_err(repo_to_internal)?;
    append_activity(
        state,
        project.id,
        ActivityEventType::ConvergenceStarted,
        ActivitySubject::Convergence(convergence.id),
        serde_json::json!({ "item_id": item.id }),
    )
    .await?;

    let source_base_commit_oid = effective_authoring_base_commit_oid(state, revision)
        .await?
        .ok_or_else(|| {
            ApiError::UseCase(UseCaseError::Internal(
                "convergence requires a bound authoring base commit".into(),
            ))
        })?;
    let source_commit_oids = infra
        .list_commits_oldest_first(project.id, &source_base_commit_oid, source_head_commit_oid)
        .await?;
    let mut operation = GitOperation {
        id: ingot_domain::ids::GitOperationId::new(),
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
    state
        .db
        .create_git_operation(&operation)
        .await
        .map_err(repo_to_internal)?;
    append_activity(
        state,
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
        if let Err(error) = infra
            .cherry_pick_no_commit(&integration_workspace_dir, source_commit_oid)
            .await
        {
            let _ = infra.abort_cherry_pick(&integration_workspace_dir).await;
            return Err(record_replay_failure(
                &mut ReplayContext {
                    state,
                    project,
                    item,
                    convergence: &mut convergence,
                    integration_workspace: &mut integration_workspace,
                    operation: &mut operation,
                    source_commit_oids: &source_commit_oids,
                    prepared_commit_oids: &prepared_commit_oids,
                },
                ReplayFailureKind::Conflict,
                format!("{error:?}"),
            )
            .await);
        }

        let has_replay_changes = infra
            .working_tree_has_changes(&integration_workspace_dir)
            .await?;
        if !has_replay_changes {
            continue;
        }

        let original_message = match infra.commit_message(project.id, source_commit_oid).await {
            Ok(message) => message,
            Err(error) => {
                let summary = format!("{error:?}");
                return Err(record_replay_failure(
                    &mut ReplayContext {
                        state,
                        project,
                        item,
                        convergence: &mut convergence,
                        integration_workspace: &mut integration_workspace,
                        operation: &mut operation,
                        source_commit_oids: &source_commit_oids,
                        prepared_commit_oids: &prepared_commit_oids,
                    },
                    ReplayFailureKind::StepFailed,
                    summary,
                )
                .await);
            }
        };
        let next_prepared_tip = match infra
            .create_daemon_convergence_commit(
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
                let summary = format!("{error:?}");
                return Err(record_replay_failure(
                    &mut ReplayContext {
                        state,
                        project,
                        item,
                        convergence: &mut convergence,
                        integration_workspace: &mut integration_workspace,
                        operation: &mut operation,
                        source_commit_oids: &source_commit_oids,
                        prepared_commit_oids: &prepared_commit_oids,
                    },
                    ReplayFailureKind::StepFailed,
                    summary,
                )
                .await);
            }
        };
        if let Some(workspace_ref) = integration_workspace.workspace_ref.as_ref() {
            if let Err(error) = infra
                .update_project_ref_oid(project.id, workspace_ref, &next_prepared_tip)
                .await
            {
                let summary = format!("{error:?}");
                return Err(record_replay_failure(
                    &mut ReplayContext {
                        state,
                        project,
                        item,
                        convergence: &mut convergence,
                        integration_workspace: &mut integration_workspace,
                        operation: &mut operation,
                        source_commit_oids: &source_commit_oids,
                        prepared_commit_oids: &prepared_commit_oids,
                    },
                    ReplayFailureKind::StepFailed,
                    summary,
                )
                .await);
            }
        }
        prepared_tip = next_prepared_tip;
        prepared_commit_oids.push(prepared_tip.clone());
    }

    integration_workspace.mark_ready_with_head(prepared_tip.clone(), Utc::now());
    state
        .db
        .update_workspace(&integration_workspace)
        .await
        .map_err(repo_to_internal)?;

    convergence
        .transition_to_prepared(prepared_tip.clone(), Some(Utc::now()))
        .map_err(|error| ApiError::from(UseCaseError::Internal(error.to_string())))?;
    state
        .db
        .update_convergence(&convergence)
        .await
        .map_err(repo_to_internal)?;

    operation
        .payload
        .set_convergence_commit_result(prepared_tip)
        .map_err(|error| ApiError::from(UseCaseError::Internal(error.to_string())))?;
    operation
        .payload
        .set_replay_metadata(ConvergenceReplayMetadata {
            source_commit_oids,
            prepared_commit_oids,
        })
        .map_err(|error| ApiError::from(UseCaseError::Internal(error.to_string())))?;
    operation.status = GitOperationStatus::Applied;
    operation.completed_at = Some(Utc::now());
    state
        .db
        .update_git_operation(&operation)
        .await
        .map_err(repo_to_internal)?;

    Ok(convergence)
}
