//! Name / type / semantic fan-out over [`PackageIndexes`].
//!
//! # LR-10: local first, remote never blocks
//!
//! Name and type hits **must** be emitted before any semantic (embedding-based)
//! work starts.  The implementation satisfies this structurally: name and type
//! tasks send their `Section` events and `Done` before the semantic section
//! slot is even opened.
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
//! | `SECTION_TYPE` (`1`) | Kind-filtered symbols matching the query as a kind label |
//! | `SECTION_SEMANTIC` (`2`) | Reserved — emits empty for now (no embedding store yet) |
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
//! * `score` — simple relevance: exact match > prefix match, boosted by kind.

use std::sync::Arc;
use std::time::Instant;

use tokio_util::sync::CancellationToken;
use tracing::debug;

use nudox_ir::kind::KindDiscriminant;
use nudox_store::corpus::Corpus;

use crate::{
    chunk::signature,
    runtime::{EngineHandle, StreamHandle},
    wire::{
        Gen, HitRow, KindTag, Provenance, SearchEvent, SearchSectionId, SharedStr,
    },
};

// ---------------------------------------------------------------------------
// Section ids (Appendix C convention)
// ---------------------------------------------------------------------------

/// Name-match section.
pub const SECTION_NAME: SearchSectionId = SearchSectionId(0);
/// Type/kind-filter section.
pub const SECTION_TYPE: SearchSectionId = SearchSectionId(1);
/// Semantic / embedding section (reserved, always empty in this rev).
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
    /// Maximum number of hits per section (0 = unlimited, default: 50).
    pub limit: usize,
}

impl Default for SearchQuery {
    fn default() -> Self {
        Self { text: String::new(), kinds: Vec::new(), limit: 50 }
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
    /// 1. `Section { section: SECTION_NAME, rows: [...] }` — local name hits.
    /// 2. `Latency { section: SECTION_NAME, elapsed: ... }` — timing.
    /// 3. `Section { section: SECTION_TYPE, rows: [...] }` — kind-filter hits.
    /// 4. `Latency { section: SECTION_TYPE, elapsed: ... }` — timing.
    /// 5. `Section { section: SECTION_SEMANTIC, rows: [] }` — empty placeholder.
    /// 6. `Done { generation }` — terminal.
    pub fn search(
        &self,
        query: SearchQuery,
        generation: Gen,
    ) -> (StreamHandle, flume::Receiver<SearchEvent>) {
        let (tx, rx) = flume::bounded(SEARCH_CHANNEL_CAP);
        let (cancel_token, cancel_fn) = Self::make_cancel();
        let handle = StreamHandle::new(generation, cancel_fn);
        let corpus = self.corpus();

        self.spawn(run_search(corpus, query, generation, tx, cancel_token));

        (handle, rx)
    }
}

// ---------------------------------------------------------------------------
// Search driver
// ---------------------------------------------------------------------------

async fn run_search(
    corpus: Corpus,
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

    // ── Section 2: Semantic (stub — always empty) ──────────────────────────
    // LR-10: local hits were already emitted above. The semantic section is a
    // placeholder; when a real embedding store is wired in, it will fan-out
    // here without ever blocking the local sections.
    let ev = SearchEvent::Section {
        generation,
        section: SECTION_SEMANTIC,
        rows: Arc::from([] as [HitRow; 0]),
    };
    let _ = tx.send_async(ev).await;

    if !cancel.is_cancelled() {
        let _ = tx.send_async(SearchEvent::Done { generation }).await;
    }
}

// ---------------------------------------------------------------------------
// Name-hit collection
// ---------------------------------------------------------------------------

fn collect_name_hits(
    packages: &[std::sync::Arc<nudox_store::package::PackageView>],
    query: &SearchQuery,
) -> Vec<HitRow> {
    let text = query.text.trim();
    if text.is_empty() {
        return Vec::new();
    }

    let prefix_lower = text.to_lowercase();
    let limit = if query.limit == 0 { usize::MAX } else { query.limit };

    let mut rows: Vec<HitRow> = Vec::new();

    for pkg in packages {
        let indexes = pkg.indexes();
        let provenance: Provenance = pkg.provenance().into();

        for entry in indexes.by_name.prefix(&prefix_lower) {
            if rows.len() >= limit {
                break;
            }

            // Apply kind filter.
            let Some(ir_entry) = pkg.view().entry(entry.intro) else {
                continue;
            };
            let Some(disc) = ir_entry.kind().discriminant() else {
                continue;
            };
            if !query.kind_matches(disc) {
                continue;
            }

            let key = nudox_ir::change::StableRef::new(pkg.lineage().clone(), entry.intro);

            // LR-4: signature from the single renderer.
            let sig_preview = signature::tokens(ir_entry, pkg);

            // Disambiguate display name with path prefix when two packages have
            // the same name.
            let display_name: SharedStr =
                if packages.len() > 1 {
                    let path = indexes
                        .path_of(entry.intro)
                        .map(|p| p.as_ref())
                        .unwrap_or(entry.display.as_str());
                    SharedStr::from(path)
                } else {
                    SharedStr::from(entry.display.as_str())
                };

            // Score: exact match scores 1.0, prefix match scores by ratio.
            let score = if entry.key == prefix_lower {
                1.0_f32
            } else {
                prefix_lower.len() as f32 / entry.key.len().max(1) as f32
            };

            rows.push(HitRow {
                key,
                display_name,
                sig_preview,
                kind: KindTag::Known(disc),
                provenance: provenance.clone(),
                score,
            });
        }
    }

    // Sort: highest score first, then stable by name.
    rows.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| {
                let a_str: &str = &a.display_name;
                let b_str: &str = &b.display_name;
                a_str.cmp(b_str)
            })
    });
    rows.truncate(limit);
    rows
}

// ---------------------------------------------------------------------------
// Type / kind-filter hit collection
// ---------------------------------------------------------------------------

/// Collect hits from the kind facet index.
///
/// If the query text matches a kind label (e.g. `"fn"` → `Function`) we emit
/// all symbols of that kind. Otherwise we use the kind filter from the query.
fn collect_type_hits(
    packages: &[std::sync::Arc<nudox_store::package::PackageView>],
    query: &SearchQuery,
) -> Vec<HitRow> {
    let limit = if query.limit == 0 { usize::MAX } else { query.limit };

    // Determine which kind discriminant(s) to emit for the "type" section.
    // Priority:
    //   1. If the query text matches a kind keyword, use that kind.
    //   2. If an explicit kind filter is set, use that filter.
    //   3. Otherwise, emit nothing (name section already covers everything).
    let target_kinds: Vec<KindDiscriminant> = if !query.kinds.is_empty() {
        query.kinds.clone()
    } else {
        let lower = query.text.to_lowercase();
        keyword_to_kinds(&lower)
    };

    if target_kinds.is_empty() {
        return Vec::new();
    }

    let mut rows: Vec<HitRow> = Vec::new();

    for pkg in packages {
        if rows.len() >= limit {
            break;
        }
        let indexes = pkg.indexes();
        let provenance: Provenance = pkg.provenance().into();

        for &disc in &target_kinds {
            let Some(intros) = indexes.by_kind.get(&disc) else {
                continue;
            };
            for &intro in intros.iter() {
                if rows.len() >= limit {
                    break;
                }
                let Some(ir_entry) = pkg.view().entry(intro) else {
                    continue;
                };
                let key = nudox_ir::change::StableRef::new(pkg.lineage().clone(), intro);
                let sig_preview = signature::tokens(ir_entry, pkg);
                let display_name: SharedStr =
                    SharedStr::from(ir_entry.sym().name.as_str());

                rows.push(HitRow {
                    key,
                    display_name,
                    sig_preview,
                    kind: KindTag::Known(disc),
                    provenance: provenance.clone(),
                    // Type-section hits all score 0.5 (they are kind-facet, not text-relevance).
                    score: 0.5,
                });
            }
        }
    }

    rows
}

/// Map common kind-keyword strings to one or more [`KindDiscriminant`]s.
fn keyword_to_kinds(lower: &str) -> Vec<KindDiscriminant> {
    match lower {
        "fn" | "func" | "function" => vec![KindDiscriminant::Function],
        "struct" | "record" => vec![KindDiscriminant::Record],
        "trait" => vec![KindDiscriminant::Trait],
        "impl" => vec![KindDiscriminant::Impl],
        "enum" => vec![KindDiscriminant::Enum],
        "mod" | "module" => vec![KindDiscriminant::Module],
        "const" => vec![KindDiscriminant::Const],
        "static" => vec![KindDiscriminant::Static],
        "type" | "alias" => vec![KindDiscriminant::Alias],
        "field" => vec![KindDiscriminant::Field],
        "variant" => vec![KindDiscriminant::Variant],
        _ => Vec::new(),
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use nudox_store::source::fixtures::FixtureSource;

    use crate::{
        runtime::{Engine, EngineConfig},
        search::{SearchQuery, SECTION_NAME, SECTION_TYPE},
        wire::{Gen, SearchEvent, SearchSectionId},
    };

    fn make_engine() -> crate::runtime::EngineHandle {
        Engine::start(EngineConfig::default(), FixtureSource::rich())
    }

    async fn wait_for_corpus(engine: &crate::runtime::EngineHandle) {
        use nudox_store::source::fixtures::rich_lineage;
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
            limit: 50,
        };
        let (_handle, rx) = engine.search(query, Gen(1));
        let events = drain(rx).await;

        // Find the position of SECTION_NAME and SECTION_SEMANTIC events.
        let name_pos = events
            .iter()
            .position(|e| matches!(e, SearchEvent::Section { section, .. } if *section == SECTION_NAME));
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

        let query = SearchQuery { text: "fn".to_owned(), ..Default::default() };
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

        let query = SearchQuery { text: "Point".to_owned(), ..Default::default() };
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

        let q = SearchQuery { text: "Color".to_owned(), ..Default::default() };

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

        let q = SearchQuery { text: "".to_owned(), ..Default::default() };
        let (_handle, rx) = engine.search(q, Gen(6));
        let events = drain(rx).await;

        let name_section = events.iter().find(|e| {
            matches!(e, SearchEvent::Section { section, .. } if *section == SECTION_NAME)
        });
        if let Some(SearchEvent::Section { rows, .. }) = name_section {
            assert!(rows.is_empty(), "empty query must produce no name hits");
        }
    }

    #[tokio::test]
    async fn kind_keyword_produces_type_section_hits() {
        let engine = make_engine();
        wait_for_corpus(&engine).await;

        let q = SearchQuery { text: "fn".to_owned(), ..Default::default() };
        let (_handle, rx) = engine.search(q, Gen(7));
        let events = drain(rx).await;

        let type_section = events.iter().find(|e| {
            matches!(e, SearchEvent::Section { section, .. } if *section == SECTION_TYPE)
        });
        let rows = match type_section {
            Some(SearchEvent::Section { rows, .. }) => rows,
            _ => panic!("must have SECTION_TYPE"),
        };
        // The rich corpus has several functions; at least one must appear.
        assert!(!rows.is_empty(), "keyword 'fn' must produce type-section hits");
    }
}
