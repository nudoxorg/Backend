//! The graph runtime store (terminus) for surfacing direct/known relationships
//! between symbols.

pub mod expansion;
pub mod resolution;

use heart::{Id, StoreError, Symbol};
use secrecy::SecretString;
use url::Url;

/// A trait for objects which are or hold graph-based data storing mechanisms, for surfacing direct/known relationships
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

/// The TerminusDB organization a database is namespaced under.
pub struct Organization(String);

/// The name of a TerminusDB database within an [`Organization`].
pub struct Database(String);

/// HTTP basic-auth credentials for a TerminusDB endpoint.
pub struct Credentials {
	user: String,
	password: SecretString,
}

/// Our graph database of choice (terminus)
pub struct Graph {
	/// The pooled HTTP client used for every request to the endpoint.
	client: reqwest::Client,

	/// The base URL of the TerminusDB server.
	endpoint: Url,

	/// The organization + database the documents live in.
	organization: Organization,
	database: Database,

	/// The credentials presented on each request.
	credentials: Credentials,
}
