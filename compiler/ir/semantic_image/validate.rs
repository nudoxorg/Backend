//! Validation for borrowed portable core semantic images.

use crate::{
    DeclarationIdentity, FactAvailability, ParentageAuthority, Visibility,
};

use super::{
    model::{
        decode_entity, decode_image_facts, get_u16, get_u32, CoreAuthorityFault,
        CoreAuthorityPlane, CoreImageLayout, CoreSemanticImageFault, CoreSemanticImageField, DirectoryKind,
        ATOM_ROW_BYTES, DIRECTORY_BYTES, DIRECTORY_COUNT, ENTITY_ROW_BYTES, HEADER_BYTES, MAGIC,
        SCHEMA,
    },
    view::CoreSemanticImageView,
};

/// Validates a subordinate portable core image and borrows its original bytes.
///
/// This never reinterprets native Rust layouts or allocates a reconstructed
/// owner.  Successful reopening proves only `SemanticCoreReader` coverage.
pub(crate) fn reopen_core_semantic_image(
    bytes: &[u8],
) -> Result<CoreSemanticImageView<'_>, CoreSemanticImageFault> {
    if bytes.len() < HEADER_BYTES {
        return Err(CoreSemanticImageFault::Truncated {
            field: CoreSemanticImageField::Header,
            offset: bytes.len(),
        });
    }
    let magic = super::model::read_array::<4>(bytes, 0, CoreSemanticImageField::Header)?;
    if magic != MAGIC {
        return Err(CoreSemanticImageFault::Magic { expected: MAGIC, observed: magic });
    }
    let schema = get_u16(bytes, 4, CoreSemanticImageField::Header)?;
    if schema != SCHEMA {
        return Err(CoreSemanticImageFault::Schema { expected: SCHEMA, observed: schema });
    }
    let directory_count = get_u16(bytes, 6, CoreSemanticImageField::Directory)?;
    let expected_directory_count = u16::try_from(DIRECTORY_COUNT).map_err(|_| {
        CoreSemanticImageFault::LengthOverflow { field: CoreSemanticImageField::Directory }
    })?;
    if directory_count != expected_directory_count {
        return Err(CoreSemanticImageFault::DirectoryCount {
            expected: expected_directory_count,
            observed: directory_count,
        });
    }
    let claimed = get_u32(bytes, 8, CoreSemanticImageField::Header)?;
    if usize::try_from(claimed).ok() != Some(bytes.len()) {
        return Err(CoreSemanticImageFault::Length { claimed, actual: bytes.len() });
    }
    let directory_offset = get_u32(bytes, 12, CoreSemanticImageField::Directory)?;
    if usize::try_from(directory_offset).ok() != Some(HEADER_BYTES) {
        return Err(CoreSemanticImageFault::DirectoryRange {
            kind: DirectoryKind::Atoms,
            offset: directory_offset,
            length: 0,
            image_bytes: bytes.len(),
        });
    }
    let directory_end = HEADER_BYTES
        .checked_add(DIRECTORY_BYTES.checked_mul(DIRECTORY_COUNT).ok_or(
            CoreSemanticImageFault::LengthOverflow { field: CoreSemanticImageField::Directory },
        )?)
        .ok_or(CoreSemanticImageFault::LengthOverflow { field: CoreSemanticImageField::Directory })?;
    if directory_end > bytes.len() {
        return Err(CoreSemanticImageFault::Truncated {
            field: CoreSemanticImageField::Directory,
            offset: HEADER_BYTES,
        });
    }
    let mut offsets = [(0_usize, 0_usize, 0_u32); DIRECTORY_COUNT];
    let mut expected_offset = directory_end;
    for (index, expected) in DirectoryKind::ALL.iter().copied().enumerate() {
        let offset = HEADER_BYTES + index * DIRECTORY_BYTES;
        let observed = get_u16(bytes, offset, CoreSemanticImageField::Directory)?;
        if observed != expected.code() {
            return Err(CoreSemanticImageFault::DirectoryKind {
                entry: u16::try_from(index).map_err(|_| CoreSemanticImageFault::LengthOverflow {
                    field: CoreSemanticImageField::Directory,
                })?,
                expected,
                observed,
            });
        }
        for reserved in &bytes[offset + 2..offset + 4] {
            if *reserved != 0 {
                return Err(CoreSemanticImageFault::Reserved {
                    field: CoreSemanticImageField::Directory,
                    row: u32::try_from(index).map_err(|_| CoreSemanticImageFault::LengthOverflow {
                        field: CoreSemanticImageField::Directory,
                    })?,
                    observed: *reserved,
                });
            }
        }
        let payload_offset = get_u32(bytes, offset + 4, CoreSemanticImageField::Directory)?;
        let payload_len = get_u32(bytes, offset + 8, CoreSemanticImageField::Directory)?;
        let count = get_u32(bytes, offset + 12, CoreSemanticImageField::Directory)?;
        let payload_offset_usize = usize::try_from(payload_offset).map_err(|_| {
            CoreSemanticImageFault::DirectoryRange {
                kind: expected,
                offset: payload_offset,
                length: payload_len,
                image_bytes: bytes.len(),
            }
        })?;
        let payload_len_usize = usize::try_from(payload_len).map_err(|_| {
            CoreSemanticImageFault::DirectoryRange {
                kind: expected,
                offset: payload_offset,
                length: payload_len,
                image_bytes: bytes.len(),
            }
        })?;
        let end = payload_offset_usize.checked_add(payload_len_usize).ok_or(
            CoreSemanticImageFault::DirectoryRange {
                kind: expected,
                offset: payload_offset,
                length: payload_len,
                image_bytes: bytes.len(),
            },
        )?;
        if payload_offset_usize != expected_offset || end > bytes.len() {
            return Err(CoreSemanticImageFault::DirectoryRange {
                kind: expected,
                offset: payload_offset,
                length: payload_len,
                image_bytes: bytes.len(),
            });
        }
        offsets[index] = (payload_offset_usize, payload_len_usize, count);
        expected_offset = end;
    }
    if expected_offset != bytes.len() {
        return Err(CoreSemanticImageFault::DirectoryRange {
            kind: DirectoryKind::Entities,
            offset: wire_usize(expected_offset, CoreSemanticImageField::Directory)?,
            length: 0,
            image_bytes: bytes.len(),
        });
    }
    let (atoms, atom_rows_len, atom_count) = offsets[0];
    let (atom_bytes, atom_bytes_len, atom_bytes_count) = offsets[1];
    let (entities, entity_rows_len, entity_count) = offsets[2];
    let expected_atom_rows_len = usize::try_from(atom_count)
        .ok()
        .and_then(|count| count.checked_mul(ATOM_ROW_BYTES))
        .ok_or(CoreSemanticImageFault::LengthOverflow { field: CoreSemanticImageField::AtomRange })?;
    if atom_rows_len != expected_atom_rows_len {
        return Err(CoreSemanticImageFault::DirectoryCountLane {
            kind: DirectoryKind::Atoms,
            expected: u32::try_from(expected_atom_rows_len).map_err(|_| CoreSemanticImageFault::LengthOverflow {
                field: CoreSemanticImageField::AtomRange,
            })?,
            observed: u32::try_from(atom_rows_len).map_err(|_| CoreSemanticImageFault::LengthOverflow {
                field: CoreSemanticImageField::AtomRange,
            })?,
        });
    }
    if atom_bytes_count != u32::try_from(atom_bytes_len).map_err(|_| CoreSemanticImageFault::LengthOverflow {
        field: CoreSemanticImageField::AtomRange,
    })? {
        return Err(CoreSemanticImageFault::DirectoryCountLane {
            kind: DirectoryKind::AtomBytes,
            expected: u32::try_from(atom_bytes_len).map_err(|_| CoreSemanticImageFault::LengthOverflow {
                field: CoreSemanticImageField::AtomRange,
            })?,
            observed: atom_bytes_count,
        });
    }
    let expected_entity_rows_len = usize::try_from(entity_count)
        .ok()
        .and_then(|count| count.checked_mul(ENTITY_ROW_BYTES))
        .ok_or(CoreSemanticImageFault::LengthOverflow { field: CoreSemanticImageField::EntityVersion })?;
    if entity_rows_len != expected_entity_rows_len {
        return Err(CoreSemanticImageFault::DirectoryCountLane {
            kind: DirectoryKind::Entities,
            expected: u32::try_from(expected_entity_rows_len).map_err(|_| CoreSemanticImageFault::LengthOverflow {
                field: CoreSemanticImageField::EntityVersion,
            })?,
            observed: u32::try_from(entity_rows_len).map_err(|_| CoreSemanticImageFault::LengthOverflow {
                field: CoreSemanticImageField::EntityVersion,
            })?,
        });
    }
    let layout = CoreImageLayout {
        atom_rows: usize::try_from(atom_count).map_err(|_| CoreSemanticImageFault::LengthOverflow {
            field: CoreSemanticImageField::AtomRange,
        })?,
        atom_bytes: atom_bytes_len,
        entity_rows: usize::try_from(entity_count).map_err(|_| CoreSemanticImageFault::LengthOverflow {
            field: CoreSemanticImageField::EntityVersion,
        })?,
        atoms,
        bytes: atom_bytes,
        entities,
    };
    validate_atoms(bytes, layout)?;
    let image = decode_image_facts(bytes, layout)?;
    validate_entities(bytes, layout)?;
    Ok(CoreSemanticImageView::from_validated(
        bytes,
        layout,
        image,
        atom_count,
        entity_count,
    ))
}

fn validate_atoms(bytes: &[u8], layout: CoreImageLayout) -> Result<(), CoreSemanticImageFault> {
    let atom_bytes = bytes.get(layout.bytes..layout.bytes + layout.atom_bytes).ok_or(
        CoreSemanticImageFault::Truncated { field: CoreSemanticImageField::AtomRange, offset: layout.bytes },
    )?;
    let mut expected_start = 0_usize;
    let mut previous: Option<&[u8]> = None;
    for row in 0..layout.atom_rows {
        let row_wire = u32::try_from(row).map_err(|_| CoreSemanticImageFault::LengthOverflow {
            field: CoreSemanticImageField::AtomRange,
        })?;
        let offset = layout.atoms + row * ATOM_ROW_BYTES;
        let start_raw = get_u32(bytes, offset, CoreSemanticImageField::AtomRange)?;
        let start = usize::try_from(start_raw).map_err(|_| {
            CoreSemanticImageFault::Reference {
                field: CoreSemanticImageField::AtomRange,
                row: row_wire,
                expected: wire_usize(layout.atom_bytes, CoreSemanticImageField::AtomRange)?,
                observed: start_raw,
            }
        })?;
        let length_raw = get_u32(bytes, offset + 4, CoreSemanticImageField::AtomRange)?;
        let length = usize::try_from(length_raw).map_err(|_| {
            CoreSemanticImageFault::Reference {
                field: CoreSemanticImageField::AtomRange,
                row: row_wire,
                expected: wire_usize(layout.atom_bytes, CoreSemanticImageField::AtomRange)?,
                observed: length_raw,
            }
        })?;
        let end = start.checked_add(length).ok_or(CoreSemanticImageFault::Reference {
            field: CoreSemanticImageField::AtomRange,
            row: row_wire,
            expected: wire_usize(layout.atom_bytes, CoreSemanticImageField::AtomRange)?,
            observed: wire_usize(length, CoreSemanticImageField::AtomRange)?,
        })?;
        if start != expected_start || end > atom_bytes.len() {
            return Err(CoreSemanticImageFault::Reference {
                field: CoreSemanticImageField::AtomRange,
                row: row_wire,
                expected: wire_usize(layout.atom_bytes, CoreSemanticImageField::AtomRange)?,
                observed: wire_usize(end, CoreSemanticImageField::AtomRange)?,
            });
        }
        let current = &atom_bytes[start..end];
        if previous.is_some_and(|prior| prior >= current) {
            return Err(CoreSemanticImageFault::CanonicalOrder {
                field: CoreSemanticImageField::AtomOrder,
                previous: row_wire.saturating_sub(1),
                row: row_wire,
            });
        }
        previous = Some(current);
        expected_start = end;
    }
    if expected_start != atom_bytes.len() {
        return Err(CoreSemanticImageFault::Reference {
            field: CoreSemanticImageField::AtomRange,
            row: wire_usize(layout.atom_rows, CoreSemanticImageField::AtomRange)?,
            expected: wire_usize(atom_bytes.len(), CoreSemanticImageField::AtomRange)?,
            observed: wire_usize(expected_start, CoreSemanticImageField::AtomRange)?,
        });
    }
    Ok(())
}

fn validate_entities(bytes: &[u8], layout: CoreImageLayout) -> Result<(), CoreSemanticImageFault> {
    let entity_count = u32::try_from(layout.entity_rows).map_err(|_| CoreSemanticImageFault::LengthOverflow {
        field: CoreSemanticImageField::EntityVersion,
    })?;
    let atom_count = u32::try_from(layout.atom_rows).map_err(|_| CoreSemanticImageFault::LengthOverflow {
        field: CoreSemanticImageField::AtomRange,
    })?;
    let mut previous: Option<DeclarationIdentity> = None;
    for row in 0..entity_count {
        let offset = layout.entities + usize::try_from(row).map_err(|_| CoreSemanticImageFault::LengthOverflow {
            field: CoreSemanticImageField::EntityVersion,
        })? * ENTITY_ROW_BYTES;
        for reserved in bytes[offset + 32..offset + 36]
            .iter()
            .chain(bytes[offset + 116..offset + ENTITY_ROW_BYTES].iter())
        {
            if *reserved != 0 {
                return Err(CoreSemanticImageFault::Reserved {
                    field: CoreSemanticImageField::EntityVersion,
                    row,
                    observed: *reserved,
                });
            }
        }
        validate_entity_malleability(bytes, offset, row)?;
        let entity = decode_entity(bytes, layout, row)?;
        if entity.name.raw >= atom_count {
            return Err(CoreSemanticImageFault::Reference {
                field: CoreSemanticImageField::EntityName,
                row,
                expected: atom_count,
                observed: entity.name.raw,
            });
        }
        if let Some(parent) = entity.parent {
            if parent.raw >= entity_count {
                return Err(CoreSemanticImageFault::Reference {
                    field: CoreSemanticImageField::EntityParent,
                    row,
                    expected: entity_count,
                    observed: parent.raw,
                });
            }
        }
        validate_parent_chain(bytes, layout, row, entity.parent)?;
        if let Some(source) = entity.source {
            if source.file().raw >= atom_count {
                return Err(CoreSemanticImageFault::Reference {
                    field: CoreSemanticImageField::EntitySource,
                    row,
                    expected: atom_count,
                    observed: source.file().raw,
                });
            }
        }
        validate_authority(bytes, layout, row, entity)?;
        if entity.visibility != Visibility::Unknown
            && entity.authority.visibility != FactAvailability::Captured
        {
            return Err(CoreSemanticImageFault::Authority {
                row,
                cause: CoreAuthorityFault::Availability {
                    plane: CoreAuthorityPlane::Visibility,
                    claimed: entity.authority.visibility,
                    present: true,
                },
            });
        }
        if let Some(prior) = previous {
            if prior >= entity.version.identity() {
                if prior == entity.version.identity() {
                    return Err(CoreSemanticImageFault::DuplicateIdentity { row, existing: row - 1 });
                }
                return Err(CoreSemanticImageFault::CanonicalOrder {
                    field: CoreSemanticImageField::EntityVersion,
                    previous: row - 1,
                    row,
                });
            }
        }
        previous = Some(entity.version.identity());
    }
    Ok(())
}

fn validate_parent_chain(
    bytes: &[u8],
    layout: CoreImageLayout,
    entity: u32,
    mut parent: Option<crate::EntityId>,
) -> Result<(), CoreSemanticImageFault> {
    let limit = u32::try_from(layout.entity_rows).map_err(|_| CoreSemanticImageFault::LengthOverflow {
        field: CoreSemanticImageField::EntityParent,
    })?;
    let mut steps = 0_u32;
    while let Some(current) = parent {
        if current.raw == entity || steps >= limit {
            return Err(CoreSemanticImageFault::ParentCycle {
                entity,
                parent: current.raw,
            });
        }
        parent = decode_entity(bytes, layout, current.raw)?.parent;
        steps = steps.checked_add(1).ok_or(CoreSemanticImageFault::LengthOverflow {
            field: CoreSemanticImageField::EntityParent,
        })?;
    }
    Ok(())
}

fn validate_authority(
    bytes: &[u8],
    layout: CoreImageLayout,
    row: u32,
    entity: crate::CoreSemanticEntity,
) -> Result<(), CoreSemanticImageFault> {
    let offset = layout.entities + usize::try_from(row).map_err(|_| CoreSemanticImageFault::LengthOverflow {
        field: CoreSemanticImageField::EntityVersion,
    })? * ENTITY_ROW_BYTES;
    let local_parent = entity.parent.map(|parent| decode_entity(bytes, layout, parent.raw).map(|value| value.version.identity())).transpose()?;
    let parentage_matches = match entity.authority.parentage {
        ParentageAuthority::Root => local_parent.is_none(),
        ParentageAuthority::Bound(parent) => local_parent == Some(parent),
        ParentageAuthority::UnrepresentedAuthorityOwner(_) | ParentageAuthority::Unavailable => local_parent.is_none(),
    };
    if !parentage_matches {
        return Err(CoreSemanticImageFault::Authority {
            row,
            cause: CoreAuthorityFault::Parentage {
                parentage: bytes[offset + 7],
                parent: get_u32(bytes, offset + 8, CoreSemanticImageField::EntityParent)?,
            },
        });
    }
    let has_source = entity.source.is_some();
    if (entity.authority.source == FactAvailability::Captured) != has_source
        || entity.authority.source != entity.authority.source_file
    {
        return Err(CoreSemanticImageFault::Authority {
            row,
            cause: CoreAuthorityFault::SourceAvailability {
                source: bytes[offset + 24],
                source_file: bytes[offset + 25],
                has_source,
            },
        });
    }
    Ok(())
}

fn validate_entity_malleability(
    bytes: &[u8],
    offset: usize,
    row: u32,
) -> Result<(), CoreSemanticImageFault> {
    let source_file = get_u32(bytes, offset + 12, CoreSemanticImageField::EntitySource)?;
    if source_file == super::model::NONE
        && (get_u32(bytes, offset + 16, CoreSemanticImageField::EntitySource)? != 0
            || get_u32(bytes, offset + 20, CoreSemanticImageField::EntitySource)? != 0)
    {
        for observed in &bytes[offset + 16..offset + 24] {
            if *observed != 0 {
                return Err(CoreSemanticImageFault::Reserved {
                    field: CoreSemanticImageField::EntitySource,
                    row,
                    observed: *observed,
                });
            }
        }
    }
    match bytes[offset + 7] {
        0 | 3 => {
            for value in &bytes[offset + 84..offset + 116] {
                if *value != 0 {
                    return Err(CoreSemanticImageFault::Reserved {
                        field: CoreSemanticImageField::EntityParentage,
                        row,
                        observed: *value,
                    });
                }
            }
        }
        2 => {
            for value in &bytes[offset + 100..offset + 116] {
                if *value != 0 {
                    return Err(CoreSemanticImageFault::Reserved {
                        field: CoreSemanticImageField::EntityParentage,
                        row,
                        observed: *value,
                    });
                }
            }
        }
        1 => {}
        _ => {}
    }
    Ok(())
}

fn wire_usize(
    value: usize,
    field: CoreSemanticImageField,
) -> Result<u32, CoreSemanticImageFault> {
    u32::try_from(value).map_err(|_| CoreSemanticImageFault::LengthOverflow { field })
}
