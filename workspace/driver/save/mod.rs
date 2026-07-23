//! Save / rebuild — the derived stores are exactly that: *derived*. The
//! content-addressed blobs (owned by `registry`) are the root of truth and
//! can be used to regenerate derived stores deterministically.
//!
//! This module is the rebuild/verify surface: given the blobs, re-emit each
//! derived store, and *verify determinism* by re-deriving the package's
//! [`heart::ContentHash`] and comparing it to the recorded snapshot rather than
//! trusting it blindly.

#[allow(unused_imports)]
use crate::{registry};
pub mod blobs;

use heart::{ContentHash, PackageId, ResolutionState};
use crate::registry::RegistryError;
use crate::registry::blob::BlobManifest;

use registry::vector::EmbeddingModel;
use crate::Server;
use crate::authz::AdminCap;
use crate::error::{BadRequestReason, ServerResult};

/// Which derived store to rebuild — the shared [`heart::DerivedStore`], the same
/// type the registry outbox fans out to (no duplicate enum).
pub use heart::DerivedStore;

impl<M: EmbeddingModel> Server<M> {
	/// Rebuild a derived store for a package from its blob, at a known snapshot
	/// (the recorded [`ContentHash`] from `ResolutionState::Stored`).
	///
	/// The rebuild *is* a fan-out re-enqueue: the blob is the root of truth and
	/// every derived store is a poller, so re-emitting means appending the
	/// sink's outbox intent for the current generation and letting the poller
	/// re-materialize. The append dedupes on `(package, generation, sink)`, so
	/// the whole operation is idempotent by construction.
	///
	/// The caller must hold an [`AdminCap`] proving authorization has occurred.
	#[tracing::instrument(skip(self, _cap), fields(%package, %store))]
	pub async fn rebuild(
		&self,
		_cap: &AdminCap,
		package: PackageId,
		store: DerivedStore,
		snapshot: ContentHash,
	) -> ServerResult<()> {
		let stores = self.base();

		// The manifest the blobs currently point at must *be* the requested
		// snapshot — rebuilding a derived store from a different generation than
		// the caller named would silently mix generations.
		let manifest = self.current_manifest(package).await?;
		let current = ContentHash::of_bytes(&manifest.identity_bytes());
		if current != snapshot {
			return Err(BadRequestReason::SnapshotMismatch { package }.into());
		}

		stores
			.outbox
			.append(package, snapshot, &[store])
			.await
			.map_err(RegistryError::from)?;
		tracing::info!("rebuild intent re-emitted");
		Ok(())
	}

	/// Verify determinism: re-derive a package's content hash from its blob and
	/// confirm it matches the recorded snapshot. A mismatch means non-reproducible
	/// output — an alert-worthy invariant break.
	///
	/// The caller must hold an [`AdminCap`] proving authorization has occurred.
	#[tracing::instrument(skip(self, _cap), fields(%package))]
	pub async fn verify_reproducible(&self, _cap: &AdminCap, package: PackageId) -> ServerResult<bool> {
		let stores = self.base();

		let record = stores.global_store.get(package).await.map_err(RegistryError::from)?;
		let ResolutionState::Stored { hash: recorded } = record.state else {
			return Err(BadRequestReason::NoRecordedSnapshot { package }.into());
		};

		// Re-read every section through the integrity-verifying store path (each
		// read re-hashes the bytes against its key), then re-derive the snapshot
		// fold. Together that is a full recomputation from the stored bytes.
		let manifest = self.current_manifest(package).await?;
		for entry in manifest.files.iter() {
			stores.blobs.get_section(entry.hash).await.map_err(RegistryError::from)?;
		}
		stores.blobs.get_section(manifest.ir_ref).await.map_err(RegistryError::from)?;
		stores.blobs.get_section(manifest.references_ref).await.map_err(RegistryError::from)?;

		let recomputed = ContentHash::of_bytes(&manifest.identity_bytes());
		let reproducible = recomputed == recorded;
		if !reproducible {
			tracing::error!(
				?recorded,
				?recomputed,
				"snapshot hash mismatch: non-reproducible derivation"
			);
		}
		Ok(reproducible)
	}

	/// The manifest the blob store currently points at for a package.
	async fn current_manifest(&self, package: PackageId) -> ServerResult<BlobManifest> {
		let stores = self.base();
		let record = stores.global_store.get(package).await.map_err(RegistryError::from)?;
		Ok(stores
			.blobs
			.get_manifest(&record.package.coordinates)
			.await
			.map_err(RegistryError::from)?)
	}
}
