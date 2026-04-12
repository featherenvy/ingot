use std::collections::{BTreeSet, HashSet};
use std::path::{Path, PathBuf};

use ingot_domain::commit_oid::CommitOid;
use ingot_domain::git_operation::OperationKind;
use ingot_domain::git_ref::GitRef;
use ingot_domain::ids::ProjectId;
use ingot_domain::ports::{GitOperationRepository, RepositoryError};
use ingot_domain::project::Project;
use tokio::process::Command;

use crate::commands::{GitCommandError, current_head_ref, git, head_oid, resolve_ref_oid};

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

async fn tracked_working_tree_has_changes(repo_path: &Path) -> Result<bool, GitCommandError> {
    let output = git(
        repo_path,
        &["status", "--porcelain", "--untracked-files=no"],
    )
    .await?;
    Ok(!String::from_utf8_lossy(&output.stdout).trim().is_empty())
}

async fn local_checkout_artifact_paths(repo_path: &Path) -> Result<Vec<String>, GitCommandError> {
    let output = git(
        repo_path,
        &[
            "status",
            "--porcelain=v1",
            "-z",
            "--untracked-files=all",
            "--ignored=matching",
        ],
    )
    .await?;
    Ok(output
        .stdout
        .split(|byte| *byte == 0)
        .filter_map(|entry| {
            if entry.starts_with(b"?? ") || entry.starts_with(b"!! ") {
                Some(String::from_utf8_lossy(&entry[3..]).into_owned())
            } else {
                None
            }
        })
        .collect())
}

async fn tracked_paths_in_commit(
    repo_path: &Path,
    commit_oid: &CommitOid,
) -> Result<Vec<String>, GitCommandError> {
    let output = git(
        repo_path,
        &["ls-tree", "-r", "--name-only", "-z", commit_oid.as_str()],
    )
    .await?;
    Ok(output
        .stdout
        .split(|byte| *byte == 0)
        .filter(|entry| !entry.is_empty())
        .map(|entry| String::from_utf8_lossy(entry).into_owned())
        .collect())
}

fn path_ancestors(path: &str) -> impl Iterator<Item = &str> {
    path.match_indices('/')
        .map(move |(index, _)| &path[..index])
}

fn collect_local_artifact_collision_paths(
    checkout_path: &Path,
    local_artifact_paths: &[String],
    tracked_paths: &[String],
) -> Vec<String> {
    let tracked_path_set: HashSet<&str> = tracked_paths.iter().map(String::as_str).collect();
    let mut untracked_dir_paths = BTreeSet::new();
    for path in local_artifact_paths {
        for ancestor in path_ancestors(path) {
            if checkout_path.join(ancestor).is_dir() {
                untracked_dir_paths.insert(ancestor.to_string());
            }
        }
    }

    let mut conflicts = BTreeSet::new();
    for path in local_artifact_paths {
        if tracked_path_set.contains(path.as_str()) {
            conflicts.insert(path.clone());
        }
        for ancestor in path_ancestors(path) {
            if tracked_path_set.contains(ancestor) {
                conflicts.insert(ancestor.to_string());
            }
        }
    }
    for path in tracked_paths {
        for ancestor in path_ancestors(path) {
            if untracked_dir_paths.contains(ancestor) {
                conflicts.insert(ancestor.to_string());
            }
        }
    }

    conflicts.into_iter().collect()
}

fn format_local_artifact_collision_message(conflicts: &[String]) -> String {
    let display_paths = conflicts.iter().take(3).cloned().collect::<Vec<_>>();
    let remainder = conflicts.len().saturating_sub(display_paths.len());
    if remainder == 0 {
        format!(
            "Registered checkout has local untracked or ignored paths that would be overwritten by the prepared commit: {}",
            display_paths.join(", ")
        )
    } else {
        format!(
            "Registered checkout has local untracked or ignored paths that would be overwritten by the prepared commit: {} (and {remainder} more)",
            display_paths.join(", ")
        )
    }
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

pub fn project_repo_paths_for_project(state_root: &Path, project: &Project) -> ProjectRepoPaths {
    project_repo_paths(state_root, project.id, &project.path)
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

pub async fn refresh_project_mirror_for_project(
    git_ops: &impl GitOperationRepository,
    state_root: &Path,
    project: &Project,
) -> Result<ProjectRepoPaths, RefreshMirrorError> {
    refresh_project_mirror(git_ops, state_root, project.id, &project.path).await
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

    if tracked_working_tree_has_changes(checkout_path).await? {
        return Ok(CheckoutSyncStatus::Blocked {
            code: "checkout_dirty",
            message:
                "Registered checkout has tracked or staged changes; clean it before finalizing"
                    .into(),
        });
    }

    Ok(CheckoutSyncStatus::Ready)
}

pub async fn checkout_sync_status_for_commit(
    checkout_path: &Path,
    commit_tree_repo_path: &Path,
    target_ref: &GitRef,
    commit_oid: &CommitOid,
) -> Result<CheckoutSyncStatus, GitCommandError> {
    match checkout_sync_status(checkout_path, target_ref).await? {
        CheckoutSyncStatus::Ready => {
            let local_artifact_paths = local_checkout_artifact_paths(checkout_path).await?;
            if local_artifact_paths.is_empty() {
                return Ok(CheckoutSyncStatus::Ready);
            }

            let tracked_paths = tracked_paths_in_commit(commit_tree_repo_path, commit_oid).await?;
            let conflicts = collect_local_artifact_collision_paths(
                checkout_path,
                &local_artifact_paths,
                &tracked_paths,
            );
            if conflicts.is_empty() {
                Ok(CheckoutSyncStatus::Ready)
            } else {
                Ok(CheckoutSyncStatus::Blocked {
                    code: "checkout_untracked_conflict",
                    message: format_local_artifact_collision_message(&conflicts),
                })
            }
        }
        blocked @ CheckoutSyncStatus::Blocked { .. } => Ok(blocked),
    }
}

pub async fn checkout_finalization_status(
    checkout_path: &Path,
    commit_tree_repo_path: &Path,
    target_ref: &GitRef,
    commit_oid: &CommitOid,
) -> Result<CheckoutFinalizationStatus, GitCommandError> {
    match checkout_sync_status_for_commit(
        checkout_path,
        commit_tree_repo_path,
        target_ref,
        commit_oid,
    )
    .await?
    {
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
    if let CheckoutSyncStatus::Blocked { message, .. } =
        checkout_sync_status_for_commit(checkout_path, mirror_git_dir, target_ref, commit_oid)
            .await?
    {
        return Err(GitCommandError::CommandFailed(message));
    }

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
    use super::{
        CheckoutFinalizationStatus, ensure_mirror, project_repo_paths, sync_checkout_to_commit,
    };
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

    #[tokio::test]
    async fn checkout_finalization_status_allows_non_conflicting_untracked_files() {
        let checkout_path = temp_git_repo("ingot-git-project-repo");
        let base_commit = git_output(&checkout_path, &["rev-parse", "HEAD"]);

        let state_root = unique_temp_path("ingot-git-project-repo-state");
        let paths = project_repo_paths(state_root.as_path(), ProjectId::new(), &checkout_path);
        ensure_mirror(&paths).await.expect("initial ensure mirror");

        fs::write(checkout_path.join("tracked.txt"), "updated\n").expect("write tracked file");
        git_sync(&checkout_path, &["add", "tracked.txt"]);
        git_sync(&checkout_path, &["commit", "-m", "update main"]);
        let prepared_commit = git_output(&checkout_path, &["rev-parse", "HEAD"]);
        ensure_mirror(&paths).await.expect("refresh mirror");

        git_sync(&checkout_path, &["reset", "--hard", &base_commit]);
        fs::write(checkout_path.join("note.tmp"), "scratch\n").expect("write untracked note");

        let status = super::checkout_finalization_status(
            &checkout_path,
            &paths.mirror_git_dir,
            &GitRef::new("refs/heads/main"),
            &CommitOid::new(prepared_commit),
        )
        .await
        .expect("checkout finalization status");

        assert_eq!(status, CheckoutFinalizationStatus::NeedsSync);
    }

    #[tokio::test]
    async fn checkout_finalization_status_blocks_conflicting_untracked_path() {
        let checkout_path = temp_git_repo("ingot-git-project-repo");
        let base_commit = git_output(&checkout_path, &["rev-parse", "HEAD"]);

        let state_root = unique_temp_path("ingot-git-project-repo-state");
        let paths = project_repo_paths(state_root.as_path(), ProjectId::new(), &checkout_path);
        ensure_mirror(&paths).await.expect("initial ensure mirror");

        fs::write(checkout_path.join("collide.txt"), "prepared\n").expect("write tracked file");
        git_sync(&checkout_path, &["add", "collide.txt"]);
        git_sync(&checkout_path, &["commit", "-m", "add collide file"]);
        let prepared_commit = git_output(&checkout_path, &["rev-parse", "HEAD"]);
        ensure_mirror(&paths).await.expect("refresh mirror");

        git_sync(&checkout_path, &["reset", "--hard", &base_commit]);
        fs::write(checkout_path.join("collide.txt"), "scratch\n")
            .expect("write conflicting untracked file");

        let status = super::checkout_finalization_status(
            &checkout_path,
            &paths.mirror_git_dir,
            &GitRef::new("refs/heads/main"),
            &CommitOid::new(prepared_commit),
        )
        .await
        .expect("checkout finalization status");

        assert!(matches!(
            status,
            CheckoutFinalizationStatus::Blocked { code, message }
                if code == "checkout_untracked_conflict"
                    && message.contains("collide.txt")
                    && message.contains("local untracked or ignored paths")
        ));
    }

    #[tokio::test]
    async fn checkout_finalization_status_blocks_conflicting_ignored_path() {
        let checkout_path = temp_git_repo("ingot-git-project-repo");
        fs::write(checkout_path.join(".gitignore"), "generated.out\n").expect("write gitignore");
        git_sync(&checkout_path, &["add", ".gitignore"]);
        git_sync(
            &checkout_path,
            &["commit", "-m", "ignore generated artifact"],
        );
        let base_commit = git_output(&checkout_path, &["rev-parse", "HEAD"]);

        let state_root = unique_temp_path("ingot-git-project-repo-state");
        let paths = project_repo_paths(state_root.as_path(), ProjectId::new(), &checkout_path);
        ensure_mirror(&paths).await.expect("initial ensure mirror");

        fs::write(checkout_path.join("generated.out"), "prepared\n").expect("write tracked file");
        git_sync(&checkout_path, &["add", "-f", "generated.out"]);
        git_sync(&checkout_path, &["commit", "-m", "track generated output"]);
        let prepared_commit = git_output(&checkout_path, &["rev-parse", "HEAD"]);
        ensure_mirror(&paths).await.expect("refresh mirror");

        git_sync(&checkout_path, &["reset", "--hard", &base_commit]);
        fs::write(checkout_path.join("generated.out"), "scratch\n")
            .expect("write conflicting ignored file");

        let status = super::checkout_finalization_status(
            &checkout_path,
            &paths.mirror_git_dir,
            &GitRef::new("refs/heads/main"),
            &CommitOid::new(prepared_commit),
        )
        .await
        .expect("checkout finalization status");

        assert!(matches!(
            status,
            CheckoutFinalizationStatus::Blocked { code, message }
                if code == "checkout_untracked_conflict"
                    && message.contains("generated.out")
                    && message.contains("ignored")
        ));
    }

    #[tokio::test]
    async fn checkout_finalization_status_blocks_untracked_descendant_of_tracked_file() {
        let checkout_path = temp_git_repo("ingot-git-project-repo");
        let base_commit = git_output(&checkout_path, &["rev-parse", "HEAD"]);

        let state_root = unique_temp_path("ingot-git-project-repo-state");
        let paths = project_repo_paths(state_root.as_path(), ProjectId::new(), &checkout_path);
        ensure_mirror(&paths).await.expect("initial ensure mirror");

        fs::write(checkout_path.join("foo"), "prepared\n").expect("write tracked file");
        git_sync(&checkout_path, &["add", "foo"]);
        git_sync(&checkout_path, &["commit", "-m", "add foo file"]);
        let prepared_commit = git_output(&checkout_path, &["rev-parse", "HEAD"]);
        ensure_mirror(&paths).await.expect("refresh mirror");

        git_sync(&checkout_path, &["reset", "--hard", &base_commit]);
        fs::create_dir_all(checkout_path.join("foo")).expect("create conflicting directory");
        fs::write(checkout_path.join("foo/note.txt"), "scratch\n")
            .expect("write conflicting nested file");

        let status = super::checkout_finalization_status(
            &checkout_path,
            &paths.mirror_git_dir,
            &GitRef::new("refs/heads/main"),
            &CommitOid::new(prepared_commit),
        )
        .await
        .expect("checkout finalization status");

        assert!(matches!(
            status,
            CheckoutFinalizationStatus::Blocked { code, message }
                if code == "checkout_untracked_conflict"
                    && message.contains("foo")
        ));
    }

    #[tokio::test]
    async fn sync_checkout_to_commit_preserves_non_conflicting_untracked_files() {
        let checkout_path = temp_git_repo("ingot-git-project-repo");
        let base_commit = git_output(&checkout_path, &["rev-parse", "HEAD"]);

        let state_root = unique_temp_path("ingot-git-project-repo-state");
        let paths = project_repo_paths(state_root.as_path(), ProjectId::new(), &checkout_path);
        ensure_mirror(&paths).await.expect("initial ensure mirror");

        fs::write(checkout_path.join("tracked.txt"), "updated\n").expect("write tracked file");
        git_sync(&checkout_path, &["add", "tracked.txt"]);
        git_sync(&checkout_path, &["commit", "-m", "update main"]);
        let prepared_commit = git_output(&checkout_path, &["rev-parse", "HEAD"]);
        ensure_mirror(&paths).await.expect("refresh mirror");

        git_sync(&checkout_path, &["reset", "--hard", &base_commit]);
        fs::write(checkout_path.join("note.tmp"), "scratch\n").expect("write untracked note");

        sync_checkout_to_commit(
            &checkout_path,
            &paths.mirror_git_dir,
            &GitRef::new("refs/heads/main"),
            &CommitOid::new(prepared_commit.clone()),
        )
        .await
        .expect("sync checkout");

        assert_eq!(
            git_output(&checkout_path, &["rev-parse", "HEAD"]),
            prepared_commit
        );
        assert_eq!(
            fs::read_to_string(checkout_path.join("note.tmp")).expect("read untracked note"),
            "scratch\n"
        );
    }

    #[tokio::test]
    async fn sync_checkout_to_commit_rejects_conflicting_ignored_paths() {
        let checkout_path = temp_git_repo("ingot-git-project-repo");
        fs::write(checkout_path.join(".gitignore"), "generated.out\n").expect("write gitignore");
        git_sync(&checkout_path, &["add", ".gitignore"]);
        git_sync(
            &checkout_path,
            &["commit", "-m", "ignore generated artifact"],
        );
        let base_commit = git_output(&checkout_path, &["rev-parse", "HEAD"]);

        let state_root = unique_temp_path("ingot-git-project-repo-state");
        let paths = project_repo_paths(state_root.as_path(), ProjectId::new(), &checkout_path);
        ensure_mirror(&paths).await.expect("initial ensure mirror");

        fs::write(checkout_path.join("generated.out"), "prepared\n").expect("write tracked file");
        git_sync(&checkout_path, &["add", "-f", "generated.out"]);
        git_sync(&checkout_path, &["commit", "-m", "track generated output"]);
        let prepared_commit = git_output(&checkout_path, &["rev-parse", "HEAD"]);
        ensure_mirror(&paths).await.expect("refresh mirror");

        git_sync(&checkout_path, &["reset", "--hard", &base_commit]);
        fs::write(checkout_path.join("generated.out"), "scratch\n")
            .expect("write conflicting ignored file");

        let error = sync_checkout_to_commit(
            &checkout_path,
            &paths.mirror_git_dir,
            &GitRef::new("refs/heads/main"),
            &CommitOid::new(prepared_commit),
        )
        .await
        .expect_err("sync should reject conflicting ignored paths");

        assert!(
            error.to_string().contains("generated.out"),
            "error should identify the conflicting ignored path"
        );
        assert_eq!(
            git_output(&checkout_path, &["rev-parse", "HEAD"]),
            base_commit,
            "sync must not reset the checkout when ignored paths would be clobbered"
        );
        assert_eq!(
            fs::read_to_string(checkout_path.join("generated.out"))
                .expect("read conflicting ignored file"),
            "scratch\n"
        );
    }
}
