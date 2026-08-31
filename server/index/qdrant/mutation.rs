//! Defines mutation behavior for `server-index-qdrant`, whose purpose is to adapt typed vector authorities and queries to the Qdrant service.
//! This module owns the mutation invariants and typed state transitions.
//! Its narrow surface prevents representation and policy details from leaking outward.
//! Verified point mutations and readback admission.

use arrayvec::ArrayVec;
use server_index_graph_vector::ValidatedVectorSegment;

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

impl QdrantBlockingAdapter {
    /// Upserts a bounded batch, then independently reads every physical point back.
    pub fn upsert(
        &self,
        segments: &[ValidatedVectorSegment<'_>],
    ) -> Result<QdrantMutationReceipt, QdrantError> {
        let prepared = admission::prepare_segments(self.authority, segments)?;
        if prepared.is_empty() {
            return Ok(QdrantMutationReceipt {
                attempted: 0,
                verified: 0,
            });
        }
        self.ensure_existing_ids_are_compatible(&prepared)?;
        let request = wire::UpsertRequest::from_points(&prepared);
        let response = self.request_json(
            RequestPhase::UpsertPoints,
            Method::Put,
            &self.url(UPSERT_POINTS_PATH),
            request,
        )?;
        if !transport::is_success(response.status) {
            return Err(transport::status_error(
                RequestPhase::UpsertPoints,
                response,
            ));
        }
        wire::parse_completed_ack(RequestPhase::UpsertPoints, &response.body)?;
        let mut readback: ArrayVec<Option<QdrantReadback>, MAX_BATCH_POINTS> = ArrayVec::new();
        for _ in &prepared {
            readback.push(None);
        }
        let verified = self.readback_into(&prepared, &mut readback)?;
        Ok(QdrantMutationReceipt {
            attempted: prepared.len(),
            verified,
        })
    }

    /// Reads and validates a bounded batch of points without changing caller output on failure.
    pub fn readback(
        &self,
        keys: &[QdrantDataKey],
        output: &mut [Option<QdrantReadback>],
    ) -> Result<usize, QdrantError> {
        let prepared = admission::prepare_keys(self.authority, keys)?;
        if prepared.is_empty() {
            return Ok(0);
        }
        if prepared.len() > output.len() {
            return Err(QdrantAdmissionError::InsufficientOutput {
                required: prepared.len(),
                available: output.len(),
            }
            .into());
        }
        let readbacks = self.fetch_points(&prepared, RequestPhase::ReadPoints, true, true)?;
        for slot in output.iter_mut().take(prepared.len()) {
            *slot = None;
        }
        for point in readbacks {
            let Some(index) = prepared
                .iter()
                .position(|key| key.physical_id == point.physical_id)
            else {
                return Err(QdrantError::UnexpectedPoint {
                    phase: RequestPhase::ReadPoints,
                    physical_id: point.physical_id,
                });
            };
            let Some(slot) = output.get_mut(index) else {
                return Err(QdrantError::MalformedResponse {
                    phase: RequestPhase::ReadPoints,
                    cause: MalformedResponseCause::ReadbackOutputIndex,
                });
            };
            *slot = Some(point);
        }
        Ok(prepared.len())
    }

    /// Deletes a bounded batch, then independently reads all physical IDs back as absent.
    pub fn delete(&self, keys: &[QdrantDataKey]) -> Result<QdrantMutationReceipt, QdrantError> {
        let prepared = admission::prepare_keys(self.authority, keys)?;
        if prepared.is_empty() {
            return Ok(QdrantMutationReceipt {
                attempted: 0,
                verified: 0,
            });
        }
        self.ensure_existing_ids_are_compatible(&prepared)?;
        let response = self.request_json(
            RequestPhase::DeletePoints,
            Method::Post,
            &self.url(DELETE_POINTS_PATH),
            wire::DeleteRequest::new(&prepared),
        )?;
        if !transport::is_success(response.status) {
            return Err(transport::status_error(
                RequestPhase::DeletePoints,
                response,
            ));
        }
        wire::parse_completed_ack(RequestPhase::DeletePoints, &response.body)?;
        let remaining = self.fetch_points_allow_missing(&prepared, RequestPhase::VerifyDelete)?;
        if let Some(point) = remaining.first() {
            return Err(QdrantError::VectorMismatch {
                phase: RequestPhase::VerifyDelete,
                physical_id: point.physical_id,
            });
        }
        Ok(QdrantMutationReceipt {
            attempted: prepared.len(),
            verified: prepared.len(),
        })
    }

    fn ensure_existing_ids_are_compatible(
        &self,
        keys: &[impl PreparedIdentity],
    ) -> Result<(), QdrantError> {
        let compare_coordinates = keys.iter().any(|key| key.coordinates().is_some());
        let readbacks =
            self.fetch_points(keys, RequestPhase::ReadPoints, compare_coordinates, false)?;
        for readback in readbacks {
            let Some(expected) = keys
                .iter()
                .find(|key| key.physical_id() == readback.physical_id)
            else {
                return Err(QdrantError::UnexpectedPoint {
                    phase: RequestPhase::ReadPoints,
                    physical_id: readback.physical_id,
                });
            };
            if expected.key() != readback.key {
                return Err(QdrantAdmissionError::RemoteIdentityMismatch {
                    physical_id: readback.physical_id,
                    key: expected.key().into(),
                }
                .into());
            }
            if let Some(coordinates) = expected.coordinates()
                && !admission::vector_matches(coordinates, &readback.coordinates)
            {
                return Err(QdrantAdmissionError::ImmutableVectorConflict {
                    physical_id: readback.physical_id,
                    key: readback.key.into(),
                }
                .into());
            }
        }
        Ok(())
    }

    fn fetch_points(
        &self,
        keys: &[impl PreparedIdentity],
        phase: RequestPhase,
        require_vectors: bool,
        require_all: bool,
    ) -> Result<ArrayVec<QdrantReadback, MAX_BATCH_POINTS>, QdrantError> {
        let response = self.request_json(
            phase,
            Method::Post,
            &self.url(READ_POINTS_PATH),
            wire::RetrieveRequest::new(keys, require_vectors),
        )?;
        if !transport::is_success(response.status) {
            return Err(transport::status_error(phase, response));
        }
        let points = wire::retrieve_points(phase, &response.body)?;
        if points.len() > MAX_BATCH_POINTS {
            return Err(QdrantError::MalformedResponse {
                phase,
                cause: MalformedResponseCause::PointBatchExceeded,
            });
        }
        let mut readbacks: ArrayVec<QdrantReadback, MAX_BATCH_POINTS> = ArrayVec::new();
        for point in &points {
            let physical_id = super::contract::PhysicalPointId(point.id);
            if readbacks
                .iter()
                .any(|readback: &QdrantReadback| readback.physical_id == physical_id)
            {
                return Err(QdrantError::MalformedResponse {
                    phase,
                    cause: MalformedResponseCause::DuplicatePhysicalPoint,
                });
            }
            let Some(expected) = keys.iter().find(|key| key.physical_id().0 == physical_id.0)
            else {
                return Err(QdrantError::UnexpectedPoint { phase, physical_id });
            };
            let key = wire::decode_identity(point, phase, self.authority.dimension)?;
            if key != expected.key() {
                return Err(QdrantError::PayloadMismatch {
                    phase,
                    physical_id,
                    cause: super::contract::PayloadMismatchCause::RequestedIdentity {
                        expected: expected.key().into(),
                        observed: key.into(),
                    },
                });
            }
            let coordinates = if require_vectors {
                let decoded = wire::decode_vector(
                    point,
                    phase,
                    physical_id,
                    usize::from(self.authority.dimension),
                )?;
                QdrantCoordinates::copy_from(decoded).map_err(|source| {
                    QdrantError::CoordinateCapacity {
                        phase,
                        physical_id,
                        source,
                    }
                })?
            } else {
                QdrantCoordinates::empty()
            };
            if let Err(_rejected) = readbacks.try_push(QdrantReadback {
                key,
                physical_id,
                coordinates,
            }) {
                return Err(QdrantError::MalformedResponse {
                    phase,
                    cause: MalformedResponseCause::PointBatchExceeded,
                });
            }
        }
        if require_all {
            for key in keys {
                let Some(readback) = readbacks
                    .iter()
                    .find(|point| point.physical_id == key.physical_id())
                else {
                    return Err(QdrantError::MissingPoint {
                        phase,
                        physical_id: key.physical_id(),
                        key: key.key().into(),
                    });
                };
                if let Some(point) = key.coordinates()
                    && !admission::vector_matches(point, &readback.coordinates)
                {
                    return Err(QdrantError::VectorMismatch {
                        phase,
                        physical_id: readback.physical_id,
                    });
                }
            }
        }
        Ok(readbacks)
    }

    fn fetch_points_allow_missing(
        &self,
        keys: &[impl PreparedIdentity],
        phase: RequestPhase,
    ) -> Result<ArrayVec<QdrantReadback, MAX_BATCH_POINTS>, QdrantError> {
        let response = self.request_json(
            phase,
            Method::Post,
            &self.url(READ_POINTS_PATH),
            wire::RetrieveRequest::new(keys, false),
        )?;
        if !transport::is_success(response.status) {
            return Err(transport::status_error(phase, response));
        }
        let points = wire::retrieve_points(phase, &response.body)?;
        if points.len() > MAX_BATCH_POINTS {
            return Err(QdrantError::MalformedResponse {
                phase,
                cause: MalformedResponseCause::PointBatchExceeded,
            });
        }
        let mut readbacks: ArrayVec<QdrantReadback, MAX_BATCH_POINTS> = ArrayVec::new();
        for point in &points {
            let physical_id = super::contract::PhysicalPointId(point.id);
            let key = wire::decode_identity(point, phase, self.authority.dimension)?;
            if !keys
                .iter()
                .any(|expected| expected.physical_id() == physical_id)
            {
                return Err(QdrantError::UnexpectedPoint { phase, physical_id });
            }
            if readbacks
                .iter()
                .any(|readback: &QdrantReadback| readback.physical_id == physical_id)
            {
                return Err(QdrantError::MalformedResponse {
                    phase,
                    cause: MalformedResponseCause::DuplicatePhysicalPoint,
                });
            }
            if let Err(_rejected) = readbacks.try_push(QdrantReadback {
                key,
                physical_id,
                coordinates: QdrantCoordinates::empty(),
            }) {
                return Err(QdrantError::MalformedResponse {
                    phase,
                    cause: MalformedResponseCause::PointBatchExceeded,
                });
            }
        }
        Ok(readbacks)
    }

    fn readback_into(
        &self,
        keys: &[PreparedPoint<'_>],
        output: &mut [Option<QdrantReadback>],
    ) -> Result<usize, QdrantError> {
        let readbacks = self.fetch_points(keys, RequestPhase::ReadPoints, true, true)?;
        if readbacks.len() > output.len() {
            return Err(QdrantAdmissionError::InsufficientOutput {
                required: readbacks.len(),
                available: output.len(),
            }
            .into());
        }
        for slot in output.iter_mut().take(readbacks.len()) {
            *slot = None;
        }
        for readback in readbacks {
            let Some(index) = keys
                .iter()
                .position(|key| key.physical_id == readback.physical_id)
            else {
                return Err(QdrantError::UnexpectedPoint {
                    phase: RequestPhase::ReadPoints,
                    physical_id: readback.physical_id,
                });
            };
            let Some(slot) = output.get_mut(index) else {
                return Err(QdrantError::MalformedResponse {
                    phase: RequestPhase::ReadPoints,
                    cause: MalformedResponseCause::ReadbackOutputIndex,
                });
            };
            *slot = Some(readback);
        }
        for key in keys {
            if output.get(key.index).is_none_or(|slot| slot.is_none()) {
                return Err(QdrantError::MissingPoint {
                    phase: RequestPhase::ReadPoints,
                    physical_id: key.physical_id,
                    key: key.key.into(),
                });
            }
        }
        Ok(keys.len())
    }
}
