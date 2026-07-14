//! Our shared vocabulary.
#![feature(adt_const_params)]
#![feature(return_type_notation)]

pub mod access;
pub mod cache;
pub mod connection;
pub mod content;
pub mod cursor;
pub mod ecosystem;
pub mod error;
pub mod health;
pub mod identity;
pub mod package;
pub mod progress;
pub mod score;
pub mod search;
pub mod sink;
pub mod symbol;
pub mod tenant;
pub mod version;

/// Unified observability (OTLP traces/logs/metrics + Pyroscope profiling).
///
/// Gated behind the optional `telemetry` feature so the heavy OpenTelemetry
/// stack is pulled in only by the crate that actually installs it (`server`);
/// every other `heart` consumer — and the Buck build — stays lean.
#[cfg(feature = "telemetry")]
pub mod telemetry;

pub use access::{Federation, Source, SourceId, SourceRole, Sourced};
pub use health::{assert_probe_future_send, timed as timed_probe, Probe, Probeable};
pub use connection::{Cold, Connect, Live};
pub use content::{ContentHash, ContentHasher, Freshness, JobKey};
pub use cursor::{Advisory, Cursor, CursorError, Enforced, PolicyTag, SnapshotPolicy};
pub use ecosystem::{Edition, Language, Toolchain};
pub use error::{
    BackendKind, ConnectError, ConnectFailure, ErrorDetails, Failure, FailureKind, Phase, ResolutionState,
    Retryable, StoreError,
};
pub use identity::{
    CargoVersionError, EntryUri, Id, NameError, NpmVersionError, Package, PackageId,
    PackageCoordinates, PackageVersion, PythonVersionError, RegistryOrigin, SymbolId, VersionError,
};
pub use progress::{JobProgress, Percent, Progressive};
pub use score::{Score, Scored};
pub use search::Page;
pub use sink::DerivedStore;
pub use symbol::{Name, Symbol, SymbolKind};
pub use tenant::{OwnerKind, Visibility};
pub use version::Versioned;

/// A globally-unique identifier.
pub type Guid = uuid::Uuid;
