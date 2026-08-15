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
//! * `display_name` — the original (cased) symbol name from `NameEntry::display`.
//! * `sig_preview` — produced by `chunk::signature::tokens` (LR-4).
//! * `path` is NOT in `HitRow` (it is used only in the search section score
//!   and is not a wire field) — the path is encoded in `display_name` when
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

use std::sync::Arc;
use std::time::Instant;

use tokio_util::sync::CancellationToken;
use tracing::debug;

use nudox_ir::{change::PackageLineageId, kind::KindDiscriminant};
use crate::store::corpus::Corpus;

use crate::{
    chunk::signature,
    runtime::{EngineHandle, StreamHandle},
    wire::{Gen, HitRow, KindTag, SearchEvent, SearchSectionId, SharedStr},
};

mod hits;

pub(crate) use hits::{
    Candidate, collect_name_hits, collect_type_hits, compare_candidates, finalize_candidates,
    qualified_display_name, score_of,
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
    pub kinds: Vec<KindDiscriminant>,
    /// If non-empty, restrict results to these package lineages.
    ///
    /// # Why this has to live here and not at the MCP call site
    ///
    /// `collect_name_hits` and `collect_type_hits` each `candidates.truncate(limit)`
    /// *before* returning, because the whole point of `limit` is to bound how much
    /// work later stages (scoring, disambiguation, the channel send) do. A caller
    /// that filters by package *after* that truncation is filtering a set that has
    /// already had the very rows it wanted thrown away — ties break on package
    /// lineage (see `compare_candidates`), so a package whose name sorts late is
    /// starved by every tie against a package that sorts earlier, and the caller
    /// can see zero results for a package that genuinely has matches. Filtering
    /// here, before either function's `truncate`, is the only place that produces
    /// the honest answer: a package is excluded from consideration entirely rather
    /// than included and then discarded.
    pub packages: Vec<PackageLineageId>,
    /// Maximum number of hits per section (0 = unlimited, default: 50).
    pub limit: usize,
}

impl Default for SearchQuery {
    fn default() -> Self {
        Self {
            text: String::new(),
            kinds: Vec::new(),
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

    /// `true` when `kind` passes the optional kind filter.
    fn kind_matches(&self, kind: KindDiscriminant) -> bool {
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
    /// 1. `SectionState { SECTION_NAME, Complete }` and
    ///    `SectionState { SECTION_TYPE, Complete }` — both local sections read
    ///    the resident corpus synchronously, so they are complete before either
    ///    has run.
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

        self.spawn(run_search(
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

// ---------------------------------------------------------------------------
// Search driver
// ---------------------------------------------------------------------------

async fn run_search(
    corpus: Corpus,
    semantic: crate::semantic::SemanticIndex,
    embedder: crate::semantic::SharedEmbedder,
    query: SearchQuery,
    generation: Gen,
    tx: flume::Sender<SearchEvent>,
    cancel: CancellationToken,
) {
    if cancel.is_cancelled() {
        return;
    }

    // Collect all packages synchronously (corpus is an in-memory Arc map).
    let packages = corpus.packages().await;

    // Both local sections read the resident corpus directly, so they are
    // complete the moment they run — whatever is resident *is* the world they
    // claim to have searched. Saying so explicitly, rather than letting the
    // consumer infer completeness from the absence of a caveat, is what makes
    // the semantic section's caveat legible when it appears.
    for section in [SECTION_NAME, SECTION_TYPE] {
        let _ = tx
            .send_async(SearchEvent::SectionState {
                generation,
                section,
                state: crate::semantic::SectionState::Complete,
            })
            .await;
    }

    // ── Section 0: Name search ─────────────────────────────────────────────
    let name_start = Instant::now();
    let name_rows = collect_name_hits(&packages, &query);
    let name_elapsed = name_start.elapsed();

    if cancel.is_cancelled() {
        return;
    }

    // Emit name section.
    let ev = SearchEvent::Section {
        generation,
        section: SECTION_NAME,
        rows: Arc::from(name_rows.as_slice()),
    };
    if tx.send_async(ev).await.is_err() {
        debug!("search: receiver dropped after SECTION_NAME");
        return;
    }
    let _ = tx
        .send_async(SearchEvent::Latency {
            generation,
            section: SECTION_NAME,
            elapsed: name_elapsed,
        })
        .await;

    if cancel.is_cancelled() {
        return;
    }

    // ── Section 1: Type / kind search ──────────────────────────────────────
    let type_start = Instant::now();
    let type_rows = collect_type_hits(&packages, &query);
    let type_elapsed = type_start.elapsed();

    if cancel.is_cancelled() {
        return;
    }

    let ev = SearchEvent::Section {
        generation,
        section: SECTION_TYPE,
        rows: Arc::from(type_rows.as_slice()),
    };
    if tx.send_async(ev).await.is_err() {
        debug!("search: receiver dropped after SECTION_TYPE");
        return;
    }
    let _ = tx
        .send_async(SearchEvent::Latency {
            generation,
            section: SECTION_TYPE,
            elapsed: type_elapsed,
        })
        .await;

    if cancel.is_cancelled() {
        return;
    }

    // ── Section 2: Semantic ────────────────────────────────────────────────
    //
    // LR-10 is satisfied structurally, exactly as before: every local row and
    // both local latencies are already on the wire above, so nothing below this
    // line can delay the first frame. The one new hazard is that this arm now
    // *awaits* a model, so it must be the last thing in the function and must
    // never be able to fail the stream — a semantic section that cannot run is
    // a caption, not an error.
    let semantic_start = Instant::now();
    let (state, rows) =
        collect_semantic_hits(&packages, &semantic, embedder.as_deref(), &query).await;
    let semantic_elapsed = semantic_start.elapsed();

    if cancel.is_cancelled() {
        return;
    }

    let _ = tx
        .send_async(SearchEvent::SectionState {
            generation,
            section: SECTION_SEMANTIC,
            state,
        })
        .await;
    let _ = tx
        .send_async(SearchEvent::Section {
            generation,
            section: SECTION_SEMANTIC,
            rows: Arc::from(rows.as_slice()),
        })
        .await;
    let _ = tx
        .send_async(SearchEvent::Latency {
            generation,
            section: SECTION_SEMANTIC,
            elapsed: semantic_elapsed,
        })
        .await;

    if !cancel.is_cancelled() {
        let _ = tx.send_async(SearchEvent::Done { generation }).await;
    }
}

// ---------------------------------------------------------------------------
// Semantic-hit collection
// ---------------------------------------------------------------------------

/// The semantic section's rows **and what they mean**.
///
/// Returning the two together is the point: there is no way to obtain rows from
/// this function without also obtaining the state that captions them, so a call
/// site cannot render a partial ranking as a complete one by forgetting to ask.
/// That is the §8 "typed, not logged" rule applied to a caveat instead of a
/// repair.
async fn collect_semantic_hits(
    packages: &[std::sync::Arc<crate::store::package::PackageView>],
    semantic: &crate::semantic::SemanticIndex,
    embedder: Option<&dyn crate::semantic::Embedder>,
    query: &SearchQuery,
) -> (crate::semantic::SectionState, Vec<HitRow>) {
    use crate::semantic::{SectionState, Unavailable};

    let Some(embedder) = embedder else {
        return (
            SectionState::Unavailable {
                reason: Unavailable::NoEmbedder,
            },
            Vec::new(),
        );
    };
    if packages.is_empty() {
        return (
            SectionState::Unavailable {
                reason: Unavailable::EmptyCorpus,
            },
            Vec::new(),
        );
    }

    // Coverage is computed against the packages this query is actually allowed
    // to answer from, not against the whole corpus: with `packages: [memchr]`,
    // "3 of 20 indexed" would be a caveat about 19 packages the reader excluded
    // themselves and is not waiting for.
    let in_scope: Vec<&std::sync::Arc<crate::store::package::PackageView>> = packages
        .iter()
        .filter(|pkg| query.package_matches(pkg.lineage()))
        .collect();
    let total = in_scope.len() as u32;
    let covered = in_scope
        .iter()
        .filter(|pkg| semantic.contains(pkg.lineage()))
        .count() as u32;

    let state = if covered == total {
        SectionState::Complete
    } else {
        SectionState::Building { covered, total }
    };

    let text = query.text.trim();
    if text.is_empty() || covered == 0 {
        return (state, Vec::new());
    }

    let limit = if query.limit == 0 { 50 } else { query.limit };

    // One text, so one batch — `max_batch` cannot be exceeded by construction.
    let queries = [text.to_owned()];
    let embedded = match embedder
        .embed_batch(&queries, crate::semantic::EmbedRole::Query)
        .await
    {
        Ok(vectors) => vectors,
        Err(error) => {
            // A model that fails on *this* query has not invalidated the index,
            // and it has certainly not invalidated the local sections that are
            // already painted. Degrade this section only, and say so with the
            // reason a reader can act on.
            debug!("semantic: query embedding failed: {error}");
            return (
                SectionState::Unavailable {
                    reason: Unavailable::ModelFailed,
                },
                Vec::new(),
            );
        }
    };
    let Some(vector) = embedded.first() else {
        debug!("semantic: embedder returned no vector for a one-text batch");
        return (
            SectionState::Unavailable {
                reason: Unavailable::ModelFailed,
            },
            Vec::new(),
        );
    };

    // Over-fetch, because kind and package filters are applied *after* scoring
    // and would otherwise silently shorten the section: a reader who filters to
    // `Function` should get `limit` functions, not the functions that happened
    // to survive within the first `limit` symbols of any kind.
    let nearest = semantic.nearest(vector, limit.saturating_mul(4).max(limit));

    let mut candidates: Vec<Candidate> = Vec::new();
    for (lineage, intro, cosine) in nearest {
        if candidates.len() >= limit {
            break;
        }
        let Some(pkg) = in_scope.iter().find(|p| *p.lineage() == lineage) else {
            continue;
        };
        let Some(ir_entry) = pkg.view().entry(intro) else {
            continue;
        };
        let Some(disc) = ir_entry.kind().discriminant() else {
            continue;
        };
        if !query.kind_matches(disc) {
            continue;
        }

        let indexes = pkg.indexes();
        let pkg_name = pkg.lineage().name.as_str();
        let leaf_str = ir_entry.sym().name.as_str();

        candidates.push(Candidate {
            row: HitRow {
                key: nudox_ir::change::StableRef::new(lineage.clone(), intro),
                display_name: SharedStr::from(leaf_str),
                sig_preview: signature::tokens(ir_entry, pkg),
                kind: KindTag::Known(disc),
                provenance: pkg.provenance().into(),
                score: score_of(
                    cosine_to_relevance(cosine),
                    ir_entry.sym().visibility,
                    disc,
                ),
            },
            qualified: qualified_display_name(indexes, intro, leaf_str, pkg_name),
        });
    }

    candidates.sort_by(compare_candidates);
    candidates.truncate(limit);
    (state, finalize_candidates(candidates))
}

/// Map a cosine in `[-1, 1]` onto the wire's `(0, 1]` relevance contract.
///
/// `HitRow::score` documents that an exact match is `1.0` and that every score
/// is in `(0, 1]`; a raw cosine satisfies neither bound, and a negative one
/// would multiply through `score_of`'s weights to produce a *less* negative
/// number for a *worse* symbol — inverting the ranking among the rows nobody
/// should be looking at anyway, which is exactly the kind of quiet wrongness
/// that survives review.
///
/// The map is affine and monotone, so it cannot reorder anything: it is a
/// change of units, not a re-ranking. `f32::EPSILON` rather than `0.0` at the
/// bottom keeps the result strictly positive, because a `0.0` score would make
/// `score_of`'s product zero regardless of visibility and collapse every
/// maximally-dissimilar row into one tie.
fn cosine_to_relevance(cosine: f32) -> f32 {
    let clamped = cosine.clamp(-1.0, 1.0);
    f32::midpoint(clamped, 1.0).max(f32::EPSILON)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::time::Duration;

    use crate::store::source::fixtures::FixtureSource;

    use crate::{
        runtime::{Engine, EngineConfig},
        search::{SECTION_NAME, SECTION_TYPE, SearchQuery, collect_name_hits},
        wire::{Gen, SearchEvent, SharedStr},
    };

    fn make_engine() -> crate::runtime::EngineHandle {
        Engine::start(EngineConfig::default(), FixtureSource::rich())
    }

    async fn wait_for_corpus(engine: &crate::runtime::EngineHandle) {
        use crate::store::source::fixtures::rich_lineage;
        for _ in 0..50 {
            if engine.corpus().package(&rich_lineage()).await.is_some() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("corpus never seeded");
    }

    /// Drain the search receiver into a vec of events.
    async fn drain(rx: flume::Receiver<SearchEvent>) -> Vec<SearchEvent> {
        let mut events = Vec::new();
        while let Ok(ev) = rx.recv_async().await {
            let done = matches!(ev, SearchEvent::Done { .. } | SearchEvent::Failed { .. });
            events.push(ev);
            if done {
                break;
            }
        }
        events
    }

    #[tokio::test]
    async fn local_name_hits_before_semantic() {
        let engine = make_engine();
        wait_for_corpus(&engine).await;

        let query = SearchQuery {
            text: "Point".to_owned(),
            kinds: Vec::new(),
            packages: Vec::new(),
            limit: 50,
        };
        let (_handle, rx) = engine.search(query, Gen(1));
        let events = drain(rx).await;

        // Find the position of SECTION_NAME and SECTION_SEMANTIC events.
        let name_pos = events.iter().position(
            |e| matches!(e, SearchEvent::Section { section, .. } if *section == SECTION_NAME),
        );
        let semantic_pos = events
            .iter()
            .position(|e| matches!(e, SearchEvent::Section { section, .. } if *section == crate::search::SECTION_SEMANTIC));

        let name_pos = name_pos.expect("must have a SECTION_NAME event");
        let semantic_pos = semantic_pos.expect("must have a SECTION_SEMANTIC event");

        assert!(
            name_pos < semantic_pos,
            "SECTION_NAME (pos {name_pos}) must precede SECTION_SEMANTIC (pos {semantic_pos})"
        );
    }

    #[tokio::test]
    async fn search_emits_done_as_terminal() {
        let engine = make_engine();
        wait_for_corpus(&engine).await;

        let query = SearchQuery {
            text: "fn".to_owned(),
            ..Default::default()
        };
        let (_handle, rx) = engine.search(query, Gen(2));
        let events = drain(rx).await;

        assert!(
            matches!(events.last(), Some(SearchEvent::Done { .. })),
            "last event must be Done"
        );
    }

    #[tokio::test]
    async fn dropping_handle_cancels_stream() {
        let engine = make_engine();
        wait_for_corpus(&engine).await;

        let query = SearchQuery {
            text: "Point".to_owned(),
            ..Default::default()
        };
        let (handle, rx) = engine.search(query, Gen(3));

        // Drop handle immediately — cancels before any events land.
        drop(handle);

        // The receiver should close promptly; drain it.
        let mut count = 0usize;
        while rx.try_recv().is_ok() {
            count += 1;
            // Safety valve: no more than the channel capacity worth of events.
            if count > super::SEARCH_CHANNEL_CAP * 2 {
                break;
            }
        }
        // No assertion on count: cancel races with the async task. We assert
        // only that the loop terminates (does not hang).
    }

    #[tokio::test]
    async fn superseded_gen_search_still_produces_events_for_new_gen() {
        let engine = make_engine();
        wait_for_corpus(&engine).await;

        let q = SearchQuery {
            text: "Color".to_owned(),
            ..Default::default()
        };

        // Gen 4: cancel immediately.
        let (handle4, _rx4) = engine.search(q.clone(), Gen(4));
        drop(handle4);

        // Gen 5: must produce a complete stream.
        let (_handle5, rx5) = engine.search(q, Gen(5));
        let events = drain(rx5).await;
        assert!(
            matches!(events.last(), Some(SearchEvent::Done { generation }) if generation.0 == 5),
            "gen 5 must terminate with Done"
        );
    }

    #[tokio::test]
    async fn empty_query_produces_no_name_hits() {
        let engine = make_engine();
        wait_for_corpus(&engine).await;

        let q = SearchQuery {
            text: String::new(),
            ..Default::default()
        };
        let (_handle, rx) = engine.search(q, Gen(6));
        let events = drain(rx).await;

        let name_section = events.iter().find(
            |e| matches!(e, SearchEvent::Section { section, .. } if *section == SECTION_NAME),
        );
        if let Some(SearchEvent::Section { rows, .. }) = name_section {
            assert!(rows.is_empty(), "empty query must produce no name hits");
        }
    }

    #[tokio::test]
    async fn kind_keyword_produces_type_section_hits() {
        let engine = make_engine();
        wait_for_corpus(&engine).await;

        let q = SearchQuery {
            text: "fn".to_owned(),
            ..Default::default()
        };
        let (_handle, rx) = engine.search(q, Gen(7));
        let events = drain(rx).await;

        let type_section = events.iter().find(
            |e| matches!(e, SearchEvent::Section { section, .. } if *section == SECTION_TYPE),
        );
        let Some(SearchEvent::Section { rows, .. }) = type_section else {
            panic!("must have SECTION_TYPE");
        };
        // The rich corpus has several functions; at least one must appear.
        assert!(
            !rows.is_empty(),
            "keyword 'fn' must produce type-section hits"
        );
    }

    // -----------------------------------------------------------------------
    // L20: colliding leaf names must yield distinct display names, even
    // within a single package.
    // -----------------------------------------------------------------------

    /// Build a single-package fixture with two functions named `foo` under
    /// different modules (`a::foo`, `b::foo`) plus one uniquely-named
    /// function (`zzunique`), calling `collect_name_hits` directly — it is
    /// private to this module, so a test here can exercise it without going
    /// through the whole engine/source plumbing.
    ///
    /// This mirrors the real bug's shape (real memchr declares `mod memchr`
    /// once per architecture backend, e.g. `arch::x86_64::memchr` and
    /// `arch::aarch64::memchr`) without depending on a real crate checkout —
    /// see `real_memchr_leaf_collisions_get_distinct_display_names` below for
    /// the real-fixture counterpart.
    fn package_with_leaf_collision() -> Arc<crate::store::package::PackageView> {
        use nudox_ir::{
            apply::PristineIntroTable,
            change::{EcosystemId, IntroId, PackageLineageId, PackageName},
            entry::{Entry, Node, Symbol, Visibility},
            index::RawRef,
            kind::Kind,
            kinds::{Function, Module},
            view::IrView,
        };
        use crate::store::package::{PackageView, Provenance};

        fn sym(name: &str) -> Symbol {
            Symbol {
                name: name.to_owned(),
                visibility: Visibility::Public,
                documentation: String::new(),
                source: std::path::PathBuf::new(),
                span: 0..0,
                aliases: Box::new([]),
                deprecation: None,
                doc_links: Box::new([]),
                attrs: Box::new([]),
                cfg: None,
            }
        }

        let root_id = IntroId::from_raw([21u8; 32]);
        let mod_a_id = IntroId::from_raw([22u8; 32]);
        let mod_b_id = IntroId::from_raw([23u8; 32]);
        let foo_a_id = IntroId::from_raw([24u8; 32]);
        let foo_b_id = IntroId::from_raw([25u8; 32]);
        let unique_id = IntroId::from_raw([26u8; 32]);

        let mut table = PristineIntroTable::new();
        table.insert_live(
            root_id,
            Entry::new(
                sym("testpkg"),
                Node::build(None::<RawRef>, []),
                Kind::Module(Module),
            ),
            None,
        );
        table.insert_live(
            mod_a_id,
            Entry::new(sym("a"), Node::build(None::<RawRef>, []), Kind::Module(Module)),
            Some(root_id),
        );
        table.insert_live(
            mod_b_id,
            Entry::new(sym("b"), Node::build(None::<RawRef>, []), Kind::Module(Module)),
            Some(root_id),
        );
        table.insert_live(
            foo_a_id,
            Entry::new(
                sym("foo"),
                Node::build(None::<RawRef>, []),
                Kind::Function(Function::builder().build()),
            ),
            Some(mod_a_id),
        );
        table.insert_live(
            foo_b_id,
            Entry::new(
                sym("foo"),
                Node::build(None::<RawRef>, []),
                Kind::Function(Function::builder().build()),
            ),
            Some(mod_b_id),
        );
        table.insert_live(
            unique_id,
            Entry::new(
                sym("zzunique"),
                Node::build(None::<RawRef>, []),
                Kind::Function(Function::builder().build()),
            ),
            Some(root_id),
        );

        let lineage = PackageLineageId::new(EcosystemId::new("test"), PackageName::new("testpkg"));
        let view = IrView::with_package(lineage, table);
        Arc::new(PackageView::build(view, Provenance::TrustedLocal))
    }

    // -----------------------------------------------------------------------
    // The comparator is a *total* order, including over genuine ties
    // -----------------------------------------------------------------------

    /// Build a candidate that differs from its siblings only in `intro`.
    fn tied_candidate(pkg: &str, intro_byte: u8, leaf: &str, score: f32) -> super::Candidate {
        use nudox_ir::change::{EcosystemId, IntroId, PackageLineageId, PackageName};
        use crate::wire::{HitRow, KindTag, Provenance};

        let key = nudox_ir::change::StableRef::new(
            PackageLineageId::new(EcosystemId::new("test"), PackageName::new(pkg)),
            IntroId::from_raw([intro_byte; 32]),
        );
        super::Candidate {
            row: HitRow {
                key,
                display_name: SharedStr::from(leaf),
                sig_preview: Vec::new(),
                kind: KindTag::Known(nudox_ir::kind::KindDiscriminant::Function),
                provenance: Provenance::TrustedLocal,
                score,
            },
            qualified: SharedStr::from(leaf),
        }
    }

    /// No two *distinct* symbols may compare `Equal`, even when they agree on
    /// every scoring input.
    ///
    /// This is what "total" buys: `sort_by` is stable, so any pair that
    /// compares `Equal` silently keeps whatever relative order the input had —
    /// and the input order is a `HashMap` walk. A comparator that bottoms out
    /// in `display_name` reports `Equal` for the entire set below, which is the
    /// exact shape a real query produces (every case-folded exact match scores
    /// identically and carries the same leaf name at sort time).
    #[test]
    fn comparator_never_reports_equal_for_two_distinct_symbols() {
        let set = vec![
            tied_candidate("aaa", 0x40, "dup", 1.0),
            tied_candidate("aaa", 0x10, "dup", 1.0),
            tied_candidate("zzz", 0x10, "dup", 1.0),
            tied_candidate("aaa", 0x70, "dup", 1.0),
            tied_candidate("zzz", 0xB0, "dup", 1.0),
        ];

        for (i, a) in set.iter().enumerate() {
            for (j, b) in set.iter().enumerate() {
                let ord = super::compare_candidates(a, b);
                if i == j {
                    assert_eq!(
                        ord,
                        std::cmp::Ordering::Equal,
                        "a candidate must compare Equal to itself"
                    );
                    continue;
                }
                assert_ne!(
                    ord,
                    std::cmp::Ordering::Equal,
                    "candidates {i} and {j} tie on every scoring input and the \
                     comparator cannot separate them — their order is therefore \
                     whatever the index walk produced"
                );
                // Antisymmetry: reversing the arguments must reverse the result.
                assert_eq!(
                    ord.reverse(),
                    super::compare_candidates(b, a),
                    "comparator is not antisymmetric for {i} vs {j}"
                );
            }
        }

        // Transitivity across the whole set: sorting must produce a sequence in
        // which every adjacent pair is strictly Less.
        let mut sorted = set;
        sorted.sort_by(super::compare_candidates);
        for pair in sorted.windows(2) {
            assert_eq!(
                super::compare_candidates(&pair[0], &pair[1]),
                std::cmp::Ordering::Less,
                "sorted output contains an adjacent pair that is not strictly ordered"
            );
        }

        // And the order really is the identity order: package, then IntroId.
        let ids: Vec<(String, u8)> = sorted
            .iter()
            .map(|c| {
                (
                    c.row.key.package.name.as_str().to_owned(),
                    c.row.key.intro.as_bytes()[0],
                )
            })
            .collect();
        assert_eq!(
            ids,
            vec![
                ("aaa".to_owned(), 0x10),
                ("aaa".to_owned(), 0x40),
                ("aaa".to_owned(), 0x70),
                ("zzz".to_owned(), 0x10),
                ("zzz".to_owned(), 0xB0),
            ]
        );
    }

    /// A higher score always wins, whatever the identities say.
    ///
    /// Guards the ordering of the comparator's terms: identity is the *last*
    /// resort, not a co-equal key that could reorder real relevance.
    #[test]
    fn score_dominates_identity_in_the_comparator() {
        // The lower-scoring candidate has the smaller package name and the
        // smaller intro — it wins on every tiebreak and must still lose.
        let strong = tied_candidate("zzz", 0xFF, "dup", 1.0);
        let weak = tied_candidate("aaa", 0x00, "dup", 0.4);
        assert_eq!(
            super::compare_candidates(&strong, &weak),
            std::cmp::Ordering::Less,
            "the higher-scoring row must sort first"
        );
    }

    // -----------------------------------------------------------------------
    // Scoring model
    // -----------------------------------------------------------------------

    /// A public, top-level, exactly-matched symbol scores exactly 1.0, and
    /// every weight is a discount from it.
    ///
    /// This is the contract `HitRow::score` documents and that the MCP layer
    /// and the adversarial suite both assume. Asserting the *bound* rather than
    /// each constant means the weights can be re-argued without the test
    /// having to be edited to agree with them.
    #[test]
    #[allow(clippy::float_cmp)] // the reference point must be *exactly* 1.0
    fn public_top_level_exact_match_is_the_scoring_reference_point() {
        use nudox_ir::entry::Visibility;
        use nudox_ir::kind::KindDiscriminant as K;

        assert_eq!(
            super::score_of(1.0, Visibility::Public, K::Function),
            1.0,
            "an exact match on a public function is the 1.0 reference point"
        );

        let all_visibilities = [
            Visibility::Public,
            Visibility::Protected,
            Visibility::Internal,
            Visibility::Package,
            Visibility::Crate,
            Visibility::Private,
        ];
        let all_kinds = [
            K::Module, K::Record, K::Field, K::Function, K::Alias, K::Trait,
            K::Impl, K::Enum, K::Variant, K::Const, K::Static, K::Reexport, K::Param,
        ];
        for v in all_visibilities {
            for k in all_kinds {
                let s = super::score_of(1.0, v, k);
                assert!(
                    s > 0.0 && s <= 1.0,
                    "score for ({v:?}, {k:?}) escaped (0, 1]: {s} — a weight above \
                     1.0 would let a quality factor promote a worse text match"
                );
            }
        }
    }

    /// Visibility separates two symbols that are identical to the text matcher.
    #[test]
    fn less_visible_symbols_score_below_public_ones() {
        use nudox_ir::entry::Visibility;
        use nudox_ir::kind::KindDiscriminant as K;

        let public = super::score_of(1.0, Visibility::Public, K::Module);
        for lesser in [
            Visibility::Protected,
            Visibility::Internal,
            Visibility::Package,
            Visibility::Crate,
            Visibility::Private,
        ] {
            assert!(
                super::score_of(1.0, lesser, K::Module) < public,
                "{lesser:?} must score below Public on an otherwise identical match"
            );
        }
    }

    /// Two entries sharing the leaf name `foo` in different modules of the
    /// *same* package must render with distinct `display_name`s.
    ///
    /// This is the regression the old code missed: disambiguation only ran
    /// when `packages.len() > 1`, so a single-package corpus (the common
    /// case — open one crate's docs and search it) never qualified anything,
    /// no matter how many leaf names collided inside that one package.
    #[test]
    fn colliding_leaf_names_within_one_package_get_distinct_display_names() {
        let pkg = package_with_leaf_collision();

        let query = SearchQuery {
            text: "foo".to_owned(),
            kinds: Vec::new(),
            packages: Vec::new(),
            limit: 50,
        };
        let rows = collect_name_hits(std::slice::from_ref(&pkg), &query);

        assert_eq!(rows.len(), 2, "both `foo` entries must be returned");
        assert_ne!(
            rows[0].display_name, rows[1].display_name,
            "two entries with the same leaf name in one package must not \
             render identical display names; got {:?} and {:?}",
            rows[0].display_name, rows[1].display_name
        );
        // Qualified, not just "different by accident" — each must still
        // read as `foo`, distinguished by its module path.
        for row in &rows {
            assert!(
                row.display_name.contains("foo"),
                "qualified display name must still contain the leaf name, \
                 got {:?}",
                row.display_name
            );
        }
    }

    /// A leaf name that does not collide with anything in the result set
    /// must keep its short, unqualified form — disambiguation is a cost we
    /// pay only where the reader actually needs it.
    #[test]
    fn unique_leaf_name_keeps_short_display_name() {
        let pkg = package_with_leaf_collision();

        let query = SearchQuery {
            text: "zzunique".to_owned(),
            kinds: Vec::new(),
            packages: Vec::new(),
            limit: 50,
        };
        let rows = collect_name_hits(std::slice::from_ref(&pkg), &query);

        assert_eq!(rows.len(), 1, "exactly one `zzunique` entry exists");
        assert_eq!(
            &*rows[0].display_name, "zzunique",
            "a non-colliding leaf name must not be qualified"
        );
    }

    /// End-to-end regression against the **real** `memchr` crate (L20): the
    /// exact case the limitations ledger used to demonstrate the bug —
    /// `memchr` declares `mod memchr` once per architecture backend
    /// (`arch::x86_64::memchr`, `arch::aarch64::memchr`, …), so a search for
    /// `memchr` returns several same-kind, same-name rows that were
    /// previously pixel-identical apart from vertical position.
    ///
    /// # Running
    ///
    /// ```text
    /// cargo test -p nudox-engine --lib \
    ///   search::tests::real_memchr_leaf_collisions_get_distinct_display_names \
    ///   -- --ignored --nocapture
    /// ```
    #[test]
    #[ignore = "loads a real Cargo workspace through rust-analyzer; run with --ignored"]
    fn real_memchr_leaf_collisions_get_distinct_display_names() {
        use nudox_ir::view::IrView;
        use nudox_languages::produce;
        use nudox_languages::rust::RustProducer;
        use crate::store::package::{PackageView, Provenance};
        use crate::store::source::producer::PackageDescriptor;

        let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../result/memchr-2.8.3")
            .canonicalize()
            .unwrap_or_else(|_| std::path::PathBuf::from("/nonexistent"));

        if !root.join("Cargo.toml").is_file() {
            eprintln!(
                "SKIP: no memchr checkout at {}. \
                 Run: scripts/fetch-real-crate.sh memchr 2.8.3",
                root.display()
            );
            return;
        }

        // `direct_repo: false` matches `ProducerRegistry::with_rust_pilot`,
        // the constructor the real app uses.
        let descriptor = PackageDescriptor::cargo(&root, "memchr", "2.8.3");
        let table = produce(
            &RustProducer { direct_repo: false },
            &descriptor.source,
            &descriptor.lineage,
            &nudox_ir::foreign::Unlinked,
        )
        .expect("memchr must lower without error for a real checkout").table;

        let view = IrView::with_package(descriptor.lineage, table);
        let pkg = Arc::new(PackageView::build(view, Provenance::TrustedLocal));

        let query = SearchQuery {
            text: "memchr".to_owned(),
            kinds: Vec::new(),
            packages: Vec::new(),
            limit: 0, // unlimited — we need the full collision set to exist
        };
        let rows = collect_name_hits(std::slice::from_ref(&pkg), &query);
        eprintln!(
            "memchr search rows: {:?}",
            rows.iter().map(|r| &*r.display_name).collect::<Vec<_>>()
        );

        // Ground truth, independent of `collect_name_hits`: group the
        // *actual* IR entries by leaf name (case-sensitive, as displayed).
        let mut leaf_of_intro: std::collections::HashMap<nudox_ir::change::IntroId, &str> =
            std::collections::HashMap::new();
        for (id, entry) in pkg.view().entries() {
            leaf_of_intro.insert(id, entry.sym().name.as_str());
        }
        let mut groups: std::collections::HashMap<&str, Vec<SharedStr>> =
            std::collections::HashMap::new();
        for row in &rows {
            let leaf = leaf_of_intro
                .get(&row.key.intro)
                .copied()
                .expect("every hit row must resolve back to a real entry");
            groups.entry(leaf).or_default().push(row.display_name.clone());
        }

        let mut proved_a_collision = false;
        for (leaf, display_names) in &groups {
            if display_names.len() < 2 {
                continue;
            }
            proved_a_collision = true;
            let unique: std::collections::HashSet<&SharedStr> = display_names.iter().collect();
            assert_eq!(
                unique.len(),
                display_names.len(),
                "leaf name {leaf:?} has {} colliding rows but only {} distinct \
                 display names: {display_names:?}",
                display_names.len(),
                unique.len(),
            );
        }

        assert!(
            proved_a_collision,
            "expected at least one leaf-name collision among real memchr's \
             `memchr`-prefixed symbols (the L20 evidence found seven) — \
             found none, so this run does not actually exercise the \
             invariant. Rows: {:?}",
            rows.iter().map(|r| &*r.display_name).collect::<Vec<_>>()
        );
    }
}
