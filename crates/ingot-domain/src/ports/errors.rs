#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConflictKind {
    JobNotActive,
    JobRevisionStale,
    JobMissingWorkspace,
    JobUpdateConflict,
    PreparedConvergenceMissing,
    PreparedConvergenceStale,
    DatabaseConstraint(String),
    Other(String),
}

impl std::fmt::Display for ConflictKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::JobNotActive => write!(f, "job_not_active"),
            Self::JobRevisionStale => write!(f, "job_revision_stale"),
            Self::JobMissingWorkspace => write!(f, "job_missing_workspace"),
            Self::JobUpdateConflict => write!(f, "job_update_conflict"),
            Self::PreparedConvergenceMissing => write!(f, "prepared_convergence_missing"),
            Self::PreparedConvergenceStale => write!(f, "prepared_convergence_stale"),
            Self::DatabaseConstraint(msg) | Self::Other(msg) => write!(f, "{msg}"),
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum RepositoryError {
    #[error("entity not found")]
    NotFound,
    #[error("conflict: {0}")]
    Conflict(ConflictKind),
    #[error("database error: {0}")]
    Database(#[from] Box<dyn std::error::Error + Send + Sync>),
}
