//! Deterministic directory ingestion for NDPK v1 packs.
//!
//! # Determinism
//!
//! [`ingest_directory`] and [`ingest_directory_into`] both produce the same
//! member set for the same on-disk tree, regardless of the order that the OS
//! returns directory entries (which is filesystem-dependent and non-deterministic
//! on most systems). This is achieved by:
//!
//! 1. Collecting all entries in a directory before recursing.
//! 2. Sorting the collected entries by their file name (byte order) before
//!    processing them.
//! 3. Building [`RelativePath`] values by joining components with `'/'`,
//!    never using the OS path separator.
//!
//! # Symlink policy
//!
//! Symlinks are **rejected** and cause [`PackError::UnsafePathSegment`] to be
//! returned immediately. Rationale: symlinks can escape the tree root (dangling
//! or relative-traversal links), are non-deterministic across platforms
//! (Windows junction vs. Unix symlink), and a future version may record them as
//! [`MemberKey::Meta`] entries instead. For now, packs must be self-contained
//! regular-file trees.
//!
//! # Special files
//!
//! Directory entries that are neither regular files nor directories (sockets,
//! FIFOs, device nodes, …) are also rejected with [`PackError::UnsafePathSegment`].

use std::path::Path;

use bytes::Bytes;
use smol_str::SmolStr;

use heart::object_pack::RelativePath;

use crate::pack::builder::ObjectPackBuilder;
use crate::pack::error::{PackError, PackResult};

/// A namespace type for the directory ingestion entry points.
///
/// All functionality is exposed as associated functions; there is no instance
/// state.
pub struct TreeIngest;

impl TreeIngest {
    /// Walk `root` deterministically and return a builder ready for sealing.
    ///
    /// The returned [`ObjectPackBuilder`] contains one [`MemberKey::Source`]
    /// per regular file found under `root`, keyed by its `/`-separated path
    /// relative to `root`.
    ///
    /// # Errors
    ///
    /// - [`PackError::Io`] on any I/O failure.
    /// - [`PackError::UnsafePathSegment`] if a symlink or special file is
    ///   encountered.
    pub fn ingest_directory(root: &Path) -> PackResult<ObjectPackBuilder> {
        let mut builder = ObjectPackBuilder::new();
        Self::ingest_directory_into(root, &mut builder)?;
        Ok(builder)
    }

    /// Walk `root` deterministically and add its files to `builder`.
    ///
    /// Equivalent to [`ingest_directory`] but writes into an existing builder,
    /// allowing multiple directory roots to be merged into a single pack.
    ///
    /// [`ingest_directory`]: TreeIngest::ingest_directory
    pub fn ingest_directory_into(
        root: &Path,
        builder: &mut ObjectPackBuilder,
    ) -> PackResult<()> {
        // Resolve the root to an absolute path so our recursive helper can
        // strip it cleanly when building relative paths.
        let absolute_root = root.canonicalize().map_err(PackError::Io)?;

        visit_directory(&absolute_root, &absolute_root, builder)
    }
}

// ---------------------------------------------------------------------------
// Private recursive walk
// ---------------------------------------------------------------------------

/// Recursively visit `directory`, collecting and sorting its entries before
/// descending, to guarantee a deterministic traversal order.
///
/// `root` is the original ingestion root used to derive relative paths.
fn visit_directory(
    root: &Path,
    directory: &Path,
    builder: &mut ObjectPackBuilder,
) -> PackResult<()> {
    // Collect all entries first so we can sort them.
    let mut entries: Vec<std::fs::DirEntry> = std::fs::read_dir(directory)
        .map_err(PackError::Io)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(PackError::Io)?;

    // Sort by file name (byte order) for determinism.
    entries.sort_by(|left, right| left.file_name().cmp(&right.file_name()));

    for entry in entries {
        let entry_path = entry.path();

        // Use `symlink_metadata` so we see symlinks as symlinks, not their
        // target.
        let metadata = std::fs::symlink_metadata(&entry_path).map_err(PackError::Io)?;
        let file_type = metadata.file_type();

        if file_type.is_symlink() {
            let relative_display = relative_path_string(root, &entry_path);
            return Err(PackError::UnsafePathSegment {
                path: relative_display,
                reason: "symlink not permitted in source tree".to_owned(),
            });
        }

        if file_type.is_dir() {
            visit_directory(root, &entry_path, builder)?;
            continue;
        }

        if !file_type.is_file() {
            // Reject sockets, FIFOs, device nodes, etc.
            let relative_display = relative_path_string(root, &entry_path);
            return Err(PackError::UnsafePathSegment {
                path: relative_display,
                reason: "special file (not a regular file or directory) not permitted in source \
                         tree"
                    .to_owned(),
            });
        }

        // Regular file — read it and add to the builder.
        let relative_key = build_relative_path(root, &entry_path)?;
        let contents: Vec<u8> = std::fs::read(&entry_path).map_err(PackError::Io)?;
        builder.add_source_file(relative_key, Bytes::from(contents))?;
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// Path helpers
// ---------------------------------------------------------------------------

/// Build a [`RelativePath`] by stripping `root` from `absolute_entry` and
/// joining the remaining components with `/`.
///
/// Always uses `/` as the separator, even on Windows, so packs are
/// cross-platform compatible.
fn build_relative_path(root: &Path, absolute_entry: &Path) -> PackResult<RelativePath> {
    let stripped = absolute_entry.strip_prefix(root).map_err(|_| PackError::BadStructure {
        detail: format!(
            "entry {:?} is not under root {:?}",
            absolute_entry, root
        ),
    })?;

    // Join components with '/' regardless of the host OS separator.
    let relative_string: String = stripped
        .components()
        .map(|component| {
            // `OsStr` may be non-UTF-8 on some platforms. We require UTF-8
            // file names — reject lossy conversions.
            component
                .as_os_str()
                .to_str()
                .ok_or_else(|| PackError::UnsafePathSegment {
                    path: absolute_entry.to_string_lossy().into_owned(),
                    reason: "non-UTF-8 path component".to_owned(),
                })
        })
        .collect::<PackResult<Vec<&str>>>()?
        .join("/");

    Ok(RelativePath(SmolStr::new(relative_string)))
}

/// Produce a human-readable relative path string for error messages.
///
/// Falls back to the absolute path if stripping fails (should not happen during
/// a well-formed walk, but we never panic).
fn relative_path_string(root: &Path, absolute_entry: &Path) -> String {
    absolute_entry
        .strip_prefix(root)
        .map(|rel| rel.to_string_lossy().into_owned())
        .unwrap_or_else(|_| absolute_entry.to_string_lossy().into_owned())
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// Ingesting a small tree must report the correct member count.
    #[test]
    fn ingest_directory_counts_members() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();

        // Create a nested structure.
        std::fs::create_dir(root.join("src")).unwrap();
        std::fs::write(root.join("README.txt"), b"readme").unwrap();
        std::fs::write(root.join("src/main.rs"), b"fn main() {}").unwrap();
        std::fs::write(root.join("src/lib.rs"), b"pub fn hello() {}").unwrap();

        let builder = TreeIngest::ingest_directory(root).unwrap();
        assert_eq!(builder.member_count(), 3, "expected 3 files");
    }

    /// Ingesting the same tree twice must produce byte-identical packs.
    #[test]
    fn two_ingests_of_same_tree_produce_identical_packs() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();

        std::fs::create_dir(root.join("alpha")).unwrap();
        std::fs::write(root.join("alpha/a.txt"), b"aaaa").unwrap();
        std::fs::write(root.join("alpha/b.txt"), b"bbbb").unwrap();
        std::fs::write(root.join("top.txt"), b"top level").unwrap();

        let (bytes_first, id_first) =
            TreeIngest::ingest_directory(root).unwrap().seal_to_bytes().unwrap();

        let (bytes_second, id_second) =
            TreeIngest::ingest_directory(root).unwrap().seal_to_bytes().unwrap();

        assert_eq!(id_first, id_second, "ObjectPackId must match across two ingests");
        assert_eq!(
            bytes_first, bytes_second,
            "pack bytes must be byte-identical across two ingests"
        );
    }

    /// Symlinks must be rejected.
    #[cfg(unix)]
    #[test]
    fn symlink_is_rejected() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();

        std::fs::write(root.join("real.txt"), b"real content").unwrap();
        std::os::unix::fs::symlink(root.join("real.txt"), root.join("link.txt")).unwrap();

        let result = TreeIngest::ingest_directory(root).unwrap_err();
        assert!(
            matches!(result, PackError::UnsafePathSegment { ref reason, .. }
                if reason.contains("symlink")),
            "expected symlink rejection, got {result:?}"
        );
    }

    /// An empty directory must produce an empty builder (seals fine).
    #[test]
    fn empty_directory_produces_empty_builder() {
        let temp = tempfile::tempdir().unwrap();
        let builder = TreeIngest::ingest_directory(temp.path()).unwrap();
        assert!(builder.is_empty());
        // Must seal without error.
        let (_bytes, _id) = builder.seal_to_bytes().unwrap();
    }
}
