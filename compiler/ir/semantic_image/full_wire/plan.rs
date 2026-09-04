//! Measured all-or-nothing full-image write preparation.
//!
//! This is the only place where owned `Ir` facts are read for the full wire.
//! It finishes canonical remapping, atom/entity/external lookups, and every
//! checked length conversion before an encoder receives caller output.  The
//! write phase can then copy pre-admitted byte rows without a late failure.

use alloc::vec::Vec;

use crate::{
    ExternalTarget, FactAvailability, ForeignTargetOrigin, ImageProvenance, Ir, LinkTarget,
    ParentageAuthority, SemanticReader, SourceSpan, VariantAvailability,
};

use super::{
    fault::{FullSemanticImageFault, FullSemanticImageField},
    wire::{
        FullDirectoryEntry, FullDirectoryKind, FullImageLayout, ATOM_ROW_BYTES, DIRECTORY_BYTES,
        ENTITY_ROW_BYTES, EXTERNAL_ROW_BYTES, HEADER_BYTES, LINK_ROW_BYTES, NONE,
        OCCURRENCE_ROW_BYTES, RANGE_ROW_BYTES, SPARSE_BINDING_ROW_BYTES,
    },
};
use crate::semantic_image::full::{FullSemanticPlan, FullPlanError};

/// Fully remapped rows and measured directory spans for an infallible full
/// image writer.  All vectors are proportional to real rows/edges; the plan
/// never allocates per child or reserves a max-of-language geometry.
pub(super) struct FullSemanticImagePlan<'image> {
    pub(super) semantic: FullSemanticPlan<'image>,
    pub(super) image: crate::SemanticImageFacts,
    pub(super) provenance_scope_atoms: [u32; 3],
    pub(super) entities: Vec<[u8; ENTITY_ROW_BYTES]>,
    pub(super) externals: Vec<[u8; EXTERNAL_ROW_BYTES]>,
    pub(super) links: Vec<[u8; LINK_ROW_BYTES]>,
    pub(super) occurrences: Vec<[u8; OCCURRENCE_ROW_BYTES]>,
    pub(super) members: CanonicalVariablePool,
    pub(super) documentation: CanonicalVariablePool,
    pub(super) extensions: FullExtensionPayloads,
    pub(super) layout: FullImageLayout,
    pub(super) required: usize,
    pub(super) required_wire: u32,
}

/// Canonical variable-row payload. Ranges are relative to `bytes` and are in
/// the same canonical order as the owning directory, never the source
/// interner order retained by a planning scratch arena.
pub(super) struct CanonicalVariablePool {
    pub(super) ranges: Vec<crate::ArenaRange>,
    pub(super) bytes: Vec<u8>,
}

/// Seven named variable fact payloads. Keeping this as named fields prevents
/// full writer/view code from reintroducing a dense language union.
pub(super) struct FullExtensionPayloads {
    pub(super) typescript: CanonicalVariablePool,
    pub(super) csharp: CanonicalVariablePool,
    pub(super) go: CanonicalVariablePool,
    pub(super) rust: CanonicalVariablePool,
    pub(super) python: CanonicalVariablePool,
    pub(super) java: CanonicalVariablePool,
    pub(super) clang: CanonicalVariablePool,
}

#[derive(Clone, Copy)]
struct Lane {
    length: usize,
    count: u32,
}

impl Lane {
    const ZERO: Self = Self { length: 0, count: 0 };
}

impl<'image> FullSemanticImagePlan<'image> {
    pub(super) fn build(ir: &'image Ir) -> Result<Self, FullPlanError> {
        let semantic = FullSemanticPlan::build(ir)?;
        let canonical = semantic.typed.canonical();
        let image = canonical.core.image;
        let provenance_scope_atoms = match image.provenance {
            ImageProvenance::Unavailable => [0, 0, 0],
            ImageProvenance::Captured { scope, .. } => [
                canonical.atom(scope.ecosystem)?,
                canonical.atom(scope.package)?,
                canonical.atom(scope.path)?,
            ],
        };
        let entities = plan_entities(ir, &semantic)?;
        let externals = plan_externals(ir, &semantic)?;
        let links = plan_links(ir, &semantic)?;
        let occurrences = plan_occurrences(ir, &semantic)?;
        let members = canonical_variable_pool(
            &semantic.terminal.members.order,
            &semantic.terminal.members.key_ranges,
            &semantic.terminal.members.key_bytes,
            FullSemanticImageField::EntityLists,
        )?;
        let documentation = canonical_variable_pool(
            &semantic.terminal.docs.order,
            &semantic.terminal.docs.key_ranges,
            &semantic.terminal.docs.key_bytes,
            FullSemanticImageField::Documentation,
        )?;
        let extensions = FullExtensionPayloads {
            typescript: extension_payload(&semantic.extensions.typescript)?,
            csharp: extension_payload(&semantic.extensions.csharp)?,
            go: extension_payload(&semantic.extensions.go)?,
            rust: extension_payload(&semantic.extensions.rust)?,
            python: extension_payload(&semantic.extensions.python)?,
            java: extension_payload(&semantic.extensions.java)?,
            clang: extension_payload(&semantic.extensions.clang)?,
        };
        let lanes = lanes(
            &semantic,
            &entities,
            &externals,
            &links,
            &occurrences,
            &members,
            &documentation,
            &extensions,
        )?;
        let (layout, required, required_wire) = layout(lanes)?;
        Ok(Self {
            semantic,
            image,
            provenance_scope_atoms,
            entities,
            externals,
            links,
            occurrences,
            members,
            documentation,
            extensions,
            layout,
            required,
            required_wire,
        })
    }
}

fn extension_payload(
    plan: &crate::semantic_image::full::ExtensionPlanePlan,
) -> Result<CanonicalVariablePool, FullSemanticImageFault> {
    canonical_variable_pool(
        &plan.order,
        &plan.key_ranges,
        &plan.key_bytes,
        FullSemanticImageField::ExtensionFacts,
    )
}

fn canonical_variable_pool(
    order: &[u32],
    source_ranges: &[crate::ArenaRange],
    source_bytes: &[u8],
    field: FullSemanticImageField,
) -> Result<CanonicalVariablePool, FullSemanticImageFault> {
    let mut ranges = Vec::with_capacity(order.len());
    let mut bytes = Vec::with_capacity(source_bytes.len());
    for raw in order.iter().copied() {
        let index = usize::try_from(raw).map_err(|_| FullSemanticImageFault::Reference {
            field,
            row: 0,
            expected: count(source_ranges.len(), field)?,
            observed: raw,
        })?;
        let range = source_ranges.get(index).copied().ok_or(FullSemanticImageFault::Reference {
            field,
            row: count(ranges.len(), field)?,
            expected: count(source_ranges.len(), field)?,
            observed: raw,
        })?;
        let start = usize::try_from(range.start).map_err(|_| FullSemanticImageFault::LengthOverflow { field })?;
        let length = usize::try_from(range.len).map_err(|_| FullSemanticImageFault::LengthOverflow { field })?;
        let end = start.checked_add(length).ok_or(FullSemanticImageFault::LengthOverflow { field })?;
        let value = source_bytes.get(start..end).ok_or(FullSemanticImageFault::Reference {
            field,
            row: count(ranges.len(), field)?,
            expected: count(source_bytes.len(), field)?,
            observed: range.start,
        })?;
        let destination = count(bytes.len(), field)?;
        bytes.extend_from_slice(value);
        ranges.push(crate::ArenaRange {
            start: destination,
            len: count(value.len(), field)?,
        });
    }
    Ok(CanonicalVariablePool { ranges, bytes })
}

fn plan_entities(
    ir: &Ir,
    semantic: &FullSemanticPlan<'_>,
) -> Result<Vec<[u8; ENTITY_ROW_BYTES]>, FullPlanError> {
    let rows = &semantic.entities.rows;
    let mut planned = Vec::with_capacity(rows.len());
    let canonical = semantic.typed.canonical();
    for (index, row) in rows.iter().copied().enumerate() {
        let facts = ir.semantic_entity(row.entity).ok_or(FullSemanticImageFault::Reference {
            field: FullSemanticImageField::Entities,
            row: u32::try_from(index).map_err(|_| FullSemanticImageFault::LengthOverflow {
                field: FullSemanticImageField::Entities,
            })?,
            expected: u32::try_from(rows.len()).map_err(|_| FullSemanticImageFault::LengthOverflow {
                field: FullSemanticImageField::Entities,
            })?,
            observed: row.entity.raw,
        })?;
        let mut bytes = [0_u8; ENTITY_ROW_BYTES];
        write_core_entity(&mut bytes, facts, canonical)?;
        put_u32_array(&mut bytes, 120, row.semantic_type.unwrap_or(NONE));
        put_u32_array(&mut bytes, 124, row.members);
        put_u32_array(&mut bytes, 128, row.docs);
        put_u32_array(&mut bytes, 132, row.attributes);
        planned.push(bytes);
    }
    Ok(planned)
}

fn plan_externals(
    ir: &Ir,
    semantic: &FullSemanticPlan<'_>,
) -> Result<Vec<[u8; EXTERNAL_ROW_BYTES]>, FullPlanError> {
    let canonical = semantic.typed.canonical();
    let mut rows = Vec::with_capacity(canonical.externals.len());
    for (index, external) in canonical.externals.iter().copied().enumerate() {
        let target = ir.external(external).copied().ok_or(FullSemanticImageFault::Reference {
            field: FullSemanticImageField::Externals,
            row: u32::try_from(index).map_err(|_| FullSemanticImageFault::LengthOverflow {
                field: FullSemanticImageField::Externals,
            })?,
            expected: u32::try_from(canonical.externals.len()).map_err(|_| {
                FullSemanticImageFault::LengthOverflow {
                    field: FullSemanticImageField::Externals,
                }
            })?,
            observed: external.raw,
        })?;
        rows.push(external_row(target, canonical)?);
    }
    Ok(rows)
}

fn plan_links(
    ir: &Ir,
    semantic: &FullSemanticPlan<'_>,
) -> Result<Vec<[u8; LINK_ROW_BYTES]>, FullPlanError> {
    let canonical = semantic.typed.canonical();
    let mut rows = Vec::with_capacity(semantic.graph.relations.len());
    for (index, id) in semantic.graph.relations.iter().copied().enumerate() {
        let link = ir.link(id).ok_or(FullSemanticImageFault::Reference {
            field: FullSemanticImageField::Links,
            row: u32::try_from(index).map_err(|_| FullSemanticImageFault::LengthOverflow {
                field: FullSemanticImageField::Links,
            })?,
            expected: u32::try_from(semantic.graph.relations.len()).map_err(|_| {
                FullSemanticImageFault::LengthOverflow { field: FullSemanticImageField::Links }
            })?,
            observed: id.raw,
        })?;
        rows.push(link_row(link, canonical)?);
    }
    Ok(rows)
}

fn plan_occurrences(
    ir: &Ir,
    semantic: &FullSemanticPlan<'_>,
) -> Result<Vec<[u8; OCCURRENCE_ROW_BYTES]>, FullPlanError> {
    let canonical = semantic.typed.canonical();
    let mut rows = Vec::with_capacity(semantic.graph.occurrences.len());
    for (index, id) in semantic.graph.occurrences.iter().copied().enumerate() {
        let occurrence = ir.link_occurrence(id).ok_or(FullSemanticImageFault::Reference {
            field: FullSemanticImageField::Occurrences,
            row: u32::try_from(index).map_err(|_| FullSemanticImageFault::LengthOverflow {
                field: FullSemanticImageField::Occurrences,
            })?,
            expected: u32::try_from(semantic.graph.occurrences.len()).map_err(|_| {
                FullSemanticImageFault::LengthOverflow {
                    field: FullSemanticImageField::Occurrences,
                }
            })?,
            observed: id.raw,
        })?;
        let authority = ir.occurrence_authority(id).ok_or(FullSemanticImageFault::Reference {
            field: FullSemanticImageField::Occurrences,
            row: u32::try_from(index).map_err(|_| FullSemanticImageFault::LengthOverflow {
                field: FullSemanticImageField::Occurrences,
            })?,
            expected: u32::try_from(semantic.graph.occurrences.len()).map_err(|_| {
                FullSemanticImageFault::LengthOverflow {
                    field: FullSemanticImageField::Occurrences,
                }
            })?,
            observed: id.raw,
        })?;
        let relation = semantic
            .graph
            .relation_remap
            .get(occurrence.link.index())
            .copied()
            .ok_or(FullSemanticImageFault::Reference {
                field: FullSemanticImageField::Occurrences,
                row: u32::try_from(index).map_err(|_| FullSemanticImageFault::LengthOverflow {
                    field: FullSemanticImageField::Occurrences,
                })?,
                expected: u32::try_from(semantic.graph.relations.len()).map_err(|_| {
                    FullSemanticImageFault::LengthOverflow {
                        field: FullSemanticImageField::Occurrences,
                    }
                })?,
                observed: occurrence.link.raw,
            })?;
        rows.push(occurrence_row(occurrence, authority, relation, canonical)?);
    }
    Ok(rows)
}

fn lanes(
    semantic: &FullSemanticPlan<'_>,
    entities: &[[u8; ENTITY_ROW_BYTES]],
    externals: &[[u8; EXTERNAL_ROW_BYTES]],
    links: &[[u8; LINK_ROW_BYTES]],
    occurrences: &[[u8; OCCURRENCE_ROW_BYTES]],
    members: &CanonicalVariablePool,
    documentation: &CanonicalVariablePool,
    extensions: &FullExtensionPayloads,
) -> Result<[Lane; 26], FullPlanError> {
    let canonical = semantic.typed.canonical();
    let mut lanes = [Lane::ZERO; 26];
    set_lane(
        &mut lanes,
        FullDirectoryKind::Atoms,
        checked_bytes(canonical.core.atoms.len(), ATOM_ROW_BYTES, FullSemanticImageField::Atoms)?,
        count(canonical.core.atoms.len(), FullSemanticImageField::Atoms)?,
    );
    set_lane(
        &mut lanes,
        FullDirectoryKind::AtomBytes,
        canonical.core.atom_bytes_len()?,
        count(canonical.core.atom_bytes_len()?, FullSemanticImageField::Atoms)?,
    );
    set_lane(
        &mut lanes,
        FullDirectoryKind::Entities,
        checked_bytes(entities.len(), ENTITY_ROW_BYTES, FullSemanticImageField::Entities)?,
        count(entities.len(), FullSemanticImageField::Entities)?,
    );
    set_lane(
        &mut lanes,
        FullDirectoryKind::TypedNodes,
        semantic.typed_wire.node_bytes_len()?,
        count(semantic.typed_wire.nodes.len(), FullSemanticImageField::TypedNodes)?,
    );
    set_lane(
        &mut lanes,
        FullDirectoryKind::TypedEdges,
        semantic.typed_wire.edge_bytes_len()?,
        count(semantic.typed_wire.edges.len(), FullSemanticImageField::TypedEdges)?,
    );
    set_terminal_lanes(
        &mut lanes,
        FullDirectoryKind::EntityLists,
        FullDirectoryKind::EntityListBytes,
        &members.ranges,
        &members.bytes,
        FullSemanticImageField::EntityLists,
    )?;
    set_terminal_lanes(
        &mut lanes,
        FullDirectoryKind::Documentation,
        FullDirectoryKind::DocumentationBytes,
        &documentation.ranges,
        &documentation.bytes,
        FullSemanticImageField::Documentation,
    )?;
    set_lane(
        &mut lanes,
        FullDirectoryKind::Externals,
        checked_bytes(externals.len(), EXTERNAL_ROW_BYTES, FullSemanticImageField::Externals)?,
        count(externals.len(), FullSemanticImageField::Externals)?,
    );
    set_lane(
        &mut lanes,
        FullDirectoryKind::Links,
        checked_bytes(links.len(), LINK_ROW_BYTES, FullSemanticImageField::Links)?,
        count(links.len(), FullSemanticImageField::Links)?,
    );
    set_lane(
        &mut lanes,
        FullDirectoryKind::Occurrences,
        checked_bytes(occurrences.len(), OCCURRENCE_ROW_BYTES, FullSemanticImageField::Occurrences)?,
        count(occurrences.len(), FullSemanticImageField::Occurrences)?,
    );
    set_extension_lanes(
        &mut lanes,
        FullDirectoryKind::TypeScriptFacts,
        FullDirectoryKind::TypeScriptBindings,
        &semantic.extensions.typescript,
        &extensions.typescript,
    )?;
    set_extension_lanes(
        &mut lanes,
        FullDirectoryKind::CSharpFacts,
        FullDirectoryKind::CSharpBindings,
        &semantic.extensions.csharp,
        &extensions.csharp,
    )?;
    set_extension_lanes(
        &mut lanes,
        FullDirectoryKind::GoFacts,
        FullDirectoryKind::GoBindings,
        &semantic.extensions.go,
        &extensions.go,
    )?;
    set_extension_lanes(
        &mut lanes,
        FullDirectoryKind::RustFacts,
        FullDirectoryKind::RustBindings,
        &semantic.extensions.rust,
        &extensions.rust,
    )?;
    set_extension_lanes(
        &mut lanes,
        FullDirectoryKind::PythonFacts,
        FullDirectoryKind::PythonBindings,
        &semantic.extensions.python,
        &extensions.python,
    )?;
    set_extension_lanes(
        &mut lanes,
        FullDirectoryKind::JavaFacts,
        FullDirectoryKind::JavaBindings,
        &semantic.extensions.java,
        &extensions.java,
    )?;
    set_extension_lanes(
        &mut lanes,
        FullDirectoryKind::ClangFacts,
        FullDirectoryKind::ClangBindings,
        &semantic.extensions.clang,
        &extensions.clang,
    )?;
    Ok(lanes)
}

fn set_terminal_lanes(
    lanes: &mut [Lane; 26],
    range_kind: FullDirectoryKind,
    bytes_kind: FullDirectoryKind,
    rows: &[crate::ArenaRange],
    values: &[u8],
    field: FullSemanticImageField,
) -> Result<(), FullSemanticImageFault> {
    set_lane(
        lanes,
        range_kind,
        checked_bytes(rows.len(), RANGE_ROW_BYTES, field)?,
        count(rows.len(), field)?,
    );
    set_lane(lanes, bytes_kind, values.len(), count(values.len(), field)?);
    Ok(())
}

fn set_extension_lanes(
    lanes: &mut [Lane; 26],
    facts: FullDirectoryKind,
    bindings: FullDirectoryKind,
    plan: &crate::semantic_image::full::ExtensionPlanePlan,
    payload: &CanonicalVariablePool,
) -> Result<(), FullSemanticImageFault> {
    let ranges = checked_bytes(payload.ranges.len(), RANGE_ROW_BYTES, FullSemanticImageField::ExtensionFacts)?;
    let length = ranges.checked_add(payload.bytes.len()).ok_or(
        FullSemanticImageFault::LengthOverflow {
            field: FullSemanticImageField::ExtensionFacts,
        },
    )?;
    set_lane(
        lanes,
        facts,
        length,
        count(payload.ranges.len(), FullSemanticImageField::ExtensionFacts)?,
    );
    set_lane(
        lanes,
        bindings,
        checked_bytes(
            plan.bindings.len(),
            SPARSE_BINDING_ROW_BYTES,
            FullSemanticImageField::ExtensionBindings,
        )?,
        count(plan.bindings.len(), FullSemanticImageField::ExtensionBindings)?,
    );
    Ok(())
}

fn layout(lanes: [Lane; 26]) -> Result<(FullImageLayout, usize, u32), FullSemanticImageFault> {
    let directory_bytes = DIRECTORY_BYTES
        .checked_mul(usize::from(FullDirectoryKind::count()))
        .ok_or(FullSemanticImageFault::LengthOverflow {
            field: FullSemanticImageField::Directory,
        })?;
    let mut next = HEADER_BYTES.checked_add(directory_bytes).ok_or(
        FullSemanticImageFault::LengthOverflow { field: FullSemanticImageField::Directory },
    )?;
    let mut entries = [FullDirectoryEntry {
        offset: 0,
        length: 0,
        offset_wire: 0,
        length_wire: 0,
        count: 0,
    }; 26];
    for kind in FullDirectoryKind::ALL {
        let lane = lanes[kind.index()];
        entries[kind.index()] = FullDirectoryEntry {
            offset: next,
            length: lane.length,
            offset_wire: count(next, FullSemanticImageField::Directory)?,
            length_wire: count(lane.length, FullSemanticImageField::Directory)?,
            count: lane.count,
        };
        next = next.checked_add(lane.length).ok_or(FullSemanticImageFault::LengthOverflow {
            field: FullSemanticImageField::Directory,
        })?;
    }
    Ok((
        FullImageLayout { entries },
        next,
        count(next, FullSemanticImageField::Header)?,
    ))
}

fn set_lane(lanes: &mut [Lane; 26], kind: FullDirectoryKind, length: usize, count: u32) {
    lanes[kind.index()] = Lane { length, count };
}

fn checked_bytes(
    rows: usize,
    width: usize,
    field: FullSemanticImageField,
) -> Result<usize, FullSemanticImageFault> {
    rows.checked_mul(width)
        .ok_or(FullSemanticImageFault::LengthOverflow { field })
}

fn count(value: usize, field: FullSemanticImageField) -> Result<u32, FullSemanticImageFault> {
    u32::try_from(value).map_err(|_| FullSemanticImageFault::LengthOverflow { field })
}

fn write_core_entity(
    output: &mut [u8; ENTITY_ROW_BYTES],
    entity: crate::SemanticEntity,
    canonical: &crate::semantic_image::canonical::CanonicalFullPlan<'_>,
) -> Result<(), FullPlanError> {
    put_u32_array(output, 0, canonical.atom(entity.name)?);
    put_u16_array(output, 4, u16::from(entity.kind));
    output[6] = visibility_code(entity.visibility);
    output[7] = parentage_code(entity.authority.parentage);
    put_u32_array(
        output,
        8,
        entity.parent.map(|value| canonical.entity(value)).transpose()?.unwrap_or(NONE),
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

fn external_row(
    target: ExternalTarget,
    canonical: &crate::semantic_image::canonical::CanonicalFullPlan<'_>,
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
                ForeignTargetOrigin::Namespace { ecosystem, namespace } => {
                    (1, canonical.atom(ecosystem)?, canonical.atom(namespace)?)
                }
                ForeignTargetOrigin::Universe { ecosystem } => (2, canonical.atom(ecosystem)?, NONE),
                ForeignTargetOrigin::Unspecified { ecosystem } => (3, canonical.atom(ecosystem)?, NONE),
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

fn link_row(
    link: crate::Link,
    canonical: &crate::semantic_image::canonical::CanonicalFullPlan<'_>,
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

fn occurrence_row(
    occurrence: crate::LinkOccurrence,
    authority: crate::OccurrenceAuthorityFacts,
    relation: u32,
    canonical: &crate::semantic_image::canonical::CanonicalFullPlan<'_>,
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
    canonical: &crate::semantic_image::canonical::CanonicalFullPlan<'_>,
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
fn put_u32_array<const N: usize>(output: &mut [u8; N], offset: usize, value: u32) {
    output[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
}
fn put_u32_slice(output: &mut [u8], offset: usize, value: u32) {
    output[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
}

const fn visibility_code(value: crate::Visibility) -> u8 {
    match value {
        crate::Visibility::Unknown => 0,
        crate::Visibility::Private => 1,
        crate::Visibility::Restricted => 2,
        crate::Visibility::Package => 3,
        crate::Visibility::Public => 4,
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
const fn confidence_code(value: crate::Confidence) -> u8 {
    match value {
        crate::Confidence::Syntactic => 0,
        crate::Confidence::Heuristic => 1,
        crate::Confidence::Indexed => 2,
        crate::Confidence::Imported => 3,
        crate::Confidence::Compiler => 4,
    }
}
const fn link_kind_code(value: crate::LinkKind) -> u8 {
    match value {
        crate::LinkKind::Calls => 0,
        crate::LinkKind::MethodCall => 1,
        crate::LinkKind::TypeReference => 2,
        crate::LinkKind::Reads => 3,
        crate::LinkKind::Writes => 4,
        crate::LinkKind::Imports => 5,
        crate::LinkKind::Implements => 6,
        crate::LinkKind::Overrides => 7,
        crate::LinkKind::Reexports => 8,
        crate::LinkKind::Inherits => 9,
        crate::LinkKind::Documents => 10,
    }
}
