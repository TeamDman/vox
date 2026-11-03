//! Request identity and pre-admission reservation foundation.
//!
//! This module is not connected to the outbound path yet. In particular it
//! is not used to authorize transport writes or lane reuse. See
//! `docs/design/response-reservation-2026-11-03.md`.

use super::{LaneLifetime, lane_retirement::LaneWriteLease};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use vox_types::{MethodId, RequestId};

/// One lane lifetime. Construct a fresh book for every new lane, even if a
/// numeric lane ID is reused; old capabilities keep referring to the old book.
#[derive(Clone)]
pub(crate) struct RequestBook {
    inner: Arc<Mutex<BookState>>,
    lane: Arc<LaneLifetime>,
}

impl Default for RequestBook {
    fn default() -> Self {
        Self::for_lane(Arc::new(LaneLifetime::new()))
    }
}

#[derive(Default)]
struct BookState {
    closed: bool,
    entries: HashMap<RequestId, RequestEntry>,
}

struct RequestEntry {
    identity: Arc<()>,
    method: MethodId,
    reservation: Option<ReservationState>,
}

enum ReservationState {
    Reserved(Arc<()>),
    Queued(Arc<()>),
    Writing(Arc<()>),
    Uncertain(Arc<()>),
}

/// Identity captured when an incoming request is admitted, not obtained by a
/// later numeric-ID lookup. It must eventually travel through all reply paths.
#[derive(Clone)]
pub(crate) struct RequestCapability {
    book: Arc<Mutex<BookState>>,
    id: RequestId,
    identity: Arc<()>,
    lane: Arc<LaneLifetime>,
}

impl RequestCapability {
    pub(crate) fn request_id(&self) -> RequestId {
        self.id
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ReservationError {
    Closed,
    DuplicateRequest,
    StaleRequest,
    AlreadyReserved,
    LaneUnavailable,
}

/// A reservation before queue admission. Dropping it releases only this exact
/// reservation, and never revives a retired request or closed lane.
pub(crate) struct ResponseReservation {
    request: RequestCapability,
    identity: Arc<()>,
    committed: bool,
}

/// Owned by exactly one accepted batch. A drop is conservative: it keeps the
/// request terminal because queue discard and partial write are not yet known.
pub(super) struct QueuedResponse {
    request: RequestCapability,
    identity: Arc<()>,
    finished: bool,
}

impl RequestBook {
    pub(crate) fn for_lane(lane: Arc<LaneLifetime>) -> Self {
        Self {
            inner: Arc::new(Mutex::new(BookState::default())),
            lane,
        }
    }
    pub(crate) fn record(
        &self,
        id: RequestId,
        method: MethodId,
    ) -> Result<RequestCapability, ReservationError> {
        let mut state = self
            .inner
            .lock()
            .map_err(|_| ReservationError::LaneUnavailable)?;
        if state.closed {
            return Err(ReservationError::Closed);
        }
        if state.entries.contains_key(&id) {
            return Err(ReservationError::DuplicateRequest);
        }
        let identity = Arc::new(());
        state.entries.insert(
            id,
            RequestEntry {
                identity: Arc::clone(&identity),
                method,
                reservation: None,
            },
        );
        Ok(RequestCapability {
            book: Arc::clone(&self.inner),
            id,
            identity,
            lane: self.lane.clone(),
        })
    }

    pub(crate) fn reserve(
        &self,
        request: &RequestCapability,
    ) -> Result<(MethodId, ResponseReservation), ReservationError> {
        if !Arc::ptr_eq(&self.inner, &request.book) {
            return Err(ReservationError::StaleRequest);
        }
        let mut state = self
            .inner
            .lock()
            .map_err(|_| ReservationError::LaneUnavailable)?;
        if state.closed {
            return Err(ReservationError::Closed);
        }
        let entry = state
            .entries
            .get_mut(&request.id)
            .ok_or(ReservationError::StaleRequest)?;
        if !Arc::ptr_eq(&entry.identity, &request.identity) {
            return Err(ReservationError::StaleRequest);
        }
        if entry.reservation.is_some() {
            return Err(ReservationError::AlreadyReserved);
        }
        let identity = Arc::new(());
        entry.reservation = Some(ReservationState::Reserved(Arc::clone(&identity)));
        Ok((
            entry.method,
            ResponseReservation {
                request: request.clone(),
                identity,
                committed: false,
            },
        ))
    }

    /// Remove a request only after its lifecycle owner has established that no
    /// reservation remains. Queued/writing/uncertain states remain reserved.
    pub(super) fn retire_unreserved(
        &self,
        request: &RequestCapability,
    ) -> Result<(), ReservationError> {
        if !Arc::ptr_eq(&self.inner, &request.book) {
            return Err(ReservationError::StaleRequest);
        }
        let mut state = self
            .inner
            .lock()
            .map_err(|_| ReservationError::LaneUnavailable)?;
        if state.closed {
            return Err(ReservationError::Closed);
        }
        let entry = state
            .entries
            .get(&request.id)
            .ok_or(ReservationError::StaleRequest)?;
        if !Arc::ptr_eq(&entry.identity, &request.identity) {
            return Err(ReservationError::StaleRequest);
        }
        if entry.reservation.is_some() {
            return Err(ReservationError::AlreadyReserved);
        }
        state.entries.remove(&request.id);
        Ok(())
    }

    pub(super) fn close(&self) -> Result<(), ReservationError> {
        // Never wait indefinitely for either synchronous lock. Busy or poison
        // requires the caller to terminate/quarantine, not claim closure.
        if !matches!(
            self.lane.try_begin_retirement(),
            Ok(super::lane_retirement::LanePhase::Retired)
        ) {
            return Err(ReservationError::LaneUnavailable);
        }
        let mut state = self
            .inner
            .try_lock()
            .map_err(|_| ReservationError::LaneUnavailable)?;
        state.closed = true;
        state.entries.clear();
        Ok(())
    }
}

impl Drop for ResponseReservation {
    fn drop(&mut self) {
        if self.committed {
            return;
        }
        // A poisoned book is not safe to reopen. Avoid a second panic during
        // unwinding and leave the lane fail-closed for its owner to terminate.
        let Ok(mut state) = self.request.book.lock() else {
            return;
        };
        if state.closed {
            return;
        }
        let Some(entry) = state.entries.get_mut(&self.request.id) else {
            return;
        };
        if Arc::ptr_eq(&entry.identity, &self.request.identity)
            && matches!(&entry.reservation, Some(ReservationState::Reserved(marker)) if Arc::ptr_eq(marker, &self.identity))
        {
            entry.reservation = None;
        }
    }
}

impl ResponseReservation {
    pub(super) fn belongs_to_lane(&self, lane: &Arc<LaneLifetime>) -> bool {
        Arc::ptr_eq(&self.request.lane, lane)
    }

    /// Transfer the reservation and an already-reserved queue permit in the
    /// lane admission critical section. `enqueue` must not await or block and
    /// must consume the batch. A panic quarantines the lane; dropping the batch
    /// conservatively leaves the request uncertain.
    pub(super) fn commit_with(
        mut self,
        enqueue: impl FnOnce(QueuedResponse),
    ) -> Result<(), ReservationError> {
        let lane = self.request.lane.clone();
        lane.commit_admission(|| {
            let mut state = self.request.book.lock().map_err(|_| ReservationError::LaneUnavailable)?;
            if state.closed {
                return Err(ReservationError::Closed);
            }
            let entry = state.entries.get_mut(&self.request.id).ok_or(ReservationError::StaleRequest)?;
            if !Arc::ptr_eq(&entry.identity, &self.request.identity)
                || !matches!(&entry.reservation, Some(ReservationState::Reserved(marker)) if Arc::ptr_eq(marker, &self.identity))
            {
                return Err(ReservationError::StaleRequest);
            }
            entry.reservation = Some(ReservationState::Queued(self.identity.clone()));
            self.committed = true;
            let queued = QueuedResponse {
                request: self.request.clone(),
                identity: self.identity.clone(),
                finished: false,
            };
            drop(state);
            enqueue(queued);
            Ok(())
        })
        .map_err(|_| ReservationError::LaneUnavailable)?
    }
}

impl QueuedResponse {
    /// The queued guard may only accompany the response for its original ID
    /// and method. `begin_write` still performs the lease and identity checks.
    pub(super) fn matches_request(&self, id: RequestId, method: MethodId) -> bool {
        if self.request.id != id {
            return false;
        }
        let Ok(state) = self.request.book.lock() else {
            return false;
        };
        if state.closed {
            return false;
        }
        state.entries.get(&id).is_some_and(|entry| {
            Arc::ptr_eq(&entry.identity, &self.request.identity)
                && entry.method == method
                && matches!(&entry.reservation, Some(ReservationState::Queued(marker)) if Arc::ptr_eq(marker, &self.identity))
        })
    }

    /// Validate the exact reservation before a write. A lane write lease must
    /// also be held by the real worker; this method alone cannot authorize I/O.
    pub(super) fn begin_write(&self, lease: &LaneWriteLease) -> Result<(), ReservationError> {
        if !lease.claim_response(&self.request.lane) {
            return Err(ReservationError::LaneUnavailable);
        }
        let mut state = self
            .request
            .book
            .lock()
            .map_err(|_| ReservationError::LaneUnavailable)?;
        if state.closed {
            return Err(ReservationError::Closed);
        }
        let entry = state
            .entries
            .get_mut(&self.request.id)
            .ok_or(ReservationError::StaleRequest)?;
        if !Arc::ptr_eq(&entry.identity, &self.request.identity)
            || !matches!(&entry.reservation, Some(ReservationState::Queued(marker)) if Arc::ptr_eq(marker, &self.identity))
        {
            return Err(ReservationError::StaleRequest);
        }
        entry.reservation = Some(ReservationState::Writing(self.identity.clone()));
        Ok(())
    }

    /// Success means a locally settled write, not peer receipt. The caller must
    /// establish transport settlement and hold the lane write lease first.
    pub(super) fn settle(
        mut self,
        lease: &LaneWriteLease,
        success: bool,
    ) -> Result<(), ReservationError> {
        if !lease.belongs_to(&self.request.lane) {
            return Err(ReservationError::LaneUnavailable);
        }
        let mut state = self
            .request
            .book
            .lock()
            .map_err(|_| ReservationError::LaneUnavailable)?;
        if state.closed {
            return Err(ReservationError::Closed);
        }
        let entry = state
            .entries
            .get_mut(&self.request.id)
            .ok_or(ReservationError::StaleRequest)?;
        if !Arc::ptr_eq(&entry.identity, &self.request.identity)
            || !matches!(&entry.reservation, Some(ReservationState::Writing(marker)) if Arc::ptr_eq(marker, &self.identity))
        {
            return Err(ReservationError::StaleRequest);
        }
        if success {
            state.entries.remove(&self.request.id);
        } else {
            entry.reservation = Some(ReservationState::Uncertain(self.identity.clone()));
        }
        self.finished = true;
        Ok(())
    }
}

impl Drop for QueuedResponse {
    fn drop(&mut self) {
        if self.finished {
            return;
        }
        let Ok(mut state) = self.request.book.lock() else {
            return;
        };
        let Some(entry) = state.entries.get_mut(&self.request.id) else {
            return;
        };
        if !Arc::ptr_eq(&entry.identity, &self.request.identity) {
            return;
        }
        let same = match &entry.reservation {
            Some(
                ReservationState::Queued(marker)
                | ReservationState::Writing(marker)
                | ReservationState::Uncertain(marker),
            ) => Arc::ptr_eq(marker, &self.identity),
            _ => false,
        };
        if same {
            entry.reservation = Some(ReservationState::Uncertain(self.identity.clone()));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Barrier;

    #[test]
    fn only_one_concurrent_reservation_wins_until_dropped() {
        let book = RequestBook::default();
        let request = book.record(RequestId(3), MethodId(7)).unwrap();
        let start = Arc::new(Barrier::new(3));
        let held = Arc::new(Barrier::new(3));
        let mut threads = Vec::new();
        for _ in 0..2 {
            let (book, request, start, held) =
                (book.clone(), request.clone(), start.clone(), held.clone());
            threads.push(std::thread::spawn(move || {
                start.wait();
                let reservation = book.reserve(&request);
                let won = reservation.is_ok();
                held.wait();
                drop(reservation);
                won
            }));
        }
        start.wait();
        held.wait();
        let winners = threads
            .into_iter()
            .map(|thread| thread.join().unwrap())
            .filter(|won| *won)
            .count();
        assert_eq!(winners, 1);
        assert!(book.reserve(&request).is_ok());
    }

    #[test]
    fn numeric_id_reuse_does_not_revalidate_old_capabilities() {
        let book = RequestBook::default();
        let old = book.record(RequestId(3), MethodId(7)).unwrap();
        assert!(matches!(
            book.record(RequestId(3), MethodId(7)),
            Err(ReservationError::DuplicateRequest)
        ));
        book.retire_unreserved(&old).unwrap();
        let same_method = book.record(RequestId(3), MethodId(7)).unwrap();
        assert!(matches!(
            book.reserve(&old),
            Err(ReservationError::StaleRequest)
        ));
        book.retire_unreserved(&same_method).unwrap();
        let other_method = book.record(RequestId(3), MethodId(9)).unwrap();
        assert!(matches!(
            book.reserve(&same_method),
            Err(ReservationError::StaleRequest)
        ));
        assert_eq!(book.reserve(&other_method).ok().unwrap().0, MethodId(9));
    }

    #[test]
    fn abandoned_reservation_releases_only_its_own_live_request() {
        let book = RequestBook::default();
        let request = book.record(RequestId(3), MethodId(7)).unwrap();
        let (_, held) = book.reserve(&request).ok().unwrap();
        assert!(matches!(
            book.retire_unreserved(&request),
            Err(ReservationError::AlreadyReserved)
        ));
        assert!(matches!(
            book.reserve(&request),
            Err(ReservationError::AlreadyReserved)
        ));
        drop(held);
        let (_, next) = book.reserve(&request).ok().unwrap();
        drop(next);
        book.retire_unreserved(&request).unwrap();
        assert!(matches!(
            book.reserve(&request),
            Err(ReservationError::StaleRequest)
        ));
    }

    #[test]
    fn poisoned_request_book_fails_closed_without_panicking() {
        let book = RequestBook::default();
        let request = book.record(RequestId(3), MethodId(7)).unwrap();
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _guard = book.inner.lock().unwrap();
            panic!("injected request book poison");
        }));
        assert!(matches!(
            book.record(RequestId(5), MethodId(7)),
            Err(ReservationError::LaneUnavailable)
        ));
        assert!(matches!(
            book.reserve(&request),
            Err(ReservationError::LaneUnavailable)
        ));
        assert!(matches!(
            book.retire_unreserved(&request),
            Err(ReservationError::LaneUnavailable)
        ));
        assert_eq!(book.close(), Err(ReservationError::LaneUnavailable));
    }

    #[test]
    fn closing_a_lane_does_not_reopen_it_or_authorize_another_lane() {
        let old_lane = RequestBook::default();
        let old = old_lane.record(RequestId(3), MethodId(7)).unwrap();
        let (_, held) = old_lane.reserve(&old).ok().unwrap();
        old_lane.close().unwrap();
        drop(held);
        assert!(matches!(
            old_lane.reserve(&old),
            Err(ReservationError::Closed)
        ));
        let replacement_lane = RequestBook::default();
        let new = replacement_lane.record(RequestId(3), MethodId(7)).unwrap();
        assert!(matches!(
            replacement_lane.reserve(&old),
            Err(ReservationError::StaleRequest)
        ));
        assert!(replacement_lane.reserve(&new).is_ok());
    }

    #[test]
    fn queue_transfer_keeps_identity_until_a_settled_write() {
        let book = RequestBook::default();
        let request = book.record(RequestId(3), MethodId(7)).unwrap();
        let (_, reservation) = book.reserve(&request).ok().unwrap();
        let mut batch = None;
        reservation
            .commit_with(|queued| batch = Some(queued))
            .unwrap();
        assert!(matches!(
            book.reserve(&request),
            Err(ReservationError::AlreadyReserved)
        ));
        let batch = batch.unwrap();
        assert!(batch.matches_request(RequestId(3), MethodId(7)));
        assert!(!batch.matches_request(RequestId(5), MethodId(7)));
        assert!(!batch.matches_request(RequestId(3), MethodId(9)));
        let wrong_lane = LaneLifetime::new();
        let wrong_lease = wrong_lane.acquire_write().ok().unwrap();
        assert_eq!(
            batch.begin_write(&wrong_lease),
            Err(ReservationError::LaneUnavailable)
        );
        let lease = book.lane.acquire_write().ok().unwrap();
        batch.begin_write(&lease).unwrap();
        assert_eq!(
            batch.begin_write(&lease),
            Err(ReservationError::LaneUnavailable)
        );
        batch.settle(&lease, true).unwrap();
        lease.settle(true).unwrap();
        let replacement = book.record(RequestId(3), MethodId(7)).unwrap();
        assert!(matches!(
            book.reserve(&request),
            Err(ReservationError::StaleRequest)
        ));
        assert!(book.reserve(&replacement).is_ok());
    }

    #[test]
    fn discarded_or_uncertain_queued_response_never_reopens_request() {
        for begun in [false, true] {
            let book = RequestBook::default();
            let request = book.record(RequestId(3), MethodId(7)).unwrap();
            let (_, reservation) = book.reserve(&request).ok().unwrap();
            let mut batch = None;
            reservation
                .commit_with(|queued| batch = Some(queued))
                .unwrap();
            let batch = batch.unwrap();
            if begun {
                let lease = book.lane.acquire_write().ok().unwrap();
                batch.begin_write(&lease).unwrap();
                batch.settle(&lease, false).unwrap();
                lease.settle(false).unwrap();
            } else {
                drop(batch);
            }
            assert!(matches!(
                book.reserve(&request),
                Err(ReservationError::AlreadyReserved)
            ));
            assert!(matches!(
                book.record(RequestId(3), MethodId(7)),
                Err(ReservationError::DuplicateRequest)
            ));
        }
    }

    #[test]
    fn one_write_lease_cannot_authorize_two_queued_responses() {
        let book = RequestBook::default();
        let first = book.record(RequestId(3), MethodId(7)).unwrap();
        let second = book.record(RequestId(5), MethodId(7)).unwrap();
        let (_, first_reservation) = book.reserve(&first).ok().unwrap();
        let (_, second_reservation) = book.reserve(&second).ok().unwrap();
        let mut first_batch = None;
        let mut second_batch = None;
        first_reservation
            .commit_with(|q| first_batch = Some(q))
            .unwrap();
        second_reservation
            .commit_with(|q| second_batch = Some(q))
            .unwrap();
        let first_batch = first_batch.unwrap();
        let second_batch = second_batch.unwrap();
        let lease = book.lane.acquire_write().ok().unwrap();
        first_batch.begin_write(&lease).unwrap();
        assert_eq!(
            second_batch.begin_write(&lease),
            Err(ReservationError::LaneUnavailable)
        );
        first_batch.settle(&lease, true).unwrap();
        lease.settle(true).unwrap();
        let next_lease = book.lane.acquire_write().ok().unwrap();
        second_batch.begin_write(&next_lease).unwrap();
        second_batch.settle(&next_lease, true).unwrap();
        next_lease.settle(true).unwrap();
    }

    #[test]
    fn quarantined_lane_rejects_an_existing_write_lease() {
        let book = RequestBook::default();
        let request = book.record(RequestId(3), MethodId(7)).unwrap();
        let (_, reservation) = book.reserve(&request).ok().unwrap();
        let mut batch = None;
        reservation.commit_with(|q| batch = Some(q)).unwrap();
        let lease = book.lane.acquire_write().ok().unwrap();
        let panic = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _ = book
                .lane
                .commit_admission(|| panic!("injected admission panic"));
        }));
        assert!(panic.is_err());
        assert_eq!(
            batch.unwrap().begin_write(&lease),
            Err(ReservationError::LaneUnavailable)
        );
    }

    #[test]
    fn busy_lane_retirement_fails_without_interrupting_queue_commit() {
        let book = RequestBook::default();
        let request = book.record(RequestId(3), MethodId(7)).unwrap();
        let (_, reservation) = book.reserve(&request).ok().unwrap();
        let (entered_tx, entered_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let committing = std::thread::spawn(move || {
            reservation.commit_with(|queued| {
                entered_tx.send(()).unwrap();
                release_rx.recv().unwrap();
                drop(queued);
            })
        });
        entered_rx.recv().unwrap();
        let other = book.clone();
        let (closed_tx, closed_rx) = std::sync::mpsc::channel();
        let closing = std::thread::spawn(move || {
            closed_tx.send(other.close()).unwrap();
        });
        let outcome = closed_rx.recv_timeout(std::time::Duration::from_secs(1));
        release_tx.send(()).unwrap();
        committing.join().unwrap().unwrap();
        closing.join().unwrap();
        assert_eq!(outcome.unwrap(), Err(ReservationError::LaneUnavailable));
        book.close().unwrap();
        assert!(matches!(
            book.reserve(&request),
            Err(ReservationError::Closed)
        ));
    }

    #[test]
    fn panic_during_enqueue_quarantines_lane_and_request() {
        let book = RequestBook::default();
        let request = book.record(RequestId(3), MethodId(7)).unwrap();
        let (_, reservation) = book.reserve(&request).ok().unwrap();
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _ = reservation.commit_with(|_queued| panic!("injected enqueue panic"));
        }));
        assert!(result.is_err());
        assert_eq!(
            book.lane.phase(),
            super::super::lane_retirement::LanePhase::Quarantined
        );
        assert!(matches!(
            book.reserve(&request),
            Err(ReservationError::AlreadyReserved)
        ));
    }
}
