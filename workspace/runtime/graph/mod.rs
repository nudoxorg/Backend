//! The graph runtime store (terminus) for surfacing direct/known relationships
//! between symbols.

pub mod expansion;
pub mod resolution;
pub mod structure;

use std::marker::PhantomData;

use heart::{Cold, Id, Live, StoreError, Symbol};
use secrecy::SecretString;
use url::Url;

/// A trait for objects which are or hold graph-based data storing mechanisms,
/// for surfacing direct/known relationships
#[diagnostic::on_unimplemented(
	message = "`{Self}` is not a `GraphStore`",
	note = "implement `GraphStore` (e.g. terminus-backed) to surface symbol relationships"
)]
pub trait GraphStore {
	type Error: StoreError;

	// TODO: Store the kind of relation two items have? Like is this a member?
	// TODO: Make all streaming

	/// Return all of the symbols which hold the input within their declaration
	async fn get_occurrences(&self, item: Id<Symbol>) -> Result<Vec<Id<Symbol>>, Self::Error>;

	/// What points to this?
	async fn get_references(&self, item: Id<Symbol>) -> Result<Vec<Id<Symbol>>, Self::Error>;

	/// Determine if two items are related.
	async fn are_related(&self, from: Id<Symbol>, to: Id<Symbol>) -> Result<bool, Self::Error>;
}

/// Some boilerplate to avoid needless boxing
pub fn assert_graph_futures_send<G>()
where
	G: GraphStore,
	G::get_occurrences(..): Send,
	G::get_references(..): Send,
	G::are_related(..): Send,
{
}

/// The TerminusDB organization a database is namespaced under.
pub struct Organization(String);

/// The name of a TerminusDB database within an [`Organization`].
pub struct Database(String);

/// HTTP basic-auth credentials for a TerminusDB endpoint.
pub struct Credentials {
	user:     String,
	password: SecretString,
}

/// Raised when a `Cold` graph store fails to come up.
#[derive(Debug)]
pub struct ConnectError;

/// Our graph database of choice (terminus). `S` is the connection state
/// ([`Cold`] until [`connect`](Graph::connect) verifies it, then [`Live`]).
pub struct Graph<S = Cold> {
	/// The pooled HTTP client used for every request to the endpoint.
	client: reqwest::Client,

	/// The base URL of the TerminusDB server.
	endpoint: Url,

	/// The organization + database the documents live in.
	organization: Organization,
	database:     Database,

	/// The credentials presented on each request.
	credentials: Credentials,

	_state: PhantomData<S>,
}

impl Graph<Cold> {
	/// Verify the endpoint/credentials and the org/db exist, promoting the handle
	/// to [`Live`]. Query methods exist only on the `Live` form.
	pub async fn connect(self) -> Result<Graph<Live>, ConnectError> {
		let _ = (&self.client, &self.endpoint, &self.organization, &self.database, &self.credentials);
		todo!("authenticate against terminus + verify org/db, then go Live")
	}
}
