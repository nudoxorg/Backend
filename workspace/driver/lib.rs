//! The Server — the coordination mesh and the single request entrypoint.
//!
//! The server fronts a **federation** of registries ([`heart::Federation`]): one
//! definitive base (centrally hosted) plus zero or more self-hosted overlays
//! that extend and override it. Each source is a full stack of *connected*
//! (`Live`) backing stores, so the server is only constructible once every store
//! of every source has verified — "served a request against a store that wasn't
//! up" is a compile error. Assembly brings each source's stores up
//! **concurrently** through the shared [`heart::Connect`] trait and one uniform
//! [`ServerError`].
//!
//! Fields are private; the read/coordination flows reach them through accessors
//! (the common single-source path delegates to the definitive base).

#![feature(return_type_notation)]

// ── Layer-facade (§8 re-layering compatibility shim) ──────────────────────────
// The staged `server` modules were authored against the *old monolithic
// `registry`* surface, where the data/storage/coordination plane, the graph
// plane and the vector plane all hung off one crate. Post-relayering they are
// split three ways: `index` (data/storage/coordination), `registry`
// (`graph` + `vector`) and `heart` (`client`). Rather than rewrite every `use`
// across ~13k lines of moved composition, this crate re-projects the three
// layers under the paths the moved code expects:
//
//   * a crate-local `mod registry` that *shadows* the extern-prelude `registry`
//     crate: it re-exports the `index` modules under their old names PLUS the
//     real registry's `graph`/`vector`. Bare `registry::…` and `crate::registry::…`
//     paths in the moved modules both resolve here.
//   * a crate-local `mod vector` re-exporting `::registry::vector` (+ its `core`
//     submodules flattened) so `vector::shard`/`vector::quant`/… keep resolving.
//
// The real crates remain reachable as `::index`, `::registry`, `::heart`.
#[allow(unused_imports)]
pub mod registry {
    //! Facade: the old monolithic `registry` surface, re-projected onto the
    //! post-relayering `index` (data plane) + `::registry` (graph + vector).

    // Data / storage / coordination plane → `index`.
    pub use ::index::{
        bakery, blob, cas, catalog, compiled, coordination, error, health, identity, metadata,
        pack, package, protocol, queue, resolve, schema, search, upstream,
    };
    // `ingest` = the untrusted-archive extraction plane, now owned by
    // `index::ingest::archive` per the first redistribution wave.
    // Callers must use the new owning module path directly.

    /// Runtime sub-facade: `index::runtime` error types plus the
    /// session module (exploration-graph semilattice, now owned by
    /// `index::runtime::session` per the first redistribution wave).
    pub mod runtime {
        pub use ::index::runtime::error::{RuntimeError, SessionError, TextError};
        pub use ::index::runtime::session;
        pub use ::index::runtime::*;
    }
    // The old `registry::index` module is now `index::catalog`.
    pub use ::index::catalog as index;
    // The old `registry::store::Store` is now `index::cas::Store`; re-export the
    // module and the type at the facade root (moved code says `registry::Store`).
    pub use ::index::cas as store;
    pub use ::index::{
        BlobError, GlobalPackage, IngestError, Package, QueueError, RegistryError, Store,
        StoreError,
    };

    // Graph + vector planes → real `::registry`.
    pub use ::xregistry::{graph, vector};
}

/// Facade: the old top-level `vector` crate, now `::registry::vector`. The
/// `core` submodules (shard/quant/model/routing/admission/recipe/store/embed/…)
/// were top-level in the standalone crate; flatten them back so `vector::shard`
/// et al. resolve.
#[allow(unused_imports)]
pub mod vector {
    pub use ::xregistry::vector::core::{
        admission, embed, embedding, fusion, key, model, quant, recipe, routing, shard, store,
    };
    pub use ::xregistry::vector::*;
    pub use ::xregistry::vector::{cache, gate, local, remote};
}

pub mod authz;
pub mod bakery;
// `session` moved to `index::runtime::session` (first redistribution wave).
// `ingest` moved to `index::ingest::archive` (first redistribution wave).
// `compiler_client` DELETED — the compiler daemon it spoke to no longer exists
// (the cage is ephemeral, SMOLVM-PLAN). See CONSOLIDATION-NOTES §9c. The compile
// call in the indexing pipeline was stubbed on the `index` side.
pub mod config;
pub mod coordination;
pub mod error;
pub mod http;
mod poll;
pub mod rerank;
pub mod save;
pub mod search;
/// The federation-aware sync driver (CONSOLIDATION-NOTES §8e): fan a verified
/// content item across the federation topology over `heart::sync` + `transport`.
pub mod sync;

use std::num::NonZeroU32;
use std::sync::Arc;
use std::time::Duration;

use crate::registry::vector::{EmbeddingCache, EmbeddingModel};
use crate::registry::{
    Store,
    coordination::Outbox,
    index::{GlobalStore, InstanceToken},
    metadata::{Specifics, Synonyms},
    queue::{Queue, RetryPolicy},
};
use crate::vector::remote::store::{CollectionConfig, RemoteStore, ensure_collection};
use heart::{BackendKind, Connect, ConnectError, ConnectFailure, Federation, Live};
use index::compiled::ObjectCompiledStore;
use index::runtime::session::ScratchSessionStore;
use index::store::writer::CatalogWriter;

pub use config::{Deployment, Endpoints, Limits, Role, ServerConfiguration, SourceConfig};
use error::InternalError;
pub use error::{ServerError, ServerResult};

/// The composition struct is `Driver<M>` (§8: the client/driver composes the
/// `index` + `registry` layers — there is no monolithic server). The moved
/// modules still spell it `Server<M>` throughout; this alias keeps those paths
/// resolving during the dissolution without a mechanical crate-wide rename.
pub type Server<M> = Driver<M>;

use crate::search::registry::PackageSearchIndex;
use crate::search::semantic::embedder::HttpEmbedder;

/// The retry schedule every source's job queue runs under: five attempts with
/// exponential backoff between one second and five minutes.
const INDEXING_RETRY_POLICY: RetryPolicy = RetryPolicy {
    max_attempts: NonZeroU32::new(5).expect("five is non-zero"),
    base_backoff: Duration::from_secs(1),
    max_backoff: Duration::from_secs(300),
};

/// How many query-text embeddings the in-process cache retains.
const EMBEDDING_CACHE_CAPACITY: u64 = 65_536;

/// The one catalog engine the server links: real DoltLite (INDEX-PLAN ID-1;
/// the `index` crate's `dolt-engine` feature). Tests inside `index`/`registry`
/// use the in-memory facade; the serving binary is versioned-catalog-only.
pub type CatalogEngine = index::engine::dolt::DoltEngine;

/// The full set of connected backing stores for one federated source. Every
/// source — definitive or overlay — is a complete, independently-verified stack.
pub struct SourceStores<M: EmbeddingModel> {
    /// Global index / orchestration spine (the versioned catalog).
    pub global_store: GlobalStore<CatalogEngine>,
    /// Content-addressed blob store (object store).
    pub blobs: Store<Live>,
    /// Durable, poison-pill-safe job queue (scratch-backed).
    pub queue: Queue,
    /// Transactional outbox for derived-store fan-out (catalog-backed).
    pub outbox: Outbox<CatalogEngine>,
    /// Semantic/vector store (qdrant): the remote index client, bound to the
    /// collection schema for the compiled-in model brand `M`.
    pub semantics: RemoteStore<M>,
    /// Replica-local package-search index (tantivy over the package catalog
    /// projection), shared with the sync poller.
    pub packages: Arc<PackageSearchIndex>,
}

/// The assembled server, generic over the embedding-model brand `M` (lifted into
/// the type so a wrong-*model* vector store — not merely wrong-dimension — can
/// never be wired in).
pub struct Driver<M: EmbeddingModel> {
    /// The resolved configuration this server was built from.
    config: ServerConfiguration,

    /// The federation of sources: the definitive base plus overlays, each a full
    /// connected stack. Reads resolve overlay-first (override), then base.
    federation: Federation<SourceStores<M>>,

    /// The query planner — the only place a *user-facing* semantic gate is
    /// minted ([`registry::vector::SemanticGate::issue`]). Store readiness uses
    /// the separate [`registry::vector::SemanticGate::for_readiness`] constructor.
    planner: crate::search::SearchPlanner,

    /// The (model-branded) query embedder behind the gated semantic path.
    embedder: HttpEmbedder<M>,

    /// The content-addressed embedding cache in front of the embedder.
    embedding_cache: EmbeddingCache<M>,

    /// Per-session exploration graphs (join-semilattice merge), backed by the
    /// ephemeral `scratch.sqlite` store (INDEX-PLAN ID-2/ID-19). The backing
    /// `sessions` table is created when the scratch store is opened.
    sessions: ScratchSessionStore,

    /// Keyword-normalization heuristics (synonyms + specifics), loaded once from
    /// `config.metadata_data_dir`. `None` disables them (the extractor then runs
    /// with `(None, None)`, name/identifier facets only). Loaded once because the
    /// tables are expensive to parse and immutable for the process lifetime.
    heuristics: Option<Heuristics>,

    /// The compiled-output lookup store for `POST /v1/compiled/lookup` (SV-6),
    /// sharing the definitive base's object-store backend. Records are written
    /// by the iroh SyncService apply-hook via [`registry::compiled::ObjectCompiledStore::record`]
    /// after a merged change-set is verified and applied.
    compiled_store: ObjectCompiledStore,

    /// The shard-bakery claim ledger + artifact index (the catalog\'s
    /// `edgepack_artifacts` table). `None` when `config.bakery.enabled` is false — callers
    /// check with [`Self::edgepacks`] before using.
    edgepacks: Option<std::sync::Arc<crate::bakery::CatalogEdgepackStore>>,

    /// The reverse-position usage-query backend for `Target::Usages`
    /// ([`registry::graph::ReversePositionIndex`], INDEX-PLAN §5.5). Constructed
    /// [`empty`](crate::registry::search::ReverseIndexUsageBackend::empty) at
    /// assembly — no package IR is materialized in-process yet — so usage queries
    /// return the honest `IndexUnavailable` (`503`) until a scope is loaded. The
    /// wiring is real: swapping in a loaded backend (an IR view + its reverse
    /// index) makes the route serve live results with no other change.
    usage_backend: crate::registry::search::ReverseIndexUsageBackend,
}

/// The metadata keyword-normalization tables, loaded once at assembly and shared
/// (read-only) across every facet extraction. Bundling both keeps the "either
/// both wired or neither" invariant the extractor expects.
pub struct Heuristics {
    synonyms: Synonyms,
    specifics: Specifics,
}

impl Heuristics {
    /// Load both heuristics tables from `dir` (expects `tag-synonyms.csv`,
    /// `specific-keywords.txt`, `bland-keywords.txt`). Fails loudly if the
    /// directory is configured but the files cannot be parsed — no silent
    /// half-wired state (parse, don't validate).
    pub fn load(dir: &std::path::Path) -> std::io::Result<Self> {
        Ok(Self {
            synonyms: Synonyms::new(dir)?,
            specifics: Specifics::new(dir)?,
        })
    }

    /// The synonym table, ready to pass to `crate::registry::metadata::rich::extract`.
    pub fn synonyms(&self) -> &Synonyms {
        &self.synonyms
    }

    /// The specifics table, ready to pass to `crate::registry::metadata::rich::extract`.
    pub fn specifics(&self) -> &Specifics {
        &self.specifics
    }
}

impl<M: EmbeddingModel> Driver<M> {
    /// Assemble a server: connect the definitive base and every overlay
    /// concurrently, then materialize the federation. Any single connection
    /// failure of any source aborts assembly with a context-rich [`ServerError`].
    ///
    /// Access control for hosted deployments is enforced at the fronting proxy
    /// (see [`heart::access`]); there is no in-process policy trait yet.
    pub async fn assemble(config: ServerConfiguration) -> ServerResult<Self> {
        config.validate()?;
        http::dto::register_custom_registries(&config.custom_registries);

        // The definitive base is the federation's required root.
        let base = Self::connect_source(&config.definitive).await?;
        // The compiled-lookup store rides the base's own object-store backend —
        // one namespace for cas/, ptr/, and compiled/ (SV-6).
        let compiled_store = ObjectCompiledStore::new(base.blobs.backend());
        let mut federation = Federation::new(config.definitive.source_id(), base);

        // Overlays, in declared precedence order (highest first).
        for overlay in &config.overlays {
            let stores = Self::connect_source(overlay).await?;
            federation = federation.with_overlay(overlay.source_id(), stores);
        }

        let endpoints = &config.definitive.endpoints;
        let embedder = HttpEmbedder::new(
            endpoints.embeddings.clone(),
            endpoints.embeddings_api_key.clone(),
        );
        let embedding_cache = EmbeddingCache::new(EMBEDDING_CACHE_CAPACITY);

        // Exploration-graph sessions live in the ephemeral scratch store
        // (`scratch.sqlite`) under the definitive source's data directory — working
        // state, delete-anytime, never replicated (INDEX-PLAN ID-2). Opening the
        // store creates the `sessions` table, so there is no separate migrate step.
        let scratch_dir = config.definitive.data_directory();
        std::fs::create_dir_all(&scratch_dir).map_err(|error| {
            ServerError::Runtime(registry::runtime::error::TextError::Io(error).into())
        })?;
        let scratch_store = index::scratch::ScratchStore::open(&scratch_dir.join("scratch.sqlite"))
            .map_err(|error| {
                ServerError::Runtime(registry::runtime::error::SessionError::Scratch(error).into())
            })?;
        let sessions = ScratchSessionStore::new(scratch_store);

        // The compiler daemon is gone (the cage is ephemeral, SMOLVM-PLAN); no
        // long-lived compile client is constructed. The indexing pipeline's
        // compile step is stubbed on the `index` side.

        // Load keyword-normalization heuristics once, if a data dir is configured.
        // A configured-but-unloadable dir aborts assembly rather than silently
        // degrading to name-only facets.
        let heuristics = match &config.metadata_data_dir {
            Some(dir) => Some(Heuristics::load(dir).map_err(|error| {
                invalid_configuration(format!(
                    "metadata_data_dir {} configured but heuristics failed to load: {error}",
                    dir.display()
                ))
            })?),
            None => None,
        };

        // The bakery claim-ledger + artifact index rides the definitive base's
        // catalog (`edgepack_artifacts` table, migrated with schema v4) and
        // scratch claims — no separate migration step.
        let edgepacks = if config.bakery.enabled || config.depshards.enabled {
            Some(std::sync::Arc::new(
                crate::bakery::CatalogEdgepackStore::new(std::sync::Arc::clone(
                    federation.base().global_store.writer(),
                )),
            ))
        } else {
            None
        };

        Ok(Self {
            config,
            federation,
            planner: crate::search::SearchPlanner::new(),
            embedder,
            embedding_cache,
            sessions,
            heuristics,
            compiled_store,
            edgepacks,
            // No package IR is materialized in-process at assembly, so the usage
            // backend starts empty (queries answer `IndexUnavailable`/503, never a
            // fake empty). A loaded scope is swapped in when IR is materialized.
            usage_backend: crate::registry::search::ReverseIndexUsageBackend::empty(),
        })
    }

    /// The reverse-position usage-query backend behind `Target::Usages`.
    /// Empty until a package's IR view + reverse index is loaded; queries then
    /// answer the honest `IndexUnavailable` rather than a `501` or a fake empty
    /// page (INDEX-PLAN §5.5).
    pub(crate) fn usage_backend(&self) -> &crate::registry::search::ReverseIndexUsageBackend {
        &self.usage_backend
    }

    /// The keyword-normalization heuristics, if a `metadata_data_dir` was
    /// configured. `None` means facet extraction runs without synonym/specifics
    /// normalization.
    pub(crate) fn heuristics(&self) -> Option<&Heuristics> {
        self.heuristics.as_ref()
    }

    /// Build and connect the full store stack for one configured source, bringing
    /// its backends up concurrently.
    async fn connect_source(cfg: &SourceConfig) -> ServerResult<SourceStores<M>> {
        let endpoints = &cfg.endpoints;

        // The versioned catalog underlies every relational store of this source
        // (index, outbox, package-search hydration). It is a local DoltLite
        // engine: open, migrate, then share one single-writer handle
        // (INDEX-PLAN ID-1/ID-5).
        std::fs::create_dir_all(&endpoints.catalog_directory).map_err(|error| {
            ConnectError::new(
                BackendKind::Catalog,
                ConnectFailure::Io(std::io::Error::other(error)),
            )
        })?;
        let engine = CatalogEngine::open(&endpoints.catalog_directory.join("catalog.dolt"))
            .map_err(|error| {
                ConnectError::new(
                    BackendKind::Catalog,
                    ConnectFailure::Io(std::io::Error::other(error)),
                )
            })?;
        index::migrations::runner::migrate_to_v4(&engine).map_err(|error| {
            ConnectError::new(
                BackendKind::Catalog,
                ConnectFailure::Io(std::io::Error::other(error)),
            )
        })?;
        let writer = Arc::new(CatalogWriter::new(engine));

        // The queue + claims live in this source's ephemeral scratch store
        // (INDEX-PLAN ID-2); a second connection to the same file the session
        // store uses is fine (sqlite serializes).
        let scratch_dir = cfg.data_directory();
        std::fs::create_dir_all(&scratch_dir).map_err(|error| {
            ConnectError::new(
                BackendKind::Catalog,
                ConnectFailure::Io(std::io::Error::other(error)),
            )
        })?;
        let queue_scratch = index::scratch::ScratchStore::open(&scratch_dir.join("scratch.sqlite"))
            .map_err(|error| {
                ConnectError::new(
                    BackendKind::Catalog,
                    ConnectFailure::Io(std::io::Error::other(error)),
                )
            })?;

        let instance = InstanceToken::new(endpoints.instance_token())
            .map_err(crate::registry::RegistryError::from)?;

        // Catalog-backed stores construct directly (no remote handshake to
        // verify); network-backed stores keep the Connect typestate below.
        let global_store = GlobalStore::new(Arc::clone(&writer), instance);
        let queue = Queue::new(
            Arc::new(std::sync::Mutex::new(queue_scratch)),
            format!("server:{}", cfg.source_id()),
            INDEXING_RETRY_POLICY,
        );
        let outbox = Outbox::new(Arc::clone(&writer));

        let blobs_cold: Store<heart::Cold> =
            Store::new(object_store_backend(&endpoints.object_store)?);

        // The remote vector store binds to the frozen collection schema for the
        // compiled-in model brand `M` (`CollectionConfig::for_model`). There is no
        // `Connect` typestate on the new plane: constructing the qdrant client and
        // bootstrapping the collection *is* the connection. `ensure_collection` is
        // idempotent (skips if the collection exists) and does the network round
        // trip that surfaces a bad endpoint, so it stands in for the old
        // `Semantic::connect` handshake.
        let qdrant = Arc::new(
            qdrant_client::Qdrant::from_url(endpoints.qdrant.as_str())
                .build()
                .map_err(|error| {
                    ConnectError::new(
                        BackendKind::Qdrant,
                        ConnectFailure::Io(std::io::Error::other(error)),
                    )
                })?,
        );
        let collection_config = CollectionConfig::for_model::<M>();
        ensure_collection(&qdrant, collection_config)
            .await
            .map_err(|error| {
                ConnectError::new(
                    BackendKind::Qdrant,
                    ConnectFailure::Io(std::io::Error::other(error)),
                )
            })?;
        let semantics = RemoteStore::new(qdrant, collection_config);

        // The blob store still carries the `Connect` typestate; bring it up.
        let blobs = blobs_cold.connect().await?;

        // The package-search index is replica-local, fed by the sync poller off
        // the catalog outbox feed.
        let packages_dir = cfg.package_index_directory();
        std::fs::create_dir_all(&packages_dir).map_err(|error| {
            ServerError::Runtime(registry::runtime::error::TextError::Io(error).into())
        })?;
        let packages = Arc::new(PackageSearchIndex::open(&packages_dir)?);

        Ok(SourceStores {
            global_store,
            blobs,
            queue,
            outbox,
            semantics,
            packages,
        })
    }

    /// The resolved configuration.
    pub fn config(&self) -> &ServerConfiguration {
        &self.config
    }

    /// The full federation of sources (base + overlays), for queries that resolve
    /// across sources with overlay-override precedence.
    pub fn federation(&self) -> &Federation<SourceStores<M>> {
        &self.federation
    }

    /// The definitive base source's stores — the common single-source path.
    pub fn base(&self) -> &SourceStores<M> {
        self.federation.base()
    }

    // ── Convenience accessors delegating to the definitive base. Federated flows
    // use `federation()` to walk overlays-then-base; single-source flows use these.

    /// The definitive base's global index handle.
    pub fn global_store(&self) -> &GlobalStore<CatalogEngine> {
        &self.base().global_store
    }

    /// The definitive base's blob store handle.
    pub fn blobs(&self) -> &Store<Live> {
        &self.base().blobs
    }

    /// The definitive base's job queue handle.
    pub fn queue(&self) -> &Queue {
        &self.base().queue
    }

    /// The definitive base's outbox handle.
    pub fn outbox(&self) -> &Outbox<CatalogEngine> {
        &self.base().outbox
    }

    /// The definitive base's semantic store handle.
    pub fn semantics(&self) -> &RemoteStore<M> {
        &self.base().semantics
    }

    /// The model-branded query embedder behind the gated semantic path — also the
    /// embedder the vector fan-out consumer drives to materialize symbol points.
    pub(crate) fn embedder(&self) -> &HttpEmbedder<M> {
        &self.embedder
    }

    /// The content-addressed embedding cache in front of the embedder.
    pub(crate) fn embedding_cache(&self) -> &EmbeddingCache<M> {
        &self.embedding_cache
    }

    /// The per-session exploration graphs (scratch-backed; process-local).
    pub fn sessions(&self) -> &ScratchSessionStore {
        &self.sessions
    }

    /// The compiled-output lookup store for `POST /v1/compiled/lookup` (SV-6).
    ///
    /// Returns a reference to the store so the handler can call `.lookup()`.
    /// Backed by the definitive base's object store: reads the
    /// `compiled/{job_key}` records the fleet's emit pipeline writes.
    pub fn compiled_store(&self) -> &ObjectCompiledStore {
        &self.compiled_store
    }

    /// The shard-bakery claim ledger (`edgepack_artifacts` table), or `None`
    /// when neither `bakery.enabled` nor `depshards.enabled` is true (the table
    /// was not created). Handlers that serve dep-shard manifests always check
    /// before querying; the bakery worker checks before starting.
    pub fn edgepacks(&self) -> Option<&crate::bakery::CatalogEdgepackStore> {
        self.edgepacks.as_deref()
    }

    /// The optional reranker service, built from `config.rerank`. `None` when
    /// no endpoint is configured (calls return `rerank_unavailable`).
    pub fn reranker(&self) -> Option<&crate::rerank::HttpProxyReranker> {
        // The reranker is built lazily by the handler on first call, or could
        // be pre-built here. For now the handler constructs it from the config.
        None
    }

    /// Serve until shutdown: bind the HTTP router to `config.serving_address` and
    /// run the background pollers (queue workers + derived-store consumers) for
    /// every source in the federation.
    pub async fn serve(self: Arc<Self>) -> ServerResult<()> {
        let listener = tokio::net::TcpListener::bind(self.config.serving_address)
            .await
            .map_err(|source| {
                ServerError::Internal(InternalError::BindFailed {
                    address: self.config.serving_address,
                    source,
                })
            })?;
        self.serve_on(listener).await
    }

    /// Serve on an already-bound listener.
    ///
    /// Production uses [`Self::serve`] so configuration owns the listen address.
    /// Tests and embedders use this entry point to keep the listener reservation
    /// across setup and to discover an ephemeral port without a bind-then-release
    /// race.
    pub async fn serve_on(self: Arc<Self>, listener: tokio::net::TcpListener) -> ServerResult<()> {
        // The `metrics` recorder behind `/metrics` (fanned out to OTLP) is
        // installed by `heart::telemetry::init` in `main`, before any request or
        // poller can emit a metric — no per-serve install here anymore.
        let router = http::router::router(Arc::clone(&self));
        tracing::info!(address = ?listener.local_addr().ok(), "serving");

        // Supervised background pollers, gated by this node's role. The queue
        // worker runs on forge/compile nodes; the derived-store fan-out consumers
        // and the replica-local index sync/watermark loops run on gateway nodes.
        // `All` (the default) runs both; every role still serves the HTTP surface.
        let role = self.config.role;
        let mut pollers = tokio::task::JoinSet::new();

        // Graceful-drain signal for the queue worker: on shutdown we fire it,
        // let the worker stop dequeuing new jobs and finish in-flight ones (up to
        // a bounded deadline), then abort whatever remains.
        let drain = tokio_util::sync::CancellationToken::new();
        let mut queue_worker: Option<tokio::task::JoinHandle<()>> = None;
        if role.runs_forge() {
            queue_worker = Some(tokio::spawn(poll::queue_worker(
                Arc::clone(&self),
                drain.clone(),
            )));
        }
        if role.runs_gateway() {
            // The bakery worker scans for packages that need edge-shard baking.
            // Gated by `config.bakery.enabled` (default false) so it never runs
            // unless the operator explicitly opts in.
            if self.config.bakery.enabled {
                pollers.spawn(crate::bakery::bakery_worker(Arc::clone(&self)));
            }

            for sink in <heart::DerivedStore as strum::IntoEnumIterator>::iter() {
                pollers.spawn(poll::outbox_consumer(Arc::clone(&self), sink));
            }
            pollers.spawn(poll::package_index_poller(Arc::clone(&self)));
            pollers.spawn(poll::package_signals_poller(Arc::clone(&self)));
            // Storage reclamation (CAS GC): purge consumed outbox rows below every
            // sink's watermark. The delete is idempotent, so overlapping replicas
            // are harmless — no advisory lock needed. Blob GC is a documented TODO
            // (see `poll::cas_gc`) pending a safe live-reference oracle.
            pollers.spawn(poll::cas_gc(Arc::clone(&self)));

            // Catalog followers (M2): one task per configured ecosystem. The list
            // defaults to empty (demand-pull only); operators opt in via
            // `[mirror] follow = ["csharp", "rust"]`. Each follower is built here
            // and driven by `catalog_follower_worker`.
            for lang_str in &self.config.mirror.follow {
                use std::str::FromStr;
                let Ok(lang) = index::ecosystem::Language::from_str(lang_str.as_str()) else {
                    tracing::warn!(lang = %lang_str, "unknown language in mirror.follow; skipping");
                    continue;
                };
                let follower: Box<dyn crate::registry::upstream::CatalogFollower> = match lang {
                    index::ecosystem::Language::CSharp => {
                        Box::new(registry::upstream::NuGetCatalogFollower::production())
                    }
                    index::ecosystem::Language::Rust => {
                        Box::new(registry::upstream::CratesCatalogFollower::production())
                    }
                    other => {
                        tracing::warn!(%other, "no catalog follower implemented for language; skipping");
                        continue;
                    }
                };
                tracing::info!(%lang, "starting catalog follower");
                pollers.spawn(poll::catalog_follower_worker(Arc::clone(&self), follower));
            }
        }
        tracing::info!(?role, "background pollers started");

        axum::serve(listener, router)
            .with_graceful_shutdown(shutdown_signal())
            .await
            .map_err(|source| ServerError::Internal(InternalError::ServeFailed { source }))?;

        // The HTTP listener has drained. Wind down in two phases:
        //
        // 1. **Graceful drain of the queue worker.** Signal it to stop dequeuing;
        //    its in-flight jobs run to completion (or their own deadline). We wait
        //    at most `drain_deadline` for the worker's clean return before forcing
        //    it — a wedged job cannot hold shutdown open forever. Idempotent job
        //    settling means a forced abort here re-delivers rather than corrupts.
        if let Some(handle) = queue_worker {
            tracing::info!("draining queue worker (no new jobs; finishing in-flight)");
            drain.cancel();
            let deadline = self.config.limits.drain_deadline;
            match tokio::time::timeout(deadline, handle).await {
                Ok(Ok(())) => tracing::info!("queue worker drained cleanly"),
                Ok(Err(join)) => {
                    tracing::warn!(error = %join, "queue worker task ended abnormally")
                }
                Err(_) => tracing::warn!(
                    ?deadline,
                    "drain deadline exceeded; forcing queue worker down"
                ),
                // `handle` is dropped on timeout, aborting the still-running task at
                // its next await point — the same idempotent-abort semantics as below.
            }
        }

        // 2. **Abort the remaining (gateway) pollers** at their next await point
        //    and reap them so nothing outlives the server. Every unit of poller
        //    work is idempotent, so an abort mid-tick is safe.
        tracing::info!("shutting background pollers down");
        pollers.abort_all();
        while pollers.join_next().await.is_some() {}
        Ok(())
    }
}

/// Resolve an object-store URL into a backend handle. The vendored
/// `object_store` build enables no cloud features, so `file://` (and
/// `memory://`) schemes are what this deployment can speak; the parse error for
/// anything else names the scheme.
fn object_store_backend(url: &url::Url) -> ServerResult<Arc<dyn object_store::ObjectStore>> {
    // A local blob root must exist before the store will accept writes.
    if url.scheme() == "file"
        && let Ok(path) = url.to_file_path()
    {
        std::fs::create_dir_all(&path).map_err(|error| {
            ConnectError::new(
                BackendKind::ObjectStore,
                ConnectFailure::Io(std::io::Error::other(error)),
            )
        })?;
    }
    let (backend, prefix) = object_store::parse_url(url).map_err(|error| {
        ConnectError::new(
            BackendKind::ObjectStore,
            ConnectFailure::Io(std::io::Error::other(error)),
        )
    })?;
    Ok(Arc::new(object_store::prefix::PrefixStore::new(
        backend, prefix,
    )))
}

/// A config-shape error surfaced while building store handles.
fn invalid_configuration(error: impl std::fmt::Display) -> ServerError {
    ServerError::Config(config::ConfigError::Validation(
        config::ConfigValidationError::Other {
            detail: error.to_string(),
        },
    ))
}

/// Resolve when the process is asked to stop: SIGINT (ctrl-c) or SIGTERM.
async fn shutdown_signal() {
    let interrupt = async {
        if tokio::signal::ctrl_c().await.is_err() {
            // No signal handler could be installed; park forever rather than
            // spuriously shutting down.
            std::future::pending::<()>().await;
        }
    };

    #[cfg(unix)]
    let terminate = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut sigterm) => {
                sigterm.recv().await;
            }
            Err(_) => std::future::pending().await,
        }
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        () = interrupt => {},
        () = terminate => {},
    }
    tracing::info!("shutdown signal received");
}
