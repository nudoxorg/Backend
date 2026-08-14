//! What the semantic section reports when an embedder *fails* mid-index.
//!
//! `semantic_local_mode.rs` proves the honest-coverage story for an embedder
//! that works: `Building { covered, total }` while packages are still landing,
//! `Complete` once they all have. This file proves the two failure paths in
//! `crate::runtime::semantic_indexer` — both of which are currently correct,
//! and neither of which had a test.
//!
//! # Why these two specifically
//!
//! `semantic_indexer` (`src/runtime.rs`) distinguishes three outcomes per
//! package, and the interesting property is that **two of them look identical
//! to a caller unless something asserts otherwise**:
//!
//! | outcome | `covered` | what a reader is told |
//! |---|---|---|
//! | all batches embedded | +1 | `Complete` once every package lands |
//! | `embed_batch` returned `Err` | unchanged | `Building { covered: 1, total: 2 }`, forever |
//! | `embed_batch` returned the wrong *number* of vectors | unchanged | same |
//!
//! The second and third rows are the ones worth defending. Both are one small
//! refactor away from reversing into a silent lie:
//!
//! - Dropping the `failed` flag's `continue` would insert the package with
//!   whatever partial vectors survived, mark it covered, and let the section
//!   report `Complete` — telling a reader "rows are the whole answer" about a
//!   package that was never searched. That is the silent-repair failure
//!   `docs/AGENTS-DOCTRINE.md` §8 names: presenting a degraded case as the good one.
//!
//! - Pairing a short vector list positionally (`zip` truncates without
//!   complaint, so this compiles and looks fine) would attach each vector to
//!   the *wrong* symbol. Every row would then be individually plausible and
//!   collectively wrong — the worst failure mode this index has, because
//!   nothing downstream can detect it. The indexer refuses instead, and this
//!   file is what keeps that refusal.
//!
//! Both tests are deterministic without any wall-clock assumption: they gate on
//! the embedder's own call counter, so "the failing package has been attempted"
//! is an observed fact rather than an elapsed duration. A test that concluded
//! "beta never appeared" while beta simply had not been reached yet would pass
//! against a stub, which §4 rules out.

use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use nudox_ir::apply::PristineIntroTable;
use nudox_ir::change::{EcosystemId, IntroId, PackageLineageId, PackageName};
use nudox_ir::entry::{Entry, Node, Symbol, Visibility};
use nudox_ir::index::RawRef;
use nudox_ir::kind::Kind;
use nudox_ir::kinds::Module;
use nudox_ir::view::IrView;
use nudox_engine::store::package::{PackageView, Provenance};
use nudox_engine::store::source::{
    IrSource, LoadEvent, LoadRequest, PackageHint, SourceDescriptor, Error,
};

use futures::stream::BoxStream;
use nudox_engine::wire::{Gen, SearchEvent};
use nudox_engine::{
    EmbedError, EmbedRole, Embedder, EmbedderInfo, Engine, EngineConfig, PackageLoadEvent,
    SearchQuery, SectionState, SharedStr,
};

// ---------------------------------------------------------------------------
// Fixture packages
// ---------------------------------------------------------------------------

const DIMS: usize = 3;

fn lineage(name: &str) -> PackageLineageId {
    PackageLineageId::new(EcosystemId::new("test-semantic-fail"), PackageName::new(name))
}

fn intro(n: u8) -> IntroId {
    IntroId::from_raw([n; 32])
}

fn public_module_entry(name: &str) -> Entry {
    let sym = Symbol {
        name: name.to_owned(),
        visibility: Visibility::Public,
        documentation: format!("{name} is a fixture symbol for the semantic failure-mode tests."),
        source: std::path::PathBuf::new(),
        span: 0..0,
        aliases: Box::new([]),
        deprecation: None,
        doc_links: Box::new([]),
        attrs: Box::new([]),
        cfg: None,
    };
    Entry::new(sym, Node::build(None::<RawRef>, []), Kind::Module(Module))
}

/// A package with one documentable symbol per name in `entry_names`.
///
/// The mispairing test needs a package with **two** symbols: with one symbol a
/// short vector list is empty, which is indistinguishable from a plain
/// failure. Two symbols and one returned vector is the case where truncating
/// `zip` would silently attach `BetaOne`'s text to whichever vector came back.
fn build_package(lid: &PackageLineageId, entry_names: &[&str]) -> Arc<PackageView> {
    let mut table = PristineIntroTable::new();
    for (i, name) in entry_names.iter().enumerate() {
        table.insert_live(intro(i as u8 + 1), public_module_entry(name), None);
    }
    let view = IrView::with_package(lid.clone(), table);
    Arc::new(PackageView::build(view, Provenance::TrustedLocal))
}

struct TwoPackageSource {
    first: (PackageLineageId, Arc<PackageView>),
    second: (PackageLineageId, Arc<PackageView>),
}

impl IrSource for TwoPackageSource {
    fn describe(&self) -> SourceDescriptor {
        SourceDescriptor {
            label: "two-package-semantic-failure-fixture".to_owned(),
            package_count_hint: Some(2),
        }
    }

    fn load(&self, _req: LoadRequest) -> BoxStream<'static, Result<LoadEvent, Error>> {
        use futures::StreamExt as _;
        let events: Vec<Result<LoadEvent, Error>> = [&self.first, &self.second]
            .into_iter()
            .flat_map(|(lid, pkg)| {
                [
                    Ok(LoadEvent::Discovered {
                        lineage: lid.clone(),
                        hint: PackageHint {
                            display_name: lid.name.as_str().to_owned(),
                            ecosystem: lid.ecosystem.as_str().to_owned(),
                            version: Some("0.0.1".to_owned()),
                        },
                    }),
                    Ok(LoadEvent::Ready {
                        package: Arc::clone(pkg),
                    }),
                ]
            })
            .collect();
        futures::stream::iter(events).boxed()
    }
}

// ---------------------------------------------------------------------------
// An embedder that succeeds once, then fails in a chosen way
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Eq)]
enum Failure {
    /// `embed_batch` returns `Err` — a missing weights file, an OOM runtime, a
    /// revoked key.
    Error,
    /// `embed_batch` returns `Ok` with **fewer vectors than texts**. The
    /// dangerous one: it is a success as far as the type system is concerned.
    ShortVectorList,
}

struct FailAfterFirstEmbedder {
    document_calls: Arc<AtomicUsize>,
    failure: Failure,
}

impl FailAfterFirstEmbedder {
    fn new(failure: Failure) -> (Arc<Self>, Arc<AtomicUsize>) {
        let document_calls = Arc::new(AtomicUsize::new(0));
        (
            Arc::new(Self {
                document_calls: Arc::clone(&document_calls),
                failure,
            }),
            document_calls,
        )
    }
}

impl Embedder for FailAfterFirstEmbedder {
    fn info(&self) -> EmbedderInfo {
        EmbedderInfo {
            model_id: SharedStr::from("test-fail-after-first-embedder"),
            dimensions: DIMS,
            // One `embed_batch` call per package, so the call counter maps 1:1
            // onto packages and "the second package was attempted" is exact.
            max_batch: 1_000_000,
            durable_canonical: false,
        }
    }

    fn embed_batch<'a>(
        &'a self,
        texts: &'a [String],
        role: EmbedRole,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<Vec<f32>>, EmbedError>> + Send + 'a>> {
        Box::pin(async move {
            let unit = {
                let mut v = vec![0.0f32; DIMS];
                v[0] = 1.0;
                v
            };

            // Query-role calls must keep working: a failure to *index* one
            // package is not a failure to *search* the packages that did index,
            // and a test where the query itself broke would prove nothing about
            // coverage.
            if role != EmbedRole::Document {
                return Ok(texts.iter().map(|_| unit.clone()).collect());
            }

            let call = self.document_calls.fetch_add(1, Ordering::SeqCst);
            if call == 0 {
                return Ok(texts.iter().map(|_| unit.clone()).collect());
            }

            match self.failure {
                Failure::Error => Err(EmbedError::Backend(
                    "fixture: the model refused this batch".to_owned(),
                )),
                // One fewer vector than texts. `zip` would silently truncate
                // and mispair; the indexer must refuse the whole package.
                Failure::ShortVectorList => {
                    Ok(texts.iter().skip(1).map(|_| unit.clone()).collect())
                }
            }
        })
    }
}

// ---------------------------------------------------------------------------
// Harness
// ---------------------------------------------------------------------------

async fn wait_for_n_loaded(engine: &nudox_engine::EngineHandle, n: usize) {
    let rx = engine.packages();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    let mut seen = 0;
    while seen < n {
        match tokio::time::timeout_at(deadline, rx.recv_async()).await {
            Ok(Ok(PackageLoadEvent::Loaded { .. })) => seen += 1,
            Ok(Ok(_)) => continue,
            Ok(Err(_)) => break,
            Err(_) => panic!("only {seen}/{n} packages loaded within 5s"),
        }
    }
    assert_eq!(seen, n, "expected exactly {n} Loaded events");
}

/// Block until the indexer has *attempted* `n` document batches.
///
/// This is what makes the assertions below deterministic rather than a race:
/// "beta never appeared in results" is only meaningful once beta has actually
/// been handed to the embedder and rejected.
async fn wait_for_document_calls(counter: &AtomicUsize, n: usize) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    while counter.load(Ordering::SeqCst) < n {
        if tokio::time::Instant::now() >= deadline {
            panic!(
                "embedder saw only {} document batches within 5s, expected {n}",
                counter.load(Ordering::SeqCst)
            );
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
}

async fn observe_semantic(
    engine: &nudox_engine::EngineHandle,
    generation: Gen,
) -> (Option<SectionState>, Vec<String>) {
    let (_stream, rx) = engine.search(
        SearchQuery {
            text: "fixture".to_owned(),
            ..Default::default()
        },
        generation,
    );
    let mut state = None;
    let mut names = Vec::new();
    while let Ok(event) = rx.recv_async().await {
        match event {
            SearchEvent::SectionState {
                section, state: s, ..
            } if section == nudox_engine::search::SECTION_SEMANTIC => {
                state = Some(s);
            }
            SearchEvent::Section { section, rows, .. }
            | SearchEvent::Merge { section, rows, .. }
                if section == nudox_engine::search::SECTION_SEMANTIC =>
            {
                names.extend(rows.iter().map(|r| r.display_name.to_string()));
            }
            SearchEvent::Done { .. } => break,
            SearchEvent::Failed { error, .. } => panic!("search failed: {error:?}"),
            _ => {}
        }
    }
    (state, names)
}

/// The shared body of both tests: alpha indexes, beta fails in `failure`'s way.
///
/// Returns nothing — it asserts. Written as one function because the two
/// failure modes must produce *identical* observable behaviour, and writing
/// that twice invites the two copies to drift apart until only one of them is
/// still checking the thing that matters.
async fn a_failing_package_is_never_covered(failure: Failure) {
    let alpha = lineage("alpha");
    let beta = lineage("beta");
    let alpha_pkg = build_package(&alpha, &["Alpha"]);
    // Two symbols so a truncating `zip` would have something to mispair.
    let beta_pkg = build_package(&beta, &["BetaOne", "BetaTwo"]);

    let (embedder, document_calls) = FailAfterFirstEmbedder::new(failure);
    let engine = Engine::start(
        EngineConfig {
            embedder: Some(embedder as Arc<dyn Embedder>),
            ..EngineConfig::default()
        },
        TwoPackageSource {
            first: (alpha.clone(), alpha_pkg),
            second: (beta.clone(), beta_pkg),
        },
    );

    wait_for_n_loaded(&engine, 2).await;
    // Both packages have been handed to the embedder — alpha accepted, beta
    // rejected. Everything below is now a statement about a settled index.
    wait_for_document_calls(&document_calls, 2).await;

    let mut gens = Gen(200);
    // Sample repeatedly: the claim is not "it is Building right now" but "it
    // never becomes Complete", and a single observation cannot say that. The
    // indexer's queue is drained by the time we get here, so every one of these
    // must agree.
    for _ in 0..10 {
        gens.0 += 1;
        let (state, names) = observe_semantic(&engine, gens).await;
        let state = state.expect("the semantic section must report a state");

        assert_eq!(
            state,
            SectionState::Building {
                covered: 1,
                total: 2
            },
            "a package whose embedding failed must stay in the uncovered count. \
             `Complete` here would tell a reader the rows are the whole answer \
             while one of two packages was never searched (runtime.rs `failed` \
             path). failure mode: {}",
            match failure {
                Failure::Error => "embed_batch returned Err",
                Failure::ShortVectorList => "embed_batch returned too few vectors",
            }
        );

        assert!(
            names.iter().any(|n| n == "Alpha"),
            "the package that DID index must remain searchable — one package's \
             failure must not take the working one down with it; rows: {names:?}"
        );
        for bad in ["BetaOne", "BetaTwo"] {
            assert!(
                !names.iter().any(|n| n == bad),
                "`{bad}` came from the package the embedder failed on. Either it \
                 was inserted with partial vectors, or a short vector list was \
                 paired positionally and this row is attached to the wrong \
                 symbol. Both are worse than returning nothing; rows: {names:?}"
            );
        }

        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

// ---------------------------------------------------------------------------
// The tests
// ---------------------------------------------------------------------------

/// An `Err` from `embed_batch` leaves the package uncovered rather than
/// completing the section over a package that was never searched.
#[tokio::test]
async fn a_package_whose_embedder_errors_is_never_counted_as_covered() {
    a_failing_package_is_never_covered(Failure::Error).await;
}

/// An embedder that returns fewer vectors than it was given has broken the
/// positional pairing its trait documents. The indexer must abandon the
/// package, not `zip` the lists and attach each vector to whichever symbol
/// happens to line up.
///
/// This is the failure this file exists for. An `Err` is loud; a short `Ok` is
/// silent, type-correct, and produces an index in which every row looks right
/// and points at the wrong thing.
#[tokio::test]
async fn an_embedder_returning_too_few_vectors_abandons_the_package_rather_than_mispairing() {
    a_failing_package_is_never_covered(Failure::ShortVectorList).await;
}
