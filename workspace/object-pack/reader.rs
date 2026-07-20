//! NDPK v1 container reader.
//!
//! Provides [`ObjectPackReader`], which can be opened from a filesystem path
//! (mmap-backed, zero-copy) or from an owned [`bytes::Bytes`] buffer
//! (in-memory, useful for tests and in-process transfers).
//!
//! # Adversarial input
//!
//! Every public method returns [`PackError`] on malformed or truncated input.
//! No path panics; no `unwrap` is used in non-test code.
//!
//! # Range reads
//!
//! [`ObjectPackReader::get_member_range`] decompresses only the chunks that
//! overlap the requested byte range, keeping peak memory low for snippet reads.

use std::ops::Range;
use std::path::Path;

use bytes::Bytes;
use memmap2::Mmap;

use heart::content::ContentHash;
use heart::object_pack::{ChunkEntry, MemberKey, MemberRecord, ObjectPackId};

use crate::error::{PackError, PackResult};
use crate::format::{
    derive_object_pack_id, PackHeader, TableOfContents, HEADER_LENGTH_BYTES,
};

// ---------------------------------------------------------------------------
// Backing store
// ---------------------------------------------------------------------------

/// The byte source backing an [`ObjectPackReader`].
///
/// Either a memory-mapped file or an owned [`Bytes`] buffer.
#[derive(Debug)]
enum Backing {
    /// A memory-mapped file opened for reading.
    Mapped(Mmap),
    /// An owned in-memory buffer.
    Owned(Bytes),
}

impl Backing {
    /// Return a read-only view of all backing bytes.
    fn as_slice(&self) -> &[u8] {
        match self {
            Backing::Mapped(mapping) => &mapping[..],
            Backing::Owned(buffer) => &buffer[..],
        }
    }
}

// ---------------------------------------------------------------------------
// Reader
// ---------------------------------------------------------------------------

/// A parsed, ready-to-query NDPK v1 container reader.
///
/// Holds the decoded header and table of contents in memory; member bytes are
/// read (and decompressed) on demand from the backing store.
///
/// Construct via [`ObjectPackReader::open_path`] (mmap) or
/// [`ObjectPackReader::open_bytes`] (owned buffer).
#[derive(Debug)]
pub struct ObjectPackReader {
    /// The raw byte source (mmap or owned buffer).
    backing: Backing,
    /// The parsed fixed 24-byte header.
    header: PackHeader,
    /// The parsed and ordering-verified table of contents.
    toc: TableOfContents,
    /// The pack identity derived from TOC bytes + policy.
    id: ObjectPackId,
}

impl ObjectPackReader {
    // -----------------------------------------------------------------------
    // Constructors
    // -----------------------------------------------------------------------

    /// Open a pack file from `path` using a memory map.
    ///
    /// The file must not be mutated concurrently while the reader is live.
    /// The mmap is read-only; the operating system may lazily page-in content,
    /// so reads that decompress chunks never load the whole file up front.
    ///
    /// # Errors
    ///
    /// Returns [`PackError::Io`] if the file cannot be opened or mapped, or
    /// any structural [`PackError`] variant if the file is malformed.
    pub fn open_path(path: &Path) -> PackResult<Self> {
        let file = std::fs::File::open(path)?;
        // SAFETY: The caller must not mutate or truncate the file while this
        // Mmap is live. We open in read-only mode and hold no write handles.
        // On all supported platforms a concurrent writer does not cause UB for
        // a read-only mmap; it may produce corrupt-but-typed errors instead,
        // which the parser catches deterministically.
        let mapping = unsafe { Mmap::map(&file) }.map_err(PackError::Io)?;
        Self::parse(Backing::Mapped(mapping))
    }

    /// Parse a pack from an owned in-memory [`Bytes`] buffer.
    ///
    /// Useful for tests, in-process transfers, and caches that have already
    /// loaded the pack.
    ///
    /// # Errors
    ///
    /// Returns a structural [`PackError`] variant if `bytes` is malformed.
    pub fn open_bytes(bytes: Bytes) -> PackResult<Self> {
        Self::parse(Backing::Owned(bytes))
    }

    // -----------------------------------------------------------------------
    // Private parse core
    // -----------------------------------------------------------------------

    /// Shared parse path used by both constructors.
    ///
    /// 1. Decodes the fixed header (magic, version, flags, offsets).
    /// 2. Validates TOC region bounds with overflow-safe arithmetic.
    /// 3. Decodes and sort-verifies the TOC.
    /// 4. Derives the [`ObjectPackId`] from the TOC bytes + policy.
    fn parse(backing: Backing) -> PackResult<Self> {
        let slice = backing.as_slice();

        // Step 1: decode header.
        let header = PackHeader::decode(slice)?;

        let toc_offset = header.toc_offset as usize;
        let toc_length = header.toc_length as usize;

        // Step 2: validate TOC region bounds.
        //
        // The offset must be at or past the fixed header (an offset inside the
        // header bytes would be structurally impossible in a well-formed pack).
        if toc_offset < HEADER_LENGTH_BYTES {
            return Err(PackError::BadStructure {
                detail: format!(
                    "toc_offset {toc_offset} is within the fixed header \
                     ({HEADER_LENGTH_BYTES} bytes)"
                ),
            });
        }

        // The offset itself must be in bounds.
        if toc_offset > slice.len() {
            return Err(PackError::BadStructure {
                detail: format!(
                    "toc_offset {toc_offset} is past end of pack ({} bytes)",
                    slice.len()
                ),
            });
        }

        // The offset + length must not overflow or exceed the slice.
        let toc_end = toc_offset.checked_add(toc_length).ok_or_else(|| {
            PackError::BadStructure {
                detail: format!(
                    "toc_offset {toc_offset} + toc_length {toc_length} overflows usize"
                ),
            }
        })?;

        if toc_end > slice.len() {
            return Err(PackError::Truncated {
                offset: toc_offset as u64,
                needed: toc_length as u64,
                available: (slice.len() - toc_offset) as u64,
            });
        }

        // A canonical pack ends exactly where the TOC ends: member frames
        // precede the TOC and nothing follows it. Trailing bytes would let a
        // tampered file alias the id of a clean pack (the id commits only to
        // the TOC), so reject them outright.
        if toc_end != slice.len() {
            return Err(PackError::BadStructure {
                detail: format!(
                    "{} trailing bytes after the table of contents",
                    slice.len() - toc_end
                ),
            });
        }

        // Step 3: decode TOC (postcard + sort verification).
        let toc_bytes = &slice[toc_offset..toc_end];
        let toc = TableOfContents::decode(toc_bytes)?;

        // Step 4: derive the pack id from the on-disk TOC bytes.
        //
        // A tampered TOC that still postcard-decodes will produce a *different*
        // id from what was announced; callers can detect this with
        // `verify_table_of_contents`. A TOC that fails to decode never reaches
        // this point.
        let id = derive_object_pack_id(&toc.policy, toc_bytes);

        Ok(Self { backing, header, toc, id })
    }

    // -----------------------------------------------------------------------
    // Accessors
    // -----------------------------------------------------------------------

    /// The pack's derived identity (BLAKE3 over TOC + policy).
    pub fn id(&self) -> ObjectPackId {
        self.id
    }

    /// The parsed fixed header (version, flags, TOC pointer).
    pub fn header(&self) -> &PackHeader {
        &self.header
    }

    /// Iterate over every member record in TOC order (strictly ascending key).
    pub fn members(&self) -> impl Iterator<Item = &MemberRecord> {
        self.toc.records.iter()
    }

    /// The number of members registered in this pack.
    pub fn member_count(&self) -> usize {
        self.toc.records.len()
    }

    /// Look up a member record by key; returns `None` if absent.
    pub fn find_member(&self, key: &MemberKey) -> Option<&MemberRecord> {
        self.toc.find(key)
    }

    // -----------------------------------------------------------------------
    // Private helpers
    // -----------------------------------------------------------------------

    /// The raw on-disk TOC bytes (the slice the id was derived from).
    fn toc_bytes(&self) -> &[u8] {
        let offset = self.header.toc_offset as usize;
        let length = self.header.toc_length as usize;
        // Bounds were validated in `parse`; this slice is always safe.
        &self.backing.as_slice()[offset..offset + length]
    }

    /// Decompress a single chunk entry, returning the uncompressed bytes.
    ///
    /// Bounds-checks the compressed frame region before any I/O and verifies
    /// that the decompressed length matches the TOC expectation.
    fn decompress_chunk(
        &self,
        chunk: &ChunkEntry,
    ) -> PackResult<Vec<u8>> {
        let slice = self.backing.as_slice();

        let frame_offset = chunk.frame_offset.get() as usize;
        let compressed_length = chunk.compressed_length.get() as usize;

        // Overflow-safe end bound.
        let frame_end = frame_offset.checked_add(compressed_length).ok_or_else(|| {
            PackError::BadStructure {
                detail: format!(
                    "chunk frame_offset {frame_offset} + compressed_length \
                     {compressed_length} overflows usize"
                ),
            }
        })?;

        if frame_end > slice.len() {
            return Err(PackError::Truncated {
                offset: frame_offset as u64,
                needed: compressed_length as u64,
                available: (slice.len().saturating_sub(frame_offset)) as u64,
            });
        }

        let frame_bytes = &slice[frame_offset..frame_end];
        let expected_uncompressed = chunk.uncompressed_length.get() as usize;

        let decompressed = zstd::bulk::decompress(frame_bytes, expected_uncompressed)
            .map_err(|error| PackError::FrameDecode {
                detail: format!("zstd error at offset {frame_offset}: {error}"),
            })?;

        if decompressed.len() != expected_uncompressed {
            return Err(PackError::FrameDecode {
                detail: format!(
                    "chunk length mismatch: expected {expected_uncompressed} \
                     uncompressed bytes, got {}",
                    decompressed.len()
                ),
            });
        }

        Ok(decompressed)
    }

    // -----------------------------------------------------------------------
    // Member reads
    // -----------------------------------------------------------------------

    /// Decompress and return all bytes for the member identified by `key`.
    ///
    /// Decompresses every chunk in ascending order and concatenates them into
    /// a single [`Bytes`] allocation. For large members prefer
    /// [`ObjectPackReader::get_member_range`].
    ///
    /// # Errors
    ///
    /// - [`PackError::MemberNotFound`] — key is not in this pack.
    /// - [`PackError::Truncated`] / [`PackError::FrameDecode`] — corrupt data.
    pub fn get_member(&self, key: &MemberKey) -> PackResult<Bytes> {
        let record = self
            .toc
            .find(key)
            .ok_or_else(|| PackError::MemberNotFound { key: key.clone() })?;

        let total_uncompressed = record.uncompressed_length.get() as usize;
        let mut buffer: Vec<u8> = Vec::with_capacity(total_uncompressed);

        for chunk in &record.chunks {
            let chunk_bytes = self.decompress_chunk(chunk)?;
            buffer.extend_from_slice(&chunk_bytes);
        }

        Ok(Bytes::from(buffer))
    }

    /// Decompress and return a sub-range of a member's uncompressed bytes.
    ///
    /// Only the chunks that overlap `byte_range` are decompressed; chunks
    /// outside the range are skipped entirely. This makes snippet reads — the
    /// primary use-case — efficient for large source files.
    ///
    /// `byte_range` is a half-open interval `[start, end)` over the member's
    /// **uncompressed** bytes.
    ///
    /// # Errors
    ///
    /// - [`PackError::MemberNotFound`] — key absent.
    /// - [`PackError::RangeOutOfBounds`] — `start > end` or `end > member length`.
    /// - [`PackError::Truncated`] / [`PackError::FrameDecode`] — corrupt frame.
    pub fn get_member_range(
        &self,
        key: &MemberKey,
        byte_range: Range<u64>,
    ) -> PackResult<Bytes> {
        let record = self
            .toc
            .find(key)
            .ok_or_else(|| PackError::MemberNotFound { key: key.clone() })?;

        let total = record.uncompressed_length.get();

        // Validate the range.
        if byte_range.start > byte_range.end {
            return Err(PackError::RangeOutOfBounds {
                start: byte_range.start,
                end: byte_range.end,
                member_length: total,
            });
        }
        if byte_range.end > total {
            return Err(PackError::RangeOutOfBounds {
                start: byte_range.start,
                end: byte_range.end,
                member_length: total,
            });
        }

        // Empty range: nothing to decompress.
        if byte_range.start == byte_range.end {
            return Ok(Bytes::new());
        }

        let requested_start = byte_range.start;
        let requested_end = byte_range.end;
        let output_length = (requested_end - requested_start) as usize;
        let mut output: Vec<u8> = Vec::with_capacity(output_length);

        // Walk chunks, tracking the running uncompressed byte offset.
        let mut covered: u64 = 0;

        for chunk in &record.chunks {
            let chunk_uncompressed_length = chunk.uncompressed_length.get();
            let chunk_end = covered
                .checked_add(chunk_uncompressed_length)
                .ok_or_else(|| PackError::BadStructure {
                    detail: format!(
                        "chunk uncompressed offset overflow at covered={covered}"
                    ),
                })?;

            // Check whether this chunk overlaps [requested_start, requested_end).
            //
            // Chunk covers [covered, chunk_end).
            // Intersection: [max(start, covered), min(end, chunk_end)).
            // Non-empty when max(start, covered) < min(end, chunk_end).
            let intersection_start = requested_start.max(covered);
            let intersection_end = requested_end.min(chunk_end);

            if intersection_start < intersection_end {
                // This chunk overlaps the requested range — decompress it.
                let chunk_bytes = self.decompress_chunk(chunk)?;

                // Translate the intersection into local (within-chunk) offsets.
                let local_start = (intersection_start - covered) as usize;
                let local_end = (intersection_end - covered) as usize;

                output.extend_from_slice(&chunk_bytes[local_start..local_end]);
            }

            covered = chunk_end;

            // Early exit once we've passed the end of the requested range.
            if covered >= requested_end {
                break;
            }
        }

        Ok(Bytes::from(output))
    }

    // -----------------------------------------------------------------------
    // Integrity verification
    // -----------------------------------------------------------------------

    /// Verify the content hash of a single member.
    ///
    /// Decompresses all chunks, hashes the concatenated uncompressed bytes, and
    /// compares against the BLAKE3 digest recorded in the TOC.
    ///
    /// # Errors
    ///
    /// - [`PackError::MemberNotFound`] — key absent.
    /// - [`PackError::MemberHashMismatch`] — content does not match the TOC digest.
    /// - [`PackError::FrameDecode`] / [`PackError::Truncated`] — corrupt frame.
    pub fn verify_member(&self, key: &MemberKey) -> PackResult<()> {
        let record = self
            .toc
            .find(key)
            .ok_or_else(|| PackError::MemberNotFound { key: key.clone() })?;

        let expected_hash = record.content;
        let content = self.get_member(key)?;
        let actual_hash = ContentHash::of_bytes(&content);

        if actual_hash != expected_hash {
            return Err(PackError::MemberHashMismatch { key: key.clone() });
        }

        Ok(())
    }

    /// Verify the structural integrity of the table of contents.
    ///
    /// Re-encodes the in-memory [`TableOfContents`] to its canonical postcard
    /// form and compares it byte-for-byte against the on-disk TOC region. If
    /// the two differ, the on-disk bytes are non-canonical or have been tampered
    /// with, and [`PackError::TocHashMismatch`] is returned.
    ///
    /// # Semantics
    ///
    /// The id (an [`ObjectPackId`]) was derived from the on-disk TOC bytes at
    /// parse time, so the id always commits to what is on disk. The meaningful
    /// question is whether the on-disk TOC bytes are the *canonical* encoding of
    /// what was decoded — i.e., whether re-encoding the decoded value reproduces
    /// the exact same bytes. Any tampered or non-canonical byte sequence that
    /// still postcard-decodes successfully will fail this check because postcard
    /// is uniquely-encodable for a given value, so the re-encoded form is
    /// deterministic and stable.
    ///
    /// # Errors
    ///
    /// - [`PackError::TocHashMismatch`] — on-disk TOC bytes are non-canonical.
    /// - [`PackError::TocDecode`] — re-encoding the in-memory value failed
    ///   (should never happen; indicates a logic error).
    pub fn verify_table_of_contents(&self) -> PackResult<()> {
        let on_disk_toc_bytes = self.toc_bytes();
        let reencoded = self.toc.encode()?;

        if reencoded.as_slice() != on_disk_toc_bytes {
            return Err(PackError::TocHashMismatch);
        }

        Ok(())
    }

    /// Verify the entire pack: TOC canonicality, then content hash for every
    /// member.
    ///
    /// Suitable for integrity checks after download or long-term storage; not
    /// intended for the hot read path.
    ///
    /// # Errors
    ///
    /// Returns the first [`PackError`] encountered (TOC first, then members in
    /// TOC order).
    pub fn verify_all(&self) -> PackResult<()> {
        self.verify_table_of_contents()?;
        for record in &self.toc.records {
            self.verify_member(&record.key)?;
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::builder::ObjectPackBuilder;
    use heart::object_pack::{MemberKey, RelativePath};
    use smol_str::SmolStr;

    /// Build a pack containing the given (path, content) pairs.
    fn build_pack(members: &[(&str, &[u8])]) -> Bytes {
        let mut builder = ObjectPackBuilder::new();
        for (path, content) in members {
            let key = MemberKey::Source {
                path: RelativePath(SmolStr::new(*path)),
            };
            builder.add_member(key, Bytes::copy_from_slice(content)).unwrap();
        }
        let (sealed, _id) = builder.seal_to_bytes().unwrap();
        sealed
    }

    fn key(path: &str) -> MemberKey {
        MemberKey::Source { path: RelativePath(SmolStr::new(path)) }
    }

    // -----------------------------------------------------------------------
    // Round-trip
    // -----------------------------------------------------------------------

    #[test]
    fn round_trip_small_member() {
        let content = b"hello, NDPK";
        let packed = build_pack(&[("hello.txt", content)]);
        let reader = ObjectPackReader::open_bytes(packed).unwrap();
        assert_eq!(reader.member_count(), 1);
        let got = reader.get_member(&key("hello.txt")).unwrap();
        assert_eq!(got.as_ref(), content);
    }

    #[test]
    fn round_trip_multiple_members() {
        let pairs: &[(&str, &[u8])] = &[
            ("a.txt", b"alpha"),
            ("b.txt", b"beta beta beta"),
            ("c.txt", b"gamma gamma gamma gamma"),
        ];
        let packed = build_pack(pairs);
        let reader = ObjectPackReader::open_bytes(packed).unwrap();
        assert_eq!(reader.member_count(), 3);
        for (path, expected) in pairs {
            let got = reader.get_member(&key(path)).unwrap();
            assert_eq!(got.as_ref(), *expected, "mismatch for {path}");
        }
    }

    // -----------------------------------------------------------------------
    // Range reads
    // -----------------------------------------------------------------------

    #[test]
    fn range_read_within_single_chunk() {
        let content: Vec<u8> = (0u8..=255).cycle().take(1024).collect();
        let packed = build_pack(&[("data.bin", &content)]);
        let reader = ObjectPackReader::open_bytes(packed).unwrap();

        let got = reader.get_member_range(&key("data.bin"), 10..50).unwrap();
        assert_eq!(got.as_ref(), &content[10..50]);
    }

    #[test]
    fn range_read_spanning_chunk_boundary() {
        // Build a member large enough to span two 128 KiB chunks.
        let chunk_size = heart::object_pack::SOURCE_CHUNK_SIZE_BYTES as usize;
        // Total: 1.5 chunks — the boundary is at chunk_size.
        let content: Vec<u8> = (0u8..=255).cycle().take(chunk_size + chunk_size / 2).collect();
        let packed = build_pack(&[("large.bin", &content)]);
        let reader = ObjectPackReader::open_bytes(packed).unwrap();

        // Request a range that spans the chunk boundary.
        let range_start = (chunk_size - 64) as u64;
        let range_end = (chunk_size + 64) as u64;
        let got = reader
            .get_member_range(&key("large.bin"), range_start..range_end)
            .unwrap();
        assert_eq!(got.as_ref(), &content[range_start as usize..range_end as usize]);
    }

    #[test]
    fn range_read_at_exact_chunk_boundary() {
        let chunk_size = heart::object_pack::SOURCE_CHUNK_SIZE_BYTES as usize;
        let content: Vec<u8> = (0u8..=255).cycle().take(chunk_size * 2).collect();
        let packed = build_pack(&[("exact.bin", &content)]);
        let reader = ObjectPackReader::open_bytes(packed).unwrap();

        // Exactly the second chunk.
        let start = chunk_size as u64;
        let end = (chunk_size * 2) as u64;
        let got = reader.get_member_range(&key("exact.bin"), start..end).unwrap();
        assert_eq!(got.as_ref(), &content[chunk_size..chunk_size * 2]);
    }

    #[test]
    fn zero_length_member_reads_back_empty() {
        let packed = build_pack(&[("empty.txt", b"")]);
        let reader = ObjectPackReader::open_bytes(packed).unwrap();
        let got = reader.get_member(&key("empty.txt")).unwrap();
        assert!(got.is_empty());
        reader.verify_member(&key("empty.txt")).unwrap();
        // A range get over an empty member only admits the empty range.
        assert!(reader.get_member_range(&key("empty.txt"), 0..0).unwrap().is_empty());
        assert!(reader.get_member_range(&key("empty.txt"), 0..1).is_err());
    }

    #[test]
    fn trailing_bytes_after_toc_are_rejected() {
        let packed = build_pack(&[("x.txt", b"payload")]);
        let mut extended = packed.to_vec();
        extended.push(0u8);
        let err = ObjectPackReader::open_bytes(Bytes::from(extended)).unwrap_err();
        assert!(
            matches!(err, PackError::BadStructure { .. }),
            "expected BadStructure for trailing bytes, got {err:?}"
        );
    }

    #[test]
    fn empty_range_returns_empty_bytes_without_decompressing() {
        let content = b"nonempty content here";
        let packed = build_pack(&[("f.txt", content)]);
        let reader = ObjectPackReader::open_bytes(packed).unwrap();

        let got = reader.get_member_range(&key("f.txt"), 5..5).unwrap();
        assert!(got.is_empty());
    }

    // -----------------------------------------------------------------------
    // Out-of-range error typing
    // -----------------------------------------------------------------------

    #[test]
    fn out_of_range_end_beyond_member_length() {
        let content = b"short";
        let packed = build_pack(&[("s.txt", content)]);
        let reader = ObjectPackReader::open_bytes(packed).unwrap();

        let err = reader.get_member_range(&key("s.txt"), 0..100).unwrap_err();
        assert!(
            matches!(err, PackError::RangeOutOfBounds { end: 100, .. }),
            "expected RangeOutOfBounds, got {err:?}"
        );
    }

    #[test]
    fn out_of_range_start_greater_than_end() {
        let content = b"hello world";
        let packed = build_pack(&[("x.txt", content)]);
        let reader = ObjectPackReader::open_bytes(packed).unwrap();

        let err = reader.get_member_range(&key("x.txt"), 8..3).unwrap_err();
        assert!(
            matches!(err, PackError::RangeOutOfBounds { start: 8, end: 3, .. }),
            "expected RangeOutOfBounds, got {err:?}"
        );
    }

    // -----------------------------------------------------------------------
    // Truncated bytes
    // -----------------------------------------------------------------------

    #[test]
    fn truncated_bytes_yields_typed_error_not_panic() {
        let packed = build_pack(&[("x.txt", b"some content")]);

        // Chop to fewer than HEADER_LENGTH_BYTES.
        let truncated = packed.slice(0..8);
        let err = ObjectPackReader::open_bytes(truncated).unwrap_err();
        assert!(
            matches!(err, PackError::Truncated { .. }),
            "expected Truncated, got {err:?}"
        );
    }

    #[test]
    fn truncated_bytes_past_header_yields_typed_error() {
        let packed = build_pack(&[("x.txt", b"data")]);
        // Keep header but chop most of the rest.
        let truncated = packed.slice(0..HEADER_LENGTH_BYTES + 4);
        // This may yield Truncated or BadStructure depending on where the TOC is.
        let err = ObjectPackReader::open_bytes(truncated).unwrap_err();
        // Must be a typed PackError, not a panic.
        let _ = format!("{err}"); // ensure Display is implemented
    }

    // -----------------------------------------------------------------------
    // Integrity verification
    // -----------------------------------------------------------------------

    #[test]
    fn verify_member_ok() {
        let packed = build_pack(&[("good.txt", b"verified content")]);
        let reader = ObjectPackReader::open_bytes(packed).unwrap();
        reader.verify_member(&key("good.txt")).unwrap();
    }

    #[test]
    fn verify_all_ok() {
        let packed = build_pack(&[("a.txt", b"aaa"), ("b.txt", b"bbb")]);
        let reader = ObjectPackReader::open_bytes(packed).unwrap();
        reader.verify_all().unwrap();
    }

    #[test]
    fn corrupt_frame_yields_typed_error() {
        let mut packed: Vec<u8> = build_pack(&[("corrupt.txt", b"original content")]).to_vec();

        // Find a byte in the frame region (past the 24-byte header) and flip it.
        // The TOC is at the end; member frames follow the header.
        // We corrupt a byte deep enough in to avoid changing the magic/version
        // but early enough to be in the zstd frame data.
        if packed.len() > HEADER_LENGTH_BYTES + 4 {
            packed[HEADER_LENGTH_BYTES + 4] ^= 0xFF;
        }

        let reader = ObjectPackReader::open_bytes(Bytes::from(packed)).unwrap();
        // The header and TOC are intact; the corruption is in the frame body.
        // get_member or verify_member should yield a typed error.
        let err = reader.verify_member(&key("corrupt.txt")).unwrap_err();
        assert!(
            matches!(
                err,
                PackError::MemberHashMismatch { .. } | PackError::FrameDecode { .. }
            ),
            "expected MemberHashMismatch or FrameDecode, got {err:?}"
        );
    }

    #[test]
    fn member_not_found_yields_typed_error() {
        let packed = build_pack(&[("exists.txt", b"hi")]);
        let reader = ObjectPackReader::open_bytes(packed).unwrap();
        let err = reader.get_member(&key("does_not_exist.txt")).unwrap_err();
        assert!(
            matches!(err, PackError::MemberNotFound { .. }),
            "expected MemberNotFound, got {err:?}"
        );
    }

    #[test]
    fn verify_table_of_contents_ok_on_well_formed_pack() {
        let packed = build_pack(&[("toc.txt", b"toc content")]);
        let reader = ObjectPackReader::open_bytes(packed).unwrap();
        reader.verify_table_of_contents().unwrap();
    }

    #[test]
    fn id_is_stable_for_same_input() {
        let packed_a = build_pack(&[("a.txt", b"stable")]);
        let packed_b = build_pack(&[("a.txt", b"stable")]);
        let reader_a = ObjectPackReader::open_bytes(packed_a).unwrap();
        let reader_b = ObjectPackReader::open_bytes(packed_b).unwrap();
        assert_eq!(reader_a.id(), reader_b.id());
    }

    #[test]
    fn id_differs_for_different_content() {
        let packed_a = build_pack(&[("a.txt", b"version one")]);
        let packed_b = build_pack(&[("a.txt", b"version two")]);
        let reader_a = ObjectPackReader::open_bytes(packed_a).unwrap();
        let reader_b = ObjectPackReader::open_bytes(packed_b).unwrap();
        assert_ne!(reader_a.id(), reader_b.id());
    }
}
