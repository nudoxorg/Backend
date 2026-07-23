//! The sanitizing archive extractor — the security boundary.
//!
//! Given a tar stream and an [`ExtractionLimits`] envelope, this yields only
//! *safe* regular-file entries: everything else (symlinks, hardlinks, device
//! nodes, fifos, sockets, traversal paths, oversize files, bombs) is rejected
//! as an [`crate::error::UnsafeArchive`]. The path jail canonicalizes every
//! entry inside a root and refuses anything that escapes it.
//!
//! Streaming is essential: we decompress and inspect entries incrementally,
//! enforcing the running total-bytes ceiling *before* writing, so a bomb is
//! stopped at the ceiling rather than after it has already exhausted memory or
//! disk.

use std::path::PathBuf;

use bytes::Bytes;
use smol_str::SmolStr;

use super::ExtractionLimits;
use index::error::{EntryKind, IngestError, UnsafeArchive};

/// Which tar entry types are permitted through the boundary. Everything not
/// explicitly allowed is rejected — a default-deny allowlist, not a blocklist.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct EntryAllowlist {
	/// Permit regular files (essentially always `true`).
	pub regular: bool,
	/// Permit directory entries (structural; carry no bytes).
	pub directories: bool,
}

impl EntryAllowlist {
	/// The only safe default: regular files + directories, nothing else. Symlinks,
	/// hardlinks, and device/fifo/socket nodes are always rejected.
	pub const SAFE: Self = Self { regular: true, directories: true };

	/// Classify a raw tar entry-type byte and decide if it is permitted.
	pub fn admits(&self, kind: EntryKind) -> bool {
		match kind {
			EntryKind::Regular => self.regular,
			EntryKind::Directory => self.directories,
			// Never configurable: links can point out of the jail, and device /
			// fifo / socket nodes have no business in a source archive.
			EntryKind::Symlink
			| EntryKind::Hardlink
			| EntryKind::Character
			| EntryKind::Block
			| EntryKind::Fifo
			| EntryKind::Other => false,
		}
	}
}

impl Default for EntryAllowlist {
	fn default() -> Self { Self::SAFE }
}

/// One sanitized, safe-to-store entry that survived the boundary: a jailed
/// relative path and its bytes (already size-checked).
#[derive(Debug, Clone)]
pub struct SanitizedEntry {
	/// The package-relative path, guaranteed to be normalized and inside the
	/// extraction root.
	pub path: SmolStr,

	/// The file's bytes.
	pub bytes: Bytes,
}

/// The running budget tracker enforcing the [`ExtractionLimits`] envelope across
/// a streaming extraction. Consulted before every write so a bomb is stopped at
/// the ceiling.
#[derive(Debug)]
pub struct Budget {
	limits: ExtractionLimits,
	total_bytes: u64,
	file_count: usize,
}

impl Budget {
	/// Start a fresh budget under the given limits.
	pub fn new(limits: ExtractionLimits) -> Self {
		Self { limits, total_bytes: 0, file_count: 0 }
	}

	/// Charge a would-be file against the budget, erroring (with the specific
	/// [`UnsafeArchive`] variant) if it would breach any ceiling. Nothing is
	/// consumed on failure. A `FileTooLarge` is raised with an empty path — the
	/// budget doesn't know one; [`sanitize_entry`] fills it in.
	pub fn charge(&mut self, size: u64) -> Result<(), UnsafeArchive> {
		let file_count = self.file_count + 1;
		if file_count > self.limits.max_files {
			return Err(UnsafeArchive::TooManyFiles {
				actual: file_count,
				limit: self.limits.max_files,
			});
		}
		if size > self.limits.max_file_bytes {
			return Err(UnsafeArchive::FileTooLarge {
				path: PathBuf::new(),
				actual: size,
				limit: self.limits.max_file_bytes,
			});
		}
		let total_bytes = self.total_bytes.saturating_add(size);
		if total_bytes > self.limits.max_total_bytes {
			return Err(UnsafeArchive::TotalTooLarge {
				actual: total_bytes,
				limit: self.limits.max_total_bytes,
			});
		}
		self.file_count = file_count;
		self.total_bytes = total_bytes;
		Ok(())
	}
}

/// Jail a raw archive path inside the extraction root.
///
/// Rejects absolute paths, `..` components that escape, and anything whose
/// canonical form would land outside the root. Returns the normalized
/// package-relative path on success. The single choke point for path safety —
/// no other code interprets archive paths.
pub fn jail_path(root_depth_limit: usize, raw: &str) -> Result<SmolStr, UnsafeArchive> {
	let traversal = || UnsafeArchive::PathTraversal { path: PathBuf::from(raw) };

	// Absolute paths (unix or windows-drive shaped), backslash separators, and
	// embedded NULs never survive — each is a jail-relevant ambiguity.
	let drive_absolute =
		raw.len() >= 2 && raw.as_bytes()[0].is_ascii_alphabetic() && raw.as_bytes()[1] == b':';
	if raw.starts_with('/') || raw.contains('\\') || raw.contains('\0') || drive_absolute {
		return Err(traversal());
	}

	// Normalize component-by-component; a `..` that pops past the root escaped.
	let mut segments: Vec<&str> = Vec::new();
	for component in raw.split('/') {
		match component {
			"" | "." => {}
			".." => {
				if segments.pop().is_none() {
					return Err(traversal());
				}
			}
			normal => segments.push(normal),
		}
	}
	if segments.is_empty() {
		return Err(traversal());
	}
	if segments.len() > root_depth_limit {
		return Err(UnsafeArchive::PathTooDeep {
			path: PathBuf::from(raw),
			actual: segments.len(),
			limit: root_depth_limit,
		});
	}
	Ok(SmolStr::from(segments.join("/")))
}

/// Classify a tar header's entry type into the boundary's [`EntryKind`].
pub fn classify(entry_type: tar::EntryType) -> EntryKind {
	use tar::EntryType;
	match entry_type {
		EntryType::Regular | EntryType::Continuous | EntryType::GNUSparse => EntryKind::Regular,
		EntryType::Directory => EntryKind::Directory,
		EntryType::Symlink => EntryKind::Symlink,
		EntryType::Link => EntryKind::Hardlink,
		EntryType::Char => EntryKind::Character,
		EntryType::Block => EntryKind::Block,
		EntryType::Fifo => EntryKind::Fifo,
		_ => EntryKind::Other,
	}
}

/// Sanitize one raw tar entry into a [`SanitizedEntry`], or reject it.
///
/// Applies, in order: entry-type allowlist, path jail, per-entry size charge.
/// Only regular files with clean paths under budget survive. The caller must
/// pre-bound `read_bytes` (e.g. against the declared header size) so a hostile
/// entry cannot materialize past the ceiling before the charge lands. `// runs
/// on spawn_blocking (synchronous tar read)`.
pub fn sanitize_entry(
	allow: EntryAllowlist,
	budget: &mut Budget,
	limits: ExtractionLimits,
	raw_path: &str,
	entry_type: tar::EntryType,
	read_bytes: impl FnOnce() -> std::io::Result<Bytes>,
) -> Result<Option<SanitizedEntry>, IngestError> {
	let kind = classify(entry_type);
	if !allow.admits(kind) {
		tracing::warn!(path = raw_path, ?kind, "disallowed archive entry rejected");
		return Err(IngestError::Unsafe(UnsafeArchive::DisallowedEntry {
			kind,
			path: PathBuf::from(raw_path),
		}));
	}

	let path = jail_path(limits.max_path_depth, raw_path).map_err(IngestError::Unsafe)?;

	// Directories are structural: admitted (their path was still validated) but
	// they carry no bytes and produce no entry.
	if kind == EntryKind::Directory {
		return Ok(None);
	}

	let bytes = read_bytes().map_err(IngestError::Io)?;
	budget
		.charge(bytes.len() as u64)
		.map_err(|violation| IngestError::Unsafe(locate(violation, &path)))?;
	Ok(Some(SanitizedEntry { path, bytes }))
}

/// Attach the offending path to a budget violation ([`Budget::charge`] doesn't
/// know one).
fn locate(violation: UnsafeArchive, path: &str) -> UnsafeArchive {
	match violation {
		UnsafeArchive::FileTooLarge { actual, limit, .. } => {
			UnsafeArchive::FileTooLarge { path: PathBuf::from(path), actual, limit }
		}
		other => other,
	}
}
