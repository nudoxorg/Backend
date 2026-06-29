//! Any record that a dependency or crate exists on an external registry don't
//! have to be typed/explicitly recorded in any real way. I think at best we
//! just want to link out to it, but to any processing code, the location is
//! weird overhead, they just need the text.
//!
//!
//!
//!
//! When it comes to storing the information as a blob, treesitter isn't a
//! lossless parse, and we should maintain the text alongside it, in the same
//! blob. For this we need to have a (sort of) custom binary serialization
//! process (probably just bincode)

pub enum EmbeddingPurpose {
	/// We're creating embeddings based on the code/surface
	Code,
	/// We're creating embeddings based on the documentation around a code object
	/// (like this right here)
	Documentation,
}

/// A hit for a search result or otherwise
pub struct Hit<T> {
	pub value: T,
	pub score: Option<f32>
}

//! I figure we don't need a special embedding type -- it seems a nonempty vec would do all of the heavy lifting we need it to do.


/// Not worth keeping this as an enum, as they're proprietary for the embedding model
const EMBEDDING_MODEL: &'static str = "text-embedding-3-small";

/// A particular language variant, and its identifying information for the toolchain it was built on (so like rust 1.89 + edition 2024) Some languages will only need a version.
pub enum Language {
	/// https://rust-lang.org/
	Rust(Version),
	/// https://www.typescriptlang.org/
	Typescript(Version)
}


pub enum ResolutionState {
	/// No indexing has been attempted yet
	Unindexed {
		/// Indexing for a package which is related to this has been accomplished, and is incomplete with this package left untouched
	 needed: bool
	},
	/// The indexing is in progress, and will soon be ready to work with.
	Progressing(Phase),
	/// All phases complete, stored and actionable
	Stored
}

/// The phase that the indexing is currently in, and the particular progress that that phase has gone through
pub enum Phase {
	/// The compiler is chewing through it
	Compiling,
	/// Treesitter has chewed through the library and produced a concrete tree
	Treesat

	// I don't know the rest of this
}


//! The concept of a resolution outcome is frankly pointless, A package always holds a state, it's not something with an "outcome" if we need to re-index, then we change the state. The outcome is just weird and voltaile.


/// An abstract query, fed to qdrant for responses based on semantic similarity rather than some ground response
pub enum AbstractQuery{
	/// A query like "error types" or "stuff in axum" that isn't structured in any kind of particular way
	NaturalLanguage(String),

	/// An (assumed to be) code snippet for an assumed language (little we can do to verify either)
	CodeSnippet(String)
}

pub enum Query {
	Abstract(AbstractQuery),
	Literal(String), // TODO: Replace with the native tantivy search type
}

pub struct Search {
	filter: Filter,
	query: Query
	// TODO: Find out the best way to support things like AND and OR (various ways to chain?)
}

/// The scope for which a search takes place
// Should maybe be a generic so we don't make people use vec![] for single language queries (also should be non-empty)
// Also this is something that would be a parameter on a search, as an optional (with a default implementation) to avoid a ton of optionals on fields
pub struct Filter {
	/// The language(s) we should search under
	pub language: Vec<Language>,

	/// The package(s) we should search under
	pub package: Vec<Package>,

	/// The range of packages we want to see
	/// You would use this to bound the number of results
	pub limits: Range<NonZeroUsize>

	// Establish a curtain of popularity, only showing results that... i don't think this is that useful TODO: remove?
	// occurences: Occurences
}

/// We pulled a match !
/// Includes all of the possible useful information about the said match
/// Mainly used temporarily when displaying results
// Because we're only showing this for a moment, things like symbol origination are dumb (inferrable by context?) and same for metadata and embeddings and lifecycle(bruh)
pub struct Match {
	// TODO: Change to the actual type for symbols (which should include source)
	/// The fully qualified name of the symbol
	pub symbol_name: String,

	/// The kind of thing this is
	pub kind: Kind,
}

//! Not doing any kind of embedder trait, because again, it's just keyed to something specific, we're not going to have more of these unfort


// TODO: Wire up postgres types for global indexing, and the parse queue and association (establishing a link)

/// A trait for stores/targets of search to support both abstract and literal search queries.
pub trait SearchTarget {
	/// Make a search, returning a stream of results
	async fn search(&self, request: &Search, scope: Option<Filter>) -> Result<impl Stream<Item = Result<Hit>>>;

	/// Find an item by an id
	async fn get_by_id(&self, id: Guid) -> Result<Hit>;

	// Again I don't believe in listing
}

/// A trait for objects which are or hold graph-based data storing mechanisms, for surfacing direct/known relationships
pub trait GraphStore {
	// TODO: Store the kind of relation two items have? Like is this a member?
	// TODO: Make all streaming

	/// Return all of the symbols which hold the input within their declaration
	async fn get_occurences(&self, item: Guid) -> Vec<Guid> {}

	/// What points to this?
	async fn get_references(&self, item: Guid) -> Vec<Guid>;

	/// Determine if two items are related
	async fn are_related(&self, from: Guid, to: Guid) -> bool;
}

/// A store of symbols with both semantic and precise search
pub trait SymbolStore: SearchTarget + GraphStore {
	/// Given a hit from search, walk its relationships and score them
    async fn related_hits(&self, hit: &Hit) -> Result<Vec<Hit>>;
}

/// Our server configuration
/// This is not meant to be constructed in any other way besides "default", as this is a private-facing configuration, where for simplicity we want static defaults
pub struct ServerConfiguration {
	serving_address: SocketAddr,
	remote_dependencies: HashSet<String, Url>
	// TODO: Add fields for the rest of the configuration for these databases or simply scope it in the function bodies? Don't see a reason to carry most of this data around
}

impl Default for ServerConfiguration {
	fn default() -> Self {
		let serving_address = "1000";

		// We assign them here as untyped key/value pairs, typing them through the methods
		// TODO: Write as an enum?
		let server_friends = HashMap::from(("terminus", "1000"));

		Self {
			serving_address
		}
	}
}

impl ServerConfiguration {
	/// The amount of time we're going to wait before we give up on uploading a document
	const UPLOAD_TIMEOUT: u32 = 2000;

	fn terminus_endpoint(&self) -> Url {
		self.remote_dependencies.get("terminus").unwrap()
	}
}

//! Also I'm not messing with any of the env stuff. I think we'll be able to get with configuration flags for most of this, just having separate default implementation/etc for dev/prod/other

pub struct Server {
	// TODO: Figure out connection types
	pub graph: Graph,
	pub semantics: Semantic,
	pub global_store: GlobalStore,
}

impl Server {

}

/// Our graph database of choice (terminus)
pub struct Graph {

}

/// Our semantic/vector embedding database of choice (Qdrant)
pub struct Semantic {

}

/// Our globalstore/connective tissue (postgres)
pub struct GlobalStore {

}

/// A remote sink or data place, that we reach out to process information
/// Used for any external/remote source that is ingesting information that we;re producing here
pub trait Sink: Sync {
	// TODO: Either here or in another mechanism add support for multiple parents/sources

	/// The thing that we're uploading
	type Item: Sync;

	/// The failure mode of an upload.
    type Error: std::error::Error + Send + Sync + 'static;

	/// How we're going to approach the upload, and what to do under an error?
	type RetryStrategy;

	type RetryMechanism: FnMut();

	/// Upload the documents to the store
	async fn upload_mechanism(&self, object: Self::UploadObject) -> Result<()>;

	/// How we're going to handle an opportunity to try again
	fn backoff(&self) -> impl BackoffBuilder {
		// Uses https://crates.io/crates/backon
		ExponentialBuilder::default().with_jitter();
	}

	/// Did we get an error that's unproblematic and avoidable?
	fn retryable(error: &Self::Error);

	/// Handle the process of delivering the record
	async fn deliver(&self, item: &Self::Item, retry: RetryMechamism) {
		// Combine all of our expressive work
		self.upload_mechanism().retry(self.backoff()).when(self.retryable).await
	}
}

/// A sink which responds well to batch operators
pub trait BatchSink: Sink {}
// Seems like qdrant and terminus both have constants for concurrency or batching, and I think this could be resolved to just one abstraction, but still am working on conceiving it

/// Our trait for anything that can communicate progress or hold an in-between state
pub trait Progressive {
	/// The information the item is observing to update its internal progress, can be just the item itself.
	type Change;

	/// Get the progress of this struct based on an internal calculation
	fn get_progress(&self) -> u8;

	/// Update the progress of the object with a new variant of the struct, diffing and incorporating
	fn update_progress(mut self, information: Self::Change);

	/// Returns `true` if progress has reached a terminal state.
    fn is_complete(&self) -> bool {
        self.get_progress() >= 100
    }

	/// The action to take when the progress has reached a natural end or a stopping point.
	/// Called when progress reaches a natural end.
    fn on_complete(&self, mut callback: impl FnMut());
}

/// A registry that holds all of the packages and metadata for a particular language, including their code.
pub trait Registry {
	/// The particular language that this is oworking within
	const LANGUAGE: Language;

	/// The package that the registry is in charge of holding/indexing over
	type Package;

	// TODO: Find some way to avoid this annoying error repition

	/// The failure mode for this registry
    type Error: std::error::Error + Send + Sync + 'static;
}

/// A store of packages, of some flavor.
pub trait ReadRegistry: Registry {
	/// An enum that represents various supported filters/conditions for search and listing
	type Condition;

	const Language: Language;

	// TODO: Add some kind of mechanism for abstracting over how ranking should be handled? I wonder about splitting this into two traits, one for frontend/read-only and one for backend/mutability?

	/// List the packages in this registry
	async fn list(&self, conditions: &[Self::Condition]);

	async fn get(&self, package: &Self::Package) -> Result<Option<Package>>;

	/// Search through the packages in this registry
	async fn search(&self, conditions: &[Self::Condition]);
}

pub trait WriteRegistry: Registry {
	/// The package that the registry is in charge of holding/indexing over
	type Payload;

	/// The structure that defines how popularity should be decided
	// TODO: Should by closure, maybe? Idk what this is serializing against
	type RankingPolicy;

	/// Publish a package to the registry.
	/// Returns the final, global package object, for you to syndicate out to the global store
	async fn publish(&self, payload: Payload) -> Result<Self::Package>;

	/// Alter the engines ranking system for a registry
	async fn rank(&mut self, policy: RankingPolicy) -> Result<()>;

	/// Change the state of an already-published package, name, description, yank status, etc.
	/// Not expected to be called often, but should also signify any of the dependent infra
	/// Returns the newly minted global package object
	async fn modify(&self, payload: Payload, package: Package) -> Result<Self::Package>;

	// Will likely sit on top of: https://lib.rs/crates/object_store
}
