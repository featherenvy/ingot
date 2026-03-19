use super::*;
use crate::bootstrap;

pub(super) async fn drain_until_idle<F, Fut>(mut step: F) -> Result<(), RuntimeError>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = Result<bool, RuntimeError>>,
{
    while step().await? {}
    Ok(())
}

impl JobDispatcher {
    pub async fn reconcile_startup(&self) -> Result<(), RuntimeError> {
        bootstrap::ensure_default_agent(&self.db).await?;
        ReconciliationService::new(RuntimeReconciliationPort {
            dispatcher: self.clone(),
        })
        .reconcile_startup()
        .await
        .map_err(usecase_to_runtime_error)?;
        drain_until_idle(|| self.tick_system_action()).await?;
        let _ = self.recover_projected_review_jobs().await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;
    use std::sync::{Arc, Mutex};

    #[tokio::test]
    async fn drain_until_idle_stops_after_first_idle_result() {
        let script = Arc::new(Mutex::new(VecDeque::from([Ok(false)])));
        let calls = Arc::new(Mutex::new(0usize));

        drain_until_idle({
            let script = Arc::clone(&script);
            let calls = Arc::clone(&calls);
            move || {
                *calls.lock().expect("calls lock") += 1;
                let next = script
                    .lock()
                    .expect("script lock")
                    .pop_front()
                    .expect("scripted result");
                std::future::ready(next)
            }
        })
        .await
        .expect("drain should stop");

        assert_eq!(*calls.lock().expect("calls lock"), 1);
        assert!(script.lock().expect("script lock").is_empty());
    }

    #[tokio::test]
    async fn drain_until_idle_retries_until_idle_result() {
        let script = Arc::new(Mutex::new(VecDeque::from([Ok(true), Ok(true), Ok(false)])));
        let calls = Arc::new(Mutex::new(0usize));

        drain_until_idle({
            let script = Arc::clone(&script);
            let calls = Arc::clone(&calls);
            move || {
                *calls.lock().expect("calls lock") += 1;
                let next = script
                    .lock()
                    .expect("script lock")
                    .pop_front()
                    .expect("scripted result");
                std::future::ready(next)
            }
        })
        .await
        .expect("drain should stop");

        assert_eq!(*calls.lock().expect("calls lock"), 3);
        assert!(script.lock().expect("script lock").is_empty());
    }

    #[tokio::test]
    async fn drain_until_idle_returns_first_error() {
        let script = Arc::new(Mutex::new(VecDeque::from([
            Ok(true),
            Err(RuntimeError::InvalidState("boom".into())),
        ])));
        let calls = Arc::new(Mutex::new(0usize));

        let error = drain_until_idle({
            let script = Arc::clone(&script);
            let calls = Arc::clone(&calls);
            move || {
                *calls.lock().expect("calls lock") += 1;
                let next = script
                    .lock()
                    .expect("script lock")
                    .pop_front()
                    .expect("scripted result");
                std::future::ready(next)
            }
        })
        .await
        .expect_err("drain should surface error");

        assert!(matches!(error, RuntimeError::InvalidState(message) if message == "boom"));
        assert_eq!(*calls.lock().expect("calls lock"), 2);
        assert!(script.lock().expect("script lock").is_empty());
    }
}
