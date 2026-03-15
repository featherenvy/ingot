use std::future::Future;
use std::path::Path;

use chrono::{DateTime, Utc};

use crate::activity::Activity;
use crate::agent::Agent;
use crate::convergence::Convergence;
use crate::convergence_queue::ConvergenceQueueEntry;
use crate::finding::Finding;
use crate::git_operation::GitOperation;
use crate::ids::*;
use crate::item::{EscalationReason, Item};
use crate::job::{Job, JobStatus, OutcomeClass};
use crate::project::Project;
use crate::revision::ItemRevision;
use crate::revision_context::RevisionContext;
use crate::workspace::Workspace;
use crate::{ids::ConvergenceId, item::ApprovalState};

pub trait ProjectRepository: Send + Sync {
    fn list(&self) -> impl Future<Output = Result<Vec<Project>, RepositoryError>> + Send;
    fn get(&self, id: ProjectId) -> impl Future<Output = Result<Project, RepositoryError>> + Send;
    fn create(&self, project: &Project)
    -> impl Future<Output = Result<(), RepositoryError>> + Send;
    fn update(&self, project: &Project)
    -> impl Future<Output = Result<(), RepositoryError>> + Send;
    fn delete(&self, id: ProjectId) -> impl Future<Output = Result<(), RepositoryError>> + Send;
}

pub trait AgentRepository: Send + Sync {
    fn list(&self) -> impl Future<Output = Result<Vec<Agent>, RepositoryError>> + Send;
    fn get(&self, id: AgentId) -> impl Future<Output = Result<Agent, RepositoryError>> + Send;
    fn create(&self, agent: &Agent) -> impl Future<Output = Result<(), RepositoryError>> + Send;
    fn update(&self, agent: &Agent) -> impl Future<Output = Result<(), RepositoryError>> + Send;
    fn delete(&self, id: AgentId) -> impl Future<Output = Result<(), RepositoryError>> + Send;
}

pub trait ItemRepository: Send + Sync {
    fn list_by_project(
        &self,
        project_id: ProjectId,
    ) -> impl Future<Output = Result<Vec<Item>, RepositoryError>> + Send;
    fn get(&self, id: ItemId) -> impl Future<Output = Result<Item, RepositoryError>> + Send;
    fn create(&self, item: &Item) -> impl Future<Output = Result<(), RepositoryError>> + Send;
    fn update(&self, item: &Item) -> impl Future<Output = Result<(), RepositoryError>> + Send;
    fn create_with_revision(
        &self,
        item: &Item,
        revision: &ItemRevision,
    ) -> impl Future<Output = Result<(), RepositoryError>> + Send;
}

pub trait RevisionRepository: Send + Sync {
    fn list_by_item(
        &self,
        item_id: ItemId,
    ) -> impl Future<Output = Result<Vec<ItemRevision>, RepositoryError>> + Send;
    fn get(
        &self,
        id: ItemRevisionId,
    ) -> impl Future<Output = Result<ItemRevision, RepositoryError>> + Send;
    fn create(
        &self,
        revision: &ItemRevision,
    ) -> impl Future<Output = Result<(), RepositoryError>> + Send;
}

pub trait RevisionContextRepository: Send + Sync {
    fn get(
        &self,
        revision_id: ItemRevisionId,
    ) -> impl Future<Output = Result<Option<RevisionContext>, RepositoryError>> + Send;
    fn upsert(
        &self,
        context: &RevisionContext,
    ) -> impl Future<Output = Result<(), RepositoryError>> + Send;
}

pub trait JobRepository: Send + Sync {
    fn list_by_project(
        &self,
        project_id: ProjectId,
    ) -> impl Future<Output = Result<Vec<Job>, RepositoryError>> + Send;
    fn list_by_revision(
        &self,
        revision_id: ItemRevisionId,
    ) -> impl Future<Output = Result<Vec<Job>, RepositoryError>> + Send;
    fn get(&self, id: JobId) -> impl Future<Output = Result<Job, RepositoryError>> + Send;
    fn create(&self, job: &Job) -> impl Future<Output = Result<(), RepositoryError>> + Send;
    fn update(&self, job: &Job) -> impl Future<Output = Result<(), RepositoryError>> + Send;
    fn find_active_for_revision(
        &self,
        revision_id: ItemRevisionId,
    ) -> impl Future<Output = Result<Option<Job>, RepositoryError>> + Send;
    fn list_by_item(
        &self,
        item_id: ItemId,
    ) -> impl Future<Output = Result<Vec<Job>, RepositoryError>> + Send;
    fn list_queued(
        &self,
        limit: u32,
    ) -> impl Future<Output = Result<Vec<Job>, RepositoryError>> + Send;
    fn list_active(&self) -> impl Future<Output = Result<Vec<Job>, RepositoryError>> + Send;
    fn start_execution(
        &self,
        params: StartJobExecutionParams,
    ) -> impl Future<Output = Result<(), RepositoryError>> + Send;
    fn heartbeat_execution(
        &self,
        job_id: JobId,
        item_id: ItemId,
        revision_id: ItemRevisionId,
        lease_owner_id: &str,
        lease_expires_at: DateTime<Utc>,
    ) -> impl Future<Output = Result<(), RepositoryError>> + Send;
    fn finish_non_success(
        &self,
        params: FinishJobNonSuccessParams,
    ) -> impl Future<Output = Result<(), RepositoryError>> + Send;
}

pub trait WorkspaceRepository: Send + Sync {
    fn list_by_project(
        &self,
        project_id: ProjectId,
    ) -> impl Future<Output = Result<Vec<Workspace>, RepositoryError>> + Send;
    fn get(
        &self,
        id: WorkspaceId,
    ) -> impl Future<Output = Result<Workspace, RepositoryError>> + Send;
    fn create(
        &self,
        workspace: &Workspace,
    ) -> impl Future<Output = Result<(), RepositoryError>> + Send;
    fn update(
        &self,
        workspace: &Workspace,
    ) -> impl Future<Output = Result<(), RepositoryError>> + Send;
    fn find_authoring_for_revision(
        &self,
        revision_id: ItemRevisionId,
    ) -> impl Future<Output = Result<Option<Workspace>, RepositoryError>> + Send;
    fn list_by_item(
        &self,
        item_id: ItemId,
    ) -> impl Future<Output = Result<Vec<Workspace>, RepositoryError>> + Send;
}

pub trait ConvergenceRepository: Send + Sync {
    fn list_by_revision(
        &self,
        revision_id: ItemRevisionId,
    ) -> impl Future<Output = Result<Vec<Convergence>, RepositoryError>> + Send;
    fn get(
        &self,
        id: ConvergenceId,
    ) -> impl Future<Output = Result<Convergence, RepositoryError>> + Send;
    fn create(
        &self,
        convergence: &Convergence,
    ) -> impl Future<Output = Result<(), RepositoryError>> + Send;
    fn update(
        &self,
        convergence: &Convergence,
    ) -> impl Future<Output = Result<(), RepositoryError>> + Send;
    fn find_active_for_revision(
        &self,
        revision_id: ItemRevisionId,
    ) -> impl Future<Output = Result<Option<Convergence>, RepositoryError>> + Send;
    fn find_prepared_for_revision(
        &self,
        revision_id: ItemRevisionId,
    ) -> impl Future<Output = Result<Option<Convergence>, RepositoryError>> + Send;
    fn list_by_item(
        &self,
        item_id: ItemId,
    ) -> impl Future<Output = Result<Vec<Convergence>, RepositoryError>> + Send;
    fn list_active(&self)
    -> impl Future<Output = Result<Vec<Convergence>, RepositoryError>> + Send;
}

pub trait FindingRepository: Send + Sync {
    fn list_by_item(
        &self,
        item_id: ItemId,
    ) -> impl Future<Output = Result<Vec<Finding>, RepositoryError>> + Send;
    fn get(&self, id: FindingId) -> impl Future<Output = Result<Finding, RepositoryError>> + Send;
    fn create(&self, finding: &Finding)
    -> impl Future<Output = Result<(), RepositoryError>> + Send;
    fn update(&self, finding: &Finding)
    -> impl Future<Output = Result<(), RepositoryError>> + Send;
    fn find_by_source(
        &self,
        job_id: JobId,
        source_finding_key: &str,
    ) -> impl Future<Output = Result<Option<Finding>, RepositoryError>> + Send;
    fn triage(&self, finding: &Finding)
    -> impl Future<Output = Result<(), RepositoryError>> + Send;
    fn triage_with_origin_detached(
        &self,
        finding: &Finding,
        detached_item_id: Option<ItemId>,
    ) -> impl Future<Output = Result<(), RepositoryError>> + Send;
    fn link_backlog(
        &self,
        finding: &Finding,
        linked_item: &Item,
        linked_revision: &ItemRevision,
        detached_item_id: Option<ItemId>,
    ) -> impl Future<Output = Result<(), RepositoryError>> + Send;
}

pub trait GitOperationRepository: Send + Sync {
    fn create(
        &self,
        operation: &GitOperation,
    ) -> impl Future<Output = Result<(), RepositoryError>> + Send;
    fn update(
        &self,
        operation: &GitOperation,
    ) -> impl Future<Output = Result<(), RepositoryError>> + Send;
    fn find_unresolved(
        &self,
    ) -> impl Future<Output = Result<Vec<GitOperation>, RepositoryError>> + Send;
}

pub trait ActivityRepository: Send + Sync {
    fn append(
        &self,
        activity: &Activity,
    ) -> impl Future<Output = Result<(), RepositoryError>> + Send;
    fn list_by_project(
        &self,
        project_id: ProjectId,
        limit: u32,
        offset: u32,
    ) -> impl Future<Output = Result<Vec<Activity>, RepositoryError>> + Send;
}

pub trait ConvergenceQueueRepository: Send + Sync {
    fn list_by_item(
        &self,
        item_id: ItemId,
    ) -> impl Future<Output = Result<Vec<ConvergenceQueueEntry>, RepositoryError>> + Send;
    fn get(
        &self,
        id: ConvergenceQueueEntryId,
    ) -> impl Future<Output = Result<ConvergenceQueueEntry, RepositoryError>> + Send;
    fn find_active_for_revision(
        &self,
        revision_id: ItemRevisionId,
    ) -> impl Future<Output = Result<Option<ConvergenceQueueEntry>, RepositoryError>> + Send;
    fn find_head(
        &self,
        project_id: ProjectId,
        target_ref: &str,
    ) -> impl Future<Output = Result<Option<ConvergenceQueueEntry>, RepositoryError>> + Send;
    fn find_next_queued(
        &self,
        project_id: ProjectId,
        target_ref: &str,
    ) -> impl Future<Output = Result<Option<ConvergenceQueueEntry>, RepositoryError>> + Send;
    fn list_active_by_project(
        &self,
        project_id: ProjectId,
    ) -> impl Future<Output = Result<Vec<ConvergenceQueueEntry>, RepositoryError>> + Send;
    fn list_active_for_lane(
        &self,
        project_id: ProjectId,
        target_ref: &str,
    ) -> impl Future<Output = Result<Vec<ConvergenceQueueEntry>, RepositoryError>> + Send;
    fn create(
        &self,
        entry: &ConvergenceQueueEntry,
    ) -> impl Future<Output = Result<(), RepositoryError>> + Send;
    fn update(
        &self,
        entry: &ConvergenceQueueEntry,
    ) -> impl Future<Output = Result<(), RepositoryError>> + Send;
}

// --- Job execution parameter types (owned, no lifetimes) ---

pub struct StartJobExecutionParams {
    pub job_id: JobId,
    pub item_id: ItemId,
    pub expected_item_revision_id: ItemRevisionId,
    pub workspace_id: Option<WorkspaceId>,
    pub agent_id: Option<AgentId>,
    pub lease_owner_id: String,
    pub process_pid: Option<u32>,
    pub lease_expires_at: DateTime<Utc>,
}

pub struct FinishJobNonSuccessParams {
    pub job_id: JobId,
    pub item_id: ItemId,
    pub expected_item_revision_id: ItemRevisionId,
    pub status: JobStatus,
    pub outcome_class: Option<OutcomeClass>,
    pub error_code: Option<String>,
    pub error_message: Option<String>,
    pub escalation_reason: Option<EscalationReason>,
}

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
    pub target_ref: String,
    pub expected_target_head_oid: String,
    pub next_approval_state: Option<ApprovalState>,
}

#[derive(Debug, Clone)]
pub struct JobCompletionMutation {
    pub job_id: JobId,
    pub item_id: ItemId,
    pub expected_item_revision_id: ItemRevisionId,
    pub outcome_class: OutcomeClass,
    pub clear_item_escalation: bool,
    pub result_schema_version: Option<String>,
    pub result_payload: Option<serde_json::Value>,
    pub output_commit_oid: Option<String>,
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

// --- Convergence service ports ---

#[derive(Debug, Clone)]
pub struct ConvergenceQueuePrepareContext {
    pub project: Project,
    pub item: Item,
    pub revision: ItemRevision,
    pub jobs: Vec<Job>,
    pub findings: Vec<Finding>,
    pub convergences: Vec<Convergence>,
    pub active_queue_entry: Option<ConvergenceQueueEntry>,
    pub lane_head: Option<ConvergenceQueueEntry>,
}

#[derive(Debug, Clone)]
pub struct ConvergenceSystemActionContext {
    pub project: Project,
    pub item: Item,
    pub revision: ItemRevision,
    pub jobs: Vec<Job>,
    pub findings: Vec<Finding>,
    pub convergences: Vec<Convergence>,
    pub active_queue_entry: Option<ConvergenceQueueEntry>,
}

#[derive(Debug, thiserror::Error)]
pub enum UseCasePortError {
    #[error("repository error: {0}")]
    Repository(#[from] RepositoryError),
    #[error("external error: {0}")]
    External(String),
}

pub trait ConvergenceServicePort: Send + Sync {
    fn queue_prepare_context(
        &self,
        project_id: ProjectId,
        item_id: ItemId,
    ) -> impl Future<Output = Result<ConvergenceQueuePrepareContext, UseCasePortError>> + Send;

    fn create_queue_entry(
        &self,
        queue_entry: &ConvergenceQueueEntry,
    ) -> impl Future<Output = Result<(), UseCasePortError>> + Send;

    fn update_queue_entry(
        &self,
        queue_entry: &ConvergenceQueueEntry,
    ) -> impl Future<Output = Result<(), UseCasePortError>> + Send;

    fn append_activity(
        &self,
        activity: &Activity,
    ) -> impl Future<Output = Result<(), UseCasePortError>> + Send;

    fn list_projects(&self) -> impl Future<Output = Result<Vec<Project>, UseCasePortError>> + Send;

    fn list_items_by_project(
        &self,
        project_id: ProjectId,
    ) -> impl Future<Output = Result<Vec<Item>, UseCasePortError>> + Send;

    fn load_system_action_context(
        &self,
        project_id: ProjectId,
        item_id: ItemId,
    ) -> impl Future<Output = Result<ConvergenceSystemActionContext, UseCasePortError>> + Send;

    fn promote_queue_heads(
        &self,
        project_id: ProjectId,
    ) -> impl Future<Output = Result<bool, UseCasePortError>> + Send;

    fn prepare_queue_head_convergence(
        &self,
        project_id: ProjectId,
        item_id: ItemId,
    ) -> impl Future<Output = Result<(), UseCasePortError>> + Send;

    fn auto_finalize_prepared_convergence(
        &self,
        project_id: ProjectId,
        item_id: ItemId,
    ) -> impl Future<Output = Result<(), UseCasePortError>> + Send;

    fn invalidate_prepared_convergence(
        &self,
        project_id: ProjectId,
        item_id: ItemId,
    ) -> impl Future<Output = Result<(), UseCasePortError>> + Send;
}

pub trait ReconciliationServicePort: Send + Sync {
    fn reconcile_git_operations(
        &self,
    ) -> impl Future<Output = Result<bool, UseCasePortError>> + Send;

    fn reconcile_active_jobs(&self) -> impl Future<Output = Result<(), UseCasePortError>> + Send;

    fn reconcile_active_convergences(
        &self,
    ) -> impl Future<Output = Result<(), UseCasePortError>> + Send;

    fn reconcile_workspace_retention(
        &self,
    ) -> impl Future<Output = Result<(), UseCasePortError>> + Send;
}

pub trait ProjectMutationLockPort: Send + Sync {
    type Guard: Send;

    fn acquire_project_mutation(
        &self,
        project_id: ProjectId,
    ) -> impl Future<Output = Self::Guard> + Send;
}

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
        commit_oid: &str,
    ) -> impl Future<Output = Result<bool, GitPortError>> + Send;

    fn verify_and_hold_target_ref(
        &self,
        repo_path: &Path,
        target_ref: &str,
        expected_oid: &str,
    ) -> impl Future<Output = Result<Self::Hold, TargetRefHoldError>> + Send;

    fn release_hold(
        &self,
        hold: Self::Hold,
    ) -> impl Future<Output = Result<(), GitPortError>> + Send;
}

#[derive(Debug, thiserror::Error)]
pub enum RepositoryError {
    #[error("entity not found")]
    NotFound,
    #[error("conflict: {0}")]
    Conflict(String),
    #[error("database error: {0}")]
    Database(#[from] Box<dyn std::error::Error + Send + Sync>),
}
