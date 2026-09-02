//! Defines admission behavior for `server-index-qdrant`, whose purpose is to adapt typed vector authorities and queries to the Qdrant service.
//! This module owns the admission invariants and typed state transitions.
//! Its narrow surface prevents representation and policy details from leaking outward.
//! Bounded point/query admission and prepared request identities.

use arrayvec::ArrayVec;
use server_index_graph_vector::{ValidatedVectorSegment, VectorAuthority, VectorSegmentDescriptor};

use super::{
    contract::{PhysicalPointId, QdrantAdmissionError, QdrantDataKey, QdrantError},
    limits::{MAX_BATCH_POINTS, MAX_QUERY_SEGMENTS},
};

#[derive(Clone, Copy, Debug)]
pub(super) struct PreparedPoint<'coordinates> {
    pub(super) index: usize,
    pub(super) key: QdrantDataKey,
    pub(super) coordinates: &'coordinates [i16],
    pub(super) physical_id: PhysicalPointId,
}

#[derive(Clone, Copy, Debug)]
pub(super) struct PreparedKey {
    pub(super) index: usize,
    pub(super) key: QdrantDataKey,
    pub(super) physical_id: PhysicalPointId,
}

/// Shared prepared identity capability used by retrieval, delete, and upsert preflight.
pub(super) trait PreparedIdentity {
    fn key(&self) -> QdrantDataKey;
    fn physical_id(&self) -> PhysicalPointId;
    fn coordinates(&self) -> Option<&[i16]>;
}

impl PreparedIdentity for PreparedKey {
    fn key(&self) -> QdrantDataKey {
        self.key
    }

    fn physical_id(&self) -> PhysicalPointId {
        self.physical_id
    }

    fn coordinates(&self) -> Option<&[i16]> {
        None
    }
}

impl<'coordinates> PreparedIdentity for PreparedPoint<'coordinates> {
    fn key(&self) -> QdrantDataKey {
        self.key
    }

    fn physical_id(&self) -> PhysicalPointId {
        self.physical_id
    }

    fn coordinates(&self) -> Option<&[i16]> {
        Some(self.coordinates)
    }
}

pub(super) fn validate_query(
    authority: VectorAuthority,
    selected: &[VectorSegmentDescriptor],
    query_coordinates: &[i16],
    requested_limit: usize,
    available_output: usize,
) -> Result<(), QdrantError> {
    if selected.len() > MAX_QUERY_SEGMENTS {
        return Err(QdrantAdmissionError::TooManySegments {
            maximum: MAX_QUERY_SEGMENTS,
            observed: selected.len(),
        }
        .into());
    }
    if requested_limit > available_output {
        return Err(QdrantAdmissionError::InsufficientOutput {
            required: requested_limit,
            available: available_output,
        }
        .into());
    }
    if requested_limit > MAX_BATCH_POINTS {
        return Err(QdrantAdmissionError::BatchTooLarge {
            maximum: MAX_BATCH_POINTS,
            observed: requested_limit,
        }
        .into());
    }
    let expected = usize::from(authority.dimension);
    if query_coordinates.len() != expected {
        return Err(QdrantAdmissionError::WrongQueryDimension {
            expected,
            observed: query_coordinates.len(),
        }
        .into());
    }
    for (index, descriptor) in selected.iter().copied().enumerate() {
        reject_wrong_authority(index, authority, descriptor.authority)?;
        for (first_index, first) in selected[..index].iter().copied().enumerate() {
            if first.id == descriptor.id {
                return Err(QdrantAdmissionError::DuplicateSegment {
                    first_index,
                    index,
                    segment: descriptor.id,
                }
                .into());
            }
        }
    }
    Ok(())
}

pub(super) fn prepare_segments<'coordinates>(
    authority: VectorAuthority,
    segments: &[ValidatedVectorSegment<'coordinates>],
) -> Result<ArrayVec<PreparedPoint<'coordinates>, MAX_BATCH_POINTS>, QdrantError> {
    let mut prepared: ArrayVec<PreparedPoint<'coordinates>, MAX_BATCH_POINTS> = ArrayVec::new();
    for (segment_index, segment) in segments.iter().enumerate() {
        reject_wrong_authority(segment_index, authority, segment.authority)?;
        for point in segment.facts.iter().copied() {
            let index = prepared.len();
            let key = QdrantDataKey::new(
                segment.authority,
                segment.id,
                segment.partition,
                point.entity,
            );
            let physical_id = PhysicalPointId::for_key(key);
            for first in prepared.iter().copied() {
                reject_identity_pair(
                    first.index,
                    first.key,
                    first.physical_id,
                    index,
                    key,
                    physical_id,
                )?;
            }
            if let Err(_rejected) = prepared.try_push(PreparedPoint {
                index,
                key,
                coordinates: point.coordinates,
                physical_id,
            }) {
                return Err(QdrantAdmissionError::BatchTooLarge {
                    maximum: MAX_BATCH_POINTS,
                    observed: index.saturating_add(1),
                }
                .into());
            }
        }
    }
    Ok(prepared)
}

pub(super) fn prepare_keys(
    authority: VectorAuthority,
    keys: &[QdrantDataKey],
) -> Result<ArrayVec<PreparedKey, MAX_BATCH_POINTS>, QdrantError> {
    if keys.len() > MAX_BATCH_POINTS {
        return Err(QdrantAdmissionError::BatchTooLarge {
            maximum: MAX_BATCH_POINTS,
            observed: keys.len(),
        }
        .into());
    }
    let mut prepared: ArrayVec<PreparedKey, MAX_BATCH_POINTS> = ArrayVec::new();
    for (index, key) in keys.iter().copied().enumerate() {
        reject_wrong_authority(index, authority, key.authority)?;
        let physical_id = PhysicalPointId::for_key(key);
        for first in prepared.iter().copied() {
            reject_identity_pair(
                first.index,
                first.key,
                first.physical_id,
                index,
                key,
                physical_id,
            )?;
        }
        if let Err(_rejected) = prepared.try_push(PreparedKey {
            index,
            key,
            physical_id,
        }) {
            return Err(QdrantAdmissionError::BatchTooLarge {
                maximum: MAX_BATCH_POINTS,
                observed: keys.len(),
            }
            .into());
        }
    }
    Ok(prepared)
}

pub(super) fn reject_wrong_authority(
    index: usize,
    expected: VectorAuthority,
    observed: VectorAuthority,
) -> Result<(), QdrantAdmissionError> {
    if observed.snapshot != expected.snapshot {
        return Err(QdrantAdmissionError::WrongSnapshot {
            index,
            expected: expected.snapshot,
            observed: observed.snapshot,
        });
    }
    if observed.model != expected.model {
        return Err(QdrantAdmissionError::WrongModel {
            index,
            expected: expected.model,
            observed: observed.model,
        });
    }
    if observed.dimension != expected.dimension {
        return Err(QdrantAdmissionError::WrongModelDimension {
            index,
            expected: expected.dimension,
            observed: observed.dimension,
        });
    }
    if observed.metric != expected.metric {
        return Err(QdrantAdmissionError::WrongMetric {
            index,
            expected: expected.metric,
            observed: observed.metric,
        });
    }
    Ok(())
}

pub(super) fn reject_identity_pair(
    first_index: usize,
    first: QdrantDataKey,
    first_physical_id: PhysicalPointId,
    index: usize,
    second: QdrantDataKey,
    second_physical_id: PhysicalPointId,
) -> Result<(), QdrantAdmissionError> {
    if first == second {
        return Err(QdrantAdmissionError::DuplicateKey {
            first_index,
            index,
            key: second.into(),
        });
    }
    if first_physical_id == second_physical_id {
        return Err(QdrantAdmissionError::PhysicalIdCollision {
            physical_id: second_physical_id,
            first: first.into(),
            second: second.into(),
        });
    }
    Ok(())
}

pub(super) fn vector_matches(expected: &[i16], observed: &[f64]) -> bool {
    expected.len() == observed.len()
        && expected
            .iter()
            .zip(observed)
            .all(|(expected, observed)| f64::from(*expected) == *observed)
}

#[cfg(test)]
mod tests;
