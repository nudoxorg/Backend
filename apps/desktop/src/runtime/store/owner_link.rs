//! What a window knows about the owner behind its reads (W-Open I1): whether
//! it is answering, the pages asked while it starts, and the gate that
//! restarts a failed one.
//!
//! While the owner starts, reads are held here, never queued behind a socket
//! that does not exist yet, and never asked at a root nobody served. A failed
//! owner is a fault on every page, in its own words.

use crate::runtime::owner::{Epoch, OwnerFault, OwnerGate, OwnerState};
use crate::model::pages::PageKey;
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
    /// Attached generation whose reads this store currently admits.
    served_epoch: Option<Epoch>,
    /// Pages asked for while the owner starts, fetched once it answers.
    held: BTreeSet<PageKey>,
}

impl OwnerLink {
    /// An owner that is already answering (the harness and tests).
    pub(super) const fn serving() -> Self {
        Self { phase: OwnerPhase::Serving, gate: None, served_epoch: None, held: BTreeSet::new() }
    }

    /// The link to the owner `gate` says it is now.
    pub(super) fn behind(gate: OwnerGate) -> Self {
        let phase = match gate.state() {
            OwnerState::Starting => OwnerPhase::Starting,
            OwnerState::Ready { .. } => OwnerPhase::Serving,
            OwnerState::Failed(fault) => OwnerPhase::Failed(fault),
        };
        let served_epoch = gate.attached_ready_epoch();
        Self { phase, gate: Some(gate), served_epoch, held: BTreeSet::new() }
    }

    pub(super) const fn phase(&self) -> &OwnerPhase {
        &self.phase
    }

    pub(super) fn is_serving(&self) -> bool {
        self.phase == OwnerPhase::Serving
    }

    /// A same-root reattachment still changes the authority for in-flight
    /// results. Check the gate directly, since several state publications
    /// can coalesce before the UI watcher runs.
    pub(super) fn attachment_changed(&self) -> bool {
        self.gate.as_ref().is_some_and(|gate| gate.attached_ready_epoch() != self.served_epoch)
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
        self.served_epoch = self.gate.as_ref().and_then(OwnerGate::attached_ready_epoch);
        std::mem::take(&mut self.held)
    }

    /// The owner failed with `fault`: the pages that waited on it.
    pub(super) fn failed(&mut self, fault: OwnerFault) -> BTreeSet<PageKey> {
        self.phase = OwnerPhase::Failed(fault);
        self.served_epoch = None;
        std::mem::take(&mut self.held)
    }

    /// The owner is starting again: pages asked from now on are held. A UI
    /// watcher may observe this state without observing the preceding loss.
    pub(super) fn starting(&mut self) -> bool {
        let moved = self.phase != OwnerPhase::Starting;
        self.phase = OwnerPhase::Starting;
        self.served_epoch = None;
        moved
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::VersionedRoot;
    use crate::model::ServiceMode;

    #[test]
    fn a_same_root_reattachment_is_new_authority_even_when_the_watcher_skips_states() {
        let root = VersionedRoot::unserved();
        let gate = OwnerGate::ready(root, ServiceMode::Attached);
        let mut link = OwnerLink::behind(gate.clone());
        assert!(!link.attachment_changed());
        let old = gate.attached_ready_epoch().expect("attached owner");
        assert!(gate.attached_lost_at(old, "socket closed".into()));
        assert!(gate.restart());
        gate.publish(OwnerState::Ready { key: root, mode: ServiceMode::Attached });
        assert!(link.attachment_changed(), "the store must revoke old reads even when it only observes the final Ready");
        let _ = link.answered();
        assert!(!link.attachment_changed());
    }

    #[test]
    fn a_starting_state_interrupts_serving_when_the_watcher_skips_the_failure() {
        let gate = OwnerGate::ready(VersionedRoot::unserved(), ServiceMode::Attached);
        let mut link = OwnerLink::behind(gate.clone());
        assert!(link.is_serving());
        gate.publish(OwnerState::Starting);
        assert!(link.attachment_changed());
        assert!(link.starting());
        assert!(!link.is_serving());
        link.hold(PageKey::Health);
        assert_eq!(link.answered(), BTreeSet::from([PageKey::Health]));
    }
}
