//! Measured all-or-nothing full-image write preparation.
//!
//! This is the only place where owned `Ir` facts are read for the full wire.
//! It finishes canonical remapping, atom/entity/external lookups, and every
//! checked length conversion before an encoder receives caller output.  The
//! write phase can then copy pre-admitted byte rows without a late failure.

use alloc::vec::Vec;

use crate::ir::{
    ImageProvenance, Ir, SemanticReader, semantic_image::fault::CoreSemanticImageFault,
};

use super::{
    fault::{FullSemanticImageFault, FullSemanticImageField},
    wire::{
        ATOM_ROW_BYTES, DIRECTORY_BYTES, ENTITY_ROW_BYTES, EXTERNAL_ROW_BYTES, FullDirectoryEntry,
        FullDirectoryKind, FullImageLayout, HEADER_BYTES, LINK_ROW_BYTES, NONE,
        OCCURRENCE_ROW_BYTES, RANGE_ROW_BYTES, SPARSE_BINDING_ROW_BYTES,
    },
};
use crate::ir::semantic_image::full::{FullPlanError, FullSemanticPlan};

mod rows;

use rows::{external_row, link_row, occurrence_row, put_u32_array, write_core_entity};

/// Fully remapped rows and measured directory spans for an infallible full
/// image writer.  All vectors are proportional to real rows/edges; the plan
/// never allocates per child or reserves a max-of-language geometry.
pub(super) struct FullSemanticImagePlan<'image> {
    pub(super) semantic: FullSemanticPlan<'image>,
    pub(super) image: crate::ir::SemanticImageFacts,
    pub(super) provenance_scope_atoms: [u32; 4],
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
    pub(super) ranges: Vec<crate::ir::ArenaRange>,
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
    const ZERO: Self = Self {
        length: 0,
        count: 0,
    };
}

impl<'image> FullSemanticImagePlan<'image> {
    pub(super) fn build(ir: &'image Ir) -> Result<Self, FullPlanError> {
        let semantic = FullSemanticPlan::build(ir)?;
        let canonical = semantic.typed.canonical();
        let image = canonical.core.image;
        let provenance_scope_atoms = match image.provenance {
            // Zero is the optional-coordinate sentinel. Package coordinates
            // are encoded one-based so images written before this field was
            // introduced (whose reserved cell is zero) remain coordinate-free.
            ImageProvenance::Unavailable => [0, 0, 0, 0],
            ImageProvenance::Captured { scope, .. } => [
                canonical.atom(scope.ecosystem)?,
                canonical.atom(scope.package)?,
                canonical.atom(scope.path)?,
                scope.coordinate.map_or(Ok(0), |atom| {
                    canonical.atom(atom)?.checked_add(1).ok_or_else(|| {
                        FullPlanError::from(CoreSemanticImageFault::LengthOverflow {
                            field: crate::ir::semantic_image::fault::CoreSemanticImageField::Provenance,
                        })
                    })
                })?,
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
    plan: &crate::ir::semantic_image::full::ExtensionPlanePlan,
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
    source_ranges: &[crate::ir::ArenaRange],
    source_bytes: &[u8],
    field: FullSemanticImageField,
) -> Result<CanonicalVariablePool, FullSemanticImageFault> {
    let mut ranges = Vec::with_capacity(order.len());
    let mut bytes = Vec::with_capacity(source_bytes.len());
    let source_count = count(source_ranges.len(), field)?;
    for raw in order.iter().copied() {
        let index = usize::try_from(raw).map_err(|_| FullSemanticImageFault::Reference {
            field,
            row: 0,
            expected: source_count,
            observed: raw,
        })?;
        let range = source_ranges
            .get(index)
            .copied()
            .ok_or(FullSemanticImageFault::Reference {
                field,
                row: count(ranges.len(), field)?,
                expected: source_count,
                observed: raw,
            })?;
        let start = usize::try_from(range.start)
            .map_err(|_| FullSemanticImageFault::LengthOverflow { field })?;
        let length = usize::try_from(range.len)
            .map_err(|_| FullSemanticImageFault::LengthOverflow { field })?;
        let end = start
            .checked_add(length)
            .ok_or(FullSemanticImageFault::LengthOverflow { field })?;
        let value = source_bytes
            .get(start..end)
            .ok_or(FullSemanticImageFault::Reference {
                field,
                row: count(ranges.len(), field)?,
                expected: count(source_bytes.len(), field)?,
                observed: range.start,
            })?;
        let destination = count(bytes.len(), field)?;
        bytes.extend_from_slice(value);
        ranges.push(crate::ir::ArenaRange {
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
        let facts = ir
            .semantic_entity(row.entity)
            .ok_or(FullSemanticImageFault::Reference {
                field: FullSemanticImageField::Entities,
                row: u32::try_from(index).map_err(|_| FullSemanticImageFault::LengthOverflow {
                    field: FullSemanticImageField::Entities,
                })?,
                expected: u32::try_from(rows.len()).map_err(|_| {
                    FullSemanticImageFault::LengthOverflow {
                        field: FullSemanticImageField::Entities,
                    }
                })?,
                observed: row.entity.raw,
            })?;
        let mut bytes = [0_u8; ENTITY_ROW_BYTES];
        write_core_entity(&mut bytes, facts, canonical)?;
        put_u32_array(
            &mut bytes,
            120,
            match row.semantic_type {
                Some(value) => value,
                None => NONE,
            },
        );
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
        let target = ir
            .external(external)
            .copied()
            .ok_or(FullSemanticImageFault::Reference {
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
                FullSemanticImageFault::LengthOverflow {
                    field: FullSemanticImageField::Links,
                }
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
        let occurrence = ir
            .link_occurrence(id)
            .ok_or(FullSemanticImageFault::Reference {
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
        let authority = ir
            .occurrence_authority(id)
            .ok_or(FullSemanticImageFault::Reference {
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
        checked_bytes(
            canonical.core.atoms.len(),
            ATOM_ROW_BYTES,
            FullSemanticImageField::Atoms,
        )?,
        count(canonical.core.atoms.len(), FullSemanticImageField::Atoms)?,
    );
    set_lane(
        &mut lanes,
        FullDirectoryKind::AtomBytes,
        canonical.core.atom_bytes_len()?,
        count(
            canonical.core.atom_bytes_len()?,
            FullSemanticImageField::Atoms,
        )?,
    );
    set_lane(
        &mut lanes,
        FullDirectoryKind::Entities,
        checked_bytes(
            entities.len(),
            ENTITY_ROW_BYTES,
            FullSemanticImageField::Entities,
        )?,
        count(entities.len(), FullSemanticImageField::Entities)?,
    );
    set_lane(
        &mut lanes,
        FullDirectoryKind::TypedNodes,
        semantic.typed_wire.node_bytes_len()?,
        count(
            semantic.typed_wire.nodes.len(),
            FullSemanticImageField::TypedNodes,
        )?,
    );
    set_lane(
        &mut lanes,
        FullDirectoryKind::TypedEdges,
        semantic.typed_wire.edge_bytes_len()?,
        count(
            semantic.typed_wire.edges.len(),
            FullSemanticImageField::TypedEdges,
        )?,
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
        checked_bytes(
            externals.len(),
            EXTERNAL_ROW_BYTES,
            FullSemanticImageField::Externals,
        )?,
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
        checked_bytes(
            occurrences.len(),
            OCCURRENCE_ROW_BYTES,
            FullSemanticImageField::Occurrences,
        )?,
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
    rows: &[crate::ir::ArenaRange],
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
    plan: &crate::ir::semantic_image::full::ExtensionPlanePlan,
    payload: &CanonicalVariablePool,
) -> Result<(), FullSemanticImageFault> {
    let ranges = checked_bytes(
        payload.ranges.len(),
        RANGE_ROW_BYTES,
        FullSemanticImageField::ExtensionFacts,
    )?;
    let length =
        ranges
            .checked_add(payload.bytes.len())
            .ok_or(FullSemanticImageFault::LengthOverflow {
                field: FullSemanticImageField::ExtensionFacts,
            })?;
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
        count(
            plan.bindings.len(),
            FullSemanticImageField::ExtensionBindings,
        )?,
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
        FullSemanticImageFault::LengthOverflow {
            field: FullSemanticImageField::Directory,
        },
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
        next = next
            .checked_add(lane.length)
            .ok_or(FullSemanticImageFault::LengthOverflow {
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
