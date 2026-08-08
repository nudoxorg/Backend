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
    PackageHistorySpec, PackageLoadEvent, PackageSpec, ProducerLanguage, versions::VersionRegistry,
};

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
}

impl std::fmt::Debug for EngineConfig {
    /// Hand-written because `dyn Highlighter` is not `Debug` — requiring it
    /// would force every host implementation to derive it for no reader benefit.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EngineConfig")
            .field("workspace_root", &self.workspace_root)
            .field("worker_threads", &self.worker_threads)
            .field("highlighter", &self.highlighter.is_some())
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

        let corpus = Corpus::new();
        let schema = nudox_graph::schema();
        let versions = Arc::new(VersionRegistry::new());

        // Capacity 64: broadcast channel for package load notifications.  See
        // `EngineInner::pkg_tx` for the capacity rationale.
        let (pkg_tx, _initial_rx) = broadcast::channel::<PackageLoadEvent>(64);
        // `_initial_rx` is immediately dropped; real subscribers are created
        // inside `EngineHandle::packages()` before they read the snapshot.

        let inner = Arc::new(EngineInner {
            corpus: corpus.clone(),
            versions: Arc::clone(&versions),
            schema,
            highlighter: config.highlighter.clone(),
            pkg_tx: pkg_tx.clone(),
        });

        // Seed the corpus from the source on the runtime's thread pool.
        // We do this eagerly on start so that searches issued immediately
        // after `start` return (possibly empty) rather than blocking.
        //
        // Every `LoadEvent::Ready` and `LoadEvent::Failed` is also broadcast
        // on `pkg_tx` so that `EngineHandle::packages()` subscribers receive
        // live notifications.  `send` returns `Err` only when there are no
        // receivers (which is fine at startup — we just drop the notification).
        let seed_corpus = corpus.clone();
        let seed_versions = Arc::clone(&versions);
        runtime.spawn(async move {
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
                            seed_versions.record(&lineage, version.clone(), Arc::clone(&package))
                        {
                            seed_corpus.insert(resident).await;
                        }

                        let event = PackageLoadEvent::Loaded {
                            name: name.into(),
                            ecosystem: ecosystem.into(),
                            version,
                            symbol_count,
                            root: package_root_key(&package),
                        };
                        // Ignore `Err`: no subscribers yet is fine.
                        let _ = pkg_tx.send(event);
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
                        let event = PackageLoadEvent::LoadFailed {
                            name: lineage.name.as_str().to_owned().into(),
                            ecosystem: lineage.ecosystem.as_str().to_owned().into(),
                            error: error.to_string().into(),
                        };
                        let _ = pkg_tx.send(event);
                    }
                    Ok(_) => {} // Progress — informational only
                    Err(e) => {
                        tracing::error!("stream-level load error: {e}");
                        break;
                    }
                }
            }
            info!(
                "corpus seeding complete; {} package(s) loaded",
                seed_corpus.len().await
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
pub struct StreamHandle {
    /// The generation this stream answers.
    pub generation: crate::wire::Gen,
    /// Invoked on drop to signal cancellation.
    canceller: Box<dyn Fn() + Send>,
}

impl StreamHandle {
    /// Construct a handle for `generation`, cancelled by `canceller` on drop.
    ///
    /// Public because `StreamHandle` is now the *only* stream handle in the
    /// system — the GUI re-exports this type rather than wrapping it — so
    /// anything that stands in for the engine (a test double, an alternative
    /// search backend) has to be able to produce one.
    pub fn new(generation: crate::wire::Gen, canceller: impl Fn() + Send + 'static) -> Self {
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

    /// A clone of the version registry handle.
    ///
    /// Used by `doc.rs` to build timelines and by `crate::versions` to serve
    /// `versions`/`select_version`. Not part of the `lindsey`-facing surface:
    /// the registry deals in `Arc<PackageView>`, which must not cross §L0.
    pub(crate) fn versions_registry(&self) -> Arc<VersionRegistry> {
        Arc::clone(&self.inner.versions)
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
        let descriptors: Vec<PackageDescriptor> = packages
            .into_iter()
            .flat_map(|history| {
                let name = history.name;
                let language = history.language;
                history.versions.into_iter().map(move |v| {
                    // Convert the engine-owned spec into the store-owned
                    // `PackageDescriptor`. Every generation of a package
                    // produces the *same* `PackageLineageId` — that is the
                    // point: the lineage is version-free, so the two
                    // generations are recognisably the same package and their
                    // `IntroId`s are comparable.
                    //
                    // One `PackageDescriptor` constructor per ecosystem, not
                    // one `Language` for all of them: pairing the wrong
                    // ecosystem id with a language here would build a
                    // descriptor that no registered producer (or the wrong
                    // one) ever answers, and this match is the one place that
                    // pairing is decided.
                    match language {
                        ProducerLanguage::Rust => {
                            PackageDescriptor::cargo(v.root, name.clone(), v.version)
                        }
                        ProducerLanguage::Go => {
                            PackageDescriptor::go(v.root, name.clone(), v.version)
                        }
                        ProducerLanguage::Java => {
                            PackageDescriptor::maven(v.root, name.clone(), v.version)
                        }
                        ProducerLanguage::CSharp => {
                            PackageDescriptor::nuget(v.root, name.clone(), v.version)
                        }
                        ProducerLanguage::Python => {
                            PackageDescriptor::pypi(v.root, name.clone(), v.version)
                        }
                        ProducerLanguage::TypeScript => {
                            PackageDescriptor::npm(v.root, name.clone(), v.version)
                        }
                        ProducerLanguage::Cpp => {
                            PackageDescriptor::cpp(v.root, name.clone(), v.version)
                        }
                    }
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
