//! Heart — the methods and types that are most widely shared across modules,
//! usually common or shared generic types.
//!
//! Anything that is specific to a single subsystem (the registry, the runtime
//! stores, the compiler, the server) lives in that subsystem's crate. What
//! remains here is deliberately small and generic: the type-tagged identifier,
//! the scored-result wrapper, the language enum, the versioned wrapper, and the
//! cross-cutting `Progressive` / `Sink` behavioural traits.
#![feature(adt_const_params)]

pub mod access;
pub mod connection;
pub mod hit;
pub mod identifier;
pub mod language;
pub mod model;
pub mod progress;
pub mod score;
pub mod sink;
pub mod version;

pub use connection::{Cold, Live};
pub use hit::Hit;
pub use identifier::Id;
pub use language::{Language, LanguageTag};
pub use model::ModelId;
pub use progress::Progressive;
pub use score::Score;
pub use sink::{BatchSink, Sink};
pub use version::Versioned;

/// A shared bound for the error type of any store/backend in the system, so the
/// subsystem traits can write `type Error: StoreError` without re-stating the
/// `std::error::Error + Send + Sync + 'static` litany every time.
pub trait StoreError: std::error::Error + Send + Sync + 'static {}
impl<T: std::error::Error + Send + Sync + 'static> StoreError for T {}

/// A globally-unique identifier — the raw UUID that backs an [`Id`].
pub type Guid = uuid::Uuid;

/// The kind of thing a [`Symbol`] is — the shared taxonomy used by both the
/// search results and the graph layer.
pub enum SymbolKind {
	Function,
	Struct,
	Enum,
	Trait,
	Method,
	Closure,
	TypeAlias,
	Const,
	Other,
}

/// The canonical symbol record shared by the search/graph layers.
pub struct Symbol {
	/// The stable, version-agnostic global identity of this symbol.
	pub id: Id<Symbol>,

	/// The bare symbol name (e.g. `Router`).
	pub name: String,

	/// The fully-qualified name (e.g. `axum::Router`).
	pub fq_name: String,

	/// What kind of thing this symbol is.
	pub kind: SymbolKind,
}
