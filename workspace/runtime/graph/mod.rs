//! The graph runtime store (terminus) — the **source of truth** for how a
//! package's symbols are structured and related.
//!
//! Keyed on the durable [`SymbolId`] (not the old `Id<Symbol>`), so
//! results join cleanly with tantivy and qdrant. Every method that returns a set
//! streams it.
//!
//! ## Connection lifecycle
//! [`Graph<Cold>`] implements [`Connect`]: `connect()` verifies the endpoint,
//! credentials, and that the org/db exist, then promotes to [`Graph<Live>`]. The
//! verification is **one-time**. Thereafter the [`Live`] handle owns its own
//! reconnection: the pooled HTTP client re-establishes dropped connections
//! transparently, so a transient blip does not require re-running `connect()`.

pub mod expansion;
pub mod resolution;
pub mod structure;

use std::marker::PhantomData;

use futures::Stream;
use secrecy::SecretString;
use serde::{Deserialize, Serialize};
use smol_str::SmolStr;
use url::Url;

use heart::{
	Cold, Connect, ConnectError, SymbolId, Live, Scored,
	StoreError,
};

use crate::error::GraphError;

/// The kind of edge two symbols share. `are_related` returns the specific kind,
/// so callers can branch on *how* two symbols connect, not merely *whether*.
#[derive(
	Debug,
	Clone,
	Copy,
	PartialEq,
	Eq,
	PartialOrd,
	Ord,
	Hash,
	Serialize,
	Deserialize,
	strum::Display,
	strum::EnumString,
)]
pub enum RelationKind {
	/// The target is a member of the source (a method of a type, a field of a
	/// record, an item of a module).
	Member,
	/// The source references the target (a call, a use, a mention).
	Reference,
	/// The target occurs within the source's declaration/signature.
	Occurrence,
	/// The source implements the target (a type implements a trait/interface).
	Implements,
	/// The source extends/subclasses the target.
	Extends,
	/// The source re-exports the target.
	ReExport,
}

/// A trait for objects which hold graph-based symbol relationships, streaming.
/// Implemented by the terminus-backed [`Graph`].
#[diagnostic::on_unimplemented(
	message = "`{Self}` is not a `GraphStore`",
	note = "implement `GraphStore` (e.g. terminus-backed) to surface symbol relationships"
)]
pub trait GraphStore: Send + Sync {
	/// The failure mode of a graph operation. Classifies itself as
	/// [`heart::Retryable`] via the [`StoreError`] bound + the runtime enum.
	type Error: StoreError;

	/// Every symbol whose declaration/signature holds `item` (its occurrences),
	/// scored by relevance and streamed.
	fn get_occurrences(
		&self,
		item: SymbolId,
	) -> impl Stream<Item = Result<Scored<SymbolId>, Self::Error>> + Send;

	/// Everything that points at `item` (its callers/users), scored and streamed.
	fn get_references(
		&self,
		item: SymbolId,
	) -> impl Stream<Item = Result<Scored<SymbolId>, Self::Error>> + Send;

	/// If `from` and `to` are directly linked, the [`RelationKind`] of that link;
	/// `None` if unrelated.
	async fn are_related(
		&self,
		from: SymbolId,
		to: SymbolId,
	) -> Result<Option<RelationKind>, Self::Error>;
}

/// Assert the graph query futures/streams are `Send` (so the store composes into
/// a multi-threaded server) without boxing.
pub fn assert_graph_futures_send<G>()
where
	G: GraphStore<
		get_occurrences(..): Send,
		get_references(..): Send,
		are_related(..): Send,
	>,
{
}

/// Why a raw TerminusDB organization name was rejected.
#[derive(Debug, thiserror::Error)]
pub enum GraphNameError {
	/// The name was empty after trimming.
	#[error("terminus name is empty")]
	Empty,
	/// The name contained characters illegal in a terminus path segment.
	#[error("terminus name {raw:?} is invalid")]
	Invalid {
		/// The offending raw input.
		raw: String,
	},
}

/// The TerminusDB organization a database is namespaced under.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Organization(SmolStr);

/// Whether `raw` is a legal terminus path segment: non-empty ASCII
/// alphanumerics plus `_` and `-` (the conservative common subset, so a name is
/// never URL-escaped or misparsed in an API path).
fn validate_terminus_name(raw: String) -> Result<SmolStr, GraphNameError> {
	let trimmed = raw.trim();
	if trimmed.is_empty() {
		return Err(GraphNameError::Empty);
	}
	if trimmed.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-')) {
		Ok(SmolStr::new(trimmed))
	} else {
		Err(GraphNameError::Invalid { raw })
	}
}

impl Organization {
	/// Validate and wrap an organization name.
	pub fn new(raw: impl Into<String>) -> Result<Self, GraphNameError> {
		validate_terminus_name(raw.into()).map(Self)
	}

	/// The underlying name.
	pub fn as_str(&self) -> &str { &self.0 }
}

/// The name of a TerminusDB database within an [`Organization`]. Also the
/// **instance token** salted into every [`SymbolId`], so it must be stable.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Database(SmolStr);

impl Database {
	/// Validate and wrap a database name.
	pub fn new(raw: impl Into<String>) -> Result<Self, GraphNameError> {
		validate_terminus_name(raw.into()).map(Self)
	}

	/// The underlying name.
	pub fn as_str(&self) -> &str { &self.0 }

	/// The instance token used to salt deterministic symbol ids for this graph.
	pub fn instance_token(&self) -> &str { &self.0 }
}

/// HTTP basic-auth credentials for a TerminusDB endpoint. The password is held
/// in [`SecretString`] so it never lands in a `Debug` render or a log line.
#[derive(Clone)]
pub struct Credentials {
	user: SmolStr,
	password: SecretString,
}

impl Credentials {
	/// Build credentials from a (non-empty) user and a secret password.
	pub fn new(user: impl Into<String>, password: SecretString) -> Result<Self, GraphNameError> {
		let user = user.into();
		let trimmed = user.trim();
		if trimmed.is_empty() {
			return Err(GraphNameError::Empty);
		}
		Ok(Self { user: SmolStr::new(trimmed), password })
	}

	/// The basic-auth user.
	pub fn user(&self) -> &str { &self.user }

	/// The secret password (still wrapped).
	pub fn password(&self) -> &SecretString { &self.password }
}

impl std::fmt::Debug for Credentials {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		f.debug_struct("Credentials").field("user", &self.user).field("password", &"<redacted>").finish()
	}
}

/// Our graph database of choice (terminus). `S` is the connection state
/// ([`Cold`] until [`Connect::connect`] verifies it, then [`Live`]).
pub struct Graph<S = Cold> {
	/// The pooled HTTP client used for every request to the endpoint. Owns its
	/// own reconnection once [`Live`].
	client: reqwest::Client,

	/// The base URL of the TerminusDB server.
	endpoint: Url,

	/// The organization + database the documents live in.
	organization: Organization,
	database: Database,

	/// The credentials presented on each request.
	credentials: Credentials,

	_state: PhantomData<fn() -> S>,
}

impl Graph<Cold> {
	/// Configure a cold handle. Reachability/credentials/schema are only checked
	/// at [`Connect::connect`].
	pub fn new(
		client: reqwest::Client,
		endpoint: Url,
		organization: Organization,
		database: Database,
		credentials: Credentials,
	) -> Self {
		Self { client, endpoint, organization, database, credentials, _state: PhantomData }
	}
}

/// Classify a reqwest transport failure into a [`heart::ConnectFailure`].
fn transport_failure(error: reqwest::Error) -> heart::ConnectFailure {
	use heart::ConnectFailure;
	if error.is_timeout() {
		ConnectFailure::Timeout
	} else if error.is_connect() {
		ConnectFailure::Unreachable
	} else {
		ConnectFailure::Other(error.into())
	}
}

impl Connect for Graph<Cold> {
	type Live = Graph<Live>;

	/// Authenticate against terminus and verify the org/db exist (one-time),
	/// then promote to [`Live`]. Thereafter the client handles reconnection.
	async fn connect(self) -> Result<Self::Live, ConnectError> {
		use heart::{BackendKind, ConnectFailure};
		use secrecy::ExposeSecret;

		let terminus = BackendKind::Terminus;
		let fail = |kind| ConnectError::new(terminus, kind);

		// Reachability + credentials: `/api/info` is authenticated and cheap.
		let info = self
			.endpoint
			.join("api/info")
			.map_err(|error| fail(ConnectFailure::Other(error.into())))?;
		let reply = self
			.client
			.get(info)
			.basic_auth(self.credentials.user(), Some(self.credentials.password().expose_secret()))
			.send()
			.await
			.map_err(|error| fail(transport_failure(error)))?;
		match reply.status() {
			status if status.is_success() => {}
			reqwest::StatusCode::UNAUTHORIZED | reqwest::StatusCode::FORBIDDEN => {
				return Err(fail(ConnectFailure::Auth));
			}
			status => {
				return Err(fail(ConnectFailure::Other(
					anyhow::anyhow!("terminus /api/info answered {status}"),
				)));
			}
		}

		// Existence of the org/db pair — the schema this handle serves from.
		let database = self
			.endpoint
			.join(&format!("api/db/{}/{}", self.organization.as_str(), self.database.as_str()))
			.map_err(|error| fail(ConnectFailure::Other(error.into())))?;
		let reply = self
			.client
			.get(database)
			.basic_auth(self.credentials.user(), Some(self.credentials.password().expose_secret()))
			.send()
			.await
			.map_err(|error| fail(transport_failure(error)))?;
		match reply.status() {
			status if status.is_success() => {}
			reqwest::StatusCode::UNAUTHORIZED | reqwest::StatusCode::FORBIDDEN => {
				return Err(fail(ConnectFailure::Auth));
			}
			reqwest::StatusCode::NOT_FOUND => return Err(fail(ConnectFailure::SchemaMismatch)),
			status => {
				return Err(fail(ConnectFailure::Other(
					anyhow::anyhow!("terminus db check answered {status}"),
				)));
			}
		}

		tracing::info!(
			endpoint = %self.endpoint,
			organization = self.organization.as_str(),
			database = self.database.as_str(),
			"graph store verified; promoting to Live"
		);
		let Graph { client, endpoint, organization, database, credentials, .. } = self;
		Ok(Graph { client, endpoint, organization, database, credentials, _state: PhantomData })
	}
}

// ─────────────────────────────────────────────────────────────────────────────
// The assumed terminus document model (written by the emit pipeline):
//
// - class `Symbol` — one document per symbol, string properties `id` (uuid),
//   `package` (uuid), `ecosystem` (lowercase [`heart::Language`] token),
//   `plain`, `fully_qualified`, and `kind` ([`heart::SymbolKind`] name);
// - class `Relation` — one document per edge, string properties `from` (uuid),
//   `kind` ([`RelationKind`] name), and `to` (uuid).
//
// Everything below queries that model through WOQL JSON-LD over HTTP.
// ─────────────────────────────────────────────────────────────────────────────

/// Minimal WOQL JSON-LD builders — just the fragment of the algebra the
/// runtime's read queries need (typed triples, conjunction, projection).
mod woql {
	use serde_json::{Value, json};

	/// A data-position variable.
	pub fn variable(name: &str) -> Value {
		json!({ "@type": "Value", "variable": name })
	}

	/// A concrete `xsd:string` literal in data position.
	pub fn string(value: &str) -> Value {
		json!({ "@type": "Value", "data": { "@type": "xsd:string", "@value": value } })
	}

	/// `Triple(?subject, @schema:property, object)`.
	pub fn triple(subject: &str, property: &str, object: Value) -> Value {
		json!({
			"@type": "Triple",
			"subject": { "@type": "NodeValue", "variable": subject },
			"predicate": { "@type": "NodeValue", "node": format!("@schema:{property}") },
			"object": object,
		})
	}

	/// `Triple(?subject, rdf:type, @schema:class)` — pin the document class.
	pub fn is_a(subject: &str, class: &str) -> Value {
		json!({
			"@type": "Triple",
			"subject": { "@type": "NodeValue", "variable": subject },
			"predicate": { "@type": "NodeValue", "node": "rdf:type" },
			"object": { "@type": "Value", "node": format!("@schema:{class}") },
		})
	}

	/// Conjunction.
	pub fn and(queries: Vec<Value>) -> Value {
		json!({ "@type": "And", "and": queries })
	}

	/// Projection onto `variables`.
	pub fn select(variables: &[&str], query: Value) -> Value {
		json!({ "@type": "Select", "variables": variables, "query": query })
	}
}

/// A decoded-but-wrong payload, reported through [`GraphError::Decode`] with a
/// message that names exactly what was malformed.
fn decode_error(message: impl std::fmt::Display) -> GraphError {
	GraphError::Decode(<serde_json::Error as serde::de::Error>::custom(message))
}

/// Pull the string content of one bound variable out of a WOQL binding row
/// (literals arrive as `{"@type": "xsd:...", "@value": ...}`, nodes as strings).
fn binding_string<'row>(
	row: &'row serde_json::Map<String, serde_json::Value>,
	variable: &str,
) -> Result<&'row str, GraphError> {
	let value = row
		.get(variable)
		.ok_or_else(|| decode_error(format_args!("binding missing variable {variable:?}")))?;
	match value {
		serde_json::Value::String(node) => Ok(node),
		serde_json::Value::Object(literal) => literal
			.get("@value")
			.and_then(serde_json::Value::as_str)
			.ok_or_else(|| decode_error(format_args!("variable {variable:?} is not a string literal"))),
		other => Err(decode_error(format_args!("variable {variable:?} bound to unexpected {other}"))),
	}
}

/// Parse a bound variable as a [`SymbolId`] (stored as its uuid string).
fn binding_symbol(
	row: &serde_json::Map<String, serde_json::Value>,
	variable: &str,
) -> Result<SymbolId, GraphError> {
	let raw = binding_string(row, variable)?;
	raw.parse::<heart::Guid>()
		.map(SymbolId::from_uuid)
		.map_err(|_| decode_error(format_args!("variable {variable:?} holds non-uuid {raw:?}")))
}

/// Parse a [`RelationKind`] token as stored on `Relation.kind`.
fn parse_relation_kind(raw: &str) -> Result<RelationKind, GraphError> {
	raw.parse().map_err(|_| decode_error(format_args!("unknown relation kind {raw:?}")))
}

/// The score assigned to a direct (depth-one) graph relation — graph edges are
/// facts, not fuzzy matches, so a hit is fully relevant.
pub(crate) fn direct_score() -> heart::Score {
	heart::Score::try_new(1.0).expect("1.0 is finite")
}

impl Graph<Live> {
	/// POST one WOQL query to the endpoint and return its binding rows.
	pub(crate) async fn bindings(
		&self,
		query: serde_json::Value,
	) -> Result<Vec<serde_json::Map<String, serde_json::Value>>, GraphError> {
		use secrecy::ExposeSecret;

		let url = self
			.endpoint
			.join(&format!("api/woql/{}/{}", self.organization.as_str(), self.database.as_str()))
			.expect("validated org/db names always form a legal url path");
		let body = serde_json::to_vec(&serde_json::json!({ "query": query }))
			.expect("woql queries are plain json trees and serialize infallibly");
		let reply = self
			.client
			.post(url)
			.basic_auth(self.credentials.user(), Some(self.credentials.password().expose_secret()))
			.header(reqwest::header::CONTENT_TYPE, "application/json")
			.body(body)
			.send()
			.await
			.map_err(GraphError::Transport)?;

		let status = reply.status();
		let bytes = reply.bytes().await.map_err(GraphError::Transport)?;
		if !status.is_success() {
			return Err(GraphError::Query(crate::error::GraphQueryError::HttpStatus {
				status,
				body: String::from_utf8_lossy(&bytes).into_owned(),
			}));
		}

		#[derive(serde::Deserialize)]
		struct Reply {
			#[serde(default)]
			bindings: Vec<serde_json::Map<String, serde_json::Value>>,
		}
		let reply: Reply = serde_json::from_slice(&bytes).map_err(GraphError::Decode)?;
		tracing::debug!(rows = reply.bindings.len(), "woql query answered");
		Ok(reply.bindings)
	}

	/// The symbols on the named `side` of `kind`-edges whose other side is
	/// `anchor` — the shared body of `get_occurrences` / `get_references`.
	async fn edge_endpoints(
		&self,
		anchor_property: &str,
		anchor: SymbolId,
		kind: RelationKind,
		yielded_property: &str,
	) -> Result<Vec<Scored<SymbolId>>, GraphError> {
		let query = woql::select(
			&["Yielded"],
			woql::and(vec![
				woql::is_a("Edge", "Relation"),
				woql::triple("Edge", anchor_property, woql::string(&anchor.as_uuid().to_string())),
				woql::triple("Edge", "kind", woql::string(&kind.to_string())),
				woql::triple("Edge", yielded_property, woql::variable("Yielded")),
			]),
		);
		self.bindings(query)
			.await?
			.iter()
			.map(|row| binding_symbol(row, "Yielded").map(|id| Scored::new(id, direct_score())))
			.collect()
	}

	/// All outgoing `(kind, target)` edges of `origin` — the expansion primitive.
	pub(crate) async fn outgoing_edges(
		&self,
		origin: SymbolId,
	) -> Result<Vec<(RelationKind, SymbolId)>, GraphError> {
		let query = woql::select(
			&["Kind", "Target"],
			woql::and(vec![
				woql::is_a("Edge", "Relation"),
				woql::triple("Edge", "from", woql::string(&origin.as_uuid().to_string())),
				woql::triple("Edge", "kind", woql::variable("Kind")),
				woql::triple("Edge", "to", woql::variable("Target")),
			]),
		);
		self.bindings(query)
			.await?
			.iter()
			.map(|row| {
				Ok((
					parse_relation_kind(binding_string(row, "Kind")?)?,
					binding_symbol(row, "Target")?,
				))
			})
			.collect()
	}

	/// Every symbol document belonging to `package`, fully hydrated.
	pub(crate) async fn symbols_in_package(
		&self,
		package: heart::PackageId,
	) -> Result<Vec<heart::Symbol>, GraphError> {
		let query = woql::select(
			&["Id", "Ecosystem", "Plain", "FullyQualified", "Kind"],
			woql::and(vec![
				woql::is_a("Doc", "Symbol"),
				woql::triple("Doc", "package", woql::string(&package.as_uuid().to_string())),
				woql::triple("Doc", "id", woql::variable("Id")),
				woql::triple("Doc", "ecosystem", woql::variable("Ecosystem")),
				woql::triple("Doc", "plain", woql::variable("Plain")),
				woql::triple("Doc", "fully_qualified", woql::variable("FullyQualified")),
				woql::triple("Doc", "kind", woql::variable("Kind")),
			]),
		);
		self.bindings(query)
			.await?
			.iter()
			.map(|row| {
				let ecosystem = binding_string(row, "Ecosystem")?;
				let kind = binding_string(row, "Kind")?;
				Ok(heart::Symbol {
					id: binding_symbol(row, "Id")?,
					package,
					ecosystem: ecosystem
						.parse()
						.map_err(|_| decode_error(format_args!("unknown ecosystem token {ecosystem:?}")))?,
					name: heart::Name {
						plain: binding_string(row, "Plain")?.into(),
						fully_qualified: binding_string(row, "FullyQualified")?.into(),
					},
					kind: parse_symbol_kind(kind)?,
				})
			})
			.collect()
	}

	/// One symbol document by id, if the graph holds it.
	pub(crate) async fn symbol_record(
		&self,
		id: SymbolId,
	) -> Result<Option<heart::Symbol>, GraphError> {
		let query = woql::select(
			&["Package", "Ecosystem", "Plain", "FullyQualified", "Kind"],
			woql::and(vec![
				woql::is_a("Doc", "Symbol"),
				woql::triple("Doc", "id", woql::string(&id.as_uuid().to_string())),
				woql::triple("Doc", "package", woql::variable("Package")),
				woql::triple("Doc", "ecosystem", woql::variable("Ecosystem")),
				woql::triple("Doc", "plain", woql::variable("Plain")),
				woql::triple("Doc", "fully_qualified", woql::variable("FullyQualified")),
				woql::triple("Doc", "kind", woql::variable("Kind")),
			]),
		);
		self.bindings(query)
			.await?
			.first()
			.map(|row| {
				let ecosystem = binding_string(row, "Ecosystem")?;
				let kind = binding_string(row, "Kind")?;
				Ok(heart::Symbol {
					id,
					package: binding_symbol(row, "Package")?.cast(),
					ecosystem: ecosystem
						.parse()
						.map_err(|_| decode_error(format_args!("unknown ecosystem token {ecosystem:?}")))?,
					name: heart::Name {
						plain: binding_string(row, "Plain")?.into(),
						fully_qualified: binding_string(row, "FullyQualified")?.into(),
					},
					kind: parse_symbol_kind(kind)?,
				})
			})
			.transpose()
	}

	/// The `Member` parent-edges among a package's symbols: child id → (relation,
	/// parent id). Feeds [`structure::assemble`].
	pub(crate) async fn member_parents(
		&self,
		package: heart::PackageId,
	) -> Result<std::collections::BTreeMap<SymbolId, (RelationKind, SymbolId)>, GraphError> {
		let query = woql::select(
			&["Parent", "Child"],
			woql::and(vec![
				woql::is_a("Edge", "Relation"),
				woql::triple("Edge", "kind", woql::string(&RelationKind::Member.to_string())),
				woql::triple("Edge", "from", woql::variable("Parent")),
				woql::triple("Edge", "to", woql::variable("Child")),
				// Constrain to edges whose child lives in this package.
				woql::is_a("Doc", "Symbol"),
				woql::triple("Doc", "id", woql::variable("Child")),
				woql::triple("Doc", "package", woql::string(&package.as_uuid().to_string())),
			]),
		);
		self.bindings(query)
			.await?
			.iter()
			.map(|row| {
				Ok((
					binding_symbol(row, "Child")?,
					(RelationKind::Member, binding_symbol(row, "Parent")?),
				))
			})
			.collect()
	}
}

/// Parse a [`heart::SymbolKind`] from its stored `Display` name (the same token
/// the registry persists), rejecting anything unknown rather than guessing.
pub(crate) fn parse_symbol_kind(raw: &str) -> Result<heart::SymbolKind, GraphError> {
	use heart::SymbolKind::*;
	match raw {
		"Function" => Ok(Function),
		"Type" => Ok(Type),
		"Module" => Ok(Module),
		"Constant" => Ok(Constant),
		"Variable" => Ok(Variable),
		"Trait" => Ok(Trait),
		"Impl" => Ok(Impl),
		"Other" => Ok(Other),
		_ => Err(decode_error(format_args!("unknown symbol kind {raw:?}"))),
	}
}

impl GraphStore for Graph<Live> {
	type Error = GraphError;

	fn get_occurrences(
		&self,
		item: SymbolId,
	) -> impl Stream<Item = Result<Scored<SymbolId>, Self::Error>> + Send {
		use futures::TryFutureExt;
		// Holders are the *sources* of Occurrence edges pointing at `item`.
		async move {
			let hits = self.edge_endpoints("to", item, RelationKind::Occurrence, "from").await?;
			Ok(futures::stream::iter(hits.into_iter().map(Ok)))
		}
		.try_flatten_stream()
	}

	fn get_references(
		&self,
		item: SymbolId,
	) -> impl Stream<Item = Result<Scored<SymbolId>, Self::Error>> + Send {
		use futures::TryFutureExt;
		async move {
			let hits = self.edge_endpoints("to", item, RelationKind::Reference, "from").await?;
			Ok(futures::stream::iter(hits.into_iter().map(Ok)))
		}
		.try_flatten_stream()
	}

	async fn are_related(
		&self,
		from: SymbolId,
		to: SymbolId,
	) -> Result<Option<RelationKind>, Self::Error> {
		let query = woql::select(
			&["Kind"],
			woql::and(vec![
				woql::is_a("Edge", "Relation"),
				woql::triple("Edge", "from", woql::string(&from.as_uuid().to_string())),
				woql::triple("Edge", "to", woql::string(&to.as_uuid().to_string())),
				woql::triple("Edge", "kind", woql::variable("Kind")),
			]),
		);
		self.bindings(query)
			.await?
			.first()
			.map(|row| parse_relation_kind(binding_string(row, "Kind")?))
			.transpose()
	}
}
