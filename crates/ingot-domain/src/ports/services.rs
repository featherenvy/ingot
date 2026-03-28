use std::future::Future;

use crate::activity::Activity;
use crate::convergence::Convergence;
use crate::convergence_queue::ConvergenceQueueEntry;
use crate::finding::Finding;
use crate::ids::*;
use crate::item::Item;
use crate::job::Job;
use crate::project::Project;
use crate::revision::ItemRevision;

use super::errors::RepositoryError;

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
