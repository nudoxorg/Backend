//! Reproducibility — the derived stores are exactly that: *derived*. The
//! content-addressed blobs (owned by `registry`) are the root of truth; qdrant,
//! terminus, and tantivy can all be regenerated from them deterministically.
//!
//! This module is the rebuild/verify surface: given the blobs, re-emit each
//! derived store, and *verify determinism* by re-deriving and comparing the
//! [`heart::Generation`] rather than trusting it blindly.

pub mod blobs;
pub mod qdrant;
pub mod tantivy;
pub mod terminus;

use heart::{Generation, package::PackageId};

use runtime::vector::EmbeddingModel;
use crate::Server;
use crate::error::ServerResult;

/// Which derived store to rebuild — the shared [`heart::DerivedStore`], the same
/// type the registry outbox fans out to (no duplicate enum).
pub use heart::DerivedStore;

impl<M: EmbeddingModel> Server<M> {
	/// Rebuild a derived store for a package from its blob, at a known generation.
	pub async fn rebuild(
		&self,
		package: PackageId,
		store: DerivedStore,
		generation: Generation,
	) -> ServerResult<()> {
		let _ = (package, store, generation);
		todo!("read blob manifest, re-emit into the chosen store idempotently")
	}

	/// Verify determinism: re-derive a package's generation from its blob and
	/// confirm it matches the recorded one. A mismatch means non-reproducible
	/// output — an alert-worthy invariant break.
	pub async fn verify_reproducible(&self, package: PackageId) -> ServerResult<bool> {
		let _ = package;
		todo!("recompute generation from blob, compare to recorded")
	}
}
