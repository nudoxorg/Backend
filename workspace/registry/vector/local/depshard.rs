//! Dep-shard install / upgrade / evict — the client side of the §20.3
//! shard bakery.
//!
//! Baked shards land under `dep_root/<package-uuid>/<version>/`, are opened
//! **read-only** ([`super::store::LocalShardStore::open_read_only`], no
//! advisory lock) and are **never written after install**; an upgrade is a
//! whole-directory swap, an evict is close-plus-delete.
//!
//! Frozen fallback ladder (§20.3 / §20.9 — the client **never** embeds a
//! dependency locally, R2):
//!
//! - artifact missing upstream → [`InstallOutcome::RemoteRoute`]
//!   ([`RemoteRouteReason::ArtifactMissing`]); the caller enqueues a bake,
//! - hash mismatch → **one** refetch, then `RemoteRoute` +
//!   telemetry alarm ([`RemoteRouteReason::HashMismatch`]),
//! - Edge load failure → delete the directory, **one** refetch, then
//!   `RemoteRoute` ([`RemoteRouteReason::LoadFailure`]).

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::vector::core::{ShardSchema, StoreError};
use async_trait::async_trait;
use heart::sync::{ContentIo, VerifyError};
use heart::{ContentHash, PackageId};
use smol_str::SmolStr;

use super::fanout::SharedWorkingSet;
use super::pack::{self, PackError};
use super::store::LocalShardStore;

/// Fetches shard artifacts from the trusted INDEX origin / CAS by content
/// hash. Implementations own transport (HTTP, iroh, test fixtures);
/// verification stays here.
#[async_trait]
pub trait ArtifactFetcher: Send + Sync {
    async fn fetch(&self, artifact: &ContentHash) -> Result<Vec<u8>, FetchError>;
}

/// Why a fetch failed.
#[derive(Debug, thiserror::Error)]
pub enum FetchError {
    /// The origin has no artifact under this hash (not baked yet, or baked
    /// for a different `edgepack_key`).
    #[error("shard artifact {0} not found upstream")]
    NotFound(ContentHash),

    /// Transient transport failure (network, auth, 5xx).
    #[error("shard artifact transport failure: {0}")]
    Transport(String),
}

/// One row of the signed package manifest the server publishes per baked
/// shard (§20.3 step 5).
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct DepManifestEntry {
    pub package: PackageId,
    /// Resolved package version (one directory level under the package).
    pub version: SmolStr,
    /// BLAKE3 of the `tar.zst` artifact bytes — verified before unpack.
    pub artifact_id: ContentHash,
    /// Bakery-computed resident-RAM estimate (§20.4 admission cost).
    pub ram_estimate: u64,
}

/// How an install ended (both are successes of the *flow* — `RemoteRoute`
/// means the package is served by INDEX until conditions change).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InstallOutcome {
    /// The shard is unpacked, loaded read-only, and registered in the
    /// working set.
    Installed,
    /// The package could not be installed; route its queries to the server
    /// parity collection and label the UI honestly (§20.4).
    RemoteRoute(RemoteRouteReason),
}

/// Why a package fell back to remote routing (§20.9 telemetry vocabulary).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RemoteRouteReason {
    /// No artifact upstream for the pinned edgepack key.
    ArtifactMissing,
    /// Two fetches, two hash mismatches — CAS or manifest is lying.
    HashMismatch,
    /// The verified artifact would not load as an Edge shard, twice.
    LoadFailure,
}

/// Hard failures of the install machinery itself (as opposed to the
/// expected fallbacks, which are [`InstallOutcome::RemoteRoute`]).
#[derive(Debug, thiserror::Error)]
pub enum InstallError {
    #[error("artifact fetch failed: {0}")]
    Fetch(#[from] FetchError),

    #[error(transparent)]
    Pack(#[from] PackError),

    #[error("dep shard store failure: {0}")]
    Store(#[from] StoreError),

    #[error("dep shard install io: {0}")]
    Io(#[from] std::io::Error),
}

/// A [`ContentIo`] implementation that only verifies artifact bytes via BLAKE3
/// and does not persist them anywhere. Used by [`install`] (which unpacks
/// artifact bytes to disk directly) as the seam adapter when no remote CAS
/// backing is available from within `registry`.
///
/// `read` and `write` both no-op (write succeeds silently; read returns
/// not-found): the install path never calls them — it uses `verify` then
/// unpacks the bytes it already holds.
struct LocalVerifyIo;

impl ContentIo for LocalVerifyIo {
    type Id = ContentHash;

    fn read(&self, _id: &ContentHash) -> io::Result<Vec<u8>> {
        Err(io::Error::new(
            io::ErrorKind::NotFound,
            "LocalVerifyIo: no backing store",
        ))
    }

    fn write(&self, _id: &ContentHash, _bytes: &[u8]) -> io::Result<()> {
        // No CAS backing in this context; the bytes are passed to unpack_shard
        // directly by the caller. A no-op write is correct: the install path
        // does not call write — it holds the bytes in memory through the unpack.
        Ok(())
    }

    fn has(&self, _id: &ContentHash) -> io::Result<bool> {
        Ok(false)
    }

    /// BLAKE3 of `bytes` must equal `id` — the content-address check for the
    /// shard-artifact plane. Mirrors `ShardContentIo::verify` in `index`.
    fn verify(&self, id: &ContentHash, bytes: &[u8]) -> Result<(), VerifyError> {
        let derived = ContentHash::of_bytes(bytes);
        if derived != *id {
            return Err(VerifyError::HashMismatch {
                expected: id.to_string(),
                got: derived.to_string(),
            });
        }
        Ok(())
    }
}

/// Fetch → verify → unpack → load read-only → register.
///
/// On success the shard is registered in the working set under
/// `entry.package`; a previously registered version is swapped out, closed,
/// and its directory deleted (upgrade = whole-directory swap). The baked
/// directory is never written again after this returns.
///
/// Verification is routed through [`LocalVerifyIo`] (a [`ContentIo`] that
/// does BLAKE3 verify without persisting to a remote CAS). Callers that also
/// want to persist the verified bytes to a CAS should use [`install_with_io`]
/// instead.
pub async fn install(
    fetcher: &dyn ArtifactFetcher,
    entry: &DepManifestEntry,
    dep_root: &Path,
    schema: &ShardSchema,
    working_set: &SharedWorkingSet,
) -> Result<InstallOutcome, InstallError> {
    install_with_io(
        fetcher,
        entry,
        dep_root,
        schema,
        working_set,
        &LocalVerifyIo,
    )
    .await
}

/// Fetch → verify-via-ContentIo → write-via-ContentIo → unpack → load → register.
///
/// Like [`install`] but routes both the verify and write steps through the
/// provided `content_io`, which is the [`heart::sync::ContentIo`] seam. The
/// write persists the verified packed bytes so they can be served to other
/// peers without re-fetching.
///
/// The shard PACK format and local vector search are separate concerns — the
/// unpack and read-only shard load are performed after the seam write.
pub async fn install_with_io(
    fetcher: &dyn ArtifactFetcher,
    entry: &DepManifestEntry,
    dep_root: &Path,
    schema: &ShardSchema,
    working_set: &SharedWorkingSet,
    content_io: &dyn ContentIo<Id = ContentHash>,
) -> Result<InstallOutcome, InstallError> {
    // Fetch + verify through the ContentIo seam, with exactly one refetch on
    // hash mismatch (§20.3). A passing verify is the sole write licence.
    let bytes = match fetch_and_verify_via_io(fetcher, entry, content_io).await? {
        FetchVerified::Ok(bytes) => bytes,
        FetchVerified::Missing => {
            return Ok(InstallOutcome::RemoteRoute(
                RemoteRouteReason::ArtifactMissing,
            ));
        }
        FetchVerified::Mismatch => {
            tracing::error!(
                package = %entry.package,
                artifact = %entry.artifact_id,
                "shard artifact hash mismatch after refetch; remote-routing package"
            );
            return Ok(InstallOutcome::RemoteRoute(RemoteRouteReason::HashMismatch));
        }
    };

    let shard_dir = shard_dir(dep_root, entry);
    match unpack_and_load(&bytes, entry, &shard_dir, schema).await {
        Ok(store) => {
            register(entry, dep_root, &shard_dir, store, working_set).await?;
            return Ok(InstallOutcome::Installed);
        }
        Err(err) => {
            tracing::warn!(
                package = %entry.package,
                error = %err,
                "dep shard failed to load; deleting directory and refetching once"
            );
            remove_dir_if_present(&shard_dir)?;
        }
    }

    // Load-failure ladder: one clean refetch, then remote-route.
    let bytes = match fetch_and_verify_via_io(fetcher, entry, content_io).await? {
        FetchVerified::Ok(bytes) => bytes,
        FetchVerified::Missing => {
            return Ok(InstallOutcome::RemoteRoute(
                RemoteRouteReason::ArtifactMissing,
            ));
        }
        FetchVerified::Mismatch => {
            return Ok(InstallOutcome::RemoteRoute(RemoteRouteReason::HashMismatch));
        }
    };
    match unpack_and_load(&bytes, entry, &shard_dir, schema).await {
        Ok(store) => {
            register(entry, dep_root, &shard_dir, store, working_set).await?;
            Ok(InstallOutcome::Installed)
        }
        Err(err) => {
            tracing::error!(
                package = %entry.package,
                error = %err,
                "dep shard failed to load twice; remote-routing package"
            );
            remove_dir_if_present(&shard_dir)?;
            Ok(InstallOutcome::RemoteRoute(RemoteRouteReason::LoadFailure))
        }
    }
}

/// Evict a dep shard: deregister it, close its actor, delete its directory.
///
/// **Call only at idle** (§20.4 cadence) — an in-flight search holds its
/// own `Arc` and finishes safely, but eviction mid-search would still make
/// the answered scope narrower than the one displayed.
pub async fn evict(
    package: PackageId,
    dep_root: &Path,
    working_set: &SharedWorkingSet,
) -> Result<(), InstallError> {
    let removed = working_set.write().await.remove_dep(&package);
    if let Some(store) = removed {
        // Sole holder (idle, as documented) → flush-and-close through the
        // actor before the directory goes away. Otherwise the actor closes
        // when the last in-flight search drops its clone.
        if let Ok(store) = Arc::try_unwrap(store) {
            let _ = store.close().await;
        }
    }
    remove_dir_if_present(&dep_root.join(package.to_string()))?;
    Ok(())
}

/// The versioned install directory: `dep_root/<package-uuid>/<version>/`.
fn shard_dir(dep_root: &Path, entry: &DepManifestEntry) -> PathBuf {
    dep_root
        .join(entry.package.to_string())
        .join(entry.version.as_str())
}

enum FetchVerified {
    Ok(Vec<u8>),
    Missing,
    /// Two attempts, both hashed wrong.
    Mismatch,
}

/// Fetch and verify the artifact through a [`ContentIo`] seam, refetching
/// exactly once on mismatch. When verify passes, write the bytes through the
/// seam (sole write licence). Transport failures propagate as hard errors
/// (retry policy belongs to the fetcher).
async fn fetch_and_verify_via_io(
    fetcher: &dyn ArtifactFetcher,
    entry: &DepManifestEntry,
    content_io: &dyn ContentIo<Id = ContentHash>,
) -> Result<FetchVerified, InstallError> {
    for attempt in 0..2 {
        match fetcher.fetch(&entry.artifact_id).await {
            Ok(bytes) => {
                // Route verify through the ContentIo seam — the trait's
                // verify is the BLAKE3 content-address check.
                match content_io.verify(&entry.artifact_id, &bytes) {
                    Ok(()) => {
                        // Verified: write through the seam (persists to
                        // CAS when the io has a backing store; no-ops for
                        // LocalVerifyIo). Ignore write errors: the bytes
                        // are still usable for the local unpack.
                        let _ = content_io.write(&entry.artifact_id, &bytes);
                        return Ok(FetchVerified::Ok(bytes));
                    }
                    Err(_) => {
                        tracing::warn!(
                            package = %entry.package,
                            artifact = %entry.artifact_id,
                            attempt,
                            "shard artifact hash mismatch"
                        );
                    }
                }
            }
            Err(FetchError::NotFound(_)) => return Ok(FetchVerified::Missing),
            Err(err) => return Err(err.into()),
        }
    }
    Ok(FetchVerified::Mismatch)
}

/// Unpack the verified bytes into `shard_dir` and open the shard read-only
/// against the expected schema. Any leftover directory is swept first so
/// the unpack's fresh-destination invariant holds.
async fn unpack_and_load(
    bytes: &[u8],
    entry: &DepManifestEntry,
    shard_dir: &Path,
    schema: &ShardSchema,
) -> Result<LocalShardStore, InstallError> {
    remove_dir_if_present(shard_dir)?;
    pack::unpack_shard(bytes, &entry.artifact_id, shard_dir)?;
    let store = LocalShardStore::open_read_only(shard_dir, schema.clone()).await?;
    Ok(store)
}

/// Swap the store into the working set and finish the whole-directory
/// upgrade: close any replaced store and delete every *other* version
/// directory of the package.
async fn register(
    entry: &DepManifestEntry,
    dep_root: &Path,
    shard_dir: &Path,
    store: LocalShardStore,
    working_set: &SharedWorkingSet,
) -> Result<(), InstallError> {
    let replaced = working_set
        .write()
        .await
        .insert_dep(entry.package, Arc::new(store));
    if let Some(old) = replaced
        && let Ok(old) = Arc::try_unwrap(old)
    {
        let _ = old.close().await;
    }

    let package_dir = dep_root.join(entry.package.to_string());
    for dir_entry in fs::read_dir(&package_dir)? {
        let path = dir_entry?.path();
        if path != *shard_dir {
            remove_dir_if_present(&path)?;
        }
    }
    Ok(())
}

fn remove_dir_if_present(dir: &Path) -> Result<(), std::io::Error> {
    match fs::remove_dir_all(dir) {
        Ok(()) => Ok(()),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(err) => Err(err),
    }
}
