use std::{
	collections::{BTreeMap, HashMap},
	fs, hash::{DefaultHasher, Hash, Hasher},
	io, path::{Path, PathBuf},
	sync::Arc,
};

use serde::{Deserialize, Serialize};
use serde_json::Value as JsonValue;
use tokio::sync::Mutex;

use crate::http::error::AppError;

use super::graph::{GraphEdge, GraphNode, GraphResponse};
use super::session_file;

#[derive(Clone)]
pub struct SessionStore {
	inner: Arc<Mutex<HashMap<String, SessionGraphState>>>,
	dir:   Option<PathBuf>,
}

impl SessionStore {
	pub fn new(dir: impl Into<PathBuf>) -> io::Result<Self> {
		let dir = dir.into();
		fs::create_dir_all(&dir)?;
		let mut sessions = HashMap::new();

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

		Ok(Self { inner: Arc::new(Mutex::new(sessions)), dir: Some(dir) })
	}

	pub(crate) async fn merge(
		&self,
		session: &str,
		addition: SessionGraphState,
	) -> GraphResponse {
		let mut sessions = self.inner.lock().await;
		let graph = sessions.entry(session.to_owned()).or_default();
		graph.merge(addition);
		if let Some(dir) = &self.dir
			&& let Err(error) = self.persist_session(dir, session, graph)
		{
			tracing::warn!(session, error = %error, "failed to persist session graph");
		}
		graph.to_response()
	}

	pub async fn clear(&self, session: &str) {
		let mut sessions = self.inner.lock().await;
		sessions.remove(session);
		if let Some(dir) = &self.dir {
			let path = session_file(dir, session);
			if let Err(error) = fs::remove_file(&path)
				&& error.kind() != io::ErrorKind::NotFound
			{
				tracing::warn!(session, error = %error, "failed to remove persisted session");
			}
		}
	}

	fn persist_session(
		&self,
		dir: &Path,
		session: &str,
		graph: &SessionGraphState,
	) -> Result<(), AppError> {
		let path = session_file(dir, session);
		let payload = PersistedSessionGraph { session: session.to_owned(), graph: graph.clone() };
		fs::write(&path, serde_json::to_vec_pretty(&payload)?)
			.map_err(|source| AppError::Storage { path, source })?;
		Ok(())
	}
}

impl Default for SessionStore {
	fn default() -> Self {
		Self { inner: Arc::new(Mutex::new(HashMap::new())), dir: None }
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
