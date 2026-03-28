mod command;
mod finalization;
mod system_actions;
#[cfg(test)]
mod test_support;
#[cfg(test)]
mod tests;
mod types;

pub use command::ConvergenceService;
pub use finalization::{
    finalize_prepared_convergence, find_or_create_finalize_operation,
    should_auto_finalize_prepared_convergence, should_invalidate_prepared_convergence,
    should_prepare_convergence,
};
pub use system_actions::{invalidate_prepared_convergence, promote_queue_heads};
pub use types::{
    ApprovalFinalizeReadiness, CheckoutFinalizationReadiness, ConvergenceApprovalContext,
    ConvergenceCommandPort, ConvergenceSystemActionPort, FinalizationTarget,
    FinalizePreparedTrigger, FinalizeTargetRefResult, PreparedConvergenceFinalizePort,
    RejectApprovalContext, RejectApprovalTeardown, SystemActionItemState, SystemActionProjectState,
};
