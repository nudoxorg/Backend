//! Runtime — the data layer that backs all search functionality, either
//! directly or through a thin abstraction. The canonical, always-on serving
//! store; persistence and coordination live elsewhere.
//!
//! ## The read plane, in four stores
//! - [`text`] — tantivy, the **default** search surface. Replica-local: each
//!   server replica owns its own tantivy directory, built from a durable
//!   postgres watermark. Cheap, precise name/signature lookup.
//! - [`vector`] — qdrant, the **gated** semantic surface. Heavy, so it is only
//!   ever reached with an explicit [`vector::SemanticGate`] capability.
//! - [`graph`] — terminus, the **source of truth** for how a package's symbols
//!   are structured and related.
//! - [`session`] — per-session exploration state: a join-semilattice of the
//!   nodes/edges a user has accumulated, merged monotonically.
//!
//! ## Cross-cutting rules
//! - Connection typestates ([`heart::Cold`]/[`heart::Live`]) gate query methods;
//!   every store implements [`heart::Connect`] so the server brings them all up
//!   through one uniform path.
//! - Freshness is a content hash ([`heart::ContentHash`]) computed by an isolated
//!   subroutine when a package's git history changes — not a stamp carried on
//!   every derived record.
//! - Every query threads an [`heart::AccessContext`] for authorization.
//! - One `thiserror` enum per area lives in [`error`]; each is
//!   [`heart::Retryable`], and [`error::RuntimeError`] aggregates them.
#![feature(return_type_notation)]

pub mod error;
pub mod graph;
pub(crate) mod pagination;
pub mod session;
pub mod text;
pub mod vector;

pub use error::RuntimeError;
