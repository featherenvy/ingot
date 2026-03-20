use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use tokio::sync::watch;

/// Hints that dispatcher-visible state may have changed.
///
/// The HTTP router applies a middleware layer that calls [`notify`](Self::notify)
/// after every successful non-GET response, so individual handlers do not need
/// to call it manually. Notifications are level-triggered hints, not work
/// tokens: the dispatcher drains all available work before awaiting a listener
/// alongside its fallback poll interval. Multiple notifications that arrive
/// during a single drain are harmlessly collapsed.
#[derive(Clone)]
pub struct DispatchNotify {
    inner: Arc<DispatchNotifyInner>,
}

struct DispatchNotifyInner {
    sender: watch::Sender<DispatchNotification>,
    next_generation: AtomicU64,
}

pub struct DispatchNotifyListener {
    receiver: watch::Receiver<DispatchNotification>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DispatchNotification {
    generation: u64,
    reason: Arc<str>,
}

impl DispatchNotification {
    fn new(generation: u64, reason: impl Into<Arc<str>>) -> Self {
        Self {
            generation,
            reason: reason.into(),
        }
    }

    pub fn generation(&self) -> u64 {
        self.generation
    }

    pub fn reason(&self) -> &str {
        self.reason.as_ref()
    }
}

impl DispatchNotify {
    pub fn new() -> Self {
        let (sender, _receiver) = watch::channel(DispatchNotification::new(0, "initial"));
        Self {
            inner: Arc::new(DispatchNotifyInner {
                sender,
                next_generation: AtomicU64::new(0),
            }),
        }
    }

    /// Signal the dispatcher that new work may be available.
    pub fn notify(&self) {
        self.notify_with_reason("unspecified");
    }

    /// Signal the dispatcher that new work may be available and include context.
    pub fn notify_with_reason(&self, reason: impl Into<Arc<str>>) {
        let generation = self
            .inner
            .next_generation
            .fetch_add(1, Ordering::Relaxed)
            .wrapping_add(1);
        let _ = self
            .inner
            .sender
            .send_replace(DispatchNotification::new(generation, reason));
    }

    /// Create an independent listener for notification wakeups.
    pub fn subscribe(&self) -> DispatchNotifyListener {
        DispatchNotifyListener {
            receiver: self.inner.sender.subscribe(),
        }
    }
}

impl DispatchNotifyListener {
    /// Wait until this listener observes the next notification generation.
    pub async fn notified(&mut self) -> DispatchNotification {
        let _ = self.receiver.changed().await;
        self.receiver.borrow_and_update().clone()
    }
}

impl Default for DispatchNotify {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::DispatchNotify;

    #[tokio::test]
    async fn listener_reports_generation_and_reason() {
        let notify = DispatchNotify::new();
        let mut listener = notify.subscribe();

        notify.notify_with_reason("http POST /api/demo-project");
        let notification = listener.notified().await;

        assert_eq!(notification.generation(), 1);
        assert_eq!(notification.reason(), "http POST /api/demo-project");
    }

    #[tokio::test]
    async fn notify_defaults_reason_to_unspecified() {
        let notify = DispatchNotify::new();
        let mut listener = notify.subscribe();

        notify.notify();
        let notification = listener.notified().await;

        assert_eq!(notification.generation(), 1);
        assert_eq!(notification.reason(), "unspecified");
    }
}
