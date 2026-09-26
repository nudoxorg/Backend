//! Defines mutation behavior for `backend-extension-qdrant`, whose purpose is to adapt typed vector authorities and queries to the Qdrant service.
//! This module owns the mutation invariants and typed state transitions.
//! Its narrow surface prevents representation and policy details from leaking outward.
//! Verified point mutations and readback admission.

use arrayvec::ArrayVec;
use backend_semantic::graph_vector::ValidatedVectorSegment;

use super::{
    QdrantBlockingAdapter,
    admission::{self, PreparedIdentity, PreparedPoint},
    contract::{
        MalformedResponseCause, QdrantAdmissionError, QdrantCoordinates, QdrantDataKey,
        QdrantError, QdrantMutationReceipt, QdrantReadback, RequestPhase,
    },
    limits::{DELETE_POINTS_PATH, MAX_BATCH_POINTS, READ_POINTS_PATH, UPSERT_POINTS_PATH},
    transport::{self, Method},
    wire,
};

mod apply;

/// Returns prepared indexes that still require an upsert write, or the preflight conflict error.
fn upsert_due_indexes(
    prepared: &[PreparedPoint<'_>],
    readbacks: &[QdrantReadback],
) -> Result<ArrayVec<usize, MAX_BATCH_POINTS>, QdrantError> {
    for readback in readbacks {
        if !prepared
            .iter()
            .any(|point| point.physical_id == readback.physical_id)
        {
            return Err(QdrantError::UnexpectedPoint {
                phase: RequestPhase::ReadPoints,
                physical_id: readback.physical_id,
            });
        }
    }
    let mut due = ArrayVec::new();
    for (index, point) in prepared.iter().enumerate() {
        let Some(readback) = readbacks
            .iter()
            .find(|readback| readback.physical_id == point.physical_id)
        else {
            if let Err(_rejected) = due.try_push(index) {
                return Err(QdrantError::MalformedResponse {
                    phase: RequestPhase::ReadPoints,
                    cause: MalformedResponseCause::PointBatchExceeded,
                });
            }
            continue;
        };
        if point.key != readback.key {
            return Err(QdrantAdmissionError::RemoteIdentityMismatch {
                physical_id: readback.physical_id,
                key: point.key.into(),
            }
            .into());
        }
        if !admission::vector_matches(point.coordinates, &readback.coordinates) {
            return Err(QdrantAdmissionError::ImmutableVectorConflict {
                physical_id: readback.physical_id,
                key: readback.key.into(),
            }
            .into());
        }
    }
    Ok(due)
}


#[cfg(test)]
mod tests {
    use backend_semantic::graph_vector::{Metric, ModelId, PartitionId, VectorAuthority};
    use backend_semantic::index_vocabulary::{IndexSnapshotId, VectorSegmentId};
    use backend_semantic::ir::EntityId;

    use super::upsert_due_indexes;
    use crate::server::admission::PreparedPoint;
    use crate::server::contract::{
        PhysicalPointId, QdrantAdmissionError, QdrantCoordinates, QdrantDataKey, QdrantError,
        QdrantReadback,
    };

    fn authority() -> VectorAuthority {
        VectorAuthority::new(
            IndexSnapshotId::from_canonical_bytes(&[1; 32]),
            ModelId::new([2; 16]),
            1,
            Metric::SquaredEuclidean,
        )
    }

    fn prepared_point(coordinates: &[i16]) -> PreparedPoint<'_> {
        let authority = authority();
        let key = QdrantDataKey::new(
            authority,
            VectorSegmentId::from_canonical_bytes(&[3; 32]),
            PartitionId::new(1),
            EntityId::new(1),
        );
        PreparedPoint {
            index: 0,
            key,
            coordinates,
            physical_id: PhysicalPointId::for_key(key),
        }
    }

    fn readback(key: QdrantDataKey, coordinates: &[f64]) -> QdrantReadback {
        QdrantReadback {
            key,
            physical_id: PhysicalPointId::for_key(key),
            coordinates: QdrantCoordinates::copy_from(coordinates).expect("coordinate capacity"),
        }
    }

    #[test]
    fn upsert_due_indexes_skips_matching_readbacks() {
        let coordinates = [1_i16];
        let point = prepared_point(&coordinates);
        let readbacks = [readback(point.key, &[1.0])];
        let due = upsert_due_indexes(&[point], &readbacks);
        assert!(due.is_ok());
        let Ok(due) = due else {
            return;
        };
        assert!(due.is_empty());
    }

    #[test]
    fn upsert_due_indexes_marks_absent_ids_as_due() {
        let coordinates = [1_i16];
        let point = prepared_point(&coordinates);
        let due = upsert_due_indexes(&[point], &[]);
        assert!(due.is_ok());
        let Ok(due) = due else {
            return;
        };
        assert_eq!(due.as_slice(), [0]);
    }

    #[test]
    fn upsert_due_indexes_rejects_immutable_vector_conflict() {
        let coordinates = [1_i16];
        let point = prepared_point(&coordinates);
        let readbacks = [readback(point.key, &[2.0])];
        let due = upsert_due_indexes(&[point], &readbacks);
        assert!(matches!(
            due,
            Err(QdrantError::Admission(
                QdrantAdmissionError::ImmutableVectorConflict { .. }
            ))
        ));
    }
}
