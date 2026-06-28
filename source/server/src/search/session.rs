use std::{
	collections::BTreeMap,
	fs, io, path::PathBuf,
	sync::Arc,
};

use dashmap::DashMap;
use im::OrdMap;
use serde::{Deserialize, Serialize};
use serde_json::Value as JsonValue;

use crate::http::error::AppError;

use super::graph::{GraphEdge, GraphNode, GraphResponse};
use super::session_file;

/// Concurrent store of per-session graphs.
///
/// Sessions live in a [`DashMap`] so distinct sessions never contend on a
/// single lock. A merge mutates the entry under its shard lock, takes a cheap
/// structural-sharing snapshot, then releases the lock *before* persisting the
/// snapshot off the async reactor via `spawn_blocking`.
#[derive(Clone)]
pub struct SessionStore {
	inner: Arc<DashMap<Arc<str>, SessionGraphState>>,
	dir:   Option<Arc<PathBuf>>,
}

impl SessionStore {
	pub fn new(dir: impl Into<PathBuf>) -> io::Result<Self> {
		let dir = dir.into();
		fs::create_dir_all(&dir)?;
		let sessions = DashMap::new();

		for entry in fs::read_dir(&dir)? {
			let entry = entry?;
			if !entry.file_type()?.is_file() {
				continue;
			}

			let path = entry.path();
			if path.extension().and_then(|ext| ext.to_str()) != Some("json") {
				continue;
			}

			let contents = fs::read(&path)?;
			let persisted: PersistedSessionGraph =
				serde_json::from_slice(&contents).map_err(|source| {
					io::Error::new(
						io::ErrorKind::InvalidData,
						format!(
							"failed to parse persisted session `{}`: {source}",
							path.display()
						),
					)
				})?;
			sessions.insert(Arc::from(persisted.session.as_str()), persisted.graph.into_state());
		}

		Ok(Self { inner: Arc::new(sessions), dir: Some(Arc::new(dir)) })
	}

	pub(crate) async fn merge(
		&self,
		session: &str,
		addition: SessionGraphState,
	) -> GraphResponse {
		// Mutate under the shard lock, then snapshot cheaply and release. The
		// `im`/`Arc` collections make both the merge and the snapshot clone
		// structural (reference-count) operations rather than O(n) deep copies.
		let (snapshot, response) = {
			let mut entry = self.inner.entry(Arc::from(session)).or_default();
			entry.merge(addition);
			(entry.clone(), entry.to_response())
		};
		// Shard lock released above; persist outside any lock and off the reactor.
		if let Some(dir) = &self.dir {
			persist_off_reactor(dir.clone(), session.to_owned(), snapshot).await;
		}
		response
	}

	pub async fn clear(&self, session: &str) {
		self.inner.remove(session);
		if let Some(dir) = &self.dir {
			let path = session_file(dir, session);
			let session = session.to_owned();
			// Removal is filesystem I/O — keep it off the reactor.
			let result = tokio::task::spawn_blocking(move || match fs::remove_file(&path) {
				Err(error) if error.kind() != io::ErrorKind::NotFound => Err(error),
				_ => Ok(()),
			})
			.await;
			match result {
				Ok(Err(error)) => {
					tracing::warn!(session, error = %error, "failed to remove persisted session")
				}
				Err(error) => {
					tracing::warn!(session, error = %error, "session removal task panicked")
				}
				Ok(Ok(())) => {}
			}
		}
	}
}

/// Serialize and write `snapshot` to disk off the async reactor.
async fn persist_off_reactor(dir: Arc<PathBuf>, session: String, snapshot: SessionGraphState) {
	let session_for_log = session.clone();
	let result = tokio::task::spawn_blocking(move || -> Result<(), AppError> {
		let path = session_file(&dir, &session);
		let payload = PersistedSessionGraph::from_state(&session, &snapshot);
		fs::write(&path, serde_json::to_vec_pretty(&payload)?)
			.map_err(|source| AppError::Storage { path, source })?;
		Ok(())
	})
	.await;

	match result {
		Ok(Err(error)) => {
			tracing::warn!(session = session_for_log, error = %error, "failed to persist session graph")
		}
		Err(error) => {
			tracing::warn!(session = session_for_log, error = %error, "session persist task panicked")
		}
		Ok(Ok(())) => {}
	}
}

impl Default for SessionStore {
	fn default() -> Self {
		Self { inner: Arc::new(DashMap::new()), dir: None }
	}
}

/// In-memory session graph backed by structural-sharing collections.
///
/// `nodes` and `edges` are `im::OrdMap`s keyed by interned `Arc<str>`, so
/// cloning the whole graph (for a snapshot) and merging two graphs are cheap
/// structural operations. Documents are shared as `Arc<JsonValue>`.
#[derive(Clone, Default)]
pub(crate) struct SessionGraphState {
	nodes: OrdMap<Arc<str>, Arc<JsonValue>>,
	edges: OrdMap<Arc<str>, GraphEdge>,
}

impl SessionGraphState {
	pub(crate) fn add_node(&mut self, uri: Arc<str>, document: Arc<JsonValue>) {
		self.nodes.insert(uri, document);
	}

	pub(crate) fn add_edge(&mut self, source: Arc<str>, target: Arc<str>, relation: Arc<str>) {
		let key: Arc<str> = Arc::from(format!("{source}\n{relation}\n{target}"));
		self.edges.insert(key, GraphEdge { source, target, relation });
	}

	/// The interned URIs of every node, in sorted order (for `/expand`).
	pub(crate) fn node_uris(&self) -> impl Iterator<Item = &Arc<str>> {
		self.nodes.keys()
	}

	fn merge(&mut self, other: SessionGraphState) {
		// `union` keeps the left operand's value on key collisions, so put
		// `other` on the left to preserve the original "addition overrides
		// existing" semantics. Both unions are structural (no deep copies).
		self.nodes = other.nodes.union(std::mem::take(&mut self.nodes));
		self.edges = other.edges.union(std::mem::take(&mut self.edges));
	}

	pub(crate) fn to_response(&self) -> GraphResponse {
		GraphResponse {
			nodes: self
				.nodes
				.iter()
				.map(|(uri, document)| GraphNode {
					uri:      uri.clone(),
					document: document.clone(),
				})
				.collect(),
			edges: self.edges.values().cloned().collect(),
		}
	}
}

// ── Persistence DTO ─────────────────────────────────────────────────────────
//
// Kept separate from the in-memory `SessionGraphState` so the on-disk JSON
// shape stays byte-compatible with previously persisted sessions, and so the
// `Arc`-based in-memory types don't depend on serde's `rc` feature for the
// persistence path.

#[derive(Debug, Serialize, Deserialize)]
struct PersistedSessionGraph {
	session: String,
	graph:   PersistedGraph,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct PersistedGraph {
	nodes: BTreeMap<String, JsonValue>,
	edges: BTreeMap<String, PersistedEdge>,
}

#[derive(Debug, Serialize, Deserialize)]
struct PersistedEdge {
	source:   String,
	target:   String,
	relation: String,
}

impl PersistedSessionGraph {
	fn from_state(session: &str, state: &SessionGraphState) -> Self {
		let nodes = state
			.nodes
			.iter()
			.map(|(uri, doc)| (uri.to_string(), (**doc).clone()))
			.collect();
		let edges = state
			.edges
			.iter()
			.map(|(key, edge)| {
				(key.to_string(), PersistedEdge {
					source:   edge.source.to_string(),
					target:   edge.target.to_string(),
					relation: edge.relation.to_string(),
				})
			})
			.collect();
		Self { session: session.to_owned(), graph: PersistedGraph { nodes, edges } }
	}
}

impl PersistedGraph {
	fn into_state(self) -> SessionGraphState {
		let nodes = self
			.nodes
			.into_iter()
			.map(|(uri, doc)| (Arc::<str>::from(uri), Arc::new(doc)))
			.collect();
		let edges = self
			.edges
			.into_iter()
			.map(|(key, edge)| {
				(Arc::<str>::from(key), GraphEdge {
					source:   Arc::from(edge.source),
					target:   Arc::from(edge.target),
					relation: Arc::from(edge.relation),
				})
			})
			.collect();
		SessionGraphState { nodes, edges }
	}
}
