//! `crate::runtime::semantic_indexer` only embeds what changed.
//!
//! # Why this file exists
//!
//! `semantic_local_mode.rs` and `semantic_failure_modes.rs` prove the
//! coverage/failure contract of the indexer; neither says anything about
//! *how much work* a re-index does. Before this file, a reload of an
//! unchanged package re-embedded every public symbol from scratch — the
//! model call count scaled with the package's whole documented surface on
//! every load event, not with what actually changed. This file is the
//! enforcement for the fix: a content hash travels alongside each vector
//! (`crate::semantic::SemanticIndex::snapshot_hashes`/`apply_delta`), and
//! `semantic_indexer` diffs against it before calling the embedder at all.
//!
//! # How re-indexing is driven without a real producer
//!
//! [`ReloadSource`] replays `Discovered`/`Ready` pairs off a channel the test
//! holds the sender for, all under one lineage. `VersionRegistry::record`'s
//! own rule ("two `PackageView`s claiming to be the same version are one
//! generation produced twice, not two") means even a same-version resend
//! always reaches `semantic_indexer` — which is exactly the scenario the
//! no-op test needs: the corpus repoints every time, so an unchanged embed
//! count can only be explained by the diff, never by the resend being
//! dropped upstream.

use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures::stream::BoxStream;
use nudox_engine::store::package::{PackageView, Provenance};
use nudox_engine::store::source::{
    Error as SourceError, IrSource, LoadEvent, LoadRequest, PackageHint, SourceDescriptor,
};
use nudox_engine::wire::{Gen, SearchEvent};
use nudox_engine::{
    EmbedError, EmbedRole, Embedder, EmbedderInfo, Engine, EngineConfig, EngineHandle,
    PackageLoadEvent, SearchQuery, SharedStr,
};
use nudox_ir::apply::PristineIntroTable;
use nudox_ir::change::{EcosystemId, IntroId, PackageLineageId, PackageName};
use nudox_ir::entry::{Entry, Node, Symbol, Visibility};
use nudox_ir::index::RawRef;
use nudox_ir::kind::Kind;
use nudox_ir::kinds::Module;
use nudox_ir::view::IrView;

const DIMS: usize = 3;

// ---------------------------------------------------------------------------
// Fixture packages
// ---------------------------------------------------------------------------

fn lineage(name: &str) -> PackageLineageId {
    PackageLineageId::new(EcosystemId::new("test-semantic-incremental"), PackageName::new(name))
}

fn intro(n: u8) -> IntroId {
    IntroId::from_raw([n; 32])
}

fn public_module_entry(name: &str, documentation: &str) -> Entry {
    let sym = Symbol {
        name: name.to_owned(),
        visibility: Visibility::Public,
        documentation: documentation.to_owned(),
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

/// Build one generation of `lid` with one public entry per `(byte, name, doc)`.
fn build_package(lid: &PackageLineageId, symbols: &[(u8, &str, &str)]) -> Arc<PackageView> {
    let mut table = PristineIntroTable::new();
    for (byte, name, doc) in symbols {
        table.insert_live(intro(*byte), public_module_entry(name, doc), None);
    }
    let view = IrView::with_package(lid.clone(), table);
    Arc::new(PackageView::build(view, Provenance::TrustedLocal))
}

// ---------------------------------------------------------------------------
// ReloadSource: replays Discovered+Ready pairs off a test-controlled channel
// ---------------------------------------------------------------------------

/// Feeds `Engine::start` an open-ended stream of generations, all reported at
/// the fixed version `"0.0.1"` — the version does not vary across a reload in
/// these tests because what's under test is content diffing, not version
/// arbitration (`versions_adversarial.rs` owns that).
struct ReloadSource {
    rx: flume::Receiver<Arc<PackageView>>,
}

impl IrSource for ReloadSource {
    fn describe(&self) -> SourceDescriptor {
        SourceDescriptor {
            label: "semantic-incremental-fixture".to_owned(),
            package_count_hint: None,
        }
    }

    fn load(&self, _req: LoadRequest) -> BoxStream<'static, Result<LoadEvent, SourceError>> {
        use futures::StreamExt as _;
        futures::stream::unfold(self.rx.clone(), |rx| async move {
            let package = rx.recv_async().await.ok()?;
            let lineage = package.lineage().clone();
            let events = [
                Ok(LoadEvent::Discovered {
                    lineage: lineage.clone(),
                    hint: PackageHint {
                        display_name: lineage.name.as_str().to_owned(),
                        ecosystem: lineage.ecosystem.as_str().to_owned(),
                        version: Some("0.0.1".to_owned()),
                    },
                }),
                Ok(LoadEvent::Ready { package }),
            ];
            Some((futures::stream::iter(events), rx))
        })
        .flatten()
        .boxed()
    }
}

// ---------------------------------------------------------------------------
// CountingEmbedder: records exactly which texts each Document call sent
// ---------------------------------------------------------------------------

/// A real [`Embedder`] whose vector is a deterministic function of the input
/// text (so two calls embedding the same text are indistinguishable by
/// output, which is the point — everything this file asserts is about *call
/// arguments*, never about vector values standing in for "was this
/// re-embedded").
struct CountingEmbedder {
    /// One entry per `Document`-role `embed_batch` call, in order.
    calls: Mutex<Vec<Vec<String>>>,
    queries: AtomicUsize,
}

impl CountingEmbedder {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            calls: Mutex::new(Vec::new()),
            queries: AtomicUsize::new(0),
        })
    }

    fn document_call_count(&self) -> usize {
        self.calls.lock().expect("lock poisoned").len()
    }

    /// The texts sent in call number `n` (0-indexed), or `None` if that call
    /// has not happened (yet).
    fn call_texts(&self, n: usize) -> Option<Vec<String>> {
        self.calls.lock().expect("lock poisoned").get(n).cloned()
    }
}

fn deterministic_vector(text: &str) -> Vec<f32> {
    let hash = heart::ContentHash::of_bytes(text.as_bytes());
    let bytes = hash.as_bytes();
    (0..DIMS)
        .map(|i| f32::from(bytes[i]) / 255.0 + 0.01)
        .collect()
}

impl Embedder for CountingEmbedder {
    fn info(&self) -> EmbedderInfo {
        EmbedderInfo {
            model_id: SharedStr::from("test-counting-embedder"),
            dimensions: DIMS,
            // Large enough that every package's whole document set lands in
            // one `embed_batch` call, so "how many calls" tracks "how many
            // reload events actually embedded something", not batching noise.
            max_batch: 1_000_000,
            durable_canonical: true,
        }
    }

    fn embed_batch<'a>(
        &'a self,
        texts: &'a [String],
        role: EmbedRole,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<Vec<f32>>, EmbedError>> + Send + 'a>> {
        Box::pin(async move {
            if role == EmbedRole::Document {
                self.calls
                    .lock()
                    .expect("lock poisoned")
                    .push(texts.to_vec());
            } else {
                self.queries.fetch_add(1, Ordering::SeqCst);
            }
            Ok(texts.iter().map(|t| deterministic_vector(t)).collect())
        })
    }
}

// ---------------------------------------------------------------------------
// Harness
// ---------------------------------------------------------------------------

async fn wait_for_n_loaded(engine: &EngineHandle, n: usize) {
    let rx = engine.packages();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    let mut seen = 0;
    while seen < n {
        match tokio::time::timeout_at(deadline, rx.recv_async()).await {
            Ok(Ok(PackageLoadEvent::Loaded { .. })) => seen += 1,
            Ok(Ok(_)) => {}
            Ok(Err(_)) => break,
            Err(elapsed) => panic!("only {seen}/{n} packages loaded within 5s: {elapsed:?}"),
        }
    }
    assert_eq!(seen, n, "expected exactly {n} Loaded events");
}

/// Poll `predicate` until it's true or 5s elapse.
async fn wait_until(predicate: impl Fn() -> bool, message: &str) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    while !predicate() {
        assert!(tokio::time::Instant::now() < deadline, "{message}");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

/// The display names section 2 (semantic) returns for `text`.
async fn semantic_names(engine: &EngineHandle, generation: Gen, text: &str) -> Vec<String> {
    let (_stream, rx) = engine.search(
        SearchQuery {
            text: text.to_owned(),
            ..Default::default()
        },
        generation,
    );
    let mut names = Vec::new();
    while let Ok(event) = rx.recv_async().await {
        match event {
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
    names
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

/// A reload with byte-identical content — including a resend at the *same*
/// version, the case `VersionRegistry::record` always repoints on — must not
/// call the embedder again.
#[tokio::test]
async fn identical_reload_never_reembeds() {
    let lid = lineage("noop");
    let symbols = [(1, "Alpha", "alpha doc"), (2, "Beta", "beta doc")];

    let embedder = CountingEmbedder::new();
    let (tx, rx) = flume::unbounded();
    let engine = Engine::start(
        EngineConfig {
            embedder: Some(embedder.clone() as Arc<dyn Embedder>),
            ..EngineConfig::default()
        },
        ReloadSource { rx },
    );

    tx.send(build_package(&lid, &symbols)).unwrap();
    wait_for_n_loaded(&engine, 1).await;
    wait_until(
        || embedder.document_call_count() == 1,
        "first load never embedded",
    )
    .await;
    wait_until(
        || engine.semantic_coverage() == 1,
        "first load never became covered",
    )
    .await;

    // Same lineage, same version, same content, freshly built — "produced
    // twice", not a new generation.
    tx.send(build_package(&lid, &symbols)).unwrap();
    wait_for_n_loaded(&engine, 2).await;

    // No event this diff produces is externally observable on a no-op, so
    // there is nothing to poll *for* — settle, then assert nothing moved.
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(
        embedder.document_call_count(),
        1,
        "an identical reload must not call the embedder again"
    );
}

/// A reload where exactly one of several symbols' documentation changed must
/// embed exactly that one symbol — not the package's whole public surface —
/// and the unchanged symbols must remain searchable throughout.
#[tokio::test]
async fn one_changed_symbol_reembeds_only_that_symbol() {
    let lid = lineage("one-change");
    let before = [
        (1, "Alpha", "alpha doc"),
        (2, "Beta", "beta doc original"),
        (3, "Gamma", "gamma doc"),
    ];
    let after = [
        (1, "Alpha", "alpha doc"),
        (2, "Beta", "beta doc REVISED"),
        (3, "Gamma", "gamma doc"),
    ];

    let embedder = CountingEmbedder::new();
    let (tx, rx) = flume::unbounded();
    let engine = Engine::start(
        EngineConfig {
            embedder: Some(embedder.clone() as Arc<dyn Embedder>),
            ..EngineConfig::default()
        },
        ReloadSource { rx },
    );

    tx.send(build_package(&lid, &before)).unwrap();
    wait_for_n_loaded(&engine, 1).await;
    wait_until(
        || embedder.document_call_count() == 1,
        "first load never embedded",
    )
    .await;
    assert_eq!(
        embedder.call_texts(0).unwrap().len(),
        3,
        "the first load must embed every symbol — there is no prior state to diff against"
    );

    tx.send(build_package(&lid, &after)).unwrap();
    wait_for_n_loaded(&engine, 2).await;
    wait_until(
        || embedder.document_call_count() == 2,
        "the changed symbol was never embedded",
    )
    .await;

    let second_call = embedder.call_texts(1).unwrap();
    assert_eq!(
        second_call.len(),
        1,
        "only the one changed symbol may reach the embedder, got {second_call:?}"
    );
    assert!(
        second_call[0].contains("beta doc REVISED"),
        "the embedded text must be Beta's new documentation, got {second_call:?}"
    );

    // Settle, then confirm no further (redundant) calls follow.
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(embedder.document_call_count(), 2);

    let names = semantic_names(&engine, Gen(1), "doc").await;
    assert!(names.contains(&"Alpha".to_owned()));
    assert!(names.contains(&"Beta".to_owned()));
    assert!(names.contains(&"Gamma".to_owned()));
}

/// Removing a symbol must drop it from the index without re-embedding
/// whatever remains.
#[tokio::test]
async fn removed_symbol_drops_without_reembedding_rest() {
    let lid = lineage("removal");
    let before = [
        (1, "Alpha", "alpha doc"),
        (2, "Beta", "beta doc"),
        (3, "Gamma", "gamma doc"),
    ];
    let after = [(1, "Alpha", "alpha doc"), (3, "Gamma", "gamma doc")];

    let embedder = CountingEmbedder::new();
    let (tx, rx) = flume::unbounded();
    let engine = Engine::start(
        EngineConfig {
            embedder: Some(embedder.clone() as Arc<dyn Embedder>),
            ..EngineConfig::default()
        },
        ReloadSource { rx },
    );

    tx.send(build_package(&lid, &before)).unwrap();
    wait_for_n_loaded(&engine, 1).await;
    wait_until(
        || embedder.document_call_count() == 1,
        "first load never embedded",
    )
    .await;

    tx.send(build_package(&lid, &after)).unwrap();
    wait_for_n_loaded(&engine, 2).await;

    // A pure removal produces no embed call at all — poll for the corpus
    // having repointed (a second `Loaded`, already awaited above) and the
    // package still covered, then settle before checking the call count.
    wait_until(
        || engine.semantic_coverage() == 1,
        "the package must remain covered after a removal-only diff",
    )
    .await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(
        embedder.document_call_count(),
        1,
        "removing a symbol must not re-embed the symbols that remain"
    );

    let names = semantic_names(&engine, Gen(1), "doc").await;
    assert!(names.contains(&"Alpha".to_owned()));
    assert!(names.contains(&"Gamma".to_owned()));
    assert!(
        !names.contains(&"Beta".to_owned()),
        "a removed symbol must not still be searchable, got {names:?}"
    );
}

/// The content hash that gates re-embedding must itself survive a restart —
/// otherwise every process start would pay for a full re-embed regardless of
/// what changed, defeating the point of persisting vectors at all.
#[tokio::test(flavor = "multi_thread")]
async fn restart_then_edit_only_reembeds_changed_symbol() {
    let state = tempfile::tempdir().expect("state dir");
    let index_file = state.path().join("semantic-index.json");
    let lid = lineage("restart");
    let before = [
        (1, "Alpha", "alpha doc"),
        (2, "Beta", "beta doc original"),
        (3, "Gamma", "gamma doc"),
    ];

    {
        let embedder = CountingEmbedder::new();
        let (tx, rx) = flume::unbounded();
        let engine = Engine::start(
            EngineConfig {
                embedder: Some(embedder.clone() as Arc<dyn Embedder>),
                semantic_index_dir: Some(state.path().to_owned()),
                ..EngineConfig::default()
            },
            ReloadSource { rx },
        );
        tx.send(build_package(&lid, &before)).unwrap();
        wait_for_n_loaded(&engine, 1).await;
        wait_until(
            || index_file.is_file(),
            "the initial index was never persisted",
        )
        .await;
    }

    let after = [
        (1, "Alpha", "alpha doc"),
        (2, "Beta", "beta doc REVISED"),
        (3, "Gamma", "gamma doc"),
    ];
    let embedder = CountingEmbedder::new();
    let (tx, rx) = flume::unbounded();
    let engine = Engine::start(
        EngineConfig {
            embedder: Some(embedder.clone() as Arc<dyn Embedder>),
            semantic_index_dir: Some(state.path().to_owned()),
            ..EngineConfig::default()
        },
        ReloadSource { rx },
    );
    tx.send(build_package(&lid, &after)).unwrap();
    wait_for_n_loaded(&engine, 1).await;
    wait_until(
        || embedder.document_call_count() == 1,
        "the changed symbol was never embedded after restart",
    )
    .await;

    let call = embedder.call_texts(0).unwrap();
    assert_eq!(
        call.len(),
        1,
        "only Beta should reach the embedder after restart — Alpha and \
         Gamma's hashes must have survived the JSON round trip, got {call:?}"
    );
    assert!(call[0].contains("beta doc REVISED"), "got {call:?}");

    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(embedder.document_call_count(), 1);
}
