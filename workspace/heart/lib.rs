//! Our shared vocabulary.
#![feature(adt_const_params)]
#![feature(return_type_notation)]

pub mod access;
pub mod connection;
pub mod content;
pub mod cursor;
pub mod ecosystem;
pub mod error;
pub mod identity;
pub mod lifecycle;
pub mod progress;
pub mod score;
pub mod search;
pub mod sink;
pub mod symbol;
pub mod version;

pub use access::{
	AccessContext, AccessDecision, Federation, Principal, Source, SourceId, SourceRole, Sourced,
	Tenant, Visibility,
};
pub use connection::{Cold, Connect, Live};
pub use content::{ContentHash, ContentHasher, Freshness};
pub use cursor::Cursor;
pub use ecosystem::{Edition, Language, Toolchain};
pub use error::{BackendKind, ConnectError, ConnectFailure, Retryable, StoreError};
pub use identity::{
	EntryUri, Id, Package, PackageCoordinates, PackageId, PackageName, PackageVersion,
	RegistryOrigin, SymbolId,
};
pub use lifecycle::{Failure, FailureKind, Phase, ResolutionState};
pub use progress::{JobProgress, Percent, Progressive};
pub use score::{Score, Scored};
pub use search::Page;
pub use sink::{BatchSink, DerivedStore, Sink};
pub use symbol::{Name, Symbol, SymbolKind};
pub use version::Versioned;

/// A globally-unique identifier.
pub type Guid = uuid::Uuid;
