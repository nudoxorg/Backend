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
	AccessContext, Cold, Connect, ConnectError, SymbolId, Live, Scored,
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
		scope: &AccessContext,
	) -> impl Stream<Item = Result<Scored<SymbolId>, Self::Error>> + Send;

	/// Everything that points at `item` (its callers/users), scored and streamed.
	fn get_references(
		&self,
		item: SymbolId,
		scope: &AccessContext,
	) -> impl Stream<Item = Result<Scored<SymbolId>, Self::Error>> + Send;

	/// If `from` and `to` are directly linked, the [`RelationKind`] of that link;
	/// `None` if unrelated.
	async fn are_related(
		&self,
		from: SymbolId,
		to: SymbolId,
		scope: &AccessContext,
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

impl Organization {
	/// Validate and wrap an organization name.
	pub fn new(raw: impl Into<String>) -> Result<Self, GraphNameError> {
		let _ = raw;
		todo!("trim, reject empty, validate terminus-legal charset")
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
		let _ = raw;
		todo!("trim, reject empty, validate terminus-legal charset")
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
		let _ = (user, password);
		todo!("reject empty user; wrap")
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

impl Connect for Graph<Cold> {
	type Live = Graph<Live>;

	/// Authenticate against terminus and verify the org/db exist (one-time),
	/// then promote to [`Live`]. Thereafter the client handles reconnection.
	async fn connect(self) -> Result<Self::Live, ConnectError> {
		let _ = (&self.client, &self.endpoint, &self.organization, &self.database, &self.credentials);
		todo!("authenticate + verify org/db + schema, then go Live")
	}
}

impl GraphStore for Graph<Live> {
	type Error = GraphError;

	fn get_occurrences(
		&self,
		item: SymbolId,
		scope: &AccessContext,
	) -> impl Stream<Item = Result<Scored<SymbolId>, Self::Error>> + Send {
		let _ = (item, scope, &self.client);
		todo!("WOQL: symbols whose declaration holds `item`, access-filtered");
		#[allow(unreachable_code)]
		futures::stream::empty()
	}

	fn get_references(
		&self,
		item: SymbolId,
		scope: &AccessContext,
	) -> impl Stream<Item = Result<Scored<SymbolId>, Self::Error>> + Send {
		let _ = (item, scope, &self.client);
		todo!("WOQL: symbols referencing `item`, access-filtered");
		#[allow(unreachable_code)]
		futures::stream::empty()
	}

	async fn are_related(
		&self,
		from: SymbolId,
		to: SymbolId,
		scope: &AccessContext,
	) -> Result<Option<RelationKind>, Self::Error> {
		let _ = (from, to, scope, &self.client);
		todo!("WOQL: the edge kind between `from` and `to`, if any")
	}
}
