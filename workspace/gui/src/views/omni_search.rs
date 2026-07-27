//! Omni-search overlay — GUI-PLAN §15. The homepage of lindsey.
//!
//! `cmd-K` opens a centred 640 px overlay: an input row with mode and scope
//! chips, three independently-arriving result sections (Name / Type / Semantic)
//! each under a sticky caption header carrying a per-section latency readout,
//! and a footer of [`KeyHint`]s.
//!
//! # What makes this beat a flat search list
//!
//! * **Sections arrive independently.** Local name/type hits paint as soon as
//!   the drain delivers them (< 10 ms, §15 acceptance). The semantic section
//!   shimmers *in its own section only* and never clears or reorders what is
//!   already on screen (LD-15).
//! * **Every row is typed.** Kind badge, display name, a real
//!   [`SignatureLine`] built from pre-tokenised signature tokens, the module
//!   path, and a trust badge (LD-8).
//! * **The selection bar glides.** §15 calls this the single most-felt polish
//!   detail in the app, so it is not a per-row background swap: it is a
//!   [`Spring::SNAPPY`] y-offset painted as a [`UniformListDecoration`], which
//!   means it lives in the list's *scrolled content space* and therefore
//!   tracks the rows exactly — sub-pixel — instead of drifting when the list
//!   scrolls under it. See [`SelectionBar`].
//!
//! # Zero string work in render (§1.1.4 / §15 Perf)
//!
//! Rows are [`PreparedRow`]s, not [`HitRow`]s. The wire→UI conversion (splitting
//! the display name into path + leaf, mapping `KindTag` to a local kind,
//! mapping wire provenance to trust chrome, and translating each wire signature
//! token into the one [`SignatureLine`] consumes) happens **once, at ingest**,
//! via [`PreparedRow::prepare`]. `render` only clones `Arc`s and
//! `SharedString`s.
//!
//! # Entrance identity (§4.1 — the rule that makes virtualization safe)
//!
//! `uniform_list` mounts and unmounts rows on scroll. Row entrances are keyed
//! to *(generation, index)* through [`entrance_id`] and only wrap themselves in
//! [`row_enter`] while the generation is younger than `ROW_CASCADE_WINDOW`
//! (400 ms). Scrolling therefore never re-animates; only new results cascade.
//!
//! # Keyboard (LD-13)
//!
//! Navigation uses the **global** actions declared in `app::actions` and bound
//! in `app::keymaps` under the `Overlay` and `OmniSearch` contexts — this view
//! declares no actions of its own, so the `?` cheat sheet and the command
//! palette stay in sync with what actually works. Text entry is handled
//! directly in [`OmniSearch::on_key_down`] rather than by a third-party input
//! widget, because such a widget binds `up`/`down`/`enter`/`escape` in its own
//! deeper key context and would swallow the §15 navigation keys before they
//! could reach this view.

use std::sync::Arc;
use std::time::Instant;

use gpui::{
    AnyElement, App, Bounds, Context, Entity, EventEmitter, FocusHandle, Focusable, Hsla,
    InteractiveElement as _, IntoElement, KeyDownEvent, Keystroke, ParentElement as _, Pixels,
    Point, Render, ScrollStrategy, SharedString, StatefulInteractiveElement as _, Styled,
    UniformListDecoration, UniformListScrollHandle, Window, div, prelude::FluentBuilder as _, px,
    relative, uniform_list,
};
use gpui_component::{Icon, IconName, h_flex, v_flex};

use crate::app::actions::{
    ConfirmOverlay, DismissOverlay, JumpToSection1, JumpToSection2, JumpToSection3,
    MoveSelectionDown, MoveSelectionUp, NextSection, OpenInBackgroundTab, OpenWithoutClosing,
    PrevSection,
};
use crate::motion::permits::LoopPermit;
use crate::motion::tokens::{MotionTokens, ROW_CASCADE_WINDOW};
use crate::motion::{Motion, Spring, entrance_id, row_enter, shimmer};
use crate::theme::ext::{Provenance, ThemeExtAccessor as _};
use crate::theme::kind::LocalKindDiscriminant;
use crate::ui::count_label::{count_rounds, should_skip};
use crate::ui::signature_line::{SigToken as UiSigToken, SymbolKey as UiSymbolKey};
use crate::ui::toolbar::ToolbarChip;
use crate::ui::{Badge, CountLabel, EmptyState, KeyHint, SectionHeader, SignatureLine, Toolbar};
use nudox_engine::wire::{
    HitRow, KindTag, Provenance as WireProvenance, SigToken as WireSigToken, SymbolKey,
};

// ─────────────────────────────────────────────────────────────────────────────
// Layout constants
//
// §15 fixes dimensions that are not on the 4 px token grid because they are
// *composition* decisions, not spacing decisions. They are named here so no
// bare literal ever appears at a call site.
// ─────────────────────────────────────────────────────────────────────────────

/// §15: the overlay is 640 px wide, always.
const OVERLAY_WIDTH: Pixels = px(640.0);

/// §15: the overlay never exceeds 60 % of the window height.
const OVERLAY_MAX_HEIGHT_FRACTION: f32 = 0.60;

/// The 4 px overlay entrance rise (§5.3 `overlay.in`).
const OVERLAY_RISE_PX: f32 = 4.0;

/// How many rows of a section are visible before it scrolls internally.
///
/// Bounded so all three sections can be on screen at once; the section still
/// virtualizes through `uniform_list`, so a 10 000-hit section costs the same
/// as an 8-hit one (LD-6, §15 acceptance).
const VISIBLE_ROWS_PER_SECTION: usize = 8;

/// Rows in the semantic section's loading shimmer (§15 states).
const SEMANTIC_SHIMMER_ROWS: usize = 3;

/// Static opacity for a shimmer that could not obtain a loop permit (§5.4).
const SHIMMER_SHED_OPACITY: f32 = 0.55;

/// Alpha of the selection bar's fill. Low enough that the row's text stays
/// fully legible, since `uniform_list` paints decorations *after* items.
const SELECTION_FILL_ALPHA: f32 = 0.16;

/// Opacity of the faint module path beside a hit's name.
const PATH_OPACITY: f32 = 0.75;

// ─────────────────────────────────────────────────────────────────────────────
// Sections
// ─────────────────────────────────────────────────────────────────────────────

/// The three fused result sections (§15).
///
/// Frozen at three: `cmd-1/2/3` jump to them by ordinal, and `SearchResults`
/// in §12.3 is `[SectionBuf; 3]`.
pub const SECTION_COUNT: usize = 3;

/// Which of the three §15 sections a row or cursor belongs to.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Section {
    /// Lexical name matches — local, fastest.
    Name,
    /// Type-signature matches — local.
    Type,
    /// Embedding / semantic matches — may arrive 20–200 ms later.
    Semantic,
}

impl Section {
    /// All three, in display order.
    pub const ALL: [Section; SECTION_COUNT] = [Section::Name, Section::Type, Section::Semantic];

    /// Zero-based display index, used to key the fixed-size arrays.
    pub fn index(self) -> usize {
        match self {
            Section::Name => 0,
            Section::Type => 1,
            Section::Semantic => 2,
        }
    }

    /// Recover a section from its display index.
    pub fn from_index(ix: usize) -> Option<Section> {
        Section::ALL.get(ix).copied()
    }

    /// The sticky caption header label. `SectionHeader` upper-cases it.
    pub fn caption(self) -> &'static str {
        match self {
            Section::Name => "Name",
            Section::Type => "Type",
            Section::Semantic => "Semantic",
        }
    }

    /// Stable `ElementId` namespace for this section's row entrances (LD-19).
    ///
    /// Distinct per section: `entrance_id` packs `(gen, ix)` into one integer,
    /// so two sections sharing a namespace would collide at the same index.
    fn entrance_namespace(self) -> &'static str {
        match self {
            Section::Name => "search.row.enter.name",
            Section::Type => "search.row.enter.type",
            Section::Semantic => "search.row.enter.semantic",
        }
    }

    /// Stable `ElementId` for this section's list (LD-19).
    fn list_id(self) -> &'static str {
        match self {
            Section::Name => "search.results.list.name",
            Section::Type => "search.results.list.type",
            Section::Semantic => "search.results.list.semantic",
        }
    }

    /// Stable `ElementId` namespace for this section's rows (LD-19).
    fn row_id(self) -> &'static str {
        match self {
            Section::Name => "search.results.row.name",
            Section::Type => "search.results.row.type",
            Section::Semantic => "search.results.row.semantic",
        }
    }
}

/// A position in the fused result set: which section, and which row inside it.
///
/// Deliberately *not* a flat index. §15 requires that selection movement never
/// wraps silently across sections, and a flat index makes that invariant
/// invisible — you cannot tell from `7` whether the next step leaves a section.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Cursor {
    /// Display index of the section (see [`Section::index`]).
    pub section: usize,
    /// Row index within that section.
    pub row: usize,
}

// ─────────────────────────────────────────────────────────────────────────────
// Modes and scopes
// ─────────────────────────────────────────────────────────────────────────────

/// The mode chip selection on the input row (§15 layout, §12.3 `mode`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SearchMode {
    /// Route the query by shape: `->` implies type search, prose implies semantic.
    Auto,
    /// Force lexical name search.
    Name,
    /// Force type-signature search.
    Type,
    /// Force semantic search.
    Semantic,
}

impl SearchMode {
    /// All four, in chip order.
    pub const ALL: [SearchMode; 4] = [
        SearchMode::Auto,
        SearchMode::Name,
        SearchMode::Type,
        SearchMode::Semantic,
    ];

    /// Chip label. `&'static str`, so rendering a chip allocates nothing.
    pub fn label(self) -> &'static str {
        match self {
            SearchMode::Auto => "Auto",
            SearchMode::Name => "Name",
            SearchMode::Type => "Type",
            SearchMode::Semantic => "Semantic",
        }
    }
}

/// One scope chip (dep-set / kind / language / package filter, §12.3 `scope`).
///
/// The label is pre-computed by the store; this view never formats one.
#[derive(Clone, Debug, PartialEq)]
pub struct ScopeChip {
    /// Pre-computed label (e.g. `"this project"`, `"rust"`).
    pub label: SharedString,
    /// Whether the filter is currently applied.
    pub active: bool,
}

// ─────────────────────────────────────────────────────────────────────────────
// PreparedRow — the render-ready hit
// ─────────────────────────────────────────────────────────────────────────────

/// A [`HitRow`] converted once, at ingest, into exactly what `render` draws.
///
/// The wire type is render-*ready* in the sense that it needs no I/O, but it is
/// not render-*able* without three mappings that each allocate: the wire
/// signature token is a different type from the one [`SignatureLine`] consumes,
/// the `KindTag` must resolve to a local kind for its colour, and the display
/// name carries its module path inline. Doing any of that per frame would
/// defeat GPUI's shaped-text cache, so it happens here instead — once per hit,
/// on the generation that produced it.
#[derive(Clone, Debug)]
pub struct PreparedRow {
    /// Stable identity, emitted on open.
    pub key: SymbolKey,
    /// The symbol's own name, without its path prefix.
    pub leaf: SharedString,
    /// The module path prefix, or empty when the name had none.
    pub path: SharedString,
    /// Signature preview, already in the component's token vocabulary.
    pub sig: Vec<UiSigToken>,
    /// Resolved kind, or `None` for a discriminant this binary does not know
    /// (LD-7: renders as a visible chip, never a panic).
    pub kind: Option<LocalKindDiscriminant>,
    /// Label for the unknown-kind chip. Empty when `kind` is `Some`.
    pub unknown_kind_label: SharedString,
    /// Trust chrome selector (LD-8).
    pub provenance: Provenance,
}

impl PreparedRow {
    /// Convert one wire hit.
    pub fn from_hit(hit: &HitRow) -> PreparedRow {
        let (path, leaf) = split_qualified_name(&hit.display_name);
        let kind = match hit.kind {
            KindTag::Known(d) => LocalKindDiscriminant::from_u16(d.as_u16()),
            // `KindTag` is `#[non_exhaustive]`: this arm covers `Unknown` and
            // any variant a future wire version adds (LD-7).
            _ => None,
        };
        let unknown_kind_label = if kind.is_some() {
            SharedString::default()
        } else {
            SharedString::from("unknown")
        };
        PreparedRow {
            key: hit.key.clone(),
            leaf,
            path,
            sig: hit.sig_preview.iter().map(prepare_sig_token).collect(),
            kind,
            unknown_kind_label,
            provenance: prepare_provenance(&hit.provenance),
        }
    }

    /// Convert a whole section's worth of hits.
    ///
    /// This is the call a `SearchStore` makes in `apply_search_event`, so that
    /// the cost lands on the arriving generation rather than on every frame.
    pub fn prepare(hits: &[HitRow]) -> Arc<[PreparedRow]> {
        hits.iter().map(PreparedRow::from_hit).collect()
    }
}

/// Split `a::b::c` into (`"a::b::"`, `"c"`).
///
/// `HitRow::display_name` "may include path prefix for disambiguation" and the
/// wire carries no separate path field, so the split happens here. Done at
/// ingest, never in render.
fn split_qualified_name(name: &str) -> (SharedString, SharedString) {
    match name.rfind("::") {
        Some(ix) => (
            SharedString::from(name[..ix + 2].to_owned()),
            SharedString::from(name[ix + 2..].to_owned()),
        ),
        None => (SharedString::default(), SharedString::from(name.to_owned())),
    }
}

/// Map wire provenance onto the trust-chrome selector (LD-8).
///
/// The payloads (`generation`, `as_of`) do not affect chrome; §10.4 keys the
/// colour, glyph, and label off the level alone.
fn prepare_provenance(p: &WireProvenance) -> Provenance {
    match p {
        WireProvenance::TrustedLocal => Provenance::TrustedLocal,
        WireProvenance::SyncedLocal { .. } => Provenance::SyncedLocal,
        WireProvenance::Remote { .. } => Provenance::Remote,
        WireProvenance::Stale { .. } => Provenance::Stale,
        // `WireProvenance` is `#[non_exhaustive]` (LR-12): a level added by a
        // newer engine reads as remote — visibly not-local, never a panic.
        _ => Provenance::Remote,
    }
}

/// Translate one wire signature token into the component's vocabulary.
///
/// The two enums are deliberately near-identical (see `signature_line.rs`), so
/// this is a mapping and not a re-implementation. Lifetimes have no dedicated
/// component variant and render as generics, which is how they read anyway.
fn prepare_sig_token(t: &WireSigToken) -> UiSigToken {
    match t {
        WireSigToken::Kw(s) => UiSigToken::Kw(s),
        WireSigToken::Ident(s) => UiSigToken::Ident(SharedString::from(s.to_string())),
        WireSigToken::Ty { text, target } => UiSigToken::Ty {
            text: SharedString::from(text.to_string()),
            // `SignatureLine`'s key is still the pre-wire mirror; until it
            // adopts `nudox_engine::wire::SymbolKey` (its own TODO(wire)), a
            // resolved target is carried as its debug identity so the link is
            // present and clickable rather than silently dropped.
            target: target
                .as_ref()
                .map(|k| UiSymbolKey(SharedString::from(format!("{k:?}")))),
        },
        WireSigToken::Punct(s) => UiSigToken::Punct(s),
        WireSigToken::Ws => UiSigToken::Ws,
        WireSigToken::Generic(s) => UiSigToken::Generic(SharedString::from(s.to_string())),
        WireSigToken::Lifetime(s) => UiSigToken::Generic(SharedString::from(s.to_string())),
        // `WireSigToken` is `#[non_exhaustive]`: an unknown token renders as a
        // visible mark rather than vanishing, so a signature never silently
        // loses a piece (LD-7).
        _ => UiSigToken::Punct("?"),
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Section data
// ─────────────────────────────────────────────────────────────────────────────

/// What one section is currently doing (§15 states).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SectionStatus {
    /// No query issued yet.
    Idle,
    /// Query out, rows not yet delivered. Only the semantic section shows a
    /// shimmer for this — local sections beat the shimmer grace and would only
    /// flash (§8.1 grace periods, §15 "no skeleton" for local).
    Loading,
    /// Rows delivered.
    Ready,
    /// The backend is unreachable; this section is replaced by a `trust.stale`
    /// notice rather than an error (§15 offline state).
    Offline,
}

/// One section's render-ready state.
#[derive(Clone, Debug)]
pub struct SectionData {
    /// Rows, pre-sorted by score by the store. Cloning this is a refcount bump.
    pub rows: Arc<[PreparedRow]>,
    /// Current phase.
    pub status: SectionStatus,
    /// Pre-formatted per-section latency readout (e.g. `"4 ms"`). Empty until
    /// a `SearchEvent::Latency` for this section arrives.
    pub latency: SharedString,
}

impl Default for SectionData {
    fn default() -> Self {
        SectionData {
            rows: Arc::from([] as [PreparedRow; 0]),
            status: SectionStatus::Idle,
            latency: SharedString::default(),
        }
    }
}

/// Everything the overlay reads in one frame.
///
/// A single snapshot rather than a dozen getters: the view takes one immutable
/// borrow of the store, clones a handful of `Arc`s and `SharedString`s out of
/// it, and then never touches the store again while building elements — which
/// is what keeps `render` free of interleaved store borrows.
#[derive(Clone, Debug)]
pub struct SearchSnapshot {
    /// Current query text.
    pub input: SharedString,
    /// Active mode chip.
    pub mode: SearchMode,
    /// Scope chips, in display order.
    pub scopes: Arc<[ScopeChip]>,
    /// The three sections, indexed by [`Section::index`].
    pub sections: [SectionData; SECTION_COUNT],
    /// Recent symbols, shown when the input is empty (§15, from `NavHistory`).
    pub recents: Arc<[PreparedRow]>,
    /// Current selection, if any.
    pub selection: Option<Cursor>,
    /// Monotonic generation counter. Drives entrance identity (§4.1).
    pub generation: u64,
    /// When this generation's first page landed. `None` until it does.
    pub gen_arrival: Option<Instant>,
    /// Whether the semantic plane is unreachable.
    pub offline: bool,
}

impl Default for SearchSnapshot {
    fn default() -> Self {
        SearchSnapshot {
            input: SharedString::default(),
            mode: SearchMode::Auto,
            scopes: Arc::from([] as [ScopeChip; 0]),
            sections: [
                SectionData::default(),
                SectionData::default(),
                SectionData::default(),
            ],
            recents: Arc::from([] as [PreparedRow; 0]),
            selection: None,
            generation: 0,
            gen_arrival: None,
            offline: false,
        }
    }
}

impl SearchSnapshot {
    /// Row counts per section — the input to every selection-movement rule.
    pub fn counts(&self) -> [usize; SECTION_COUNT] {
        [
            self.sections[0].rows.len(),
            self.sections[1].rows.len(),
            self.sections[2].rows.len(),
        ]
    }

    /// Total hits across all sections.
    fn total_hits(&self) -> usize {
        self.counts().iter().sum()
    }

    /// The row a cursor points at, if it is in range.
    fn row_at(&self, cursor: Cursor) -> Option<&PreparedRow> {
        self.sections
            .get(cursor.section)
            .and_then(|s| s.rows.get(cursor.row))
    }

    /// Whether any section has finished at least once — i.e. whether "zero
    /// hits" is a real answer rather than "nothing has arrived yet".
    fn any_section_settled(&self) -> bool {
        self.sections.iter().any(|s| {
            matches!(
                s.status,
                SectionStatus::Ready | SectionStatus::Offline
            )
        })
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// The store contract  —  TODO(store)
// ─────────────────────────────────────────────────────────────────────────────

/// The contract this view needs from `SearchStore` (§12.3).
///
/// TODO(store): the concrete `SearchStore` entity is written separately; it
/// must `impl SearchAccess`. The read half is a single [`SearchSnapshot`]
/// producer so that adding a field to the overlay never adds a method to the
/// store. The write half is deliberately tiny: **movement policy lives in this
/// file**, as the pure functions [`step_cursor`], [`next_section_cursor`] and
/// [`jump_to_section_cursor`], because §15's "never wraps silently across
/// sections" is a *view* invariant and is unit-tested here. The store only
/// records where the cursor ended up.
pub trait SearchAccess: 'static + Sized {
    /// Everything the overlay reads for one frame.
    ///
    /// Implementations return pre-computed state (§12: stores hold
    /// render-ready state); this must not sort, filter, or format.
    fn snapshot(&self) -> SearchSnapshot;

    /// The user typed. Restarts the 24 ms debounce and supersedes the
    /// generation (§7.4).
    fn set_input(&mut self, text: SharedString, cx: &mut Context<Self>);

    /// A mode chip was clicked; re-issue the query.
    fn set_mode(&mut self, mode: SearchMode, cx: &mut Context<Self>);

    /// A scope chip at `ix` in [`SearchSnapshot::scopes`] was toggled.
    fn toggle_scope(&mut self, ix: usize, cx: &mut Context<Self>);

    /// Record the new cursor. Pure state; the view has already applied policy.
    fn set_selection(&mut self, cursor: Option<Cursor>, cx: &mut Context<Self>);

    /// Re-issue the current query against the remote INDEX (§15 zero-hit
    /// empty-state action).
    fn search_remote(&mut self, cx: &mut Context<Self>);
}

/// A stand-in [`SearchAccess`] so this view compiles and can be driven before
/// the real store lands.
///
/// TODO(store): delete once `SearchStore: SearchAccess` exists.
#[derive(Default)]
pub struct StubSearchStore {
    /// The snapshot handed to the view verbatim.
    pub snapshot: SearchSnapshot,
}

impl SearchAccess for StubSearchStore {
    fn snapshot(&self) -> SearchSnapshot {
        self.snapshot.clone()
    }

    fn set_input(&mut self, text: SharedString, cx: &mut Context<Self>) {
        self.snapshot.input = text;
        self.snapshot.generation = self.snapshot.generation.wrapping_add(1);
        self.snapshot.gen_arrival = Some(Instant::now());
        cx.notify();
    }

    fn set_mode(&mut self, mode: SearchMode, cx: &mut Context<Self>) {
        self.snapshot.mode = mode;
        cx.notify();
    }

    fn toggle_scope(&mut self, ix: usize, cx: &mut Context<Self>) {
        let mut scopes = self.snapshot.scopes.to_vec();
        if let Some(chip) = scopes.get_mut(ix) {
            chip.active = !chip.active;
            self.snapshot.scopes = Arc::from(scopes);
            cx.notify();
        }
    }

    fn set_selection(&mut self, cursor: Option<Cursor>, cx: &mut Context<Self>) {
        self.snapshot.selection = cursor;
        cx.notify();
    }

    fn search_remote(&mut self, cx: &mut Context<Self>) {
        cx.notify();
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Events
// ─────────────────────────────────────────────────────────────────────────────

/// How a committed hit should open (mirrors `stores::events::OpenDisposition`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OpenDisposition {
    /// `enter` — open and close the overlay.
    Replace,
    /// `cmd-enter` — open without closing the overlay.
    KeepOverlay,
    /// `alt-enter` — open in a background tab; the overlay keeps focus.
    BackgroundTab,
}

/// What the overlay reports upward. The shell owns the overlay stack (§13.5),
/// so this view never closes itself — it says what happened.
#[derive(Clone, Debug)]
pub enum OmniSearchEvent {
    /// Open this symbol with the given disposition.
    Open {
        /// The hit's stable identity.
        key: SymbolKey,
        /// Where to put it.
        disposition: OpenDisposition,
    },
    /// `esc` on an already-empty input: pop this overlay.
    Dismiss,
}

// ─────────────────────────────────────────────────────────────────────────────
// Selection policy — pure, and therefore testable without a window
// ─────────────────────────────────────────────────────────────────────────────

/// Direction of an `↑`/`↓` step.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Step {
    /// `↑`
    Up,
    /// `↓`
    Down,
}

/// The first non-empty section at or after `from`, searching forward.
fn first_populated_from(counts: &[usize; SECTION_COUNT], from: usize) -> Option<usize> {
    (from..SECTION_COUNT).find(|&s| counts[s] > 0)
}

/// The last non-empty section at or before `from`, searching backward.
fn last_populated_before(counts: &[usize; SECTION_COUNT], from: usize) -> Option<usize> {
    (0..=from.min(SECTION_COUNT - 1))
        .rev()
        .find(|&s| counts[s] > 0)
}

/// Move the cursor one row, crossing into an adjacent section when it runs off
/// the end of the current one.
///
/// Two properties matter and are tested below:
///
/// 1. **No silent wrap.** Stepping down from the last row of the last populated
///    section stays put; it does not jump back to the top. §15 requires that a
///    section change be *announced* (by scrolling that section's sticky header
///    into place), and a wrap from bottom to top is the one movement that
///    cannot be announced coherently.
/// 2. **Empty sections are transparent.** A section that has not arrived yet —
///    the usual state of Semantic — is skipped rather than trapping the cursor.
///
/// Returns `None` when there is nothing selectable at all.
pub fn step_cursor(
    current: Option<Cursor>,
    step: Step,
    counts: &[usize; SECTION_COUNT],
) -> Option<Cursor> {
    let Some(cursor) = current else {
        // Nothing selected: `↓` takes the first hit, `↑` takes the last.
        return match step {
            Step::Down => first_populated_from(counts, 0).map(|s| Cursor {
                section: s,
                row: 0,
            }),
            Step::Up => last_populated_before(counts, SECTION_COUNT - 1).map(|s| Cursor {
                section: s,
                row: counts[s].saturating_sub(1),
            }),
        };
    };

    // A cursor can be left dangling by a generation that shrank a section;
    // re-seat it before moving so movement is always well-defined.
    let cursor = clamp_cursor(cursor, counts)?;

    match step {
        Step::Down => {
            if cursor.row + 1 < counts[cursor.section] {
                Some(Cursor {
                    section: cursor.section,
                    row: cursor.row + 1,
                })
            } else {
                match first_populated_from(counts, cursor.section + 1) {
                    Some(s) => Some(Cursor {
                        section: s,
                        row: 0,
                    }),
                    // Last row of the last populated section: hold.
                    None => Some(cursor),
                }
            }
        }
        Step::Up => {
            if cursor.row > 0 {
                Some(Cursor {
                    section: cursor.section,
                    row: cursor.row - 1,
                })
            } else if cursor.section == 0 {
                Some(cursor)
            } else {
                match last_populated_before(counts, cursor.section - 1) {
                    Some(s) => Some(Cursor {
                        section: s,
                        row: counts[s].saturating_sub(1),
                    }),
                    None => Some(cursor),
                }
            }
        }
    }
}

/// `tab` / `shift-tab`: jump to the first row of the next (or previous)
/// populated section, without wrapping past the ends.
pub fn next_section_cursor(
    current: Option<Cursor>,
    forward: bool,
    counts: &[usize; SECTION_COUNT],
) -> Option<Cursor> {
    let here = current.and_then(|c| clamp_cursor(c, counts));
    if forward {
        let start = here.map(|c| c.section + 1).unwrap_or(0);
        match first_populated_from(counts, start) {
            Some(section) => Some(Cursor { section, row: 0 }),
            // Already in the last populated section: hold there.
            None => here.map(|c| Cursor {
                section: c.section,
                row: 0,
            }),
        }
    } else {
        let Some(current_cursor) = here else {
            return last_populated_before(counts, SECTION_COUNT - 1)
                .map(|section| Cursor { section, row: 0 });
        };
        if current_cursor.section == 0 {
            return Some(Cursor {
                section: 0,
                row: 0,
            });
        }
        match last_populated_before(counts, current_cursor.section - 1) {
            Some(section) => Some(Cursor { section, row: 0 }),
            None => Some(Cursor {
                section: current_cursor.section,
                row: 0,
            }),
        }
    }
}

/// `cmd-1/2/3`: jump directly to a section, if it has rows.
pub fn jump_to_section_cursor(
    section: Section,
    counts: &[usize; SECTION_COUNT],
) -> Option<Cursor> {
    let ix = section.index();
    (counts[ix] > 0).then_some(Cursor {
        section: ix,
        row: 0,
    })
}

/// Re-seat a cursor that a newly-arrived generation may have invalidated.
///
/// Semantic results landing is the common case: §15's acceptance says that
/// arrival "never moves the selection", so this only intervenes when the row
/// the cursor pointed at genuinely no longer exists.
pub fn clamp_cursor(cursor: Cursor, counts: &[usize; SECTION_COUNT]) -> Option<Cursor> {
    if cursor.section < SECTION_COUNT && cursor.row < counts[cursor.section] {
        return Some(cursor);
    }
    let section = cursor.section.min(SECTION_COUNT - 1);
    if counts[section] > 0 {
        return Some(Cursor {
            section,
            row: counts[section] - 1,
        });
    }
    first_populated_from(counts, 0).map(|s| Cursor { section: s, row: 0 })
}

/// Keys that `app::keymaps` binds to actions in the `Overlay`/`OmniSearch`
/// contexts, and which must therefore never be treated as text.
fn is_navigation_key(key: &str) -> bool {
    matches!(
        key,
        "up" | "down"
            | "left"
            | "right"
            | "tab"
            | "enter"
            | "escape"
            | "home"
            | "end"
            | "pageup"
            | "pagedown"
    )
}

/// Delete back to the start of the previous word (`alt-backspace`).
fn truncate_last_word(text: &mut String) {
    while text.ends_with(char::is_whitespace) {
        text.pop();
    }
    while !text.is_empty() && !text.ends_with(char::is_whitespace) {
        text.pop();
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// The selection bar
// ─────────────────────────────────────────────────────────────────────────────

/// The gliding selection bar, painted as a `uniform_list` decoration.
///
/// # Why a decoration and not an absolutely-positioned sibling
///
/// A sibling positioned at `top: row_ix * row_height` is only correct while the
/// list is scrolled to the top; the moment the list scrolls, the rows move and
/// the bar does not. `UniformListDecoration::compute` is handed the list's
/// *content* origin (`padded_bounds.origin + scroll_offset`), so an element
/// placed at `top: y` inside it lands on content row `y / item_height` at any
/// scroll position, sub-pixel exact, clipped by the list's own content mask.
/// That is what makes the bar track the rows instead of sliding over them.
///
/// The spring value is snapshotted into `y` once per frame by the view, so the
/// decoration holds no state and needs no interior mutability.
struct SelectionBar {
    /// Current spring position, in content-space pixels.
    y: f32,
    /// Fill colour, already alpha-adjusted.
    fill: Hsla,
    /// Leading-edge colour.
    edge: Hsla,
    /// Width of the leading edge.
    edge_width: Pixels,
    /// Corner radius.
    radius: Pixels,
}

impl UniformListDecoration for SelectionBar {
    fn compute(
        &self,
        _visible_range: std::ops::Range<usize>,
        _bounds: Bounds<Pixels>,
        _scroll_offset: Point<Pixels>,
        item_height: Pixels,
        _item_count: usize,
        _window: &mut Window,
        _cx: &mut App,
    ) -> AnyElement {
        div()
            .size_full()
            .relative()
            .child(
                div()
                    .absolute()
                    .top(px(self.y))
                    .left_0()
                    .right_0()
                    .h(item_height)
                    .rounded(self.radius)
                    .bg(self.fill)
                    .border_l(self.edge_width)
                    .border_color(self.edge),
            )
            .into_any_element()
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Per-section view-owned runtime state
// ─────────────────────────────────────────────────────────────────────────────

/// State the *view* owns for one section: its scroll handle and its count
/// ticker spring (§15 motion: `count.tick` per-section counts).
struct SectionRuntime {
    /// Scroll handle, so selection movement can keep the cursor on screen and
    /// `cmd-1/2/3` can bring a section's header into place.
    scroll: UniformListScrollHandle,
    /// `count.tick` spring (GENTLE per §5.1).
    count: Motion,
    /// Last integer the spring displayed, so the label is re-formatted only
    /// when it actually changes rather than on every frame (§1.1.4).
    last_count: i64,
    /// The pre-formatted label handed to [`CountLabel`].
    count_label: SharedString,
}

impl SectionRuntime {
    fn new() -> Self {
        SectionRuntime {
            scroll: UniformListScrollHandle::new(),
            count: Motion::new(0.0, Spring::GENTLE),
            last_count: 0,
            count_label: SharedString::from("0"),
        }
    }

    /// Advance the count ticker toward `target`, returning `true` while moving.
    fn tick_count(&mut self, now: Instant, target: f32, reduced: bool) -> bool {
        if reduced || should_skip(self.count.value(), target) {
            // §5.1: a jump of more than 500 is information the user wants
            // immediately, not a six-hundred-millisecond roll.
            self.count.snap_to(target);
        } else {
            self.count.animate_to(target);
        }

        let moving = if reduced { false } else { self.count.tick(now) };

        let rounded = count_rounds(self.count.value());
        if rounded != self.last_count {
            self.last_count = rounded;
            self.count_label = SharedString::from(rounded.to_string());
        }
        moving
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Footer hints
// ─────────────────────────────────────────────────────────────────────────────

/// A pre-parsed footer hint. `Keystroke::parse` returns a `Result`, so parsing
/// happens once at construction and never in render.
struct FooterHint {
    description: SharedString,
    keys: Vec<Keystroke>,
}

/// The §15 footer, in display order.
const FOOTER_SPEC: [(&str, &str); 5] = [
    ("open", "enter"),
    ("keep open", "cmd-enter"),
    ("background", "alt-enter"),
    ("section", "tab"),
    ("close", "escape"),
];

fn build_footer_hints() -> Vec<FooterHint> {
    FOOTER_SPEC
        .iter()
        .filter_map(|(description, chord)| {
            // A chord that fails to parse is dropped rather than unwrapped: a
            // missing hint is a cosmetic loss, a panic on startup is not.
            Keystroke::parse(chord).ok().map(|k| FooterHint {
                description: SharedString::from(*description),
                keys: vec![k],
            })
        })
        .collect()
}

// ─────────────────────────────────────────────────────────────────────────────
// OmniSearch
// ─────────────────────────────────────────────────────────────────────────────

/// The `cmd-K` overlay (§15).
///
/// Generic over the store so this file compiles and is testable before the real
/// `SearchStore` exists; the shell instantiates `OmniSearch<SearchStore>`.
pub struct OmniSearch<S: SearchAccess> {
    /// The store (LD-1: views subscribe to stores and never do I/O).
    store: Entity<S>,
    /// Focus handle. The overlay traps focus while open (§13.5).
    focus: FocusHandle,

    /// Per-section scroll handles and count tickers.
    sections: [SectionRuntime; SECTION_COUNT],

    /// The gliding selection bar's y-offset, in content-space pixels.
    bar: Motion,
    /// Which section the bar currently lives in. When the cursor moves to a
    /// different section the bar *snaps*, because the two positions are in
    /// different coordinate spaces and interpolating between them would send
    /// the bar sliding through a header it never occupies.
    bar_section: Option<usize>,

    /// Overlay entrance: opacity 0→1 (§5.3 `overlay.in`, SNAPPY).
    enter_opacity: Motion,
    /// Overlay entrance: 4 px rise.
    enter_rise: Motion,

    /// Held for as long as the semantic shimmer is on screen (§5.4 cap of 3).
    shimmer_permit: Option<LoopPermit>,

    /// Pre-parsed footer hints.
    footer: Vec<FooterHint>,

    /// Row height in pixels, derived from type tokens on first render and
    /// reused by the bar spring so the spring and the layout cannot disagree.
    row_height: f32,
}

impl<S: SearchAccess> OmniSearch<S> {
    /// Build the overlay over `store` and take focus.
    pub fn new(store: Entity<S>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let focus = cx.focus_handle();
        window.focus(&focus, cx);

        let mut enter_opacity = Motion::new(0.0, Spring::SNAPPY);
        enter_opacity.animate_to(1.0);
        let mut enter_rise = Motion::new(OVERLAY_RISE_PX, Spring::SNAPPY);
        enter_rise.animate_to(0.0);

        OmniSearch {
            store,
            focus,
            sections: [
                SectionRuntime::new(),
                SectionRuntime::new(),
                SectionRuntime::new(),
            ],
            bar: Motion::new(0.0, Spring::SNAPPY),
            bar_section: None,
            enter_opacity,
            enter_rise,
            shimmer_permit: None,
            footer: build_footer_hints(),
            row_height: 0.0,
        }
    }

    // ── Selection ────────────────────────────────────────────────────────────

    /// Apply a new cursor: record it in the store, retarget or snap the bar,
    /// and bring the row into view.
    fn apply_cursor(&mut self, next: Option<Cursor>, cx: &mut Context<Self>) {
        let row_height = self.row_height;
        let same_section = match (self.bar_section, next) {
            (Some(prev), Some(c)) => prev == c.section,
            _ => false,
        };

        if let Some(cursor) = next {
            let target = cursor.row as f32 * row_height;
            if same_section {
                // Same coordinate space: glide (§15's most-felt detail).
                self.bar.animate_to(target);
            } else {
                // Crossing a section boundary is announced by the header
                // scroll, not by sliding the bar across a gap it never covers.
                self.bar.snap_to(target);
            }
            self.bar_section = Some(cursor.section);

            if let Some(runtime) = self.sections.get(cursor.section) {
                // `Nearest` leaves an already-visible row exactly where it is,
                // so a keyboard step never jolts the list.
                runtime
                    .scroll
                    .scroll_to_item(cursor.row, ScrollStrategy::Nearest);
            }
        } else {
            self.bar_section = None;
        }

        self.store
            .update(cx, |store, cx| store.set_selection(next, cx));
        cx.notify();
    }

    /// Current counts, read straight from the store.
    fn counts(&self, cx: &App) -> [usize; SECTION_COUNT] {
        self.store.read(cx).snapshot().counts()
    }

    /// Current cursor, read straight from the store.
    fn selection(&self, cx: &App) -> Option<Cursor> {
        self.store.read(cx).snapshot().selection
    }

    // ── Action handlers (bound in `app::keymaps`, contexts Overlay/OmniSearch)

    fn on_move_up(&mut self, _: &MoveSelectionUp, _: &mut Window, cx: &mut Context<Self>) {
        let counts = self.counts(cx);
        let next = step_cursor(self.selection(cx), Step::Up, &counts);
        self.apply_cursor(next, cx);
    }

    fn on_move_down(&mut self, _: &MoveSelectionDown, _: &mut Window, cx: &mut Context<Self>) {
        let counts = self.counts(cx);
        let next = step_cursor(self.selection(cx), Step::Down, &counts);
        self.apply_cursor(next, cx);
    }

    fn on_next_section(&mut self, _: &NextSection, _: &mut Window, cx: &mut Context<Self>) {
        let counts = self.counts(cx);
        let next = next_section_cursor(self.selection(cx), true, &counts);
        self.announce_section(next);
        self.apply_cursor(next, cx);
    }

    fn on_prev_section(&mut self, _: &PrevSection, _: &mut Window, cx: &mut Context<Self>) {
        let counts = self.counts(cx);
        let next = next_section_cursor(self.selection(cx), false, &counts);
        self.announce_section(next);
        self.apply_cursor(next, cx);
    }

    fn on_jump_1(&mut self, _: &JumpToSection1, _: &mut Window, cx: &mut Context<Self>) {
        self.jump(Section::Name, cx);
    }

    fn on_jump_2(&mut self, _: &JumpToSection2, _: &mut Window, cx: &mut Context<Self>) {
        self.jump(Section::Type, cx);
    }

    fn on_jump_3(&mut self, _: &JumpToSection3, _: &mut Window, cx: &mut Context<Self>) {
        self.jump(Section::Semantic, cx);
    }

    fn jump(&mut self, section: Section, cx: &mut Context<Self>) {
        let counts = self.counts(cx);
        let next = jump_to_section_cursor(section, &counts);
        self.announce_section(next);
        self.apply_cursor(next, cx);
    }

    /// §15: a section change is announced by scrolling that section's sticky
    /// header into place, so the user sees *which* section they landed in.
    fn announce_section(&mut self, next: Option<Cursor>) {
        if let Some(cursor) = next {
            if let Some(runtime) = self.sections.get(cursor.section) {
                runtime.scroll.scroll_to_item(0, ScrollStrategy::Top);
            }
        }
    }

    fn on_confirm(&mut self, _: &ConfirmOverlay, _: &mut Window, cx: &mut Context<Self>) {
        self.commit(OpenDisposition::Replace, cx);
    }

    fn on_open_keep(&mut self, _: &OpenWithoutClosing, _: &mut Window, cx: &mut Context<Self>) {
        self.commit(OpenDisposition::KeepOverlay, cx);
    }

    fn on_open_background(
        &mut self,
        _: &OpenInBackgroundTab,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.commit(OpenDisposition::BackgroundTab, cx);
    }

    fn commit(&mut self, disposition: OpenDisposition, cx: &mut Context<Self>) {
        let snapshot = self.store.read(cx).snapshot();
        let Some(cursor) = snapshot.selection else {
            return;
        };
        let Some(row) = snapshot.row_at(cursor) else {
            return;
        };
        let key = row.key.clone();
        cx.emit(OmniSearchEvent::Open { key, disposition });
    }

    /// `esc`: clear a non-empty input first, and only close on the second press
    /// (§15). Escape that destroys a query the user is still refining is the
    /// fastest way to make an overlay feel hostile.
    fn on_dismiss(&mut self, _: &DismissOverlay, _: &mut Window, cx: &mut Context<Self>) {
        let input = self.store.read(cx).snapshot().input;
        if input.is_empty() {
            cx.emit(OmniSearchEvent::Dismiss);
        } else {
            self.set_input(SharedString::default(), cx);
        }
    }

    // ── Text entry ───────────────────────────────────────────────────────────

    fn set_input(&mut self, text: SharedString, cx: &mut Context<Self>) {
        self.store.update(cx, |store, cx| store.set_input(text, cx));
        // A new query invalidates the old cursor; drop it so the first `↓`
        // lands on the new top hit rather than resuming an unrelated position.
        self.bar_section = None;
        self.bar.snap_to(0.0);
        cx.notify();
    }

    /// Handle a key as *text*. Navigation keys are already actions (they are
    /// resolved by `app::keymaps` before key listeners run), so they are
    /// filtered out here and never double-handled.
    fn on_key_down(&mut self, event: &KeyDownEvent, _: &mut Window, cx: &mut Context<Self>) {
        let keystroke = &event.keystroke;
        let modifiers = keystroke.modifiers;

        if is_navigation_key(&keystroke.key) {
            return;
        }

        let current = self.store.read(cx).snapshot().input;

        if modifiers.platform || modifiers.control {
            match keystroke.key.as_str() {
                "v" => {
                    if let Some(pasted) = cx.read_from_clipboard().and_then(|item| item.text()) {
                        let mut next = current.to_string();
                        next.push_str(pasted.trim());
                        self.set_input(SharedString::from(next), cx);
                    }
                }
                // `cmd-backspace` / `ctrl-u`: clear the line.
                "backspace" | "u" => self.set_input(SharedString::default(), cx),
                _ => {}
            }
            return;
        }

        if keystroke.key == "backspace" {
            if current.is_empty() {
                return;
            }
            let mut next = current.to_string();
            if modifiers.alt {
                truncate_last_word(&mut next);
            } else {
                next.pop();
            }
            self.set_input(SharedString::from(next), cx);
            return;
        }

        let Some(typed) = keystroke.key_char.as_deref() else {
            return;
        };
        if typed.is_empty() || typed.chars().any(char::is_control) {
            return;
        }
        let mut next = current.to_string();
        next.push_str(typed);
        self.set_input(SharedString::from(next), cx);
    }

    // ── Motion ───────────────────────────────────────────────────────────────

    /// Advance every spring. Returns `true` while anything is still moving, in
    /// which case the caller requests another frame for *this entity only*
    /// (§4.2 render-loop contract).
    fn tick(&mut self, now: Instant, snapshot: &SearchSnapshot, reduced: bool) -> bool {
        let counts = snapshot.counts();

        if reduced {
            self.enter_opacity.snap_to(1.0);
            self.enter_rise.snap_to(0.0);
            if let Some(cursor) = snapshot.selection {
                self.bar.snap_to(cursor.row as f32 * self.row_height);
                self.bar_section = Some(cursor.section);
            }
            for (ix, runtime) in self.sections.iter_mut().enumerate() {
                runtime.tick_count(now, counts[ix] as f32, true);
            }
            return false;
        }

        let mut animating = false;
        animating |= self.enter_opacity.tick(now);
        animating |= self.enter_rise.tick(now);
        animating |= self.bar.tick(now);

        for (ix, runtime) in self.sections.iter_mut().enumerate() {
            animating |= runtime.tick_count(now, counts[ix] as f32, false);
        }

        animating
    }

    /// Place the bar for a selection this view did not itself move.
    ///
    /// View-local by design: it never writes to the store and never notifies.
    /// Correcting store state from inside `render` would mean mutating another
    /// entity mid-frame, and a `notify` from render is how render loops are
    /// born. A cursor left dangling by a shrinking generation is harmless —
    /// [`step_cursor`] re-seats it on the next keypress and
    /// [`SearchSnapshot::row_at`] simply yields nothing until then.
    ///
    /// §15's acceptance ("semantic arriving never moves the selection") holds
    /// because [`clamp_cursor`] is a no-op whenever the row still exists.
    fn reconcile_selection(&mut self, snapshot: &SearchSnapshot) {
        let counts = snapshot.counts();
        let Some(cursor) = snapshot.selection.and_then(|c| clamp_cursor(c, &counts)) else {
            self.bar_section = None;
            return;
        };
        if self.bar_section != Some(cursor.section) {
            // First paint after a selection that did not come from a key (a
            // click, or a restored session): place the bar without a glide.
            self.bar_section = Some(cursor.section);
            self.bar.snap_to(cursor.row as f32 * self.row_height);
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Rendering
// ─────────────────────────────────────────────────────────────────────────────

impl<S: SearchAccess> OmniSearch<S> {
    /// Row height, derived from the type scale rather than fixed.
    ///
    /// A row is one `ui` line (the name) plus one `mono` line (the signature)
    /// plus a `space_2` gutter. Deriving it means changing the type scale moves
    /// the rows and the selection bar together — they cannot drift apart,
    /// because the bar's spring targets `row * this`.
    fn row_height(cx: &App) -> f32 {
        let ext = cx.theme_ext();
        f32::from(ext.type_scale.ui.line_height)
            + f32::from(ext.type_scale.mono.line_height)
            + f32::from(ext.space.space_2)
    }

    /// The input row: query text with a caret, then mode and scope chips.
    fn render_input_row(&self, snapshot: &SearchSnapshot, cx: &App) -> AnyElement {
        let ext = cx.theme_ext();
        let sp = ext.space;
        let ts = ext.type_scale;
        let colours = ext.colours;
        let store = self.store.clone();

        let query_is_empty = snapshot.input.is_empty();
        let query: SharedString = if query_is_empty {
            SharedString::from("Search symbols, types, or ask in prose")
        } else {
            snapshot.input.clone()
        };

        let query_line = h_flex()
            .w_full()
            .items_center()
            .gap(sp.space_2)
            .px(sp.space_3)
            .py(sp.space_3)
            .child(
                Icon::new(IconName::Search).text_color(colours.fg_faint),
            )
            .child(
                div()
                    .flex_1()
                    .overflow_hidden()
                    .whitespace_nowrap()
                    .text_size(ts.title.size)
                    .line_height(ts.title.line_height)
                    .text_color(if query_is_empty {
                        colours.fg_faint
                    } else {
                        colours.fg_default
                    })
                    .child(query),
            )
            // The caret. Static rather than blinking: a blink is an infinite
            // animation and §5.4 only allows three in the whole app — a search
            // caret is not worth one of them.
            .when(!query_is_empty, |el| {
                el.child(
                    div()
                        .w(sp.border_width)
                        .h(ts.title.line_height)
                        .bg(colours.accent),
                )
            });

        let mode = snapshot.mode;
        let mut chips = Toolbar::new("search.chips").chips(SearchMode::ALL.iter().map(|m| {
            let m = *m;
            let store = store.clone();
            ToolbarChip::new(("search.chip.mode", m as usize), m.label())
                .active(mode == m)
                .on_click(move |_, cx| {
                    store.update(cx, |s, cx| s.set_mode(m, cx));
                })
        }));

        for (ix, scope) in snapshot.scopes.iter().enumerate() {
            let store = store.clone();
            chips = chips.chip(
                ToolbarChip::new(("search.chip.scope", ix), scope.label.clone())
                    .active(scope.active)
                    .on_click(move |_, cx| {
                        store.update(cx, |s, cx| s.toggle_scope(ix, cx));
                    }),
            );
        }

        v_flex()
            .w_full()
            .child(query_line)
            .child(chips)
            .into_any_element()
    }

    /// One hit row: kind badge · name · signature · path · trust badge.
    ///
    /// A free function of `(section, ix, row)` rather than a method, because it
    /// runs inside the `'static` `uniform_list` closure which cannot borrow the
    /// view.
    fn render_row(
        section: Section,
        ix: usize,
        row: &PreparedRow,
        row_height: Pixels,
        cx: &mut App,
    ) -> gpui::Stateful<gpui::Div> {
        let ext = cx.theme_ext();
        let sp = ext.space;
        let ts = ext.type_scale;
        let colours = ext.colours;

        let kind_badge = match row.kind {
            Some(kind) => Badge::for_kind(("search.row.kind", ix), kind, cx),
            // LD-7: a kind from a newer producer is a visible chip, never a gap.
            None => Badge::custom(
                ("search.row.kind", ix),
                row.unknown_kind_label.clone(),
                colours.bg_hover,
                colours.fg_muted,
            ),
        };

        div()
            .id((section.row_id(), ix))
            .h(row_height)
            .w_full()
            .px(sp.space_3)
            .flex()
            .items_center()
            .gap(sp.space_2)
            .cursor_pointer()
            // `hover:` is a GPUI style state — no notify, no frame (§5.1).
            .hover(|s| s.bg(colours.bg_hover))
            .child(kind_badge)
            .child(
                v_flex()
                    .flex_1()
                    .overflow_hidden()
                    .child(
                        h_flex()
                            .items_baseline()
                            .gap(sp.space_2)
                            .overflow_hidden()
                            .whitespace_nowrap()
                            .child(
                                div()
                                    .text_size(ts.ui.size)
                                    .line_height(ts.ui.line_height)
                                    .font_weight(gpui::FontWeight(ts.title.weight as f32))
                                    .text_color(colours.fg_default)
                                    .child(row.leaf.clone()),
                            )
                            .when(!row.path.is_empty(), |el| {
                                el.child(
                                    div()
                                        .text_size(ts.caption.size)
                                        .line_height(ts.caption.line_height)
                                        .text_color(colours.fg_faint)
                                        .opacity(PATH_OPACITY)
                                        .child(row.path.clone()),
                                )
                            }),
                    )
                    .child(
                        div().overflow_hidden().child(SignatureLine::new(
                            ("search.row.sig", ix),
                            row.sig.clone(),
                        )),
                    ),
            )
            .child(Badge::for_provenance(
                ("search.row.trust", ix),
                row.provenance,
                cx,
            ))
    }

    /// One section: sticky caption header (count + latency) over a virtualized
    /// list carrying the selection-bar decoration when the cursor lives here.
    fn render_section(
        &self,
        section: Section,
        snapshot: &SearchSnapshot,
        scale: f32,
        cx: &Context<Self>,
    ) -> AnyElement {
        let index = section.index();
        let data = &snapshot.sections[index];
        let ext = cx.theme_ext();
        let sp = ext.space;
        let ts = ext.type_scale;
        let colours = ext.colours;
        let radius = sp.r_sm;
        let edge_width = sp.focus_ring_width;
        let accent = colours.accent;
        let fill = accent.opacity(SELECTION_FILL_ALPHA);

        let row_height_px = self.row_height;
        let row_height = px(row_height_px);
        let count = data.rows.len();

        // The sticky header. It is a sibling *above* the list rather than a
        // child of it, so it never scrolls with the rows — which is what
        // "sticky" has to mean when the rows are virtualized.
        // The count is the *ticking* one (§15 `count.tick`), so it goes in the
        // action slot as a `CountLabel` driven by this section's spring rather
        // than in `SectionHeader`'s static `count` slot — two counts in one
        // header would disagree with each other mid-roll.
        let header = SectionHeader::new(section.caption()).action(
            h_flex()
                .gap(sp.space_2)
                .items_center()
                .child(CountLabel::new(self.sections[index].count_label.clone()))
                .when(!data.latency.is_empty(), |el| {
                    el.child(
                        div()
                            .text_size(ts.caption.size)
                            .line_height(ts.caption.line_height)
                            .text_color(colours.fg_faint)
                            .child(data.latency.clone()),
                    )
                })
                .into_any_element(),
        );

        // §15 offline: the semantic section is replaced by a `trust.stale`
        // notice, not an error — the rest of the results are still true.
        if data.status == SectionStatus::Offline {
            return v_flex()
                .w_full()
                .child(header)
                .child(Self::render_stale_notice(cx))
                .into_any_element();
        }

        // §15 states: the semantic section — and only it — shimmers while the
        // local sections are already painted.
        if count == 0 && data.status == SectionStatus::Loading {
            return v_flex()
                .w_full()
                .child(header)
                .child(self.render_shimmer(row_height, scale, cx))
                .into_any_element();
        }

        if count == 0 {
            return div().into_any_element();
        }

        let rows = data.rows.clone();
        let generation = snapshot.generation;
        // §4.1: entrances fire only inside the generation's 400 ms window, so
        // scrolling a virtualized list never replays them.
        let cascading = snapshot
            .gen_arrival
            .is_some_and(|at| at.elapsed() < ROW_CASCADE_WINDOW)
            && scale > 0.0;
        let namespace = section.entrance_namespace();
        let this = cx.weak_entity();

        let list = uniform_list(
            section.list_id(),
            count,
            move |range, _window, cx| {
                // One token set per visible page, not per row: `MotionTokens`
                // carries a census handle and constructing it is not free.
                let tokens = MotionTokens::new(scale);
                range
                    .map(|ix| {
                        let Some(row) = rows.get(ix) else {
                            return div().into_any_element();
                        };
                        let key = row.key.clone();
                        let this = this.clone();
                        let element = Self::render_row(section, ix, row, row_height, cx).on_click(
                            move |_, _window, cx| {
                                let key = key.clone();
                                let _ = this.update(cx, |view, cx| {
                                    view.apply_cursor(
                                        Some(Cursor {
                                            section: index,
                                            row: ix,
                                        }),
                                        cx,
                                    );
                                    cx.emit(OmniSearchEvent::Open {
                                        key,
                                        disposition: OpenDisposition::Replace,
                                    });
                                });
                            },
                        );

                        if cascading {
                            row_enter(
                                element,
                                entrance_id(namespace, generation, ix),
                                ix,
                                &tokens,
                            )
                        } else {
                            element.into_any_element()
                        }
                    })
                    .collect()
            },
        )
        .track_scroll(&self.sections[index].scroll);

        // The gliding bar rides in this list's scrolled content space, so it
        // only attaches to the section that actually holds the cursor.
        let list = match snapshot.selection {
            Some(cursor) if cursor.section == index => list.with_decoration(SelectionBar {
                y: self.bar.value(),
                fill,
                edge: accent,
                edge_width,
                radius,
            }),
            _ => list,
        };

        let visible = count.min(VISIBLE_ROWS_PER_SECTION);
        v_flex()
            .w_full()
            .child(header)
            .child(div().w_full().h(px(row_height_px * visible as f32)).child(list))
            .into_any_element()
    }

    /// The semantic section's three-row shimmer (§15 states, §5.2).
    fn render_shimmer(&self, row_height: Pixels, scale: f32, cx: &App) -> AnyElement {
        let ext = cx.theme_ext();
        let sp = ext.space;
        let colours = ext.colours;
        let ts = ext.type_scale;
        // No permit (census full, §5.4) or reduced motion: render at the
        // pulse's midpoint so a shed shimmer looks like a still frame of one,
        // never a differently-styled element (§6.2).
        let animate = self.shimmer_permit.is_some() && scale > 0.0;

        v_flex()
            .w_full()
            .children((0..SEMANTIC_SHIMMER_ROWS).map(|ix| {
                let block = h_flex()
                    .h(row_height)
                    .w_full()
                    .px(sp.space_3)
                    .items_center()
                    .gap(sp.space_2)
                    .child(
                        div()
                            .w(sp.space_6)
                            .h(ts.caption.line_height)
                            .rounded(sp.r_sm)
                            .bg(colours.bg_hover),
                    )
                    .child(
                        v_flex()
                            .flex_1()
                            .gap(sp.space_1)
                            .child(
                                div()
                                    .w_1_2()
                                    .h(ts.ui.size)
                                    .rounded(sp.r_sm)
                                    .bg(colours.bg_hover),
                            )
                            .child(
                                div()
                                    .w_3_4()
                                    .h(ts.caption.size)
                                    .rounded(sp.r_sm)
                                    .bg(colours.bg_hover),
                            ),
                    );

                if animate {
                    shimmer(block, ("search.shimmer", ix)).into_any_element()
                } else {
                    block.opacity(SHIMMER_SHED_OPACITY).into_any_element()
                }
            }))
            .into_any_element()
    }

    /// §15 offline: a one-line `trust.stale` notice in place of the semantic
    /// section. Not an error state — nothing failed, the plane is simply
    /// unreachable and the local answers above are still correct.
    fn render_stale_notice(cx: &App) -> AnyElement {
        let ext = cx.theme_ext();
        let sp = ext.space;
        let ts = ext.type_scale;
        let stale = ext.trust.stale;

        h_flex()
            .w_full()
            .items_center()
            .gap(sp.space_2)
            .px(sp.space_3)
            .py(sp.space_2)
            .child(div().w(sp.space_2).h(sp.space_2).rounded_full().bg(stale.colour))
            .child(
                div()
                    .text_size(ts.dense.size)
                    .line_height(ts.dense.line_height)
                    .text_color(ext.colours.fg_muted)
                    .child(SharedString::from(
                        "Offline — semantic results unavailable; local matches shown above",
                    )),
            )
            .into_any_element()
    }

    /// Recent symbols, shown while the input is empty (§15 states).
    fn render_recents(&self, snapshot: &SearchSnapshot, cx: &Context<Self>) -> AnyElement {
        let ext = cx.theme_ext();
        let sp = ext.space;
        let motion = MotionTokens::new(ext.motion_scale);

        if snapshot.recents.is_empty() {
            return div()
                .w_full()
                .p(sp.space_6)
                .child(EmptyState::new(
                    IconName::Search,
                    SharedString::from("Search everything"),
                    SharedString::from(
                        "Names, type signatures, or a plain-English description. Local results arrive instantly.",
                    ),
                    motion,
                ))
                .into_any_element();
        }

        let rows = snapshot.recents.clone();
        let count = rows.len();
        let row_height = px(self.row_height);
        let this = cx.weak_entity();

        let list = uniform_list("search.recents.list", count, move |range, _window, cx| {
            range
                .map(|ix| {
                    let Some(row) = rows.get(ix) else {
                        return div().into_any_element();
                    };
                    let key = row.key.clone();
                    let this = this.clone();
                    Self::render_row(Section::Name, ix, row, row_height, cx)
                        .on_click(move |_, _window, cx| {
                            let key = key.clone();
                            let _ = this.update(cx, |_view, cx| {
                                cx.emit(OmniSearchEvent::Open {
                                    key,
                                    disposition: OpenDisposition::Replace,
                                });
                            });
                        })
                        .into_any_element()
                })
                .collect()
        });

        let visible = count.min(VISIBLE_ROWS_PER_SECTION);
        v_flex()
            .w_full()
            .child(SectionHeader::new("Recent"))
            .child(
                div()
                    .w_full()
                    .h(px(self.row_height * visible as f32))
                    .child(list),
            )
            .into_any_element()
    }

    /// §15 zero-hit state: a designed screen with a remote-search escape hatch
    /// (LD-16 — an empty result is a state, not an absence).
    fn render_no_hits(&self, cx: &Context<Self>) -> AnyElement {
        let ext = cx.theme_ext();
        let sp = ext.space;
        let motion = MotionTokens::new(ext.motion_scale);
        let store = self.store.clone();

        div()
            .w_full()
            .p(sp.space_6)
            .child(
                EmptyState::new(
                    IconName::Inbox,
                    SharedString::from("No matches here"),
                    SharedString::from(
                        "Nothing in the local corpus matches. The remote INDEX covers packages you have not synced.",
                    ),
                    motion,
                )
                .action(SharedString::from("Search remote INDEX"), move |_, cx| {
                    store.update(cx, |s, cx| s.search_remote(cx));
                }),
            )
            .into_any_element()
    }

    /// The footer hint row (§15 layout).
    fn render_footer(&self, cx: &App) -> AnyElement {
        let ext = cx.theme_ext();
        let sp = ext.space;
        let colours = ext.colours;

        h_flex()
            .w_full()
            .items_center()
            .gap(sp.space_4)
            .px(sp.space_3)
            .py(sp.space_2)
            .border_t(sp.border_width)
            .border_color(colours.border_default)
            .bg(colours.bg_raised)
            .children(
                self.footer
                    .iter()
                    .map(|hint| KeyHint::new(hint.description.clone(), hint.keys.clone())),
            )
            .into_any_element()
    }
}

impl<S: SearchAccess> Focusable for OmniSearch<S> {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl<S: SearchAccess> EventEmitter<OmniSearchEvent> for OmniSearch<S> {}

impl<S: SearchAccess> Render for OmniSearch<S> {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let now = Instant::now();

        // Read the scale out as a plain `f32` rather than holding a borrow of
        // the global across element construction, which needs `&mut App`.
        let scale = cx.theme_ext().motion_scale;
        let reduced = scale == 0.0;
        self.row_height = Self::row_height(cx);

        let snapshot = self.store.read(cx).snapshot();
        self.reconcile_selection(&snapshot);

        // Hold a loop permit for exactly as long as the shimmer is on screen
        // (§5.4). Acquired from the *global* census, not a local clone, or the
        // cap would not be a cap.
        let wants_shimmer = snapshot.sections[Section::Semantic.index()].status
            == SectionStatus::Loading
            && snapshot.sections[Section::Semantic.index()].rows.is_empty();
        if wants_shimmer && self.shimmer_permit.is_none() && !reduced {
            self.shimmer_permit = cx.global::<MotionTokens>().acquire_loop_slot();
        } else if !wants_shimmer {
            self.shimmer_permit = None;
        }

        let animating = self.tick(now, &snapshot, reduced);
        if animating {
            // Notifies only this entity on the next frame (window.rs:2169).
            window.request_animation_frame();
        }

        let ext = cx.theme_ext();
        let sp = ext.space;
        let colours = ext.colours;
        let elevation = ext.elev.overlay.shadows.clone();
        let dark_border = ext.elev.overlay.dark_border;
        let opacity = self.enter_opacity.value();
        let rise = self.enter_rise.value();

        let showing_recents = snapshot.input.is_empty();
        let no_hits = !showing_recents && snapshot.total_hits() == 0;

        let body: AnyElement = if showing_recents {
            self.render_recents(&snapshot, cx)
        } else if no_hits && snapshot.any_section_settled() {
            self.render_no_hits(cx)
        } else {
            let mut column = v_flex().w_full();
            for section in Section::ALL {
                column = column.child(self.render_section(section, &snapshot, scale, cx));
            }
            column.into_any_element()
        };

        let panel = v_flex()
            .w(OVERLAY_WIDTH)
            .max_h(relative(OVERLAY_MAX_HEIGHT_FRACTION))
            .overflow_hidden()
            .bg(colours.bg_overlay)
            .rounded(sp.r_xl)
            .border(sp.border_width)
            .border_color(dark_border)
            .shadow(elevation)
            .child(self.render_input_row(&snapshot, cx))
            .child(
                div()
                    .id("search.results")
                    .flex_1()
                    .overflow_y_scroll()
                    .child(body),
            )
            .child(self.render_footer(cx));

        div()
            .id("search.overlay")
            .track_focus(&self.focus)
            // Both contexts: `Overlay` carries ↑↓/tab/enter/escape, `OmniSearch`
            // carries cmd-1/2/3 (see `app::keymaps`).
            .key_context("OmniSearch Overlay")
            .on_action(cx.listener(Self::on_move_up))
            .on_action(cx.listener(Self::on_move_down))
            .on_action(cx.listener(Self::on_next_section))
            .on_action(cx.listener(Self::on_prev_section))
            .on_action(cx.listener(Self::on_confirm))
            .on_action(cx.listener(Self::on_open_keep))
            .on_action(cx.listener(Self::on_open_background))
            .on_action(cx.listener(Self::on_dismiss))
            .on_action(cx.listener(Self::on_jump_1))
            .on_action(cx.listener(Self::on_jump_2))
            .on_action(cx.listener(Self::on_jump_3))
            .on_key_down(cx.listener(Self::on_key_down))
            .size_full()
            .flex()
            .justify_center()
            .items_start()
            .pt(sp.space_8)
            .opacity(opacity)
            .child(div().mt(px(rise)).child(panel))
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Tests
// ─────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn counts(a: usize, b: usize, c: usize) -> [usize; SECTION_COUNT] {
        [a, b, c]
    }

    #[test]
    fn down_from_nothing_selects_the_first_hit() {
        let c = counts(3, 2, 0);
        assert_eq!(
            step_cursor(None, Step::Down, &c),
            Some(Cursor {
                section: 0,
                row: 0
            })
        );
    }

    #[test]
    fn down_from_nothing_skips_an_empty_leading_section() {
        // Name has not arrived yet; the cursor must not land in a void.
        let c = counts(0, 4, 0);
        assert_eq!(
            step_cursor(None, Step::Down, &c),
            Some(Cursor {
                section: 1,
                row: 0
            })
        );
    }

    #[test]
    fn down_crosses_into_the_next_populated_section() {
        let c = counts(2, 0, 5);
        let at_end_of_name = Cursor {
            section: 0,
            row: 1,
        };
        assert_eq!(
            step_cursor(Some(at_end_of_name), Step::Down, &c),
            Some(Cursor {
                section: 2,
                row: 0
            }),
            "an empty Type section must be transparent, not a trap"
        );
    }

    /// The §15 invariant: movement never wraps silently across sections.
    #[test]
    fn down_at_the_very_end_holds_instead_of_wrapping() {
        let c = counts(2, 2, 0);
        let last = Cursor {
            section: 1,
            row: 1,
        };
        assert_eq!(
            step_cursor(Some(last), Step::Down, &c),
            Some(last),
            "stepping past the last hit must hold, never jump back to the top"
        );
    }

    #[test]
    fn up_at_the_very_start_holds_instead_of_wrapping() {
        let c = counts(2, 2, 0);
        let first = Cursor {
            section: 0,
            row: 0,
        };
        assert_eq!(step_cursor(Some(first), Step::Up, &c), Some(first));
    }

    #[test]
    fn up_crosses_back_to_the_last_row_of_the_previous_section() {
        let c = counts(3, 0, 2);
        let top_of_semantic = Cursor {
            section: 2,
            row: 0,
        };
        assert_eq!(
            step_cursor(Some(top_of_semantic), Step::Up, &c),
            Some(Cursor {
                section: 0,
                row: 2
            })
        );
    }

    #[test]
    fn no_selection_is_possible_when_every_section_is_empty() {
        let c = counts(0, 0, 0);
        assert_eq!(step_cursor(None, Step::Down, &c), None);
        assert_eq!(step_cursor(None, Step::Up, &c), None);
    }

    #[test]
    fn tab_advances_to_the_next_populated_section_only() {
        let c = counts(2, 0, 3);
        let in_name = Cursor {
            section: 0,
            row: 1,
        };
        assert_eq!(
            next_section_cursor(Some(in_name), true, &c),
            Some(Cursor {
                section: 2,
                row: 0
            })
        );
    }

    #[test]
    fn tab_from_the_last_section_does_not_wrap() {
        let c = counts(2, 0, 3);
        let in_semantic = Cursor {
            section: 2,
            row: 1,
        };
        assert_eq!(
            next_section_cursor(Some(in_semantic), true, &c),
            Some(Cursor {
                section: 2,
                row: 0
            }),
            "tab past the end must stay in the last section, not wrap to the first"
        );
    }

    #[test]
    fn shift_tab_goes_back_one_populated_section() {
        let c = counts(2, 0, 3);
        let in_semantic = Cursor {
            section: 2,
            row: 1,
        };
        assert_eq!(
            next_section_cursor(Some(in_semantic), false, &c),
            Some(Cursor {
                section: 0,
                row: 0
            })
        );
    }

    #[test]
    fn cmd_number_jump_is_refused_for_an_empty_section() {
        let c = counts(2, 0, 3);
        assert_eq!(jump_to_section_cursor(Section::Type, &c), None);
        assert_eq!(
            jump_to_section_cursor(Section::Semantic, &c),
            Some(Cursor {
                section: 2,
                row: 0
            })
        );
    }

    /// §15 acceptance: semantic arriving must not move the selection.
    #[test]
    fn a_section_arriving_leaves_an_existing_cursor_untouched() {
        let cursor = Cursor {
            section: 0,
            row: 2,
        };
        assert_eq!(clamp_cursor(cursor, &counts(4, 0, 0)), Some(cursor));
        assert_eq!(
            clamp_cursor(cursor, &counts(4, 0, 20)),
            Some(cursor),
            "appending a later section must not disturb the cursor"
        );
    }

    #[test]
    fn a_shrinking_section_reseats_the_cursor_to_its_last_row() {
        let cursor = Cursor {
            section: 0,
            row: 9,
        };
        assert_eq!(
            clamp_cursor(cursor, &counts(3, 0, 0)),
            Some(Cursor {
                section: 0,
                row: 2
            })
        );
    }

    #[test]
    fn a_cursor_in_a_now_empty_section_moves_to_the_first_populated_one() {
        let cursor = Cursor {
            section: 1,
            row: 0,
        };
        assert_eq!(
            clamp_cursor(cursor, &counts(2, 0, 0)),
            Some(Cursor {
                section: 0,
                row: 0
            })
        );
    }

    #[test]
    fn qualified_names_split_into_path_and_leaf() {
        let (path, leaf) = split_qualified_name("serde_json::value::Value");
        assert_eq!(path.as_ref(), "serde_json::value::");
        assert_eq!(leaf.as_ref(), "Value");
    }

    #[test]
    fn bare_names_have_no_path() {
        let (path, leaf) = split_qualified_name("Value");
        assert!(path.is_empty());
        assert_eq!(leaf.as_ref(), "Value");
    }

    #[test]
    fn alt_backspace_deletes_one_word_and_its_trailing_space() {
        let mut text = String::from("serde json ");
        truncate_last_word(&mut text);
        assert_eq!(text, "serde ");
        truncate_last_word(&mut text);
        assert_eq!(text, "");
    }

    #[test]
    fn navigation_keys_are_never_treated_as_text() {
        for key in ["up", "down", "tab", "enter", "escape"] {
            assert!(is_navigation_key(key), "{key} must not reach the input");
        }
        assert!(!is_navigation_key("a"));
        assert!(!is_navigation_key("backspace"));
    }

    #[test]
    fn every_section_has_a_distinct_entrance_namespace() {
        let namespaces: Vec<&str> = Section::ALL
            .iter()
            .map(|s| s.entrance_namespace())
            .collect();
        assert_eq!(namespaces[0], "search.row.enter.name");
        assert!(
            namespaces[1] != namespaces[0] && namespaces[2] != namespaces[1],
            "entrance ids pack (gen, ix): a shared namespace would collide"
        );
    }
}
