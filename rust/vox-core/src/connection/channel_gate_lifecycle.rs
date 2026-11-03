//! A one-shot channel declaration outcome. Live sends use the persistent wake
//! mechanism; failure outcomes are not yet connected to their error paths.
//!
//! Owners must retain the gate for the lifetime of each sender and bind it to
//! the originating lane generation. A lookup that treats a reclaimed failed
//! gate as open would reintroduce undeclared channel sends.

use super::{LaneLifetime, wake_registry::WakeRegistry};
use std::sync::{Arc, Mutex};
use vox_rt::sync::watch;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum GateState {
    Pending,
    Open,
    Failed,
}

struct GateData {
    outcome: GateState,
    completing: bool,
}

pub(crate) struct ChannelDeclarationGate {
    _lane_generation: Arc<LaneLifetime>,
    state: Mutex<GateData>,
    changed_tx: watch::Sender<GateState>,
    _changed_rx: watch::Receiver<GateState>,
    waiters: WakeRegistry,
    #[cfg(test)]
    injected_notification_failures: std::sync::atomic::AtomicUsize,
    #[cfg(test)]
    after_notification: Mutex<Option<Box<dyn FnOnce() + Send>>>,
}

impl ChannelDeclarationGate {
    pub(crate) fn new(lane_generation: Arc<LaneLifetime>) -> Self {
        let (changed_tx, changed_rx) = watch::channel(GateState::Pending);
        Self {
            _lane_generation: lane_generation,
            state: Mutex::new(GateData {
                outcome: GateState::Pending,
                completing: false,
            }),
            changed_tx,
            _changed_rx: changed_rx,
            waiters: WakeRegistry::default(),
            #[cfg(test)]
            injected_notification_failures: std::sync::atomic::AtomicUsize::new(0),
            #[cfg(test)]
            after_notification: Mutex::new(None),
        }
    }

    /// Complete exactly once. Opening is allowed only after the declaring call
    /// is enqueued; cancellation, preparation failure and lane closure fail it.
    pub(crate) fn finish(&self, outcome: GateState) -> bool {
        if outcome == GateState::Pending {
            return false;
        }
        let accepted = match self.state.lock() {
            Ok(mut state) => {
                if state.outcome != GateState::Pending || state.completing {
                    return false;
                }
                // Publish no terminal outcome until the fallible primary
                // notification finishes. Otherwise another waiter could see
                // Open before a notification failure changes it to Failed.
                state.completing = true;
                true
            }
            Err(poisoned) => {
                // Never turn poisoned state into a successful declaration.
                let mut state = poisoned.into_inner();
                state.outcome = GateState::Failed;
                state.completing = true;
                false
            }
        };
        // Drop the state lock before notifying. Catch a notification panic so a
        // destructor invoked during unwinding cannot trigger a second panic.
        let notified = self.notify(if accepted { outcome } else { GateState::Failed });
        #[cfg(test)]
        {
            let hook = self.after_notification.lock().unwrap().take();
            if let Some(hook) = hook {
                hook();
            }
        }
        let published = if accepted && notified {
            outcome
        } else {
            GateState::Failed
        };
        match self.state.lock() {
            Ok(mut state) => {
                state.outcome = published;
                state.completing = false;
            }
            Err(poisoned) => {
                let mut state = poisoned.into_inner();
                state.outcome = GateState::Failed;
                state.completing = false;
            }
        }
        if published == GateState::Failed && !notified {
            let _ = self.notify(GateState::Failed);
        }
        self.waiters.wake_all();
        accepted && notified && self.state() == outcome
    }

    fn notify(&self, state: GateState) -> bool {
        #[cfg(test)]
        if self
            .injected_notification_failures
            .fetch_update(
                std::sync::atomic::Ordering::AcqRel,
                std::sync::atomic::Ordering::Acquire,
                |remaining| remaining.checked_sub(1),
            )
            .is_ok()
        {
            return false;
        }
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| self.changed_tx.send(state)))
            .is_ok_and(|result| result.is_ok())
    }

    pub(crate) fn state(&self) -> GateState {
        match self.state.lock() {
            Ok(state) => state.outcome,
            Err(_) => GateState::Failed,
        }
    }

    #[cfg(test)]
    pub(crate) fn poison_for_test(&self) {
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _locked = self.state.lock().unwrap();
            panic!("injected channel gate lock poison");
        }));
    }

    /// Register an independent wakeup before checking the persistent state.
    /// Cancellation drops the waiter; the registry keeps only a weak reference.
    pub(crate) async fn wait(&self) -> Result<(), ()> {
        let waiter = self.waiters.register();
        futures_util::future::poll_fn(|cx| {
            waiter.register(cx.waker());
            match self.state() {
                GateState::Open => std::task::Poll::Ready(Ok(())),
                GateState::Failed => std::task::Poll::Ready(Err(())),
                GateState::Pending => std::task::Poll::Pending,
            }
        })
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::time::Duration;

    #[tokio::test]
    async fn failure_wakes_all_waiters_and_late_waiters_cannot_send() {
        let gate = Arc::new(ChannelDeclarationGate::new(Arc::new(LaneLifetime::new())));
        let mut first = Box::pin(gate.wait());
        let mut second = Box::pin(gate.wait());
        assert!(matches!(
            futures_util::poll!(&mut first),
            std::task::Poll::Pending
        ));
        assert!(matches!(
            futures_util::poll!(&mut second),
            std::task::Poll::Pending
        ));
        assert!(gate.finish(GateState::Failed));
        assert!(!gate.finish(GateState::Open));
        assert!(
            tokio::time::timeout(Duration::from_secs(2), first)
                .await
                .unwrap()
                .is_err()
        );
        assert!(
            tokio::time::timeout(Duration::from_secs(2), second)
                .await
                .unwrap()
                .is_err()
        );
        assert!(gate.wait().await.is_err());
        assert_eq!(gate.state(), GateState::Failed);
    }

    #[tokio::test]
    async fn successful_declaration_opens_once_for_existing_and_late_waiters() {
        let gate = Arc::new(ChannelDeclarationGate::new(Arc::new(LaneLifetime::new())));
        let waiter = {
            let gate = gate.clone();
            tokio::spawn(async move { gate.wait().await })
        };
        assert!(gate.finish(GateState::Open));
        assert!(!gate.finish(GateState::Failed));
        assert!(
            tokio::time::timeout(Duration::from_secs(2), waiter)
                .await
                .unwrap()
                .unwrap()
                .is_ok()
        );
        assert!(gate.wait().await.is_ok());
    }

    #[tokio::test]
    async fn transition_between_check_and_wait_is_not_lost() {
        let gate = ChannelDeclarationGate::new(Arc::new(LaneLifetime::new()));
        let mut receiver = gate._changed_rx.clone();
        assert_eq!(*receiver.borrow(), GateState::Pending);
        assert!(gate.finish(GateState::Open));
        // No `changed` future existed at the instant of the transition.
        tokio::time::timeout(Duration::from_secs(2), receiver.changed())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(*receiver.borrow(), GateState::Open);
    }

    #[tokio::test]
    async fn poisoned_gate_cleanup_does_not_panic_or_open_and_wakes_waiter() {
        let gate = ChannelDeclarationGate::new(Arc::new(LaneLifetime::new()));
        let mut waiter = Box::pin(gate.wait());
        assert!(matches!(
            futures_util::poll!(&mut waiter),
            std::task::Poll::Pending
        ));
        gate.poison_for_test();
        let finished = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            gate.finish(GateState::Open)
        }));
        assert!(!finished.unwrap());
        assert_eq!(gate.state(), GateState::Failed);
        assert!(
            tokio::time::timeout(Duration::from_secs(2), waiter)
                .await
                .unwrap()
                .is_err()
        );
    }

    #[tokio::test]
    async fn repeated_notification_failure_wakes_parked_waiter() {
        let gate = ChannelDeclarationGate::new(Arc::new(LaneLifetime::new()));
        let mut waiter = Box::pin(gate.wait());
        let mut second = Box::pin(gate.wait());
        let mut cancelled = Box::pin(gate.wait());
        assert!(matches!(
            futures_util::poll!(&mut waiter),
            std::task::Poll::Pending
        ));
        assert!(matches!(
            futures_util::poll!(&mut second),
            std::task::Poll::Pending
        ));
        assert!(matches!(
            futures_util::poll!(&mut cancelled),
            std::task::Poll::Pending
        ));
        drop(cancelled);
        gate.injected_notification_failures
            .store(2, std::sync::atomic::Ordering::Release);
        assert!(!gate.finish(GateState::Failed));
        assert_eq!(gate.state(), GateState::Failed);
        let outcome = tokio::time::timeout(Duration::from_millis(50), &mut waiter).await;
        if outcome.is_err() {
            // Clean up the diagnostic without counting this artificial wake as success.
            gate.changed_tx.send(GateState::Failed).unwrap();
            assert!(
                tokio::time::timeout(Duration::from_secs(2), waiter)
                    .await
                    .unwrap()
                    .is_err()
            );
        }
        assert!(
            matches!(outcome, Ok(Err(()))),
            "parked waiter did not wake with failure after both notifications failed"
        );
        assert!(
            tokio::time::timeout(Duration::from_secs(2), second)
                .await
                .unwrap()
                .is_err()
        );
        assert!(gate.wait().await.is_err());
    }

    #[tokio::test]
    async fn failed_notification_cannot_publish_open_to_racing_waiter() {
        let gate = Arc::new(ChannelDeclarationGate::new(Arc::new(LaneLifetime::new())));
        let (started_tx, started_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        *gate.after_notification.lock().unwrap() = Some(Box::new(move || {
            started_tx.send(()).unwrap();
            release_rx.recv().unwrap();
        }));
        gate.injected_notification_failures
            .store(2, std::sync::atomic::Ordering::Release);
        let completing = gate.clone();
        let thread = std::thread::spawn(move || completing.finish(GateState::Open));
        started_rx.recv_timeout(Duration::from_secs(2)).unwrap();
        assert_eq!(gate.state(), GateState::Pending);
        let mut waiter = Box::pin(gate.wait());
        assert!(matches!(
            futures_util::poll!(&mut waiter),
            std::task::Poll::Pending
        ));
        assert!(!gate.finish(GateState::Open));
        release_tx.send(()).unwrap();
        assert!(!thread.join().unwrap());
        assert!(
            tokio::time::timeout(Duration::from_secs(2), waiter)
                .await
                .unwrap()
                .is_err()
        );
        assert_eq!(gate.state(), GateState::Failed);
    }
}
