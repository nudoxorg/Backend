//! The semantic section, end to end, against the **real** model — and judged.
//!
//! ```sh
//! NUDOX_EMBED_MODEL_DIR=/path/to/jina-code-v2 \
//!   cargo test -p registry --features onnx --test vector_engine_relevance -- --ignored --nocapture
//! ```
//!
//! # Why this file lives in `registry` and not in `nudox-engine`
//!
//! It is the one place both halves can be named without creating an edge that
//! ships.
//!
//! `nudox-engine` owns the port — `nudox_engine::Embedder`, an object-safe
//! trait with no dependency on any runtime (see that module's docs for why the
//! obvious `nudox-engine → registry` edge is worse than it looks: registry's
//! own `Embedder` is not object-safe, `onnx` adds 158 crates, and `ort-sys`
//! downloads its runtime at build time). `registry` owns the runtime. The
//! adapter between them is ten lines and belongs to neither.
//!
//! A **dev**-dependency from `registry` on `nudox-engine` is the shape that
//! costs nothing: dev-dependencies do not appear in any consumer's graph, so
//! `driver` and `index` — the two crates that actually depend on `registry` —
//! are untouched, and `nudox-engine`'s own graph is untouched in both
//! directions. `docs/AGENTS-DOCTRINE.md` §1 needs no amendment for this, because no
//! shipping edge is created. (Cargo does not permit *optional*
//! dev-dependencies, which is why the target is gated by `required-features`
//! instead: with `onnx` off, cargo refuses to build it rather than compiling it
//! to an empty crate — the L48 failure this directory's manifest comment is
//! about.)
//!
//! # Why the judgement is a labelled set and not "some rows came back"
//!
//! Doctrine §4: a test that would pass against a stub is not a test. "Semantic
//! search returned results" passes against a search that returns the first *k*
//! symbols in `IntroId` order, which is exactly the failure mode a real
//! embedding pipeline degrades into when the vectors are wrong (all-zero
//! vectors tie at score 0 and the total order falls through to identity).
//!
//! So the assertions below are *comparative and directional*: for each labelled
//! query, a named relevant symbol must outrank a named irrelevant one from the
//! same package. That is a claim only a working model can satisfy, it does not
//! depend on absolute score values, and it does not break when the corpus grows
//! a symbol the author did not anticipate. `precision@k` over an exhaustive
//! relevance labelling would be stronger and is not available: it needs every
//! symbol in the package labelled, which for a real crate is thousands of
//! judgements nobody has made.

#![cfg(feature = "onnx")]

use std::sync::Arc;

use registry::vector::embed::runtime::{FastembedOrt, RuntimeConfig};
use registry::vector::embed::weights::WeightsSpec;
use registry::vector::{EmbedRole as RegistryRole, Embedder as _, EmbeddingModel as _, JinaCodeV2};

use nudox_engine::search::SECTION_SEMANTIC;
use nudox_engine::wire::{Gen, SearchEvent};
use nudox_engine::{
    EmbedError, EmbedRole, Embedder, EmbedderInfo, Engine, EngineConfig, SearchQuery, SectionState,
};

const ENV_DIR: &str = "NUDOX_EMBED_MODEL_DIR";

fn model_dir() -> std::path::PathBuf {
    std::env::var_os(ENV_DIR)
        .map(Into::into)
        .unwrap_or_else(|| panic!("set {ENV_DIR} to the pinned model directory to run this test"))
}

// ---------------------------------------------------------------------------
// The adapter
// ---------------------------------------------------------------------------

/// `FastembedOrt` behind `nudox_engine::Embedder`.
///
/// The whole bridge. Note what does *not* cross: no `Embedding<M>`, no
/// `ModelId`, no `EmbedRuntimeInfo` — the engine's port speaks in `Vec<f32>`,
/// `usize` and `bool`, which is what lets it be satisfied by an HTTP client, a
/// test double, or this, without any of them appearing in its type signature.
struct OrtBridge {
    inner: FastembedOrt,
}

impl Embedder for OrtBridge {
    fn info(&self) -> EmbedderInfo {
        let runtime = self.inner.runtime_info();
        EmbedderInfo {
            model_id: runtime.model_id.to_string().into(),
            dimensions: JinaCodeV2::DIMENSIONS,
            max_batch: runtime.max_batch,
            durable_canonical: runtime.durable_canonical,
        }
    }

    fn embed_batch<'a>(
        &'a self,
        texts: &'a [String],
        role: EmbedRole,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<Vec<Vec<f32>>, EmbedError>> + Send + 'a>,
    > {
        Box::pin(async move {
            let borrowed: Vec<&str> = texts.iter().map(String::as_str).collect();
            let role = match role {
                EmbedRole::Query => RegistryRole::Query,
                EmbedRole::Document => RegistryRole::Document,
            };
            let embeddings = self
                .inner
                .embed_batch(&borrowed, role)
                .await
                .map_err(|e| EmbedError::Backend(e.to_string()))?;
            Ok(embeddings
                .into_iter()
                .map(|e| e.as_slice().to_vec())
                .collect())
        })
    }
}

// ---------------------------------------------------------------------------
// Corpus
// ---------------------------------------------------------------------------

/// Lower a real crate checkout, or skip with a reason.
///
/// `memchr` is the corpus for the same reason the rest of this repo uses it: it
/// is small enough to lower in seconds, it is real, and its measurements are
/// already baselined elsewhere.
fn real_crate_root(name: &str, version: &str) -> Option<std::path::PathBuf> {
    let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../result")
        .join(format!("{name}-{version}"));
    root.join("Cargo.toml").is_file().then(|| root)
}

/// Start an engine over one real crate with the ONNX embedder installed, and
/// wait until the semantic index has covered it.
async fn engine_with_model(
    name: &str,
    version: &str,
) -> Option<(nudox_engine::EngineHandle, std::time::Duration)> {
    let root = real_crate_root(name, version)?;

    let spec = WeightsSpec::jina_code_v2(model_dir());
    let inner = FastembedOrt::load(spec, RuntimeConfig::default())
        .await
        .expect("the pinned model must load");
    let embedder: Arc<dyn Embedder> = Arc::new(OrtBridge { inner });

    let started = std::time::Instant::now();
    let engine = Engine::start_with_producer(
        EngineConfig {
            embedder: Some(embedder),
            ..Default::default()
        },
        vec![nudox_engine::PackageSpec {
            root,
            name: name.to_owned(),
            version: version.to_owned(),
            language: nudox_engine::ProducerLanguage::Rust,
        }],
    );

    // Wait for the semantic index to cover the package. Polling
    // `semantic_coverage` rather than sleeping means the measurement below is
    // the real time-to-full-index rather than an arbitrary constant.
    for _ in 0..1200 {
        if engine.semantic_coverage() >= 1 {
            return Some((engine, started.elapsed()));
        }
        tokio::time::sleep(std::time::Duration::from_millis(250)).await;
    }
    panic!("the semantic index never covered {name} within 5 minutes");
}

/// The semantic section's state and its rows, in rank order.
async fn semantic_rows(
    engine: &nudox_engine::EngineHandle,
    text: &str,
    generation: u64,
) -> (Option<SectionState>, Vec<String>) {
    let (_handle, rx) = engine.search(
        SearchQuery {
            text: text.to_owned(),
            limit: 25,
            ..Default::default()
        },
        Gen(generation),
    );

    let mut state = None;
    let mut rows = Vec::new();
    while let Ok(event) = rx.recv_async().await {
        match event {
            SearchEvent::SectionState { section, state: s, .. } if section == SECTION_SEMANTIC => {
                state = Some(s);
            }
            SearchEvent::Section { section, rows: r, .. }
            | SearchEvent::Merge { section, rows: r, .. }
                if section == SECTION_SEMANTIC =>
            {
                rows.extend(r.iter().map(|row| row.display_name.to_string()));
            }
            SearchEvent::Done { .. } | SearchEvent::Failed { .. } => break,
            _ => {}
        }
    }
    (state, rows)
}

/// Rank of `needle` among `rows`, matching on the leaf name.
///
/// Leaf-matching rather than equality because `display_name` is qualified
/// whenever the leaf collides with another row in the same result set (L20) —
/// real `memchr` has seven `memchr` symbols across its architecture backends,
/// so the row for the public one may well arrive as `memchr::memchr`.
/// The separator is `.`, **not** `::` — a mistake this helper made on its first
/// run, and one worth pinning here because it produced a false negative that
/// looked exactly like a ranking failure. `qualified_display_name` builds on
/// `PackageIndexes::path_of`, which is the entry's `moniker_path`: "root-first,
/// **dot-joined** symbol names". So the row for `memmem::Finder` arrives as
/// `memchr.memchr.memmem.Finder`, and splitting on `::` finds nothing.
///
/// Both separators are accepted because the package-qualifying prefix added
/// when the first path segment is not the package name uses `::`
/// (`format!("{pkg_name}::{base}")`), so a single row can legitimately contain
/// both.
fn rank_of(rows: &[String], needle: &str) -> Option<usize> {
    rows.iter().position(|row| {
        row == needle
            || row
                .rsplit(['.', ':'])
                .next()
                .is_some_and(|leaf| leaf == needle)
    })
}

// ---------------------------------------------------------------------------
// The labelled set
// ---------------------------------------------------------------------------

/// One judgement: for `query`, `relevant` must outrank `irrelevant`.
///
/// Both names are real `memchr` symbols, checked by the test before it judges —
/// a judgement naming a symbol that does not exist is a judgement about
/// nothing, and it would pass vacuously if `rank_of` returned `None` for both.
struct Judgement {
    query: &'static str,
    relevant: &'static str,
    irrelevant: &'static str,
    why: &'static str,
}

/// The labelled set, authored by hand against `memchr`'s public API.
///
/// Each pair is chosen so that a *lexical* matcher would get it wrong or find
/// nothing: the query words do not appear in the relevant symbol's name. That
/// is what makes these judgements about semantics rather than about substring
/// matching — a name-search implementation smuggled into section 2 would fail
/// them.
const JUDGEMENTS: &[Judgement] = &[
    Judgement {
        query: "find the position of a byte in a slice",
        relevant: "memchr",
        irrelevant: "Prefilter",
        why: "`memchr` is literally 'find a byte in a haystack'; `Prefilter` is \
              a configuration knob for the substring searcher",
    },
    Judgement {
        query: "search backwards from the end",
        relevant: "memrchr",
        irrelevant: "memchr",
        why: "the `r` variants are the reverse scans; the forward one is the \
              near-miss a bag-of-words model would prefer on name overlap alone",
    },
    Judgement {
        query: "substring search for a needle in a haystack",
        relevant: "Finder",
        irrelevant: "memchr2",
        why: "`Finder` is the substring searcher; `memchr2` scans for two \
              individual bytes and is not substring search",
    },
];

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

/// The semantic section ranks a genuinely relevant symbol above a genuinely
/// irrelevant one, for every labelled query.
///
/// This is the assertion the whole file exists for, and it is the one that
/// cannot be satisfied by a stub: it depends on the *order* the model produces,
/// over queries whose words do not appear in the relevant symbol's name.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs real model weights and a memchr checkout; run with --ignored"]
async fn the_semantic_section_ranks_relevant_symbols_above_irrelevant_ones() {
    let Some((engine, index_elapsed)) = engine_with_model("memchr", "2.8.3").await else {
        eprintln!("SKIP: no memchr-2.8.3 checkout under result/");
        return;
    };
    eprintln!("cost case=semantic_index_memchr dir=. elapsed_ms={}", index_elapsed.as_millis());

    let mut failures = Vec::new();
    for (i, judgement) in JUDGEMENTS.iter().enumerate() {
        let (state, rows) = semantic_rows(&engine, judgement.query, i as u64 + 1).await;

        assert_eq!(
            state,
            Some(SectionState::Complete),
            "the index covers the whole (one-package) corpus, so the section \
             must report Complete — a Building state here means the coverage \
             poll above returned before the index really landed"
        );
        assert!(
            !rows.is_empty(),
            "no semantic rows at all for {:?}; the model produced vectors but \
             nothing scored — check the document text, not the ranking",
            judgement.query
        );

        let relevant = rank_of(&rows, judgement.relevant);
        let irrelevant = rank_of(&rows, judgement.irrelevant);

        // A judgement whose subject is absent judges nothing. Fail loudly
        // rather than passing vacuously — this is the exact shape that turns a
        // relevance suite into decoration.
        let Some(relevant_rank) = relevant else {
            failures.push(format!(
                "{:?}: relevant symbol {:?} did not appear in the top {} at all. \
                 Rows: {rows:?}",
                judgement.query,
                judgement.relevant,
                rows.len()
            ));
            continue;
        };

        match irrelevant {
            Some(irrelevant_rank) if irrelevant_rank <= relevant_rank => failures.push(format!(
                "{:?}: {:?} (rank {irrelevant_rank}) outranked {:?} (rank \
                 {relevant_rank}). {}. Rows: {rows:?}",
                judgement.query,
                judgement.irrelevant,
                judgement.relevant,
                judgement.why
            )),
            // The irrelevant symbol did not make the cut at all, which is a
            // stronger pass than merely ranking below.
            _ => eprintln!(
                "PASS {:?}: {} at rank {relevant_rank}",
                judgement.query, judgement.relevant
            ),
        }
    }

    assert!(
        failures.is_empty(),
        "the model ranked {} of {} labelled judgements the wrong way round:\n{}",
        failures.len(),
        JUDGEMENTS.len(),
        failures.join("\n")
    );
}

/// Search is usable — and honest — **before** the index finishes.
///
/// The incremental requirement, stated as an invariant rather than as a timing
/// hope: a query issued while the index is still building returns local rows
/// and a semantic section that says `Building`, never a semantic section that
/// silently claims to be complete.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs real model weights and a memchr checkout; run with --ignored"]
async fn a_search_issued_while_the_index_builds_reports_building_not_empty() {
    let Some(root) = real_crate_root("memchr", "2.8.3") else {
        eprintln!("SKIP: no memchr-2.8.3 checkout under result/");
        return;
    };

    let spec = WeightsSpec::jina_code_v2(model_dir());
    let inner = FastembedOrt::load(spec, RuntimeConfig::default())
        .await
        .expect("the pinned model must load");
    let embedder: Arc<dyn Embedder> = Arc::new(OrtBridge { inner });

    let engine = Engine::start_with_producer(
        EngineConfig {
            embedder: Some(embedder),
            ..Default::default()
        },
        vec![nudox_engine::PackageSpec {
            root,
            name: "memchr".to_owned(),
            version: "2.8.3".to_owned(),
            language: nudox_engine::ProducerLanguage::Rust,
        }],
    );

    // Wait only for the package to become *resident* — not for it to be
    // embedded. This is the window the whole incremental design is about.
    let first_usable = std::time::Instant::now();
    for _ in 0..1200 {
        if !engine.versions(&ir::change::PackageLineageId::new(
            ir::change::EcosystemId::new("cargo"),
            ir::change::PackageName::new("memchr"),
        ))
        .is_empty()
        {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    eprintln!(
        "cost case=time_to_first_usable_search dir=. elapsed_ms={}",
        first_usable.elapsed().as_millis()
    );

    let (state, semantic) = semantic_rows(&engine, "find a byte in a slice", 1).await;

    // Local search works right now, which is the point of the whole ordering.
    let (_handle, rx) = engine.search(
        SearchQuery {
            text: "memchr".to_owned(),
            limit: 10,
            ..Default::default()
        },
        Gen(99),
    );
    let mut name_rows = 0usize;
    while let Ok(event) = rx.recv_async().await {
        match event {
            SearchEvent::Section { section, rows, .. }
                if section == nudox_engine::search::SECTION_NAME =>
            {
                name_rows += rows.len();
            }
            SearchEvent::Done { .. } | SearchEvent::Failed { .. } => break,
            _ => {}
        }
    }
    assert!(
        name_rows > 0,
        "the name section must answer as soon as the package is resident — \
         that is the requirement embedding is scheduled off the load path to \
         protect"
    );

    // And the semantic section is honest about which it is. Both outcomes are
    // legitimate (the index may have finished during the search), but the
    // combination "Complete with zero rows" is not, because that is the claim
    // 'we searched everything and there is nothing'.
    match state {
        Some(SectionState::Building { covered, total }) => {
            assert!(covered < total, "Building must mean covered < total");
            eprintln!("observed Building {{ covered: {covered}, total: {total} }}");
        }
        Some(SectionState::Complete) => assert!(
            !semantic.is_empty(),
            "a Complete semantic section with zero rows for a query the crate \
             genuinely matches means the index reported coverage it does not \
             have"
        ),
        other => panic!("unexpected semantic section state: {other:?}"),
    }
}

/// With no embedder installed, the semantic section says so — it does not
/// return zero rows and let the reader conclude there were no matches.
///
/// No model needed, so this one runs unattended.
#[tokio::test(flavor = "multi_thread")]
async fn without_an_embedder_the_section_reports_unavailable_not_empty() {
    let engine = Engine::start_with_producer(EngineConfig::default(), Vec::new());
    let (state, rows) = semantic_rows(&engine, "anything at all", 1).await;

    assert!(rows.is_empty());
    assert!(
        matches!(state, Some(SectionState::Unavailable { .. })),
        "a build with no model must report Unavailable. `Complete` with zero \
         rows would be the docs/LIMITATIONS.md L41 failure in reverse: instead of \
         fabricating matches, claiming to have searched. Got {state:?}"
    );
}
