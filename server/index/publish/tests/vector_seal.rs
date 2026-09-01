//! Proves the vector family journey from durable compiler publication to a sealed snapshot.
//! It covers durable reopen, sealed authority pinning, and coordinate mutation identity checks.
//! The fixture keeps model provenance typed and caller-owned.
#[path = "../../build/tests/support/mod.rs"]
mod support;

use core::mem::MaybeUninit;

use compiler_ir::{Atom, AtomInput, EntityKind, EntityRecord, PrimitiveType, TypeNode};
use compiler_ir_vocabulary::{AtomId, TypeId};
use server_index_build::{
    CoordinateLane, EntityEmbedder, EntityProjection, IndexBuildScratch,
    MAX_VECTOR_POINTS_PER_SEGMENT, VectorProjectionScratch, build, build_vector_projection,
};
use server_index_core::{ExactRow, LexicalRow};
use server_index_graph_vector::{Metric, ModelId, PartitionId, ValidatedVectorSegment};
use server_index_publish::seal_compilation_index;
use server_index_vocabulary::VectorSegmentId;
use server_journal::{DurablePublisher, PublicationLimits, PublicationOpenError, PublicationPaths};
use support::{OpenBuffers, compiled, next_fragment, publish, write_fragment, written};
use thiserror::Error;

const ENTITY_COUNT: usize = 17;
const SEGMENT_SLOTS: usize = 4;
const COORDINATE_CELLS: usize = ENTITY_COUNT * TestEmbedder::DIMENSION;
const SECOND_PARTITION_COORDINATE: usize = MAX_VECTOR_POINTS_PER_SEGMENT * TestEmbedder::DIMENSION;

#[derive(Debug, Error)]
enum SealVectorError {
    #[error(transparent)]
    Fixture(#[from] support::TestError),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Projection(#[from] server_index_build::VectorProjectionError),
    #[error(transparent)]
    Seal(#[from] server_index_publish::CompilationIndexError),
    #[error(transparent)]
    Shutdown(#[from] server_journal::ShutdownError),
    #[error(transparent)]
    Limits(#[from] server_journal::PublicationLimitError),
    #[error(transparent)]
    Publisher(#[from] PublicationOpenError),
    #[error("expected two vector segments")]
    SegmentCount,
    #[error("durably published compilation was not selected")]
    MissingPublication,
    #[error("vector authority was not pinned to the sealed snapshot")]
    AuthorityPin,
    #[error("embedder dimension conversion failed")]
    AuthorityDimension(#[source] core::num::TryFromIntError),
    #[error("mutated projection unexpectedly retained the recorded segment identity")]
    MutationNotObserved,
}

struct Fixture {
    directory: std::path::PathBuf,
}

struct LargeBuildBuffers<'bytes> {
    projections: [MaybeUninit<EntityProjection<'bytes>>; ENTITY_COUNT],
    entities: [MaybeUninit<server_index_build::EntityFact<'bytes>>; ENTITY_COUNT],
    exact: [MaybeUninit<ExactRow<'bytes>>; ENTITY_COUNT],
    lexical: [MaybeUninit<LexicalRow<'bytes>>; ENTITY_COUNT],
    atoms: [MaybeUninit<Atom<'bytes>>; ENTITY_COUNT],
    types: [MaybeUninit<TypeNode>; ENTITY_COUNT],
}

impl<'bytes> LargeBuildBuffers<'bytes> {
    const fn new() -> Self {
        Self {
            projections: [MaybeUninit::uninit(); ENTITY_COUNT],
            entities: [MaybeUninit::uninit(); ENTITY_COUNT],
            exact: [MaybeUninit::uninit(); ENTITY_COUNT],
            lexical: [MaybeUninit::uninit(); ENTITY_COUNT],
            atoms: [MaybeUninit::uninit(); ENTITY_COUNT],
            types: [MaybeUninit::uninit(); ENTITY_COUNT],
        }
    }

    fn build(
        &'bytes mut self,
        fragment: &'bytes compiler_publication::OpenedFragment<'bytes>,
    ) -> Result<server_index_build::PreparedIndex<'bytes>, support::TestError> {
        build(
            fragment,
            IndexBuildScratch {
                projections: &mut self.projections,
                entities: &mut self.entities,
                exact_rows: &mut self.exact,
                lexical_rows: &mut self.lexical,
                atoms: &mut self.atoms,
                type_nodes: &mut self.types,
            },
        )
        .map_err(|error| support::TestError::Build(error.into()))
    }
}

impl Fixture {
    fn new(label: &str) -> Result<Self, std::io::Error> {
        let directory = std::env::temp_dir().join(format!(
            "server-index-publish-{label}-{}",
            std::process::id()
        ));
        std::fs::create_dir(&directory)?;
        Ok(Self { directory })
    }

    fn publisher(&self) -> Result<DurablePublisher, SealVectorError> {
        let limits =
            PublicationLimits::new(core::num::NonZeroUsize::MIN, core::num::NonZeroUsize::MIN)?;
        Ok(DurablePublisher::create(
            &PublicationPaths::in_directory(&self.directory.join("journal")),
            limits,
        )?)
    }

    fn reopen(&self) -> Result<DurablePublisher, SealVectorError> {
        let limits =
            PublicationLimits::new(core::num::NonZeroUsize::MIN, core::num::NonZeroUsize::MIN)?;
        Ok(DurablePublisher::reopen(
            &PublicationPaths::in_directory(&self.directory.join("journal")),
            limits,
        )?)
    }

    fn artifacts(&self) -> std::path::PathBuf {
        self.directory.join("artifacts")
    }

    fn remove(self) -> Result<(), std::io::Error> {
        std::fs::remove_dir_all(self.directory)
    }
}

#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
#[error("vector segment identity differed from its recorded family identity")]
struct RecordedIdentityMismatch {
    expected: VectorSegmentId,
    observed: VectorSegmentId,
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
        if coordinates.iter().any(|cell| *cell != 0) {
            return;
        }
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

#[test]
fn sealed_compilation_snapshots_pin_vector_family_across_reopen_and_mutation()
-> Result<(), SealVectorError> {
    let fixture = Fixture::new("vector-seal")?;
    let publisher = fixture.publisher()?;
    let mut bytes = [0_u8; 16_384];
    let entities: [EntityRecord; ENTITY_COUNT] = core::array::from_fn(|_| EntityRecord {
        semantic_type: TypeId::new(0),
        name: AtomId::new(0),
        kind: EntityKind::Function,
    });
    let length = write_fragment(
        &mut bytes,
        b"vector-seal-fixture",
        &entities,
        &[TypeNode::Primitive(PrimitiveType::I32)],
        &[AtomInput { bytes: b"entity" }],
    )?;
    let selected = publish(
        &publisher,
        &fixture.artifacts(),
        &[compiled(written(&bytes, length)?)?],
    )?;
    publisher.shutdown()?;

    let reopened = fixture.reopen()?;
    let mut opened_buffers = OpenBuffers::new();
    let opened = opened_buffers.open(&reopened, &fixture.artifacts())?;
    if opened.publication != selected.publication {
        return Err(SealVectorError::MissingPublication);
    }
    let mut fragments = opened.fragments();
    let fragment = next_fragment(&mut fragments, 0)?;
    let mut build_buffers = LargeBuildBuffers::new();
    let prepared = build_buffers.build(&fragment)?;
    let mut exact_ids = [prepared.exact.id];
    let mut lexical_ids = [prepared.lexical.id];
    let prepared_indexes = [prepared];
    let sealed = seal_compilation_index(
        opened,
        &prepared_indexes,
        server_index_publish::CompilationIndexScratch {
            exact: &mut exact_ids,
            lexical: &mut lexical_ids,
        },
    )
    .map_err(|rejected| rejected.error)?;
    if sealed.snapshot.generation != selected.publication.generation.pinned_root {
        return Err(SealVectorError::AuthorityPin);
    }
    let embedder = TestEmbedder;
    let authority = server_index_graph_vector::VectorAuthority::new(
        sealed.snapshot.id,
        embedder.model(),
        u8::try_from(TestEmbedder::DIMENSION)
            .map_err(SealVectorError::AuthorityDimension)?
            .into(),
        embedder.metric(),
    );
    let recorded = derive_ids(&prepared, authority)?;
    let reopened_ids = derive_ids(&prepared, authority)?;
    if recorded != reopened_ids {
        return Err(SealVectorError::MutationNotObserved);
    }

    let mut mutated_coordinates = [0_i16; COORDINATE_CELLS];
    derive_with_coordinates(&prepared, authority, &mut mutated_coordinates)?;
    let cell = mutated_coordinates
        .get(SECOND_PARTITION_COORDINATE)
        .copied()
        .ok_or(SealVectorError::MutationNotObserved)?;
    let target = mutated_coordinates
        .get_mut(SECOND_PARTITION_COORDINATE)
        .ok_or(SealVectorError::MutationNotObserved)?;
    *target = cell ^ 1;
    let mutated = derive_with_coordinates(&prepared, authority, &mut mutated_coordinates)?;
    if mutated[0] != recorded[0] {
        return Err(SealVectorError::MutationNotObserved);
    }
    let rejection = compare_recorded(recorded[1], mutated[1]);
    assert_eq!(
        rejection,
        Err(RecordedIdentityMismatch {
            expected: recorded[1],
            observed: mutated[1],
        })
    );
    if mutated[1] == recorded[1] {
        return Err(SealVectorError::MutationNotObserved);
    }

    let shutdown = reopened.shutdown();
    let cleanup = fixture.remove();
    match (shutdown, cleanup) {
        (Ok(()), Ok(())) => Ok(()),
        (Err(error), _) => Err(error.into()),
        (Ok(()), Err(error)) => Err(error.into()),
    }
}

fn derive_ids(
    prepared: &server_index_build::PreparedIndex<'_>,
    authority: server_index_graph_vector::VectorAuthority,
) -> Result<[VectorSegmentId; 2], SealVectorError> {
    let mut coordinates = [0_i16; COORDINATE_CELLS];
    derive_with_coordinates(prepared, authority, &mut coordinates)
}

fn derive_with_coordinates(
    prepared: &server_index_build::PreparedIndex<'_>,
    authority: server_index_graph_vector::VectorAuthority,
    coordinates: &mut [i16; COORDINATE_CELLS],
) -> Result<[VectorSegmentId; 2], SealVectorError> {
    let mut segments = [MaybeUninit::<ValidatedVectorSegment<'_>>::uninit(); SEGMENT_SLOTS];
    let mut points = [MaybeUninit::uninit(); ENTITY_COUNT];
    let projection =
        derive_into_with_scratch(prepared, authority, coordinates, &mut segments, &mut points)?;
    let mut ids = projection.iter().map(|segment| segment.id);
    let first = ids.next().ok_or(SealVectorError::SegmentCount)?;
    let second = ids.next().ok_or(SealVectorError::SegmentCount)?;
    if ids.next().is_some() {
        return Err(SealVectorError::SegmentCount);
    }
    Ok([first, second])
}

fn derive_into_with_scratch<'prepared, 'slots>(
    prepared: &'prepared server_index_build::PreparedIndex<'prepared>,
    authority: server_index_graph_vector::VectorAuthority,
    coordinates: &'slots mut [i16; COORDINATE_CELLS],
    segments: &'slots mut [MaybeUninit<ValidatedVectorSegment<'slots>>; SEGMENT_SLOTS],
    points: &'slots mut [MaybeUninit<server_index_graph_vector::VectorPoint<'slots>>; ENTITY_COUNT],
) -> Result<&'slots [ValidatedVectorSegment<'slots>], SealVectorError> {
    Ok(build_vector_projection(
        prepared,
        authority,
        PartitionId::new(0),
        &TestEmbedder,
        VectorProjectionScratch {
            segments,
            points,
            coordinates: CoordinateLane::new(coordinates),
        },
    )?)
}

fn compare_recorded(
    expected: VectorSegmentId,
    observed: VectorSegmentId,
) -> Result<(), RecordedIdentityMismatch> {
    if expected == observed {
        Ok(())
    } else {
        Err(RecordedIdentityMismatch { expected, observed })
    }
}
