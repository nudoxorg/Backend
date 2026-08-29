#![no_std]
#![forbid(unsafe_code)]
//! Closed Wave 1 schema vocabulary, compatibility rules, and lowerable wire limits.

mod ids;
mod limits;
mod vocabulary;

pub use ids::{OperationId, SchemaId, UnknownOperationId, UnknownSchemaId};
pub use limits::{
    ArenaBytesLimit, DecodeLimits, FrameBytesLimit, LimitAmount, LimitError, LimitKind,
    MAX_ARENA_BYTES, MAX_FRAME_BYTES, MAX_ROWS, MAX_SECTIONS, RowCount, RowCountLimit,
    SectionCountLimit,
};
pub use vocabulary::{
    DESCRIPTOR_BODY_LENGTH_OFFSET, DESCRIPTOR_BODY_OFFSET, DESCRIPTOR_FLAGS_OFFSET,
    DESCRIPTOR_KIND_OFFSET, DESCRIPTOR_RESERVED_OFFSET, DESCRIPTOR_ROWS_OFFSET, FORMAT_MAJOR,
    FORMAT_MINOR, FRAME_HEADER_BYTES, FRAME_MAGIC, FRAME_SCHEMA_ID, FrameHeader,
    HEADER_LENGTH_OFFSET, HEADER_MAGIC_OFFSET, HEADER_MAJOR_OFFSET, HEADER_MINOR_OFFSET,
    HEADER_RESERVED_A_OFFSET, HEADER_RESERVED_B_OFFSET, HEADER_SCHEMA_OFFSET,
    HEADER_SECTION_COUNT_OFFSET, HEADER_TOTAL_LENGTH_OFFSET, OPTIONAL_SECTION_FLAG, ProtocolMajor,
    ProtocolMinor, SECTION_ALIGNMENT, SECTION_DESCRIPTOR_BYTES, SectionCompatibilityError,
    SectionDescriptor, SectionDisposition, SectionFlags, SectionKind, SectionKindCode,
    UnknownSectionKind, section_compatibility,
};
