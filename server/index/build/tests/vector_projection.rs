//! Exercises caller-embedded vector derivation and its capacity-first contract.
//! The cases prove external trait implementation, canonical points, and sentinel-preserving admission.
//! Assertions retain exact typed projection errors.
mod support;

use core::mem::MaybeUninit;

use compiler_ir::{AtomInput, EntityKind, EntityRecord, PrimitiveType, TypeNode};
use compiler_ir_vocabulary::{AtomId, TypeId};
use server_index_build::{
    CoordinateLane, EntityEmbedder, VectorProjectionError, VectorProjectionScratch,
    build_vector_projection,
};
use server_index_graph_vector::{
    MAX_VECTOR_DIMENSION, Metric, ModelId, PartitionId, ValidatedVectorSegment, VectorAuthority,
};
use server_index_vocabulary::IndexSnapshotId;
use support::{
    BuildBuffers, Fixture, OpenBuffers, TestError, compiled, next_fragment, publish,
    write_fragment, written,
};
use thiserror::Error;

#[derive(Debug, Error)]
enum VectorTestError {
    #[error(transparent)]
    Fixture(#[from] TestError),
    #[error(transparent)]
    Projection(#[from] VectorProjectionError),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Shutdown(#[from] server_journal::ShutdownError),
    #[error("projection did not contain its admitted segment")]
    MissingSegment,
    #[error("segment did not contain its admitted point")]
    MissingPoint,
}

struct TestEmbedder;

impl EntityEmbedder for TestEmbedder {
    const DIMENSION: usize = 2;

    fn model(&self) -> ModelId {
        ModelId::new([7; 16])
    }
    fn metric(&self) -> Metric {
        Metric::SquaredEuclidean
    }
    fn embed(&self, fact: server_index_build::EntityFactView<'_>, coordinates: &mut [i16]) {
        let Ok(entity) = i16::try_from(fact.entity.raw) else { return };
        let Ok(name_length) = i16::try_from(fact.name.len()) else { return };
        let mut cells = coordinates.iter_mut();
        if let Some(cell) = cells.next() {
            *cell = entity;
        }
        if let Some(cell) = cells.next() {
            *cell = name_length;
        }
    }
}

#[test]
fn external_embedder_projects_canonical_points() -> Result<(), VectorTestError> {
    let fixture = Fixture::new("vector-projection")?;
    let publisher = fixture.publisher()?;
    let mut bytes = [0_u8; 512];
    let length = write_fragment(
        &mut bytes,
        b"vector-projection",
        &[EntityRecord {
            semantic_type: TypeId::new(0),
            name: AtomId::new(0),
            kind: EntityKind::Function,
        }],
        &[TypeNode::Primitive(PrimitiveType::I32)],
        &[AtomInput { bytes: b"one" }],
    )?;
    publish(
        &publisher,
        &fixture.artifacts(),
        &[compiled(written(&bytes, length)?)?],
    )?;
    let mut opened = OpenBuffers::new();
    let opened = opened.open(&publisher, &fixture.artifacts())?;
    let mut fragments = opened.fragments();
    let fragment = next_fragment(&mut fragments, 0)?;
    let mut build = BuildBuffers::new();
    let prepared = build.build(&fragment)?;
    let mut segments = [MaybeUninit::<ValidatedVectorSegment<'_>>::uninit(); 4];
    let mut points = [MaybeUninit::uninit(); 1];
    let mut coordinates = [99_i16; 2];
    let authority = VectorAuthority::new(
        IndexSnapshotId::from_canonical_bytes(b"vector-snapshot"),
        ModelId::new([7; 16]),
        2,
        Metric::SquaredEuclidean,
    );
    let projection = build_vector_projection(
        &prepared,
        authority,
        PartitionId::new(0),
        &TestEmbedder,
        VectorProjectionScratch {
            segments: &mut segments,
            points: &mut points,
            coordinates: CoordinateLane::new(&mut coordinates),
        },
    )?;
    assert_eq!(projection.segments.len(), 1);
    let segment = projection.segments.iter().next().ok_or(VectorTestError::MissingSegment)?;
    assert_eq!(segment.authority, authority);
    assert_eq!(segment.partition, PartitionId::new(0));
    assert_eq!(segment.point_count, 1);
    assert_eq!(segment.id, segment.descriptor().id);
    let point = segment.facts.iter().next().ok_or(VectorTestError::MissingPoint)?;
    assert_eq!(point.coordinates, &[0, 3]);
    publisher.shutdown()?;
    fixture.remove()?;
    Ok(())
}

#[test]
fn dimension_rejection_preserves_coordinate_sentinel() -> Result<(), VectorTestError> {
    let fixture = Fixture::new("vector-dimension")?;
    let publisher = fixture.publisher()?;
    let mut bytes = [0_u8; 512];
    let length = write_fragment(
        &mut bytes,
        b"vector-dimension",
        &[EntityRecord {
            semantic_type: TypeId::new(0),
            name: AtomId::new(0),
            kind: EntityKind::Function,
        }],
        &[TypeNode::Primitive(PrimitiveType::I32)],
        &[AtomInput { bytes: b"one" }],
    )?;
    publish(
        &publisher,
        &fixture.artifacts(),
        &[compiled(written(&bytes, length)?)?],
    )?;
    let mut opened = OpenBuffers::new();
    let opened = opened.open(&publisher, &fixture.artifacts())?;
    let mut fragments = opened.fragments();
    let fragment = next_fragment(&mut fragments, 0)?;
    let mut build = BuildBuffers::new();
    let prepared = build.build(&fragment)?;
    let mut segments = [MaybeUninit::<ValidatedVectorSegment<'_>>::uninit(); 4];
    let mut points = [MaybeUninit::uninit(); 1];
    let mut coordinates = [99_i16; MAX_VECTOR_DIMENSION];
    let authority = VectorAuthority::new(
        IndexSnapshotId::from_canonical_bytes(b"vector-snapshot"),
        ModelId::new([7; 16]),
        3,
        Metric::SquaredEuclidean,
    );
    let result = build_vector_projection(
        &prepared,
        authority,
        PartitionId::new(0),
        &TestEmbedder,
        VectorProjectionScratch {
            segments: &mut segments,
            points: &mut points,
            coordinates: CoordinateLane::new(&mut coordinates),
        },
    );
    assert_eq!(
        result,
        Err(VectorProjectionError::DimensionAuthority {
            expected: 2,
            observed: 3
        })
    );
    assert!(coordinates.iter().all(|cell| *cell == 99));
    publisher.shutdown()?;
    fixture.remove()?;
    Ok(())
}
