//! Content-addressed forge blobs and source-file frontiers.
//!
//! Association identity, candidate admission, and provenance stay with the
//! parent module. This module checks page roots and blob shape.

use super::{
    MAX_REGISTRY_FORGE_BLOBS, MAX_REGISTRY_FORGE_PAGES, REGISTRY_FORGE_BLOB_FRONTIER_VERSION,
    RegistryForgeAssociationError,
};
use crate::surface::ProductText;
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

/// A complete, content-addressed source-file membership frontier.
///
/// Each page points at an immutable CAS page manifest. Ordered page indexes
/// and aggregate counts make the frontier deterministic while keeping
/// admission and GUI projection bounded. The root commits the complete
/// membership set; callers can fetch pages lazily when a file-level view is
/// requested.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RegistryForgeBlobFrontier {
    /// Frontier schema version.
    pub version: u16,
    /// Merkle/content root for the complete source-file membership set.
    pub root: [u8; 32],
    /// Ordered immutable CAS page manifests.
    pub pages: Box<[RegistryForgeBlobPage]>,
    /// Number of source files represented by all pages.
    pub files: u64,
    /// Total source-file bytes represented by all pages.
    pub bytes: u64,
}

impl RegistryForgeBlobFrontier {
    /// Builds and validates a source-file frontier, deriving its root from
    /// the ordered page descriptors and aggregate totals.
    pub fn new(
        pages: Box<[RegistryForgeBlobPage]>,
        files: u64,
        bytes: u64,
    ) -> Result<Self, RegistryForgeAssociationError> {
        if pages.len() > MAX_REGISTRY_FORGE_PAGES {
            return Err(RegistryForgeAssociationError::FrontierShape);
        }
        let frontier = Self {
            version: REGISTRY_FORGE_BLOB_FRONTIER_VERSION,
            root: frontier_root(REGISTRY_FORGE_BLOB_FRONTIER_VERSION, &pages, files, bytes),
            pages,
            files,
            bytes,
        };
        frontier.admit().map(|()| frontier)
    }

    /// Revalidates the page ordering, aggregate totals, duplicate IDs, and
    /// authenticated root after deserialization or journal recovery.
    pub fn admit(&self) -> Result<(), RegistryForgeAssociationError> {
        if self.version != REGISTRY_FORGE_BLOB_FRONTIER_VERSION
            || self.root == [0; 32]
            || self.pages.len() > MAX_REGISTRY_FORGE_PAGES
        {
            return Err(RegistryForgeAssociationError::FrontierShape);
        }
        let mut page_files = 0_u64;
        let mut page_bytes = 0_u64;
        let mut page_ids = BTreeSet::new();
        for (expected, page) in self.pages.iter().enumerate() {
            if page.index != u32::try_from(expected).unwrap_or(u32::MAX)
                || page.content_id == [0; 32]
                || page.files == 0
                || page.bytes == 0
            {
                return Err(RegistryForgeAssociationError::FrontierShape);
            }
            page_files = page_files
                .checked_add(page.files)
                .ok_or(RegistryForgeAssociationError::FrontierShape)?;
            page_bytes = page_bytes
                .checked_add(page.bytes)
                .ok_or(RegistryForgeAssociationError::FrontierShape)?;
            if !page_ids.insert(page.content_id) {
                return Err(RegistryForgeAssociationError::FrontierShape);
            }
        }
        if page_files != self.files || page_bytes != self.bytes {
            return Err(RegistryForgeAssociationError::FrontierShape);
        }
        if self.root != frontier_root(self.version, &self.pages, self.files, self.bytes) {
            return Err(RegistryForgeAssociationError::FrontierShape);
        }
        Ok(())
    }
}

/// One immutable CAS page manifest in a source-file frontier.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RegistryForgeBlobPage {
    /// Zero-based page position in the frontier.
    pub index: u32,
    /// Content identity of the page manifest.
    pub content_id: [u8; 32],
    /// Number of files in this page.
    pub files: u64,
    /// Total file bytes in this page.
    pub bytes: u64,
}

/// Content-addressed object retained or shared by a forge acquisition.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RegistryForgeBlobRef {
    /// Kind of immutable content.
    pub kind: RegistryForgeBlobKind,
    /// Content identity shared by registry and forge projections.
    pub content_id: [u8; 32],
    /// Encoded byte size used for bounded admission and telemetry.
    pub bytes: u64,
    /// Relative source path when this is a source-file blob.
    pub path: Option<ProductText>,
}

impl RegistryForgeBlobRef {
    /// Creates a content reference with the kind/path invariant checked.
    pub fn new(
        kind: RegistryForgeBlobKind,
        content_id: [u8; 32],
        bytes: u64,
        path: Option<ProductText>,
    ) -> Result<Self, RegistryForgeAssociationError> {
        let blob = Self {
            kind,
            content_id,
            bytes,
            path,
        };
        blob.admit()?;
        Ok(blob)
    }

    fn admit(&self) -> Result<(), RegistryForgeAssociationError> {
        if self.content_id == [0; 32] {
            return Err(RegistryForgeAssociationError::BlobShape);
        }
        match (self.kind, self.path.as_ref()) {
            (RegistryForgeBlobKind::SourceFile, Some(_))
            | (
                RegistryForgeBlobKind::Archive
                | RegistryForgeBlobKind::TreeManifest
                | RegistryForgeBlobKind::SourceSnapshot
                | RegistryForgeBlobKind::SourceDelta,
                None,
            ) => Ok(()),
            _ => Err(RegistryForgeAssociationError::BlobShape),
        }
    }
}

/// Kind of immutable content referenced by a forge relationship.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum RegistryForgeBlobKind {
    /// Downloaded source archive.
    Archive,
    /// Canonical tree manifest.
    TreeManifest,
    /// Filtered source snapshot.
    SourceSnapshot,
    /// One source-file content blob.
    SourceFile,
    /// Canonical source snapshot delta.
    SourceDelta,
}

/// Computes the domain-separated root for an ordered source-file frontier.
///
/// The page count, every ordered descriptor, and the aggregate totals are
/// included so a root cannot be reused for a different page layout or a
/// truncated membership set. The helper remains private because callers
/// should construct frontiers through [`RegistryForgeBlobFrontier::new`].
fn frontier_root(
    version: u16,
    pages: &[RegistryForgeBlobPage],
    files: u64,
    bytes: u64,
) -> [u8; 32] {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"nudox.registry-forge-blob-frontier.v1\0");
    hasher.update(&version.to_be_bytes());
    hasher.update(&u64::try_from(pages.len()).unwrap_or(u64::MAX).to_be_bytes());
    for page in pages {
        hasher.update(&page.index.to_be_bytes());
        hasher.update(&page.content_id);
        hasher.update(&page.files.to_be_bytes());
        hasher.update(&page.bytes.to_be_bytes());
    }
    hasher.update(&files.to_be_bytes());
    hasher.update(&bytes.to_be_bytes());
    *hasher.finalize().as_bytes()
}

/// Rejects an over-bound, malformed, or duplicated blob list.
pub(super) fn admit_blobs(
    blobs: &[RegistryForgeBlobRef],
) -> Result<(), RegistryForgeAssociationError> {
    if blobs.len() > MAX_REGISTRY_FORGE_BLOBS {
        return Err(RegistryForgeAssociationError::BlobBound);
    }
    for (index, blob) in blobs.iter().enumerate() {
        blob.admit()?;
        if blobs[index + 1..].iter().any(|other| {
            other.content_id == blob.content_id
                || (other.kind == blob.kind && blob.kind.is_singleton())
                || (other.kind == RegistryForgeBlobKind::SourceFile
                    && blob.kind == RegistryForgeBlobKind::SourceFile
                    && other.path == blob.path)
        }) {
            return Err(RegistryForgeAssociationError::BlobShape);
        }
    }
    Ok(())
}

impl RegistryForgeBlobKind {
    const fn is_singleton(self) -> bool {
        matches!(
            self,
            Self::Archive | Self::TreeManifest | Self::SourceSnapshot | Self::SourceDelta
        )
    }
}
