//! Defines manifest wire behavior for `compiler-publication`, whose purpose is to publish verified compiler fragments as immutable generations.
//! This module owns the manifest wire invariants and typed state transitions.
//! Its narrow surface prevents representation and policy details from leaking outward.
use compiler_driver::CompiledFragment;
use compiler_ir::{FragmentRange, FragmentRangeManifest, RecipeFact, SectionKind, SourceIdentity};
use compiler_vocabulary::{CompileRecipeFact, LanguageProfile, NativeTool, Stage};
use heart_identity::{
    ArtifactId, CompileRecipeDomain, ContentId, IrFragmentDomain, IrFragmentEncoding,
    IrFragmentRangeEncoding, SourceFactDomain, ToolchainDomain,
};

use super::{
    CompilationManifestError, StoredFragmentFacts,
    build::{
        FRAGMENT_IDENTITY_OFFSET, FRAGMENT_LENGTH_OFFSET, RANGE_BYTES, RANGE_COUNT, RANGE_OFFSET,
        RECIPE_IDENTITY_OFFSET, RECIPE_PROFILE_OFFSET, RECIPE_STAGE_OFFSET, RECIPE_TOOL_OFFSET,
        RECIPE_TOOLCHAIN_OFFSET, SECTION_ORDER, SOURCE_IDENTITY_OFFSET, SOURCE_LENGTH_OFFSET,
    },
};

pub(super) fn write_entry(output: &mut [u8], manifest: &FragmentRangeManifest) {
    output[SOURCE_IDENTITY_OFFSET..SOURCE_IDENTITY_OFFSET + 32]
        .copy_from_slice(manifest.source.identity.as_ref());
    output[SOURCE_LENGTH_OFFSET..SOURCE_LENGTH_OFFSET + 4]
        .copy_from_slice(&manifest.source.byte_len.to_le_bytes());
    output[RECIPE_IDENTITY_OFFSET..RECIPE_IDENTITY_OFFSET + 32]
        .copy_from_slice(manifest.recipe.identity.as_ref());
    output[RECIPE_PROFILE_OFFSET..RECIPE_PROFILE_OFFSET + 2]
        .copy_from_slice(&<[u8; 2]>::from(manifest.recipe.profile));
    output[RECIPE_STAGE_OFFSET] = u8::from(manifest.recipe.stage);
    output[RECIPE_TOOL_OFFSET] = u8::from(manifest.recipe.tool);
    output[RECIPE_TOOLCHAIN_OFFSET..RECIPE_TOOLCHAIN_OFFSET + 32]
        .copy_from_slice(manifest.recipe.toolchain.as_ref());
    output[FRAGMENT_IDENTITY_OFFSET..FRAGMENT_IDENTITY_OFFSET + 32]
        .copy_from_slice(manifest.fragment.as_ref());
    output[FRAGMENT_LENGTH_OFFSET..FRAGMENT_LENGTH_OFFSET + 4]
        .copy_from_slice(&manifest.fragment_length.to_le_bytes());
    for (ordinal, range) in manifest.ranges.iter().enumerate() {
        let offset = RANGE_OFFSET + ordinal * RANGE_BYTES;
        output[offset..offset + 2].copy_from_slice(&u16::from(range.section).to_le_bytes());
        output[offset + 2..offset + 4].copy_from_slice(&0_u16.to_le_bytes());
        output[offset + 4..offset + 8].copy_from_slice(&range.offset.to_le_bytes());
        output[offset + 8..offset + 12].copy_from_slice(&range.length.to_le_bytes());
        output[offset + 12..offset + 44].copy_from_slice(range.identity.as_ref());
    }
}

pub(super) fn fragment_identity(
    compiled: &CompiledFragment<'_>,
) -> ArtifactId<IrFragmentEncoding, IrFragmentDomain> {
    ArtifactId::<IrFragmentEncoding, IrFragmentDomain>::from_encoded_bytes(
        compiled.fragment.as_ref(),
    )
}

#[allow(
    clippy::result_large_err,
    reason = "the decoder retains exact typed malformed-record facts without allocation or erasure"
)]
pub(super) fn decode_entry(
    bytes: &[u8],
    ordinal: usize,
) -> Result<StoredFragmentFacts, CompilationManifestError> {
    let source = SourceIdentity {
        identity: ContentId::<SourceFactDomain>::try_from(
            &bytes[SOURCE_IDENTITY_OFFSET..SOURCE_IDENTITY_OFFSET + 32],
        )
        .map_err(|source| CompilationManifestError::SourceIdentity { ordinal, source })?,
        byte_len: u32::from_le_bytes(fixed::<4>(bytes, SOURCE_LENGTH_OFFSET)),
    };
    let profile = LanguageProfile::try_from([
        bytes[RECIPE_PROFILE_OFFSET],
        bytes[RECIPE_PROFILE_OFFSET + 1],
    ])
    .map_err(|error| CompilationManifestError::Profile {
        ordinal,
        observed: error.code,
    })?;
    let stage = Stage::try_from(bytes[RECIPE_STAGE_OFFSET])
        .map_err(|observed| CompilationManifestError::Stage { ordinal, observed })?;
    let tool = NativeTool::try_from(bytes[RECIPE_TOOL_OFFSET])
        .map_err(|observed| CompilationManifestError::Tool { ordinal, observed })?;
    let recipe = RecipeFact {
        identity: ContentId::<CompileRecipeDomain>::try_from(
            &bytes[RECIPE_IDENTITY_OFFSET..RECIPE_IDENTITY_OFFSET + 32],
        )
        .map_err(|source| CompilationManifestError::RecipeIdentityAuthority { ordinal, source })?,
        profile,
        stage,
        tool,
        toolchain: ContentId::<ToolchainDomain>::try_from(
            &bytes[RECIPE_TOOLCHAIN_OFFSET..RECIPE_TOOLCHAIN_OFFSET + 32],
        )
        .map_err(|source| CompilationManifestError::ToolchainAuthority { ordinal, source })?,
    };
    let expected_recipe =
        CompileRecipeFact::derive(profile, stage, tool, source.identity, recipe.toolchain);
    if recipe != expected_recipe {
        return Err(CompilationManifestError::RecipeIdentity {
            ordinal,
            expected: expected_recipe,
            observed: recipe,
        });
    }
    if stage != Stage::LowerIr {
        return Err(CompilationManifestError::SemanticStage {
            ordinal,
            observed: stage,
        });
    }
    let fragment = ArtifactId::<IrFragmentEncoding, IrFragmentDomain>::try_from(
        &bytes[FRAGMENT_IDENTITY_OFFSET..FRAGMENT_IDENTITY_OFFSET + 32],
    )
    .map_err(|source| CompilationManifestError::FragmentIdentity { ordinal, source })?;
    let fragment_length = u32::from_le_bytes(fixed::<4>(bytes, FRAGMENT_LENGTH_OFFSET));
    let mut ranges = [FragmentRange {
        section: SectionKind::EntityTypes,
        offset: 0,
        length: 0,
        identity: ArtifactId::<IrFragmentRangeEncoding, IrFragmentDomain>::from_encoded_bytes(&[]),
    }; RANGE_COUNT];
    for (range_ordinal, expected_section) in SECTION_ORDER.iter().copied().enumerate() {
        let offset = RANGE_OFFSET + range_ordinal * RANGE_BYTES;
        let raw_section = u16::from_le_bytes(fixed::<2>(bytes, offset));
        let section = SectionKind::try_from(raw_section).map_err(|observed| {
            CompilationManifestError::RangeSection {
                ordinal,
                range: range_ordinal,
                observed,
            }
        })?;
        if section != expected_section {
            return Err(CompilationManifestError::RangeOrder {
                ordinal,
                range: range_ordinal,
                expected: expected_section,
                observed: section,
            });
        }
        let reserved = u16::from_le_bytes(fixed::<2>(bytes, offset + 2));
        if reserved != 0 {
            return Err(CompilationManifestError::RangeReserved {
                ordinal,
                range: range_ordinal,
                observed: reserved,
            });
        }
        let range_offset = u32::from_le_bytes(fixed::<4>(bytes, offset + 4));
        let range_length = u32::from_le_bytes(fixed::<4>(bytes, offset + 8));
        let end = range_offset.checked_add(range_length).ok_or(
            CompilationManifestError::RangeExtentOverflow {
                ordinal,
                range: range_ordinal,
                offset: range_offset,
                length: range_length,
            },
        )?;
        if end > fragment_length {
            return Err(CompilationManifestError::RangeOutsideFragment {
                ordinal,
                range: range_ordinal,
                end,
                fragment_length,
            });
        }
        ranges[range_ordinal] = FragmentRange {
            section,
            offset: range_offset,
            length: range_length,
            identity: ArtifactId::<IrFragmentRangeEncoding, IrFragmentDomain>::try_from(
                &bytes[offset + 12..offset + 44],
            )
            .map_err(|source| CompilationManifestError::RangeIdentity {
                ordinal,
                range: range_ordinal,
                source,
            })?,
        };
    }
    Ok(StoredFragmentFacts {
        fragment,
        fragment_length,
        source,
        recipe,
        ranges,
    })
}

pub(super) fn fixed<const WIDTH: usize>(input: &[u8], offset: usize) -> [u8; WIDTH] {
    let mut output = [0; WIDTH];
    output.copy_from_slice(&input[offset..offset + WIDTH]);
    output
}
