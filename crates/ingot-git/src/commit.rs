use std::path::Path;
use std::process::Stdio;

use ingot_domain::ids::{ConvergenceId, GitOperationId, ItemId, JobId};
use tokio::process::Command;

use crate::commands::{GitCommandError, git, head_oid};

#[derive(Debug, Clone)]
pub struct JobCommitTrailers {
    pub operation_id: GitOperationId,
    pub item_id: ItemId,
    pub revision_no: u32,
    pub job_id: JobId,
}

#[derive(Debug, Clone)]
pub struct ConvergenceCommitTrailers {
    pub operation_id: GitOperationId,
    pub item_id: ItemId,
    pub revision_no: u32,
    pub convergence_id: ConvergenceId,
    pub source_commit_oid: String,
}

pub async fn working_tree_has_changes(repo_path: &Path) -> Result<bool, GitCommandError> {
    let output = git(repo_path, &["status", "--porcelain"]).await?;
    Ok(!String::from_utf8_lossy(&output.stdout).trim().is_empty())
}

pub async fn create_daemon_job_commit(
    repo_path: &Path,
    subject: &str,
    summary: &str,
    trailers: &JobCommitTrailers,
) -> Result<String, GitCommandError> {
    let message = format!(
        "{subject}\n\n{summary}\n\nIngot-Operation: {}\nIngot-Item: {}\nIngot-Revision: {}\nIngot-Job: {}",
        trailers.operation_id, trailers.item_id, trailers.revision_no, trailers.job_id
    );
    create_daemon_commit_from_staged(repo_path, &message).await
}

pub async fn create_daemon_convergence_commit(
    repo_path: &Path,
    original_message: &str,
    trailers: &ConvergenceCommitTrailers,
) -> Result<String, GitCommandError> {
    let message = format!(
        "{}\n\nIngot-Operation: {}\nIngot-Item: {}\nIngot-Revision: {}\nIngot-Convergence: {}\nIngot-Source-Commit: {}",
        original_message.trim_end(),
        trailers.operation_id,
        trailers.item_id,
        trailers.revision_no,
        trailers.convergence_id,
        trailers.source_commit_oid
    );
    create_daemon_commit_from_staged(repo_path, &message).await
}

pub async fn create_daemon_commit_from_staged(
    repo_path: &Path,
    message: &str,
) -> Result<String, GitCommandError> {
    git(repo_path, &["add", "-A"]).await?;

    let mut child = Command::new("git")
        .args(["commit", "--no-verify", "-F", "-"])
        .current_dir(repo_path)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;

    if let Some(mut stdin) = child.stdin.take() {
        use tokio::io::AsyncWriteExt;
        stdin.write_all(message.as_bytes()).await?;
    }

    let output = child.wait_with_output().await?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let stdout = String::from_utf8_lossy(&output.stdout);
        let message = if stderr.trim().is_empty() {
            stdout.trim().to_string()
        } else {
            stderr.trim().to_string()
        };
        return Err(GitCommandError::CommandFailed(message));
    }

    head_oid(repo_path).await
}

pub async fn commit_message(repo_path: &Path, commit_oid: &str) -> Result<String, GitCommandError> {
    let output = git(repo_path, &["show", "-s", "--format=%B", commit_oid]).await?;
    Ok(String::from_utf8_lossy(&output.stdout)
        .trim_end()
        .to_string())
}

pub async fn list_commits_oldest_first(
    repo_path: &Path,
    base_commit_oid: &str,
    head_commit_oid: &str,
) -> Result<Vec<String>, GitCommandError> {
    if base_commit_oid == head_commit_oid {
        return Ok(vec![]);
    }

    let range = format!("{base_commit_oid}..{head_commit_oid}");
    let output = git(repo_path, &["rev-list", "--reverse", &range]).await?;
    Ok(String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(ToOwned::to_owned)
        .collect())
}

pub async fn cherry_pick_no_commit(
    repo_path: &Path,
    commit_oid: &str,
) -> Result<(), GitCommandError> {
    git(repo_path, &["cherry-pick", "--no-commit", commit_oid]).await?;
    Ok(())
}

pub async fn abort_cherry_pick(repo_path: &Path) -> Result<(), GitCommandError> {
    git(repo_path, &["cherry-pick", "--abort"])
        .await
        .map(|_| ())
}
