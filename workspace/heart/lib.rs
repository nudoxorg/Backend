//! Our shared vocabulary.
#![feature(return_type_notation)]

pub mod access;
pub mod availability;
pub mod cache;
/// Client glue (§8): the typed wire DTOs and capability vocabulary a client
/// uses to compose the `index` and `registry` layers. Transport-free.
pub mod client;
pub mod connection;
pub mod content;
pub mod cursor;
pub mod deployment;
pub mod ecosystem;
pub mod egress;
pub mod error;
pub mod health;
pub mod identity;
pub mod object_pack;
pub mod package;
pub mod page;
pub mod progress;
pub mod query;
pub mod score;
pub mod search;
pub mod sink;
/// The typed NDJSON streaming envelope ([`stream::StreamFrame`]) shared by the
/// `index` server's search writer and the `heart::client::http` reader.
pub mod stream;
pub mod symbol;
/// The generic content-addressed sync seam (`ContentIo`/`ApplyHook`), shared by
/// the IR VCS change-sync and the object-pack member-sync planes.
pub mod sync;
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
pub use availability::{AvailabilityStoreId, IrAvailability, ObjectAvailability};
pub use connection::{Cold, Connect, Live};
pub use content::{ContentHash, ContentHasher, Freshness, JobKey};
pub use cursor::{Advisory, Cursor, CursorError, Enforced, PolicyTag, SnapshotPolicy};
pub use deployment::{DeploymentKind, DeploymentProfile, TrustedRemote};
pub use ecosystem::{Edition, Language, Toolchain};
pub use egress::{EgressDenied, EgressPolicy, EgressRequest, HostGlob};
pub use error::{
    BackendKind, ConnectError, ConnectFailure, ErrorDetails, Failure, FailureKind, Phase,
    ResolutionState, Retryable, StoreError,
};
pub use health::{Probe, Probeable, assert_probe_future_send, timed as timed_probe};
pub use identity::{
    CargoVersionError, EntryUri, Id, NameError, NpmVersionError, Package, PackageCoordinates,
    PackageId, PackageVersion, PythonVersionError, RegistryOrigin, SymbolId, VersionError,
};
pub use object_pack::{MemberKey, MemberRecord, ObjectPackId};
pub use progress::{JobProgress, Percent, Progressive};
pub use query::{
    AsOf, CatalogCommitHash, PageSpecification, QualityMode, Query, QueryEngine, QueryMode,
    QueryReach, RankSpecification, Routing, Scope, StableReference, StableReferenceError, Target,
    UnixMilliseconds,
};
pub use package::{Coordinates, PackageHit, PackageName};
pub use score::{RankKey, Score, Scored};
pub use search::Page;
pub use sink::DerivedStore;
pub use stream::{StreamFrame, StreamSummary, SymbolFrame, WireError};
pub use symbol::{Name, Symbol, SymbolKind};
pub use tenant::{OwnerKind, Visibility};
pub use version::Versioned;

/// A globally-unique identifier.
pub type Guid = uuid::Uuid;
