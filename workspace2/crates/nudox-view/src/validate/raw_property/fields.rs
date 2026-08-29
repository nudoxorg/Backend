use nudox_schema::{
    DESCRIPTOR_BODY_LENGTH_OFFSET, DESCRIPTOR_BODY_OFFSET, DESCRIPTOR_FLAGS_OFFSET,
    DESCRIPTOR_KIND_OFFSET, DESCRIPTOR_RESERVED_OFFSET, DESCRIPTOR_ROWS_OFFSET, FRAME_HEADER_BYTES,
    HEADER_LENGTH_OFFSET, HEADER_MAGIC_OFFSET, HEADER_MAJOR_OFFSET, HEADER_MINOR_OFFSET,
    HEADER_RESERVED_A_OFFSET, HEADER_RESERVED_B_OFFSET, HEADER_SCHEMA_OFFSET,
    HEADER_SECTION_COUNT_OFFSET, HEADER_TOTAL_LENGTH_OFFSET, SECTION_DESCRIPTOR_BYTES, SectionKind,
};

use super::super::ValidateError;

/// A closed byte range and the exact structural rule it exercises.
///
/// The table is derived from wire-record offsets, so extending the header or
/// descriptor makes the const coverage assertion fail until its mutation law
/// is added deliberately.
#[derive(Clone, Copy)]
pub(super) struct MutationField<Field> {
    pub(super) kind: Field,
    pub(super) offset: usize,
    pub(super) bytes: usize,
}

#[derive(Clone, Copy)]
pub(super) enum HeaderField {
    Magic,
    Major,
    Minor,
    HeaderLength,
    TotalLength,
    SectionCount,
    ReservedA,
    Schema,
    ReservedB,
}

pub(super) const HEADER_MUTATION_FIELDS: [MutationField<HeaderField>; 9] = [
    MutationField {
        kind: HeaderField::Magic,
        offset: HEADER_MAGIC_OFFSET,
        bytes: HEADER_MAJOR_OFFSET - HEADER_MAGIC_OFFSET,
    },
    MutationField {
        kind: HeaderField::Major,
        offset: HEADER_MAJOR_OFFSET,
        bytes: HEADER_MINOR_OFFSET - HEADER_MAJOR_OFFSET,
    },
    MutationField {
        kind: HeaderField::Minor,
        offset: HEADER_MINOR_OFFSET,
        bytes: HEADER_LENGTH_OFFSET - HEADER_MINOR_OFFSET,
    },
    MutationField {
        kind: HeaderField::HeaderLength,
        offset: HEADER_LENGTH_OFFSET,
        bytes: HEADER_TOTAL_LENGTH_OFFSET - HEADER_LENGTH_OFFSET,
    },
    MutationField {
        kind: HeaderField::TotalLength,
        offset: HEADER_TOTAL_LENGTH_OFFSET,
        bytes: HEADER_SECTION_COUNT_OFFSET - HEADER_TOTAL_LENGTH_OFFSET,
    },
    MutationField {
        kind: HeaderField::SectionCount,
        offset: HEADER_SECTION_COUNT_OFFSET,
        bytes: HEADER_RESERVED_A_OFFSET - HEADER_SECTION_COUNT_OFFSET,
    },
    MutationField {
        kind: HeaderField::ReservedA,
        offset: HEADER_RESERVED_A_OFFSET,
        bytes: HEADER_SCHEMA_OFFSET - HEADER_RESERVED_A_OFFSET,
    },
    MutationField {
        kind: HeaderField::Schema,
        offset: HEADER_SCHEMA_OFFSET,
        bytes: HEADER_RESERVED_B_OFFSET - HEADER_SCHEMA_OFFSET,
    },
    MutationField {
        kind: HeaderField::ReservedB,
        offset: HEADER_RESERVED_B_OFFSET,
        bytes: FRAME_HEADER_BYTES - HEADER_RESERVED_B_OFFSET,
    },
];

#[derive(Clone, Copy)]
pub(super) enum DescriptorField {
    Kind,
    Flags,
    Reserved,
    BodyOffset,
    BodyLength,
    Rows,
}

pub(super) const DESCRIPTOR_MUTATION_FIELDS: [MutationField<DescriptorField>; 6] = [
    MutationField {
        kind: DescriptorField::Kind,
        offset: DESCRIPTOR_KIND_OFFSET,
        bytes: DESCRIPTOR_FLAGS_OFFSET - DESCRIPTOR_KIND_OFFSET,
    },
    MutationField {
        kind: DescriptorField::Flags,
        offset: DESCRIPTOR_FLAGS_OFFSET,
        bytes: DESCRIPTOR_RESERVED_OFFSET - DESCRIPTOR_FLAGS_OFFSET,
    },
    MutationField {
        kind: DescriptorField::Reserved,
        offset: DESCRIPTOR_RESERVED_OFFSET,
        bytes: DESCRIPTOR_BODY_OFFSET - DESCRIPTOR_RESERVED_OFFSET,
    },
    MutationField {
        kind: DescriptorField::BodyOffset,
        offset: DESCRIPTOR_BODY_OFFSET,
        bytes: DESCRIPTOR_BODY_LENGTH_OFFSET - DESCRIPTOR_BODY_OFFSET,
    },
    MutationField {
        kind: DescriptorField::BodyLength,
        offset: DESCRIPTOR_BODY_LENGTH_OFFSET,
        bytes: DESCRIPTOR_ROWS_OFFSET - DESCRIPTOR_BODY_LENGTH_OFFSET,
    },
    MutationField {
        kind: DescriptorField::Rows,
        offset: DESCRIPTOR_ROWS_OFFSET,
        bytes: SECTION_DESCRIPTOR_BYTES - DESCRIPTOR_ROWS_OFFSET,
    },
];

#[allow(
    clippy::indexing_slicing,
    reason = "the loop guard is the const array length; stable Rust 1.97 cannot use slice::get in const evaluation"
)]
const fn fields_cover<const FIELD_COUNT: usize, Field>(
    fields: &[MutationField<Field>; FIELD_COUNT],
    total_bytes: usize,
) -> bool {
    let mut next_offset = 0;
    let mut index = 0;
    while index < FIELD_COUNT {
        let field = &fields[index];
        if field.offset != next_offset || field.bytes == 0 {
            return false;
        }
        next_offset += field.bytes;
        index += 1;
    }
    next_offset == total_bytes
}

const _: () = assert!(fields_cover(&HEADER_MUTATION_FIELDS, FRAME_HEADER_BYTES));
const _: () = assert!(fields_cover(
    &DESCRIPTOR_MUTATION_FIELDS,
    SECTION_DESCRIPTOR_BYTES
));

pub(super) enum ExpectedValidation<'bytes> {
    Error(ValidateError),
    KnownSection {
        kind: SectionKind,
        rows: u32,
        body: &'bytes [u8],
    },
}
