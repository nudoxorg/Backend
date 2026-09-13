//! Registration-level exact and interval reverse arrangements.
//!
//! The reverse index is divided by invariant: key-role/API types live in
//! `api`, interval overlap traversal lives in `interval`, and registration
//! ownership plus manifest admission lives in `registry`.  Keeping those
//! layers private makes it harder to update one arrangement without updating
//! the corresponding reader membership and dependency graph.

mod api;
mod interval;
mod registry;
mod relation;

pub(in crate::reuse) use api::{ReaderBucket, RegistrationId};
pub use api::{ReaderRegistration, SemanticReaderKey, SemanticReaderKeyAdapter, SemanticWorkKey};
pub(in crate::reuse) use interval::selector_interval;
pub use registry::RetainedReaders;
pub use relation::SemanticRelationRoot;
pub(in crate::reuse::index) use relation::{RelationIndexes, VersionedDependencyRelation};
