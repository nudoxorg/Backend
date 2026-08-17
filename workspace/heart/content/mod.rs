//! Content addressing, job keys, and freshness.

pub mod availability;
pub mod object_pack;

use serde::{Deserialize, Serialize};

/// The content hash which serves three roles:
/// 1. Ensuring that package freshness hasn't changed.
/// 2. Dedupe on the content
/// 3. type marking anything that depends on it
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct ContentHash([u8; 32]);

impl ContentHash {
    /// Wrap a raw 32-byte digest (e.g. read back from postgres).
    pub const fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    /// The raw digest bytes.
    pub const fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }

    /// Hash a contiguous byte buffer.
    pub fn of_bytes(bytes: &[u8]) -> Self {
        Self(*blake3::hash(bytes).as_bytes())
    }

    /// Lower-hex encoding for filesystem names and log lines.
    pub fn hex(&self) -> String {
        data_encoding::HEXLOWER.encode(&self.0)
    }

    pub fn builder() -> ContentHasher {
        ContentHasher(blake3::Hasher::new())
    }
}

impl std::fmt::Display for ContentHash {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.hex())
    }
}

/// An incremental hasher for building a [`ContentHash`] from a stream of parts
/// without holding the whole package in memory.
pub struct ContentHasher(blake3::Hasher);

impl ContentHasher {
    /// Fold another chunk into the digest.
    pub fn update(&mut self, bytes: &[u8]) -> &mut Self {
        self.0.update(bytes);
        self
    }

    /// Finalize into a [`ContentHash`].
    pub fn finalize(&self) -> ContentHash {
        ContentHash(*self.0.finalize().as_bytes())
    }
}

/// Domain-separated cache / producer job identity.
///
/// `JobKey = H(producer_version ‖ toolchain ‖ source ‖ dep_lock)` with each
/// component length-prefixed (little-endian `u64`), matching the historical
/// `CacheKey::derive` layout so keys stay stable across the CAS migration.
///
/// Resource limits that do not change output bytes are **not** part of the key.
/// Capability-relevant policy (producer version, hermeticity, net mode) is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct JobKey(ContentHash);

impl JobKey {
    /// Build from the four design components (length-prefixed streaming).
    pub fn derive(
        producer_version: &[u8],
        toolchain: &[u8],
        source: &[u8],
        dep_lock: &[u8],
    ) -> Self {
        let mut h = blake3::Hasher::new();
        for part in [producer_version, toolchain, source, dep_lock] {
            h.update(&(part.len() as u64).to_le_bytes());
            h.update(part);
        }
        Self(ContentHash(*h.finalize().as_bytes()))
    }

    /// The underlying digest (CAS key for this job).
    pub const fn as_hash(self) -> ContentHash {
        self.0
    }

    /// Raw digest bytes.
    pub const fn as_bytes(&self) -> &[u8; 32] {
        self.0.as_bytes()
    }

    /// Lower-hex encoding.
    pub fn hex(&self) -> String {
        self.0.hex()
    }

    /// Domain-separated child key: `H(jobkey ‖ tag)` (e.g. `"cst"`, `"archive"`).
    pub fn with_tag(self, tag: &[u8]) -> ContentHash {
        let mut h = blake3::Hasher::new();
        h.update(self.as_bytes());
        h.update(&(tag.len() as u64).to_le_bytes());
        h.update(tag);
        ContentHash(*h.finalize().as_bytes())
    }
}

impl From<JobKey> for ContentHash {
    fn from(key: JobKey) -> Self {
        key.0
    }
}

impl std::fmt::Display for JobKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.hex())
    }
}

/// Whether a package's stored representation still matches its source.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Freshness {
    /// The recorded hash matches a freshly-computed one; no work needed.
    Fresh,

    /// The representation changed; the package must be re-acquired and
    /// re-indexed.
    Stale,
}

impl Freshness {
    /// Compare a recorded hash against a freshly-computed one.
    pub fn compare(recorded: ContentHash, recomputed: ContentHash) -> Self {
        if recorded == recomputed {
            Freshness::Fresh
        } else {
            Freshness::Stale
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn job_key_derive_is_stable() {
        let a = JobKey::derive(b"1.0.0", b"tc", b"src", b"lock");
        let b = JobKey::derive(b"1.0.0", b"tc", b"src", b"lock");
        assert_eq!(a, b);
        let c = JobKey::derive(b"1.0.1", b"tc", b"src", b"lock");
        assert_ne!(a, c);
    }

    #[test]
    fn job_key_tag_differs() {
        let k = JobKey::derive(b"p", b"t", b"s", b"d");
        assert_ne!(k.with_tag(b"cst"), k.with_tag(b"archive"));
        assert_ne!(k.with_tag(b"cst"), k.as_hash());
    }

    /// Golden digest for the historical `CacheKey::derive` length-prefix layout.
    /// Changing the encoding is a cache-invalidation event — fail loudly.
    #[test]
    fn job_key_derive_matches_historical_layout() {
        let k = JobKey::derive(b"1.0.0", b"tc", b"src", b"lock");
        assert_eq!(
            k.hex(),
            "3ae60363db0e590a112c9a1bf085bc8ad744a168a7263fea78b1390b2bb26e3e"
        );
    }
}
