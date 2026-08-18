//! LR-9: the engine owns every runtime.
//!
//! One Tokio multi-thread runtime for I/O and producer work.
//! One `LocalSet` for query execution (the Trustfall fork's result stream is
//! `!Send` — that is a design constraint of the pinned fork, not an oversight,
//! and must not be "fixed" by blocking the caller).
//!
//! `lindsey` never creates a Tokio runtime of its own. Everything async goes
//! through the `EngineHandle` returned by [`Engine::start`].

use std::{collections::HashMap, path::{Path, PathBuf}, sync::{Arc, Mutex}, time::SystemTime};

use tokio::runtime::Runtime;
use tokio::sync::broadcast;
use tokio::task::LocalSet;
use tracing::info;

use nudox_ir::change::PackageLineageId;
use crate::store::{
    corpus::Corpus,
    source::IrSource,
};

use crate::{
    JobEvent, JobId, PackageHistorySpec, PackageLoadEvent, PackageSpec,
    ProducerLanguage, ProjectEvent, SyncEvent, versions::VersionRegistry,
    wire::SharedStr,
};

mod load;
mod semantic;

// The load driver and its durable failure record are re-exported so that
// `crate::runtime::drive_load` / `crate::runtime::LoadFailures` — the paths
// `doc` and `acquire` already reference — keep resolving unchanged.
pub(crate) use load::{LoadFailures, drive_load};

#[derive(Clone, Debug, PartialEq, Eq)]
struct ManifestStamp {
    modified: Option<SystemTime>,
    len: u64,
    language: ProducerLanguage,
}

pub(crate) async fn watch_project(
    root: PathBuf, generation: crate::wire::Gen, tx: flume::Sender<ProjectEvent>,
    cancel: tokio_util::sync::CancellationToken,
) {
    let initial = match scan_manifests(&root) {
        Ok(snapshot) => snapshot,
        Err(error) => {
            let _ = tx.send(ProjectEvent::Error { message: SharedStr::from(format!("{}: {error}", root.display())) });
            let _ = tx.send(ProjectEvent::Done { generation });
            return;
        }
    };
    for (path, stamp) in &initial {
        if tx.send(ProjectEvent::Discovered { path: path.clone(), language: stamp.language }).is_err() { return; }
    }
    if tx.send(ProjectEvent::Done { generation }).is_err() { return; }
    let mut previous = initial;
    let mut ticker = tokio::time::interval(std::time::Duration::from_millis(250));
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        tokio::select! { () = cancel.cancelled() => return, _ = ticker.tick() => {} }
        let current = match scan_manifests(&root) {
            Ok(snapshot) => snapshot,
            Err(error) => {
                if tx.send(ProjectEvent::Error { message: SharedStr::from(format!("{}: {error}", root.display())) }).is_err() { return; }
                continue;
            }
        };
        for (path, stamp) in &current {
            match previous.get(path) {
                None => if tx.send(ProjectEvent::Discovered { path: path.clone(), language: stamp.language }).is_err() { return; },
                Some(old) if old != stamp => if tx.send(ProjectEvent::Changed { path: path.clone() }).is_err() { return; },
                Some(_) => {}
            }
        }
        for path in previous.keys().filter(|path| !current.contains_key(*path)) {
            if tx.send(ProjectEvent::Removed { path: path.clone() }).is_err() { return; }
        }
        previous = current;
    }
}

fn scan_manifests(root: &Path) -> std::io::Result<HashMap<PathBuf, ManifestStamp>> {
    if !root.is_dir() { return Err(std::io::Error::new(std::io::ErrorKind::NotFound, "project root is not a directory")); }
    let mut pending = vec![root.to_owned()];
    let mut manifests = HashMap::new();
    while let Some(directory) = pending.pop() {
        for entry in std::fs::read_dir(directory)? {
            let entry = entry?;
            let path = entry.path();
            let file_type = entry.file_type()?;
            if file_type.is_dir() {
                let name = entry.file_name();
                if name != ".git" && name != "target" && name != "node_modules" { pending.push(path); }
                continue;
            }
            if !file_type.is_file() { continue; }
            let Some(language) = manifest_language(&path) else { continue; };
            let metadata = entry.metadata()?;
            manifests.insert(path, ManifestStamp { modified: metadata.modified().ok(), len: metadata.len(), language });
        }
    }
    Ok(manifests)
}

pub(crate) fn sync_specs(root: &Path) -> std::io::Result<Vec<crate::PackageSpec>> {
    let manifests = scan_manifests(root)?;
    let mut specs = Vec::new();
    for (path, stamp) in manifests {
        if stamp.language != ProducerLanguage::Rust { continue; }
        let text = std::fs::read_to_string(&path)?;
        let value: toml::Value = text.parse().map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, format!("{}: {e}", path.display())))?;
        let package = value.get("package").and_then(toml::Value::as_table)
            .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::InvalidData, "Cargo.toml has no [package]"))?;
        let name = package.get("name").and_then(toml::Value::as_str)
            .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::InvalidData, "package.name is missing"))?;
        let version = package.get("version").and_then(toml::Value::as_str).unwrap_or("0.0.0");
        specs.push(crate::PackageSpec { root: path.parent().unwrap().to_owned(), name: name.to_owned(), version: version.to_owned(), language: ProducerLanguage::Rust });
    }
    specs.sort_by(|a, b| a.root.cmp(&b.root));
    Ok(specs)
}

fn manifest_language(path: &Path) -> Option<ProducerLanguage> {
    match path.file_name()?.to_str()? {
        "Cargo.toml" => Some(ProducerLanguage::Rust),
        "go.mod" => Some(ProducerLanguage::Go),
        "pom.xml" | "build.gradle" | "build.gradle.kts" => Some(ProducerLanguage::Java),
        "pyproject.toml" | "setup.py" => Some(ProducerLanguage::Python),
        "package.json" => Some(ProducerLanguage::TypeScript),
        _ if path.extension().and_then(|e| e.to_str()) == Some("csproj") => Some(ProducerLanguage::CSharp),
        "CMakeLists.txt" | "meson.build" | "BUILD" | "BUILD.bazel" => Some(ProducerLanguage::Cpp),
        _ => None,
    }
}

use load::package_root_key;
use semantic::semantic_indexer;

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
    /// `None` → [`crate::packages::acquire::default_cache_dir`], which honours
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
    /// Root of this host's local, libpijul-backed IR store — one
    /// `ir_vcs::IrRepository` per resident package lineage under this
    /// directory. See `crate::store::persistence` and
    /// `docs/IR-STORAGE-PLAN.md` §1a/P5.
    ///
    /// `None` is the *default* and means **no local persistence**: every
    /// launch re-runs the producer over every package, exactly the behaviour
    /// before this field existed. That is deliberate, not an oversight —
    /// unlike `package_cache` (which only ever holds ephemeral, re-fetchable
    /// unpacked archives), this directory accumulates a libpijul pristine +
    /// changestore per resident package, indefinitely, and a library must
    /// never start writing files of its own accord that a caller did not ask
    /// for. A host that wants IR to survive a restart opts in explicitly —
    /// exactly the way `heart::deployment::DeploymentProfile::embedded()`
    /// already provisions `ir_repo_root: PathBuf` for the embedded GUI shape
    /// (*"Root of the local libpijul `IrRepository`"*): pass that path (or any
    /// directory this process owns) here to turn persistence on.
    ///
    /// On a successful produce the engine records the generation here
    /// (`runtime::load::drive_load`); on start, everything already recorded
    /// here is materialized into the corpus **before** the configured
    /// sources are driven, so a lineage this store already holds is served
    /// from disk rather than re-produced (`store::persistence::PersistedSource`).
    /// Re-indexing unchanged sources does not grow the store: recording
    /// defers to `IrRepository::record_generation`'s own per-symbol content
    /// diff, which is a true no-op (no libpijul change recorded at all) when
    /// nothing differs.
    ///
    /// Ignored unless this crate is built with the `local-persistence`
    /// feature (in this crate's `default` feature set — see `Cargo.toml`).
    /// With that feature off, this field is still present — so `EngineConfig`
    /// has the same shape regardless of which features a caller built this
    /// crate with — but every value is treated as `None`.
    pub ir_repo_root: Option<PathBuf>,
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
            .field("ir_repo_root", &self.ir_repo_root)
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
    pub(crate) acquire: Arc<crate::packages::acquire::AcquireContext>,
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
    pub(crate) semantic_tx: Option<flume::Sender<Arc<crate::store::package::PackageView>>>,
    pub(crate) jobs: Mutex<HashMap<JobId, JobStatus>>,
    pub(crate) job_tx: broadcast::Sender<JobEvent>,
    pub(crate) sync_tx: broadcast::Sender<SyncEvent>,
    pub(crate) next_job: std::sync::atomic::AtomicU64,
    pub(crate) cancellations: Mutex<HashMap<JobId, tokio_util::sync::CancellationToken>>,
    /// The local, libpijul-backed IR store, if `EngineConfig::ir_repo_root`
    /// was set. Consulted by `runtime::load::drive_load` on every
    /// successful, freshly-produced package (see that function's Ready arm)
    /// so a generation persists across the *next* restart the moment it is
    /// produced, not only at some later checkpoint.
    #[cfg(feature = "local-persistence")]
    pub(crate) persistence: Option<crate::store::persistence::PersistenceStore>,
}

#[derive(Clone, Debug)]
pub(crate) enum JobStatus {
    Running,
    Succeeded,
    Failed,
    Cancelled,
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
        let (job_tx, _job_rx) = broadcast::channel::<JobEvent>(256);
        let (sync_tx, _sync_rx) = broadcast::channel::<SyncEvent>(256);
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
            schema: crate::graph::schema(),
            highlighter: config.highlighter.clone(),
            pkg_tx,
            load_failures: LoadFailures::new(),
            acquire: crate::packages::acquire::AcquireContext::new(config.package_cache.clone()),
            embedder: config.embedder.clone(),
            semantic,
            semantic_tx,
            jobs: Mutex::new(HashMap::new()),
            job_tx,
            sync_tx,
            next_job: std::sync::atomic::AtomicU64::new(1),
            cancellations: Mutex::new(HashMap::new()),
            #[cfg(feature = "local-persistence")]
            persistence: config
                .ir_repo_root
                .clone()
                .map(crate::store::persistence::PersistenceStore::new),
        });

        // Materialize anything the local store already holds before driving
        // `source` — `PersistedSource` (a no-op wrapper when `ir_repo_root`
        // is `None`) is what makes a restart with no explicit specs at all
        // still repopulate the corpus, and what stops a spec whose source
        // tree has since been deleted from ever reaching a real producer for
        // a lineage the local store already answers for. See
        // `store::persistence`'s module docs.
        #[cfg(feature = "local-persistence")]
        let source = crate::store::persistence::PersistedSource::new(config.ir_repo_root, source);

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
/// inline — `crate::mcp`'s index-job registry is the first such caller, and the
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
            .finish_non_exhaustive()
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
    /// `crate::packages::acquire::classify_load_failure`).
    pub(crate) async fn load_one(
        &self,
        spec: crate::PackageSpec,
    ) -> Result<crate::packages::acquire::LoadedPackage, SharedStr> {
        use crate::store::source::producer::{ProducerRegistry, ProducerSource};

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
                ..
            }) => Ok(crate::packages::acquire::LoadedPackage {
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
        corpus: crate::store::corpus::Corpus,
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
            let snapshot: Vec<Arc<crate::store::package::PackageView>> = corpus.packages().await;

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
                    metadata: Box::new(pkg.metadata().clone()),
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
        Self::start_with_fixture_set(
            config,
            crate::store::source::fixtures::FixtureSet::RichOnly,
        )
    }

    /// Start an engine over one of the deterministic fixture corpus sizes.
    ///
    /// This is primarily useful to integration benches that need to compare a
    /// relationship-rich package with a large, repetitive package while still
    /// exercising the production engine and MCP server path.
    #[cfg(feature = "fixtures")]
    pub fn start_with_fixture_set(
        config: EngineConfig,
        set: crate::store::source::fixtures::FixtureSet,
    ) -> EngineHandle {
        Self::start(
            config,
            crate::store::source::fixtures::FixtureSource { set },
        )
    }

    /// Start an engine that produces IR for real on-disk packages.
    ///
    /// # Why [`PackageSpec`] and not [`crate::store::source::producer::PackageDescriptor`]
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
        use crate::store::source::producer::{PackageDescriptor, ProducerRegistry, ProducerSource};

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
        generations: Vec<(&str, Arc<crate::store::package::PackageView>)>,
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
