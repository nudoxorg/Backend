//! Streaming [`ObjectPackBuilder`] — accumulates members then seals to a
//! deterministic NDPK v1 pack.
//!
//! # Determinism guarantee
//!
//! The same set of (key, bytes) pairs — regardless of insertion order — always
//! produces byte-identical output and thus the same [`ObjectPackId`]. This is
//! ensured by:
//!
//! - Storage in a [`BTreeMap`] (canonical ascending key order).
//! - A fixed zstd compression level ([`ZSTD_COMPRESSION_LEVEL`]).
//! - A fixed chunk size ([`SOURCE_CHUNK_SIZE_BYTES`]).
//! - A stable, length-prefixed binary codec (postcard) for the TOC.
//! - No timestamps or host-specific bytes anywhere.
//!
//! [`ObjectPackId`]: heart::object_pack::ObjectPackId

use std::collections::BTreeMap;

use bytes::Bytes;
use heart::content::ContentHash;
use heart::object_pack::{
    BAO_OUTBOARD_THRESHOLD_BYTES, ByteLength, ChunkEntry, MemberKey, MemberRecord, ObjectPackId,
    PackOffset, RelativePath, SOURCE_CHUNK_SIZE_BYTES,
};

use crate::pack::error::{PackError, PackResult};
use crate::pack::format::{
    HEADER_LENGTH_BYTES, PackHeader, PackPolicy, TableOfContents, derive_object_pack_id, flags,
};
use crate::pack::outboard::MemberOutboard;
use crate::pack::{NDPK_VERSION_CURRENT, ZSTD_COMPRESSION_LEVEL};

// ---------------------------------------------------------------------------
// Path validation helpers
// ---------------------------------------------------------------------------

/// Validate a [`RelativePath`] for safety before accepting it as a member key.
///
/// Rejects:
/// - Paths with a leading `/` (Unix-absolute).
/// - Paths with a Windows drive letter prefix (`C:`) or UNC prefix (`\\`).
/// - Any path segment that is empty, `.`, or `..`.
/// - Any character that is `\` (backslash) or `\0` (NUL).
fn validate_relative_path(path: &RelativePath) -> PackResult<()> {
    let raw: &str = path.0.as_str();

    // Reject NUL bytes before any other check — they can hide in any position.
    if raw.contains('\0') {
        return Err(PackError::UnsafePathSegment {
            path: raw.to_owned(),
            reason: "NUL byte in path".to_owned(),
        });
    }

    // Reject backslashes — they are a Windows path separator and can cause
    // ambiguity between platforms.
    if raw.contains('\\') {
        return Err(PackError::UnsafePathSegment {
            path: raw.to_owned(),
            reason: "backslash in path".to_owned(),
        });
    }

    // Reject Unix-absolute paths.
    if raw.starts_with('/') {
        return Err(PackError::AbsolutePath {
            path: raw.to_owned(),
        });
    }

    // Reject Windows drive letters (e.g. `C:`, `c:`) and UNC paths (`\\` was
    // caught above but include the check for clarity).
    {
        let mut chars = raw.chars();
        let first = chars.next();
        let second = chars.next();
        if let (Some(drive), Some(':')) = (first, second)
            && drive.is_ascii_alphabetic()
        {
            return Err(PackError::AbsolutePath {
                path: raw.to_owned(),
            });
        }
    }

    // Validate every `/`-separated segment.
    for segment in raw.split('/') {
        if segment.is_empty() {
            return Err(PackError::UnsafePathSegment {
                path: raw.to_owned(),
                reason: "empty path segment (double slash or trailing slash)".to_owned(),
            });
        }
        if segment == "." || segment == ".." {
            return Err(PackError::UnsafePathSegment {
                path: raw.to_owned(),
                reason: format!("`.` / `..` segment not permitted: {segment:?}"),
            });
        }
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// ObjectPackBuilder
// ---------------------------------------------------------------------------

/// Accumulates members and seals them into a deterministic NDPK v1 pack.
///
/// Members are stored internally in a [`BTreeMap`] so their order in the output
/// is always the canonical ascending [`MemberKey`] order, independent of the
/// order they were inserted.
///
/// After all members have been added, call [`seal_to_bytes`] or
/// [`seal_to_writer`] to produce the final pack.
///
/// [`seal_to_bytes`]: ObjectPackBuilder::seal_to_bytes
/// [`seal_to_writer`]: ObjectPackBuilder::seal_to_writer
#[derive(Debug, Default)]
pub struct ObjectPackBuilder {
    /// Pending members, keyed in canonical ascending order.
    pending: BTreeMap<MemberKey, Bytes>,
}

impl ObjectPackBuilder {
    /// Create an empty builder.
    pub fn new() -> Self {
        Self {
            pending: BTreeMap::new(),
        }
    }

    // -----------------------------------------------------------------------
    // Mutation
    // -----------------------------------------------------------------------

    /// Add a member to the builder.
    ///
    /// - `key`: the member's identity in the TOC.
    /// - `uncompressed`: the raw, uncompressed bytes for this member.
    ///
    /// # Errors
    ///
    /// - [`PackError::DuplicateMember`] if a member with the same key was
    ///   already added.
    /// - [`PackError::AbsolutePath`] or [`PackError::UnsafePathSegment`] if
    ///   `key` is a [`MemberKey::Source`] with an invalid path.
    pub fn add_member(&mut self, key: MemberKey, uncompressed: Bytes) -> Result<(), PackError> {
        // Path validation for Source members.
        if let MemberKey::Source { path } = &key {
            validate_relative_path(path)?;
        }

        // Duplicate detection.
        if self.pending.contains_key(&key) {
            return Err(PackError::DuplicateMember { key });
        }

        self.pending.insert(key, uncompressed);
        Ok(())
    }

    /// Convenience wrapper: build a [`MemberKey::Source`] from `path` and
    /// delegate to [`add_member`].
    ///
    /// [`add_member`]: ObjectPackBuilder::add_member
    pub fn add_source_file(
        &mut self,
        path: RelativePath,
        contents: Bytes,
    ) -> Result<(), PackError> {
        let key = MemberKey::Source { path };
        self.add_member(key, contents)
    }

    // -----------------------------------------------------------------------
    // Introspection
    // -----------------------------------------------------------------------

    /// The number of members currently pending in this builder.
    pub fn member_count(&self) -> usize {
        self.pending.len()
    }

    /// Returns `true` if no members have been added yet.
    pub fn is_empty(&self) -> bool {
        self.pending.is_empty()
    }

    // -----------------------------------------------------------------------
    // Sealing
    // -----------------------------------------------------------------------

    /// Seal all pending members into NDPK v1 bytes and return them together
    /// with the pack's [`ObjectPackId`].
    ///
    /// The produced bytes are byte-identical for the same set of members,
    /// regardless of insertion order.
    pub fn seal_to_bytes(self) -> Result<(Bytes, ObjectPackId), PackError> {
        let (vec, id, _outboards) = self.build()?;
        Ok((Bytes::from(vec), id))
    }

    /// Seal all pending members and additionally return the Bao outboards for
    /// every member at or above [`heart::object_pack::BAO_OUTBOARD_THRESHOLD_BYTES`]
    /// (INDEX-PLAN §6.2). Sub-threshold members yield no outboard.
    ///
    /// The pack bytes and id are identical to [`Self::seal_to_bytes`]; the
    /// outboards are computed over each member's uncompressed bytes and are
    /// meant to be stored as sidecars beside the pack (never inside it).
    pub fn seal_with_outboards(
        self,
    ) -> Result<(Bytes, ObjectPackId, Vec<MemberOutboard>), PackError> {
        let (vec, id, outboards) = self.build()?;
        Ok((Bytes::from(vec), id, outboards))
    }

    /// Seal and write all bytes to `writer`.
    ///
    /// Returns only the [`ObjectPackId`]; the bytes are written to `writer`.
    pub fn seal_to_writer<W: std::io::Write>(
        self,
        writer: &mut W,
    ) -> Result<ObjectPackId, PackError> {
        let (vec, id, _outboards) = self.build()?;
        writer.write_all(&vec)?;
        Ok(id)
    }

    // -----------------------------------------------------------------------
    // Private seal algorithm
    // -----------------------------------------------------------------------

    /// Core deterministic build routine shared by both public seal entry points.
    ///
    /// See the module-level doc for the byte layout.
    fn build(self) -> PackResult<(Vec<u8>, ObjectPackId, Vec<MemberOutboard>)> {
        // Pre-allocate: header placeholder + rough estimate.
        let mut file: Vec<u8> = Vec::new();

        // Bao outboards for large members, collected as we walk (INDEX-PLAN §6.2).
        let mut outboards: Vec<MemberOutboard> = Vec::new();

        // Step 1 — Reserve the 24-byte header region with a placeholder.
        file.extend_from_slice(&[0u8; HEADER_LENGTH_BYTES]);

        // Running file-length cursor.  All offsets are relative to file start.
        // We track this separately (rather than calling `file.len()`) so every
        // cast from usize → u64 is done once in a checked way here.
        let mut cursor: u64 = HEADER_LENGTH_BYTES as u64;

        // Step 2 — Iterate members in BTreeMap order and write compressed chunks.
        let mut records: Vec<MemberRecord> = Vec::with_capacity(self.pending.len());

        for (key, uncompressed_bytes) in &self.pending {
            let uncompressed_total_len = uncompressed_bytes.len();

            // Chunk the uncompressed bytes.  Even an empty member produces exactly
            // one (zero-length) chunk so that `chunks` is never empty.
            let chunks_data: Vec<&[u8]> = if uncompressed_bytes.is_empty() {
                vec![&[][..]]
            } else {
                let chunk_size = SOURCE_CHUNK_SIZE_BYTES as usize;
                uncompressed_bytes.chunks(chunk_size).collect()
            };

            let member_start_offset = PackOffset(cursor);
            let mut chunk_entries: Vec<ChunkEntry> = Vec::with_capacity(chunks_data.len());
            let mut total_compressed_len: u64 = 0;

            for raw_chunk in chunks_data {
                // Compress the chunk into an independent zstd frame.
                let compressed_frame = zstd::bulk::compress(raw_chunk, ZSTD_COMPRESSION_LEVEL)
                    .map_err(PackError::FrameEncode)?;

                let compressed_frame_len = compressed_frame.len();

                // Checked arithmetic: frame length must fit in u64.
                let compressed_frame_len_u64: u64 =
                    compressed_frame_len
                        .try_into()
                        .map_err(|_| PackError::TooLarge {
                            detail: format!(
                                "compressed frame length {compressed_frame_len} overflows u64"
                            ),
                        })?;

                let chunk_entry = ChunkEntry {
                    frame_offset: PackOffset(cursor),
                    compressed_length: ByteLength(compressed_frame_len_u64),
                    uncompressed_length: ByteLength(raw_chunk.len() as u64),
                };
                chunk_entries.push(chunk_entry);

                // Advance cursor.
                cursor = cursor
                    .checked_add(compressed_frame_len_u64)
                    .ok_or_else(|| PackError::TooLarge {
                        detail: "file offset overflowed u64 while appending chunk frame".to_owned(),
                    })?;

                // Accumulate compressed total.
                total_compressed_len = total_compressed_len
                    .checked_add(compressed_frame_len_u64)
                    .ok_or_else(|| PackError::TooLarge {
                    detail: "total compressed length overflowed u64".to_owned(),
                })?;

                file.extend_from_slice(&compressed_frame);
            }

            // Whole-member BLAKE3: hash over the original uncompressed bytes.
            let content_hash: ContentHash = ContentHash::of_bytes(uncompressed_bytes);

            let uncompressed_total_len_u64: u64 =
                uncompressed_total_len
                    .try_into()
                    .map_err(|_| PackError::TooLarge {
                        detail: format!(
                            "uncompressed member length {uncompressed_total_len} overflows u64"
                        ),
                    })?;

            // Generate a Bao outboard for verified range streaming (INDEX-PLAN
            // §6.2) when the member is at or above the threshold. The outboard
            // is computed over the *uncompressed* bytes (see outboard.rs for the
            // rationale) and its root hash equals `content_hash` above.
            let needs_bao_outboard: bool =
                uncompressed_total_len_u64 >= BAO_OUTBOARD_THRESHOLD_BYTES;
            if needs_bao_outboard {
                outboards.push(MemberOutboard::generate(key.clone(), uncompressed_bytes));
            }

            let record = MemberRecord {
                key: key.clone(),
                offset: member_start_offset,
                compressed_length: ByteLength(total_compressed_len),
                uncompressed_length: ByteLength(uncompressed_total_len_u64),
                content: content_hash,
                chunks: chunk_entries,
                bao_outboard: needs_bao_outboard,
            };
            records.push(record);
        }

        // Step 3 — Append the TOC.
        let toc_offset: u64 = cursor;

        let policy = PackPolicy {
            version: NDPK_VERSION_CURRENT,
            zstd_compression_level: ZSTD_COMPRESSION_LEVEL,
            source_chunk_size_bytes: SOURCE_CHUNK_SIZE_BYTES,
        };

        let toc = TableOfContents { policy, records };
        let toc_bytes: Vec<u8> = toc.encode()?;

        let toc_length_usize = toc_bytes.len();
        let toc_length: u64 = toc_length_usize
            .try_into()
            .map_err(|_| PackError::TooLarge {
                detail: format!("TOC length {toc_length_usize} overflows u64"),
            })?;

        file.extend_from_slice(&toc_bytes);

        // Step 4 — Derive the ObjectPackId from policy + TOC bytes.
        let pack_id: ObjectPackId = derive_object_pack_id(&policy, &toc_bytes);

        // Step 5 — Backfill the header placeholder at offset 0..24.
        let header = PackHeader {
            version: NDPK_VERSION_CURRENT,
            flags: flags::NONE,
            toc_offset,
            toc_length,
        };
        let encoded_header: [u8; HEADER_LENGTH_BYTES] = header.encode();
        file[0..HEADER_LENGTH_BYTES].copy_from_slice(&encoded_header);

        Ok((file, pack_id, outboards))
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use smol_str::SmolStr;

    fn source_key(path: &str) -> MemberKey {
        MemberKey::Source {
            path: RelativePath(SmolStr::new(path)),
        }
    }

    fn relative_path(path: &str) -> RelativePath {
        RelativePath(SmolStr::new(path))
    }

    /// The same set of members inserted in two different orders must produce
    /// byte-identical pack bytes and the same [`ObjectPackId`].
    #[test]
    fn determinism_insertion_order_independent() {
        let mut builder_a = ObjectPackBuilder::new();
        builder_a
            .add_member(
                source_key("alpha/one.txt"),
                Bytes::from_static(b"hello world"),
            )
            .unwrap();
        builder_a
            .add_member(
                source_key("beta/two.txt"),
                Bytes::from_static(b"goodbye world"),
            )
            .unwrap();

        let mut builder_b = ObjectPackBuilder::new();
        builder_b
            .add_member(
                source_key("beta/two.txt"),
                Bytes::from_static(b"goodbye world"),
            )
            .unwrap();
        builder_b
            .add_member(
                source_key("alpha/one.txt"),
                Bytes::from_static(b"hello world"),
            )
            .unwrap();

        let (bytes_a, id_a) = builder_a.seal_to_bytes().unwrap();
        let (bytes_b, id_b) = builder_b.seal_to_bytes().unwrap();

        assert_eq!(id_a, id_b, "ObjectPackId must be order-independent");
        assert_eq!(bytes_a, bytes_b, "pack bytes must be byte-identical");
    }

    /// Inserting the same key twice must yield [`PackError::DuplicateMember`].
    #[test]
    fn duplicate_key_is_rejected() {
        let mut builder = ObjectPackBuilder::new();
        builder
            .add_member(
                source_key("src/lib.rs"),
                Bytes::from_static(b"fn main() {}"),
            )
            .unwrap();

        let result = builder
            .add_member(
                source_key("src/lib.rs"),
                Bytes::from_static(b"fn main() {}"),
            )
            .unwrap_err();

        assert!(
            matches!(result, PackError::DuplicateMember { .. }),
            "expected DuplicateMember, got {result:?}"
        );
    }

    /// A path starting with `/` must be rejected as absolute.
    #[test]
    fn absolute_path_unix_is_rejected() {
        let mut builder = ObjectPackBuilder::new();
        let result = builder
            .add_source_file(relative_path("/etc/passwd"), Bytes::from_static(b"root"))
            .unwrap_err();

        assert!(
            matches!(result, PackError::AbsolutePath { .. }),
            "expected AbsolutePath, got {result:?}"
        );
    }

    /// A Windows drive-letter path must be rejected.
    #[test]
    fn absolute_path_windows_drive_is_rejected() {
        let mut builder = ObjectPackBuilder::new();
        let result = builder
            .add_source_file(relative_path("C:evil"), Bytes::from_static(b"bad"))
            .unwrap_err();

        assert!(
            matches!(result, PackError::AbsolutePath { .. }),
            "expected AbsolutePath, got {result:?}"
        );
    }

    /// A `..` segment must be rejected.
    #[test]
    fn dotdot_segment_is_rejected() {
        let mut builder = ObjectPackBuilder::new();
        let result = builder
            .add_source_file(
                relative_path("src/../../../etc/passwd"),
                Bytes::from_static(b"escape attempt"),
            )
            .unwrap_err();

        assert!(
            matches!(result, PackError::UnsafePathSegment { .. }),
            "expected UnsafePathSegment, got {result:?}"
        );
    }

    /// A bare `.` segment must be rejected.
    #[test]
    fn dot_segment_is_rejected() {
        let mut builder = ObjectPackBuilder::new();
        let result = builder
            .add_source_file(
                relative_path("src/./main.rs"),
                Bytes::from_static(b"dot segment"),
            )
            .unwrap_err();

        assert!(
            matches!(result, PackError::UnsafePathSegment { .. }),
            "expected UnsafePathSegment, got {result:?}"
        );
    }

    /// A backslash in the path must be rejected.
    #[test]
    fn backslash_in_path_is_rejected() {
        let mut builder = ObjectPackBuilder::new();
        let result = builder
            .add_source_file(
                relative_path("src\\main.rs"),
                Bytes::from_static(b"backslash"),
            )
            .unwrap_err();

        assert!(
            matches!(result, PackError::UnsafePathSegment { .. }),
            "expected UnsafePathSegment, got {result:?}"
        );
    }

    /// An empty pack (no members) must seal without error and produce a valid
    /// 24-byte header at the start.
    #[test]
    fn empty_pack_seals_with_valid_header() {
        use crate::pack::format::PackHeader;

        let builder = ObjectPackBuilder::new();
        let (bytes, _id) = builder.seal_to_bytes().unwrap();

        // Header must be decodable and sane.
        let header = PackHeader::decode(&bytes).unwrap();
        assert_eq!(header.version, NDPK_VERSION_CURRENT);
        assert_eq!(header.flags, flags::NONE);
        // TOC immediately follows the header because there are no member frames.
        assert_eq!(header.toc_offset, HEADER_LENGTH_BYTES as u64);
    }

    /// A member with empty bytes (zero-length) must seal without error and
    /// produce a single chunk entry.
    #[test]
    fn zero_length_member_produces_one_chunk() {
        let mut builder = ObjectPackBuilder::new();
        builder
            .add_member(source_key("empty.txt"), Bytes::new())
            .unwrap();

        let (bytes, _id) = builder.seal_to_bytes().unwrap();
        assert!(
            !bytes.is_empty(),
            "pack must have bytes even for empty member"
        );
    }

    /// `seal_to_writer` must produce the same bytes as `seal_to_bytes`.
    #[test]
    fn seal_to_writer_matches_seal_to_bytes() {
        let make_builder = || {
            let mut builder = ObjectPackBuilder::new();
            builder
                .add_source_file(
                    relative_path("src/main.rs"),
                    Bytes::from_static(b"fn main() { println!(\"hello\"); }"),
                )
                .unwrap();
            builder
        };

        let (expected_bytes, expected_id) = make_builder().seal_to_bytes().unwrap();

        let mut writer_output: Vec<u8> = Vec::new();
        let writer_id = make_builder().seal_to_writer(&mut writer_output).unwrap();

        assert_eq!(expected_id, writer_id);
        assert_eq!(expected_bytes.as_ref(), writer_output.as_slice());
    }
}
