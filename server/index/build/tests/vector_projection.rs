//! Exercises caller-embedded vector derivation and its capacity-first contract.
//! The cases prove external trait implementation, canonical points, and sentinel-preserving admission.
//! Assertions retain exact typed projection errors.
mod support;

use core::mem::MaybeUninit;

use allocation_counter::{AllocationInfo, measure};
use compiler_ir::Atom;
use compiler_ir::{AtomInput, EntityKind, EntityRecord, PrimitiveType, TypeNode};
use compiler_ir_vocabulary::{AtomId, TypeId};
use server_index_build::{
    CoordinateLane, EntityEmbedder, EntityProjection, IndexBuildScratch, MAX_VECTOR_SEGMENTS,
    VectorProjectionError, VectorProjectionScratch, build, build_vector_projection,
};
use server_index_core::{ExactRow, LexicalRow};
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

struct ZeroEmbedder;
struct WideEmbedder;

impl EntityEmbedder for ZeroEmbedder {
    const DIMENSION: usize = 0;
    fn model(&self) -> ModelId {
        ModelId::new([8; 16])
    }
    fn metric(&self) -> Metric {
        Metric::SquaredEuclidean
    }
    fn embed(&self, _fact: server_index_build::EntityFactView<'_>, _coordinates: &mut [i16]) {}
}

impl EntityEmbedder for WideEmbedder {
    const DIMENSION: usize = 17;
    fn model(&self) -> ModelId {
        ModelId::new([8; 16])
    }
    fn metric(&self) -> Metric {
        Metric::SquaredEuclidean
    }
    fn embed(&self, _fact: server_index_build::EntityFactView<'_>, _coordinates: &mut [i16]) {}
}

impl EntityEmbedder for TestEmbedder {
    const DIMENSION: usize = 2;

    fn model(&self) -> ModelId {
        ModelId::new([7; 16])
    }
    fn metric(&self) -> Metric {
        Metric::SquaredEuclidean
    }
    fn embed(&self, fact: server_index_build::EntityFactView<'_>, coordinates: &mut [i16]) {
        let Ok(entity) = i16::try_from(fact.entity.raw) else {
            return;
        };
        let Ok(name_length) = i16::try_from(fact.name.len()) else {
            return;
        };
        let mut cells = coordinates.iter_mut();
        if let Some(cell) = cells.next() {
            *cell = entity;
        }
        if let Some(cell) = cells.next() {
            *cell = name_length;
        }
    }
}

fn with_prepared<R>(
    label: &str,
    entities: &[EntityRecord],
    action: impl FnOnce(&server_index_build::PreparedIndex<'_>) -> Result<R, VectorTestError>,
) -> Result<R, VectorTestError> {
    let fixture = Fixture::new(label)?;
    let publisher = fixture.publisher()?;
    let mut bytes = [0_u8; 16_384];
    let length = write_fragment(
        &mut bytes,
        label.as_bytes(),
        entities,
        &[TypeNode::Primitive(PrimitiveType::I32)],
        &[AtomInput { bytes: b"entity" }],
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
    let mut projections = [MaybeUninit::<EntityProjection<'_>>::uninit(); 128];
    let mut facts = [MaybeUninit::<server_index_build::EntityFact<'_>>::uninit(); 128];
    let mut exact = [MaybeUninit::<ExactRow<'_>>::uninit(); 128];
    let mut lexical = [MaybeUninit::<LexicalRow<'_>>::uninit(); 128];
    let mut atoms = [MaybeUninit::<Atom<'_>>::uninit(); 128];
    let mut types = [MaybeUninit::<TypeNode>::uninit(); 128];
    let prepared = build(
        &fragment,
        IndexBuildScratch {
            projections: &mut projections,
            entities: &mut facts,
            exact_rows: &mut exact,
            lexical_rows: &mut lexical,
            atoms: &mut atoms,
            type_nodes: &mut types,
        },
    )
    .map_err(|error| TestError::Build(error.into()))?;
    let result = action(&prepared);
    let shutdown = publisher.shutdown();
    let cleanup = fixture.remove();
    match (result, shutdown, cleanup) {
        (Ok(value), Ok(()), Ok(())) => Ok(value),
        (Err(error), _, _) => Err(error),
        (Ok(_), Err(error), _) => Err(error.into()),
        (Ok(_), Ok(()), Err(error)) => Err(error.into()),
    }
}

fn authority() -> VectorAuthority {
    VectorAuthority::new(
        IndexSnapshotId::from_canonical_bytes(b"vector-test-snapshot"),
        ModelId::new([7; 16]),
        2,
        Metric::SquaredEuclidean,
    )
}

fn derive<'prepared, 'slots>(
    prepared: &'prepared server_index_build::PreparedIndex<'prepared>,
    segments: &'slots mut [MaybeUninit<ValidatedVectorSegment<'slots>>],
    points: &'slots mut [MaybeUninit<server_index_graph_vector::VectorPoint<'slots>>],
    coordinates: &'slots mut [i16],
) -> Result<&'slots [ValidatedVectorSegment<'slots>], VectorProjectionError> {
    derive_at(prepared, PartitionId::new(3), segments, points, coordinates)
}

fn derive_at<'prepared, 'slots>(
    prepared: &'prepared server_index_build::PreparedIndex<'prepared>,
    base: PartitionId,
    segments: &'slots mut [MaybeUninit<ValidatedVectorSegment<'slots>>],
    points: &'slots mut [MaybeUninit<server_index_graph_vector::VectorPoint<'slots>>],
    coordinates: &'slots mut [i16],
) -> Result<&'slots [ValidatedVectorSegment<'slots>], VectorProjectionError> {
    build_vector_projection(
        prepared,
        authority(),
        base,
        &TestEmbedder,
        VectorProjectionScratch {
            segments,
            points,
            coordinates: CoordinateLane::new(coordinates),
        },
    )
}

fn entities<const COUNT: usize>() -> [EntityRecord; COUNT] {
    core::array::from_fn(|_| EntityRecord {
        semantic_type: TypeId::new(0),
        name: AtomId::new(0),
        kind: EntityKind::Function,
    })
}

#[test]
fn multi_chunk_family_ascends_partitions() -> Result<(), VectorTestError> {
    let records = entities::<17>();
    with_prepared("vector-partitions", &records, |prepared| {
        let mut segments = [MaybeUninit::<ValidatedVectorSegment<'_>>::uninit(); 2];
        let mut points = [MaybeUninit::uninit(); 17];
        let mut coordinates = [0_i16; 34];
        let projection = derive_at(
            prepared,
            PartitionId::new(40),
            &mut segments,
            &mut points,
            &mut coordinates,
        )?;
        assert_eq!(projection.len(), 2);
        for (segment, (expected, expected_points)) in projection
            .iter()
            .zip([(PartitionId::new(40), 16_u8), (PartitionId::new(41), 1)])
        {
            assert_eq!(segment.partition, expected);
            assert_eq!(segment.point_count, expected_points);
        }
        Ok(())
    })
}

#[test]
fn partition_space_rejects_base_overflow_before_embedding() -> Result<(), VectorTestError> {
    let records = entities::<17>();
    with_prepared("vector-partition-overflow", &records, |prepared| {
        let mut segments = [MaybeUninit::<ValidatedVectorSegment<'_>>::uninit(); 2];
        let mut points = [MaybeUninit::uninit(); 17];
        let mut coordinates = [0_i16; 34];
        let result = derive_at(
            prepared,
            PartitionId::new(u16::MAX),
            &mut segments,
            &mut points,
            &mut coordinates,
        );
        assert_eq!(
            result,
            Err(VectorProjectionError::PartitionSpace {
                base: PartitionId::new(u16::MAX),
                required_partitions: 2
            })
        );
        assert!(coordinates.iter().all(|cell| *cell == 0));
        Ok(())
    })
}

#[test]
fn exact_family_limit_admits_four_chunks() -> Result<(), VectorTestError> {
    let records = entities::<64>();
    with_prepared("vector-family-limit", &records, |prepared| {
        let mut segments =
            [MaybeUninit::<ValidatedVectorSegment<'_>>::uninit(); MAX_VECTOR_SEGMENTS];
        let mut points = [MaybeUninit::uninit(); 64];
        let mut coordinates = [0_i16; 128];
        let projection = derive(prepared, &mut segments, &mut points, &mut coordinates)?;
        assert_eq!(projection.len(), MAX_VECTOR_SEGMENTS);
        for (segment, expected) in projection.iter().zip([3_u16, 4, 5, 6]) {
            assert_eq!(segment.partition, PartitionId::new(expected));
            assert_eq!(segment.point_count, 16_u8);
        }
        Ok(())
    })
}

#[test]
fn zero_dimension_embedder_rejects_without_writing() -> Result<(), VectorTestError> {
    let records = entities::<1>();
    with_prepared("vector-zero-dimension", &records, |prepared| {
        let mut segments = [MaybeUninit::<ValidatedVectorSegment<'_>>::uninit(); 1];
        let mut points = [MaybeUninit::uninit(); 1];
        let mut coordinates = [19_i16; 1];
        let embedder = ZeroEmbedder;
        let authority = VectorAuthority::new(
            IndexSnapshotId::from_canonical_bytes(b"vector-test-snapshot"),
            embedder.model(),
            0,
            embedder.metric(),
        );
        let result = build_vector_projection(
            prepared,
            authority,
            PartitionId::new(0),
            &embedder,
            VectorProjectionScratch {
                segments: &mut segments,
                points: &mut points,
                coordinates: CoordinateLane::new(&mut coordinates),
            },
        );
        assert_eq!(
            result,
            Err(VectorProjectionError::EmbedderDimension {
                maximum: 16,
                observed: 0
            })
        );
        assert!(coordinates.iter().all(|cell| *cell == 19));
        Ok(())
    })
}

#[test]
fn wide_dimension_embedder_rejects_without_writing() -> Result<(), VectorTestError> {
    let records = entities::<1>();
    with_prepared("vector-wide-dimension", &records, |prepared| {
        let mut segments = [MaybeUninit::<ValidatedVectorSegment<'_>>::uninit(); 1];
        let mut points = [MaybeUninit::uninit(); 1];
        let mut coordinates = [23_i16; 17];
        let embedder = WideEmbedder;
        let authority = VectorAuthority::new(
            IndexSnapshotId::from_canonical_bytes(b"vector-test-snapshot"),
            embedder.model(),
            17,
            embedder.metric(),
        );
        let result = build_vector_projection(
            prepared,
            authority,
            PartitionId::new(0),
            &embedder,
            VectorProjectionScratch {
                segments: &mut segments,
                points: &mut points,
                coordinates: CoordinateLane::new(&mut coordinates),
            },
        );
        assert_eq!(
            result,
            Err(VectorProjectionError::EmbedderDimension {
                maximum: 16,
                observed: 17
            })
        );
        assert!(coordinates.iter().all(|cell| *cell == 23));
        Ok(())
    })
}

#[test]
fn partition_limit_rejects_65_entities_before_embedding() -> Result<(), VectorTestError> {
    let entities: [EntityRecord; 65] = core::array::from_fn(|_| EntityRecord {
        semantic_type: TypeId::new(0),
        name: AtomId::new(0),
        kind: EntityKind::Function,
    });
    with_prepared("vector-family-capacity", &entities, |prepared| {
        let mut segments = [MaybeUninit::<ValidatedVectorSegment<'_>>::uninit(); 4];
        let mut points = [MaybeUninit::uninit(); 64];
        let mut coordinates = [0_i16; 128];
        let result = derive(prepared, &mut segments, &mut points, &mut coordinates);
        assert_eq!(
            result,
            Err(VectorProjectionError::PartitionLimit {
                maximum: 4,
                observed: 5,
            })
        );
        assert!(coordinates.iter().all(|cell| *cell == 0));
        Ok(())
    })
}

#[test]
fn identity_is_deterministic_across_independent_scratch() -> Result<(), VectorTestError> {
    let entities = [
        EntityRecord {
            semantic_type: TypeId::new(0),
            name: AtomId::new(0),
            kind: EntityKind::Function,
        },
        EntityRecord {
            semantic_type: TypeId::new(0),
            name: AtomId::new(0),
            kind: EntityKind::Record,
        },
        EntityRecord {
            semantic_type: TypeId::new(0),
            name: AtomId::new(0),
            kind: EntityKind::Constant,
        },
    ];
    with_prepared("vector-identity", &entities, |prepared| {
        let mut first_segments = [MaybeUninit::<ValidatedVectorSegment<'_>>::uninit(); 4];
        let mut first_points = [MaybeUninit::uninit(); 3];
        let mut first_coordinates = [0_i16; 6];
        let first = derive(
            prepared,
            &mut first_segments,
            &mut first_points,
            &mut first_coordinates,
        )?;
        let first_ids: [_; 1] = [first
            .iter()
            .next()
            .map(|segment| segment.id)
            .ok_or(VectorTestError::MissingSegment)?];
        let mut second_segments = [MaybeUninit::<ValidatedVectorSegment<'_>>::uninit(); 4];
        let mut second_points = [MaybeUninit::uninit(); 3];
        let mut second_coordinates = [0_i16; 6];
        let second = derive(
            prepared,
            &mut second_segments,
            &mut second_points,
            &mut second_coordinates,
        )?;
        let second_id = second
            .iter()
            .next()
            .map(|segment| segment.id)
            .ok_or(VectorTestError::MissingSegment)?;
        assert_eq!(first_ids[0], second_id);
        Ok(())
    })
}

#[test]
fn warm_vector_derivation_allocates_nothing() -> Result<(), VectorTestError> {
    let entities = [EntityRecord {
        semantic_type: TypeId::new(0),
        name: AtomId::new(0),
        kind: EntityKind::Function,
    }];
    with_prepared("vector-allocation", &entities, |prepared| {
        let mut warm_segments = [MaybeUninit::<ValidatedVectorSegment<'_>>::uninit(); 4];
        let mut warm_points = [MaybeUninit::uninit(); 1];
        let mut warm_coordinates = [0_i16; 2];
        let _warm = derive(
            prepared,
            &mut warm_segments,
            &mut warm_points,
            &mut warm_coordinates,
        )?;
        let mut succeeded = false;
        let measurement = measure(|| {
            let mut segments = [MaybeUninit::<ValidatedVectorSegment<'_>>::uninit(); 4];
            let mut points = [MaybeUninit::uninit(); 1];
            let mut coordinates = [0_i16; 2];
            succeeded = derive(prepared, &mut segments, &mut points, &mut coordinates).is_ok();
        });
        assert_eq!(measurement, AllocationInfo::default());
        assert!(succeeded);
        Ok(())
    })
}

#[test]
fn empty_fragment_is_honestly_empty_and_preserves_lanes() -> Result<(), VectorTestError> {
    with_prepared("vector-empty", &[], |prepared| {
        let mut segments = [MaybeUninit::<ValidatedVectorSegment<'_>>::uninit(); 4];
        let mut points = [MaybeUninit::uninit(); 1];
        let mut coordinates = [17_i16; 2];
        let projection = derive(prepared, &mut segments, &mut points, &mut coordinates)?;
        assert!(projection.is_empty());
        assert!(coordinates.iter().all(|cell| *cell == 17));
        Ok(())
    })
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
    assert_eq!(projection.len(), 1);
    let segment = projection
        .iter()
        .next()
        .ok_or(VectorTestError::MissingSegment)?;
    assert_eq!(segment.authority, authority);
    assert_eq!(segment.partition, PartitionId::new(0));
    assert_eq!(segment.point_count, 1);
    assert_eq!(segment.id, segment.descriptor().id);
    let point = segment
        .facts
        .iter()
        .next()
        .ok_or(VectorTestError::MissingPoint)?;
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
