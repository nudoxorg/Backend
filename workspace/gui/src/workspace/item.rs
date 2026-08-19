//! `WorkspaceItem` — the trait every centre-pane content view implements.
//!
//! GUI-PLAN §13.4 specifies this trait by analogy with Zed's `Item` pattern.
//! The analogy is about *structure*, not code: Zed's `Item` carries lifetime
//! complexity from its incremental parser integration.  Ours is simpler; it
//! only needs three things from each content view:
//!
//! 1. **`tab_content`** — what to render inside the tab strip button.
//!    Per §13.4 the required elements are: icon, title, provenance badge, and
//!    an optional `CountLabel`.  None of those allocations happen here — the
//!    view returns a pre-built `AnyElement`.
//!
//! 2. **`telemetry_id`** — a `&'static str` naming the kind of content for
//!    instrumentation (§25), e.g. `"symbol-page"`, `"package-browser"`,
//!    `"settings"`.  Must not be a dynamic string — it is used as a key in
//!    the perf harness.
//!
//! 3. **`nav_entry`** — enough to restore this view from nav history (§12.8).
//!    Returns `None` for views that are not navigable (e.g. Settings).
//!
//! # Split-readiness (LD-20)
//!
//! v1 has one pane.  The trait is designed so items are `AnyView`s owned by
//! `ItemSlot`s inside a `Pane`.  When pane splitting arrives (P2), `Pane` grows
//! from `Vec<ItemSlot>` to a tree of panes — no change to `WorkspaceItem` or
//! to the views that implement it.  Items are never coupled to their containing
//! pane except through the slot.
//!
//! # `Focusable` is a supertrait — this is the L16 fix
//!
//! GPUI's `dispatch_action` only walks the ancestor chain of whatever element
//! currently holds *window focus*. `Pane` tracks focus on its own root div so
//! pane-level actions (`ActivateTabN`, `CloseTab`) always work, but that does
//! **not** extend focus down into a tab's own content: an item that registers
//! `.on_action` handlers on its own root div can only receive them while its
//! own `FocusHandle` is the focused one.
//!
//! `SymbolPage` learned this the hard way (docs/LIMITATIONS.md L16): it declared
//! `.on_action` handlers for `GoToSourceTab`, `GoToRefsTab`,
//! `OpenVersionPicker` and never called `window.focus` on itself, so none of
//! them could ever fire — not in tests, not from a real keystroke.
//!
//! Requiring `Focusable` here, rather than patching that one view, makes the
//! failure mode unrepresentable for every *future* `WorkspaceItem` too:
//! `Pane::activate_ix` (the single place a tab becomes active — on open, on
//! click, on `ActivateTabN`) focuses the newly active item's handle as part of
//! activation. A `WorkspaceItem` that cannot produce a `FocusHandle` cannot be
//! opened in a pane at all; the compiler catches it at `open_item`'s call
//! site instead of a reviewer catching it at a keystroke that silently does
//! nothing.

use gpui::{AnyElement, Focusable, SharedString};

use crate::theme::ext::Provenance;

// ─────────────────────────────────────────────────────────────────────────────
// NavEntry
// ─────────────────────────────────────────────────────────────────────────────

/// A navigation entry that can be stored in [`NavHistory`] (§12.8).
///
/// The `kind` field names the screen class so `NavHistory` can re-open the
/// right view.  The `key` is screen-specific: a serialised `SymbolKey` for
/// symbol pages, a `PackageLineageId` for package browsers, etc.
///
/// `scroll_fraction` is `0.0`–`1.0`; restored via `UniformListScrollHandle`
/// on re-activation.
///
/// [`NavHistory`]: TODO(views) crate::stores::NavHistory
#[derive(Debug, Clone, PartialEq)]
pub struct NavEntry {
    /// Screen class identifier, e.g. `"symbol-page"`.
    pub kind: SharedString,
    /// Screen-specific serialised key (opaque to nav history).
    pub key: SharedString,
    /// Scroll position fraction to restore on re-activation.
    pub scroll_fraction: f32,
}

// ─────────────────────────────────────────────────────────────────────────────
// WorkspaceItem
// ─────────────────────────────────────────────────────────────────────────────

/// Implemented by every view that can occupy a centre-pane tab.
///
/// # Performance contract
///
/// `tab_content` is called during `Pane::render`.  It **must** be fast:
/// no allocation beyond building the returned element, no `format!`, no
/// sorting, no filtering.  All strings must already live in the view's
/// store-derived state as `SharedString`.
///
/// # Implementing the trait
///
/// ```rust,ignore
/// impl WorkspaceItem for SymbolPage {
///     fn tab_content(&mut self, _: &mut Window, _: &mut Context<Self>) -> AnyElement {
///         h_flex()
///             .gap(cx.theme_ext().space.space_1)
///             .child(Icon::new(IconName::Code))
///             .child(Label::new(self.title.clone()))
///             .child(ProvenanceDot::new(self.provenance))
///             .into_any_element()
///     }
///     fn telemetry_id(&self) -> &'static str { "symbol-page" }
///     fn nav_entry(&self) -> Option<NavEntry> {
///         Some(NavEntry {
///             kind: "symbol-page".into(),
///             key: self.symbol_key.to_shared_string(),
///             scroll_fraction: self.scroll_fraction,
///         })
///     }
/// }
/// ```
pub trait WorkspaceItem: 'static + Focusable {
    /// Element displayed inside the tab button.
    ///
    /// The implementation must include at minimum:
    /// - A recognisable icon for the content kind.
    /// - A title label (the symbol name, package name, or screen name).
    /// - A provenance badge ([`Provenance`]) if the content has one.
    /// - An optional count label (e.g., `128 refs`) while streaming.
    ///
    /// No `format!` or allocation-heavy work here — use pre-built
    /// `SharedString`s from store state.
    /// # Why `&self` and `&App`
    ///
    /// `Pane::render` calls this for every open tab, from inside its own
    /// `update`. Taking `&mut Context<Self>` would mean re-entering the item's
    /// entity while the pane is mid-render — legal in GPUI but only by
    /// accident, and it invites an implementation that mutates during layout
    /// and then wonders why the tab strip is a frame behind. A tab label is a
    /// projection of state the item already holds; `&self` says so.
    fn tab_content(&self, cx: &gpui::App) -> AnyElement;

    /// A compile-time constant identifying the screen class.
    ///
    /// Used by the perf harness and telemetry (§25).  Must not be dynamic.
    /// Examples: `"symbol-page"`, `"package-browser"`, `"settings"`.
    fn telemetry_id(&self) -> &'static str;

    /// Navigation history entry for this view, if it is re-openable.
    ///
    /// Return `None` for views that are singletons without navigable state
    /// (e.g., Settings, `?` shortcuts).
    fn nav_entry(&self) -> Option<NavEntry>;

    /// The provenance of the content (LD-8).
    ///
    /// `Pane` uses this to update the tab's badge after streaming completes.
    /// Views that do not have a meaningful provenance return
    /// [`Provenance::TrustedLocal`] (local is always the truth — LR-10).
    fn provenance(&self) -> Provenance {
        Provenance::TrustedLocal
    }
}
