//! Save / rebuild — the derived stores are exactly that: *derived*. The
//! content-addressed blobs (owned by `registry`) are the root of truth and
//! can be used to regenerate derived stores deterministically.
//!
//! This module is the rebuild/verify surface: given the blobs, re-emit each
//! derived store, and *verify determinism* by re-deriving the package's
//! [`heart::ContentHash`] and comparing it to the recorded snapshot rather than
//! trusting it blindly.

#[allow(unused_imports)]
use crate::server::registry;
pub mod blobs;

use crate::server::registry::RegistryError;
use crate::server::registry::blob::BlobManifest;
use heart::{ContentHash, PackageId, ResolutionState};

use crate::server::Server;
use crate::server::authz::AdminCap;
use crate::server::error::{BadRequestReason, ServerResult};
use registry::vector::EmbeddingModel;

/// Which derived store to rebuild — the shared [`heart::DerivedStore`], the same
/// type the registry outbox fans out to (no duplicate enum).
pub use heart::DerivedStore;

impl<M: EmbeddingModel> Server<M> {
    /// Read one source file from the current package manifest.
    ///
    /// The manifest is the allow-list: callers cannot turn this into an arbitrary
    /// CAS read by guessing a hash or a filesystem path. `Store::get_section`
    /// verifies the returned bytes against the content hash before they leave the
    /// server.
    pub async fn source_file(
        &self,
        package: PackageId,
        path: &str,
    ) -> ServerResult<(String, bytes::Bytes)> {
        let manifest = self.current_manifest(package).await?;
        let entry = manifest
            .files
            .iter()
            .find(|entry| entry.path.as_str() == path)
            .ok_or(crate::server::error::ServerError::NotFound)?;
        let bytes = self
            .base()
            .blobs
            .get_section(entry.hash)
            .await
            .map_err(RegistryError::from)?;
        Ok((entry.hash.to_string(), bytes))
    }

    /// Look up a source file's content hash and size in the current manifest,
    /// without reading any bytes. The ranged HTTP handler uses this to learn
    /// the file's size (needed to validate a `Range` header and build a
    /// correct `Content-Range`) before deciding whether — or how — to read
    /// from the CAS at all.
    pub async fn source_file_meta(&self, package: PackageId, path: &str) -> ServerResult<(String, u64)> {
        let (hash, size) = self.locate_file(package, path).await?;
        Ok((hash.to_string(), size))
    }

    /// Read a byte range of one source file from the current package manifest.
    ///
    /// Same manifest allow-list as [`Server::source_file`]. `range` is clamped
    /// to `[0, entry.size)` before the CAS read, so a caller-supplied range that
    /// overruns the file never turns into an out-of-bounds backend request —
    /// the clamped slice plus the returned file size is what a caller needs to
    /// build a correct `Content-Range`. A ranged read trades the whole-object
    /// integrity check `get_section` performs for bandwidth (see
    /// [`crate::cas::Store::get_section_range`]).
    pub async fn source_file_range(
        &self,
        package: PackageId,
        path: &str,
        range: std::ops::Range<u64>,
    ) -> ServerResult<(String, u64, bytes::Bytes)> {
        let (hash, size) = self.locate_file(package, path).await?;
        let clamped = range.start.min(size)..range.end.min(size);
        let bytes = self
            .base()
            .blobs
            .get_section_range(hash, clamped)
            .await
            .map_err(RegistryError::from)?;
        Ok((hash.to_string(), size, bytes))
    }

    /// Shared manifest lookup behind [`Server::source_file_meta`] and
    /// [`Server::source_file_range`] (`source_file` keeps its own copy rather
    /// than being rewired onto this, to leave its existing behavior untouched).
    async fn locate_file(&self, package: PackageId, path: &str) -> ServerResult<(ContentHash, u64)> {
        let manifest = self.current_manifest(package).await?;
        let entry = manifest
            .files
            .iter()
            .find(|entry| entry.path.as_str() == path)
            .ok_or(crate::server::error::ServerError::NotFound)?;
        Ok((entry.hash, entry.size))
    }

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
    pub async fn verify_reproducible(
        &self,
        _cap: &AdminCap,
        package: PackageId,
    ) -> ServerResult<bool> {
        let stores = self.base();

        let record = stores
            .global_store
            .get(package)
            .await
            .map_err(RegistryError::from)?;
        let ResolutionState::Stored { hash: recorded } = record.state else {
            return Err(BadRequestReason::NoRecordedSnapshot { package }.into());
        };

        // Re-read every section through the integrity-verifying store path (each
        // read re-hashes the bytes against its key), then re-derive the snapshot
        // fold. Together that is a full recomputation from the stored bytes.
        let manifest = self.current_manifest(package).await?;
        for entry in manifest.files.iter() {
            stores
                .blobs
                .get_section(entry.hash)
                .await
                .map_err(RegistryError::from)?;
        }
        stores
            .blobs
            .get_section(manifest.ir_ref)
            .await
            .map_err(RegistryError::from)?;
        stores
            .blobs
            .get_section(manifest.references_ref)
            .await
            .map_err(RegistryError::from)?;

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
        let record = stores
            .global_store
            .get(package)
            .await
            .map_err(RegistryError::from)?;
        Ok(stores
            .blobs
            .get_manifest(&record.package.coordinates)
            .await
            .map_err(RegistryError::from)?)
    }
}
