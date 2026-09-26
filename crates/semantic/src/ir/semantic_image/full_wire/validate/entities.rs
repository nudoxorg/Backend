//! Entity-row and terminal-list admission for a full semantic image.
//!
//! Directory, typed, and graph checks stay beside this module. These functions
//! prove entity rows, parent chains, list pools, and documentation bytes.

use core::cmp::Ordering;

use super::{
    ENTITY_ROW_BYTES, FactAvailability, FullDirectoryKind, FullImageLayout, FullSemanticImageFault,
    FullSemanticImageField, NONE, ParentageAuthority, TypedLayout, atom_value, availability_code,
    decode, get_u32, parentage_code, range_value, read_value_u32, reference, reserve,
    validate_ranges, wire,
};

/// Proves every entity row, parent chain, and authority plane.
pub(super) fn validate_entities(
    bytes: &[u8],
    layout: FullImageLayout,
    typed: TypedLayout,
) -> Result<(), FullSemanticImageFault> {
    let entities = layout.entry(FullDirectoryKind::Entities);
    let atoms = layout.entry(FullDirectoryKind::Atoms).count;
    let mut previous: Option<crate::ir::DeclarationIdentity> = None;
    for row in 0..entities.count {
        let entity = decode::entity(bytes, layout, row)?;
        validate_entity_reserved(bytes, layout, row)?;
        reference(
            FullSemanticImageField::Entities,
            row,
            atoms,
            entity.name.raw,
        )?;
        if let Some(parent) = entity.parent {
            reference(
                FullSemanticImageField::Entities,
                row,
                entities.count,
                parent.raw,
            )?;
        }
        if let Some(source) = entity.source {
            reference(
                FullSemanticImageField::Entities,
                row,
                atoms,
                source.file().raw,
            )?;
        }
        if let Some(ty) = entity.semantic_type {
            let type_count = typed.count(0).ok_or(FullSemanticImageFault::TypedDomain {
                node: row,
                domain: 0,
            })?;
            reference(FullSemanticImageField::TypedNodes, row, type_count, ty.raw)?;
        }
        reference(
            FullSemanticImageField::EntityLists,
            row,
            layout.entry(FullDirectoryKind::EntityLists).count,
            entity.members.raw,
        )?;
        reference(
            FullSemanticImageField::Documentation,
            row,
            layout.entry(FullDirectoryKind::Documentation).count,
            entity.docs.raw,
        )?;
        reference(
            FullSemanticImageField::TypedNodes,
            row,
            typed.count(5).ok_or(FullSemanticImageFault::TypedDomain {
                node: row,
                domain: 5,
            })?,
            entity.attributes.raw,
        )?;
        validate_parent_chain(bytes, layout, row, entity.parent)?;
        validate_entity_authority(bytes, layout, row, entity)?;
        if let Some(previous) = previous {
            match previous.cmp(&entity.version.identity()) {
                Ordering::Less => {}
                Ordering::Equal => {
                    return Err(FullSemanticImageFault::DuplicateIdentity {
                        row,
                        existing: row - 1,
                    });
                }
                Ordering::Greater => {
                    return Err(FullSemanticImageFault::CanonicalOrder {
                        field: FullSemanticImageField::Entities,
                        previous: row - 1,
                        row,
                    });
                }
            }
        }
        previous = Some(entity.version.identity());
    }
    Ok(())
}

fn validate_entity_reserved(
    bytes: &[u8],
    layout: FullImageLayout,
    row: u32,
) -> Result<(), FullSemanticImageFault> {
    let entry = layout.entry(FullDirectoryKind::Entities);
    let offset = entry.offset
        + usize::try_from(row).map_err(|_| FullSemanticImageFault::LengthOverflow {
            field: FullSemanticImageField::Entities,
        })? * ENTITY_ROW_BYTES;
    for value in bytes[offset + 32..offset + 36]
        .iter()
        .chain(bytes[offset + 116..offset + 120].iter())
    {
        if *value != 0 {
            return Err(FullSemanticImageFault::Reserved {
                field: FullSemanticImageField::Entities,
                row,
                observed: *value,
            });
        }
    }
    let source_file = get_u32(bytes, offset + 12, FullSemanticImageField::Entities)?;
    if source_file == NONE {
        for value in &bytes[offset + 16..offset + 24] {
            if *value != 0 {
                return Err(FullSemanticImageFault::Reserved {
                    field: FullSemanticImageField::Entities,
                    row,
                    observed: *value,
                });
            }
        }
    }
    match bytes[offset + 7] {
        0 | 3 => reserve(
            bytes,
            offset + 84,
            offset + 116,
            FullSemanticImageField::Entities,
            row,
        )?,
        1 => {}
        2 => reserve(
            bytes,
            offset + 100,
            offset + 116,
            FullSemanticImageField::Entities,
            row,
        )?,
        _ => {}
    }
    Ok(())
}

fn validate_parent_chain(
    bytes: &[u8],
    layout: FullImageLayout,
    entity: u32,
    mut parent: Option<crate::ir::EntityId>,
) -> Result<(), FullSemanticImageFault> {
    let limit = layout.entry(FullDirectoryKind::Entities).count;
    let mut steps = 0_u32;
    while let Some(current) = parent {
        if current.raw == entity || steps >= limit {
            return Err(FullSemanticImageFault::ParentCycle {
                entity,
                parent: current.raw,
            });
        }
        parent = decode::entity(bytes, layout, current.raw)?.parent;
        steps = steps
            .checked_add(1)
            .ok_or(FullSemanticImageFault::LengthOverflow {
                field: FullSemanticImageField::Entities,
            })?;
    }
    Ok(())
}

fn validate_entity_authority(
    bytes: &[u8],
    layout: FullImageLayout,
    row: u32,
    entity: crate::ir::SemanticEntity,
) -> Result<(), FullSemanticImageFault> {
    let parent_identity = entity
        .parent
        .map(|parent| {
            decode::entity(bytes, layout, parent.raw).map(|value| value.version.identity())
        })
        .transpose()?;
    let parentage_matches = match entity.authority.parentage {
        ParentageAuthority::Root => parent_identity.is_none(),
        ParentageAuthority::Bound(parent) => parent_identity == Some(parent),
        ParentageAuthority::UnrepresentedAuthorityOwner(_) | ParentageAuthority::Unavailable => {
            parent_identity.is_none()
        }
    };
    if !parentage_matches {
        return Err(FullSemanticImageFault::Authority {
            row,
            plane: 0,
            claimed: parentage_code(entity.authority.parentage),
            present: parent_identity.is_some(),
        });
    }
    availability_match(row, 1, entity.authority.source, entity.source.is_some())?;
    availability_match(
        row,
        2,
        entity.authority.source_file,
        entity.source.is_some(),
    )?;
    if entity.authority.source != entity.authority.source_file {
        return Err(FullSemanticImageFault::Authority {
            row,
            plane: 2,
            claimed: availability_code(entity.authority.source_file),
            present: entity.source.is_some(),
        });
    }
    availability_match(
        row,
        3,
        entity.authority.semantic_type,
        entity.semantic_type.is_some(),
    )?;
    if entity.visibility != crate::ir::Visibility::Unknown {
        availability_match(row, 6, entity.authority.visibility, true)?;
    }
    Ok(())
}

/// Proves a captured-empty fact is present exactly when its plane says so.
pub(in crate::ir::semantic_image::full_wire) fn availability_match(
    row: u32,
    plane: u8,
    claimed: FactAvailability,
    present: bool,
) -> Result<(), FullSemanticImageFault> {
    if matches!(claimed, FactAvailability::Captured) != present {
        return Err(FullSemanticImageFault::Authority {
            row,
            plane,
            claimed: availability_code(claimed),
            present,
        });
    }
    Ok(())
}

/// Proves terminal list pools, documentation, and sparse entity authority.
pub(super) fn validate_terminal_lists(
    bytes: &[u8],
    layout: FullImageLayout,
    typed: TypedLayout,
) -> Result<(), FullSemanticImageFault> {
    validate_ranges(
        bytes,
        layout,
        FullDirectoryKind::EntityLists,
        FullDirectoryKind::EntityListBytes,
        FullSemanticImageField::EntityLists,
    )?;
    validate_ranges(
        bytes,
        layout,
        FullDirectoryKind::Documentation,
        FullDirectoryKind::DocumentationBytes,
        FullSemanticImageField::Documentation,
    )?;
    let entities = layout.entry(FullDirectoryKind::Entities).count;
    let members = layout.entry(FullDirectoryKind::EntityLists);
    let member_bytes = layout.entry(FullDirectoryKind::EntityListBytes);
    for row in 0..members.count {
        let value = range_value(
            bytes,
            members,
            member_bytes,
            row,
            FullSemanticImageField::EntityLists,
        )?;
        if value.len() < 5 {
            return Err(FullSemanticImageFault::Reference {
                field: FullSemanticImageField::EntityLists,
                row,
                expected: 5,
                observed: wire(value.len(), FullSemanticImageField::EntityLists)?,
            });
        }
        if value[0] != 0 {
            return Err(FullSemanticImageFault::Discriminant {
                field: FullSemanticImageField::EntityLists,
                row,
                observed: value[0],
            });
        }
        let length = usize::try_from(u32::from_le_bytes([value[1], value[2], value[3], value[4]]))
            .map_err(|_| FullSemanticImageFault::LengthOverflow {
                field: FullSemanticImageField::EntityLists,
            })?;
        let expected =
            5_usize
                .checked_add(length.checked_mul(4).ok_or(
                    FullSemanticImageFault::LengthOverflow {
                        field: FullSemanticImageField::EntityLists,
                    },
                )?)
                .ok_or(FullSemanticImageFault::LengthOverflow {
                    field: FullSemanticImageField::EntityLists,
                })?;
        if value.len() != expected {
            return Err(FullSemanticImageFault::Reference {
                field: FullSemanticImageField::EntityLists,
                row,
                expected: wire(expected, FullSemanticImageField::EntityLists)?,
                observed: wire(value.len(), FullSemanticImageField::EntityLists)?,
            });
        }
        for index in 0..length {
            let at = 5 + index * 4;
            let id = u32::from_le_bytes([value[at], value[at + 1], value[at + 2], value[at + 3]]);
            reference(FullSemanticImageField::EntityLists, row, entities, id)?;
        }
    }
    validate_docs(bytes, layout)?;
    validate_entity_sparse_authority(bytes, layout, typed)
}

fn validate_entity_sparse_authority(
    bytes: &[u8],
    layout: FullImageLayout,
    typed: TypedLayout,
) -> Result<(), FullSemanticImageFault> {
    let members = layout.entry(FullDirectoryKind::EntityLists);
    let member_bytes = layout.entry(FullDirectoryKind::EntityListBytes);
    let docs = layout.entry(FullDirectoryKind::Documentation);
    let doc_bytes = layout.entry(FullDirectoryKind::DocumentationBytes);
    for row in 0..layout.entry(FullDirectoryKind::Entities).count {
        let entity = decode::entity(bytes, layout, row)?;
        let member_value = range_value(
            bytes,
            members,
            member_bytes,
            entity.members.raw,
            FullSemanticImageField::EntityLists,
        )?;
        let member_count = list_count(member_value, row, FullSemanticImageField::EntityLists)?;
        sparse_availability(row, 4, entity.authority.members, member_count)?;
        let doc_value = range_value(
            bytes,
            docs,
            doc_bytes,
            entity.docs.raw,
            FullSemanticImageField::Documentation,
        )?;
        let doc_count = list_count(doc_value, row, FullSemanticImageField::Documentation)?;
        sparse_availability(row, 5, entity.authority.documentation, doc_count)?;
        let attribute_count =
            super::super::typed_decode::list_count(bytes, layout, typed, 5, entity.attributes.raw)?;
        sparse_availability(row, 7, entity.authority.attributes, attribute_count)?;
    }
    Ok(())
}

fn list_count(
    value: &[u8],
    row: u32,
    field: FullSemanticImageField,
) -> Result<u32, FullSemanticImageFault> {
    let count = value
        .get(1..5)
        .ok_or(FullSemanticImageFault::Truncated { field, offset: 1 })?;
    let raw = <[u8; 4]>::try_from(count)
        .map_err(|_| FullSemanticImageFault::Truncated { field, offset: 1 })?;
    let _ = row;
    Ok(u32::from_le_bytes(raw))
}

fn sparse_availability(
    row: u32,
    plane: u8,
    claimed: FactAvailability,
    length: u32,
) -> Result<(), FullSemanticImageFault> {
    if length != 0 && claimed != FactAvailability::Captured {
        return Err(FullSemanticImageFault::Authority {
            row,
            plane,
            claimed: availability_code(claimed),
            present: true,
        });
    }
    Ok(())
}

fn validate_docs(bytes: &[u8], layout: FullImageLayout) -> Result<(), FullSemanticImageFault> {
    let docs = layout.entry(FullDirectoryKind::Documentation);
    let values = layout.entry(FullDirectoryKind::DocumentationBytes);
    let atoms = layout.entry(FullDirectoryKind::Atoms).count;
    let entities = layout.entry(FullDirectoryKind::Entities).count;
    let externals = layout.entry(FullDirectoryKind::Externals).count;
    for row in 0..docs.count {
        let value = range_value(
            bytes,
            docs,
            values,
            row,
            FullSemanticImageField::Documentation,
        )?;
        if value.len() < 5 {
            return Err(FullSemanticImageFault::Reference {
                field: FullSemanticImageField::Documentation,
                row,
                expected: 5,
                observed: wire(value.len(), FullSemanticImageField::Documentation)?,
            });
        }
        if value[0] != 1 {
            return Err(FullSemanticImageFault::Discriminant {
                field: FullSemanticImageField::Documentation,
                row,
                observed: value[0],
            });
        }
        let count = usize::try_from(u32::from_le_bytes([value[1], value[2], value[3], value[4]]))
            .map_err(|_| FullSemanticImageFault::LengthOverflow {
            field: FullSemanticImageField::Documentation,
        })?;
        let mut cursor = 5_usize;
        for _ in 0..count {
            let tag = *value.get(cursor).ok_or(FullSemanticImageFault::Truncated {
                field: FullSemanticImageField::Documentation,
                offset: values.offset + cursor,
            })?;
            cursor = cursor
                .checked_add(1)
                .ok_or(FullSemanticImageFault::LengthOverflow {
                    field: FullSemanticImageField::Documentation,
                })?;
            match tag {
                0 | 1 => {
                    let atom =
                        read_value_u32(value, cursor, row, FullSemanticImageField::Documentation)?;
                    reference(FullSemanticImageField::Documentation, row, atoms, atom)?;
                    let atom_bytes = atom_value(bytes, layout, atom)?;
                    core::str::from_utf8(atom_bytes).map_err(|_| {
                        FullSemanticImageFault::Discriminant {
                            field: FullSemanticImageField::Documentation,
                            row,
                            observed: tag,
                        }
                    })?;
                    cursor += 4;
                }
                2 => {
                    let atom =
                        read_value_u32(value, cursor, row, FullSemanticImageField::Documentation)?;
                    reference(FullSemanticImageField::Documentation, row, atoms, atom)?;
                    let atom_bytes = atom_value(bytes, layout, atom)?;
                    core::str::from_utf8(atom_bytes).map_err(|_| {
                        FullSemanticImageFault::Discriminant {
                            field: FullSemanticImageField::Documentation,
                            row,
                            observed: tag,
                        }
                    })?;
                    let target =
                        *value
                            .get(cursor + 4)
                            .ok_or(FullSemanticImageFault::Truncated {
                                field: FullSemanticImageField::Documentation,
                                offset: values.offset + cursor + 4,
                            })?;
                    let target_value = read_value_u32(
                        value,
                        cursor + 5,
                        row,
                        FullSemanticImageField::Documentation,
                    )?;
                    match target {
                        0 => reference(
                            FullSemanticImageField::Documentation,
                            row,
                            entities,
                            target_value,
                        )?,
                        1 => reference(
                            FullSemanticImageField::Documentation,
                            row,
                            externals,
                            target_value,
                        )?,
                        observed => {
                            return Err(FullSemanticImageFault::Discriminant {
                                field: FullSemanticImageField::Documentation,
                                row,
                                observed,
                            });
                        }
                    }
                    cursor += 9;
                }
                3 | 4 => {}
                observed => {
                    return Err(FullSemanticImageFault::Discriminant {
                        field: FullSemanticImageField::Documentation,
                        row,
                        observed,
                    });
                }
            }
        }
        if cursor != value.len() {
            return Err(FullSemanticImageFault::Reference {
                field: FullSemanticImageField::Documentation,
                row,
                expected: wire(cursor, FullSemanticImageField::Documentation)?,
                observed: wire(value.len(), FullSemanticImageField::Documentation)?,
            });
        }
    }
    Ok(())
}
