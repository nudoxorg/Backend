use crate::{
    view::{DirectoryFault, FragmentError, WireField},
    wire::{
        ByteLength, ByteOffset, DIRECTORY_ENTRY_LAYOUT, HEADER_LAYOUT, ItemCount, LaneLayout,
        SectionRequirement, read_u16, read_u32,
    },
};

#[derive(Clone, Copy)]
pub(super) struct DirectoryState {
    ordinal: u16,
    previous_kind: Option<u16>,
    pub(super) expected_offset: usize,
}

#[derive(Clone, Copy)]
pub(super) struct ParsedDirectoryEntry {
    pub(super) ordinal: u16,
    pub(super) kind: u16,
    pub(super) requirement: SectionRequirement,
    pub(super) lane: LaneLayout,
}

impl DirectoryState {
    pub(super) const fn first(expected_offset: usize) -> Self {
        Self {
            ordinal: 0,
            previous_kind: None,
            expected_offset,
        }
    }

    pub(super) fn parse(
        self,
        envelope: &[u8],
    ) -> Result<(Self, ParsedDirectoryEntry), FragmentError> {
        let record = HEADER_LAYOUT.encoded_len
            + usize::from(self.ordinal) * DIRECTORY_ENTRY_LAYOUT.encoded_len;
        let kind = read_u16(envelope, record + DIRECTORY_ENTRY_LAYOUT.kind);
        if let Some(previous) = self.previous_kind
            && kind <= previous
        {
            return Err(directory_fault(
                self.ordinal,
                DirectoryFault::Order {
                    previous,
                    actual: kind,
                },
            ));
        }
        let raw_requirement = read_u16(envelope, record + DIRECTORY_ENTRY_LAYOUT.requirement);
        let requirement = SectionRequirement::try_from(raw_requirement).map_err(|actual| {
            directory_fault(self.ordinal, DirectoryFault::Flags { kind, actual })
        })?;
        let count = ItemCount::from(read_u32(
            envelope,
            record + DIRECTORY_ENTRY_LAYOUT.item_count,
        ));
        let start = ByteOffset::from(read_u32(
            envelope,
            record + DIRECTORY_ENTRY_LAYOUT.byte_offset,
        ));
        let length = ByteLength::from(read_u32(
            envelope,
            record + DIRECTORY_ENTRY_LAYOUT.byte_length,
        ));
        let start_index = usize::try_from(start).map_err(|source| FragmentError::WireWidth {
            field: WireField::SectionOffset {
                ordinal: self.ordinal,
            },
            actual: u32::from(start),
            source,
        })?;
        if start_index != self.expected_offset {
            return Err(directory_fault(
                self.ordinal,
                DirectoryFault::Offset {
                    kind,
                    expected: self.expected_offset,
                    actual: u32::from(start),
                },
            ));
        }
        let length_index = usize::try_from(length).map_err(|source| FragmentError::WireWidth {
            field: WireField::SectionByteLength {
                ordinal: self.ordinal,
            },
            actual: u32::from(length),
            source,
        })?;
        let end_index = start_index.checked_add(length_index).ok_or_else(|| {
            directory_fault(
                self.ordinal,
                DirectoryFault::RangeOverflow {
                    kind,
                    start: u32::from(start),
                    length: u32::from(length),
                },
            )
        })?;
        if end_index > envelope.len() {
            return Err(FragmentError::Extent {
                required: end_index,
                actual: envelope.len(),
            });
        }
        let entry = ParsedDirectoryEntry {
            ordinal: self.ordinal,
            kind,
            requirement,
            lane: LaneLayout {
                count,
                start,
                length,
                start_index,
                end_index,
            },
        };
        let next = Self {
            ordinal: self.ordinal + 1,
            previous_kind: Some(kind),
            expected_offset: end_index,
        };
        Ok((next, entry))
    }
}

pub(super) fn directory_fault(ordinal: u16, fault: DirectoryFault) -> FragmentError {
    FragmentError::Directory { ordinal, fault }
}
