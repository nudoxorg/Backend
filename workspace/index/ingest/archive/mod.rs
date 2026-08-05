//! Archive extraction — turning an **untrusted** source archive into a
//! sanitized, content-addressed blob.
//!
//! Everything here treats its input as hostile: a package archive from a public
//! registry may be a decompression bomb, a path-traversal attempt, or a nest of
//! symlinks pointing at `/etc/shadow`. The safety policy is modelled *as types*
//! ([`ExtractionLimits`], the entry-type allowlist, the path jail) so a caller
//! cannot forget to apply it — and every violation maps to
//! [`heart::FailureKind::Unsafe`] so the queue dead-letters bombs instead of
//! retrying them.

use std::path::PathBuf;

use heart::{PackageId, Toolchain};

use crate::blob::BlobBuilder;
use crate::error::IngestError;

pub mod extract;

pub use extract::{Budget, EntryAllowlist, SanitizedEntry, jail_path};

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
    pub const DEFAULT: Self = Self {
        max_total_bytes: 512 << 20,
        max_file_bytes: 64 << 20,
        max_files: 50_000,
        max_path_depth: 32,
    };
}

impl Default for ExtractionLimits {
    fn default() -> Self {
        Self::DEFAULT
    }
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
    bounded
        .read_to_end(&mut compressed)
        .await
        .map_err(IngestError::Io)?;
    if compressed.len() as u64 > ceiling {
        return Err(IngestError::Unsafe(UnsafeArchive::CompressedSizeExceeded {
            actual: compressed.len() as u64,
            limit: ceiling,
        }));
    }
    tracing::debug!(
        compressed_bytes = compressed.len(),
        "archive buffered under ceiling"
    );

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

    if format == ArchiveFormat::Zip {
        return extract_zip_into_builder(package, toolchain, compressed, limits, allow);
    }

    let cursor = std::io::Cursor::new(compressed);
    let decompressed: Box<dyn Read> = match format {
        ArchiveFormat::TarGz => Box::new(flate2::read::GzDecoder::new(cursor)),
        ArchiveFormat::TarZst => Box::new(
            zstd::stream::read::Decoder::new(cursor).map_err(IngestError::DecompressorInit)?,
        ),
        ArchiveFormat::Tar => Box::new(cursor),
        ArchiveFormat::Zip => unreachable!("handled above"),
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
                return Err(IngestError::Unsafe(UnsafeArchive::NonUtf8Path {
                    bytes: entry.path_bytes().into_owned(),
                }));
            }
        };

        // A lying header can never make us materialize past the per-file
        // ceiling: the declared size is checked first and the read is clamped.
        let declared = entry.header().size().map_err(IngestError::Malformed)?;
        if declared > limits.max_file_bytes {
            return Err(IngestError::Unsafe(UnsafeArchive::FileTooLarge {
                path: PathBuf::from(raw_path),
                actual: declared,
                limit: limits.max_file_bytes,
            }));
        }

        let entry_type = entry.header().entry_type();
        let sanitized =
            extract::sanitize_entry(allow, &mut budget, limits, &raw_path, entry_type, || {
                let mut bytes = Vec::with_capacity(declared as usize);
                (&mut entry)
                    .take(limits.max_file_bytes + 1)
                    .read_to_end(&mut bytes)?;
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

/// Zip-specific extraction (nupkg, goproxy module zips, jars). M1 fix.
///
/// Uses the same `Budget`, path-jail, and entry-type policy as the tar path:
/// symlink entries (unix mode bits 0o120000) are rejected; path traversal and
/// bombs are stopped identically.
fn extract_zip_into_builder(
    package: PackageId,
    toolchain: Toolchain,
    compressed: Vec<u8>,
    limits: ExtractionLimits,
    allow: EntryAllowlist,
) -> Result<BlobBuilder, IngestError> {
    use std::io::Read;

    use crate::error::UnsafeArchive;

    let cursor = std::io::Cursor::new(compressed);
    let mut zip = zip::ZipArchive::new(cursor)
        .map_err(|e| IngestError::Malformed(std::io::Error::other(e.to_string())))?;

    let mut builder = BlobBuilder::new(package, toolchain);
    let mut budget = extract::Budget::new(limits);
    let mut admitted = 0usize;

    for i in 0..zip.len() {
        let mut entry = zip
            .by_index(i)
            .map_err(|e| IngestError::Malformed(std::io::Error::other(e.to_string())))?;

        // Non-UTF-8 names are jail-ambiguous. `entry.name()` is already a
        // decoded (possibly cp437-mangled) string, so the check must run on
        // the raw name bytes.
        let raw_path = match std::str::from_utf8(entry.name_raw()) {
            Ok(path) => path.to_owned(),
            Err(_) => {
                return Err(IngestError::Unsafe(UnsafeArchive::NonUtf8Path {
                    bytes: entry.name_raw().to_owned(),
                }));
            }
        };

        // Reject symlink entries (unix mode bits 0o120000 = 0xA000).
        if let Some(mode) = entry.unix_mode()
            && (mode & 0o170000) == 0o120000
        {
            return Err(IngestError::Unsafe(UnsafeArchive::DisallowedEntry {
                kind: crate::error::EntryKind::Symlink,
                path: PathBuf::from(raw_path),
            }));
        }

        let path =
            extract::jail_path(limits.max_path_depth, &raw_path).map_err(IngestError::Unsafe)?;

        // Directories (names ending in '/') are structural; validate path but emit nothing.
        if raw_path.ends_with('/') {
            continue;
        }

        if !allow.regular {
            return Err(IngestError::Unsafe(UnsafeArchive::DisallowedEntry {
                kind: crate::error::EntryKind::Regular,
                path: PathBuf::from(raw_path),
            }));
        }

        // Read bounded — never trust zip's declared uncompressed size.
        let mut bytes = Vec::new();
        (&mut entry)
            .take(limits.max_file_bytes + 1)
            .read_to_end(&mut bytes)
            .map_err(IngestError::Io)?;

        budget.charge(bytes.len() as u64).map_err(|violation| {
            IngestError::Unsafe(match violation {
                UnsafeArchive::FileTooLarge { actual, limit, .. } => UnsafeArchive::FileTooLarge {
                    path: PathBuf::from(&raw_path),
                    actual,
                    limit,
                },
                other => other,
            })
        })?;

        builder.push_file(path, bytes::Bytes::from(bytes))?;
        admitted += 1;
    }

    tracing::info!(package = %package, files = admitted, "zip archive sanitized and ingested");
    Ok(builder)
}

/// The compression framing of an incoming archive — the ecosystem crate's
/// [`crate::ecosystem::archive::ArchiveKind`], re-exported under the registry's
/// historical name so `spec(lang).archive()` feeds ingest without conversion.
pub use crate::ecosystem::archive::ArchiveKind as ArchiveFormat;

#[cfg(test)]
mod tests {
    use std::io::Write;

    use super::*;
    use crate::error::{EntryKind, UnsafeArchive};

    fn test_package() -> PackageId {
        PackageId::from_uuid(uuid::Uuid::from_u128(0xBEEF_0001))
    }

    fn test_toolchain() -> Toolchain {
        Toolchain::CSharp {
            sdk: semver::Version::new(10, 0, 0),
        }
    }

    /// Build an in-memory zip from `(name, bytes)` pairs (stored, no compression
    /// tricks — bomb tests use deflate explicitly).
    fn build_zip(entries: &[(&str, &[u8])]) -> Vec<u8> {
        let mut writer = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
        let options = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Stored);
        for (name, bytes) in entries {
            writer.start_file(*name, options).unwrap();
            writer.write_all(bytes).unwrap();
        }
        writer.finish().unwrap().into_inner()
    }

    async fn ingest_zip(
        bytes: &[u8],
        limits: ExtractionLimits,
    ) -> Result<BlobBuilder, IngestError> {
        ingest_archive(
            test_package(),
            test_toolchain(),
            bytes,
            ArchiveFormat::Zip,
            limits,
            EntryAllowlist::SAFE,
        )
        .await
    }

    #[tokio::test]
    async fn nominal_nupkg_shaped_zip_ingests_end_to_end() {
        let mut writer = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
        let options = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Stored);
        // A directory entry is structural and must be skipped, not stored.
        writer.add_directory("lib/net8.0", options).unwrap();
        writer.start_file("Package.nuspec", options).unwrap();
        writer.write_all(b"<package/>").unwrap();
        writer
            .start_file("lib/net8.0/Package.dll", options)
            .unwrap();
        writer.write_all(b"MZ\x90\x00").unwrap();
        writer
            .start_file("lib/net8.0/Package.xml", options)
            .unwrap();
        writer.write_all(b"<doc/>").unwrap();
        let bytes = writer.finish().unwrap().into_inner();

        let builder = ingest_zip(&bytes, ExtractionLimits::DEFAULT).await.unwrap();
        let files: std::collections::BTreeMap<_, _> = builder
            .source_files()
            .map(|(path, bytes)| (path.to_string(), bytes.clone()))
            .collect();
        assert_eq!(files.len(), 3, "directory entry must not be materialized");
        assert_eq!(files["Package.nuspec"].as_ref(), b"<package/>");
        assert_eq!(files["lib/net8.0/Package.dll"].as_ref(), b"MZ\x90\x00");
        assert_eq!(files["lib/net8.0/Package.xml"].as_ref(), b"<doc/>");
    }

    #[tokio::test]
    async fn zip_slip_relative_traversal_rejected() {
        let bytes = build_zip(&[("../evil.txt", b"pwned")]);
        let error = ingest_zip(&bytes, ExtractionLimits::DEFAULT)
            .await
            .unwrap_err();
        assert!(
            matches!(
                error,
                IngestError::Unsafe(UnsafeArchive::PathTraversal { .. })
            ),
            "expected PathTraversal, got {error:?}"
        );
    }

    #[tokio::test]
    async fn zip_slip_absolute_path_rejected() {
        let bytes = build_zip(&[("/etc/passwd", b"root:x:0:0")]);
        let error = ingest_zip(&bytes, ExtractionLimits::DEFAULT)
            .await
            .unwrap_err();
        assert!(
            matches!(
                error,
                IngestError::Unsafe(UnsafeArchive::PathTraversal { .. })
            ),
            "expected PathTraversal, got {error:?}"
        );
    }

    #[tokio::test]
    async fn zip_symlink_entry_rejected() {
        let mut writer = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
        let options = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Stored);
        writer.start_file("ok.txt", options).unwrap();
        writer.write_all(b"fine").unwrap();
        writer.add_symlink("link", "/etc/shadow", options).unwrap();
        let bytes = writer.finish().unwrap().into_inner();

        let error = ingest_zip(&bytes, ExtractionLimits::DEFAULT)
            .await
            .unwrap_err();
        assert!(
            matches!(
                error,
                IngestError::Unsafe(UnsafeArchive::DisallowedEntry {
                    kind: EntryKind::Symlink,
                    ..
                })
            ),
            "expected symlink rejection, got {error:?}"
        );
    }

    #[tokio::test]
    async fn zip_bomb_stopped_by_cumulative_budget() {
        // Each file is under the per-file ceiling, but together they breach the
        // total: the budget must stop the walk mid-archive. Deflate makes the
        // *compressed* stream tiny so stage 1's compressed-size cap stays out of
        // the way and the cumulative check is what fires.
        let mut writer = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
        let options = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated);
        let chunk = vec![0u8; 3000];
        for name in ["a.bin", "b.bin", "c.bin"] {
            writer.start_file(name, options).unwrap();
            writer.write_all(&chunk).unwrap();
        }
        let bytes = writer.finish().unwrap().into_inner();

        let limits = ExtractionLimits {
            max_total_bytes: 8_000,
            max_file_bytes: 4_000,
            max_files: 100,
            max_path_depth: 8,
        };
        let error = ingest_zip(&bytes, limits).await.unwrap_err();
        assert!(
            matches!(
                error,
                IngestError::Unsafe(UnsafeArchive::TotalTooLarge { .. })
            ),
            "expected TotalTooLarge, got {error:?}"
        );
    }

    #[tokio::test]
    async fn zip_oversize_single_file_rejected_despite_lying_header() {
        // The declared size is never trusted: the read is clamped and the actual
        // byte count is what gets charged.
        let bytes = build_zip(&[("big.bin", &vec![7u8; 5000][..])]);
        let limits = ExtractionLimits {
            max_total_bytes: 100_000,
            max_file_bytes: 4_000,
            max_files: 100,
            max_path_depth: 8,
        };
        let error = ingest_zip(&bytes, limits).await.unwrap_err();
        assert!(
            matches!(
                error,
                IngestError::Unsafe(UnsafeArchive::FileTooLarge { .. })
            ),
            "expected FileTooLarge, got {error:?}"
        );
    }

    #[tokio::test]
    async fn zip_non_utf8_entry_name_rejected() {
        // The zip writer only accepts &str names, so a raw archive is crafted by
        // hand: one empty stored entry whose name contains 0xFF, UTF-8 flag unset.
        let bytes = raw_zip_with_name(b"bad\xFFname.txt");
        let error = ingest_zip(&bytes, ExtractionLimits::DEFAULT)
            .await
            .unwrap_err();
        assert!(
            matches!(
                error,
                IngestError::Unsafe(UnsafeArchive::NonUtf8Path { .. })
            ),
            "expected NonUtf8Path, got {error:?}"
        );
    }

    /// Minimal single-entry zip (empty stored file, crc 0) with an arbitrary raw
    /// name — the only way to produce a non-UTF-8 name the writer API forbids.
    fn raw_zip_with_name(name: &[u8]) -> Vec<u8> {
        let n = name.len() as u16;
        let mut out = Vec::new();
        // Local file header.
        out.extend_from_slice(&[0x50, 0x4B, 0x03, 0x04]);
        out.extend_from_slice(&20u16.to_le_bytes()); // version needed
        out.extend_from_slice(&0u16.to_le_bytes()); // gp flags (UTF-8 bit unset)
        out.extend_from_slice(&0u16.to_le_bytes()); // method: stored
        out.extend_from_slice(&0u32.to_le_bytes()); // dos time+date
        out.extend_from_slice(&0u32.to_le_bytes()); // crc32 (empty)
        out.extend_from_slice(&0u32.to_le_bytes()); // compressed size
        out.extend_from_slice(&0u32.to_le_bytes()); // uncompressed size
        out.extend_from_slice(&n.to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes()); // extra len
        out.extend_from_slice(name);
        let central_offset = out.len() as u32;
        // Central directory header.
        out.extend_from_slice(&[0x50, 0x4B, 0x01, 0x02]);
        out.extend_from_slice(&20u16.to_le_bytes()); // version made by (DOS)
        out.extend_from_slice(&20u16.to_le_bytes()); // version needed
        out.extend_from_slice(&0u16.to_le_bytes()); // gp flags
        out.extend_from_slice(&0u16.to_le_bytes()); // method
        out.extend_from_slice(&0u32.to_le_bytes()); // dos time+date
        out.extend_from_slice(&0u32.to_le_bytes()); // crc32
        out.extend_from_slice(&0u32.to_le_bytes()); // compressed size
        out.extend_from_slice(&0u32.to_le_bytes()); // uncompressed size
        out.extend_from_slice(&n.to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes()); // extra len
        out.extend_from_slice(&0u16.to_le_bytes()); // comment len
        out.extend_from_slice(&0u16.to_le_bytes()); // disk number
        out.extend_from_slice(&0u16.to_le_bytes()); // internal attrs
        out.extend_from_slice(&0u32.to_le_bytes()); // external attrs
        out.extend_from_slice(&0u32.to_le_bytes()); // local header offset
        out.extend_from_slice(name);
        let central_size = out.len() as u32 - central_offset;
        // End of central directory.
        out.extend_from_slice(&[0x50, 0x4B, 0x05, 0x06]);
        out.extend_from_slice(&0u16.to_le_bytes()); // this disk
        out.extend_from_slice(&0u16.to_le_bytes()); // cd start disk
        out.extend_from_slice(&1u16.to_le_bytes()); // entries this disk
        out.extend_from_slice(&1u16.to_le_bytes()); // entries total
        out.extend_from_slice(&central_size.to_le_bytes());
        out.extend_from_slice(&central_offset.to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes()); // comment len
        out
    }
}
