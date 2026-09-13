//! Seven sparse extension-plane validation for a full semantic image.

use core::cmp::Ordering;

use crate::{FactAvailability, SemanticImageAuthority};

use super::super::{
    decode, extensions_decode,
    fault::{FullSemanticImageFault, FullSemanticImageField},
    wire::{
        FullDirectoryEntry, FullDirectoryKind, FullImageLayout, RANGE_ROW_BYTES,
        SPARSE_BINDING_ROW_BYTES, get_u32,
    },
};
use super::{TypedLayout, availability_code, reference, wire};

pub(super) fn validate_extensions(
    bytes: &[u8],
    layout: FullImageLayout,
    typed: TypedLayout,
    authority: SemanticImageAuthority,
) -> Result<(), FullSemanticImageFault> {
    let entities = layout.entry(FullDirectoryKind::Entities).count;
    for (plane_index, (facts_kind, bindings_kind)) in extension_pairs().into_iter().enumerate() {
        let bindings = layout.entry(bindings_kind);
        let facts = layout.entry(facts_kind);
        if !authority_selects_plane(authority, language_for_extension(plane_index)) {
            if facts.count != 0 || facts.length != 0 || bindings.count != 0 || bindings.length != 0
            {
                return Err(FullSemanticImageFault::ExtensionPlaneAuthority {
                    plane: facts_kind,
                    authority,
                    facts: facts.count,
                    fact_bytes: facts.length_wire,
                    bindings: bindings.count,
                    binding_bytes: bindings.length_wire,
                });
            }
            continue;
        }
        validate_extension_ranges(bytes, layout, facts_kind)?;
        let counts = extensions_decode::ExtensionCounts {
            atoms: layout.entry(FullDirectoryKind::Atoms).count,
            members: layout.entry(FullDirectoryKind::EntityLists).count,
            typed,
        };
        for fact in 0..facts.count {
            let value = extension_value(bytes, facts, fact)?;
            extensions_decode::validate(
                u8::try_from(plane_index).map_err(|_| FullSemanticImageFault::LengthOverflow {
                    field: FullSemanticImageField::ExtensionFacts,
                })?,
                value,
                fact,
                counts,
            )?;
        }
        let mut previous = None;
        for row in 0..bindings.count {
            let (entity, fact) = binding(bytes, bindings, row)?;
            reference(
                FullSemanticImageField::ExtensionBindings,
                row,
                entities,
                entity,
            )?;
            reference(
                FullSemanticImageField::ExtensionBindings,
                row,
                facts.count,
                fact,
            )?;
            if let Some((previous_entity, _)) = previous {
                match entity.cmp(&previous_entity) {
                    Ordering::Less => {
                        return Err(FullSemanticImageFault::CanonicalOrder {
                            field: FullSemanticImageField::ExtensionBindings,
                            previous: row - 1,
                            row,
                        });
                    }
                    Ordering::Equal => {
                        return Err(FullSemanticImageFault::DuplicateExtensionBinding {
                            plane: facts_kind,
                            entity,
                            existing: row - 1,
                            row,
                        });
                    }
                    Ordering::Greater => {}
                }
            }
            previous = Some((entity, fact));
            let entity_row = decode::entity(bytes, layout, entity)?;
            if entity_row.authority.language_extension != FactAvailability::Captured {
                return Err(FullSemanticImageFault::Authority {
                    row: entity,
                    plane: 9,
                    claimed: availability_code(entity_row.authority.language_extension),
                    present: true,
                });
            }
        }
        for fact in 0..facts.count {
            if !fact_is_bound(bytes, bindings, fact)? {
                return Err(FullSemanticImageFault::UnboundExtensionFact {
                    plane: facts_kind,
                    fact,
                });
            }
        }
        validate_fact_order(bytes, facts)?;
    }
    // A Captured claim must bind one exact named sparse extension row; merely
    // carrying a captured-empty fact pool is not enough to manufacture an
    // entity's extension authority.
    for entity in 0..entities {
        let row = decode::entity(bytes, layout, entity)?;
        let mut present = 0_u8;
        for (_, bindings_kind) in extension_pairs() {
            let bindings = layout.entry(bindings_kind);
            if sparse_has_entity(bytes, bindings, entity)? {
                present = present
                    .checked_add(1)
                    .ok_or(FullSemanticImageFault::LengthOverflow {
                        field: FullSemanticImageField::ExtensionBindings,
                    })?;
            }
        }
        if present > 1
            || matches!(row.authority.language_extension, FactAvailability::Captured)
                != (present == 1)
        {
            return Err(FullSemanticImageFault::Authority {
                row: entity,
                plane: 9,
                claimed: availability_code(row.authority.language_extension),
                present: present == 1,
            });
        }
    }
    Ok(())
}

fn authority_selects_plane(authority: SemanticImageAuthority, language: crate::Language) -> bool {
    matches!(authority, SemanticImageAuthority::Language(profile) if crate::Language::from(profile) == language)
}

fn validate_fact_order(
    bytes: &[u8],
    facts: FullDirectoryEntry,
) -> Result<(), FullSemanticImageFault> {
    for row in 1..facts.count {
        let previous = extension_value(bytes, facts, row - 1)?;
        let current = extension_value(bytes, facts, row)?;
        match previous.cmp(current) {
            Ordering::Less => {}
            Ordering::Greater => {
                return Err(FullSemanticImageFault::CanonicalOrder {
                    field: FullSemanticImageField::ExtensionFacts,
                    previous: row - 1,
                    row,
                });
            }
            // Every owned language plane interns exact `Facts` values before
            // planning. Equal canonical payloads therefore cannot form a
            // legitimate secondary binding-order case in bytes.
            Ordering::Equal => {
                return Err(FullSemanticImageFault::DuplicateCanonicalRow {
                    field: FullSemanticImageField::ExtensionFacts,
                    row,
                    existing: row - 1,
                });
            }
        }
    }
    Ok(())
}

fn fact_is_bound(
    bytes: &[u8],
    bindings: FullDirectoryEntry,
    fact: u32,
) -> Result<bool, FullSemanticImageFault> {
    let mut row = 0;
    Ok(next_fact_entity(bytes, bindings, &mut row, fact)?.is_some())
}

fn next_fact_entity(
    bytes: &[u8],
    bindings: FullDirectoryEntry,
    next: &mut u32,
    fact: u32,
) -> Result<Option<u32>, FullSemanticImageFault> {
    while *next < bindings.count {
        let row = *next;
        *next = row + 1;
        let (entity, observed) = binding(bytes, bindings, row)?;
        if observed == fact {
            return Ok(Some(entity));
        }
    }
    Ok(None)
}

fn validate_extension_ranges(
    bytes: &[u8],
    layout: FullImageLayout,
    kind: FullDirectoryKind,
) -> Result<(), FullSemanticImageFault> {
    let entry = layout.entry(kind);
    let range_bytes = usize::try_from(entry.count)
        .map_err(|_| FullSemanticImageFault::LengthOverflow {
            field: FullSemanticImageField::ExtensionFacts,
        })?
        .checked_mul(RANGE_ROW_BYTES)
        .ok_or(FullSemanticImageFault::LengthOverflow {
            field: FullSemanticImageField::ExtensionFacts,
        })?;
    let value_offset =
        entry
            .offset
            .checked_add(range_bytes)
            .ok_or(FullSemanticImageFault::LengthOverflow {
                field: FullSemanticImageField::ExtensionFacts,
            })?;
    let value_len = entry.length.checked_sub(range_bytes).ok_or(
        FullSemanticImageFault::DirectoryCountLane {
            kind,
            expected: wire(range_bytes, FullSemanticImageField::ExtensionFacts)?,
            observed: entry.length_wire,
        },
    )?;
    let mut expected = 0_usize;
    for row in 0..entry.count {
        let offset = entry.offset
            + usize::try_from(row).map_err(|_| FullSemanticImageFault::LengthOverflow {
                field: FullSemanticImageField::ExtensionFacts,
            })? * RANGE_ROW_BYTES;
        let start_wire = get_u32(bytes, offset, FullSemanticImageField::ExtensionFacts)?;
        let start = usize::try_from(start_wire).map_err(|_| FullSemanticImageFault::Reference {
            field: FullSemanticImageField::ExtensionFacts,
            row,
            expected: entry.count,
            observed: start_wire,
        })?;
        let length_wire = get_u32(bytes, offset + 4, FullSemanticImageField::ExtensionFacts)?;
        let length =
            usize::try_from(length_wire).map_err(|_| FullSemanticImageFault::Reference {
                field: FullSemanticImageField::ExtensionFacts,
                row,
                expected: entry.count,
                observed: length_wire,
            })?;
        let end = start
            .checked_add(length)
            .ok_or(FullSemanticImageFault::LengthOverflow {
                field: FullSemanticImageField::ExtensionFacts,
            })?;
        if start != expected || end > value_len {
            return Err(FullSemanticImageFault::Reference {
                field: FullSemanticImageField::ExtensionFacts,
                row,
                expected: wire(value_len, FullSemanticImageField::ExtensionFacts)?,
                observed: wire(end, FullSemanticImageField::ExtensionFacts)?,
            });
        }
        let _ = bytes.get(value_offset + start..value_offset + end).ok_or(
            FullSemanticImageFault::Truncated {
                field: FullSemanticImageField::ExtensionFacts,
                offset: value_offset + start,
            },
        )?;
        expected = end;
    }
    if expected != value_len {
        return Err(FullSemanticImageFault::Reference {
            field: FullSemanticImageField::ExtensionFacts,
            row: entry.count,
            expected: wire(value_len, FullSemanticImageField::ExtensionFacts)?,
            observed: wire(expected, FullSemanticImageField::ExtensionFacts)?,
        });
    }
    Ok(())
}

pub(crate) fn extension_value<'bytes>(
    bytes: &'bytes [u8],
    facts: FullDirectoryEntry,
    row: u32,
) -> Result<&'bytes [u8], FullSemanticImageFault> {
    if row >= facts.count {
        return Err(FullSemanticImageFault::Reference {
            field: FullSemanticImageField::ExtensionFacts,
            row,
            expected: facts.count,
            observed: row,
        });
    }
    let range_bytes = usize::try_from(facts.count)
        .map_err(|_| FullSemanticImageFault::LengthOverflow {
            field: FullSemanticImageField::ExtensionFacts,
        })?
        .checked_mul(RANGE_ROW_BYTES)
        .ok_or(FullSemanticImageFault::LengthOverflow {
            field: FullSemanticImageField::ExtensionFacts,
        })?;
    let range_offset = facts
        .offset
        .checked_add(
            usize::try_from(row)
                .map_err(|_| FullSemanticImageFault::LengthOverflow {
                    field: FullSemanticImageField::ExtensionFacts,
                })?
                .checked_mul(RANGE_ROW_BYTES)
                .ok_or(FullSemanticImageFault::LengthOverflow {
                    field: FullSemanticImageField::ExtensionFacts,
                })?,
        )
        .ok_or(FullSemanticImageFault::LengthOverflow {
            field: FullSemanticImageField::ExtensionFacts,
        })?;
    let start = usize::try_from(get_u32(
        bytes,
        range_offset,
        FullSemanticImageField::ExtensionFacts,
    )?)
    .map_err(|_| FullSemanticImageFault::LengthOverflow {
        field: FullSemanticImageField::ExtensionFacts,
    })?;
    let length = usize::try_from(get_u32(
        bytes,
        range_offset + 4,
        FullSemanticImageField::ExtensionFacts,
    )?)
    .map_err(|_| FullSemanticImageFault::LengthOverflow {
        field: FullSemanticImageField::ExtensionFacts,
    })?;
    let values = facts
        .offset
        .checked_add(range_bytes)
        .and_then(|offset| offset.checked_add(start))
        .ok_or(FullSemanticImageFault::LengthOverflow {
            field: FullSemanticImageField::ExtensionFacts,
        })?;
    let end = values
        .checked_add(length)
        .ok_or(FullSemanticImageFault::LengthOverflow {
            field: FullSemanticImageField::ExtensionFacts,
        })?;
    bytes
        .get(values..end)
        .ok_or(FullSemanticImageFault::Truncated {
            field: FullSemanticImageField::ExtensionFacts,
            offset: values,
        })
}

pub(super) fn extension_pairs() -> [(FullDirectoryKind, FullDirectoryKind); 7] {
    [
        (
            FullDirectoryKind::TypeScriptFacts,
            FullDirectoryKind::TypeScriptBindings,
        ),
        (
            FullDirectoryKind::CSharpFacts,
            FullDirectoryKind::CSharpBindings,
        ),
        (FullDirectoryKind::GoFacts, FullDirectoryKind::GoBindings),
        (
            FullDirectoryKind::RustFacts,
            FullDirectoryKind::RustBindings,
        ),
        (
            FullDirectoryKind::PythonFacts,
            FullDirectoryKind::PythonBindings,
        ),
        (
            FullDirectoryKind::JavaFacts,
            FullDirectoryKind::JavaBindings,
        ),
        (
            FullDirectoryKind::ClangFacts,
            FullDirectoryKind::ClangBindings,
        ),
    ]
}

fn language_for_extension(index: usize) -> crate::Language {
    match index {
        0 => crate::Language::TypeScript,
        1 => crate::Language::CSharp,
        2 => crate::Language::Go,
        3 => crate::Language::Rust,
        4 => crate::Language::Python,
        5 => crate::Language::Java,
        _ => crate::Language::Clang,
    }
}

fn binding(
    bytes: &[u8],
    bindings: FullDirectoryEntry,
    row: u32,
) -> Result<(u32, u32), FullSemanticImageFault> {
    if row >= bindings.count {
        return Err(FullSemanticImageFault::Reference {
            field: FullSemanticImageField::ExtensionBindings,
            row,
            expected: bindings.count,
            observed: row,
        });
    }
    let offset = bindings
        .offset
        .checked_add(
            usize::try_from(row)
                .map_err(|_| FullSemanticImageFault::LengthOverflow {
                    field: FullSemanticImageField::ExtensionBindings,
                })?
                .checked_mul(SPARSE_BINDING_ROW_BYTES)
                .ok_or(FullSemanticImageFault::LengthOverflow {
                    field: FullSemanticImageField::ExtensionBindings,
                })?,
        )
        .ok_or(FullSemanticImageFault::LengthOverflow {
            field: FullSemanticImageField::ExtensionBindings,
        })?;
    Ok((
        get_u32(bytes, offset, FullSemanticImageField::ExtensionBindings)?,
        get_u32(bytes, offset + 4, FullSemanticImageField::ExtensionBindings)?,
    ))
}

fn sparse_has_entity(
    bytes: &[u8],
    bindings: FullDirectoryEntry,
    entity: u32,
) -> Result<bool, FullSemanticImageFault> {
    let mut lower = 0_u32;
    let mut upper = bindings.count;
    while lower < upper {
        let middle = lower + (upper - lower) / 2;
        let (observed, _) = binding(bytes, bindings, middle)?;
        if observed < entity {
            lower = middle + 1;
        } else {
            upper = middle;
        }
    }
    if lower >= bindings.count {
        return Ok(false);
    }
    Ok(binding(bytes, bindings, lower)?.0 == entity)
}
