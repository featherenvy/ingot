use std::future::Future;

use crate::ids::ProjectId;

pub trait ProjectMutationLockPort: Send + Sync {
    type Guard: Send;

    fn acquire_project_mutation(
        &self,
        project_id: ProjectId,
    ) -> impl Future<Output = Self::Guard> + Send;
}
