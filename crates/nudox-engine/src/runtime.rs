//! LR-9: the engine owns every runtime.
//!
//! One Tokio multi-thread runtime for I/O and producer work.
//! One `LocalSet` for query execution (the Trustfall fork's result stream is
//! `!Send` — that is a design constraint of the pinned fork, not an oversight,
//! and must not be "fixed" by blocking the caller).
//!
//! `lindsey` never creates a Tokio runtime of its own. Everything async goes
//! through the `EngineHandle` returned by [`Engine::start`].

use std::{path::PathBuf, sync::Arc};

use tokio::runtime::Runtime;
use tokio::task::LocalSet;
use tracing::info;

use nudox_store::{
    corpus::Corpus,
    source::{IrSource, LoadEvent, LoadRequest},
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
    pub(crate) corpus: Corpus,
    /// The Trustfall schema singleton — returned by `EngineHandle::schema`.
    pub(crate) schema: &'static trustfall::Schema,
    /// The host-supplied highlighter, if any (see `crate::highlight`).
    pub(crate) highlighter: crate::highlight::SharedHighlighter,
}

// ---------------------------------------------------------------------------
// Engine
// ---------------------------------------------------------------------------

/// The engine itself — constructed once, consumed by [`Engine::start`].
pub struct Engine {
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
            builder.build().expect("Tokio runtime creation must succeed"),
        ));

        let corpus = Corpus::new();
        let schema = nudox_graph::schema();
        let inner = Arc::new(EngineInner {
            corpus: corpus.clone(),
            schema,
            highlighter: config.highlighter.clone(),
        });

        // Seed the corpus from the source on the runtime's thread pool.
        // We do this eagerly on start so that searches issued immediately
        // after `start` return (possibly empty) rather than blocking.
        let seed_corpus = corpus.clone();
        runtime.spawn(async move {
            use futures::StreamExt as _;
            let mut stream = source.load(LoadRequest::default());
            while let Some(event) = stream.next().await {
                match event {
                    Ok(LoadEvent::Ready { package }) => {
                        seed_corpus.insert(package).await;
                    }
                    Ok(LoadEvent::Failed { lineage, error }) => {
                        tracing::warn!(
                            package = %lineage,
                            "package load failed: {error}",
                        );
                    }
                    Ok(_) => {} // Discovered / Progress — informational only
                    Err(e) => {
                        tracing::error!("stream-level load error: {e}");
                        break;
                    }
                }
            }
            info!("corpus seeding complete; {} package(s) loaded", seed_corpus.len().await);
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
        Self { generation, canceller: Box::new(canceller) }
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
    pub(crate) fn make_cancel() -> (tokio_util::sync::CancellationToken, impl Fn() + Send + 'static) {
        let token = tokio_util::sync::CancellationToken::new();
        let cancel = {
            let t = token.clone();
            move || t.cancel()
        };
        (token, cancel)
    }
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
        Self::start(
            config,
            nudox_store::source::fixtures::FixtureSource::rich(),
        )
    }
}
