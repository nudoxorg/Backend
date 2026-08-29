//! Fixed envelope parsing and canonical header checks.

use nudox_schema::{
    DecodeLimits, FORMAT_MAJOR, FORMAT_MINOR, FRAME_HEADER_BYTES, FRAME_MAGIC, FrameHeader,
    LimitKind, SECTION_DESCRIPTOR_BYTES, SchemaId, SectionDescriptor,
};
use zerocopy::FromBytes;

use super::directory::enforce_limit;
use super::{ValidateError, take};

#[derive(Clone, Copy)]
pub(crate) struct Header<'a> {
    pub(crate) bytes: usize,
    pub(crate) directory_end: usize,
    pub(crate) descriptors: &'a [SectionDescriptor],
}

pub(crate) fn parse_header(
    bytes: &[u8],
    limits: DecodeLimits,
) -> Result<Header<'_>, ValidateError> {
    let fixed = take(bytes, 0, FRAME_HEADER_BYTES)?;
    let header = FrameHeader::ref_from_bytes(fixed).map_err(|source| ValidateError::Truncated {
        offset: 0,
        needed: FRAME_HEADER_BYTES,
        available: source.into_src().len(),
    })?;
    if header.magic != FRAME_MAGIC {
        return Err(ValidateError::BadMagic {
            observed: header.magic,
        });
    }
    if header.major != FORMAT_MAJOR || header.minor != FORMAT_MINOR {
        return Err(ValidateError::UnsupportedVersion {
            major: u8::from(header.major),
            minor: u8::from(header.minor),
        });
    }
    if header.reserved_a.get() != 0 {
        return Err(ValidateError::NonzeroHeaderReserved {
            offset: nudox_schema::HEADER_RESERVED_A_OFFSET,
        });
    }
    let schema_wire = header.schema.get();
    match SchemaId::try_from(schema_wire) {
        Ok(SchemaId::Frame) => {}
        Ok(SchemaId::Object) | Err(_) => {
            return Err(ValidateError::UnsupportedSchema { wire: schema_wire });
        }
    }
    if header.reserved_b.get() != 0 {
        return Err(ValidateError::NonzeroHeaderReserved {
            offset: nudox_schema::HEADER_RESERVED_B_OFFSET,
        });
    }
    canonical_header(bytes, limits, header)
}

fn canonical_header<'bytes>(
    bytes: &'bytes [u8],
    limits: DecodeLimits,
    header: &FrameHeader,
) -> Result<Header<'bytes>, ValidateError> {
    let declared_total = header.total_bytes.get();
    enforce_limit(LimitKind::FrameBytes, declared_total, *limits.frame_bytes)?;
    #[allow(
        clippy::as_conversions,
        reason = "accepted frame bytes are bounded below one mebibyte on the project's supported 32/64-bit targets"
    )]
    let total_bytes = declared_total as usize;
    if total_bytes != bytes.len() {
        return Err(ValidateError::TotalLengthMismatch {
            declared: declared_total,
            actual: bytes.len(),
        });
    }
    if total_bytes < FRAME_HEADER_BYTES {
        return Err(ValidateError::FrameTooShort {
            declared: total_bytes,
            minimum: FRAME_HEADER_BYTES,
        });
    }
    let section_count = header.section_count.get();
    enforce_limit(
        LimitKind::Sections,
        u32::from(section_count),
        u32::from(*limits.sections),
    )?;
    let (directory_bytes, directory_end) = directory_layout(section_count);
    if usize::from(header.header_bytes.get()) != directory_end {
        return Err(ValidateError::HeaderLengthMismatch {
            declared: header.header_bytes.get(),
            expected: directory_end,
        });
    }
    if directory_end > total_bytes {
        return Err(ValidateError::HeaderOutsideFrame {
            header_bytes: directory_end,
            total_bytes,
        });
    }
    let descriptors = borrow_descriptors(bytes, directory_bytes, section_count)?;
    Ok(Header {
        bytes: total_bytes,
        directory_end,
        descriptors,
    })
}

fn directory_layout(section_count: u16) -> (usize, usize) {
    let directory_bytes = usize::from(section_count) * SECTION_DESCRIPTOR_BYTES;
    (directory_bytes, FRAME_HEADER_BYTES + directory_bytes)
}

fn borrow_descriptors(
    bytes: &[u8],
    directory_bytes: usize,
    section_count: u16,
) -> Result<&[SectionDescriptor], ValidateError> {
    let directory = take(bytes, FRAME_HEADER_BYTES, directory_bytes)?;
    <[SectionDescriptor]>::ref_from_bytes_with_elems(directory, usize::from(section_count)).map_err(
        |source| ValidateError::Truncated {
            offset: FRAME_HEADER_BYTES,
            needed: directory_bytes,
            available: source.into_src().len(),
        },
    )
}
