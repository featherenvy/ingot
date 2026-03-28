use std::future::Future;

use crate::convergence::Convergence;
use crate::convergence_queue::ConvergenceQueueEntry;
use crate::git_ref::GitRef;
use crate::ids::*;

use super::super::errors::RepositoryError;

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
        target_ref: &GitRef,
    ) -> impl Future<Output = Result<Option<ConvergenceQueueEntry>, RepositoryError>> + Send;
    fn find_next_queued(
        &self,
        project_id: ProjectId,
        target_ref: &GitRef,
    ) -> impl Future<Output = Result<Option<ConvergenceQueueEntry>, RepositoryError>> + Send;
    fn list_active_by_project(
        &self,
        project_id: ProjectId,
    ) -> impl Future<Output = Result<Vec<ConvergenceQueueEntry>, RepositoryError>> + Send;
    fn list_active_for_lane(
        &self,
        project_id: ProjectId,
        target_ref: &GitRef,
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
