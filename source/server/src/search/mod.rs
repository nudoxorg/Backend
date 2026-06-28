mod session;
mod graph;
mod qdrant;

pub use session::SessionStore;
pub use qdrant::semantic_search;

use std::collections::HashMap;

use serde::Serialize;
use serde_json::Value as JsonValue;
use terminusdb_client::{BranchSpec, GetOpts, TerminusDBHttpClient};

use crate::config::{PipelineConfig, QdrantSettings};
use crate::http::error::{AppError, ConfigError, TerminusError};
use identity::EntryUri;
use crate::terminus::upload::TerminusConfig;

pub mod text;

// ── Response types ─────────────────────────────────────

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
	pub graph:   graph::GraphResponse,
}

#[derive(Debug, Serialize)]
pub struct ExpandResponse {
	pub uri:      String,
	pub session:  Option<String>,
	pub expanded: Vec<String>,
	pub graph:    graph::GraphResponse,
}

#[derive(Debug, Clone, Serialize)]
pub struct MatchedSymbol {
	#[serde(flatten)]
	pub result:   SearchResult,
	pub document: JsonValue,
	pub kind:     Option<JsonValue>,
}

// ── Validation helpers ─────────────────────────────────

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

fn session_file(dir: &std::path::Path, session: &str) -> std::path::PathBuf {
	use std::hash::{Hash, Hasher, DefaultHasher};

	let mut hasher = DefaultHasher::new();
	session.hash(&mut hasher);
	dir.join(format!("{:016x}.json", hasher.finish()))
}

fn structured_symbol_uri(language: &str, symbol: &str, package: Option<&str>) -> String {
	match package {
		Some(pkg) => EntryUri::new(language, pkg, symbol).to_string(),
		None => format!("Entry/{}/{}", language.trim().to_ascii_lowercase(), symbol),
	}
}

// ── Terminus client ────────────────────────────────────

async fn terminus_client(config: &TerminusConfig) -> Result<TerminusDBHttpClient, AppError> {
	TerminusDBHttpClient::new_with_database(
		config.endpoint.clone(),
		&config.user,
		&config.password,
		&config.db,
		&config.org,
	)
	.await
	.map_err(|source| AppError::Terminus(TerminusError::ClientCreation {
		org: config.org.clone(),
		db: config.db.clone(),
		source,
	}))
}

// ── Public API functions ───────────────────────────────

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
	let document = fetch_document(&client, &spec, &mut cache, &requested_uri)
		.await?
		.ok_or_else(|| AppError::SymbolNotFound { uri: requested_uri.clone() })?;

	let kind = match document.get("kind").and_then(JsonValue::as_str) {
		Some(kind_uri) => fetch_document(&client, &spec, &mut cache, kind_uri).await?,
		None => None,
	};

	Ok(LookupResponse { uri: requested_uri, document, kind })
}

pub async fn run_search(
	config: &PipelineConfig,
	sessions: &SessionStore,
	query: &str,
	limit: usize,
	session: Option<&str>,
) -> Result<RunSearchResponse, AppError> {
	let normalized_session = normalize_session(session);
	let results = qdrant::search_results(config, query, limit).await?;
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
	let addition = graph::build_graph(&client, &spec, roots, 1, 10).await?;
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

	let addition = graph::build_graph(&client, &spec, vec![uri.to_owned()], depth, breadth).await?;
	let expanded = addition.nodes.keys().cloned().collect();
	let graph = match normalized_session.as_deref() {
		Some(session_id) => sessions.merge(session_id, addition).await,
		None => addition.to_response(),
	};

	Ok(ExpandResponse { uri: uri.to_owned(), session: normalized_session, expanded, graph })
}

// ── Terminus helpers ───────────────────────────────────

struct ResolvedSymbol {
	document: JsonValue,
	kind:     Option<JsonValue>,
}

async fn resolve_symbol(
	client: &TerminusDBHttpClient,
	spec: &BranchSpec,
	cache: &mut HashMap<String, Option<JsonValue>>,
	uri: &str,
) -> Result<ResolvedSymbol, AppError> {
	let resolved_uri = EntryUri::parse(uri).map_or_else(|| uri.to_owned(), |u| u.to_string());
	let document = fetch_document(client, spec, cache, &resolved_uri)
		.await?
		.ok_or_else(|| AppError::SymbolNotFound { uri: uri.to_owned() })?;

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

	let document = client
		.get_document_if_exists(uri, spec, GetOpts::default().with_unfold(true))
		.await
		.map_err(|source| AppError::Terminus(TerminusError::DocumentFetch {
			uri: uri.to_owned(),
			source,
		}))?;
	cache.insert(uri.to_owned(), document.clone());
	Ok(document)
}

#[cfg(test)]
mod tests {
	use super::structured_symbol_uri;

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
	fn structured_rust_lookup_uri_canonicalizes_underscored_package() {
		assert_eq!(
			structured_symbol_uri("rust", "cranelift_object::ObjectModule", Some("cranelift_object")),
			"Entry/rust/cranelift-object/cranelift_object::ObjectModule"
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
