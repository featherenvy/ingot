use std::sync::Arc;

use tokio::sync::Notify;

/// Hints that dispatcher-visible state may have changed.
///
/// The HTTP router applies a middleware layer that calls [`notify`](Self::notify)
/// after every successful non-GET response, so individual handlers do not need
/// to call it manually. Notifications are level-triggered hints, not work
/// tokens: the dispatcher drains all available work (looping while `tick()`
/// returns progress) before awaiting [`notified`](Self::notified) alongside
/// its fallback poll interval. Multiple notifications that arrive during a
/// single drain are harmlessly collapsed.
#[derive(Clone)]
pub struct DispatchNotify {
    inner: Arc<Notify>,
}

impl DispatchNotify {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Notify::new()),
        }
    }

    /// Signal the dispatcher that new work may be available.
    pub fn notify(&self) {
        self.inner.notify_one();
    }

    /// Wait until signalled. Returns immediately if a notification was stored
    /// since the last call.
    pub async fn notified(&self) {
        self.inner.notified().await;
    }
}

impl Default for DispatchNotify {
    fn default() -> Self {
        Self::new()
    }
}
