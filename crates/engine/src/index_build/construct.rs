//! Defines construct behavior for `backend-engine index_build`, whose purpose is to project reopened compiler IR into immutable exact and lexical segments.
//! This module owns the construct invariants and typed state transitions.
//! Its narrow surface prevents representation and policy details from leaking outward.
use core::{cmp::Ordering, mem::MaybeUninit, ops::Deref};

use crate::index_build::{
    error::{BuildAdmissionError, BuildDerivationError, BuildError, BuildRegion},
    fact::{EntityFact, EntityProjection},
    initialized::{InitializationError, Initialized, try_initialize},
};
use crate::publication::OpenedFragment;
use crate::publication::manifest::StoredFragmentFacts;
use backend_semantic::index_core::{
    EntityArtifactIdentity, EntityDocumentId, ExactRow, ExactSegment, LexicalRow, LexicalScore,
    LexicalSegment, MAX_EXACT_ROWS, MAX_LEXICAL_ROWS,
};
use backend_semantic::ir::{Atom, FragmentView, TypeNode};
use backend_semantic::ir::{AtomId, EntityId, TypeId};

mod semantic;

pub use self::semantic::{
    SemanticIndexBuildCapacity, SemanticIndexBuildScratch, build_semantic, preflight_semantic,
};

/// Maximum declarations accepted by one builder invocation.
pub const MAX_INDEX_ROWS: usize = if MAX_EXACT_ROWS < MAX_LEXICAL_ROWS {
    MAX_EXACT_ROWS
} else {
    MAX_LEXICAL_ROWS
};

const ENTITY_NAME_SCORE_UNITS: u32 = 1;

type DerivationResult<Value> = Result<Value, BuildDerivationError>;

/// Caller-owned regions for one allocation-free index projection.
///
/// All regions are checked before the first write. Callers supply `MaybeUninit` slots; the only
/// conversion to initialized slices is sealed inside the reviewed `initialized` module after
/// every admitted slot is written. Empty placeholders are never admitted.
pub struct IndexBuildScratch<'bytes> {
    /// Sortable declaration metadata, one entry per fragment entity.
    pub projections: &'bytes mut [MaybeUninit<EntityProjection<'bytes>>],
    /// Typed canonical entity facts, one entry per fragment entity.
    pub entities: &'bytes mut [MaybeUninit<EntityFact<'bytes>>],
    /// Canonical exact-core rows, one entry per fragment entity.
    pub exact_rows: &'bytes mut [MaybeUninit<ExactRow<'bytes>>],
    /// Canonical lexical-core rows, one entry per fragment entity.
    pub lexical_rows: &'bytes mut [MaybeUninit<LexicalRow<'bytes>>],
    /// Direct atom lookup entries, one entry per fragment atom.
    pub atoms: &'bytes mut [MaybeUninit<Atom<'bytes>>],
    /// Direct type-node lookup entries, one entry per fragment type node.
    pub type_nodes: &'bytes mut [MaybeUninit<TypeNode>],
}

/// Exact caller capacities inspected by no-write admission.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct IndexBuildCapacity {
    /// Slots available for sortable declarations.
    pub projections: usize,
    /// Slots available for canonical entity facts.
    pub entities: usize,
    /// Slots available for existing exact-core rows.
    pub exact_rows: usize,
    /// Slots available for existing lexical-core rows.
    pub lexical_rows: usize,
    /// Slots available for direct atom lookup.
    pub atoms: usize,
    /// Slots available for direct type-node lookup.
    pub type_nodes: usize,
}

impl IndexBuildScratch<'_> {
    /// Reports the exact reusable capacities without writing any caller slot.
    #[must_use]
    pub const fn capacity(&self) -> IndexBuildCapacity {
        IndexBuildCapacity {
            projections: self.projections.len(),
            entities: self.entities.len(),
            exact_rows: self.exact_rows.len(),
            lexical_rows: self.lexical_rows.len(),
            atoms: self.atoms.len(),
            type_nodes: self.type_nodes.len(),
        }
    }
}

/// Nonforgeable complete output of one reopened compiler fragment index build.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PreparedIndex<'bytes>(PreparedIndexView<'bytes>);

/// Immutable public facts of one prepared index projection.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PreparedIndexView<'bytes> {
    /// Complete manifest-selected fragment authority for the reopened bytes that were indexed.
    pub fragment: StoredFragmentFacts,
    /// Canonically ordered typed declaration facts.
    pub entities: &'bytes [EntityFact<'bytes>],
    /// Canonical exact-core segment directly consumable by exact retrieval.
    pub exact: ExactSegment<'bytes>,
    /// Canonical lexical-core segment directly consumable by Tantivy and lexical retrieval.
    pub lexical: LexicalSegment<'bytes>,
}

impl<'bytes> Deref for PreparedIndex<'bytes> {
    type Target = PreparedIndexView<'bytes>;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

/// Checks bounded caller capacity for one manifest-named reopened fragment without writing.
///
/// # Errors
///
/// Returns the first exact caller-owned region that cannot hold the selected fragment's lanes.
pub fn preflight(
    fragment: &OpenedFragment<'_>,
    capacity: IndexBuildCapacity,
) -> Result<(), BuildAdmissionError> {
    RequiredScratch::from_view(&fragment.view).check(capacity)
}

/// Builds existing exact-core and lexical-core segment proofs from a manifest-named fragment.
///
/// The input is deliberately [`OpenedFragment`] rather than a raw `FragmentView`: only durable
/// reopen can construct it, binding the exact bytes to manifest facts and generation closure.
/// Entity declaration order is canonicalized; type-node coordinates are retained as compiler IR
/// semantics, as described by [`EntityFact`].
///
/// # Errors
///
/// Returns the exact caller-capacity, compact-IR-coordinate, initialization, or existing-core
/// segment rejection. Capacity is checked before any caller-owned slot is written.
#[allow(
    clippy::result_large_err,
    reason = "cold capacity and core-segment failures retain caller regions and borrowed rows"
)]
pub fn build<'opened, 'fragment: 'scratch, 'scratch>(
    fragment: &'opened OpenedFragment<'fragment>,
    scratch: IndexBuildScratch<'scratch>,
) -> Result<PreparedIndex<'scratch>, BuildError<'scratch>> {
    let required = RequiredScratch::from_view(&fragment.view);
    required.check(scratch.capacity())?;
    let selected = required.select(scratch)?;
    let atoms = collect_atoms(selected.atoms, &fragment.view)?;
    let type_nodes = collect_type_nodes(selected.type_nodes, &fragment.view)?;
    let projections =
        canonical_projections(selected.projections, &fragment.view, atoms, type_nodes)?;
    let artifact = EntityArtifactIdentity::Compact(fragment.facts.fragment);
    let entities = derive_entities(selected.entities, projections, artifact)?;
    let exact_rows = derive_exact_rows(selected.exact_rows, entities)?;
    let lexical_rows = derive_lexical_rows(selected.lexical_rows, entities, artifact)?;
    let exact = ExactSegment::new(exact_rows).map_err(|cause| BuildError::Exact { cause })?;
    let lexical =
        LexicalSegment::new(lexical_rows).map_err(|cause| BuildError::Lexical { cause })?;
    Ok(PreparedIndex(PreparedIndexView {
        fragment: fragment.facts,
        entities,
        exact,
        lexical,
    }))
}

struct SelectedScratch<'slots> {
    projections: &'slots mut [MaybeUninit<EntityProjection<'slots>>],
    entities: &'slots mut [MaybeUninit<EntityFact<'slots>>],
    exact_rows: &'slots mut [MaybeUninit<ExactRow<'slots>>],
    lexical_rows: &'slots mut [MaybeUninit<LexicalRow<'slots>>],
    atoms: &'slots mut [MaybeUninit<Atom<'slots>>],
    type_nodes: &'slots mut [MaybeUninit<TypeNode>],
}

fn collect_atoms<'slots>(
    output: &'slots mut [MaybeUninit<Atom<'slots>>],
    view: &FragmentView<'slots>,
) -> DerivationResult<&'slots [Atom<'slots>]> {
    initialize(BuildRegion::Atoms, output, view.atoms(), Ok).map(Initialized::into_shared)
}

fn collect_type_nodes<'slots>(
    output: &'slots mut [MaybeUninit<TypeNode>],
    view: &FragmentView<'slots>,
) -> DerivationResult<&'slots [TypeNode]> {
    initialize(BuildRegion::TypeNodes, output, view.type_nodes(), Ok).map(Initialized::into_shared)
}

fn canonical_projections<'slots>(
    output: &'slots mut [MaybeUninit<EntityProjection<'slots>>],
    view: &FragmentView<'slots>,
    atoms: &[Atom<'slots>],
    type_nodes: &[TypeNode],
) -> DerivationResult<&'slots [EntityProjection<'slots>]> {
    let mut projections = initialize(
        BuildRegion::Projections,
        output,
        view.entities(),
        |entity| {
            let name = resolve_atom(atoms, entity.entity, entity.name)?;
            let semantic_type = resolve_type(type_nodes, entity.entity, entity.semantic_type)?;
            Ok(EntityProjection {
                name: name.bytes,
                kind: entity.kind,
                semantic_type,
            })
        },
    )?;
    projections.sort_unstable_by(compare_projection);
    Ok(projections.into_shared())
}

fn derive_entities<'slots>(
    output: &'slots mut [MaybeUninit<EntityFact<'slots>>],
    projections: &[EntityProjection<'slots>],
    artifact: EntityArtifactIdentity,
) -> DerivationResult<&'slots [EntityFact<'slots>]> {
    initialize(
        BuildRegion::Entities,
        output,
        projections.iter().copied().enumerate(),
        |(ordinal, projection)| {
            let entity = EntityId::new(u32::try_from(ordinal).map_err(|source| {
                BuildDerivationError::EntityOrdinalAddressSpace { ordinal, source }
            })?);
            Ok(EntityFact::new(
                EntityDocumentId { artifact, entity },
                entity,
                projection.name,
                projection.kind,
                projection.semantic_type,
            ))
        },
    )
    .map(Initialized::into_shared)
}

fn derive_exact_rows<'slots>(
    output: &'slots mut [MaybeUninit<ExactRow<'slots>>],
    entities: &'slots [EntityFact<'slots>],
) -> DerivationResult<&'slots [ExactRow<'slots>]> {
    initialize(BuildRegion::ExactRows, output, entities.iter(), |entity| {
        Ok(ExactRow::present(
            entity.exact_key.as_ref(),
            entity.exact_value.as_ref(),
        ))
    })
    .map(Initialized::into_shared)
}

fn derive_lexical_rows<'slots>(
    output: &'slots mut [MaybeUninit<LexicalRow<'slots>>],
    entities: &[EntityFact<'slots>],
    artifact: EntityArtifactIdentity,
) -> DerivationResult<&'slots [LexicalRow<'slots>]> {
    initialize(
        BuildRegion::LexicalRows,
        output,
        entities.iter(),
        |entity| {
            let document = EntityDocumentId {
                artifact,
                entity: entity.entity,
            };
            Ok(LexicalRow::new(
                entity.name,
                document,
                LexicalScore::from(ENTITY_NAME_SCORE_UNITS),
            ))
        },
    )
    .map(Initialized::into_shared)
}

/// Exact caller-region requirements derived before any caller storage is changed.
#[derive(Clone, Copy)]
struct RequiredScratch {
    entities: usize,
    atoms: usize,
    type_nodes: usize,
}

impl RequiredScratch {
    fn from_view(view: &FragmentView<'_>) -> Self {
        Self {
            entities: view.entities().len(),
            atoms: view.atoms().len(),
            type_nodes: view.type_nodes().len(),
        }
    }

    fn check(self, capacity: IndexBuildCapacity) -> Result<(), BuildAdmissionError> {
        if self.entities > MAX_INDEX_ROWS {
            return Err(BuildAdmissionError::EntityLimit {
                maximum: MAX_INDEX_ROWS,
                observed: self.entities,
            });
        }
        for (region, required, available) in [
            (
                BuildRegion::Projections,
                self.entities,
                capacity.projections,
            ),
            (BuildRegion::Entities, self.entities, capacity.entities),
            (BuildRegion::ExactRows, self.entities, capacity.exact_rows),
            (
                BuildRegion::LexicalRows,
                self.entities,
                capacity.lexical_rows,
            ),
            (BuildRegion::Atoms, self.atoms, capacity.atoms),
            (BuildRegion::TypeNodes, self.type_nodes, capacity.type_nodes),
        ] {
            if available < required {
                return Err(BuildAdmissionError::OutputTooSmall {
                    region,
                    required,
                    available,
                });
            }
        }
        Ok(())
    }

    fn select(self, scratch: IndexBuildScratch<'_>) -> DerivationResult<SelectedScratch<'_>> {
        let IndexBuildScratch {
            projections,
            entities,
            exact_rows,
            lexical_rows,
            atoms,
            type_nodes,
        } = scratch;
        Ok(SelectedScratch {
            projections: selected_region(projections, self.entities, BuildRegion::Projections)?,
            entities: selected_region(entities, self.entities, BuildRegion::Entities)?,
            exact_rows: selected_region(exact_rows, self.entities, BuildRegion::ExactRows)?,
            lexical_rows: selected_region(lexical_rows, self.entities, BuildRegion::LexicalRows)?,
            atoms: selected_region(atoms, self.atoms, BuildRegion::Atoms)?,
            type_nodes: selected_region(type_nodes, self.type_nodes, BuildRegion::TypeNodes)?,
        })
    }
}

fn selected_region<Value>(
    output: &mut [MaybeUninit<Value>],
    required: usize,
    region: BuildRegion,
) -> DerivationResult<&mut [MaybeUninit<Value>]> {
    let available = output.len();
    output
        .get_mut(..required)
        .ok_or(BuildDerivationError::ScratchInitialization {
            region,
            required,
            available,
        })
}

fn initialize<Source, Value: Copy>(
    region: BuildRegion,
    output: &mut [MaybeUninit<Value>],
    source: impl ExactSizeIterator<Item = Source>,
    transform: impl FnMut(Source) -> DerivationResult<Value>,
) -> DerivationResult<Initialized<'_, Value>> {
    try_initialize(output, source, transform).map_err(|error| match error {
        InitializationError::Length {
            required,
            available,
        }
        | InitializationError::Exhausted {
            required,
            initialized: available,
        } => BuildDerivationError::ScratchInitialization {
            region,
            required,
            available,
        },
        InitializationError::Surplus { required } => BuildDerivationError::ScratchInitialization {
            region,
            required,
            available: required,
        },
        InitializationError::Value(error) => error,
    })
}

fn resolve_atom<'bytes>(
    atoms: &[Atom<'bytes>],
    entity: EntityId,
    name: AtomId,
) -> DerivationResult<Atom<'bytes>> {
    let index =
        usize::try_from(name.raw).map_err(|source| BuildDerivationError::AtomAddressSpace {
            entity,
            name,
            source,
        })?;
    atoms
        .get(index)
        .copied()
        .ok_or(BuildDerivationError::MissingAtom { entity, name })
}

fn resolve_type(
    type_nodes: &[TypeNode],
    entity: EntityId,
    semantic_type: TypeId,
) -> DerivationResult<TypeNode> {
    let index = usize::try_from(semantic_type.raw).map_err(|source| {
        BuildDerivationError::TypeAddressSpace {
            entity,
            semantic_type,
            source,
        }
    })?;
    type_nodes
        .get(index)
        .copied()
        .ok_or(BuildDerivationError::MissingTypeNode {
            entity,
            semantic_type,
        })
}

fn compare_projection(left: &EntityProjection<'_>, right: &EntityProjection<'_>) -> Ordering {
    left.name
        .cmp(right.name)
        .then_with(|| type_order(left.semantic_type, right.semantic_type))
        .then_with(|| u16::from(left.kind).cmp(&u16::from(right.kind)))
}

fn type_order(left: TypeNode, right: TypeNode) -> Ordering {
    match (left, right) {
        (TypeNode::Primitive(left), TypeNode::Primitive(right)) => {
            u32::from(left).cmp(&u32::from(right))
        }
        (TypeNode::Primitive(_), TypeNode::Reference(_)) => Ordering::Less,
        (TypeNode::Reference(_), TypeNode::Primitive(_)) => Ordering::Greater,
        (TypeNode::Reference(left), TypeNode::Reference(right)) => left.raw.cmp(&right.raw),
    }
}
