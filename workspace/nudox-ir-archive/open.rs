//! Archive opening and validation: [`open_archive`].
//!
//! # Validation steps
//!
//! 1. Check magic bytes (`b"NdIr"`).
//! 2. Check `format_version` ≤ [`FORMAT_VERSION`]; reject unknown with
//!    [`ArchiveError::UnsupportedFormat`].
//! 3. Verify `header_crc32` over the whole header minus the CRC field itself;
//!    reject nonzero reserved header/TOC fields.
//! 4. Parse the TOC; for each entry:
//!    - Check `length` ≤ [`MAX_SECTION_UNCOMPRESSED`]; return
//!      [`ArchiveError::SectionTooLarge`] if violated.
//!    - Check that `offset + length` fits in the byte slice.
//!    - Verify `uncompressed_crc32` of the section body.
//!    - Unknown `section_id`: if `TOC_ENTRY_FLAG_OPTIONAL` is set, skip;
//!      otherwise return [`ArchiveError::UnknownSection`].
//! 5. Construct and return a [`YokedArchive`].

use std::sync::Arc;

use zerocopy::FromBytes;

use crate::error::ArchiveError;
use crate::header::{
    ArchiveHeader, SectionId, TocEntry, FORMAT_VERSION, MAGIC, MAX_SECTION_UNCOMPRESSED,
};
use crate::section::crc32_of;
use crate::view::{YokedArchive, ParsedSections};

/// Open and validate a `NdIr` archive from raw bytes.
///
/// On success returns a [`YokedArchive`] that borrows the sections from
/// the provided `Arc<[u8]>` with zero copies.
pub fn open_archive(bytes: Arc<[u8]>) -> Result<YokedArchive, ArchiveError> {
    // --- Step 1: magic ---
    if bytes.len() < 4 || bytes[0..4] != MAGIC {
        return Err(ArchiveError::BadMagic);
    }

    // --- Step 2: parse + validate header ---
    if bytes.len() < std::mem::size_of::<ArchiveHeader>() {
        return Err(ArchiveError::Truncated);
    }
    let hdr = ArchiveHeader::ref_from_bytes(&bytes[0..64])
        .map_err(|_| ArchiveError::Truncated)?;

    if hdr.format_version() > FORMAT_VERSION {
        return Err(ArchiveError::UnsupportedFormat {
            found: hdr.format_version(),
            max: FORMAT_VERSION,
        });
    }

    // --- Step 3: header CRC (whole header minus the CRC field itself) ---
    let computed_crc = ArchiveHeader::header_crc(&bytes[0..64]);
    if computed_crc != hdr.header_crc32() {
        return Err(ArchiveError::Crc);
    }

    // v1: reserved header fields must be zero.
    if hdr.flags() != 0 {
        return Err(ArchiveError::ReservedNonzero("header.flags"));
    }
    if hdr.reserved != [0u8; 2] || hdr._reserved_tail != [0u8; 12] {
        return Err(ArchiveError::ReservedNonzero("header.reserved"));
    }

    // --- Step 4: parse TOC and validate sections ---
    let toc_offset = hdr.toc_offset() as usize;
    let toc_len = hdr.toc_len() as usize;
    // Both the multiply and the add can wrap on crafted headers — fail closed.
    let toc_end = toc_len
        .checked_mul(std::mem::size_of::<TocEntry>()) // 32 bytes each
        .and_then(|toc_bytes_len| toc_offset.checked_add(toc_bytes_len))
        .ok_or(ArchiveError::Truncated)?;

    if bytes.len() < toc_end {
        return Err(ArchiveError::Truncated);
    }

    let mut sections: std::collections::HashMap<u32, (usize, usize)> =
        std::collections::HashMap::new();

    for i in 0..toc_len {
        let toc_entry_off = toc_offset + i * std::mem::size_of::<TocEntry>();
        let toc_e = TocEntry::ref_from_bytes(&bytes[toc_entry_off..toc_entry_off + 32])
            .map_err(|_| ArchiveError::Truncated)?;

        let section_id = toc_e.section_id();
        let offset = toc_e.offset() as usize;
        let length = toc_e.length();
        let stored_crc = toc_e.uncompressed_crc32();

        // v1: only the OPTIONAL flag bit is defined; reserved bits/bytes must
        // be zero so corrupted or non-canonical TOC entries fail closed.
        if toc_e.flags() & !crate::header::TOC_ENTRY_FLAG_OPTIONAL != 0 {
            return Err(ArchiveError::ReservedNonzero("toc.flags"));
        }
        if toc_e.reserved != [0u8; 4] {
            return Err(ArchiveError::ReservedNonzero("toc.reserved"));
        }

        // Size guard.
        if length > MAX_SECTION_UNCOMPRESSED {
            return Err(ArchiveError::SectionTooLarge {
                section: section_id,
                got: length,
                max: MAX_SECTION_UNCOMPRESSED,
            });
        }

        // Bounds check.
        let end = offset.checked_add(length as usize)
            .ok_or(ArchiveError::Truncated)?;
        if end > bytes.len() {
            return Err(ArchiveError::Truncated);
        }

        // Section CRC.
        let body = &bytes[offset..end];
        let actual_crc = crc32_of(body);
        if actual_crc != stored_crc {
            return Err(ArchiveError::Crc);
        }

        // Forward-compat: unknown section.
        if SectionId::try_from(section_id).is_none() {
            if toc_e.is_optional() {
                continue; // skip gracefully
            } else {
                return Err(ArchiveError::UnknownSection(section_id));
            }
        }

        sections.insert(section_id, (offset, length as usize));
    }

    // --- Step 5: build YokedArchive ---
    //
    // We parse section byte-ranges here and pass them along with the Arc to
    // construct the YokedArchive.  The YokedArchive stores an Arc and the
    // byte ranges; accessors borrow slices from the Arc.

    let parsed = ParsedSections {
        entry_count: hdr.entry_count(),
        string_count: hdr.string_count(),
        link_count: hdr.link_count(),
        kind_table_version: hdr.kind_table_version(),
        sections,
    };

    Ok(YokedArchive::new(bytes, parsed))
}
