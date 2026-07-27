//! Omni-search overlay — GUI-PLAN §15.
//!
//! The homepage of lindsey: `cmd-K` opens a centred 640 px overlay with
//! sectioned Name / Type / Semantic results, local hits in under 10 ms, typed
//! signature previews on every row, trust badges on every row, and a selection
//! bar that *glides* between rows on a SNAPPY spring (the single most-felt
//! polish detail in the app — §15 motion spec).
//!
//! # `SearchStore` contract (TODO(store))
//!
//! `OmniSearch` codes against the following trait.  The concrete `SearchStore`
//! entity (written separately) must satisfy it:
//!
//! ```text
//! /// The three search sections in result order.
//! pub const SECTION_NAME:     SearchSectionId = SearchSectionId(0);
//! pub const SECTION_TYPE:     SearchSectionId = SearchSectionId(1);
//! pub const SECTION_SEMANTIC: SearchSectionId = SearchSectionId(2);
//!
//! pub trait SearchStoreRead {
//!     /// The current query text (SharedString, already in store state).
//!     fn input(&self) -> &SharedString;
//!     /// Current mode chip selection.
//!     fn mode(&self) -> SearchMode;
//!     /// Rows for the given section.  Pre-sorted by score.  Zero-copy slice.
//!     fn section_rows(&self, id: SearchSectionId) -> &[HitRow];
//!     /// Pre-formatted row count label for a section (e.g. "247").
//!     fn section_count_label(&self, id: SearchSectionId) -> &SharedString;
//!     /// Per-section latency label (e.g. "4 ms"), empty if not yet known.
//!     fn section_latency_label(&self, id: SearchSectionId) -> &SharedString;
//!     /// Whether the semantic section is still loading (shimmer needed).
//!     fn semantic_loading(&self) -> bool;
//!     /// Whether the backend is offline (semantic section replaced by notice).
//!     fn is_offline(&self) -> bool;
//!     /// Current flat global selection index (None = nothing selected).
//!     fn selection_flat(&self) -> Option<usize>;
//!     /// The generation of the current result set (drives entrance windows).
//!     fn current_gen(&self) -> Gen;
//!     /// Instant the current generation's first page arrived.
//!     fn gen_first_arrived(&self) -> Option<Instant>;
//!     /// Recent symbols / recent searches to show when input is empty.
//!     fn recents(&self) -> &[HitRow];
//! }
//!
//! pub trait SearchStoreWrite {
//!     fn set_input(&mut self, text: SharedString, cx: &mut Context<Self>);
//!     fn set_mode(&mut self, mode: SearchMode, cx: &mut Context<Self>);
//!     fn move_selection(&mut self, delta: SelectionDelta, cx: &mut Context<Self>);
//!     fn commit_selection(&mut self, disposition: OpenDisposition, cx: &mut Context<Self>);
//!     fn jump_to_section(&mut self, section: SearchSectionId, cx: &mut Context<Self>);
//!     fn tab_next_section(&mut self, cx: &mut Context<Self>);
//! }
//!
//! pub enum SelectionDelta { Up, Down }
//! pub enum OpenDisposition { Replace, KeepOverlay, BackgroundTab }
//! ```
//!
//! The entity handle type used below is `Entity<SearchStore>` — substitute the
//! real type when `SearchStore` lands.

use std::time::Instant;

use gpui::{
    AnyElement, App, Context, Element, Entity, FocusHandle,
    Focusable, IntoElement, Keystroke, ParentElement, Render, SharedString, Styled, Window,
    actions, div, prelude::FluentBuilder as _, px, uniform_list,
};
use gpui::prelude::*;
use gpui::{UniformListScrollHandle, ScrollStrategy};
use gpui_component::{
    ActiveTheme as _, IconName, h_flex, v_flex,
};

use crate::bridge::generation::Gen;
use crate::motion::{
    Motion, Spring,
    entrance_id, row_enter, shimmer,
    MotionTokens, ROW_CASCADE_WINDOW,
};
use crate::theme::ext::{Provenance, ThemeExtAccessor as _};
use crate::theme::kind::LocalKindDiscriminant;
use crate::ui::{
    Badge, EmptyState, KeyHint, SectionHeader, SignatureLine,
    count_label::{count_rounds, should_skip},
};
use nudox_engine::wire::{HitRow, KindTag, SearchSectionId, SigToken as WireSigToken};

// ── Actions (LD-13 / Appendix B) ─────────────────────────────────────────────

actions!(
    omni_search,
    [
        SelectionUp,
        SelectionDown,
        TabNextSection,
        CommitSelection,
        CommitSelectionKeepOverlay,
        CommitSelectionBackgroundTab,
        ClearThenClose,
        JumpToSection1,
        JumpToSection2,
        JumpToSection3,
    ]
);

// ── Section constants ─────────────────────────────────────────────────────────

/// Name-search section (local, fast).
pub const SECTION_NAME: SearchSectionId = SearchSectionId(0);
/// Type-search section (local, fast).
pub const SECTION_TYPE: SearchSectionId = SearchSectionId(1);
/// Semantic / vector search section (slower, may be async).
pub const SECTION_SEMANTIC: SearchSectionId = SearchSectionId(2);

// ── SearchMode chip ───────────────────────────────────────────────────────────

/// The mode chip selection on the omni-search input row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SearchMode {
    Auto,
    Name,
    Type,
    Semantic,
}

impl SearchMode {
    fn label(self) -> &'static str {
        match self {
            Self::Auto => "Auto",
            Self::Name => "Name",
            Self::Type => "Type",
            Self::Semantic => "Semantic",
        }
    }
    fn all() -> &'static [SearchMode] {
        &[Self::Auto, Self::Name, Self::Type, Self::Semantic]
    }
}

// ── Open disposition ─────────────────────────────────────────────────────────

/// How to open the selected hit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OpenDisposition {
    /// Navigate to the symbol, close the overlay.
    Replace,
    /// Navigate to the symbol, keep the overlay open (cmd-enter).
    KeepOverlay,
    /// Open in a background tab, keep focus here (alt-enter).
    BackgroundTab,
}

// ── Selection movement ────────────────────────────────────────────────────────

/// Direction for `move_selection`.
#[derive(Debug, Clone, Copy)]
pub enum SelectionDelta {
    Up,
    Down,
}

// ── TODO(store) facade ───────────────────────────────────────────────────────
//
// OmniSearch reads from the store via `store.read(cx)` which gives us a `&SearchStore`.
// We define the methods we expect below; the compiler will tell us if the real
// SearchStore doesn't have them when the dependency is wired.
//
// For now we define a placeholder type so the file compiles standalone.

/// Placeholder until the real `SearchStore` lands.
///
/// TODO(store): delete this struct and replace `Entity<StubSearchStore>` with
/// `Entity<SearchStore>`.  Every method listed here must be present on the real store.
pub struct StubSearchStore {
    pub input: SharedString,
    pub mode: SearchMode,
    pub name_rows: Vec<HitRow>,
    pub type_rows: Vec<HitRow>,
    pub semantic_rows: Vec<HitRow>,
    pub name_count: SharedString,
    pub type_count: SharedString,
    pub semantic_count: SharedString,
    pub name_latency: SharedString,
    pub type_latency: SharedString,
    pub semantic_latency: SharedString,
    pub semantic_loading: bool,
    pub is_offline: bool,
    pub selection_flat: Option<usize>,
    pub current_gen: Gen,
    pub gen_first_arrived: Option<Instant>,
    pub recents: Vec<HitRow>,
}

impl StubSearchStore {
    fn section_rows(&self, id: SearchSectionId) -> &[HitRow] {
        match id {
            SECTION_NAME => &self.name_rows,
            SECTION_TYPE => &self.type_rows,
            SECTION_SEMANTIC => &self.semantic_rows,
            _ => &[],
        }
    }
    fn section_count_label(&self, id: SearchSectionId) -> &SharedString {
        match id {
            SECTION_NAME => &self.name_count,
            SECTION_TYPE => &self.type_count,
            SECTION_SEMANTIC => &self.semantic_count,
            _ => &self.name_count,
        }
    }
    fn section_latency_label(&self, id: SearchSectionId) -> &SharedString {
        match id {
            SECTION_NAME => &self.name_latency,
            SECTION_TYPE => &self.type_latency,
            SECTION_SEMANTIC => &self.semantic_latency,
            _ => &self.name_latency,
        }
    }
}

// ── Per-section count ticker spring state ────────────────────────────────────

struct SectionState {
    count_spring: Motion,
    count_display: SharedString,
    scroll_handle: UniformListScrollHandle,
}

impl SectionState {
    fn new() -> Self {
        Self {
            count_spring: Motion::new(0.0, Spring::GENTLE),
            count_display: SharedString::from("0"),
            scroll_handle: UniformListScrollHandle::new(),
        }
    }

    /// Tick the count spring and update the display label.
    /// Returns true if still animating.
    fn tick(&mut self, now: Instant, target: f32) -> bool {
        if should_skip(self.count_spring.target(), target) {
            self.count_spring.snap_to(target);
        } else {
            self.count_spring.animate_to(target);
        }
        let still = self.count_spring.tick(now);
        let rounded = count_rounds(self.count_spring.value());
        // Update-phase allocation (not in the element tree): integer → SharedString
        // cached so RenderOnce components see a stable pointer across frames.
        self.count_display = SharedString::from(rounded.to_string());
        still
    }
}

// ── The view ─────────────────────────────────────────────────────────────────

/// Omni-search overlay — §15.
///
/// Open via the `OverlayStack` mechanism (§13.5); closed by `ClearThenClose`
/// or by popping the overlay stack.  The store is injected at construction.
///
/// ## Spring state
///
/// - `sel_bar_y`: SNAPPY spring for the selection bar's y-offset.  Retargeted
///   whenever `SearchStore.selection_flat` changes; glides between rows.
/// - `overlay_opacity`: SNAPPY spring for the overlay fade-in (§5.3 `overlay.in`).
/// - `overlay_rise`: SNAPPY spring for the 4 px rise-in on open.
/// - Per-section `SectionState.count_spring`: GENTLE for the count tickers.
pub struct OmniSearch {
    /// Injected store handle.
    store: Entity<StubSearchStore>,
    /// GPUI focus handle — required for `Focusable` (LD-13).
    focus_handle: FocusHandle,

    // ── Spring state ───────────────────────────────────────────────────────
    /// Y-offset of the selection highlight bar (§15 "glide" detail).
    sel_bar_y: Motion,
    /// Overlay entrance opacity (0 → 1 on open).
    overlay_opacity: Motion,
    /// Overlay entrance rise offset in px (4 → 0 on open).
    overlay_rise: Motion,
    /// Per-section state (count tickers, scroll handles).
    sections: [SectionState; 3],

    // ── Last-known selection ───────────────────────────────────────────────
    /// Tracks which selection index the bar is targeting, so we retarget only
    /// on change.
    last_sel_flat: Option<usize>,
    /// Row height in px, measured lazily on first render (set once).
    row_height_px: f32,
}

impl OmniSearch {
    /// Construct the overlay bound to `store`, acquiring a focus handle.
    pub fn new(store: Entity<StubSearchStore>, cx: &mut Context<Self>) -> Self {
        Self {
            store,
            focus_handle: cx.focus_handle(),
            sel_bar_y: Motion::new(0.0, Spring::SNAPPY),
            overlay_opacity: {
                let mut m = Motion::new(0.0, Spring::SNAPPY);
                m.animate_to(1.0);
                m
            },
            overlay_rise: {
                let mut m = Motion::new(4.0, Spring::SNAPPY);
                m.animate_to(0.0);
                m
            },
            sections: [SectionState::new(), SectionState::new(), SectionState::new()],
            last_sel_flat: None,
            row_height_px: 48.0, // §10.1: each row is 48 px (3 × space_4 = 3 × 16)
        }
    }

    // ── Action dispatch ────────────────────────────────────────────────────

    fn on_selection_up(
        &mut self,
        _: &SelectionUp,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.store.update(cx, |store, _cx| {
            // Move selection up — real store will impl this
            let _ = store;
        });
        cx.notify();
    }

    fn on_selection_down(
        &mut self,
        _: &SelectionDown,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.store.update(cx, |store, _cx| {
            let _ = store;
        });
        cx.notify();
    }

    fn on_tab_next_section(
        &mut self,
        _: &TabNextSection,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.store.update(cx, |store, _cx| {
            let _ = store;
        });
        cx.notify();
    }

    fn on_commit(&mut self, _: &CommitSelection, _window: &mut Window, cx: &mut Context<Self>) {
        self.store.update(cx, |store, _cx| {
            let _ = store;
        });
        cx.notify();
    }

    fn on_commit_keep(
        &mut self,
        _: &CommitSelectionKeepOverlay,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.store.update(cx, |store, _cx| {
            let _ = store;
        });
        cx.notify();
    }

    fn on_commit_bg(
        &mut self,
        _: &CommitSelectionBackgroundTab,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.store.update(cx, |store, _cx| {
            let _ = store;
        });
        cx.notify();
    }

    fn on_clear_then_close(
        &mut self,
        _: &ClearThenClose,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.store.update(cx, |store, _cx| {
            let _ = store;
        });
        cx.notify();
    }

    fn on_jump_section_1(
        &mut self,
        _: &JumpToSection1,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.sections[0]
            .scroll_handle
            .scroll_to_item(0, ScrollStrategy::Top);
        cx.notify();
    }

    fn on_jump_section_2(
        &mut self,
        _: &JumpToSection2,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.sections[1]
            .scroll_handle
            .scroll_to_item(0, ScrollStrategy::Top);
        cx.notify();
    }

    fn on_jump_section_3(
        &mut self,
        _: &JumpToSection3,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.sections[2]
            .scroll_handle
            .scroll_to_item(0, ScrollStrategy::Top);
        cx.notify();
    }

    // ── Spring advance ─────────────────────────────────────────────────────

    /// Tick all springs and retarget the selection bar if selection changed.
    /// Returns true while any spring is still moving.
    fn tick_springs(
        &mut self,
        now: Instant,
        store: &StubSearchStore,
        tokens: &MotionTokens,
    ) -> bool {
        if tokens.scale == 0.0 {
            // Instant-cut: snap all springs.
            self.overlay_opacity.snap_to(1.0);
            self.overlay_rise.snap_to(0.0);
            // Snap selection bar if selection exists.
            if let Some(flat) = store.selection_flat {
                let target_y = flat as f32 * self.row_height_px;
                self.sel_bar_y.snap_to(target_y);
            }
            for (i, section) in self.sections.iter_mut().enumerate() {
                let id = SearchSectionId(i as u8);
                let count = store.section_rows(id).len() as f32;
                section.count_spring.snap_to(count);
            }
            return false;
        }

        let mut animating = false;
        animating |= self.overlay_opacity.tick(now);
        animating |= self.overlay_rise.tick(now);

        // Retarget selection bar when selection changes.
        if store.selection_flat != self.last_sel_flat {
            self.last_sel_flat = store.selection_flat;
            if let Some(flat) = store.selection_flat {
                let target_y = flat as f32 * self.row_height_px;
                self.sel_bar_y.animate_to(target_y);

                // Scroll the selected item's section into view (§15 scroll.glide).
                // Determine which section + local index the flat index falls in.
                let (section_idx, local_idx) = self.flat_to_section_local(flat, store);
                self.sections[section_idx]
                    .scroll_handle
                    .scroll_to_item(local_idx, ScrollStrategy::Nearest);
            }
        }
        animating |= self.sel_bar_y.tick(now);

        // Tick per-section count tickers.
        for (i, section) in self.sections.iter_mut().enumerate() {
            let id = SearchSectionId(i as u8);
            let count = store.section_rows(id).len() as f32;
            animating |= section.tick(now, count);
        }

        animating
    }

    /// Map a flat selection index to (section_index, local_row_index).
    fn flat_to_section_local(&self, flat: usize, store: &StubSearchStore) -> (usize, usize) {
        let mut remaining = flat;
        for i in 0..3 {
            let id = SearchSectionId(i as u8);
            let count = store.section_rows(id).len();
            if remaining < count {
                return (i, remaining);
            }
            remaining -= count;
        }
        // Fallback: clamp to last section.
        let last = store.section_rows(SearchSectionId(2)).len();
        (2, last.saturating_sub(1))
    }

    // ── Element builders ───────────────────────────────────────────────────

    /// Build the input row: text input + mode chips + (future) scope chips.
    fn render_input_row(&self, _window: &mut Window, cx: &mut App) -> AnyElement {
        let ext = cx.theme_ext();
        let sp = ext.space;
        let ts = ext.type_scale;
        let theme = cx.theme();
        let current_mode = SearchMode::Auto; // TODO(store): read from store

        h_flex()
            .w_full()
            .gap(sp.space_2)
            .px(sp.space_3)
            .py(sp.space_2)
            .border_b_1()
            .border_color(theme.border)
            // Search icon placeholder — real input will have icon slot from TextInput
            .child(
                div()
                    .flex_1()
                    .h(sp.space_8)
                    .bg(theme.secondary)
                    .rounded(sp.r_md)
                    .px(sp.space_3)
                    .flex()
                    .items_center()
                    .text_color(theme.muted_foreground)
                    .text_size(ts.ui.size)
                    .line_height(ts.ui.line_height)
                    .child(SharedString::from("Search…")),
            )
            // Mode chips
            .children(SearchMode::all().iter().map(|&mode| {
                let is_active = mode == current_mode;
                let bg = if is_active {
                    theme.accent
                } else {
                    theme.secondary
                };
                let fg = if is_active {
                    theme.accent_foreground
                } else {
                    theme.muted_foreground
                };
                div()
                    .id(gpui::ElementId::Name(gpui::SharedString::from(format!(
                        "search.mode_chip.{}",
                        mode.label()
                    ))))
                    .px(sp.space_2)
                    .py(sp.space_1)
                    .rounded(sp.r_sm)
                    .bg(bg)
                    .text_color(fg)
                    .text_size(ts.caption.size)
                    .line_height(ts.caption.line_height)
                    .cursor_pointer()
                    .hover(|s| s.opacity(0.8))
                    .active(|s| s.opacity(0.65))
                    .child(SharedString::from(mode.label()))
            }))
            .into_any_element()
    }

    /// Build one hit row from a `HitRow` (all data pre-computed — §1.1.4).
    ///
    /// The row contains: kind badge · display name · signature preview · path · provenance badge.
    fn render_hit_row(
        row: &HitRow,
        flat_index: usize,
        is_selected: bool,
        cx: &mut App,
    ) -> AnyElement {
        let ext = cx.theme_ext();
        let sp = ext.space;
        let ts = ext.type_scale;
        let theme = cx.theme();

        // Kind badge — map KindTag to LocalKindDiscriminant via wire u16.
        let kind_badge: AnyElement = match row.kind {
            KindTag::Known(disc) => {
                // Map through the wire u16: KindDiscriminant::as_u16 is the stable
                // encoding shared between nudox-ir and the GUI's local mirror.
                let local = LocalKindDiscriminant::from_u16(disc.as_u16())
                    .unwrap_or(LocalKindDiscriminant::Module);
                Badge::for_kind(
                    ("search.row.badge.kind", flat_index as u64),
                    local,
                    cx,
                )
                .into_any_element()
            }
            // `KindTag` is `#[non_exhaustive]`, so this arm covers both
            // `Unknown(code)` and any variant a future wire version adds.
            //
            // LD-7: a kind this build does not understand still gets a visible
            // chip. A symbol from a newer producer is then present-but-unlabelled
            // rather than silently absent, which is the difference between "we
            // do not know what this is" and "there is nothing here".
            _ => Badge::custom(
                ("search.row.badge.unknown", flat_index as u64),
                SharedString::from("?"),
                theme.muted_foreground.opacity(0.3),
                theme.muted_foreground,
            )
            .into_any_element(),
        };

        // Provenance badge.
        let prov_gui = wire_prov_to_gui(&row.provenance);
        let prov_badge = Badge::for_provenance(
            ("search.row.badge.prov", flat_index as u64),
            prov_gui,
            cx,
        );

        // Signature tokens — convert wire SigToken to ui SigToken.
        let sig_tokens: Vec<crate::ui::signature_line::SigToken> =
            row.sig_preview.iter().map(wire_sig_to_ui).collect();

        // Highlight background for selected row.
        let bg = if is_selected {
            theme.list_active.opacity(0.12)
        } else {
            theme.transparent
        };
        let border_l = if is_selected {
            theme.accent
        } else {
            theme.transparent
        };

        h_flex()
            .id(("search.hit.row", flat_index as u64))
            .w_full()
            .h(px(48.0))
            .px(sp.space_3)
            .gap(sp.space_2)
            .items_center()
            .bg(bg)
            .border_l(px(2.0))
            .border_color(border_l)
            .cursor_pointer()
            .hover(|s| s.bg(theme.list_head.opacity(0.06)))
            .active(|s| s.bg(theme.list_active.opacity(0.16)))
            // Kind badge
            .child(kind_badge)
            // Name + signature
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .gap(px(1.0))
                    .child(
                        div()
                            .text_color(theme.foreground)
                            .text_size(ts.ui.size)
                            .line_height(ts.ui.line_height)
                            .truncate()
                            .child(SharedString::from(row.display_name.to_string())),
                    )
                    .child(
                        SignatureLine::new(
                            ("search.sig", flat_index as u64),
                            sig_tokens,
                        ),
                    ),
            )
            // Provenance badge (right-aligned)
            .child(prov_badge)
            .into_any_element()
    }

    /// Build a three-row shimmer skeleton for the semantic section (§15 states).
    fn render_semantic_shimmer(&self, cx: &mut App) -> AnyElement {
        let ext = cx.theme_ext();
        let sp = ext.space;
        let theme = cx.theme();

        v_flex()
            .w_full()
            .children((0..3usize).map(|i| {
                // Try to acquire a loop permit for shimmer (§5.4).
                let permit = cx.global::<MotionTokens>().acquire_loop_slot();
                let el = h_flex()
                    .w_full()
                    .h(px(48.0))
                    .px(sp.space_3)
                    .gap(sp.space_2)
                    .items_center()
                    .child(
                        div()
                            .w(sp.space_8)
                            .h(sp.space_4)
                            .rounded(sp.r_sm)
                            .bg(theme.muted_foreground.opacity(0.12)),
                    )
                    .child(
                        v_flex()
                            .flex_1()
                            .gap(px(4.0))
                            .child(
                                div()
                                    .w_3_4()
                                    .h(px(12.0))
                                    .rounded(sp.r_sm)
                                    .bg(theme.muted_foreground.opacity(0.12)),
                            )
                            .child(
                                div()
                                    .w_1_2()
                                    .h(px(10.0))
                                    .rounded(sp.r_sm)
                                    .bg(theme.muted_foreground.opacity(0.08)),
                            ),
                    );

                if permit.is_some() {
                    shimmer(el, ("search.shimmer", i as u64)).into_any_element()
                } else {
                    // Census full: render at midpoint opacity, no loop (§5.4).
                    el.opacity(0.55).into_any_element()
                }
            }))
            .into_any_element()
    }

    /// Build one section: sticky header + uniform_list of rows.
    fn render_section(
        &self,
        section_idx: usize,
        rows: Vec<HitRow>,
        latency_label: SharedString,
        generation: Gen,
        gen_age: std::time::Duration,
        flat_start: usize,
        selected_flat: Option<usize>,
        _window: &mut Window,
        cx: &mut App,
    ) -> AnyElement {
        let row_count = rows.len();
        let count_display = self.sections[section_idx].count_display.clone();
        let scroll_handle = self.sections[section_idx].scroll_handle.clone();

        let section_label: &'static str = match section_idx {
            0 => "Name",
            1 => "Type",
            2 => "Semantic",
            _ => "Results",
        };

        // Sticky header with per-section latency readout.
        let header_action: AnyElement = {
            let ext = cx.theme_ext();
            let ts = ext.type_scale;
            let theme = cx.theme();
            div()
                .text_color(theme.muted_foreground.opacity(0.5))
                .text_size(ts.caption.size)
                .line_height(ts.caption.line_height)
                .child(latency_label)
                .into_any_element()
        };

        let header = SectionHeader::new(SharedString::from(section_label))
            .count(count_display.clone())
            .action(header_action);

        // Determine whether we are inside the entrance window.
        let in_entrance_window = gen_age < ROW_CASCADE_WINDOW;
        let gen_epoch = generation.0;

        // rows captured for the closure.
        let rows_for_list = rows;
        let list_el = uniform_list(
            (
                "search.results",
                // ElementId has no 3-tuple form; pack (section, generation) into
                // one u64 so the id is still unique and stable per data
                // generation (LD-19 / §4.1 entrance identity).
                ((section_idx as u64) << 32) | (gen_epoch & 0xFFFF_FFFF),
            ),
            row_count,
            move |range, _window, cx| {
                range
                    .map(|ix| {
                        let row = &rows_for_list[ix];
                        let flat = flat_start + ix;
                        let is_selected = selected_flat == Some(flat);
                        let hit = OmniSearch::render_hit_row(row, flat, is_selected, cx);

                        if in_entrance_window {
                            row_enter(
                                div().child(hit),
                                entrance_id("search.row.enter", gen_epoch, ix),
                                ix,
                                cx.global::<MotionTokens>(),
                            )
                        } else {
                            div().child(hit).into_any()
                        }
                    })
                    .collect::<Vec<_>>()
            },
        )
        .track_scroll(&scroll_handle)
        .flex_grow(1.0);

        v_flex()
            .w_full()
            // Sticky header: z-order is managed by placement in the element tree.
            .child(header)
            .child(list_el)
            .into_any_element()
    }

    /// Build the footer key hints row.
    fn render_footer(cx: &mut App) -> AnyElement {
        let ext = cx.theme_ext();
        let sp = ext.space;
        let theme = cx.theme();

        // Parse known-valid keystroke strings; filter out any that fail so the
        // footer degrades gracefully rather than panicking (no unwrap rule).
        let hint = |desc: &'static str, spec: &'static str| -> Option<KeyHint> {
            let k = Keystroke::parse(spec).ok()?;
            Some(KeyHint::new(SharedString::from(desc), vec![k]))
        };

        let mut row = h_flex()
            .w_full()
            .px(sp.space_3)
            .py(sp.space_2)
            .gap(sp.space_4)
            .border_t_1()
            .border_color(theme.border)
            .bg(theme.secondary.opacity(0.4));

        if let Some(h) = hint("Open", "enter") { row = row.child(h); }
        if let Some(h) = hint("Keep open", "cmd-enter") { row = row.child(h); }
        if let Some(h) = hint("Background", "alt-enter") { row = row.child(h); }
        if let Some(h) = hint("Close", "escape") { row = row.child(h); }
        if let Some(h) = hint("Next section", "tab") { row = row.child(h); }

        row.into_any_element()
    }

    /// Build the "offline: semantic unavailable" notice (replaces the section).
    fn render_offline_notice(cx: &mut App) -> AnyElement {
        let ext = cx.theme_ext();
        let sp = ext.space;
        let ts = ext.type_scale;
        let trust = ext.trust;
        // Use the stale trust colour for the offline notice (§10.4).
        let stale_col = trust.stale.colour;

        h_flex()
            .w_full()
            .px(sp.space_3)
            .py(sp.space_2)
            .gap(sp.space_2)
            .items_center()
            .bg(stale_col.opacity(0.08))
            .rounded(sp.r_md)
            .child(
                div()
                    .text_color(stale_col)
                    .text_size(ts.caption.size)
                    .line_height(ts.caption.line_height)
                    .child(SharedString::from(
                        "Offline — semantic search unavailable",
                    )),
            )
            .into_any_element()
    }

    /// Render the selection bar — an absolutely-positioned highlight quad that
    /// glides between rows via `self.sel_bar_y`.
    fn render_selection_bar(&self, cx: &mut App) -> AnyElement {
        let theme = cx.theme();
        if self.last_sel_flat.is_none() {
            return div().into_any_element();
        }
        let y = self.sel_bar_y.value();
        // Selection bar — paint-only quad behind the rows.
        // Placed as the first child so it is drawn under the row list (element order = paint order).
        div()
            .absolute()
            .left_0()
            .top(px(y))
            .w_full()
            .h(px(self.row_height_px))
            .bg(theme.list_active.opacity(0.10))
            .into_any_element()
    }
}

// ── Focusable ─────────────────────────────────────────────────────────────

impl Focusable for OmniSearch {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

// ── Render ────────────────────────────────────────────────────────────────────

impl Render for OmniSearch {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // ── 1. Tick springs ────────────────────────────────────────────────
        let now = Instant::now();
        // Read scale once; MotionTokens is not Clone so we construct a fresh
        // one with the same scale where a by-value MotionTokens is needed
        // (e.g. EmptyState).
        let motion_scale = cx.global::<MotionTokens>().scale;

        let (generation, gen_first_arrived, selection_flat, is_offline, semantic_loading) = {
            let store = self.store.read(cx);
            (
                store.current_gen,
                store.gen_first_arrived,
                store.selection_flat,
                store.is_offline,
                store.semantic_loading,
            )
        };

        let gen_age = gen_first_arrived
            .map(|t| t.elapsed())
            .unwrap_or(std::time::Duration::MAX);

        {
            let tokens_ref = cx.global::<MotionTokens>();
            let tokens_snapshot = MotionTokens::new(tokens_ref.scale);
            let store_ref = self.store.read(cx);
            let animating = self.tick_springs(now, store_ref, &tokens_snapshot);
            if animating && motion_scale > 0.0 {
                window.request_animation_frame();
            }
        }

        // ── 2. Read display state ──────────────────────────────────────────
        let ext = cx.theme_ext().clone();
        let sp = ext.space;
        let theme = cx.theme().clone();

        let opacity = self.overlay_opacity.value();
        let rise_px = self.overlay_rise.value();
        let motion_tokens_for_empty = MotionTokens::new(motion_scale);
        let is_empty_input = self.store.read(cx).input.is_empty();

        // Flat starts per section.
        let flat_start_name = 0usize;
        let flat_start_type = {
            let store = self.store.read(cx);
            store.section_rows(SECTION_NAME).len()
        };
        let flat_start_semantic = {
            let store = self.store.read(cx);
            flat_start_type + store.section_rows(SECTION_TYPE).len()
        };

        // ── 3. Build sections ──────────────────────────────────────────────

        // Empty-input state: show recents.
        let results_body: AnyElement = if is_empty_input {
            let store = self.store.read(cx);
            if store.recents.is_empty() {
                EmptyState::new(
                    IconName::Search,
                    SharedString::from("Search symbols"),
                    SharedString::from("Type to search by name, type, or meaning."),
                    motion_tokens_for_empty,
                )
                .into_any_element()
            } else {
                let recents = store.recents.clone();
                let count = recents.len();
                let sel = selection_flat;
                uniform_list(
                    "search.recents",
                    count,
                    move |range, _window, cx| {
                        range
                            .map(|ix| {
                                let row = &recents[ix];
                                let is_sel = sel == Some(ix);
                                OmniSearch::render_hit_row(row, ix, is_sel, cx)
                            })
                            .collect::<Vec<_>>()
                    },
                )
                .flex_grow(1.0)
                .into_any_element()
            }
        } else {
            // Active search: three sections.
            // Snapshot what the sections need, then release the store borrow:
            // `render_section` takes `&mut App` for listeners, and an outstanding
            // immutable borrow of `cx` would make that impossible.
            let (
                name_rows,
                name_latency,
                type_rows,
                type_latency,
                semantic_rows,
                semantic_latency,
            ) = {
                let store = self.store.read(cx);
                (
                    store.section_rows(SearchSectionId(0)).to_vec(),
                    store.section_latency_label(SearchSectionId(0)).clone(),
                    store.section_rows(SearchSectionId(1)).to_vec(),
                    store.section_latency_label(SearchSectionId(1)).clone(),
                    store.section_rows(SECTION_SEMANTIC).to_vec(),
                    store.section_latency_label(SECTION_SEMANTIC).clone(),
                )
            };
            let name_section = self.render_section(
                0,
                name_rows,
                name_latency,
                generation,
                gen_age,
                flat_start_name,
                selection_flat,
                window,
                cx,
            );
            let type_section = self.render_section(
                1,
                type_rows,
                type_latency,
                generation,
                gen_age,
                flat_start_type,
                selection_flat,
                window,
                cx,
            );

            // Semantic: shimmer while loading, offline notice if offline.
            let semantic_section: AnyElement = if is_offline {
                v_flex()
                    .w_full()
                    .child(
                        SectionHeader::new(SharedString::from("Semantic"))
                            .count(SharedString::from("—")),
                    )
                    .child(Self::render_offline_notice(cx))
                    .into_any_element()
            } else if semantic_loading && semantic_rows.is_empty() {
                v_flex()
                    .w_full()
                    .child(
                        SectionHeader::new(SharedString::from("Semantic"))
                            .count(self.sections[2].count_display.clone()),
                    )
                    .child(self.render_semantic_shimmer(cx))
                    .into_any_element()
            } else {
                self.render_section(
                    2,
                    semantic_rows,
                    semantic_latency,
                    generation,
                    gen_age,
                    flat_start_semantic,
                    selection_flat,
                    window,
                    cx,
                )
            };

            v_flex()
                .w_full()
                .flex_grow(1.0)
                .min_h_0()
                .child(name_section)
                .child(type_section)
                .child(semantic_section)
                .into_any_element()
        };

        // ── 4. Compose overlay ─────────────────────────────────────────────

        div()
            // Full-screen centring container (not interactive itself).
            .w_full()
            .h_full()
            .flex()
            .items_start()
            .justify_center()
            .pt(sp.space_8)
            .on_action(cx.listener(Self::on_selection_up))
            .on_action(cx.listener(Self::on_selection_down))
            .on_action(cx.listener(Self::on_tab_next_section))
            .on_action(cx.listener(Self::on_commit))
            .on_action(cx.listener(Self::on_commit_keep))
            .on_action(cx.listener(Self::on_commit_bg))
            .on_action(cx.listener(Self::on_clear_then_close))
            .on_action(cx.listener(Self::on_jump_section_1))
            .on_action(cx.listener(Self::on_jump_section_2))
            .on_action(cx.listener(Self::on_jump_section_3))
            // Overlay panel
            .child(
                v_flex()
                    // §15 layout: 640 px wide, max-height 60 %.
                    .w(px(640.0))
                    .max_h(gpui::relative(0.60))
                    .bg(theme.background)
                    .rounded(sp.r_xl)
                    .shadow(ext.elev.overlay.shadows.clone())
                    .border_1()
                    .border_color(theme.border)
                    .overflow_hidden()
                    // §5.3 overlay.in: opacity + 4 px rise on SNAPPY spring.
                    .opacity(opacity)
                    .mt(px(rise_px))
                    .track_focus(&self.focus_handle)
                    // Input row (always visible)
                    .child(self.render_input_row(window, cx))
                    // Scrollable results area — the selection bar lives here.
                    .child(
                        div()
                            .relative()
                            .flex_1()
                            .min_h_0()
                            .overflow_hidden()
                            // Glide selection bar (absolutely positioned behind rows).
                            .child(self.render_selection_bar(cx))
                            // Results body (scrollable).
                            .child(
                                div()
                                    .id("search.results.scroll")
                                    .w_full()
                                    .h_full()
                                    .overflow_y_scroll()
                                    .child(results_body),
                            ),
                    )
                    // Footer key hints
                    .child(Self::render_footer(cx)),
            )
    }
}

// ── Wire type adapters ────────────────────────────────────────────────────────

/// Convert `nudox_engine::wire::Provenance` to the GUI's local `Provenance`.
fn wire_prov_to_gui(p: &nudox_engine::wire::Provenance) -> Provenance {
    match p {
        nudox_engine::wire::Provenance::TrustedLocal => Provenance::TrustedLocal,
        nudox_engine::wire::Provenance::SyncedLocal { .. } => Provenance::SyncedLocal,
        nudox_engine::wire::Provenance::Remote { .. } => Provenance::Remote,
        nudox_engine::wire::Provenance::Stale { .. } => Provenance::Stale,
        _ => Provenance::Stale,
    }
}

/// Convert a `nudox_engine::wire::SigToken` to the UI component's `SigToken`.
///
/// The UI's `SigToken` still carries its own `SymbolKey` mirror; once the
/// dependency is wired through, this becomes a no-op `use` alias.
fn wire_sig_to_ui(t: &WireSigToken) -> crate::ui::signature_line::SigToken {
    use crate::ui::signature_line::{SigToken, SymbolKey};
    match t {
        WireSigToken::Kw(s) => SigToken::Kw(s),
        WireSigToken::Punct(s) => SigToken::Punct(s),
        WireSigToken::Ws => SigToken::Ws,
        WireSigToken::Ident(s) => SigToken::Ident(SharedString::from(s.to_string())),
        WireSigToken::Generic(s) => SigToken::Generic(SharedString::from(s.to_string())),
        WireSigToken::Lifetime(s) => {
            // Render lifetimes as Generic (no specific token in the UI SigToken yet).
            SigToken::Generic(SharedString::from(s.to_string()))
        }
        WireSigToken::Ty { text, target } => SigToken::Ty {
            text: SharedString::from(text.to_string()),
            target: target
                .as_ref()
                .map(|_key| SymbolKey(SharedString::from("TODO(wire)"))),
        },
        _ => SigToken::Ident(SharedString::from("?")),
    }
}
