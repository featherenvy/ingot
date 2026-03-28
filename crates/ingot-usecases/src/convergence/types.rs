use std::future::Future;

use ingot_domain::commit_oid::CommitOid;
use ingot_domain::convergence::Convergence;
use ingot_domain::convergence_queue::ConvergenceQueueEntry;
use ingot_domain::finding::Finding;
use ingot_domain::git_operation::GitOperation;
use ingot_domain::ids::{ItemId, ProjectId};
use ingot_domain::job::Job;
use ingot_domain::ports::ConvergenceQueuePrepareContext;
use ingot_domain::project::Project;
use ingot_domain::revision::ItemRevision;

use crate::UseCaseError;

#[derive(Debug, Clone)]
pub struct SystemActionItemState {
    pub item_id: ItemId,
    pub item: ingot_domain::item::Item,
    pub revision: ItemRevision,
    pub jobs: Vec<Job>,
    pub findings: Vec<Finding>,
    pub convergences: Vec<Convergence>,
    pub queue_entry: Option<ConvergenceQueueEntry>,
}

#[derive(Debug, Clone)]
pub struct SystemActionProjectState {
    pub project: Project,
    pub items: Vec<SystemActionItemState>,
}

#[derive(Debug, Clone)]
pub struct ConvergenceApprovalContext {
    pub project: Project,
    pub item: ingot_domain::item::Item,
    pub revision: ItemRevision,
    pub has_active_job: bool,
    pub has_active_convergence: bool,
    pub finalize_readiness: ApprovalFinalizeReadiness,
}

#[derive(Debug, Clone)]
pub enum ApprovalFinalizeReadiness {
    MissingPreparedConvergence,
    PreparedConvergenceStale,
    ConvergenceNotQueued,
    ConvergenceNotLaneHead,
    Ready {
        convergence: Box<Convergence>,
        queue_entry: ConvergenceQueueEntry,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FinalizePreparedTrigger {
    ApprovalCommand,
    SystemCommand,
}

pub struct FinalizationTarget<'a> {
    pub convergence: &'a Convergence,
    pub queue_entry: &'a ConvergenceQueueEntry,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CheckoutFinalizationReadiness {
    Blocked { message: String },
    NeedsSync,
    Synced,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FinalizeTargetRefResult {
    AlreadyFinalized,
    UpdatedNow,
    Stale,
}

#[derive(Debug, Clone, Default)]
pub struct RejectApprovalTeardown {
    pub has_cancelled_convergence: bool,
    pub has_cancelled_queue_entry: bool,
    pub first_cancelled_convergence_id: Option<String>,
    pub first_cancelled_queue_entry_id: Option<String>,
}

#[derive(Debug, Clone)]
pub struct RejectApprovalContext {
    pub item: ingot_domain::item::Item,
    pub has_active_job: bool,
    pub has_active_convergence: bool,
}

pub trait ConvergenceCommandPort: Send + Sync {
    fn load_queue_prepare_context(
        &self,
        project_id: ProjectId,
        item_id: ItemId,
    ) -> impl Future<Output = Result<ConvergenceQueuePrepareContext, UseCaseError>> + Send;

    fn create_queue_entry(
        &self,
        queue_entry: &ConvergenceQueueEntry,
    ) -> impl Future<Output = Result<(), UseCaseError>> + Send;

    fn update_queue_entry(
        &self,
        queue_entry: &ConvergenceQueueEntry,
    ) -> impl Future<Output = Result<(), UseCaseError>> + Send;

    fn append_activity(
        &self,
        activity: &ingot_domain::activity::Activity,
    ) -> impl Future<Output = Result<(), UseCaseError>> + Send;

    fn load_approval_context(
        &self,
        project_id: ProjectId,
        item_id: ItemId,
    ) -> impl Future<Output = Result<ConvergenceApprovalContext, UseCaseError>> + Send;

    fn update_item(
        &self,
        item: &ingot_domain::item::Item,
    ) -> impl Future<Output = Result<(), UseCaseError>> + Send;

    fn load_reject_approval_context(
        &self,
        project_id: ProjectId,
        item_id: ItemId,
    ) -> impl Future<Output = Result<RejectApprovalContext, UseCaseError>> + Send;

    fn teardown_reject_approval(
        &self,
        project_id: ProjectId,
        item_id: ItemId,
    ) -> impl Future<Output = Result<RejectApprovalTeardown, UseCaseError>> + Send;

    fn apply_rejected_approval(
        &self,
        item: &ingot_domain::item::Item,
        next_revision: &ItemRevision,
    ) -> impl Future<Output = Result<(), UseCaseError>> + Send;
}

pub trait ConvergenceSystemActionPort: Send + Sync {
    fn load_system_action_projects(
        &self,
    ) -> impl Future<Output = Result<Vec<SystemActionProjectState>, UseCaseError>> + Send;

    fn promote_queue_heads(
        &self,
        project_id: ProjectId,
    ) -> impl Future<Output = Result<(), UseCaseError>> + Send;

    fn prepare_queue_head_convergence(
        &self,
        project: &Project,
        state: &SystemActionItemState,
        queue_entry: &ConvergenceQueueEntry,
    ) -> impl Future<Output = Result<(), UseCaseError>> + Send;

    fn invalidate_prepared_convergence(
        &self,
        project_id: ProjectId,
        item_id: ItemId,
    ) -> impl Future<Output = Result<(), UseCaseError>> + Send;

    fn auto_finalize_prepared_convergence(
        &self,
        project_id: ProjectId,
        item_id: ItemId,
    ) -> impl Future<Output = Result<bool, UseCaseError>> + Send;

    fn auto_queue_convergence(
        &self,
        project_id: ProjectId,
        item_id: ItemId,
    ) -> impl Future<Output = Result<bool, UseCaseError>> + Send;
}

pub trait PreparedConvergenceFinalizePort: Send + Sync {
    fn find_or_create_finalize_operation(
        &self,
        operation: &GitOperation,
    ) -> impl Future<Output = Result<GitOperation, UseCaseError>> + Send;

    fn finalize_target_ref(
        &self,
        project: &Project,
        convergence: &Convergence,
    ) -> impl Future<Output = Result<FinalizeTargetRefResult, UseCaseError>> + Send;

    fn checkout_finalization_readiness(
        &self,
        project: &Project,
        item: &ingot_domain::item::Item,
        revision: &ItemRevision,
        prepared_commit_oid: &CommitOid,
    ) -> impl Future<Output = Result<CheckoutFinalizationReadiness, UseCaseError>> + Send;

    fn sync_checkout_to_prepared_commit(
        &self,
        project: &Project,
        revision: &ItemRevision,
        prepared_commit_oid: &CommitOid,
    ) -> impl Future<Output = Result<(), UseCaseError>> + Send;

    fn update_git_operation(
        &self,
        operation: &GitOperation,
    ) -> impl Future<Output = Result<(), UseCaseError>> + Send;

    fn apply_successful_finalization(
        &self,
        trigger: FinalizePreparedTrigger,
        project: &Project,
        item: &ingot_domain::item::Item,
        revision: &ItemRevision,
        target: FinalizationTarget<'_>,
        operation: &GitOperation,
    ) -> impl Future<Output = Result<(), UseCaseError>> + Send;
}
