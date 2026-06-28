use std::{
	collections::BTreeMap,
	fs, io, path::PathBuf,
	sync::Arc,
};

use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use serde_json::Value as JsonValue;

use super::graph::{GraphEdge, GraphNode, GraphResponse};
use super::session_file;

#[derive(Clone)]
pub struct SessionStore {
	// Per-session sharded map: unrelated sessions never contend on a global lock,
	// and (critically) persistence happens *after* the shard guard is dropped, so a
	// blocking disk write no longer serializes every session merge.
	inner: Arc<DashMap<String, SessionGraphState>>,
	dir:   Option<PathBuf>,
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
			sessions.insert(persisted.session, persisted.graph);
		}

		Ok(Self { inner: Arc::new(sessions), dir: Some(dir) })
	}

	pub(crate) async fn merge(
		&self,
		session: &str,
		addition: SessionGraphState,
	) -> GraphResponse {
		// Hold the shard guard only for the in-memory merge + serialization (pure
		// CPU). Serialize through a borrowed view so we never clone the whole graph,
		// then drop the guard *before* the async disk write.
		let (response, persist) = {
			let mut graph = self.inner.entry(session.to_owned()).or_default();
			graph.merge(addition);
			let response = graph.to_response();
			let persist = self.dir.as_ref().map(|dir| {
				let payload = PersistedSessionGraphRef { session, graph: &graph };
				(session_file(dir, session), serde_json::to_vec_pretty(&payload))
			});
			(response, persist)
		};

		if let Some((path, payload)) = persist {
			match payload {
				Ok(bytes) => {
					if let Err(error) = tokio::fs::write(&path, bytes).await {
						tracing::warn!(session, error = %error, "failed to persist session graph");
					}
				}
				Err(error) => {
					tracing::warn!(session, error = %error, "failed to serialize session graph");
				}
			}
		}

		response
	}

	pub async fn clear(&self, session: &str) {
		self.inner.remove(session);
		if let Some(dir) = &self.dir {
			let path = session_file(dir, session);
			if let Err(error) = tokio::fs::remove_file(&path).await
				&& error.kind() != io::ErrorKind::NotFound
			{
				tracing::warn!(session, error = %error, "failed to remove persisted session");
			}
		}
	}
}

impl Default for SessionStore {
	fn default() -> Self {
		Self { inner: Arc::new(DashMap::new()), dir: None }
	}
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub(crate) struct SessionGraphState {
	pub(crate) nodes: BTreeMap<String, JsonValue>,
	pub(crate) edges: BTreeMap<String, GraphEdge>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct PersistedSessionGraph {
	session: String,
	graph:   SessionGraphState,
}

/// Borrowed counterpart of [`PersistedSessionGraph`] used for the write path, so
/// persistence serializes the live graph in place instead of cloning it first.
#[derive(Serialize)]
struct PersistedSessionGraphRef<'a> {
	session: &'a str,
	graph:   &'a SessionGraphState,
}

impl SessionGraphState {
	pub(crate) fn add_node(&mut self, uri: String, document: JsonValue) {
		self.nodes.insert(uri, document);
	}

	pub(crate) fn add_edge(&mut self, source: String, target: String, relation: String) {
		let key = format!("{source}\n{relation}\n{target}");
		self.edges.insert(key, GraphEdge { source, target, relation });
	}

	fn merge(&mut self, other: SessionGraphState) {
		self.nodes.extend(other.nodes);
		self.edges.extend(other.edges);
	}

	pub(crate) fn to_response(&self) -> GraphResponse {
		GraphResponse {
			nodes: self
				.nodes
				.iter()
				.map(|(uri, document)| GraphNode {
					uri: uri.clone(),
					document: document.clone(),
				})
				.collect(),
			edges: self.edges.values().cloned().collect(),
		}
	}
}
