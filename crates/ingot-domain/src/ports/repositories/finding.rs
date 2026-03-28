use std::future::Future;

use crate::finding::Finding;
use crate::ids::*;
use crate::item::Item;
use crate::revision::ItemRevision;

use super::super::errors::RepositoryError;

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
