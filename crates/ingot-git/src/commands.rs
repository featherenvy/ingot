use std::future::Future;
use std::path::Path;
use std::process::Output;

use tokio::process::Command;

#[derive(Debug, thiserror::Error)]
pub enum GitCommandError {
    #[error("git command failed: {0}")]
    CommandFailed(String),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FinalizeTargetRefOutcome {
    AlreadyFinalized,
    UpdatedNow,
    Stale,
}

/// Run a git command in the given working directory.
pub async fn git(repo_path: &Path, args: &[&str]) -> Result<Output, GitCommandError> {
    let output = Command::new("git")
        .args(args)
        .current_dir(repo_path)
        .output()
        .await?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(GitCommandError::CommandFailed(stderr.to_string()));
    }

    Ok(output)
}

/// Get the current HEAD commit OID.
pub async fn head_oid(repo_path: &Path) -> Result<String, GitCommandError> {
    let output = git(repo_path, &["rev-parse", "HEAD"]).await?;
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

/// Get the current branch name for HEAD.
pub async fn current_branch_name(repo_path: &Path) -> Result<String, GitCommandError> {
    let output = git(repo_path, &["symbolic-ref", "--short", "HEAD"]).await?;
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

/// Get the symbolic ref for HEAD, or None when detached.
pub async fn current_head_ref(repo_path: &Path) -> Result<Option<String>, GitCommandError> {
    let output = Command::new("git")
        .args(["symbolic-ref", "--quiet", "HEAD"])
        .current_dir(repo_path)
        .output()
        .await?;

    decode_optional_verify(output)
}

/// Get the OID a ref points to.
pub async fn ref_oid(repo_path: &Path, ref_name: &str) -> Result<String, GitCommandError> {
    let output = git(repo_path, &["rev-parse", ref_name]).await?;
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

/// Resolve a ref if it exists, returning None for missing refs.
pub async fn resolve_ref_oid(
    repo_path: &Path,
    ref_name: &str,
) -> Result<Option<String>, GitCommandError> {
    verify_revision(repo_path, ref_name).await
}

/// Return whether the commit is reachable from any local ref.
pub async fn is_commit_reachable_from_any_ref(
    repo_path: &Path,
    commit_oid: &str,
) -> Result<bool, GitCommandError> {
    let verify_arg = format!("{commit_oid}^{{commit}}");
    if verify_revision(repo_path, &verify_arg).await?.is_none() {
        return Ok(false);
    }

    let output = Command::new("git")
        .args([
            "for-each-ref",
            "--contains",
            commit_oid,
            "--format=%(refname)",
        ])
        .current_dir(repo_path)
        .output()
        .await?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(GitCommandError::CommandFailed(stderr.to_string()));
    }

    Ok(!String::from_utf8_lossy(&output.stdout).trim().is_empty())
}

/// Return whether the commit object exists in the repository.
pub async fn commit_exists(repo_path: &Path, commit_oid: &str) -> Result<bool, GitCommandError> {
    let verify_arg = format!("{commit_oid}^{{commit}}");
    verify_revision(repo_path, &verify_arg)
        .await
        .map(|resolved| resolved.is_some())
}

async fn verify_revision(
    repo_path: &Path,
    revision: &str,
) -> Result<Option<String>, GitCommandError> {
    let output = Command::new("git")
        .args(["rev-parse", "--verify", "--quiet", revision])
        .current_dir(repo_path)
        .output()
        .await?;

    decode_optional_verify(output)
}

fn decode_optional_verify(output: Output) -> Result<Option<String>, GitCommandError> {
    if output.status.success() {
        return Ok(Some(
            String::from_utf8_lossy(&output.stdout).trim().to_string(),
        ));
    }

    let stderr = String::from_utf8_lossy(&output.stderr);
    if output.status.code() == Some(1) && stderr.trim().is_empty() {
        return Ok(None);
    }

    Err(GitCommandError::CommandFailed(stderr.to_string()))
}

pub async fn compare_and_swap_ref(
    repo_path: &Path,
    ref_name: &str,
    new_oid: &str,
    expected_old_oid: &str,
) -> Result<(), GitCommandError> {
    git(
        repo_path,
        &["update-ref", ref_name, new_oid, expected_old_oid],
    )
    .await?;
    Ok(())
}

pub async fn finalize_target_ref(
    repo_path: &Path,
    ref_name: &str,
    new_oid: &str,
    expected_old_oid: &str,
) -> Result<FinalizeTargetRefOutcome, GitCommandError> {
    finalize_target_ref_with(
        || resolve_ref_oid(repo_path, ref_name),
        || compare_and_swap_ref(repo_path, ref_name, new_oid, expected_old_oid),
        new_oid,
        expected_old_oid,
    )
    .await
}

async fn finalize_target_ref_with<Resolve, ResolveFut, Cas, CasFut>(
    mut resolve_ref: Resolve,
    compare_and_swap: Cas,
    new_oid: &str,
    expected_old_oid: &str,
) -> Result<FinalizeTargetRefOutcome, GitCommandError>
where
    Resolve: FnMut() -> ResolveFut,
    ResolveFut: Future<Output = Result<Option<String>, GitCommandError>>,
    Cas: FnOnce() -> CasFut,
    CasFut: Future<Output = Result<(), GitCommandError>>,
{
    let current_target_oid = resolve_ref().await?;
    if current_target_oid.as_deref() == Some(new_oid) {
        return Ok(FinalizeTargetRefOutcome::AlreadyFinalized);
    }
    if current_target_oid.as_deref() != Some(expected_old_oid) {
        return Ok(FinalizeTargetRefOutcome::Stale);
    }

    match compare_and_swap().await {
        Ok(()) => Ok(FinalizeTargetRefOutcome::UpdatedNow),
        Err(error) => {
            let current_target_oid = resolve_ref().await?;
            if current_target_oid.as_deref() == Some(new_oid) {
                Ok(FinalizeTargetRefOutcome::AlreadyFinalized)
            } else if current_target_oid.as_deref() != Some(expected_old_oid) {
                Ok(FinalizeTargetRefOutcome::Stale)
            } else {
                Err(error)
            }
        }
    }
}

pub async fn update_ref(
    repo_path: &Path,
    ref_name: &str,
    new_oid: &str,
) -> Result<(), GitCommandError> {
    git(repo_path, &["update-ref", ref_name, new_oid]).await?;
    Ok(())
}

pub async fn delete_ref(repo_path: &Path, ref_name: &str) -> Result<(), GitCommandError> {
    git(repo_path, &["update-ref", "-d", ref_name]).await?;
    Ok(())
}

/// Return whether a fully qualified ref name is accepted by Git.
pub async fn check_ref_format(ref_name: &str) -> Result<bool, GitCommandError> {
    let output = Command::new("git")
        .args(["check-ref-format", ref_name])
        .output()
        .await?;
    Ok(output.status.success())
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::{
        FinalizeTargetRefOutcome, GitCommandError, check_ref_format, finalize_target_ref,
        finalize_target_ref_with, resolve_ref_oid,
    };
    use ingot_test_support::git::{git_output, run_git as git_sync, temp_git_repo};

    #[tokio::test]
    async fn check_ref_format_accepts_valid_head_refs() {
        assert!(
            check_ref_format("refs/heads/main")
                .await
                .expect("check main ref")
        );
        assert!(
            check_ref_format("refs/heads/feature/ref-hardening")
                .await
                .expect("check nested ref")
        );
        assert!(
            check_ref_format("refs/heads/release-2026.03")
                .await
                .expect("check dotted ref")
        );
        assert!(
            check_ref_format("refs/heads/@")
                .await
                .expect("check at-sign ref")
        );
        assert!(
            check_ref_format("refs/heads/-leading-dash")
                .await
                .expect("check leading-dash ref")
        );
    }

    #[tokio::test]
    async fn check_ref_format_rejects_git_invalid_head_refs() {
        for invalid_ref in [
            "refs/heads/foo..bar",
            "refs/heads/foo.lock",
            "refs/heads/bad@{name}",
            "refs/heads/.hidden",
            "refs/heads/feature/.hidden",
            "refs/heads/feature/trailing.",
            "refs/heads/with space",
            "refs/heads/line\nbreak",
            "refs/heads/tab\tname",
            "refs/heads/with~tilde",
            "refs/heads/with^caret",
            "refs/heads/with:colon",
            "refs/heads/with?question",
            "refs/heads/with*star",
            "refs/heads/with[bracket",
            "refs/heads/with\\backslash",
        ] {
            assert!(
                !check_ref_format(invalid_ref)
                    .await
                    .unwrap_or_else(|_| panic!("check invalid ref: {invalid_ref}")),
                "{invalid_ref} should be rejected by git check-ref-format"
            );
        }
    }

    #[tokio::test]
    async fn finalize_target_ref_reports_already_finalized_when_ref_is_at_new_oid() {
        let repo = temp_git_repo("ingot-git-finalize-ref");
        let base_commit_oid = git_output(&repo, &["rev-parse", "HEAD"]);
        std::fs::write(repo.join("tracked.txt"), "prepared").expect("write prepared");
        git_sync(&repo, &["add", "tracked.txt"]);
        git_sync(&repo, &["commit", "-m", "prepared"]);
        let prepared_commit_oid = git_output(&repo, &["rev-parse", "HEAD"]);

        let outcome = finalize_target_ref(
            &repo,
            "refs/heads/main",
            &prepared_commit_oid,
            &base_commit_oid,
        )
        .await
        .expect("finalize target ref");

        assert_eq!(outcome, FinalizeTargetRefOutcome::AlreadyFinalized);
    }

    #[tokio::test]
    async fn finalize_target_ref_updates_ref_when_target_is_at_expected_old_oid() {
        let repo = temp_git_repo("ingot-git-finalize-ref");
        let base_commit_oid = git_output(&repo, &["rev-parse", "HEAD"]);
        std::fs::write(repo.join("tracked.txt"), "prepared").expect("write prepared");
        git_sync(&repo, &["add", "tracked.txt"]);
        git_sync(&repo, &["commit", "-m", "prepared"]);
        let prepared_commit_oid = git_output(&repo, &["rev-parse", "HEAD"]);
        git_sync(&repo, &["reset", "--hard", &base_commit_oid]);

        let outcome = finalize_target_ref(
            &repo,
            "refs/heads/main",
            &prepared_commit_oid,
            &base_commit_oid,
        )
        .await
        .expect("finalize target ref");

        assert_eq!(outcome, FinalizeTargetRefOutcome::UpdatedNow);
        assert_eq!(
            resolve_ref_oid(&repo, "refs/heads/main")
                .await
                .expect("resolve main"),
            Some(prepared_commit_oid)
        );
    }

    #[tokio::test]
    async fn finalize_target_ref_reports_stale_when_ref_has_moved_elsewhere() {
        let repo = temp_git_repo("ingot-git-finalize-ref");
        let base_commit_oid = git_output(&repo, &["rev-parse", "HEAD"]);
        std::fs::write(repo.join("tracked.txt"), "prepared").expect("write prepared");
        git_sync(&repo, &["add", "tracked.txt"]);
        git_sync(&repo, &["commit", "-m", "prepared"]);
        let prepared_commit_oid = git_output(&repo, &["rev-parse", "HEAD"]);
        git_sync(&repo, &["reset", "--hard", &base_commit_oid]);
        std::fs::write(repo.join("tracked.txt"), "moved").expect("write moved");
        git_sync(&repo, &["add", "tracked.txt"]);
        git_sync(&repo, &["commit", "-m", "moved"]);

        let outcome = finalize_target_ref(
            &repo,
            "refs/heads/main",
            &prepared_commit_oid,
            &base_commit_oid,
        )
        .await
        .expect("finalize target ref");

        assert_eq!(outcome, FinalizeTargetRefOutcome::Stale);
    }

    #[tokio::test]
    async fn finalize_target_ref_reports_already_finalized_when_cas_loses_race_to_new_oid() {
        let resolve_calls = Arc::new(AtomicUsize::new(0));
        let resolve_calls_for_closure = resolve_calls.clone();
        let outcome = finalize_target_ref_with(
            move || {
                let resolve_calls = resolve_calls_for_closure.clone();
                async move {
                    let call_no = resolve_calls.fetch_add(1, Ordering::SeqCst);
                    Ok(match call_no {
                        0 => Some("base".to_string()),
                        1 => Some("prepared".to_string()),
                        _ => unreachable!("only two resolve calls expected"),
                    })
                }
            },
            || async { Err(GitCommandError::CommandFailed("stale old oid".into())) },
            "prepared",
            "base",
        )
        .await
        .expect("finalize target ref");

        assert_eq!(outcome, FinalizeTargetRefOutcome::AlreadyFinalized);
        assert_eq!(resolve_calls.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn finalize_target_ref_preserves_cas_error_when_ref_is_still_at_expected_old_oid() {
        let outcome = finalize_target_ref_with(
            || async { Ok(Some("base".to_string())) },
            || async { Err(GitCommandError::CommandFailed("update-ref failed".into())) },
            "prepared",
            "base",
        )
        .await;

        assert!(matches!(
            outcome,
            Err(GitCommandError::CommandFailed(message)) if message == "update-ref failed"
        ));
    }
}
