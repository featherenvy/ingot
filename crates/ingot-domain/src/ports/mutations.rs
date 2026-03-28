use std::future::Future;

use crate::activity::Activity;
use crate::commit_oid::CommitOid;
use crate::convergence::Convergence;
use crate::convergence_queue::ConvergenceQueueEntry;
use crate::finding::Finding;
use crate::git_operation::GitOperation;
use crate::git_ref::GitRef;
use crate::ids::*;
use crate::item::{ApprovalState, Item};
use crate::job::Job;
use crate::project::Project;
use crate::revision::ItemRevision;
use crate::workspace::Workspace;

use super::errors::RepositoryError;
use super::repositories::FinishJobNonSuccessParams;

// --- Job completion types ---

#[derive(Debug, Clone)]
pub struct JobCompletionContext {
    pub job: Job,
    pub item: Item,
    pub project: Project,
    pub revision: ItemRevision,
    pub convergences: Vec<Convergence>,
}

#[derive(Debug, Clone)]
pub struct PreparedConvergenceGuard {
    pub convergence_id: ConvergenceId,
    pub item_revision_id: ItemRevisionId,
    pub target_ref: GitRef,
    pub expected_target_head_oid: CommitOid,
    pub next_approval_state: Option<ApprovalState>,
}

#[derive(Debug, Clone)]
pub struct JobCompletionMutation {
    pub job_id: JobId,
    pub item_id: ItemId,
    pub expected_item_revision_id: ItemRevisionId,
    pub outcome_class: crate::job::OutcomeClass,
    pub clear_item_escalation: bool,
    pub result_schema_version: Option<String>,
    pub result_payload: Option<serde_json::Value>,
    pub output_commit_oid: Option<CommitOid>,
    pub findings: Vec<Finding>,
    pub prepared_convergence_guard: Option<PreparedConvergenceGuard>,
}

#[derive(Debug, Clone)]
pub struct CompletedJobCompletion {
    pub job: Job,
    pub finding_count: usize,
}

pub trait JobCompletionRepository: Send + Sync {
    fn load_job_completion_context(
        &self,
        job_id: JobId,
    ) -> impl Future<Output = Result<JobCompletionContext, RepositoryError>> + Send;

    fn load_completed_job_completion(
        &self,
        job_id: JobId,
    ) -> impl Future<Output = Result<Option<CompletedJobCompletion>, RepositoryError>> + Send;

    fn apply_job_completion(
        &self,
        mutation: JobCompletionMutation,
    ) -> impl Future<Output = Result<(), RepositoryError>> + Send;
}

// --- Revision lane teardown types ---

#[derive(Debug, Clone)]
pub struct TeardownJobCancellation {
    pub params: FinishJobNonSuccessParams,
    pub workspace_update: Option<Workspace>,
    pub activity: Activity,
}

#[derive(Debug, Clone, Default)]
pub struct RevisionLaneTeardownMutation {
    pub job_cancellations: Vec<TeardownJobCancellation>,
    pub convergence_updates: Vec<Convergence>,
    pub workspace_abandonments: Vec<Workspace>,
    pub queue_entry_update: Option<ConvergenceQueueEntry>,
    pub git_operation_updates: Vec<GitOperation>,
    pub git_operation_activities: Vec<Activity>,
}

pub trait RevisionLaneTeardownRepository: Send + Sync {
    fn apply_revision_lane_teardown(
        &self,
        mutation: RevisionLaneTeardownMutation,
    ) -> impl Future<Output = Result<(), RepositoryError>> + Send;
}

// --- Invalidate prepared convergence types ---

#[derive(Debug, Clone)]
pub struct InvalidatePreparedConvergenceMutation {
    pub convergence: Convergence,
    pub workspace_update: Option<Workspace>,
    pub item: Item,
    pub activity: Activity,
}

pub trait InvalidatePreparedConvergenceRepository: Send + Sync {
    fn apply_invalidate_prepared_convergence(
        &self,
        mutation: InvalidatePreparedConvergenceMutation,
    ) -> impl Future<Output = Result<(), RepositoryError>> + Send;
}
