//! Name / type / semantic fan-out over [`PackageIndexes`].
//!
//! # LR-10: local first, remote never blocks
//!
//! Name and type hits **must** be emitted before any semantic (embedding-based)
//! work starts.  The implementation satisfies this structurally: name and type
//! send their `Section` events before the semantic arm — which is the only one
//! that `await`s a model — is reached at all.
//!
//! The same requirement applies to *building* the index, and is met the same
//! way: packages are embedded by a task fed from `drive_load` over an unbounded
//! channel, so a package is searchable by name and type the instant it becomes
//! resident, however long its vectors take. See [`crate::semantic`].
//!
//! A slow semantic section must never delay or clear a fast name/type section.
//! Each section is independent: the receiver sees three independent
//! `SearchEvent::Section` batches stamped with their respective
//! [`SearchSectionId`]s, and a slow section only adds to what is already
//! visible (never replaces or clears it).
//!
//! # Sections
//!
//! | `SearchSectionId` | Content |
//! |---|---|
//! | `SECTION_NAME` (`0`) | Exact and prefix name matches from [`NameIndex`] |
//! | `SECTION_TYPE` (`1`) | Signature matches (`return:Result`, `param:Path` — see [`crate::typequery`]); falls back to the kind facet when the query names no facet |
//! | `SECTION_SEMANTIC` (`2`) | Nearest neighbours in [`crate::semantic::SemanticIndex`], when the host installed an [`Embedder`](crate::semantic::Embedder) |
//!
//! # Every section says what its rows mean
//!
//! Each section is preceded by a [`SearchEvent::SectionState`] carrying
//! [`SectionState`](crate::semantic::SectionState). This is not decoration: an
//! empty `Section` is a *claim* — "we searched and there is no match" — and for
//! the semantic section that claim is false whenever the index is still
//! building or no model is installed. The local sections report `Complete`,
//! which they are entitled to because they read the resident corpus
//! synchronously.
//!
//! # Hit rows
//!
//! [`HitRow`]s are fully render-ready:
//! * `display_name` — the original (cased) symbol name from
//!   `NameEntry::display`.
//! * `sig_preview` — produced by `chunk::signature::tokens` (LR-4).
//! * `path` is NOT in `HitRow` (it is used only in the search section score and
//!   is not a wire field) — the path is encoded in `display_name` when
//!   disambiguation is needed.
//! * `kind` — `KindTag::from(discriminant)`.
//! * `provenance` — from the owning `PackageView`.
//! * `score` — see [Relevance](#relevance) below.
//!
//! # Relevance
//!
//! `score = text_relevance × visibility_weight × kind_weight`, every factor in
//! `(0, 1]`. A **public, top-level** symbol whose name the query matched
//! exactly therefore scores exactly `1.0`: that case is the reference point the
//! other two factors discount away from, so `score` keeps its documented "an
//! exact match is 1.0" wire meaning. See `visibility_weight` and
//! `kind_weight` for what each factor means and why.
//!
//! # Result order
//!
//! Rows are ordered by a **total** comparator — score descending, then display
//! name, then the symbol's `StableRef` (package lineage, then `IntroId`). The
//! last term is what makes it total: two rows that agree on every *scoring*
//! input still have distinct identities, so their relative order is fixed by
//! the data rather than inherited from whatever order the index walk happened
//! to produce. It has to be, because that walk used to be a `HashMap`
//! iteration, and its per-process seed was reaching the user's screen as a
//! different ranking on every launch.

use nudox_ir::{change::PackageLineageId, kind::KindDiscriminant};

use crate::{
    runtime::{EngineHandle, StreamHandle},
    wire::{Gen, SearchEvent, SearchSectionId},
};

mod hits;

pub(crate) use hits::{
    Candidate, collect_name_hits, collect_type_hits, compare_candidates, finalize_candidates,
    is_member_kind, qualified_display_name, score_of,
};

// ---------------------------------------------------------------------------
// Section ids (Appendix C convention)
// ---------------------------------------------------------------------------

/// Name-match section.
pub const SECTION_NAME: SearchSectionId = SearchSectionId(0);
/// Type/kind-filter section.
pub const SECTION_TYPE: SearchSectionId = SearchSectionId(1);
/// Semantic / embedding section.
///
/// Empty in any build that installs no [`Embedder`](crate::semantic::Embedder),
/// which is every build in this repository today — but empty *and stated*, via
/// [`SectionState::Unavailable`](crate::semantic::SectionState::Unavailable),
/// never empty and silent.
pub const SECTION_SEMANTIC: SearchSectionId = SearchSectionId(2);

// ---------------------------------------------------------------------------
// Channel capacity (Appendix C)
// ---------------------------------------------------------------------------

const SEARCH_CHANNEL_CAP: usize = 64;

// ---------------------------------------------------------------------------
// SearchQuery
// ---------------------------------------------------------------------------

/// The query parameters for a search.
#[derive(Debug, Clone)]
pub struct SearchQuery {
    /// The raw query text typed by the user.
    pub text: String,
    /// If non-empty, restrict results to these kind discriminants.
    ///
    /// This is the **caller naming kinds**, and it does two things at once:
    /// it filters the name section, and it *drives* the kind-facet section
    /// (`collect_type_hits`), which emits every symbol of these kinds whether
    /// or not the query text matches anything. That second behaviour is the
    /// point of `search("trait")`, and it is why this field must not be used
    /// to express a *default* scope — see [`Self::exclude_kinds`].
    pub kinds: Vec<KindDiscriminant>,
    /// Kinds to drop from results the caller did not ask about.
    ///
    /// # Why this is not just `kinds` with the complement filled in
    ///
    /// Populating `kinds` with "everything except members" looks equivalent
    /// and is not: a non-empty `kinds` switches the kind-facet section on, so
    /// every query — including one matching nothing — starts returning every
    /// declaration in every package, at facet relevance. Search stops being a
    /// search. That is a real regression this field exists to avoid, caught by
    /// `tool_integration::search_symbols_nonsense_query_returns_empty`.
    ///
    /// So exclusion is its own axis: it subtracts from whatever the sections
    /// produced, and never causes a section to produce anything.
    pub exclude_kinds: Vec<KindDiscriminant>,
    /// If non-empty, restrict results to these package lineages.
    ///
    /// # Why this has to live here and not at the MCP call site
    ///
    /// `collect_name_hits` and `collect_type_hits` each
    /// `candidates.truncate(limit)` *before* returning, because the whole
    /// point of `limit` is to bound how much work later stages (scoring,
    /// disambiguation, the channel send) do. A caller that filters by
    /// package *after* that truncation is filtering a set that has
    /// already had the very rows it wanted thrown away — ties break on package
    /// lineage (see `compare_candidates`), so a package whose name sorts late
    /// is starved by every tie against a package that sorts earlier, and
    /// the caller can see zero results for a package that genuinely has
    /// matches. Filtering here, before either function's `truncate`, is the
    /// only place that produces the honest answer: a package is excluded
    /// from consideration entirely rather than included and then discarded.
    pub packages: Vec<PackageLineageId>,
    /// Maximum number of hits per section (0 = unlimited, default: 50).
    pub limit: usize,
}

impl Default for SearchQuery {
    fn default() -> Self {
        Self {
            text: String::new(),
            kinds: Vec::new(),
            exclude_kinds: Vec::new(),
            packages: Vec::new(),
            limit: 50,
        }
    }
}

impl SearchQuery {
    /// `true` when no kind filter is applied (all kinds shown).
    fn any_kind(&self) -> bool {
        self.kinds.is_empty()
    }

    /// `true` when `kind` passes both the optional kind filter and the
    /// default-scope exclusion.
    ///
    /// Exclusion is checked first and is unconditional: a caller that named
    /// kinds explicitly leaves `exclude_kinds` empty, so the two can never
    /// disagree about the same kind.
    fn kind_matches(&self, kind: KindDiscriminant) -> bool {
        if self.exclude_kinds.contains(&kind) {
            return false;
        }
        self.any_kind() || self.kinds.contains(&kind)
    }

    /// `true` when no package filter is applied (all loaded packages searched).
    fn any_package(&self) -> bool {
        self.packages.is_empty()
    }

    /// `true` when `lineage` passes the optional package filter.
    fn package_matches(&self, lineage: &PackageLineageId) -> bool {
        self.any_package() || self.packages.contains(lineage)
    }
}

// ---------------------------------------------------------------------------
// EngineHandle::search
// ---------------------------------------------------------------------------

impl EngineHandle {
    /// Fan-out name / type / semantic search over the loaded corpus.
    ///
    /// Returns `(StreamHandle, receiver)`. Dropping the `StreamHandle` cancels
    /// the stream (§2.3). The receiver closes after `SearchEvent::Done`.
    ///
    /// # Ordering guarantee (LR-10)
    ///
    /// `SECTION_NAME` and `SECTION_TYPE` events are always sent before any
    /// semantic work begins. The receiver sees:
    ///
    /// 1. `SectionState { SECTION_NAME, Complete }` and `SectionState {
    ///    SECTION_TYPE, Complete }` — both local sections read the resident
    ///    corpus synchronously, so they are complete before either has run.
    /// 2. `Section { section: SECTION_NAME, rows: [...] }` — local name hits.
    /// 3. `Latency { section: SECTION_NAME, elapsed: ... }` — timing.
    /// 4. `Section { section: SECTION_TYPE, rows: [...] }` — signature or
    ///    kind-facet hits.
    /// 5. `Latency { section: SECTION_TYPE, elapsed: ... }` — timing.
    /// 6. `SectionState { SECTION_SEMANTIC, .. }` — `Complete`, `Building` or
    ///    `Unavailable`. Sent **before** the rows it describes, so a consumer
    ///    that paints on first delivery already knows how to caption them.
    /// 7. `Section { section: SECTION_SEMANTIC, rows: [...] }`.
    /// 8. `Latency { section: SECTION_SEMANTIC, elapsed: ... }` — timing.
    /// 9. `Done { generation }` — terminal.
    ///
    /// Steps 1–5 never await anything but the corpus lock, so the semantic
    /// arm's model call in step 6–7 cannot delay the first frame.
    pub fn search(
        &self,
        query: SearchQuery,
        generation: Gen,
    ) -> (StreamHandle, flume::Receiver<SearchEvent>) {
        let (tx, rx) = flume::bounded(SEARCH_CHANNEL_CAP);
        let (cancel_token, cancel_fn) = Self::make_cancel();
        let handle = StreamHandle::new(generation, cancel_fn);
        let corpus = self.corpus();
        let semantic = self.semantic_index();
        let embedder = self.embedder();

        self.spawn(execute::run_search(
            corpus,
            semantic,
            embedder,
            query,
            generation,
            tx,
            cancel_token,
        ));

        (handle, rx)
    }
}

mod execute;

#[cfg(test)]
#[path = "tests.rs"]
mod tests;
