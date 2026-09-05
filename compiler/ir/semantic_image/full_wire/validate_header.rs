//! Header, directory, and atom-plane admission for the full image.

use super::super::{
    decode,
    fault::{FullSemanticImageFault, FullSemanticImageField},
    wire::{
        ATOM_ROW_BYTES, DIRECTORY_BYTES, ENTITY_ROW_BYTES, EXTERNAL_ROW_BYTES, FullDirectoryEntry,
        FullDirectoryKind, FullImageLayout, HEADER_BYTES, LINK_ROW_BYTES, MAGIC,
        OCCURRENCE_ROW_BYTES, RANGE_ROW_BYTES, SCHEMA, SPARSE_BINDING_ROW_BYTES,
        TYPED_EDGE_ROW_BYTES, TYPED_NODE_ROW_BYTES, get_u16, get_u32, read_array,
    },
};

use super::{extensions::extension_pairs, wire};

pub(super) fn directory(bytes: &[u8]) -> Result<FullImageLayout, FullSemanticImageFault> {
    let observed = read_array::<4>(bytes, 0, FullSemanticImageField::Header)?;
    if observed != MAGIC {
        return Err(FullSemanticImageFault::Magic {
            expected: MAGIC,
            observed,
        });
    }
    let schema = get_u16(bytes, 4, FullSemanticImageField::Header)?;
    if schema != SCHEMA {
        return Err(FullSemanticImageFault::Schema {
            expected: SCHEMA,
            observed: schema,
        });
    }
    let directory_count = get_u16(bytes, 6, FullSemanticImageField::Directory)?;
    if directory_count != FullDirectoryKind::count() {
        return Err(FullSemanticImageFault::DirectoryCount {
            expected: FullDirectoryKind::count(),
            observed: directory_count,
        });
    }
    let claimed = get_u32(bytes, 8, FullSemanticImageField::Header)?;
    if usize::try_from(claimed).ok() != Some(bytes.len()) {
        return Err(FullSemanticImageFault::Length {
            claimed,
            actual: bytes.len(),
        });
    }
    let directory_offset = get_u32(bytes, 12, FullSemanticImageField::Directory)?;
    if usize::try_from(directory_offset).ok() != Some(HEADER_BYTES) {
        return Err(FullSemanticImageFault::DirectoryRange {
            kind: FullDirectoryKind::Atoms,
            offset: directory_offset,
            length: 0,
            image_bytes: bytes.len(),
        });
    }
    let entries_bytes = DIRECTORY_BYTES
        .checked_mul(usize::from(FullDirectoryKind::count()))
        .ok_or(FullSemanticImageFault::LengthOverflow {
            field: FullSemanticImageField::Directory,
        })?;
    let mut expected_offset =
        HEADER_BYTES
            .checked_add(entries_bytes)
            .ok_or(FullSemanticImageFault::LengthOverflow {
                field: FullSemanticImageField::Directory,
            })?;
    let mut entries = [FullDirectoryEntry {
        offset: 0,
        length: 0,
        offset_wire: 0,
        length_wire: 0,
        count: 0,
    }; 26];
    for kind in FullDirectoryKind::ALL {
        let index = kind.index();
        let offset = HEADER_BYTES + index * DIRECTORY_BYTES;
        let observed_kind = get_u16(bytes, offset, FullSemanticImageField::Directory)?;
        if observed_kind != kind.code() {
            return Err(FullSemanticImageFault::DirectoryKind {
                entry: u16::try_from(index).map_err(|_| {
                    FullSemanticImageFault::LengthOverflow {
                        field: FullSemanticImageField::Directory,
                    }
                })?,
                expected: kind,
                observed: observed_kind,
            });
        }
        for value in &bytes[offset + 2..offset + 4] {
            if *value != 0 {
                return Err(FullSemanticImageFault::Reserved {
                    field: FullSemanticImageField::Directory,
                    row: u32::try_from(index).map_err(|_| {
                        FullSemanticImageFault::LengthOverflow {
                            field: FullSemanticImageField::Directory,
                        }
                    })?,
                    observed: *value,
                });
            }
        }
        let offset_wire = get_u32(bytes, offset + 4, FullSemanticImageField::Directory)?;
        let length_wire = get_u32(bytes, offset + 8, FullSemanticImageField::Directory)?;
        let count = get_u32(bytes, offset + 12, FullSemanticImageField::Directory)?;
        let payload_offset =
            usize::try_from(offset_wire).map_err(|_| FullSemanticImageFault::DirectoryRange {
                kind,
                offset: offset_wire,
                length: length_wire,
                image_bytes: bytes.len(),
            })?;
        let length =
            usize::try_from(length_wire).map_err(|_| FullSemanticImageFault::DirectoryRange {
                kind,
                offset: offset_wire,
                length: length_wire,
                image_bytes: bytes.len(),
            })?;
        let end =
            payload_offset
                .checked_add(length)
                .ok_or(FullSemanticImageFault::DirectoryRange {
                    kind,
                    offset: offset_wire,
                    length: length_wire,
                    image_bytes: bytes.len(),
                })?;
        if payload_offset != expected_offset || end > bytes.len() {
            return Err(FullSemanticImageFault::DirectoryRange {
                kind,
                offset: offset_wire,
                length: length_wire,
                image_bytes: bytes.len(),
            });
        }
        entries[index] = FullDirectoryEntry {
            offset: payload_offset,
            length,
            offset_wire,
            length_wire,
            count,
        };
        expected_offset = end;
    }
    if expected_offset != bytes.len() {
        return Err(FullSemanticImageFault::DirectoryRange {
            kind: FullDirectoryKind::ClangBindings,
            offset: wire(expected_offset, FullSemanticImageField::Directory)?,
            length: 0,
            image_bytes: bytes.len(),
        });
    }
    Ok(FullImageLayout { entries })
}

pub(super) fn validate_lane_widths(layout: FullImageLayout) -> Result<(), FullSemanticImageFault> {
    fixed(layout, FullDirectoryKind::Atoms, ATOM_ROW_BYTES)?;
    fixed(layout, FullDirectoryKind::Entities, ENTITY_ROW_BYTES)?;
    fixed(layout, FullDirectoryKind::TypedNodes, TYPED_NODE_ROW_BYTES)?;
    fixed(layout, FullDirectoryKind::TypedEdges, TYPED_EDGE_ROW_BYTES)?;
    fixed(layout, FullDirectoryKind::EntityLists, RANGE_ROW_BYTES)?;
    fixed(layout, FullDirectoryKind::Documentation, RANGE_ROW_BYTES)?;
    fixed(layout, FullDirectoryKind::Externals, EXTERNAL_ROW_BYTES)?;
    fixed(layout, FullDirectoryKind::Links, LINK_ROW_BYTES)?;
    fixed(layout, FullDirectoryKind::Occurrences, OCCURRENCE_ROW_BYTES)?;
    for (facts, bindings) in extension_pairs() {
        let entry = layout.entry(facts);
        let ranges = usize::try_from(entry.count)
            .map_err(|_| FullSemanticImageFault::LengthOverflow {
                field: FullSemanticImageField::ExtensionFacts,
            })?
            .checked_mul(RANGE_ROW_BYTES)
            .ok_or(FullSemanticImageFault::LengthOverflow {
                field: FullSemanticImageField::ExtensionFacts,
            })?;
        if entry.length < ranges {
            return Err(FullSemanticImageFault::DirectoryCountLane {
                kind: facts,
                expected: wire(ranges, FullSemanticImageField::ExtensionFacts)?,
                observed: entry.length_wire,
            });
        }
        fixed(layout, bindings, SPARSE_BINDING_ROW_BYTES)?;
    }
    if layout.entry(FullDirectoryKind::AtomBytes).count
        != layout.entry(FullDirectoryKind::AtomBytes).length_wire
        || layout.entry(FullDirectoryKind::EntityListBytes).count
            != layout.entry(FullDirectoryKind::EntityListBytes).length_wire
        || layout.entry(FullDirectoryKind::DocumentationBytes).count
            != layout
                .entry(FullDirectoryKind::DocumentationBytes)
                .length_wire
    {
        return Err(FullSemanticImageFault::DirectoryCountLane {
            kind: FullDirectoryKind::AtomBytes,
            expected: layout.entry(FullDirectoryKind::AtomBytes).length_wire,
            observed: layout.entry(FullDirectoryKind::AtomBytes).count,
        });
    }
    Ok(())
}

fn fixed(
    layout: FullImageLayout,
    kind: FullDirectoryKind,
    width: usize,
) -> Result<(), FullSemanticImageFault> {
    let entry = layout.entry(kind);
    let expected = usize::try_from(entry.count)
        .map_err(|_| FullSemanticImageFault::LengthOverflow {
            field: decode::field_for(kind),
        })?
        .checked_mul(width)
        .ok_or(FullSemanticImageFault::LengthOverflow {
            field: decode::field_for(kind),
        })?;
    if entry.length != expected {
        return Err(FullSemanticImageFault::DirectoryCountLane {
            kind,
            expected: wire(expected, decode::field_for(kind))?,
            observed: entry.length_wire,
        });
    }
    Ok(())
}

/// The shared image header references the canonical full atom/entity lanes.
/// It is not a second image layout or reopen capability.
pub(super) fn image_layout(
    layout: FullImageLayout,
) -> Result<super::super::super::decode::ImageHeaderLayout, FullSemanticImageFault> {
    let atoms = layout.entry(FullDirectoryKind::Atoms);
    let values = layout.entry(FullDirectoryKind::AtomBytes);
    Ok(super::super::super::decode::ImageHeaderLayout {
        atom_rows: usize::try_from(atoms.count).map_err(|_| {
            FullSemanticImageFault::LengthOverflow {
                field: FullSemanticImageField::Atoms,
            }
        })?,
        atom_bytes: values.length,
        atoms: atoms.offset,
        bytes: values.offset,
    })
}

pub(super) fn validate_atoms(
    bytes: &[u8],
    layout: FullImageLayout,
) -> Result<(), FullSemanticImageFault> {
    let rows = layout.entry(FullDirectoryKind::Atoms);
    let values = layout.entry(FullDirectoryKind::AtomBytes);
    let mut next = 0_usize;
    let mut previous: Option<&[u8]> = None;
    for row in 0..rows.count {
        let offset = rows.offset
            + usize::try_from(row).map_err(|_| FullSemanticImageFault::LengthOverflow {
                field: FullSemanticImageField::Atoms,
            })? * ATOM_ROW_BYTES;
        let start_wire = get_u32(bytes, offset, FullSemanticImageField::Atoms)?;
        let start = usize::try_from(start_wire).map_err(|_| FullSemanticImageFault::Reference {
            field: FullSemanticImageField::Atoms,
            row,
            expected: values.count,
            observed: start_wire,
        })?;
        let length_wire = get_u32(bytes, offset + 4, FullSemanticImageField::Atoms)?;
        let length =
            usize::try_from(length_wire).map_err(|_| FullSemanticImageFault::Reference {
                field: FullSemanticImageField::Atoms,
                row,
                expected: values.count,
                observed: length_wire,
            })?;
        let end = start
            .checked_add(length)
            .ok_or(FullSemanticImageFault::Reference {
                field: FullSemanticImageField::Atoms,
                row,
                expected: values.count,
                observed: start_wire,
            })?;
        if start != next || end > values.length {
            return Err(FullSemanticImageFault::Reference {
                field: FullSemanticImageField::Atoms,
                row,
                expected: values.count,
                observed: wire(end, FullSemanticImageField::Atoms)?,
            });
        }
        let current = bytes
            .get(values.offset + start..values.offset + end)
            .ok_or(FullSemanticImageFault::Truncated {
                field: FullSemanticImageField::Atoms,
                offset: values.offset + start,
            })?;
        if previous.is_some_and(|value| value >= current) {
            return Err(FullSemanticImageFault::CanonicalOrder {
                field: FullSemanticImageField::Atoms,
                previous: row - 1,
                row,
            });
        }
        previous = Some(current);
        next = end;
    }
    if next != values.length {
        return Err(FullSemanticImageFault::Reference {
            field: FullSemanticImageField::Atoms,
            row: rows.count,
            expected: values.count,
            observed: wire(next, FullSemanticImageField::Atoms)?,
        });
    }
    Ok(())
}
