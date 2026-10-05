//! What a window knows about the owner behind its reads (W-Open I1): whether
//! it is answering, the pages asked while it starts, and the gate that
//! restarts a failed one.
//!
//! While the owner starts, reads are held here, never queued behind a socket
//! that does not exist yet, and never asked at a root nobody served. A failed
//! owner is a fault on every page, in its own words.

use crate::runtime::owner::{Epoch, OwnerFault, OwnerGate, OwnerState, RetryGeneration};
use crate::model::pages::PageKey;
use std::collections::BTreeSet;

/// A serving attachment captured by one UI visit. The synthetic owner used
/// by ungated tests has no gate; it cannot establish real-owner acceptance.
#[derive(Clone, Debug)]
pub(crate) struct OwnerAttachment {
    gate: Option<OwnerGate>,
    epoch: Option<Epoch>,
}

impl PartialEq for OwnerAttachment {
    fn eq(&self, other: &Self) -> bool {
        self.epoch == other.epoch
            && match (&self.gate, &other.gate) {
                (Some(left), Some(right)) => left.same_gate(right),
                (None, None) => true,
                _ => false,
            }
    }
}

impl Eq for OwnerAttachment {}

/// A rendered Retry belongs to this exact failed publication and gate.
#[derive(Clone, Debug)]
pub(crate) struct OwnerRetryAttachment {
    gate: OwnerGate,
    generation: RetryGeneration,
}

impl PartialEq for OwnerRetryAttachment {
    fn eq(&self, other: &Self) -> bool {
        self.generation == other.generation && self.gate.same_gate(&other.gate)
    }
}
impl Eq for OwnerRetryAttachment {}

/// One Ready publication captured before the store revokes previous reads.
pub(super) struct OwnerAnswer {
    epoch: Option<Epoch>,
    pub(super) attachment_changed: bool,
}

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
    /// Ready publication whose reads this store currently admits.
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
        let served_epoch = gate.ready_epoch();
        Self { phase, gate: Some(gate), served_epoch, held: BTreeSet::new() }
    }

    pub(super) const fn phase(&self) -> &OwnerPhase {
        &self.phase
    }

    pub(super) fn is_serving(&self) -> bool {
        self.phase == OwnerPhase::Serving
    }

    /// Paint checks the gate itself as well as the UI-observed phase: owner
    /// publications can precede the watcher or coalesce into a same-root Ready.
    pub(super) fn is_current_serving(&self) -> bool {
        self.is_serving()
            && self.gate.as_ref().is_none_or(|gate| gate.serves_attachment(self.served_epoch))
    }

    /// Capture only an attachment that the live gate still admits. A saved
    /// visit must not become actionable again after a same-root replacement.
    pub(super) fn current_attachment(&self) -> Option<OwnerAttachment> {
        self.is_current_serving().then(|| OwnerAttachment {
            gate: self.gate.clone(),
            epoch: self.served_epoch,
        })
    }

    pub(super) fn current_mutation(&self) -> Option<crate::runtime::actor::IndexMutationLease> {
        if !self.is_current_serving() { return None; }
        crate::runtime::actor::IndexMutationLease::capture(self.gate.as_ref(), self.served_epoch)
    }

    pub(super) fn current_fault(&self) -> Option<OwnerFault> {
        if let Some(gate) = &self.gate {
            return match gate.state() {
                OwnerState::Failed(fault) => Some(fault),
                OwnerState::Starting | OwnerState::Ready { .. } => None,
            };
        }
        match &self.phase {
            OwnerPhase::Failed(fault) => Some(fault.clone()),
            OwnerPhase::Serving | OwnerPhase::Starting => None,
        }
    }

    pub(super) fn can_retry_current(&self) -> bool {
        self.gate.as_ref().is_some_and(OwnerGate::can_retry_current)
    }

    pub(super) fn current_retry_attachment(&self) -> Option<OwnerRetryAttachment> {
        let gate = self.gate.as_ref()?;
        Some(OwnerRetryAttachment { gate: gate.clone(), generation: gate.retry_generation()? })
    }

    pub(super) fn retry_at(&mut self, expected: &OwnerRetryAttachment, key: PageKey) -> bool {
        let Some(gate) = self.gate.as_ref() else { return false; };
        if !gate.same_gate(&expected.gate) || !gate.restart_at(expected.generation) { return false; }
        self.phase = OwnerPhase::Starting;
        self.held.insert(key);
        true
    }

    /// A same-root reattachment still changes the authority for in-flight
    /// results. Check the gate directly, since several state publications
    /// can coalesce before the UI watcher runs.
    pub(super) fn attachment_changed(&self) -> bool {
        self.gate.as_ref().is_some_and(|gate| gate.ready_epoch() != self.served_epoch)
    }

    /// Remembers a page asked for while the owner is not answering.
    pub(super) fn hold(&mut self, key: PageKey) {
        self.held.insert(key);
    }

    /// Forgets the held pages `keep` does not keep (the route moved on).
    pub(super) fn retain_held(&mut self, keep: impl FnMut(&PageKey) -> bool) {
        self.held.retain(keep);
    }

    /// Keep the exact attachment the watcher observed while its previous
    /// read admissions are revoked; do not reread the gate when serving it.
    pub(super) fn prepare_answer(&self) -> OwnerAnswer {
        let epoch = self.gate.as_ref().and_then(OwnerGate::ready_epoch);
        OwnerAnswer { epoch, attachment_changed: epoch != self.served_epoch }
    }

    /// The owner answered: previous reads are revoked, serving now, and the
    /// pages that waited can be fetched at the captured attachment.
    pub(super) fn answered(&mut self, answer: OwnerAnswer) -> BTreeSet<PageKey> {
        self.phase = OwnerPhase::Serving;
        self.served_epoch = answer.epoch;
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
        if let Some(expected) = self.current_retry_attachment() { let _ = self.retry_at(&expected, key); }
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
        let captured = link.current_attachment().expect("serving attachment");
        let old = gate.attached_ready_epoch().expect("attached owner");
        assert!(gate.attached_lost_at(old, "socket closed".into()));
        assert!(gate.restart());
        gate.publish(OwnerState::Ready { key: root, mode: ServiceMode::Attached });
        assert!(link.attachment_changed(), "the store must revoke old reads even when it only observes the final Ready");
        assert!(!link.is_current_serving());
        assert_eq!(link.current_attachment(), None);
        let answer = link.prepare_answer();
        assert!(answer.attachment_changed, "revocation uses the epoch actually recorded as serving");
        let _ = link.answered(answer);
        assert!(!link.attachment_changed());
        assert!(link.is_current_serving());
        assert_ne!(link.current_attachment(), Some(captured));
    }

    #[test]
    fn an_ungated_serving_owner_has_a_stable_lease_but_a_starting_one_does_not() {
        let mut link = OwnerLink::serving();
        let captured = link.current_attachment().expect("stable harness owner");
        assert_eq!(link.current_attachment().as_ref(), Some(&captured));
        assert!(link.starting());
        assert_eq!(link.current_attachment(), None);
        let answer = link.prepare_answer();
        let _ = link.answered(answer);
        assert_eq!(link.current_attachment().as_ref(), Some(&captured));
    }

    #[test]
    fn different_gates_do_not_share_a_lease_even_at_the_same_initial_epoch() {
        let root = VersionedRoot::unserved();
        let first = OwnerLink::behind(OwnerGate::ready(root, ServiceMode::Attached));
        let second = OwnerLink::behind(OwnerGate::ready(root, ServiceMode::Attached));
        assert_ne!(first.current_attachment(), second.current_attachment());
    }

    #[test]
    fn a_coalesced_embedded_restart_also_revokes_the_old_serving_lease() {
        let root = VersionedRoot::unserved();
        let gate = OwnerGate::ready(root, ServiceMode::Embedded);
        let mut link = OwnerLink::behind(gate.clone());
        let captured = link.current_attachment().expect("first embedded owner");
        assert_eq!(gate.attached_ready_epoch(), None, "embedded owners do not report socket loss");
        gate.publish(OwnerState::Starting);
        gate.publish(OwnerState::Ready { key: root, mode: ServiceMode::Embedded });
        assert_eq!(link.current_attachment(), None);
        let answer = link.prepare_answer();
        assert!(answer.attachment_changed);
        let _ = link.answered(answer);
        assert_ne!(link.current_attachment(), Some(captured));
        assert!(link.is_current_serving());
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
        let answer = link.prepare_answer();
        assert_eq!(link.answered(answer), BTreeSet::from([PageKey::Health]));
    }
    #[test]
    fn an_incapable_failure_remains_failed_when_retry_is_requested() {
        let gate = OwnerGate::starting();
        gate.disable_restart();
        gate.publish(OwnerState::Failed("there is no startup worker".into()));
        let mut link = OwnerLink::behind(gate.clone());
        assert!(!link.can_retry_current());
        assert!(link.current_retry_attachment().is_none());
        link.retry(PageKey::Health);
        assert!(matches!(link.phase(), OwnerPhase::Failed(_)));
        assert!(link.held.is_empty());
        assert!(matches!(gate.state(), OwnerState::Failed(_)));
    }

    #[test]
    fn a_rendered_retry_cannot_act_on_a_later_failure_or_another_gate() {
        let gate = OwnerGate::starting();
        gate.publish(OwnerState::Failed("first failure".into()));
        let mut link = OwnerLink::behind(gate.clone());
        let old = link.current_retry_attachment().expect("first retry capability");
        gate.publish(OwnerState::Failed("later failure".into()));
        assert!(!link.retry_at(&old, PageKey::Health));
        assert!(matches!(gate.state(), OwnerState::Failed(_)));
        let current = link.current_retry_attachment().expect("current retry capability");
        let other = OwnerGate::starting();
        other.publish(OwnerState::Failed("first failure".into()));
        other.publish(OwnerState::Failed("later failure".into()));
        let mut other_link = OwnerLink::behind(other);
        assert!(!other_link.retry_at(&current, PageKey::Health));
        assert!(link.retry_at(&current, PageKey::Health));
        assert_eq!(gate.state(), OwnerState::Starting);
        assert!(matches!(link.phase(), OwnerPhase::Starting));
        assert!(!link.retry_at(&current, PageKey::Health), "one failure capability starts at most once");
        assert!(!link.can_retry_current());
    }

}
