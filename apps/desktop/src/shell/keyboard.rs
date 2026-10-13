//! A short-lived claim on the current window's native keyboard receiver.
//!
//! The snapshot remains the overlay/visit authority. This only records the
//! focus handle a transition requested and the handle it displaced, so a
//! post-paint check cannot steal focus from a newer user action.

use crate::navigation::{Overlay, presentation::VisitId};
use crate::runtime::content_visit::ContentVisit;
use gpui::{FocusHandle, WindowId};

/// The destination that owns local presentation and editing. This is not a
/// producer or resource lease: owner replacement does not change a reading
/// visit, while navigation, a cover, or a preview retires its local input.
#[derive(Clone, PartialEq, Eq)]
pub(crate) struct LocalReadingScope {
    pub(crate) visit: VisitId,
    destination: ContentVisit,
}

/// Actual current native editor ownership, used only for local draft writes.
/// The private fields prevent an old logical target from manufacturing it.
pub(crate) struct LocalNativeEditorFrame {
    _reading: LocalReadingScope,
    _window: WindowId,
    _focus_epoch: u64,
}

impl LocalReadingScope {
    pub(crate) fn capture(snapshot: &crate::model::AppSnapshot) -> Self {
        Self { visit: snapshot.session().reading.current.id, destination: ContentVisit::capture(snapshot) }
    }

    pub(crate) fn matches_destination(&self, snapshot: &crate::model::AppSnapshot) -> bool {
        self.visit == snapshot.session().reading.current.id && self.destination.matches_destination(snapshot)
    }

    pub(crate) fn admits_input(&self, snapshot: &crate::model::AppSnapshot) -> bool {
        self.matches_destination(snapshot) && snapshot.overlay().is_none()
            && snapshot.page_overlay().is_none() && snapshot.session().preview.is_none()
    }

    pub(crate) fn native_editor_frame(&self, snapshot: &crate::model::AppSnapshot,
        handle: &FocusHandle, window: &gpui::Window, cx: &gpui::App) -> Option<LocalNativeEditorFrame>
    {
        (self.admits_input(snapshot) && window.is_window_active()
            && window.is_focus_handle_mounted(handle) && handle.is_focused(window))
            .then(|| LocalNativeEditorFrame { _reading: self.clone(),
                _window: window.window_handle().window_id(), _focus_epoch: window.focus_epoch() })
    }
}

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
    reading: LocalReadingScope,
    window: WindowId,
    generation: u64,
    focus_epoch: u64,
    target: FocusHandle,
    origin: Option<FocusHandle>,
}

impl KeyboardClaim {
    pub(super) fn new(
        overlay: Overlay,
        reading: LocalReadingScope,
        window: WindowId,
        generation: Option<u64>,
        focus_epoch: u64,
        target: FocusHandle,
        origin: Option<FocusHandle>,
    ) -> Option<Self> {
        Some(Self { overlay, reading, window, generation: generation?, focus_epoch, target, origin })
    }

    pub(super) fn target(&self) -> &FocusHandle { &self.target }

    pub(super) fn same_request(&self, other: &Self) -> bool {
        self.overlay == other.overlay
            && self.reading == other.reading
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
        snapshot: &crate::model::AppSnapshot,
        window: WindowId,
        generation: Option<u64>,
    ) -> bool {
        snapshot.overlay() == Some(self.overlay)
            && self.reading.matches_destination(snapshot)
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{core::VersionedRoot, model::AppSnapshot, navigation::{Intent, Route, View}};

    #[test]
    fn local_editor_and_keyboard_claims_cannot_revive_after_back_or_a_view_round_trip() {
        let root = VersionedRoot::synthetic(backend_library::view_state_root(&[
            ("keyboard".to_owned(), "activation".to_owned())]), 3);
        let base = crate::navigation::reduce(&AppSnapshot::empty(root), Intent::Navigate(
            crate::shell::tests::view_route("RelationLabel", View::Code))).snapshot;
        let captured = LocalReadingScope::capture(&base);
        let page = crate::navigation::reduce(&base, Intent::SetView(View::Page)).snapshot;
        let code = crate::navigation::reduce(&page, Intent::SetView(View::Code)).snapshot;
        assert_eq!(code.route(), base.route());
        assert_eq!(code.session().reading.current.id, captured.visit);
        assert!(!captured.admits_input(&code));
        let away = crate::navigation::reduce(&base, Intent::Navigate(Route::World)).snapshot;
        let returned = crate::navigation::reduce(&away, Intent::Back).snapshot;
        assert_eq!(returned.session().reading.current, base.session().reading.current);
        assert!(!captured.matches_destination(&returned));
        assert!(LocalReadingScope::capture(&returned).admits_input(&returned));
    }

    #[test]
    fn local_reading_survives_producer_changes_and_a_cover_restores_only_its_existing_activation() {
        let root = VersionedRoot::synthetic(backend_library::view_state_root(&[
            ("keyboard".to_owned(), "local".to_owned())]), 2);
        let base = AppSnapshot::empty(root);
        let reading = LocalReadingScope::capture(&base);
        let changed = base.with_key(root.with_generation(3), None);
        assert!(reading.admits_input(&changed), "local reading needs no serving-owner receipt");
        let covered = crate::navigation::reduce(&base, Intent::OpenCommandPalette).snapshot;
        assert!(reading.matches_destination(&covered));
        assert!(!reading.admits_input(&covered));
        let returned = crate::navigation::reduce(&covered, Intent::DismissOverlay).snapshot;
        assert!(reading.admits_input(&returned), "a cover preserves the underlying activation");
    }
}
