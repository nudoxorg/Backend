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

use bytes::Bytes;
use smol_str::SmolStr;

use super::ExtractionLimits;
use crate::error::{EntryKind, IngestError, UnsafeArchive};

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
		let _ = kind;
		todo!("map EntryKind onto allowlist; reject Symlink/Hardlink/Char/Block/Fifo/Other")
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
	/// [`UnsafeArchive`] variant) if it would breach any ceiling.
	pub fn charge(&mut self, size: u64) -> Result<(), UnsafeArchive> {
		let _ = (size, &mut self.total_bytes, &mut self.file_count, &self.limits);
		todo!("check per-file, running-total, and file-count ceilings; update on success")
	}
}

/// Jail a raw archive path inside the extraction root.
///
/// Rejects absolute paths, `..` components that escape, and anything whose
/// canonical form would land outside the root. Returns the normalized
/// package-relative path on success. The single choke point for path safety —
/// no other code interprets archive paths.
pub fn jail_path(root_depth_limit: usize, raw: &str) -> Result<SmolStr, UnsafeArchive> {
	let _ = (root_depth_limit, raw);
	todo!("reject absolute/traversal, enforce depth <= limit, normalize to relative SmolStr")
}

/// Classify a tar header's entry type into the boundary's [`EntryKind`].
pub fn classify(entry_type: tar::EntryType) -> EntryKind {
	let _ = entry_type;
	todo!("map tar::EntryType onto EntryKind, folding unknowns to Other")
}

/// Sanitize one raw tar entry into a [`SanitizedEntry`], or reject it.
///
/// Applies, in order: entry-type allowlist, path jail, per-entry size charge.
/// Only regular files with clean paths under budget survive. `// runs on
/// spawn_blocking (synchronous tar read)`.
pub fn sanitize_entry(
	allow: EntryAllowlist,
	budget: &mut Budget,
	limits: ExtractionLimits,
	raw_path: &str,
	entry_type: tar::EntryType,
	read_bytes: impl FnOnce() -> std::io::Result<Bytes>,
) -> Result<Option<SanitizedEntry>, IngestError> {
	let _ = (allow, budget, limits, raw_path, entry_type, read_bytes);
	todo!("classify+admit, jail_path, charge budget, read bytes; None for admitted dirs")
}
