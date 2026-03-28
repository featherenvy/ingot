use std::path::Path;

use ingot_domain::commit_oid::CommitOid;
use ingot_domain::git_ref::GitRef;
use ingot_domain::ids::ProjectId;
use ingot_domain::workspace::{Workspace, WorkspaceKind};
use ingot_git::project_repo::ProjectRepoPaths;
use ingot_usecases::UseCaseError;
use ingot_usecases::dispatch::DispatchInfraPort;
use ingot_usecases::workspace::WorkspaceInfraPort;

use super::AppState;
use super::support::{git_to_internal, refresh_project_mirror, workspace_to_api_error};

fn api_to_uc(err: crate::error::ApiError) -> UseCaseError {
    UseCaseError::Internal(format!("{err:?}"))
}

/// Adapter bridging infrastructure (ingot-git, ingot-workspace) to the
/// `DispatchInfraPort` / `WorkspaceInfraPort` traits defined in ingot-usecases.
pub(super) struct HttpInfraAdapter {
    pub(super) state: AppState,
}

impl HttpInfraAdapter {
    pub(super) fn new(state: &AppState) -> Self {
        Self {
            state: state.clone(),
        }
    }

    async fn mirror_paths(&self, project_id: ProjectId) -> Result<ProjectRepoPaths, UseCaseError> {
        let project = self
            .state
            .db
            .get_project(project_id)
            .await
            .map_err(UseCaseError::Repository)?;
        refresh_project_mirror(&self.state, &project)
            .await
            .map_err(api_to_uc)
    }
}

impl DispatchInfraPort for HttpInfraAdapter {
    async fn resolve_ref_oid(
        &self,
        project_id: ProjectId,
        ref_name: &GitRef,
    ) -> Result<Option<CommitOid>, UseCaseError> {
        let paths = self.mirror_paths(project_id).await?;
        ingot_git::commands::resolve_ref_oid(paths.mirror_git_dir.as_path(), ref_name)
            .await
            .map_err(git_to_internal)
            .map_err(api_to_uc)
    }

    async fn update_ref(
        &self,
        project_id: ProjectId,
        ref_name: &GitRef,
        commit_oid: &CommitOid,
    ) -> Result<(), UseCaseError> {
        let paths = self.mirror_paths(project_id).await?;
        ingot_git::commands::update_ref(paths.mirror_git_dir.as_path(), ref_name, commit_oid)
            .await
            .map_err(git_to_internal)
            .map_err(api_to_uc)
    }

    async fn delete_ref(
        &self,
        project_id: ProjectId,
        ref_name: &GitRef,
    ) -> Result<(), UseCaseError> {
        let paths = self.mirror_paths(project_id).await?;
        ingot_git::commands::delete_ref(paths.mirror_git_dir.as_path(), ref_name)
            .await
            .map_err(git_to_internal)
            .map_err(api_to_uc)
    }

    async fn remove_workspace_files(
        &self,
        project_id: ProjectId,
        workspace: &Workspace,
    ) -> Result<(), UseCaseError> {
        let paths = self.mirror_paths(project_id).await?;
        ingot_workspace::remove_workspace(paths.mirror_git_dir.as_path(), &workspace.path)
            .await
            .map_err(workspace_to_api_error)
            .map_err(api_to_uc)?;
        if let Some(workspace_ref) = workspace.workspace_ref.as_ref() {
            let _ = ingot_git::commands::delete_ref(paths.mirror_git_dir.as_path(), workspace_ref)
                .await;
        }
        Ok(())
    }
}

impl WorkspaceInfraPort for HttpInfraAdapter {
    async fn reset_worktree(
        &self,
        project_id: ProjectId,
        workspace_path: &Path,
        workspace_ref: Option<&GitRef>,
        expected_head: &CommitOid,
        kind: WorkspaceKind,
    ) -> Result<(), UseCaseError> {
        let paths = self.mirror_paths(project_id).await?;
        match kind {
            WorkspaceKind::Authoring | WorkspaceKind::Integration => {
                ingot_git::commands::git(
                    workspace_path,
                    &["reset", "--hard", expected_head.as_str()],
                )
                .await
                .map_err(git_to_internal)
                .map_err(api_to_uc)?;
                ingot_git::commands::git(workspace_path, &["clean", "-fd"])
                    .await
                    .map_err(git_to_internal)
                    .map_err(api_to_uc)?;
                if let Some(workspace_ref) = workspace_ref {
                    ingot_git::commands::git(
                        paths.mirror_git_dir.as_path(),
                        &["update-ref", workspace_ref.as_str(), expected_head.as_str()],
                    )
                    .await
                    .map_err(git_to_internal)
                    .map_err(api_to_uc)?;
                }
            }
            WorkspaceKind::Review => {
                ingot_workspace::provision_review_workspace(
                    paths.mirror_git_dir.as_path(),
                    workspace_path,
                    expected_head,
                )
                .await
                .map_err(workspace_to_api_error)
                .map_err(api_to_uc)?;
            }
        }
        Ok(())
    }

    async fn remove_workspace_files(
        &self,
        project_id: ProjectId,
        workspace_path: &Path,
    ) -> Result<(), UseCaseError> {
        let paths = self.mirror_paths(project_id).await?;
        ingot_workspace::remove_workspace(paths.mirror_git_dir.as_path(), workspace_path)
            .await
            .map_err(workspace_to_api_error)
            .map_err(api_to_uc)
    }

    async fn resolve_ref_oid(
        &self,
        project_id: ProjectId,
        ref_name: &GitRef,
    ) -> Result<Option<CommitOid>, UseCaseError> {
        let paths = self.mirror_paths(project_id).await?;
        ingot_git::commands::resolve_ref_oid(paths.mirror_git_dir.as_path(), ref_name)
            .await
            .map_err(git_to_internal)
            .map_err(api_to_uc)
    }

    async fn delete_ref(
        &self,
        project_id: ProjectId,
        ref_name: &GitRef,
    ) -> Result<(), UseCaseError> {
        let paths = self.mirror_paths(project_id).await?;
        ingot_git::commands::delete_ref(paths.mirror_git_dir.as_path(), ref_name)
            .await
            .map_err(git_to_internal)
            .map_err(api_to_uc)
    }
}
