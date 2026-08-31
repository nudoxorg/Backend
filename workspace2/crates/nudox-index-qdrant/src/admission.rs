//! Bounded point/query admission and prepared request identities.

use arrayvec::ArrayVec;
use nudox_index_graph_vector::{PartitionId, VectorAuthority};

use super::{
    contract::{PhysicalPointId, QdrantAdmissionError, QdrantDataKey, QdrantError, QdrantPoint},
    limits::{MAX_BATCH_POINTS, MAX_QUERY_PARTITIONS},
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
    selected: &[PartitionId],
    query_coordinates: &[i16],
    requested_limit: usize,
    available_output: usize,
) -> Result<(), QdrantError> {
    if selected.len() > MAX_QUERY_PARTITIONS {
        return Err(QdrantAdmissionError::TooManyPartitions {
            maximum: MAX_QUERY_PARTITIONS,
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
    reject_duplicate_partitions(selected)?;
    Ok(())
}

pub(super) fn prepare_points<'coordinates>(
    authority: VectorAuthority,
    points: &[QdrantPoint<'coordinates>],
) -> Result<ArrayVec<PreparedPoint<'coordinates>, MAX_BATCH_POINTS>, QdrantError> {
    if points.len() > MAX_BATCH_POINTS {
        return Err(QdrantAdmissionError::BatchTooLarge {
            maximum: MAX_BATCH_POINTS,
            observed: points.len(),
        }
        .into());
    }
    let mut prepared: ArrayVec<PreparedPoint<'coordinates>, MAX_BATCH_POINTS> = ArrayVec::new();
    for (index, point) in points.iter().copied().enumerate() {
        validate_point(authority, index, point)?;
        let physical_id = point.physical_id();
        for first in prepared.iter().copied() {
            reject_identity_pair(
                first.index,
                first.key,
                first.physical_id,
                index,
                point.key,
                physical_id,
            )?;
        }
        prepared
            .try_push(PreparedPoint {
                index,
                key: point.key,
                coordinates: point.coordinates,
                physical_id,
            })
            .map_err(|_| QdrantAdmissionError::BatchTooLarge {
                maximum: MAX_BATCH_POINTS,
                observed: points.len(),
            })?;
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
        prepared
            .try_push(PreparedKey {
                index,
                key,
                physical_id,
            })
            .map_err(|_| QdrantAdmissionError::BatchTooLarge {
                maximum: MAX_BATCH_POINTS,
                observed: keys.len(),
            })?;
    }
    Ok(prepared)
}

fn validate_point(
    authority: VectorAuthority,
    index: usize,
    point: QdrantPoint<'_>,
) -> Result<(), QdrantError> {
    reject_wrong_authority(index, authority, point.key.authority)?;
    let expected = usize::from(authority.dimension);
    if point.coordinates.len() != expected {
        return Err(QdrantAdmissionError::WrongDimension {
            index,
            expected,
            observed: point.coordinates.len(),
        }
        .into());
    }
    Ok(())
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

pub(super) fn reject_duplicate_partitions(
    selected: &[PartitionId],
) -> Result<(), QdrantAdmissionError> {
    for index in 0..selected.len() {
        for first_index in 0..index {
            let Some(first) = selected.get(first_index) else {
                continue;
            };
            let Some(current) = selected.get(index) else {
                continue;
            };
            if first == current {
                return Err(QdrantAdmissionError::DuplicatePartition {
                    first_index,
                    index,
                    partition: *current,
                });
            }
        }
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
            first_partition: first.partition,
            first_entity: first.entity,
            second_partition: second.partition,
            second_entity: second.entity,
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
