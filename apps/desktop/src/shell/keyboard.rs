//! A short-lived claim on the current window's native keyboard receiver.
//!
//! The snapshot remains the overlay/visit authority. This only records the
//! focus handle a transition requested and the handle it displaced, so a
//! post-paint check cannot steal focus from a newer user action.

use crate::navigation::{Overlay, presentation::VisitId};
use gpui::{FocusHandle, WindowId};

/// A deferred return may run only while its stable handoff still owns input.
/// Window identity, native focus changes, and user navigation independently
/// revoke it; an exhausted generation can never grant a new return.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) struct NativeReturnLease {
    window: WindowId,
    generation: u64,
    focus_epoch: u64,
}

impl NativeReturnLease {
    pub(super) fn new(window: WindowId, generation: Option<u64>, focus_epoch: u64) -> Option<Self> {
        Some(Self { window, generation: generation?, focus_epoch })
    }

    pub(super) fn current(&self, window: WindowId, generation: Option<u64>, focus_epoch: u64) -> bool {
        self.window == window && generation == Some(self.generation) && self.focus_epoch == focus_epoch
    }
}

#[derive(Clone)]
pub(super) struct KeyboardClaim {
    overlay: Overlay,
    visit: VisitId,
    window: WindowId,
    generation: u64,
    focus_epoch: u64,
    target: FocusHandle,
    origin: Option<FocusHandle>,
}

impl KeyboardClaim {
    pub(super) fn new(
        overlay: Overlay,
        visit: VisitId,
        window: WindowId,
        generation: Option<u64>,
        focus_epoch: u64,
        target: FocusHandle,
        origin: Option<FocusHandle>,
    ) -> Option<Self> {
        Some(Self { overlay, visit, window, generation: generation?, focus_epoch, target, origin })
    }

    pub(super) fn target(&self) -> &FocusHandle { &self.target }

    pub(super) fn same_request(&self, other: &Self) -> bool {
        self.overlay == other.overlay
            && self.visit == other.visit
            && self.window == other.window
            && self.generation == other.generation
            && self.focus_epoch == other.focus_epoch
            && self.target == other.target
            && self.origin == other.origin
    }

    pub(super) fn record_request(&mut self, focus_epoch: u64) {
        self.focus_epoch = focus_epoch;
    }

    pub(super) fn current(
        &self,
        overlay: Option<Overlay>,
        visit: VisitId,
        window: WindowId,
        generation: Option<u64>,
    ) -> bool {
        overlay == Some(self.overlay)
            && visit == self.visit
            && window == self.window
            && generation == Some(self.generation)
    }

    /// A different focused handle means a later pointer, Tab, or native
    /// action has chosen its own receiver. Never override that choice.
    pub(super) fn may_reassert(&self, focused: Option<&FocusHandle>) -> bool {
        focused.is_none_or(|focused| focused == &self.target || self.origin.as_ref() == Some(focused))
    }

    /// Detect even a later explicit blur, whose current handle is `None`.
    pub(super) fn unchanged_focus(&self, focus_epoch: u64) -> bool {
        self.focus_epoch == focus_epoch
    }
}
