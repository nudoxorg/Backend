//! Header/provenance and core-row decoding after explicit wire validation.

use alloc::vec;

use compiler_vocabulary::{CompileRecipeFact, LanguageProfile, NativeTool, Stage};
use heart_identity::{
    CompileRecipeDomain, ContentId, SemanticScopeDomain, SourceFactDomain, ToolchainDomain,
};

use crate::{
    AtomId, CorePayloadHash, CoreSemanticEntity, DeclarationFamilyId, DeclarationIdentity,
    DeclarationKey, EntityAuthorityFacts, EntityId, EntityKind, EntityVersion, FactAvailability,
    ImageProvenance, PackageLineage, ParentageAuthority, SemanticImageAuthority,
    SemanticScopeClaim, SemanticScopeFacts, SourceIdentity, SourceSpan,
    UnrepresentedAuthorityOwner, VariantFingerprint, Visibility,
};

use super::{
    fault::{
        CoreProvenanceFault, CoreProvenanceIdentityField, CoreSemanticImageFault,
        CoreSemanticImageField, ScopeComponent,
    },
    wire::{get_u16, get_u32, read_array, CoreImageLayout, HEADER_BYTES, NONE},
};

pub(crate) fn decode_image_facts(
    bytes: &[u8],
    layout: CoreImageLayout,
) -> Result<crate::SemanticImageFacts, CoreSemanticImageFault> {
    let atom_count = u32::try_from(layout.atom_rows).map_err(|_| CoreSemanticImageFault::LengthOverflow {
        field: CoreSemanticImageField::AtomRange,
    })?;
    let authority = match *bytes.get(16).ok_or(CoreSemanticImageFault::Truncated {
        field: CoreSemanticImageField::Authority,
        offset: 16,
    })? {
        0 => {
            for observed in &bytes[17..20] {
                if *observed != 0 {
                    return Err(CoreSemanticImageFault::Reserved {
                        field: CoreSemanticImageField::Authority,
                        row: 0,
                        observed: *observed,
                    });
                }
            }
            SemanticImageAuthority::Shared
        }
        1 => {
            if bytes[19] != 0 {
                return Err(CoreSemanticImageFault::Reserved {
                    field: CoreSemanticImageField::Authority,
                    row: 0,
                    observed: bytes[19],
                });
            }
            let profile = read_array::<2>(bytes, 17, CoreSemanticImageField::Authority)?;
            SemanticImageAuthority::Language(LanguageProfile::try_from(profile).map_err(|source| {
                CoreSemanticImageFault::Profile { observed: profile, source }
            })?)
        }
        observed => return Err(CoreSemanticImageFault::Discriminant {
            field: CoreSemanticImageField::Authority,
            row: 0,
            observed,
        }),
    };
    for observed in bytes[21..24].iter().chain(bytes[172..HEADER_BYTES].iter()) {
        if *observed != 0 {
            return Err(CoreSemanticImageFault::Reserved {
                field: CoreSemanticImageField::Header,
                row: 0,
                observed: *observed,
            });
        }
    }
    let provenance = match bytes[20] {
        0 => {
            for observed in &bytes[24..HEADER_BYTES] {
                if *observed != 0 {
                    return Err(CoreSemanticImageFault::Reserved {
                        field: CoreSemanticImageField::Provenance,
                        row: 0,
                        observed: *observed,
                    });
                }
            }
            ImageProvenance::Unavailable
        }
        1 => decode_captured_provenance(bytes, layout, atom_count, authority)?,
        observed => return Err(CoreSemanticImageFault::Discriminant {
            field: CoreSemanticImageField::Provenance,
            row: 0,
            observed,
        }),
    };
    Ok(crate::SemanticImageFacts { authority, provenance })
}

fn decode_captured_provenance(
    bytes: &[u8],
    layout: CoreImageLayout,
    atom_count: u32,
    authority: SemanticImageAuthority,
) -> Result<ImageProvenance, CoreSemanticImageFault> {
    let source = SourceIdentity {
        identity: ContentId::<SourceFactDomain>::try_from(read_array::<32>(
            bytes, 24, CoreSemanticImageField::Provenance,
        )?)
        .map_err(|source| CoreSemanticImageFault::ProvenanceIdentity {
            field: CoreProvenanceIdentityField::Source,
            source,
        })?,
        byte_len: get_u32(bytes, 56, CoreSemanticImageField::Provenance)?,
    };
    let profile_bytes = read_array::<2>(bytes, 92, CoreSemanticImageField::Provenance)?;
    let profile = LanguageProfile::try_from(profile_bytes)
        .map_err(|source| CoreSemanticImageFault::Profile { observed: profile_bytes, source })?;
    let stage = Stage::try_from(bytes[94]).map_err(|observed| CoreSemanticImageFault::Provenance {
        cause: CoreProvenanceFault::RecipeStage { observed },
    })?;
    let tool = NativeTool::try_from(bytes[95]).map_err(|observed| CoreSemanticImageFault::Provenance {
        cause: CoreProvenanceFault::RecipeTool { observed },
    })?;
    let recipe = CompileRecipeFact {
        identity: ContentId::<CompileRecipeDomain>::try_from(read_array::<32>(
            bytes, 60, CoreSemanticImageField::Provenance,
        )?)
        .map_err(|source| CoreSemanticImageFault::ProvenanceIdentity {
            field: CoreProvenanceIdentityField::Recipe,
            source,
        })?,
        profile,
        stage,
        tool,
        toolchain: ContentId::<ToolchainDomain>::try_from(read_array::<32>(
            bytes, 96, CoreSemanticImageField::Provenance,
        )?)
        .map_err(|source| CoreSemanticImageFault::ProvenanceIdentity {
            field: CoreProvenanceIdentityField::Toolchain,
            source,
        })?,
    };
    let expected_recipe = CompileRecipeFact::derive(
        recipe.profile, recipe.stage, recipe.tool, source.identity, recipe.toolchain,
    );
    if expected_recipe != recipe {
        return Err(CoreSemanticImageFault::Provenance {
            cause: CoreProvenanceFault::RecipeIdentity {
                expected: *expected_recipe.identity.as_ref(),
                observed: *recipe.identity.as_ref(),
            },
        });
    }
    let authority_profile = match authority {
        SemanticImageAuthority::Language(value) => <[u8; 2]>::from(value),
        SemanticImageAuthority::Shared => [0, 0],
    };
    if authority_profile != profile_bytes {
        return Err(CoreSemanticImageFault::Provenance {
            cause: CoreProvenanceFault::AuthorityProfile {
                authority: authority_profile,
                recipe: profile_bytes,
            },
        });
    }
    let scope_atoms = [
        get_u32(bytes, 160, CoreSemanticImageField::Provenance)?,
        get_u32(bytes, 164, CoreSemanticImageField::Provenance)?,
        get_u32(bytes, 168, CoreSemanticImageField::Provenance)?,
    ];
    for (index, raw) in scope_atoms.into_iter().enumerate() {
        if raw >= atom_count {
            let component = match index {
                0 => ScopeComponent::Ecosystem,
                1 => ScopeComponent::Package,
                _ => ScopeComponent::Path,
            };
            return Err(CoreSemanticImageFault::Provenance {
                cause: CoreProvenanceFault::ScopeAtom { component, raw, atom_count },
            });
        }
    }
    let claim = SemanticScopeClaim {
        identity: ContentId::<SemanticScopeDomain>::try_from(read_array::<32>(
            bytes, 128, CoreSemanticImageField::Provenance,
        )?)
        .map_err(|source| CoreSemanticImageFault::ProvenanceIdentity {
            field: CoreProvenanceIdentityField::ScopeClaim,
            source,
        })?,
    };
    let scope = SemanticScopeFacts {
        ecosystem: AtomId::new(scope_atoms[0]),
        package: AtomId::new(scope_atoms[1]),
        path: AtomId::new(scope_atoms[2]),
    };
    validate_scope_claim(bytes, layout, claim, scope)?;
    Ok(ImageProvenance::Captured { source, recipe, claim, scope })
}

fn validate_scope_claim(
    bytes: &[u8],
    layout: CoreImageLayout,
    claim: SemanticScopeClaim,
    scope: SemanticScopeFacts,
) -> Result<(), CoreSemanticImageFault> {
    let ecosystem = core::str::from_utf8(atom_bytes(bytes, layout, scope.ecosystem)?)
        .map_err(|source| CoreSemanticImageFault::ScopeUtf8 { component: ScopeComponent::Ecosystem, source })?;
    let package = core::str::from_utf8(atom_bytes(bytes, layout, scope.package)?)
        .map_err(|source| CoreSemanticImageFault::ScopeUtf8 { component: ScopeComponent::Package, source })?;
    let path = core::str::from_utf8(atom_bytes(bytes, layout, scope.path)?)
        .map_err(|source| CoreSemanticImageFault::ScopeUtf8 { component: ScopeComponent::Path, source })?;
    let lineage = PackageLineage::new(ecosystem, package)
        .map_err(|cause| CoreSemanticImageFault::ScopeLineage { cause })?;
    let key = DeclarationKey::new(lineage, path, EntityKind::Module, b"_")
        .map_err(|cause| CoreSemanticImageFault::ScopeKey { cause })?;
    let length = key.preimage_len().map_err(|cause| CoreSemanticImageFault::ScopePreimage { cause })?;
    let mut preimage = vec![0_u8; length];
    let written = key.write_preimage(&mut preimage)
        .map_err(|cause| CoreSemanticImageFault::ScopePreimage { cause })?;
    let expected = ContentId::<SemanticScopeDomain>::from_canonical_bytes(&preimage[..written]);
    if expected != claim.identity {
        return Err(CoreSemanticImageFault::Provenance {
            cause: CoreProvenanceFault::ScopeClaim {
                expected: *expected.as_ref(),
                observed: *claim.identity.as_ref(),
            },
        });
    }
    Ok(())
}

fn atom_bytes<'bytes>(
    bytes: &'bytes [u8],
    layout: CoreImageLayout,
    atom: AtomId,
) -> Result<&'bytes [u8], CoreSemanticImageFault> {
    let row = atom.index();
    let row_wire = u32::try_from(row).map_err(|_| CoreSemanticImageFault::LengthOverflow {
        field: CoreSemanticImageField::AtomRange,
    })?;
    let atom_bytes_wire = u32::try_from(layout.atom_bytes).map_err(|_| CoreSemanticImageFault::LengthOverflow {
        field: CoreSemanticImageField::AtomRange,
    })?;
    if row >= layout.atom_rows {
        return Err(CoreSemanticImageFault::Reference {
            field: CoreSemanticImageField::Provenance,
            row: 0,
            expected: u32::try_from(layout.atom_rows).map_err(|_| CoreSemanticImageFault::LengthOverflow {
                field: CoreSemanticImageField::AtomRange,
            })?,
            observed: row_wire,
        });
    }
    let offset = layout.atoms + row * super::wire::ATOM_ROW_BYTES;
    let start_raw = get_u32(bytes, offset, CoreSemanticImageField::AtomRange)?;
    let start = usize::try_from(start_raw).map_err(|_| {
        CoreSemanticImageFault::Reference {
            field: CoreSemanticImageField::AtomRange,
            row: row_wire,
            expected: atom_bytes_wire,
            observed: start_raw,
        }
    })?;
    let length_raw = get_u32(bytes, offset + 4, CoreSemanticImageField::AtomRange)?;
    let length = usize::try_from(length_raw).map_err(|_| {
        CoreSemanticImageFault::Reference {
            field: CoreSemanticImageField::AtomRange,
            row: row_wire,
            expected: atom_bytes_wire,
            observed: length_raw,
        }
    })?;
    let start = layout.bytes.checked_add(start).ok_or(CoreSemanticImageFault::Truncated {
        field: CoreSemanticImageField::AtomRange, offset: layout.bytes,
    })?;
    let end = start.checked_add(length).ok_or(CoreSemanticImageFault::Truncated {
        field: CoreSemanticImageField::AtomRange, offset: start,
    })?;
    bytes.get(start..end).ok_or(CoreSemanticImageFault::Truncated {
        field: CoreSemanticImageField::AtomRange, offset: start,
    })
}

pub(crate) fn decode_entity(
    bytes: &[u8],
    layout: CoreImageLayout,
    row: u32,
) -> Result<CoreSemanticEntity, CoreSemanticImageFault> {
    let entity_count = u32::try_from(layout.entity_rows).map_err(|_| CoreSemanticImageFault::LengthOverflow {
        field: CoreSemanticImageField::EntityVersion,
    })?;
    let row_index = usize::try_from(row).map_err(|_| CoreSemanticImageFault::Reference {
        field: CoreSemanticImageField::EntityVersion, row, expected: entity_count, observed: row,
    })?;
    if row_index >= layout.entity_rows {
        return Err(CoreSemanticImageFault::Reference {
            field: CoreSemanticImageField::EntityVersion, row, expected: entity_count, observed: row,
        });
    }
    let offset = layout.entities + row_index * super::wire::ENTITY_ROW_BYTES;
    let name = get_u32(bytes, offset, CoreSemanticImageField::EntityName)?;
    let kind_raw = get_u16(bytes, offset + 4, CoreSemanticImageField::EntityKind)?;
    let kind = EntityKind::try_from(kind_raw)
        .map_err(|_| CoreSemanticImageFault::EntityKind { row, observed: kind_raw })?;
    let visibility = decode_visibility(bytes[offset + 6], row)?;
    let parent_raw = get_u32(bytes, offset + 8, CoreSemanticImageField::EntityParent)?;
    let parent = (parent_raw != NONE).then(|| EntityId::new(parent_raw));
    let source_file = get_u32(bytes, offset + 12, CoreSemanticImageField::EntitySource)?;
    let source = if source_file == NONE {
        None
    } else {
        let start = get_u32(bytes, offset + 16, CoreSemanticImageField::EntitySource)?;
        let end = get_u32(bytes, offset + 20, CoreSemanticImageField::EntitySource)?;
        Some(SourceSpan::new(AtomId::new(source_file), start, end).ok_or(
            CoreSemanticImageFault::SourceSpan { row, start, end },
        )?)
    };
    let authority = EntityAuthorityFacts {
        parentage: decode_parentage(bytes, offset, row)?,
        source: decode_availability(bytes[offset + 24], row)?,
        source_file: decode_availability(bytes[offset + 25], row)?,
        members: decode_availability(bytes[offset + 26], row)?,
        semantic_type: decode_availability(bytes[offset + 27], row)?,
        documentation: decode_availability(bytes[offset + 28], row)?,
        visibility: decode_availability(bytes[offset + 29], row)?,
        attributes: decode_availability(bytes[offset + 30], row)?,
        language_extension: decode_availability(bytes[offset + 31], row)?,
    };
    let family = DeclarationFamilyId::from_raw(read_array::<16>(bytes, offset + 36, CoreSemanticImageField::EntityVersion)?);
    let variant = VariantFingerprint::from_raw(read_array::<16>(bytes, offset + 52, CoreSemanticImageField::EntityVersion)?);
    let core_payload = CorePayloadHash::from_raw(read_array::<16>(bytes, offset + 68, CoreSemanticImageField::EntityVersion)?);
    Ok(CoreSemanticEntity {
        id: EntityId::new(row), name: AtomId::new(name), kind, visibility, parent, authority, source,
        version: EntityVersion { family, variant, core_payload },
    })
}

fn decode_visibility(value: u8, row: u32) -> Result<Visibility, CoreSemanticImageFault> {
    match value {
        0 => Ok(Visibility::Unknown),
        1 => Ok(Visibility::Private),
        2 => Ok(Visibility::Restricted),
        3 => Ok(Visibility::Package),
        4 => Ok(Visibility::Public),
        observed => Err(CoreSemanticImageFault::Discriminant {
            field: CoreSemanticImageField::EntityVisibility, row, observed,
        }),
    }
}

fn decode_availability(value: u8, row: u32) -> Result<FactAvailability, CoreSemanticImageFault> {
    match value {
        0 => Ok(FactAvailability::Unavailable),
        1 => Ok(FactAvailability::Captured),
        observed => Err(CoreSemanticImageFault::Discriminant {
            field: CoreSemanticImageField::EntityAvailability, row, observed,
        }),
    }
}

fn decode_parentage(
    bytes: &[u8], offset: usize, row: u32,
) -> Result<ParentageAuthority, CoreSemanticImageFault> {
    match bytes[offset + 7] {
        0 => Ok(ParentageAuthority::Root),
        1 => Ok(ParentageAuthority::Bound(DeclarationIdentity {
            family: DeclarationFamilyId::from_raw(read_array::<16>(bytes, offset + 84, CoreSemanticImageField::EntityParentage)?),
            variant: VariantFingerprint::from_raw(read_array::<16>(bytes, offset + 100, CoreSemanticImageField::EntityParentage)?),
        })),
        2 => Ok(ParentageAuthority::UnrepresentedAuthorityOwner(
            UnrepresentedAuthorityOwner::new(read_array::<16>(bytes, offset + 84, CoreSemanticImageField::EntityParentage)?),
        )),
        3 => Ok(ParentageAuthority::Unavailable),
        observed => Err(CoreSemanticImageFault::Discriminant {
            field: CoreSemanticImageField::EntityParentage, row, observed,
        }),
    }
}
