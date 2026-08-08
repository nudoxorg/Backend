//! `Pane` — the tab-strip container for [`WorkspaceItem`] views (GUI-PLAN §13.4).
//!
//! # Structural guarantee: one-drop tab teardown (§3.2)
//!
//! This is the most important property in this file, and it is the most likely
//! to be accidentally broken by a well-meaning edit.  Read this before touching
//! the slot type.
//!
//! Each open tab is stored as an [`ItemSlot`]:
//!
//! ```text
//! pub struct ItemSlot {
//!     item: Box<dyn TabItem>,           // the content view, type-erased
//!     _subs: Vec<Subscription>,         // subscriptions this tab holds
//!     // (future: in-flight StreamHandle goes here too — drops = cancels)
//! }
//! ```
//!
//! **Dropping `ItemSlot` is the only required cleanup.** When a tab is closed:
//!
//! 1. `Vec::remove` on `self.slots` drops the `ItemSlot`.
//! 2. Dropping `_subs` drops every `Subscription`.  GPUI will stop calling the
//!    associated callbacks — *no manual unsubscribe needed*.
//! 3. Dropping `item` releases the `AnyView` reference count.  When nothing
//!    else holds the entity (stores with weak refs upgrade-fail and become
//!    no-ops), the entity is deallocated by GPUI's `EntityMap`.
//! 4. (Phase 2) Dropping a `StreamHandle` calls its `Drop::drop`, which fires
//!    the cancellation callback and stops the in-flight stream.
//!
//! This one-drop property is the structural enforcement of LD-18 ("no detach")
//! and the elimination of the audit's detached-subscription hazard (§3.2 of
//! GUI-PLAN).  It requires **no** state in `Pane` beyond `Vec<ItemSlot>`.
//!
//! # Leaf-animation contract (GUI-PLAN §4.2, §1.1.2)
//!
//! The active-tab underline is a 2 px absolutely-positioned quad whose x-offset
//! is driven by a `Motion` (SNAPPY spring).  The spring lives in `Pane` state
//! and is ticked in `Pane::render`.  `window.request_animation_frame()` is
//! called only while the spring is unsettled, so a sliding underline animates
//! *alone* — the symbol page, logs panel, and root `Shell` render zero frames.
//! Do not move the spring or the tick call to a parent entity.
//!
//! # Tab reorder (sibling springs)
//!
//! Drag-reorder uses a `Vec<TabSpring>`, one per slot, driven as `Motion`
//! DEFAULT springs.  During a drag the dragged tab follows the pointer 1:1;
//! on mouse-up the slots reorder and the siblings spring to their new positions.
//! Each spring also lives in this entity — same leaf-animation guarantee.

use std::num::NonZeroU32;
use std::time::Instant;

use gpui::{
    AnyElement, AnyView, App, AppContext as _, Context, Element, ElementId, Entity, EventEmitter,
    FocusHandle, Focusable, IntoElement, MouseButton, MouseDownEvent, ParentElement, Render, SharedString, Styled, Subscription, Window, div, px,
    prelude::FluentBuilder as _,
};

use crate::motion::spring::{Motion, Spring};
use crate::app::actions::{
    ActivateTab1, ActivateTab2, ActivateTab3, ActivateTab4, ActivateTab5, ActivateTab6,
    ActivateTab7, ActivateTab8, ActivateTab9, CloseTab,
};
use crate::theme::ext::{Provenance, ThemeExtAccessor as _};
use crate::workspace::item::WorkspaceItem;
use gpui::prelude::*;

// ─────────────────────────────────────────────────────────────────────────────
// TabId
// ─────────────────────────────────────────────────────────────────────────────

/// A strongly-typed tab identifier (GUI-LOCAL-PLAN §L7.2).
///
/// `NonZeroU32` is used so `Option<TabId>` is pointer-sized (no discriminant
/// byte wasted), and `NonZero` prevents the zero sentinel from escaping.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TabId(NonZeroU32);

impl TabId {
    pub(crate) fn new(n: u32) -> Option<Self> {
        NonZeroU32::new(n).map(TabId)
    }
}

/// Source for tab ids.  One per `Pane`.
struct TabIdSource(u32);

impl TabIdSource {
    fn next(&mut self) -> TabId {
        self.0 += 1;
        TabId::new(self.0).expect("tab id overflow (u32)")
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// ItemSlot — the one-drop container (§3.2)
// ─────────────────────────────────────────────────────────────────────────────

// ─────────────────────────────────────────────────────────────────────────────
// TabItem — the erased [`WorkspaceItem`]
// ─────────────────────────────────────────────────────────────────────────────

/// What a `Pane` needs from a tab once the concrete view type is forgotten.
///
/// # Why this exists rather than a bare `AnyView`
///
/// `AnyView` is enough to *render* a tab's content and nothing else. It cannot
/// answer "what should the tab button say?", because [`WorkspaceItem`] is not
/// object-safe through `AnyView` — there is no downcast back to the trait.
///
/// A pane holding only `AnyView`s therefore has to invent tab labels, and the
/// invention is always the same one: `"Tab 1"`, `"Tab 2"`. That is not a
/// placeholder that gets replaced later; it is a structural dead end, because
/// nothing in the pane's types will ever complain about it.
///
/// So the erasure happens one level up. [`Pane::open_item`] takes an
/// `Entity<V>` where `V: WorkspaceItem`, and the blanket impl below is the only
/// way to produce a `Box<dyn TabItem>` — which means every tab that exists has
/// a real label by construction.
pub trait TabItem: 'static {
    /// The element drawn inside the tab button (§13.4).
    fn tab_content(&self, cx: &App) -> AnyElement;

    /// The content view, for the pane's body.
    fn view(&self) -> AnyView;

    /// The screen class, for the perf harness (§25).
    fn telemetry_id(&self, cx: &App) -> &'static str;

    /// Trust chrome for this tab's content (LD-8).
    fn provenance(&self, cx: &App) -> Provenance;

    /// The item's own `FocusHandle` (L16 — see `WorkspaceItem`'s doc comment).
    ///
    /// `Pane::activate_ix` calls this on every activation (open, click,
    /// `ActivateTabN`) and hands the result to `window.focus`, so a tab's own
    /// `.on_action` handlers are reachable the moment it becomes active — the
    /// same mechanism that already made `ActivateTab2` reachable on `Pane`
    /// itself, extended one level down to the content it switches to.
    fn focus_handle(&self, cx: &App) -> FocusHandle;
}

impl<V> TabItem for Entity<V>
where
    V: WorkspaceItem + Render,
{
    fn tab_content(&self, cx: &App) -> AnyElement {
        self.read(cx).tab_content(cx)
    }

    fn view(&self) -> AnyView {
        self.clone().into()
    }

    fn telemetry_id(&self, cx: &App) -> &'static str {
        self.read(cx).telemetry_id()
    }

    fn provenance(&self, cx: &App) -> Provenance {
        self.read(cx).provenance()
    }

    fn focus_handle(&self, cx: &App) -> FocusHandle {
        // `WorkspaceItem: Focusable` and GPUI provides a blanket
        // `impl<V: Focusable> Focusable for Entity<V>` — fully qualified so
        // this does not recurse into `TabItem::focus_handle` itself (both
        // methods share a name).
        Focusable::focus_handle(self, cx)
    }
}

/// A single open tab.
///
/// **Structural guarantee:** dropping this value closes the tab completely.
///
/// - `item` is the erased content view.  When the count drops to zero (no other
///   entity holds a strong ref), GPUI frees the view.
/// - `_subs` is the set of `Subscription`s this tab owns.  Dropping them
///   unregisters the callbacks without any manual cleanup call.  No
///   `.detach()` is legal here — LD-18.
/// - `id` is stable across reorders (the index changes; the id does not).
///
/// Add `stream_handle: Option<StreamHandle>` here when Phase 2 wires up
/// the async kernel — its `Drop` will cancel in-flight I/O automatically,
/// extending the one-drop guarantee to network work.
pub struct ItemSlot {
    /// The tab's stable identity.
    pub id: TabId,
    /// The content view, with its concrete type erased but its tab label kept.
    pub item: Box<dyn TabItem>,
    /// All subscriptions this tab holds.  Dropped with the slot — LD-18.
    pub _subs: Vec<Subscription>,
}

/// Which slot becomes active after the tab at `closed_ix` is removed.
///
/// `remaining` is the slot count *after* removal.
///
/// # Why this is a free function
///
/// Activation policy is the subtle part of closing a tab — closing the active
/// one, closing a tab left of the active one, and closing the last tab all need
/// different answers, and getting any of them wrong is the kind of bug you only
/// notice as "the wrong page appeared". It lives out here, taking plain
/// integers, so it can be tested directly.
///
/// That matters more than it looks: the alternative is a test double that
/// *reimplements* this arithmetic beside the real pane, which passes happily
/// while proving nothing about the code that actually ships.
pub(crate) fn active_ix_after_close(
    closed_ix: usize,
    active_ix: usize,
    remaining: usize,
) -> usize {
    if remaining == 0 {
        // Nothing left to activate.
        0
    } else if closed_ix == active_ix {
        // Closed the active tab: prefer the left neighbour, else the tab that
        // slid into this index, clamped to the new end.
        closed_ix.saturating_sub(1).min(remaining - 1)
    } else if closed_ix < active_ix {
        // A tab before the active one vanished; shift down to keep the *same
        // content* active rather than the same index.
        active_ix - 1
    } else {
        // Closed to the right of the active tab — nothing moves.
        active_ix
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// DragState
// ─────────────────────────────────────────────────────────────────────────────

/// In-progress drag state for tab reordering.
///
/// While `Some`, the dragged tab follows the pointer 1:1 (no spring).
/// On mouse-up, slots reorder and siblings spring to their new positions.
struct DragState {
    /// Index of the tab being dragged (into `slots`).
    dragging_ix: usize,
    /// Pointer offset from the tab's leading edge, for correct positioning.
    pointer_offset_x: f32,
    /// Current pointer x in the tab-strip coordinate space.
    pointer_x: f32,
}

// ─────────────────────────────────────────────────────────────────────────────
// Activation
// ─────────────────────────────────────────────────────────────────────────────

/// Whether activating a tab should also move the keyboard into it.
///
/// # Why this is not a `bool`, and not implicit
///
/// Window focus is a *window*-wide resource with exactly one holder, so
/// "activate this tab" and "take the keyboard" are two different requests that
/// happened to be spelled the same way. `Pane::activate_ix` used to make both
/// unconditionally, which is right for a keybinding and wrong for every
/// activation the user did not ask for:
///
/// * `Shell::reveal_document` activates twice for a background open (alt-Enter)
///   while the omni-search overlay is still up on purpose. Seizing focus there
///   sent the next `Escape` to a `SymbolPage` that had no handler for it, and
///   the overlay became unclosable from the keyboard — the exact LD-13 failure
///   ("an overlay that opens and cannot be closed is worse than one that never
///   opened"), reached from the other direction.
/// * Restoring the previously-active tab after such an open is pure
///   bookkeeping; there is no user intent in it at all.
///
/// Naming the two cases makes the choice appear at every call site, so a new
/// activation path cannot inherit the wrong one by default — AGENTS-DOCTRINE §3.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Activation {
    /// The user asked for this tab (a keybinding, a click on the strip, a hit
    /// committed with Enter). Move the keyboard into its content, so the page's
    /// own `.on_action` handlers are reachable (L16).
    Focus,
    /// The tab set changed underneath the user — a background open, or putting
    /// the previously-active tab back after one. Leave the keyboard exactly
    /// where it is.
    Preserve,
}

// ─────────────────────────────────────────────────────────────────────────────
// Pane
// ─────────────────────────────────────────────────────────────────────────────

/// Open tabs and their lifecycle.
///
/// `Pane` is a GPUI `Entity`.  It owns:
/// - `slots: Vec<ItemSlot>` — the tab list; dropping an entry closes a tab.
/// - `active_ix: usize` — current active tab index.
/// - `underline: Motion` — SNAPPY spring for the active-tab underline x-offset.
/// - `tab_springs: Vec<Motion>` — DEFAULT springs for sibling reorder.
/// - `drag: Option<DragState>` — in-flight drag metadata.
///
/// All motion state lives here — no parent entity is involved.
pub struct Pane {
    slots: Vec<ItemSlot>,
    active_ix: usize,

    // ── Leaf-motion state (GUI-PLAN §4.2 contract) ───────────────────────────
    /// X-offset of the active-tab underline, SNAPPY spring.
    underline_x: Motion,
    /// Width of the active-tab underline, SNAPPY spring (tabs can differ in width).
    underline_w: Motion,
    /// Per-slot x-offset springs for reorder choreography, DEFAULT spring.
    tab_springs: Vec<Motion>,

    // ── Drag state ───────────────────────────────────────────────────────────
    drag: Option<DragState>,

    // ── Misc ─────────────────────────────────────────────────────────────────
    id_source: TabIdSource,
    focus: FocusHandle,
}

impl Pane {
    /// Construct an empty pane.
    pub fn new(_: &mut Window, cx: &mut Context<Self>) -> Self {
        Self {
            slots: Vec::new(),
            active_ix: 0,
            underline_x: Motion::new(0.0, Spring::SNAPPY),
            underline_w: Motion::new(0.0, Spring::SNAPPY),
            tab_springs: Vec::new(),
            drag: None,
            id_source: TabIdSource(0),
            focus: cx.focus_handle(),
        }
    }

    // ── Public API ────────────────────────────────────────────────────────────

    /// Open a new tab containing `item` and activate it.
    ///
    /// Takes the concrete `Entity<V>` rather than an `AnyView` so the tab label
    /// comes from the item itself — see [`TabItem`] for why that is a type-level
    /// concern and not a convenience.
    ///
    /// `activation` decides whether the keyboard follows; see [`Activation`].
    ///
    /// Returns the stable [`TabId`] for the new tab.
    pub fn open_item<V>(
        &mut self,
        item: Entity<V>,
        subs: Vec<Subscription>,
        activation: Activation,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> TabId
    where
        V: WorkspaceItem + Render,
    {
        let id = self.id_source.next();
        self.slots.push(ItemSlot {
            id,
            item: Box::new(item),
            _subs: subs,
        });
        self.tab_springs.push(Motion::new(0.0, Spring::DEFAULT));
        let ix = self.slots.len() - 1;
        // Routed through `activate_ix` rather than setting `self.active_ix`
        // directly: that is the one place activation also moves window focus
        // onto the new item (L16), and a newly opened tab is exactly the case
        // that bug was found in.
        self.activate_ix(ix, activation, window, cx);
        id
    }

    /// Close the tab identified by `id`.
    ///
    /// Dropping the `ItemSlot` is the teardown: subscriptions are unregistered,
    /// the `AnyView` ref-count is decremented, and (future) any in-flight
    /// `StreamHandle` is cancelled.
    ///
    /// After close, activation moves to the nearest sensible neighbour:
    /// - if there was a tab to the left of the closed one, that becomes active;
    /// - otherwise the tab to the right (which now occupies the same index);
    /// - otherwise the pane is empty.
    pub fn close_tab(&mut self, id: TabId, window: &mut Window, cx: &mut Context<Self>) {
        let Some(ix) = self.slots.iter().position(|s| s.id == id) else {
            return;
        };
        // One drop does everything: subs, view, future stream handle.
        self.slots.remove(ix);
        self.tab_springs.remove(ix);

        self.active_ix = active_ix_after_close(ix, self.active_ix, self.slots.len());
        // Re-focus the resulting active tab (L16). Closing a tab can change
        // *which* tab is active without going through `activate_ix` — most
        // often a no-op (closing the tab that was itself just focused, whose
        // neighbour now occupies the same window focus target it already
        // had), but not always: `Shell::reveal_document`'s Replace-disposition
        // cleanup closes the *outgoing* tab, at an index left of the newly
        // active one, which shifts the active tab's *index* without ever
        // calling `activate_ix` for it. Leaving focus wherever it happened to
        // be would be exactly the kind of implicit-and-therefore-wrong state
        // this file's whole `activate_ix` doc comment exists to rule out.
        if let Some(slot) = self.slots.get(self.active_ix) {
            let focus = slot.item.focus_handle(cx);
            window.focus(&focus, cx);
        }
    }

    /// Activate the tab at the given slot index.
    ///
    /// # Focus, not just index (L16)
    ///
    /// Setting `active_ix` alone used to be the whole method. That left the
    /// window's focused element wherever it was before — usually `Pane`'s own
    /// root, from the last time nothing more specific had focus — so any
    /// `.on_action` handler the newly active item registered on its own root
    /// div was structurally unreachable: GPUI's `dispatch_action` only walks
    /// the ancestor chain of the *focused* element, and an unfocused
    /// descendant is not on that chain.
    ///
    /// Every activation path — `open_item`, `activate_tab`, `activate_action`
    /// (the `ActivateTabN` bindings), and the tab-strip click handler in
    /// `render` — funnels through here, so fixing it once here fixes all of
    /// them. `Pane`'s own actions (`ActivateTabN`, `CloseTab`) keep working
    /// after this, because they are registered on `Pane`'s root div, an
    /// *ancestor* of the newly focused item — `dispatch_action` walks the
    /// whole chain upward, not just the leaf.
    ///
    /// # …and not *always* focus, either
    ///
    /// The L16 fix then took the opposite thing for granted: that every
    /// activation is one the user asked for. `Shell::reveal_document` activates
    /// a tab twice for a background open (once implicitly via `open_item`, once
    /// to put the reader back), while the omni-search overlay is deliberately
    /// still on screen — so alt-Enter silently moved the keyboard out of the
    /// overlay and the next `Escape` reached nothing at all. The screenshot
    /// suite caught it as `23-escape-clears-query-first: only 0 of 5184000
    /// pixels changed`.
    ///
    /// [`Activation`] is why that is now a decision a caller has to make rather
    /// than a property of which method it happened to reach.
    pub fn activate_ix(
        &mut self,
        ix: usize,
        activation: Activation,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(slot) = self.slots.get(ix) {
            self.active_ix = ix;
            if activation == Activation::Focus {
                let focus = slot.item.focus_handle(cx);
                window.focus(&focus, cx);
            }
        }
    }

    /// Activate the tab with the given id.
    pub fn activate_tab(
        &mut self,
        id: TabId,
        activation: Activation,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(ix) = self.slots.iter().position(|s| s.id == id) {
            self.activate_ix(ix, activation, window, cx);
        }
    }

    fn activate_action(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        let previous = self.active_id();
        // A keybinding or a tab click: the user is asking to read this tab, so
        // the keyboard goes with it.
        self.activate_ix(ix, Activation::Focus, window, cx);
        if self.active_id() != previous {
            cx.emit(PaneEvent::ActiveTabChanged {
                id: self.active_id(),
            });
            cx.notify();
        }
    }

    fn close_active(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(id) = self.active_id() else { return };
        self.close_tab(id, window, cx);
        cx.emit(PaneEvent::TabClosed { id });
        cx.emit(PaneEvent::ActiveTabChanged {
            id: self.active_id(),
        });
        cx.notify();
    }

    /// The currently active content view, if any tab is open.
    pub fn active_item(&self) -> Option<AnyView> {
        self.slots.get(self.active_ix).map(|s| s.item.view())
    }

    /// The id of the currently active tab.
    pub fn active_id(&self) -> Option<TabId> {
        self.slots.get(self.active_ix).map(|s| s.id)
    }

    /// The `FocusHandle` of the active tab's *content*, if a tab is open.
    ///
    /// Exposed because "did activating this tab really move window focus into
    /// its content?" is otherwise unanswerable from outside the pane, and the
    /// answer is load-bearing. `activate_ix` focuses this handle; if the item
    /// then renders without attaching it (`track_focus`), GPUI cannot find it
    /// in the rendered dispatch tree and silently falls back to the *window
    /// root*, which carries no key context — so every ancestor binding,
    /// including this pane's own `CloseTab` / `ActivateTabN`, stops resolving.
    /// That failure is invisible in a screenshot and silent at the keystroke;
    /// asserting on the handle is the only way to catch it. See
    /// `views::symbol_page::SymbolPage::page_root` (LIMITATIONS.md L22).
    pub fn active_item_focus_handle(&self, cx: &App) -> Option<FocusHandle> {
        self.slots
            .get(self.active_ix)
            .map(|slot| slot.item.focus_handle(cx))
    }

    /// Number of open tabs.
    pub fn len(&self) -> usize {
        self.slots.len()
    }

    /// True if no tabs are open.
    pub fn is_empty(&self) -> bool {
        self.slots.is_empty()
    }

    // ── Private helpers ────────────────────────────────────────────────────────

    /// Move a tab from `from_ix` to `to_ix`, maintaining active item identity.
    fn reorder(&mut self, from_ix: usize, to_ix: usize) {
        if from_ix == to_ix
            || from_ix >= self.slots.len()
            || to_ix >= self.slots.len()
        {
            return;
        }
        // Track which id was active so we can restore it after the move.
        let active_id = self.active_id();
        let slot = self.slots.remove(from_ix);
        let spring = self.tab_springs.remove(from_ix);
        self.slots.insert(to_ix, slot);
        self.tab_springs.insert(to_ix, spring);
        // Restore active index by id.
        if let Some(aid) = active_id {
            self.active_ix = self
                .slots
                .iter()
                .position(|s| s.id == aid)
                .unwrap_or(0);
        }
    }

    /// Compute the target x-offset and width for the underline under `active_ix`.
    ///
    /// Uses a fixed tab width estimate because actual element sizes are not
    /// available until layout.  Actual pixel widths will be fed back in Phase 2
    /// via `with_element_state`.  For now the spring's visual accuracy is
    /// sufficient: the motion communicates direction, and the end state is exact.
    fn underline_target(active_ix: usize, tab_count: usize) -> (f32, f32) {
        // Approximate: 120 px per tab (enough for typical symbol names).
        // TODO(views): replace with measured tab widths via `with_element_state`.
        let approx_tab_w = 120.0_f32;
        let x = active_ix as f32 * approx_tab_w;
        let w = approx_tab_w.min(approx_tab_w);
        let _ = tab_count; // used in the approximation formula above
        (x, w)
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// EventEmitter
// ─────────────────────────────────────────────────────────────────────────────

/// Events emitted by `Pane` to its parent (`Shell`).
pub enum PaneEvent {
    /// The active tab changed.  Parent may want to update the nav history.
    ActiveTabChanged { id: Option<TabId> },
    /// A tab was closed.  Parent removes it from nav history if present.
    TabClosed { id: TabId },
}

impl EventEmitter<PaneEvent> for Pane {}

// ─────────────────────────────────────────────────────────────────────────────
// Focusable
// ─────────────────────────────────────────────────────────────────────────────

impl Focusable for Pane {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Render
// ─────────────────────────────────────────────────────────────────────────────

impl Render for Pane {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // ── §4.2 render-loop contract ──────────────────────────────────────────
        // Tick both springs.  `request_animation_frame` only if still moving.
        let now = Instant::now();
        let mut animating = self.underline_x.tick(now);
        animating |= self.underline_w.tick(now);
        for s in &mut self.tab_springs {
            animating |= s.tick(now);
        }
        let reduced = cx.theme_ext().reduced_motion();
        if animating && !reduced {
            window.request_animation_frame();
        }

        // ── Retarget underline spring ──────────────────────────────────────────
        // Compute targets from the current active index and retarget if needed.
        let (target_x, target_w) = Self::underline_target(self.active_ix, self.slots.len());
        if (self.underline_x.target() - target_x).abs() > 0.5 {
            if reduced {
                self.underline_x.snap_to(target_x);
            } else {
                self.underline_x.animate_to(target_x);
            }
        }
        if (self.underline_w.target() - target_w).abs() > 0.5 {
            if reduced {
                self.underline_w.snap_to(target_w);
            } else {
                self.underline_w.animate_to(target_w);
            }
        }

        let underline_x_val = self.underline_x.value();
        let underline_w_val = self.underline_w.value();

        let theme = cx.theme_ext().clone();
        let slots_len = self.slots.len();
        let active_ix = self.active_ix;

        // ── Tab strip ──────────────────────────────────────────────────────────
        // The strip is a horizontal flex row.  Each tab button is a child.
        // The active underline is a 2 px absolutely-positioned quad layered below.
        //
        // LD-6: the tab list is bounded in practice (rarely more than 10–15
        // tabs), but we still keep the strip simple and non-virtualised.
        // If the count exceeds the visible width, an overflow chevron appears.
        // Rows inside item content go through uniform_list (not our concern here).
        let tab_strip = div()
            .id("pane-tab-strip")
            .relative()
            .flex()
            .flex_row()
            .border_b_1()
            .border_color(theme.colours.border_default)
            .bg(theme.colours.bg_raised)
            .children(
                self.slots
                    .iter()
                    .enumerate()
                    .map(|(ix, slot)| {
                        let tab_id = slot.id;
                        let is_active = ix == active_ix;
                        let entity = cx.entity();

                        // Each tab is a button-like div with hover reveal of close button.
                        div()
                            .id(ElementId::Integer(tab_id.0.get() as u64))
                            .flex()
                            .flex_row()
                            .items_center()
                            .gap(theme.space.space_1)
                            .px(theme.space.space_2)
                            .py(theme.space.space_1)
                            .cursor_pointer()
                            // Hover tint (§5.1) — GPUI built-in, zero cost, no notify.
                            .hover(|s| s.bg(theme.colours.bg_hover))
                            .when(is_active, |s| {
                                s.text_color(theme.colours.fg_default)
                            })
                            .when(!is_active, |s| {
                                s.text_color(theme.colours.fg_muted)
                            })
                            // Activate on click.
                            .on_mouse_down(
                                MouseButton::Left,
                                {
                                    let entity = entity.clone();
                                    move |_: &MouseDownEvent, window, cx| {
                                        entity.update(cx, |pane, cx| {
                                            // A direct click on the strip is
                                            // the user asking to read this tab.
                                            pane.activate_ix(
                                                ix,
                                                Activation::Focus,
                                                window,
                                                cx,
                                            );
                                            cx.emit(PaneEvent::ActiveTabChanged {
                                                id: pane.active_id(),
                                            });
                                            cx.notify();
                                        });
                                    }
                                },
                            )
                            // Middle-click closes (§13.4).
                            .on_mouse_down(
                                MouseButton::Middle,
                                {
                                    let entity = entity.clone();
                                    move |_: &MouseDownEvent, window, cx| {
                                        entity.update(cx, |pane, cx| {
                                            pane.close_tab(tab_id, window, cx);
                                            cx.emit(PaneEvent::TabClosed { id: tab_id });
                                            cx.notify();
                                        });
                                    }
                                },
                            )
                            // The label is the item's own (§13.4): icon, title,
                            // provenance dot, and any streaming count. `TabItem`
                            // keeps that reachable after type erasure, so a pane
                            // can never invent a name for content it holds.
                            .child(slot.item.tab_content(cx))
                            // Close button — revealed by group-hover (§13.4).
                            .child(
                                div()
                                    .id(ElementId::Integer(
                                        (tab_id.0.get() as u64) << 16 | 0xFFFF,
                                    ))
                                    .w(theme.space.space_3)
                                    .h(theme.space.space_3)
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .rounded(theme.space.r_sm)
                                    // Hover tint on the close button itself.
                                    .hover(|s| s.bg(theme.colours.bg_active))
                                    .cursor_pointer()
                                    .on_mouse_down(
                                        MouseButton::Left,
                                        {
                                            let entity = entity.clone();
                                            move |_ev: &MouseDownEvent, window, cx| {
                                                // Stop propagation so the tab isn't also activated.
                                                cx.stop_propagation();
                                                entity.update(cx, |pane, cx| {
                                                    pane.close_tab(tab_id, window, cx);
                                                    cx.emit(PaneEvent::TabClosed { id: tab_id });
                                                    cx.notify();
                                                });
                                            }
                                        },
                                    )
                                    // × glyph for the close button.
                                    .child(
                                        div()
                                            .text_size(theme.type_scale.dense.size)
                                            .text_color(theme.colours.fg_faint)
                                            .child("×"),
                                    ),
                            )
                    }),
            );

        // ── Active underline (leaf animation, §5.3 `tab.switch`) ───────────────
        // A 2 px absolutely-positioned quad sliding under the active tab.
        // Lives at the leaf — no parent entity is notified.
        let underline = div()
            .absolute()
            .bottom(px(0.0))
            .left(px(underline_x_val))
            .w(px(underline_w_val))
            .h(px(2.0))
            .bg(theme.colours.accent);

        // ── Content area ───────────────────────────────────────────────────────
        let content = if let Some(slot) = self.slots.get(active_ix) {
            div()
                .flex_1()
                .overflow_hidden()
                .child(slot.item.view())
                .into_any_element()
        } else {
            // Empty pane — designed empty state (LD-9/LD-16).
            div()
                .flex_1()
                .flex()
                .items_center()
                .justify_center()
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .items_center()
                        .gap(theme.space.space_2)
                        .child(
                            div()
                                .text_size(theme.type_scale.ui.size)
                                .text_color(theme.colours.fg_muted)
                                // TODO(views): replace with EmptyState component once ui/ is complete.
                                .child("Open a symbol to get started"),
                        ),
                )
                .into_any_element()
        };

        // ── Full pane layout ───────────────────────────────────────────────────
        div()
            .id("pane")
            .key_context("Pane")
            .flex()
            .flex_col()
            .size_full()
            .overflow_hidden()
            .track_focus(&self.focus)
            .on_action(cx.listener(|pane, _: &CloseTab, window, cx| {
                pane.close_active(window, cx);
            }))
            .on_action(cx.listener(|pane, _: &ActivateTab1, window, cx| {
                pane.activate_action(0, window, cx);
            }))
            .on_action(cx.listener(|pane, _: &ActivateTab2, window, cx| {
                pane.activate_action(1, window, cx);
            }))
            .on_action(cx.listener(|pane, _: &ActivateTab3, window, cx| {
                pane.activate_action(2, window, cx);
            }))
            .on_action(cx.listener(|pane, _: &ActivateTab4, window, cx| {
                pane.activate_action(3, window, cx);
            }))
            .on_action(cx.listener(|pane, _: &ActivateTab5, window, cx| {
                pane.activate_action(4, window, cx);
            }))
            .on_action(cx.listener(|pane, _: &ActivateTab6, window, cx| {
                pane.activate_action(5, window, cx);
            }))
            .on_action(cx.listener(|pane, _: &ActivateTab7, window, cx| {
                pane.activate_action(6, window, cx);
            }))
            .on_action(cx.listener(|pane, _: &ActivateTab8, window, cx| {
                pane.activate_action(7, window, cx);
            }))
            .on_action(cx.listener(|pane, _: &ActivateTab9, window, cx| {
                pane.activate_action(8, window, cx);
            }))
            // Tab strip with the sliding underline.
            .child(
                div()
                    .relative()
                    .flex_none()
                    .child(tab_strip)
                    .when(slots_len > 0, |s| s.child(underline)),
            )
            // Content fills the rest.
            .child(content)
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Tests
// ─────────────────────────────────────────────────────────────────────────────


#[cfg(test)]
mod tests {
    use super::*;

    /// Tab ids are handed out strictly increasing and never reuse a value.
    ///
    /// Reuse would be silent corruption: a closed tab's id landing on a new tab
    /// makes stale nav-history entries and stale stream events address the wrong
    /// content.
    #[test]
    fn tab_ids_are_unique_and_increasing() {
        let mut src = TabIdSource(0);
        let ids: Vec<TabId> = (0..64).map(|_| src.next()).collect();
        for pair in ids.windows(2) {
            assert!(pair[1].0 > pair[0].0, "tab ids must strictly increase");
        }
        let unique: std::collections::HashSet<_> = ids.iter().collect();
        assert_eq!(unique.len(), ids.len(), "tab ids must never repeat");
    }

    /// Closing the active tab activates its left neighbour.
    #[test]
    fn closing_active_tab_activates_left_neighbour() {
        // [a, b, c] active = c(2); close c -> [a, b], active should be b(1).
        assert_eq!(active_ix_after_close(2, 2, 2), 1);
        // [a, b, c] active = b(1); close b -> [a, c], active should be a(0).
        assert_eq!(active_ix_after_close(1, 1, 2), 0);
    }

    /// Closing the *first* tab while it is active falls through to the tab that
    /// slid into index 0, rather than underflowing.
    #[test]
    fn closing_first_active_tab_activates_the_new_first() {
        assert_eq!(active_ix_after_close(0, 0, 2), 0);
    }

    /// Closing a tab to the left of the active one keeps the same *content*
    /// active, which means the index must shift down.
    #[test]
    fn closing_left_of_active_keeps_same_content_active() {
        // [a, b, c] active = c(2); close a -> [b, c], c is now index 1.
        assert_eq!(active_ix_after_close(0, 2, 2), 1);
    }

    /// Closing a tab to the right of the active one changes nothing.
    #[test]
    fn closing_right_of_active_does_not_move_activation() {
        // [a, b, c] active = a(0); close c -> [a, b], a is still index 0.
        assert_eq!(active_ix_after_close(2, 0, 2), 0);
    }

    /// Closing the last remaining tab leaves a valid (if empty) index.
    #[test]
    fn closing_the_last_tab_yields_index_zero() {
        assert_eq!(active_ix_after_close(0, 0, 0), 0);
    }

    /// Whatever the inputs, the result must be a valid index into the remaining
    /// slots — an out-of-range `active_ix` is an immediate panic in `render`.
    #[test]
    fn result_is_always_in_bounds() {
        for remaining in 0..8usize {
            let before = remaining + 1;
            for closed_ix in 0..before {
                for active_ix in 0..before {
                    let got = active_ix_after_close(closed_ix, active_ix, remaining);
                    if remaining == 0 {
                        assert_eq!(got, 0);
                    } else {
                        assert!(
                            got < remaining,
                            "close({closed_ix}) with active={active_ix}, \
                             remaining={remaining} gave out-of-range index {got}"
                        );
                    }
                }
            }
        }
    }
}
