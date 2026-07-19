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

pub mod authz;
pub mod compiler_client;
pub mod config;
pub mod coordination;
pub mod error;
pub mod http;
mod poll;
pub mod save;
pub mod search;

// The registry is now THE service crate; server's former in-tree copy was a
// stale duplicate. Re-export the crate as `crate::registry` so existing
// `crate::registry::…` paths keep resolving against the single source of truth.
pub use ::registry;

use std::num::NonZeroU32;
use std::sync::Arc;
use std::time::Duration;

use heart::{BackendKind, ConnectError, ConnectFailure, Connect, Federation, Live};
use crate::registry::{
	Store,
	coordination::Outbox,
	index::{GlobalStore, TerminusInstance},
	metadata::{Specifics, Synonyms},
	queue::{Queue, RetryPolicy},
};
use registry::compiled::ObjectCompiledStore;
use registry::runtime::{
	graph::{Credentials, Database, Graph, Organization},
	session::PgSessionStore,
	text::TextIndex,
	vector::{CollectionName, EmbeddingCache, EmbeddingModel, Semantic},
};
use secrecy::ExposeSecret;

pub use config::{Deployment, Endpoints, Limits, Role, ServerConfiguration, SourceConfig};
pub use error::{ServerError, ServerResult};
use error::InternalError;

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

/// The full set of connected backing stores for one federated source. Every
/// source — definitive or overlay — is a complete, independently-verified stack.
pub struct SourceStores<M: EmbeddingModel> {
	/// Global index / orchestration spine (postgres).
	pub global_store: GlobalStore<Live>,
	/// Content-addressed blob store (object store).
	pub blobs: Store<Live>,
	/// Durable, poison-pill-safe job queue (postgres).
	pub queue: Queue<Live>,
	/// Transactional outbox for derived-store fan-out (postgres).
	pub outbox: Outbox<Live>,
	/// Graph store (terminus).
	pub graph: Graph<Live>,
	/// Semantic/vector store (qdrant).
	pub semantics: Semantic<M, Live>,
	/// Replica-local precise-search index (tantivy) for this source.
	pub text: TextIndex,
	/// Replica-local package-search index (tantivy over the postgres package
	/// projection), shared with the sync poller.
	pub packages: Arc<PackageSearchIndex>,
}

/// The assembled server, generic over the embedding-model brand `M` (lifted into
/// the type so a wrong-*model* vector store — not merely wrong-dimension — can
/// never be wired in).
pub struct Server<M: EmbeddingModel> {
	/// The resolved configuration this server was built from.
	config: ServerConfiguration,

	/// The federation of sources: the definitive base plus overlays, each a full
	/// connected stack. Reads resolve overlay-first (override), then base.
	federation: Federation<SourceStores<M>>,

	/// The query planner — the only place a *user-facing* semantic gate is
	/// minted ([`registry::runtime::vector::SemanticGate::issue`]). Store readiness uses
	/// the separate [`registry::runtime::vector::SemanticGate::for_readiness`] constructor.
	planner: crate::search::SearchPlanner,

	/// The (model-branded) query embedder behind the gated semantic path.
	embedder: HttpEmbedder<M>,

	/// The content-addressed embedding cache in front of the embedder.
	embedding_cache: EmbeddingCache<M>,

	/// Per-session exploration graphs (join-semilattice merge), backed by
	/// postgres so **any** gateway replica can serve **any** session (the
	/// node-local `MemorySessionStore` is invisible to sibling replicas). The
	/// backing `sessions` table is created at boot via [`PgSessionStore::migrate`].
	sessions: PgSessionStore,

	/// The HTTP client for the compiler daemon (replaces the in-process
	/// `ForgeRuntime` that was removed in the Buck2→Cargo migration).
	compiler_client: crate::compiler_client::CompilerClient,

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
		Ok(Self { synonyms: Synonyms::new(dir)?, specifics: Specifics::new(dir)? })
	}

	/// The synonym table, ready to pass to `crate::registry::metadata::rich::extract`.
	pub fn synonyms(&self) -> &Synonyms { &self.synonyms }

	/// The specifics table, ready to pass to `crate::registry::metadata::rich::extract`.
	pub fn specifics(&self) -> &Specifics { &self.specifics }
}

impl<M: EmbeddingModel> Server<M> {
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

		// Sessions live in postgres so any gateway replica serves any session. The
		// pool is built the same way every other pg-backed store of the definitive
		// source is (`connect_lazy` — no I/O until first use), from the definitive
		// source's postgres endpoint. `migrate()` creates the `sessions` table
		// (`IF NOT EXISTS`, idempotent). We reach the definitive source's stores
		// (already connected above) only for their liveness; the pool here is its
		// own lazy handle onto the same database.
		let session_pool = sqlx::postgres::PgPoolOptions::new()
			.max_connections(8)
			.connect_lazy(config.definitive.endpoints.postgres.expose_secret())
			.map_err(|error| {
				ConnectError::new(BackendKind::Postgres, ConnectFailure::Other(error.into()))
			})?;
		let sessions = PgSessionStore::new(session_pool);
		sessions
			.migrate()
			.await
			.map_err(|error| ServerError::Runtime(error.into()))?;

		// Build the compiler-daemon HTTP client from the configured endpoint.
		let compiler_client = crate::compiler_client::CompilerClient::new(
			config.compiler_endpoint.clone(),
		);

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

		Ok(Self {
			config,
			federation,
			planner: crate::search::SearchPlanner::new(),
			embedder,
			embedding_cache,
			sessions,
			compiler_client,
			heuristics,
			compiled_store,
		})
	}

	/// The keyword-normalization heuristics, if a `metadata_data_dir` was
	/// configured. `None` means facet extraction runs without synonym/specifics
	/// normalization.
	pub(crate) fn heuristics(&self) -> Option<&Heuristics> { self.heuristics.as_ref() }

	/// Build and connect the full store stack for one configured source, bringing
	/// its backends up concurrently.
	async fn connect_source(cfg: &SourceConfig) -> ServerResult<SourceStores<M>> {
		let endpoints = &cfg.endpoints;

		// One pool underlies every postgres-backed store of this source (index,
		// queue, outbox, package-search hydration). `connect_lazy` does no I/O;
		// verification happens in each store's `Connect` below.
		let pool = sqlx::postgres::PgPoolOptions::new()
			.max_connections(16)
			.connect_lazy(endpoints.postgres.expose_secret())
			.map_err(|error| {
				ConnectError::new(BackendKind::Postgres, ConnectFailure::Other(error.into()))
			})?;

		let instance = TerminusInstance::new(endpoints.terminus_instance())
			.map_err(crate::registry::RegistryError::from)?;

		// Construct the cold (configured-but-unverified) store handles for this source.
		let global_cold: GlobalStore<heart::Cold> = GlobalStore::new(pool.clone(), instance);
		let blobs_cold: Store<heart::Cold> = Store::new(object_store_backend(&endpoints.object_store)?);
		let queue_cold: Queue<heart::Cold> = Queue::new(pool.clone(), INDEXING_RETRY_POLICY);
		let outbox_cold: Outbox<heart::Cold> = Outbox::new(pool.clone());
		let graph_cold: Graph<heart::Cold> = Graph::new(
			reqwest::Client::new(),
			endpoints.terminus.clone(),
			Organization::new(endpoints.terminus_organization.as_str())
				.map_err(invalid_configuration)?,
			Database::new(endpoints.terminus_database.as_str()).map_err(invalid_configuration)?,
			Credentials::new(endpoints.terminus_user.as_str(), endpoints.terminus_password.clone())
				.map_err(invalid_configuration)?,
		);
		let semantic_cold: Semantic<M, heart::Cold> = Semantic::new(
			qdrant_client::Qdrant::from_url(endpoints.qdrant.as_str()).build().map_err(
				|error| ConnectError::new(BackendKind::Qdrant, ConnectFailure::Other(error.into())),
			)?,
			CollectionName::new(endpoints.qdrant_collection.as_str())
				.map_err(invalid_configuration)?,
		);

		// Apply the postgres schema first. Queue/outbox `connect` only probe for
		// existing tables (`SELECT 1 FROM jobs/outbox`), so they must not race
		// the schema transaction on a cold database.
		let global_store = global_cold.connect().await?;

		// Remaining backends come up concurrently; the first failure aborts
		// with its backend + cause.
		let (blobs, queue, outbox, graph, semantics) = tokio::try_join!(
			blobs_cold.connect(),
			queue_cold.connect(),
			outbox_cold.connect(),
			graph_cold.connect(),
			semantic_cold.connect(),
		)?;

		// The text index is replica-local: opened/rebuilt from this source's
		// postgres watermark, not "connected".
		let text_dir: std::path::PathBuf = cfg.text_index_directory();
		std::fs::create_dir_all(&text_dir)
			.map_err(|error| ServerError::Runtime(registry::runtime::error::TextError::Io(error).into()))?;
		let text =
			TextIndex::open_or_create(&text_dir).map_err(|e| ServerError::Runtime(e.into()))?;

		// So is the package-search index, fed by the sync poller off the same pool.
		let packages_dir = cfg.package_index_directory();
		std::fs::create_dir_all(&packages_dir)
			.map_err(|error| ServerError::Runtime(registry::runtime::error::TextError::Io(error).into()))?;
		let packages = Arc::new(PackageSearchIndex::open(&packages_dir, pool)?);

		Ok(SourceStores { global_store, blobs, queue, outbox, graph, semantics, text, packages })
	}

	/// The resolved configuration.
	pub fn config(&self) -> &ServerConfiguration { &self.config }

	/// The full federation of sources (base + overlays), for queries that resolve
	/// across sources with overlay-override precedence.
	pub fn federation(&self) -> &Federation<SourceStores<M>> { &self.federation }

	/// The definitive base source's stores — the common single-source path.
	pub fn base(&self) -> &SourceStores<M> { self.federation.base() }

	// ── Convenience accessors delegating to the definitive base. Federated flows
	// use `federation()` to walk overlays-then-base; single-source flows use these.

	/// The definitive base's global index handle.
	pub fn global_store(&self) -> &GlobalStore<Live> { &self.base().global_store }

	/// The definitive base's blob store handle.
	pub fn blobs(&self) -> &Store<Live> { &self.base().blobs }

	/// The definitive base's job queue handle.
	pub fn queue(&self) -> &Queue<Live> { &self.base().queue }

	/// The definitive base's outbox handle.
	pub fn outbox(&self) -> &Outbox<Live> { &self.base().outbox }

	/// The definitive base's graph store handle.
	pub fn graph(&self) -> &Graph<Live> { &self.base().graph }

	/// The definitive base's semantic store handle.
	pub fn semantics(&self) -> &Semantic<M, Live> { &self.base().semantics }

	/// The definitive base's text index.
	pub fn text(&self) -> &TextIndex { &self.base().text }

	/// The model-branded query embedder behind the gated semantic path — also the
	/// embedder the vector fan-out consumer drives to materialize symbol points.
	pub(crate) fn embedder(&self) -> &HttpEmbedder<M> { &self.embedder }

	/// The content-addressed embedding cache in front of the embedder.
	pub(crate) fn embedding_cache(&self) -> &EmbeddingCache<M> { &self.embedding_cache }

	/// The per-session exploration graphs (postgres-backed; replica-shared).
	pub fn sessions(&self) -> &PgSessionStore { &self.sessions }

	/// The HTTP client for the compiler daemon.
	pub fn compiler_client(&self) -> &crate::compiler_client::CompilerClient { &self.compiler_client }

	/// The compiled-output lookup store for `POST /v1/compiled/lookup` (SV-6).
	///
	/// Returns a reference to the store so the handler can call `.lookup()`.
	/// Backed by the definitive base's object store: reads the
	/// `compiled/{job_key}` records the fleet's emit pipeline writes.
	pub fn compiled_store(&self) -> &ObjectCompiledStore { &self.compiled_store }

	/// Serve until shutdown: bind the HTTP router to `config.serving_address` and
	/// run the background pollers (queue workers + derived-store consumers) for
	/// every source in the federation.
	pub async fn serve(self: Arc<Self>) -> ServerResult<()> {
		// The `metrics` recorder behind `/metrics` (fanned out to OTLP) is
		// installed by `heart::telemetry::init` in `main`, before any request or
		// poller can emit a metric — no per-serve install here anymore.
		let router = http::router::router(Arc::clone(&self));
		let listener = tokio::net::TcpListener::bind(self.config.serving_address)
			.await
			.map_err(|source| {
				ServerError::Internal(InternalError::BindFailed {
					address: self.config.serving_address,
					source,
				})
			})?;
		tracing::info!(address = %self.config.serving_address, "serving");

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
			queue_worker = Some(tokio::spawn(poll::queue_worker(Arc::clone(&self), drain.clone())));
		}
		if role.runs_gateway() {
			for sink in <heart::DerivedStore as strum::IntoEnumIterator>::iter() {
				pollers.spawn(poll::outbox_consumer(Arc::clone(&self), sink));
			}
			pollers.spawn(poll::package_index_poller(Arc::clone(&self)));
			pollers.spawn(poll::text_index_poller(Arc::clone(&self)));
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
				let Ok(lang) = ecosystem::Language::from_str(lang_str.as_str()) else {
					tracing::warn!(lang = %lang_str, "unknown language in mirror.follow; skipping");
					continue;
				};
				let follower: Box<dyn crate::registry::upstream::CatalogFollower> = match lang {
					ecosystem::Language::CSharp => {
						Box::new(registry::upstream::NuGetCatalogFollower::production())
					}
					ecosystem::Language::Rust => {
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
				Ok(Err(join)) => tracing::warn!(error = %join, "queue worker task ended abnormally"),
				Err(_) => tracing::warn!(?deadline, "drain deadline exceeded; forcing queue worker down"),
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
		&& let Ok(path) = url.to_file_path() {
			std::fs::create_dir_all(&path).map_err(|error| {
				ConnectError::new(BackendKind::ObjectStore, ConnectFailure::Other(error.into()))
			})?;
		}
	let (backend, prefix) = object_store::parse_url(url).map_err(|error| {
		ConnectError::new(BackendKind::ObjectStore, ConnectFailure::Other(error.into()))
	})?;
	Ok(Arc::new(object_store::prefix::PrefixStore::new(backend, prefix)))
}

/// A config-shape error surfaced while building store handles.
fn invalid_configuration(error: impl std::fmt::Display) -> ServerError {
	ServerError::Config(config::ConfigError::Validation(
		config::ConfigValidationError::Other { detail: error.to_string() },
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
