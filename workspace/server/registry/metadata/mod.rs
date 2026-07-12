//! Package metadata — the postgres-backed record tying a package's canonical
//! identity to its content hash and cross-store links.
//!
//! [`hash`] re-exports heart's content-addressing vocabulary (which moved to
//! heart). [`heuristics`] provides keyword-normalization and synonym/specifics
//! tables ported from the lib.rs upstream.

use heart::identity::PackageId;

pub mod hash;
pub mod heuristics;
pub mod rich;

pub use rich::{RichMetadata, SearchFacets};

pub use heuristics::{normalize_keyword, Specifics, Synonyms};

/// The metadata row for one package: its identity and the per-store
/// materialization links the read plane joins on.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct PackageMetadata {
	/// The deterministic global identity.
	pub id: PackageId,

	/// Whether each derived store has materialized this generation.
	pub links: StoreLinks,
}

/// Which derived stores currently hold this package's latest generation — the
/// cross-store link bitmap the outbox pollers advance.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub struct StoreLinks {
	/// Materialized in the vector store (qdrant).
	pub vector: bool,
	/// Materialized in the graph store (terminus).
	pub graph: bool,
	/// Materialized in the text index (tantivy).
	pub text: bool,
}

impl StoreLinks {
	/// Whether every derived store is caught up to this generation.
	pub const fn fully_linked(self) -> bool { self.vector && self.graph && self.text }
}
