#[derive(Debug, thiserror::Error)]
pub enum UseCaseError {
    #[error("project not found")]
    ProjectNotFound,
    #[error("item not found")]
    ItemNotFound,
    #[error("item not open")]
    ItemNotOpen,
    #[error("item not idle")]
    ItemNotIdle,
    #[error("approval not pending")]
    ApprovalNotPending,
    #[error("convergence is not preparable")]
    ConvergenceNotPreparable,
    #[error("convergence is not queued")]
    ConvergenceNotQueued,
    #[error("convergence is not lane head")]
    ConvergenceNotLaneHead,
    #[error("job is not active")]
    JobNotActive,
    #[error("finding not found")]
    FindingNotFound,
    #[error("finding is not triageable")]
    FindingNotTriageable,
    #[error("finding subject is unreachable")]
    FindingSubjectUnreachable,
    #[error("invalid finding triage: {0}")]
    InvalidFindingTriage(String),
    #[error("illegal step dispatch: {0}")]
    IllegalStepDispatch(String),
    #[error("active job exists")]
    ActiveJobExists,
    #[error("active convergence exists")]
    ActiveConvergenceExists,
    #[error("completed item cannot reopen")]
    CompletedItemCannotReopen,
    #[error("invalid target ref: {0}")]
    InvalidTargetRef(String),
    #[error("target ref unresolved: {0}")]
    TargetRefUnresolved(String),
    #[error("revision seed unreachable: {0}")]
    RevisionSeedUnreachable(String),
    #[error("linked item not found")]
    LinkedItemNotFound,
    #[error("linked item must belong to the same project")]
    LinkedItemProjectMismatch,
    #[error("prepared convergence missing")]
    PreparedConvergenceMissing,
    #[error("prepared convergence stale")]
    PreparedConvergenceStale,
    #[error("protocol violation: {0}")]
    ProtocolViolation(String),
    #[error("repository error: {0}")]
    Repository(#[from] ingot_domain::ports::RepositoryError),
    #[error("internal error: {0}")]
    Internal(String),
}
