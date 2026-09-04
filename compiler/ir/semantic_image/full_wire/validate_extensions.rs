//! Seven sparse extension-plane validation for a full semantic image.

use crate::{FactAvailability, SemanticImageAuthority};

use super::super::{
    decode,
    extensions_decode,
    fault::{FullSemanticImageFault, FullSemanticImageField},
    wire::{get_u32, FullDirectoryEntry, FullDirectoryKind, FullImageLayout, RANGE_ROW_BYTES, SPARSE_BINDING_ROW_BYTES},
};
use super::{availability_code, reference, wire, TypedLayout};

pub(super) fn validate_extensions(
    bytes: &[u8], layout: FullImageLayout, typed: TypedLayout, authority: SemanticImageAuthority,
) -> Result<(), FullSemanticImageFault> {
    let entities = layout.entry(FullDirectoryKind::Entities).count;
    for (plane_index, (facts_kind, bindings_kind)) in extension_pairs().into_iter().enumerate() {
        validate_extension_ranges(bytes, layout, facts_kind)?;
        let bindings = layout.entry(bindings_kind);
        let facts = layout.entry(facts_kind);
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
            let offset = bindings.offset + usize::try_from(row).map_err(|_| FullSemanticImageFault::LengthOverflow {
                field: FullSemanticImageField::ExtensionBindings,
            })? * SPARSE_BINDING_ROW_BYTES;
            let entity = get_u32(bytes, offset, FullSemanticImageField::ExtensionBindings)?;
            let fact = get_u32(bytes, offset + 4, FullSemanticImageField::ExtensionBindings)?;
            reference(FullSemanticImageField::ExtensionBindings, row, entities, entity)?;
            reference(FullSemanticImageField::ExtensionBindings, row, facts.count, fact)?;
            if let Some((previous_entity, previous_fact)) = previous {
                if (entity, fact) <= (previous_entity, previous_fact) {
                    return Err(FullSemanticImageFault::CanonicalOrder {
                        field: FullSemanticImageField::ExtensionBindings, previous: row - 1, row,
                    });
                }
            }
            previous = Some((entity, fact));
            let entity_row = decode::entity(bytes, layout, entity)?;
            if entity_row.authority.language_extension != FactAvailability::Captured {
                return Err(FullSemanticImageFault::Authority {
                    row: entity, plane: 9, claimed: availability_code(entity_row.authority.language_extension), present: true,
                });
            }
            if let SemanticImageAuthority::Language(profile) = authority {
                if crate::Language::from(profile) != language_for_extension(plane_index) {
                    return Err(FullSemanticImageFault::Authority {
                        row: entity, plane: 9, claimed: availability_code(entity_row.authority.language_extension), present: true,
                    });
                }
            } else {
                return Err(FullSemanticImageFault::Authority {
                    row: entity, plane: 9, claimed: availability_code(entity_row.authority.language_extension), present: true,
                });
            }
        }
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
                present = present.checked_add(1).ok_or(FullSemanticImageFault::LengthOverflow {
                    field: FullSemanticImageField::ExtensionBindings,
                })?;
            }
        }
        if present > 1 || matches!(row.authority.language_extension, FactAvailability::Captured) != (present == 1) {
            return Err(FullSemanticImageFault::Authority {
                row: entity, plane: 9, claimed: availability_code(row.authority.language_extension), present: present == 1,
            });
        }
    }
    Ok(())
}

fn validate_extension_ranges(
    bytes: &[u8], layout: FullImageLayout, kind: FullDirectoryKind,
) -> Result<(), FullSemanticImageFault> {
    let entry = layout.entry(kind);
    let range_bytes = usize::try_from(entry.count).map_err(|_| FullSemanticImageFault::LengthOverflow {
        field: FullSemanticImageField::ExtensionFacts,
    })?.checked_mul(RANGE_ROW_BYTES).ok_or(FullSemanticImageFault::LengthOverflow {
        field: FullSemanticImageField::ExtensionFacts,
    })?;
    let value_offset = entry.offset.checked_add(range_bytes).ok_or(FullSemanticImageFault::LengthOverflow {
        field: FullSemanticImageField::ExtensionFacts,
    })?;
    let value_len = entry.length.checked_sub(range_bytes).ok_or(FullSemanticImageFault::DirectoryCountLane {
        kind, expected: wire(range_bytes, FullSemanticImageField::ExtensionFacts)?, observed: entry.length_wire,
    })?;
    let mut expected = 0_usize;
    for row in 0..entry.count {
        let offset = entry.offset + usize::try_from(row).map_err(|_| FullSemanticImageFault::LengthOverflow {
            field: FullSemanticImageField::ExtensionFacts,
        })? * RANGE_ROW_BYTES;
        let start_wire = get_u32(bytes, offset, FullSemanticImageField::ExtensionFacts)?;
        let start = usize::try_from(start_wire).map_err(|_| {
            FullSemanticImageFault::Reference { field: FullSemanticImageField::ExtensionFacts, row, expected: entry.count, observed: start_wire }
        })?;
        let length_wire = get_u32(bytes, offset + 4, FullSemanticImageField::ExtensionFacts)?;
        let length = usize::try_from(length_wire).map_err(|_| {
            FullSemanticImageFault::Reference { field: FullSemanticImageField::ExtensionFacts, row, expected: entry.count, observed: length_wire }
        })?;
        let end = start.checked_add(length).ok_or(FullSemanticImageFault::LengthOverflow {
            field: FullSemanticImageField::ExtensionFacts,
        })?;
        if start != expected || end > value_len {
            return Err(FullSemanticImageFault::Reference {
                field: FullSemanticImageField::ExtensionFacts, row,
                expected: wire(value_len, FullSemanticImageField::ExtensionFacts)?,
                observed: wire(end, FullSemanticImageField::ExtensionFacts)?,
            });
        }
        let _ = bytes.get(value_offset + start..value_offset + end).ok_or(FullSemanticImageFault::Truncated {
            field: FullSemanticImageField::ExtensionFacts, offset: value_offset + start,
        })?;
        expected = end;
    }
    if expected != value_len {
        return Err(FullSemanticImageFault::Reference {
            field: FullSemanticImageField::ExtensionFacts, row: entry.count,
            expected: wire(value_len, FullSemanticImageField::ExtensionFacts)?,
            observed: wire(expected, FullSemanticImageField::ExtensionFacts)?,
        });
    }
    Ok(())
}

pub(crate) fn extension_value<'bytes>(
    bytes: &'bytes [u8], facts: FullDirectoryEntry, row: u32,
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
        .map_err(|_| FullSemanticImageFault::LengthOverflow { field: FullSemanticImageField::ExtensionFacts })?
        .checked_mul(RANGE_ROW_BYTES)
        .ok_or(FullSemanticImageFault::LengthOverflow { field: FullSemanticImageField::ExtensionFacts })?;
    let range_offset = facts.offset.checked_add(
        usize::try_from(row)
            .map_err(|_| FullSemanticImageFault::LengthOverflow { field: FullSemanticImageField::ExtensionFacts })?
            .checked_mul(RANGE_ROW_BYTES)
            .ok_or(FullSemanticImageFault::LengthOverflow { field: FullSemanticImageField::ExtensionFacts })?,
    ).ok_or(FullSemanticImageFault::LengthOverflow { field: FullSemanticImageField::ExtensionFacts })?;
    let start = usize::try_from(get_u32(bytes, range_offset, FullSemanticImageField::ExtensionFacts)?)
        .map_err(|_| FullSemanticImageFault::LengthOverflow { field: FullSemanticImageField::ExtensionFacts })?;
    let length = usize::try_from(get_u32(bytes, range_offset + 4, FullSemanticImageField::ExtensionFacts)?)
        .map_err(|_| FullSemanticImageFault::LengthOverflow { field: FullSemanticImageField::ExtensionFacts })?;
    let values = facts.offset.checked_add(range_bytes)
        .and_then(|offset| offset.checked_add(start))
        .ok_or(FullSemanticImageFault::LengthOverflow { field: FullSemanticImageField::ExtensionFacts })?;
    let end = values.checked_add(length)
        .ok_or(FullSemanticImageFault::LengthOverflow { field: FullSemanticImageField::ExtensionFacts })?;
    bytes.get(values..end).ok_or(FullSemanticImageFault::Truncated {
        field: FullSemanticImageField::ExtensionFacts,
        offset: values,
    })
}

pub(super) fn extension_pairs() -> [(FullDirectoryKind, FullDirectoryKind); 7] {
    [
        (FullDirectoryKind::TypeScriptFacts, FullDirectoryKind::TypeScriptBindings),
        (FullDirectoryKind::CSharpFacts, FullDirectoryKind::CSharpBindings),
        (FullDirectoryKind::GoFacts, FullDirectoryKind::GoBindings),
        (FullDirectoryKind::RustFacts, FullDirectoryKind::RustBindings),
        (FullDirectoryKind::PythonFacts, FullDirectoryKind::PythonBindings),
        (FullDirectoryKind::JavaFacts, FullDirectoryKind::JavaBindings),
        (FullDirectoryKind::ClangFacts, FullDirectoryKind::ClangBindings),
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

fn sparse_has_entity(
    bytes: &[u8], bindings: FullDirectoryEntry, entity: u32,
) -> Result<bool, FullSemanticImageFault> {
    let mut lower = 0_u32;
    let mut upper = bindings.count;
    while lower < upper {
        let middle = lower + (upper - lower) / 2;
        let offset = bindings.offset + usize::try_from(middle).map_err(|_| FullSemanticImageFault::LengthOverflow {
            field: FullSemanticImageField::ExtensionBindings,
        })? * SPARSE_BINDING_ROW_BYTES;
        let observed = get_u32(bytes, offset, FullSemanticImageField::ExtensionBindings)?;
        if observed < entity { lower = middle + 1; } else { upper = middle; }
    }
    if lower >= bindings.count { return Ok(false); }
    let offset = bindings.offset + usize::try_from(lower).map_err(|_| FullSemanticImageFault::LengthOverflow {
        field: FullSemanticImageField::ExtensionBindings,
    })? * SPARSE_BINDING_ROW_BYTES;
    Ok(get_u32(bytes, offset, FullSemanticImageField::ExtensionBindings)? == entity)
}
