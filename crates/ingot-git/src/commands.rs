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

/// Get the OID a ref points to.
pub async fn ref_oid(repo_path: &Path, ref_name: &str) -> Result<String, GitCommandError> {
    let output = git(repo_path, &["rev-parse", ref_name]).await?;
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}
