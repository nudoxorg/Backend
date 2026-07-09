//! Our shared vocabulary.
#![feature(adt_const_params)]
#![feature(return_type_notation)]

pub mod access;
pub mod connection;
pub mod content;
pub mod cursor;
pub mod ecosystem;
pub mod error;
pub mod health;
pub mod identity;
pub mod progress;
pub mod score;
pub mod search;
pub mod sink;
pub mod symbol;
pub mod version;

pub use access::{Federation, Source, SourceId, SourceRole, Sourced};
pub use health::{assert_probe_future_send, timed as timed_probe, Probe, Probeable};
pub use connection::{Cold, Connect, Live};
pub use content::{ContentHash, ContentHasher, Freshness};
pub use cursor::Cursor;
pub use ecosystem::{Edition, Language, Toolchain};
pub use error::{
    BackendKind, ConnectError, ConnectFailure, ErrorDetails, Failure, FailureKind, Phase, ResolutionState,
    Retryable, StoreError,
};
pub use identity::{
    CargoVersionError, EntryUri, Id, NameError, NpmVersionError, Package, PackageId, PackageVersion,
    PythonVersionError, RegistryOrigin, SymbolId, VersionError,
};
pub use progress::{JobProgress, Percent, Progressive};
pub use score::{Score, Scored};
pub use search::Page;
pub use sink::DerivedStore;
pub use symbol::{Name, Symbol, SymbolKind};
pub use version::Versioned;

/// A globally-unique identifier.
pub type Guid = uuid::Uuid;
