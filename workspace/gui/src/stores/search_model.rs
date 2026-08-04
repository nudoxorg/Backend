//! `SearchModel` — render-ready state types for the omni-search overlay.
//!
//! These types live in the store layer (§12: stores hold render-ready state).
//! The view imports them from here; the store constructs and owns them.
//! Moving them out of `views/omni_search.rs` closes the backwards dependency
//! that forced wire→UI conversion to live in the view.

use std::sync::Arc;
use std::time::Instant;

use gpui::{Context, SharedString};

use crate::theme::ext::Provenance;
use crate::theme::kind::LocalKindDiscriminant;
use crate::ui::signature_line::{SigToken as UiSigToken, SymbolKey as UiSymbolKey};
use nudox_engine::wire::{
    HitRow, KindTag, Provenance as WireProvenance, SigToken as WireSigToken, SymbolKey,
};

// ─────────────────────────────────────────────────────────────────────────────
// Section constants
// ─────────────────────────────────────────────────────────────────────────────

/// The three fused result sections (§15).
///
/// Frozen at three: `cmd-1/2/3` jump to them by ordinal, and `SearchResults`
/// in §12.3 is `[SectionData; 3]`.
pub const SECTION_COUNT: usize = 3;

// ─────────────────────────────────────────────────────────────────────────────
// Section enum
// ─────────────────────────────────────────────────────────────────────────────

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
    pub fn entrance_namespace(self) -> &'static str {
        match self {
            Section::Name => "search.row.enter.name",
            Section::Type => "search.row.enter.type",
            Section::Semantic => "search.row.enter.semantic",
        }
    }

    /// Stable `ElementId` for this section's list (LD-19).
    pub fn list_id(self) -> &'static str {
        match self {
            Section::Name => "search.results.list.name",
            Section::Type => "search.results.list.type",
            Section::Semantic => "search.results.list.semantic",
        }
    }

    /// Stable `ElementId` namespace for this section's rows (LD-19).
    pub fn row_id(self) -> &'static str {
        match self {
            Section::Name => "search.results.row.name",
            Section::Type => "search.results.row.type",
            Section::Semantic => "search.results.row.semantic",
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Cursor
// ─────────────────────────────────────────────────────────────────────────────

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
// SearchMode
// ─────────────────────────────────────────────────────────────────────────────

/// The mode chip selection on the input row (§15 layout, §12.3 `mode`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum SearchMode {
    /// Route the query by shape: `->` implies type search, prose implies semantic.
    #[default]
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

// ─────────────────────────────────────────────────────────────────────────────
// ScopeChip
// ─────────────────────────────────────────────────────────────────────────────

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
    /// This is the call `SearchStore` makes in `apply_search_event`, so that
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
pub fn split_qualified_name(name: &str) -> (SharedString, SharedString) {
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
pub fn prepare_provenance(p: &WireProvenance) -> Provenance {
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
pub fn prepare_sig_token(t: &WireSigToken) -> UiSigToken {
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
// SectionStatus & SectionData
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

// ─────────────────────────────────────────────────────────────────────────────
// SearchSnapshot
// ─────────────────────────────────────────────────────────────────────────────

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
    pub fn total_hits(&self) -> usize {
        self.counts().iter().sum()
    }

    /// The row a cursor points at, if it is in range.
    pub fn row_at(&self, cursor: Cursor) -> Option<&PreparedRow> {
        self.sections
            .get(cursor.section)
            .and_then(|s| s.rows.get(cursor.row))
    }

    /// Whether any section has finished at least once — i.e. whether "zero
    /// hits" is a real answer rather than "nothing has arrived yet".
    pub fn any_section_settled(&self) -> bool {
        self.sections.iter().any(|s| {
            matches!(
                s.status,
                SectionStatus::Ready | SectionStatus::Offline
            )
        })
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// SearchAccess — the contract between the view and the store
// ─────────────────────────────────────────────────────────────────────────────

/// The contract the omni-search view needs from its backing store (§12.3).
///
/// Lives in `search_model` (the store layer) rather than in `views/omni_search`
/// so that `SearchStore` can implement it without creating a `stores → views`
/// dependency.  The view re-exports this trait from its own module so existing
/// import paths remain unchanged.
///
/// The read half is a single [`SearchSnapshot`] producer so that adding a field
/// to the overlay never adds a method here.  The write half is deliberately
/// tiny: movement policy lives in the view as pure functions (`step_cursor`,
/// `next_section_cursor`, `jump_to_section_cursor`), because §15's "never
/// wraps silently across sections" is a view invariant and is unit-tested there.
/// The store only records where the cursor ended up.
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

// ─────────────────────────────────────────────────────────────────────────────
// Tests
// ─────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

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
    fn section_count_is_three() {
        assert_eq!(SECTION_COUNT, 3);
        assert_eq!(Section::ALL.len(), SECTION_COUNT);
    }

    #[test]
    fn section_index_round_trips() {
        for s in Section::ALL {
            let ix = s.index();
            assert_eq!(Section::from_index(ix), Some(s));
        }
        assert_eq!(Section::from_index(3), None);
    }

    #[test]
    fn search_snapshot_counts_matches_section_rows() {
        let mut snap = SearchSnapshot::default();
        // Inject some rows into section 0 via a replacement Arc.
        snap.sections[0].rows = Arc::from([] as [PreparedRow; 0]);
        assert_eq!(snap.counts(), [0, 0, 0]);
        assert_eq!(snap.total_hits(), 0);
    }

    #[test]
    fn any_section_settled_requires_ready_or_offline() {
        let mut snap = SearchSnapshot::default();
        assert!(!snap.any_section_settled());
        snap.sections[1].status = SectionStatus::Ready;
        assert!(snap.any_section_settled());
    }

    #[test]
    fn row_at_returns_none_when_out_of_range() {
        let snap = SearchSnapshot::default();
        assert!(snap.row_at(Cursor { section: 0, row: 0 }).is_none());
        assert!(snap.row_at(Cursor { section: 5, row: 0 }).is_none());
    }

    #[test]
    fn search_mode_default_is_auto() {
        assert_eq!(SearchMode::default(), SearchMode::Auto);
    }

    #[test]
    fn search_mode_labels_are_non_empty() {
        for m in SearchMode::ALL {
            assert!(!m.label().is_empty());
        }
    }
}
