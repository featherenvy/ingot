use std::future::Future;
use std::path::Path;

use chrono::Utc;
use ingot_domain::commit_oid::CommitOid;
use ingot_domain::git_operation::{
    GitOperation, GitOperationEntityRef, GitOperationStatus, OperationPayload,
};
use ingot_domain::git_ref::GitRef;
use ingot_domain::ids::{GitOperationId, ProjectId};
use ingot_domain::ports::{ActivityRepository, GitOperationRepository, WorkspaceRepository};
use ingot_domain::workspace::{Workspace, WorkspaceKind, WorkspaceStatus};

use crate::UseCaseError;
use crate::git_operation_journal::{create_planned, mark_applied};

pub async fn abandon_workspace<W: WorkspaceRepository>(
    workspace_repo: &W,
    workspace: &Workspace,
) -> Result<Workspace, UseCaseError> {
    if workspace.state.status() == WorkspaceStatus::Abandoned {
        return Ok(workspace.clone());
    }
    let mut updated = workspace.clone();
    updated.mark_abandoned(Utc::now());
    workspace_repo.update(&updated).await?;
    Ok(updated)
}

pub async fn plan_workspace_removal<W: WorkspaceRepository>(
    workspace_repo: &W,
    workspace: &Workspace,
) -> Result<Workspace, UseCaseError> {
    let mut updated = workspace.clone();
    updated.mark_removing(Utc::now());
    workspace_repo.update(&updated).await?;
    Ok(updated)
}

pub async fn finalize_workspace_removal<W: WorkspaceRepository>(
    workspace_repo: &W,
    workspace: &Workspace,
) -> Result<Workspace, UseCaseError> {
    let mut updated = workspace.clone();
    updated.mark_abandoned(Utc::now());
    workspace_repo.update(&updated).await?;
    Ok(updated)
}

pub trait WorkspaceInfraPort: Send + Sync {
    fn reset_worktree(
        &self,
        project_id: ProjectId,
        workspace_path: &Path,
        workspace_ref: Option<&GitRef>,
        expected_head: &CommitOid,
        kind: WorkspaceKind,
    ) -> impl Future<Output = Result<(), UseCaseError>> + Send;

    fn remove_workspace_files(
        &self,
        project_id: ProjectId,
        workspace_path: &Path,
    ) -> impl Future<Output = Result<(), UseCaseError>> + Send;

    fn resolve_ref_oid(
        &self,
        project_id: ProjectId,
        ref_name: &GitRef,
    ) -> impl Future<Output = Result<Option<CommitOid>, UseCaseError>> + Send;

    fn delete_ref(
        &self,
        project_id: ProjectId,
        ref_name: &GitRef,
    ) -> impl Future<Output = Result<(), UseCaseError>> + Send;
}

pub async fn reset_workspace<W, GO, A, G>(
    workspace_repo: &W,
    git_op_repo: &GO,
    activity_repo: &A,
    git_port: &G,
    project_id: ProjectId,
    workspace: &Workspace,
) -> Result<Workspace, UseCaseError>
where
    W: WorkspaceRepository,
    GO: GitOperationRepository,
    A: ActivityRepository,
    G: WorkspaceInfraPort,
{
    let expected_head = workspace
        .state
        .head_commit_oid()
        .cloned()
        .ok_or_else(|| UseCaseError::Internal("workspace missing head_commit_oid".into()))?;

    let now = Utc::now();
    let mut operation = GitOperation {
        id: GitOperationId::new(),
        project_id,
        entity: GitOperationEntityRef::Workspace(workspace.id),
        payload: OperationPayload::ResetWorkspace {
            workspace_id: workspace.id,
            ref_name: workspace.workspace_ref.clone(),
            expected_old_oid: workspace.state.head_commit_oid().cloned(),
            new_oid: expected_head.clone(),
        },
        status: GitOperationStatus::Planned,
        created_at: now,
        completed_at: None,
    };
    create_planned(git_op_repo, activity_repo, &operation, project_id).await?;

    git_port
        .reset_worktree(
            project_id,
            &workspace.path,
            workspace.workspace_ref.as_ref(),
            &expected_head,
            workspace.kind,
        )
        .await?;

    let mut updated = workspace.clone();
    updated.mark_ready_with_head(expected_head, Utc::now());
    workspace_repo
        .update(&updated)
        .await
        .map_err(UseCaseError::Repository)?;

    mark_applied(git_op_repo, &mut operation).await?;

    Ok(updated)
}

pub async fn remove_workspace_full<W, GO, A, G>(
    workspace_repo: &W,
    git_op_repo: &GO,
    activity_repo: &A,
    git_port: &G,
    project_id: ProjectId,
    workspace: &Workspace,
) -> Result<Workspace, UseCaseError>
where
    W: WorkspaceRepository,
    GO: GitOperationRepository,
    A: ActivityRepository,
    G: WorkspaceInfraPort,
{
    let workspace = plan_workspace_removal(workspace_repo, workspace).await?;

    if workspace.path.exists() {
        git_port
            .remove_workspace_files(project_id, &workspace.path)
            .await?;
    }

    if let Some(workspace_ref) = workspace.workspace_ref.as_ref() {
        let current_ref_oid = git_port.resolve_ref_oid(project_id, workspace_ref).await?;
        if let Some(expected_old_oid) = current_ref_oid {
            let now = Utc::now();
            let mut operation = GitOperation {
                id: GitOperationId::new(),
                project_id,
                entity: GitOperationEntityRef::Workspace(workspace.id),
                payload: OperationPayload::RemoveWorkspaceRef {
                    workspace_id: workspace.id,
                    ref_name: workspace_ref.clone(),
                    expected_old_oid,
                },
                status: GitOperationStatus::Planned,
                created_at: now,
                completed_at: None,
            };
            create_planned(git_op_repo, activity_repo, &operation, project_id).await?;
            git_port.delete_ref(project_id, workspace_ref).await?;
            mark_applied(git_op_repo, &mut operation).await?;
        }
    }

    finalize_workspace_removal(workspace_repo, &workspace).await
}
