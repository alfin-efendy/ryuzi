//! Shared capacity gate for async delegation (spec §6.2). Counts in-flight
//! background delegations against the SAME `max_concurrent_runs` setting the
//! task-batch semaphore and orch dispatcher already enforce locally — it does
//! NOT introduce a second capacity SETTING. Also holds each in-flight
//! delegation's cancel token keyed by the dispatching (parent) session, so
//! `end_session` can interrupt orphaned work (spec §6.1).
//!
//! The shared `n` (`max_concurrent_runs`) is a capacity CAP, not one unified
//! global semaphore across sync-batch + orch + background: this registry's
//! own live counter gates only the background-worker population against `n`.
//! Capping background workers separately (rather than sharing one semaphore
//! object with the task-batch/orch paths) avoids a parent-holds-slot deadlock,
//! where a parent holding a sync-batch/orch slot would deadlock awaiting its
//! own background child for a slot from the same semaphore.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use tokio_util::sync::CancellationToken;

#[derive(Default)]
struct Inner {
    active: AtomicU64,
    /// parent_session_pk -> live tokens of delegations it dispatched.
    tokens: Mutex<HashMap<String, Vec<(u64, CancellationToken)>>>,
    next_id: AtomicU64,
}

#[derive(Clone, Default)]
pub struct BackgroundRegistry(Arc<Inner>);

impl BackgroundRegistry {
    pub fn new() -> Arc<Self> {
        Arc::new(BackgroundRegistry::default())
    }

    pub fn active(&self) -> usize {
        self.0.active.load(Ordering::SeqCst) as usize
    }

    /// Reserve one background slot if under `capacity`. The returned guard
    /// releases the slot (and deregisters its token) on drop.
    ///
    /// The check-then-reserve happens while holding `tokens`'s mutex, so two
    /// concurrent callers can never both observe a slot free and both admit
    /// past `capacity` (no TOCTOU). The lock is dropped before returning —
    /// never held across an `.await` — so this cannot deadlock callers.
    pub fn try_reserve(&self, capacity: usize, parent_session_pk: &str) -> Option<Reservation> {
        let mut map = self.0.tokens.lock().unwrap();
        if self.active() >= capacity {
            return None;
        }
        self.0.active.fetch_add(1, Ordering::SeqCst);
        let slot = self.0.next_id.fetch_add(1, Ordering::SeqCst);
        let token = CancellationToken::new();
        map.entry(parent_session_pk.to_string())
            .or_default()
            .push((slot, token.clone()));
        Some(Reservation {
            reg: self.clone(),
            parent: parent_session_pk.to_string(),
            slot,
            token,
        })
    }

    /// Cancel every in-flight delegation dispatched by `parent_session_pk`.
    /// A parent with no reservations is a harmless no-op.
    pub fn interrupt_for_session(&self, parent_session_pk: &str) {
        let map = self.0.tokens.lock().unwrap();
        if let Some(entries) = map.get(parent_session_pk) {
            for (_, tok) in entries {
                tok.cancel();
            }
        }
    }

    fn release(&self, parent: &str, slot: u64) {
        self.0.active.fetch_sub(1, Ordering::SeqCst);
        let mut map = self.0.tokens.lock().unwrap();
        if let Some(v) = map.get_mut(parent) {
            v.retain(|(s, _)| *s != slot);
            if v.is_empty() {
                map.remove(parent);
            }
        }
    }
}

/// A held background slot. Dropping it frees the slot and deregisters the
/// delegation's token, so a panicking or early-returning worker can never
/// leak a slot.
pub struct Reservation {
    reg: BackgroundRegistry,
    parent: String,
    slot: u64,
    token: CancellationToken,
}

impl Reservation {
    pub fn token(&self) -> CancellationToken {
        self.token.clone()
    }
}

impl Drop for Reservation {
    fn drop(&mut self) {
        self.reg.release(&self.parent, self.slot);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reserve_caps_at_capacity_and_releases_on_drop() {
        let reg = BackgroundRegistry::new();
        let a = reg.try_reserve(2, "parent").unwrap();
        let b = reg.try_reserve(2, "parent").unwrap();
        assert_eq!(reg.active(), 2);
        assert!(reg.try_reserve(2, "parent").is_none(), "at capacity");
        drop(a);
        assert_eq!(reg.active(), 1);
        let _c = reg.try_reserve(2, "parent").unwrap(); // slot freed
        drop(b);
    }

    #[test]
    fn interrupt_for_session_cancels_only_that_parents_tokens() {
        let reg = BackgroundRegistry::new();
        let a = reg.try_reserve(4, "p1").unwrap();
        let b = reg.try_reserve(4, "p2").unwrap();
        reg.interrupt_for_session("p1");
        assert!(a.token().is_cancelled());
        assert!(!b.token().is_cancelled());
    }

    #[test]
    fn interrupt_for_session_with_no_reservations_is_a_noop() {
        let reg = BackgroundRegistry::new();
        // No reservations for "nobody" have ever been made; this must not
        // panic and must not disturb other parents' reservations.
        reg.interrupt_for_session("nobody");
        let a = reg.try_reserve(2, "parent").unwrap();
        assert!(!a.token().is_cancelled());
    }

    #[test]
    fn concurrent_admission_never_exceeds_capacity() {
        use std::sync::{Barrier, Mutex as StdMutex};
        use std::thread;

        let reg = (*BackgroundRegistry::new()).clone();
        let capacity = 3usize;
        let contenders = 32usize;
        let start = Arc::new(Barrier::new(contenders));
        // Reservations are collected here (not dropped) so every thread's
        // admission decision has landed before we inspect the outcome —
        // otherwise an early Drop could free a slot mid-race and mask a
        // TOCTOU bug by letting a later caller "correctly" fill it.
        let admitted: Arc<StdMutex<Vec<Reservation>>> = Arc::new(StdMutex::new(Vec::new()));

        let handles: Vec<_> = (0..contenders)
            .map(|_| {
                let reg = reg.clone();
                let start = Arc::clone(&start);
                let admitted = Arc::clone(&admitted);
                thread::spawn(move || {
                    start.wait();
                    if let Some(reservation) = reg.try_reserve(capacity, "parent") {
                        admitted.lock().unwrap().push(reservation);
                    }
                })
            })
            .collect();

        for h in handles {
            h.join().unwrap();
        }

        // With all `contenders` threads racing simultaneously (via the
        // barrier) and none of the reservations dropped yet, the atomic
        // check-and-reserve must have admitted exactly `capacity` of them —
        // never more (no TOCTOU over-admission) and never fewer (no lost
        // wakeups/slots).
        let mut admitted = admitted.lock().unwrap();
        assert_eq!(admitted.len(), capacity);
        assert_eq!(reg.active(), capacity);

        // Dropping every reservation must release all slots cleanly.
        admitted.clear();
        assert_eq!(reg.active(), 0, "all slots released after drop");
    }
}
