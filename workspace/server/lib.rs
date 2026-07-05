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
pub mod save;
pub mod search;

use std::sync::Arc;

use heart::{Connect, Federation, Live, access::AccessPolicy};
use registry::{Store, coordination::Outbox, index::GlobalStore, queue::Queue};
use runtime::{
	graph::Graph,
	text::TextIndex,
	vector::{EmbeddingModel, Semantic},
};

pub use config::{Endpoints, Limits, ServerConfiguration, SourceConfig};
pub use error::{ServerError, ServerResult};

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
}

impl<M: EmbeddingModel> Server<M> {
	/// Assemble a server: connect the definitive base and every overlay
	/// concurrently, then materialize the federation. Any single connection
	/// failure of any source aborts assembly with a context-rich [`ServerError`].
	pub async fn assemble(
		config: ServerConfiguration,
		policy: Arc<dyn AccessPolicy>,
	) -> ServerResult<Self> {
		// The definitive base is the federation's required root.
		let base = Self::connect_source(&config.definitive).await?;
		let mut federation = Federation::new(config.definitive.source_id(), base);

		// Overlays, in declared precedence order (highest first).
		for overlay in &config.overlays {
			let stores = Self::connect_source(overlay).await?;
			federation = federation.with_overlay(overlay.source_id(), stores);
		}

		Ok(Self { config, federation, policy })
	}

	/// Build and connect the full store stack for one configured source, bringing
	/// its backends up concurrently.
	async fn connect_source(cfg: &SourceConfig) -> ServerResult<SourceStores<M>> {
		let _ = &cfg.endpoints;
		// Construct the cold (configured-but-unverified) store handles for this source.
		let global_cold: GlobalStore<heart::Cold> = todo!("build PgPool + TerminusInstance from cfg.endpoints");
		let blobs_cold: Store<heart::Cold> = todo!("build object_store backend from cfg.endpoints");
		let queue_cold: Queue<heart::Cold> = todo!("build queue over the same PgPool");
		let outbox_cold: Outbox<heart::Cold> = todo!("build outbox over the same PgPool");
		let graph_cold: Graph<heart::Cold> = todo!("build terminus Graph from cfg.endpoints");
		let semantic_cold: Semantic<M, heart::Cold> = todo!("build qdrant Semantic from cfg.endpoints");

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
		let text_dir: std::path::PathBuf = todo!("per-source text-index dir from cfg");
		let text =
			TextIndex::open_or_create(&text_dir).map_err(|e| ServerError::Runtime(e.into()))?;

		Ok(SourceStores { global_store, blobs, queue, outbox, graph, semantics, text })
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

	/// Serve until shutdown: bind the HTTP router to `config.serving_address` and
	/// run the background pollers (queue workers + derived-store consumers) for
	/// every source in the federation.
	pub async fn serve(self: Arc<Self>) -> ServerResult<()> {
		todo!("build the axum router, spawn per-source pollers, bind + serve with graceful shutdown")
	}
}
