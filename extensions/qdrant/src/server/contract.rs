//! Defines contract behavior for `backend-extension-qdrant`, whose purpose is to adapt typed vector authorities and queries to the Qdrant service.
//! This module owns the contract invariants and typed state transitions.
//! Its narrow surface prevents representation and policy details from leaking outward.
//! Public immutable Qdrant projection contract and typed failure taxonomy.

use std::{
    num::{NonZeroU8, TryFromIntError},
    ops::Deref,
};

use arrayvec::ArrayVec;
use backend_semantic::ir::EntityId;
use serde::Serialize;
use backend_semantic::graph_vector::{
    MAX_VECTOR_DIMENSION, Metric, ModelId, PartitionId, VectorAuthority,
};
use backend_semantic::index_vocabulary::{IndexSnapshotId, VectorSegmentId};

use super::limits::{DEFAULT_MAX_ATTEMPTS, MAX_QUERY_SEGMENTS};

/// Payload key carrying the immutable snapshot digest as lowercase hexadecimal.
pub const SNAPSHOT_PAYLOAD_KEY: &str = "server_snapshot";
/// Payload key carrying the complete model registry identity as lowercase hexadecimal.
pub const MODEL_PAYLOAD_KEY: &str = "server_model";
/// Payload key carrying the immutable vector-segment identity as lowercase hexadecimal.
pub const SEGMENT_PAYLOAD_KEY: &str = "server_segment";
/// Payload key carrying the metric recipe name.
pub const METRIC_PAYLOAD_KEY: &str = "server_metric";
/// Payload key carrying the projection partition coordinate.
pub const PARTITION_PAYLOAD_KEY: &str = "server_partition";
/// Payload key carrying the semantic entity coordinate.
pub const ENTITY_PAYLOAD_KEY: &str = "server_entity";

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

/// Immutable adapter facts exposed together so callers cannot mistake one field for a mutable
/// authority update.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct QdrantAdapterConfig<'adapter> {
    /// Normalized endpoint without a trailing slash.
    pub endpoint: &'adapter str,
    /// Validated collection path component.
    pub collection: &'adapter str,
    /// Snapshot/model/dimension/metric authority pinned by this adapter.
    pub authority: VectorAuthority,
    /// Bounded retry policy used by every request.
    pub retry: RetryPolicy,
}

/// A disposable physical integer coordinate in one Qdrant collection.
#[repr(transparent)]
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct PhysicalPointId(pub u64);

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

/// One bounded remote candidate after typed scope checks and deterministic local reranking.
///
/// The segment is a remote payload claim, not a membership proof. Exact consumers must resolve the
/// candidate against an authenticated segment artifact before promoting it to a trusted hit.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct QdrantCandidate {
    /// Snapshot, model, dimension, and metric authority.
    pub authority: VectorAuthority,
    /// Remotely claimed immutable vector segment.
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
pub struct QueryCandidateCount {
    /// Exact initialized slot count.
    pub count: usize,
}

impl From<usize> for QueryCandidateCount {
    fn from(count: usize) -> Self {
        Self { count }
    }
}

impl Deref for QueryCandidateCount {
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
    pub coordinates: QdrantCoordinates,
}

/// Stack-resident coordinates retained by a verified remote readback.
#[derive(Clone, Debug, PartialEq)]
pub struct QdrantCoordinates(ArrayVec<f64, MAX_VECTOR_DIMENSION>);

impl QdrantCoordinates {
    pub(crate) fn copy_from(coordinates: &[f64]) -> Result<Self, CoordinateCapacity> {
        let observed = coordinates.len();
        match ArrayVec::try_from(coordinates) {
            Ok(values) => Ok(Self(values)),
            Err(_capacity) => Err(CoordinateCapacity {
                maximum: MAX_VECTOR_DIMENSION,
                observed,
            }),
        }
    }

    pub(crate) const fn empty() -> Self {
        Self(ArrayVec::new_const())
    }
}

impl Deref for QdrantCoordinates {
    type Target = [f64];

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl AsRef<[f64]> for QdrantCoordinates {
    fn as_ref(&self) -> &[f64] {
        self
    }
}

/// Complete coordinate capacity rejection from a bounded readback conversion.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
#[error("readback has {observed} coordinates; stack capacity is {maximum}")]
pub struct CoordinateCapacity {
    /// Maximum retained coordinates.
    pub maximum: usize,
    /// Complete observed coordinate count.
    pub observed: usize,
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
#[derive(Debug, Eq, PartialEq)]
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
    UnknownMetric(RejectedMetric),
    /// A successful HTTP response explicitly rejected the requested mutation.
    RejectedAcknowledgement,
    /// A verified readback could not be placed in the admitted output slice.
    ReadbackOutputIndex,
}

/// Metric spelling rejected at the external Qdrant payload edge.
#[derive(Debug, Eq, PartialEq)]
pub struct RejectedMetric(pub String);

impl std::fmt::Display for RejectedMetric {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// A typed payload member owned by the immutable Qdrant identity contract.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
pub enum PayloadField {
    /// Immutable snapshot digest.
    #[serde(rename = "server_snapshot")]
    Snapshot,
    /// Complete model identity.
    #[serde(rename = "server_model")]
    Model,
    /// Immutable vector segment identity.
    #[serde(rename = "server_segment")]
    Segment,
    /// Metric recipe.
    #[serde(rename = "server_metric")]
    Metric,
    /// Projection partition.
    #[serde(rename = "server_partition")]
    Partition,
    /// Semantic entity coordinate.
    #[serde(rename = "server_entity")]
    Entity,
}

/// The payload invariant that a response point violated.
#[derive(Debug, Eq, PartialEq)]
pub enum PayloadMismatchCause {
    /// The complete snapshot/model/dimension/metric authority differs.
    Authority(Box<AuthorityMismatchEvidence>),
    /// The point is outside the caller's selected immutable segment set.
    SegmentSelection {
        /// Complete bounded selection in request order, allocated only on this cold rejection.
        selected: Box<[Option<VectorSegmentId>; MAX_QUERY_SEGMENTS]>,
        /// Segment decoded from the response.
        observed: VectorSegmentId,
    },
    /// The response paired a selected segment with a partition outside its descriptor.
    SegmentPartition {
        /// Selected immutable segment.
        segment: VectorSegmentId,
        /// Partition committed by the selected descriptor.
        expected: PartitionId,
        /// Partition decoded from the response.
        observed: PartitionId,
    },
    /// A retrieved point's complete identity differs from its requested identity.
    RequestedIdentity {
        /// Complete requested semantic key.
        expected: QdrantKeyEvidence,
        /// Complete semantic key decoded from the response.
        observed: QdrantKeyEvidence,
    },
}

/// Complete authorities retained out-of-line on a cold payload-rejection path.
#[derive(Debug, Eq, PartialEq)]
pub struct AuthorityMismatchEvidence {
    /// Authority pinned by the adapter.
    pub expected: VectorAuthority,
    /// Authority decoded from the response.
    pub observed: VectorAuthority,
}

/// Qdrant payload-index types owned by this adapter's collection protocol.
#[derive(Clone, Copy, Debug, serde::Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum PayloadIndexKind {
    /// String equality index.
    Keyword,
    /// Integer equality index.
    Integer,
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

/// Typed expected or observed value retained by a collection mismatch.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CollectionValue {
    /// Vector coordinate count.
    Dimension(u64),
    /// Vector distance recipe.
    Metric(Metric),
    /// Replication or write-consistency factor.
    Consistency(u64),
    /// Present payload-index type.
    PayloadIndex(PayloadIndexKind),
    /// Required payload index was absent.
    MissingPayloadIndex,
}

/// Exact invalid encoding observed for a typed payload field.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PayloadEncodingCause {
    /// The typed identity width could not be represented as hexadecimal digits.
    HexWidthOverflow {
        /// Identity width in bytes.
        bytes: usize,
    },
    /// The external spelling had the wrong number of hexadecimal digits.
    HexLength {
        /// Required hexadecimal digit count.
        expected: usize,
        /// Complete observed digit count.
        observed: usize,
    },
    /// One byte was outside the ASCII hexadecimal alphabet.
    HexDigit {
        /// Zero-based digit position.
        index: usize,
        /// Exact rejected byte.
        observed: u8,
    },
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
    /// Too many immutable segments were selected for one bounded query.
    #[error("selected segment fanout has {observed}; maximum is {maximum}")]
    TooManySegments {
        /// Maximum selected segment fanout.
        maximum: usize,
        /// Complete rejected fanout.
        observed: usize,
    },
    /// An immutable vector segment was selected more than once.
    #[error("selected segment {segment:?} appears at {first_index} and {index}")]
    DuplicateSegment {
        /// First occurrence position.
        first_index: usize,
        /// Later occurrence position.
        index: usize,
        /// Repeated immutable segment.
        segment: VectorSegmentId,
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
    #[error("physical point id {physical_id:?} collides for {first:?} and {second:?}")]
    PhysicalIdCollision {
        /// Colliding disposable physical coordinate.
        physical_id: PhysicalPointId,
        /// Complete first semantic key.
        first: QdrantKeyEvidence,
        /// Complete later semantic key.
        second: QdrantKeyEvidence,
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
    /// Reading a decoded response body failed after transport succeeded.
    #[error("Qdrant {phase:?} response read failed after {attempts} attempt(s)")]
    ResponseRead {
        /// Operation phase.
        phase: RequestPhase,
        /// Number of attempts made.
        attempts: u8,
        /// Original decoded-body read failure.
        #[source]
        source: std::io::Error,
    },
    /// The decoded response exceeded the fixed in-memory boundary.
    #[error(
        "Qdrant {phase:?} decoded response has at least {observed_at_least} bytes; maximum is {maximum}"
    )]
    ResponseTooLarge {
        /// Operation phase.
        phase: RequestPhase,
        /// Maximum decoded bytes retained.
        maximum: usize,
        /// Minimum decoded length proven by the bounded reader.
        observed_at_least: usize,
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
    /// A response returned more rows for one segment than its immutable descriptor commits.
    #[error(
        "Qdrant {phase:?} returned {observed} rows for segment {segment:?}; descriptor commits {maximum}"
    )]
    SegmentCardinalityExceeded {
        /// Operation phase.
        phase: RequestPhase,
        /// Immutable selected segment.
        segment: VectorSegmentId,
        /// Maximum committed row count.
        maximum: u8,
        /// Complete observed count at rejection.
        observed: u8,
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
    /// Collection metadata disagreed with the pinned model shape, metric, or payload indexes.
    #[error(
        "Qdrant {phase:?} collection metadata disagrees in {field:?}: expected {expected:?}, observed {observed:?}"
    )]
    CollectionMismatch {
        /// Operation phase.
        phase: RequestPhase,
        /// Mismatched collection property.
        field: CollectionField,
        /// Typed value required by the adapter.
        expected: CollectionValue,
        /// Typed value returned by Qdrant.
        observed: CollectionValue,
    },
    /// A remote vector was malformed or differed from the uploaded coordinates.
    #[error("Qdrant {phase:?} vector differs for point {physical_id:?}")]
    VectorMismatch {
        /// Operation phase.
        phase: RequestPhase,
        /// Physical point coordinate.
        physical_id: PhysicalPointId,
    },
    /// A typed vector passed dimension validation but exceeded retained inline storage.
    #[error("Qdrant {phase:?} point {physical_id:?} exceeded coordinate capacity")]
    CoordinateCapacity {
        /// Operation phase.
        phase: RequestPhase,
        /// Physical point coordinate.
        physical_id: PhysicalPointId,
        /// Original complete capacity rejection.
        #[source]
        source: CoordinateCapacity,
    },
    /// A textual response field violated its typed identity encoding.
    #[error("Qdrant {phase:?} payload field {field:?} has invalid encoding: {cause:?}")]
    InvalidFieldEncoding {
        /// Operation phase.
        phase: RequestPhase,
        /// Payload field whose range was invalid.
        field: PayloadField,
        /// Exact rejected width or digit.
        cause: PayloadEncodingCause,
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
