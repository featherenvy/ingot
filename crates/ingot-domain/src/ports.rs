use std::future::Future;

use crate::activity::Activity;
use crate::agent::Agent;
use crate::convergence::Convergence;
use crate::git_operation::GitOperation;
use crate::ids::*;
use crate::item::Item;
use crate::job::Job;
use crate::project::Project;
use crate::revision::ItemRevision;
use crate::revision_context::RevisionContext;
use crate::workspace::Workspace;

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

#[derive(Debug, thiserror::Error)]
pub enum RepositoryError {
    #[error("entity not found")]
    NotFound,
    #[error("conflict: {0}")]
    Conflict(String),
    #[error("database error: {0}")]
    Database(#[from] Box<dyn std::error::Error + Send + Sync>),
}
