//! Compiled-output lookup: JobKey → VCS change-set reference.
//!
//! # SV-6 read path
//!
//! `POST /v1/compiled/lookup` (SMOLVM-PLAN §5.1) answers "has the fleet already
//! compiled this exact sealed-input hash?" before the client boots a VM. A hit
//! returns the Pijul channel name and tip change-hash; the client then pulls
//! those changes over iroh and verification happens at import by Pijul content-
//! addressing — the lookup response is a claim, the changes are the proof.
//!
//! ## Write side (fleet-only, SV-6)
//!
//! [`ObjectCompiledStore::record`] is the sole write path. It is called by the
//! server-side VCS sync ingest (the iroh SyncService apply-hook) AFTER a merged
//! change-set is verified and applied. There is no HTTP write surface.
//!
//! Per SMOLVM-PLAN §5.3: "Local results are recorded into the **local**
//! channel/CAS only. The INDEX learns nothing from desktop compiles."
//!
//! ## Persistence
//!
//! Index entries live at `compiled/{job_key_hex}` in the same object-store
//! namespace as the rest of the registry. Each entry is a postcard-encoded
//! [`CompiledRecord`]. Writes are idempotent object-store puts.
//!
//! This is NOT artifact storage — artifacts are Pijul change files distributed
//! via iroh sync. The record is a small mapping: JobKey → (channel, tip).

use std::sync::Arc;

use heart::{JobKey, PackageId, content::ContentHash};
use object_store::{ObjectStore, PutPayload, path::Path};

pub mod client;

// ── Newtypes ──────────────────────────────────────────────────────────────────

/// Registry-local newtype for a Pijul channel name — must be non-empty.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ChannelName(String);

impl ChannelName {
    pub fn new(s: impl Into<String>) -> Result<Self, CompiledError> {
        let s = s.into();
        if s.is_empty() {
            return Err(CompiledError::InvalidChannelName);
        }
        Ok(Self(s))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for ChannelName {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// 64-character lowercase hex string identifying a Pijul change.
///
/// Mirrors nudox-ir-vcs `ChangeHashHex` and ir-sync `ChangeId` BY VALUE —
/// no cross-crate dependency; validated on construction.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ChangeId(String);

impl ChangeId {
    pub fn new(s: impl Into<String>) -> Result<Self, CompiledError> {
        let s = s.into();
        if s.len() != 64 || !s.chars().all(|c| matches!(c, '0'..='9' | 'a'..='f')) {
            return Err(CompiledError::InvalidChangeId);
        }
        Ok(Self(s))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for ChangeId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

// ── CompiledRecord ────────────────────────────────────────────────────────────

/// Index entry mapping a [`JobKey`] to a VCS change-set reference.
///
/// Stored at `compiled/{job_key_hex}` (64 lowercase hex chars = 32 bytes) in
/// the same object-store backend as the rest of the registry. Postcard-encoded
/// for byte-compactness.
///
/// This is NOT artifact storage — artifacts are Pijul change files distributed
/// via iroh sync. The `channel` + `tip` pair tells the client where to pull the
/// IR; `generation_stamp` authenticates which generation produced this tip.
///
/// Writes are fleet-only (SV-6). The client trusts the mapping because:
/// (a) only the fleet can write it, and
/// (b) the referenced change-set is verified AT IMPORT by Pijul content-
///     addressing when pulled over iroh — the lookup response is a claim,
///     the changes are the proof.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct CompiledRecord {
    /// The package this generation belongs to.
    pub package: PackageId,
    /// Pijul channel that holds the IR for this package.
    pub channel: ChannelName,
    /// Tip change-hash of that channel at record time.
    pub tip: ChangeId,
    /// Hash① of the generation that produced this tip; supplied by the writer.
    pub generation_stamp: ContentHash,
    /// Wall-clock milliseconds since the Unix epoch when this record was written.
    pub recorded_at: u64,
}

// ── Public hit type ───────────────────────────────────────────────────────────

/// The payload returned for a cache hit.
#[derive(Debug, Clone)]
pub struct CompiledHit {
    pub package: PackageId,
    pub channel: ChannelName,
    pub tip: ChangeId,
    pub generation_stamp: ContentHash,
}

/// The result of a single-key lookup.
#[derive(Debug, Clone)]
pub enum LookupResult {
    /// The fleet has a completed generation for this JobKey.
    Hit(Box<CompiledHit>),
    /// No matching compilation exists in the fleet index.
    Miss,
}

// ── Error ─────────────────────────────────────────────────────────────────────

/// Errors from the compiled-output store.
#[derive(Debug, thiserror::Error)]
pub enum CompiledError {
    /// A backing-store I/O failure.
    #[error("compiled store backend error")]
    Backend(#[source] Box<dyn std::error::Error + Send + Sync>),

    /// The stored record could not be decoded.
    #[error("compiled record codec error")]
    Codec(#[source] postcard::Error),

    /// A channel name was empty.
    #[error("channel name must be non-empty")]
    InvalidChannelName,

    /// A change ID was not 64 lowercase hex characters.
    #[error("change ID must be 64 lowercase hex characters")]
    InvalidChangeId,
}

// Keep the old alias so call sites in registry error.rs don't break.
pub type CompiledStoreError = CompiledError;

impl From<object_store::Error> for CompiledError {
    fn from(e: object_store::Error) -> Self {
        CompiledError::Backend(Box::new(e))
    }
}

impl heart::Retryable for CompiledError {
    fn is_retryable(&self) -> bool {
        matches!(self, CompiledError::Backend(_))
    }
}

// ── Object-store path helpers ─────────────────────────────────────────────────

/// The object-store path for a compiled record, keyed by job_key hex.
pub(crate) fn compiled_path(job_key: &[u8; 32]) -> Path {
    Path::from(format!(
        "compiled/{}",
        data_encoding::HEXLOWER.encode(job_key)
    ))
}

// ── ObjectCompiledStore ───────────────────────────────────────────────────────

/// The compiled-output lookup store, backed by the same `Arc<dyn ObjectStore>`
/// as [`crate::store::Store`].
///
/// Objects are stored at `compiled/{job_key_hex}` as postcard-encoded
/// [`CompiledRecord`]s. Writes are idempotent puts. The [`CompiledStore`] trait
/// is kept so the handler names the lookup contract once and tests can substitute
/// a lighter implementation without the full object-store machinery.
pub struct ObjectCompiledStore {
    backend: Arc<dyn ObjectStore>,
}

impl ObjectCompiledStore {
    /// Wrap an object-store backend (the same `Arc` that [`crate::store::Store`]
    /// holds — see [`crate::store::Store::backend`]).
    pub fn new(backend: Arc<dyn ObjectStore>) -> Self {
        Self { backend }
    }

    /// Record a VCS change-set reference for a job key.
    ///
    /// Called by the server-side VCS sync ingest (the iroh SyncService
    /// apply-hook) AFTER a merged change-set is verified and applied.
    /// Fleet-only — there is no HTTP write surface for this (SV-6).
    pub async fn record(
        &self,
        job_key: JobKey,
        record: CompiledRecord,
    ) -> Result<(), CompiledError> {
        let path = compiled_path(job_key.as_bytes());
        let bytes = postcard::to_allocvec(&record).map_err(CompiledError::Codec)?;
        self.backend
            .put(&path, PutPayload::from(bytes::Bytes::from(bytes)))
            .await?;
        Ok(())
    }
}

/// Wall-clock milliseconds since the Unix epoch (for `recorded_at`).
pub fn unix_millis() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

// ── Trait ────────────────────────────────────────────────────────────────────

/// The contract for compiled-output lookup.
///
/// Object-store-backed by [`ObjectCompiledStore`]. The trait boundary lets
/// tests substitute simple implementations without a full object-store stack.
pub trait CompiledStore: Send + Sync {
    /// Look up whether the fleet has a sealed generation for `job_key`.
    ///
    /// A miss (key not found) returns `Ok(LookupResult::Miss)`.
    /// Must be cancel-safe (the handler awaits under an axum timeout).
    fn lookup(
        &self,
        job_key: &[u8; 32],
    ) -> impl std::future::Future<Output = Result<LookupResult, CompiledError>> + Send;
}

impl CompiledStore for ObjectCompiledStore {
    async fn lookup(&self, job_key: &[u8; 32]) -> Result<LookupResult, CompiledError> {
        let path = compiled_path(job_key);
        let result = match self.backend.get(&path).await {
            Ok(r) => r,
            Err(object_store::Error::NotFound { .. }) => return Ok(LookupResult::Miss),
            Err(e) => return Err(e.into()),
        };

        let record_bytes = result.bytes().await.map_err(CompiledError::from)?;
        let record: CompiledRecord =
            postcard::from_bytes(&record_bytes).map_err(CompiledError::Codec)?;

        Ok(LookupResult::Hit(Box::new(CompiledHit {
            package: record.package,
            channel: record.channel,
            tip: record.tip,
            generation_stamp: record.generation_stamp,
        })))
    }
}
