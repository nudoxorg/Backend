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

pub mod config;
pub mod coordination;
pub mod error;
pub mod http;
mod poll;
pub mod save;
pub mod search;

use std::num::NonZeroU32;
use std::sync::Arc;
use std::time::Duration;

use heart::{BackendKind, ConnectError, ConnectFailure, Connect, Federation, Live, access::AccessPolicy};
use registry::{
	Store,
	coordination::Outbox,
	index::{GlobalStore, TerminusInstance},
	queue::{Queue, RetryPolicy},
};
use runtime::{
	graph::{Credentials, Database, Graph, Organization},
	session::{MemorySessionStore, Persistence},
	text::TextIndex,
	vector::{CollectionName, EmbeddingCache, EmbeddingModel, Semantic},
};
use secrecy::ExposeSecret;

pub use config::{Endpoints, Limits, ServerConfiguration, SourceConfig};
pub use error::{ServerError, ServerResult};

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

/// The default access policy: everyone may read and write. Access control for
/// hosted deployments happens at the proxy in front of this server (see
/// [`heart::access`]); this policy is the honest name for that arrangement.
pub struct UnrestrictedAccess;

impl AccessPolicy for UnrestrictedAccess {}

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

	/// The access-control policy every read/write is checked through.
	policy: Arc<dyn AccessPolicy>,

	/// The query planner — the only place a semantic gate is minted.
	planner: crate::search::SearchPlanner,

	/// The (model-branded) query embedder behind the gated semantic path.
	embedder: HttpEmbedder<M>,

	/// The content-addressed embedding cache in front of the embedder.
	embedding_cache: EmbeddingCache<M>,

	/// Per-session exploration graphs (join-semilattice merge).
	sessions: MemorySessionStore,
}

impl<M: EmbeddingModel> Server<M> {
	/// Assemble a server: connect the definitive base and every overlay
	/// concurrently, then materialize the federation. Any single connection
	/// failure of any source aborts assembly with a context-rich [`ServerError`].
	pub async fn assemble(
		config: ServerConfiguration,
		policy: Arc<dyn AccessPolicy>,
	) -> ServerResult<Self> {
		config.validate()?;
		http::dto::register_custom_registries(&config.custom_registries);

		// The definitive base is the federation's required root.
		let base = Self::connect_source(&config.definitive).await?;
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
		let sessions = MemorySessionStore::new(Persistence::Directory(
			config.definitive.data_directory().join("sessions"),
		));

		Ok(Self {
			config,
			federation,
			policy,
			planner: crate::search::SearchPlanner::new(),
			embedder,
			embedding_cache,
			sessions,
		})
	}

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
				ConnectError::new(BackendKind::Postgres, ConnectFailure::Other(Box::new(error)))
			})?;

		let instance = TerminusInstance::new(endpoints.terminus_instance())
			.map_err(registry::RegistryError::from)?;

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
				|error| ConnectError::new(BackendKind::Qdrant, ConnectFailure::Other(Box::new(error))),
			)?,
			CollectionName::new(endpoints.qdrant_collection.as_str())
				.map_err(invalid_configuration)?,
		);

		// Bring them all up at once; the first failure aborts with its backend + cause.
		let (global_store, blobs, queue, outbox, graph, semantics) = tokio::try_join!(
			global_cold.connect(),
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
			.map_err(|error| ServerError::Runtime(runtime::error::TextError::Io(error).into()))?;
		let text =
			TextIndex::open_or_create(&text_dir).map_err(|e| ServerError::Runtime(e.into()))?;

		// So is the package-search index, fed by the sync poller off the same pool.
		let packages_dir = cfg.package_index_directory();
		std::fs::create_dir_all(&packages_dir)
			.map_err(|error| ServerError::Runtime(runtime::error::TextError::Io(error).into()))?;
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

	/// The per-session exploration graphs.
	pub fn sessions(&self) -> &MemorySessionStore { &self.sessions }

	/// The single choke point every read/write flow authorizes through.
	///
	/// [`heart::access::AccessPolicy`] currently exposes no deny surface (hosted
	/// deployments gate at the fronting proxy), so today this is an audit trace
	/// plus the structural guarantee that every flow *has* an authorization
	/// point to grow into.
	pub(crate) fn authorize(&self, action: &'static str) -> ServerResult<()> {
		let _ = &self.policy;
		tracing::trace!(action, "access granted");
		Ok(())
	}

	/// Serve until shutdown: bind the HTTP router to `config.serving_address` and
	/// run the background pollers (queue workers + derived-store consumers) for
	/// every source in the federation.
	pub async fn serve(self: Arc<Self>) -> ServerResult<()> {
		http::handlers::health::install_prometheus();

		let router = http::router::router(Arc::clone(&self));
		let listener = tokio::net::TcpListener::bind(self.config.serving_address)
			.await
			.map_err(|error| {
				ServerError::Internal(format!(
					"could not bind {}: {error}",
					self.config.serving_address
				))
			})?;
		tracing::info!(address = %self.config.serving_address, "serving");

		// Supervised background pollers: one queue worker, one outbox consumer per
		// derived sink, plus the replica-local index sync/watermark loops.
		let mut pollers = tokio::task::JoinSet::new();
		pollers.spawn(poll::queue_worker(Arc::clone(&self)));
		for sink in <heart::DerivedStore as strum::IntoEnumIterator>::iter() {
			pollers.spawn(poll::outbox_consumer(Arc::clone(&self), sink));
		}
		pollers.spawn(poll::package_index_poller(Arc::clone(&self)));
		pollers.spawn(poll::text_index_poller(Arc::clone(&self)));

		axum::serve(listener, router)
			.with_graceful_shutdown(shutdown_signal())
			.await
			.map_err(|error| ServerError::Internal(format!("http serve failed: {error}")))?;

		// The listener has drained; wind the pollers down at their next await
		// point and reap them so nothing outlives the server.
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
	if url.scheme() == "file" {
		if let Ok(path) = url.to_file_path() {
			std::fs::create_dir_all(&path).map_err(|error| {
				ConnectError::new(BackendKind::ObjectStore, ConnectFailure::Other(Box::new(error)))
			})?;
		}
	}
	let (backend, prefix) = object_store::parse_url(url).map_err(|error| {
		ConnectError::new(BackendKind::ObjectStore, ConnectFailure::Other(Box::new(error)))
	})?;
	Ok(Arc::new(object_store::prefix::PrefixStore::new(backend, prefix)))
}

/// A config-shape error surfaced while building store handles.
fn invalid_configuration(error: impl std::fmt::Display) -> ServerError {
	ServerError::Config(config::ConfigError::Invalid(error.to_string()))
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
