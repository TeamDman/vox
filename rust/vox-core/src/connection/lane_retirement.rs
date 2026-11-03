//! Lane admission/write retirement primitives under development.
//!
//! Admission uses this state, but write leases and full retirement are not yet
//! wired into connection traffic. The owner must retain quarantined lifetimes,
//! wake waiters, prevent numeric-ID reuse, and track actual transport writer
//! termination before this becomes a complete shutdown protocol.

use std::sync::{Arc, Mutex};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum LanePhase {
    Open,
    /// Admission is closed but previously queued batches may still write.
    Draining,
    Retiring,
    Retired,
    Quarantined,
}

#[derive(Debug)]
struct LaneState {
    phase: LanePhase,
    active_write: Option<Arc<()>>,
    /// A successful send on a transport that did not prove local settlement.
    /// The lane may continue, but retirement must quarantine its connection.
    unconfirmed_write: bool,
}

/// One generation of a numeric lane ID. Clones share its exact identity.
#[derive(Clone, Debug)]
pub(crate) struct LaneLifetime {
    state: Arc<Mutex<LaneState>>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum LaneWriteError {
    NotOpen,
    WriteAlreadyActive,
    Busy,
    Poisoned,
    Quarantined,
}

/// A lease must not be dropped merely because an async wrapper was cancelled:
/// its owner must retain it until the real transport write settles. An
/// unfinished drop conservatively quarantines the lane.
pub(super) struct LaneWriteLease {
    state: Arc<Mutex<LaneState>>,
    token: Arc<()>,
    settled: bool,
    response_claimed: std::sync::atomic::AtomicBool,
}

impl LaneLifetime {
    pub(crate) fn new() -> Self {
        Self {
            state: Arc::new(Mutex::new(LaneState {
                phase: LanePhase::Open,
                active_write: None,
                unconfirmed_write: false,
            })),
        }
    }

    /// Run the queue's *nonblocking* commit while holding the same state lock
    /// used by retirement. Queue capacity must already be reserved. There is no
    /// await or cancellation point inside this operation. If commit panics its
    /// outcome can be uncertain, so the lifetime is quarantined.
    pub(super) fn commit_admission<T>(
        &self,
        commit: impl FnOnce() -> T,
    ) -> Result<T, LaneWriteError> {
        let mut state = self.state.lock().map_err(|_| LaneWriteError::Poisoned)?;
        if state.phase == LanePhase::Quarantined {
            return Err(LaneWriteError::Quarantined);
        }
        if state.phase != LanePhase::Open {
            return Err(LaneWriteError::NotOpen);
        }
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(commit));
        match result {
            Ok(result) => Ok(result),
            Err(panic) => {
                state.phase = LanePhase::Quarantined;
                drop(state);
                std::panic::resume_unwind(panic)
            }
        }
    }

    /// Acquire a write lease atomically against retirement. The caller must
    /// also validate the matching request identity and actual transport owner.
    pub(super) fn acquire_write(&self) -> Result<LaneWriteLease, LaneWriteError> {
        let mut state = self.state.lock().map_err(|_| LaneWriteError::Poisoned)?;
        if state.phase == LanePhase::Quarantined {
            return Err(LaneWriteError::Quarantined);
        }
        if !matches!(state.phase, LanePhase::Open | LanePhase::Draining) {
            return Err(LaneWriteError::NotOpen);
        }
        if state.active_write.is_some() {
            return Err(LaneWriteError::WriteAlreadyActive);
        }
        let token = Arc::new(());
        state.active_write = Some(token.clone());
        Ok(LaneWriteLease {
            state: self.state.clone(),
            token,
            settled: false,
            response_claimed: std::sync::atomic::AtomicBool::new(false),
        })
    }

    /// Stop new admission. A returned `Retiring` means a real write still owns
    /// the lease; no numeric ID reuse or successful close may be inferred.
    /// This synchronous lock is not yet deadline-bounded; the connection must
    /// not treat this prototype as a complete shutdown implementation.
    pub(super) fn begin_retirement(&self) -> Result<LanePhase, LaneWriteError> {
        let mut state = self.state.lock().map_err(|_| LaneWriteError::Poisoned)?;
        Ok(Self::mark_retiring(&mut state))
    }

    /// Nonblocking building block for a future absolute-deadline coordinator.
    /// Busy is not retirement and must not permit ID reuse or claim teardown.
    pub(super) fn try_begin_retirement(&self) -> Result<LanePhase, LaneWriteError> {
        let mut state = match self.state.try_lock() {
            Ok(state) => state,
            Err(std::sync::TryLockError::WouldBlock) => return Err(LaneWriteError::Busy),
            Err(std::sync::TryLockError::Poisoned(_)) => return Err(LaneWriteError::Poisoned),
        };
        Ok(Self::mark_retiring(&mut state))
    }

    /// Close admission atomically against queue commit while keeping already
    /// admitted work eligible for the worker. The owner must fence the queue
    /// before calling `try_finish_drain` or sending LaneClose.
    pub(super) fn try_begin_drain(&self) -> Result<(), LaneWriteError> {
        let mut state = match self.state.try_lock() {
            Ok(state) => state,
            Err(std::sync::TryLockError::WouldBlock) => return Err(LaneWriteError::Busy),
            Err(std::sync::TryLockError::Poisoned(_)) => return Err(LaneWriteError::Poisoned),
        };
        if state.phase != LanePhase::Open {
            return Err(LaneWriteError::NotOpen);
        }
        if state.unconfirmed_write {
            state.phase = LanePhase::Quarantined;
            return Err(LaneWriteError::Quarantined);
        }
        state.phase = LanePhase::Draining;
        Ok(())
    }

    /// A worker fence must already have settled every earlier batch. Failure
    /// here remains uncertain and cannot authorize LaneClose or ID reuse.
    pub(super) fn try_finish_drain(&self) -> Result<(), LaneWriteError> {
        let mut state = match self.state.try_lock() {
            Ok(state) => state,
            Err(std::sync::TryLockError::WouldBlock) => return Err(LaneWriteError::Busy),
            Err(std::sync::TryLockError::Poisoned(_)) => return Err(LaneWriteError::Poisoned),
        };
        if state.phase != LanePhase::Draining {
            return Err(LaneWriteError::NotOpen);
        }
        if state.unconfirmed_write || state.active_write.is_some() {
            state.phase = LanePhase::Quarantined;
            return Err(LaneWriteError::Quarantined);
        }
        state.phase = LanePhase::Retired;
        Ok(())
    }

    fn mark_retiring(state: &mut LaneState) -> LanePhase {
        if matches!(state.phase, LanePhase::Open | LanePhase::Draining) {
            state.phase = LanePhase::Retiring;
        }
        if state.phase == LanePhase::Retiring && state.active_write.is_none() {
            state.phase = if state.unconfirmed_write {
                LanePhase::Quarantined
            } else {
                LanePhase::Retired
            };
        }
        state.phase
    }

    pub(super) fn phase(&self) -> LanePhase {
        match self.state.lock() {
            Ok(state) => state.phase,
            Err(_) => LanePhase::Quarantined,
        }
    }

    #[cfg(test)]
    pub(super) fn with_lock_for_test(&self, f: impl FnOnce()) {
        let _guard = self.state.lock().unwrap();
        f();
    }
}

impl LaneWriteLease {
    pub(super) fn belongs_to(&self, lane: &LaneLifetime) -> bool {
        if !Arc::ptr_eq(&self.state, &lane.state) || self.settled {
            return false;
        }
        let Ok(state) = self.state.lock() else {
            return false;
        };
        matches!(
            state.phase,
            LanePhase::Open | LanePhase::Draining | LanePhase::Retiring
        ) && state
            .active_write
            .as_ref()
            .is_some_and(|active| Arc::ptr_eq(active, &self.token))
    }

    /// A lease represents exactly one batch; it cannot authorize two replies.
    /// A failed claim stays consumed, conservatively preventing later reuse.
    pub(super) fn claim_response(&self, lane: &LaneLifetime) -> bool {
        if !Arc::ptr_eq(&self.state, &lane.state) || self.settled {
            return false;
        }
        let Ok(state) = self.state.lock() else {
            return false;
        };
        if !matches!(
            state.phase,
            LanePhase::Open | LanePhase::Draining | LanePhase::Retiring
        ) || !state
            .active_write
            .as_ref()
            .is_some_and(|active| Arc::ptr_eq(active, &self.token))
        {
            return false;
        }
        self.response_claimed
            .compare_exchange(
                false,
                true,
                std::sync::atomic::Ordering::AcqRel,
                std::sync::atomic::Ordering::Acquire,
            )
            .is_ok()
    }

    /// Record that the actual transport write settled. `false` includes any
    /// partial or uncertain outcome and never reopens the lane.
    pub(super) fn settle(mut self, success: bool) -> Result<LanePhase, LaneWriteError> {
        let mut state = self.state.lock().map_err(|_| LaneWriteError::Poisoned)?;
        let matches = state
            .active_write
            .as_ref()
            .is_some_and(|active| Arc::ptr_eq(active, &self.token));
        if !matches {
            return Err(LaneWriteError::NotOpen);
        }
        if success {
            state.active_write = None;
            if state.phase == LanePhase::Retiring {
                state.phase = if state.unconfirmed_write {
                    LanePhase::Quarantined
                } else {
                    LanePhase::Retired
                };
            }
        } else {
            state.phase = LanePhase::Quarantined;
        }
        self.settled = true;
        Ok(state.phase)
    }

    /// The transport returned success but cannot prove that its actual writer
    /// is finished. Keep this uncertainty sticky for the lane lifetime.
    pub(super) fn settle_unconfirmed(mut self) -> Result<LanePhase, LaneWriteError> {
        let mut state = self.state.lock().map_err(|_| LaneWriteError::Poisoned)?;
        if !state
            .active_write
            .as_ref()
            .is_some_and(|active| Arc::ptr_eq(active, &self.token))
        {
            return Err(LaneWriteError::NotOpen);
        }
        state.active_write = None;
        state.unconfirmed_write = true;
        if state.phase == LanePhase::Retiring {
            state.phase = LanePhase::Quarantined;
        }
        self.settled = true;
        Ok(state.phase)
    }
}

impl Drop for LaneWriteLease {
    fn drop(&mut self) {
        if self.settled {
            return;
        }
        let mut state = match self.state.lock() {
            Ok(state) => state,
            Err(poisoned) => poisoned.into_inner(),
        };
        if state
            .active_write
            .as_ref()
            .is_some_and(|active| Arc::ptr_eq(active, &self.token))
        {
            state.phase = LanePhase::Quarantined;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Barrier, mpsc};

    #[test]
    fn draining_rejects_new_admission_but_settles_previously_queued_work() {
        let lane = LaneLifetime::new();
        assert!(lane.commit_admission(|| ()).is_ok());
        lane.try_begin_drain().unwrap();
        assert!(matches!(
            lane.commit_admission(|| ()),
            Err(LaneWriteError::NotOpen)
        ));
        let lease = lane.acquire_write().unwrap();
        assert_eq!(lease.settle(true).unwrap(), LanePhase::Draining);
        lane.try_finish_drain().unwrap();
        assert_eq!(lane.phase(), LanePhase::Retired);
        assert!(matches!(lane.acquire_write(), Err(LaneWriteError::NotOpen)));
    }

    #[test]
    fn draining_keeps_an_existing_write_lease_until_it_settles() {
        let lane = LaneLifetime::new();
        let lease = lane.acquire_write().unwrap();
        lane.try_begin_drain().unwrap();
        assert!(matches!(
            lane.commit_admission(|| ()),
            Err(LaneWriteError::NotOpen)
        ));
        assert_eq!(lease.settle(true).unwrap(), LanePhase::Draining);
        lane.try_finish_drain().unwrap();
        assert_eq!(lane.phase(), LanePhase::Retired);
    }

    #[test]
    fn finishing_drain_with_a_live_write_quarantines_the_lane() {
        let lane = LaneLifetime::new();
        let _lease = lane.acquire_write().unwrap();
        lane.try_begin_drain().unwrap();
        assert!(matches!(
            lane.try_finish_drain(),
            Err(LaneWriteError::Quarantined)
        ));
        assert_eq!(lane.phase(), LanePhase::Quarantined);
    }

    #[test]
    fn admission_commit_and_retirement_use_the_same_lock() {
        let lane = LaneLifetime::new();
        let entered = Arc::new(Barrier::new(2));
        let (release_tx, release_rx) = mpsc::channel();
        let task_lane = lane.clone();
        let task_entered = entered.clone();
        let commit = std::thread::spawn(move || {
            task_lane.commit_admission(|| {
                task_entered.wait();
                release_rx.recv().unwrap();
                7
            })
        });
        entered.wait();
        let retiring_lane = lane.clone();
        let (result_tx, result_rx) = mpsc::channel();
        let (attempt_tx, attempt_rx) = mpsc::channel();
        let retiring = std::thread::spawn(move || {
            let _ = attempt_tx.send(());
            let _ = result_tx.send(retiring_lane.begin_retirement());
        });
        attempt_rx.recv().unwrap();
        assert!(matches!(
            result_rx.try_recv(),
            Err(mpsc::TryRecvError::Empty)
        ));
        release_tx.send(()).unwrap();
        assert_eq!(commit.join().unwrap().unwrap(), 7);
        assert_eq!(result_rx.recv().unwrap().unwrap(), LanePhase::Retired);
        retiring.join().unwrap();
        assert!(matches!(
            lane.commit_admission(|| 8),
            Err(LaneWriteError::NotOpen)
        ));
    }

    #[test]
    fn retirement_waits_for_exact_write_lease() {
        let lane = LaneLifetime::new();
        let lease = lane.acquire_write().ok().unwrap();
        assert!(matches!(
            lane.acquire_write(),
            Err(LaneWriteError::WriteAlreadyActive)
        ));
        assert_eq!(lane.begin_retirement().unwrap(), LanePhase::Retiring);
        assert!(matches!(lane.acquire_write(), Err(LaneWriteError::NotOpen)));
        assert_eq!(lease.settle(true).unwrap(), LanePhase::Retired);
    }

    #[test]
    fn nonblocking_retirement_reports_busy_without_claiming_closure() {
        let lane = LaneLifetime::new();
        let held = lane.state.lock().unwrap();
        assert_eq!(lane.try_begin_retirement(), Err(LaneWriteError::Busy));
        drop(held);
        assert_eq!(lane.try_begin_retirement().unwrap(), LanePhase::Retired);
    }

    #[test]
    fn dropped_or_failed_writes_quarantine_instead_of_reopening() {
        let lane = LaneLifetime::new();
        drop(lane.acquire_write().ok().unwrap());
        assert_eq!(lane.phase(), LanePhase::Quarantined);
        assert!(matches!(
            lane.commit_admission(|| ()),
            Err(LaneWriteError::Quarantined)
        ));
        let other = LaneLifetime::new();
        assert_eq!(
            other.acquire_write().ok().unwrap().settle(false).unwrap(),
            LanePhase::Quarantined
        );
    }

    #[test]
    fn unconfirmed_success_remains_sticky_until_lane_retirement() {
        let lane = LaneLifetime::new();
        let first = lane.acquire_write().ok().unwrap();
        assert_eq!(first.settle_unconfirmed().unwrap(), LanePhase::Open);
        // A still-open lane may continue, but later local success cannot erase
        // uncertainty from a previous unconfirmed transport write.
        let second = lane.acquire_write().ok().unwrap();
        assert_eq!(lane.begin_retirement().unwrap(), LanePhase::Retiring);
        assert_eq!(second.settle(true).unwrap(), LanePhase::Quarantined);
        assert!(matches!(
            lane.acquire_write(),
            Err(LaneWriteError::Quarantined)
        ));
    }

    #[test]
    fn unconfirmed_success_after_retirement_cannot_authorize_reuse() {
        let lane = LaneLifetime::new();
        let lease = lane.acquire_write().ok().unwrap();
        assert_eq!(lane.begin_retirement().unwrap(), LanePhase::Retiring);
        assert_eq!(lease.settle_unconfirmed().unwrap(), LanePhase::Quarantined);
    }

    #[test]
    fn panic_during_commit_quarantines_without_poisoning_the_lock() {
        let lane = LaneLifetime::new();
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _ = lane.commit_admission(|| panic!("injected enqueue panic"));
        }));
        assert!(result.is_err());
        assert_eq!(lane.phase(), LanePhase::Quarantined);
        assert!(matches!(
            lane.acquire_write(),
            Err(LaneWriteError::Quarantined)
        ));
    }
}
