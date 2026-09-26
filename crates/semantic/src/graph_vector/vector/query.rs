//! Exact local vector query over sealed segments.

use super::{
    MAX_PARTITIONS, MAX_VECTOR_DIMENSION, Metric, MissingPartitions, PartitionId,
    ValidatedVectorSegment, VectorAuthority, VectorHit, VectorQueryError, VectorQueryTerminal,
};

impl VectorQueryTerminal {
    /// Returns the complete vector authority.
    #[must_use]
    pub const fn authority(self) -> VectorAuthority {
        match self {
            Self::Complete { authority } | Self::Partial { authority, .. } => authority,
        }
    }

    /// Returns true when at least one selected partition was unavailable.
    #[must_use]
    pub const fn is_partial(self) -> bool {
        matches!(self, Self::Partial { .. })
    }

    /// Borrows exact missing coordinates in selection order.
    #[must_use]
    pub fn missing(&self) -> &[PartitionId] {
        match self {
            Self::Complete { .. } => &[],
            Self::Partial { missing, .. } => missing,
        }
    }
}

/// Bounded exact-vector result and its typed terminal.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct VectorQueryOutcome {
    /// Number of initialized top-k slots.
    pub written: usize,
    /// Complete/partial typed terminal.
    pub terminal: VectorQueryTerminal,
}

/// Executes the scalar local oracle over authority-sealed borrowed vector segments.
///
/// The query never accepts an unproved point row: every point is reached only through a
/// [`ValidatedVectorSegment`], whose private owner pins the authority and partition used for the
/// returned hit. Segment authority is preflighted before the caller's output is cleared.
#[allow(
    clippy::result_large_err,
    reason = "query rejection preserves complete authority evidence"
)]
pub fn exact_vector_query(
    authority: VectorAuthority,
    selected: &[PartitionId],
    segments: &[ValidatedVectorSegment<'_>],
    query: &[i16],
    requested_limit: usize,
    output: &mut [Option<VectorHit>],
) -> Result<VectorQueryOutcome, VectorQueryError> {
    let dimension = usize::from(authority.dimension);
    if dimension == 0 || dimension > MAX_VECTOR_DIMENSION {
        return Err(VectorQueryError::AuthorityDimension {
            maximum: MAX_VECTOR_DIMENSION,
            observed: dimension,
        });
    }
    if query.len() != dimension {
        return Err(VectorQueryError::QueryDimension {
            expected: dimension,
            observed: query.len(),
        });
    }
    if requested_limit > output.len() {
        return Err(VectorQueryError::InsufficientOutput {
            required: requested_limit,
            available: output.len(),
        });
    }
    validate_vector_segments(authority, selected, segments)?;
    for slot in output.iter_mut().take(requested_limit) {
        *slot = None;
    }
    let mut written = 0;
    for segment in segments {
        if selected.contains(&segment.partition) {
            for fact in segment.facts.iter().copied() {
                let candidate = VectorHit {
                    authority,
                    partition: segment.partition,
                    entity: fact.entity,
                    score: score(authority.metric, query, fact.coordinates),
                };
                insert_top_k(output, requested_limit, &mut written, candidate);
            }
        }
    }
    Ok(VectorQueryOutcome {
        written,
        terminal: vector_terminal(authority, selected, segments),
    })
}

#[allow(
    clippy::result_large_err,
    reason = "segment validation rejection preserves complete authority evidence"
)]
fn validate_vector_segments(
    authority: VectorAuthority,
    selected: &[PartitionId],
    segments: &[ValidatedVectorSegment<'_>],
) -> Result<(), VectorQueryError> {
    if selected.len() > MAX_PARTITIONS {
        return Err(VectorQueryError::TooManySelected {
            maximum: MAX_PARTITIONS,
            observed: selected.len(),
        });
    }
    if segments.len() > MAX_PARTITIONS {
        return Err(VectorQueryError::TooManySegments {
            maximum: MAX_PARTITIONS,
            observed: segments.len(),
        });
    }
    for (segment_index, segment) in segments.iter().enumerate() {
        if segment.authority != authority {
            return Err(VectorQueryError::WrongSegmentAuthority {
                segment_index,
                expected: authority,
                observed: segment.authority,
            });
        }
    }
    reject_duplicate_partitions(selected, segments)?;
    Ok(())
}

#[allow(
    clippy::result_large_err,
    reason = "duplicate rejection uses the same exact query error taxonomy"
)]
fn reject_duplicate_partitions(
    selected: &[PartitionId],
    segments: &[ValidatedVectorSegment<'_>],
) -> Result<(), VectorQueryError> {
    for index in 0..selected.len() {
        for first_index in 0..index {
            if selected[first_index] == selected[index] {
                return Err(VectorQueryError::DuplicateSelected {
                    first_index,
                    index,
                    partition: selected[index],
                });
            }
        }
    }
    for index in 0..segments.len() {
        for first_index in 0..index {
            if segments[first_index].partition == segments[index].partition {
                return Err(VectorQueryError::DuplicateSegment {
                    first_index,
                    index,
                    partition: segments[index].partition,
                });
            }
        }
    }
    Ok(())
}

fn score(metric: Metric, query: &[i16], coordinates: &[i16]) -> i64 {
    query
        .iter()
        .zip(coordinates)
        .map(|(query_coordinate, fact_coordinate)| match metric {
            Metric::SquaredEuclidean => {
                let difference = i64::from(*query_coordinate) - i64::from(*fact_coordinate);
                difference * difference
            }
            Metric::NegativeDotProduct => {
                -(i64::from(*query_coordinate) * i64::from(*fact_coordinate))
            }
        })
        .sum()
}

fn insert_top_k(
    output: &mut [Option<VectorHit>],
    limit: usize,
    written: &mut usize,
    candidate: VectorHit,
) {
    if limit == 0 {
        return;
    }
    let occupied = (*written).min(limit);
    let mut insertion = occupied;
    for (index, current) in output.iter().copied().take(occupied).enumerate() {
        if current.is_some_and(|current| vector_hit_precedes(candidate, current)) {
            insertion = index;
            break;
        }
    }
    if insertion == limit {
        return;
    }
    let new_written = (occupied + 1).min(limit);
    for index in (insertion + 1..new_written).rev() {
        output[index] = output[index - 1];
    }
    output[insertion] = Some(candidate);
    *written = new_written;
}

fn vector_hit_precedes(left: VectorHit, right: VectorHit) -> bool {
    (left.score, left.entity.raw, left.partition.raw)
        < (right.score, right.entity.raw, right.partition.raw)
}

fn vector_terminal(
    authority: VectorAuthority,
    selected: &[PartitionId],
    segments: &[ValidatedVectorSegment<'_>],
) -> VectorQueryTerminal {
    let mut missing = [PartitionId::new(0); MAX_PARTITIONS];
    let mut missing_len = 0;
    for partition in selected.iter().copied() {
        if !segments
            .iter()
            .any(|segment| segment.partition == partition)
        {
            missing[missing_len] = partition;
            missing_len += 1;
        }
    }
    match MissingPartitions::from_prefix(missing, missing_len) {
        Some(missing) => VectorQueryTerminal::Partial { authority, missing },
        None => VectorQueryTerminal::Complete { authority },
    }
}
