//! Independent, cancellation-aware wakeups for terminal state changes.
//!
//! A waiter registers before checking its persistent state. A transition wakes
//! every registered waiter after publishing that state. Cancelled waiters leave
//! only weak references, reclaimed at the next registration or wake.

use futures_util::task::AtomicWaker;
use std::sync::{Arc, Mutex, Weak};

#[derive(Default)]
pub(super) struct WakeRegistry {
    waiters: Mutex<Vec<Weak<AtomicWaker>>>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::task::{Wake, Waker};

    #[test]
    fn cancelled_waiters_are_reclaimed_and_wake_drains_registry() {
        let registry = WakeRegistry::default();
        let cancelled = registry.register();
        drop(cancelled);
        let active = registry.register();
        assert_eq!(registry.waiters.lock().unwrap().len(), 1);
        registry.wake_all();
        assert!(registry.waiters.lock().unwrap().is_empty());
        drop(active);
    }

    #[test]
    fn panicking_or_reentrant_waker_does_not_prevent_other_wakeups() {
        struct Panics;
        impl Wake for Panics {
            fn wake(self: Arc<Self>) {
                panic!("injected waker panic");
            }
        }
        struct Reenters {
            registry: Arc<WakeRegistry>,
            calls: Arc<AtomicUsize>,
        }
        impl Wake for Reenters {
            fn wake(self: Arc<Self>) {
                // Reentering must not deadlock on the registry lock. The new
                // weak reference is cleaned up by the next registration.
                drop(self.registry.register());
                self.calls.fetch_add(1, Ordering::SeqCst);
            }
        }
        let registry = Arc::new(WakeRegistry::default());
        let panicking = registry.register();
        let reentrant = registry.register();
        let calls = Arc::new(AtomicUsize::new(0));
        panicking.register(&Waker::from(Arc::new(Panics)));
        reentrant.register(&Waker::from(Arc::new(Reenters {
            registry: registry.clone(),
            calls: calls.clone(),
        })));
        registry.wake_all();
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }
}

impl WakeRegistry {
    pub(super) fn register(&self) -> Arc<AtomicWaker> {
        let waiter = Arc::new(AtomicWaker::new());
        let mut waiters = self.waiters.lock().unwrap_or_else(|e| e.into_inner());
        waiters.retain(|waiter| waiter.strong_count() != 0);
        waiters.push(Arc::downgrade(&waiter));
        waiter
    }

    pub(super) fn wake_all(&self) {
        let waiters = {
            let mut waiters = self.waiters.lock().unwrap_or_else(|e| e.into_inner());
            std::mem::take(&mut *waiters)
        };
        for waiter in waiters.into_iter().filter_map(|waiter| waiter.upgrade()) {
            // A panicking custom waker must not strand the other waiters or
            // panic again during destructor cleanup.
            let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| waiter.wake()));
        }
    }
}
