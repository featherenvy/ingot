use std::collections::HashMap;
use std::sync::Arc;

use ingot_domain::ids::ProjectId;
use ingot_domain::ports::ProjectMutationLockPort;
use tokio::sync::{Mutex, OwnedMutexGuard};

#[derive(Clone, Default)]
pub struct ProjectLocks {
    by_project: Arc<Mutex<HashMap<ProjectId, Arc<Mutex<()>>>>>,
}

impl ProjectLocks {
    pub async fn acquire(&self, project_id: ProjectId) -> OwnedMutexGuard<()> {
        let project_lock = {
            let mut locks = self.by_project.lock().await;
            locks
                .entry(project_id)
                .or_insert_with(|| Arc::new(Mutex::new(())))
                .clone()
        };

        project_lock.lock_owned().await
    }
}

impl ProjectMutationLockPort for ProjectLocks {
    type Guard = OwnedMutexGuard<()>;

    async fn acquire_project_mutation(&self, project_id: ProjectId) -> Self::Guard {
        self.acquire(project_id).await
    }
}
