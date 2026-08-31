//! Public immutable Qdrant projection contract and typed failure taxonomy.

use std::{
    num::{NonZeroU8, TryFromIntError},
    ops::Deref,
};

use nudox_index_graph_vector::{Metric, ModelId, PartitionId, VectorAuthority, VectorFact};
use nudox_index_vocab::{IndexSnapshotId, VectorSegmentId};
use nudox_ir_vocab::EntityId;

use super::{DEFAULT_MAX_ATTEMPTS, FnvHasher, MetricIdentityTag, PHYSICAL_ID_ZERO_REPLACEMENT};

/// A bounded retry policy for idempotent Qdrant requests.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RetryPolicy {
    /// Complete positive request-attempt bound.
    pub attempts: NonZeroU8,
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
}

/// A disposable physical integer coordinate in one Qdrant collection.
#[repr(transparent)]
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct PhysicalPointId(pub u64);

impl PhysicalPointId {
    /// Derives a deterministic physical coordinate from complete semantic authority.
    #[must_use]
    pub fn for_key(key: QdrantDataKey) -> Self {
        let mut hasher = FnvHasher::new();
        hasher.write(key.authority.snapshot().as_ref());
        hasher.write(key.authority.model().as_ref());
        hasher.write(key.segment.as_ref());
        hasher.write(&key.authority.dimension().to_be_bytes());
        hasher.write(&[u8::from(MetricIdentityTag::from(key.authority.metric()))]);
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
    /// Snapshot, model, dimension, and metric authority.
    pub authority: VectorAuthority,
    /// Immutable vector segment that produced the row.
    pub segment: VectorSegmentId,
    /// Immutable projection partition.
    pub partition: PartitionId,
    /// Semantic entity coordinate.
    pub entity: EntityId,
}

impl QdrantDataKey {
    /// Binds one semantic entity to the complete vector authority.
    #[must_use]
    pub const fn new(
        authority: VectorAuthority,
        segment: VectorSegmentId,
        partition: PartitionId,
        entity: EntityId,
    ) -> Self {
        Self {
            authority,
            segment,
            partition,
            entity,
        }
    }
}

/// Complete immutable key retained out-of-line only on diagnostic paths.
///
/// Keeping this cold evidence out of the error discriminant makes every successful `Result`
/// materially smaller without truncating the authority reported on failure.
#[derive(Debug, Eq, PartialEq)]
pub struct QdrantKeyEvidence(Box<QdrantDataKey>);

impl From<QdrantDataKey> for QdrantKeyEvidence {
    fn from(key: QdrantDataKey) -> Self {
        Self(Box::new(key))
    }
}

impl Deref for QdrantKeyEvidence {
    type Target = QdrantDataKey;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

/// One borrowed vector row admitted for remote projection or local comparison.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct QdrantPoint<'coordinates> {
    /// Complete semantic point identity.
    pub key: QdrantDataKey,
    /// Borrowed vector coordinates.
    pub coordinates: &'coordinates [i16],
}

impl<'coordinates> QdrantPoint<'coordinates> {
    /// Creates a borrowed point without performing remote I/O.
    #[must_use]
    pub const fn new(
        authority: VectorAuthority,
        segment: VectorSegmentId,
        partition: PartitionId,
        entity: EntityId,
        coordinates: &'coordinates [i16],
    ) -> Self {
        Self {
            key: QdrantDataKey::new(authority, segment, partition, entity),
            coordinates,
        }
    }

    /// Returns this point's disposable physical coordinate.
    #[must_use]
    pub fn physical_id(self) -> PhysicalPointId {
        PhysicalPointId::for_key(self.key)
    }
}

impl<'coordinates> From<QdrantPoint<'coordinates>> for VectorFact<'coordinates> {
    fn from(point: QdrantPoint<'coordinates>) -> Self {
        Self::new(
            point.key.authority,
            point.key.partition,
            point.key.entity,
            point.coordinates,
        )
    }
}

/// The deterministic score and complete provenance returned by a remote query.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct QdrantHit {
    /// Snapshot, model, dimension, and metric authority.
    pub authority: VectorAuthority,
    /// Immutable vector segment that produced the row.
    pub segment: VectorSegmentId,
    /// Source partition.
    pub partition: PartitionId,
    /// Semantic entity coordinate.
    pub entity: EntityId,
    /// Qdrant's metric-specific score.
    pub score: f64,
    /// Disposable physical coordinate observed in the response.
    pub physical_id: PhysicalPointId,
}

/// Number of initialized query-output slots.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct QueryHitCount {
    /// Exact initialized slot count.
    pub count: usize,
}

impl From<usize> for QueryHitCount {
    fn from(count: usize) -> Self {
        Self { count }
    }
}

impl Deref for QueryHitCount {
    type Target = usize;

    fn deref(&self) -> &Self::Target {
        &self.count
    }
}

/// A successful mutation receipt after the remote operation and readback verification.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct QdrantMutationReceipt {
    /// Number of submitted keys.
    pub attempted: usize,
    /// Number independently verified after the operation.
    pub verified: usize,
}

/// A verified remote point readback, retaining its identity and coordinates.
#[derive(Clone, Debug, PartialEq)]
pub struct QdrantReadback {
    /// Complete semantic key.
    pub key: QdrantDataKey,
    /// Disposable physical coordinate.
    pub physical_id: PhysicalPointId,
    /// Remotely stored coordinates.
    pub coordinates: Vec<f64>,
}

/// Exact request phase retained by transport, status, and decode failures.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RequestPhase {
    /// Collection creation or collision handling.
    CreateCollection,
    /// Collection metadata readback.
    ReadCollection,
    /// Payload index creation or idempotent verification.
    CreatePayloadIndex,
    /// Existing point identity preflight.
    ReadPoints,
    /// Batched point upload.
    UpsertPoints,
    /// Batched vector query.
    QueryPoints,
    /// Batched point deletion.
    DeletePoints,
    /// Delete readback verification.
    VerifyDelete,
    /// Collection deletion.
    DeleteCollection,
}

/// The required part of a successful Qdrant response that was absent or had an invalid shape.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MalformedResponseCause {
    /// A point list exceeded the adapter's bounded batch shape.
    PointBatchExceeded,
    /// A response repeated one physical point identifier.
    DuplicatePhysicalPoint,
    /// A query response reused one physical identifier for distinct payload identities.
    PhysicalIdAliased,
    /// A point omitted its vector where the operation requires one.
    MissingVector,
    /// The metric payload was a string outside this adapter's owned metric vocabulary.
    UnknownMetric,
    /// Internal bounded retry bookkeeping reached an impossible terminal state.
    RetryExhaustionWithoutResponse,
    /// A successful HTTP response explicitly rejected the requested mutation.
    RejectedAcknowledgement,
    /// A verified readback could not be placed in the admitted output slice.
    ReadbackOutputIndex,
}

/// A typed payload member owned by the immutable Qdrant identity contract.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PayloadField {
    /// Immutable snapshot digest.
    Snapshot,
    /// Complete model identity.
    Model,
    /// Immutable vector segment identity.
    Segment,
    /// Metric recipe.
    Metric,
    /// Projection partition.
    Partition,
    /// Semantic entity coordinate.
    Entity,
}

/// The payload invariant that a response point violated.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PayloadMismatchCause {
    /// The complete snapshot/model/dimension/metric authority differs.
    Authority,
    /// The point is outside the caller's selected partition set.
    PartitionSelection,
    /// A retrieved point's complete identity differs from its requested identity.
    RequestedIdentity,
}

/// The collection property that disagreed with the adapter's pinned contract.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CollectionField {
    /// Vector dimension.
    VectorDimension,
    /// Vector metric.
    VectorMetric,
    /// Write consistency relative to replication factor.
    WriteConsistency,
    /// A required payload index.
    PayloadIndex(PayloadField),
}

/// A rejected endpoint retained by configuration errors.
#[derive(Debug, PartialEq, Eq)]
pub struct RejectedEndpoint(pub String);

impl std::fmt::Display for RejectedEndpoint {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// A rejected collection name retained by configuration errors.
#[derive(Debug, PartialEq, Eq)]
pub struct RejectedCollectionName(pub String);

impl std::fmt::Display for RejectedCollectionName {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
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
        observed: RejectedEndpoint,
    },
    /// Endpoint contains a URI character that would make path joining ambiguous.
    #[error("Qdrant endpoint contains whitespace or a query/fragment: {observed}")]
    InvalidEndpoint {
        /// Complete rejected endpoint.
        observed: RejectedEndpoint,
    },
    /// Collection name is empty.
    #[error("Qdrant collection name is empty")]
    EmptyCollection,
    /// Collection name contains a path/control character.
    #[error("Qdrant collection name is not a single safe path component: {observed}")]
    InvalidCollection {
        /// Complete rejected collection name.
        observed: RejectedCollectionName,
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
        key: QdrantKeyEvidence,
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
        key: QdrantKeyEvidence,
    },
    /// An immutable key already exists with different vector coordinates.
    #[error("immutable point {physical_id:?} already has different coordinates for {key:?}")]
    ImmutableVectorConflict {
        /// Existing disposable physical coordinate.
        physical_id: PhysicalPointId,
        /// Complete immutable semantic key.
        key: QdrantKeyEvidence,
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
    #[error("Qdrant {phase:?} response is malformed: {cause:?}")]
    MalformedResponse {
        /// Operation phase.
        phase: RequestPhase,
        /// Exact malformed response category.
        cause: MalformedResponseCause,
    },
    /// More authority-filtered points exist than the exact bounded adapter can rank.
    #[error("Qdrant exact projection has at least {observed} points; maximum is {maximum}")]
    ProjectionCapacity {
        /// Maximum points this adapter can retrieve and rerank exactly.
        maximum: usize,
        /// Minimum complete point count proven by the overflowing scan.
        observed: usize,
    },
    /// The service returned a point that was not one of the requested identities.
    #[error("Qdrant {phase:?} returned an unrequested point {physical_id:?}")]
    UnexpectedPoint {
        /// Operation phase.
        phase: RequestPhase,
        /// Unexpected physical coordinate.
        physical_id: PhysicalPointId,
    },
    /// A response point's disposable ID was not derived from its semantic identity.
    #[error(
        "Qdrant {phase:?} returned physical point {observed_physical_id:?} for {key:?}; expected {expected_physical_id:?}"
    )]
    PhysicalIdentityMismatch {
        /// Operation phase.
        phase: RequestPhase,
        /// Full semantic identity decoded from the payload.
        key: QdrantKeyEvidence,
        /// Disposable coordinate returned by the service.
        observed_physical_id: PhysicalPointId,
        /// Collision-checked coordinate derived from the semantic identity.
        expected_physical_id: PhysicalPointId,
    },
    /// A response repeated one full semantic identity.
    #[error(
        "Qdrant {phase:?} repeated {key:?} as points {first_physical_id:?} and {second_physical_id:?}"
    )]
    DuplicateSemanticPoint {
        /// Operation phase.
        phase: RequestPhase,
        /// Repeated full semantic identity.
        key: QdrantKeyEvidence,
        /// First disposable coordinate carrying the identity.
        first_physical_id: PhysicalPointId,
        /// Later disposable coordinate carrying the same identity.
        second_physical_id: PhysicalPointId,
    },
    /// The response omitted a point required for verification.
    #[error("Qdrant {phase:?} omitted point {physical_id:?} for {key:?}")]
    MissingPoint {
        /// Operation phase.
        phase: RequestPhase,
        /// Missing physical coordinate.
        physical_id: PhysicalPointId,
        /// Expected semantic key.
        key: QdrantKeyEvidence,
    },
    /// The service returned a payload that disagreed with the requested identity.
    #[error("Qdrant {phase:?} payload identity disagrees for point {physical_id:?}: {cause:?}")]
    PayloadMismatch {
        /// Operation phase.
        phase: RequestPhase,
        /// Physical point coordinate.
        physical_id: PhysicalPointId,
        /// Exact identity invariant that failed.
        cause: PayloadMismatchCause,
    },
    /// Collection metadata disagreed with the pinned model shape or metric.
    #[error("Qdrant {phase:?} collection metadata disagrees in {field:?}")]
    CollectionMismatch {
        /// Operation phase.
        phase: RequestPhase,
        /// Mismatched collection property.
        field: CollectionField,
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
    #[error("Qdrant {phase:?} payload field {field:?} is outside its semantic range")]
    InvalidFieldRange {
        /// Operation phase.
        phase: RequestPhase,
        /// Payload field whose range was invalid.
        field: PayloadField,
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
    /// Segment bytes had the right width but failed the typed domain conversion.
    #[error("Qdrant {phase:?} vector-segment payload conversion failed")]
    SegmentDecode {
        /// Operation phase.
        phase: RequestPhase,
        /// Original typed segment conversion failure.
        #[source]
        source: <VectorSegmentId as TryFrom<[u8; 32]>>::Error,
    },
    /// An integer payload field could not fit its typed coordinate owner.
    #[error(
        "Qdrant {phase:?} payload field {field:?} value {observed} is outside its semantic range"
    )]
    IntegerRange {
        /// Operation phase.
        phase: RequestPhase,
        /// Payload field whose integer conversion failed.
        field: PayloadField,
        /// Complete observed unsigned value.
        observed: u64,
        /// Original integer conversion failure.
        #[source]
        source: TryFromIntError,
    },
}
