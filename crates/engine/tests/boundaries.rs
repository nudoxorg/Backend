//! Exercises the `backend-engine index_build` tests boundaries contract through its observable boundary.
//! The cases target malformed, partial, reordered, and resource-constrained behavior.
//! Assertions retain exact typed causes so regressions cannot pass through lossy errors.
//! Empty, overload, and bounded-admission attacks for the direct core segment builder.

mod build_support;

use core::{
    mem::{MaybeUninit, size_of_val},
    slice,
};

use backend_semantic::ir::{Atom, AtomInput, EntityKind, EntityRecord, PrimitiveType, TypeNode};
use backend_semantic::ir::{AtomId, TypeId};
use backend_engine::index_build::{
    BuildAdmissionError, BuildRegion, EntityFact, EntityProjection, IndexBuildCapacity,
    IndexBuildScratch, MAX_INDEX_ROWS, build, preflight,
};
use backend_semantic::index_core::{ExactOperation, ExactRow, LexicalRow};
use build_support::{
    BuildProofError, Fixture, OpenBuffers, TestError, compiled, next_fragment, publish,
    write_fragment, written,
};

#[test]
fn empty_reopened_fragment_builds_empty_existing_core_segments() -> Result<(), TestError> {
    let fixture = Fixture::new("empty")?;
    let publisher = fixture.publisher()?;
    let mut bytes = [0_u8; 512];
    let length = write_fragment(&mut bytes, b"empty", &[], &[], &[])?;
    publish(
        &publisher,
        &fixture.artifacts(),
        &[compiled(written(&bytes, length)?)?],
    )?;
    let mut reopened = OpenBuffers::new();
    let opened = reopened.open(&publisher, &fixture.artifacts())?;
    let fragment = next_fragment(&mut opened.fragments(), 0)?;
    let mut projections: [MaybeUninit<EntityProjection<'_>>; 0] = [];
    let mut entities: [MaybeUninit<EntityFact<'_>>; 0] = [];
    let mut exact: [MaybeUninit<ExactRow<'_>>; 0] = [];
    let mut lexical: [MaybeUninit<LexicalRow<'_>>; 0] = [];
    let mut atoms: [MaybeUninit<Atom<'_>>; 0] = [];
    let mut types: [MaybeUninit<TypeNode>; 0] = [];
    let index = build(
        &fragment,
        IndexBuildScratch {
            projections: &mut projections,
            entities: &mut entities,
            exact_rows: &mut exact,
            lexical_rows: &mut lexical,
            atoms: &mut atoms,
            type_nodes: &mut types,
        },
    )?;
    if !index.entities.is_empty()
        || !index.exact.rows.is_empty()
        || !index.lexical.rows.is_empty()
        || index.fragment.fragment_length != fragment.facts.fragment_length
    {
        return Err(TestError::BuildProof(
            BuildProofError::EmptyFragmentProjectionMismatch,
        ));
    }
    publisher.shutdown()?;
    fixture.remove()?;
    Ok(())
}

#[test]
fn every_short_region_rejects_before_mutating_caller_bytes() -> Result<(), TestError> {
    let fixture = Fixture::new("short-regions")?;
    let publisher = fixture.publisher()?;
    let mut bytes = [0_u8; 512];
    let length = write_fragment(
        &mut bytes,
        b"short-regions",
        &[entity(EntityKind::Function, 0)],
        &[TypeNode::Primitive(PrimitiveType::Bool)],
        &[AtomInput { bytes: b"short" }],
    )?;
    publish(
        &publisher,
        &fixture.artifacts(),
        &[compiled(written(&bytes, length)?)?],
    )?;
    let mut reopened = OpenBuffers::new();
    let opened = reopened.open(&publisher, &fixture.artifacts())?;
    let fragment = next_fragment(&mut opened.fragments(), 0)?;
    for region in [
        BuildRegion::Projections,
        BuildRegion::Entities,
        BuildRegion::ExactRows,
        BuildRegion::LexicalRows,
        BuildRegion::Atoms,
        BuildRegion::TypeNodes,
    ] {
        short_region(&fragment, region)?;
    }
    publisher.shutdown()?;
    fixture.remove()?;
    Ok(())
}

#[test]
fn maximum_duplicate_names_receive_unique_exact_entity_keys() -> Result<(), TestError> {
    let fixture = Fixture::new("maximum")?;
    let publisher = fixture.publisher()?;
    let entities = [entity(EntityKind::Function, 0); MAX_INDEX_ROWS];
    let mut bytes = [0_u8; MAX_INDEX_ROWS * 24 + 1_024];
    let length = write_fragment(
        &mut bytes,
        b"maximum",
        &entities,
        &[TypeNode::Primitive(PrimitiveType::Bool)],
        &[AtomInput { bytes: b"overload" }],
    )?;
    publish(
        &publisher,
        &fixture.artifacts(),
        &[compiled(written(&bytes, length)?)?],
    )?;
    let mut reopened = OpenBuffers::new();
    let opened = reopened.open(&publisher, &fixture.artifacts())?;
    let fragment = next_fragment(&mut opened.fragments(), 0)?;
    let mut projections = Box::<[EntityProjection<'_>]>::new_uninit_slice(MAX_INDEX_ROWS);
    let mut facts = Box::<[EntityFact<'_>]>::new_uninit_slice(MAX_INDEX_ROWS);
    let mut exact = Box::<[ExactRow<'_>]>::new_uninit_slice(MAX_INDEX_ROWS);
    let mut lexical = Box::<[LexicalRow<'_>]>::new_uninit_slice(MAX_INDEX_ROWS);
    let mut atoms = [MaybeUninit::<Atom<'_>>::uninit(); 1];
    let mut types = [MaybeUninit::<TypeNode>::uninit(); 1];
    let index = build(
        &fragment,
        IndexBuildScratch {
            projections: &mut projections,
            entities: &mut facts,
            exact_rows: &mut exact,
            lexical_rows: &mut lexical,
            atoms: &mut atoms,
            type_nodes: &mut types,
        },
    )?;
    if index.entities.len() != MAX_INDEX_ROWS
        || index.exact.rows.len() != MAX_INDEX_ROWS
        || index.lexical.rows.len() != MAX_INDEX_ROWS
    {
        return Err(TestError::BuildProof(
            BuildProofError::MaximumEntityCountRejected,
        ));
    }
    assert_maximum_exact_lookups(&index)?;
    publisher.shutdown()?;
    fixture.remove()?;
    Ok(())
}

#[test]
fn overloaded_name_retains_two_exact_entities_and_two_lexical_documents() -> Result<(), TestError> {
    let fixture = Fixture::new("overloads")?;
    let publisher = fixture.publisher()?;
    let mut bytes = [0_u8; 512];
    let length = write_fragment(
        &mut bytes,
        b"overloads",
        &[
            entity(EntityKind::Function, 0),
            entity(EntityKind::Record, 1),
        ],
        &[
            TypeNode::Primitive(PrimitiveType::Bool),
            TypeNode::Primitive(PrimitiveType::String),
        ],
        &[AtomInput { bytes: b"overload" }],
    )?;
    publish(
        &publisher,
        &fixture.artifacts(),
        &[compiled(written(&bytes, length)?)?],
    )?;
    let mut reopened = OpenBuffers::new();
    let opened = reopened.open(&publisher, &fixture.artifacts())?;
    let fragment = next_fragment(&mut opened.fragments(), 0)?;
    let mut projections = [MaybeUninit::<EntityProjection<'_>>::uninit(); 2];
    let mut facts = [MaybeUninit::<EntityFact<'_>>::uninit(); 2];
    let mut exact = [MaybeUninit::<ExactRow<'_>>::uninit(); 2];
    let mut lexical = [MaybeUninit::<LexicalRow<'_>>::uninit(); 2];
    let mut atoms = [MaybeUninit::<Atom<'_>>::uninit(); 1];
    let mut types = [MaybeUninit::<TypeNode>::uninit(); 2];
    let index = match build(
        &fragment,
        IndexBuildScratch {
            projections: &mut projections,
            entities: &mut facts,
            exact_rows: &mut exact,
            lexical_rows: &mut lexical,
            atoms: &mut atoms,
            type_nodes: &mut types,
        },
    ) {
        Ok(index) => index,
        Err(error) => {
            return Err(TestError::BuildProof(
                BuildProofError::DuplicateNameRejected {
                    cause: error.into(),
                },
            ));
        }
    };
    assert_overload_rows(&index)?;
    publisher.shutdown()?;
    fixture.remove()?;
    Ok(())
}

#[test]
fn one_over_maximum_rejects_before_writing_any_region() -> Result<(), TestError> {
    const OVER_LIMIT: usize = MAX_INDEX_ROWS + 1;

    let fixture = Fixture::new("over-limit")?;
    let publisher = fixture.publisher()?;
    let entities = [entity(EntityKind::Constant, 0); OVER_LIMIT];
    let mut bytes = [0_u8; OVER_LIMIT * 24 + 1_024];
    let length = write_fragment(
        &mut bytes,
        b"over-limit",
        &entities,
        &[TypeNode::Primitive(PrimitiveType::I32)],
        &[AtomInput { bytes: b"too-many" }],
    )?;
    publish(
        &publisher,
        &fixture.artifacts(),
        &[compiled(written(&bytes, length)?)?],
    )?;
    let mut reopened = OpenBuffers::new();
    let opened = reopened.open(&publisher, &fixture.artifacts())?;
    let fragment = next_fragment(&mut opened.fragments(), 0)?;
    let projections = [MaybeUninit::<EntityProjection<'_>>::zeroed(); 1];
    let facts = [MaybeUninit::<EntityFact<'_>>::zeroed(); 1];
    let exact = [MaybeUninit::<ExactRow<'_>>::zeroed(); 1];
    let lexical = [MaybeUninit::<LexicalRow<'_>>::zeroed(); 1];
    let atoms = [MaybeUninit::<Atom<'_>>::zeroed(); 1];
    let types = [MaybeUninit::<TypeNode>::zeroed(); 1];
    assert_one_over_limit_rejection(&fragment)?;
    if !slots_are_zero(&projections)
        || !slots_are_zero(&facts)
        || !slots_are_zero(&exact)
        || !slots_are_zero(&lexical)
        || !slots_are_zero(&atoms)
        || !slots_are_zero(&types)
    {
        return Err(TestError::BuildProof(
            BuildProofError::EntityLimitPreflightFailed,
        ));
    }
    publisher.shutdown()?;
    fixture.remove()?;
    Ok(())
}

#[test]
fn fragment_beyond_two_hundred_fifty_six_entities_indexes_into_existing_segments()
-> Result<(), TestError> {
    const ENTITY_COUNT: usize = 300;

    let fixture = Fixture::new("beyond-256")?;
    let publisher = fixture.publisher()?;
    let entities = [entity(EntityKind::Function, 0); ENTITY_COUNT];
    let mut bytes = [0_u8; ENTITY_COUNT * 24 + 1_024];
    let length = write_fragment(
        &mut bytes,
        b"beyond-two-hundred-fifty-six",
        &entities,
        &[TypeNode::Primitive(PrimitiveType::Bool)],
        &[AtomInput { bytes: b"wide" }],
    )?;
    publish(
        &publisher,
        &fixture.artifacts(),
        &[compiled(written(&bytes, length)?)?],
    )?;
    let mut reopened = OpenBuffers::new();
    let opened = reopened.open(&publisher, &fixture.artifacts())?;
    let fragment = next_fragment(&mut opened.fragments(), 0)?;
    let mut projections = Box::<[EntityProjection<'_>]>::new_uninit_slice(ENTITY_COUNT);
    let mut facts = Box::<[EntityFact<'_>]>::new_uninit_slice(ENTITY_COUNT);
    let mut exact = Box::<[ExactRow<'_>]>::new_uninit_slice(ENTITY_COUNT);
    let mut lexical = Box::<[LexicalRow<'_>]>::new_uninit_slice(ENTITY_COUNT);
    let mut atoms = [MaybeUninit::<Atom<'_>>::uninit(); 1];
    let mut types = [MaybeUninit::<TypeNode>::uninit(); 1];
    let index = build(
        &fragment,
        IndexBuildScratch {
            projections: &mut projections,
            entities: &mut facts,
            exact_rows: &mut exact,
            lexical_rows: &mut lexical,
            atoms: &mut atoms,
            type_nodes: &mut types,
        },
    )?;
    if index.entities.len() != ENTITY_COUNT
        || index.exact.rows.len() != ENTITY_COUNT
        || index.lexical.rows.len() != ENTITY_COUNT
    {
        return Err(TestError::BuildProof(
            BuildProofError::MaximumEntityCountRejected,
        ));
    }
    for fact in index.entities.iter() {
        let Some(row) = index
            .exact
            .lookup(ExactOperation::new(fact.exact_key.as_ref()))
        else {
            return Err(TestError::BuildProof(
                BuildProofError::ExactEntityLookupMismatch,
            ));
        };
        if row.value_bytes().is_none() {
            return Err(TestError::BuildProof(
                BuildProofError::ExactEntityLookupMismatch,
            ));
        }
    }
    publisher.shutdown()?;
    fixture.remove()?;
    Ok(())
}

const fn entity(kind: EntityKind, semantic_type: u32) -> EntityRecord {
    EntityRecord {
        semantic_type: TypeId::new(semantic_type),
        name: AtomId::new(0),
        kind,
    }
}

fn assert_one_over_limit_rejection(
    fragment: &backend_engine::publication::OpenedFragment<'_>,
) -> Result<(), TestError> {
    match preflight(
        fragment,
        IndexBuildCapacity {
            projections: 1,
            entities: 1,
            exact_rows: 1,
            lexical_rows: 1,
            atoms: 1,
            type_nodes: 1,
        },
    ) {
        Err(BuildAdmissionError::EntityLimit {
            maximum: MAX_INDEX_ROWS,
            observed,
        }) if observed == MAX_INDEX_ROWS + 1 => Ok(()),
        Err(error) => Err(error.into()),
        Ok(()) => Err(TestError::BuildProof(
            BuildProofError::EntityLimitPreflightFailed,
        )),
    }
}

fn assert_maximum_exact_lookups(
    index: &backend_engine::index_build::PreparedIndex<'_>,
) -> Result<(), TestError> {
    for (ordinal, fact) in index.entities.iter().enumerate() {
        let ordinal = u32::try_from(ordinal)?;
        let Some(row) = index
            .exact
            .lookup(ExactOperation::new(fact.exact_key.as_ref()))
        else {
            return Err(TestError::BuildProof(
                BuildProofError::ExactEntityLookupMismatch,
            ));
        };
        if row.key != fact.exact_key.as_ref()
            || row.value_bytes().is_none()
            || fact.entity.raw != ordinal
        {
            return Err(TestError::BuildProof(
                BuildProofError::ExactEntityLookupMismatch,
            ));
        }
    }
    Ok(())
}

fn assert_overload_rows(index: &backend_engine::index_build::PreparedIndex<'_>) -> Result<(), TestError> {
    let (Some(first_exact), Some(second_exact), Some(first_lexical), Some(second_lexical)) = (
        index.exact.rows.first(),
        index.exact.rows.get(1),
        index.lexical.rows.first(),
        index.lexical.rows.get(1),
    ) else {
        return Err(TestError::BuildProof(
            BuildProofError::ExactEntityLookupMismatch,
        ));
    };
    if index.exact.rows.len() != 2
        || index.lexical.rows.len() != 2
        || first_exact.key == second_exact.key
        || first_lexical.term != second_lexical.term
        || first_lexical.document == second_lexical.document
    {
        return Err(TestError::BuildProof(
            BuildProofError::ExactEntityLookupMismatch,
        ));
    }
    Ok(())
}

fn short_region(
    fragment: &backend_engine::publication::OpenedFragment<'_>,
    region: BuildRegion,
) -> Result<(), TestError> {
    let projections = [MaybeUninit::<EntityProjection<'_>>::zeroed(); 1];
    let facts = [MaybeUninit::<EntityFact<'_>>::zeroed(); 1];
    let exact = [MaybeUninit::<ExactRow<'_>>::zeroed(); 1];
    let lexical = [MaybeUninit::<LexicalRow<'_>>::zeroed(); 1];
    let atoms = [MaybeUninit::<Atom<'_>>::zeroed(); 1];
    let types = [MaybeUninit::<TypeNode>::zeroed(); 1];
    let capacity = |current| usize::from(region != current);
    match preflight(
        fragment,
        IndexBuildCapacity {
            projections: capacity(BuildRegion::Projections),
            entities: capacity(BuildRegion::Entities),
            exact_rows: capacity(BuildRegion::ExactRows),
            lexical_rows: capacity(BuildRegion::LexicalRows),
            atoms: capacity(BuildRegion::Atoms),
            type_nodes: capacity(BuildRegion::TypeNodes),
        },
    ) {
        Err(BuildAdmissionError::OutputTooSmall {
            region: observed,
            required: 1,
            available: 0,
        }) if observed == region => {}
        Err(error) => return Err(error.into()),
        Ok(()) => {
            return Err(TestError::BuildProof(
                BuildProofError::UndersizedOutputAccepted { region },
            ));
        }
    }
    if !slots_are_zero(&projections)
        || !slots_are_zero(&facts)
        || !slots_are_zero(&exact)
        || !slots_are_zero(&lexical)
        || !slots_are_zero(&atoms)
        || !slots_are_zero(&types)
    {
        return Err(TestError::BuildProof(
            BuildProofError::PreflightMutatedOutput,
        ));
    }
    Ok(())
}

#[allow(
    unsafe_code,
    reason = "test-only byte proof reads known-zeroed MaybeUninit storage after build preflight"
)]
fn slots_are_zero<Value>(slots: &[MaybeUninit<Value>]) -> bool {
    // SAFETY: every slot was created by `MaybeUninit::zeroed`, so this reads initialized byte
    // storage through `u8` without claiming that the represented `Value` is initialized.
    let bytes = unsafe { slice::from_raw_parts(slots.as_ptr().cast::<u8>(), size_of_val(slots)) };
    bytes.iter().all(|byte| *byte == 0)
}
