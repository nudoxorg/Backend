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


/// The persitent store for blobs
/// Meant to be used with a higher-level coordinator, as this is a slim wrapper for a content-addressed store, the coordinator is meant to assign these content-addressed blobs to the actual GUIDs.
pub trait BlobStore {
	/// Put a blob into the store, and get the hashed return value
	async fn put(&self, blob: Blob) -> Result<Hash>;

	/// Get a blob from the store
	async fn get(&self, hash: Hash) -> Result<Blob>;

	/// Update a blob within the store
	/// Any failure would be propogation of the store, which I supose would only be something network related
	async fn update(&self) -> Result<()>;

	// Because this content-addressed, I think listing is pointless ? Maybe iterating through?
	// Will likely sit on top of: https://lib.rs/crates/object_store
}

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
pub trait Sink {
	/// The thing that we're uploading
	type UploadObject;

	/// How many upload objects we should attempt to push at one time
	type ChunkSize;

	/// How we're going to approach the upload, and what to do under an error?
	type RetryStrategy;

	/// Upload the documents to the store
	async fn upload(object: Self::UploadObject) -> Result<()>;
}

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

