use std::path::Path;

use ingot_domain::commit_oid::CommitOid;

use crate::commands::{GitCommandError, git};

pub async fn changed_paths_between(
    repo_path: &Path,
    base_commit_oid: &CommitOid,
    head_commit_oid: &CommitOid,
) -> Result<Vec<String>, GitCommandError> {
    if base_commit_oid == head_commit_oid {
        return Ok(vec![]);
    }

    let range = format!("{base_commit_oid}..{head_commit_oid}");
    let output = git(repo_path, &["diff", "--name-only", &range]).await?;
    Ok(String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(ToOwned::to_owned)
        .collect())
}
