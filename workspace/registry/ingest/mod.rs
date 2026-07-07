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
#[tracing::instrument(skip(toolchain, reader, allow), fields(format = ?format))]
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
	use tokio::io::AsyncReadExt;

	use crate::error::UnsafeArchive;

	// Stage 1 — pull the *compressed* stream into a bounded buffer. The
	// decompressed payload is what bombs; the compressed form is capped at the
	// total-bytes ceiling (a compressed stream already larger than the
	// uncompressed ceiling cannot possibly extract under it), so this buffer is
	// finite by construction and the caller's reader borrow never has to cross
	// onto a blocking thread. Decompressed bytes are only ever materialized one
	// budget-charged file at a time.
	let ceiling = limits.max_total_bytes;
	let mut compressed = Vec::new();
	let mut bounded = reader.take(ceiling + 1);
	bounded.read_to_end(&mut compressed).await.map_err(IngestError::Io)?;
	if compressed.len() as u64 > ceiling {
		return Err(IngestError::Unsafe(UnsafeArchive::TotalTooLarge {
			actual: compressed.len() as u64,
			limit: ceiling,
		}));
	}
	tracing::debug!(compressed_bytes = compressed.len(), "archive buffered under ceiling");

	// Stage 2 — decompress + walk the tar synchronously, off the async runtime.
	// `// blake3 hashing + tar walking run on spawn_blocking`.
	tokio::task::spawn_blocking(move || {
		extract_into_builder(package, toolchain, compressed, format, limits, allow)
	})
	.await
	.map_err(|join| IngestError::Io(std::io::Error::other(join)))?
}

/// The synchronous half of [`ingest_archive`]: decompress, walk, sanitize, and
/// feed the builder. Runs on a blocking thread.
fn extract_into_builder(
	package: PackageId,
	toolchain: Toolchain,
	compressed: Vec<u8>,
	format: ArchiveFormat,
	limits: ExtractionLimits,
	allow: EntryAllowlist,
) -> Result<BlobBuilder, IngestError> {
	use std::io::Read;

	use crate::error::UnsafeArchive;

	let cursor = std::io::Cursor::new(compressed);
	let decompressed: Box<dyn Read> = match format {
		ArchiveFormat::TarGz => Box::new(flate2::read::GzDecoder::new(cursor)),
		ArchiveFormat::TarZst => {
			Box::new(zstd::stream::read::Decoder::new(cursor).map_err(IngestError::Malformed)?)
		}
		ArchiveFormat::Tar => Box::new(cursor),
	};

	let mut archive = tar::Archive::new(decompressed);
	let mut builder = BlobBuilder::new(package, toolchain);
	let mut budget = extract::Budget::new(limits);
	let mut admitted = 0usize;

	for entry in archive.entries().map_err(IngestError::Malformed)? {
		let mut entry = entry.map_err(IngestError::Malformed)?;

		// A non-UTF-8 path is jail-ambiguous; the boundary rejects it outright.
		let raw_path = match std::str::from_utf8(&entry.path_bytes()) {
			Ok(path) => path.to_owned(),
			Err(_) => {
				return Err(IngestError::Unsafe(UnsafeArchive::PathTraversal {
					path: String::from_utf8_lossy(&entry.path_bytes()).into_owned(),
				}));
			}
		};

		// A lying header can never make us materialize past the per-file
		// ceiling: the declared size is checked first and the read is clamped.
		let declared = entry.header().size().map_err(IngestError::Malformed)?;
		if declared > limits.max_file_bytes {
			return Err(IngestError::Unsafe(UnsafeArchive::FileTooLarge {
				path: raw_path,
				actual: declared,
				limit: limits.max_file_bytes,
			}));
		}

		let entry_type = entry.header().entry_type();
		let sanitized =
			extract::sanitize_entry(allow, &mut budget, limits, &raw_path, entry_type, || {
				let mut bytes = Vec::with_capacity(declared as usize);
				(&mut entry).take(limits.max_file_bytes + 1).read_to_end(&mut bytes)?;
				Ok(bytes::Bytes::from(bytes))
			})?;

		if let Some(file) = sanitized {
			builder.push_file(file.path, file.bytes)?;
			admitted += 1;
		}
	}

	tracing::info!(package = %package, files = admitted, "archive sanitized and ingested");
	Ok(builder)
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
