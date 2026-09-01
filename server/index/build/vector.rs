//! Defines vector projection behavior for `server-index-build`, whose purpose is to project reopened compiler IR into immutable exact and lexical segments.
//! This module owns the vector-projection invariants and typed state transitions.
//! Its narrow surface prevents representation and policy details from leaking outward.
//! Caller-embedded vectors retain caller coordinates and immutable ingest authority.
use core::mem::MaybeUninit;

use server_index_graph_vector::{
    MAX_PARTITIONS, MAX_VECTOR_DIMENSION, Metric, ModelId, PartitionId, ValidatedVectorSegment,
    VectorAuthority, VectorPoint, VectorSegmentError,
};
use thiserror::Error;

use crate::{
    EntityFactView, PreparedIndex,
    initialized::{InitializationError, try_initialize},
    partition::{partition_at, partition_range_fits},
};

/// Maximum vector points in one immutable segment.
pub const MAX_VECTOR_POINTS_PER_SEGMENT: usize = 16;
/// Maximum vector segments in one fragment family.
pub const MAX_VECTOR_SEGMENTS: usize = MAX_PARTITIONS;
const _: () = assert!(MAX_VECTOR_SEGMENTS * MAX_VECTOR_POINTS_PER_SEGMENT == 64);

/// Caller-owned output regions for one allocation-free vector projection.
pub struct VectorProjectionScratch<'slots> {
    /// Sealed segment output slots.
    pub segments: &'slots mut [MaybeUninit<ValidatedVectorSegment<'slots>>],
    /// Point output slots borrowed by sealed segments.
    pub points: &'slots mut [MaybeUninit<VectorPoint<'slots>>],
    /// Flat coordinate cells, with one contiguous dimension-sized run per entity.
    pub coordinates: CoordinateLane<'slots>,
}

/// Borrowed flat coordinate lane supplied by the ingest caller.
pub struct CoordinateLane<'slots> {
    cells: &'slots mut [i16],
}

impl<'slots> CoordinateLane<'slots> {
    /// Wraps caller-owned coordinate cells without allocating or copying.
    #[must_use]
    pub const fn new(cells: &'slots mut [i16]) -> Self {
        Self { cells }
    }

    const fn len(&self) -> usize {
        self.cells.len()
    }
}

/// Caller-supplied embedding seam. The build crate knows only identity and metric.
pub trait EntityEmbedder {
    /// Declared coordinate count of this embedder.
    const DIMENSION: usize;

    /// Stable model registry identity, never model bytes.
    fn model(&self) -> ModelId;

    /// Metric that gives returned coordinates their meaning.
    fn metric(&self) -> Metric;

    /// Writes exactly `coordinates.len()` coordinate cells. Every cell is committed into the
    /// immutable segment identity; stale lane contents therefore become identity bytes, so
    /// embedders must fill the whole slice.
    fn embed(&self, fact: EntityFactView<'_>, coordinates: &mut [i16]);
}

/// Exact vector projection rejection retaining checked operands.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum VectorProjectionError {
    /// The authority dimension disagreed with the embedder shape.
    #[error("vector authority dimension {observed} disagrees with embedder dimension {expected}")]
    DimensionAuthority {
        /// Embedder-declared dimension.
        expected: usize,
        /// Authority dimension.
        observed: usize,
    },
    /// The embedder declared an unsupported shape.
    #[error("embedder dimension {observed} is outside maximum {maximum}")]
    EmbedderDimension {
        /// Maximum supported dimension.
        maximum: usize,
        /// Embedder-declared dimension.
        observed: usize,
    },
    /// The derived partition count exceeded vector admission.
    #[error("vector projection requires {observed} partitions, limit is {maximum}")]
    PartitionLimit {
        /// Maximum admitted partitions.
        maximum: usize,
        /// Complete derived partition count.
        observed: usize,
    },
    /// Caller segment output was too short.
    #[error("vector segment output needs {required} slots but has {available}")]
    SegmentCapacity {
        /// Required segment slots.
        required: usize,
        /// Supplied segment slots.
        available: usize,
    },
    /// Caller point output was too short.
    #[error("vector point output needs {required} slots but has {available}")]
    PointCapacity {
        /// Required point slots.
        required: usize,
        /// Supplied point slots.
        available: usize,
    },
    /// Caller coordinate output was too short.
    #[error("vector coordinate output needs {required} cells but has {available}")]
    CoordinateCapacity {
        /// Required coordinate cells.
        required: usize,
        /// Supplied coordinate cells.
        available: usize,
    },
    /// Coordinate requirement overflowed host address space.
    #[error(
        "vector coordinate requirement overflowed for {entities} entities and dimension {dimension}"
    )]
    CoordinateAddressSpace {
        /// Entity count in the fragment.
        entities: usize,
        /// Embedder dimension.
        dimension: usize,
    },
    /// The family partition range exceeded `u16` coordinates.
    #[error(
        "vector projection partitions from {base:?} require {required_partitions} partitions and overflow the partition space"
    )]
    PartitionSpace {
        /// First partition coordinate.
        base: PartitionId,
        /// Complete required partition count.
        required_partitions: usize,
    },
    /// Sealing one derived chunk rejected its exact source facts.
    #[error("vector segment admission rejected derived chunk: {cause:?}")]
    SegmentAdmission {
        /// Exact source rejection retaining its operands.
        cause: VectorSegmentError,
    },
}

/// Derives one immutable vector family in canonical entity order.
///
/// The analytic work is O(N×DIM): one caller embed and one fixed-width copy per entity,
/// followed by bounded segment admission. Every capacity check precedes the first lane write.
///
/// # Errors
/// Returns the exact shape, family-capacity, partition-space, caller-output, or admission rejection.
pub fn build_vector_projection<'facts, 'slots, Embedder: EntityEmbedder>(
    prepared: &'facts PreparedIndex<'facts>,
    authority: VectorAuthority,
    base_partition: PartitionId,
    embedder: &Embedder,
    scratch: VectorProjectionScratch<'slots>,
) -> Result<&'slots [ValidatedVectorSegment<'slots>], VectorProjectionError> {
    let dimension = Embedder::DIMENSION;
    if dimension == 0 || dimension > MAX_VECTOR_DIMENSION {
        return Err(VectorProjectionError::EmbedderDimension {
            maximum: MAX_VECTOR_DIMENSION,
            observed: dimension,
        });
    }
    let observed = usize::from(authority.dimension);
    if observed != dimension {
        return Err(VectorProjectionError::DimensionAuthority {
            expected: dimension,
            observed,
        });
    }
    let entity_count = prepared.entities.len();
    let required_chunks = entity_count.div_ceil(MAX_VECTOR_POINTS_PER_SEGMENT);
    if required_chunks > MAX_VECTOR_SEGMENTS {
        return Err(VectorProjectionError::PartitionLimit {
            maximum: MAX_VECTOR_SEGMENTS,
            observed: required_chunks,
        });
    }
    if !partition_range_fits(base_partition, required_chunks) {
        return Err(VectorProjectionError::PartitionSpace {
            base: base_partition,
            required_partitions: required_chunks,
        });
    }
    let Some(required_coordinates) = entity_count.checked_mul(dimension) else {
        return Err(VectorProjectionError::CoordinateAddressSpace {
            entities: entity_count,
            dimension,
        });
    };
    let VectorProjectionScratch {
        segments,
        points,
        coordinates,
    } = scratch;
    let segment_capacity = segments.len();
    if segment_capacity < required_chunks {
        return Err(VectorProjectionError::SegmentCapacity {
            required: required_chunks,
            available: segment_capacity,
        });
    }
    let point_capacity = points.len();
    if point_capacity < entity_count {
        return Err(VectorProjectionError::PointCapacity {
            required: entity_count,
            available: point_capacity,
        });
    }
    let coordinate_capacity = coordinates.len();
    if coordinate_capacity < required_coordinates {
        return Err(VectorProjectionError::CoordinateCapacity {
            required: required_coordinates,
            available: coordinate_capacity,
        });
    }

    let CoordinateLane { cells: coordinates } = coordinates;
    for (index, fact) in prepared.entities.iter().enumerate() {
        let start = index * dimension;
        let end = start + dimension;
        let Some(cells) = coordinates.get_mut(start..end) else {
            return Err(VectorProjectionError::CoordinateCapacity {
                required: end,
                available: coordinate_capacity,
            });
        };
        embedder.embed(**fact, cells);
    }
    let initialized_points = try_initialize(
        points
            .get_mut(..entity_count)
            .ok_or(VectorProjectionError::PointCapacity {
                required: entity_count,
                available: point_capacity,
            })?,
        prepared.entities.iter().enumerate(),
        |(index, fact)| {
            let start = index * dimension;
            let Some(cells) = coordinates.get(start..start + dimension) else {
                return Err(VectorProjectionError::CoordinateCapacity {
                    required: start + dimension,
                    available: coordinate_capacity,
                });
            };
            Ok(VectorPoint::new(fact.entity, cells))
        },
    )
    .map_err(point_initialization_error)?;
    let point_slice = initialized_points.into_shared();
    let initialized_segments = try_initialize(
        segments
            .get_mut(..required_chunks)
            .ok_or(VectorProjectionError::SegmentCapacity {
                required: required_chunks,
                available: segment_capacity,
            })?,
        point_slice
            .chunks(MAX_VECTOR_POINTS_PER_SEGMENT)
            .zip(0..required_chunks),
        |(chunk, ordinal)| {
            let Some(partition) = partition_at(base_partition, ordinal) else {
                return Err(VectorProjectionError::PartitionSpace {
                    base: base_partition,
                    required_partitions: required_chunks,
                });
            };
            ValidatedVectorSegment::try_new(authority, partition, chunk)
                .map_err(|cause| VectorProjectionError::SegmentAdmission { cause })
        },
    )
    .map_err(segment_initialization_error)?;
    Ok(initialized_segments.into_shared())
}

const fn point_initialization_error(
    error: InitializationError<VectorProjectionError>,
) -> VectorProjectionError {
    match error {
        InitializationError::Value(error) => error,
        InitializationError::Length {
            required,
            available,
        }
        | InitializationError::Exhausted {
            required,
            initialized: available,
        } => VectorProjectionError::PointCapacity {
            required,
            available,
        },
        InitializationError::Surplus { required } => VectorProjectionError::PointCapacity {
            required,
            available: required,
        },
    }
}

const fn segment_initialization_error(
    error: InitializationError<VectorProjectionError>,
) -> VectorProjectionError {
    match error {
        InitializationError::Value(error) => error,
        InitializationError::Length {
            required,
            available,
        }
        | InitializationError::Exhausted {
            required,
            initialized: available,
        } => VectorProjectionError::SegmentCapacity {
            required,
            available,
        },
        InitializationError::Surplus { required } => VectorProjectionError::SegmentCapacity {
            required,
            available: required,
        },
    }
}
