//! Durable producer-side admission for stable-key semantic IR objects.
//!
//! This path writes bounded SPIR segments and jumbo-rope objects directly to
//! the immutable FileStore. It does not create a V1 ordinal NXFI plane, select
//! a generation, or claim that one family's local grammar check proves the
//! complete V2 aggregate. A producer callback can record a prefix of durable
//! admissions before a later row or family fails. Such objects are safe
//! immutable orphans, but callers must not publish from a partial receipt set.
//! The semantic-to-physical mapping exists only in caller receipts; a crash
//! before persisting that mapping is safe but not resumable. V2 publication
//! remains gated on complete-family and aggregate verification elsewhere.

use std::fmt;

use backend_semantic::ir::{
    CanonicalSemanticPlaneSegmentRef, CanonicalSemanticPlaneSegmentSink, JumboRopeLeafRef,
    JumboRopeNode, JumboRopeObjectId, JumboRopeObjectSink, SemanticIrPlane, SemanticPlaneKind,
    SemanticPlaneRecordError, ValidatedCanonicalSemanticPlaneSegment,
};
use backend_store::{FileStore, GcPinGuard, ObjectId, ObjectWriteReceipt, TypedObject};
use backend_version::{ObjectKey, Schema, SchemaIdentity};

use crate::ir_hydration_store::SemanticSegmentPayload;

/// A semantic identity plus the physical FileStore identity that durably
/// admitted its exact payload.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProducedSemanticObjectIdentity {
    /// One locally validated stable-key SPIR segment.
    Segment {
        /// Exact IR family encoded by the segment.
        family: SemanticIrPlane,
        /// Content identity bound to family, stable range, count, and bytes.
        id: backend_semantic::ir::SemanticSegmentId,
        /// Inclusive first stable row key.
        first_key: [u8; 32],
        /// Inclusive last stable row key.
        last_key: [u8; 32],
        /// Exact number of complete rows.
        row_count: u32,
    },
    /// One content-defined jumbo leaf.
    ///
    /// Repeated equal leaves at different offsets may share one physical
    /// object ID. Receipts preserve occurrences; closure membership must
    /// deduplicate physical IDs.
    JumboLeaf {
        /// Jumbo-rope leaf identity from the semantic rope grammar.
        id: JumboRopeObjectId,
        /// Position within the complete ordered rope.
        ordinal: u64,
        /// Exact byte offset within the complete value.
        byte_offset: u64,
    },
    /// One authenticated interior node in a jumbo rope.
    JumboInterior {
        /// Jumbo-rope interior identity from the semantic rope grammar.
        id: JumboRopeObjectId,
        /// First leaf ordinal covered by the node.
        first_leaf: u64,
        /// Number of leaves covered by the node.
        leaf_count: u64,
    },
}

/// Closed physical object kinds admitted by the stable semantic producer.
/// Keep this mapping as the source of truth for cold-read schema checks: a
/// semantic identity alone does not identify its FileStore envelope schema.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProducedSemanticObjectKind {
    /// Canonical stable-key SPIR segment payload.
    Segment,
    /// Content-defined jumbo-rope leaf payload.
    JumboLeaf,
    /// Canonical jumbo-rope interior-node wire payload.
    JumboInterior,
}

impl ProducedSemanticObjectIdentity {
    /// Exact physical object kind carrying this semantic identity.
    #[must_use]
    pub const fn kind(self) -> ProducedSemanticObjectKind {
        match self {
            Self::Segment { .. } => ProducedSemanticObjectKind::Segment,
            Self::JumboLeaf { .. } => ProducedSemanticObjectKind::JumboLeaf,
            Self::JumboInterior { .. } => ProducedSemanticObjectKind::JumboInterior,
        }
    }
}

impl ProducedSemanticObjectKind {
    /// Exact FileStore envelope schema used by this producer object kind.
    #[must_use]
    pub const fn schema_identity(self) -> SchemaIdentity {
        match self {
            Self::Segment => SchemaIdentity::new(
                SemanticSegmentPayload::DOMAIN,
                SemanticSegmentPayload::TYPE,
                SemanticSegmentPayload::VERSION,
            ),
            Self::JumboLeaf => SchemaIdentity::new(
                SemanticJumboLeafPayload::DOMAIN,
                SemanticJumboLeafPayload::TYPE,
                SemanticJumboLeafPayload::VERSION,
            ),
            Self::JumboInterior => SchemaIdentity::new(
                SemanticJumboInteriorPayload::DOMAIN,
                SemanticJumboInteriorPayload::TYPE,
                SemanticJumboInteriorPayload::VERSION,
            ),
        }
    }
}

/// Receipt produced only after a typed object is durably admitted by FileStore.
/// A new object is read back and checked; on a CAS hit FileStore has already
/// read and byte-compared the existing encoded envelope.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DurableSemanticObjectAdmission {
    identity: ProducedSemanticObjectIdentity,
    object_id: ObjectId,
    payload_bytes: u64,
    envelope_bytes: u64,
    created: bool,
}

/// GC-only pin held while producer objects await caller-managed publication.
/// This guard is not coupled to receipts, does not certify a complete family
/// or aggregate, and does not enforce publication before it is dropped.
#[must_use = "retain the GC guard while admitted objects await publication"]
#[derive(Debug)]
pub struct DurableSemanticObjectPin {
    _guard: GcPinGuard,
}

impl DurableSemanticObjectAdmission {
    /// Semantic identity covered by this durable object receipt.
    #[must_use]
    pub const fn identity(self) -> ProducedSemanticObjectIdentity {
        self.identity
    }

    /// Physical FileStore object identity to add to a verified closure.
    #[must_use]
    pub const fn object_id(self) -> ObjectId {
        self.object_id
    }

    /// Exact semantic payload bytes stored in the object.
    #[must_use]
    pub const fn payload_bytes(self) -> u64 {
        self.payload_bytes
    }

    /// Exact encoded FileStore envelope bytes admitted.
    #[must_use]
    pub const fn envelope_bytes(self) -> u64 {
        self.envelope_bytes
    }

    /// Whether this call created the physical immutable object file.
    #[must_use]
    pub const fn created(self) -> bool {
        self.created
    }
}

/// Caller-owned destination for durable object receipts.
///
/// Production callers can stream these receipts into bounded manifest or
/// closure construction. The implementation does not retain an Arc or mutex
/// per row; it invokes this coarse object-level callback only after durable
/// admission succeeds. Callers must treat callback prefixes as incomplete if
/// family encoding or a later aggregate check fails.
pub trait SemanticObjectAdmissionSink {
    /// Failure while recording a completed durable admission.
    type Error: fmt::Display;

    /// Records one immutable object receipt.
    fn record_admission(
        &mut self,
        admission: DurableSemanticObjectAdmission,
    ) -> Result<(), Self::Error>;
}

/// Caller-owned, unbounded receipt collection for tests and small producer passes.
///
/// A seven-family V2 publisher can consume these entries into its manifest
/// builder, then feed their object IDs into FileStore closure composition.
/// This buffer has no receipt-count cap and retains one entry per admitted
/// occurrence, including duplicate physical IDs for equal rope leaves. Large
/// rope producers should provide a streaming [`SemanticObjectAdmissionSink`]
/// instead. Row payload bytes are never retained here.
#[derive(Default)]
pub struct SemanticObjectAdmissionBuffer {
    admissions: Vec<DurableSemanticObjectAdmission>,
}

impl SemanticObjectAdmissionBuffer {
    /// Receipts recorded so far, in deterministic producer order.
    #[must_use]
    pub fn admissions(&self) -> &[DurableSemanticObjectAdmission] {
        &self.admissions
    }

    /// Consumes the buffer and returns the durable object receipts.
    #[must_use]
    pub fn into_admissions(self) -> Vec<DurableSemanticObjectAdmission> {
        self.admissions
    }
}

impl SemanticObjectAdmissionSink for SemanticObjectAdmissionBuffer {
    type Error = String;

    fn record_admission(
        &mut self,
        admission: DurableSemanticObjectAdmission,
    ) -> Result<(), Self::Error> {
        self.admissions
            .try_reserve(1)
            .map_err(|error| format!("reserve semantic object receipt: {error}"))?;
        self.admissions.push(admission);
        Ok(())
    }
}

/// Counters for one stable-segment or jumbo-rope producer-to-FileStore pass.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct SemanticProducerStoreMetrics {
    attempted_objects: u64,
    created_objects: u64,
    reused_objects: u64,
    semantic_segment_hash_bytes: u64,
    jumbo_leaf_hash_bytes: u64,
    jumbo_interior_hash_bytes: u64,
    payload_bytes: u64,
    admitted_envelope_bytes: u64,
    newly_stored_envelope_bytes: u64,
    cas_hit_compare_envelope_bytes: u64,
    created_object_readback_envelope_bytes: u64,
}

impl SemanticProducerStoreMetrics {
    /// Number of segment or rope objects offered to FileStore.
    #[must_use]
    pub const fn attempted_objects(self) -> u64 {
        self.attempted_objects
    }

    /// Number of new physical object files created.
    #[must_use]
    pub const fn created_objects(self) -> u64 {
        self.created_objects
    }

    /// Number of objects reused by content identity.
    #[must_use]
    pub const fn reused_objects(self) -> u64 {
        self.reused_objects
    }

    /// SPIR bytes traversed by the segment's semantic identity verifier.
    ///
    /// This excludes FileStore's own key, version, and envelope hashing. It
    /// measures physical producer validation work, not total compiler work.
    #[must_use]
    pub const fn semantic_segment_hash_bytes(self) -> u64 {
        self.semantic_segment_hash_bytes
    }

    /// Jumbo leaf value bytes traversed by the content-defined leaf identity
    /// hash before each leaf is handed to this storage sink.
    #[must_use]
    pub const fn jumbo_leaf_hash_bytes(self) -> u64 {
        self.jumbo_leaf_hash_bytes
    }

    /// Fixed-size jumbo interior wire bytes traversed by the authenticated
    /// interior identity hash before each node is handed to this sink.
    #[must_use]
    pub const fn jumbo_interior_hash_bytes(self) -> u64 {
        self.jumbo_interior_hash_bytes
    }

    /// Exact segment/rope payload bytes offered for durable admission.
    #[must_use]
    pub const fn payload_bytes(self) -> u64 {
        self.payload_bytes
    }

    /// Encoded FileStore envelope bytes admitted, including reuse hits.
    #[must_use]
    pub const fn admitted_envelope_bytes(self) -> u64 {
        self.admitted_envelope_bytes
    }

    /// Encoded FileStore envelope bytes created on disk by this pass.
    #[must_use]
    pub const fn newly_stored_envelope_bytes(self) -> u64 {
        self.newly_stored_envelope_bytes
    }

    /// Encoded envelopes read and compared by FileStore on CAS hits.
    #[must_use]
    pub const fn cas_hit_compare_envelope_bytes(self) -> u64 {
        self.cas_hit_compare_envelope_bytes
    }

    /// Encoded envelopes read back by this adapter after creating new objects.
    #[must_use]
    pub const fn created_object_readback_envelope_bytes(self) -> u64 {
        self.created_object_readback_envelope_bytes
    }
}

/// FileStore-backed sink for borrowed stable-key SPIR segments.
///
/// The sink pins garbage collection from construction through its lifetime.
/// After encoding, consume it with [`Self::into_collection_pin`] and retain
/// that GC-only guard while these objects await caller-managed publication.
/// The returned token does not bind a complete receipt set or enforce closure
/// publication; partial callback output must not be published.
pub struct FileSemanticPlaneSegmentSink<'store, 'receipts, Receipts: ?Sized> {
    store: &'store FileStore,
    receipts: &'receipts mut Receipts,
    metrics: SemanticProducerStoreMetrics,
    gc_pin: GcPinGuard,
}

impl<'store, 'receipts, Receipts: SemanticObjectAdmissionSink + ?Sized>
    FileSemanticPlaneSegmentSink<'store, 'receipts, Receipts>
{
    /// Starts a segment sink over the existing immutable object CAS.
    pub fn new(
        store: &'store FileStore,
        receipts: &'receipts mut Receipts,
    ) -> Result<Self, String> {
        let gc_pin = store
            .pin_garbage_collection()
            .map_err(|error| format!("pin semantic producer segments against GC: {error:?}"))?;
        Ok(Self {
            store,
            receipts,
            metrics: SemanticProducerStoreMetrics::default(),
            gc_pin,
        })
    }

    /// Returns exact durable-object and semantic-hash counters so far.
    #[must_use]
    pub const fn metrics(&self) -> SemanticProducerStoreMetrics {
        self.metrics
    }

    /// Consumes this sink and transfers its GC guard to the caller. Keep it
    /// until a durable closure/ref protects the objects, while tracking full
    /// family and aggregate success separately; this token does not enforce
    /// that handoff.
    #[must_use]
    pub fn into_collection_pin(self) -> DurableSemanticObjectPin {
        DurableSemanticObjectPin {
            _guard: self.gc_pin,
        }
    }

    fn persist_validated(
        &mut self,
        segment: ValidatedCanonicalSemanticPlaneSegment<'_>,
    ) -> Result<(), SemanticPlaneRecordError> {
        let payload = segment.bytes();
        let key = ObjectKey::<SemanticSegmentPayload>::from_value(payload);
        let object = TypedObject::from_value(&key, payload);
        let admitted = commit_and_read(self.store, &object, payload)
            .map_err(SemanticPlaneRecordError::JumboObjectStore)?;
        let byte_length =
            u64::try_from(payload.len()).map_err(|_| SemanticPlaneRecordError::MetricsOverflow)?;
        let identity = ProducedSemanticObjectIdentity::Segment {
            family: match segment.kind() {
                SemanticPlaneKind::Ir(family) => family,
                _ => return Err(SemanticPlaneRecordError::IrKindRequired),
            },
            id: segment.id(),
            first_key: segment.first_key(),
            last_key: segment.last_key(),
            row_count: segment.row_count(),
        };
        let receipt = DurableSemanticObjectAdmission {
            identity,
            object_id: admitted.id(),
            payload_bytes: byte_length,
            envelope_bytes: admitted.bytes(),
            created: admitted.created(),
        };
        self.receipts
            .record_admission(receipt)
            .map_err(|error| SemanticPlaneRecordError::JumboObjectStore(error.to_string()))?;
        self.metrics
            .record(admitted, byte_length, byte_length, 0, 0)?;
        Ok(())
    }
}

impl<Receipts: SemanticObjectAdmissionSink + ?Sized> CanonicalSemanticPlaneSegmentSink
    for FileSemanticPlaneSegmentSink<'_, '_, Receipts>
{
    type Error = SemanticPlaneRecordError;

    fn write_segment(
        &mut self,
        segment: CanonicalSemanticPlaneSegmentRef<'_>,
    ) -> Result<(), Self::Error> {
        self.persist_validated(segment.validate()?)
    }
}

/// FileStore-backed sink for leaves and interior records emitted by the
/// semantic jumbo-rope writer. It pins collection through the producer pass;
/// the transferred token only guards against collection and does not certify
/// complete-family success or bind receipts to a published closure.
pub struct FileSemanticJumboRopeSink<'store, 'receipts, Receipts: ?Sized> {
    store: &'store FileStore,
    receipts: &'receipts mut Receipts,
    metrics: SemanticProducerStoreMetrics,
    gc_pin: GcPinGuard,
}

impl<'store, 'receipts, Receipts: SemanticObjectAdmissionSink + ?Sized>
    FileSemanticJumboRopeSink<'store, 'receipts, Receipts>
{
    /// Starts a rope-object sink over the existing immutable object CAS.
    pub fn new(
        store: &'store FileStore,
        receipts: &'receipts mut Receipts,
    ) -> Result<Self, String> {
        let gc_pin = store
            .pin_garbage_collection()
            .map_err(|error| format!("pin jumbo producer objects against GC: {error:?}"))?;
        Ok(Self {
            store,
            receipts,
            metrics: SemanticProducerStoreMetrics::default(),
            gc_pin,
        })
    }

    /// Returns exact durable-object counters so far.
    #[must_use]
    pub const fn metrics(&self) -> SemanticProducerStoreMetrics {
        self.metrics
    }

    /// Consumes this sink and transfers its GC guard to the caller. Keep it
    /// until a durable closure/ref protects the objects, while tracking full
    /// family and aggregate success separately; this token does not enforce
    /// that handoff.
    #[must_use]
    pub fn into_collection_pin(self) -> DurableSemanticObjectPin {
        DurableSemanticObjectPin {
            _guard: self.gc_pin,
        }
    }
}

impl<Receipts: SemanticObjectAdmissionSink + ?Sized> JumboRopeObjectSink
    for FileSemanticJumboRopeSink<'_, '_, Receipts>
{
    type Error = SemanticPlaneRecordError;

    fn write_leaf(&mut self, leaf: JumboRopeLeafRef<'_>) -> Result<(), Self::Error> {
        let payload = leaf.bytes();
        let key = ObjectKey::<SemanticJumboLeafPayload>::from_value(payload);
        let object = TypedObject::from_value(&key, payload);
        let admitted = commit_and_read(self.store, &object, payload)
            .map_err(SemanticPlaneRecordError::JumboObjectStore)?;
        let byte_length =
            u64::try_from(payload.len()).map_err(|_| SemanticPlaneRecordError::MetricsOverflow)?;
        self.receipts
            .record_admission(DurableSemanticObjectAdmission {
                identity: ProducedSemanticObjectIdentity::JumboLeaf {
                    id: leaf.id(),
                    ordinal: leaf.ordinal(),
                    byte_offset: leaf.byte_offset(),
                },
                object_id: admitted.id(),
                payload_bytes: byte_length,
                envelope_bytes: admitted.bytes(),
                created: admitted.created(),
            })
            .map_err(|error| SemanticPlaneRecordError::JumboObjectStore(error.to_string()))?;
        self.metrics
            .record(admitted, byte_length, 0, byte_length, 0)?;
        Ok(())
    }

    fn write_interior(&mut self, node: &JumboRopeNode) -> Result<(), Self::Error> {
        let payload = node.encode_wire();
        let key = ObjectKey::<SemanticJumboInteriorPayload>::from_value(payload.as_slice());
        let object = TypedObject::from_value(&key, payload.as_slice());
        let admitted = commit_and_read(self.store, &object, payload.as_slice())
            .map_err(SemanticPlaneRecordError::JumboObjectStore)?;
        let byte_length =
            u64::try_from(payload.len()).map_err(|_| SemanticPlaneRecordError::MetricsOverflow)?;
        self.receipts
            .record_admission(DurableSemanticObjectAdmission {
                identity: ProducedSemanticObjectIdentity::JumboInterior {
                    id: node.id(),
                    first_leaf: node.first_leaf(),
                    leaf_count: node.leaf_count(),
                },
                object_id: admitted.id(),
                payload_bytes: byte_length,
                envelope_bytes: admitted.bytes(),
                created: admitted.created(),
            })
            .map_err(|error| SemanticPlaneRecordError::JumboObjectStore(error.to_string()))?;
        self.metrics
            .record(admitted, byte_length, 0, 0, byte_length)?;
        Ok(())
    }
}

impl SemanticProducerStoreMetrics {
    fn record(
        &mut self,
        receipt: ObjectWriteReceipt,
        payload_bytes: u64,
        segment_hash_bytes: u64,
        jumbo_leaf_hash_bytes: u64,
        jumbo_interior_hash_bytes: u64,
    ) -> Result<(), SemanticPlaneRecordError> {
        self.attempted_objects = self
            .attempted_objects
            .checked_add(1)
            .ok_or(SemanticPlaneRecordError::MetricsOverflow)?;
        if receipt.created() {
            self.created_objects = self
                .created_objects
                .checked_add(1)
                .ok_or(SemanticPlaneRecordError::MetricsOverflow)?;
            self.newly_stored_envelope_bytes = self
                .newly_stored_envelope_bytes
                .checked_add(receipt.bytes())
                .ok_or(SemanticPlaneRecordError::MetricsOverflow)?;
            self.created_object_readback_envelope_bytes = self
                .created_object_readback_envelope_bytes
                .checked_add(receipt.bytes())
                .ok_or(SemanticPlaneRecordError::MetricsOverflow)?;
        } else {
            self.reused_objects = self
                .reused_objects
                .checked_add(1)
                .ok_or(SemanticPlaneRecordError::MetricsOverflow)?;
            self.cas_hit_compare_envelope_bytes = self
                .cas_hit_compare_envelope_bytes
                .checked_add(receipt.bytes())
                .ok_or(SemanticPlaneRecordError::MetricsOverflow)?;
        }
        self.semantic_segment_hash_bytes = self
            .semantic_segment_hash_bytes
            .checked_add(segment_hash_bytes)
            .ok_or(SemanticPlaneRecordError::MetricsOverflow)?;
        self.jumbo_leaf_hash_bytes = self
            .jumbo_leaf_hash_bytes
            .checked_add(jumbo_leaf_hash_bytes)
            .ok_or(SemanticPlaneRecordError::MetricsOverflow)?;
        self.jumbo_interior_hash_bytes = self
            .jumbo_interior_hash_bytes
            .checked_add(jumbo_interior_hash_bytes)
            .ok_or(SemanticPlaneRecordError::MetricsOverflow)?;
        self.payload_bytes = self
            .payload_bytes
            .checked_add(payload_bytes)
            .ok_or(SemanticPlaneRecordError::MetricsOverflow)?;
        self.admitted_envelope_bytes = self
            .admitted_envelope_bytes
            .checked_add(receipt.bytes())
            .ok_or(SemanticPlaneRecordError::MetricsOverflow)?;
        Ok(())
    }
}

fn commit_and_read(
    store: &FileStore,
    object: &TypedObject,
    expected_payload: &[u8],
) -> Result<ObjectWriteReceipt, String> {
    let receipt = store
        .write_object_with_receipt(object)
        .map_err(|error| format!("durably write semantic producer object: {error:?}"))?;
    if receipt.created() {
        let reopened = store
            .read_object(receipt.id())
            .map_err(|error| format!("read back semantic producer object: {error:?}"))?;
        if reopened.id() != receipt.id()
            || reopened.schema() != object.schema()
            || reopened.key() != object.key()
            || reopened.version() != object.version()
            || reopened.bytes() != expected_payload
        {
            return Err("FileStore read-back differs from the producer object".to_owned());
        }
    }
    // The CAS hit path has already opened and compared the complete encoded
    // envelope byte-for-byte inside write_object_with_receipt. Reading it a
    // second time here would add I/O without strengthening the admission.
    Ok(receipt)
}

struct SemanticJumboLeafPayload;

impl Schema for SemanticJumboLeafPayload {
    const DOMAIN: u8 = 0x52;
    const TYPE: u16 = 0xfff9;
    const VERSION: u8 = 1;
    type Value = [u8];

    fn encode(value: &Self::Value, output: &mut Vec<u8>) {
        output.extend_from_slice(value);
    }
}

struct SemanticJumboInteriorPayload;

impl Schema for SemanticJumboInteriorPayload {
    const DOMAIN: u8 = 0x52;
    const TYPE: u16 = 0xfffa;
    const VERSION: u8 = 1;
    type Value = [u8];

    fn encode(value: &Self::Value, output: &mut Vec<u8>) {
        output.extend_from_slice(value);
    }
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, BTreeSet, HashSet};
    use std::fs;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};

    use backend_semantic::ir::{
        BorrowedTree, CanonicalPlaneSegmentBoundaryPolicy, CorePayloadHash, DeclarationFamilyId,
        DocInput, DocumentationRows, EntityAuthorityFacts, EntityVersion, FactAvailability, Ir,
        IrBuilder, ItemKind, JUMBO_ROPE_MAX_LEAF_BYTES, JumboRopeObjectId, JumboRopeObjectSource,
        MAX_SEMANTIC_SEGMENT_BYTES, ParentageAuthority, ROPE_NODE_WIRE_BYTES, SemanticInputWitness,
        SemanticIrPlane, SemanticPlaneKind, SemanticPlaneSegment, TreeItemInput,
        VariantFingerprint, Visibility, encode_full_semantic_image, full_semantic_image_len,
        stream_canonical_plane_family_with_jumbo,
        stream_canonical_plane_family_with_jumbo_and_stable_key_anchors,
        verify_canonical_semantic_plane_segment_boundaries, verify_jumbo_plane_family_closures,
    };
    use backend_store::{ClosureCompositionBudget, ClosureMembershipChange};
    use backend_version::ScopeRoot;

    use super::*;

    const FIXTURE_ROWS: usize = 768;
    const SEGMENT_BYTES: usize = 2 * 1024;
    const ORDINAL_V1_CHUNK_BYTES: usize = 16 * 1024;

    fn boundary_policy() -> CanonicalPlaneSegmentBoundaryPolicy {
        CanonicalPlaneSegmentBoundaryPolicy::stable_key_hash_ramp(
            (SEGMENT_BYTES / 4) as u32,
            (SEGMENT_BYTES * 3 / 4) as u32,
            SEGMENT_BYTES as u32,
        )
        .expect("fixture stable-key boundary policy is valid")
    }
    const INSERTED_FIXTURE_INDEX: usize = usize::MAX;

    fn fixture_family_value(index: usize) -> u64 {
        if index == INSERTED_FIXTURE_INDEX {
            (FIXTURE_ROWS as u64 / 2) * 2 + 1
        } else {
            (index as u64 + 1) * 2
        }
    }

    static NEXT_TEMP_ID: AtomicU64 = AtomicU64::new(1);

    struct TestDirectory(PathBuf);

    impl TestDirectory {
        fn new() -> Self {
            let id = NEXT_TEMP_ID.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "semantic-producer-store-{}-{id}",
                std::process::id()
            ));
            fs::create_dir_all(&path).expect("test store directory is created");
            Self(path)
        }
    }

    impl Drop for TestDirectory {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    struct CapturedSegmentSink<'store, 'receipts> {
        durable: FileSemanticPlaneSegmentSink<'store, 'receipts, SemanticObjectAdmissionBuffer>,
        payloads: Vec<Vec<u8>>,
    }

    impl CanonicalSemanticPlaneSegmentSink for CapturedSegmentSink<'_, '_> {
        type Error = SemanticPlaneRecordError;

        fn write_segment(
            &mut self,
            segment: CanonicalSemanticPlaneSegmentRef<'_>,
        ) -> Result<(), Self::Error> {
            self.durable.write_segment(segment)?;
            self.payloads.push(segment.bytes().to_vec());
            Ok(())
        }
    }

    #[derive(Default)]
    struct CapturedLegacySegments {
        ids: Vec<[u8; 32]>,
        payloads: Vec<Vec<u8>>,
    }

    impl CanonicalSemanticPlaneSegmentSink for CapturedLegacySegments {
        type Error = SemanticPlaneRecordError;

        fn write_segment(
            &mut self,
            segment: CanonicalSemanticPlaneSegmentRef<'_>,
        ) -> Result<(), Self::Error> {
            let validated = segment.validate()?;
            self.ids.push(*validated.id().as_bytes());
            self.payloads.push(segment.bytes().to_vec());
            Ok(())
        }
    }

    struct DiscardJumboObjects;

    impl JumboRopeObjectSink for DiscardJumboObjects {
        type Error = SemanticPlaneRecordError;

        fn write_leaf(&mut self, _leaf: JumboRopeLeafRef<'_>) -> Result<(), Self::Error> {
            Ok(())
        }

        fn write_interior(&mut self, _node: &JumboRopeNode) -> Result<(), Self::Error> {
            Ok(())
        }
    }

    fn fixture(edit: Option<usize>, insert_at_middle: bool, delete_middle: bool) -> Ir {
        let mut indices = (0..FIXTURE_ROWS)
            .filter(|index| !(delete_middle && *index == FIXTURE_ROWS / 2))
            .collect::<Vec<_>>();
        if insert_at_middle {
            indices.insert(FIXTURE_ROWS / 2, INSERTED_FIXTURE_INDEX);
        }
        let names = indices
            .iter()
            .map(|index| {
                if *index == INSERTED_FIXTURE_INDEX {
                    "fixture_inserted_mid".to_owned()
                } else {
                    format!("fixture_{index:04}")
                }
            })
            .collect::<Vec<_>>();
        let docs = indices
            .iter()
            .map(|index| {
                if edit == Some(*index) {
                    "bravo semantic documentation ".repeat(8)
                } else if *index == 123 {
                    // Exercise the production content-defined jumbo rope path
                    // while keeping the changed regular row separate.
                    "jumbo semantic documentation ".repeat(40_000)
                } else if *index == INSERTED_FIXTURE_INDEX {
                    "inserted semantic documentation ".repeat(8)
                } else {
                    "alpha semantic documentation ".repeat(8)
                }
            })
            .collect::<Vec<_>>();
        let doc_inputs = docs
            .iter()
            .map(|text| [DocInput::Text(text.as_str())])
            .collect::<Vec<_>>();
        let versions = indices
            .iter()
            .map(|index| {
                let mut family = [0_u8; 16];
                family[8..].copy_from_slice(&fixture_family_value(*index).to_be_bytes());
                EntityVersion {
                    family: DeclarationFamilyId::from_raw(family),
                    variant: VariantFingerprint::from_raw([0x42; 16]),
                    core_payload: CorePayloadHash::from_raw([0x43; 16]),
                }
            })
            .collect::<Vec<_>>();
        let authority = EntityAuthorityFacts {
            parentage: ParentageAuthority::Root,
            members: FactAvailability::Captured,
            documentation: FactAvailability::Captured,
            visibility: FactAvailability::Captured,
            attributes: FactAvailability::Captured,
            ..EntityAuthorityFacts::default()
        };
        let items = indices
            .iter()
            .enumerate()
            .map(|(position, _)| TreeItemInput {
                name: names[position].as_bytes(),
                kind: ItemKind::Function,
                visibility: Visibility::Public,
                authority,
                parent: None,
                semantic_type: None,
                members: &[],
                docs: &doc_inputs[position],
                attributes: &[],
                source: None,
                extension: None,
            })
            .collect::<Vec<_>>();
        let mut builder = IrBuilder::new();
        builder
            .add_borrowed_tree(BorrowedTree {
                versions: &versions,
                items: &items,
                links: &[],
            })
            .expect("stable-key IR fixture is valid");
        builder.finish().expect("stable-key fixture IR is valid")
    }

    fn input() -> SemanticInputWitness {
        SemanticInputWitness::claimed([0xA1; 32], ScopeRoot::from_bytes([0xB2; 32]))
    }

    fn encode_and_persist<'store, 'receipts>(
        store: &'store FileStore,
        ir: &Ir,
        policy: CanonicalPlaneSegmentBoundaryPolicy,
        segment_receipts: &'receipts mut SemanticObjectAdmissionBuffer,
        jumbo_receipts: &mut SemanticObjectAdmissionBuffer,
    ) -> (
        backend_semantic::ir::CanonicalPlaneEncodingMetrics,
        SemanticProducerStoreMetrics,
        SemanticProducerStoreMetrics,
        Vec<Vec<u8>>,
        DurableSemanticObjectPin,
        DurableSemanticObjectPin,
    ) {
        let durable = FileSemanticPlaneSegmentSink::new(store, segment_receipts)
            .expect("pin producer segment objects against GC");
        let mut captured = CapturedSegmentSink {
            durable,
            payloads: Vec::new(),
        };
        let mut jumbo = FileSemanticJumboRopeSink::new(store, jumbo_receipts)
            .expect("pin producer rope objects against GC");
        let metrics = stream_canonical_plane_family_with_jumbo_and_stable_key_anchors(
            ir,
            &DocumentationRows,
            input(),
            policy,
            &mut jumbo,
            &mut captured,
        )
        .expect("canonical documentation rows persist to FileStore");
        let segment_store_metrics = captured.durable.metrics();
        let segment_pin = captured.durable.into_collection_pin();
        let rope_store_metrics = jumbo.metrics();
        let rope_pin = jumbo.into_collection_pin();
        (
            metrics,
            segment_store_metrics,
            rope_store_metrics,
            captured.payloads,
            segment_pin,
            rope_pin,
        )
    }

    fn capture_legacy_segments(ir: &Ir) -> CapturedLegacySegments {
        let mut segments = CapturedLegacySegments::default();
        let mut jumbo = DiscardJumboObjects;
        stream_canonical_plane_family_with_jumbo(
            ir,
            &DocumentationRows,
            input(),
            SEGMENT_BYTES,
            &mut jumbo,
            &mut segments,
        )
        .expect("existing prefix/size strategy emits legacy comparison segments");
        segments
    }

    fn captured_new_payload_bytes(
        base_ids: &HashSet<[u8; 32]>,
        target: &CapturedLegacySegments,
    ) -> u64 {
        target
            .ids
            .iter()
            .zip(&target.payloads)
            .filter(|(id, _)| !base_ids.contains(*id))
            .map(|(_, payload)| payload.len() as u64)
            .sum()
    }

    #[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
    struct StableSegmentChurn {
        new_payload_bytes: u64,
        new_segments: usize,
        removed_segments: usize,
    }

    fn segment_inventory(receipts: &[DurableSemanticObjectAdmission]) -> BTreeMap<[u8; 32], u64> {
        let mut inventory = BTreeMap::new();
        for receipt in receipts {
            if let ProducedSemanticObjectIdentity::Segment { id, .. } = receipt.identity() {
                if let Some(previous) = inventory.insert(*id.as_bytes(), receipt.payload_bytes()) {
                    assert_eq!(
                        previous,
                        receipt.payload_bytes(),
                        "one stable segment identity has one payload length"
                    );
                }
            }
        }
        inventory
    }

    fn stable_segment_churn(
        base: &[DurableSemanticObjectAdmission],
        target: &[DurableSemanticObjectAdmission],
    ) -> StableSegmentChurn {
        let base = segment_inventory(base);
        let target = segment_inventory(target);
        let mut churn = StableSegmentChurn::default();
        for (id, length) in &target {
            if !base.contains_key(id) {
                churn.new_payload_bytes = churn
                    .new_payload_bytes
                    .checked_add(*length)
                    .expect("bounded stable payload byte count fits u64");
                churn.new_segments += 1;
            }
        }
        churn.removed_segments = base.keys().filter(|id| !target.contains_key(*id)).count();
        churn
    }

    #[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
    struct MatchedOrdinalDelta {
        changed_payload_bytes: u64,
        hashed_bytes: u64,
        changed_chunks: usize,
        first_changed_ordinal: Option<usize>,
        last_changed_ordinal: Option<usize>,
    }

    fn matched_ordinal_delta(base: &[Vec<u8>], target: &[Vec<u8>]) -> MatchedOrdinalDelta {
        fn chunk_ids(payloads: &[Vec<u8>]) -> BTreeSet<(usize, [u8; 32])> {
            let total = payloads.iter().map(Vec::len).sum::<usize>();
            let mut bytes = Vec::with_capacity(total);
            for payload in payloads {
                bytes.extend_from_slice(payload);
            }
            bytes
                .chunks(ORDINAL_V1_CHUNK_BYTES)
                .enumerate()
                .map(|(ordinal, chunk)| {
                    let mut hasher = blake3::Hasher::new();
                    hasher.update(b"test.v1.ordinal-segment.v1\0");
                    hasher.update(&(ordinal as u64).to_be_bytes());
                    hasher.update(chunk);
                    (ordinal, *hasher.finalize().as_bytes())
                })
                .collect()
        }

        let base_length = base.iter().map(Vec::len).sum::<usize>();
        let base_chunks = chunk_ids(base);
        let total = target.iter().map(Vec::len).sum::<usize>();
        let mut bytes = Vec::with_capacity(total);
        for payload in target {
            bytes.extend_from_slice(payload);
        }
        let mut delta = MatchedOrdinalDelta {
            hashed_bytes: u64::try_from(base_length + total)
                .expect("bounded ordinal fixture bytes fit u64"),
            ..MatchedOrdinalDelta::default()
        };
        for (ordinal, chunk) in bytes.chunks(ORDINAL_V1_CHUNK_BYTES).enumerate() {
            let mut hasher = blake3::Hasher::new();
            hasher.update(b"test.v1.ordinal-segment.v1\0");
            hasher.update(&(ordinal as u64).to_be_bytes());
            hasher.update(chunk);
            if !base_chunks.contains(&(ordinal, *hasher.finalize().as_bytes())) {
                delta.changed_payload_bytes = delta
                    .changed_payload_bytes
                    .checked_add(chunk.len() as u64)
                    .expect("bounded changed ordinal payload bytes fit u64");
                delta.changed_chunks += 1;
                delta.first_changed_ordinal.get_or_insert(ordinal);
                delta.last_changed_ordinal = Some(ordinal);
            }
        }
        delta
    }

    fn v1_image(ir: &Ir) -> Vec<u8> {
        let length = full_semantic_image_len(ir).expect("plan V1 NXFI fixture image");
        let mut image = vec![0; length];
        encode_full_semantic_image(ir, &mut image).expect("encode V1 NXFI fixture image");
        image
    }

    fn v1_ordinal_metrics(base: &[u8], target: &[u8]) -> (u64, u64) {
        // Match the existing V1 history layout: one `Ir(Core)` byte segment
        // per MAX_SEMANTIC_SEGMENT_BYTES slice, keyed by its ordinal.
        fn segment_ids(image: &[u8]) -> Vec<(backend_semantic::ir::SemanticSegmentId, u64)> {
            image
                .chunks(MAX_SEMANTIC_SEGMENT_BYTES)
                .enumerate()
                .map(|(ordinal, chunk)| {
                    let ordinal = u64::try_from(ordinal).expect("small V1 chunk ordinal");
                    let mut key = [0; 32];
                    key[24..].copy_from_slice(&ordinal.to_be_bytes());
                    let segment =
                        backend_semantic::ir::SemanticPlaneSegment::from_payload_with_witness(
                            SemanticPlaneKind::Ir(SemanticIrPlane::Core),
                            key,
                            key,
                            1,
                            chunk,
                            input(),
                        )
                        .expect("admit production V1 ordinal NXFI segment");
                    (
                        segment
                            .admitted_id()
                            .expect("exact V1 chunk bytes were hashed"),
                        chunk.len() as u64,
                    )
                })
                .collect()
        }

        let before = segment_ids(base);
        let after = segment_ids(target);
        let changed_bytes = after
            .iter()
            .enumerate()
            .filter(|(ordinal, (id, _))| {
                before
                    .get(*ordinal)
                    .is_none_or(|(before_id, _)| before_id != id)
            })
            .map(|(_, (_, length))| *length)
            .sum();
        let hashed_bytes = u64::try_from(base.len() + target.len())
            .expect("bounded V1 fixture byte count fits u64");
        (changed_bytes, hashed_bytes)
    }

    /// Test-only CAS membership composer. Its result is not a family or
    /// aggregate publication admission; callers need those proofs separately.
    fn compose_object_closure(
        store: &FileStore,
        receipts: &[DurableSemanticObjectAdmission],
    ) -> backend_store::ClosureId {
        let mut objects = BTreeMap::<ObjectId, u64>::new();
        for receipt in receipts {
            match objects.insert(receipt.object_id(), receipt.payload_bytes()) {
                Some(previous) => assert_eq!(
                    previous,
                    receipt.payload_bytes(),
                    "one immutable object has one exact payload length"
                ),
                None => {}
            }
        }
        let changes = objects
            .keys()
            .copied()
            .map(ClosureMembershipChange::add)
            .collect::<Vec<_>>();
        let payload_bytes = objects.values().copied().sum::<u64>();
        let metadata_bytes = ClosureCompositionBudget::metadata_bytes_for(changes.len())
            .expect("bounded closure metadata charge");
        let budget = ClosureCompositionBudget::new(
            changes.len(),
            changes.len(),
            payload_bytes.max(1),
            metadata_bytes,
        );
        store
            .compose_closure_index(None, &changes, budget)
            .expect("compose durable producer object closure")
            .receipt()
            .closure()
    }

    struct ColdJumboSource<'store> {
        store: &'store FileStore,
        leaves: BTreeMap<JumboRopeObjectId, ObjectId>,
        interiors: BTreeMap<JumboRopeObjectId, ObjectId>,
    }

    impl JumboRopeObjectSource for ColdJumboSource<'_> {
        type Error = String;

        fn read_leaf(
            &mut self,
            id: JumboRopeObjectId,
            output: &mut [u8; JUMBO_ROPE_MAX_LEAF_BYTES],
        ) -> Result<Option<usize>, Self::Error> {
            let Some(object_id) = self.leaves.get(&id).copied() else {
                return Ok(None);
            };
            let object = self
                .store
                .read_object(object_id)
                .map_err(|error| format!("cold read jumbo leaf object: {error}"))?;
            if object.id() != object_id
                || object.schema() != ProducedSemanticObjectKind::JumboLeaf.schema_identity()
            {
                return Err("cold jumbo leaf object kind or ID mismatch".to_owned());
            }
            let bytes = object.bytes();
            if bytes.len() > output.len() {
                return Ok(Some(output.len() + 1));
            }
            output[..bytes.len()].copy_from_slice(bytes);
            Ok(Some(bytes.len()))
        }

        fn read_interior(
            &mut self,
            id: JumboRopeObjectId,
        ) -> Result<Option<[u8; ROPE_NODE_WIRE_BYTES]>, Self::Error> {
            let Some(object_id) = self.interiors.get(&id).copied() else {
                return Ok(None);
            };
            let object = self
                .store
                .read_object(object_id)
                .map_err(|error| format!("cold read jumbo interior object: {error}"))?;
            if object.id() != object_id
                || object.schema() != ProducedSemanticObjectKind::JumboInterior.schema_identity()
            {
                return Err("cold jumbo interior object kind or ID mismatch".to_owned());
            }
            let wire = object
                .bytes()
                .try_into()
                .map_err(|_| "cold jumbo interior has a non-canonical wire length".to_owned())?;
            Ok(Some(wire))
        }
    }

    fn reopen_and_verify_objects(
        store: &FileStore,
        receipts: &[DurableSemanticObjectAdmission],
        policy: CanonicalPlaneSegmentBoundaryPolicy,
        expected_rows: u64,
    ) -> backend_semantic::ir::VerifiedJumboPlaneClosure {
        let mut seen_physical = BTreeSet::new();
        let mut segment_descriptors = Vec::new();
        let mut segment_payloads = Vec::new();
        let mut leaves = BTreeMap::new();
        let mut interiors = BTreeMap::new();
        for receipt in receipts {
            match receipt.identity() {
                ProducedSemanticObjectIdentity::JumboLeaf { id, .. } => {
                    if let Some(previous) = leaves.insert(id, receipt.object_id()) {
                        assert_eq!(previous, receipt.object_id());
                    }
                }
                ProducedSemanticObjectIdentity::JumboInterior { id, .. } => {
                    if let Some(previous) = interiors.insert(id, receipt.object_id()) {
                        assert_eq!(previous, receipt.object_id());
                    }
                }
                ProducedSemanticObjectIdentity::Segment { .. } => {}
            }
            if !seen_physical.insert(receipt.object_id()) {
                continue;
            }
            let object = store
                .read_object(receipt.object_id())
                .expect("cold FileStore reopens every producer closure member");
            assert_eq!(object.id(), receipt.object_id());
            assert_eq!(object.schema(), receipt.identity().kind().schema_identity());
            assert_eq!(object.bytes().len() as u64, receipt.payload_bytes());
            if let ProducedSemanticObjectIdentity::Segment {
                family,
                id,
                first_key,
                last_key,
                row_count,
            } = receipt.identity()
            {
                assert_eq!(family, SemanticIrPlane::Documentation);
                let descriptor = SemanticPlaneSegment::from_payload(
                    SemanticPlaneKind::Ir(family),
                    first_key,
                    last_key,
                    row_count,
                    object.bytes(),
                )
                .expect("cold SPIR descriptor matches its stable range");
                assert_eq!(descriptor.admitted_id(), Some(id));
                segment_descriptors.push(descriptor);
                segment_payloads.push(object.bytes().to_vec());
            }
        }
        let payload_refs = segment_payloads
            .iter()
            .map(Vec::as_slice)
            .collect::<Vec<_>>();
        verify_canonical_semantic_plane_segment_boundaries(
            SemanticPlaneKind::Ir(SemanticIrPlane::Documentation),
            &segment_descriptors,
            &payload_refs,
            policy,
            expected_rows,
        )
        .expect("cold reader verifies canonical cuts across the complete family");
        let mut source = ColdJumboSource {
            store,
            leaves,
            interiors,
        };
        verify_jumbo_plane_family_closures(
            SemanticPlaneKind::Ir(SemanticIrPlane::Documentation),
            &segment_descriptors,
            &payload_refs,
            &mut source,
        )
        .expect("cold SPIR and jumbo roots re-admit through strict family closure verification")
    }

    #[test]
    fn real_reader_edits_store_stable_spir_and_rope_objects_with_locality_metrics() {
        // This proof writes only DocumentationRows on the stable side. The
        // V1 baseline is its full NXFI Core image; these bytes establish
        // plane-local physical locality, not a complete seven-family V2
        // generation write-set or end-to-end compiler speedup.
        let directory = TestDirectory::new();
        let cas_path = directory.0.join("cas");
        let store = FileStore::open(&cas_path, 4 * 1024 * 1024).expect("open local FileStore");
        let policy = boundary_policy();
        let base_ir = fixture(None, false, false);
        let base_v1_image = v1_image(&base_ir);
        let base_legacy = capture_legacy_segments(&base_ir);
        let mut base_segments = SemanticObjectAdmissionBuffer::default();
        let mut base_jumbo = SemanticObjectAdmissionBuffer::default();
        let (
            base_encoding,
            base_store_metrics,
            base_rope_metrics,
            base_spir,
            _base_segment_pin,
            _base_rope_pin,
        ) = encode_and_persist(
            &store,
            &base_ir,
            policy,
            &mut base_segments,
            &mut base_jumbo,
        );
        assert!(base_rope_metrics.created_objects() > 0);
        assert!(base_rope_metrics.jumbo_leaf_hash_bytes() > 0);
        assert!(base_rope_metrics.jumbo_interior_hash_bytes() > 0);
        assert_eq!(
            base_rope_metrics.payload_bytes(),
            base_rope_metrics.jumbo_leaf_hash_bytes()
                + base_rope_metrics.jumbo_interior_hash_bytes(),
            "rope hash byte counters reconcile with exact leaf and interior payload lengths"
        );
        assert!(
            base_spir
                .iter()
                .all(|payload| payload.len() <= SEGMENT_BYTES)
        );
        assert!(base_encoding.peak_segment_scratch_capacity_bytes() <= SEGMENT_BYTES as u64);
        assert_eq!(
            base_store_metrics.semantic_segment_hash_bytes(),
            base_encoding.output_bytes(),
            "local segment validation hashes each exact SPIR byte once"
        );
        assert_eq!(
            base_store_metrics.admitted_envelope_bytes(),
            base_store_metrics.cas_hit_compare_envelope_bytes()
                + base_store_metrics.created_object_readback_envelope_bytes(),
            "base envelope I/O counters account for each compare or created-object readback"
        );
        assert_eq!(
            base_encoding.anchor_hash_rows() * 32,
            base_encoding.anchor_key_hash_bytes(),
            "base boundary-key hash counters include every hash exactly once"
        );
        println!(
            "base: boundary_policy=stable-key-hash-ramp-v1 minimum_segment_bytes={} target_segment_bytes={} maximum_segment_bytes={} v1_image_bytes={} spir_payload_bytes={} segment_hash_bytes={} anchor_hash_rows={} anchor_key_hash_bytes={} rope_leaf_hash_bytes={} rope_interior_hash_bytes={} rope_new_envelopes={} segment_scratch_peak={} row_index_capacity={} tracked_pass_memory_peak={}",
            policy.minimum_bytes(),
            policy.target_bytes(),
            policy.maximum_bytes(),
            base_v1_image.len(),
            base_encoding.output_bytes(),
            base_store_metrics.semantic_segment_hash_bytes(),
            base_encoding.anchor_hash_rows(),
            base_encoding.anchor_key_hash_bytes(),
            base_rope_metrics.jumbo_leaf_hash_bytes(),
            base_rope_metrics.jumbo_interior_hash_bytes(),
            base_rope_metrics.newly_stored_envelope_bytes(),
            base_encoding.peak_segment_scratch_capacity_bytes(),
            base_encoding.row_index_capacity_bytes(),
            base_encoding.peak_tracked_scratch_upper_bound_bytes(),
        );

        let mut base_all = base_segments.admissions().to_vec();
        base_all.extend_from_slice(base_jumbo.admissions());
        let base_root = compose_object_closure(&store, &base_all);
        let duplicate_receipt = base_jumbo
            .admissions()
            .iter()
            .find(|receipt| {
                matches!(
                    receipt.identity(),
                    ProducedSemanticObjectIdentity::JumboLeaf { .. }
                )
            })
            .copied()
            .expect("fixture contains a jumbo leaf receipt");
        let mut duplicate_receipts = base_all.clone();
        duplicate_receipts.push(duplicate_receipt);
        assert_eq!(
            compose_object_closure(&store, &duplicate_receipts),
            base_root,
            "duplicate occurrence receipts preserve unique physical closure membership"
        );
        let cold_store = FileStore::open(&cas_path, 4 * 1024 * 1024)
            .expect("cold reopen FileStore without producer state");
        assert_eq!(
            cold_store
                .open_closure(base_root)
                .expect("reopen producer payload closure")
                .id(),
            base_root,
            "closure root survives a cold FileStore reopen"
        );
        let cold_family =
            reopen_and_verify_objects(&cold_store, &base_all, policy, base_encoding.row_count());
        assert_eq!(cold_family.family(), SemanticIrPlane::Documentation);
        assert_eq!(cold_family.segment_count(), base_encoding.segment_count());
        assert_eq!(cold_family.jumbo_value_count(), 1);

        let cases = [
            ("early edit", Some(0), false, false),
            ("middle edit", Some(FIXTURE_ROWS / 2 - 1), false, false),
            ("tail edit", Some(FIXTURE_ROWS - 1), false, false),
            ("middle insert", None, true, false),
            ("middle delete", None, false, true),
        ];
        for (label, edit, insert, delete) in cases {
            let target_ir = fixture(edit, insert, delete);
            let target_v1_image = v1_image(&target_ir);
            let target_legacy = capture_legacy_segments(&target_ir);
            let mut target_segments = SemanticObjectAdmissionBuffer::default();
            let mut target_jumbo = SemanticObjectAdmissionBuffer::default();
            let (
                encoding,
                storage,
                rope_storage,
                target_spir,
                _target_segment_pin,
                _target_rope_pin,
            ) = encode_and_persist(
                &store,
                &target_ir,
                policy,
                &mut target_segments,
                &mut target_jumbo,
            );
            let stable_churn =
                stable_segment_churn(base_segments.admissions(), target_segments.admissions());
            let stable_bytes = stable_churn.new_payload_bytes;
            let legacy_base_ids = base_legacy.ids.iter().copied().collect::<HashSet<_>>();
            let legacy_stable_bytes = captured_new_payload_bytes(&legacy_base_ids, &target_legacy);
            let ordinal_delta =
                matched_ordinal_delta(&base_legacy.payloads, &target_legacy.payloads);
            let ordinal_bytes = ordinal_delta.changed_payload_bytes;
            let (v1_bytes, v1_hashed_bytes) = v1_ordinal_metrics(&base_v1_image, &target_v1_image);
            let stable_hashed_bytes = base_store_metrics.semantic_segment_hash_bytes()
                + storage.semantic_segment_hash_bytes();
            let stable_anchor_hash_rows =
                base_encoding.anchor_hash_rows() + encoding.anchor_hash_rows();
            let stable_anchor_hash_bytes =
                base_encoding.anchor_key_hash_bytes() + encoding.anchor_key_hash_bytes();
            assert_eq!(
                stable_anchor_hash_rows * 32,
                stable_anchor_hash_bytes,
                "base plus target stable-key hash metrics are symmetric and exact"
            );
            assert!(
                stable_bytes < ordinal_bytes,
                "{label}: stable-key new documentation-plane payload bytes {stable_bytes} should beat matched 16 KiB ordinal bytes {ordinal_bytes}"
            );
            if insert || delete {
                assert!(
                    stable_bytes.saturating_mul(2) < legacy_stable_bytes,
                    "{label}: stable-key-ramp bytes {stable_bytes} should be at least 2x below existing prefix/size bytes {legacy_stable_bytes}"
                );
            }
            assert!(
                stable_bytes < v1_bytes,
                "{label}: stable-key new documentation-plane payload bytes {stable_bytes} should beat V1 ordinal NXFI bytes {v1_bytes}"
            );
            assert!(
                storage.newly_stored_envelope_bytes() > 0,
                "{label}: changed segment is durably written"
            );
            assert_eq!(
                storage.newly_stored_envelope_bytes(),
                target_segments
                    .admissions()
                    .iter()
                    .filter(|receipt| receipt.created())
                    .map(|receipt| receipt.envelope_bytes())
                    .sum::<u64>(),
                "{label}: FileStore byte metrics reconcile with durable segment receipts"
            );
            assert_eq!(
                storage.semantic_segment_hash_bytes(),
                encoding.output_bytes(),
                "{label}: counters expose the full reader scan and family hashing"
            );
            assert_eq!(target_spir.len() as u64, encoding.segment_count());
            assert!(
                target_spir
                    .iter()
                    .all(|payload| payload.len() <= policy.maximum_bytes())
            );
            assert_eq!(
                storage.admitted_envelope_bytes(),
                storage.cas_hit_compare_envelope_bytes()
                    + storage.created_object_readback_envelope_bytes(),
                "{label}: account for existing-envelope comparisons and created-object readbacks"
            );
            assert_eq!(
                rope_storage.created_objects(),
                0,
                "{label}: jumbo rope objects reuse"
            );
            assert!(
                rope_storage.reused_objects() > 0,
                "{label}: rope closure is reused"
            );
            assert_eq!(
                rope_storage.jumbo_leaf_hash_bytes(),
                base_rope_metrics.jumbo_leaf_hash_bytes(),
                "{label}: unchanged jumbo value is still streamed and rehashed"
            );
            assert_eq!(
                rope_storage.jumbo_interior_hash_bytes(),
                base_rope_metrics.jumbo_interior_hash_bytes(),
                "{label}: unchanged jumbo interior identities are still recomputed once"
            );
            assert!(encoding.peak_segment_scratch_capacity_bytes() <= SEGMENT_BYTES as u64);
            if label == "middle insert" {
                let midpoint_value = fixture_family_value(FIXTURE_ROWS / 2 - 1);
                let inserted_value = fixture_family_value(INSERTED_FIXTURE_INDEX);
                let next_value = fixture_family_value(FIXTURE_ROWS / 2);
                assert!(midpoint_value < inserted_value && inserted_value < next_value);
                let base_stream_bytes = base_legacy.payloads.iter().map(Vec::len).sum::<usize>();
                let midpoint_ordinal = (base_stream_bytes / 2) / ORDINAL_V1_CHUNK_BYTES;
                assert!(ordinal_delta.changed_chunks > 1);
                assert!(
                    ordinal_delta
                        .first_changed_ordinal
                        .is_some_and(|ordinal| ordinal <= midpoint_ordinal)
                );
                assert!(
                    ordinal_delta
                        .last_changed_ordinal
                        .is_some_and(|ordinal| ordinal > midpoint_ordinal)
                );
            }
            println!(
                "{label}: stable_key_ramp_new_payload_bytes={stable_bytes} stable_new_segments={} stable_removed_segments={} stable_segment_churn={} existing_prefix_size_new_payload={legacy_stable_bytes} stable_base_plus_target_segment_hash_bytes={stable_hashed_bytes} stable_base_plus_target_anchor_hash_rows={stable_anchor_hash_rows} stable_base_plus_target_anchor_key_hash_bytes={stable_anchor_hash_bytes} matched_ordinal_16k_payload={ordinal_bytes} matched_ordinal_hash_bytes={} ordinal_changed_chunks={} ordinal_changed_range={:?}..{:?} v1_ordinal_nxfi_payload={v1_bytes} v1_ordinal_hash_bytes={v1_hashed_bytes} stable_target_segment_hash_bytes={} stable_target_anchor_hash_rows={} stable_target_anchor_key_hash_bytes={} newly_stored_segment_envelopes={} compared_envelope_bytes={} created_readback_envelope_bytes={} rope_leaf_hash_bytes={} rope_interior_hash_bytes={} segment_scratch_peak={} row_index_capacity={} tracked_pass_memory_peak={}",
                stable_churn.new_segments,
                stable_churn.removed_segments,
                stable_churn.new_segments + stable_churn.removed_segments,
                ordinal_delta.hashed_bytes,
                ordinal_delta.changed_chunks,
                ordinal_delta.first_changed_ordinal,
                ordinal_delta.last_changed_ordinal,
                storage.semantic_segment_hash_bytes(),
                encoding.anchor_hash_rows(),
                encoding.anchor_key_hash_bytes(),
                storage.newly_stored_envelope_bytes(),
                storage.cas_hit_compare_envelope_bytes(),
                storage.created_object_readback_envelope_bytes(),
                rope_storage.jumbo_leaf_hash_bytes(),
                rope_storage.jumbo_interior_hash_bytes(),
                encoding.peak_segment_scratch_capacity_bytes(),
                encoding.row_index_capacity_bytes(),
                encoding.peak_tracked_scratch_upper_bound_bytes(),
            );
        }

        let second_root = compose_object_closure(&cold_store, &base_all);
        assert_eq!(
            second_root, base_root,
            "same exact payload set has same root"
        );
        let reopened = cold_store
            .read_closure(base_root)
            .expect("cold read exact SPIR and rope closure");
        assert_eq!(reopened.id(), base_root);
        assert_eq!(
            reopened.object_count() as usize,
            base_all
                .iter()
                .copied()
                .map(DurableSemanticObjectAdmission::object_id)
                .collect::<HashSet<_>>()
                .len()
        );
        assert!(base_all.iter().any(|receipt| matches!(
            receipt.identity(),
            ProducedSemanticObjectIdentity::JumboLeaf { .. }
        )));
        assert!(base_all.iter().any(|receipt| matches!(
            receipt.identity(),
            ProducedSemanticObjectIdentity::JumboInterior { .. }
        )));
        assert!(base_all.iter().any(|receipt| matches!(
            receipt.identity(),
            ProducedSemanticObjectIdentity::Segment {
                family: SemanticIrPlane::Documentation,
                ..
            }
        )));
    }
}
