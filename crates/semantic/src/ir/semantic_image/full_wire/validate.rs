//! Structural full-image admission.
//!
//! Validation has no allocation and no fallback interpretation.  It proves
//! the directory, canonical remaps, row alignments, generic typed endpoint
//! graph, terminal pools, graph evidence, and sparse extension bindings before
//! a future borrowed `SemanticReader` may hold the source bytes.

#[path = "validate_extensions.rs"]
mod extensions;
#[path = "validate_graph.rs"]
mod graph;
#[path = "validate_header.rs"]
mod header;
#[path = "validate_typed.rs"]
mod typed;

#[path = "validate/entities.rs"]
mod entities;

use entities::{validate_entities, validate_terminal_lists};
pub(super) use entities::availability_match;

use crate::ir::{FactAvailability, ParentageAuthority};

use super::{
    decode,
    fault::{FullSemanticImageError, FullSemanticImageFault, FullSemanticImageField},
    wire::{
        ATOM_ROW_BYTES, ENTITY_ROW_BYTES, FullDirectoryEntry, FullDirectoryKind, FullImageLayout,
        NONE, RANGE_ROW_BYTES, get_u32,
    },
};

/// Proven typed-node domain spans.  The rows remain in the input bytes; these
/// eight narrow ranges let a future view resolve `TypeId`/typed list IDs in
/// O(1) without an owned index or a raw global-node coordinate.
#[derive(Clone, Copy, Debug)]
pub(crate) struct TypedLayout {
    pub(crate) starts: [u32; 9],
    pub(crate) counts: [u32; 9],
}

impl TypedLayout {
    pub(crate) const fn count(self, domain: u8) -> Option<u32> {
        match domain {
            0 => Some(self.counts[0]),
            1 => Some(self.counts[1]),
            2 => Some(self.counts[2]),
            3 => Some(self.counts[3]),
            4 => Some(self.counts[4]),
            5 => Some(self.counts[5]),
            6 => Some(self.counts[6]),
            7 => Some(self.counts[7]),
            8 => Some(self.counts[8]),
            _ => None,
        }
    }

    pub(crate) const fn start(self, domain: u8) -> Option<u32> {
        match domain {
            0 => Some(self.starts[0]),
            1 => Some(self.starts[1]),
            2 => Some(self.starts[2]),
            3 => Some(self.starts[3]),
            4 => Some(self.starts[4]),
            5 => Some(self.starts[5]),
            6 => Some(self.starts[6]),
            7 => Some(self.starts[7]),
            8 => Some(self.starts[8]),
            _ => None,
        }
    }
}

/// All non-owning facts proved by full-image admission.
#[derive(Clone, Copy, Debug)]
pub(crate) struct ValidatedFullImage {
    pub(crate) layout: FullImageLayout,
    pub(crate) image: crate::ir::SemanticImageFacts,
    pub(crate) typed: TypedLayout,
}

pub(crate) fn reopen_full_semantic_image(
    bytes: &[u8],
) -> Result<ValidatedFullImage, FullSemanticImageError> {
    let layout = header::directory(bytes)?;
    header::validate_lane_widths(layout)?;
    header::validate_atoms(bytes, layout)?;
    let header_layout = header::image_layout(layout)?;
    let image = super::super::decode::decode_image_facts(bytes, header_layout)?;
    let typed = typed::validate_typed(bytes, layout)?;
    super::typed_decode::validate_semantic_nodes(bytes, layout, typed)?;
    validate_entities(bytes, layout, typed)?;
    validate_terminal_lists(bytes, layout, typed)?;
    graph::validate_externals(bytes, layout)?;
    graph::validate_graph(bytes, layout)?;
    extensions::validate_extensions(bytes, layout, typed, image.authority)?;
    Ok(ValidatedFullImage {
        layout,
        image,
        typed,
    })
}

fn validate_ranges(
    bytes: &[u8],
    layout: FullImageLayout,
    range_kind: FullDirectoryKind,
    bytes_kind: FullDirectoryKind,
    field: FullSemanticImageField,
) -> Result<(), FullSemanticImageFault> {
    let ranges = layout.entry(range_kind);
    let values = layout.entry(bytes_kind);
    let mut expected = 0_usize;
    for row in 0..ranges.count {
        let offset = ranges.offset
            + usize::try_from(row).map_err(|_| FullSemanticImageFault::LengthOverflow { field })?
                * RANGE_ROW_BYTES;
        let start_wire = get_u32(bytes, offset, field)?;
        let start = usize::try_from(start_wire).map_err(|_| FullSemanticImageFault::Reference {
            field,
            row,
            expected: values.count,
            observed: start_wire,
        })?;
        let length_wire = get_u32(bytes, offset + 4, field)?;
        let length =
            usize::try_from(length_wire).map_err(|_| FullSemanticImageFault::Reference {
                field,
                row,
                expected: values.count,
                observed: length_wire,
            })?;
        let end = start
            .checked_add(length)
            .ok_or(FullSemanticImageFault::LengthOverflow { field })?;
        if start != expected || end > values.length {
            return Err(FullSemanticImageFault::Reference {
                field,
                row,
                expected: values.count,
                observed: wire(end, field)?,
            });
        }
        expected = end;
    }
    if expected != values.length {
        return Err(FullSemanticImageFault::Reference {
            field,
            row: ranges.count,
            expected: values.count,
            observed: wire(expected, field)?,
        });
    }
    Ok(())
}

pub(crate) use extensions::extension_value;

pub(crate) fn range_value<'bytes>(
    bytes: &'bytes [u8],
    ranges: FullDirectoryEntry,
    values: FullDirectoryEntry,
    row: u32,
    field: FullSemanticImageField,
) -> Result<&'bytes [u8], FullSemanticImageFault> {
    if row >= ranges.count {
        return Err(FullSemanticImageFault::Reference {
            field,
            row,
            expected: ranges.count,
            observed: row,
        });
    }
    let offset = ranges.offset
        + usize::try_from(row).map_err(|_| FullSemanticImageFault::LengthOverflow { field })?
            * RANGE_ROW_BYTES;
    let start = usize::try_from(get_u32(bytes, offset, field)?)
        .map_err(|_| FullSemanticImageFault::LengthOverflow { field })?;
    let length = usize::try_from(get_u32(bytes, offset + 4, field)?)
        .map_err(|_| FullSemanticImageFault::LengthOverflow { field })?;
    let start = values
        .offset
        .checked_add(start)
        .ok_or(FullSemanticImageFault::LengthOverflow { field })?;
    let end = start
        .checked_add(length)
        .ok_or(FullSemanticImageFault::LengthOverflow { field })?;
    bytes
        .get(start..end)
        .ok_or(FullSemanticImageFault::Truncated {
            field,
            offset: start,
        })
}

pub(crate) fn atom_value<'bytes>(
    bytes: &'bytes [u8],
    layout: FullImageLayout,
    atom: u32,
) -> Result<&'bytes [u8], FullSemanticImageFault> {
    let rows = layout.entry(FullDirectoryKind::Atoms);
    let values = layout.entry(FullDirectoryKind::AtomBytes);
    reference(FullSemanticImageField::Atoms, atom, rows.count, atom)?;
    let offset = rows.offset
        + usize::try_from(atom).map_err(|_| FullSemanticImageFault::LengthOverflow {
            field: FullSemanticImageField::Atoms,
        })? * ATOM_ROW_BYTES;
    let start =
        usize::try_from(get_u32(bytes, offset, FullSemanticImageField::Atoms)?).map_err(|_| {
            FullSemanticImageFault::LengthOverflow {
                field: FullSemanticImageField::Atoms,
            }
        })?;
    let length = usize::try_from(get_u32(bytes, offset + 4, FullSemanticImageField::Atoms)?)
        .map_err(|_| FullSemanticImageFault::LengthOverflow {
            field: FullSemanticImageField::Atoms,
        })?;
    bytes
        .get(values.offset + start..values.offset + start + length)
        .ok_or(FullSemanticImageFault::Truncated {
            field: FullSemanticImageField::Atoms,
            offset: values.offset + start,
        })
}

fn read_value_u32(
    value: &[u8],
    offset: usize,
    row: u32,
    field: FullSemanticImageField,
) -> Result<u32, FullSemanticImageFault> {
    let bytes = value
        .get(offset..offset + 4)
        .ok_or(FullSemanticImageFault::Truncated { field, offset })?;
    let raw = <[u8; 4]>::try_from(bytes)
        .map_err(|_| FullSemanticImageFault::Truncated { field, offset })?;
    let _ = row;
    Ok(u32::from_le_bytes(raw))
}

pub(super) fn reserve(
    bytes: &[u8],
    start: usize,
    end: usize,
    field: FullSemanticImageField,
    row: u32,
) -> Result<(), FullSemanticImageFault> {
    for value in bytes
        .get(start..end)
        .ok_or(FullSemanticImageFault::Truncated {
            field,
            offset: start,
        })?
    {
        if *value != 0 {
            return Err(FullSemanticImageFault::Reserved {
                field,
                row,
                observed: *value,
            });
        }
    }
    Ok(())
}

pub(super) fn reference(
    field: FullSemanticImageField,
    row: u32,
    count: u32,
    observed: u32,
) -> Result<(), FullSemanticImageFault> {
    if observed >= count {
        return Err(FullSemanticImageFault::Reference {
            field,
            row,
            expected: count,
            observed,
        });
    }
    Ok(())
}

pub(super) fn wire(
    value: usize,
    field: FullSemanticImageField,
) -> Result<u32, FullSemanticImageFault> {
    u32::try_from(value).map_err(|_| FullSemanticImageFault::LengthOverflow { field })
}

pub(super) const fn availability_code(value: FactAvailability) -> u8 {
    match value {
        FactAvailability::Unavailable => 0,
        FactAvailability::Captured => 1,
    }
}
pub(super) const fn confidence_code(value: crate::ir::Confidence) -> u8 {
    match value {
        crate::ir::Confidence::Syntactic => 0,
        crate::ir::Confidence::Heuristic => 1,
        crate::ir::Confidence::Indexed => 2,
        crate::ir::Confidence::Imported => 3,
        crate::ir::Confidence::Compiler => 4,
    }
}
pub(super) const fn link_kind_code(value: crate::ir::LinkKind) -> u8 {
    match value {
        crate::ir::LinkKind::Calls => 0,
        crate::ir::LinkKind::MethodCall => 1,
        crate::ir::LinkKind::TypeReference => 2,
        crate::ir::LinkKind::Reads => 3,
        crate::ir::LinkKind::Writes => 4,
        crate::ir::LinkKind::Imports => 5,
        crate::ir::LinkKind::Implements => 6,
        crate::ir::LinkKind::Overrides => 7,
        crate::ir::LinkKind::Reexports => 8,
        crate::ir::LinkKind::Inherits => 9,
        crate::ir::LinkKind::Documents => 10,
    }
}
const fn parentage_code(value: ParentageAuthority) -> u8 {
    match value {
        ParentageAuthority::Root => 0,
        ParentageAuthority::Bound(_) => 1,
        ParentageAuthority::UnrepresentedAuthorityOwner(_) => 2,
        ParentageAuthority::Unavailable => 3,
    }
}
