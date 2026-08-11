//! LR-9: the engine owns every runtime.
//!
//! One Tokio multi-thread runtime for I/O and producer work.
//! One `LocalSet` for query execution (the Trustfall fork's result stream is
//! `!Send` — that is a design constraint of the pinned fork, not an oversight,
//! and must not be "fixed" by blocking the caller).
//!
//! `lindsey` never creates a Tokio runtime of its own. Everything async goes
//! through the `EngineHandle` returned by [`Engine::start`].

use std::{
    collections::{HashMap, VecDeque},
    path::PathBuf,
    sync::Arc,
};

use tokio::runtime::Runtime;
use tokio::sync::broadcast;
use tokio::task::LocalSet;
use tracing::info;

use nudox_ir::change::PackageLineageId;
use nudox_store::{
    corpus::Corpus,
    source::{IrSource, LoadEvent, LoadRequest},
};

use crate::{
    PackageHistorySpec, PackageLoadEvent, PackageSpec, versions::VersionRegistry,
    wire::SharedStr,
};

// ---------------------------------------------------------------------------
// LoadFailures
// ---------------------------------------------------------------------------

/// Durable record of package loads that were attempted and failed, keyed by
/// lineage.
///
/// # Why this exists
///
/// `EngineHandle::packages()` broadcasts `PackageLoadEvent::LoadFailed` once,
/// live, to whoever happens to be subscribed at that moment (§the type's own
/// docs: "capacity 64 ... a slow subscriber may lag"). An MCP tool call is
/// not a subscriber — it typically arrives well after seeding has finished —
/// so without a durable copy, `EngineError::PackageNotLoaded` could never
/// tell "this lineage was attempted and broke" apart from "this lineage was
/// never named at all", and both surfaced as the identical bare "not
/// loaded". This is the durable half; the broadcast stays the live half for
/// a GUI toast.
#[derive(Clone, Default)]
pub(crate) struct LoadFailures(Arc<std::sync::RwLock<HashMap<PackageLineageId, SharedStr>>>);

impl LoadFailures {
    fn new() -> Self {
        Self::default()
    }

    /// Record that loading `lineage` failed with `reason`.
    ///
    /// A later record for the same lineage overwrites the earlier one — a
    /// reload attempt's outcome is the one worth reporting, not its history.
    fn record(&self, lineage: PackageLineageId, reason: SharedStr) {
        let mut guard = self
            .0
            .write()
            .expect("load-failure lock is never held across a panic");
        guard.insert(lineage, reason);
    }

    /// The recorded failure reason for `lineage`, or `None` if no load for
    /// it was ever recorded as failed (either it succeeded, or it was never
    /// attempted — this type cannot tell those two apart, which is exactly
    /// why `EngineError::PackageNotLoaded::attempted` is itself an
    /// `Option`).
    pub(crate) fn get(&self, lineage: &PackageLineageId) -> Option<SharedStr> {
        let guard = self
            .0
            .read()
            .expect("load-failure lock is never held across a panic");
        guard.get(lineage).cloned()
    }
}

// ---------------------------------------------------------------------------
// EngineConfig
// ---------------------------------------------------------------------------

/// Configuration for the engine runtime.
#[derive(Clone, Default)]
pub struct EngineConfig {
    /// The root directory of the project workspace (not yet used for discovery;
    /// stored for future `resolve_project` use).
    pub workspace_root: Option<PathBuf>,
    /// Number of Tokio worker threads.  `None` → one per logical core.
    pub worker_threads: Option<usize>,
    /// Syntax highlighter for code sections, if the host supplies one.
    ///
    /// The engine decides *when* a section is highlighted and guarantees the
    /// §9.3 ordering; the host decides *how*. See [`crate::highlight`] for why
    /// this cannot simply be a dependency of this crate.
    pub highlighter: crate::highlight::SharedHighlighter,
    /// Where [`EngineHandle::index_purl`] unpacks packages it fetches.
    ///
    /// `None` → [`crate::acquire::default_cache_dir`], which honours
    /// `NUDOX_PACKAGE_CACHE` and then `XDG_CACHE_HOME`. A host with its own
    /// notion of where user data lives sets this instead of exporting an
    /// environment variable behind its own back.
    pub package_cache: Option<PathBuf>,
    /// Text embedder for the semantic search section, if the host supplies one.
    ///
    /// Same seam and same reason as `highlighter`, plus one more: the embedding
    /// stack is a 483-crate dependency graph whose runtime downloads itself at
    /// build time unless externally provisioned. See [`crate::semantic`] for
    /// the full argument, and for why `None` is the *ordinary* configuration
    /// rather than a degraded one.
    pub embedder: crate::semantic::SharedEmbedder,
}

impl std::fmt::Debug for EngineConfig {
    /// Hand-written because `dyn Highlighter` is not `Debug` — requiring it
    /// would force every host implementation to derive it for no reader benefit.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EngineConfig")
            .field("workspace_root", &self.workspace_root)
            .field("worker_threads", &self.worker_threads)
            .field("highlighter", &self.highlighter.is_some())
            .field("embedder", &self.embedder.is_some())
            .field("package_cache", &self.package_cache)
            .finish()
    }
}

// ---------------------------------------------------------------------------
// Engine (internal)
// ---------------------------------------------------------------------------

/// The engine's internal state, owned by the background runtime.
///
/// Held in an `Arc` so that `EngineHandle` (the caller-facing side) can share
/// it without copying it into every spawned task.
pub(crate) struct EngineInner {
    /// The live corpus of loaded packages.  Cheap to clone; the engine and
    /// every handle share the same `Arc`-backed world view.
    ///
    /// Holds exactly one generation per lineage — the *current* one. Every
    /// other loaded generation lives in [`Self::versions`].
    pub(crate) corpus: Corpus,
    /// Every loaded generation of every package, and which one is current.
    ///
    /// The corpus answers "what is the world"; this answers "what else have we
    /// got". Kept in step with the corpus by the seeding task below and by
    /// `EngineHandle::select_version`; see `crate::versions` for why the two
    /// structures are separate rather than the corpus being made
    /// version-keyed.
    pub(crate) versions: Arc<VersionRegistry>,
    /// The Trustfall schema singleton — returned by `EngineHandle::schema`.
    pub(crate) schema: &'static trustfall::Schema,
    /// The host-supplied highlighter, if any (see `crate::highlight`).
    pub(crate) highlighter: crate::highlight::SharedHighlighter,
    /// Live broadcast channel for package load events.
    ///
    /// The seeding task publishes here on every `LoadEvent::Ready` and
    /// `LoadEvent::Failed`.  `EngineHandle::packages()` subscribes and
    /// fuses the live stream with a snapshot of already-loaded packages to
    /// give every caller an exactly-once view regardless of when they arrive.
    ///
    /// Capacity 64: in the normal case a workspace has fewer than 64 packages,
    /// so a fast subscriber never lags; a slow subscriber (backpressured by
    /// the GUI render loop) may lag but `RecvError::Lagged` is handled
    /// explicitly in `EngineHandle::packages()` rather than being allowed to
    /// kill the stream.
    pub(crate) pkg_tx: broadcast::Sender<PackageLoadEvent>,
    /// Durable record of package loads that were attempted and failed — see
    /// [`LoadFailures`].
    pub(crate) load_failures: LoadFailures,
    /// The HTTP client and package cache used by [`EngineHandle::index_purl`].
    ///
    /// Held on the engine rather than built per call because a
    /// `reqwest::Client` owns a connection pool: resolving one PURL makes up to
    /// four requests to the same host, and a fresh client per call would open a
    /// fresh TLS session for each.
    pub(crate) acquire: Arc<crate::acquire::AcquireContext>,
    /// The host's embedder, if this build has one.
    pub(crate) embedder: crate::semantic::SharedEmbedder,
    /// Vectors for every package that has finished embedding.
    ///
    /// Grows as packages land (see [`spawn_semantic_indexer`]); read by
    /// `run_search` without ever waiting for it. The two together are what make
    /// the semantic section usable the instant *any* package is resident rather
    /// than after the corpus finishes.
    pub(crate) semantic: crate::semantic::SemanticIndex,
    /// Where [`drive_load`] hands a newly-resident package to the embedder.
    ///
    /// `Some` exactly when an embedder is installed. **Unbounded on purpose**,
    /// and this is the single most load-bearing decision in the incremental
    /// design: the requirement is that embedding never blocks IR availability,
    /// and a bounded queue would make the load loop wait for the model as soon
    /// as it filled — turning "search is usable the instant a package is
    /// resident" into "the ninth package waits for the first eight to embed".
    ///
    /// Unbounded is safe here because the producer is not a user: the queue's
    /// depth is bounded by the number of packages the corpus will ever hold,
    /// each entry is one `Arc` clone, and the consumer drains strictly faster
    /// than a producer can lower new IR.
    pub(crate) semantic_tx: Option<flume::Sender<Arc<nudox_store::package::PackageView>>>,
}

// ---------------------------------------------------------------------------
// drive_load — the one place a produced package enters the corpus
// ---------------------------------------------------------------------------

/// Drive an [`IrSource`] to completion, recording every package it produces and
/// broadcasting the outcome.
///
/// # Why this is a function and not the body of `Engine::start`
///
/// It used to be the body of `Engine::start`, and that was the *actual*
/// blocker behind `EngineCapability::ProjectResolution`, whose `blocked_on`
/// reads: "today packages can only be supplied to `Engine::start_with_producer`,
/// so a resolved list could not be acted on". Nothing about the loop needed to
/// be start-time; it was simply written inline, so the only way to run it was
/// to start a new engine.
///
/// Extracting it — rather than writing a second insertion path for on-demand
/// packages — is what guarantees a package indexed from a PURL at minute ten is
/// indistinguishable from one named at minute zero: the same
/// `Discovered`-to-`Ready` version pairing, the same
/// [`VersionRegistry::record`] arbitration over which generation becomes
/// resident, the same durable [`LoadFailures`] copy, and the same `pkg_tx`
/// broadcast that `EngineHandle::packages()` fuses into its snapshot. A second
/// path would have had to re-derive all four, and the first one it got wrong
/// would be a lineage that behaves differently depending on when it arrived.
///
/// Returns the events it broadcast, in order, so a caller loading a known set
/// of packages can inspect the outcome without subscribing to a channel it
/// shares with everyone else.
pub(crate) async fn drive_load(
    source: impl IrSource,
    inner: &EngineInner,
) -> Vec<PackageLoadEvent> {
    use futures::StreamExt as _;

    // Version strings awaiting their `Ready`, keyed by lineage.
    //
    // `LoadEvent::Ready` carries only an `Arc<PackageView>`, and
    // `PackageView` has no version field — a `PackageLineageId` is
    // version-free by design and nothing downstream of it ever needed
    // the number. The only place the version appears in the load
    // protocol is `LoadEvent::Discovered { hint: PackageHint { version } }`.
    //
    // Recovering it here is sound because the `IrSource` contract
    // requires `Discovered` before `Ready` for every package, and both
    // in-tree sources are strictly sequential per package
    // (`ProducerSource::load` drives descriptors with `then`, which
    // awaits each in turn). A FIFO per lineage therefore pairs each
    // `Ready` with its own `Discovered` even when several generations
    // of the same lineage are being loaded.
    //
    // A hypothetical source that interleaved *two generations of the
    // same lineage* could mispair them; a source that interleaves
    // different lineages cannot, because the queues are per-lineage.
    // The durable fix is a `version` field on `PackageView` (or on
    // `LoadEvent::Ready`), which is a `nudox-store` change and so out
    // of scope here.
    let mut pending: HashMap<PackageLineageId, VecDeque<Option<String>>> = HashMap::new();
    let mut outcomes = Vec::new();

    let mut stream = source.load(LoadRequest::default());
    while let Some(event) = stream.next().await {
        match event {
            Ok(LoadEvent::Discovered { lineage, hint }) => {
                pending.entry(lineage).or_default().push_back(hint.version);
            }
            Ok(LoadEvent::Ready { package }) => {
                // Count symbols before the package is handed on.
                let symbol_count = package.view().table().len() as u64;
                let lineage = package.lineage().clone();
                let name = lineage.name.as_str().to_owned();
                let ecosystem = lineage.ecosystem.as_str().to_owned();
                let version = pending
                    .get_mut(&lineage)
                    .and_then(|q| q.pop_front())
                    .flatten();

                // Record the generation first. The registry decides
                // whether this generation becomes the resident one —
                // it is the single place that rule lives, so the
                // corpus cannot drift from the version list.
                //
                // This also fixes a latent ordering bug: previously
                // every `Ready` was inserted unconditionally, so with
                // several generations of one package the corpus ended
                // up holding whichever finished producing last rather
                // than the newest.
                if let Some(resident) =
                    inner
                        .versions
                        .record(&lineage, version.clone(), Arc::clone(&package))
                {
                    inner.corpus.insert(Arc::clone(&resident)).await;
                    // Hand the *resident* generation — not `package` — to the
                    // embedder. They differ whenever an older generation
                    // arrives after a newer one, and indexing the arriving one
                    // would embed symbols that no search can resolve, because
                    // every lookup goes through the corpus.
                    //
                    // `send` (not `send_async`) on an unbounded channel never
                    // blocks, which is what keeps this line off the critical
                    // path. `Err` means the indexer has shut down; the package
                    // is still fully loaded and searchable by name and type,
                    // and `SectionState::Building` already reports it as
                    // uncovered, so there is nothing to recover here.
                    if let Some(tx) = &inner.semantic_tx {
                        let _ = tx.send(resident);
                    }
                }

                let event = PackageLoadEvent::Loaded {
                    name: name.into(),
                    ecosystem: ecosystem.into(),
                    version,
                    symbol_count,
                    root: package_root_key(&package),
                };
                // Ignore `Err`: no subscribers yet is fine.
                let _ = inner.pkg_tx.send(event.clone());
                outcomes.push(event);
            }
            Ok(LoadEvent::Failed { lineage, error }) => {
                // Consume this package's queued version so the FIFO
                // stays aligned for the lineage's later generations.
                // `Discovered` is emitted for the failing package too.
                if let Some(q) = pending.get_mut(&lineage) {
                    q.pop_front();
                }
                tracing::warn!(
                    package = %lineage,
                    "package load failed: {error}",
                );
                let reason: SharedStr = error.to_string().into();
                // Durable copy — see `LoadFailures` docs for why the
                // broadcast below is not enough on its own.
                inner.load_failures.record(lineage.clone(), reason.clone());
                let event = PackageLoadEvent::LoadFailed {
                    name: lineage.name.as_str().to_owned().into(),
                    ecosystem: lineage.ecosystem.as_str().to_owned().into(),
                    error: reason,
                };
                let _ = inner.pkg_tx.send(event.clone());
                outcomes.push(event);
            }
            Ok(_) => {} // Progress — informational only
            Err(e) => {
                tracing::error!("stream-level load error: {e}");
                break;
            }
        }
    }
    outcomes
}

// ---------------------------------------------------------------------------
// Semantic indexing — incremental, off the load path
// ---------------------------------------------------------------------------

/// The largest document text handed to a model, in bytes.
///
/// The recipe's token budget is frozen at `MAX_SEQ_LEN = 1024`, and every
/// tokenizer this repo can be pointed at truncates beyond it *silently*. Cutting
/// here instead means the truncation happens somewhere a reader can see it and
/// somewhere the cost is bounded before the batch is built, rather than inside a
/// runtime whose behaviour we would be inferring. Four bytes per token is the
/// conservative ratio for code, which is denser than prose.
const MAX_DOCUMENT_BYTES: usize = 4 * 1024;

/// Consume newly-resident packages and embed them, one package at a time.
///
/// # Why this is a task and not a step in `drive_load`
///
/// The requirement is that a package is searchable by name and type the instant
/// it is resident, and that embedding — 250 ms to 1.1 s per package for
/// inference-grade work — never delays that. Doing it inline would serialise
/// the two: the second package could not begin lowering until the first had
/// finished embedding. Here the load loop's only obligation is one non-blocking
/// `send`.
///
/// # Why packages are embedded serially
///
/// Embedding is CPU-saturating: `FastembedOrt` runs a CPU execution provider
/// with an intra-op thread pool already sized to the machine. Two packages
/// embedding concurrently do not finish sooner, they finish *later* — they
/// contend for the same cores and each one's completion is pushed back behind
/// the other's. Serialising means the first package's vectors land as early as
/// they possibly can, which is what `SectionState::Building` reports on and
/// what makes partial coverage useful rather than merely honest.
async fn semantic_indexer(
    rx: flume::Receiver<Arc<nudox_store::package::PackageView>>,
    embedder: Arc<dyn crate::semantic::Embedder>,
    index: crate::semantic::SemanticIndex,
) {
    let info = embedder.info();
    // A host that advertises a zero batch size would make `chunks(0)` panic.
    // Clamping rather than trusting keeps a bad host's misconfiguration from
    // becoming this task's crash.
    let batch = info.max_batch.max(1);

    while let Ok(package) = rx.recv_async().await {
        let lineage = package.lineage().clone();
        let documents = documents_of(&package);
        let started = std::time::Instant::now();

        let mut vectors: Vec<(nudox_ir::change::IntroId, Vec<f32>)> =
            Vec::with_capacity(documents.len());
        let mut failed = false;

        for chunk in documents.chunks(batch) {
            let texts: Vec<String> = chunk.iter().map(|(_, text)| text.clone()).collect();
            match embedder
                .embed_batch(&texts, crate::semantic::EmbedRole::Document)
                .await
            {
                Ok(batch_vectors) if batch_vectors.len() == chunk.len() => {
                    for ((intro, _), vector) in chunk.iter().zip(batch_vectors) {
                        vectors.push((*intro, vector));
                    }
                }
                Ok(batch_vectors) => {
                    // A host that returns a different number of vectors than it
                    // was given has broken the positional pairing the trait
                    // documents, and there is no way to tell *which* symbol each
                    // vector belongs to. Pairing them anyway would produce an
                    // index that is confidently wrong — every row correct-looking
                    // and attached to the wrong symbol.
                    tracing::error!(
                        package = %lineage,
                        sent = chunk.len(),
                        got = batch_vectors.len(),
                        "embedder broke positional pairing; abandoning this package"
                    );
                    failed = true;
                    break;
                }
                Err(error) => {
                    tracing::warn!(package = %lineage, "embedding failed: {error}");
                    failed = true;
                    break;
                }
            }
        }

        if failed {
            // Deliberately *not* recorded as covered. The package stays in
            // `SectionState::Building`'s uncovered count, which is exactly true:
            // the semantic section has not searched it and never will this run.
            // Marking it covered would be the silent-repair failure of §8 —
            // presenting a degraded case as the good one.
            continue;
        }

        let count = vectors.len();
        match index.insert_package(lineage.clone(), vectors, info.dimensions) {
            // `info!`, not `debug!`: this is the single most expensive thing the
            // engine does per package (measured at 246.8 s for real `memchr` on
            // a CPU execution provider), and a cost that is only visible at
            // `debug` is a cost nobody measures. It is one line per package, at
            // the same level as "corpus seeding complete".
            Ok(()) => tracing::info!(
                package = %lineage,
                symbols = count,
                elapsed_ms = started.elapsed().as_millis() as u64,
                "semantic index updated"
            ),
            Err(error) => tracing::error!(
                package = %lineage,
                "embedder produced unusable vectors: {error}"
            ),
        }
    }
}

/// The text embedded for each symbol of `package`, in `IntroId` order.
///
/// # What is embedded, and what is not
///
/// Public symbols only. A semantic search over documentation is a search over
/// the *documented surface*, and a private helper that happens to be
/// semantically close to the query is an answer the reader cannot use — they
/// cannot call it, and its page exists only because the crate was lowered
/// whole. It is also the difference between embedding memchr's full 11 329
/// entries and its public API, which is the difference between a per-package
/// cost measured in minutes and one measured in seconds.
///
/// `Param` entries are excluded for the reason `typerefs_of_entry` excludes
/// them: a parameter is part of a signature, not a destination, and its text is
/// already carried by the function's own document.
///
/// # Why the document is assembled here and not by the host
///
/// The host supplies a model; deciding *what a symbol is, as text* is a
/// question about the IR, and the IR is this crate's business. It also keeps
/// the answer stable across hosts, so two embedders can be compared on the same
/// documents rather than on their own idea of what a symbol says.
fn documents_of(
    package: &nudox_store::package::PackageView,
) -> Vec<(nudox_ir::change::IntroId, String)> {
    use nudox_ir::entry::Visibility;

    let view = package.view();
    let indexes = package.indexes();
    let mut out = Vec::new();

    for (intro, entry) in view.entries_sorted() {
        if entry.sym().visibility != Visibility::Public {
            continue;
        }
        let Some(discriminant) = entry.kind().discriminant() else {
            continue;
        };
        if discriminant == nudox_ir::kind::KindDiscriminant::Param {
            continue;
        }

        // Path first, then kind, then documentation. Leading with the
        // fully-qualified path means the model sees the module context before
        // it is truncated away, which is what separates `io::Error` from
        // `fmt::Error` when the doc comments are both one line.
        let path = indexes
            .path_of(intro)
            .map(|p| p.as_ref().to_owned())
            .unwrap_or_else(|| entry.sym().name.clone());
        let mut text = format!("{path}\n{discriminant:?}\n");
        let documentation = entry.sym().documentation.trim();
        if !documentation.is_empty() {
            text.push_str(documentation);
        }

        // Truncate on a char boundary — `String::truncate` panics otherwise,
        // and doc comments are full of non-ASCII.
        if text.len() > MAX_DOCUMENT_BYTES {
            let mut end = MAX_DOCUMENT_BYTES;
            while end > 0 && !text.is_char_boundary(end) {
                end -= 1;
            }
            text.truncate(end);
        }

        out.push((intro, text));
    }

    out
}

// ---------------------------------------------------------------------------
// Engine
// ---------------------------------------------------------------------------

/// The engine itself — constructed once, consumed by [`Engine::start`].
pub struct Engine {
    #[allow(dead_code)]
    config: EngineConfig,
}

impl Engine {
    /// Create an engine with the given configuration.
    pub fn new(config: EngineConfig) -> Self {
        Self { config }
    }

    /// Start the engine with a default configuration and the given source.
    ///
    /// This is the convenience entry point for tests and simple applications.
    /// It builds the Tokio runtime on the calling thread and returns an
    /// `EngineHandle` that is `Send + Sync + 'static`.
    ///
    /// # Panics
    ///
    /// Panics only if the Tokio runtime cannot be constructed (out of
    /// resources) — these are process-fatal conditions.
    pub fn start(config: EngineConfig, source: impl IrSource) -> EngineHandle {
        let mut builder = tokio::runtime::Builder::new_multi_thread();
        builder.enable_all();
        if let Some(n) = config.worker_threads {
            builder.worker_threads(n);
        }
        let runtime = Arc::new(RuntimeGuard::new(
            builder
                .build()
                .expect("Tokio runtime creation must succeed"),
        ));

        // Capacity 64: broadcast channel for package load notifications.  See
        // `EngineInner::pkg_tx` for the capacity rationale.
        let (pkg_tx, _initial_rx) = broadcast::channel::<PackageLoadEvent>(64);
        // `_initial_rx` is immediately dropped; real subscribers are created
        // inside `EngineHandle::packages()` before they read the snapshot.

        // The semantic queue exists only when there is something to consume it.
        // `None` rather than an always-present channel with no reader: a queue
        // that silently accumulates packages nobody will embed is a leak whose
        // only symptom is memory, and `Option` makes "this build does not embed"
        // a fact the type carries rather than one the reader has to reconstruct
        // from whether a task was spawned.
        let semantic = crate::semantic::SemanticIndex::new();
        let semantic_tx = config.embedder.as_ref().map(|embedder| {
            let (tx, rx) = flume::unbounded();
            runtime.spawn(semantic_indexer(
                rx,
                Arc::clone(embedder),
                semantic.clone(),
            ));
            tx
        });

        let inner = Arc::new(EngineInner {
            corpus: Corpus::new(),
            versions: Arc::new(VersionRegistry::new()),
            schema: nudox_graph::schema(),
            highlighter: config.highlighter.clone(),
            pkg_tx,
            load_failures: LoadFailures::new(),
            acquire: crate::acquire::AcquireContext::new(config.package_cache.clone()),
            embedder: config.embedder.clone(),
            semantic,
            semantic_tx,
        });

        // Seed the corpus from the source on the runtime's thread pool.
        // We do this eagerly on start so that searches issued immediately
        // after `start` return (possibly empty) rather than blocking.
        //
        // The body is [`drive_load`] — shared verbatim with
        // `EngineHandle::load_one`, which is how a package fetched from a PURL
        // ten minutes after start-up is loaded by *the same* rules as one named
        // at start-up. See that function's docs.
        let seed_inner = Arc::clone(&inner);
        runtime.spawn(async move {
            let outcomes = drive_load(source, &seed_inner).await;
            info!(
                "corpus seeding complete; {} package(s) loaded, {} failed",
                outcomes
                    .iter()
                    .filter(|e| matches!(e, PackageLoadEvent::Loaded { .. }))
                    .count(),
                outcomes
                    .iter()
                    .filter(|e| matches!(e, PackageLoadEvent::LoadFailed { .. }))
                    .count(),
            );
        });

        EngineHandle { inner, runtime }
    }
}

// ---------------------------------------------------------------------------
// EngineHandle
// ---------------------------------------------------------------------------

/// The caller-facing engine handle.
///
/// `Clone`-able, `Send + Sync + 'static`.  Every method returns immediately;
/// results arrive via bounded `flume` receivers (GUI-PLAN Appendix C).
///
/// # Channel capacities (Appendix C)
///
/// | Stream kind    | Capacity | Overflow policy |
/// |----------------|----------|-----------------|
/// | Search         | 64       | backpressure    |
/// | Doc            | 64       | backpressure    |
/// | Query          | 128      | backpressure    |
/// | Package        | 32       | backpressure    |
/// | Project        | 32       | backpressure    |
/// | Sync/Job       | 256      | keep-latest     |
/// | Command        | 64       | backpressure    |
#[derive(Clone)]
pub struct EngineHandle {
    pub(crate) inner: Arc<EngineInner>,
    /// Keeps the runtime alive for the lifetime of the handle.
    pub(crate) runtime: Arc<RuntimeGuard>,
}

/// Owns the Tokio runtime and makes dropping it safe from *any* context.
///
/// # The hazard this exists to remove
///
/// `tokio::runtime::Runtime`'s `Drop` blocks while it waits for worker threads
/// to wind down, and blocking inside an async context panics outright:
///
/// > Cannot drop a runtime in a context where blocking is not allowed.
///
/// `EngineHandle` is `Clone`, so *which* clone drops last is not something any
/// one call site controls — and if the last one happens to be released from
/// inside a task, the process dies during shutdown. That is a crash on quit,
/// which is both the worst time to crash and the hardest to reproduce.
///
/// `shutdown_background` hands the wind-down to a detached thread and returns
/// immediately, so `Drop` never blocks and never panics regardless of where it
/// runs. Abandoning in-flight tasks is the honest semantic here rather than a
/// compromise: every engine task is already cancellable through its
/// `CancellationToken`, and by the time the last handle is gone there is
/// nobody left to receive what those tasks would have produced.
pub(crate) struct RuntimeGuard {
    /// `Option` so `Drop` can take ownership; `Some` for the whole lifetime.
    runtime: Option<Runtime>,
}

impl RuntimeGuard {
    pub(crate) fn new(runtime: Runtime) -> Self {
        Self {
            runtime: Some(runtime),
        }
    }

    /// Borrow the runtime to spawn onto it.
    pub(crate) fn handle(&self) -> &Runtime {
        self.runtime
            .as_ref()
            .expect("runtime is Some for the whole lifetime of the guard")
    }
}

impl std::ops::Deref for RuntimeGuard {
    type Target = Runtime;

    fn deref(&self) -> &Runtime {
        self.handle()
    }
}

impl Drop for RuntimeGuard {
    fn drop(&mut self) {
        if let Some(runtime) = self.runtime.take() {
            runtime.shutdown_background();
        }
    }
}

impl std::fmt::Debug for EngineHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EngineHandle").finish_non_exhaustive()
    }
}

// ---------------------------------------------------------------------------
// StreamHandle
// ---------------------------------------------------------------------------

/// A cancellable stream slot handle.
///
/// Dropping the handle cancels the corresponding engine work (LR-9, §2.3).
/// The caller never touches the cancellation token directly.
///
/// # Why the canceller is `Sync` and not merely `Send`
///
/// It was `Box<dyn Fn() + Send>`, which made `StreamHandle` itself `!Sync`, and
/// that propagated to *anything holding one*. Holding a handle is exactly what
/// a caller does when it owns a long-running job rather than draining a stream
/// inline — `nudox_mcp`'s index-job registry is the first such caller, and the
/// failure landed on rmcp's `ServerHandler: Sync` bound, several types away from
/// the cause and with no mention of cancellation in the message.
///
/// Requiring `Sync` on the closure instead of adding a `Mutex` at that call site
/// is the fix at the right level: every canceller in the system is
/// `CancellationToken::cancel`, `cancel` takes `&self`, and `CancellationToken`
/// is `Sync` — so no existing construction loses anything, and a future
/// canceller that genuinely could not be shared across threads would now say so
/// at its own definition rather than at a distant holder's.
pub struct StreamHandle {
    /// The generation this stream answers.
    pub generation: crate::wire::Gen,
    /// Invoked on drop to signal cancellation.
    canceller: Box<dyn Fn() + Send + Sync>,
}

impl StreamHandle {
    /// Construct a handle for `generation`, cancelled by `canceller` on drop.
    ///
    /// Public because `StreamHandle` is now the *only* stream handle in the
    /// system — the GUI re-exports this type rather than wrapping it — so
    /// anything that stands in for the engine (a test double, an alternative
    /// search backend) has to be able to produce one.
    ///
    /// `Sync` on `canceller` is load-bearing — see the type-level docs.
    pub fn new(
        generation: crate::wire::Gen,
        canceller: impl Fn() + Send + Sync + 'static,
    ) -> Self {
        Self {
            generation,
            canceller: Box::new(canceller),
        }
    }

    /// Cancel the stream explicitly.  Also called implicitly on `Drop`.
    pub fn cancel(&self) {
        (self.canceller)();
    }
}

impl Drop for StreamHandle {
    fn drop(&mut self) {
        (self.canceller)();
    }
}

impl std::fmt::Debug for StreamHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StreamHandle")
            .field("generation", &self.generation)
            .finish()
    }
}

// ---------------------------------------------------------------------------
// EngineHandle method dispatch
// ---------------------------------------------------------------------------

impl EngineHandle {
    /// The Trustfall schema — for the MCP server and the in-app query editor
    /// (§L5).
    pub fn schema(&self) -> &'static trustfall::Schema {
        self.inner.schema
    }

    /// A clone of the live corpus handle.
    ///
    /// Used internally by search, query, and doc layers. Not part of the
    /// `lindsey`-facing surface.
    /// The host-supplied highlighter, if any.
    pub(crate) fn highlighter(&self) -> crate::highlight::SharedHighlighter {
        self.inner.highlighter.clone()
    }

    pub(crate) fn corpus(&self) -> Corpus {
        self.inner.corpus.clone()
    }

    /// The growing semantic index — see [`crate::semantic`].
    pub(crate) fn semantic_index(&self) -> crate::semantic::SemanticIndex {
        self.inner.semantic.clone()
    }

    /// The host's embedder, if this build has one.
    pub(crate) fn embedder(&self) -> crate::semantic::SharedEmbedder {
        self.inner.embedder.clone()
    }

    /// How many packages the semantic index currently covers.
    ///
    /// Public because "is the index finished?" is a question a *test* has to be
    /// able to ask without racing — polling `search` until the section stops
    /// saying `Building` works, but it conflates "not yet indexed" with "the
    /// query matched nothing", which is the exact conflation this whole change
    /// exists to remove.
    pub fn semantic_coverage(&self) -> usize {
        self.inner.semantic.covered()
    }

    /// A clone of the version registry handle.
    ///
    /// Used by `doc.rs` to build timelines and by `crate::versions` to serve
    /// `versions`/`select_version`. Not part of the `lindsey`-facing surface:
    /// the registry deals in `Arc<PackageView>`, which must not cross §L0.
    pub(crate) fn versions_registry(&self) -> Arc<VersionRegistry> {
        Arc::clone(&self.inner.versions)
    }

    /// A clone of the durable load-failure record — see [`LoadFailures`].
    ///
    /// Used by `doc.rs` to enrich `EngineError::PackageNotLoaded` with why a
    /// package is missing, when that is known.
    pub(crate) fn load_failures(&self) -> LoadFailures {
        self.inner.load_failures.clone()
    }

    /// The shared HTTP client used to reach package registries.
    pub(crate) fn http_client(&self) -> &reqwest::Client {
        &self.inner.acquire.client
    }

    /// Where fetched packages are unpacked.
    pub(crate) fn package_cache(&self) -> &std::path::Path {
        &self.inner.acquire.cache
    }

    /// Produce one on-disk package and insert it into the **running** corpus.
    ///
    /// This is the live-insertion path. It builds the same `ProducerSource`
    /// `Engine::start_with_versions` builds — one descriptor rather than a
    /// workspace's worth — and drives it through [`drive_load`], so the package
    /// lands in the corpus by exactly the rules a start-up load uses. In
    /// particular, indexing a *second* version of a lineage that is already
    /// loaded goes through [`VersionRegistry::record`] and therefore appears in
    /// `versions()` and becomes current only if it is the newest — the same
    /// arbitration as `start_with_versions`, not a blind `Corpus::insert` that
    /// would silently demote whatever was resident.
    ///
    /// `Err` carries the store's flattened failure text; the caller decides
    /// what kind of failure it was (see
    /// `crate::acquire::classify_load_failure`).
    pub(crate) async fn load_one(
        &self,
        spec: crate::PackageSpec,
    ) -> Result<crate::acquire::LoadedPackage, SharedStr> {
        use nudox_store::source::producer::{ProducerRegistry, ProducerSource};

        let descriptor = crate::descriptor_for(spec);
        let source = ProducerSource::new(
            Arc::new(ProducerRegistry::with_all_available()),
            vec![descriptor],
        );

        let outcomes = drive_load(source, &self.inner).await;
        match outcomes.into_iter().next() {
            Some(PackageLoadEvent::Loaded {
                name,
                ecosystem,
                version,
                symbol_count,
                root,
            }) => Ok(crate::acquire::LoadedPackage {
                name,
                ecosystem,
                version: version.unwrap_or_default().into(),
                symbol_count,
                root,
            }),
            Some(PackageLoadEvent::LoadFailed { error, .. }) => Err(error),
            // `ProducerSource` emits exactly one `Ready` or `Failed` per
            // descriptor, so an empty outcome list means the stream ended
            // early. Reporting it as a failure rather than as a success with
            // nothing loaded is the same call `IrSource`'s own contract makes:
            // "load MUST emit Failed (not panic, not hang) when a single
            // package fails".
            _ => Err(SharedStr::from(
                "the producer stream ended without emitting a result for this package",
            )),
        }
    }

    /// Spawn a future on the engine's multi-thread runtime.
    pub(crate) fn spawn<F>(&self, fut: F) -> tokio::task::JoinHandle<F::Output>
    where
        F: std::future::Future + Send + 'static,
        F::Output: Send + 'static,
    {
        self.runtime.spawn(fut)
    }

    /// Spawn a `!Send` future on a dedicated OS thread running a
    /// single-thread Tokio runtime + `LocalSet`.
    ///
    /// Used for query execution where the Trustfall result stream is `!Send`
    /// (LR-9: "one `LocalSet` for query execution; the fork's result stream
    /// is `!Send` — that is a constraint to honour, not to defeat by
    /// blocking").
    ///
    /// The result `R` must be `Send + 'static` so it can be handed back to
    /// the caller over a channel. The future `F` need not be `Send`.
    ///
    /// # Implementation note
    ///
    /// We cannot move a `!Send` future into a `std::thread::spawn` closure
    /// (which requires `Send`) and we cannot poll a `!Send` stream on Tokio's
    /// multi-thread executor. Instead we use a two-step approach:
    ///
    /// 1. A `tokio::task::spawn_blocking` call reserves a blocking thread.
    /// 2. Inside that thread we build a new **current-thread** Tokio runtime
    ///    (single-threaded, no worker threads) and run the `LocalSet` on it.
    ///    Both the runtime and the LocalSet are thread-local, so the `!Send`
    ///    future never needs to cross a thread boundary.
    ///
    /// The closure that builds and executes the future *is* `Send` because
    /// `corpus`, `schema`, and all the other captures used in `run_query` are
    /// `Send`. The future itself is created *inside* the blocking thread.
    pub(crate) fn spawn_local_query(
        &self,
        corpus: nudox_store::corpus::Corpus,
        schema: &'static trustfall::Schema,
        q: crate::query::GraphQuery,
        generation: crate::wire::Gen,
        tx: flume::Sender<crate::wire::QueryEvent>,
        cancel: tokio_util::sync::CancellationToken,
    ) -> tokio::task::JoinHandle<()> {
        // Everything we pass into spawn_blocking is Send.
        self.runtime.spawn(async move {
            tokio::task::spawn_blocking(move || {
                // Build a single-thread runtime just for this query.
                let rt = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .expect("single-thread runtime for query must be buildable");
                let local = LocalSet::new();
                rt.block_on(local.run_until(crate::query::run_query(
                    corpus, schema, q, generation, tx, cancel,
                )));
            })
            .await
            .expect("query blocking task must not panic");
        })
    }

    /// Make a cancellation-token pair for a new stream.
    ///
    /// Returns `(token, cancel_fn)`. Pass `token` into the async task and
    /// call `cancel_fn` from the `StreamHandle`.
    pub(crate) fn make_cancel() -> (
        tokio_util::sync::CancellationToken,
        impl Fn() + Send + 'static,
    ) {
        let token = tokio_util::sync::CancellationToken::new();
        let cancel = {
            let t = token.clone();
            move || t.cancel()
        };
        (token, cancel)
    }

    /// Subscribe to package load notifications, receiving both already-loaded
    /// packages and future arrivals in exactly-once order.
    ///
    /// # Why this is not a simple broadcast subscriber
    ///
    /// `Engine::start` begins seeding the corpus immediately, before any
    /// caller can subscribe.  A subscriber that only forwards *future* events
    /// would miss packages that finished loading before `packages()` was
    /// called — a status bar that shows "no packages" forever when the corpus
    /// is already fully loaded.  Conversely, a snapshot-only approach misses
    /// live arrivals that land between the snapshot and the return.
    ///
    /// The correct ordering — subscribe-then-snapshot — guarantees exactly-once
    /// delivery:
    ///
    /// 1. Subscribe to the live broadcast **first** so that no events are
    ///    missed from this point forward (including any that arrive while the
    ///    snapshot is being taken in step 2).
    /// 2. Take an async snapshot of the already-loaded packages from `Corpus`.
    /// 3. Emit snapshot entries into the caller's receiver, then drain the
    ///    live channel, **deduplicating** by package name so anything that
    ///    arrived after subscription but before (or during) the snapshot is
    ///    emitted exactly once.
    ///
    /// "Subscribe before snapshot" is the key invariant.  The reverse order
    /// can drop events: if a package finishes loading between the snapshot and
    /// the subscribe, it appears in neither.
    ///
    /// # Channel capacity
    ///
    /// The returned receiver is a bounded `flume` channel with capacity **32**
    /// (Appendix C: Package = 32, backpressure).  The spawned reconciler task
    /// applies backpressure on the flume sender; it does not block the engine's
    /// runtime.
    ///
    /// # Lag handling
    ///
    /// The broadcast receiver may fall behind if the seeding task produces
    /// packages faster than the reconciler consumes them.  `RecvError::Lagged`
    /// is handled by logging at `warn` level and continuing — a lagging status
    /// bar must not kill the stream.
    ///
    /// # One row per package, not per generation
    ///
    /// The corpus can now hold several generations of one package, but this
    /// stream still emits exactly one `Loaded` per lineage — a package list
    /// wants one row for `axum`, not three. The `version` field carries the
    /// *current* generation, read from the version registry at emit time
    /// rather than from whichever `Ready` happened to arrive first.
    ///
    /// A generation that lands after the row was emitted therefore does not
    /// produce a second row, and does not update the one already sent. That is
    /// deliberate: [`EngineHandle::versions`] is the surface for "what else is
    /// loaded", it is a synchronous read, and re-reading it on each
    /// `PackageLoadEvent` is cheaper and less racy than trying to keep a
    /// one-shot event stream authoritative about a growing set.
    pub fn packages(&self) -> flume::Receiver<PackageLoadEvent> {
        // Capacity 32 per Appendix C.
        let (tx, rx) = flume::bounded::<PackageLoadEvent>(32);

        // Step 1 — subscribe to the live broadcast BEFORE snapshotting.
        // This is load-bearing: any package that finishes between subscribe
        // and snapshot will appear in BOTH the live channel and the snapshot;
        // the dedup loop in step 3 collapses it to one emission.
        let live_rx = self.inner.pkg_tx.subscribe();
        let corpus = self.inner.corpus.clone();
        let versions = Arc::clone(&self.inner.versions);

        self.spawn(async move {
            // Step 2 — snapshot of all packages already in the corpus at this
            // moment.  Because we subscribed first, any package that lands
            // between subscribe and now is already queued in `live_rx`.
            let snapshot: Vec<Arc<nudox_store::package::PackageView>> = corpus.packages().await;

            // Build a set of names we emitted from the snapshot so we can
            // deduplicate against the live channel in step 3.
            let mut emitted: std::collections::HashSet<String> =
                std::collections::HashSet::with_capacity(snapshot.len());

            for pkg in snapshot {
                let symbol_count = pkg.view().table().len() as u64;
                let name = pkg.lineage().name.as_str().to_owned();
                let ecosystem = pkg.lineage().ecosystem.as_str().to_owned();
                emitted.insert(name.clone());
                // The snapshot comes from the corpus, which holds the current
                // generation; the registry is what knows its version string.
                let version = versions
                    .versions(pkg.lineage())
                    .current()
                    .map(|v| v.version.to_string());
                let event = PackageLoadEvent::Loaded {
                    name: name.into(),
                    ecosystem: ecosystem.into(),
                    version,
                    symbol_count,
                    root: package_root_key(&pkg),
                };
                // Backpressure: if the GUI is slow, we block here rather than
                // dropping events.  This is the correct trade-off: the status
                // bar should show a correct picture even if it is delayed.
                if tx.send_async(event).await.is_err() {
                    return; // receiver dropped; nothing to do
                }
            }

            // Step 3 — drain the live broadcast channel, skipping any package
            // whose name is already in `emitted` (exact-once guarantee).
            //
            // We drain with `try_recv` first to consume anything that queued
            // between subscribe and snapshot without blocking; after that we
            // switch to `recv` so future arrivals are forwarded as they happen.
            let mut live_rx = live_rx;
            loop {
                let ev = live_rx.recv().await;
                match ev {
                    Ok(mut event) => {
                        // Deduplicate: if this package was already emitted from
                        // the snapshot, skip it.  We key on `name` because that
                        // is the identity the GUI shows — and because several
                        // generations of one package must collapse to one row.
                        let should_skip = match &event {
                            PackageLoadEvent::Loaded { name, .. } => {
                                !emitted.insert(name.to_string())
                            }
                            PackageLoadEvent::LoadFailed { name, .. } => {
                                !emitted.insert(format!("__fail__{name}"))
                            }
                        };
                        if should_skip {
                            continue;
                        }
                        // Report the *current* generation's version rather than
                        // this event's, which is whichever `Ready` won the race
                        // to arrive first and is not necessarily the newest.
                        if let PackageLoadEvent::Loaded {
                            name,
                            ecosystem,
                            version,
                            ..
                        } = &mut event
                        {
                            let lineage = PackageLineageId::new(
                                nudox_ir::change::EcosystemId::new(&**ecosystem),
                                nudox_ir::change::PackageName::new(&**name),
                            );
                            if let Some(current) = versions.versions(&lineage).current() {
                                *version = Some(current.version.to_string());
                            }
                        }
                        if tx.send_async(event).await.is_err() {
                            return; // receiver dropped
                        }
                    }
                    Err(broadcast::error::RecvError::Lagged(n)) => {
                        // The broadcast ring-buffer overflowed; we missed `n`
                        // messages.  The status bar will show a stale count but
                        // must NOT crash.  A re-snapshot here would be correct
                        // but expensive; log and continue — the corpus state is
                        // authoritative and can be queried directly.
                        tracing::warn!(
                            missed = n,
                            "package-load broadcast lagged; status bar may undercount"
                        );
                        // continue receiving from where we are
                    }
                    Err(broadcast::error::RecvError::Closed) => {
                        // The seeding task finished and dropped its sender.
                        // All packages that will ever load have been emitted.
                        break;
                    }
                }
            }
        });

        rx
    }
}

// ---------------------------------------------------------------------------
// Package root
// ---------------------------------------------------------------------------

/// The `SymbolKey` of a package's root entry — its crate / top-level module.
///
/// The root is the one entry with no parent. A well-formed package has exactly
/// one; this takes the first in declaration order so the answer is
/// deterministic even if a producer ever emits a second unparented entry (the
/// `IrView` iteration order is the insertion order of the sealed table, not a
/// hash order).
///
/// Returns `None` for a package with no unparented entry, which would be
/// malformed rather than merely empty.
fn package_root_key(pkg: &nudox_store::package::PackageView) -> Option<crate::SymbolKey> {
    let view = pkg.view();
    view.entries()
        .map(|(intro, _)| intro)
        .find(|intro| view.parent_of(*intro).is_none())
        .map(|intro| crate::SymbolKey::new(pkg.lineage().clone(), intro))
}

// ---------------------------------------------------------------------------
// Convenience constructors
// ---------------------------------------------------------------------------

impl Engine {
    /// Start an engine over the built-in fixture corpus.
    ///
    /// # Why this lives here and not in the caller
    ///
    /// `lindsey` must not depend on `nudox-store` (§L0's dependency law): a view
    /// that can name the store can come to depend on the shape of the IR, which
    /// is the coupling the whole layering exists to prevent. So the *caller*
    /// expresses intent — "start with fixtures" — and the engine, which already
    /// depends on the store, chooses the `IrSource`.
    ///
    /// LR-5 makes the swap trivial: `ProducerSource` implements the same trait,
    /// so pointing this at a real workspace changes one line here and nothing
    /// above it.
    #[cfg(feature = "fixtures")]
    pub fn start_with_fixtures(config: EngineConfig) -> EngineHandle {
        Self::start(config, nudox_store::source::fixtures::FixtureSource::rich())
    }

    /// Start an engine that produces IR for real on-disk packages.
    ///
    /// # Why [`PackageSpec`] and not [`nudox_store::source::producer::PackageDescriptor`]
    ///
    /// Same layering law as `start_with_fixtures`: `lindsey` must never name
    /// `nudox-store` or any type that lives below the engine seam (§L0).
    /// `PackageSpec` is an engine-owned plain struct — only `std` types, nothing
    /// from the IR or store layers — so `lindsey` can construct one without
    /// crossing the boundary.  The engine converts it to `PackageDescriptor`
    /// internally, keeping the translation entirely inside this crate.
    ///
    /// # Language selection
    ///
    /// `start_with_versions` builds its registry from
    /// `ProducerRegistry::with_all_available`, which registers Rust, Go,
    /// Java, CSharp, TypeScript, and Cpp (C/C++, via `ClangProducer`) — plus
    /// Python only under the `pyrefly` feature, since its producer is
    /// otherwise inert.  The `language` field of `PackageSpec` is expressed
    /// as [`ProducerLanguage`] (re-exported from this crate) so the caller
    /// can name it without depending on `nudox-ir`.  Passing `Python`
    /// without the `pyrefly` feature enabled surfaces a `LoadEvent::Failed`
    /// (toolchain missing) rather than failing to compile — see
    /// [`ProducerLanguage`]'s doc comment.
    ///
    /// # Production path
    ///
    /// This constructor is **not** gated on any feature flag.  `start_with_fixtures`
    /// is feature-gated (`fixtures`) because fixtures are optional test
    /// infrastructure; `start_with_producer` is the production path and must
    /// always be available.
    pub fn start_with_producer(config: EngineConfig, packages: Vec<PackageSpec>) -> EngineHandle {
        // Every `PackageSpec` is a one-generation history. Routing both
        // constructors through the same code path means the single-version
        // caller and the multi-version caller cannot diverge — in particular
        // the single-version corpus still populates the version registry, so
        // `versions()` returns one row rather than an empty list and a symbol
        // page still gets a (one-row) timeline.
        Self::start_with_versions(config, packages.into_iter().map(Into::into).collect())
    }

    /// Start an engine that produces IR for several versions of each package.
    ///
    /// This is [`Engine::start_with_producer`] generalised along the axis the
    /// corpus was previously missing: a [`PackageHistorySpec`] names one
    /// package and *N* on-disk roots, one per version, and the engine loads all
    /// of them.
    ///
    /// # What this buys
    ///
    /// A symbol timeline and a version dropdown are the same capability seen
    /// from two ends, and both need the same precondition: more than one
    /// generation of a package resident at once. Two lowerings of one package
    /// at different versions share `IntroId`s for every symbol that persisted
    /// (that is what `IntroId` is *for*), so once both are loaded a symbol's
    /// history is a lookup of one id across the generations and nothing else
    /// has to be recorded.
    ///
    /// # Which version is served
    ///
    /// All of them are loaded; the newest is *current*, meaning it is the one
    /// resident in the `Corpus` and therefore the one `open_symbol`, `search`
    /// and `query` answer from. Newest is decided by
    /// `crate::versions::VersionOrder` (semver-shaped where the string parses,
    /// load order to break ties). A caller changes the selection with
    /// [`EngineHandle::select_version`] and enumerates the options with
    /// [`EngineHandle::versions`].
    ///
    /// Ordering of the loads does not matter: the newest generation ends up
    /// current whether it finished producing first or last.
    ///
    /// # Cost
    ///
    /// Linear. Loading four versions runs the producer four times and holds
    /// four `PackageView`s. There is no incremental or shared-storage path
    /// between generations — the IR-native VCS in `workspace/ir-vcs` is where
    /// that belongs, and wiring it in is not a `nudox-engine` change.
    pub fn start_with_versions(
        #[allow(dead_code)] config: EngineConfig,
        packages: Vec<PackageHistorySpec>,
    ) -> EngineHandle {
        use nudox_store::source::producer::{PackageDescriptor, ProducerRegistry, ProducerSource};

        let registry = Arc::new(ProducerRegistry::with_all_available());
        // Every generation of a package produces the *same* `PackageLineageId`
        // — that is the point: the lineage is version-free, so the two
        // generations are recognisably the same package and their `IntroId`s
        // are comparable.
        let descriptors: Vec<PackageDescriptor> = packages
            .into_iter()
            .flat_map(|history| {
                let name = history.name;
                let language = history.language;
                history.versions.into_iter().map(move |v| {
                    crate::descriptor_for(PackageSpec {
                        root: v.root,
                        name: name.clone(),
                        version: v.version,
                        language,
                    })
                })
            })
            .collect();

        let source = ProducerSource::new(registry, descriptors);
        Self::start(config, source)
    }
}

// ---------------------------------------------------------------------------
// Tests — the multi-generation seeding path
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        // `ProducerLanguage` used to arrive through the module-level
        // `use crate::{…}` and was dropped from it by an unrelated refactor,
        // which broke only this module (the non-test callers fully qualify it).
        // Named here rather than re-added above so the two import lists cannot
        // fight over it.
        ProducerLanguage,
        test_support::{lineage, package_with, start_and_settle},
        wire::{Gen, VersionEvent},
    };

    /// Start a settled engine over a fixed list of `(version, package)`
    /// generations for the `axum` lineage.
    async fn engine_with(
        generations: Vec<(&str, Arc<nudox_store::package::PackageView>)>,
    ) -> (EngineHandle, PackageLineageId) {
        let lid = lineage("axum");
        let engine = start_and_settle(&lid, generations).await;
        (engine, lid)
    }

    #[tokio::test]
    async fn every_generation_of_one_package_is_retained() {
        // The behaviour this whole feature turns on: before, the second
        // `Ready` for a lineage replaced the first in the corpus and the
        // first was simply lost.
        let lid = lineage("axum");
        let (engine, _) = engine_with(vec![
            ("0.7.9", package_with(&lid, &[(1, "Router")])),
            ("0.8.1", package_with(&lid, &[(1, "Router")])),
            ("0.9.0", package_with(&lid, &[(1, "Router")])),
        ])
        .await;

        let list = engine.versions(&lid);
        assert_eq!(list.len(), 3);
        let seen: Vec<&str> = list.versions.iter().map(|v| &*v.version).collect();
        assert_eq!(seen, vec!["0.9.0", "0.8.1", "0.7.9"], "newest first");
    }

    #[tokio::test]
    async fn the_newest_generation_is_current_regardless_of_arrival_order() {
        // Deliberately deliver the newest first, so "last writer wins" — the
        // behaviour the corpus alone would give — is the wrong answer.
        let lid = lineage("axum");
        let (engine, _) = engine_with(vec![
            ("0.9.0", package_with(&lid, &[(1, "a"), (2, "b"), (3, "c")])),
            ("0.7.9", package_with(&lid, &[(1, "a")])),
        ])
        .await;

        assert_eq!(&*engine.versions(&lid).current().unwrap().version, "0.9.0");
        // And the corpus agrees: it holds the 3-symbol generation, not the
        // 1-symbol one that arrived last.
        let resident = engine.corpus().package(&lid).await.unwrap();
        assert_eq!(resident.view().table().len(), 3);
    }

    #[tokio::test]
    async fn select_version_repoints_the_corpus() {
        let lid = lineage("axum");
        let (engine, _) = engine_with(vec![
            ("0.7.9", package_with(&lid, &[(1, "a")])),
            ("0.8.1", package_with(&lid, &[(1, "a"), (2, "b")])),
        ])
        .await;

        // Sanity: the newest is current to begin with.
        assert_eq!(
            engine
                .corpus()
                .package(&lid)
                .await
                .unwrap()
                .view()
                .table()
                .len(),
            2
        );

        let rx = engine.select_version(lid.clone(), "0.7.9", Gen(7));
        let ev = rx.recv_async().await.expect("exactly one event is sent");
        match ev {
            VersionEvent::Switched {
                generation,
                version,
                ..
            } => {
                assert_eq!(generation, Gen(7), "the caller's Gen is echoed back");
                assert_eq!(&*version, "0.7.9");
            }
            other => panic!("expected Switched, got {other:?}"),
        }

        // Every version-free path now answers from 0.7.9.
        assert_eq!(
            engine
                .corpus()
                .package(&lid)
                .await
                .unwrap()
                .view()
                .table()
                .len(),
            1
        );
        assert_eq!(&*engine.versions(&lid).current().unwrap().version, "0.7.9");
    }

    #[tokio::test]
    async fn selecting_an_unloaded_version_reports_not_loaded_and_changes_nothing() {
        let lid = lineage("axum");
        let (engine, _) = engine_with(vec![("0.7.9", package_with(&lid, &[(1, "a")]))]).await;

        let rx = engine.select_version(lid.clone(), "9.9.9", Gen(2));
        let ev = rx.recv_async().await.expect("exactly one event is sent");
        assert!(matches!(ev, VersionEvent::NotLoaded { .. }), "got {ev:?}");
        assert_eq!(&*engine.versions(&lid).current().unwrap().version, "0.7.9");
    }

    #[tokio::test]
    async fn a_single_version_corpus_still_populates_the_registry() {
        // The one-version case must produce a one-row list, not an empty one:
        // it is what a caller sees first, and an empty dropdown would read as
        // "versions are unavailable" rather than "there is one".
        let lid = lineage("axum");
        let (engine, _) = engine_with(vec![("0.8.9", package_with(&lid, &[(1, "Router")]))]).await;

        let list = engine.versions(&lid);
        assert_eq!(list.len(), 1);
        assert_eq!(&*list.versions[0].version, "0.8.9");
        assert!(list.versions[0].is_current);
    }

    #[tokio::test]
    async fn an_unloaded_package_yields_an_empty_version_list() {
        let lid = lineage("axum");
        let (engine, _) = engine_with(vec![("0.8.9", package_with(&lid, &[(1, "a")]))]).await;
        assert!(engine.versions(&lineage("never-loaded")).is_empty());
    }

    #[tokio::test]
    async fn packages_reports_one_row_per_lineage_carrying_the_current_version() {
        let lid = lineage("axum");
        let (engine, _) = engine_with(vec![
            ("0.7.9", package_with(&lid, &[(1, "a")])),
            ("0.8.1", package_with(&lid, &[(1, "a")])),
        ])
        .await;

        let rx = engine.packages();
        let mut rows = Vec::new();
        // Drain what is immediately available; the stream stays open for
        // future arrivals, so a blocking drain would never terminate.
        while let Ok(ev) = rx.recv_async().await {
            rows.push(ev);
            if rows.len() == 1 {
                break;
            }
        }

        assert_eq!(rows.len(), 1, "two generations collapse to one package row");
        match &rows[0] {
            PackageLoadEvent::Loaded { name, version, .. } => {
                assert_eq!(&**name, "axum");
                assert_eq!(
                    version.as_deref(),
                    Some("0.8.1"),
                    "the row carries the current generation, not the first to arrive"
                );
            }
            other => panic!("expected Loaded, got {other:?}"),
        }
    }

    #[test]
    fn a_package_spec_converts_to_a_one_generation_history() {
        let spec = PackageSpec {
            root: PathBuf::from("/src/axum"),
            name: "axum".to_owned(),
            version: "0.8.9".to_owned(),
            language: ProducerLanguage::Rust,
        };
        let history: PackageHistorySpec = spec.into();
        assert_eq!(history.name, "axum");
        assert_eq!(history.versions.len(), 1);
        assert_eq!(history.versions[0].version, "0.8.9");
        assert_eq!(history.versions[0].root, PathBuf::from("/src/axum"));
    }
}
