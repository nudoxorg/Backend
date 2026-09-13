//! Shared canonical image-header and provenance decoding.
//!
//! The complete image validator supplies the proven atom/entity lane offsets
//! below. This module deliberately owns no standalone layout or reopen API.

use alloc::{borrow::ToOwned, vec};

use compiler_vocabulary::{CompileRecipeFact, LanguageProfile, NativeTool, Stage};
use backend_version::{
    CompileRecipeDomain, ContentId, SemanticScopeDomain, SourceFactDomain, ToolchainDomain,
};

use crate::{
    AtomId, DeclarationKey, EntityKind, ImageProvenance, PackageLineage, SemanticImageAuthority,
    SemanticScopeClaim, SemanticScopeFacts, SourceIdentity,
};

use super::fault::{
    CoreProvenanceFault, CoreProvenanceIdentityField, CoreSemanticImageFault,
    CoreSemanticImageField, ScopeComponent,
};

const HEADER_BYTES: usize = 176;
const ATOM_ROW_BYTES: usize = 8;

/// Proven canonical offsets for the header's atom references.
#[derive(Clone, Copy, Debug)]
pub(crate) struct ImageHeaderLayout {
    pub(crate) atom_rows: usize,
    pub(crate) atom_bytes: usize,
    pub(crate) atoms: usize,
    pub(crate) bytes: usize,
}

#[inline]
fn get_u32(
    bytes: &[u8],
    offset: usize,
    field: CoreSemanticImageField,
) -> Result<u32, CoreSemanticImageFault> {
    Ok(u32::from_le_bytes(read_array::<4>(bytes, offset, field)?))
}

#[inline]
fn read_array<const N: usize>(
    bytes: &[u8],
    offset: usize,
    field: CoreSemanticImageField,
) -> Result<[u8; N], CoreSemanticImageFault> {
    let end = offset
        .checked_add(N)
        .ok_or(CoreSemanticImageFault::Truncated { field, offset })?;
    let slice = bytes
        .get(offset..end)
        .ok_or(CoreSemanticImageFault::Truncated { field, offset })?;
    <[u8; N]>::try_from(slice).map_err(|_| CoreSemanticImageFault::Truncated { field, offset })
}

pub(crate) fn decode_image_facts(
    bytes: &[u8],
    layout: ImageHeaderLayout,
) -> Result<crate::SemanticImageFacts, CoreSemanticImageFault> {
    let atom_count =
        u32::try_from(layout.atom_rows).map_err(|_| CoreSemanticImageFault::LengthOverflow {
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
            SemanticImageAuthority::Language(LanguageProfile::try_from(profile).map_err(
                |source| CoreSemanticImageFault::Profile {
                    observed: profile,
                    source,
                },
            )?)
        }
        observed => {
            return Err(CoreSemanticImageFault::Discriminant {
                field: CoreSemanticImageField::Authority,
                row: 0,
                observed,
            });
        }
    };
    for observed in bytes[21..24].iter().chain(bytes[176..HEADER_BYTES].iter()) {
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
        observed => {
            return Err(CoreSemanticImageFault::Discriminant {
                field: CoreSemanticImageField::Provenance,
                row: 0,
                observed,
            });
        }
    };
    Ok(crate::SemanticImageFacts {
        authority,
        provenance,
    })
}

fn decode_captured_provenance(
    bytes: &[u8],
    layout: ImageHeaderLayout,
    atom_count: u32,
    authority: SemanticImageAuthority,
) -> Result<ImageProvenance, CoreSemanticImageFault> {
    let source = SourceIdentity {
        identity: ContentId::<SourceFactDomain>::try_from(read_array::<32>(
            bytes,
            24,
            CoreSemanticImageField::Provenance,
        )?)
        .map_err(|source| CoreSemanticImageFault::ProvenanceIdentity {
            field: CoreProvenanceIdentityField::Source,
            source,
        })?,
        byte_len: get_u32(bytes, 56, CoreSemanticImageField::Provenance)?,
    };
    let profile_bytes = read_array::<2>(bytes, 92, CoreSemanticImageField::Provenance)?;
    let profile = LanguageProfile::try_from(profile_bytes).map_err(|source| {
        CoreSemanticImageFault::Profile {
            observed: profile_bytes,
            source,
        }
    })?;
    let stage =
        Stage::try_from(bytes[94]).map_err(|observed| CoreSemanticImageFault::Provenance {
            cause: CoreProvenanceFault::RecipeStage { observed },
        })?;
    let tool =
        NativeTool::try_from(bytes[95]).map_err(|observed| CoreSemanticImageFault::Provenance {
            cause: CoreProvenanceFault::RecipeTool { observed },
        })?;
    let recipe = CompileRecipeFact {
        identity: ContentId::<CompileRecipeDomain>::try_from(read_array::<32>(
            bytes,
            60,
            CoreSemanticImageField::Provenance,
        )?)
        .map_err(|source| CoreSemanticImageFault::ProvenanceIdentity {
            field: CoreProvenanceIdentityField::Recipe,
            source,
        })?,
        profile,
        stage,
        tool,
        toolchain: ContentId::<ToolchainDomain>::try_from(read_array::<32>(
            bytes,
            96,
            CoreSemanticImageField::Provenance,
        )?)
        .map_err(|source| CoreSemanticImageFault::ProvenanceIdentity {
            field: CoreProvenanceIdentityField::Toolchain,
            source,
        })?,
    };
    let expected_recipe = CompileRecipeFact::derive(
        recipe.profile,
        recipe.stage,
        recipe.tool,
        source.identity,
        recipe.toolchain,
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
                cause: CoreProvenanceFault::ScopeAtom {
                    component,
                    raw,
                    atom_count,
                },
            });
        }
    }
    let claim = SemanticScopeClaim {
        identity: ContentId::<SemanticScopeDomain>::try_from(read_array::<32>(
            bytes,
            128,
            CoreSemanticImageField::Provenance,
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
        coordinate: {
            let raw = get_u32(bytes, 172, CoreSemanticImageField::Provenance)?;
            if raw == 0 {
                None
            } else {
                let atom = raw - 1;
                if atom >= atom_count {
                    return Err(CoreSemanticImageFault::Provenance {
                        cause: CoreProvenanceFault::ScopeAtom {
                            component: ScopeComponent::Coordinate,
                            raw: atom,
                            atom_count,
                        },
                    });
                }
                Some(AtomId::new(atom))
            }
        },
    };
    validate_scope_claim(bytes, layout, claim, scope)?;
    Ok(ImageProvenance::Captured {
        source,
        recipe,
        claim,
        scope,
    })
}

fn validate_scope_claim(
    bytes: &[u8],
    layout: ImageHeaderLayout,
    claim: SemanticScopeClaim,
    scope: SemanticScopeFacts,
) -> Result<(), CoreSemanticImageFault> {
    let ecosystem =
        core::str::from_utf8(atom_bytes(bytes, layout, scope.ecosystem)?).map_err(|source| {
            CoreSemanticImageFault::ScopeUtf8 {
                component: ScopeComponent::Ecosystem,
                source,
            }
        })?;
    let package =
        core::str::from_utf8(atom_bytes(bytes, layout, scope.package)?).map_err(|source| {
            CoreSemanticImageFault::ScopeUtf8 {
                component: ScopeComponent::Package,
                source,
            }
        })?;
    let path = core::str::from_utf8(atom_bytes(bytes, layout, scope.path)?).map_err(|source| {
        CoreSemanticImageFault::ScopeUtf8 {
            component: ScopeComponent::Path,
            source,
        }
    })?;
    let lineage = PackageLineage::new(ecosystem, package)
        .map_err(|cause| CoreSemanticImageFault::ScopeLineage { cause })?;
    if let Some(coordinate) = scope.coordinate {
        let coordinate =
            core::str::from_utf8(atom_bytes(bytes, layout, coordinate)?).map_err(|source| {
                CoreSemanticImageFault::ScopeUtf8 {
                    component: ScopeComponent::Coordinate,
                    source,
                }
            })?;
        let coordinate =
            compiler_vocabulary::PackageUrl::parse(coordinate.to_owned()).map_err(|_| {
                CoreSemanticImageFault::Provenance {
                    cause: CoreProvenanceFault::Coordinate,
                }
            })?;
        if coordinate.package_type().as_str() != ecosystem || coordinate.lineage_name() != package {
            return Err(CoreSemanticImageFault::Provenance {
                cause: CoreProvenanceFault::Coordinate,
            });
        }
        let expected = SemanticScopeClaim::for_package(&coordinate, path).identity;
        if expected != claim.identity {
            return Err(CoreSemanticImageFault::Provenance {
                cause: CoreProvenanceFault::ScopeClaim {
                    expected: *expected.as_ref(),
                    observed: *claim.identity.as_ref(),
                },
            });
        }
        return Ok(());
    }
    let key = DeclarationKey::new(lineage, path, EntityKind::Module, b"_")
        .map_err(|cause| CoreSemanticImageFault::ScopeKey { cause })?;
    let length = key
        .preimage_len()
        .map_err(|cause| CoreSemanticImageFault::ScopePreimage { cause })?;
    let mut preimage = vec![0_u8; length];
    let written = key
        .write_preimage(&mut preimage)
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
    layout: ImageHeaderLayout,
    atom: AtomId,
) -> Result<&'bytes [u8], CoreSemanticImageFault> {
    let row = atom.index();
    let row_wire = u32::try_from(row).map_err(|_| CoreSemanticImageFault::LengthOverflow {
        field: CoreSemanticImageField::AtomRange,
    })?;
    let atom_bytes_wire =
        u32::try_from(layout.atom_bytes).map_err(|_| CoreSemanticImageFault::LengthOverflow {
            field: CoreSemanticImageField::AtomRange,
        })?;
    if row >= layout.atom_rows {
        return Err(CoreSemanticImageFault::Reference {
            field: CoreSemanticImageField::Provenance,
            row: 0,
            expected: u32::try_from(layout.atom_rows).map_err(|_| {
                CoreSemanticImageFault::LengthOverflow {
                    field: CoreSemanticImageField::AtomRange,
                }
            })?,
            observed: row_wire,
        });
    }
    let offset = layout.atoms + row * ATOM_ROW_BYTES;
    let start_raw = get_u32(bytes, offset, CoreSemanticImageField::AtomRange)?;
    let start = usize::try_from(start_raw).map_err(|_| CoreSemanticImageFault::Reference {
        field: CoreSemanticImageField::AtomRange,
        row: row_wire,
        expected: atom_bytes_wire,
        observed: start_raw,
    })?;
    let length_raw = get_u32(bytes, offset + 4, CoreSemanticImageField::AtomRange)?;
    let length = usize::try_from(length_raw).map_err(|_| CoreSemanticImageFault::Reference {
        field: CoreSemanticImageField::AtomRange,
        row: row_wire,
        expected: atom_bytes_wire,
        observed: length_raw,
    })?;
    let start = layout
        .bytes
        .checked_add(start)
        .ok_or(CoreSemanticImageFault::Truncated {
            field: CoreSemanticImageField::AtomRange,
            offset: layout.bytes,
        })?;
    let end = start
        .checked_add(length)
        .ok_or(CoreSemanticImageFault::Truncated {
            field: CoreSemanticImageField::AtomRange,
            offset: start,
        })?;
    bytes
        .get(start..end)
        .ok_or(CoreSemanticImageFault::Truncated {
            field: CoreSemanticImageField::AtomRange,
            offset: start,
        })
}
