//! Measures `heart-root` benches capacity-planning runner vector work with production data paths.
//! Measurements separate setup from steady-state work and retain resource counters.
//! Results support capacity decisions without changing the measured implementation.
//! Deterministic graph-vector ingress and scalar exact-query projection phases.

use core::mem::{size_of, size_of_val};

use compiler_ir_vocabulary::EntityId;
use server_index_graph_vector::{
    Metric as VectorMetric, ModelId, PartitionId, ValidatedVectorSegment, VectorAuthority,
    VectorFact, VectorPoint, compact_vector_facts, exact_vector_query,
};

use crate::{BenchmarkError, measure::StageWork, runner::support::fixed_prefix};

const VECTOR_DIMENSION: usize = 4;
const MAX_VECTOR_POINTS: usize = 16;
const VECTOR_TOP_K: usize = 4;

fn vector_authority() -> Result<VectorAuthority, BenchmarkError> {
    Ok(VectorAuthority::new(
        server_index_core::IndexSnapshotId::from_canonical_bytes(b"capacity-vector-snapshot"),
        ModelId::new([0x63; 16]),
        u16::try_from(VECTOR_DIMENSION).map_err(BenchmarkError::ByteCount)?,
        VectorMetric::SquaredEuclidean,
    ))
}

fn vector_coordinates() -> Result<[[i16; VECTOR_DIMENSION]; MAX_VECTOR_POINTS], BenchmarkError> {
    let mut coordinates = [[0_i16; VECTOR_DIMENSION]; MAX_VECTOR_POINTS];
    for (index, coordinate) in coordinates.iter_mut().enumerate() {
        let value = i16::try_from(index).map_err(BenchmarkError::ByteCount)?;
        *coordinate = [value, value.saturating_add(1), 2, -2];
    }
    Ok(coordinates)
}

fn vector_facts(
    authority: VectorAuthority,
    coordinates: &[[i16; VECTOR_DIMENSION]; MAX_VECTOR_POINTS],
) -> Result<[VectorFact<'_>; MAX_VECTOR_POINTS], BenchmarkError> {
    let first_coordinates = coordinates
        .first()
        .ok_or(BenchmarkError::FixedSliceLength {
            observed: 1,
            capacity: coordinates.len(),
        })?;
    let first = VectorFact::new(
        authority,
        PartitionId::new(0),
        EntityId::new(0),
        first_coordinates,
    );
    let mut facts = [first; MAX_VECTOR_POINTS];
    for (index, fact) in facts.iter_mut().enumerate() {
        let entity = EntityId::new(u32::try_from(index).map_err(BenchmarkError::ByteCount)?);
        let coordinate = coordinates
            .get(index)
            .ok_or(BenchmarkError::FixedSliceLength {
                observed: index.saturating_add(1),
                capacity: coordinates.len(),
            })?;
        *fact = VectorFact::new(authority, PartitionId::new(0), entity, coordinate);
    }
    Ok(facts)
}

fn vector_points(
    coordinates: &[[i16; VECTOR_DIMENSION]; MAX_VECTOR_POINTS],
) -> Result<[VectorPoint<'_>; MAX_VECTOR_POINTS], BenchmarkError> {
    let first_coordinates = coordinates
        .first()
        .ok_or(BenchmarkError::FixedSliceLength {
            observed: 1,
            capacity: coordinates.len(),
        })?;
    let first = VectorPoint::new(EntityId::new(0), first_coordinates);
    let mut points = [first; MAX_VECTOR_POINTS];
    for (index, point) in points.iter_mut().enumerate() {
        let entity = EntityId::new(u32::try_from(index).map_err(BenchmarkError::ByteCount)?);
        let coordinate = coordinates
            .get(index)
            .ok_or(BenchmarkError::FixedSliceLength {
                observed: index.saturating_add(1),
                capacity: coordinates.len(),
            })?;
        *point = VectorPoint::new(entity, coordinate);
    }
    Ok(points)
}

pub(crate) fn vector_ingress(corpus_size: usize) -> Result<StageWork, BenchmarkError> {
    let count = corpus_size.min(MAX_VECTOR_POINTS);
    let authority = vector_authority()?;
    let coordinates = vector_coordinates()?;
    let facts = vector_facts(authority, &coordinates)?;
    let mut output = vector_points(&coordinates)?;
    let facts = fixed_prefix(&facts, count)?;
    let compacted = compact_vector_facts(authority, PartitionId::new(0), facts, &mut output)
        .map_err(|cause| BenchmarkError::VectorIngress {
            cause: Box::new(cause),
        })?;
    let _segment = std::hint::black_box(
        ValidatedVectorSegment::try_new(
            authority,
            PartitionId::new(0),
            fixed_prefix(&output, compacted)?,
        )
        .map_err(|cause| BenchmarkError::VectorSegment {
            cause: Box::new(cause),
        })?,
    );
    let bytes = u64::try_from(count)
        .map_err(BenchmarkError::ByteCount)?
        .checked_mul(
            u64::try_from(size_of::<[i16; VECTOR_DIMENSION]>())
                .map_err(BenchmarkError::ByteCount)?,
        )
        .ok_or(BenchmarkError::ByteCountOverflow)?;
    Ok(StageWork {
        input_items: count,
        output_items: compacted,
        bytes_read: bytes,
        bytes_written: u64::try_from(size_of_val(fixed_prefix(&output, compacted)?))
            .map_err(BenchmarkError::ByteCount)?,
        durable_bytes: 0,
    })
}

pub(crate) fn vector_query(corpus_size: usize) -> Result<StageWork, BenchmarkError> {
    let count = corpus_size.min(MAX_VECTOR_POINTS);
    let authority = vector_authority()?;
    let coordinates = vector_coordinates()?;
    let facts = vector_facts(authority, &coordinates)?;
    let mut compact = vector_points(&coordinates)?;
    let compacted = compact_vector_facts(
        authority,
        PartitionId::new(0),
        fixed_prefix(&facts, count)?,
        &mut compact,
    )
    .map_err(|cause| BenchmarkError::VectorIngress {
        cause: Box::new(cause),
    })?;
    let segment = ValidatedVectorSegment::try_new(
        authority,
        PartitionId::new(0),
        fixed_prefix(&compact, compacted)?,
    )
    .map_err(|cause| BenchmarkError::VectorSegment {
        cause: Box::new(cause),
    })?;
    let segments = [segment];
    let selected = [PartitionId::new(0)];
    let mut output = [None; VECTOR_TOP_K];
    let result = std::hint::black_box(
        exact_vector_query(
            authority,
            &selected,
            &segments,
            &[0, 0, 2, -2],
            VECTOR_TOP_K,
            &mut output,
        )
        .map_err(|cause| BenchmarkError::VectorQuery {
            cause: Box::new(cause),
        })?,
    );
    Ok(StageWork {
        input_items: count,
        output_items: result.written,
        bytes_read: u64::try_from(size_of::<[i16; VECTOR_DIMENSION]>())
            .map_err(BenchmarkError::ByteCount)?,
        bytes_written: u64::try_from(size_of_val(fixed_prefix(&output, result.written)?))
            .map_err(BenchmarkError::ByteCount)?,
        durable_bytes: 0,
    })
}
