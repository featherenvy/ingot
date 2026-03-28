use std::future::Future;

use crate::activity::Activity;
use crate::ids::ProjectId;

use super::super::errors::RepositoryError;

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
