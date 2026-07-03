//! Ingest — turning an **untrusted** source archive into a sanitized,
//! content-addressed blob.
//!
//! Everything here treats its input as hostile: a package archive from a public
//! registry may be a decompression bomb, a path-traversal attempt, or a nest of
//! symlinks pointing at `/etc/shadow`. The safety policy is modelled *as types*
//! ([`ExtractionLimits`], the entry-type allowlist, the path jail) so a caller
//! cannot forget to apply it — and every violation maps to
//! [`heart::FailureKind::Unsafe`] so the queue dead-letters bombs instead of
//! retrying them.

use heart::{PackageId, Toolchain};

use crate::{blob::BlobBuilder, error::IngestError};

pub mod extract;

pub use extract::{EntryAllowlist, SanitizedEntry};

/// The safety envelope every extraction is bounded by. There is no unbounded
/// mode — a default must still be *some* finite policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ExtractionLimits {
	/// Ceiling on total uncompressed bytes across all entries (bomb defence).
	pub max_total_bytes: u64,

	/// Ceiling on any single file's uncompressed size.
	pub max_file_bytes: u64,

	/// Ceiling on the number of entries.
	pub max_files: usize,

	/// Ceiling on path nesting depth (defence against pathological trees).
	pub max_path_depth: usize,
}

impl ExtractionLimits {
	/// A conservative default policy (documented, never "unlimited").
	pub const DEFAULT: Self =
		Self { max_total_bytes: 512 << 20, max_file_bytes: 64 << 20, max_files: 50_000, max_path_depth: 32 };
}

impl Default for ExtractionLimits {
	fn default() -> Self { Self::DEFAULT }
}

/// The full ingest of one package: decompress + extract + sanitize an archive
/// stream, feeding each surviving `(path, bytes)` into a [`BlobBuilder`], and
/// finalize.
///
/// Takes an async reader (not a path or a materialized buffer) so the whole
/// archive is never resident. `// blake3 hashing + tar walking run on
/// spawn_blocking`.
pub async fn ingest_archive<R>(
	package: PackageId,
	toolchain: Toolchain,
	reader: R,
	format: ArchiveFormat,
	limits: ExtractionLimits,
	allow: EntryAllowlist,
) -> Result<BlobBuilder, IngestError>
where
	R: tokio::io::AsyncRead + Unpin + Send,
{
	let _ = (package, toolchain, reader, format, limits, allow);
	todo!("stream-decompress, extract::sanitize each entry, push into BlobBuilder, return it")
}

/// The compression framing of an incoming archive.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum ArchiveFormat {
	/// A `.tar.gz` (crates.io, npm sdists).
	TarGz,
	/// A `.tar.zst`.
	TarZst,
	/// A bare uncompressed `.tar`.
	Tar,
}
