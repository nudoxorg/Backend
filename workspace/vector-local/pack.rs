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
//! - extraction lands in `<dest>.tmp` and is atomically renamed to `dest`,
//!   so a crash mid-unpack never leaves a half-shard where a loader could
//!   find it.

use std::fs;
use std::io::Write as _;
use std::path::{Component, Path, PathBuf};

use heart::ContentHash;
use tar::EntryType;

use crate::lock::LOCK_FILE;

/// zstd level for shard artifacts: fast to decode on install, high enough
/// to matter on the wire and in CAS.
const ZSTD_LEVEL: i32 = 9;

/// Pack/unpack failures.
#[derive(Debug, thiserror::Error)]
pub enum PackError {
	#[error("shard artifact io: {0}")]
	Io(#[from] std::io::Error),

	/// The artifact bytes do not hash to the manifest's `artifact_id`.
	/// Nothing was decompressed or written.
	#[error("shard artifact hash mismatch: expected {expected}, got {actual}; refusing to unpack")]
	HashMismatch { expected: ContentHash, actual: ContentHash },

	/// A tar entry tried to escape the destination (absolute path, `..`
	/// component) or is a link — hostile input, whole artifact rejected.
	#[error("unsafe tar entry {0:?}; artifact rejected")]
	UnsafeEntry(String),

	/// The unpack destination already exists; upgrades are whole-directory
	/// swaps performed by the caller, never in-place merges.
	#[error("unpack destination {} already exists", .0.display())]
	DestinationExists(PathBuf),
}

/// Pack the shard directory at `dir` into a `tar.zst` artifact, returning
/// the compressed bytes and their BLAKE3 [`ContentHash`] (the artifact id).
///
/// Entries are archived in sorted relative-path order with deterministic
/// headers, so a fixed byte tree packs to fixed bytes (though the §20.3
/// contract only promises identity for the *once-baked* artifact — Edge's
/// own HNSW build is not bit-reproducible). The advisory lock file is
/// skipped: it is per-machine state, not shard content. Symlinks inside a
/// shard are unexpected and rejected rather than followed.
pub fn pack_shard(dir: &Path) -> Result<(Vec<u8>, ContentHash), PackError> {
	let mut entries = Vec::new();
	collect_entries(dir, Path::new(""), &mut entries)?;
	entries.sort();

	let mut builder = tar::Builder::new(Vec::new());
	builder.mode(tar::HeaderMode::Deterministic);
	// We reject links on read anyway; never follow them on write either.
	builder.follow_symlinks(false);
	for relative in &entries {
		let absolute = dir.join(relative);
		if absolute.is_dir() {
			builder.append_dir(relative, &absolute)?;
		} else {
			builder.append_path_with_name(&absolute, relative)?;
		}
	}
	let tar_bytes = builder.into_inner()?;

	let compressed = zstd::stream::encode_all(tar_bytes.as_slice(), ZSTD_LEVEL)?;
	let hash = ContentHash::of_bytes(&compressed);
	Ok((compressed, hash))
}

/// Pack a shard directly to a file (convenience over [`pack_shard`]),
/// returning the artifact id of the written bytes.
pub fn pack_shard_to_file(dir: &Path, artifact_path: &Path) -> Result<ContentHash, PackError> {
	let (bytes, hash) = pack_shard(dir)?;
	let mut file = fs::File::create(artifact_path)?;
	file.write_all(&bytes)?;
	file.sync_all()?;
	Ok(hash)
}

/// Verify and unpack a shard artifact to `dest`.
///
/// Order of operations (each step gates the next):
/// 1. BLAKE3 of `artifact` must equal `expected` — checked **before** any
///    decompression ([`PackError::HashMismatch`] otherwise).
/// 2. `dest` must not exist ([`PackError::DestinationExists`]; upgrades
///    swap whole directories, the caller removes the old one first).
/// 3. Entries are extracted into `<dest>.tmp` with path hardening: only
///    `Normal`/`CurDir` components, only regular files and directories.
///    Any rejection deletes the partial extraction.
/// 4. `<dest>.tmp` is atomically renamed to `dest`.
pub fn unpack_shard(artifact: &[u8], expected: &ContentHash, dest: &Path) -> Result<(), PackError> {
	let actual = ContentHash::of_bytes(artifact);
	if actual != *expected {
		return Err(PackError::HashMismatch { expected: *expected, actual });
	}
	if dest.exists() {
		return Err(PackError::DestinationExists(dest.to_path_buf()));
	}

	let tmp = tmp_sibling(dest)?;
	if tmp.exists() {
		// Leftover from a crashed earlier unpack; it never became `dest`,
		// so it is garbage by construction.
		fs::remove_dir_all(&tmp)?;
	}
	if let Some(parent) = dest.parent() {
		fs::create_dir_all(parent)?;
	}
	fs::create_dir_all(&tmp)?;

	if let Err(err) = unpack_into(artifact, &tmp) {
		// Never leave partial extraction behind a rejected artifact.
		let _ = fs::remove_dir_all(&tmp);
		return Err(err);
	}

	fs::rename(&tmp, dest)?;
	Ok(())
}

/// Extract all (already hash-verified) entries under `root`, hardening
/// every path.
fn unpack_into(artifact: &[u8], root: &Path) -> Result<(), PackError> {
	let decoder = zstd::stream::read::Decoder::new(artifact)?;
	let mut archive = tar::Archive::new(decoder);

	for entry in archive.entries()? {
		let mut entry = entry?;
		let path = entry.path()?.into_owned();
		reject_unsafe_path(&path)?;

		match entry.header().entry_type() {
			EntryType::Directory => {
				fs::create_dir_all(root.join(&path))?;
			}
			EntryType::Regular => {
				let target = root.join(&path);
				if let Some(parent) = target.parent() {
					fs::create_dir_all(parent)?;
				}
				entry.unpack(&target)?;
			}
			// Links (hard or symbolic) inside a shard artifact are hostile.
			other => {
				return Err(PackError::UnsafeEntry(format!(
					"{} (entry type {other:?})",
					path.display()
				)));
			}
		}
	}
	Ok(())
}

/// Only plain forward-relative paths survive: no roots, no prefixes, no
/// `..`.
fn reject_unsafe_path(path: &Path) -> Result<(), PackError> {
	let safe = path
		.components()
		.all(|component| matches!(component, Component::Normal(_) | Component::CurDir));
	if path.as_os_str().is_empty() || !safe {
		return Err(PackError::UnsafeEntry(path.display().to_string()));
	}
	Ok(())
}

/// `<dest>.tmp` next to the destination (same filesystem, so the final
/// rename is atomic).
fn tmp_sibling(dest: &Path) -> Result<PathBuf, PackError> {
	let name = dest.file_name().ok_or_else(|| {
		PackError::UnsafeEntry(format!("unpack destination {} has no name", dest.display()))
	})?;
	let mut tmp_name = name.to_os_string();
	tmp_name.push(".tmp");
	Ok(dest.with_file_name(tmp_name))
}

/// Walk `dir` collecting relative paths (files and directories). The lock
/// file is skipped; symlinks are rejected.
fn collect_entries(
	root: &Path,
	relative: &Path,
	entries: &mut Vec<PathBuf>,
) -> Result<(), PackError> {
	for dir_entry in fs::read_dir(root.join(relative))? {
		let dir_entry = dir_entry?;
		let name = dir_entry.file_name();
		if name.to_str() == Some(LOCK_FILE) {
			continue;
		}
		let rel = relative.join(&name);
		let file_type = dir_entry.file_type()?;
		if file_type.is_symlink() {
			return Err(PackError::UnsafeEntry(format!(
				"{} is a symlink; shard directories must be plain trees",
				rel.display()
			)));
		}
		entries.push(rel.clone());
		if file_type.is_dir() {
			collect_entries(root, &rel, entries)?;
		}
	}
	Ok(())
}
