use crate::JobDispatcher;

use std::sync::Arc;
use std::time::Duration;

pub(super) type PreSpawnPauseHook = PauseHook<PreSpawnPausePoint>;
pub(super) type AutoQueuePauseHook = PauseHook<AutoQueuePausePoint>;
pub(super) type ProjectedRecoveryPauseHook = PauseHook<ProjectedRecoveryPausePoint>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum PreSpawnPausePoint {
    AgentBeforeSpawn,
    HarnessBeforeSpawn,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum AutoQueuePausePoint {
    BeforeGuard,
    BeforeInsert,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ProjectedRecoveryPausePoint {
    BeforeGuard,
}

#[derive(Clone)]
pub(super) struct PauseHook<P> {
    point: P,
    state: Arc<PauseHookState>,
}

struct PauseHookState {
    entered: std::sync::Mutex<usize>,
    released: std::sync::Mutex<bool>,
    entered_notify: tokio::sync::Notify,
    release_notify: tokio::sync::Notify,
}

impl<P> PauseHook<P>
where
    P: Copy + Eq,
{
    pub(super) fn new(point: P) -> Self {
        Self {
            point,
            state: Arc::new(PauseHookState {
                entered: std::sync::Mutex::new(0),
                released: std::sync::Mutex::new(false),
                entered_notify: tokio::sync::Notify::new(),
                release_notify: tokio::sync::Notify::new(),
            }),
        }
    }

    async fn pause_if_matching(&self, point: P) {
        if self.point != point {
            return;
        }

        {
            let mut entered = self.state.entered.lock().expect("pause hook entered lock");
            *entered += 1;
        }
        self.state.entered_notify.notify_waiters();

        loop {
            if *self
                .state
                .released
                .lock()
                .expect("pause hook released lock")
            {
                return;
            }
            self.state.release_notify.notified().await;
        }
    }

    pub(super) async fn wait_until_entered(&self, expected: usize, timeout_duration: Duration) {
        tokio::time::timeout(timeout_duration, async {
            loop {
                if *self.state.entered.lock().expect("pause hook entered lock") >= expected {
                    return;
                }
                self.state.entered_notify.notified().await;
            }
        })
        .await
        .expect("timed out waiting for pre-spawn pause hook");
    }

    pub(super) fn release(&self) {
        *self
            .state
            .released
            .lock()
            .expect("pause hook released lock") = true;
        self.state.release_notify.notify_waiters();
    }
}

impl JobDispatcher {
    pub(super) async fn pause_before_pre_spawn_guard(&self, point: PreSpawnPausePoint) {
        if let Some(hook) = &self.pre_spawn_pause_hook {
            hook.pause_if_matching(point).await;
        }
    }

    pub(super) async fn pause_before_auto_queue_guard(&self) {
        if let Some(hook) = &self.auto_queue_pause_hook {
            hook.pause_if_matching(AutoQueuePausePoint::BeforeGuard)
                .await;
        }
    }

    pub(super) async fn pause_before_auto_queue_insert(&self) {
        if let Some(hook) = &self.auto_queue_pause_hook {
            hook.pause_if_matching(AutoQueuePausePoint::BeforeInsert)
                .await;
        }
    }

    pub(super) async fn pause_before_projected_recovery_guard(&self) {
        if let Some(hook) = &self.projected_recovery_pause_hook {
            hook.pause_if_matching(ProjectedRecoveryPausePoint::BeforeGuard)
                .await;
        }
    }
}
