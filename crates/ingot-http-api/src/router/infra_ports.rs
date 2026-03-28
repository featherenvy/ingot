use std::path::Path;

use ingot_domain::commit_oid::CommitOid;
use ingot_domain::git_ref::GitRef;
use ingot_domain::ids::ProjectId;
use ingot_domain::job::Job;
use ingot_domain::project::Project;
use ingot_domain::revision::ItemRevision;
use ingot_domain::workspace::{Workspace, WorkspaceKind};
use ingot_git::commands::FinalizeTargetRefOutcome;
use ingot_git::diff::changed_paths_between;
use ingot_git::project_repo::ProjectRepoPaths;
use ingot_git::project_repo::{
    CheckoutFinalizationStatus, CheckoutSyncStatus, checkout_finalization_status,
    checkout_sync_status, sync_checkout_to_commit,
};
use ingot_usecases::UseCaseError;
use ingot_usecases::dispatch::DispatchInfraPort;
use ingot_usecases::workspace::WorkspaceInfraPort;
use ingot_workspace::ensure_authoring_workspace_state;

use super::AppState;
use super::support::{git_to_internal, refresh_project_mirror, workspace_to_api_error};
use crate::error::ApiError;

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

    async fn mirror_paths(&self, project_id: ProjectId) -> Result<ProjectRepoPaths, ApiError> {
        let project = self
            .state
            .db
            .get_project(project_id)
            .await
            .map_err(UseCaseError::Repository)
            .map_err(ApiError::from)?;
        refresh_project_mirror(&self.state, &project).await
    }

    pub(super) async fn resolve_project_ref_oid(
        &self,
        project_id: ProjectId,
        ref_name: &GitRef,
    ) -> Result<Option<CommitOid>, ApiError> {
        let paths = self.mirror_paths(project_id).await?;
        ingot_git::commands::resolve_ref_oid(paths.mirror_git_dir.as_path(), ref_name)
            .await
            .map_err(git_to_internal)
    }

    pub(super) async fn changed_paths_between(
        &self,
        project_id: ProjectId,
        base_commit_oid: &CommitOid,
        head_commit_oid: &CommitOid,
    ) -> Result<Vec<String>, ApiError> {
        let paths = self.mirror_paths(project_id).await?;
        changed_paths_between(
            paths.mirror_git_dir.as_path(),
            base_commit_oid,
            head_commit_oid,
        )
        .await
        .map_err(git_to_internal)
    }

    pub(super) async fn ensure_authoring_workspace(
        &self,
        project_id: ProjectId,
        revision: &ItemRevision,
        job: &Job,
        existing: Option<Workspace>,
    ) -> Result<Workspace, ApiError> {
        let paths = self.mirror_paths(project_id).await?;
        ensure_authoring_workspace_state(
            existing,
            project_id,
            paths.mirror_git_dir.as_path(),
            paths.worktree_root.as_path(),
            revision,
            job,
            chrono::Utc::now(),
        )
        .await
        .map_err(workspace_to_api_error)
    }

    pub(super) async fn checkout_sync_status(
        &self,
        project: &Project,
        target_ref: &GitRef,
    ) -> Result<CheckoutSyncStatus, ApiError> {
        checkout_sync_status(&project.path, target_ref)
            .await
            .map_err(git_to_internal)
    }

    pub(super) async fn checkout_finalization_status(
        &self,
        project: &Project,
        target_ref: &GitRef,
        prepared_commit_oid: &CommitOid,
    ) -> Result<CheckoutFinalizationStatus, ApiError> {
        checkout_finalization_status(&project.path, target_ref, prepared_commit_oid)
            .await
            .map_err(git_to_internal)
    }

    pub(super) async fn sync_checkout_to_prepared_commit(
        &self,
        project: &Project,
        target_ref: &GitRef,
        prepared_commit_oid: &CommitOid,
    ) -> Result<(), ApiError> {
        let paths = self.mirror_paths(project.id).await?;
        sync_checkout_to_commit(
            &project.path,
            paths.mirror_git_dir.as_path(),
            target_ref,
            prepared_commit_oid,
        )
        .await
        .map_err(git_to_internal)
    }

    pub(super) async fn finalize_target_ref(
        &self,
        project_id: ProjectId,
        ref_name: &GitRef,
        prepared_commit_oid: &CommitOid,
        expected_old_oid: &CommitOid,
    ) -> Result<FinalizeTargetRefOutcome, ApiError> {
        let paths = self.mirror_paths(project_id).await?;
        ingot_git::commands::finalize_target_ref(
            paths.mirror_git_dir.as_path(),
            ref_name,
            prepared_commit_oid,
            expected_old_oid,
        )
        .await
        .map_err(git_to_internal)
    }

    pub(super) async fn remove_workspace_path(
        &self,
        project_id: ProjectId,
        workspace_path: &Path,
    ) -> Result<(), ApiError> {
        let paths = self.mirror_paths(project_id).await?;
        ingot_workspace::remove_workspace(paths.mirror_git_dir.as_path(), workspace_path)
            .await
            .map_err(workspace_to_api_error)
    }

    async fn remove_workspace_with_ref_cleanup(
        &self,
        project_id: ProjectId,
        workspace: &Workspace,
    ) -> Result<(), ApiError> {
        self.remove_workspace_path(project_id, &workspace.path)
            .await?;
        if let Some(workspace_ref) = workspace.workspace_ref.as_ref() {
            let _ = self.delete_project_ref(project_id, workspace_ref).await;
        }
        Ok(())
    }

    async fn update_project_ref(
        &self,
        project_id: ProjectId,
        ref_name: &GitRef,
        commit_oid: &CommitOid,
    ) -> Result<(), ApiError> {
        let paths = self.mirror_paths(project_id).await?;
        ingot_git::commands::update_ref(paths.mirror_git_dir.as_path(), ref_name, commit_oid)
            .await
            .map_err(git_to_internal)
    }

    async fn delete_project_ref(
        &self,
        project_id: ProjectId,
        ref_name: &GitRef,
    ) -> Result<(), ApiError> {
        let paths = self.mirror_paths(project_id).await?;
        ingot_git::commands::delete_ref(paths.mirror_git_dir.as_path(), ref_name)
            .await
            .map_err(git_to_internal)
    }
}

impl DispatchInfraPort for HttpInfraAdapter {
    async fn resolve_ref_oid(
        &self,
        project_id: ProjectId,
        ref_name: &GitRef,
    ) -> Result<Option<CommitOid>, UseCaseError> {
        self.resolve_project_ref_oid(project_id, ref_name)
            .await
            .map_err(api_to_uc)
    }

    async fn update_ref(
        &self,
        project_id: ProjectId,
        ref_name: &GitRef,
        commit_oid: &CommitOid,
    ) -> Result<(), UseCaseError> {
        self.update_project_ref(project_id, ref_name, commit_oid)
            .await
            .map_err(api_to_uc)
    }

    async fn delete_ref(
        &self,
        project_id: ProjectId,
        ref_name: &GitRef,
    ) -> Result<(), UseCaseError> {
        self.delete_project_ref(project_id, ref_name)
            .await
            .map_err(api_to_uc)
    }

    async fn remove_workspace_files(
        &self,
        project_id: ProjectId,
        workspace: &Workspace,
    ) -> Result<(), UseCaseError> {
        self.remove_workspace_with_ref_cleanup(project_id, workspace)
            .await
            .map_err(api_to_uc)
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
        let paths = self.mirror_paths(project_id).await.map_err(api_to_uc)?;
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
        self.remove_workspace_path(project_id, workspace_path)
            .await
            .map_err(api_to_uc)
    }

    async fn resolve_ref_oid(
        &self,
        project_id: ProjectId,
        ref_name: &GitRef,
    ) -> Result<Option<CommitOid>, UseCaseError> {
        self.resolve_project_ref_oid(project_id, ref_name)
            .await
            .map_err(api_to_uc)
    }

    async fn delete_ref(
        &self,
        project_id: ProjectId,
        ref_name: &GitRef,
    ) -> Result<(), UseCaseError> {
        self.delete_project_ref(project_id, ref_name)
            .await
            .map_err(api_to_uc)
    }
}
