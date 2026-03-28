use std::path::{Path, PathBuf};

use ingot_domain::commit_oid::CommitOid;
use ingot_domain::git_operation::OperationKind;
use ingot_domain::git_ref::GitRef;
use ingot_domain::ids::ProjectId;
use ingot_domain::ports::{GitOperationRepository, RepositoryError};
use tokio::process::Command;

use crate::commands::{GitCommandError, current_head_ref, git, head_oid, resolve_ref_oid};
use crate::commit::working_tree_has_changes;

#[derive(Debug, Clone)]
pub struct ProjectRepoPaths {
    pub checkout_path: PathBuf,
    pub mirror_git_dir: PathBuf,
    pub worktree_root: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CheckoutSyncStatus {
    Ready,
    Blocked { code: &'static str, message: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CheckoutFinalizationStatus {
    Synced,
    NeedsSync,
    Blocked { code: &'static str, message: String },
}

pub fn project_repo_paths(
    state_root: &Path,
    project_id: ProjectId,
    checkout_path: &Path,
) -> ProjectRepoPaths {
    ProjectRepoPaths {
        checkout_path: checkout_path.to_path_buf(),
        mirror_git_dir: state_root.join("repos").join(format!("{project_id}.git")),
        worktree_root: state_root.join("worktrees").join(project_id.to_string()),
    }
}

pub async fn ensure_mirror(paths: &ProjectRepoPaths) -> Result<(), GitCommandError> {
    if let Some(parent) = paths.mirror_git_dir.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }
    if let Some(parent) = paths.worktree_root.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }

    if !paths.mirror_git_dir.exists() {
        let mirror_parent = paths
            .mirror_git_dir
            .parent()
            .unwrap_or(paths.mirror_git_dir.as_path());
        let checkout = paths.checkout_path.to_string_lossy().into_owned();
        let mirror_name = paths
            .mirror_git_dir
            .file_name()
            .and_then(|value| value.to_str())
            .ok_or_else(|| GitCommandError::CommandFailed("invalid mirror path".into()))?
            .to_string();
        git_clone_mirror(mirror_parent, &checkout, &mirror_name).await?;
        return Ok(());
    }

    git(
        &paths.mirror_git_dir,
        &[
            "remote",
            "set-url",
            "origin",
            paths.checkout_path.to_string_lossy().as_ref(),
        ],
    )
    .await?;
    git(
        &paths.mirror_git_dir,
        &["fetch", "--prune", "origin", "+refs/heads/*:refs/heads/*"],
    )
    .await?;
    git(
        &paths.mirror_git_dir,
        &["fetch", "--prune", "origin", "+refs/tags/*:refs/tags/*"],
    )
    .await?;
    if let Some(head_ref) = current_head_ref(&paths.checkout_path).await?
        && resolve_ref_oid(&paths.mirror_git_dir, &head_ref)
            .await?
            .is_some()
    {
        git(
            &paths.mirror_git_dir,
            &["symbolic-ref", "HEAD", head_ref.as_str()],
        )
        .await?;
    }
    Ok(())
}

#[derive(Debug, thiserror::Error)]
pub enum RefreshMirrorError {
    #[error(transparent)]
    Repository(RepositoryError),
    #[error(transparent)]
    Git(GitCommandError),
}

/// Refresh the project mirror, skipping the fetch when an unresolved
/// finalize-target-ref operation is in flight for this project and the
/// mirror already exists on disk.
pub async fn refresh_project_mirror(
    git_ops: &impl GitOperationRepository,
    state_root: &Path,
    project_id: ProjectId,
    checkout_path: &Path,
) -> Result<ProjectRepoPaths, RefreshMirrorError> {
    let paths = project_repo_paths(state_root, project_id, checkout_path);
    let has_unresolved_finalize = git_ops
        .find_unresolved()
        .await
        .map_err(RefreshMirrorError::Repository)?
        .into_iter()
        .any(|op| {
            op.project_id == project_id && op.operation_kind() == OperationKind::FinalizeTargetRef
        });
    if !(has_unresolved_finalize && paths.mirror_git_dir.exists()) {
        ensure_mirror(&paths)
            .await
            .map_err(RefreshMirrorError::Git)?;
    }
    Ok(paths)
}

pub async fn checkout_sync_status(
    checkout_path: &Path,
    target_ref: &GitRef,
) -> Result<CheckoutSyncStatus, GitCommandError> {
    let current_head_ref = current_head_ref(checkout_path).await?;
    if current_head_ref.as_ref() != Some(target_ref) {
        let current = current_head_ref
            .map(|ref_name| ref_name.to_string())
            .unwrap_or_else(|| "detached".into());
        return Ok(CheckoutSyncStatus::Blocked {
            code: "checkout_wrong_branch",
            message: format!(
                "Registered checkout is on {current}; switch it to {target_ref} before finalizing"
            ),
        });
    }

    if working_tree_has_changes(checkout_path).await? {
        return Ok(CheckoutSyncStatus::Blocked {
            code: "checkout_dirty",
            message: "Registered checkout has uncommitted changes; clean it before finalizing"
                .into(),
        });
    }

    Ok(CheckoutSyncStatus::Ready)
}

pub async fn checkout_finalization_status(
    checkout_path: &Path,
    target_ref: &GitRef,
    commit_oid: &CommitOid,
) -> Result<CheckoutFinalizationStatus, GitCommandError> {
    match checkout_sync_status(checkout_path, target_ref).await? {
        CheckoutSyncStatus::Ready => {
            if head_oid(checkout_path).await? == *commit_oid {
                Ok(CheckoutFinalizationStatus::Synced)
            } else {
                Ok(CheckoutFinalizationStatus::NeedsSync)
            }
        }
        CheckoutSyncStatus::Blocked { code, message } => {
            Ok(CheckoutFinalizationStatus::Blocked { code, message })
        }
    }
}

pub async fn sync_checkout_to_commit(
    checkout_path: &Path,
    mirror_git_dir: &Path,
    target_ref: &GitRef,
    commit_oid: &CommitOid,
) -> Result<(), GitCommandError> {
    let sync_ref = format!(
        "refs/ingot/sync-targets/{}",
        target_ref
            .as_str()
            .trim_start_matches("refs/")
            .replace(['/', ':'], "_")
    );
    let fetch_spec = format!("{}:{sync_ref}", target_ref.as_str());
    let mirror_path = mirror_git_dir.to_string_lossy().into_owned();
    git(
        checkout_path,
        &[
            "fetch",
            "--no-tags",
            mirror_path.as_str(),
            fetch_spec.as_str(),
        ],
    )
    .await?;
    let sync_ref = GitRef::new(sync_ref);
    let fetched_oid = resolve_ref_oid(checkout_path, &sync_ref).await?;
    if !fetched_oid.as_ref().is_some_and(|oid| oid == commit_oid) {
        let _ = crate::commands::delete_ref(checkout_path, &sync_ref).await;
        return Err(GitCommandError::CommandFailed(format!(
            "fetched {target_ref} as {}, expected {commit_oid}",
            fetched_oid
                .map(|oid| oid.into_inner())
                .unwrap_or_else(|| "missing".into())
        )));
    }
    let reset_result = git(checkout_path, &["reset", "--hard", commit_oid.as_str()]).await;
    let _ = crate::commands::delete_ref(checkout_path, &sync_ref).await;
    reset_result?;
    Ok(())
}

async fn git_clone_mirror(
    workdir: &Path,
    checkout_path: &str,
    mirror_name: &str,
) -> Result<(), GitCommandError> {
    let output = Command::new("git")
        .args(["clone", "--mirror", checkout_path, mirror_name])
        .current_dir(workdir)
        .output()
        .await?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(GitCommandError::CommandFailed(stderr.to_string()));
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{ensure_mirror, project_repo_paths, sync_checkout_to_commit};
    use crate::commands::{current_head_ref, resolve_ref_oid};
    use ingot_domain::commit_oid::CommitOid;
    use ingot_domain::git_ref::GitRef;
    use ingot_domain::ids::ProjectId;
    use ingot_test_support::git::{
        git_output, run_git as git_sync, temp_git_repo, unique_temp_path,
    };
    use std::fs;

    #[tokio::test]
    async fn ensure_mirror_preserves_daemon_refs_while_pruning_checkout_refs() {
        let checkout_path = temp_git_repo("ingot-git-project-repo");
        let initial_head = git_output(&checkout_path, &["rev-parse", "HEAD"]);
        git_sync(&checkout_path, &["branch", "stale-branch"]);
        git_sync(&checkout_path, &["tag", "stale-tag"]);

        let state_root = unique_temp_path("ingot-git-project-repo-state");
        let paths = project_repo_paths(state_root.as_path(), ProjectId::new(), &checkout_path);
        ensure_mirror(&paths).await.expect("initial ensure mirror");

        crate::commands::git(
            &paths.mirror_git_dir,
            &[
                "update-ref",
                "refs/ingot/workspaces/wrk_test",
                &initial_head,
            ],
        )
        .await
        .expect("create daemon ref");

        fs::write(checkout_path.join("tracked.txt"), "updated\n").expect("write tracked file");
        git_sync(&checkout_path, &["add", "tracked.txt"]);
        git_sync(&checkout_path, &["commit", "-m", "update main"]);
        let updated_head = git_output(&checkout_path, &["rev-parse", "HEAD"]);
        git_sync(&checkout_path, &["branch", "fresh-branch"]);
        git_sync(&checkout_path, &["tag", "fresh-tag"]);
        git_sync(&checkout_path, &["branch", "-D", "stale-branch"]);
        git_sync(&checkout_path, &["tag", "-d", "stale-tag"]);

        ensure_mirror(&paths).await.expect("refresh mirror");

        assert_eq!(
            resolve_ref_oid(
                &paths.mirror_git_dir,
                &GitRef::new("refs/ingot/workspaces/wrk_test")
            )
            .await
            .expect("resolve daemon ref"),
            Some(CommitOid::from(initial_head))
        );
        assert_eq!(
            resolve_ref_oid(&paths.mirror_git_dir, &GitRef::new("refs/heads/main"))
                .await
                .expect("resolve main"),
            Some(CommitOid::from(updated_head.clone()))
        );
        assert_eq!(
            resolve_ref_oid(
                &paths.mirror_git_dir,
                &GitRef::new("refs/heads/fresh-branch")
            )
            .await
            .expect("resolve fresh branch"),
            Some(CommitOid::from(updated_head.clone()))
        );
        assert_eq!(
            resolve_ref_oid(
                &paths.mirror_git_dir,
                &GitRef::new("refs/heads/stale-branch")
            )
            .await
            .expect("resolve stale branch"),
            None
        );
        assert_eq!(
            resolve_ref_oid(&paths.mirror_git_dir, &GitRef::new("refs/tags/stale-tag"))
                .await
                .expect("resolve stale tag"),
            None
        );
        assert_eq!(
            resolve_ref_oid(&paths.mirror_git_dir, &GitRef::new("refs/tags/fresh-tag"))
                .await
                .expect("resolve fresh tag"),
            Some(CommitOid::from(updated_head))
        );
        assert_eq!(
            current_head_ref(&paths.mirror_git_dir)
                .await
                .expect("resolve mirror head ref"),
            Some(GitRef::new("refs/heads/main"))
        );
    }

    #[tokio::test]
    async fn sync_checkout_to_commit_cleans_temporary_ref_when_sync_fails() {
        let checkout_path = temp_git_repo("ingot-git-project-repo");
        let initial_head = git_output(&checkout_path, &["rev-parse", "HEAD"]);

        let state_root = unique_temp_path("ingot-git-project-repo-state");
        let paths = project_repo_paths(state_root.as_path(), ProjectId::new(), &checkout_path);
        ensure_mirror(&paths).await.expect("initial ensure mirror");

        fs::write(checkout_path.join("tracked.txt"), "updated\n").expect("write tracked file");
        git_sync(&checkout_path, &["add", "tracked.txt"]);
        git_sync(&checkout_path, &["commit", "-m", "update main"]);
        ensure_mirror(&paths).await.expect("refresh mirror");

        let error = sync_checkout_to_commit(
            &checkout_path,
            &paths.mirror_git_dir,
            &GitRef::new("refs/heads/main"),
            &CommitOid::new(initial_head),
        )
        .await
        .expect_err("sync should fail on fetched oid mismatch");
        assert!(
            error.to_string().contains("expected"),
            "error should describe fetched oid mismatch"
        );
        assert_eq!(
            resolve_ref_oid(
                &checkout_path,
                &GitRef::new("refs/ingot/sync-targets/heads_main")
            )
            .await
            .expect("resolve scratch ref"),
            None,
            "temporary sync ref should be deleted after a failed sync"
        );
    }
}
