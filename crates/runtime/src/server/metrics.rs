//! Defines metrics behavior for `backend_runtime::server`, whose purpose is to schedule bounded work with explicit credits, ownership, and wakeups.
//! This module owns the metrics invariants and typed state transitions.
//! Its narrow surface prevents representation and policy details from leaking outward.
//! Static accounting policies and their explicitly scoped observations.
#![allow(
    missing_docs,
    reason = "the public snapshot types define the accounting contract at their boundaries"
)]

use core::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

/// Always-available structural state derived from live bounded permits.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct RuntimeMetrics {
    pub capacity: usize,
    pub active_capacity: usize,
    pub available: usize,
    pub checked_out: usize,
    pub reserved_bytes: usize,
    pub retired_work_slots: usize,
    pub terminal_occupied: usize,
}

/// Eventually consistent historical observations available only from an observed policy.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct RuntimeHistory {
    pub high_water_checked_out: usize,
    pub rejected_work_slots: u64,
    pub rejected_byte_budget: u64,
    pub cancelled: u64,
    pub completed: u64,
    pub failed: u64,
    pub stale: u64,
}

/// Closed terminal class used for optional historical counting.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TerminalClass {
    Completed,
    Failed,
    Cancelled,
    StaleGeneration,
    ExecutorUnwound,
}

/// Opt-in relaxed accounting for servers and tests that need historical observations.
#[derive(Debug, Default)]
pub struct AtomicAccounting {
    counters: Counters,
}

/// Statically composed accounting behavior; no runtime `Option`, `dyn`, or branch is involved.
pub trait Accounting: private::Sealed + Default {
    fn enter(&self, checked_out: usize);
    fn reject_work_slot(&self);
    fn reject_byte_budget(&self);
    fn terminal(&self, class: TerminalClass);
}

/// Accounting policy that can return historical observations without inventing zero facts.
pub trait ObservedAccounting: Accounting {
    fn history(&self) -> RuntimeHistory;
}

mod private {
    pub trait Sealed {}
}

impl private::Sealed for () {}
impl private::Sealed for AtomicAccounting {}

impl Accounting for () {
    #[inline]
    fn enter(&self, _checked_out: usize) {}
    #[inline]
    fn reject_work_slot(&self) {}
    #[inline]
    fn reject_byte_budget(&self) {}
    #[inline]
    fn terminal(&self, _class: TerminalClass) {}
}

impl Accounting for AtomicAccounting {
    fn enter(&self, checked_out: usize) {
        self.counters.high.fetch_max(checked_out, Ordering::Relaxed);
    }

    fn reject_work_slot(&self) {
        self.counters.rejected_slots.fetch_add(1, Ordering::Relaxed);
    }

    fn reject_byte_budget(&self) {
        self.counters.rejected_bytes.fetch_add(1, Ordering::Relaxed);
    }
    fn terminal(&self, class: TerminalClass) {
        match class {
            TerminalClass::Completed => {
                self.counters.completed.fetch_add(1, Ordering::Relaxed);
            }
            TerminalClass::Failed | TerminalClass::ExecutorUnwound => {
                self.counters.failed.fetch_add(1, Ordering::Relaxed);
            }
            TerminalClass::Cancelled => {
                self.counters.cancelled.fetch_add(1, Ordering::Relaxed);
            }
            TerminalClass::StaleGeneration => {
                self.counters.stale.fetch_add(1, Ordering::Relaxed);
            }
        }
    }
}

impl ObservedAccounting for AtomicAccounting {
    fn history(&self) -> RuntimeHistory {
        RuntimeHistory {
            high_water_checked_out: self.counters.high.load(Ordering::Relaxed),
            rejected_work_slots: self.counters.rejected_slots.load(Ordering::Relaxed),
            rejected_byte_budget: self.counters.rejected_bytes.load(Ordering::Relaxed),
            cancelled: self.counters.cancelled.load(Ordering::Relaxed),
            completed: self.counters.completed.load(Ordering::Relaxed),
            failed: self.counters.failed.load(Ordering::Relaxed),
            stale: self.counters.stale.load(Ordering::Relaxed),
        }
    }
}

#[derive(Debug, Default)]
struct Counters {
    high: AtomicUsize,
    rejected_slots: AtomicU64,
    rejected_bytes: AtomicU64,
    cancelled: AtomicU64,
    completed: AtomicU64,
    failed: AtomicU64,
    stale: AtomicU64,
}
