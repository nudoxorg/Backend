//! Fixed-width entity, external, link, and occurrence rows for a full image.
use crate::ir::semantic_image::full::FullPlanError;
use crate::ir::{
    ExternalTarget, FactAvailability, ForeignTargetOrigin, LinkTarget, ParentageAuthority,
    SourceSpan, VariantAvailability,
};

use super::super::wire::{
    ENTITY_ROW_BYTES, EXTERNAL_ROW_BYTES, LINK_ROW_BYTES, NONE, OCCURRENCE_ROW_BYTES,
};

pub(super) fn write_core_entity(
    output: &mut [u8; ENTITY_ROW_BYTES],
    entity: crate::ir::SemanticEntity,
    canonical: &crate::ir::semantic_image::canonical::CanonicalFullPlan<'_>,
) -> Result<(), FullPlanError> {
    put_u32_array(output, 0, canonical.atom(entity.name)?);
    put_u16_array(output, 4, u16::from(entity.kind));
    output[6] = visibility_code(entity.visibility);
    output[7] = parentage_code(entity.authority.parentage);
    put_u32_array(
        output,
        8,
        match entity.parent {
            Some(value) => canonical.entity(value)?,
            None => NONE,
        },
    );
    match entity.source {
        Some(source) => {
            put_u32_array(output, 12, canonical.atom(source.file())?);
            put_u32_array(output, 16, source.start());
            put_u32_array(output, 20, source.end());
        }
        None => put_u32_array(output, 12, NONE),
    }
    let availability = [
        entity.authority.source,
        entity.authority.source_file,
        entity.authority.members,
        entity.authority.semantic_type,
        entity.authority.documentation,
        entity.authority.visibility,
        entity.authority.attributes,
        entity.authority.language_extension,
    ];
    for (index, value) in availability.into_iter().enumerate() {
        output[24 + index] = availability_code(value);
    }
    output[36..52].copy_from_slice(entity.version.family.as_bytes());
    output[52..68].copy_from_slice(entity.version.variant.as_bytes());
    output[68..84].copy_from_slice(entity.version.core_payload.as_bytes());
    match entity.authority.parentage {
        ParentageAuthority::Bound(parent) => {
            output[84..100].copy_from_slice(parent.family.as_bytes());
            output[100..116].copy_from_slice(parent.variant.as_bytes());
        }
        ParentageAuthority::UnrepresentedAuthorityOwner(owner) => {
            output[84..100].copy_from_slice(&owner.as_bytes());
        }
        ParentageAuthority::Root | ParentageAuthority::Unavailable => {}
    }
    Ok(())
}

pub(super) fn external_row(
    target: ExternalTarget,
    canonical: &crate::ir::semantic_image::canonical::CanonicalFullPlan<'_>,
) -> Result<[u8; EXTERNAL_ROW_BYTES], FullPlanError> {
    let mut output = [0_u8; EXTERNAL_ROW_BYTES];
    match target {
        ExternalTarget::Stable { target } => {
            output[0] = 0;
            output[4..36].copy_from_slice(target.fragment.as_ref());
            output[36..52].copy_from_slice(target.declaration.family.as_bytes());
            output[52..68].copy_from_slice(target.declaration.variant.as_bytes());
        }
        ExternalTarget::Foreign(value) => {
            output[0] = 1;
            match value.identity.variant {
                VariantAvailability::Known(variant) => {
                    output[1] = 1;
                    output[52..68].copy_from_slice(variant.as_bytes());
                }
                VariantAvailability::Unavailable => output[1] = 0,
            }
            output[36..52].copy_from_slice(value.identity.foreign.as_bytes());
            let (origin, first, second) = match value.origin {
                ForeignTargetOrigin::Package { ecosystem, package } => {
                    (0, canonical.atom(ecosystem)?, canonical.atom(package)?)
                }
                ForeignTargetOrigin::Namespace {
                    ecosystem,
                    namespace,
                } => (1, canonical.atom(ecosystem)?, canonical.atom(namespace)?),
                ForeignTargetOrigin::Universe { ecosystem } => {
                    (2, canonical.atom(ecosystem)?, NONE)
                }
                ForeignTargetOrigin::Unspecified { ecosystem } => {
                    (3, canonical.atom(ecosystem)?, NONE)
                }
            };
            output[2] = origin;
            put_u32_array(&mut output, 68, first);
            put_u32_array(&mut output, 72, second);
            put_u32_array(&mut output, 76, canonical.atom(value.path)?);
            put_u32_array(&mut output, 80, canonical.atom(value.display)?);
            match value.kind {
                Some(kind) => {
                    output[3] = 1;
                    put_u16_array(&mut output, 84, u16::from(kind));
                }
                None => {}
            }
        }
        ExternalTarget::FragmentEntity { target, display } => {
            output[0] = 2;
            output[4..36].copy_from_slice(target.fragment.as_ref());
            put_u32_array(&mut output, 68, target.ordinal);
            put_u32_array(&mut output, 80, canonical.atom(display)?);
        }
    }
    Ok(output)
}

pub(super) fn link_row(
    link: crate::ir::Link,
    canonical: &crate::ir::semantic_image::canonical::CanonicalFullPlan<'_>,
) -> Result<[u8; LINK_ROW_BYTES], FullPlanError> {
    let mut output = [0_u8; LINK_ROW_BYTES];
    put_u32_array(&mut output, 0, canonical.entity(link.from)?);
    match link.target {
        LinkTarget::Local(entity) => {
            output[4] = 0;
            put_u32_array(&mut output, 8, canonical.entity(entity)?);
        }
        LinkTarget::External(external) => {
            output[4] = 1;
            put_u32_array(&mut output, 8, canonical.external(external)?);
        }
    }
    output[5] = link_kind_code(link.kind);
    output[6] = confidence_code(link.confidence);
    write_optional_source(&mut output, 7, 12, link.source, canonical)?;
    Ok(output)
}

pub(super) fn occurrence_row(
    occurrence: crate::ir::LinkOccurrence,
    authority: crate::ir::OccurrenceAuthorityFacts,
    relation: u32,
    canonical: &crate::ir::semantic_image::canonical::CanonicalFullPlan<'_>,
) -> Result<[u8; OCCURRENCE_ROW_BYTES], FullPlanError> {
    let mut output = [0_u8; OCCURRENCE_ROW_BYTES];
    put_u32_array(&mut output, 0, relation);
    output[4] = confidence_code(occurrence.confidence);
    output[5] = availability_code(authority.source);
    write_optional_source(&mut output, 6, 8, occurrence.source, canonical)?;
    Ok(output)
}

fn write_optional_source(
    output: &mut [u8],
    present_offset: usize,
    value_offset: usize,
    source: Option<SourceSpan>,
    canonical: &crate::ir::semantic_image::canonical::CanonicalFullPlan<'_>,
) -> Result<(), FullPlanError> {
    if let Some(source) = source {
        output[present_offset] = 1;
        put_u32_slice(output, value_offset, canonical.atom(source.file())?);
        put_u32_slice(output, value_offset + 4, source.start());
        put_u32_slice(output, value_offset + 8, source.end());
    }
    Ok(())
}

fn put_u16_array<const N: usize>(output: &mut [u8; N], offset: usize, value: u16) {
    output[offset..offset + 2].copy_from_slice(&value.to_le_bytes());
}
pub(super) fn put_u32_array<const N: usize>(output: &mut [u8; N], offset: usize, value: u32) {
    output[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
}
fn put_u32_slice(output: &mut [u8], offset: usize, value: u32) {
    output[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
}

const fn visibility_code(value: crate::ir::Visibility) -> u8 {
    match value {
        crate::ir::Visibility::Unknown => 0,
        crate::ir::Visibility::Private => 1,
        crate::ir::Visibility::Restricted => 2,
        crate::ir::Visibility::Package => 3,
        crate::ir::Visibility::Public => 4,
    }
}
const fn availability_code(value: FactAvailability) -> u8 {
    match value {
        FactAvailability::Unavailable => 0,
        FactAvailability::Captured => 1,
    }
}
const fn parentage_code(value: ParentageAuthority) -> u8 {
    match value {
        ParentageAuthority::Root => 0,
        ParentageAuthority::Bound(_) => 1,
        ParentageAuthority::UnrepresentedAuthorityOwner(_) => 2,
        ParentageAuthority::Unavailable => 3,
    }
}
const fn confidence_code(value: crate::ir::Confidence) -> u8 {
    match value {
        crate::ir::Confidence::Syntactic => 0,
        crate::ir::Confidence::Heuristic => 1,
        crate::ir::Confidence::Indexed => 2,
        crate::ir::Confidence::Imported => 3,
        crate::ir::Confidence::Compiler => 4,
    }
}
const fn link_kind_code(value: crate::ir::LinkKind) -> u8 {
    match value {
        crate::ir::LinkKind::Calls => 0,
        crate::ir::LinkKind::MethodCall => 1,
        crate::ir::LinkKind::TypeReference => 2,
        crate::ir::LinkKind::Reads => 3,
        crate::ir::LinkKind::Writes => 4,
        crate::ir::LinkKind::Imports => 5,
        crate::ir::LinkKind::Implements => 6,
        crate::ir::LinkKind::Overrides => 7,
        crate::ir::LinkKind::Reexports => 8,
        crate::ir::LinkKind::Inherits => 9,
        crate::ir::LinkKind::Documents => 10,
    }
}
