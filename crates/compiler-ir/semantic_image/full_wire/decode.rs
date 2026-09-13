//! Decoding portable full-image rows after structural admission.

use crate::{
    AtomId, CorePayloadHash, DeclarationFamilyId, DeclarationIdentity, EntityAuthorityFacts,
    EntityId, EntityKind, EntityVersion, FactAvailability, ForeignDeclarationId,
    ForeignExternalTarget, ForeignTargetOrigin, Link, LinkKind, LinkOccurrence, LinkTarget,
    ParentageAuthority, SemanticEntity, SourceSpan, UnrepresentedAuthorityOwner,
    VariantAvailability, VariantFingerprint, Visibility,
};
use backend_semantic::ir_vocabulary::{ExternalCoordinate, ExternalDeclarationIdentity, StableRef};
use backend_version::{ContentId, IrFragmentDomain};

use super::{
    fault::{FullSemanticImageFault, FullSemanticImageField, FullSemanticImageIdentityField},
    wire::{
        ENTITY_ROW_BYTES, EXTERNAL_ROW_BYTES, FullDirectoryKind, FullImageLayout, LINK_ROW_BYTES,
        NONE, OCCURRENCE_ROW_BYTES, get_u16, get_u32, read_array,
    },
};

pub(crate) fn entity(
    bytes: &[u8],
    layout: FullImageLayout,
    row: u32,
) -> Result<SemanticEntity, FullSemanticImageFault> {
    let entries = layout.entry(FullDirectoryKind::Entities);
    if row >= entries.count {
        return Err(FullSemanticImageFault::Reference {
            field: FullSemanticImageField::Entities,
            row,
            expected: entries.count,
            observed: row,
        });
    }
    let offset = entries
        .offset
        .checked_add(
            usize::try_from(row)
                .map_err(|_| FullSemanticImageFault::LengthOverflow {
                    field: FullSemanticImageField::Entities,
                })?
                .checked_mul(ENTITY_ROW_BYTES)
                .ok_or(FullSemanticImageFault::LengthOverflow {
                    field: FullSemanticImageField::Entities,
                })?,
        )
        .ok_or(FullSemanticImageFault::LengthOverflow {
            field: FullSemanticImageField::Entities,
        })?;
    let name = AtomId::new(get_u32(bytes, offset, FullSemanticImageField::Entities)?);
    let kind_raw = get_u16(bytes, offset + 4, FullSemanticImageField::Entities)?;
    let kind = EntityKind::try_from(kind_raw).map_err(|_| FullSemanticImageFault::EntityKind {
        row,
        observed: kind_raw,
    })?;
    let visibility = visibility(bytes[offset + 6], row)?;
    let parent = option_entity(get_u32(
        bytes,
        offset + 8,
        FullSemanticImageField::Entities,
    )?);
    let source_file = get_u32(bytes, offset + 12, FullSemanticImageField::Entities)?;
    let source = if source_file == NONE {
        None
    } else {
        let start = get_u32(bytes, offset + 16, FullSemanticImageField::Entities)?;
        let end = get_u32(bytes, offset + 20, FullSemanticImageField::Entities)?;
        Some(
            SourceSpan::new(AtomId::new(source_file), start, end)
                .ok_or(FullSemanticImageFault::SourceSpan { row, start, end })?,
        )
    };
    let authority = EntityAuthorityFacts {
        parentage: parentage(bytes, offset, row)?,
        source: availability(bytes[offset + 24], row)?,
        source_file: availability(bytes[offset + 25], row)?,
        members: availability(bytes[offset + 26], row)?,
        semantic_type: availability(bytes[offset + 27], row)?,
        documentation: availability(bytes[offset + 28], row)?,
        visibility: availability(bytes[offset + 29], row)?,
        attributes: availability(bytes[offset + 30], row)?,
        language_extension: availability(bytes[offset + 31], row)?,
    };
    let version = EntityVersion {
        family: DeclarationFamilyId::from_raw(read_array::<16>(
            bytes,
            offset + 36,
            FullSemanticImageField::Entities,
        )?),
        variant: VariantFingerprint::from_raw(read_array::<16>(
            bytes,
            offset + 52,
            FullSemanticImageField::Entities,
        )?),
        core_payload: CorePayloadHash::from_raw(read_array::<16>(
            bytes,
            offset + 68,
            FullSemanticImageField::Entities,
        )?),
    };
    let semantic_type = option_type(get_u32(
        bytes,
        offset + 120,
        FullSemanticImageField::Entities,
    )?);
    Ok(SemanticEntity {
        id: EntityId::new(row),
        name,
        kind,
        visibility,
        parent,
        semantic_type,
        members: crate::EntityListId::new(get_u32(
            bytes,
            offset + 124,
            FullSemanticImageField::Entities,
        )?),
        docs: crate::DocId::new(get_u32(
            bytes,
            offset + 128,
            FullSemanticImageField::Entities,
        )?),
        attributes: crate::AtomListId::new(get_u32(
            bytes,
            offset + 132,
            FullSemanticImageField::Entities,
        )?),
        source,
        authority,
        version,
    })
}

pub(crate) fn external(
    bytes: &[u8],
    layout: FullImageLayout,
    row: u32,
) -> Result<crate::ExternalTarget, FullSemanticImageFault> {
    let entries = layout.entry(FullDirectoryKind::Externals);
    if row >= entries.count {
        return Err(FullSemanticImageFault::Reference {
            field: FullSemanticImageField::Externals,
            row,
            expected: entries.count,
            observed: row,
        });
    }
    let offset = entries
        .offset
        .checked_add(
            usize::try_from(row)
                .map_err(|_| FullSemanticImageFault::LengthOverflow {
                    field: FullSemanticImageField::Externals,
                })?
                .checked_mul(EXTERNAL_ROW_BYTES)
                .ok_or(FullSemanticImageFault::LengthOverflow {
                    field: FullSemanticImageField::Externals,
                })?,
        )
        .ok_or(FullSemanticImageFault::LengthOverflow {
            field: FullSemanticImageField::Externals,
        })?;
    match bytes[offset] {
        0 => Ok(crate::ExternalTarget::Stable {
            target: StableRef {
                fragment: ContentId::<IrFragmentDomain>::try_from(read_array::<32>(
                    bytes,
                    offset + 4,
                    FullSemanticImageField::Externals,
                )?)
                .map_err(|cause| FullSemanticImageFault::ContentIdentity {
                    field: FullSemanticImageField::Externals,
                    row,
                    identity: FullSemanticImageIdentityField::StableFragment,
                    cause,
                })?,
                declaration: DeclarationIdentity {
                    family: DeclarationFamilyId::from_raw(read_array::<16>(
                        bytes,
                        offset + 36,
                        FullSemanticImageField::Externals,
                    )?),
                    variant: VariantFingerprint::from_raw(read_array::<16>(
                        bytes,
                        offset + 52,
                        FullSemanticImageField::Externals,
                    )?),
                },
            },
        }),
        1 => {
            let variant =
                match bytes[offset + 1] {
                    0 => VariantAvailability::Unavailable,
                    1 => VariantAvailability::Known(VariantFingerprint::from_raw(
                        read_array::<16>(bytes, offset + 52, FullSemanticImageField::Externals)?,
                    )),
                    observed => {
                        return Err(FullSemanticImageFault::Discriminant {
                            field: FullSemanticImageField::Externals,
                            row,
                            observed,
                        });
                    }
                };
            let first = AtomId::new(get_u32(
                bytes,
                offset + 68,
                FullSemanticImageField::Externals,
            )?);
            let second = get_u32(bytes, offset + 72, FullSemanticImageField::Externals)?;
            let origin = match bytes[offset + 2] {
                0 => ForeignTargetOrigin::Package {
                    ecosystem: first,
                    package: AtomId::new(second),
                },
                1 => ForeignTargetOrigin::Namespace {
                    ecosystem: first,
                    namespace: AtomId::new(second),
                },
                2 => ForeignTargetOrigin::Universe { ecosystem: first },
                3 => ForeignTargetOrigin::Unspecified { ecosystem: first },
                observed => {
                    return Err(FullSemanticImageFault::Discriminant {
                        field: FullSemanticImageField::Externals,
                        row,
                        observed,
                    });
                }
            };
            let kind = match bytes[offset + 3] {
                0 => None,
                1 => {
                    let observed = get_u16(bytes, offset + 84, FullSemanticImageField::Externals)?;
                    Some(
                        EntityKind::try_from(observed)
                            .map_err(|_| FullSemanticImageFault::EntityKind { row, observed })?,
                    )
                }
                observed => {
                    return Err(FullSemanticImageFault::Discriminant {
                        field: FullSemanticImageField::Externals,
                        row,
                        observed,
                    });
                }
            };
            Ok(crate::ExternalTarget::Foreign(ForeignExternalTarget {
                identity: ExternalDeclarationIdentity {
                    foreign: ForeignDeclarationId::from_raw(read_array::<16>(
                        bytes,
                        offset + 36,
                        FullSemanticImageField::Externals,
                    )?),
                    variant,
                },
                origin,
                path: AtomId::new(get_u32(
                    bytes,
                    offset + 76,
                    FullSemanticImageField::Externals,
                )?),
                display: AtomId::new(get_u32(
                    bytes,
                    offset + 80,
                    FullSemanticImageField::Externals,
                )?),
                kind,
            }))
        }
        2 => Ok(crate::ExternalTarget::FragmentEntity {
            target: ExternalCoordinate::bind(
                ContentId::<IrFragmentDomain>::try_from(read_array::<32>(
                    bytes,
                    offset + 4,
                    FullSemanticImageField::Externals,
                )?)
                .map_err(|cause| FullSemanticImageFault::ContentIdentity {
                    field: FullSemanticImageField::Externals,
                    row,
                    identity: FullSemanticImageIdentityField::FragmentEntityFragment,
                    cause,
                })?,
                get_u32(bytes, offset + 68, FullSemanticImageField::Externals)?,
            ),
            display: AtomId::new(get_u32(
                bytes,
                offset + 80,
                FullSemanticImageField::Externals,
            )?),
        }),
        observed => Err(FullSemanticImageFault::Discriminant {
            field: FullSemanticImageField::Externals,
            row,
            observed,
        }),
    }
}

pub(crate) fn link(
    bytes: &[u8],
    layout: FullImageLayout,
    row: u32,
) -> Result<Link, FullSemanticImageFault> {
    let offset = fixed_offset(layout, FullDirectoryKind::Links, row, LINK_ROW_BYTES)?;
    let target_raw = get_u32(bytes, offset + 8, FullSemanticImageField::Links)?;
    let target = match bytes[offset + 4] {
        0 => LinkTarget::Local(EntityId::new(target_raw)),
        1 => LinkTarget::External(crate::ExternalId::new(target_raw)),
        observed => {
            return Err(FullSemanticImageFault::Discriminant {
                field: FullSemanticImageField::Links,
                row,
                observed,
            });
        }
    };
    Ok(Link {
        from: EntityId::new(get_u32(bytes, offset, FullSemanticImageField::Links)?),
        target,
        kind: link_kind(bytes[offset + 5], row)?,
        confidence: confidence(bytes[offset + 6], row, FullSemanticImageField::Links)?,
        source: source(
            bytes,
            offset + 7,
            offset + 12,
            row,
            FullSemanticImageField::Links,
        )?,
    })
}

pub(crate) fn occurrence(
    bytes: &[u8],
    layout: FullImageLayout,
    row: u32,
) -> Result<(LinkOccurrence, crate::OccurrenceAuthorityFacts), FullSemanticImageFault> {
    let offset = fixed_offset(
        layout,
        FullDirectoryKind::Occurrences,
        row,
        OCCURRENCE_ROW_BYTES,
    )?;
    let source = source(
        bytes,
        offset + 6,
        offset + 8,
        row,
        FullSemanticImageField::Occurrences,
    )?;
    Ok((
        LinkOccurrence {
            link: crate::LinkId::new(get_u32(bytes, offset, FullSemanticImageField::Occurrences)?),
            confidence: confidence(bytes[offset + 4], row, FullSemanticImageField::Occurrences)?,
            source,
        },
        crate::OccurrenceAuthorityFacts {
            source: availability(bytes[offset + 5], row)?,
        },
    ))
}

pub(crate) fn fixed_offset(
    layout: FullImageLayout,
    kind: FullDirectoryKind,
    row: u32,
    width: usize,
) -> Result<usize, FullSemanticImageFault> {
    let entry = layout.entry(kind);
    if row >= entry.count {
        return Err(FullSemanticImageFault::Reference {
            field: field_for(kind),
            row,
            expected: entry.count,
            observed: row,
        });
    }
    entry
        .offset
        .checked_add(
            usize::try_from(row)
                .map_err(|_| FullSemanticImageFault::LengthOverflow {
                    field: field_for(kind),
                })?
                .checked_mul(width)
                .ok_or(FullSemanticImageFault::LengthOverflow {
                    field: field_for(kind),
                })?,
        )
        .ok_or(FullSemanticImageFault::LengthOverflow {
            field: field_for(kind),
        })
}

pub(crate) const fn field_for(kind: FullDirectoryKind) -> FullSemanticImageField {
    match kind {
        FullDirectoryKind::Atoms | FullDirectoryKind::AtomBytes => FullSemanticImageField::Atoms,
        FullDirectoryKind::Entities => FullSemanticImageField::Entities,
        FullDirectoryKind::TypedNodes => FullSemanticImageField::TypedNodes,
        FullDirectoryKind::TypedEdges => FullSemanticImageField::TypedEdges,
        FullDirectoryKind::EntityLists | FullDirectoryKind::EntityListBytes => {
            FullSemanticImageField::EntityLists
        }
        FullDirectoryKind::Documentation | FullDirectoryKind::DocumentationBytes => {
            FullSemanticImageField::Documentation
        }
        FullDirectoryKind::Externals => FullSemanticImageField::Externals,
        FullDirectoryKind::Links => FullSemanticImageField::Links,
        FullDirectoryKind::Occurrences => FullSemanticImageField::Occurrences,
        FullDirectoryKind::TypeScriptFacts
        | FullDirectoryKind::CSharpFacts
        | FullDirectoryKind::GoFacts
        | FullDirectoryKind::RustFacts
        | FullDirectoryKind::PythonFacts
        | FullDirectoryKind::JavaFacts
        | FullDirectoryKind::ClangFacts => FullSemanticImageField::ExtensionFacts,
        FullDirectoryKind::TypeScriptBindings
        | FullDirectoryKind::CSharpBindings
        | FullDirectoryKind::GoBindings
        | FullDirectoryKind::RustBindings
        | FullDirectoryKind::PythonBindings
        | FullDirectoryKind::JavaBindings
        | FullDirectoryKind::ClangBindings => FullSemanticImageField::ExtensionBindings,
    }
}

fn option_entity(raw: u32) -> Option<EntityId> {
    (raw != NONE).then(|| EntityId::new(raw))
}
fn option_type(raw: u32) -> Option<crate::TypeId> {
    (raw != NONE).then(|| crate::TypeId::new(raw))
}

fn visibility(value: u8, row: u32) -> Result<Visibility, FullSemanticImageFault> {
    match value {
        0 => Ok(Visibility::Unknown),
        1 => Ok(Visibility::Private),
        2 => Ok(Visibility::Restricted),
        3 => Ok(Visibility::Package),
        4 => Ok(Visibility::Public),
        observed => Err(FullSemanticImageFault::Discriminant {
            field: FullSemanticImageField::Entities,
            row,
            observed,
        }),
    }
}
pub(crate) fn availability(
    value: u8,
    row: u32,
) -> Result<FactAvailability, FullSemanticImageFault> {
    match value {
        0 => Ok(FactAvailability::Unavailable),
        1 => Ok(FactAvailability::Captured),
        observed => Err(FullSemanticImageFault::Discriminant {
            field: FullSemanticImageField::Entities,
            row,
            observed,
        }),
    }
}
fn parentage(
    bytes: &[u8],
    offset: usize,
    row: u32,
) -> Result<ParentageAuthority, FullSemanticImageFault> {
    match bytes[offset + 7] {
        0 => Ok(ParentageAuthority::Root),
        1 => Ok(ParentageAuthority::Bound(DeclarationIdentity {
            family: DeclarationFamilyId::from_raw(read_array::<16>(
                bytes,
                offset + 84,
                FullSemanticImageField::Entities,
            )?),
            variant: VariantFingerprint::from_raw(read_array::<16>(
                bytes,
                offset + 100,
                FullSemanticImageField::Entities,
            )?),
        })),
        2 => Ok(ParentageAuthority::UnrepresentedAuthorityOwner(
            UnrepresentedAuthorityOwner::new(read_array::<16>(
                bytes,
                offset + 84,
                FullSemanticImageField::Entities,
            )?),
        )),
        3 => Ok(ParentageAuthority::Unavailable),
        observed => Err(FullSemanticImageFault::Discriminant {
            field: FullSemanticImageField::Entities,
            row,
            observed,
        }),
    }
}
fn source(
    bytes: &[u8],
    present: usize,
    fields: usize,
    row: u32,
    field: FullSemanticImageField,
) -> Result<Option<SourceSpan>, FullSemanticImageFault> {
    match bytes[present] {
        0 => {
            for value in
                bytes
                    .get(fields..fields + 12)
                    .ok_or(FullSemanticImageFault::Truncated {
                        field,
                        offset: fields,
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
            Ok(None)
        }
        1 => {
            let start = get_u32(bytes, fields + 4, field)?;
            let end = get_u32(bytes, fields + 8, field)?;
            SourceSpan::new(AtomId::new(get_u32(bytes, fields, field)?), start, end)
                .ok_or(FullSemanticImageFault::SourceSpan { row, start, end })
                .map(Some)
        }
        observed => Err(FullSemanticImageFault::Discriminant {
            field,
            row,
            observed,
        }),
    }
}
fn confidence(
    value: u8,
    row: u32,
    field: FullSemanticImageField,
) -> Result<crate::Confidence, FullSemanticImageFault> {
    match value {
        0 => Ok(crate::Confidence::Syntactic),
        1 => Ok(crate::Confidence::Heuristic),
        2 => Ok(crate::Confidence::Indexed),
        3 => Ok(crate::Confidence::Imported),
        4 => Ok(crate::Confidence::Compiler),
        observed => Err(FullSemanticImageFault::Discriminant {
            field,
            row,
            observed,
        }),
    }
}
fn link_kind(value: u8, row: u32) -> Result<LinkKind, FullSemanticImageFault> {
    match value {
        0 => Ok(LinkKind::Calls),
        1 => Ok(LinkKind::MethodCall),
        2 => Ok(LinkKind::TypeReference),
        3 => Ok(LinkKind::Reads),
        4 => Ok(LinkKind::Writes),
        5 => Ok(LinkKind::Imports),
        6 => Ok(LinkKind::Implements),
        7 => Ok(LinkKind::Overrides),
        8 => Ok(LinkKind::Reexports),
        9 => Ok(LinkKind::Inherits),
        10 => Ok(LinkKind::Documents),
        observed => Err(FullSemanticImageFault::Discriminant {
            field: FullSemanticImageField::Links,
            row,
            observed,
        }),
    }
}
