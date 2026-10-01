//! What a window knows about the owner behind its reads (W-Open I1): whether
//! it is answering, the pages asked while it starts, and the gate that
//! restarts a failed one.
//!
//! While the owner starts, reads are held here, never queued behind a socket
//! that does not exist yet, and never asked at a root nobody served. A failed
//! owner is a fault on every page, in its own words.

use crate::model::pages::PageKey;
use crate::runtime::owner::{OwnerFault, OwnerGate, OwnerState};
use std::collections::BTreeSet;

/// Whether the owner behind this window's reads is answering.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) enum OwnerPhase {
    /// Answering (the harness, tests, and every window once its owner is).
    Serving,
    /// Not answered yet: pages ask, and wait here.
    Starting,
    /// Could not start: every page says so.
    Failed(OwnerFault),
}

/// The phase, the gate that can restart it, and the pages waiting on it.
#[derive(Debug)]
pub(super) struct OwnerLink {
    phase: OwnerPhase,
    gate: Option<OwnerGate>,
    /// Pages asked for while the owner starts, fetched once it answers.
    held: BTreeSet<PageKey>,
}

impl OwnerLink {
    /// An owner that is already answering (the harness and tests).
    pub(super) const fn serving() -> Self {
        Self {
            phase: OwnerPhase::Serving,
            gate: None,
            held: BTreeSet::new(),
        }
    }

    /// The link to the owner `gate` says it is now.
    pub(super) fn behind(gate: OwnerGate) -> Self {
        let phase = match gate.state() {
            OwnerState::Starting => OwnerPhase::Starting,
            OwnerState::Ready { .. } => OwnerPhase::Serving,
            OwnerState::Failed(fault) => OwnerPhase::Failed(fault),
        };
        Self {
            phase,
            gate: Some(gate),
            held: BTreeSet::new(),
        }
    }

    pub(super) const fn phase(&self) -> &OwnerPhase {
        &self.phase
    }

    pub(super) fn is_serving(&self) -> bool {
        self.phase == OwnerPhase::Serving
    }

    /// Remembers a page asked for while the owner is not answering.
    pub(super) fn hold(&mut self, key: PageKey) {
        self.held.insert(key);
    }

    /// Forgets the held pages `keep` does not keep (the route moved on).
    pub(super) fn retain_held(&mut self, keep: impl FnMut(&PageKey) -> bool) {
        self.held.retain(keep);
    }

    /// The owner answered: serving now, and the pages that waited.
    pub(super) fn answered(&mut self) -> BTreeSet<PageKey> {
        self.phase = OwnerPhase::Serving;
        std::mem::take(&mut self.held)
    }

    /// The owner failed with `fault`: the pages that waited on it.
    pub(super) fn failed(&mut self, fault: OwnerFault) -> BTreeSet<PageKey> {
        self.phase = OwnerPhase::Failed(fault);
        std::mem::take(&mut self.held)
    }

    /// The owner is starting again: pages asked from now on are held. Whether
    /// the phase moved (a serving owner is left alone).
    pub(super) fn starting(&mut self) -> bool {
        if self.is_serving() {
            return false;
        }
        self.phase = OwnerPhase::Starting;
        true
    }

    /// "Try again" on a failed owner: asks it to start again (a starting one
    /// is left alone) and holds the page until it answers.
    pub(super) fn retry(&mut self, key: PageKey) {
        if let Some(gate) = &self.gate {
            let _ = gate.restart();
        }
        self.phase = OwnerPhase::Starting;
        self.held.insert(key);
    }
}
