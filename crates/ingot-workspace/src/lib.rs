use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use ingot_domain::commit_oid::CommitOid;
use ingot_domain::git_ref::GitRef;
use ingot_domain::ids::ProjectId;
use ingot_domain::job::Job;
use ingot_domain::revision::ItemRevision;
use ingot_domain::workspace::{
    RetentionPolicy, Workspace, WorkspaceCommitState, WorkspaceKind, WorkspaceState,
    WorkspaceStatus, WorkspaceStrategy,
};
use ingot_git::commands::{GitCommandError, current_head_ref, git, head_oid, resolve_ref_oid};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProvisionedAuthoringWorkspace {
    pub workspace_path: PathBuf,
    pub workspace_ref: GitRef,
    pub head_commit_oid: CommitOid,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProvisionedReviewWorkspace {
    pub workspace_path: PathBuf,
    pub head_commit_oid: CommitOid,
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
        expected: GitRef,
        actual: Option<GitRef>,
    },
    #[error("workspace head mismatch: expected {expected}, got {actual}")]
    WorkspaceHeadMismatch {
        expected: CommitOid,
        actual: CommitOid,
    },
}

pub async fn provision_authoring_workspace(
    repo_path: &Path,
    workspace_path: &Path,
    workspace_ref: &GitRef,
    expected_head_oid: &CommitOid,
) -> Result<ProvisionedAuthoringWorkspace, WorkspaceError> {
    if let Some(parent) = workspace_path.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }

    let current_ref = resolve_ref_oid(repo_path, workspace_ref).await?;
    if current_ref.as_ref() != Some(expected_head_oid) {
        git(
            repo_path,
            &[
                "update-ref",
                workspace_ref.as_str(),
                expected_head_oid.as_str(),
            ],
        )
        .await?;
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
                workspace_ref.as_str(),
            ],
        )
        .await?;
    } else {
        reset_existing_worktree(workspace_path, expected_head_oid).await?;
    }

    verify_authoring_workspace(repo_path, workspace_path, workspace_ref, expected_head_oid).await
}

pub async fn verify_authoring_workspace(
    repo_path: &Path,
    workspace_path: &Path,
    workspace_ref: &GitRef,
    expected_head_oid: &CommitOid,
) -> Result<ProvisionedAuthoringWorkspace, WorkspaceError> {
    let actual_ref = resolve_ref_oid(repo_path, workspace_ref).await?;
    if actual_ref.as_ref() != Some(expected_head_oid) {
        return Err(WorkspaceError::WorkspaceRefMismatch {
            expected: workspace_ref.clone(),
            actual: current_head_ref(workspace_path).await?,
        });
    }

    let actual_head = head_oid(workspace_path).await?;
    if actual_head != *expected_head_oid {
        return Err(WorkspaceError::WorkspaceHeadMismatch {
            expected: expected_head_oid.clone(),
            actual: actual_head,
        });
    }

    Ok(ProvisionedAuthoringWorkspace {
        workspace_path: workspace_path.to_path_buf(),
        workspace_ref: workspace_ref.clone(),
        head_commit_oid: expected_head_oid.clone(),
    })
}

pub async fn provision_review_workspace(
    repo_path: &Path,
    workspace_path: &Path,
    expected_head_oid: &CommitOid,
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
            expected_head_oid.as_str(),
        ],
    )
    .await?;

    verify_review_workspace(workspace_path, expected_head_oid).await
}

pub async fn provision_integration_workspace(
    repo_path: &Path,
    workspace_path: &Path,
    workspace_ref: &GitRef,
    expected_head_oid: &CommitOid,
) -> Result<ProvisionedAuthoringWorkspace, WorkspaceError> {
    provision_authoring_workspace(repo_path, workspace_path, workspace_ref, expected_head_oid).await
}

async fn reset_existing_worktree(
    workspace_path: &Path,
    expected_head_oid: &CommitOid,
) -> Result<(), WorkspaceError> {
    git(
        workspace_path,
        &[
            "checkout",
            "--detach",
            "--force",
            expected_head_oid.as_str(),
        ],
    )
    .await?;
    git(
        workspace_path,
        &["reset", "--hard", expected_head_oid.as_str()],
    )
    .await?;
    git(workspace_path, &["clean", "-fd"]).await?;
    Ok(())
}

pub async fn verify_review_workspace(
    workspace_path: &Path,
    expected_head_oid: &CommitOid,
) -> Result<ProvisionedReviewWorkspace, WorkspaceError> {
    let actual_head = head_oid(workspace_path).await?;
    if actual_head != *expected_head_oid {
        return Err(WorkspaceError::WorkspaceHeadMismatch {
            expected: expected_head_oid.clone(),
            actual: actual_head,
        });
    }

    Ok(ProvisionedReviewWorkspace {
        workspace_path: workspace_path.to_path_buf(),
        head_commit_oid: expected_head_oid.clone(),
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
    fn resolve_base_commit_oid(
        workspace: Option<&Workspace>,
        revision: &ItemRevision,
        job: &Job,
        expected_head_commit_oid: &CommitOid,
    ) -> CommitOid {
        workspace
            .and_then(|workspace| workspace.state.base_commit_oid().map(ToOwned::to_owned))
            .or_else(|| revision.seed.seed_commit_oid().map(ToOwned::to_owned))
            .or_else(|| job.job_input.head_commit_oid().map(ToOwned::to_owned))
            .unwrap_or_else(|| expected_head_commit_oid.clone())
    }

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
        .map(|workspace| workspace.path.clone())
        .unwrap_or_else(|| workspace_root.join(workspace_id.to_string()));
    let workspace_ref = existing
        .as_ref()
        .and_then(|workspace| workspace.workspace_ref.clone())
        .unwrap_or_else(|| GitRef::new(format!("refs/ingot/workspaces/{workspace_id}")));

    if let Some(mut workspace) = existing {
        if workspace.state.status() == WorkspaceStatus::Busy {
            return Err(WorkspaceError::Busy);
        }

        let provisioned = provision_authoring_workspace(
            repo_path,
            &workspace_path,
            &workspace_ref,
            &expected_head_commit_oid,
        )
        .await?;
        workspace.path = provisioned.workspace_path.clone();
        workspace.target_ref = Some(revision.target_ref.clone());
        workspace.workspace_ref = Some(provisioned.workspace_ref);
        workspace.mark_ready(
            WorkspaceCommitState::new(
                resolve_base_commit_oid(Some(&workspace), revision, job, &expected_head_commit_oid),
                provisioned.head_commit_oid,
            ),
            now,
        );
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
            path: provisioned.workspace_path.clone(),
            created_for_revision_id: Some(revision.id),
            parent_workspace_id: None,
            target_ref: Some(revision.target_ref.clone()),
            workspace_ref: Some(provisioned.workspace_ref),
            retention_policy: RetentionPolicy::Persistent,
            state: WorkspaceState::Ready {
                commits: WorkspaceCommitState::new(
                    resolve_base_commit_oid(None, revision, job, &expected_head_commit_oid),
                    provisioned.head_commit_oid,
                ),
            },
            created_at: now,
            updated_at: now,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::{
        provision_authoring_workspace, provision_integration_workspace, verify_authoring_workspace,
    };
    use ingot_domain::commit_oid::CommitOid;
    use ingot_domain::git_ref::GitRef;
    use ingot_git::commands::head_oid;
    use ingot_test_support::git::{
        git_output, run_git as git_sync, temp_git_repo, unique_temp_path,
    };

    #[tokio::test]
    async fn provision_authoring_workspace_creates_worktree_and_anchor_ref() {
        let repo = temp_git_repo("ingot-workspace");
        let expected_head = CommitOid::new(git_output(&repo, &["rev-parse", "HEAD"]));
        let workspace_path = unique_temp_path("ingot-workspace");

        let provisioned = provision_authoring_workspace(
            &repo,
            &workspace_path,
            &GitRef::new("refs/ingot/workspaces/wrk_test"),
            &expected_head,
        )
        .await
        .expect("provision workspace");

        assert_eq!(provisioned.head_commit_oid, expected_head);
        assert!(workspace_path.exists(), "workspace path should exist");

        verify_authoring_workspace(
            &repo,
            &workspace_path,
            &GitRef::new("refs/ingot/workspaces/wrk_test"),
            &provisioned.head_commit_oid,
        )
        .await
        .expect("verify provisioned workspace");
    }

    #[tokio::test]
    async fn provision_authoring_workspace_reuses_existing_worktree() {
        let repo = temp_git_repo("ingot-workspace");
        let expected_head = CommitOid::new(git_output(&repo, &["rev-parse", "HEAD"]));
        let workspace_path = unique_temp_path("ingot-workspace");

        provision_authoring_workspace(
            &repo,
            &workspace_path,
            &GitRef::new("refs/ingot/workspaces/wrk_test"),
            &expected_head,
        )
        .await
        .expect("first provision");

        provision_authoring_workspace(
            &repo,
            &workspace_path,
            &GitRef::new("refs/ingot/workspaces/wrk_test"),
            &expected_head,
        )
        .await
        .expect("second provision");

        verify_authoring_workspace(
            &repo,
            &workspace_path,
            &GitRef::new("refs/ingot/workspaces/wrk_test"),
            &expected_head,
        )
        .await
        .expect("workspace should still verify after reprovision");
    }

    #[tokio::test]
    async fn provision_authoring_workspace_resets_existing_worktree_to_expected_head() {
        let repo = temp_git_repo("ingot-workspace");
        let base_head = CommitOid::new(git_output(&repo, &["rev-parse", "HEAD"]));
        let workspace_path = unique_temp_path("ingot-workspace");
        let workspace_ref = "refs/ingot/workspaces/wrk_test";

        provision_authoring_workspace(
            &repo,
            &workspace_path,
            &GitRef::new(workspace_ref),
            &base_head,
        )
        .await
        .expect("first provision");

        std::fs::write(repo.join("tracked.txt"), "next").expect("write tracked");
        git_sync(&repo, &["add", "tracked.txt"]);
        git_sync(&repo, &["commit", "-m", "next"]);
        let next_head = head_oid(&repo).await.expect("next head").into_inner();

        git_sync(&workspace_path, &["checkout", &next_head]);

        provision_authoring_workspace(
            &repo,
            &workspace_path,
            &GitRef::new(workspace_ref),
            &base_head,
        )
        .await
        .expect("re-provision drifted workspace");

        assert_eq!(
            head_oid(&workspace_path)
                .await
                .expect("workspace head")
                .into_inner(),
            base_head.as_str()
        );
    }

    #[tokio::test]
    async fn provision_authoring_workspace_detaches_before_resetting_branch_attached_worktree() {
        let repo = temp_git_repo("ingot-workspace");
        let base_head = CommitOid::new(git_output(&repo, &["rev-parse", "HEAD"]));
        let workspace_path = unique_temp_path("ingot-workspace");
        let workspace_ref = "refs/ingot/workspaces/wrk_test";

        provision_authoring_workspace(
            &repo,
            &workspace_path,
            &GitRef::new(workspace_ref),
            &base_head,
        )
        .await
        .expect("first provision");

        std::fs::write(repo.join("tracked.txt"), "next").expect("write tracked");
        git_sync(&repo, &["add", "tracked.txt"]);
        git_sync(&repo, &["commit", "-m", "next"]);
        let next_head = head_oid(&repo).await.expect("next head").into_inner();

        git_sync(&repo, &["branch", "feature/drift", &next_head]);
        git_sync(&workspace_path, &["checkout", "feature/drift"]);

        provision_authoring_workspace(
            &repo,
            &workspace_path,
            &GitRef::new(workspace_ref),
            &base_head,
        )
        .await
        .expect("re-provision drifted workspace");

        assert_eq!(
            head_oid(&workspace_path)
                .await
                .expect("workspace head")
                .into_inner(),
            base_head.as_str()
        );
        assert_eq!(
            git_output(&repo, &["rev-parse", "refs/heads/feature/drift"]),
            next_head
        );
    }

    #[tokio::test]
    async fn provision_integration_workspace_resets_existing_worktree_to_expected_head() {
        let repo = temp_git_repo("ingot-workspace");
        let base_head = CommitOid::new(git_output(&repo, &["rev-parse", "HEAD"]));
        let workspace_path = unique_temp_path("ingot-workspace");
        let workspace_ref = "refs/ingot/workspaces/wrk_integration";

        provision_integration_workspace(
            &repo,
            &workspace_path,
            &GitRef::new(workspace_ref),
            &base_head,
        )
        .await
        .expect("first provision");

        std::fs::write(repo.join("tracked.txt"), "next").expect("write tracked");
        git_sync(&repo, &["add", "tracked.txt"]);
        git_sync(&repo, &["commit", "-m", "next"]);
        let next_head = head_oid(&repo).await.expect("next head").into_inner();

        git_sync(&workspace_path, &["checkout", &next_head]);

        provision_integration_workspace(
            &repo,
            &workspace_path,
            &GitRef::new(workspace_ref),
            &base_head,
        )
        .await
        .expect("re-provision drifted workspace");

        assert_eq!(
            head_oid(&workspace_path)
                .await
                .expect("workspace head")
                .into_inner(),
            base_head.as_str()
        );
    }
}
