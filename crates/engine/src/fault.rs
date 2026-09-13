//! Production fault-injection seams.
//!
//! The hooks are inert unless a test or an operator explicitly arms them.
//! They live in the production ordering code so crash tests exercise the same
//! fsync, head-selection, transfer, and effect boundaries as real processes.

use std::fmt;
use std::sync::atomic::{AtomicU8, Ordering};

/// A durable ordering boundary at which a process may be aborted.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum Boundary {
    /// Intent and target validation completed.
    Prepare = 1,
    /// Immutable object closure was flushed.
    ObjectFlush = 2,
    /// A journal frame was synced.
    JournalFlush = 3,
    /// The durable head pointer was selected.
    HeadSelection = 4,
    /// A publication notification was emitted.
    Notification = 5,
    /// A transfer frame was staged.
    Transfer = 6,
    /// An effect sink was called.
    ExternalEffect = 7,
    /// A worker output was admitted.
    OutputAdmission = 8,
    /// Owner fencing/epoch acquisition completed.
    OwnerFence = 9,
    /// The unique temporary path was created.
    TempCreate = 10,
    /// Temporary file bytes were written.
    TempWrite = 11,
    /// Temporary file data was synced.
    FileSync = 12,
    /// Atomic rename completed.
    Rename = 13,
    /// Parent directory entry was synced.
    DirSync = 14,
    /// The lower store pack write boundary was reached.
    ObjectWrite = 15,
    /// The lower store closure write boundary was reached.
    ClosureWrite = 16,
    /// The engine Prepared journal record is about to be appended.
    JournalPrepared = 17,
    /// The engine Select journal record is about to be appended.
    JournalSelect = 18,
    /// The engine Published journal record is about to be appended.
    JournalPublished = 19,
    /// The lower store's fixed-size selected-head write is about to begin.
    HeadWrite = 20,
    /// Recovery has begun reading durable state.
    Recovery = 21,
    /// An effect intent is about to be persisted.
    EffectPrepared = 22,
    /// An effect pre-call fence is about to be persisted.
    EffectExecuting = 23,
    /// The external effect sink call is about to begin.
    EffectCall = 24,
    /// An ambiguous effect marker is about to be persisted.
    EffectAmbiguous = 25,
    /// A confirmed effect receipt is about to be persisted.
    EffectConfirmed = 26,
}

impl Boundary {
    const fn token(self) -> u8 {
        self as u8
    }
}

/// Error returned when a configured boundary requests a cooperative abort.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct InjectedCrash {
    /// Boundary that fired.
    pub boundary: Boundary,
}

impl fmt::Display for InjectedCrash {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "fault injected at {:?}", self.boundary)
    }
}

impl std::error::Error for InjectedCrash {}

/// One-shot fault controller shared by production components.
#[derive(Default)]
pub struct Faults {
    armed: AtomicU8,
}

impl fmt::Debug for Faults {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Faults")
            .field("armed", &self.armed.load(Ordering::Acquire))
            .finish()
    }
}

impl Faults {
    /// Arms a one-shot abort at `boundary`.
    pub fn arm(&self, boundary: Boundary) {
        self.armed.store(boundary.token(), Ordering::Release);
    }

    /// Compatibility spelling for fault-matrix callers.
    pub fn crash_at(&self, boundary: Boundary) {
        self.arm(boundary);
    }

    /// Consumes the armed fault when the boundary is reached.
    /// # Errors
    ///
    /// Returns an error when validation, persistence, or admission of the
    /// supplied value fails.
    pub fn trip(&self, boundary: Boundary) -> Result<(), InjectedCrash> {
        self.armed
            .compare_exchange(boundary.token(), 0, Ordering::AcqRel, Ordering::Acquire)
            .map_or(Ok(()), |_| Err(InjectedCrash { boundary }))
    }

    /// Clears any armed fault.
    pub fn disarm(&self) {
        self.armed.store(0, Ordering::Release);
    }
}
