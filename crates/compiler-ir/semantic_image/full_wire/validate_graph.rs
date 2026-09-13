//! External-target and graph evidence validation for a full semantic image.

use core::cmp::Ordering;

use super::super::{
    decode,
    fault::{FullSemanticImageFault, FullSemanticImageField},
    wire::{EXTERNAL_ROW_BYTES, FullDirectoryKind, FullImageLayout, NONE, get_u32},
};
use super::{
    availability_code, availability_match, confidence_code, link_kind_code, reference, reserve,
};

pub(super) fn validate_externals(
    bytes: &[u8],
    layout: FullImageLayout,
) -> Result<(), FullSemanticImageFault> {
    let entry = layout.entry(FullDirectoryKind::Externals);
    let atoms = layout.entry(FullDirectoryKind::Atoms).count;
    let mut previous: Option<ExternalOrderKey> = None;
    for row in 0..entry.count {
        let offset = entry.offset
            + usize::try_from(row).map_err(|_| FullSemanticImageFault::LengthOverflow {
                field: FullSemanticImageField::Externals,
            })? * EXTERNAL_ROW_BYTES;
        match bytes[offset] {
            0 => {
                reserve(
                    bytes,
                    offset + 1,
                    offset + 4,
                    FullSemanticImageField::Externals,
                    row,
                )?;
                reserve(
                    bytes,
                    offset + 68,
                    offset + EXTERNAL_ROW_BYTES,
                    FullSemanticImageField::Externals,
                    row,
                )?;
                let _ = decode::external(bytes, layout, row)?;
            }
            1 => {
                reserve(
                    bytes,
                    offset + 4,
                    offset + 36,
                    FullSemanticImageField::Externals,
                    row,
                )?;
                match bytes[offset + 1] {
                    0 => reserve(
                        bytes,
                        offset + 52,
                        offset + 68,
                        FullSemanticImageField::Externals,
                        row,
                    )?,
                    1 => {}
                    observed => {
                        return Err(FullSemanticImageFault::Discriminant {
                            field: FullSemanticImageField::Externals,
                            row,
                            observed,
                        });
                    }
                }
                let origin = bytes[offset + 2];
                let first = get_u32(bytes, offset + 68, FullSemanticImageField::Externals)?;
                let second = get_u32(bytes, offset + 72, FullSemanticImageField::Externals)?;
                reference(FullSemanticImageField::Externals, row, atoms, first)?;
                match origin {
                    0 | 1 => reference(FullSemanticImageField::Externals, row, atoms, second)?,
                    2 | 3 if second == NONE => {}
                    2 | 3 => {
                        return Err(FullSemanticImageFault::Reserved {
                            field: FullSemanticImageField::Externals,
                            row,
                            observed: bytes[offset + 72],
                        });
                    }
                    observed => {
                        return Err(FullSemanticImageFault::Discriminant {
                            field: FullSemanticImageField::Externals,
                            row,
                            observed,
                        });
                    }
                }
                reference(
                    FullSemanticImageField::Externals,
                    row,
                    atoms,
                    get_u32(bytes, offset + 76, FullSemanticImageField::Externals)?,
                )?;
                reference(
                    FullSemanticImageField::Externals,
                    row,
                    atoms,
                    get_u32(bytes, offset + 80, FullSemanticImageField::Externals)?,
                )?;
                match bytes[offset + 3] {
                    0 => reserve(
                        bytes,
                        offset + 84,
                        offset + 86,
                        FullSemanticImageField::Externals,
                        row,
                    )?,
                    1 => {
                        let _ = decode::external(bytes, layout, row)?;
                    }
                    observed => {
                        return Err(FullSemanticImageFault::Discriminant {
                            field: FullSemanticImageField::Externals,
                            row,
                            observed,
                        });
                    }
                }
                reserve(
                    bytes,
                    offset + 86,
                    offset + EXTERNAL_ROW_BYTES,
                    FullSemanticImageField::Externals,
                    row,
                )?;
            }
            2 => {
                reserve(
                    bytes,
                    offset + 1,
                    offset + 4,
                    FullSemanticImageField::Externals,
                    row,
                )?;
                reserve(
                    bytes,
                    offset + 36,
                    offset + 68,
                    FullSemanticImageField::Externals,
                    row,
                )?;
                reference(
                    FullSemanticImageField::Externals,
                    row,
                    atoms,
                    get_u32(bytes, offset + 80, FullSemanticImageField::Externals)?,
                )?;
                reserve(
                    bytes,
                    offset + 72,
                    offset + 80,
                    FullSemanticImageField::Externals,
                    row,
                )?;
                reserve(
                    bytes,
                    offset + 84,
                    offset + EXTERNAL_ROW_BYTES,
                    FullSemanticImageField::Externals,
                    row,
                )?;
                let _ = decode::external(bytes, layout, row)?;
            }
            observed => {
                return Err(FullSemanticImageFault::Discriminant {
                    field: FullSemanticImageField::Externals,
                    row,
                    observed,
                });
            }
        }
        let key = external_key(decode::external(bytes, layout, row)?);
        if let Some(previous) = previous {
            match previous.cmp(&key) {
                Ordering::Less => {}
                Ordering::Equal => {
                    return Err(FullSemanticImageFault::DuplicateCanonicalRow {
                        field: FullSemanticImageField::Externals,
                        row,
                        existing: row - 1,
                    });
                }
                Ordering::Greater => {
                    return Err(FullSemanticImageFault::CanonicalOrder {
                        field: FullSemanticImageField::Externals,
                        previous: row - 1,
                        row,
                    });
                }
            }
        }
        previous = Some(key);
    }
    Ok(())
}

#[derive(Clone, Copy, Eq, Ord, PartialEq, PartialOrd)]
enum ExternalOrderKey {
    Stable {
        fragment: [u8; 32],
        family: [u8; 16],
        variant: [u8; 16],
    },
    Foreign {
        foreign: [u8; 16],
        variant_tag: u8,
        variant: [u8; 16],
        origin_tag: u8,
        origin_first: u32,
        origin_second: u32,
        path: u32,
        display: u32,
        kind_present: u8,
        kind: u16,
    },
    FragmentEntity {
        fragment: [u8; 32],
        ordinal: u32,
        display: u32,
    },
}

fn external_key(target: crate::ExternalTarget) -> ExternalOrderKey {
    match target {
        crate::ExternalTarget::Stable { target } => ExternalOrderKey::Stable {
            fragment: *target.fragment.as_ref(),
            family: *target.declaration.family.as_bytes(),
            variant: *target.declaration.variant.as_bytes(),
        },
        crate::ExternalTarget::Foreign(value) => {
            let (origin_tag, origin_first, origin_second) = match value.origin {
                crate::ForeignTargetOrigin::Package { ecosystem, package } => {
                    (0, ecosystem.raw, package.raw)
                }
                crate::ForeignTargetOrigin::Namespace {
                    ecosystem,
                    namespace,
                } => (1, ecosystem.raw, namespace.raw),
                crate::ForeignTargetOrigin::Universe { ecosystem } => (2, ecosystem.raw, NONE),
                crate::ForeignTargetOrigin::Unspecified { ecosystem } => (3, ecosystem.raw, NONE),
            };
            let (variant_tag, variant) = match value.identity.variant {
                crate::VariantAvailability::Unavailable => (0, [0; 16]),
                crate::VariantAvailability::Known(value) => (1, *value.as_bytes()),
            };
            ExternalOrderKey::Foreign {
                foreign: *value.identity.foreign.as_bytes(),
                variant_tag,
                variant,
                origin_tag,
                origin_first,
                origin_second,
                path: value.path.raw,
                display: value.display.raw,
                kind_present: u8::from(value.kind.is_some()),
                kind: match value.kind {
                    Some(kind) => u16::from(kind),
                    None => 0,
                },
            }
        }
        crate::ExternalTarget::FragmentEntity { target, display } => {
            ExternalOrderKey::FragmentEntity {
                fragment: *target.fragment.as_ref(),
                ordinal: target.ordinal,
                display: display.raw,
            }
        }
    }
}

pub(super) fn validate_graph(
    bytes: &[u8],
    layout: FullImageLayout,
) -> Result<(), FullSemanticImageFault> {
    let links = layout.entry(FullDirectoryKind::Links);
    let entities = layout.entry(FullDirectoryKind::Entities).count;
    let externals = layout.entry(FullDirectoryKind::Externals).count;
    let atoms = layout.entry(FullDirectoryKind::Atoms).count;
    let mut previous_link: Option<(u32, u8, u32, u8, u8, u8, u32, u32, u32)> = None;
    for row in 0..links.count {
        let link = decode::link(bytes, layout, row)?;
        reference(FullSemanticImageField::Links, row, entities, link.from.raw)?;
        match link.target {
            crate::LinkTarget::Local(entity) => {
                reference(FullSemanticImageField::Links, row, entities, entity.raw)?
            }
            crate::LinkTarget::External(external) => {
                reference(FullSemanticImageField::Links, row, externals, external.raw)?
            }
        }
        if let Some(source) = link.source {
            reference(FullSemanticImageField::Links, row, atoms, source.file().raw)?;
        }
        let key = link_key(link);
        if let Some(previous) = previous_link {
            match previous.cmp(&key) {
                Ordering::Less => {}
                Ordering::Equal => {
                    return Err(FullSemanticImageFault::DuplicateCanonicalRow {
                        field: FullSemanticImageField::Links,
                        row,
                        existing: row - 1,
                    });
                }
                Ordering::Greater => {
                    return Err(FullSemanticImageFault::CanonicalOrder {
                        field: FullSemanticImageField::Links,
                        previous: row - 1,
                        row,
                    });
                }
            }
        }
        previous_link = Some(key);
    }
    let occurrences = layout.entry(FullDirectoryKind::Occurrences);
    let mut previous_occurrence = None;
    for row in 0..occurrences.count {
        let (occurrence, authority) = decode::occurrence(bytes, layout, row)?;
        reference(
            FullSemanticImageField::Occurrences,
            row,
            links.count,
            occurrence.link.raw,
        )?;
        if let Some(source) = occurrence.source {
            reference(
                FullSemanticImageField::Occurrences,
                row,
                atoms,
                source.file().raw,
            )?;
        }
        availability_match(row, 8, authority.source, occurrence.source.is_some())?;
        let key = occurrence_key(occurrence, authority);
        if previous_occurrence.is_some_and(|previous| previous > key) {
            return Err(FullSemanticImageFault::CanonicalOrder {
                field: FullSemanticImageField::Occurrences,
                previous: row - 1,
                row,
            });
        }
        previous_occurrence = Some(key);
    }
    Ok(())
}

fn link_key(link: crate::Link) -> (u32, u8, u32, u8, u8, u8, u32, u32, u32) {
    let (target_tag, target) = match link.target {
        crate::LinkTarget::Local(value) => (0, value.raw),
        crate::LinkTarget::External(value) => (1, value.raw),
    };
    let (source_present, source_file, source_start, source_end) = match link.source {
        Some(value) => (1, value.file().raw, value.start(), value.end()),
        None => (0, 0, 0, 0),
    };
    (
        link.from.raw,
        target_tag,
        target,
        link_kind_code(link.kind),
        confidence_code(link.confidence),
        source_present,
        source_file,
        source_start,
        source_end,
    )
}

fn occurrence_key(
    occurrence: crate::LinkOccurrence,
    authority: crate::OccurrenceAuthorityFacts,
) -> (u32, u8, u8, u8, u32, u32, u32) {
    let (source_present, source_file, source_start, source_end) = match occurrence.source {
        Some(value) => (1, value.file().raw, value.start(), value.end()),
        None => (0, 0, 0, 0),
    };
    (
        occurrence.link.raw,
        confidence_code(occurrence.confidence),
        availability_code(authority.source),
        source_present,
        source_file,
        source_start,
        source_end,
    )
}
