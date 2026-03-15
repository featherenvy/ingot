use chrono::Utc;
use ingot_domain::ports::WorkspaceRepository;
use ingot_domain::workspace::{Workspace, WorkspaceStatus};

use crate::UseCaseError;

/// Mark a workspace as Abandoned and clear its current_job_id. Pure DB operation.
/// Idempotent: returns the workspace unchanged if already Abandoned.
pub async fn abandon_workspace<W: WorkspaceRepository>(
    workspace_repo: &W,
    workspace: &Workspace,
) -> Result<Workspace, UseCaseError> {
    if workspace.status == WorkspaceStatus::Abandoned {
        return Ok(workspace.clone());
    }
    let mut updated = workspace.clone();
    updated.status = WorkspaceStatus::Abandoned;
    updated.current_job_id = None;
    updated.updated_at = Utc::now();
    workspace_repo.update(&updated).await?;
    Ok(updated)
}

/// Set a workspace status to Removing (prior to filesystem cleanup). Pure DB operation.
pub async fn plan_workspace_removal<W: WorkspaceRepository>(
    workspace_repo: &W,
    workspace: &Workspace,
) -> Result<Workspace, UseCaseError> {
    let mut updated = workspace.clone();
    updated.status = WorkspaceStatus::Removing;
    updated.updated_at = Utc::now();
    workspace_repo.update(&updated).await?;
    Ok(updated)
}

/// Set a workspace status to Abandoned after filesystem cleanup is done. Pure DB operation.
pub async fn finalize_workspace_removal<W: WorkspaceRepository>(
    workspace_repo: &W,
    workspace: &Workspace,
) -> Result<Workspace, UseCaseError> {
    let mut updated = workspace.clone();
    updated.status = WorkspaceStatus::Abandoned;
    updated.current_job_id = None;
    updated.updated_at = Utc::now();
    workspace_repo.update(&updated).await?;
    Ok(updated)
}
