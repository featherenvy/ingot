use std::future::Future;
use std::path::Path;

use crate::commit_oid::CommitOid;
use crate::git_ref::GitRef;

#[derive(Debug, thiserror::Error)]
pub enum GitPortError {
    #[error("git operation failed: {0}")]
    Internal(String),
}

#[derive(Debug, thiserror::Error)]
pub enum TargetRefHoldError {
    #[error("target ref moved")]
    Stale,
    #[error("git operation failed: {0}")]
    Internal(String),
}

pub trait JobCompletionGitPort: Send + Sync {
    type Hold: Send;

    fn commit_exists(
        &self,
        repo_path: &Path,
        commit_oid: &CommitOid,
    ) -> impl Future<Output = Result<bool, GitPortError>> + Send;

    fn verify_and_hold_target_ref(
        &self,
        repo_path: &Path,
        target_ref: &GitRef,
        expected_oid: &CommitOid,
    ) -> impl Future<Output = Result<Self::Hold, TargetRefHoldError>> + Send;

    fn release_hold(
        &self,
        hold: Self::Hold,
    ) -> impl Future<Output = Result<(), GitPortError>> + Send;
}
