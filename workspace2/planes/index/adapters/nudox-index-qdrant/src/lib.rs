#![forbid(unsafe_code)]
#![warn(missing_docs)]
//! Blocking Qdrant projection for immutable vector snapshots.
//!
//! The adapter owns only the disposable remote projection.  A point's payload retains the
//! complete snapshot/model/metric/partition/entity authority, while its Qdrant integer ID is a
//! physical coordinate checked against that payload on every write and readback.  Network calls
//! are deliberately synchronous: callers that need an async stream can place this named blocking
//! edge behind their bounded runtime adapter.

use std::{num::NonZeroU8, time::Duration};

use nudox_index_graph_vector::{Metric, ModelId, PartitionId, VectorAuthority};
use serde::Serialize;

mod contract;
mod wire;

pub use contract::*;

const DEFAULT_MAX_ATTEMPTS: NonZeroU8 = match NonZeroU8::new(3) {
    Some(value) => value,
    None => NonZeroU8::MIN,
};
const MAX_BATCH_POINTS: usize = 16;
const QUERY_SCAN_LIMIT: usize = MAX_BATCH_POINTS + 1;
const MAX_QUERY_PARTITIONS: usize = 4;
const MAX_VECTOR_DIMENSION: usize = 4096;
const MAX_RESPONSE_BYTES: u64 = 1_048_576;
const READ_POINTS_PATH: &str = "/points?consistency=all";
const QUERY_POINTS_PATH: &str = "/points/query?consistency=all";
const UPSERT_POINTS_PATH: &str = "/points?wait=true&ordering=strong";
const DELETE_POINTS_PATH: &str = "/points/delete?wait=true&ordering=strong";
const CREATE_PAYLOAD_INDEX_PATH: &str = "/index?wait=true&ordering=strong";
const FNV_OFFSET: u64 = 14_695_981_039_346_656_037;
const FNV_PRIME: u64 = 1_099_511_628_211;
const PHYSICAL_ID_ZERO_REPLACEMENT: u64 = 1;

/// Payload key carrying the immutable snapshot digest as lowercase hexadecimal.
pub const SNAPSHOT_PAYLOAD_KEY: &str = "nudox_snapshot";
/// Payload key carrying the complete model registry identity as lowercase hexadecimal.
pub const MODEL_PAYLOAD_KEY: &str = "nudox_model";
/// Payload key carrying the metric recipe name.
pub const METRIC_PAYLOAD_KEY: &str = "nudox_metric";
/// Payload key carrying the projection partition coordinate.
pub const PARTITION_PAYLOAD_KEY: &str = "nudox_partition";
/// Payload key carrying the semantic entity coordinate.
pub const ENTITY_PAYLOAD_KEY: &str = "nudox_entity";

const PAYLOAD_INDEXES: [(PayloadField, &str); 4] = [
    (PayloadField::Snapshot, "keyword"),
    (PayloadField::Model, "keyword"),
    (PayloadField::Metric, "keyword"),
    (PayloadField::Partition, "integer"),
];

/// A named blocking Qdrant adapter owning endpoint, collection, authority, and connection pool.
pub struct QdrantBlockingAdapter {
    agent: ureq::Agent,
    endpoint: String,
    collection: String,
    authority: VectorAuthority,
    retry: RetryPolicy,
}

impl QdrantBlockingAdapter {
    /// Creates a blocking adapter over a caller-provided HTTP(S) endpoint.
    pub fn new(
        endpoint: &str,
        collection: &str,
        authority: VectorAuthority,
    ) -> Result<Self, QdrantError> {
        Self::with_retry(
            endpoint,
            collection,
            authority,
            RetryPolicy::default_policy(),
        )
    }

    /// Creates an adapter with an explicit bounded retry policy.
    pub fn with_retry(
        endpoint: &str,
        collection: &str,
        authority: VectorAuthority,
        retry: RetryPolicy,
    ) -> Result<Self, QdrantError> {
        let endpoint = normalize_endpoint(endpoint)?;
        let collection = validate_collection(collection)?;
        let dimension = usize::from(authority.dimension());
        if dimension == 0 || dimension > MAX_VECTOR_DIMENSION {
            return Err(QdrantConfigError::InvalidDimension {
                maximum: MAX_VECTOR_DIMENSION,
                observed: dimension,
            }
            .into());
        }
        let config = ureq::Agent::config_builder()
            .timeout_global(Some(Duration::from_secs(15)))
            .http_status_as_error(false)
            .build();
        Ok(Self {
            agent: config.new_agent(),
            endpoint,
            collection,
            authority,
            retry,
        })
    }

    /// Returns the adapter's immutable vector authority.
    #[must_use]
    pub const fn authority(&self) -> VectorAuthority {
        self.authority
    }

    /// Returns the caller-selected collection name.
    #[must_use]
    pub fn collection(&self) -> &str {
        &self.collection
    }

    /// Returns the normalized endpoint, without a trailing slash.
    #[must_use]
    pub fn endpoint(&self) -> &str {
        &self.endpoint
    }

    /// Returns the bounded retry policy.
    #[must_use]
    pub const fn retry_policy(&self) -> RetryPolicy {
        self.retry
    }

    /// Creates the collection when absent and verifies its dimension and metric when present.
    pub fn ensure_collection(&self) -> Result<(), QdrantError> {
        let body = wire::CollectionRequest::new(self.authority);
        let response = self.request_json(
            RequestPhase::CreateCollection,
            Method::Put,
            &self.url(""),
            body,
        )?;
        if response.status == 404 || response.status == 409 {
            if response.status == 404 {
                let create = self.request_json(
                    RequestPhase::CreateCollection,
                    Method::Put,
                    &self.url("?wait=true"),
                    wire::CollectionRequest::new(self.authority),
                )?;
                if !is_success(create.status) && create.status != 409 {
                    return Err(status_error(RequestPhase::CreateCollection, create));
                }
            }
        } else if !is_success(response.status) {
            return Err(status_error(RequestPhase::CreateCollection, response));
        }
        self.verify_collection()?;
        self.ensure_payload_indexes()?;
        self.verify_payload_indexes()
    }

    /// Deletes the caller-selected collection and verifies a successful service response.
    pub fn delete_collection(&self) -> Result<(), QdrantError> {
        let response = self.request_json(
            RequestPhase::DeleteCollection,
            Method::Delete,
            &self.url(""),
            (),
        )?;
        if !is_success(response.status) {
            return Err(status_error(RequestPhase::DeleteCollection, response));
        }
        wire::parse_ack(RequestPhase::DeleteCollection, &response.body)
    }

    /// Upserts a bounded batch, then independently reads every physical point back.
    pub fn upsert(&self, points: &[QdrantPoint<'_>]) -> Result<QdrantMutationReceipt, QdrantError> {
        let prepared = self.prepare_points(points)?;
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
        if !is_success(response.status) {
            return Err(status_error(RequestPhase::UpsertPoints, response));
        }
        wire::parse_ack(RequestPhase::UpsertPoints, &response.body)?;
        let mut readback = vec![None; prepared.len()];
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
        let prepared = self.prepare_keys(keys)?;
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
        let prepared = self.prepare_keys(keys)?;
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
        if !is_success(response.status) {
            return Err(status_error(RequestPhase::DeletePoints, response));
        }
        wire::parse_ack(RequestPhase::DeletePoints, &response.body)?;
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

    /// Queries Qdrant with full authority filters and reports exact absent selected partitions.
    pub fn query(
        &self,
        selected: &[PartitionId],
        query_coordinates: &[i16],
        requested_limit: usize,
        output: &mut [Option<QdrantHit>],
    ) -> Result<QdrantQueryOutcome, QdrantError> {
        self.validate_query(selected, query_coordinates, requested_limit, output.len())?;
        let response = self.request_json(
            RequestPhase::QueryPoints,
            Method::Post,
            &self.url(QUERY_POINTS_PATH),
            wire::QueryRequest::new(self.authority, selected, query_coordinates),
        )?;
        if !is_success(response.status) {
            return Err(status_error(RequestPhase::QueryPoints, response));
        }
        let mut hits =
            wire::parse_query_hits(self.authority, selected, query_coordinates, &response.body)?;
        let terminal = remote_terminal(self.authority, selected, &hits);
        hits.sort_by(|left, right| compare_hits(*left, *right));
        hits.truncate(requested_limit);
        let written = hits.len();
        for slot in output.iter_mut().take(requested_limit) {
            *slot = None;
        }
        for (slot, hit) in output.iter_mut().zip(hits) {
            *slot = Some(hit);
        }
        Ok(QdrantQueryOutcome { written, terminal })
    }

    /// Builds the local scalar control using the same authority and identity types as the graph/vector plane.
    pub fn local_brute_force(
        &self,
        selected: &[PartitionId],
        points: &[QdrantPoint<'_>],
        query_coordinates: &[i16],
        requested_limit: usize,
        output: &mut [Option<QdrantHit>],
    ) -> Result<QdrantQueryOutcome, QdrantError> {
        self.validate_query(selected, query_coordinates, requested_limit, output.len())?;
        let prepared = self.prepare_points(points)?;
        let mut hits = Vec::with_capacity(prepared.len());
        for point in prepared.iter().copied() {
            if selected.is_empty() || selected.contains(&point.key.partition()) {
                hits.push(QdrantHit {
                    authority: self.authority,
                    partition: point.key.partition(),
                    entity: point.key.entity(),
                    score: local_score(
                        self.authority.metric(),
                        query_coordinates,
                        point.coordinates,
                    ),
                    physical_id: point.physical_id,
                });
            }
        }
        hits.sort_by(|left, right| compare_hits(*left, *right));
        hits.truncate(requested_limit);
        let written = hits.len();
        let terminal = local_terminal(self.authority, selected, &prepared);
        for slot in output.iter_mut().take(requested_limit) {
            *slot = None;
        }
        for (slot, hit) in output.iter_mut().zip(hits) {
            *slot = Some(hit);
        }
        Ok(QdrantQueryOutcome { written, terminal })
    }

    fn verify_collection(&self) -> Result<(), QdrantError> {
        let response =
            self.request_json(RequestPhase::ReadCollection, Method::Get, &self.url(""), ())?;
        if !is_success(response.status) {
            return Err(status_error(RequestPhase::ReadCollection, response));
        }
        let metadata = wire::collection_metadata(RequestPhase::ReadCollection, &response.body)?;
        let expected_size = u64::from(self.authority.dimension());
        if metadata.dimension != expected_size {
            return Err(QdrantError::CollectionMismatch {
                phase: RequestPhase::ReadCollection,
                field: CollectionField::VectorDimension,
            });
        }
        if metadata.metric != metric_name(self.authority.metric()) {
            return Err(QdrantError::CollectionMismatch {
                phase: RequestPhase::ReadCollection,
                field: CollectionField::VectorMetric,
            });
        }
        if metadata.write_consistency_factor < metadata.replication_factor {
            return Err(QdrantError::CollectionMismatch {
                phase: RequestPhase::ReadCollection,
                field: CollectionField::WriteConsistency,
            });
        }
        Ok(())
    }

    fn ensure_payload_indexes(&self) -> Result<(), QdrantError> {
        for (field, schema) in PAYLOAD_INDEXES {
            let response = self.request_json(
                RequestPhase::CreatePayloadIndex,
                Method::Put,
                &self.url(CREATE_PAYLOAD_INDEX_PATH),
                wire::PayloadIndexRequest::new(field, schema),
            )?;
            if !is_success(response.status) && response.status != 409 {
                return Err(status_error(RequestPhase::CreatePayloadIndex, response));
            }
            if is_success(response.status) {
                wire::parse_ack(RequestPhase::CreatePayloadIndex, &response.body)?;
            }
        }
        Ok(())
    }

    fn verify_payload_indexes(&self) -> Result<(), QdrantError> {
        let response =
            self.request_json(RequestPhase::ReadCollection, Method::Get, &self.url(""), ())?;
        if !is_success(response.status) {
            return Err(status_error(RequestPhase::ReadCollection, response));
        }
        wire::verify_payload_indexes(
            RequestPhase::ReadCollection,
            &response.body,
            &PAYLOAD_INDEXES,
        )
    }

    fn validate_query(
        &self,
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
        let expected = usize::from(self.authority.dimension());
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

    fn prepare_points<'coordinates>(
        &self,
        points: &[QdrantPoint<'coordinates>],
    ) -> Result<Vec<PreparedPoint<'coordinates>>, QdrantError> {
        if points.len() > MAX_BATCH_POINTS {
            return Err(QdrantAdmissionError::BatchTooLarge {
                maximum: MAX_BATCH_POINTS,
                observed: points.len(),
            }
            .into());
        }
        let mut prepared: Vec<PreparedPoint<'_>> = Vec::with_capacity(points.len());
        for (index, point) in points.iter().copied().enumerate() {
            self.validate_point(index, point)?;
            let physical_id = point.physical_id();
            for first in prepared.iter().copied() {
                reject_identity_pair(
                    first.index,
                    first.key,
                    first.physical_id,
                    index,
                    point.key(),
                    physical_id,
                )?;
            }
            prepared.push(PreparedPoint {
                index,
                key: point.key(),
                coordinates: point.coordinates(),
                physical_id,
            });
        }
        Ok(prepared)
    }

    fn prepare_keys(&self, keys: &[QdrantDataKey]) -> Result<Vec<PreparedKey>, QdrantError> {
        if keys.len() > MAX_BATCH_POINTS {
            return Err(QdrantAdmissionError::BatchTooLarge {
                maximum: MAX_BATCH_POINTS,
                observed: keys.len(),
            }
            .into());
        }
        let mut prepared: Vec<PreparedKey> = Vec::with_capacity(keys.len());
        for (index, key) in keys.iter().copied().enumerate() {
            reject_wrong_authority(index, self.authority, key.authority())?;
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
            prepared.push(PreparedKey {
                index,
                key,
                physical_id,
            });
        }
        Ok(prepared)
    }

    fn validate_point(&self, index: usize, point: QdrantPoint<'_>) -> Result<(), QdrantError> {
        reject_wrong_authority(index, self.authority, point.key.authority())?;
        let expected = usize::from(self.authority.dimension());
        if point.coordinates().len() != expected {
            return Err(QdrantAdmissionError::WrongDimension {
                index,
                expected,
                observed: point.coordinates().len(),
            }
            .into());
        }
        Ok(())
    }

    fn ensure_existing_ids_are_compatible(
        &self,
        keys: &[impl PreparedIdentity],
    ) -> Result<(), QdrantError> {
        let readbacks = self.fetch_points(keys, RequestPhase::ReadPoints, false, false)?;
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
                    key: expected.key(),
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
    ) -> Result<Vec<QdrantReadback>, QdrantError> {
        let response = self.request_json(
            phase,
            Method::Post,
            &self.url(READ_POINTS_PATH),
            wire::RetrieveRequest::new(keys, require_vectors),
        )?;
        if !is_success(response.status) {
            return Err(status_error(phase, response));
        }
        let points = wire::retrieve_points(phase, &response.body)?;
        if points.len() > MAX_BATCH_POINTS {
            return Err(QdrantError::MalformedResponse {
                phase,
                cause: MalformedResponseCause::PointBatchExceeded,
            });
        }
        let mut readbacks = Vec::with_capacity(points.len());
        for point in &points {
            let physical_id = PhysicalPointId(point.id);
            if readbacks
                .iter()
                .any(|readback: &QdrantReadback| readback.physical_id == physical_id)
            {
                return Err(QdrantError::MalformedResponse {
                    phase,
                    cause: MalformedResponseCause::DuplicatePhysicalPoint,
                });
            }
            let Some(expected) = keys
                .iter()
                .find(|key| key.physical_id().raw() == physical_id.raw())
            else {
                return Err(QdrantError::UnexpectedPoint { phase, physical_id });
            };
            let key = wire::decode_identity(point, phase, self.authority.dimension())?;
            if key != expected.key() {
                return Err(QdrantError::PayloadMismatch {
                    phase,
                    physical_id,
                    cause: PayloadMismatchCause::RequestedIdentity,
                });
            }
            let coordinates = if require_vectors {
                wire::decode_vector(
                    point,
                    phase,
                    physical_id,
                    usize::from(self.authority.dimension()),
                )?
                .to_vec()
            } else {
                Vec::new()
            };
            readbacks.push(QdrantReadback {
                key,
                physical_id,
                coordinates,
            });
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
                        key: key.key(),
                    });
                };
                if let Some(point) = key.coordinates()
                    && !vector_matches(point, &readback.coordinates)
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
    ) -> Result<Vec<QdrantReadback>, QdrantError> {
        let response = self.request_json(
            phase,
            Method::Post,
            &self.url(READ_POINTS_PATH),
            wire::RetrieveRequest::new(keys, false),
        )?;
        if !is_success(response.status) {
            return Err(status_error(phase, response));
        }
        let points = wire::retrieve_points(phase, &response.body)?;
        if points.len() > MAX_BATCH_POINTS {
            return Err(QdrantError::MalformedResponse {
                phase,
                cause: MalformedResponseCause::PointBatchExceeded,
            });
        }
        let mut readbacks = Vec::with_capacity(points.len());
        for point in &points {
            let physical_id = PhysicalPointId(point.id);
            let key = wire::decode_identity(point, phase, self.authority.dimension())?;
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
            readbacks.push(QdrantReadback {
                key,
                physical_id,
                coordinates: Vec::new(),
            });
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
                    key: key.key,
                });
            }
        }
        Ok(keys.len())
    }

    fn url(&self, suffix: &str) -> String {
        format!(
            "{}/collections/{}{}",
            self.endpoint, self.collection, suffix
        )
    }

    fn request_json<T: Serialize>(
        &self,
        phase: RequestPhase,
        method: Method,
        url: &str,
        body: T,
    ) -> Result<ResponseEnvelope, QdrantError> {
        let encoded =
            serde_json::to_vec(&body).map_err(|source| QdrantError::Encode { phase, source })?;
        let attempts = self.retry.attempts.get();
        let mut last_transport = None;
        for attempt in 1..=attempts {
            let result = match method {
                Method::Get => self.agent.get(url).call(),
                Method::Put => self
                    .agent
                    .put(url)
                    .content_type("application/json")
                    .send(encoded.clone()),
                Method::Post => self
                    .agent
                    .post(url)
                    .content_type("application/json")
                    .send(encoded.clone()),
                Method::Delete => self.agent.delete(url).call(),
            };
            match result {
                Ok(mut response) => {
                    let status = response.status().as_u16();
                    match response
                        .body_mut()
                        .with_config()
                        .limit(MAX_RESPONSE_BYTES)
                        .read_to_string()
                    {
                        Ok(body) => {
                            if is_success(status)
                                || !retryable_status(status)
                                || attempt == attempts
                            {
                                return Ok(ResponseEnvelope {
                                    status,
                                    attempts: attempt,
                                    body,
                                });
                            }
                        }
                        Err(source) => {
                            if attempt == attempts {
                                return Err(QdrantError::Transport {
                                    phase,
                                    attempts,
                                    source,
                                });
                            }
                            last_transport = Some(source);
                        }
                    }
                }
                Err(source) => {
                    if attempt == attempts {
                        return Err(QdrantError::Transport {
                            phase,
                            attempts,
                            source,
                        });
                    }
                    last_transport = Some(source);
                }
            }
        }
        match last_transport {
            Some(source) => Err(QdrantError::Transport {
                phase,
                attempts,
                source,
            }),
            None => Err(QdrantError::MalformedResponse {
                phase,
                cause: MalformedResponseCause::RetryExhaustionWithoutResponse,
            }),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Method {
    Get,
    Put,
    Post,
    Delete,
}

#[derive(Clone, Debug)]
struct ResponseEnvelope {
    status: u16,
    attempts: u8,
    body: String,
}

#[derive(Clone, Copy, Debug)]
struct PreparedPoint<'coordinates> {
    index: usize,
    key: QdrantDataKey,
    coordinates: &'coordinates [i16],
    physical_id: PhysicalPointId,
}

#[derive(Clone, Copy, Debug)]
struct PreparedKey {
    index: usize,
    key: QdrantDataKey,
    physical_id: PhysicalPointId,
}

trait PreparedIdentity {
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

struct FnvHasher(u64);

impl FnvHasher {
    const fn new() -> Self {
        Self(FNV_OFFSET)
    }

    fn write(&mut self, bytes: &[u8]) {
        for byte in bytes {
            self.0 ^= u64::from(*byte);
            self.0 = self.0.wrapping_mul(FNV_PRIME);
        }
    }

    const fn finish(self) -> u64 {
        self.0
    }
}

fn normalize_endpoint(endpoint: &str) -> Result<String, QdrantConfigError> {
    let trimmed = endpoint.trim_end_matches('/');
    if trimmed.is_empty() {
        return Err(QdrantConfigError::EmptyEndpoint);
    }
    if trimmed.chars().any(char::is_whitespace) || trimmed.contains(['?', '#']) {
        return Err(QdrantConfigError::InvalidEndpoint {
            observed: RejectedEndpoint(endpoint.to_owned()),
        });
    }
    if !trimmed.starts_with("http://") && !trimmed.starts_with("https://") {
        return Err(QdrantConfigError::UnsupportedScheme {
            observed: RejectedEndpoint(endpoint.to_owned()),
        });
    }
    if trimmed.ends_with("://") {
        return Err(QdrantConfigError::InvalidEndpoint {
            observed: RejectedEndpoint(endpoint.to_owned()),
        });
    }
    Ok(trimmed.to_owned())
}

fn validate_collection(collection: &str) -> Result<String, QdrantConfigError> {
    if collection.is_empty() {
        return Err(QdrantConfigError::EmptyCollection);
    }
    if !collection
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.'))
    {
        return Err(QdrantConfigError::InvalidCollection {
            observed: RejectedCollectionName(collection.to_owned()),
        });
    }
    Ok(collection.to_owned())
}

fn reject_wrong_authority(
    index: usize,
    expected: VectorAuthority,
    observed: VectorAuthority,
) -> Result<(), QdrantAdmissionError> {
    if observed.snapshot() != expected.snapshot() {
        return Err(QdrantAdmissionError::WrongSnapshot {
            index,
            expected: expected.snapshot(),
            observed: observed.snapshot(),
        });
    }
    if observed.model() != expected.model() {
        return Err(QdrantAdmissionError::WrongModel {
            index,
            expected: expected.model(),
            observed: observed.model(),
        });
    }
    if observed.dimension() != expected.dimension() {
        return Err(QdrantAdmissionError::WrongModelDimension {
            index,
            expected: expected.dimension(),
            observed: observed.dimension(),
        });
    }
    if observed.metric() != expected.metric() {
        return Err(QdrantAdmissionError::WrongMetric {
            index,
            expected: expected.metric(),
            observed: observed.metric(),
        });
    }
    Ok(())
}

fn reject_duplicate_partitions(selected: &[PartitionId]) -> Result<(), QdrantAdmissionError> {
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

fn reject_identity_pair(
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
            key: second,
        });
    }
    if first_physical_id == second_physical_id {
        return Err(QdrantAdmissionError::PhysicalIdCollision {
            physical_id: second_physical_id,
            first_partition: first.partition(),
            first_entity: first.entity(),
            second_partition: second.partition(),
            second_entity: second.entity(),
        });
    }
    Ok(())
}

fn is_success(status: u16) -> bool {
    (200..300).contains(&status)
}

fn retryable_status(status: u16) -> bool {
    status == 408 || status == 425 || status == 429 || status >= 500
}

fn status_error(phase: RequestPhase, response: ResponseEnvelope) -> QdrantError {
    QdrantError::HttpStatus {
        phase,
        attempts: response.attempts,
        status: response.status,
        body: response.body,
    }
}

fn vector_matches(expected: &[i16], observed: &[f64]) -> bool {
    expected.len() == observed.len()
        && expected
            .iter()
            .zip(observed)
            .all(|(expected, observed)| f64::from(*expected) == *observed)
}

fn metric_name(metric: Metric) -> &'static str {
    match metric {
        Metric::SquaredEuclidean => "Euclid",
        Metric::NegativeDotProduct => "Dot",
    }
}

fn metric_payload_name(metric: Metric) -> &'static str {
    match metric {
        Metric::SquaredEuclidean => "squared_euclidean",
        Metric::NegativeDotProduct => "negative_dot_product",
    }
}

fn metric_tag(metric: Metric) -> u8 {
    match metric {
        Metric::SquaredEuclidean => 1,
        Metric::NegativeDotProduct => 2,
    }
}

fn local_score(metric: Metric, query: &[i16], coordinates: &[i16]) -> f64 {
    match metric {
        Metric::SquaredEuclidean => query
            .iter()
            .zip(coordinates)
            .map(|(left, right)| {
                let difference = f64::from(*left) - f64::from(*right);
                difference * difference
            })
            .sum(),
        Metric::NegativeDotProduct => -query
            .iter()
            .zip(coordinates)
            .map(|(left, right)| f64::from(*left) * f64::from(*right))
            .sum::<f64>(),
    }
}

fn projected_score(metric: Metric, query: &[i16], coordinates: &[f64]) -> f64 {
    match metric {
        Metric::SquaredEuclidean => query
            .iter()
            .zip(coordinates)
            .map(|(left, right)| {
                let difference = f64::from(*left) - *right;
                difference * difference
            })
            .sum(),
        Metric::NegativeDotProduct => -query
            .iter()
            .zip(coordinates)
            .map(|(left, right)| f64::from(*left) * *right)
            .sum::<f64>(),
    }
}

fn compare_hits(left: QdrantHit, right: QdrantHit) -> std::cmp::Ordering {
    left.score
        .total_cmp(&right.score)
        .then_with(|| left.entity.raw.cmp(&right.entity.raw))
        .then_with(|| left.partition.raw.cmp(&right.partition.raw))
}

fn local_terminal(
    authority: VectorAuthority,
    selected: &[PartitionId],
    points: &[PreparedPoint<'_>],
) -> QdrantQueryTerminal {
    let mut missing = [None; MAX_QUERY_PARTITIONS];
    let mut missing_len = 0;
    for partition in selected.iter().copied() {
        if !points
            .iter()
            .any(|point| point.key.partition() == partition)
            && let Some(slot) = missing.get_mut(missing_len)
        {
            *slot = Some(partition);
            missing_len += 1;
        }
    }
    QdrantQueryTerminal {
        authority,
        missing,
        missing_len,
    }
}

fn remote_terminal(
    authority: VectorAuthority,
    selected: &[PartitionId],
    hits: &[QdrantHit],
) -> QdrantQueryTerminal {
    let mut missing = [None; MAX_QUERY_PARTITIONS];
    let mut missing_len = 0;
    for partition in selected.iter().copied() {
        if !hits.iter().any(|hit| hit.partition == partition)
            && let Some(slot) = missing.get_mut(missing_len)
        {
            *slot = Some(partition);
            missing_len += 1;
        }
    }
    QdrantQueryTerminal {
        authority,
        missing,
        missing_len,
    }
}

fn hex(bytes: &[u8]) -> String {
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        output.push(hex_digit(*byte >> 4));
        output.push(hex_digit(*byte & 0x0f));
    }
    output
}

fn hex_digit(nibble: u8) -> char {
    match nibble {
        0 => '0',
        1 => '1',
        2 => '2',
        3 => '3',
        4 => '4',
        5 => '5',
        6 => '6',
        7 => '7',
        8 => '8',
        9 => '9',
        10 => 'a',
        11 => 'b',
        12 => 'c',
        13 => 'd',
        14 => 'e',
        15 => 'f',
        _ => '?',
    }
}

fn hex_nibble(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nudox_index_vocab::IndexSnapshotId;
    use nudox_ir_vocab::EntityId;

    fn authority(byte: u8) -> VectorAuthority {
        VectorAuthority::new(
            IndexSnapshotId::from_canonical_bytes(&[byte; 32]),
            ModelId::new([byte.wrapping_add(1); 16]),
            2,
            Metric::SquaredEuclidean,
        )
    }

    #[test]
    fn physical_id_is_deterministic_and_retains_every_identity_axis() {
        let first = QdrantDataKey::new(authority(1), PartitionId::new(2), EntityId::new(3));
        let same = QdrantDataKey::new(authority(1), PartitionId::new(2), EntityId::new(3));
        let snapshot = QdrantDataKey::new(authority(2), PartitionId::new(2), EntityId::new(3));
        let model = QdrantDataKey::new(
            VectorAuthority::new(
                authority(1).snapshot(),
                ModelId::new([9; 16]),
                2,
                Metric::SquaredEuclidean,
            ),
            PartitionId::new(2),
            EntityId::new(3),
        );
        let partition = QdrantDataKey::new(authority(1), PartitionId::new(4), EntityId::new(3));
        let entity = QdrantDataKey::new(authority(1), PartitionId::new(2), EntityId::new(5));
        assert_eq!(
            PhysicalPointId::for_key(first),
            PhysicalPointId::for_key(same)
        );
        assert_ne!(
            PhysicalPointId::for_key(first),
            PhysicalPointId::for_key(snapshot)
        );
        assert_ne!(
            PhysicalPointId::for_key(first),
            PhysicalPointId::for_key(model)
        );
        assert_ne!(
            PhysicalPointId::for_key(first),
            PhysicalPointId::for_key(partition)
        );
        assert_ne!(
            PhysicalPointId::for_key(first),
            PhysicalPointId::for_key(entity)
        );
    }

    #[test]
    fn identity_payload_and_filter_include_full_authority() {
        let key = QdrantDataKey::new(authority(7), PartitionId::new(11), EntityId::new(13));
        let payload = serde_json::to_value(wire::IdentityPayload::from_key(key));
        assert!(payload.is_ok());
        let Ok(payload) = payload else {
            return;
        };
        assert_eq!(
            payload.get(SNAPSHOT_PAYLOAD_KEY),
            Some(&serde_json::Value::from(hex(key.snapshot().as_ref())))
        );
        assert_eq!(
            payload.get(MODEL_PAYLOAD_KEY),
            Some(&serde_json::Value::from(hex(key.model().as_ref())))
        );
        assert_eq!(
            payload.get(METRIC_PAYLOAD_KEY),
            Some(&serde_json::Value::from("squared_euclidean"))
        );
        assert_eq!(
            payload.get(PARTITION_PAYLOAD_KEY),
            Some(&serde_json::Value::from(11_u16))
        );
        assert_eq!(
            payload.get(ENTITY_PAYLOAD_KEY),
            Some(&serde_json::Value::from(13_u32))
        );
        let filter = serde_json::to_value(wire::IdentityFilter::new(
            key.authority(),
            &[key.partition()],
        ));
        assert!(filter.is_ok());
        let Ok(filter) = filter else {
            return;
        };
        let must = filter.get("must").and_then(serde_json::Value::as_array);
        assert!(must.is_some_and(|must| must.len() == 4));
        assert_eq!(
            must.and_then(|must| must.get(3))
                .and_then(|condition| condition.get("key")),
            Some(&serde_json::Value::from(PARTITION_PAYLOAD_KEY))
        );
    }

    #[test]
    fn request_shapes_are_batched_and_idempotent() {
        let coordinates = [1_i16, -2];
        let point = QdrantPoint::new(
            authority(3),
            PartitionId::new(1),
            EntityId::new(5),
            &coordinates,
        );
        let prepared = [PreparedPoint {
            index: 0,
            key: point.key(),
            coordinates: point.coordinates(),
            physical_id: point.physical_id(),
        }];
        let upsert = serde_json::to_value(wire::UpsertRequest::from_points(&prepared));
        assert!(upsert.is_ok());
        let Ok(upsert) = upsert else {
            return;
        };
        assert_eq!(
            upsert
                .get("points")
                .and_then(serde_json::Value::as_array)
                .map(Vec::len),
            Some(1)
        );
        assert_eq!(
            upsert
                .get("points")
                .and_then(serde_json::Value::as_array)
                .and_then(|points| points.first())
                .and_then(|point| point.get("id")),
            Some(&serde_json::Value::from(point.physical_id().raw()))
        );
        let keys = [PreparedKey {
            index: 0,
            key: point.key(),
            physical_id: point.physical_id(),
        }];
        let delete = serde_json::to_value(wire::DeleteRequest::new(&keys));
        assert!(delete.is_ok());
        let Ok(delete) = delete else {
            return;
        };
        assert_eq!(
            delete
                .get("points")
                .and_then(serde_json::Value::as_array)
                .and_then(|points| points.first()),
            Some(&serde_json::Value::from(point.physical_id().raw()))
        );
        let retrieve = serde_json::to_value(wire::RetrieveRequest::new(&keys, true));
        assert!(retrieve.is_ok());
        let Ok(retrieve) = retrieve else {
            return;
        };
        assert_eq!(
            retrieve.get("with_vector"),
            Some(&serde_json::Value::from(true))
        );
        let query = serde_json::to_value(wire::QueryRequest::new(
            authority(3),
            &[PartitionId::new(1)],
            &[0, 0],
        ));
        assert!(query.is_ok());
        let Ok(query) = query else {
            return;
        };
        assert_eq!(
            query.get("limit"),
            Some(&serde_json::Value::from(QUERY_SCAN_LIMIT))
        );
        assert_eq!(
            query.get("with_vector"),
            Some(&serde_json::Value::from(true))
        );
        assert_eq!(
            query.get("params").and_then(|params| params.get("exact")),
            Some(&serde_json::Value::from(true))
        );
    }

    #[test]
    fn replica_lag_requires_all_reads_and_strong_ordered_writes() {
        assert_eq!(READ_POINTS_PATH, "/points?consistency=all");
        assert_eq!(QUERY_POINTS_PATH, "/points/query?consistency=all");
        assert_eq!(UPSERT_POINTS_PATH, "/points?wait=true&ordering=strong");
        assert_eq!(
            DELETE_POINTS_PATH,
            "/points/delete?wait=true&ordering=strong"
        );
        assert_eq!(
            CREATE_PAYLOAD_INDEX_PATH,
            "/index?wait=true&ordering=strong"
        );
        let collection = serde_json::to_value(wire::CollectionRequest::new(authority(3)));
        assert!(collection.is_ok());
        let Ok(collection) = collection else {
            return;
        };
        assert_eq!(
            collection.get("replication_factor"),
            Some(&serde_json::Value::from(1))
        );
        assert_eq!(
            collection.get("write_consistency_factor"),
            Some(&serde_json::Value::from(1))
        );
    }

    #[test]
    fn collision_checker_rejects_same_physical_id_for_distinct_keys() {
        let first = QdrantDataKey::new(authority(1), PartitionId::new(1), EntityId::new(1));
        let second = QdrantDataKey::new(authority(1), PartitionId::new(2), EntityId::new(3));
        let physical_id = PhysicalPointId(42);
        assert_eq!(
            reject_identity_pair(0, first, physical_id, 1, second, physical_id),
            Err(QdrantAdmissionError::PhysicalIdCollision {
                physical_id,
                first_partition: first.partition(),
                first_entity: first.entity(),
                second_partition: second.partition(),
                second_entity: second.entity(),
            })
        );
    }

    #[test]
    fn local_brute_force_orders_ties_by_entity_and_preserves_provenance() {
        let authority = authority(9);
        let adapter = QdrantBlockingAdapter::new("http://127.0.0.1:6337", "unit", authority);
        assert!(adapter.is_ok());
        let Ok(adapter) = adapter else {
            return;
        };
        let first_coordinates = [0_i16, 1];
        let second_coordinates = [0_i16, -1];
        let points = [
            QdrantPoint::new(
                authority,
                PartitionId::new(0),
                EntityId::new(9),
                &first_coordinates,
            ),
            QdrantPoint::new(
                authority,
                PartitionId::new(0),
                EntityId::new(3),
                &second_coordinates,
            ),
        ];
        let mut output = [None, None];
        let outcome = adapter.local_brute_force(&[], &points, &[0, 0], 2, &mut output);
        assert!(outcome.is_ok());
        let Ok(outcome) = outcome else {
            return;
        };
        assert_eq!(outcome.written(), 2);
        assert_eq!(
            output.first().copied().flatten().map(QdrantHit::entity),
            Some(EntityId::new(3))
        );
        assert_eq!(
            output.get(1).copied().flatten().map(QdrantHit::entity),
            Some(EntityId::new(9))
        );
        assert_eq!(
            output.first().copied().flatten().map(QdrantHit::authority),
            Some(authority)
        );
    }

    #[test]
    fn backend_coordinates_reconstruct_portable_metric_scores() {
        let query = [1_i16, -2];
        let local = [4_i16, 2];
        let remote = [4.0_f64, 2.0];
        assert_eq!(
            projected_score(Metric::SquaredEuclidean, &query, &remote),
            local_score(Metric::SquaredEuclidean, &query, &local)
        );
        assert_eq!(
            projected_score(Metric::NegativeDotProduct, &query, &remote),
            local_score(Metric::NegativeDotProduct, &query, &local)
        );
    }

    #[test]
    fn query_rejects_alias_physical_id_and_duplicate_semantic_key() {
        let authority = authority(6);
        let key = QdrantDataKey::new(authority, PartitionId::new(2), EntityId::new(7));
        let canonical_id = PhysicalPointId::for_key(key);
        let alias_id = PhysicalPointId(canonical_id.raw().wrapping_add(1));
        let payload = serde_json::to_value(wire::IdentityPayload::from_key(key));
        assert!(payload.is_ok());
        let Ok(payload) = payload else {
            return;
        };
        let point = |physical_id: PhysicalPointId| {
            serde_json::json!({
                "id": physical_id.raw(),
                "payload": payload,
                "vector": [1.0, 2.0],
            })
        };
        let alias_body = serde_json::json!({
            "result": { "points": [point(alias_id)] },
        })
        .to_string();
        assert!(matches!(
            wire::parse_query_hits(authority, &[key.partition()], &[0, 0], &alias_body),
            Err(QdrantError::PhysicalIdentityMismatch {
                phase: RequestPhase::QueryPoints,
                key: observed_key,
                observed_physical_id,
                expected_physical_id,
            }) if observed_key == key
                && observed_physical_id == alias_id
                && expected_physical_id == canonical_id
        ));

        let duplicate_body = serde_json::json!({
            "result": { "points": [point(canonical_id), point(canonical_id)] },
        })
        .to_string();
        assert!(matches!(
            wire::parse_query_hits(authority, &[key.partition()], &[0, 0], &duplicate_body),
            Err(QdrantError::DuplicateSemanticPoint {
                phase: RequestPhase::QueryPoints,
                key: observed_key,
                first_physical_id,
                second_physical_id,
            }) if observed_key == key
                && first_physical_id == canonical_id
                && second_physical_id == canonical_id
        ));
    }

    #[test]
    fn typed_decoder_rejects_unknown_metric_and_malformed_query_shapes() {
        let authority = authority(12);
        let key = QdrantDataKey::new(authority, PartitionId::new(2), EntityId::new(7));
        let point_id = PhysicalPointId::for_key(key);
        let payload = serde_json::to_value(wire::IdentityPayload::from_key(key));
        assert!(payload.is_ok());
        let Ok(mut invalid_metric) = payload else {
            return;
        };
        let Some(payload) = invalid_metric.as_object_mut() else {
            return;
        };
        payload.insert(
            METRIC_PAYLOAD_KEY.to_owned(),
            serde_json::Value::from("cosine"),
        );
        let unknown_metric = serde_json::json!({
            "result": { "points": [{
                "id": point_id.raw(), "payload": invalid_metric, "vector": [1.0, 2.0],
            }] },
        })
        .to_string();
        assert!(matches!(
            wire::parse_query_hits(authority, &[key.partition()], &[0, 0], &unknown_metric),
            Err(QdrantError::MalformedResponse {
                phase: RequestPhase::QueryPoints,
                cause: MalformedResponseCause::UnknownMetric,
            })
        ));

        let missing_payload = r#"{\"result\":{\"points\":[{\"id\":1,\"vector\":[1.0,2.0]}]}}"#;
        assert!(matches!(
            wire::parse_query_hits(authority, &[], &[0, 0], missing_payload),
            Err(QdrantError::Decode {
                phase: RequestPhase::QueryPoints,
                ..
            })
        ));

        let points_not_array = r#"{\"result\":{\"points\":{}}}"#;
        assert!(matches!(
            wire::parse_query_hits(authority, &[], &[0, 0], points_not_array),
            Err(QdrantError::Decode {
                phase: RequestPhase::QueryPoints,
                ..
            })
        ));
    }

    #[test]
    fn remote_terminal_uses_the_complete_pre_truncation_scan() {
        let authority = authority(8);
        let present = PartitionId::new(2);
        let absent = PartitionId::new(3);
        let key = QdrantDataKey::new(authority, present, EntityId::new(9));
        let hits = [QdrantHit {
            authority,
            partition: present,
            entity: key.entity(),
            score: 4.0,
            physical_id: PhysicalPointId::for_key(key),
        }];
        let terminal = remote_terminal(authority, &[present, absent], &hits);
        assert_eq!(terminal.authority(), authority);
        assert_eq!(terminal.missing_len(), 1);
        assert_eq!(terminal.missing_at(0), Some(absent));
    }
}
