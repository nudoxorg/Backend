//! The condensed source resolution: a content-addressed set of the package's
//! source files.
//!
//! Rather than one opaque tar blob, each file is hashed individually (BLAKE3 via
//! [`heart::ContentHash`]) so the registry can content-address, dedupe across
//! versions, and serve ranged reads. A tar view is still producible on demand
//! (see [`super::tar`]); it is not the stored representation.

use std::{fs, io::BufRead, path::{Path, PathBuf}};

use heart::content::ContentHash;

use crate::{error::GenerateError, generate::PackageInput};

/// One source file, content-addressed. Mirrors the registry's `FileEntry`, so
/// the generated archive maps directly onto the stored blob manifest.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
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
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
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
    let mut files = Vec::new();
    let mut pending = vec![input.root.clone()];

    while let Some(dir) = pending.pop() {
        for entry in fs::read_dir(&dir)? {
            let entry = entry?;
            let file_type = entry.file_type()?;
            let path = entry.path();

            if file_type.is_dir() {
                // VCS metadata and language toolchain build dirs are not package
                // source (rustdoc may create `target/` during surface lowering).
                let name = entry.file_name();
                if name != ".git" && name != "target" && name != "node_modules" && name != "__pycache__" {
                    pending.push(path);
                }
            } else if file_type.is_file() {
                let (hash, size) = hash_file(&path)?;
                let relative = path
                    .strip_prefix(&input.root)
                    .expect("walk stays under the package root")
                    .to_path_buf();
                files.push(FileDigest { path: relative, hash, size });
            }
            // Symlinks are dropped: the extracted tree is already sanitized,
            // and following them could escape the root.
        }
    }

    files.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(SourceArchive { files })
}

/// Stream one file through a [`ContentHash`] builder, returning its digest and
/// byte size without ever holding the whole file in memory.
fn hash_file(path: &Path) -> Result<(ContentHash, u64), GenerateError> {
    let file = fs::File::open(path)?;
    let mut reader = std::io::BufReader::new(file);
    let mut hasher = ContentHash::builder();
    let mut size = 0u64;

    loop {
        let chunk = reader.fill_buf()?;
        if chunk.is_empty() {
            break;
        }
        hasher.update(chunk);
        size += chunk.len() as u64;
        let consumed = chunk.len();
        reader.consume(consumed);
    }

    Ok((hasher.finalize(), size))
}
