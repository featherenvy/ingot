use std::future::Future;

use crate::agent::Agent;
use crate::ids::*;
use crate::item::Item;
use crate::project::Project;
use crate::revision::ItemRevision;

use super::super::errors::RepositoryError;

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
