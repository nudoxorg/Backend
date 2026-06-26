use std::{collections::{BTreeMap, BTreeSet, HashMap, HashSet, VecDeque, hash_map::DefaultHasher}, fs, hash::{Hash, Hasher}, io, path::{Path, PathBuf}, sync::Arc};

use qdrant_client::{Qdrant, qdrant::{ListCollectionsResponse, QueryPointsBuilder, ScoredPoint, Value, value::Kind}};
use serde::{Deserialize, Serialize};
use serde_json::Value as JsonValue;
use terminusdb_client::{BranchSpec, GetOpts, TerminusDBHttpClient};
use tokio::sync::Mutex;

use crate::{config::{PipelineConfig, QdrantSettings}, error::{AppError, ConfigError, EmbeddingError, QdrantError, TerminusError}, identity::EntryUri, terminusdb::{embedding_service::{EmbeddingProvider, OpenAIEmbeddingProvider}, upload::TerminusConfig}};

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
						format!("failed to parse persisted session `{}`: {source}", path.display()),
					)
				})?;
			sessions.insert(persisted.session, persisted.graph);
		}

		Ok(Self { inner: Arc::new(Mutex::new(sessions)), dir: Some(dir) })
	}

	pub(crate) async fn merge(&self, session: &str, addition: SessionGraphState) -> GraphResponse {
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
	fn default() -> Self { Self { inner: Arc::new(Mutex::new(HashMap::new())), dir: None } }
}

#[derive(Debug, Serialize)]
pub struct SearchResponse {
	pub query:   String,
	pub results: Vec<SearchResult>,
}

#[derive(Debug, Clone, Serialize)]
pub struct SearchResult {
	pub uri:         String,
	pub score:       f32,
	pub collection:  String,
	pub fq_name:     Option<String>,
	pub language:    Option<String>,
	pub package:     Option<String>,
	pub version:     Option<String>,
	pub symbol_kind: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct LookupResponse {
	pub uri:      String,
	pub document: JsonValue,
	pub kind:     Option<JsonValue>,
}

#[derive(Debug, Serialize)]
pub struct RunSearchResponse {
	pub query:   String,
	pub session: Option<String>,
	pub matches: Vec<MatchedSymbol>,
	pub graph:   GraphResponse,
}

#[derive(Debug, Serialize)]
pub struct ExpandResponse {
	pub uri:      String,
	pub session:  Option<String>,
	pub expanded: Vec<String>,
	pub graph:    GraphResponse,
}

#[derive(Debug, Clone, Serialize)]
pub struct MatchedSymbol {
	#[serde(flatten)]
	pub result:   SearchResult,
	pub document: JsonValue,
	pub kind:     Option<JsonValue>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GraphNode {
	pub uri:      String,
	pub document: JsonValue,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GraphEdge {
	pub source:   String,
	pub target:   String,
	pub relation: String,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct GraphResponse {
	pub nodes: Vec<GraphNode>,
	pub edges: Vec<GraphEdge>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub(crate) struct SessionGraphState {
	nodes: BTreeMap<String, JsonValue>,
	edges: BTreeMap<String, GraphEdge>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct PersistedSessionGraph {
	session: String,
	graph:   SessionGraphState,
}

#[derive(Debug, Clone)]
struct ResolvedSymbol {
	document: JsonValue,
	kind:     Option<JsonValue>,
}

impl SessionGraphState {
	fn add_node(&mut self, uri: String, document: JsonValue) { self.nodes.insert(uri, document); }

	fn add_edge(&mut self, source: String, target: String, relation: String) {
		let key = format!("{source}\n{relation}\n{target}");
		self.edges.insert(key, GraphEdge { source, target, relation });
	}

	fn merge(&mut self, other: SessionGraphState) {
		self.nodes.extend(other.nodes);
		self.edges.extend(other.edges);
	}

	fn to_response(&self) -> GraphResponse {
		GraphResponse {
			nodes: self
				.nodes
				.iter()
				.map(|(uri, document)| GraphNode { uri: uri.clone(), document: document.clone() })
				.collect(),
			edges: self.edges.values().cloned().collect(),
		}
	}
}

pub async fn semantic_search(
	config: &PipelineConfig,
	query: &str,
	limit: usize,
) -> Result<SearchResponse, AppError> {
	let results = search_results(config, query, limit).await?;
	Ok(SearchResponse { query: query.trim().to_owned(), results })
}

pub async fn lookup_symbol(config: &PipelineConfig, uri: &str) -> Result<LookupResponse, AppError> {
	let uri = require_non_empty("q", uri)?;
	let terminus = require_terminus(config)?;
	let client = terminus_client(terminus).await?;
	let spec = BranchSpec::new(&terminus.db);
	let mut cache = HashMap::new();
	let resolved = resolve_symbol(&client, &spec, &mut cache, uri).await?;

	Ok(LookupResponse {
		uri:      uri.to_owned(),
		document: resolved.document,
		kind:     resolved.kind,
	})
}

pub async fn lookup_symbol_with_context(
	config: &PipelineConfig,
	symbol: &str,
	language: &str,
	package: Option<&str>,
) -> Result<LookupResponse, AppError> {
	let symbol = require_non_empty("symbol", symbol)?;
	let language = require_non_empty("language", language)?;
	let package = package.and_then(|value| {
		let trimmed = value.trim();
		if trimmed.is_empty() { None } else { Some(trimmed) }
	});
	let terminus = require_terminus(config)?;
	let client = terminus_client(terminus).await?;
	let spec = BranchSpec::new(&terminus.db);
	let mut cache = HashMap::new();

	let requested_uri = structured_symbol_uri(language, symbol, package);
	let mut resolved_uri = None;
	let mut document = None;
	for candidate in candidate_symbol_uris(&requested_uri) {
		if let Some(found) = fetch_document(&client, &spec, &mut cache, &candidate).await? {
			resolved_uri = Some(candidate);
			document = Some(found);
			break;
		}
	}

	let resolved_uri = resolved_uri.unwrap_or(requested_uri);
	let document = document.ok_or_else(|| AppError::SymbolNotFound {
		uri: requested_uri.clone(),
	})?;

	let kind = match document.get("kind").and_then(JsonValue::as_str) {
		Some(kind_uri) => fetch_document(&client, &spec, &mut cache, kind_uri).await?,
		None => None,
	};

	Ok(LookupResponse { uri: resolved_uri, document, kind })
}

pub async fn run_search(
	config: &PipelineConfig,
	sessions: &SessionStore,
	query: &str,
	limit: usize,
	session: Option<&str>,
) -> Result<RunSearchResponse, AppError> {
	let normalized_session = normalize_session(session);
	let results = search_results(config, query, limit).await?;
	let terminus = require_terminus(config)?;
	let client = terminus_client(terminus).await?;
	let spec = BranchSpec::new(&terminus.db);
	let mut cache = HashMap::new();
	let mut matches = Vec::with_capacity(results.len());

	for result in &results {
		let resolved = resolve_symbol(&client, &spec, &mut cache, &result.uri).await?;
		matches.push(MatchedSymbol {
			result:   result.clone(),
			document: resolved.document,
			kind:     resolved.kind,
		});
	}

	let roots: Vec<String> = results.iter().map(|result| result.uri.clone()).collect();
	let addition = build_graph(&client, &spec, roots, 1, 10).await?;
	let graph = match normalized_session.as_deref() {
		Some(session_id) => sessions.merge(session_id, addition).await,
		None => addition.to_response(),
	};

	Ok(RunSearchResponse {
		query: query.trim().to_owned(),
		session: normalized_session,
		matches,
		graph,
	})
}

pub async fn expand_symbol(
	config: &PipelineConfig,
	sessions: &SessionStore,
	uri: &str,
	depth: usize,
	breadth: usize,
	session: Option<&str>,
) -> Result<ExpandResponse, AppError> {
	let uri = require_non_empty("uri", uri)?;
	let normalized_session = normalize_session(session);
	let terminus = require_terminus(config)?;
	let client = terminus_client(terminus).await?;
	let spec = BranchSpec::new(&terminus.db);
	let depth = if depth == 0 { 2 } else { depth };
	let breadth = if breadth == 0 { 10 } else { breadth };

	let addition = build_graph(&client, &spec, vec![uri.to_owned()], depth, breadth).await?;
	let expanded = addition.nodes.keys().cloned().collect();
	let graph = match normalized_session.as_deref() {
		Some(session_id) => sessions.merge(session_id, addition).await,
		None => addition.to_response(),
	};

	Ok(ExpandResponse { uri: uri.to_owned(), session: normalized_session, expanded, graph })
}

async fn search_results(
	config: &PipelineConfig,
	query: &str,
	limit: usize,
) -> Result<Vec<SearchResult>, AppError> {
	let qdrant = require_qdrant(config)?;
	let query = require_non_empty("q", query)?;
	let provider = OpenAIEmbeddingProvider::new(config.embedding_model.clone());
	let embedding = provider.embed_text(query).await?;

	let client = Qdrant::from_url(qdrant.endpoint.as_str())
		.build()
		.map_err(|source| AppError::Qdrant(QdrantError::ConnectionFailed {
			endpoint: qdrant.endpoint.to_string(),
			source,
		}))?;
	let collections = backend_collections(&client, qdrant).await?;

	let mut results = Vec::new();
	for collection in collections {
		let response = client
			.query(
				QueryPointsBuilder::new(&collection)
					.query(embedding.clone())
					.limit(limit.max(1) as u64)
					.with_payload(true),
			)
			.await
			.map_err(|source| AppError::Qdrant(QdrantError::QueryFailed {
				collection: collection.clone(),
				source,
			}))?;

		results.extend(
			response.result.into_iter().filter_map(|point| parse_search_result(point, &collection)),
		);
	}

	results.sort_by(|left, right| right.score.total_cmp(&left.score));
	results.truncate(limit.max(1));
	Ok(results)
}

async fn build_graph(
	client: &TerminusDBHttpClient,
	spec: &BranchSpec,
	roots: Vec<String>,
	depth: usize,
	breadth: usize,
) -> Result<SessionGraphState, AppError> {
	let mut graph = SessionGraphState::default();
	let mut cache = HashMap::<String, Option<JsonValue>>::new();
	let mut visited = HashSet::<String>::new();
	let mut queue: VecDeque<(String, usize)> = roots.into_iter().map(|uri| (uri, depth)).collect();

	while let Some((uri, remaining_depth)) = queue.pop_front() {
		if !visited.insert(uri.clone()) {
			continue;
		}

		let Some(document) = fetch_document(client, spec, &mut cache, &uri).await? else {
			continue;
		};
		graph.add_node(uri.clone(), document.clone());

		let links = extract_links(&document, breadth);
		for (relation, target) in links {
			graph.add_edge(uri.clone(), target.clone(), relation);
			if remaining_depth > 0 {
				queue.push_back((target, remaining_depth - 1));
			}
		}
	}

	Ok(graph)
}

async fn resolve_symbol(
	client: &TerminusDBHttpClient,
	spec: &BranchSpec,
	cache: &mut HashMap<String, Option<JsonValue>>,
	uri: &str,
) -> Result<ResolvedSymbol, AppError> {
	let mut resolved_uri = None;
	let mut document = None;
	for candidate in candidate_symbol_uris(uri) {
		if let Some(found) = fetch_document(client, spec, cache, &candidate).await? {
			resolved_uri = Some(candidate);
			document = Some(found);
			break;
		}
	}

	let resolved_uri = resolved_uri.unwrap_or_else(|| uri.to_owned());
	let document = document.ok_or_else(|| AppError::SymbolNotFound { uri: uri.to_owned() })?;

	let kind = match document.get("kind").and_then(JsonValue::as_str) {
		Some(kind_uri) => fetch_document(client, spec, cache, kind_uri).await?,
		None => None,
	};

	if resolved_uri != uri {
		tracing::info!(requested_uri = uri, resolved_uri, "resolved symbol via canonical URI");
	}

	Ok(ResolvedSymbol { document, kind })
}

async fn fetch_document(
	client: &TerminusDBHttpClient,
	spec: &BranchSpec,
	cache: &mut HashMap<String, Option<JsonValue>>,
	uri: &str,
) -> Result<Option<JsonValue>, AppError> {
	if let Some(document) = cache.get(uri) {
		return Ok(document.clone());
	}

	let document =
		client.get_document_if_exists(uri, spec, GetOpts::default().with_unfold(true)).await?;
	cache.insert(uri.to_owned(), document.clone());
	Ok(document)
}

async fn backend_collections(
	client: &Qdrant,
	settings: &QdrantSettings,
) -> Result<Vec<String>, AppError> {
	let ListCollectionsResponse { collections, .. } =
		client.list_collections().await.map_err(|source| AppError::Qdrant(QdrantError::ListCollectionsFailed {
			prefix: settings.collection_prefix.clone(),
			source,
		}))?;
	let prefix = format!("{}_", settings.collection_prefix);

	let mut names: Vec<String> = collections
		.into_iter()
		.map(|collection| collection.name)
		.filter(|name| name.starts_with(&prefix))
		.collect();
	names.sort();
	Ok(names)
}

fn parse_search_result(point: ScoredPoint, collection: &str) -> Option<SearchResult> {
	let mut payload = point.payload;
	let uri = take_string(&mut payload, "uri")?;

	Some(SearchResult {
		uri,
		score: point.score,
		collection: collection.to_owned(),
		fq_name: take_string(&mut payload, "fq_name"),
		language: take_string(&mut payload, "language"),
		package: take_string(&mut payload, "package"),
		version: take_string(&mut payload, "version"),
		symbol_kind: take_string(&mut payload, "symbol_kind"),
	})
}

fn take_string(payload: &mut HashMap<String, Value>, key: &str) -> Option<String> {
	match payload.remove(key)?.kind {
		Some(Kind::StringValue(value)) => Some(value),
		Some(Kind::IntegerValue(value)) => Some(value.to_string()),
		Some(Kind::DoubleValue(value)) => Some(value.to_string()),
		Some(Kind::BoolValue(value)) => Some(value.to_string()),
		_ => None,
	}
}

fn extract_links(document: &JsonValue, breadth: usize) -> Vec<(String, String)> {
	let mut seen = BTreeSet::<(String, String)>::new();
	collect_links(document, "", &mut seen);
	seen.into_iter().take(breadth).collect()
}

fn collect_links(value: &JsonValue, relation: &str, out: &mut BTreeSet<(String, String)>) {
	match value {
		JsonValue::Object(map) => {
			if let Some(id) = map.get("@id").and_then(JsonValue::as_str)
				&& looks_like_internal_uri(id)
			{
				out.insert((relation_name(relation), id.to_owned()));
				return;
			}

			for (key, nested) in map {
				if key == "@context" || key == "@type" {
					continue;
				}
				collect_links(nested, key, out);
			}
		}
		JsonValue::Array(values) => {
			for nested in values {
				collect_links(nested, relation, out);
			}
		}
		JsonValue::String(text) => {
			if looks_like_internal_uri(text) {
				out.insert((relation_name(relation), text.to_owned()));
			}
		}
		_ => {}
	}
}

fn looks_like_internal_uri(value: &str) -> bool {
	if value.is_empty() || value.starts_with("http://") || value.starts_with("https://") {
		return false;
	}

	let Some((prefix, _rest)) = value.split_once('/') else {
		return false;
	};

	prefix.chars().next().is_some_and(|ch| ch.is_ascii_uppercase())
}

fn relation_name(relation: &str) -> String {
	if relation.is_empty() { "reference".to_owned() } else { relation.to_owned() }
}

fn structured_symbol_uri(language: &str, symbol: &str, package: Option<&str>) -> String {
	match package {
		Some(pkg) => EntryUri::new(language, pkg, symbol).to_string(),
		// No package: 2-segment heuristic form used only for lookup, not as canonical identity.
		None => format!("Entry/{}/{}", language.trim().to_ascii_lowercase(), symbol),
	}
}

fn candidate_symbol_uris(uri: &str) -> Vec<String> {
	let mut candidates = vec![uri.to_owned()];
	let Some(parsed) = EntryUri::parse(uri) else { return candidates };
	if parsed.lang() != "rust" {
		return candidates;
	}
	let canonical_package = canonical_rust_package_segment(parsed.package(), parsed.path());
	if canonical_package != parsed.package() {
		candidates.push(EntryUri::new(parsed.lang(), &canonical_package, parsed.path()).to_string());
	}
	candidates
}

fn canonical_rust_package_segment(package_segment: &str, fq_name: &str) -> String {
	let crate_root =
		fq_name.split("::").next().filter(|segment| !segment.is_empty()).unwrap_or(package_segment);
	let crate_slug = crate_root.replace('_', "-");
	let normalized_package = package_segment.replace('_', "-");
	if normalized_package.eq_ignore_ascii_case(&crate_slug) {
		crate_slug
	} else {
		package_segment.to_owned()
	}
}

fn require_non_empty<'a>(field: &'static str, value: &'a str) -> Result<&'a str, AppError> {
	let trimmed = value.trim();
	if trimmed.is_empty() {
		return Err(AppError::Config(ConfigError::EmptyValue { name: field }));
	}
	Ok(trimmed)
}

fn require_qdrant(config: &PipelineConfig) -> Result<&QdrantSettings, AppError> {
	config.qdrant.as_ref().ok_or_else(|| AppError::Config(ConfigError::MissingQdrant))
}

fn require_terminus(config: &PipelineConfig) -> Result<&TerminusConfig, AppError> {
	config.terminus.as_ref().ok_or_else(|| AppError::Config(ConfigError::MissingTerminus))
}

fn normalize_session(session: Option<&str>) -> Option<String> {
	session.and_then(|value| {
		let trimmed = value.trim();
		if trimmed.is_empty() { None } else { Some(trimmed.to_owned()) }
	})
}

fn session_file(dir: &Path, session: &str) -> PathBuf {
	let mut hasher = DefaultHasher::new();
	session.hash(&mut hasher);
	dir.join(format!("{:016x}.json", hasher.finish()))
}

async fn terminus_client(config: &TerminusConfig) -> Result<TerminusDBHttpClient, AppError> {
	Ok(
		TerminusDBHttpClient::new_with_database(
			config.endpoint.clone(),
			&config.user,
			&config.password,
			&config.db,
			&config.org,
		)
		.await?,
	)
}

#[cfg(test)]
mod tests {
	use super::{candidate_symbol_uris, canonical_rust_package_segment, structured_symbol_uri};

	#[test]
	fn rust_lookup_accepts_underscored_package_segment() {
		let candidates =
			candidate_symbol_uris("Entry/rust/cranelift_object/cranelift_object::ObjectModule");
		assert_eq!(candidates, vec![
			"Entry/rust/cranelift_object/cranelift_object::ObjectModule".to_owned(),
			"Entry/rust/cranelift-object/cranelift_object::ObjectModule".to_owned(),
		]);
	}

	#[test]
	fn rust_lookup_keeps_existing_hyphenated_package_segment() {
		let candidates =
			candidate_symbol_uris("Entry/rust/cranelift-object/cranelift_object::ObjectModule");
		assert_eq!(candidates, vec![
			"Entry/rust/cranelift-object/cranelift_object::ObjectModule".to_owned()
		]);
	}

	#[test]
	fn canonical_rust_package_segment_uses_crate_root() {
		assert_eq!(
			canonical_rust_package_segment(
				"cranelift_module",
				"cranelift_module::Module::define_function"
			),
			"cranelift-module"
		);
	}

	#[test]
	fn non_rust_uris_are_left_unchanged() {
		let candidates = candidate_symbol_uris("Entry/typescript/zod/classic.external::ZodString");
		assert_eq!(candidates, vec!["Entry/typescript/zod/classic.external::ZodString".to_owned()]);
	}

	#[test]
	fn structured_rust_lookup_uri_includes_package_segment() {
		assert_eq!(
			structured_symbol_uri(
				"Rust",
				"cranelift_module::Module::define_function",
				Some("cranelift-module")
			),
			"Entry/rust/cranelift-module/cranelift_module::Module::define_function"
		);
	}

	#[test]
	fn structured_lookup_without_package_preserves_symbol_only_shape() {
		assert_eq!(
			structured_symbol_uri("typescript", "index::parse", None),
			"Entry/typescript/index::parse"
		);
	}
}
