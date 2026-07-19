//! Consumable per-turn iteration budget replacing the flat MAX_PROVIDER_TURNS.
//! Parent turns get PARENT_MAX_ITERS; each sub-agent drive() gets its own
//! SUBAGENT_MAX_ITERS. Housekeeping-only turns can `refund()` to avoid
//! spending budget on memory/todo churn (Hermes' pattern).
use std::sync::atomic::{AtomicUsize, Ordering};

/// Iteration budget for the top-level (user-facing) turn loop.
pub const PARENT_MAX_ITERS: usize = 500;
/// Iteration budget for each sub-agent's own `drive()` call — sub-agents do
/// not share the parent's budget; each gets a fresh allotment.
pub const SUBAGENT_MAX_ITERS: usize = 500;

/// A consumable, refundable counter of remaining provider-turn iterations.
/// Thread-safe via atomics so it can be shared (`&IterationBudget`) across
/// concurrently-executing sub-agent drives without a lock.
pub struct IterationBudget {
    remaining: AtomicUsize,
}

impl IterationBudget {
    pub fn new(n: usize) -> Self {
        IterationBudget {
            remaining: AtomicUsize::new(n),
        }
    }

    /// Consume one iteration; returns false (without underflowing) when spent.
    pub fn try_consume(&self) -> bool {
        let mut cur = self.remaining.load(Ordering::Relaxed);
        loop {
            if cur == 0 {
                return false;
            }
            match self.remaining.compare_exchange_weak(
                cur,
                cur - 1,
                Ordering::Relaxed,
                Ordering::Relaxed,
            ) {
                Ok(_) => return true,
                Err(actual) => cur = actual,
            }
        }
    }

    /// Return one iteration to the budget (e.g. a housekeeping-only turn that
    /// should not count against the caller's allotment).
    pub fn refund(&self) {
        self.remaining.fetch_add(1, Ordering::Relaxed);
    }

    pub fn remaining(&self) -> usize {
        self.remaining.load(Ordering::Relaxed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn budget_consumes_and_refunds() {
        let b = IterationBudget::new(2);
        assert!(b.try_consume());
        assert!(b.try_consume());
        assert!(!b.try_consume(), "exhausted");
        b.refund();
        assert!(b.try_consume(), "refund restores one");
        assert_eq!(b.remaining(), 0);
    }
}
