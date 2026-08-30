#![forbid(unsafe_code)]
#![warn(missing_docs)]
//! Blocking Qdrant projection for immutable vector snapshots.
//!
//! The adapter owns only the disposable remote projection.  A point's payload retains the
//! complete snapshot/model/metric/partition/entity authority, while its Qdrant integer ID is a
//! physical coordinate checked against that payload on every write and readback.  Network calls
//! are deliberately synchronous: callers that need an async stream can place this named blocking
//! edge behind their bounded runtime adapter.

use std::{
    num::{NonZeroU8, TryFromIntError},
    time::Duration,
};

use nudox_index_graph_vector::{Metric, ModelId, PartitionId, VectorAuthority, VectorFact};
use nudox_index_vocab::IndexSnapshotId;
use nudox_ir_vocab::EntityId;
use serde_json::{Map, Value};

const DEFAULT_MAX_ATTEMPTS: NonZeroU8 = match NonZeroU8::new(3) {
    Some(value) => value,
    None => NonZeroU8::MIN,
};
const MAX_BATCH_POINTS: usize = 256;
const MAX_QUERY_PARTITIONS: usize = 4;
const MAX_VECTOR_DIMENSION: usize = 4096;
const MAX_RESPONSE_BYTES: u64 = 1_048_576;
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

/// A bounded retry policy for idempotent Qdrant requests.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RetryPolicy {
    attempts: NonZeroU8,
}

impl RetryPolicy {
    /// Returns the default three-attempt policy.
    #[must_use]
    pub const fn default_policy() -> Self {
        Self {
            attempts: DEFAULT_MAX_ATTEMPTS,
        }
    }

    /// Builds a policy with an explicit positive attempt bound.
    pub const fn new(attempts: NonZeroU8) -> Self {
        Self { attempts }
    }

    /// Returns the complete attempt bound.
    #[must_use]
    pub const fn attempts(self) -> NonZeroU8 {
        self.attempts
    }
}

/// A disposable physical integer coordinate in one Qdrant collection.
#[repr(transparent)]
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct PhysicalPointId(u64);

impl PhysicalPointId {
    /// Returns the integer sent to Qdrant.
    #[must_use]
    pub const fn raw(self) -> u64 {
        self.0
    }

    /// Derives a deterministic physical coordinate from complete semantic authority.
    #[must_use]
    pub fn for_key(key: QdrantDataKey) -> Self {
        let mut hasher = FnvHasher::new();
        hasher.write(key.snapshot().as_ref());
        hasher.write(key.model().as_ref());
        hasher.write(&key.dimension().to_be_bytes());
        hasher.write(&[metric_tag(key.metric())]);
        hasher.write(&key.partition.raw.to_be_bytes());
        hasher.write(&key.entity.raw.to_be_bytes());
        let raw = hasher.finish();
        Self(if raw == 0 {
            PHYSICAL_ID_ZERO_REPLACEMENT
        } else {
            raw
        })
    }
}

/// The full immutable identity carried by each projected point and delete key.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct QdrantDataKey {
    authority: VectorAuthority,
    partition: PartitionId,
    entity: EntityId,
}

impl QdrantDataKey {
    /// Binds one semantic entity to the complete vector authority.
    #[must_use]
    pub const fn new(authority: VectorAuthority, partition: PartitionId, entity: EntityId) -> Self {
        Self {
            authority,
            partition,
            entity,
        }
    }

    /// Returns snapshot, model, dimension, and metric authority.
    #[must_use]
    pub const fn authority(self) -> VectorAuthority {
        self.authority
    }

    /// Returns the immutable snapshot identity.
    #[must_use]
    pub const fn snapshot(self) -> IndexSnapshotId {
        self.authority.snapshot()
    }

    /// Returns the embedding model identity.
    #[must_use]
    pub const fn model(self) -> ModelId {
        self.authority.model()
    }

    /// Returns the model dimension.
    #[must_use]
    pub const fn dimension(self) -> u16 {
        self.authority.dimension()
    }

    /// Returns the distance metric.
    #[must_use]
    pub const fn metric(self) -> Metric {
        self.authority.metric()
    }

    /// Returns the immutable projection partition.
    #[must_use]
    pub const fn partition(self) -> PartitionId {
        self.partition
    }

    /// Returns the semantic entity coordinate.
    #[must_use]
    pub const fn entity(self) -> EntityId {
        self.entity
    }
}

/// One borrowed vector row admitted for remote projection or local comparison.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct QdrantPoint<'coordinates> {
    key: QdrantDataKey,
    coordinates: &'coordinates [i16],
}

impl<'coordinates> QdrantPoint<'coordinates> {
    /// Creates a borrowed point without performing remote I/O.
    #[must_use]
    pub const fn new(
        authority: VectorAuthority,
        partition: PartitionId,
        entity: EntityId,
        coordinates: &'coordinates [i16],
    ) -> Self {
        Self {
            key: QdrantDataKey::new(authority, partition, entity),
            coordinates,
        }
    }

    /// Returns the complete semantic key.
    #[must_use]
    pub const fn key(self) -> QdrantDataKey {
        self.key
    }

    /// Returns the borrowed coordinates.
    #[must_use]
    pub const fn coordinates(self) -> &'coordinates [i16] {
        self.coordinates
    }

    /// Projects this borrowed point into the portable graph/vector fact vocabulary.
    #[must_use]
    pub const fn as_vector_fact(self) -> VectorFact<'coordinates> {
        VectorFact::new(
            self.key.authority(),
            self.key.partition(),
            self.key.entity(),
            self.coordinates,
        )
    }

    /// Returns this point's disposable physical coordinate.
    #[must_use]
    pub fn physical_id(self) -> PhysicalPointId {
        PhysicalPointId::for_key(self.key)
    }
}

/// The deterministic score and complete provenance returned by a remote query.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct QdrantHit {
    authority: VectorAuthority,
    partition: PartitionId,
    entity: EntityId,
    score: f64,
    physical_id: PhysicalPointId,
}

impl QdrantHit {
    /// Returns snapshot, model, dimension, and metric authority.
    #[must_use]
    pub const fn authority(self) -> VectorAuthority {
        self.authority
    }

    /// Returns the source partition.
    #[must_use]
    pub const fn partition(self) -> PartitionId {
        self.partition
    }

    /// Returns the semantic entity coordinate.
    #[must_use]
    pub const fn entity(self) -> EntityId {
        self.entity
    }

    /// Returns Qdrant's metric-specific score.
    #[must_use]
    pub const fn score(self) -> f64 {
        self.score
    }

    /// Returns the disposable physical coordinate observed in the response.
    #[must_use]
    pub const fn physical_id(self) -> PhysicalPointId {
        self.physical_id
    }
}

/// A remote query terminal retaining exact absence facts for selected partitions.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct QdrantQueryTerminal {
    authority: VectorAuthority,
    missing: [Option<PartitionId>; MAX_QUERY_PARTITIONS],
    missing_len: usize,
}

impl QdrantQueryTerminal {
    /// Returns the complete vector authority.
    #[must_use]
    pub const fn authority(self) -> VectorAuthority {
        self.authority
    }

    /// Returns the number of absent selected partitions.
    #[must_use]
    pub const fn missing_len(self) -> usize {
        self.missing_len
    }

    /// Returns one absent selected partition by semantic position.
    #[must_use]
    pub const fn missing_at(self, index: usize) -> Option<PartitionId> {
        if index < self.missing_len {
            self.missing[index]
        } else {
            None
        }
    }

    /// Returns whether the query had no absent selected partition.
    #[must_use]
    pub const fn is_complete(self) -> bool {
        self.missing_len == 0
    }
}

/// A bounded remote query result and its typed terminal.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct QdrantQueryOutcome {
    written: usize,
    terminal: QdrantQueryTerminal,
}

impl QdrantQueryOutcome {
    /// Returns the number of initialized output slots.
    #[must_use]
    pub const fn written(self) -> usize {
        self.written
    }

    /// Returns the complete/partial terminal.
    #[must_use]
    pub const fn terminal(self) -> QdrantQueryTerminal {
        self.terminal
    }
}

/// A successful mutation receipt after the remote operation and readback verification.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct QdrantMutationReceipt {
    attempted: usize,
    verified: usize,
}

impl QdrantMutationReceipt {
    /// Returns the number of submitted keys.
    #[must_use]
    pub const fn attempted(self) -> usize {
        self.attempted
    }

    /// Returns the number independently verified after the operation.
    #[must_use]
    pub const fn verified(self) -> usize {
        self.verified
    }
}

/// A verified remote point readback, retaining its identity and coordinates.
#[derive(Clone, Debug, PartialEq)]
pub struct QdrantReadback {
    key: QdrantDataKey,
    physical_id: PhysicalPointId,
    coordinates: Vec<f64>,
}

impl QdrantReadback {
    /// Returns the complete semantic key.
    #[must_use]
    pub const fn key(&self) -> QdrantDataKey {
        self.key
    }

    /// Returns the disposable physical coordinate.
    #[must_use]
    pub const fn physical_id(&self) -> PhysicalPointId {
        self.physical_id
    }

    /// Returns the remotely stored coordinates.
    #[must_use]
    pub fn coordinates(&self) -> &[f64] {
        &self.coordinates
    }
}

/// Exact request phase retained by transport, status, and decode failures.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RequestPhase {
    /// Collection creation or collision handling.
    CreateCollection,
    /// Collection metadata readback.
    ReadCollection,
    /// Existing point identity preflight.
    ReadPoints,
    /// Batched point upload.
    UpsertPoints,
    /// Batched vector query.
    QueryPoints,
    /// Partition presence probe.
    ScrollPartition,
    /// Batched point deletion.
    DeletePoints,
    /// Delete readback verification.
    VerifyDelete,
    /// Collection deletion.
    DeleteCollection,
}

/// Adapter construction failure before any remote effect.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum QdrantConfigError {
    /// Endpoint is empty.
    #[error("Qdrant endpoint is empty")]
    EmptyEndpoint,
    /// Endpoint has no supported HTTP scheme.
    #[error("Qdrant endpoint must use http or https: {observed}")]
    UnsupportedScheme {
        /// Complete rejected endpoint.
        observed: String,
    },
    /// Endpoint contains a URI character that would make path joining ambiguous.
    #[error("Qdrant endpoint contains whitespace or a query/fragment: {observed}")]
    InvalidEndpoint {
        /// Complete rejected endpoint.
        observed: String,
    },
    /// Collection name is empty.
    #[error("Qdrant collection name is empty")]
    EmptyCollection,
    /// Collection name contains a path/control character.
    #[error("Qdrant collection name is not a single safe path component: {observed}")]
    InvalidCollection {
        /// Complete rejected collection name.
        observed: String,
    },
    /// Model dimension is outside the bounded request shape.
    #[error("Qdrant model dimension {observed} is outside 1..={maximum}")]
    InvalidDimension {
        /// Maximum supported coordinate count.
        maximum: usize,
        /// Rejected coordinate count.
        observed: usize,
    },
}

/// Local admission or remote identity validation failure.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum QdrantAdmissionError {
    /// A batch exceeds the bounded request shape.
    #[error("Qdrant batch has {observed} items; maximum is {maximum}")]
    BatchTooLarge {
        /// Maximum admitted item count.
        maximum: usize,
        /// Complete observed item count.
        observed: usize,
    },
    /// A point belongs to another immutable snapshot.
    #[error("point {index} belongs to snapshot {observed:?}; expected {expected:?}")]
    WrongSnapshot {
        /// Rejected point position.
        index: usize,
        /// Adapter snapshot.
        expected: IndexSnapshotId,
        /// Rejected point snapshot.
        observed: IndexSnapshotId,
    },
    /// A point belongs to another embedding model.
    #[error("point {index} belongs to model {observed:?}; expected {expected:?}")]
    WrongModel {
        /// Rejected point position.
        index: usize,
        /// Adapter model.
        expected: ModelId,
        /// Rejected point model.
        observed: ModelId,
    },
    /// A point belongs to another model dimension.
    #[error("point {index} has model dimension {observed}; expected {expected}")]
    WrongModelDimension {
        /// Rejected point position.
        index: usize,
        /// Adapter dimension.
        expected: u16,
        /// Rejected point dimension.
        observed: u16,
    },
    /// A point belongs to another metric authority.
    #[error("point {index} has metric {observed:?}; expected {expected:?}")]
    WrongMetric {
        /// Rejected point position.
        index: usize,
        /// Adapter metric.
        expected: Metric,
        /// Rejected point metric.
        observed: Metric,
    },
    /// A point's coordinates do not match its model dimension.
    #[error("point {index} has {observed} coordinates; expected {expected}")]
    WrongDimension {
        /// Rejected point position.
        index: usize,
        /// Required coordinate count.
        expected: usize,
        /// Complete observed coordinate count.
        observed: usize,
    },
    /// A query coordinate slice does not match the pinned model dimension.
    #[error("query has {observed} coordinates; expected {expected}")]
    WrongQueryDimension {
        /// Required coordinate count.
        expected: usize,
        /// Complete observed coordinate count.
        observed: usize,
    },
    /// The selected query output is too short and remains untouched.
    #[error("query output has {available} slots; requested {required}")]
    InsufficientOutput {
        /// Requested output count.
        required: usize,
        /// Caller-provided output capacity.
        available: usize,
    },
    /// Too many selected partitions were admitted.
    #[error("selected partition fanout has {observed}; maximum is {maximum}")]
    TooManyPartitions {
        /// Maximum selected partition fanout.
        maximum: usize,
        /// Complete observed fanout.
        observed: usize,
    },
    /// A selected partition was repeated.
    #[error("selected partition {partition:?} appears at {first_index} and {index}")]
    DuplicatePartition {
        /// First occurrence position.
        first_index: usize,
        /// Later occurrence position.
        index: usize,
        /// Repeated partition coordinate.
        partition: PartitionId,
    },
    /// A batch key was repeated.
    #[error("batch key {key:?} appears at {first_index} and {index}")]
    DuplicateKey {
        /// First occurrence position.
        first_index: usize,
        /// Later occurrence position.
        index: usize,
        /// Repeated full key.
        key: QdrantDataKey,
    },
    /// Two distinct semantic keys mapped to one disposable point ID.
    #[error(
        "physical point id {physical_id:?} collides for ({first_partition:?}, {first_entity:?}) and ({second_partition:?}, {second_entity:?})"
    )]
    PhysicalIdCollision {
        /// Colliding disposable physical coordinate.
        physical_id: PhysicalPointId,
        /// First partition coordinate under the already-validated adapter authority.
        first_partition: PartitionId,
        /// First entity coordinate under the already-validated adapter authority.
        first_entity: EntityId,
        /// Later partition coordinate under the same authority.
        second_partition: PartitionId,
        /// Later entity coordinate under the same authority.
        second_entity: EntityId,
    },
    /// A remote point ID was reused for a different full payload identity.
    #[error("remote point {physical_id:?} has a different payload identity for {key:?}")]
    RemoteIdentityMismatch {
        /// Reused disposable physical coordinate.
        physical_id: PhysicalPointId,
        /// Expected full semantic key.
        key: QdrantDataKey,
    },
}

/// Failure while a concrete Qdrant request was encoded, sent, or decoded.
#[derive(Debug, thiserror::Error)]
pub enum QdrantError {
    /// Construction failed before I/O.
    #[error("Qdrant adapter configuration failed")]
    Configuration(#[from] QdrantConfigError),
    /// Admission or physical identity failed before I/O.
    #[error("Qdrant request admission failed")]
    Admission(#[from] QdrantAdmissionError),
    /// Request JSON could not be encoded.
    #[error("Qdrant {phase:?} request encoding failed")]
    Encode {
        /// Operation phase.
        phase: RequestPhase,
        /// Original JSON source.
        #[source]
        source: serde_json::Error,
    },
    /// The service returned a non-success status after bounded retry.
    #[error("Qdrant {phase:?} returned HTTP {status} after {attempts} attempt(s)")]
    HttpStatus {
        /// Operation phase.
        phase: RequestPhase,
        /// Number of attempts made.
        attempts: u8,
        /// HTTP status code.
        status: u16,
        /// Bounded response body for diagnosis.
        body: String,
    },
    /// The transport failed after bounded retry.
    #[error("Qdrant {phase:?} transport failed after {attempts} attempt(s)")]
    Transport {
        /// Operation phase.
        phase: RequestPhase,
        /// Number of attempts made.
        attempts: u8,
        /// Original transport source from the final attempt.
        #[source]
        source: ureq::Error,
    },
    /// A successful HTTP body was not valid JSON.
    #[error("Qdrant {phase:?} response decode failed")]
    Decode {
        /// Operation phase.
        phase: RequestPhase,
        /// Original JSON source.
        #[source]
        source: serde_json::Error,
    },
    /// The response was JSON but did not contain the required shape.
    #[error("Qdrant {phase:?} response is malformed: {detail}")]
    MalformedResponse {
        /// Operation phase.
        phase: RequestPhase,
        /// Stable shape detail.
        detail: &'static str,
    },
    /// The service returned a point that was not one of the requested identities.
    #[error("Qdrant {phase:?} returned an unrequested point {physical_id:?}")]
    UnexpectedPoint {
        /// Operation phase.
        phase: RequestPhase,
        /// Unexpected physical coordinate.
        physical_id: PhysicalPointId,
    },
    /// The response omitted a point required for verification.
    #[error("Qdrant {phase:?} omitted point {physical_id:?} for {key:?}")]
    MissingPoint {
        /// Operation phase.
        phase: RequestPhase,
        /// Missing physical coordinate.
        physical_id: PhysicalPointId,
        /// Expected semantic key.
        key: QdrantDataKey,
    },
    /// The service returned a payload that disagreed with the requested identity.
    #[error("Qdrant {phase:?} payload identity disagrees for point {physical_id:?}: {field}")]
    PayloadMismatch {
        /// Operation phase.
        phase: RequestPhase,
        /// Physical point coordinate.
        physical_id: PhysicalPointId,
        /// Mismatched payload field.
        field: &'static str,
    },
    /// Collection metadata disagreed with the pinned model shape or metric.
    #[error("Qdrant {phase:?} collection metadata disagrees in {field}")]
    CollectionMismatch {
        /// Operation phase.
        phase: RequestPhase,
        /// Mismatched collection field.
        field: &'static str,
    },
    /// A remote vector was malformed or differed from the uploaded coordinates.
    #[error("Qdrant {phase:?} vector differs for point {physical_id:?}")]
    VectorMismatch {
        /// Operation phase.
        phase: RequestPhase,
        /// Physical point coordinate.
        physical_id: PhysicalPointId,
    },
    /// A numeric response field exceeded its semantic coordinate range.
    #[error("Qdrant {phase:?} field {field} is outside its semantic range")]
    InvalidFieldRange {
        /// Operation phase.
        phase: RequestPhase,
        /// Field whose range was invalid.
        field: &'static str,
    },
    /// Snapshot bytes had the right width but failed the typed domain conversion.
    #[error("Qdrant {phase:?} snapshot payload conversion failed")]
    SnapshotDecode {
        /// Operation phase.
        phase: RequestPhase,
        /// Original typed snapshot conversion failure.
        #[source]
        source: <IndexSnapshotId as TryFrom<[u8; 32]>>::Error,
    },
    /// A fixed-width model payload could not be represented as its typed byte array.
    #[error("Qdrant {phase:?} model payload conversion failed")]
    ModelDecode {
        /// Operation phase.
        phase: RequestPhase,
        /// Original fixed-width conversion failure.
        #[source]
        source: core::array::TryFromSliceError,
    },
    /// An integer payload field could not fit its typed coordinate owner.
    #[error("Qdrant {phase:?} field {field} value {observed} is outside its semantic range")]
    IntegerRange {
        /// Operation phase.
        phase: RequestPhase,
        /// Payload field whose integer conversion failed.
        field: &'static str,
        /// Complete observed unsigned value.
        observed: u64,
        /// Original integer conversion failure.
        #[source]
        source: TryFromIntError,
    },
}

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
        let body = collection_request(self.authority);
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
                    collection_request(self.authority),
                )?;
                if !is_success(create.status) && create.status != 409 {
                    return Err(status_error(RequestPhase::CreateCollection, create));
                }
            }
        } else if !is_success(response.status) {
            return Err(status_error(RequestPhase::CreateCollection, response));
        }
        self.verify_collection()
    }

    /// Deletes the caller-selected collection and verifies a successful service response.
    pub fn delete_collection(&self) -> Result<(), QdrantError> {
        let response = self.request_json(
            RequestPhase::DeleteCollection,
            Method::Delete,
            &self.url(""),
            Value::Null,
        )?;
        if !is_success(response.status) {
            return Err(status_error(RequestPhase::DeleteCollection, response));
        }
        parse_ack(RequestPhase::DeleteCollection, &response.body)
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
        let request = upsert_request(&prepared);
        let response = self.request_json(
            RequestPhase::UpsertPoints,
            Method::Put,
            &self.url("/points?wait=true"),
            request,
        )?;
        if !is_success(response.status) {
            return Err(status_error(RequestPhase::UpsertPoints, response));
        }
        parse_ack(RequestPhase::UpsertPoints, &response.body)?;
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
                    detail: "readback output index",
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
            &self.url("/points/delete?wait=true"),
            delete_request(&prepared),
        )?;
        if !is_success(response.status) {
            return Err(status_error(RequestPhase::DeletePoints, response));
        }
        parse_ack(RequestPhase::DeletePoints, &response.body)?;
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
            &self.url("/points/query"),
            query_request(self.authority, selected, query_coordinates, requested_limit),
        )?;
        if !is_success(response.status) {
            return Err(status_error(RequestPhase::QueryPoints, response));
        }
        let mut hits = parse_query_hits(self.authority, selected, &response.body)?;
        hits.sort_by(|left, right| compare_hits(*left, *right, self.authority.metric()));
        hits.truncate(requested_limit);
        let written = hits.len();
        let terminal = self.query_terminal(selected)?;
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
        hits.sort_by(|left, right| compare_hits(*left, *right, self.authority.metric()));
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
        let response = self.request_json(
            RequestPhase::ReadCollection,
            Method::Get,
            &self.url(""),
            Value::Null,
        )?;
        if !is_success(response.status) {
            return Err(status_error(RequestPhase::ReadCollection, response));
        }
        let value = decode_json(RequestPhase::ReadCollection, &response.body)?;
        let vectors = value
            .get("result")
            .and_then(|result| result.get("config"))
            .and_then(|config| config.get("params"))
            .and_then(|params| params.get("vectors"))
            .ok_or(QdrantError::MalformedResponse {
                phase: RequestPhase::ReadCollection,
                detail: "result.config.params.vectors",
            })?;
        let size =
            vectors
                .get("size")
                .and_then(Value::as_u64)
                .ok_or(QdrantError::MalformedResponse {
                    phase: RequestPhase::ReadCollection,
                    detail: "vectors.size",
                })?;
        let distance = vectors.get("distance").and_then(Value::as_str).ok_or(
            QdrantError::MalformedResponse {
                phase: RequestPhase::ReadCollection,
                detail: "vectors.distance",
            },
        )?;
        let expected_size = u64::from(self.authority.dimension());
        if size != expected_size {
            return Err(QdrantError::CollectionMismatch {
                phase: RequestPhase::ReadCollection,
                field: "vectors.size",
            });
        }
        if distance != metric_name(self.authority.metric()) {
            return Err(QdrantError::CollectionMismatch {
                phase: RequestPhase::ReadCollection,
                field: "vectors.distance",
            });
        }
        Ok(())
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
            &self.url("/points"),
            retrieve_request(keys, require_vectors),
        )?;
        if !is_success(response.status) {
            return Err(status_error(phase, response));
        }
        let value = decode_json(phase, &response.body)?;
        let points = value.get("result").and_then(Value::as_array).ok_or(
            QdrantError::MalformedResponse {
                phase,
                detail: "result[]",
            },
        )?;
        if points.len() > MAX_BATCH_POINTS {
            return Err(QdrantError::MalformedResponse {
                phase,
                detail: "result[] exceeds bounded batch",
            });
        }
        let mut readbacks = Vec::with_capacity(points.len());
        for point in points {
            let physical_id = point_id(point, phase)?;
            if readbacks
                .iter()
                .any(|readback: &QdrantReadback| readback.physical_id == physical_id)
            {
                return Err(QdrantError::MalformedResponse {
                    phase,
                    detail: "duplicate result[].id",
                });
            }
            let Some(expected) = keys
                .iter()
                .find(|key| key.physical_id().raw() == physical_id.raw())
            else {
                return Err(QdrantError::UnexpectedPoint { phase, physical_id });
            };
            let key = decode_identity(point, phase, self.authority)?;
            if key != expected.key() {
                return Err(QdrantError::PayloadMismatch {
                    phase,
                    physical_id,
                    field: SNAPSHOT_PAYLOAD_KEY,
                });
            }
            let coordinates = if require_vectors {
                decode_vector(
                    point,
                    phase,
                    physical_id,
                    usize::from(self.authority.dimension()),
                )?
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
            &self.url("/points"),
            retrieve_request(keys, false),
        )?;
        if !is_success(response.status) {
            return Err(status_error(phase, response));
        }
        let value = decode_json(phase, &response.body)?;
        let points = value.get("result").and_then(Value::as_array).ok_or(
            QdrantError::MalformedResponse {
                phase,
                detail: "result[]",
            },
        )?;
        if points.len() > MAX_BATCH_POINTS {
            return Err(QdrantError::MalformedResponse {
                phase,
                detail: "result[] exceeds bounded batch",
            });
        }
        let mut readbacks = Vec::with_capacity(points.len());
        for point in points {
            let physical_id = point_id(point, phase)?;
            let key = decode_identity(point, phase, self.authority)?;
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
                    detail: "duplicate result[].id",
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
                    detail: "readback output index",
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

    fn query_terminal(&self, selected: &[PartitionId]) -> Result<QdrantQueryTerminal, QdrantError> {
        let mut missing = [None; MAX_QUERY_PARTITIONS];
        let mut missing_len = 0;
        for partition in selected.iter().copied() {
            let response = self.request_json(
                RequestPhase::ScrollPartition,
                Method::Post,
                &self.url("/points/scroll"),
                scroll_request(self.authority, partition),
            )?;
            if !is_success(response.status) {
                return Err(status_error(RequestPhase::ScrollPartition, response));
            }
            let value = decode_json(RequestPhase::ScrollPartition, &response.body)?;
            let points = value
                .get("result")
                .and_then(|result| result.get("points"))
                .and_then(Value::as_array)
                .ok_or(QdrantError::MalformedResponse {
                    phase: RequestPhase::ScrollPartition,
                    detail: "result.points[]",
                })?;
            if points.len() > 1 {
                return Err(QdrantError::MalformedResponse {
                    phase: RequestPhase::ScrollPartition,
                    detail: "result.points[] exceeds presence bound",
                });
            }
            if points.is_empty() {
                let Some(slot) = missing.get_mut(missing_len) else {
                    return Err(QdrantError::MalformedResponse {
                        phase: RequestPhase::ScrollPartition,
                        detail: "missing partition capacity",
                    });
                };
                *slot = Some(partition);
                missing_len += 1;
            } else {
                let Some(point) = points.first() else {
                    return Err(QdrantError::MalformedResponse {
                        phase: RequestPhase::ScrollPartition,
                        detail: "result.points[0]",
                    });
                };
                let physical_id = point_id(point, RequestPhase::ScrollPartition)?;
                let key = decode_identity(point, RequestPhase::ScrollPartition, self.authority)?;
                if key.authority() != self.authority || key.partition() != partition {
                    return Err(QdrantError::PayloadMismatch {
                        phase: RequestPhase::ScrollPartition,
                        physical_id,
                        field: PARTITION_PAYLOAD_KEY,
                    });
                }
            }
        }
        Ok(QdrantQueryTerminal {
            authority: self.authority,
            missing,
            missing_len,
        })
    }

    fn url(&self, suffix: &str) -> String {
        format!(
            "{}/collections/{}{}",
            self.endpoint, self.collection, suffix
        )
    }

    fn request_json(
        &self,
        phase: RequestPhase,
        method: Method,
        url: &str,
        body: Value,
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
                detail: "retry loop terminated without response",
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
            observed: endpoint.to_owned(),
        });
    }
    if !trimmed.starts_with("http://") && !trimmed.starts_with("https://") {
        return Err(QdrantConfigError::UnsupportedScheme {
            observed: endpoint.to_owned(),
        });
    }
    if trimmed.ends_with("://") {
        return Err(QdrantConfigError::InvalidEndpoint {
            observed: endpoint.to_owned(),
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
            observed: collection.to_owned(),
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

fn parse_ack(phase: RequestPhase, body: &str) -> Result<(), QdrantError> {
    let value = decode_json(phase, body)?;
    if value.get("result").is_none() {
        return Err(QdrantError::MalformedResponse {
            phase,
            detail: "result",
        });
    }
    Ok(())
}

fn decode_json(phase: RequestPhase, body: &str) -> Result<Value, QdrantError> {
    serde_json::from_str(body).map_err(|source| QdrantError::Decode { phase, source })
}

fn collection_request(authority: VectorAuthority) -> Value {
    serde_json::json!({
        "vectors": {
            "size": authority.dimension(),
            "distance": metric_name(authority.metric()),
        },
    })
}

fn upsert_request(points: &[PreparedPoint<'_>]) -> Value {
    let values = points
        .iter()
        .map(|point| {
            serde_json::json!({
                "id": point.physical_id.raw(),
                "vector": point.coordinates.iter().map(|coordinate| f64::from(*coordinate)).collect::<Vec<_>>(),
                "payload": identity_payload(point.key),
            })
        })
        .collect::<Vec<_>>();
    serde_json::json!({ "points": values })
}

fn retrieve_request(keys: &[impl PreparedIdentity], require_vectors: bool) -> Value {
    let ids = keys
        .iter()
        .map(|key| Value::from(key.physical_id().raw()))
        .collect::<Vec<_>>();
    serde_json::json!({
        "ids": ids,
        "with_payload": true,
        "with_vector": require_vectors,
    })
}

fn delete_request(keys: &[impl PreparedIdentity]) -> Value {
    let ids = keys
        .iter()
        .map(|key| Value::from(key.physical_id().raw()))
        .collect::<Vec<_>>();
    serde_json::json!({ "points": ids })
}

fn query_request(
    authority: VectorAuthority,
    selected: &[PartitionId],
    coordinates: &[i16],
    requested_limit: usize,
) -> Value {
    serde_json::json!({
        "query": coordinates.iter().map(|coordinate| f64::from(*coordinate)).collect::<Vec<_>>(),
        "limit": requested_limit,
        "with_payload": true,
        "with_vector": false,
        "filter": identity_filter(authority, selected),
    })
}

fn scroll_request(authority: VectorAuthority, partition: PartitionId) -> Value {
    serde_json::json!({
        "limit": 1,
        "with_payload": true,
        "with_vector": false,
        "filter": identity_filter(authority, &[partition]),
    })
}

fn identity_payload(key: QdrantDataKey) -> Value {
    serde_json::json!({
        SNAPSHOT_PAYLOAD_KEY: hex(key.snapshot().as_ref()),
        MODEL_PAYLOAD_KEY: hex(key.model().as_ref()),
        METRIC_PAYLOAD_KEY: metric_payload_name(key.metric()),
        PARTITION_PAYLOAD_KEY: key.partition.raw,
        ENTITY_PAYLOAD_KEY: key.entity.raw,
    })
}

fn identity_filter(authority: VectorAuthority, selected: &[PartitionId]) -> Value {
    let mut must = vec![
        serde_json::json!({ "key": SNAPSHOT_PAYLOAD_KEY, "match": { "value": hex(authority.snapshot().as_ref()) } }),
        serde_json::json!({ "key": MODEL_PAYLOAD_KEY, "match": { "value": hex(authority.model().as_ref()) } }),
        serde_json::json!({ "key": METRIC_PAYLOAD_KEY, "match": { "value": metric_payload_name(authority.metric()) } }),
    ];
    let mut filter = Map::new();
    if let [partition] = selected {
        must.push(serde_json::json!({
            "key": PARTITION_PAYLOAD_KEY,
            "match": { "value": partition.raw },
        }));
    } else if selected.len() > 1 {
        filter.insert(
            "min_should".to_owned(),
            serde_json::json!({
                "conditions":
                selected
                    .iter()
                    .map(|partition| {
                        serde_json::json!({
                            "key": PARTITION_PAYLOAD_KEY,
                            "match": { "value": partition.raw },
                        })
                    })
                    .collect::<Vec<_>>(),
                "min_count": 1,
            }),
        );
    }
    filter.insert("must".to_owned(), Value::Array(must));
    Value::Object(filter)
}

fn parse_query_hits(
    authority: VectorAuthority,
    selected: &[PartitionId],
    body: &str,
) -> Result<Vec<QdrantHit>, QdrantError> {
    let phase = RequestPhase::QueryPoints;
    let value = decode_json(phase, body)?;
    let points = value
        .get("result")
        .and_then(|result| result.get("points"))
        .and_then(Value::as_array)
        .ok_or(QdrantError::MalformedResponse {
            phase,
            detail: "result.points[]",
        })?;
    if points.len() > MAX_BATCH_POINTS {
        return Err(QdrantError::MalformedResponse {
            phase,
            detail: "result.points[] exceeds bounded query",
        });
    }
    let mut hits = Vec::with_capacity(points.len());
    for point in points {
        let physical_id = point_id(point, phase)?;
        if hits
            .iter()
            .any(|hit: &QdrantHit| hit.physical_id == physical_id)
        {
            return Err(QdrantError::MalformedResponse {
                phase,
                detail: "duplicate result.points[].id",
            });
        }
        let key = decode_identity(point, phase, authority)?;
        if key.authority() != authority {
            return Err(QdrantError::PayloadMismatch {
                phase,
                physical_id,
                field: SNAPSHOT_PAYLOAD_KEY,
            });
        }
        if !selected.is_empty() && !selected.contains(&key.partition()) {
            return Err(QdrantError::PayloadMismatch {
                phase,
                physical_id,
                field: PARTITION_PAYLOAD_KEY,
            });
        }
        let score =
            point
                .get("score")
                .and_then(Value::as_f64)
                .ok_or(QdrantError::MalformedResponse {
                    phase,
                    detail: "result.points[].score",
                })?;
        hits.push(QdrantHit {
            authority,
            partition: key.partition(),
            entity: key.entity(),
            score,
            physical_id,
        });
    }
    Ok(hits)
}

fn point_id(point: &Value, phase: RequestPhase) -> Result<PhysicalPointId, QdrantError> {
    let raw = point
        .get("id")
        .and_then(Value::as_u64)
        .ok_or(QdrantError::MalformedResponse {
            phase,
            detail: "result[].id as u64",
        })?;
    Ok(PhysicalPointId(raw))
}

fn decode_identity(
    point: &Value,
    phase: RequestPhase,
    expected_authority: VectorAuthority,
) -> Result<QdrantDataKey, QdrantError> {
    let payload =
        point
            .get("payload")
            .and_then(Value::as_object)
            .ok_or(QdrantError::MalformedResponse {
                phase,
                detail: "result[].payload",
            })?;
    let snapshot = decode_snapshot(payload, phase)?;
    let model = decode_model(payload, phase)?;
    let metric = decode_metric(payload, phase)?;
    let partition = decode_partition(payload, phase)?;
    let entity = decode_entity(payload, phase)?;
    let key = QdrantDataKey::new(
        VectorAuthority::new(snapshot, model, expected_authority.dimension(), metric),
        partition,
        entity,
    );
    if key.authority().dimension() != expected_authority.dimension() {
        return Err(QdrantError::PayloadMismatch {
            phase,
            physical_id: PhysicalPointId(0),
            field: MODEL_PAYLOAD_KEY,
        });
    }
    Ok(key)
}

fn decode_snapshot(
    payload: &Map<String, Value>,
    phase: RequestPhase,
) -> Result<IndexSnapshotId, QdrantError> {
    let bytes = decode_hex_field(payload, SNAPSHOT_PAYLOAD_KEY, 32, phase)?;
    match IndexSnapshotId::try_from(bytes.as_slice()) {
        Ok(snapshot) => Ok(snapshot),
        Err(source) => Err(QdrantError::SnapshotDecode { phase, source }),
    }
}

fn decode_model(payload: &Map<String, Value>, phase: RequestPhase) -> Result<ModelId, QdrantError> {
    let bytes = decode_hex_field(payload, MODEL_PAYLOAD_KEY, 16, phase)?;
    let bytes: [u8; 16] = match bytes.as_slice().try_into() {
        Ok(bytes) => bytes,
        Err(source) => return Err(QdrantError::ModelDecode { phase, source }),
    };
    Ok(ModelId::new(bytes))
}

fn decode_metric(payload: &Map<String, Value>, phase: RequestPhase) -> Result<Metric, QdrantError> {
    match payload
        .get(METRIC_PAYLOAD_KEY)
        .and_then(Value::as_str)
        .ok_or(QdrantError::MalformedResponse {
            phase,
            detail: METRIC_PAYLOAD_KEY,
        })? {
        "squared_euclidean" => Ok(Metric::SquaredEuclidean),
        "negative_dot_product" => Ok(Metric::NegativeDotProduct),
        _ => Err(QdrantError::MalformedResponse {
            phase,
            detail: "known metric",
        }),
    }
}

fn decode_partition(
    payload: &Map<String, Value>,
    phase: RequestPhase,
) -> Result<PartitionId, QdrantError> {
    let value = payload
        .get(PARTITION_PAYLOAD_KEY)
        .and_then(Value::as_u64)
        .ok_or(QdrantError::MalformedResponse {
            phase,
            detail: PARTITION_PAYLOAD_KEY,
        })?;
    let raw = match u16::try_from(value) {
        Ok(raw) => raw,
        Err(source) => {
            return Err(QdrantError::IntegerRange {
                phase,
                field: PARTITION_PAYLOAD_KEY,
                observed: value,
                source,
            });
        }
    };
    Ok(PartitionId::new(raw))
}

fn decode_entity(
    payload: &Map<String, Value>,
    phase: RequestPhase,
) -> Result<EntityId, QdrantError> {
    let value = payload
        .get(ENTITY_PAYLOAD_KEY)
        .and_then(Value::as_u64)
        .ok_or(QdrantError::MalformedResponse {
            phase,
            detail: ENTITY_PAYLOAD_KEY,
        })?;
    let raw = match u32::try_from(value) {
        Ok(raw) => raw,
        Err(source) => {
            return Err(QdrantError::IntegerRange {
                phase,
                field: ENTITY_PAYLOAD_KEY,
                observed: value,
                source,
            });
        }
    };
    Ok(EntityId::new(raw))
}

fn decode_hex_field(
    payload: &Map<String, Value>,
    field: &'static str,
    expected_bytes: usize,
    phase: RequestPhase,
) -> Result<Vec<u8>, QdrantError> {
    let value =
        payload
            .get(field)
            .and_then(Value::as_str)
            .ok_or(QdrantError::MalformedResponse {
                phase,
                detail: field,
            })?;
    if value.len() != expected_bytes * 2 {
        return Err(QdrantError::InvalidFieldRange { phase, field });
    }
    let mut bytes = Vec::with_capacity(expected_bytes);
    let raw = value.as_bytes();
    for pair in raw.chunks_exact(2) {
        let [high_raw, low_raw] = pair else {
            return Err(QdrantError::MalformedResponse {
                phase,
                detail: field,
            });
        };
        let high = hex_nibble(*high_raw).ok_or(QdrantError::MalformedResponse {
            phase,
            detail: field,
        })?;
        let low = hex_nibble(*low_raw).ok_or(QdrantError::MalformedResponse {
            phase,
            detail: field,
        })?;
        bytes.push((high << 4) | low);
    }
    Ok(bytes)
}

fn decode_vector(
    point: &Value,
    phase: RequestPhase,
    physical_id: PhysicalPointId,
    expected_dimension: usize,
) -> Result<Vec<f64>, QdrantError> {
    let vector =
        point
            .get("vector")
            .and_then(Value::as_array)
            .ok_or(QdrantError::MalformedResponse {
                phase,
                detail: "result[].vector[]",
            })?;
    if vector.len() != expected_dimension {
        return Err(QdrantError::VectorMismatch { phase, physical_id });
    }
    vector
        .iter()
        .map(|value| {
            value
                .as_f64()
                .ok_or(QdrantError::VectorMismatch { phase, physical_id })
        })
        .collect()
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

fn compare_hits(left: QdrantHit, right: QdrantHit, metric: Metric) -> std::cmp::Ordering {
    let score = match metric {
        Metric::SquaredEuclidean => left.score.total_cmp(&right.score),
        Metric::NegativeDotProduct => right.score.total_cmp(&left.score),
    };
    score
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
        let payload = identity_payload(key);
        assert_eq!(
            payload.get(SNAPSHOT_PAYLOAD_KEY),
            Some(&Value::from(hex(key.snapshot().as_ref())))
        );
        assert_eq!(
            payload.get(MODEL_PAYLOAD_KEY),
            Some(&Value::from(hex(key.model().as_ref())))
        );
        assert_eq!(
            payload.get(METRIC_PAYLOAD_KEY),
            Some(&Value::from("squared_euclidean"))
        );
        assert_eq!(
            payload.get(PARTITION_PAYLOAD_KEY),
            Some(&Value::from(11_u16))
        );
        assert_eq!(payload.get(ENTITY_PAYLOAD_KEY), Some(&Value::from(13_u32)));
        let filter = identity_filter(key.authority(), &[key.partition()]);
        let must = filter.get("must").and_then(Value::as_array);
        assert!(must.is_some_and(|must| must.len() == 4));
        assert_eq!(
            must.and_then(|must| must.get(3))
                .and_then(|condition| condition.get("key")),
            Some(&Value::from(PARTITION_PAYLOAD_KEY))
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
        let upsert = upsert_request(&prepared);
        assert_eq!(
            upsert.get("points").and_then(Value::as_array).map(Vec::len),
            Some(1)
        );
        assert_eq!(
            upsert
                .get("points")
                .and_then(Value::as_array)
                .and_then(|points| points.first())
                .and_then(|point| point.get("id")),
            Some(&Value::from(point.physical_id().raw()))
        );
        let keys = [PreparedKey {
            index: 0,
            key: point.key(),
            physical_id: point.physical_id(),
        }];
        assert_eq!(
            delete_request(&keys)
                .get("points")
                .and_then(Value::as_array)
                .and_then(|points| points.first()),
            Some(&Value::from(point.physical_id().raw()))
        );
        assert_eq!(
            retrieve_request(&keys, true).get("with_vector"),
            Some(&Value::from(true))
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
}
