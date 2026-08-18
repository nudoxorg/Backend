//! A small content-addressed cache for registry archives.
//!
//! This intentionally stores the downloaded archive bytes, not an IPLD graph,
//! remote transport, or extracted package tree. The PURL resolver remains the
//! source of download URLs; this layer gives those bytes a stable local name
//! and lets a later process reopen them without contacting the registry.

use std::fs;
use std::fmt::Write;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::Digest as _;

use crate::packages::purl::Purl;
use nudox_languages::PackageSource;

use super::{Error, http, registry};

/// A lowercase SHA-256 content address.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ArchiveDigest(String);

impl ArchiveDigest {
    /// The hexadecimal digest.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl AsRef<str> for ArchiveDigest {
    fn as_ref(&self) -> &str {
        self.as_str()
    }
}

impl std::fmt::Display for ArchiveDigest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A successfully stored archive and the PURL that named it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ArchiveReceipt {
    purl: Purl,
    digest: ArchiveDigest,
}

#[derive(Serialize, Deserialize)]
struct StoredReceipt {
    purl: String,
    digest: String,
}

impl ArchiveReceipt {
    /// The PURL used to resolve this archive.
    pub fn purl(&self) -> &Purl {
        &self.purl
    }

    /// The content address of the archive bytes.
    pub fn digest(&self) -> &ArchiveDigest {
        &self.digest
    }
}

/// Archive bytes reopened from a content address.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CachedArchive {
    purl: Purl,
    bytes: Vec<u8>,
}

impl CachedArchive {
    /// The PURL recorded when the archive was fetched.
    pub fn purl(&self) -> &Purl {
        &self.purl
    }

    /// The original archive bytes.
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }
}

/// Errors from the archive cache boundary.
#[derive(Debug, thiserror::Error)]
pub enum ArchiveCacheError {
    /// Registry resolution or download failed.
    #[error("could not fetch archive: {0}")]
    Fetch(#[from] Error),
    /// The cache could not be read or written.
    #[error("{context}: {source}")]
    Io {
        context: String,
        #[source]
        source: std::io::Error,
    },
    /// The content address has no corresponding archive.
    #[error("archive {0} is not present in the cache")]
    Missing(ArchiveDigest),
    /// Cache metadata did not contain a valid PURL.
    #[error("archive {digest} has invalid cache metadata: {detail}")]
    Metadata {
        digest: ArchiveDigest,
        detail: String,
    },
    /// The cached archive could not be unpacked into a package root.
    #[error("archive {digest} could not be extracted: {detail}")]
    Extraction {
        digest: ArchiveDigest,
        detail: String,
    },
}

/// A local archive cache addressed by SHA-256.
#[derive(Clone, Debug)]
pub struct ArchiveCache {
    root: PathBuf,
    client: reqwest::Client,
    endpoints: registry::RegistryEndpoints,
}

impl ArchiveCache {
    /// Open (or create) a cache rooted at `root`.
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self {
            root: root.into(),
            client: http::client(),
            endpoints: registry::RegistryEndpoints { upstream: None },
        }
    }

    /// Open a cache whose registry requests are served by `upstream`.
    ///
    /// This is intended for durable mirrors and hermetic acquisition fixtures.
    /// The PURL still determines the package name, version, archive format and
    /// digest; the upstream only replaces the canonical registry host.
    pub fn with_upstream(root: impl Into<PathBuf>, upstream: &str) -> Self {
        Self {
            root: root.into(),
            client: http::client(),
            endpoints: registry::RegistryEndpoints {
                upstream: Some(upstream.trim_end_matches('/').to_owned()),
            },
        }
    }

    /// Fetch the source archive named by a versioned PURL and store it by hash.
    ///
    /// Existing bytes at the same address are reused. The registry's published
    /// digest is still checked by the existing acquisition resolver before the
    /// bytes are committed.
    pub async fn fetch(&self, purl: &Purl) -> Result<ArchiveReceipt, ArchiveCacheError> {
        let artifact =
            registry::artifact_with_endpoints(&self.client, purl, &self.endpoints).await?;
        let bytes = http::get_artifact(&self.client, &artifact.url, purl, &|_, _| {}).await?;
        if let Some(expected) = artifact.expected {
            let actual = expected.compute_over(&bytes);
            if !actual.eq_ignore_ascii_case(expected.value()) {
                return Err(Error::IntegrityMismatch {
                    purl: purl.render(),
                    registry: purl.ty().registry(),
                    url: artifact.url,
                    algorithm: expected.algorithm().to_owned(),
                    expected: expected.value().to_owned(),
                    actual,
                }
                .into());
            }
        }

        let digest = ArchiveDigest(hex(&sha2::Sha256::digest(&bytes)));
        let blob = self.blob_path(&digest);
        let metadata = self.metadata_path(&digest);
        create_parent(&blob)?;
        create_parent(&metadata)?;
        if !blob.is_file() {
            let partial = blob.with_extension("partial");
            fs::write(&partial, &bytes).map_err(|source| io("writing archive", source))?;
            fs::rename(&partial, &blob).map_err(|source| io("committing archive", source))?;
        }
        let receipt = ArchiveReceipt {
            purl: purl.clone(),
            digest,
        };
        let json = serde_json::to_vec(&StoredReceipt {
            purl: receipt.purl.render(),
            digest: receipt.digest.as_str().to_owned(),
        })
        .expect("archive receipt serialization is infallible");
        fs::write(&metadata, json).map_err(|source| io("writing archive metadata", source))?;
        Ok(receipt)
    }

    /// Reopen an archive using only its content address and local metadata.
    pub fn open(&self, digest: &ArchiveDigest) -> Result<CachedArchive, ArchiveCacheError> {
        let blob = self.blob_path(digest);
        let metadata = self.metadata_path(digest);
        if !blob.is_file() || !metadata.is_file() {
            return Err(ArchiveCacheError::Missing(digest.clone()));
        }
        let stored: StoredReceipt = serde_json::from_slice(
            &fs::read(&metadata).map_err(|source| io("reading archive metadata", source))?,
        )
        .map_err(|e| ArchiveCacheError::Metadata {
            digest: digest.clone(),
            detail: e.to_string(),
        })?;
        let purl = Purl::parse(&stored.purl).map_err(|e| ArchiveCacheError::Metadata {
            digest: digest.clone(),
            detail: e.to_string(),
        })?;
        if stored.digest != digest.0 {
            return Err(ArchiveCacheError::Metadata {
                digest: digest.clone(),
                detail: "metadata digest does not match requested address".to_owned(),
            });
        }
        let bytes = fs::read(&blob).map_err(|source| io("reading archive", source))?;
        let actual = ArchiveDigest(hex(&sha2::Sha256::digest(&bytes)));
        if actual != *digest {
            return Err(ArchiveCacheError::Metadata {
                digest: digest.clone(),
                detail: format!("blob bytes hash to {}", actual.as_str()),
            });
        }
        Ok(CachedArchive { purl, bytes })
    }

    /// Reopen and unpack an archive into its durable, content-addressed source
    /// root. The returned path is stable across processes and needs no
    /// original checkout.
    pub fn open_source(&self, digest: &ArchiveDigest) -> Result<PackageSource, ArchiveCacheError> {
        let archive = self.open(digest)?;
        let root = self.extracted_path(digest);
        if !root.is_dir() {
            let scratch = self
                .root
                .join("staging")
                .join("sha256")
                .join(digest.as_str());
            if scratch.exists() {
                fs::remove_dir_all(&scratch)
                    .map_err(|source| io("removing incomplete extraction", source))?;
            }
            fs::create_dir_all(&scratch)
                .map_err(|source| io("creating extraction staging", source))?;
            super::archive::unpack(archive.bytes(), archive.purl().ty(), &scratch, &root).map_err(
                |source| ArchiveCacheError::Extraction {
                    digest: digest.clone(),
                    detail: source.to_string(),
                },
            )?;
            let _ = fs::remove_dir_all(&scratch);
        }

        let version = archive
            .purl()
            .version()
            .ok_or_else(|| ArchiveCacheError::Metadata {
                digest: digest.clone(),
                detail: "cached PURL has no version".to_owned(),
            })?;
        Ok(PackageSource::new(
            root,
            archive.purl().lineage_name(),
            version,
        ))
    }

    /// Fetch a versioned PURL and return its durable source root.
    pub async fn fetch_source(&self, purl: &Purl) -> Result<PackageSource, ArchiveCacheError> {
        let receipt = self.fetch(purl).await?;
        self.open_source(receipt.digest())
    }

    fn blob_path(&self, digest: &ArchiveDigest) -> PathBuf {
        self.root.join("blobs").join("sha256").join(digest.as_str())
    }

    fn metadata_path(&self, digest: &ArchiveDigest) -> PathBuf {
        self.root
            .join("metadata")
            .join("sha256")
            .join(format!("{}.json", digest.as_str()))
    }

    fn extracted_path(&self, digest: &ArchiveDigest) -> PathBuf {
        self.root
            .join("extracted")
            .join("sha256")
            .join(digest.as_str())
    }
}

fn create_parent(path: &Path) -> Result<(), ArchiveCacheError> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|source| io("creating cache directory", source))?;
    }
    Ok(())
}

fn io(context: impl Into<String>, source: std::io::Error) -> ArchiveCacheError {
    ArchiveCacheError::Io {
        context: context.into(),
        source,
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().fold(String::new(), |mut acc, b| {
        let _ = write!(acc, "{b:02x}");
        acc
    })
}

#[cfg(test)]
mod tests {
    use super::ArchiveDigest;

    #[test]
    fn archive_digest_has_stable_display_and_str_forms() {
        let digest = ArchiveDigest("ab".repeat(32));
        let expected = "ab".repeat(32);
        assert_eq!(digest.to_string(), expected);
        assert_eq!(digest.as_ref(), expected.as_str());
    }
}
