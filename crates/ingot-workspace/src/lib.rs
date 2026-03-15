use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use ingot_domain::ids::ProjectId;
use ingot_domain::job::Job;
use ingot_domain::revision::ItemRevision;
use ingot_domain::workspace::{
    RetentionPolicy, Workspace, WorkspaceKind, WorkspaceStatus, WorkspaceStrategy,
};
use ingot_git::commands::{GitCommandError, git, head_oid, resolve_ref_oid};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProvisionedAuthoringWorkspace {
    pub workspace_path: PathBuf,
    pub workspace_ref: String,
    pub head_commit_oid: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProvisionedReviewWorkspace {
    pub workspace_path: PathBuf,
    pub head_commit_oid: String,
}

pub fn workspace_root_path(repo_path: &Path) -> PathBuf {
    let repo_path = repo_path.to_path_buf();
    let parent = repo_path.parent().unwrap_or(repo_path.as_path());
    parent.join(".ingot-workspaces")
}

pub fn managed_workspace_root_path(state_root: &Path, project_id: ProjectId) -> PathBuf {
    state_root.join("worktrees").join(project_id.to_string())
}

#[derive(Debug, thiserror::Error)]
pub enum WorkspaceError {
    #[error("git error: {0}")]
    Git(#[from] GitCommandError),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("authoring jobs require a job_input head")]
    MissingInputHeadCommitOid,
    #[error("authoring workspace is already busy")]
    Busy,
    #[error("workspace ref mismatch: expected {expected}, got {actual:?}")]
    WorkspaceRefMismatch {
        expected: String,
        actual: Option<String>,
    },
    #[error("workspace head mismatch: expected {expected}, got {actual}")]
    WorkspaceHeadMismatch { expected: String, actual: String },
}

pub async fn provision_authoring_workspace(
    repo_path: &Path,
    workspace_path: &Path,
    workspace_ref: &str,
    expected_head_oid: &str,
) -> Result<ProvisionedAuthoringWorkspace, WorkspaceError> {
    if let Some(parent) = workspace_path.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }

    let current_ref = resolve_ref_oid(repo_path, workspace_ref).await?;
    if current_ref.as_deref() != Some(expected_head_oid) {
        git(repo_path, &["update-ref", workspace_ref, expected_head_oid]).await?;
    }

    if !workspace_path.exists() {
        let workspace_path = workspace_path.to_string_lossy().into_owned();
        git(
            repo_path,
            &[
                "worktree",
                "add",
                "--detach",
                &workspace_path,
                workspace_ref,
            ],
        )
        .await?;
    }

    verify_authoring_workspace(repo_path, workspace_path, workspace_ref, expected_head_oid).await
}

pub async fn verify_authoring_workspace(
    repo_path: &Path,
    workspace_path: &Path,
    workspace_ref: &str,
    expected_head_oid: &str,
) -> Result<ProvisionedAuthoringWorkspace, WorkspaceError> {
    let actual_ref = resolve_ref_oid(repo_path, workspace_ref).await?;
    if actual_ref.as_deref() != Some(expected_head_oid) {
        return Err(WorkspaceError::WorkspaceRefMismatch {
            expected: expected_head_oid.to_string(),
            actual: actual_ref,
        });
    }

    let actual_head = head_oid(workspace_path).await?;
    if actual_head != expected_head_oid {
        return Err(WorkspaceError::WorkspaceHeadMismatch {
            expected: expected_head_oid.to_string(),
            actual: actual_head,
        });
    }

    Ok(ProvisionedAuthoringWorkspace {
        workspace_path: workspace_path.to_path_buf(),
        workspace_ref: workspace_ref.to_string(),
        head_commit_oid: expected_head_oid.to_string(),
    })
}

pub async fn provision_review_workspace(
    repo_path: &Path,
    workspace_path: &Path,
    expected_head_oid: &str,
) -> Result<ProvisionedReviewWorkspace, WorkspaceError> {
    if let Some(parent) = workspace_path.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }

    if workspace_path.exists() {
        let workspace_path = workspace_path.to_string_lossy().into_owned();
        git(
            repo_path,
            &["worktree", "remove", "--force", &workspace_path],
        )
        .await?;
    }

    let workspace_path_arg = workspace_path.to_string_lossy().into_owned();
    git(
        repo_path,
        &[
            "worktree",
            "add",
            "--detach",
            &workspace_path_arg,
            expected_head_oid,
        ],
    )
    .await?;

    verify_review_workspace(workspace_path, expected_head_oid).await
}

pub async fn provision_integration_workspace(
    repo_path: &Path,
    workspace_path: &Path,
    workspace_ref: &str,
    expected_head_oid: &str,
) -> Result<ProvisionedAuthoringWorkspace, WorkspaceError> {
    provision_authoring_workspace(repo_path, workspace_path, workspace_ref, expected_head_oid).await
}

pub async fn verify_review_workspace(
    workspace_path: &Path,
    expected_head_oid: &str,
) -> Result<ProvisionedReviewWorkspace, WorkspaceError> {
    let actual_head = head_oid(workspace_path).await?;
    if actual_head != expected_head_oid {
        return Err(WorkspaceError::WorkspaceHeadMismatch {
            expected: expected_head_oid.to_string(),
            actual: actual_head,
        });
    }

    Ok(ProvisionedReviewWorkspace {
        workspace_path: workspace_path.to_path_buf(),
        head_commit_oid: expected_head_oid.to_string(),
    })
}

pub async fn remove_workspace(
    repo_path: &Path,
    workspace_path: &Path,
) -> Result<(), WorkspaceError> {
    if !workspace_path.exists() {
        return Ok(());
    }

    let workspace_path = workspace_path.to_string_lossy().into_owned();
    git(
        repo_path,
        &["worktree", "remove", "--force", &workspace_path],
    )
    .await?;
    Ok(())
}

pub async fn ensure_authoring_workspace_state(
    existing: Option<Workspace>,
    project_id: ingot_domain::ids::ProjectId,
    repo_path: &Path,
    workspace_root: &Path,
    revision: &ItemRevision,
    job: &Job,
    now: DateTime<Utc>,
) -> Result<Workspace, WorkspaceError> {
    let expected_head_commit_oid = job
        .job_input
        .head_commit_oid()
        .map(ToOwned::to_owned)
        .ok_or(WorkspaceError::MissingInputHeadCommitOid)?;
    let workspace_id = existing
        .as_ref()
        .map(|workspace| workspace.id)
        .unwrap_or_default();
    let workspace_path = existing
        .as_ref()
        .map(|workspace| PathBuf::from(&workspace.path))
        .unwrap_or_else(|| workspace_root.join(workspace_id.to_string()));
    let workspace_ref = existing
        .as_ref()
        .and_then(|workspace| workspace.workspace_ref.clone())
        .unwrap_or_else(|| format!("refs/ingot/workspaces/{workspace_id}"));

    if let Some(mut workspace) = existing {
        if workspace.status == WorkspaceStatus::Busy {
            return Err(WorkspaceError::Busy);
        }

        let provisioned = provision_authoring_workspace(
            repo_path,
            &workspace_path,
            &workspace_ref,
            &expected_head_commit_oid,
        )
        .await?;
        workspace.path = provisioned.workspace_path.display().to_string();
        workspace.target_ref = Some(revision.target_ref.clone());
        workspace.workspace_ref = Some(provisioned.workspace_ref);
        workspace.base_commit_oid = workspace
            .base_commit_oid
            .clone()
            .or_else(|| revision.seed_commit_oid.clone())
            .or_else(|| job.job_input.head_commit_oid().map(ToOwned::to_owned));
        workspace.head_commit_oid = Some(provisioned.head_commit_oid);
        workspace.status = WorkspaceStatus::Ready;
        workspace.current_job_id = None;
        workspace.updated_at = now;
        Ok(workspace)
    } else {
        let provisioned = provision_authoring_workspace(
            repo_path,
            &workspace_path,
            &workspace_ref,
            &expected_head_commit_oid,
        )
        .await?;

        Ok(Workspace {
            id: workspace_id,
            project_id,
            kind: WorkspaceKind::Authoring,
            strategy: WorkspaceStrategy::Worktree,
            path: provisioned.workspace_path.display().to_string(),
            created_for_revision_id: Some(revision.id),
            parent_workspace_id: None,
            target_ref: Some(revision.target_ref.clone()),
            workspace_ref: Some(provisioned.workspace_ref),
            base_commit_oid: revision
                .seed_commit_oid
                .clone()
                .or_else(|| job.job_input.head_commit_oid().map(ToOwned::to_owned)),
            head_commit_oid: Some(provisioned.head_commit_oid),
            retention_policy: RetentionPolicy::Persistent,
            status: WorkspaceStatus::Ready,
            current_job_id: None,
            created_at: now,
            updated_at: now,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::{provision_authoring_workspace, verify_authoring_workspace};
    use ingot_test_support::git::{git_output, temp_git_repo, unique_temp_path};

    #[tokio::test]
    async fn provision_authoring_workspace_creates_worktree_and_anchor_ref() {
        let repo = temp_git_repo("ingot-workspace");
        let expected_head = git_output(&repo, &["rev-parse", "HEAD"]);
        let workspace_path =
            unique_temp_path("ingot-workspace");

        let provisioned = provision_authoring_workspace(
            &repo,
            &workspace_path,
            "refs/ingot/workspaces/wrk_test",
            &expected_head,
        )
        .await
        .expect("provision workspace");

        assert_eq!(provisioned.head_commit_oid, expected_head);
        assert!(workspace_path.exists(), "workspace path should exist");

        verify_authoring_workspace(
            &repo,
            &workspace_path,
            "refs/ingot/workspaces/wrk_test",
            &provisioned.head_commit_oid,
        )
        .await
        .expect("verify provisioned workspace");
    }

    #[tokio::test]
    async fn provision_authoring_workspace_reuses_existing_worktree() {
        let repo = temp_git_repo("ingot-workspace");
        let expected_head = git_output(&repo, &["rev-parse", "HEAD"]);
        let workspace_path =
            unique_temp_path("ingot-workspace");

        provision_authoring_workspace(
            &repo,
            &workspace_path,
            "refs/ingot/workspaces/wrk_test",
            &expected_head,
        )
        .await
        .expect("first provision");

        provision_authoring_workspace(
            &repo,
            &workspace_path,
            "refs/ingot/workspaces/wrk_test",
            &expected_head,
        )
        .await
        .expect("second provision");

        verify_authoring_workspace(
            &repo,
            &workspace_path,
            "refs/ingot/workspaces/wrk_test",
            &expected_head,
        )
        .await
        .expect("workspace should still verify after reprovision");
    }
}
