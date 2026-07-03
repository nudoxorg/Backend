//! Package metadata — the postgres-backed record tying a package's canonical
//! identity to its content hash and cross-store links.
//!
//! [`guid`] wraps heart's deterministic id minting with this instance's token;
//! [`hash`] re-exports heart's content-addressing vocabulary (which moved to
//! heart) and adds the *package-level* canonical hashing this crate owns.

use heart::identity::PackageId;

pub mod guid;
pub mod hash;

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
