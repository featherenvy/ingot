use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use tokio::sync::{Mutex, watch};

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
    sender: watch::Sender<u64>,
    // Compatibility path for the legacy single-waiter `DispatchNotify::notified()`
    // helper. Multi-waiter consumers must use `subscribe()`.
    fallback_receiver: Mutex<watch::Receiver<u64>>,
    next_generation: AtomicU64,
}

pub struct DispatchNotifyListener {
    receiver: watch::Receiver<u64>,
}

impl DispatchNotify {
    pub fn new() -> Self {
        let (sender, receiver) = watch::channel(0_u64);
        Self {
            inner: Arc::new(DispatchNotifyInner {
                sender,
                fallback_receiver: Mutex::new(receiver),
                next_generation: AtomicU64::new(0),
            }),
        }
    }

    /// Signal the dispatcher that new work may be available.
    pub fn notify(&self) {
        let generation = self
            .inner
            .next_generation
            .fetch_add(1, Ordering::Relaxed)
            .wrapping_add(1);
        let _ = self.inner.sender.send_replace(generation);
    }

    /// Create an independent listener for notification wakeups.
    pub fn subscribe(&self) -> DispatchNotifyListener {
        DispatchNotifyListener {
            receiver: self.inner.sender.subscribe(),
        }
    }

    /// Wait until signalled using a shared single-waiter listener.
    ///
    /// Multi-waiter consumers should call [`subscribe`](Self::subscribe) and
    /// await notifications on their own [`DispatchNotifyListener`].
    pub async fn notified(&self) {
        let mut receiver = self.inner.fallback_receiver.lock().await;
        let _ = receiver.changed().await;
    }
}

impl DispatchNotifyListener {
    /// Wait until this listener observes the next notification generation.
    pub async fn notified(&mut self) {
        let _ = self.receiver.changed().await;
    }
}

impl Default for DispatchNotify {
    fn default() -> Self {
        Self::new()
    }
}
