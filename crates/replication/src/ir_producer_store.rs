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

use std::collections::BTreeMap;
use std::fmt;

use backend_semantic::ir::{
    CanonicalPlaneEncodingMetrics, CanonicalPlaneRowEncoder, CanonicalSemanticPlaneSegmentRef,
    CanonicalSemanticPlaneSegmentSink, CoreDeclarationRows, DocumentationRows,
    JUMBO_ROPE_MAX_LEAF_BYTES, JumboRopeLeafRef, JumboRopeLimits, JumboRopeNode, JumboRopeObjectId,
    JumboRopeObjectSink, JumboRopeObjectSource, LanguageExtensionRows, OccurrenceRows,
    ROPE_NODE_WIRE_BYTES, RelationRows, SemanticBuildIdentity, SemanticGenerationProofError,
    SemanticImageFacts, SemanticInputWitness, SemanticIrPlane, SemanticPlaneKind,
    SemanticPlaneRecordError, SemanticPlaneSegmentBoundaryPolicy, SemanticReader,
    SemanticTypedPlaneFamilyDescriptorV2, SemanticTypedPlaneManifestV2,
    SemanticTypedPlaneManifestV2Error, SemanticTypedPlaneSegmentClaimV2,
    SemanticTypedPlaneVerificationTierV2, SemanticTypedPlaneWorkLimitsV2, SourceProvenanceRows,
    TypedPlaneSegmentSourceV2, TypesRows, UntrustedSemanticContentRootV2,
    UntrustedSemanticGenerationRootV2, UntrustedSemanticSegmentId,
    ValidatedCanonicalSemanticPlaneSegment, VerifiedTypedPlaneContentV2,
    derive_typed_plane_content_v2_from_admitted_reader,
    stream_canonical_plane_family_with_jumbo_and_stable_key_anchors,
    stream_canonical_plane_family_with_jumbo_stable_key_anchors_and_limits,
    typed_plane_work_limits_v2,
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

    /// Checks and reserves one upcoming receipt before its immutable object is
    /// written. Implementations may reject a producer budget here. The
    /// default preserves compatibility for sinks that cannot preflight.
    fn prepare_admission(
        &mut self,
        _identity: ProducedSemanticObjectIdentity,
        _payload_bytes: u64,
    ) -> Result<(), Self::Error> {
        Ok(())
    }

    /// Releases a preflight reservation when the object write itself fails.
    fn cancel_admission(&mut self) {}

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

/// A receipt sink whose allocation and object work are capped by the same
/// workload limits used by cold V2 verification.
struct V3SegmentAdmissionBuilder {
    limits: SemanticTypedPlaneWorkLimitsV2,
    admissions: Vec<DurableSemanticObjectAdmission>,
    total_bytes: u64,
    total_rows: u64,
    poisoned: bool,
    pending: Option<(ProducedSemanticObjectIdentity, u64)>,
}

impl V3SegmentAdmissionBuilder {
    fn new(limits: SemanticTypedPlaneWorkLimitsV2) -> Self {
        Self {
            limits,
            admissions: Vec::new(),
            total_bytes: 0,
            total_rows: 0,
            poisoned: false,
            pending: None,
        }
    }

    fn admissions(&self) -> &[DurableSemanticObjectAdmission] {
        &self.admissions
    }

    fn into_admissions(self) -> Vec<DurableSemanticObjectAdmission> {
        self.admissions
    }

    fn remaining_rows(&self) -> u64 {
        self.limits.max_total_rows().saturating_sub(self.total_rows)
    }
}

impl SemanticObjectAdmissionSink for V3SegmentAdmissionBuilder {
    type Error = String;

    fn prepare_admission(
        &mut self,
        identity: ProducedSemanticObjectIdentity,
        payload_bytes: u64,
    ) -> Result<(), Self::Error> {
        if self.poisoned {
            return Err("segment receipt builder cannot continue after a sink error".to_owned());
        }
        let result = (|| {
            if self.pending.is_some() {
                return Err("segment receipt reservation is already active".to_owned());
            }
            let ProducedSemanticObjectIdentity::Segment { row_count, .. } = identity else {
                return Err("segment sink received a non-segment identity".to_owned());
            };
            let next_bytes = self
                .total_bytes
                .checked_add(payload_bytes)
                .ok_or_else(|| "typed V2 segment bytes overflow".to_owned())?;
            let next_rows = self
                .total_rows
                .checked_add(u64::from(row_count))
                .ok_or_else(|| "typed V2 segment rows overflow".to_owned())?;
            if self.admissions.len() >= self.limits.max_segments()
                || next_bytes > self.limits.max_total_bytes()
                || next_rows > self.limits.max_total_rows()
            {
                return Err("typed V2 segment budget exceeded before storage write".to_owned());
            }
            self.admissions
                .try_reserve(1)
                .map_err(|error| format!("reserve typed V2 segment receipt: {error}"))?;
            self.pending = Some((identity, payload_bytes));
            Ok(())
        })();
        if result.is_err() {
            self.pending = None;
            self.poisoned = true;
        }
        result
    }

    fn cancel_admission(&mut self) {
        self.pending = None;
        self.poisoned = true;
    }

    fn record_admission(
        &mut self,
        admission: DurableSemanticObjectAdmission,
    ) -> Result<(), Self::Error> {
        if self.poisoned {
            return Err("segment receipt builder cannot continue after a sink error".to_owned());
        }
        let result = (|| {
            let (identity, payload_bytes) = self
                .pending
                .take()
                .ok_or_else(|| "segment receipt has no matching reservation".to_owned())?;
            if identity != admission.identity() || payload_bytes != admission.payload_bytes() {
                return Err("segment receipt differs from its reservation".to_owned());
            }
            let ProducedSemanticObjectIdentity::Segment { row_count, .. } = identity else {
                return Err("segment receipt reservation has a non-segment identity".to_owned());
            };
            self.total_bytes += payload_bytes;
            self.total_rows += u64::from(row_count);
            self.admissions.push(admission);
            Ok(())
        })();
        if result.is_err() {
            self.pending = None;
            self.poisoned = true;
        }
        result
    }
}

#[derive(Clone, Copy, Debug, Default)]
struct ActiveJumboValue {
    value_bytes: u64,
    leaves: u64,
    interiors: u64,
    admissions_start: usize,
    total_value_bytes_start: u64,
    total_leaves_start: u64,
    total_object_reads_start: u64,
    total_read_bytes_start: u64,
}

/// Jumbo receipt sink that reserves receipt capacity and cold-read work before
/// a leaf or interior object reaches FileStore.
struct V3JumboAdmissionBuilder {
    work_limits: SemanticTypedPlaneWorkLimitsV2,
    rope_limits: JumboRopeLimits,
    admissions: Vec<DurableSemanticObjectAdmission>,
    total_value_bytes: u64,
    total_leaves: u64,
    total_object_reads: u64,
    total_read_bytes: u64,
    active_value: Option<ActiveJumboValue>,
    poisoned: bool,
    pending: Option<(ProducedSemanticObjectIdentity, u64)>,
}

/// Per-run ceilings sourced from the exact verifier tier and the caller's
/// accepted per-rope policy.
#[derive(Clone, Copy)]
struct V3ProductionBudget {
    work_limits: SemanticTypedPlaneWorkLimitsV2,
    rope_limits: JumboRopeLimits,
}

impl V3ProductionBudget {
    fn new(
        tier: SemanticTypedPlaneVerificationTierV2,
        rope_limits: JumboRopeLimits,
    ) -> Result<Self, String> {
        if rope_limits.max_admissible_leaf_count() == 0 {
            return Err("typed V3 jumbo limits admit no leaf receipts".to_owned());
        }
        Ok(Self {
            work_limits: typed_plane_work_limits_v2(tier),
            rope_limits,
        })
    }

    fn segment_admissions(self) -> V3SegmentAdmissionBuilder {
        V3SegmentAdmissionBuilder::new(self.work_limits)
    }

    fn jumbo_admissions(self) -> Result<V3JumboAdmissionBuilder, String> {
        V3JumboAdmissionBuilder::new(self.work_limits, self.rope_limits)
    }
}

impl V3JumboAdmissionBuilder {
    fn new(
        work_limits: SemanticTypedPlaneWorkLimitsV2,
        rope_limits: JumboRopeLimits,
    ) -> Result<Self, String> {
        if rope_limits.max_admissible_leaf_count() == 0 {
            return Err("typed V3 jumbo limits admit no leaf receipts".to_owned());
        }
        Ok(Self {
            work_limits,
            rope_limits,
            admissions: Vec::new(),
            total_value_bytes: 0,
            total_leaves: 0,
            total_object_reads: 0,
            total_read_bytes: 0,
            active_value: None,
            poisoned: false,
            pending: None,
        })
    }

    fn admissions(&self) -> &[DurableSemanticObjectAdmission] {
        &self.admissions
    }

    fn into_admissions(self) -> Vec<DurableSemanticObjectAdmission> {
        self.admissions
    }

    fn reserve_receipt(&mut self) -> Result<(), String> {
        self.admissions
            .try_reserve(1)
            .map_err(|error| format!("reserve typed V3 jumbo receipt: {error}"))
    }

    fn rollback_active_value(&mut self) {
        if let Some(active) = self.active_value.take() {
            self.admissions.truncate(active.admissions_start);
            self.total_value_bytes = active.total_value_bytes_start;
            self.total_leaves = active.total_leaves_start;
            self.total_object_reads = active.total_object_reads_start;
            self.total_read_bytes = active.total_read_bytes_start;
        }
        self.poisoned = true;
    }

    fn check_global_jumbo_budget(
        &self,
        total_value_bytes: u64,
        total_leaves: u64,
        total_object_reads: u64,
        total_read_bytes: u64,
    ) -> Result<(), String> {
        if total_value_bytes > self.work_limits.max_total_jumbo_value_bytes()
            || total_leaves > self.work_limits.max_total_jumbo_leaves()
            || total_object_reads > self.work_limits.max_total_jumbo_object_reads()
            || total_read_bytes > self.work_limits.max_total_jumbo_read_bytes()
        {
            return Err("typed V2 jumbo verifier budget exceeded before storage write".to_owned());
        }
        Ok(())
    }
}

impl SemanticObjectAdmissionSink for V3JumboAdmissionBuilder {
    type Error = String;

    fn prepare_admission(
        &mut self,
        identity: ProducedSemanticObjectIdentity,
        payload_bytes: u64,
    ) -> Result<(), Self::Error> {
        if self.poisoned {
            return Err(
                "jumbo receipt builder cannot continue after a failed object write".to_owned(),
            );
        }
        let result = (|| {
            if self.pending.is_some() {
                return Err("jumbo receipt reservation is already active".to_owned());
            }
            let previous_value = self.active_value;
            let next_value;
            match identity {
                ProducedSemanticObjectIdentity::JumboLeaf {
                    ordinal,
                    byte_offset,
                    ..
                } => {
                    let before = if ordinal == 0 {
                        if let Some(previous) = previous_value {
                            if previous.leaves == 0
                                || previous.interiors.saturating_add(1) != previous.leaves
                            {
                                return Err("previous jumbo value receipt sequence is incomplete"
                                    .to_owned());
                            }
                        }
                        let fresh = ActiveJumboValue {
                            admissions_start: self.admissions.len(),
                            total_value_bytes_start: self.total_value_bytes,
                            total_leaves_start: self.total_leaves,
                            total_object_reads_start: self.total_object_reads,
                            total_read_bytes_start: self.total_read_bytes,
                            ..ActiveJumboValue::default()
                        };
                        fresh
                    } else {
                        previous_value.ok_or_else(|| "jumbo leaf has no active value".to_owned())?
                    };
                    if ordinal != before.leaves || byte_offset != before.value_bytes {
                        return Err(
                            "jumbo leaf ordinal or byte offset is not contiguous".to_owned()
                        );
                    }
                    let value_bytes = before
                        .value_bytes
                        .checked_add(payload_bytes)
                        .ok_or_else(|| "jumbo value byte count overflows".to_owned())?;
                    if payload_bytes == 0 || payload_bytes > JUMBO_ROPE_MAX_LEAF_BYTES as u64 {
                        return Err(
                            "jumbo leaf payload length is outside the writer bound".to_owned()
                        );
                    }
                    let leaves = before
                        .leaves
                        .checked_add(1)
                        .ok_or_else(|| "jumbo leaf count overflows".to_owned())?;
                    if value_bytes > self.rope_limits.max_value_bytes
                        || leaves > self.rope_limits.max_admissible_leaf_count()
                    {
                        return Err(
                            "caller jumbo rope limits exceeded before storage write".to_owned()
                        );
                    }
                    let projected_value = ActiveJumboValue {
                        value_bytes,
                        leaves,
                        interiors: before.interiors,
                        ..before
                    };
                    let current_reads = self
                        .total_object_reads
                        .checked_add(1)
                        .ok_or_else(|| "jumbo object read count overflows".to_owned())?;
                    // RopeWriter combines exactly two spans per interior node and
                    // finishes by reducing the frontier to one root. Thus a
                    // completed N-leaf rope has exactly N-1 interior objects;
                    // this is the exact count still required if the current
                    // prefix ended here, not a fanout-based upper bound.
                    let nodes_for_projected_prefix = leaves
                        .checked_sub(1)
                        .ok_or_else(|| "jumbo leaf count is invalid".to_owned())?;
                    let future_nodes = nodes_for_projected_prefix
                        .checked_sub(before.interiors)
                        .ok_or_else(|| "jumbo interior count exceeds its binary tree".to_owned())?;
                    let projected_reads = current_reads
                        .checked_add(future_nodes)
                        .ok_or_else(|| "jumbo projected object reads overflow".to_owned())?;
                    let projected_read_bytes = self
                        .total_read_bytes
                        .checked_add(payload_bytes)
                        .and_then(|bytes| {
                            future_nodes
                                .checked_mul(ROPE_NODE_WIRE_BYTES as u64)
                                .and_then(|nodes| bytes.checked_add(nodes))
                        })
                        .ok_or_else(|| "jumbo projected read bytes overflow".to_owned())?;
                    self.check_global_jumbo_budget(
                        self.total_value_bytes
                            .checked_add(payload_bytes)
                            .ok_or_else(|| "jumbo value bytes overflow".to_owned())?,
                        self.total_leaves
                            .checked_add(1)
                            .ok_or_else(|| "jumbo leaf count overflows".to_owned())?,
                        projected_reads,
                        projected_read_bytes,
                    )?;
                    next_value = Some(projected_value);
                }
                ProducedSemanticObjectIdentity::JumboInterior {
                    first_leaf,
                    leaf_count,
                    ..
                } => {
                    let before = previous_value
                        .ok_or_else(|| "jumbo interior has no active value".to_owned())?;
                    if before.leaves == 0
                        || payload_bytes != ROPE_NODE_WIRE_BYTES as u64
                        || before.interiors >= before.leaves.saturating_sub(1)
                        || before.interiors
                            >= self
                                .rope_limits
                                .max_admissible_leaf_count()
                                .saturating_sub(1)
                        || leaf_count == 0
                        || first_leaf
                            .checked_add(leaf_count)
                            .is_none_or(|end| end > before.leaves)
                    {
                        return Err("jumbo interior receipt is outside its active value".to_owned());
                    }
                    self.check_global_jumbo_budget(
                        self.total_value_bytes,
                        self.total_leaves,
                        self.total_object_reads
                            .checked_add(1)
                            .ok_or_else(|| "jumbo object read count overflows".to_owned())?,
                        self.total_read_bytes
                            .checked_add(payload_bytes)
                            .ok_or_else(|| "jumbo read bytes overflow".to_owned())?,
                    )?;
                    next_value = Some(ActiveJumboValue {
                        interiors: before
                            .interiors
                            .checked_add(1)
                            .ok_or_else(|| "jumbo interior count overflows".to_owned())?,
                        ..before
                    });
                }
                ProducedSemanticObjectIdentity::Segment { .. } => {
                    return Err("jumbo sink received a segment identity".to_owned());
                }
            }
            self.reserve_receipt()?;
            self.active_value = next_value;
            self.pending = Some((identity, payload_bytes));
            Ok(())
        })();
        if result.is_err() {
            self.pending = None;
            self.rollback_active_value();
        }
        result
    }

    fn cancel_admission(&mut self) {
        self.pending = None;
        self.rollback_active_value();
    }

    fn record_admission(
        &mut self,
        admission: DurableSemanticObjectAdmission,
    ) -> Result<(), Self::Error> {
        if self.poisoned {
            return Err("jumbo receipt builder cannot continue after a sink error".to_owned());
        }
        let result = (|| {
            let Some((identity, payload_bytes)) = self.pending.take() else {
                return Err("jumbo receipt has no matching reservation".to_owned());
            };
            if identity != admission.identity() || payload_bytes != admission.payload_bytes() {
                return Err("jumbo receipt differs from its reservation".to_owned());
            }
            match identity {
                ProducedSemanticObjectIdentity::JumboLeaf { .. } => {
                    self.total_value_bytes += payload_bytes;
                    self.total_leaves += 1;
                }
                ProducedSemanticObjectIdentity::JumboInterior { .. } => {}
                ProducedSemanticObjectIdentity::Segment { .. } => {
                    return Err("jumbo receipt reservation has a segment identity".to_owned());
                }
            }
            self.total_object_reads += 1;
            self.total_read_bytes += payload_bytes;
            self.admissions.push(admission);
            Ok(())
        })();
        if result.is_err() {
            self.pending = None;
            self.rollback_active_value();
        }
        result
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

/// Exact FileStore payload and envelope bytes reopened by the V3 semantic
/// verifier after production. This measures verifier I/O only; it does not
/// count the full reader traversal used to discover and encode canonical rows.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct SemanticProducerVerifierIoMetrics {
    segment_object_reads: u64,
    segment_payload_bytes: u64,
    segment_envelope_bytes: u64,
    jumbo_leaf_object_reads: u64,
    jumbo_interior_object_reads: u64,
    jumbo_payload_bytes: u64,
    jumbo_envelope_bytes: u64,
}

impl SemanticProducerVerifierIoMetrics {
    /// Number of c004 segment FileStore reads made by semantic verification.
    #[must_use]
    pub const fn segment_object_reads(self) -> u64 {
        self.segment_object_reads
    }

    /// Exact c004 payload bytes returned to semantic verification.
    #[must_use]
    pub const fn segment_payload_bytes(self) -> u64 {
        self.segment_payload_bytes
    }

    /// Exact durable c004 envelope bytes reopened for semantic verification.
    #[must_use]
    pub const fn segment_envelope_bytes(self) -> u64 {
        self.segment_envelope_bytes
    }

    /// Number of jumbo leaf FileStore reads made by semantic verification.
    #[must_use]
    pub const fn jumbo_leaf_object_reads(self) -> u64 {
        self.jumbo_leaf_object_reads
    }

    /// Number of jumbo interior-node FileStore reads made by semantic verification.
    #[must_use]
    pub const fn jumbo_interior_object_reads(self) -> u64 {
        self.jumbo_interior_object_reads
    }

    /// Exact jumbo payload bytes returned to semantic verification.
    #[must_use]
    pub const fn jumbo_payload_bytes(self) -> u64 {
        self.jumbo_payload_bytes
    }

    /// Exact durable jumbo envelope bytes reopened for semantic verification.
    #[must_use]
    pub const fn jumbo_envelope_bytes(self) -> u64 {
        self.jumbo_envelope_bytes
    }

    fn checked_add(self, other: Self) -> Result<Self, String> {
        Ok(Self {
            segment_object_reads: self
                .segment_object_reads
                .checked_add(other.segment_object_reads)
                .ok_or_else(|| "semantic verifier segment read counter overflows".to_owned())?,
            segment_payload_bytes: self
                .segment_payload_bytes
                .checked_add(other.segment_payload_bytes)
                .ok_or_else(|| "semantic verifier segment payload counter overflows".to_owned())?,
            segment_envelope_bytes: self
                .segment_envelope_bytes
                .checked_add(other.segment_envelope_bytes)
                .ok_or_else(|| "semantic verifier segment envelope counter overflows".to_owned())?,
            jumbo_leaf_object_reads: self
                .jumbo_leaf_object_reads
                .checked_add(other.jumbo_leaf_object_reads)
                .ok_or_else(|| "semantic verifier jumbo leaf read counter overflows".to_owned())?,
            jumbo_interior_object_reads: self
                .jumbo_interior_object_reads
                .checked_add(other.jumbo_interior_object_reads)
                .ok_or_else(|| {
                    "semantic verifier jumbo interior read counter overflows".to_owned()
                })?,
            jumbo_payload_bytes: self
                .jumbo_payload_bytes
                .checked_add(other.jumbo_payload_bytes)
                .ok_or_else(|| "semantic verifier jumbo payload counter overflows".to_owned())?,
            jumbo_envelope_bytes: self
                .jumbo_envelope_bytes
                .checked_add(other.jumbo_envelope_bytes)
                .ok_or_else(|| "semantic verifier jumbo envelope counter overflows".to_owned())?,
        })
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
    maximum_inline_row_bytes: usize,
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
        Self::new_with_row_limit(
            store,
            receipts,
            backend_semantic::ir::MAX_SEMANTIC_SEGMENT_BYTES,
        )
    }

    /// Starts a segment sink with the manifest family's canonical inline-row
    /// threshold, including policy-aware jumbo rows.
    pub fn new_with_row_limit(
        store: &'store FileStore,
        receipts: &'receipts mut Receipts,
        maximum_inline_row_bytes: usize,
    ) -> Result<Self, String> {
        if maximum_inline_row_bytes == 0
            || maximum_inline_row_bytes > backend_semantic::ir::MAX_SEMANTIC_SEGMENT_BYTES
        {
            return Err("semantic producer row limit is outside the supported range".to_owned());
        }
        let gc_pin = store
            .pin_garbage_collection()
            .map_err(|error| format!("pin semantic producer segments against GC: {error:?}"))?;
        Ok(Self {
            store,
            receipts,
            maximum_inline_row_bytes,
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
        self.receipts
            .prepare_admission(identity, byte_length)
            .map_err(|error| SemanticPlaneRecordError::JumboObjectStore(error.to_string()))?;
        let key = ObjectKey::<SemanticSegmentPayload>::from_value(payload);
        let object = TypedObject::from_value(&key, payload);
        let admitted = match commit_and_read(self.store, &object, payload) {
            Ok(admitted) => admitted,
            Err(error) => {
                self.receipts.cancel_admission();
                return Err(SemanticPlaneRecordError::JumboObjectStore(error));
            }
        };
        let receipt = DurableSemanticObjectAdmission {
            identity,
            object_id: admitted.id(),
            payload_bytes: byte_length,
            envelope_bytes: admitted.bytes(),
            created: admitted.created(),
        };
        if let Err(error) = self.receipts.record_admission(receipt) {
            self.receipts.cancel_admission();
            return Err(SemanticPlaneRecordError::JumboObjectStore(
                error.to_string(),
            ));
        }
        if let Err(error) = self
            .metrics
            .record(admitted, byte_length, byte_length, 0, 0)
        {
            self.receipts.cancel_admission();
            return Err(error);
        }
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
        self.persist_validated(segment.validate_with_row_limit(self.maximum_inline_row_bytes)?)
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
        let byte_length =
            u64::try_from(payload.len()).map_err(|_| SemanticPlaneRecordError::MetricsOverflow)?;
        let identity = ProducedSemanticObjectIdentity::JumboLeaf {
            id: leaf.id(),
            ordinal: leaf.ordinal(),
            byte_offset: leaf.byte_offset(),
        };
        self.receipts
            .prepare_admission(identity, byte_length)
            .map_err(|error| SemanticPlaneRecordError::JumboObjectStore(error.to_string()))?;
        let key = ObjectKey::<SemanticJumboLeafPayload>::from_value(payload);
        let object = TypedObject::from_value(&key, payload);
        let admitted = match commit_and_read(self.store, &object, payload) {
            Ok(admitted) => admitted,
            Err(error) => {
                self.receipts.cancel_admission();
                return Err(SemanticPlaneRecordError::JumboObjectStore(error));
            }
        };
        if let Err(error) = self
            .receipts
            .record_admission(DurableSemanticObjectAdmission {
                identity,
                object_id: admitted.id(),
                payload_bytes: byte_length,
                envelope_bytes: admitted.bytes(),
                created: admitted.created(),
            })
        {
            self.receipts.cancel_admission();
            return Err(SemanticPlaneRecordError::JumboObjectStore(
                error.to_string(),
            ));
        }
        if let Err(error) = self
            .metrics
            .record(admitted, byte_length, 0, byte_length, 0)
        {
            self.receipts.cancel_admission();
            return Err(error);
        }
        Ok(())
    }

    fn write_interior(&mut self, node: &JumboRopeNode) -> Result<(), Self::Error> {
        let payload = node.encode_wire();
        let byte_length =
            u64::try_from(payload.len()).map_err(|_| SemanticPlaneRecordError::MetricsOverflow)?;
        let identity = ProducedSemanticObjectIdentity::JumboInterior {
            id: node.id(),
            first_leaf: node.first_leaf(),
            leaf_count: node.leaf_count(),
        };
        self.receipts
            .prepare_admission(identity, byte_length)
            .map_err(|error| SemanticPlaneRecordError::JumboObjectStore(error.to_string()))?;
        let key = ObjectKey::<SemanticJumboInteriorPayload>::from_value(payload.as_slice());
        let object = TypedObject::from_value(&key, payload.as_slice());
        let admitted = match commit_and_read(self.store, &object, payload.as_slice()) {
            Ok(admitted) => admitted,
            Err(error) => {
                self.receipts.cancel_admission();
                return Err(SemanticPlaneRecordError::JumboObjectStore(error));
            }
        };
        if let Err(error) = self
            .receipts
            .record_admission(DurableSemanticObjectAdmission {
                identity,
                object_id: admitted.id(),
                payload_bytes: byte_length,
                envelope_bytes: admitted.bytes(),
                created: admitted.created(),
            })
        {
            self.receipts.cancel_admission();
            return Err(SemanticPlaneRecordError::JumboObjectStore(
                error.to_string(),
            ));
        }
        if let Err(error) = self
            .metrics
            .record(admitted, byte_length, 0, 0, byte_length)
        {
            self.receipts.cancel_admission();
            return Err(error);
        }
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

const TYPED_V2_FAMILY_COUNT: usize = 7;

/// Explicit cut policy for every c007 typed semantic family.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SemanticTypedPlaneBoundaryPoliciesV3 {
    /// Core declaration rows.
    pub core: SemanticPlaneSegmentBoundaryPolicy,
    /// Type rows.
    pub types: SemanticPlaneSegmentBoundaryPolicy,
    /// Canonical relation rows.
    pub relations: SemanticPlaneSegmentBoundaryPolicy,
    /// Source occurrence rows.
    pub occurrences: SemanticPlaneSegmentBoundaryPolicy,
    /// Documentation rows.
    pub documentation: SemanticPlaneSegmentBoundaryPolicy,
    /// Source provenance rows.
    pub source_provenance: SemanticPlaneSegmentBoundaryPolicy,
    /// Sparse language-extension rows for the build profile.
    pub language_extensions: SemanticPlaneSegmentBoundaryPolicy,
}

impl SemanticTypedPlaneBoundaryPoliciesV3 {
    /// Requires a separately selected boundary policy for all seven families.
    #[must_use]
    pub const fn new(
        core: SemanticPlaneSegmentBoundaryPolicy,
        types: SemanticPlaneSegmentBoundaryPolicy,
        relations: SemanticPlaneSegmentBoundaryPolicy,
        occurrences: SemanticPlaneSegmentBoundaryPolicy,
        documentation: SemanticPlaneSegmentBoundaryPolicy,
        source_provenance: SemanticPlaneSegmentBoundaryPolicy,
        language_extensions: SemanticPlaneSegmentBoundaryPolicy,
    ) -> Self {
        Self {
            core,
            types,
            relations,
            occurrences,
            documentation,
            source_provenance,
            language_extensions,
        }
    }

    const fn as_array(self) -> [SemanticPlaneSegmentBoundaryPolicy; TYPED_V2_FAMILY_COUNT] {
        [
            self.core,
            self.types,
            self.relations,
            self.occurrences,
            self.documentation,
            self.source_provenance,
            self.language_extensions,
        ]
    }
}

/// One complete owner-side c007 producer result.
///
/// The manifest and roots were derived from seven stable-key family streams,
/// then checked against the exact durable FileStore payloads. Keep the GC pins
/// until the caller has placed all returned objects in a durable closure.
pub struct ProducedSemanticTypedPlaneV3 {
    manifest: SemanticTypedPlaneManifestV2,
    verified_content: VerifiedTypedPlaneContentV2,
    input_witness: SemanticInputWitness,
    segment_admissions: Vec<DurableSemanticObjectAdmission>,
    jumbo_admissions: Vec<DurableSemanticObjectAdmission>,
    segment_pins: Vec<DurableSemanticObjectPin>,
    jumbo_pin: DurableSemanticObjectPin,
    encoding_metrics: [CanonicalPlaneEncodingMetrics; TYPED_V2_FAMILY_COUNT],
    segment_store_metrics: [SemanticProducerStoreMetrics; TYPED_V2_FAMILY_COUNT],
    jumbo_store_metrics: SemanticProducerStoreMetrics,
    verifier_io_metrics: SemanticProducerVerifierIoMetrics,
}

impl ProducedSemanticTypedPlaneV3 {
    /// Canonical c007 revision-3 manifest with computed typed roots.
    #[must_use]
    pub const fn manifest(&self) -> &SemanticTypedPlaneManifestV2 {
        &self.manifest
    }

    /// Independent all-family proof for the exact bytes admitted by this pass.
    #[must_use]
    pub const fn verified_content(&self) -> &VerifiedTypedPlaneContentV2 {
        &self.verified_content
    }

    /// Live owner witness used during production. It is not serialized into the
    /// claim-only manifest and must remain available to any selection gate.
    #[must_use]
    pub const fn input_witness(&self) -> SemanticInputWitness {
        self.input_witness
    }

    /// Durable segment receipts in manifest family and segment order.
    #[must_use]
    pub fn segment_admissions(&self) -> &[DurableSemanticObjectAdmission] {
        &self.segment_admissions
    }

    /// Durable rope-object receipts emitted by documentation and provenance rows.
    #[must_use]
    pub fn jumbo_admissions(&self) -> &[DurableSemanticObjectAdmission] {
        &self.jumbo_admissions
    }

    /// Per-family row, segment, hashing, and scratch counters.
    #[must_use]
    pub const fn encoding_metrics(
        &self,
    ) -> &[CanonicalPlaneEncodingMetrics; TYPED_V2_FAMILY_COUNT] {
        &self.encoding_metrics
    }

    /// Per-family FileStore admission and physical-byte counters.
    #[must_use]
    pub const fn segment_store_metrics(
        &self,
    ) -> &[SemanticProducerStoreMetrics; TYPED_V2_FAMILY_COUNT] {
        &self.segment_store_metrics
    }

    /// FileStore admission and physical-byte counters for all jumbo objects.
    #[must_use]
    pub const fn jumbo_store_metrics(&self) -> SemanticProducerStoreMetrics {
        self.jumbo_store_metrics
    }

    /// Exact segment and jumbo envelope/payload bytes reopened by the
    /// producer-side semantic verifier. Reader discovery work is reported by
    /// the per-family encoding metrics and remains a complete family scan.
    #[must_use]
    pub const fn verifier_io_metrics(&self) -> SemanticProducerVerifierIoMetrics {
        self.verifier_io_metrics
    }

    /// GC pins held until this complete result is dropped.
    #[must_use]
    pub fn segment_pins(&self) -> &[DurableSemanticObjectPin] {
        &self.segment_pins
    }

    /// GC pin held for all emitted jumbo objects.
    #[must_use]
    pub const fn jumbo_pin(&self) -> &DurableSemanticObjectPin {
        &self.jumbo_pin
    }
}

/// Streams all seven typed semantic families from one complete reader through
/// the existing FileStore sinks, then derives roots by verifying the exact
/// durable payloads. The supplied witness must carry live owner-authorized
/// complete coverage; a claim-only `Coverage::Complete` is rejected before
/// any object is written. `policies` supplies the c007-committed boundary
/// algorithm and byte limits independently for every family.
///
/// This does not select a V2 generation. The returned object keeps the live
/// witness and durable GC pins so an owner can bind selection to read-closure
/// evidence and publish the exact receipt set atomically.
///
/// Each family encoder still builds a complete row plan, collects and sorts
/// the complete stable-key index, and encodes every canonical row. Stable-key
/// boundaries can localize segment identity churn, but they do not avoid full
/// reader discovery, row encoding, or BLAKE3 work for this writer.
/// There is no changed-key frontier in this producer API, so local segment
/// churn must not be described as incremental reader-delta execution.
pub(crate) fn produce_semantic_typed_plane_v3<Reader: SemanticReader + ?Sized>(
    store: &FileStore,
    reader: &Reader,
    build: SemanticBuildIdentity,
    input_witness: SemanticInputWitness,
    policies: SemanticTypedPlaneBoundaryPoliciesV3,
    tier: SemanticTypedPlaneVerificationTierV2,
    jumbo_limits: JumboRopeLimits,
) -> Result<ProducedSemanticTypedPlaneV3, String> {
    if !input_witness.coverage().is_authorized_complete() {
        return Err(
            "typed V2 production requires live owner-admitted complete input coverage".to_owned(),
        );
    }

    // The producer remains crate-internal until an owner-bound admission type
    // couples this reader to its witness. Coverage authority alone does not
    // prove that the independently supplied reader is the admitted input.
    let budget = V3ProductionBudget::new(tier, jumbo_limits)?;

    let image_facts = reader.image_facts();
    let policies = policies.as_array();
    let mut segment_admissions = budget.segment_admissions();
    let mut jumbo_admissions = budget.jumbo_admissions()?;
    let mut segment_pins = Vec::new();
    segment_pins
        .try_reserve_exact(TYPED_V2_FAMILY_COUNT)
        .map_err(|error| format!("reserve typed V2 segment pins: {error}"))?;
    let mut family_descriptors = Vec::new();
    family_descriptors
        .try_reserve_exact(TYPED_V2_FAMILY_COUNT)
        .map_err(|error| format!("reserve typed V2 family descriptors: {error}"))?;
    let mut encoding_metrics = Vec::new();
    encoding_metrics
        .try_reserve_exact(TYPED_V2_FAMILY_COUNT)
        .map_err(|error| format!("reserve typed V2 encoding metrics: {error}"))?;
    let mut segment_store_metrics = Vec::new();
    segment_store_metrics
        .try_reserve_exact(TYPED_V2_FAMILY_COUNT)
        .map_err(|error| format!("reserve typed V2 storage metrics: {error}"))?;

    let mut jumbo_sink = FileSemanticJumboRopeSink::new(store, &mut jumbo_admissions)?;
    let mut pass = TypedPlaneProductionPass {
        store,
        reader,
        input_witness,
        jumbo_sink: &mut jumbo_sink,
        jumbo_limits,
        work_limits: budget.work_limits,
        segment_admissions: &mut segment_admissions,
        segment_pins: &mut segment_pins,
        family_descriptors: &mut family_descriptors,
        encoding_metrics: &mut encoding_metrics,
        segment_store_metrics: &mut segment_store_metrics,
    };
    pass.produce_family(&CoreDeclarationRows, SemanticIrPlane::Core, policies[0])?;
    pass.produce_family(&TypesRows, SemanticIrPlane::Types, policies[1])?;
    pass.produce_family(&RelationRows, SemanticIrPlane::Relations, policies[2])?;
    pass.produce_family(&OccurrenceRows, SemanticIrPlane::Occurrences, policies[3])?;
    pass.produce_family(
        &DocumentationRows,
        SemanticIrPlane::Documentation,
        policies[4],
    )?;
    pass.produce_family(
        &SourceProvenanceRows,
        SemanticIrPlane::SourceProvenance,
        policies[5],
    )?;
    pass.produce_family(
        &LanguageExtensionRows::new(build.profile()),
        SemanticIrPlane::LanguageExtensions(build.profile()),
        policies[6],
    )?;
    drop(pass);

    let jumbo_store_metrics = jumbo_sink.metrics();
    let jumbo_pin = jumbo_sink.into_collection_pin();
    let families: [SemanticTypedPlaneFamilyDescriptorV2; TYPED_V2_FAMILY_COUNT] =
        family_descriptors
            .try_into()
            .map_err(|_| "typed V2 producer did not emit exactly seven families".to_owned())?;
    let encoding_metrics: [CanonicalPlaneEncodingMetrics; TYPED_V2_FAMILY_COUNT] = encoding_metrics
        .try_into()
        .map_err(|_| "typed V2 producer did not retain seven metric records".to_owned())?;
    let segment_store_metrics: [SemanticProducerStoreMetrics; TYPED_V2_FAMILY_COUNT] =
        segment_store_metrics
            .try_into()
            .map_err(|_| "typed V2 producer did not retain seven storage records".to_owned())?;

    let mut source = ProducedSegmentSource::new(store, segment_admissions.admissions())?;
    let mut jumbo_source = ProducedJumboSource::new(store, jumbo_admissions.admissions())?;
    let verified_content = derive_typed_plane_content_v2_from_admitted_reader(
        build,
        image_facts,
        input_witness,
        &families,
        tier,
        jumbo_limits,
        &mut source,
        &mut jumbo_source,
    )
    .map_err(|error: SemanticGenerationProofError| {
        format!("verify durable typed V2 producer output: {error}")
    })?;
    if jumbo_source.has_unreferenced_objects() {
        return Err("typed V2 producer emitted an unreferenced jumbo object".to_owned());
    }
    let verifier_io_metrics = source.io_metrics().checked_add(jumbo_source.io_metrics())?;

    let input_claim = backend_semantic::ir::SemanticInputClaimV2::from_witness(&input_witness);
    let manifest = SemanticTypedPlaneManifestV2::from_untrusted_claims(
        build,
        image_facts,
        input_claim,
        UntrustedSemanticContentRootV2::from_wire_claim(
            *verified_content.content_root().as_bytes(),
        ),
        UntrustedSemanticGenerationRootV2::from_wire_claim(
            *verified_content.generation_root().as_bytes(),
        ),
        families,
    )
    .map_err(|error: SemanticTypedPlaneManifestV2Error| {
        format!("build canonical c007 revision-3 manifest: {error}")
    })?;
    if verified_content.input_claim() != input_claim {
        return Err("typed V2 derived roots differ from the admitted input witness".to_owned());
    }

    Ok(ProducedSemanticTypedPlaneV3 {
        manifest,
        verified_content,
        input_witness,
        segment_admissions: segment_admissions.into_admissions(),
        jumbo_admissions: jumbo_admissions.into_admissions(),
        segment_pins,
        jumbo_pin,
        encoding_metrics,
        segment_store_metrics,
        jumbo_store_metrics,
        verifier_io_metrics,
    })
}

struct TypedPlaneProductionPass<'pass, 'store, 'receipts, Reader: ?Sized> {
    store: &'pass FileStore,
    reader: &'pass Reader,
    input_witness: SemanticInputWitness,
    jumbo_sink: &'pass mut FileSemanticJumboRopeSink<'store, 'receipts, V3JumboAdmissionBuilder>,
    jumbo_limits: JumboRopeLimits,
    work_limits: SemanticTypedPlaneWorkLimitsV2,
    segment_admissions: &'pass mut V3SegmentAdmissionBuilder,
    segment_pins: &'pass mut Vec<DurableSemanticObjectPin>,
    family_descriptors: &'pass mut Vec<SemanticTypedPlaneFamilyDescriptorV2>,
    encoding_metrics: &'pass mut Vec<CanonicalPlaneEncodingMetrics>,
    segment_store_metrics: &'pass mut Vec<SemanticProducerStoreMetrics>,
}

impl<Reader: SemanticReader + ?Sized> TypedPlaneProductionPass<'_, '_, '_, Reader> {
    fn produce_family<Encoder: CanonicalPlaneRowEncoder + ?Sized>(
        &mut self,
        encoder: &Encoder,
        family: SemanticIrPlane,
        policy: SemanticPlaneSegmentBoundaryPolicy,
    ) -> Result<(), String> {
        if encoder.kind() != SemanticPlaneKind::Ir(family) {
            return Err(format!("typed V2 encoder does not match {family:?}"));
        }
        let first_receipt = self.segment_admissions.admissions().len();
        let remaining_rows = self.segment_admissions.remaining_rows();
        let mut segment_sink = FileSemanticPlaneSegmentSink::new_with_row_limit(
            self.store,
            self.segment_admissions,
            policy.maximum_bytes(),
        )?;
        let metrics = stream_canonical_plane_family_with_jumbo_stable_key_anchors_and_limits(
            self.reader,
            encoder,
            self.input_witness,
            policy,
            remaining_rows,
            self.work_limits.max_total_rows(),
            self.work_limits.max_references(),
            self.jumbo_limits,
            self.jumbo_sink,
            &mut segment_sink,
        )
        .map_err(|error| format!("stream typed V2 {family:?} family: {error:?}"))?;
        let store_metrics = segment_sink.metrics();
        self.segment_pins
            .try_reserve(1)
            .map_err(|error| format!("reserve typed V2 family pin: {error}"))?;
        self.segment_pins.push(segment_sink.into_collection_pin());
        let claims = self
            .segment_admissions
            .admissions()
            .get(first_receipt..)
            .ok_or_else(|| "typed V2 segment receipts moved during encoding".to_owned())?;
        let descriptor = descriptor_from_receipts(family, policy, metrics, claims)?;
        self.encoding_metrics
            .try_reserve(1)
            .map_err(|error| format!("reserve typed V2 family metrics: {error}"))?;
        self.segment_store_metrics
            .try_reserve(1)
            .map_err(|error| format!("reserve typed V2 FileStore metrics: {error}"))?;
        self.family_descriptors
            .try_reserve(1)
            .map_err(|error| format!("reserve typed V2 family manifest entry: {error}"))?;
        self.encoding_metrics.push(metrics);
        self.segment_store_metrics.push(store_metrics);
        self.family_descriptors.push(descriptor);
        Ok(())
    }
}

fn descriptor_from_receipts(
    family: SemanticIrPlane,
    policy: SemanticPlaneSegmentBoundaryPolicy,
    metrics: CanonicalPlaneEncodingMetrics,
    receipts: &[DurableSemanticObjectAdmission],
) -> Result<SemanticTypedPlaneFamilyDescriptorV2, String> {
    if u64::try_from(receipts.len()).ok() != Some(metrics.segment_count()) {
        return Err(format!(
            "typed V2 {family:?} segment receipt count differs from writer"
        ));
    }
    let mut claims = Vec::new();
    claims
        .try_reserve_exact(receipts.len())
        .map_err(|error| format!("reserve typed V2 segment claims: {error}"))?;
    let mut rows = 0_u64;
    for receipt in receipts {
        let ProducedSemanticObjectIdentity::Segment {
            family: observed_family,
            id,
            first_key,
            last_key,
            row_count,
        } = receipt.identity()
        else {
            return Err("typed V2 family segment sink recorded a non-segment object".to_owned());
        };
        if observed_family != family {
            return Err(format!(
                "typed V2 {family:?} receipt contains {observed_family:?}"
            ));
        }
        rows = rows
            .checked_add(u64::from(row_count))
            .ok_or_else(|| "typed V2 family row count overflows".to_owned())?;
        claims.push(
            SemanticTypedPlaneSegmentClaimV2::from_untrusted_claims(
                first_key,
                last_key,
                row_count,
                receipt.payload_bytes(),
                UntrustedSemanticSegmentId::from_raw(*id.as_bytes()),
            )
            .map_err(|error| format!("invalid durable typed V2 segment receipt: {error}"))?,
        );
    }
    if rows != metrics.row_count() {
        return Err(format!(
            "typed V2 {family:?} row receipts differ from writer count"
        ));
    }
    SemanticTypedPlaneFamilyDescriptorV2::from_untrusted_claims(
        family,
        metrics.row_count(),
        policy,
        claims,
    )
    .map_err(|error| format!("invalid typed V2 {family:?} family descriptor: {error}"))
}

struct ProducedSegmentSource<'store, 'receipts> {
    store: &'store FileStore,
    segments: &'receipts [DurableSemanticObjectAdmission],
    current: Vec<u8>,
    io_metrics: SemanticProducerVerifierIoMetrics,
}

impl<'store, 'receipts> ProducedSegmentSource<'store, 'receipts> {
    fn new(
        store: &'store FileStore,
        receipts: &'receipts [DurableSemanticObjectAdmission],
    ) -> Result<Self, String> {
        for receipt in receipts {
            let ProducedSemanticObjectIdentity::Segment { .. } = receipt.identity() else {
                return Err("typed V2 segment source received a non-segment receipt".to_owned());
            };
        }
        Ok(Self {
            store,
            segments: receipts,
            current: Vec::new(),
            io_metrics: SemanticProducerVerifierIoMetrics::default(),
        })
    }

    fn io_metrics(&self) -> SemanticProducerVerifierIoMetrics {
        self.io_metrics
    }
}

impl TypedPlaneSegmentSourceV2 for ProducedSegmentSource<'_, '_> {
    type Error = String;

    fn segment<'source>(
        &'source mut self,
        index: usize,
        claim: &SemanticTypedPlaneSegmentClaimV2,
    ) -> Result<&'source [u8], Self::Error> {
        let receipt =
            self.segments.get(index).copied().ok_or_else(|| {
                "typed V2 verifier requested an unknown produced segment".to_owned()
            })?;
        let ProducedSemanticObjectIdentity::Segment {
            family: observed_family,
            id,
            first_key,
            last_key,
            row_count,
        } = receipt.identity()
        else {
            return Err("typed V2 produced segment map is malformed".to_owned());
        };
        if claim.first_key() != &first_key
            || claim.last_key() != &last_key
            || claim.row_count() != row_count
            || claim.id_claim().as_bytes() != id.as_bytes()
            || claim.byte_length() != receipt.payload_bytes()
        {
            return Err("typed V2 manifest claim differs from durable segment receipt".to_owned());
        }
        let object = self
            .store
            .read_object(receipt.object_id())
            .map_err(|error| format!("reopen typed V2 produced segment: {error:?}"))?;
        if object.id() != receipt.object_id()
            || object.schema() != ProducedSemanticObjectKind::Segment.schema_identity()
            || object.bytes().len() as u64 != receipt.payload_bytes()
        {
            return Err("typed V2 produced segment envelope changed after admission".to_owned());
        }
        self.io_metrics.segment_object_reads = self
            .io_metrics
            .segment_object_reads
            .checked_add(1)
            .ok_or_else(|| "semantic verifier segment read counter overflows".to_owned())?;
        self.io_metrics.segment_payload_bytes = self
            .io_metrics
            .segment_payload_bytes
            .checked_add(receipt.payload_bytes())
            .ok_or_else(|| "semantic verifier segment payload counter overflows".to_owned())?;
        self.io_metrics.segment_envelope_bytes = self
            .io_metrics
            .segment_envelope_bytes
            .checked_add(receipt.envelope_bytes())
            .ok_or_else(|| "semantic verifier segment envelope counter overflows".to_owned())?;
        self.current.clear();
        self.current
            .try_reserve_exact(object.bytes().len())
            .map_err(|error| format!("reserve typed V2 segment read buffer: {error}"))?;
        self.current.extend_from_slice(object.bytes());
        Ok(&self.current)
    }
}

#[derive(Clone, Copy)]
struct JumboObjectMapping {
    object: ObjectId,
    payload_bytes: u64,
    envelope_bytes: u64,
    used: bool,
}

struct ProducedJumboSource<'store> {
    store: &'store FileStore,
    leaves: BTreeMap<JumboRopeObjectId, JumboObjectMapping>,
    interiors: BTreeMap<JumboRopeObjectId, JumboObjectMapping>,
    io_metrics: SemanticProducerVerifierIoMetrics,
}

impl<'store> ProducedJumboSource<'store> {
    fn new(
        store: &'store FileStore,
        receipts: &[DurableSemanticObjectAdmission],
    ) -> Result<Self, String> {
        let mut source = Self {
            store,
            leaves: BTreeMap::new(),
            interiors: BTreeMap::new(),
            io_metrics: SemanticProducerVerifierIoMetrics::default(),
        };
        for receipt in receipts {
            let (id, map) = match receipt.identity() {
                ProducedSemanticObjectIdentity::JumboLeaf { id, .. } => (id, &mut source.leaves),
                ProducedSemanticObjectIdentity::JumboInterior { id, .. } => {
                    (id, &mut source.interiors)
                }
                ProducedSemanticObjectIdentity::Segment { .. } => {
                    return Err("typed V2 jumbo source received a segment receipt".to_owned());
                }
            };
            if let Some(previous) = map.get(&id)
                && (previous.object != receipt.object_id()
                    || previous.payload_bytes != receipt.payload_bytes()
                    || previous.envelope_bytes != receipt.envelope_bytes())
            {
                return Err("one jumbo identity maps to conflicting FileStore receipts".to_owned());
            }
            map.entry(id).or_insert(JumboObjectMapping {
                object: receipt.object_id(),
                payload_bytes: receipt.payload_bytes(),
                envelope_bytes: receipt.envelope_bytes(),
                used: false,
            });
        }
        Ok(source)
    }

    fn has_unreferenced_objects(&self) -> bool {
        self.leaves.values().any(|mapping| !mapping.used)
            || self.interiors.values().any(|mapping| !mapping.used)
    }

    fn io_metrics(&self) -> SemanticProducerVerifierIoMetrics {
        self.io_metrics
    }

    fn read_object(
        &mut self,
        object_id: ObjectId,
        kind: ProducedSemanticObjectKind,
        expected_payload_bytes: u64,
        envelope_bytes: u64,
    ) -> Result<TypedObject, String> {
        let object = self
            .store
            .read_object(object_id)
            .map_err(|error| format!("reopen typed V2 jumbo object: {error:?}"))?;
        if object.id() != object_id
            || object.schema() != kind.schema_identity()
            || u64::try_from(object.bytes().len()).ok() != Some(expected_payload_bytes)
        {
            return Err("typed V2 jumbo envelope kind or identity changed".to_owned());
        }
        self.io_metrics.jumbo_payload_bytes = self
            .io_metrics
            .jumbo_payload_bytes
            .checked_add(expected_payload_bytes)
            .ok_or_else(|| "semantic verifier jumbo payload counter overflows".to_owned())?;
        self.io_metrics.jumbo_envelope_bytes = self
            .io_metrics
            .jumbo_envelope_bytes
            .checked_add(envelope_bytes)
            .ok_or_else(|| "semantic verifier jumbo envelope counter overflows".to_owned())?;
        match kind {
            ProducedSemanticObjectKind::JumboLeaf => {
                self.io_metrics.jumbo_leaf_object_reads = self
                    .io_metrics
                    .jumbo_leaf_object_reads
                    .checked_add(1)
                    .ok_or_else(|| {
                        "semantic verifier jumbo leaf read counter overflows".to_owned()
                    })?;
            }
            ProducedSemanticObjectKind::JumboInterior => {
                self.io_metrics.jumbo_interior_object_reads = self
                    .io_metrics
                    .jumbo_interior_object_reads
                    .checked_add(1)
                    .ok_or_else(|| {
                        "semantic verifier jumbo interior read counter overflows".to_owned()
                    })?;
            }
            ProducedSemanticObjectKind::Segment => {
                return Err("typed V2 jumbo reader received a segment kind".to_owned());
            }
        }
        Ok(object)
    }
}

impl JumboRopeObjectSource for ProducedJumboSource<'_> {
    type Error = String;

    fn read_leaf(
        &mut self,
        id: JumboRopeObjectId,
        output: &mut [u8; JUMBO_ROPE_MAX_LEAF_BYTES],
    ) -> Result<Option<usize>, Self::Error> {
        let Some(mapping) = self.leaves.get_mut(&id).map(|mapping| {
            mapping.used = true;
            *mapping
        }) else {
            return Ok(None);
        };
        let object = self.read_object(
            mapping.object,
            ProducedSemanticObjectKind::JumboLeaf,
            mapping.payload_bytes,
            mapping.envelope_bytes,
        )?;
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
        let Some(mapping) = self.interiors.get_mut(&id).map(|mapping| {
            mapping.used = true;
            *mapping
        }) else {
            return Ok(None);
        };
        let object = self.read_object(
            mapping.object,
            ProducedSemanticObjectKind::JumboInterior,
            mapping.payload_bytes,
            mapping.envelope_bytes,
        )?;
        let bytes = object.bytes();
        if bytes.len() != ROPE_NODE_WIRE_BYTES {
            return Err("typed V2 jumbo interior has the wrong fixed length".to_owned());
        }
        Ok(Some(bytes.try_into().map_err(|_| {
            "typed V2 jumbo interior has a non-canonical length".to_owned()
        })?))
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
        BorrowedTree, BuiltinType, ConcreteType, Confidence, CorePayloadHash, DeclarationFamilyId,
        DocInput, DocumentationRows, EntityAuthorityFacts, EntityVersion, FactAvailability, Ir,
        IrBuilder, ItemKind, JUMBO_ROPE_MAX_LEAF_BYTES, JumboRopeObjectId, JumboRopeObjectSource,
        JumboValueContext, JumboValueEncoding, JumboValueFamily, LanguageExtensionInput, LinkKind,
        MAX_SEMANTIC_SEGMENT_BYTES, OccurrenceAuthorityFacts, ParentageAuthority,
        ROPE_NODE_WIRE_BYTES, RustFacts, RustOwnership, SemanticImageView, SemanticInputWitness,
        SemanticIrPlane, SemanticPlaneKind, SemanticPlaneSegment,
        SemanticPlaneSegmentBoundaryPolicy as CanonicalPlaneSegmentBoundaryPolicy, SourceSpan,
        TreeEntityId, TreeItemInput, TreeLinkInput, TreeLinkTarget, TypeExpr, VariantFingerprint,
        Visibility, encode_full_semantic_image, full_semantic_image_len,
        stream_canonical_plane_family_with_jumbo,
        stream_canonical_plane_family_with_jumbo_and_stable_key_anchors,
        verify_canonical_semantic_plane_segment_boundaries, verify_jumbo_plane_family_closures,
        write_jumbo_value,
    };
    use backend_semantic::vocabulary::{LanguageProfile, RustEdition, Stage};
    use backend_store::{ClosureCompositionBudget, ClosureMembershipChange};
    use backend_version::{
        AdmittedProducerObservation, AuthorityScopeClaim, CoverageAdmissionError, CoverageWitness,
        ObjectVersion, ProducerObservationClaims, ProducerObservationVerifier, ScopeRoot,
        UntrustedProducerObservation, admit_complete_scope, admit_producer_observation,
    };

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

    #[test]
    fn v3_jumbo_budget_rejects_before_a_store_write() {
        let directory = TestDirectory::new();
        let cas_path = directory.0.join("budget-cas");
        let store =
            FileStore::open(&cas_path, 4 * 1024 * 1024).expect("open V3 budget test FileStore");
        let work_limits =
            typed_plane_work_limits_v2(SemanticTypedPlaneVerificationTierV2::Standard);
        let mut receipts = V3JumboAdmissionBuilder::new(work_limits, JumboRopeLimits::default())
            .expect("default caller jumbo policy is valid");
        receipts.total_object_reads = work_limits.max_total_jumbo_object_reads();
        let mut sink = FileSemanticJumboRopeSink::new(&store, &mut receipts)
            .expect("pin budget test jumbo writer against GC");
        let context = JumboValueContext::new(
            [0x55; 32],
            JumboValueFamily::Documentation,
            0,
            JumboValueEncoding::Bytes,
        );
        let result = write_jumbo_value(
            context,
            b"a value which must be refused before its leaf is committed",
            JumboRopeLimits::default(),
            &mut sink,
        );
        assert!(result.is_err(), "the full read budget rejects the leaf");
        assert!(
            write_jumbo_value(
                JumboValueContext::new(
                    [0x56; 32],
                    JumboValueFamily::Documentation,
                    0,
                    JumboValueEncoding::Bytes,
                ),
                b"a second value must not publish after the sink error",
                JumboRopeLimits::default(),
                &mut sink,
            )
            .is_err(),
            "a preflight sink error poisons the builder before another FileStore write"
        );
        drop(sink);
        assert!(receipts.admissions().is_empty());
        assert_eq!(
            fs::read_dir(cas_path.join("objects"))
                .expect("FileStore object directory exists")
                .count(),
            0,
            "preflight rejection leaves no immutable object behind"
        );
    }

    #[test]
    fn v3_jumbo_failed_write_rolls_back_only_the_active_value_and_poisons_builder() {
        let directory = TestDirectory::new();
        let cas_path = directory.0.join("rollback-cas");
        let store =
            FileStore::open(&cas_path, 4 * 1024 * 1024).expect("open V3 rollback test FileStore");
        let work_limits =
            typed_plane_work_limits_v2(SemanticTypedPlaneVerificationTierV2::Standard);
        let mut receipts = V3JumboAdmissionBuilder::new(work_limits, JumboRopeLimits::default())
            .expect("default caller jumbo policy is valid");
        let context = JumboValueContext::new(
            [0x61; 32],
            JumboValueFamily::Documentation,
            0,
            JumboValueEncoding::Bytes,
        );
        {
            let mut sink = FileSemanticJumboRopeSink::new(&store, &mut receipts)
                .expect("pin rollback test jumbo writer against GC");
            write_jumbo_value(context, b"x", JumboRopeLimits::default(), &mut sink)
                .expect("admit one complete baseline value");
        }
        let completed = receipts.admissions()[0];
        let ProducedSemanticObjectIdentity::JumboLeaf { id, .. } = completed.identity() else {
            panic!("one-byte value is represented by one leaf");
        };

        // Start an identical value, then fail the second leaf write. The
        // failed value's first leaf must be rolled back while the prior
        // completed value remains accounted for.
        let first_leaf = ProducedSemanticObjectIdentity::JumboLeaf {
            id,
            ordinal: 0,
            byte_offset: 0,
        };
        receipts
            .prepare_admission(first_leaf, completed.payload_bytes())
            .expect("reserve first leaf of next value");
        receipts
            .record_admission(completed)
            .expect("record first leaf of next value");
        receipts
            .prepare_admission(
                ProducedSemanticObjectIdentity::JumboLeaf {
                    id,
                    ordinal: 1,
                    byte_offset: 1,
                },
                1,
            )
            .expect("reserve second leaf before simulated FileStore failure");
        receipts.cancel_admission();

        assert!(receipts.active_value.is_none());
        assert!(receipts.poisoned);
        assert_eq!(receipts.admissions().len(), 1);
        assert_eq!(receipts.total_value_bytes, 1);
        assert_eq!(receipts.total_leaves, 1);
        assert_eq!(receipts.total_object_reads, 1);
        assert_eq!(receipts.total_read_bytes, 1);
        assert!(
            receipts
                .prepare_admission(first_leaf, completed.payload_bytes())
                .is_err(),
            "a failed value cannot leak its partial receipts into a new value"
        );
    }

    #[test]
    fn v3_jumbo_receipt_mismatch_poisons_builder_before_another_value() {
        let directory = TestDirectory::new();
        let cas_path = directory.0.join("receipt-mismatch-cas");
        let store = FileStore::open(&cas_path, 4 * 1024 * 1024)
            .expect("open V3 receipt mismatch test FileStore");
        let work_limits =
            typed_plane_work_limits_v2(SemanticTypedPlaneVerificationTierV2::Standard);
        let mut receipts = V3JumboAdmissionBuilder::new(work_limits, JumboRopeLimits::default())
            .expect("default caller jumbo policy is valid");
        let context = JumboValueContext::new(
            [0x62; 32],
            JumboValueFamily::Documentation,
            0,
            JumboValueEncoding::Bytes,
        );
        {
            let mut sink = FileSemanticJumboRopeSink::new(&store, &mut receipts)
                .expect("pin receipt mismatch writer against GC");
            write_jumbo_value(context, b"x", JumboRopeLimits::default(), &mut sink)
                .expect("admit one complete value");
        }
        let prior = receipts.admissions()[0];
        let ProducedSemanticObjectIdentity::JumboLeaf { id, .. } = prior.identity() else {
            panic!("one-byte value is represented by one leaf");
        };
        receipts
            .prepare_admission(
                ProducedSemanticObjectIdentity::JumboLeaf {
                    id,
                    ordinal: 1,
                    byte_offset: 1,
                },
                1,
            )
            .expect("reserve a later leaf in the active value");
        assert!(receipts.record_admission(prior).is_err());

        assert!(receipts.poisoned);
        assert!(receipts.active_value.is_none());
        assert!(receipts.admissions().is_empty());
        assert_eq!(receipts.total_object_reads, 0);
        assert!(
            receipts
                .prepare_admission(prior.identity(), prior.payload_bytes())
                .is_err(),
            "a mismatched receipt cannot be followed by another value"
        );
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

    struct V3InputAuthority;

    impl backend_version::Schema for V3InputAuthority {
        const DOMAIN: u8 = 0x53;
        const TYPE: u16 = 0xfffd;
        type Value = [u8; 32];

        fn encode(value: &Self::Value, output: &mut Vec<u8>) {
            output.extend_from_slice(value);
        }
    }

    struct V3InputVerifier;

    impl ProducerObservationVerifier for V3InputVerifier {
        type Error = CoverageAdmissionError;

        fn verify(
            &self,
            observation: &UntrustedProducerObservation,
        ) -> Result<ProducerObservationClaims, Self::Error> {
            Ok(ProducerObservationClaims::new(
                observation.producer_identity(),
                observation.scope_root(),
                observation.context(),
                *blake3::hash(observation.evidence()).as_bytes(),
            ))
        }
    }

    fn v3_input_witness(identity: u8) -> SemanticInputWitness {
        let input_root = [identity; 32];
        let scope_value = [identity.wrapping_add(1); 32];
        let version = ObjectVersion::<V3InputAuthority>::from_value(&scope_value);
        let scope = ScopeRoot::from_bytes(version.to_bytes());
        let claim = AuthorityScopeClaim::from_object_version(version);
        let producer: AdmittedProducerObservation = admit_producer_observation(
            UntrustedProducerObservation::new(
                [7; 32],
                claim.scope_root(),
                [8; 32],
                vec![identity, identity.wrapping_add(9)],
            ),
            &V3InputVerifier,
        )
        .expect("test producer observation is admitted");
        let witness = CoverageWitness::Complete(
            admit_complete_scope(claim, producer).expect("test input scope matches"),
        );
        SemanticInputWitness::admitted(input_root, scope, witness)
            .expect("test input witness carries live complete coverage")
    }

    fn v3_build_identity() -> SemanticBuildIdentity {
        SemanticBuildIdentity::new(
            [1; 32],
            [2; 32],
            LanguageProfile::Rust(RustEdition::Rust2024),
            Stage::LowerIr,
            [3; 32],
            [4; 32],
            [5; 32],
            [6; 32],
        )
    }

    fn v3_boundary_policies() -> SemanticTypedPlaneBoundaryPoliciesV3 {
        let policy = SemanticPlaneSegmentBoundaryPolicy::stable_key_hash_ramp(512, 1024, 4096)
            .expect("V3 fixture stable-key policy is valid");
        SemanticTypedPlaneBoundaryPoliciesV3::new(
            policy, policy, policy, policy, policy, policy, policy,
        )
    }

    #[derive(Clone, Copy)]
    enum V3FixtureShape {
        Base,
        RenameEarly,
        RenameMiddle,
        RenameTail,
        EarlyEdit,
        MiddleEdit,
        TailEdit,
        MiddleInsert,
        MiddleDelete,
    }

    impl V3FixtureShape {
        fn label(self) -> &'static str {
            match self {
                Self::Base => "base",
                Self::RenameEarly => "rename-early",
                Self::RenameMiddle => "rename-middle",
                Self::RenameTail => "rename-tail",
                Self::EarlyEdit => "early-edit",
                Self::MiddleEdit => "middle-edit",
                Self::TailEdit => "tail-edit",
                Self::MiddleInsert => "middle-insert",
                Self::MiddleDelete => "middle-delete",
            }
        }
    }

    fn v3_fixture(shape: V3FixtureShape) -> Ir {
        let mut stable_keys = vec![10_u64, 20, 30, 40];
        match shape {
            V3FixtureShape::Base
            | V3FixtureShape::RenameEarly
            | V3FixtureShape::RenameMiddle
            | V3FixtureShape::RenameTail
            | V3FixtureShape::EarlyEdit
            | V3FixtureShape::MiddleEdit
            | V3FixtureShape::TailEdit => {}
            V3FixtureShape::MiddleInsert => stable_keys.push(25),
            V3FixtureShape::MiddleDelete => stable_keys.retain(|key| *key != 20),
        }
        stable_keys.sort_unstable();

        let mut builder = IrBuilder::new();
        builder
            .set_language_profile(LanguageProfile::Rust(RustEdition::Rust2024))
            .expect("fixture profile is valid");
        let source_file = builder
            .intern_atom(b"src/v3-fixture.rs")
            .expect("source atom");
        let semantic_type = builder
            .intern_type(TypeExpr::Concrete(ConcreteType::Builtin(BuiltinType::I32)))
            .expect("fixture builtin type");
        let rust_facts = RustFacts {
            ownership: RustOwnership::Value,
            lifetimes: builder.intern_attributes(&[]).expect("empty lifetimes"),
            where_clauses: builder
                .intern_type_parameters(&[])
                .expect("empty where clauses"),
            macros: builder.intern_attributes(&[]).expect("empty macros"),
            const_defaults: builder
                .intern_attributes(&[])
                .expect("empty const defaults"),
            free_predicates: builder
                .intern_free_predicates(&[])
                .expect("empty free predicates"),
        };

        let names = stable_keys
            .iter()
            .map(|key| {
                let rename = matches!(
                    (shape, *key),
                    (V3FixtureShape::RenameEarly, 10)
                        | (V3FixtureShape::RenameMiddle, 30)
                        | (V3FixtureShape::RenameTail, 40)
                );
                if rename {
                    format!("renamed_node_{key:02}")
                } else {
                    format!("node_{key:02}")
                }
            })
            .collect::<Vec<_>>();
        let documents = stable_keys
            .iter()
            .map(|key| {
                if *key == 20 {
                    // Force the ordinary documentation writer to emit a
                    // content-defined jumbo rope and exercise its durable
                    // leaf/interior receipt path.
                    "semantic jumbo documentation ".repeat(1_400)
                } else if matches!(
                    (shape, *key),
                    (V3FixtureShape::EarlyEdit, 10)
                        | (V3FixtureShape::MiddleEdit, 30)
                        | (V3FixtureShape::TailEdit, 40)
                ) {
                    format!("edited documentation for stable node {key}")
                } else {
                    format!("documentation for stable node {key}")
                }
            })
            .collect::<Vec<_>>();
        let docs = documents
            .iter()
            .map(|document| [DocInput::Text(document.as_str())])
            .collect::<Vec<_>>();
        let versions = stable_keys
            .iter()
            .map(|key| {
                let mut family = [0_u8; 16];
                family[8..].copy_from_slice(&key.to_be_bytes());
                EntityVersion {
                    family: DeclarationFamilyId::from_raw(family),
                    variant: VariantFingerprint::from_raw([0x42; 16]),
                    core_payload: CorePayloadHash::from_raw([0x43; 16]),
                }
            })
            .collect::<Vec<_>>();
        let authority = EntityAuthorityFacts {
            parentage: ParentageAuthority::Root,
            source: FactAvailability::Captured,
            source_file: FactAvailability::Captured,
            members: FactAvailability::Captured,
            documentation: FactAvailability::Captured,
            attributes: FactAvailability::Captured,
            visibility: FactAvailability::Captured,
            semantic_type: FactAvailability::Captured,
            language_extension: FactAvailability::Captured,
            ..EntityAuthorityFacts::default()
        };
        let items = stable_keys
            .iter()
            .enumerate()
            .map(|(position, _)| TreeItemInput {
                name: names[position].as_bytes(),
                kind: ItemKind::Function,
                visibility: Visibility::Public,
                authority,
                parent: None,
                semantic_type: Some(semantic_type),
                members: &[],
                docs: &docs[position],
                attributes: &[],
                source: SourceSpan::new(
                    source_file,
                    u32::try_from(position * 32).expect("fixture source offset fits"),
                    u32::try_from(position * 32 + 8).expect("fixture source end fits"),
                ),
                extension: Some(LanguageExtensionInput::Rust(&rust_facts)),
            })
            .collect::<Vec<_>>();
        let links = (0..stable_keys.len().saturating_sub(1))
            .map(|position| TreeLinkInput {
                from: TreeEntityId::new(
                    u32::try_from(position).expect("fixture tree entity index fits u32"),
                ),
                target: TreeLinkTarget::Local(TreeEntityId::new(
                    u32::try_from(position + 1).expect("fixture tree target index fits u32"),
                )),
                kind: LinkKind::Calls,
                confidence: Confidence::Compiler,
                authority: OccurrenceAuthorityFacts {
                    source: FactAvailability::Captured,
                },
                source: SourceSpan::new(
                    source_file,
                    u32::try_from(position * 32 + 8).expect("link source offset fits"),
                    u32::try_from(position * 32 + 16).expect("link source end fits"),
                ),
            })
            .collect::<Vec<_>>();
        builder
            .add_borrowed_tree(BorrowedTree {
                versions: &versions,
                items: &items,
                links: &links,
            })
            .expect("fixture seven-family borrowed tree is valid");
        builder.finish().expect("fixture seven-family IR is valid")
    }

    fn produce_v3(
        store: &FileStore,
        reader: &impl SemanticReader,
        shape: V3FixtureShape,
        input_identity: u8,
    ) -> ProducedSemanticTypedPlaneV3 {
        produce_semantic_typed_plane_v3(
            store,
            reader,
            v3_build_identity(),
            v3_input_witness(input_identity),
            v3_boundary_policies(),
            SemanticTypedPlaneVerificationTierV2::Standard,
            JumboRopeLimits::default(),
        )
        .unwrap_or_else(|error| {
            panic!(
                "real-reader V3 {} producer emits a complete verified c007 manifest: {error}",
                shape.label()
            )
        })
    }

    fn v3_segment_inventory(
        produced: &ProducedSemanticTypedPlaneV3,
    ) -> BTreeMap<(SemanticIrPlane, [u8; 32]), u64> {
        let mut inventory = BTreeMap::new();
        for receipt in produced.segment_admissions() {
            let ProducedSemanticObjectIdentity::Segment { family, id, .. } = receipt.identity()
            else {
                continue;
            };
            let previous = inventory.insert((family, *id.as_bytes()), receipt.payload_bytes());
            assert!(
                previous.is_none_or(|bytes| bytes == receipt.payload_bytes()),
                "one family-scoped stable segment identity has one payload length"
            );
        }
        inventory
    }

    fn v3_segment_churn(
        base: &ProducedSemanticTypedPlaneV3,
        target: &ProducedSemanticTypedPlaneV3,
    ) -> (usize, usize, u64) {
        let base = v3_segment_inventory(base);
        let target = v3_segment_inventory(target);
        let new_payload_bytes = target
            .iter()
            .filter(|(identity, _)| !base.contains_key(*identity))
            .map(|(_, byte_length)| *byte_length)
            .sum();
        let new_segments = target
            .keys()
            .filter(|identity| !base.contains_key(*identity))
            .count();
        let removed_segments = base
            .keys()
            .filter(|identity| !target.contains_key(*identity))
            .count();
        (new_segments, removed_segments, new_payload_bytes)
    }

    fn cold_reverify_v3(
        store: &FileStore,
        produced: &ProducedSemanticTypedPlaneV3,
    ) -> (
        VerifiedTypedPlaneContentV2,
        SemanticProducerVerifierIoMetrics,
    ) {
        let mut segment_source = ProducedSegmentSource::new(store, produced.segment_admissions())
            .expect("cold source maps every exact segment receipt");
        let mut jumbo_source = ProducedJumboSource::new(store, produced.jumbo_admissions())
            .expect("cold source maps every exact rope receipt");
        let verified =
            backend_semantic::ir::verify_typed_plane_content_v2_with_jumbo_segment_source(
                produced.manifest(),
                SemanticTypedPlaneVerificationTierV2::Standard,
                JumboRopeLimits::default(),
                &mut segment_source,
                &mut jumbo_source,
            )
            .expect("cold V3 consumer independently verifies every target family");
        assert!(
            !jumbo_source.has_unreferenced_objects(),
            "every admitted jumbo object is reached from a verified family row"
        );
        assert_eq!(&verified, produced.verified_content());
        let io_metrics = segment_source
            .io_metrics()
            .checked_add(jumbo_source.io_metrics())
            .expect("bounded V3 verifier I/O counters fit u64");
        (verified, io_metrics)
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
                .map_err(|error| format!("cold read jumbo leaf object: {error:?}"))?;
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
                .map_err(|error| format!("cold read jumbo interior object: {error:?}"))?;
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

    #[test]
    fn v3_real_reader_edit_matrix_cold_verifies_all_seven_families() {
        let directory = TestDirectory::new();
        let cas_path = directory.0.join("v3-cas");
        let store =
            FileStore::open(&cas_path, 4 * 1024 * 1024).expect("open V3 producer FileStore");
        let base_ir = v3_fixture(V3FixtureShape::Base);
        let base = produce_v3(&store, &base_ir, V3FixtureShape::Base, 11);

        assert_eq!(base.manifest().families().len(), TYPED_V2_FAMILY_COUNT);
        assert!(base.input_witness().coverage().is_authorized_complete());
        assert_eq!(
            base.manifest().input_claim(),
            backend_semantic::ir::SemanticInputClaimV2::from_witness(&base.input_witness()),
            "c007 retains only the deterministic claim bound to the live owner witness"
        );
        assert!(
            base.manifest()
                .families()
                .iter()
                .all(|family| family.row_count() > 0)
        );
        assert_eq!(
            base.manifest().generation_root_claim().as_bytes(),
            base.verified_content().generation_root().as_bytes(),
            "c007 generation claim is derived from the exact admitted payload closure"
        );
        assert_eq!(
            base.manifest().content_root_claim().as_bytes(),
            base.verified_content().content_root().as_bytes(),
            "c007 content claim is derived from the exact admitted payload closure"
        );
        assert!(base.jumbo_store_metrics().attempted_objects() > 0);
        assert!(base.jumbo_admissions().iter().any(|receipt| matches!(
            receipt.identity(),
            ProducedSemanticObjectIdentity::JumboLeaf { .. }
        )));
        assert!(base.jumbo_admissions().iter().any(|receipt| matches!(
            receipt.identity(),
            ProducedSemanticObjectIdentity::JumboInterior { .. }
        )));

        for (index, family) in base.manifest().families().iter().enumerate() {
            let encoded = base.encoding_metrics()[index];
            let stored = base.segment_store_metrics()[index];
            assert_eq!(encoded.row_count(), family.row_count());
            assert_eq!(
                encoded.segment_count(),
                family.segments().len() as u64,
                "family {index} segment count binds to durable receipt claims"
            );
            assert_eq!(
                encoded.output_bytes(),
                family
                    .segments()
                    .iter()
                    .map(|segment| segment.byte_length())
                    .sum::<u64>(),
                "family {index} exact SPIR bytes bind to c007 descriptors"
            );
            assert_eq!(
                stored.semantic_segment_hash_bytes(),
                encoded.output_bytes(),
                "family {index} durable sink validates every exact SPIR byte"
            );
        }

        let cold_store = FileStore::open(&cas_path, 4 * 1024 * 1024)
            .expect("cold reopen V3 FileStore for consumer proof");
        let (base_cold, base_cold_io) = cold_reverify_v3(&cold_store, &base);
        assert_eq!(&base_cold, base.verified_content());
        let expected_segment_reads = (base.segment_admissions().len() as u64) * 2;
        let expected_segment_payload_bytes = base
            .segment_admissions()
            .iter()
            .map(|receipt| receipt.payload_bytes())
            .sum::<u64>()
            * 2;
        let expected_segment_envelope_bytes = base
            .segment_admissions()
            .iter()
            .map(|receipt| receipt.envelope_bytes())
            .sum::<u64>()
            * 2;
        assert_eq!(base_cold_io.segment_object_reads(), expected_segment_reads);
        assert_eq!(
            base_cold_io.segment_payload_bytes(),
            expected_segment_payload_bytes
        );
        assert_eq!(
            base_cold_io.segment_envelope_bytes(),
            expected_segment_envelope_bytes
        );
        assert_eq!(base_cold_io, base.verifier_io_metrics());
        assert!(base_cold_io.jumbo_leaf_object_reads() > 0);
        assert!(base_cold_io.jumbo_interior_object_reads() > 0);

        // A separate full-image reader reconstructs the same semantics in a
        // new residence and reruns all seven family encoders. CAS reuse must
        // produce byte-identical c007 roots without newly stored objects.
        let image = v1_image(&base_ir);
        let reopened_reader = SemanticImageView::reopen(&image)
            .expect("independent full semantic reader reopens the fixture");
        let no_op = produce_v3(&store, &reopened_reader, V3FixtureShape::Base, 11);
        assert_eq!(no_op.manifest(), base.manifest());
        assert_eq!(no_op.verified_content(), base.verified_content());
        assert_eq!(no_op.encoding_metrics(), base.encoding_metrics());
        for stored in no_op.segment_store_metrics() {
            assert_eq!(stored.created_objects(), 0);
            assert_eq!(stored.reused_objects(), stored.attempted_objects());
            assert_eq!(
                stored.admitted_envelope_bytes(),
                stored.cas_hit_compare_envelope_bytes()
            );
        }
        assert_eq!(no_op.jumbo_store_metrics().created_objects(), 0);
        assert_eq!(
            no_op.jumbo_store_metrics().reused_objects(),
            no_op.jumbo_store_metrics().attempted_objects()
        );
        assert_eq!(
            no_op.jumbo_store_metrics().admitted_envelope_bytes(),
            no_op.jumbo_store_metrics().cas_hit_compare_envelope_bytes()
        );
        let (no_op_cold, no_op_cold_io) = cold_reverify_v3(&cold_store, &no_op);
        assert_eq!(no_op_cold, *base.verified_content());
        assert_eq!(no_op_cold_io, base_cold_io);

        let claim_only_store_path = directory.0.join("claim-only-cas");
        let claim_only_store = FileStore::open(&claim_only_store_path, 4 * 1024 * 1024)
            .expect("open claim-only rejection FileStore");
        let claim_only =
            SemanticInputWitness::claimed([0x31; 32], ScopeRoot::from_bytes([0x32; 32]));
        assert!(
            produce_semantic_typed_plane_v3(
                &claim_only_store,
                &base_ir,
                v3_build_identity(),
                claim_only,
                v3_boundary_policies(),
                SemanticTypedPlaneVerificationTierV2::Standard,
                JumboRopeLimits::default(),
            )
            .is_err()
        );
        assert_eq!(
            fs::read_dir(claim_only_store_path.join("objects"))
                .expect("claim-only FileStore object directory exists")
                .count(),
            0,
            "claim-only Complete is rejected before any durable segment or rope write"
        );

        for (shape, input_identity) in [
            (V3FixtureShape::RenameEarly, 12),
            (V3FixtureShape::RenameMiddle, 13),
            (V3FixtureShape::RenameTail, 14),
            (V3FixtureShape::EarlyEdit, 15),
            (V3FixtureShape::MiddleEdit, 16),
            (V3FixtureShape::TailEdit, 17),
            (V3FixtureShape::MiddleInsert, 18),
            (V3FixtureShape::MiddleDelete, 19),
        ] {
            let target_ir = v3_fixture(shape);
            let target = produce_v3(&store, &target_ir, shape, input_identity);
            let (verified, cold_io) = cold_reverify_v3(&cold_store, &target);
            assert_ne!(
                verified.content_root(),
                base_cold.content_root(),
                "an independent reader edit changes the typed content root"
            );
            assert_ne!(
                verified.generation_root(),
                base_cold.generation_root(),
                "the changed source admission changes the derivation root"
            );
            assert_eq!(target.manifest().families().len(), TYPED_V2_FAMILY_COUNT);
            for (index, family) in target.manifest().families().iter().enumerate() {
                assert_eq!(
                    target.encoding_metrics()[index].row_count(),
                    family.row_count()
                );
                assert_eq!(
                    target.encoding_metrics()[index].segment_count(),
                    family.segments().len() as u64
                );
                assert_eq!(
                    target.segment_store_metrics()[index].semantic_segment_hash_bytes(),
                    target.encoding_metrics()[index].output_bytes()
                );
            }
            let (new_segments, removed_segments, new_segment_payload_bytes) =
                v3_segment_churn(&base, &target);
            assert!(
                new_segments + removed_segments > 0,
                "{} changes at least one family-scoped segment identity",
                shape.label()
            );
            assert_eq!(cold_io, target.verifier_io_metrics());
            let canonical_rows_encoded = target
                .encoding_metrics()
                .iter()
                .map(|metrics| metrics.row_encode_calls())
                .sum::<u64>();
            let full_family_row_index_bytes = target
                .encoding_metrics()
                .iter()
                .map(|metrics| metrics.row_index_capacity_bytes())
                .sum::<u64>();
            let encoded_spir_bytes = target
                .encoding_metrics()
                .iter()
                .map(|metrics| metrics.output_bytes())
                .sum::<u64>();
            println!(
                "v3 {}: canonical_rows_encoded={} full_family_row_index_capacity_bytes={} encoded_spir_payload_bytes={} new_segment_ids={} removed_segment_ids={} new_segment_payload_bytes={} verifier_segment_reads={} verifier_segment_payload_bytes={} verifier_segment_envelope_bytes={} verifier_jumbo_leaf_reads={} verifier_jumbo_interior_reads={} verifier_jumbo_payload_bytes={} verifier_jumbo_envelope_bytes={}",
                shape.label(),
                canonical_rows_encoded,
                full_family_row_index_bytes,
                encoded_spir_bytes,
                new_segments,
                removed_segments,
                new_segment_payload_bytes,
                cold_io.segment_object_reads(),
                cold_io.segment_payload_bytes(),
                cold_io.segment_envelope_bytes(),
                cold_io.jumbo_leaf_object_reads(),
                cold_io.jumbo_interior_object_reads(),
                cold_io.jumbo_payload_bytes(),
                cold_io.jumbo_envelope_bytes(),
            );
        }
    }
}
