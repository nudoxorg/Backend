//! Semantic search's local-mode behaviour with a real (installed, in-process)
//! [`Embedder`] — the two-thirds of `SectionState` that
//! `semantic_section_contract.rs` cannot exercise.
//!
//! # Division of labour with the other two semantic test files
//!
//! | file | embedder | proves |
//! |---|---|---|
//! | `semantic_section_contract.rs` | none (`EngineConfig::default()`) | `Unavailable { NoModelConfigured }` is reported, not an empty `Section` |
//! | `workspace/registry/tests/vector/engine_relevance.rs` | real ONNX model, `#[ignore]`, needs the artifact | ranking quality — a relevant symbol outranks an irrelevant one |
//! | **this file** | a fake, in-process [`Embedder`] | `Building { covered, total }` mid-index, honest partial coverage, and the eventual `Complete` |

//!
//! Doctrine §1 ("Capability ports") is exactly what makes this file possible
//! without the artifact gap docs/LIMITATIONS.md L41 records: the engine takes
//! `Arc<dyn Embedder>`, and a fake that returns a fixed unit vector is a
//! complete implementation of that trait. Nothing here needs ONNX, a model
//! file, or `ORT_LIB_LOCATION` — which is the whole point of the port.
//!
//! # How "mid-build" is made deterministic, not timed
//!
//! `crate::runtime::semantic_indexer` embeds packages **serially**, off one
//! unbounded channel, in the order they became resident (see that function's
//! module docs). Two packages are loaded here; the fake embedder counts its
//! own `Document`-role calls and blocks the **second** one on a
//! `tokio::sync::watch` gate the test holds the sender for. Because a
//! `watch::Receiver` always sees the current value — even one set before the
//! receiver was cloned — there is no missed-wakeup window: the first
//! package's embedding can never be delayed by the gate, and the second's can
//! never proceed until the test releases it. The only thing left to a race is
//! *how many polls* it takes to observe the first package landing, which is
//! why the observing helper below polls in a bounded loop instead of asserting
//! on the first search.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use nudox_engine::store::package::{PackageView, Provenance};
use nudox_engine::store::source::{
    Error, IrSource, LoadEvent, LoadRequest, PackageHint, SourceDescriptor,
};
use nudox_ir::apply::PristineIntroTable;
use nudox_ir::change::{EcosystemId, IntroId, PackageLineageId, PackageName};
use nudox_ir::entry::{Entry, Node, Symbol, Visibility};
use nudox_ir::index::RawRef;
use nudox_ir::kind::Kind;
use nudox_ir::kinds::Module;
use nudox_ir::view::IrView;

use futures::stream::BoxStream;
use nudox_engine::wire::{Gen, SearchEvent};
use nudox_engine::{
    EmbedError, EmbedRole, Embedder, EmbedderInfo, Engine, EngineConfig, PackageLoadEvent,
    SearchQuery, SectionState, SharedStr,
};

// ---------------------------------------------------------------------------
// A two-package, in-memory source (mirrors tests/multi_package_flows.rs)
// ---------------------------------------------------------------------------

fn lineage(name: &str) -> PackageLineageId {
    PackageLineageId::new(EcosystemId::new("test-semantic"), PackageName::new(name))
}

fn intro(n: u8) -> IntroId {
    IntroId::from_raw([n; 32])
}

/// One public, documentable module entry — enough to make `documents_of`
/// (`crate::runtime`) emit exactly one document, which is what lets the fake
/// embedder's call count map 1:1 onto packages (see the module docs).
fn public_module_entry(name: &str) -> Entry {
    let sym = Symbol {
        name: name.to_owned(),
        visibility: Visibility::Public,
        documentation: format!("{name} is a fixture symbol for the semantic local-mode tests."),
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

fn build_package(lid: &PackageLineageId, entry_name: &str) -> Arc<PackageView> {
    let mut table = PristineIntroTable::new();
    table.insert_live(intro(1), public_module_entry(entry_name), None);
    let view = IrView::with_package(lid.clone(), table);
    Arc::new(PackageView::build(view, Provenance::TrustedLocal))
}

/// Replays `Discovered` + `Ready` for two packages, in a fixed order — the
/// order [`super::GateEmbedder`] relies on to know which `Document` call
/// belongs to which package.
struct TwoPackageSource {
    first: (PackageLineageId, Arc<PackageView>),
    second: (PackageLineageId, Arc<PackageView>),
}

impl IrSource for TwoPackageSource {
    fn describe(&self) -> SourceDescriptor {
        SourceDescriptor {
            label: "two-package-semantic-fixture".to_owned(),
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
// GateEmbedder: a real Embedder impl, with the second package's indexing held
// open under the test's control.
// ---------------------------------------------------------------------------

/// Fixed unit vector every text embeds to — content is irrelevant here, only
/// *which packages have been indexed* is under test.
const DIMS: usize = 3;

struct GateEmbedder {
    /// How many `Document`-role calls have been made so far. The first
    /// (the first package loaded) returns immediately; the second (and any
    /// later, though only two packages exist here) waits on `gate`.
    document_calls: AtomicUsize,
    gate: tokio::sync::watch::Receiver<bool>,
}

impl GateEmbedder {
    fn new() -> (Arc<Self>, tokio::sync::watch::Sender<bool>) {
        let (tx, rx) = tokio::sync::watch::channel(false);
        (
            Arc::new(Self {
                document_calls: AtomicUsize::new(0),
                gate: rx,
            }),
            tx,
        )
    }
}

impl Embedder for GateEmbedder {
    fn info(&self) -> EmbedderInfo {
        EmbedderInfo {
            model_id: SharedStr::from("test-gate-embedder"),
            dimensions: DIMS,
            // Huge on purpose: every package's (single) document must land in
            // one `embed_batch` call, so the call counter maps 1:1 onto
            // packages regardless of how many symbols a package has.
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
            if role == EmbedRole::Document {
                let call = self.document_calls.fetch_add(1, Ordering::SeqCst);
                if call >= 1 {
                    // The second (and any later) package's Document call waits
                    // for the test to open the gate. `watch::Receiver::borrow`
                    // always reflects the latest value, so a gate opened
                    // before this line is observed immediately — no
                    // missed-wakeup window.
                    let mut gate = self.gate.clone();
                    while !*gate.borrow() {
                        if gate.changed().await.is_err() {
                            break;
                        }
                    }
                }
            }
            // One fixed unit vector per input. Every candidate that has made
            // it into the index therefore scores identically (cosine 1
            // against itself), so this file's assertions are entirely about
            // *which packages are in the index*, never about ranking quality
            // — that is `engine_relevance.rs`'s job, against the real model.
            let vector = {
                let mut v = vec![0.0f32; DIMS];
                v[0] = 1.0;
                v
            };
            Ok(texts.iter().map(|_| vector.clone()).collect())
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
            Ok(Ok(_)) => {},
            Ok(Err(_)) => break,
            Err(elapsed) => panic!("only {seen}/{n} packages loaded within 5s: {elapsed:?}"),
        }
    }
    assert_eq!(seen, n, "expected exactly {n} Loaded events");
}

/// One search, returning section 2's state and display names.
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

/// Poll `observe_semantic` until `matches` is satisfied or 5s elapse.
async fn poll_until(
    engine: &nudox_engine::EngineHandle,
    gens: &mut Gen,
    matches: impl Fn(&SectionState) -> bool,
) -> (SectionState, Vec<String>) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        gens.0 += 1;
        let (state, names) = observe_semantic(engine, *gens).await;
        if let Some(state) = &state
            && matches(state)
        {
            return (state.clone(), names);
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "condition never satisfied within 5s; last state: {state:?}, rows: {names:?}"
        );
        tokio::time::sleep(Duration::from_millis(15)).await;
    }
}

// ---------------------------------------------------------------------------
// The test
// ---------------------------------------------------------------------------

/// While the second of two packages is still embedding, section 2 reports
/// `Building { covered: 1, total: 2 }` and its rows come **only** from the
/// package that is actually indexed — never a row that looks like it came
/// from the package still being embedded. Once the gate opens and the second
/// package finishes, the section reports `Complete` and both packages'
/// symbols are reachable.
///
/// This is the local-mode behaviour the GUI's `SectionStatus::Building`
/// (`workspace/gui/src/stores/search.rs`) is fed from — the counts a reader
/// sees ("3 of 20 packages") are not decorative, they are the honest
/// difference between "the whole answer" and "the answer so far".
#[tokio::test]
async fn semantic_coverage_is_reported_honestly_while_building_and_completes_once_indexed() {
    let first_lineage = lineage("alpha");
    let second_lineage = lineage("beta");
    let first_pkg = build_package(&first_lineage, "Alpha");
    let second_pkg = build_package(&second_lineage, "Beta");

    let (embedder, gate) = GateEmbedder::new();
    let engine = Engine::start(
        EngineConfig {
            embedder: Some(embedder as Arc<dyn Embedder>),
            ..EngineConfig::default()
        },
        TwoPackageSource {
            first: (first_lineage.clone(), first_pkg),
            second: (second_lineage.clone(), second_pkg),
        },
    );

    // Both packages are resident (searchable by name/type) before either is
    // necessarily embedded — the whole reason `SectionState` exists.
    wait_for_n_loaded(&engine, 2).await;

    let mut gens = Gen(100);

    // --- Building: alpha covered, beta gated -------------------------------
    let (state, names) = poll_until(&engine, &mut gens, |s| {
        matches!(
            s,
            SectionState::Building {
                covered: 1,
                total: 2
            }
        )
    })
    .await;
    assert_eq!(
        state,
        SectionState::Building {
            covered: 1,
            total: 2
        }
    );
    assert!(
        names.iter().any(|n| n == "Alpha"),
        "the covered package's symbol must be findable while building; rows: {names:?}"
    );
    assert!(
        !names.iter().any(|n| n == "Beta"),
        "the package still being embedded must not appear in results — that would be a \
         fabricated row for a package `SectionState` itself says is not covered yet; \
         rows: {names:?}"
    );

    // --- Release the gate, wait for the second package to finish -----------
    gate.send(true)
        .expect("the embedder task must still be running");

    let (state, names) =
        poll_until(&engine, &mut gens, |s| matches!(s, SectionState::Complete)).await;
    assert_eq!(state, SectionState::Complete);
    assert!(
        names.iter().any(|n| n == "Alpha") && names.iter().any(|n| n == "Beta"),
        "once both packages are indexed, both must be reachable; rows: {names:?}"
    );
}
