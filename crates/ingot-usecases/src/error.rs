#[derive(Debug, thiserror::Error)]
pub enum UseCaseError {
    #[error("item not found")]
    ItemNotFound,
    #[error("item not open")]
    ItemNotOpen,
    #[error("item not idle")]
    ItemNotIdle,
    #[error("approval not pending")]
    ApprovalNotPending,
    #[error("illegal step dispatch: {0}")]
    IllegalStepDispatch(String),
    #[error("active job exists")]
    ActiveJobExists,
    #[error("active convergence exists")]
    ActiveConvergenceExists,
    #[error("completed item cannot reopen")]
    CompletedItemCannotReopen,
    #[error("prepared convergence missing")]
    PreparedConvergenceMissing,
    #[error("prepared convergence stale")]
    PreparedConvergenceStale,
    #[error("repository error: {0}")]
    Repository(#[from] ingot_domain::ports::RepositoryError),
    #[error("internal error: {0}")]
    Internal(String),
}
