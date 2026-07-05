//! The condensed source resolution: a content-addressed set of the package's
//! source files.
//!
//! Rather than one opaque tar blob, each file is hashed individually (BLAKE3 via
//! [`heart::ContentHash`]) so the registry can content-address, dedupe across
//! versions, and serve ranged reads. A tar view is still producible on demand
//! (see [`super::tar`]); it is not the stored representation.

use std::path::PathBuf;

use heart::content::ContentHash;

use crate::{error::GenerateError, generate::PackageInput};

/// One source file, content-addressed. Mirrors the registry's `FileEntry`, so
/// the generated archive maps directly onto the stored blob manifest.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileDigest {
    /// The file path, relative to the package root.
    pub path: PathBuf,

    /// The BLAKE3 hash of the file's exact bytes.
    pub hash: ContentHash,

    /// The file size in bytes.
    pub size: u64,
}

/// The content-addressed source archive: every source file's digest, sorted by
/// path for a reproducible manifest fingerprint.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SourceArchive {
    /// The per-file digests, sorted by path.
    pub files: Vec<FileDigest>,
}

/// Walk the package source, hashing each file as it is read (bounded memory —
/// files stream through the hasher, never all held at once) into a sorted set of
/// [`FileDigest`]s.
///
/// Runs on a blocking pool (filesystem + hashing are sync CPU/IO work).
pub fn build(input: &PackageInput) -> Result<SourceArchive, GenerateError> {
    let _ = input;
    todo!("walk root, hash each file streaming, collect+sort FileDigests")
}
