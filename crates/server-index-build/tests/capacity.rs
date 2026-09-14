//! Exercises the `server-index-build` tests capacity contract through its observable boundary.
//! The cases target malformed, partial, reordered, and resource-constrained behavior.
//! Assertions retain exact typed causes so regressions cannot pass through lossy errors.
//! Capacity and caller-storage reuse attacks for compiler-authoritative index building.

mod support;

use core::mem::MaybeUninit;

use backend_semantic::ir::{Atom, AtomInput, EntityKind, EntityRecord, PrimitiveType, TypeNode};
use backend_semantic::ir::{AtomId, TypeId};
use server_index_build::{
    BuildAdmissionError, BuildRegion, EntityFact, EntityProjection, IndexBuildCapacity,
    IndexBuildScratch, build, preflight,
};
use backend_semantic::index_core::{ExactRow, LexicalRow};
use support::{
    BuildProofError, Fixture, OpenBuffers, TestError, compiled, next_fragment, publish,
    write_fragment, written,
};

#[test]
fn undersized_lexical_output_does_not_poison_reusable_caller_regions() -> Result<(), TestError> {
    let fixture = Fixture::new("capacity")?;
    let publisher = fixture.publisher()?;
    let mut bytes = [0_u8; 512];
    let length = write_fragment(
        &mut bytes,
        b"capacity-source",
        &[EntityRecord {
            semantic_type: TypeId::new(0),
            name: AtomId::new(0),
            kind: EntityKind::Constant,
        }],
        &[TypeNode::Primitive(PrimitiveType::Bool)],
        &[AtomInput { bytes: b"capacity" }],
    )?;
    publish(
        &publisher,
        &fixture.artifacts(),
        &[compiled(written(&bytes, length)?)?],
    )?;
    let mut reopened = OpenBuffers::new();
    let opened = reopened.open(&publisher, &fixture.artifacts())?;
    let mut fragments = opened.fragments();
    let fragment = next_fragment(&mut fragments, 0)?;
    let mut projections = [MaybeUninit::<EntityProjection<'_>>::uninit(); 1];
    let mut entities = [MaybeUninit::<EntityFact<'_>>::uninit(); 1];
    let mut exact = [MaybeUninit::<ExactRow<'_>>::uninit(); 1];
    let mut atoms = [MaybeUninit::<Atom<'_>>::uninit(); 1];
    let mut types = [MaybeUninit::<TypeNode>::uninit(); 1];
    rejects_missing_lexical(&fragment)?;
    let mut lexical = [MaybeUninit::<LexicalRow<'_>>::uninit(); 1];
    let reused = build(
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
    if reused.exact.rows.len() != 1 || reused.lexical.rows.len() != 1 {
        return Err(TestError::BuildProof(
            BuildProofError::PreflightMutatedOutput,
        ));
    }
    publisher.shutdown()?;
    fixture.remove()?;
    Ok(())
}

fn rejects_missing_lexical(
    fragment: &backend_engine::publication::OpenedFragment<'_>,
) -> Result<(), TestError> {
    match preflight(
        fragment,
        IndexBuildCapacity {
            projections: 1,
            entities: 1,
            exact_rows: 1,
            lexical_rows: 0,
            atoms: 1,
            type_nodes: 1,
        },
    ) {
        Err(BuildAdmissionError::OutputTooSmall {
            region: BuildRegion::LexicalRows,
            required: 1,
            available: 0,
        }) => Ok(()),
        Err(error) => Err(error.into()),
        Ok(()) => Err(TestError::BuildProof(
            BuildProofError::PreflightMutatedOutput,
        )),
    }
}
