//! Search fan-out: name and type sections, then semantic neighbours.
//!
//! Name and type events are sent before this module awaits an embedding model.

use std::{sync::Arc, time::Instant};

use tokio_util::sync::CancellationToken;
use tracing::debug;

use crate::{
    chunk::signature,
    store::corpus::Corpus,
    wire::{HitRow, KindTag, SearchEvent, SharedStr},
};

use super::{
    Candidate, Gen, SECTION_NAME, SECTION_SEMANTIC, SECTION_TYPE, SearchQuery, collect_name_hits,
    collect_type_hits, compare_candidates, finalize_candidates, hits, qualified_display_name,
    score_of,
};

// ---------------------------------------------------------------------------
// Search driver
// ---------------------------------------------------------------------------

pub(super) async fn run_search(
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

    // Semantic search still reads the resident corpus. Name and kind search
    // use the corpus indexes and do not.
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
    // The corpus name index already knows which packages declare a prefix.
    // Opening the rest would repeat work the index exists to avoid.
    let name_start = Instant::now();
    let prefix = query.text.trim().to_lowercase();
    let name_packages = if prefix.is_empty() {
        Vec::new()
    } else {
        corpus.packages_with_name_prefix(&prefix).await
    };
    let name_rows = collect_name_hits(&name_packages, &query);
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
    // Signature facets resolve type names through the declaration index.
    // A kind query opens only packages that declare one of those kinds.
    let type_start = Instant::now();
    let (declared, type_packages) = match crate::typequery::TypeQuery::parse(&query.text) {
        Some(parsed) => {
            let names: Vec<String> = parsed
                .facets
                .iter()
                .map(|facet| facet.type_name.clone())
                .collect();
            let declared = corpus.packages_declaring(&names).await;
            let users = match hits::resolved_type_facets(&parsed, &declared) {
                Some(facets) => corpus.packages_referencing_facets(&facets).await,
                None => Vec::new(),
            };
            (declared, users)
        }
        None => {
            let kinds = hits::kind_targets(&query);
            let owners = corpus.packages_with_kinds(&kinds).await;
            (std::collections::BTreeMap::new(), owners)
        }
    };
    let type_rows = collect_type_hits(&type_packages, &query, &declared);
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
pub(super) async fn collect_semantic_hits(
    packages: &[std::sync::Arc<crate::store::package::PackageView>],
    semantic: &crate::semantic::SemanticIndex,
    embedder: Option<&dyn crate::semantic::Embedder>,
    query: &SearchQuery,
) -> (crate::semantic::SectionState, Vec<HitRow>) {
    use crate::semantic::{SectionState, Unavailable};

    let Some(embedder) = embedder else {
        // No embedder is installed on this engine. `embed::unavailable_reason`
        // is the same env check `embed::load_from_env` itself uses to decide
        // there is no embedder to install, so it is the truthful source for
        // why — today that is always a missing `NUDOX_EMBED_MODEL_DIR`, since
        // the runtime is compiled in unconditionally. The fallback only
        // matters for a caller that builds an `EngineConfig` with no embedder
        // by hand rather than through `load_from_env` (as the fixture-corpus
        // tests do) in an environment where the variable happens to be set;
        // `NoModelConfigured` is the right default there because it is this
        // crate's only remaining "no embedder" state.
        return (
            SectionState::Unavailable {
                reason: crate::embed::unavailable_reason()
                    .unwrap_or(Unavailable::NoModelConfigured),
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

    // Rank every survivor of the over-fetch. Stopping at `limit` here would
    // keep the first cosine hits and drop a later one whose visibility and
    // kind weights sort it ahead.
    let mut candidates: Vec<Candidate> = Vec::new();
    for (lineage, intro, cosine) in nearest {
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
                score: score_of(cosine_to_relevance(cosine), ir_entry.sym().visibility, disc),
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
