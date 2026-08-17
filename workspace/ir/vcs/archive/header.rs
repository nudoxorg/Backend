//! POD header structs for the `NdIr` PackageArchive format.
//!
//! All multi-byte integers are **little-endian**. The structs derive `zerocopy`
//! traits so they can be cast directly to/from byte slices with no copies.
//!
//! # Wire layout
//!
//! ```text
//! offset 0  : ArchiveHeader (64 bytes)
//! offset 64 : TocEntry[toc_len] (each 28 bytes)
//! offset ... : section bodies (aligned as needed by each section)
//! ```
//!
//! Each section is identified by a [`SectionId`] u32, recorded in its
//! [`TocEntry`]. Unknown section ids are rejected unless the
//! `TOC_ENTRY_FLAG_OPTIONAL` bit is set in the [`TocEntry`] reserved field
//! (treated as flags by the reader).

use zerocopy::{FromBytes, Immutable, IntoBytes, KnownLayout};

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// Archive format magic bytes (`b"NdIr"`).
pub const MAGIC: [u8; 4] = *b"NdIr";

/// Current format version.
pub const FORMAT_VERSION: u16 = 1;

/// Hard cap on any single section's uncompressed byte length (512 MiB).
pub const MAX_SECTION_UNCOMPRESSED: u64 = 512 * 1024 * 1024;

/// Hard cap on total live entries in one archive (50 M).
pub const MAX_ENTRIES: u32 = 50_000_000;

/// Hard cap on the string-blob section (256 MiB).
pub const MAX_STRING_BLOB: u64 = 256 * 1024 * 1024;

/// Bit in a [`TocEntry`]'s `flags` field that marks the section as OPTIONAL.
/// Readers that do not recognise the `section_id` may skip the section rather
/// than returning [`crate::archive::error::Error::UnknownSection`].
pub const TOC_ENTRY_FLAG_OPTIONAL: u32 = 1 << 0;

// ---------------------------------------------------------------------------
// SectionId
// ---------------------------------------------------------------------------

/// Stable numeric IDs for the sections in the archive TOC.
///
/// Values are frozen and must never be reused.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(u32)]
pub enum SectionId {
    /// Dense array of [`EntryHead`] records, one per live entry.
    EntryHeads = 1,
    /// Postcard-encoded [`ir::wire::OwnedEntryPayload`] bodies, packed
    /// end-to-end. Offsets/lengths are stored in [`EntryHead`].
    EntryPayloads = 2,
    /// Sorted `(IntroId[32], ArenaIdx u32)` pairs for binary-search lookup.
    IntroIndex = 3,
    /// Name/alias string table: LE u32 offsets + UTF-8 blob.
    StringTable = 4,
    /// Name+alias → ArenaIdx postings (sorted).
    NameIndex = 5,
    /// Tree CSR: u32 offsets + u32 children.
    TreeCsr = 6,
    /// Link CSR: sorted (ArenaIdx, link records).
    LinkCsr = 7,
    /// TypeFingerprintId → ArenaIdx postings.
    TypeSkeletonIndex = 8,
    /// Per-entry `payload_hash` column (32 bytes × entry_count).
    IntroPayloadHash = 9,
    /// Postcard-encoded metadata blob (kind_table_version, counts).
    Meta = 10,
    /// Dense u16 kind discriminant column, one per live entry.
    KindDiscCol = 11,
    /// Reserved for future use; readers should skip via OPTIONAL flag.
    Reserved12 = 12,
}

impl SectionId {
    /// Convert a raw `u32` to a `SectionId`, returning `None` for unknown values.
    #[inline]
    pub fn try_from(v: u32) -> Option<Self> {
        match v {
            1 => Some(Self::EntryHeads),
            2 => Some(Self::EntryPayloads),
            3 => Some(Self::IntroIndex),
            4 => Some(Self::StringTable),
            5 => Some(Self::NameIndex),
            6 => Some(Self::TreeCsr),
            7 => Some(Self::LinkCsr),
            8 => Some(Self::TypeSkeletonIndex),
            9 => Some(Self::IntroPayloadHash),
            10 => Some(Self::Meta),
            11 => Some(Self::KindDiscCol),
            12 => Some(Self::Reserved12),
            _ => None,
        }
    }

    /// The raw `u32` wire id.
    #[inline]
    pub fn as_u32(self) -> u32 {
        self as u32
    }
}

// ---------------------------------------------------------------------------
// ArchiveHeader (64 bytes, repr C)
// ---------------------------------------------------------------------------

/// The fixed 64-byte header at byte offset 0 of every `NdIr` archive.
///
/// ## Byte map
///
/// ```text
///  0.. 4  magic: b"NdIr"
///  4.. 6  format_version: u16 LE
///  6.. 8  flags: u16 LE
///  8..16  type_hash: [u8; 8]
/// 16..20  header_crc32: u32 LE  ← CRC covers [0..16)
/// 20..28  toc_offset: u64 LE
/// 28..36  toc_len: u64 LE
/// 36..40  entry_count: u32 LE
/// 40..44  string_count: u32 LE
/// 44..48  link_count: u32 LE
/// 48..50  kind_table_version: u16 LE
/// 50..52  reserved: [u8; 2]
/// 52..64  _reserved_tail: [u8; 12] (must be 0)
/// ```
#[derive(Clone, Copy, Debug, FromBytes, IntoBytes, KnownLayout, Immutable)]
#[repr(C)]
pub struct ArchiveHeader {
    /// Must equal `b"NdIr"` (= `[0x4E, 0x64, 0x49, 0x72]`).
    pub magic: [u8; 4],
    /// Format version; currently `FORMAT_VERSION = 1`.
    pub format_version: [u8; 2], // u16 LE  offset 4
    /// Archive-level flags (reserved; must be 0 in v1).
    pub flags: [u8; 2], // u16 LE  offset 6
    /// First 8 bytes of `blake3("nudox.kindtable.v1")` — a reader sanity check
    /// that the kind-discriminant table is the same version that wrote this file.
    pub type_hash: [u8; 8], //         offset 8
    /// CRC32 over bytes 0..16 of this header (everything before this field).
    pub header_crc32: [u8; 4], // u32 LE  offset 16
    /// Byte offset of the first [`TocEntry`] from the start of the archive.
    pub toc_offset: [u8; 8], // u64 LE  offset 20
    /// Number of [`TocEntry`] records in the TOC.
    pub toc_len: [u8; 8], // u64 LE  offset 28
    /// Total number of live entries in this archive.
    pub entry_count: [u8; 4], // u32 LE  offset 36
    /// Total number of interned strings in the StringTable.
    pub string_count: [u8; 4], // u32 LE  offset 40
    /// Total number of link records.
    pub link_count: [u8; 4], // u32 LE  offset 44
    /// Kind-table version (matches `KindDiscriminant` wire values).
    pub kind_table_version: [u8; 2], // u16 LE offset 48
    /// Reserved (must be 0).
    pub reserved: [u8; 2], //         offset 50
    /// Tail padding to reach 64 bytes (must be 0).
    pub _reserved_tail: [u8; 12], //         offset 52..64
}

const _: () = assert!(std::mem::size_of::<ArchiveHeader>() == 64);

impl ArchiveHeader {
    /// Byte offset in the header at which `header_crc32` lives.
    ///
    /// Layout: magic(4) + format_version(2) + flags(2) + type_hash(8) = 16
    pub const CRC_OFFSET: usize = 16;

    /// CRC32 over the whole 64-byte header **except** the `header_crc32` field
    /// itself: bytes `[0..16)` and `[20..64)`. Covering the full header (TOC
    /// pointer, counts, reserved tail) means any single corrupted header byte
    /// is detected at open, not deferred to a downstream bounds check.
    pub fn header_crc(header_bytes: &[u8]) -> u32 {
        debug_assert!(header_bytes.len() >= 64);
        let mut covered = [0u8; 60];
        covered[..Self::CRC_OFFSET].copy_from_slice(&header_bytes[..Self::CRC_OFFSET]);
        covered[Self::CRC_OFFSET..].copy_from_slice(&header_bytes[Self::CRC_OFFSET + 4..64]);
        crate::archive::section::crc32_of(&covered)
    }

    // Convenience readers.

    #[inline]
    pub fn format_version(&self) -> u16 {
        u16::from_le_bytes(self.format_version)
    }
    #[inline]
    pub fn flags(&self) -> u16 {
        u16::from_le_bytes(self.flags)
    }
    #[inline]
    pub fn header_crc32(&self) -> u32 {
        u32::from_le_bytes(self.header_crc32)
    }
    #[inline]
    pub fn toc_offset(&self) -> u64 {
        u64::from_le_bytes(self.toc_offset)
    }
    #[inline]
    pub fn toc_len(&self) -> u64 {
        u64::from_le_bytes(self.toc_len)
    }
    #[inline]
    pub fn entry_count(&self) -> u32 {
        u32::from_le_bytes(self.entry_count)
    }
    #[inline]
    pub fn string_count(&self) -> u32 {
        u32::from_le_bytes(self.string_count)
    }
    #[inline]
    pub fn link_count(&self) -> u32 {
        u32::from_le_bytes(self.link_count)
    }
    #[inline]
    pub fn kind_table_version(&self) -> u16 {
        u16::from_le_bytes(self.kind_table_version)
    }

    /// Encode a `u16` into `field` as LE bytes.
    #[inline]
    pub fn set_u16(field: &mut [u8; 2], v: u16) {
        *field = v.to_le_bytes();
    }
    /// Encode a `u32` into `field` as LE bytes.
    #[inline]
    pub fn set_u32(field: &mut [u8; 4], v: u32) {
        *field = v.to_le_bytes();
    }
    /// Encode a `u64` into `field` as LE bytes.
    #[inline]
    pub fn set_u64(field: &mut [u8; 8], v: u64) {
        *field = v.to_le_bytes();
    }
}

// ---------------------------------------------------------------------------
// TocEntry (32 bytes, repr C)
// ---------------------------------------------------------------------------

/// One entry in the Table of Contents; points at a section within the archive.
///
/// 32 bytes: section_id(4) + offset(8) + length(8) + crc32(4) + flags(4) +
/// reserved(4).
#[derive(Clone, Copy, Debug, FromBytes, IntoBytes, KnownLayout, Immutable)]
#[repr(C)]
pub struct TocEntry {
    /// Which [`SectionId`] this entry describes (LE u32).
    pub section_id: [u8; 4],
    /// Byte offset from the start of the archive (LE u64).
    pub offset: [u8; 8],
    /// Byte length of the section body (LE u64).
    pub length: [u8; 8],
    /// CRC32 of the section body bytes (LE u32).
    pub uncompressed_crc32: [u8; 4],
    /// Flags: bit 0 = `TOC_ENTRY_FLAG_OPTIONAL`. Reserved bits must be 0.
    pub flags: [u8; 4],
    /// Reserved; must be 0 in v1. Present so the record is a round 32 bytes
    /// (padding-free, as `zerocopy::IntoBytes` requires).
    pub reserved: [u8; 4],
}

const _: () = assert!(std::mem::size_of::<TocEntry>() == 32);

impl TocEntry {
    #[inline]
    pub fn section_id(&self) -> u32 {
        u32::from_le_bytes(self.section_id)
    }
    #[inline]
    pub fn offset(&self) -> u64 {
        u64::from_le_bytes(self.offset)
    }
    #[inline]
    pub fn length(&self) -> u64 {
        u64::from_le_bytes(self.length)
    }
    #[inline]
    pub fn uncompressed_crc32(&self) -> u32 {
        u32::from_le_bytes(self.uncompressed_crc32)
    }
    #[inline]
    pub fn flags(&self) -> u32 {
        u32::from_le_bytes(self.flags)
    }
    #[inline]
    pub fn is_optional(&self) -> bool {
        self.flags() & TOC_ENTRY_FLAG_OPTIONAL != 0
    }
}

// ---------------------------------------------------------------------------
// EntryHead (56 bytes, repr C)
// ---------------------------------------------------------------------------

/// Fixed-width per-entry record stored in the [`SectionId::EntryHeads`] section.
///
/// 56 bytes: intro(32) + name(4) + source_path(4) + span_start(4) + span_end(4)
///           + visibility(1) + flags(1) + kind_disc(2) + parent(4)
///
/// Field sizes (total 64 bytes):
/// ```text
/// intro(32) name(4) source_path(4) span_start(4) span_end(4)
/// visibility(1) flags(1) kind_disc(2) parent(4) payload_off(4) payload_len(4)
/// 32 + 4 + 4 + 4 + 4 + 1 + 1 + 2 + 4 + 4 + 4 = 64
/// ```
#[derive(Clone, Copy, Debug, FromBytes, IntoBytes, KnownLayout, Immutable)]
#[repr(C)]
pub struct EntryHead {
    /// The 32-byte [`ir::change::IntroId`] bytes.
    pub intro: [u8; 32],
    /// [`crate::vcs_types::StrId`] (LE u32) into the StringTable for the entry's primary name.
    pub name: [u8; 4],
    /// [`crate::vcs_types::StrId`] (LE u32) for `source_path`.
    pub source_path: [u8; 4],
    /// Span start byte offset (LE u32).
    pub span_start: [u8; 4],
    /// Span end byte offset (LE u32).
    pub span_end: [u8; 4],
    /// Visibility byte (language-specific).
    pub visibility: u8,
    /// Entry payload flags byte ([`ir::wire::EntryPayloadFlags`]).
    pub flags: u8,
    /// Kind discriminant (LE u16; matches [`ir::kind::KindDiscriminant::as_u16()`]).
    pub kind_disc: [u8; 2],
    /// ArenaIdx (LE u32) of the parent entry, or `u32::MAX` if this is a root.
    pub parent: [u8; 4],
    /// Byte offset of the postcard payload body within [`SectionId::EntryPayloads`] (LE u32).
    pub payload_off: [u8; 4],
    /// Byte length of the postcard payload body (LE u32).
    pub payload_len: [u8; 4],
}

const _: () = assert!(std::mem::size_of::<EntryHead>() == 64);

impl EntryHead {
    /// Sentinel value for `parent` meaning "no parent" (root entry).
    pub const NO_PARENT: u32 = u32::MAX;

    #[inline]
    pub fn name_str_id(&self) -> u32 {
        u32::from_le_bytes(self.name)
    }
    #[inline]
    pub fn source_path_str_id(&self) -> u32 {
        u32::from_le_bytes(self.source_path)
    }
    #[inline]
    pub fn span_start(&self) -> u32 {
        u32::from_le_bytes(self.span_start)
    }
    #[inline]
    pub fn span_end(&self) -> u32 {
        u32::from_le_bytes(self.span_end)
    }
    #[inline]
    pub fn kind_disc(&self) -> u16 {
        u16::from_le_bytes(self.kind_disc)
    }
    #[inline]
    pub fn parent_idx(&self) -> Option<u32> {
        let v = u32::from_le_bytes(self.parent);
        if v == Self::NO_PARENT { None } else { Some(v) }
    }
    #[inline]
    pub fn payload_off(&self) -> u32 {
        u32::from_le_bytes(self.payload_off)
    }
    #[inline]
    pub fn payload_len(&self) -> u32 {
        u32::from_le_bytes(self.payload_len)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn header_size_is_64() {
        assert_eq!(std::mem::size_of::<ArchiveHeader>(), 64);
    }

    #[test]
    fn toc_entry_size_is_32() {
        assert_eq!(std::mem::size_of::<TocEntry>(), 32);
    }

    #[test]
    fn entry_head_size_is_64() {
        assert_eq!(std::mem::size_of::<EntryHead>(), 64);
    }

    #[test]
    fn section_id_roundtrip() {
        for id in [
            SectionId::EntryHeads,
            SectionId::EntryPayloads,
            SectionId::IntroIndex,
            SectionId::StringTable,
            SectionId::NameIndex,
            SectionId::TreeCsr,
            SectionId::LinkCsr,
            SectionId::TypeSkeletonIndex,
            SectionId::IntroPayloadHash,
            SectionId::Meta,
            SectionId::KindDiscCol,
        ] {
            assert_eq!(SectionId::try_from(id.as_u32()), Some(id));
        }
        assert_eq!(SectionId::try_from(0), None);
        assert_eq!(SectionId::try_from(99), None);
    }

    #[test]
    fn entry_head_no_parent_sentinel() {
        let mut eh: EntryHead = <EntryHead as zerocopy::FromZeros>::new_zeroed();
        eh.parent = u32::MAX.to_le_bytes();
        assert_eq!(eh.parent_idx(), None);
        eh.parent = 7u32.to_le_bytes();
        assert_eq!(eh.parent_idx(), Some(7));
    }
}
