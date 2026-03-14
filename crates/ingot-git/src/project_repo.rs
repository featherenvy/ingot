use std::path::{Path, PathBuf};

use ingot_domain::ids::ProjectId;
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
        git(&paths.mirror_git_dir, &["symbolic-ref", "HEAD", &head_ref]).await?;
    }
    Ok(())
}

pub async fn checkout_sync_status(
    checkout_path: &Path,
    target_ref: &str,
) -> Result<CheckoutSyncStatus, GitCommandError> {
    let current_head_ref = current_head_ref(checkout_path).await?;
    if current_head_ref.as_deref() != Some(target_ref) {
        let current = current_head_ref.unwrap_or_else(|| "detached".into());
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
    target_ref: &str,
    commit_oid: &str,
) -> Result<CheckoutFinalizationStatus, GitCommandError> {
    match checkout_sync_status(checkout_path, target_ref).await? {
        CheckoutSyncStatus::Ready => {
            if head_oid(checkout_path).await? == commit_oid {
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
    target_ref: &str,
    commit_oid: &str,
) -> Result<(), GitCommandError> {
    let sync_ref = format!(
        "refs/ingot/sync-targets/{}",
        target_ref
            .trim_start_matches("refs/")
            .replace('/', "_")
            .replace(':', "_")
    );
    let fetch_spec = format!("{target_ref}:{sync_ref}");
    let mirror_path = mirror_git_dir.to_string_lossy().into_owned();
    git(
        checkout_path,
        &["fetch", "--no-tags", mirror_path.as_str(), fetch_spec.as_str()],
    )
    .await?;
    let fetched_oid = resolve_ref_oid(checkout_path, &sync_ref).await?;
    if fetched_oid.as_deref() != Some(commit_oid) {
        let _ = crate::commands::delete_ref(checkout_path, &sync_ref).await;
        return Err(GitCommandError::CommandFailed(format!(
            "fetched {target_ref} as {}, expected {commit_oid}",
            fetched_oid.unwrap_or_else(|| "missing".into())
        )));
    }
    let reset_result = git(checkout_path, &["reset", "--hard", commit_oid]).await;
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
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::process::Command;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};

    use super::{ensure_mirror, project_repo_paths, sync_checkout_to_commit};
    use crate::commands::{current_head_ref, resolve_ref_oid};
    use ingot_domain::ids::ProjectId;

    fn unique_temp_path(prefix: &str) -> PathBuf {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system time before unix epoch")
            .as_nanos();
        let suffix = COUNTER.fetch_add(1, Ordering::SeqCst);
        std::env::temp_dir().join(format!("{prefix}-{nanos}-{suffix}"))
    }

    fn git_sync(repo_path: &Path, args: &[&str]) {
        let output = Command::new("git")
            .args(args)
            .current_dir(repo_path)
            .output()
            .expect("run git");
        assert!(
            output.status.success(),
            "git {:?} failed\nstdout:\n{}\nstderr:\n{}",
            args,
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    fn git_output(repo_path: &Path, args: &[&str]) -> String {
        let output = Command::new("git")
            .args(args)
            .current_dir(repo_path)
            .output()
            .expect("run git");
        assert!(output.status.success(), "git {:?} failed", args);
        String::from_utf8_lossy(&output.stdout).trim().to_string()
    }

    fn temp_git_repo() -> PathBuf {
        let repo_path = unique_temp_path("ingot-git-project-repo");
        fs::create_dir_all(&repo_path).expect("create repo dir");
        git_sync(&repo_path, &["init"]);
        git_sync(&repo_path, &["symbolic-ref", "HEAD", "refs/heads/main"]);
        git_sync(&repo_path, &["config", "user.email", "ingot@example.com"]);
        git_sync(&repo_path, &["config", "user.name", "Ingot Tests"]);
        fs::write(repo_path.join("tracked.txt"), "base\n").expect("write tracked file");
        git_sync(&repo_path, &["add", "tracked.txt"]);
        git_sync(&repo_path, &["commit", "-m", "initial"]);
        repo_path
    }

    #[tokio::test]
    async fn ensure_mirror_preserves_daemon_refs_while_pruning_checkout_refs() {
        let checkout_path = temp_git_repo();
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
            resolve_ref_oid(&paths.mirror_git_dir, "refs/ingot/workspaces/wrk_test")
                .await
                .expect("resolve daemon ref"),
            Some(initial_head)
        );
        assert_eq!(
            resolve_ref_oid(&paths.mirror_git_dir, "refs/heads/main")
                .await
                .expect("resolve main"),
            Some(updated_head.clone())
        );
        assert_eq!(
            resolve_ref_oid(&paths.mirror_git_dir, "refs/heads/fresh-branch")
                .await
                .expect("resolve fresh branch"),
            Some(updated_head.clone())
        );
        assert_eq!(
            resolve_ref_oid(&paths.mirror_git_dir, "refs/heads/stale-branch")
                .await
                .expect("resolve stale branch"),
            None
        );
        assert_eq!(
            resolve_ref_oid(&paths.mirror_git_dir, "refs/tags/stale-tag")
                .await
                .expect("resolve stale tag"),
            None
        );
        assert_eq!(
            resolve_ref_oid(&paths.mirror_git_dir, "refs/tags/fresh-tag")
                .await
                .expect("resolve fresh tag"),
            Some(updated_head)
        );
        assert_eq!(
            current_head_ref(&paths.mirror_git_dir)
                .await
                .expect("resolve mirror head ref"),
            Some("refs/heads/main".into())
        );
    }

    #[tokio::test]
    async fn sync_checkout_to_commit_cleans_temporary_ref_when_sync_fails() {
        let checkout_path = temp_git_repo();
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
            "refs/heads/main",
            &initial_head,
        )
        .await
        .expect_err("sync should fail on fetched oid mismatch");
        assert!(
            error
                .to_string()
                .contains("expected"),
            "error should describe fetched oid mismatch"
        );
        assert_eq!(
            resolve_ref_oid(&checkout_path, "refs/ingot/sync-targets/heads_main")
                .await
                .expect("resolve scratch ref"),
            None,
            "temporary sync ref should be deleted after a failed sync"
        );
    }
}
