//! Heart — the vocabulary shared across every subsystem.
//!
//! Anything specific to a single subsystem (the registry stores, the runtime
//! serving layer, the compiler, the server mesh) lives in that subsystem's
//! crate. What remains here is deliberately foundational and generic: the
//! type-tagged identifier, package identity, the connection/capability
//! typestates, the cross-cutting behavioural traits, and the error scaffolding
//! every store shares.
//!
//! ## Design rules this crate enforces at the type level
//! - **Identity is minted once, deterministically.** [`package`] normalizes
//!   names per ecosystem and derives stable UUIDv5 ids, so "the same package"
//!   is a decidable, offline-recomputable fact — not a string comparison.
//! - **Illegal states are unrepresentable** where cheap to arrange: finite
//!   [`Score`], non-empty [`ModelId`], `Cold`/`Live` connection typestates,
//!   capability planes.
//! - **Failure carries context.** Every store error is a real
//!   `std::error::Error` and classifies itself as [`error::Retryable`] so the
//!   retry/queue machinery is written once, not per backend.
#![feature(adt_const_params)]
#![feature(return_type_notation)]

pub mod access;
pub mod connection;
pub mod content;
pub mod cursor;
pub mod ecosystem;
pub mod error;
pub mod identifier;
pub mod lifecycle;
pub mod model;
pub mod package;
pub mod progress;
pub mod score;
pub mod scored;
pub mod sink;
pub mod symbol;
pub mod version;

pub use access::{
	AccessContext, AccessDecision, Federation, Principal, Source, SourceId, SourceRole, Sourced,
	Tenant, Visibility,
};
pub use connection::{Cold, Connect, Live};
pub use content::{ContentHash, Freshness, Generation};
pub use cursor::Cursor;
pub use ecosystem::{Ecosystem, Edition, Toolchain};
pub use error::{BackendKind, ConnectError, ConnectFailure, Retryable, StoreError};
pub use identifier::Id;
pub use lifecycle::{Failure, FailureKind, Phase, ResolutionState};
pub use model::ModelId;
pub use package::{
	EntryUri, GlobalSymbolId, PackageCoordinates, PackageId, PackageName, PackageVersion,
	RegistryOrigin,
};
pub use progress::{JobProgress, Percent, Progressive};
pub use score::Score;
pub use scored::Scored;
pub use sink::{BatchSink, DerivedStore, Sink};
pub use symbol::{Symbol, SymbolKind};
pub use version::Versioned;

/// A globally-unique identifier — the raw UUID that backs an [`Id`].
pub type Guid = uuid::Uuid;
