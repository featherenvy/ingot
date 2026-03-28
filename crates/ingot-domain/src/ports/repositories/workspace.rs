use std::future::Future;

use crate::ids::*;
use crate::workspace::Workspace;

use super::super::errors::RepositoryError;

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
    fn delete(&self, id: WorkspaceId) -> impl Future<Output = Result<(), RepositoryError>> + Send;
}
