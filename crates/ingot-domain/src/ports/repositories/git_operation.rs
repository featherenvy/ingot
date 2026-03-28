use std::future::Future;

use crate::git_operation::GitOperation;
use crate::git_ref::GitRef;
use crate::ids::ConvergenceId;

use super::super::errors::RepositoryError;

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
    fn find_unresolved_finalize_for_convergence(
        &self,
        convergence_id: ConvergenceId,
    ) -> impl Future<Output = Result<Option<GitOperation>, RepositoryError>> + Send;
    fn delete_investigation_ref_operations(
        &self,
        ref_name: &GitRef,
    ) -> impl Future<Output = Result<(), RepositoryError>> + Send;
}
