//! Shard-artifact pack/unpack — the client half of the §20.3 bakery
//! contract.
//!
//! An artifact is a `tar.zst` of one self-contained Edge shard directory,
//! identified by the BLAKE3 [`ContentHash`] **of the compressed bytes**
//! (`artifact_id` in the package manifest). A shard artifact is parsed by
//! native code, so it is treated as untrusted input:
//!
//! - the hash is verified **before** a single byte is decompressed,
//! - tar entries are hardened: absolute paths, `..` components, and link
//!   entries are rejected outright (classic tar-slip vectors),
//! - extraction lands in `<destination>.tmp` and is atomically renamed to `destination`,
//!   so a crash mid-unpack never leaves a half-shard where a loader could
//!   find it.

use std::fs;
use std::io::Write as _;
use std::path::{Component, Path, PathBuf};

use heart::ContentHash;
use tar::EntryType;

use super::lock::LOCK_FILE;

/// Zstd compression level for shard artifacts: fast to decode on install, high enough
/// to matter on the wire and in CAS.
const ZSTD_LEVEL: i32 = 9;

/// Pack and unpack failures.
#[derive(Debug, thiserror::Error)]
pub enum PackError {
    #[error("shard artifact io: {0}")]
    Io(#[from] std::io::Error),

    /// The artifact bytes do not hash to the manifest's `artifact_id`.
    /// Nothing was decompressed or written.
    #[error("shard artifact hash mismatch: expected {expected}, got {actual}; refusing to unpack")]
    HashMismatch {
        expected: ContentHash,
        actual: ContentHash,
    },

    /// A tar entry tried to escape the destination (absolute path, `..`
    /// component) or is a link — hostile input, whole artifact rejected.
    #[error("unsafe tar entry {0:?}; artifact rejected")]
    UnsafeEntry(String),

    /// The unpack destination already exists; upgrades are whole-directory
    /// swaps performed by the caller, never in-place merges.
    #[error("unpack destination {} already exists", .0.display())]
    DestinationExists(PathBuf),
}

/// Pack the shard directory at `directory` into a `tar.zst` artifact, returning
/// the compressed bytes and their BLAKE3 [`ContentHash`] (the artifact id).
///
/// Entries are archived in sorted relative-path order with deterministic
/// headers, so a fixed byte tree packs to fixed bytes. The advisory lock file is
/// skipped: it is per-machine state, not shard content. Symlinks inside a
/// shard are unexpected and rejected rather than followed.
pub fn pack_shard(directory: &Path) -> Result<(Vec<u8>, ContentHash), PackError> {
    let entries = collect_sorted_entries(directory)?;

    let zstd_encoder = zstd::stream::write::Encoder::new(Vec::new(), ZSTD_LEVEL)?;
    let mut tar_builder = tar::Builder::new(zstd_encoder);
    tar_builder.mode(tar::HeaderMode::Deterministic);
    tar_builder.follow_symlinks(false);

    for relative_path in &entries {
        append_entry(&mut tar_builder, directory, relative_path)?;
    }

    let zstd_encoder = tar_builder.into_inner()?;
    let compressed_bytes = zstd_encoder.finish()?;
    let artifact_hash = ContentHash::of_bytes(&compressed_bytes);

    Ok((compressed_bytes, artifact_hash))
}

/// Pack a shard directly to a file (convenience over [`pack_shard`]),
/// returning the artifact id of the written bytes.
pub fn pack_shard_to_file(
    directory: &Path,
    artifact_destination: &Path,
) -> Result<ContentHash, PackError> {
    let (compressed_bytes, artifact_hash) = pack_shard(directory)?;
    let mut destination_file = fs::File::create(artifact_destination)?;
    destination_file.write_all(&compressed_bytes)?;
    destination_file.sync_all()?;
    Ok(artifact_hash)
}

/// Verify and unpack a shard artifact to `destination`.
///
/// Order of operations (each step gates the next):
/// 1. BLAKE3 of `artifact_bytes` must equal `expected_hash` — checked **before** any
///    decompression ([`PackError::HashMismatch`] otherwise).
/// 2. `destination` must not exist ([`PackError::DestinationExists`]).
/// 3. Entries are extracted into `<destination>.tmp` with path hardening: only
///    `Normal`/`CurDir` components, only regular files and directories.
///    Any rejection deletes the partial extraction.
/// 4. `<destination>.tmp` is atomically renamed to `destination`.
pub fn unpack_shard(
    artifact_bytes: &[u8],
    expected_hash: &ContentHash,
    destination: &Path,
) -> Result<(), PackError> {
    verify_hash(artifact_bytes, expected_hash)?;
    ensure_destination_absent(destination)?;

    let temporary_destination = compute_temporary_sibling(destination)?;
    prepare_temporary_directory(&temporary_destination)?;

    let unpack_result = unpack_into(artifact_bytes, &temporary_destination);
    if unpack_result.is_err() {
        let _ = fs::remove_dir_all(&temporary_destination);
    }
    unpack_result?;

    fs::rename(&temporary_destination, destination)?;
    Ok(())
}

// -----------------------------------------------------------------------------
// Helper Functions: Packing
// -----------------------------------------------------------------------------

fn collect_sorted_entries(root_directory: &Path) -> Result<Vec<PathBuf>, PackError> {
    let mut entries = Vec::new();
    collect_entries_recursive(root_directory, Path::new(""), &mut entries)?;
    entries.sort();
    Ok(entries)
}

fn collect_entries_recursive(
    root_directory: &Path,
    relative_directory: &Path,
    entries: &mut Vec<PathBuf>,
) -> Result<(), PackError> {
    for directory_entry in fs::read_dir(root_directory.join(relative_directory))? {
        let directory_entry = directory_entry?;
        let file_name = directory_entry.file_name();

        if file_name.to_str() == Some(LOCK_FILE) {
            continue;
        }

        let relative_path = relative_directory.join(&file_name);
        let file_type = directory_entry.file_type()?;

        if file_type.is_symlink() {
            return Err(PackError::UnsafeEntry(format!(
                "{} is a symlink; shard directories must be plain trees",
                relative_path.display()
            )));
        }

        entries.push(relative_path.clone());
        if file_type.is_dir() {
            collect_entries_recursive(root_directory, &relative_path, entries)?;
        }
    }
    Ok(())
}

fn append_entry<W: std::io::Write>(
    tar_builder: &mut tar::Builder<W>,
    root_directory: &Path,
    relative_path: &Path,
) -> Result<(), PackError> {
    let absolute_path = root_directory.join(relative_path);
    if absolute_path.is_dir() {
        tar_builder.append_dir(relative_path, &absolute_path)?;
    } else {
        tar_builder.append_path_with_name(&absolute_path, relative_path)?;
    }
    Ok(())
}

// -----------------------------------------------------------------------------
// Helper Functions: Unpacking
// -----------------------------------------------------------------------------

fn verify_hash(artifact_bytes: &[u8], expected_hash: &ContentHash) -> Result<(), PackError> {
    let actual_hash = ContentHash::of_bytes(artifact_bytes);
    if actual_hash != *expected_hash {
        return Err(PackError::HashMismatch {
            expected: *expected_hash,
            actual: actual_hash,
        });
    }
    Ok(())
}

fn ensure_destination_absent(destination: &Path) -> Result<(), PackError> {
    if destination.exists() {
        return Err(PackError::DestinationExists(destination.to_path_buf()));
    }
    Ok(())
}

fn prepare_temporary_directory(temporary_destination: &Path) -> Result<(), PackError> {
    if temporary_destination.exists() {
        fs::remove_dir_all(temporary_destination)?;
    }
    if let Some(parent_directory) = temporary_destination.parent() {
        fs::create_dir_all(parent_directory)?;
    }
    fs::create_dir_all(temporary_destination)?;
    Ok(())
}

fn unpack_into(artifact_bytes: &[u8], root_directory: &Path) -> Result<(), PackError> {
    let zstd_decoder = zstd::stream::read::Decoder::new(artifact_bytes)?;
    let mut tar_archive = tar::Archive::new(zstd_decoder);

    for entry in tar_archive.entries()? {
        let mut entry = entry?;
        let entry_path = entry.path()?.into_owned();
        validate_safe_path(&entry_path)?;

        unpack_single_entry(&mut entry, root_directory, &entry_path)?;
    }
    Ok(())
}

fn unpack_single_entry<R: std::io::Read>(
    entry: &mut tar::Entry<'_, R>,
    root_directory: &Path,
    relative_path: &Path,
) -> Result<(), PackError> {
    let target_path = root_directory.join(relative_path);

    match entry.header().entry_type() {
        EntryType::Directory => {
            fs::create_dir_all(&target_path)?;
        }
        EntryType::Regular => {
            if let Some(parent_directory) = target_path.parent() {
                fs::create_dir_all(parent_directory)?;
            }
            entry.unpack(&target_path)?;
        }
        other_entry_type => {
            return Err(PackError::UnsafeEntry(format!(
                "{} (entry type {other_entry_type:?})",
                relative_path.display()
            )));
        }
    }
    Ok(())
}

fn validate_safe_path(path: &Path) -> Result<(), PackError> {
    let is_safe = !path.as_os_str().is_empty()
        && path
            .components()
            .all(|component| matches!(component, Component::Normal(_) | Component::CurDir));

    if is_safe {
        Ok(())
    } else {
        Err(PackError::UnsafeEntry(path.display().to_string()))
    }
}

fn compute_temporary_sibling(destination: &Path) -> Result<PathBuf, PackError> {
    let file_name = destination.file_name().ok_or_else(|| {
        PackError::UnsafeEntry(format!(
            "unpack destination {} has no name",
            destination.display()
        ))
    })?;

    let mut temporary_file_name = file_name.to_os_string();
    temporary_file_name.push(".tmp");
    Ok(destination.with_file_name(temporary_file_name))
}
