use std::future::Future;

use crate::UseCaseError;

pub trait ReconciliationPort: Send + Sync {
    fn reconcile_git_operations(&self) -> impl Future<Output = Result<bool, UseCaseError>> + Send;

    fn reconcile_active_jobs(&self) -> impl Future<Output = Result<bool, UseCaseError>> + Send;

    fn reconcile_active_convergences(
        &self,
    ) -> impl Future<Output = Result<bool, UseCaseError>> + Send;

    fn reconcile_workspace_retention(
        &self,
    ) -> impl Future<Output = Result<bool, UseCaseError>> + Send;
}

#[derive(Clone)]
pub struct ReconciliationService<P> {
    port: P,
}

impl<P> ReconciliationService<P> {
    pub fn new(port: P) -> Self {
        Self { port }
    }
}

impl<P> ReconciliationService<P>
where
    P: ReconciliationPort,
{
    pub async fn reconcile_startup(&self) -> Result<(), UseCaseError> {
        let _ = self.port.reconcile_git_operations().await?;
        let _ = self.port.reconcile_active_jobs().await?;
        let _ = self.port.reconcile_active_convergences().await?;
        let _ = self.port.reconcile_workspace_retention().await?;
        Ok(())
    }

    pub async fn tick_maintenance(&self) -> Result<bool, UseCaseError> {
        let mut made_progress = false;

        if self.port.reconcile_git_operations().await? {
            made_progress = true;
        }
        if self.port.reconcile_active_jobs().await? {
            made_progress = true;
        }
        if self.port.reconcile_active_convergences().await? {
            made_progress = true;
        }
        if self.port.reconcile_workspace_retention().await? {
            made_progress = true;
        }

        Ok(made_progress)
    }
}

#[cfg(test)]
mod tests {
    use std::future::ready;
    use std::sync::{Arc, Mutex};

    use super::{ReconciliationPort, ReconciliationService};
    use crate::UseCaseError;

    #[derive(Clone)]
    struct FakePort {
        responses: Arc<Mutex<Vec<bool>>>,
        calls: Arc<Mutex<Vec<&'static str>>>,
    }

    impl FakePort {
        fn new(responses: [bool; 4]) -> Self {
            Self {
                responses: Arc::new(Mutex::new(responses.into_iter().collect())),
                calls: Arc::new(Mutex::new(Vec::new())),
            }
        }

        fn take_next(&self, name: &'static str) -> bool {
            self.calls.lock().expect("calls lock").push(name);
            self.responses.lock().expect("responses lock").remove(0)
        }

        fn calls(&self) -> Vec<&'static str> {
            self.calls.lock().expect("calls lock").clone()
        }
    }

    impl ReconciliationPort for FakePort {
        fn reconcile_git_operations(
            &self,
        ) -> impl std::future::Future<Output = Result<bool, UseCaseError>> + Send {
            ready(Ok(self.take_next("git_operations")))
        }

        fn reconcile_active_jobs(
            &self,
        ) -> impl std::future::Future<Output = Result<bool, UseCaseError>> + Send {
            ready(Ok(self.take_next("active_jobs")))
        }

        fn reconcile_active_convergences(
            &self,
        ) -> impl std::future::Future<Output = Result<bool, UseCaseError>> + Send {
            ready(Ok(self.take_next("active_convergences")))
        }

        fn reconcile_workspace_retention(
            &self,
        ) -> impl std::future::Future<Output = Result<bool, UseCaseError>> + Send {
            ready(Ok(self.take_next("workspace_retention")))
        }
    }

    #[tokio::test]
    async fn startup_runs_all_stages_in_order() {
        let port = FakePort::new([false, false, false, false]);
        let service = ReconciliationService::new(port.clone());

        service
            .reconcile_startup()
            .await
            .expect("startup reconcile");

        assert_eq!(
            port.calls(),
            vec![
                "git_operations",
                "active_jobs",
                "active_convergences",
                "workspace_retention"
            ]
        );
    }

    #[tokio::test]
    async fn maintenance_reports_any_progress() {
        let port = FakePort::new([false, true, false, false]);
        let service = ReconciliationService::new(port);

        let made_progress = service.tick_maintenance().await.expect("maintenance tick");

        assert!(made_progress);
    }
}
