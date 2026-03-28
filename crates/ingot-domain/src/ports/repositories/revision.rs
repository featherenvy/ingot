use std::future::Future;

use crate::ids::{ItemId, ItemRevisionId};
use crate::revision::ItemRevision;
use crate::revision_context::RevisionContext;

use super::super::errors::RepositoryError;

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
