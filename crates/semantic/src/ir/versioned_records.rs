//! Canonical stable-key row records used as the physical encoding for
//! versioned semantic-plane segments.
//!
//! Records are projections of the existing [`SemanticReader`] contract. They
//! are not another IR model: keys and payload fields are derived from the same
//! admitted rows. The V1 `GenerationId` remains the digest of materialized NXFI
//! bytes; typed semantic authority V2 is a separate root over the complete
//! admitted row-family closure. The `SPIR` envelope groups typed rows into
//! bounded, independently addressable segment payloads.

use alloc::{boxed::Box, vec::Vec};
use core::mem::size_of;

use thiserror::Error;

pub use super::segment_boundary_policy::SemanticPlaneSegmentBoundaryPolicy as CanonicalPlaneSegmentBoundaryPolicy;
use crate::ir::{
    DeclarationIdentity, JumboRopeObjectSink, JumboRopeObjectSource, SemanticInputWitness,
    SemanticIrPlane, SemanticPlaneKind, SemanticPlaneSegment, SemanticReader, SemanticSegmentId,
};

const MAGIC: [u8; 4] = *b"SPIR";
const VERSION: u16 = 2;
const HEADER_BYTES: usize = 4 + 2 + 1 + 4;
pub(crate) const SPIR_HEADER_BYTES: usize = HEADER_BYTES;
const RECORD_HEADER_BYTES: usize = 32 + 1 + 4;
const INITIAL_PREFIX_BITS: u16 = 8;

// Keep the test oracle separate from the payload source's fetch counter. A
// single borrowed source call can still lead to several independent row
// traversals, which is the cost this verifier's streaming tests need to see.
#[cfg(test)]
std::thread_local! {
    static RECORD_TRAVERSAL_METRICS: core::cell::Cell<(u64, u64)> = const {
        core::cell::Cell::new((0, 0))
    };
}

#[cfg(test)]
pub(crate) fn reset_record_traversal_metrics() {
    RECORD_TRAVERSAL_METRICS.with(|metrics| metrics.set((0, 0)));
}

#[cfg(test)]
pub(crate) fn record_traversal_metrics() -> (u64, u64) {
    RECORD_TRAVERSAL_METRICS.with(core::cell::Cell::get)
}

#[cfg(test)]
fn note_record_traversal(encoded_len: usize) {
    RECORD_TRAVERSAL_METRICS.with(|metrics| {
        let (rows, bytes) = metrics.get();
        metrics.set((
            rows.saturating_add(1),
            bytes.saturating_add(u64::try_from(encoded_len).unwrap_or(u64::MAX)),
        ));
    });
}

#[cfg(not(test))]
#[inline(always)]
fn note_record_traversal(_encoded_len: usize) {}

pub(crate) mod aggregate;
mod declarations;
mod extensions;
mod occurrences;
mod relations;
mod source_provenance;
mod types;
mod wire;

pub use declarations::{CoreDeclarationRows, DocumentationRows, encode_declaration_planes};
pub use extensions::{
    CheckedLanguageExtensionFamilyV2, LanguageExtensionRows, LanguageExtensionVerificationLimitsV2,
    encode_language_extension_plane, validate_language_extension_family_v2,
    validate_language_extension_family_v2_with_limits,
    verify_language_extension_plane_against_reader,
};
pub use occurrences::{OccurrenceHandle, OccurrenceRows};
pub use relations::RelationRows;
pub use source_provenance::{SourceProvenanceHandle, SourceProvenanceRows};
pub use types::{
    CheckedTypesFamilyV2, TypedRecordPlan, TypesClosureSemantics, TypesFamilyVerificationLimitsV2,
    TypesReferenceV2, TypesRowDomainV2, TypesRowHandle, TypesRows, validate_types_family_v2,
    validate_types_family_v2_with_limits,
};

/// One compact handle to a row in the borrowed canonical reader.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CanonicalSemanticPlaneRowKey<Handle: Copy> {
    key: [u8; 32],
    handle: Handle,
}

/// Key-only inventory collected before row payloads are encoded.
///
/// Keeping only stable keys and reader-local handles avoids retaining a
/// second copy of the complete semantic plane while records are sorted.
pub struct CanonicalSemanticPlaneKeySink<Handle: Copy> {
    kind: SemanticPlaneKind,
    maximum_rows: Option<u64>,
    rows: Vec<CanonicalSemanticPlaneRowKey<Handle>>,
}

impl<Handle: Copy> CanonicalSemanticPlaneKeySink<Handle> {
    /// Starts collecting keys for one exact IR plane.
    #[must_use]
    pub fn new(kind: SemanticPlaneKind) -> Self {
        Self {
            kind,
            maximum_rows: None,
            rows: Vec::new(),
        }
    }

    /// Starts collecting keys with a hard family row ceiling. Push rejects
    /// before growing the key/handle inventory beyond this count.
    #[must_use]
    pub fn with_row_limit(kind: SemanticPlaneKind, maximum_rows: u64) -> Self {
        Self {
            kind,
            maximum_rows: Some(maximum_rows),
            rows: Vec::new(),
        }
    }

    /// Adds one stable row key and its non-persisted reader handle.
    pub fn push(&mut self, key: [u8; 32], handle: Handle) -> Result<(), SemanticPlaneRecordError> {
        self.check_row_count(self.rows.len())?;
        self.rows
            .try_reserve(1)
            .map_err(SemanticPlaneRecordError::Allocation)?;
        self.rows.push(CanonicalSemanticPlaneRowKey { key, handle });
        Ok(())
    }

    /// Checks whether a producer-owned temporary row inventory can accept
    /// one more entry under this sink's aggregate family ceiling.
    pub(crate) fn check_row_count(
        &self,
        current_count: usize,
    ) -> Result<(), SemanticPlaneRecordError> {
        if let Some(maximum) = self.maximum_rows
            && u64::try_from(current_count).map_or(true, |current| current >= maximum)
        {
            return Err(SemanticPlaneRecordError::RowBudgetExceeded { maximum });
        }
        Ok(())
    }

    /// Plane whose keys are being collected.
    #[must_use]
    pub const fn kind(&self) -> SemanticPlaneKind {
        self.kind
    }
}

/// One owned canonical `SPIR` payload and the metadata needed to claim it.
#[derive(Clone, Debug)]
pub struct CanonicalSemanticPlaneSegmentPayload {
    kind: SemanticPlaneKind,
    first_key: [u8; 32],
    last_key: [u8; 32],
    row_count: u32,
    input: SemanticInputWitness,
    bytes: Box<[u8]>,
}

/// One not-yet-locally-validated segment borrowed only for the duration of a
/// streaming sink call. [`Self::metadata`] constructs a byte identity claim;
/// use [`Self::validate`] before crossing a durable producer boundary.
///
/// Sinks can hash, persist, or forward the segment before the encoder reuses
/// its bounded segment buffer. The borrowed bytes cannot be retained without
/// an explicit copy.
#[derive(Clone, Copy, Debug)]
pub struct CanonicalSemanticPlaneSegmentRef<'bytes> {
    kind: SemanticPlaneKind,
    first_key: [u8; 32],
    last_key: [u8; 32],
    row_count: u32,
    input: SemanticInputWitness,
    bytes: &'bytes [u8],
}

impl<'bytes> CanonicalSemanticPlaneSegmentRef<'bytes> {
    /// Exact IR plane encoded in this segment.
    #[must_use]
    pub const fn kind(self) -> SemanticPlaneKind {
        self.kind
    }
    /// First stable key in this segment.
    #[must_use]
    pub const fn first_key(self) -> [u8; 32] {
        self.first_key
    }
    /// Last stable key in this segment.
    #[must_use]
    pub const fn last_key(self) -> [u8; 32] {
        self.last_key
    }
    /// Number of rows in this segment.
    #[must_use]
    pub const fn row_count(self) -> u32 {
        self.row_count
    }
    /// Exact canonical SPIR bytes, borrowed for the sink call only.
    #[must_use]
    pub const fn bytes(self) -> &'bytes [u8] {
        self.bytes
    }
    /// Creates the exact descriptor claim for these borrowed bytes.
    pub fn metadata(self) -> Result<SemanticPlaneSegment, SemanticPlaneRecordError> {
        Ok(SemanticPlaneSegment::from_payload_with_witness(
            self.kind,
            self.first_key,
            self.last_key,
            self.row_count,
            self.bytes,
            self.input,
        )?)
    }

    /// Checks the exact SPIR framing, row grammars, stable-key order, key
    /// range, and content identity before this borrowed payload crosses a
    /// storage boundary.
    ///
    /// This is local segment validation only. Cross-family references and the
    /// complete seven-family census still require aggregate admission.
    pub fn validate(
        self,
    ) -> Result<ValidatedCanonicalSemanticPlaneSegment<'bytes>, SemanticPlaneRecordError> {
        self.validate_with_row_limit(crate::ir::MAX_SEMANTIC_SEGMENT_BYTES)
    }

    /// Validates this segment using the exact family policy's inline-row
    /// threshold for canonical jumbo placement.
    pub fn validate_with_row_limit(
        self,
        maximum_inline_row_bytes: usize,
    ) -> Result<ValidatedCanonicalSemanticPlaneSegment<'bytes>, SemanticPlaneRecordError> {
        let descriptor = self.metadata()?;
        let view = decode_semantic_plane_segment_structure(
            self.kind,
            &descriptor,
            self.bytes,
            maximum_inline_row_bytes,
        )?;
        let id = descriptor
            .admitted_id()
            .ok_or(SemanticPlaneRecordError::MissingAdmittedId)?;
        Ok(ValidatedCanonicalSemanticPlaneSegment {
            descriptor,
            id,
            view,
            bytes: self.bytes,
        })
    }
}

/// Borrowed SPIR bytes that passed exact local segment validation.
///
/// This capability is deliberately weaker than family or generation
/// admission: it proves the bytes match their segment claim and closed row
/// grammar, but does not prove cross-family references or owner input
/// completeness.
#[derive(Clone, Copy, Debug)]
pub struct ValidatedCanonicalSemanticPlaneSegment<'bytes> {
    descriptor: SemanticPlaneSegment,
    id: SemanticSegmentId,
    view: CanonicalSemanticPlaneSegmentView<'bytes>,
    bytes: &'bytes [u8],
}

impl<'bytes> ValidatedCanonicalSemanticPlaneSegment<'bytes> {
    /// Locally admitted content identity for the exact SPIR bytes.
    #[must_use]
    pub const fn id(self) -> SemanticSegmentId {
        self.id
    }

    /// Exact canonical IR family carried by the validated payload.
    #[must_use]
    pub const fn kind(self) -> SemanticPlaneKind {
        self.view.kind()
    }

    /// First stable key in this segment.
    #[must_use]
    pub const fn first_key(self) -> [u8; 32] {
        self.view.first_key()
    }

    /// Last stable key in this segment.
    #[must_use]
    pub const fn last_key(self) -> [u8; 32] {
        self.view.last_key()
    }

    /// Exact locally checked row count.
    #[must_use]
    pub const fn row_count(self) -> u32 {
        self.view.row_count()
    }

    /// Exact SPIR payload bytes, borrowed for the current sink call.
    #[must_use]
    pub const fn bytes(self) -> &'bytes [u8] {
        self.bytes
    }

    /// Claim reconstructed from the exact bytes admitted by this token.
    #[must_use]
    pub const fn descriptor(self) -> SemanticPlaneSegment {
        self.descriptor
    }
}

/// Receives one bounded segment at a time from the canonical family encoder.
pub trait CanonicalSemanticPlaneSegmentSink {
    /// Sink-specific persistence or transport failure.
    type Error;

    /// Consumes one borrowed segment before the encoder reuses its buffer.
    fn write_segment(
        &mut self,
        segment: CanonicalSemanticPlaneSegmentRef<'_>,
    ) -> Result<(), Self::Error>;
}

/// Error from encoding a family into a caller-owned streaming sink.
#[derive(Debug, Error)]
pub enum CanonicalPlaneStreamError<SinkError> {
    /// A canonical reader, grammar, allocation, or segment invariant failed.
    #[error(transparent)]
    Encoding(#[from] SemanticPlaneRecordError),
    /// The sink failed while accepting one complete segment.
    #[error("canonical semantic-plane segment sink failed")]
    Sink(SinkError),
}

/// Error from encoding and visiting canonical typed rows for a caller-owned sink.
#[derive(Debug, Error)]
pub enum CanonicalPlaneRowStreamError<SinkError> {
    /// A canonical reader, grammar, allocation, or row invariant failed.
    #[error(transparent)]
    Encoding(#[from] SemanticPlaneRecordError),
    /// The row sink failed while accepting one borrowed row.
    #[error("canonical semantic-plane row sink failed")]
    Sink(SinkError),
}

/// Receives one grammar-checked typed row from the canonical family encoder.
///
/// The row payload is borrowed from one reusable encoder scratch buffer and is
/// valid only until this method returns. A sink that persists the bytes must
/// complete that write before returning or make its own explicit copy.
pub trait CanonicalSemanticPlaneRowSink {
    /// Sink-specific persistence or indexing failure.
    type Error;

    /// Accepts one admitted row in strictly increasing stable-key order.
    fn write_row(
        &mut self,
        family: SemanticIrPlane,
        row: CanonicalSemanticPlaneRecordView<'_>,
    ) -> Result<(), Self::Error>;
}

impl CanonicalSemanticPlaneSegmentPayload {
    /// Exact IR plane whose rows are encoded in the payload.
    #[must_use]
    pub const fn kind(&self) -> SemanticPlaneKind {
        self.kind
    }

    /// First stable key in the strictly ordered range.
    #[must_use]
    pub const fn first_key(&self) -> &[u8; 32] {
        &self.first_key
    }

    /// Last stable key in the strictly ordered range.
    #[must_use]
    pub const fn last_key(&self) -> &[u8; 32] {
        &self.last_key
    }

    /// Number of complete rows in the payload.
    #[must_use]
    pub const fn row_count(&self) -> u32 {
        self.row_count
    }

    /// Exact canonical bytes admitted by the segment identity.
    #[must_use]
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    /// Builds the existing segment claim over the exact emitted bytes.
    pub fn metadata(&self) -> Result<SemanticPlaneSegment, SemanticPlaneRecordError> {
        Ok(SemanticPlaneSegment::from_payload_with_witness(
            self.kind,
            self.first_key,
            self.last_key,
            self.row_count,
            &self.bytes,
            self.input,
        )?)
    }

    /// Checks the exact SPIR framing, row grammars, stable-key order, key
    /// range, and content identity before this owned payload crosses a
    /// storage boundary.
    pub fn validate(
        &self,
    ) -> Result<ValidatedCanonicalSemanticPlaneSegment<'_>, SemanticPlaneRecordError> {
        CanonicalSemanticPlaneSegmentRef {
            kind: self.kind,
            first_key: self.first_key,
            last_key: self.last_key,
            row_count: self.row_count,
            input: self.input,
            bytes: &self.bytes,
        }
        .validate()
    }

    /// Consumes the descriptor wrapper and returns its exact owned bytes.
    #[must_use]
    pub fn into_bytes(self) -> Box<[u8]> {
        self.bytes
    }
}

/// Measured work and retained scratch for one row-family encoding pass.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CanonicalPlaneEncodingMetrics {
    row_count: u64,
    segment_count: u64,
    output_bytes: u64,
    row_encode_calls: u64,
    plan_build_calls: u64,
    key_collection_calls: u64,
    key_sort_rows: u64,
    anchor_hash_rows: u64,
    anchor_key_hash_bytes: u64,
    peak_row_scratch_capacity_bytes: u64,
    peak_segment_scratch_capacity_bytes: u64,
    peak_jumbo_rope_scratch_bytes: u64,
    peak_tracked_scratch_upper_bound_bytes: u64,
    row_index_capacity_bytes: u64,
}

impl CanonicalPlaneEncodingMetrics {
    /// Number of typed rows encoded.
    #[must_use]
    pub const fn row_count(self) -> u64 {
        self.row_count
    }
    /// Number of emitted bounded segments.
    #[must_use]
    pub const fn segment_count(self) -> u64 {
        self.segment_count
    }
    /// Total final SPIR payload bytes emitted by this family.
    #[must_use]
    pub const fn output_bytes(self) -> u64 {
        self.output_bytes
    }
    /// Number of row encoder calls.
    #[must_use]
    pub const fn row_encode_calls(self) -> u64 {
        self.row_encode_calls
    }
    /// Number of complete family-plan builder calls.
    #[must_use]
    pub const fn plan_build_calls(self) -> u64 {
        self.plan_build_calls
    }
    /// Number of complete family stable-key collection calls.
    #[must_use]
    pub const fn key_collection_calls(self) -> u64 {
        self.key_collection_calls
    }
    /// Number of full-family key/handle entries passed through the sort.
    #[must_use]
    pub const fn key_sort_rows(self) -> u64 {
        self.key_sort_rows
    }
    /// Stable 32-byte row-key input bytes traversed by the opt-in ramp rule.
    #[must_use]
    pub const fn anchor_key_hash_bytes(self) -> u64 {
        self.anchor_key_hash_bytes
    }
    /// Number of stable row keys hashed by the opt-in ramp rule.
    #[must_use]
    pub const fn anchor_hash_rows(self) -> u64 {
        self.anchor_hash_rows
    }
    /// Largest reusable row scratch capacity observed during emission.
    #[must_use]
    pub const fn peak_row_scratch_capacity_bytes(self) -> u64 {
        self.peak_row_scratch_capacity_bytes
    }
    /// Largest bounded output-segment buffer capacity observed.
    #[must_use]
    pub const fn peak_segment_scratch_capacity_bytes(self) -> u64 {
        self.peak_segment_scratch_capacity_bytes
    }
    /// Largest live scratch reported by a jumbo value writer during this
    /// family pass. This is a separate component from row and segment buffers.
    #[must_use]
    pub const fn peak_jumbo_rope_scratch_bytes(self) -> u64 {
        self.peak_jumbo_rope_scratch_bytes
    }
    /// Measured sum of the retained key-index capacity and the maximum row,
    /// segment, and jumbo-writer scratch capacities observed together in one
    /// row pass. The key index is O(rows) for the complete family, so this is
    /// not a constant memory bound; encoder plans and sink-owned storage are
    /// excluded.
    #[must_use]
    pub const fn peak_tracked_scratch_upper_bound_bytes(self) -> u64 {
        self.peak_tracked_scratch_upper_bound_bytes
    }
    /// Allocated capacity of the full-family O(rows) key/handle/length index.
    /// This capacity is not bounded by the maximum output-segment size.
    #[must_use]
    pub const fn row_index_capacity_bytes(self) -> u64 {
        self.row_index_capacity_bytes
    }
}

/// Measured work and retained scratch for producing one complete row family.
///
/// The plan/key counters report calls to the encoder hooks, not the number of
/// underlying `SemanticReader` records those hooks traverse. Production still
/// builds the full family plan, collects all stable keys, sorts all key/handle
/// entries, and encodes every row unless a separate caller has proved a
/// complete changed-key frontier.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CanonicalPlaneRowEncodingMetrics {
    row_count: u64,
    row_encode_calls: u64,
    plan_build_calls: u64,
    key_collection_calls: u64,
    key_sort_rows: u64,
    peak_row_scratch_capacity_bytes: u64,
    peak_jumbo_rope_scratch_bytes: u64,
    peak_tracked_row_scratch_upper_bound_bytes: u64,
    row_index_capacity_bytes: u64,
}

impl CanonicalPlaneRowEncodingMetrics {
    /// Number of typed rows admitted and delivered to the sink.
    #[must_use]
    pub const fn row_count(self) -> u64 {
        self.row_count
    }

    /// Number of canonical row encoder calls.
    #[must_use]
    pub const fn row_encode_calls(self) -> u64 {
        self.row_encode_calls
    }

    /// Number of complete family-plan builder calls.
    #[must_use]
    pub const fn plan_build_calls(self) -> u64 {
        self.plan_build_calls
    }

    /// Number of complete family stable-key collection calls.
    #[must_use]
    pub const fn key_collection_calls(self) -> u64 {
        self.key_collection_calls
    }

    /// Number of full-family key/handle entries passed through the sort.
    #[must_use]
    pub const fn key_sort_rows(self) -> u64 {
        self.key_sort_rows
    }

    /// Largest reusable row scratch capacity observed during emission.
    #[must_use]
    pub const fn peak_row_scratch_capacity_bytes(self) -> u64 {
        self.peak_row_scratch_capacity_bytes
    }

    /// Largest live scratch reported by a jumbo value writer during this pass.
    #[must_use]
    pub const fn peak_jumbo_rope_scratch_bytes(self) -> u64 {
        self.peak_jumbo_rope_scratch_bytes
    }

    /// Largest observed sum of the key index, row scratch, and jumbo-writer
    /// scratch. Encoder plans and sink-owned storage are excluded.
    #[must_use]
    pub const fn peak_tracked_row_scratch_upper_bound_bytes(self) -> u64 {
        self.peak_tracked_row_scratch_upper_bound_bytes
    }

    /// Allocated capacity of the full-family O(rows) key/handle index.
    #[must_use]
    pub const fn row_index_capacity_bytes(self) -> u64 {
        self.row_index_capacity_bytes
    }
}

/// Encoded family output together with objective byte-work measurements.
#[derive(Clone, Debug)]
pub struct MeasuredCanonicalPlaneEncoding {
    segments: Box<[CanonicalSemanticPlaneSegmentPayload]>,
    metrics: CanonicalPlaneEncodingMetrics,
}

impl MeasuredCanonicalPlaneEncoding {
    /// Borrows the ordered segment payloads.
    #[must_use]
    pub fn segments(&self) -> &[CanonicalSemanticPlaneSegmentPayload] {
        &self.segments
    }
    /// Returns the measurements for this exact encoding pass.
    #[must_use]
    pub const fn metrics(&self) -> CanonicalPlaneEncodingMetrics {
        self.metrics
    }
    /// Consumes the measurements wrapper and returns the owned segments.
    #[must_use]
    pub fn into_segments(self) -> Box<[CanonicalSemanticPlaneSegmentPayload]> {
        self.segments
    }
}

/// One borrowed row recovered from a canonical `SPIR` payload.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CanonicalSemanticPlaneRecordView<'bytes> {
    key: [u8; 32],
    tag: u8,
    payload: &'bytes [u8],
}

impl<'bytes> CanonicalSemanticPlaneRecordView<'bytes> {
    /// Stable row identity committed by the containing plane range.
    #[must_use]
    pub const fn key(self) -> [u8; 32] {
        self.key
    }

    /// Closed row tag selected by the semantic plane family.
    #[must_use]
    pub const fn tag(self) -> u8 {
        self.tag
    }

    /// Exact canonical bytes for this typed row.
    #[must_use]
    pub const fn payload(self) -> &'bytes [u8] {
        self.payload
    }

    /// Exact row bytes in the enclosing SPIR stream, including fixed framing.
    #[must_use]
    pub const fn encoded_len(self) -> usize {
        RECORD_HEADER_BYTES + self.payload.len()
    }
}

/// Strict borrowed view of one verified `SPIR` segment.
#[derive(Clone, Copy, Debug)]
pub struct CanonicalSemanticPlaneSegmentView<'bytes> {
    kind: SemanticPlaneKind,
    first_key: [u8; 32],
    last_key: [u8; 32],
    row_count: u32,
    rows: &'bytes [u8],
}

impl<'bytes> CanonicalSemanticPlaneSegmentView<'bytes> {
    /// Exact plane represented by this segment.
    #[must_use]
    pub const fn kind(self) -> SemanticPlaneKind {
        self.kind
    }

    /// Claimed first key, checked against the first decoded row.
    #[must_use]
    pub const fn first_key(self) -> [u8; 32] {
        self.first_key
    }

    /// Claimed last key, checked against the last decoded row.
    #[must_use]
    pub const fn last_key(self) -> [u8; 32] {
        self.last_key
    }

    /// Exact row count checked against the segment descriptor.
    #[must_use]
    pub const fn row_count(self) -> u32 {
        self.row_count
    }

    /// Exact SPIR byte length including its fixed header.
    #[must_use]
    pub const fn encoded_len(self) -> usize {
        HEADER_BYTES + self.rows.len()
    }

    /// Reopens the already validated borrowed records without allocating.
    #[must_use]
    pub fn records(self) -> CanonicalSemanticPlaneRecordCursor<'bytes> {
        CanonicalSemanticPlaneRecordCursor {
            bytes: self.rows,
            next: 0,
            count: self.row_count,
        }
    }
}

/// Exact-size cursor over the rows in one validated `SPIR` segment.
pub struct CanonicalSemanticPlaneRecordCursor<'bytes> {
    bytes: &'bytes [u8],
    next: u32,
    count: u32,
}

impl<'bytes> Iterator for CanonicalSemanticPlaneRecordCursor<'bytes> {
    type Item = CanonicalSemanticPlaneRecordView<'bytes>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.next >= self.count {
            return None;
        }
        let key: [u8; 32] = self.bytes.get(..32)?.try_into().ok()?;
        let tag = *self.bytes.get(32)?;
        let length = u32::from_be_bytes(self.bytes.get(33..37)?.try_into().ok()?);
        let length = usize::try_from(length).ok()?;
        let end = 37_usize.checked_add(length)?;
        let payload = self.bytes.get(37..end)?;
        note_record_traversal(RECORD_HEADER_BYTES + length);
        self.bytes = self.bytes.get(end..)?;
        self.next += 1;
        Some(CanonicalSemanticPlaneRecordView { key, tag, payload })
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        let count = self.count.saturating_sub(self.next) as usize;
        (count, Some(count))
    }
}

impl ExactSizeIterator for CanonicalSemanticPlaneRecordCursor<'_> {}

/// Encodes one existing semantic-plane family from the canonical reader.
pub trait CanonicalPlaneRowEncoder {
    /// Reader-local handle type retained during stable-key sorting.
    type Handle: Copy;
    /// Reusable family-specific structural facts, built once per encoding pass.
    type Plan;

    /// Exact plane selected by this row family.
    fn kind(&self) -> SemanticPlaneKind;

    /// Builds reusable family scratch once for this reader.
    fn build_plan<Reader: SemanticReader + ?Sized>(
        &self,
        reader: &Reader,
    ) -> Result<Self::Plan, SemanticPlaneRecordError>;

    /// Builds reusable facts under a family row and reference-work ceiling.
    /// Encoders with potentially large plan scratch should enforce these
    /// ceilings while traversing rather than after materializing the plan.
    fn build_plan_with_limits<Reader: SemanticReader + ?Sized>(
        &self,
        reader: &Reader,
        _maximum_rows: u64,
        _maximum_references: u64,
    ) -> Result<Self::Plan, SemanticPlaneRecordError> {
        self.build_plan(reader)
    }

    /// Emits existing canonical-reader keys and ephemeral handles into the sink.
    fn collect_keys<Reader: SemanticReader + ?Sized>(
        &self,
        reader: &Reader,
        plan: &Self::Plan,
        sink: &mut CanonicalSemanticPlaneKeySink<Self::Handle>,
    ) -> Result<(), SemanticPlaneRecordError>;

    /// Encodes one row into reusable bounded scratch and returns its closed tag.
    fn encode_row<Reader: SemanticReader + ?Sized>(
        &self,
        reader: &Reader,
        plan: &Self::Plan,
        handle: Self::Handle,
        payload: &mut Vec<u8>,
    ) -> Result<u8, SemanticPlaneRecordError>;

    /// Encodes one row with access to the existing content-addressed jumbo
    /// object sink. Families without jumbo fields use the default row writer.
    /// Implementations must emit a typed descriptor in the row and stream
    /// oversized field bytes through `jumbo_sink`; they must not split SPIR
    /// record bytes across semantic segments.
    fn encode_row_with_jumbo<Reader: SemanticReader + ?Sized>(
        &self,
        reader: &Reader,
        plan: &Self::Plan,
        handle: Self::Handle,
        _jumbo_sink: Option<&mut dyn JumboRopeObjectSink<Error = SemanticPlaneRecordError>>,
        payload: &mut Vec<u8>,
    ) -> Result<u8, SemanticPlaneRecordError> {
        self.encode_row(reader, plan, handle, payload)
    }

    /// Measured jumbo-aware row encoding. Existing family encoders retain the
    /// default behavior; jumbo-aware built-ins report the value writer's
    /// bounded scratch peak through `peak_jumbo_scratch_bytes`.
    fn encode_row_with_jumbo_measured<Reader: SemanticReader + ?Sized>(
        &self,
        reader: &Reader,
        plan: &Self::Plan,
        handle: Self::Handle,
        jumbo_sink: Option<&mut dyn JumboRopeObjectSink<Error = SemanticPlaneRecordError>>,
        _peak_jumbo_scratch_bytes: &mut u64,
        payload: &mut Vec<u8>,
    ) -> Result<u8, SemanticPlaneRecordError> {
        self.encode_row_with_jumbo(reader, plan, handle, jumbo_sink, payload)
    }

    /// Encodes one measured row using the active segment byte ceiling when
    /// deciding whether jumbo-capable fields must be externalized. Ordinary
    /// encoders keep their existing behavior through this default.
    fn encode_row_with_jumbo_measured_for_segment_limit<Reader: SemanticReader + ?Sized>(
        &self,
        reader: &Reader,
        plan: &Self::Plan,
        handle: Self::Handle,
        jumbo_sink: Option<&mut dyn JumboRopeObjectSink<Error = SemanticPlaneRecordError>>,
        maximum_segment_bytes: usize,
        jumbo_limits: crate::ir::JumboRopeLimits,
        peak_jumbo_scratch_bytes: &mut u64,
        payload: &mut Vec<u8>,
    ) -> Result<u8, SemanticPlaneRecordError> {
        let _ = (maximum_segment_bytes, jumbo_limits);
        self.encode_row_with_jumbo_measured(
            reader,
            plan,
            handle,
            jumbo_sink,
            peak_jumbo_scratch_bytes,
            payload,
        )
    }
}

/// Encodes and partitions one family without minting a generation identity.
pub fn encode_canonical_plane_family<Reader, Encoder>(
    reader: &Reader,
    encoder: &Encoder,
    input: SemanticInputWitness,
    maximum_bytes: usize,
) -> Result<Box<[CanonicalSemanticPlaneSegmentPayload]>, SemanticPlaneRecordError>
where
    Reader: SemanticReader + ?Sized,
    Encoder: CanonicalPlaneRowEncoder + ?Sized,
{
    let mut sink = OwnedPlaneSegments::default();
    stream_canonical_plane_family(reader, encoder, input, maximum_bytes, &mut sink)
        .map_err(unwrap_owned_sink_error)?;
    Ok(sink.segments.into_boxed_slice())
}

/// Encodes one complete plane family and reports measured payload and scratch
/// work. This compatibility wrapper retains the returned segments; callers
/// that persist or forward results should use [`stream_canonical_plane_family`]
/// to keep only one output segment resident at a time.
pub fn encode_canonical_plane_family_measured<Reader, Encoder>(
    reader: &Reader,
    encoder: &Encoder,
    input: SemanticInputWitness,
    maximum_bytes: usize,
) -> Result<MeasuredCanonicalPlaneEncoding, SemanticPlaneRecordError>
where
    Reader: SemanticReader + ?Sized,
    Encoder: CanonicalPlaneRowEncoder + ?Sized,
{
    let mut sink = OwnedPlaneSegments::default();
    let metrics = stream_canonical_plane_family(reader, encoder, input, maximum_bytes, &mut sink)
        .map_err(unwrap_owned_sink_error)?;
    Ok(MeasuredCanonicalPlaneEncoding {
        segments: sink.segments.into_boxed_slice(),
        metrics,
    })
}

fn unwrap_owned_sink_error(
    error: CanonicalPlaneStreamError<SemanticPlaneRecordError>,
) -> SemanticPlaneRecordError {
    match error {
        CanonicalPlaneStreamError::Encoding(error) | CanonicalPlaneStreamError::Sink(error) => {
            error
        }
    }
}

pub(super) fn map_jumbo_operation_error(
    error: crate::ir::JumboOperationError<SemanticPlaneRecordError>,
) -> SemanticPlaneRecordError {
    match error {
        crate::ir::JumboOperationError::Rope(error) => SemanticPlaneRecordError::JumboRope(error),
        crate::ir::JumboOperationError::Store(error) => error,
        crate::ir::JumboOperationError::Input(_) | crate::ir::JumboOperationError::Output(_) => {
            SemanticPlaneRecordError::JumboStream
        }
    }
}

/// Encodes a row family and lends each bounded output segment to `sink` before
/// reusing its segment buffer. The encoder retains a sorted compact key,
/// handle, and length index for the whole family (O(rows)), one row scratch
/// buffer, and one bounded segment payload buffer. The output-segment limit
/// bounds only segment scratch; it does not cap the full-family index. Rows
/// are encoded once; the sink can durably write each segment before the next
/// one is generated.
pub fn stream_canonical_plane_family<Reader, Encoder, Sink>(
    reader: &Reader,
    encoder: &Encoder,
    input: SemanticInputWitness,
    maximum_bytes: usize,
    sink: &mut Sink,
) -> Result<CanonicalPlaneEncodingMetrics, CanonicalPlaneStreamError<Sink::Error>>
where
    Reader: SemanticReader + ?Sized,
    Encoder: CanonicalPlaneRowEncoder + ?Sized,
    Sink: CanonicalSemanticPlaneSegmentSink + ?Sized,
{
    stream_canonical_plane_family_inner(
        reader,
        encoder,
        input,
        maximum_bytes,
        None,
        None,
        None,
        None,
        crate::ir::JumboRopeLimits::default(),
        None,
        sink,
    )
}

/// Encodes one complete row family while persisting jumbo field values through
/// the caller's existing object CAS. The typed descriptor remains one SPIR
/// semantic row; rope leaves and interior nodes are separate CAS objects.
///
/// This legacy prefix/size API uses the global `MAX_SEMANTIC_SEGMENT_BYTES`
/// threshold to decide when a row is externalized. `maximum_bytes` remains
/// only the hard segment ceiling. Callers whose manifest commits a smaller
/// family boundary policy should use a policy-aware stable-key entry point.
pub fn stream_canonical_plane_family_with_jumbo<Reader, Encoder, Sink>(
    reader: &Reader,
    encoder: &Encoder,
    input: SemanticInputWitness,
    maximum_bytes: usize,
    jumbo_sink: &mut dyn JumboRopeObjectSink<Error = SemanticPlaneRecordError>,
    sink: &mut Sink,
) -> Result<CanonicalPlaneEncodingMetrics, CanonicalPlaneStreamError<Sink::Error>>
where
    Reader: SemanticReader + ?Sized,
    Encoder: CanonicalPlaneRowEncoder + ?Sized,
    Sink: CanonicalSemanticPlaneSegmentSink + ?Sized,
{
    stream_canonical_plane_family_inner(
        reader,
        encoder,
        input,
        maximum_bytes,
        None,
        None,
        None,
        None,
        crate::ir::JumboRopeLimits::default(),
        Some(jumbo_sink),
        sink,
    )
}

/// Encodes one complete family with the opt-in stable-key hash-ramp policy.
///
/// Existing APIs keep their historical prefix/size cut rule. This explicit
/// entry point has a separate policy that must be recorded by any durable
/// manifest before consumers can treat its ranges as canonical.
pub fn stream_canonical_plane_family_with_stable_key_anchors<Reader, Encoder, Sink>(
    reader: &Reader,
    encoder: &Encoder,
    input: SemanticInputWitness,
    policy: CanonicalPlaneSegmentBoundaryPolicy,
    sink: &mut Sink,
) -> Result<CanonicalPlaneEncodingMetrics, CanonicalPlaneStreamError<Sink::Error>>
where
    Reader: SemanticReader + ?Sized,
    Encoder: CanonicalPlaneRowEncoder + ?Sized,
    Sink: CanonicalSemanticPlaneSegmentSink + ?Sized,
{
    stream_canonical_plane_family_inner(
        reader,
        encoder,
        input,
        policy.maximum_bytes(),
        Some(policy),
        None,
        None,
        None,
        crate::ir::JumboRopeLimits::default(),
        None,
        sink,
    )
}

/// Encodes a complete family with the stable-key hash-ramp policy while persisting
/// jumbo field values through the caller's existing object CAS. The policy's
/// maximum bytes define both the hard segment ceiling and the canonical inline
/// row threshold for Docs and SourceProvenance values.
pub fn stream_canonical_plane_family_with_jumbo_and_stable_key_anchors<Reader, Encoder, Sink>(
    reader: &Reader,
    encoder: &Encoder,
    input: SemanticInputWitness,
    policy: CanonicalPlaneSegmentBoundaryPolicy,
    jumbo_sink: &mut dyn JumboRopeObjectSink<Error = SemanticPlaneRecordError>,
    sink: &mut Sink,
) -> Result<CanonicalPlaneEncodingMetrics, CanonicalPlaneStreamError<Sink::Error>>
where
    Reader: SemanticReader + ?Sized,
    Encoder: CanonicalPlaneRowEncoder + ?Sized,
    Sink: CanonicalSemanticPlaneSegmentSink + ?Sized,
{
    stream_canonical_plane_family_inner(
        reader,
        encoder,
        input,
        policy.maximum_bytes(),
        Some(policy),
        None,
        None,
        None,
        crate::ir::JumboRopeLimits::default(),
        Some(jumbo_sink),
        sink,
    )
}

/// Bounded V3 producer entry point. The row/reference ceilings are aggregate
/// remaining-work budgets supplied by the caller; jumbo limits apply to every
/// emitted value and are checked before any leaf callback. The committed
/// family policy's maximum bytes also define the canonical inline row
/// threshold for Docs and SourceProvenance values.
pub fn stream_canonical_plane_family_with_jumbo_stable_key_anchors_and_limits<
    Reader,
    Encoder,
    Sink,
>(
    reader: &Reader,
    encoder: &Encoder,
    input: SemanticInputWitness,
    policy: CanonicalPlaneSegmentBoundaryPolicy,
    maximum_rows: u64,
    maximum_plan_rows: u64,
    maximum_references: u64,
    jumbo_limits: crate::ir::JumboRopeLimits,
    jumbo_sink: &mut dyn JumboRopeObjectSink<Error = SemanticPlaneRecordError>,
    sink: &mut Sink,
) -> Result<CanonicalPlaneEncodingMetrics, CanonicalPlaneStreamError<Sink::Error>>
where
    Reader: SemanticReader + ?Sized,
    Encoder: CanonicalPlaneRowEncoder + ?Sized,
    Sink: CanonicalSemanticPlaneSegmentSink + ?Sized,
{
    stream_canonical_plane_family_inner(
        reader,
        encoder,
        input,
        policy.maximum_bytes(),
        Some(policy),
        Some(maximum_rows),
        Some(maximum_plan_rows),
        Some(maximum_references),
        jumbo_limits,
        Some(jumbo_sink),
        sink,
    )
}

/// Encodes and grammar-checks one complete typed row family, lending each row
/// to `sink` before reusing the row scratch buffer.
///
/// `maximum_inline_row_bytes` must be the exact family policy threshold used
/// for canonical jumbo placement. `maximum_row_payload_bytes` bounds the row
/// payload accepted by the sink after canonical encoding; it does not cap the
/// transient scratch growth of an encoder. Row, plan, reference, and jumbo
/// limits bound producer work. This API checks each row's local grammar but
/// does not verify the complete seven-family closure, create SPIR framing, or
/// mint a generation identity. It still builds the full family plan, collects
/// and sorts all stable keys, and encodes every row; it does not authorize
/// sparse semantic reuse.
pub fn stream_canonical_plane_family_rows_with_limits<Reader, Encoder, Sink>(
    reader: &Reader,
    encoder: &Encoder,
    maximum_inline_row_bytes: usize,
    maximum_row_payload_bytes: usize,
    maximum_rows: u64,
    maximum_plan_rows: u64,
    maximum_references: u64,
    jumbo_limits: crate::ir::JumboRopeLimits,
    jumbo_sink: &mut dyn JumboRopeObjectSink<Error = SemanticPlaneRecordError>,
    sink: &mut Sink,
) -> Result<CanonicalPlaneRowEncodingMetrics, CanonicalPlaneRowStreamError<Sink::Error>>
where
    Reader: SemanticReader + ?Sized,
    Encoder: CanonicalPlaneRowEncoder + ?Sized,
    Sink: CanonicalSemanticPlaneRowSink + ?Sized,
{
    let mut adapter = PublicCanonicalPlaneRowSink { sink };
    encode_and_visit_canonical_plane_rows(
        reader,
        encoder,
        Some(maximum_rows),
        Some(maximum_plan_rows),
        Some(maximum_references),
        maximum_inline_row_bytes,
        Some(maximum_row_payload_bytes),
        jumbo_limits,
        Some(jumbo_sink),
        &mut adapter,
    )
}

#[derive(Clone, Copy)]
struct CanonicalPlaneRowScratchObservation {
    row_index_capacity_bytes: u64,
    row_scratch_capacity_bytes: usize,
    jumbo_rope_scratch_bytes: u64,
}

trait CanonicalPlaneEncodedRowVisitor {
    type Error: From<SemanticPlaneRecordError>;

    /// Runs after stable-key ordering is fixed but before this row is encoded.
    /// Segment sinks use this hook to flush a preceding range before a jumbo
    /// encoder can write objects for the next row.
    fn before_row(
        &mut self,
        key: [u8; 32],
        row_index_capacity_bytes: u64,
    ) -> Result<(), Self::Error>;

    /// Receives a grammar-admitted row borrowed from reusable row scratch.
    fn write_row(
        &mut self,
        family: SemanticIrPlane,
        row: CanonicalSemanticPlaneRecordView<'_>,
        scratch: CanonicalPlaneRowScratchObservation,
    ) -> Result<(), Self::Error>;
}

struct PublicCanonicalPlaneRowSink<'sink, Sink: ?Sized> {
    sink: &'sink mut Sink,
}

impl<Sink> CanonicalPlaneEncodedRowVisitor for PublicCanonicalPlaneRowSink<'_, Sink>
where
    Sink: CanonicalSemanticPlaneRowSink + ?Sized,
{
    type Error = CanonicalPlaneRowStreamError<Sink::Error>;

    fn before_row(
        &mut self,
        _key: [u8; 32],
        _row_index_capacity_bytes: u64,
    ) -> Result<(), Self::Error> {
        Ok(())
    }

    fn write_row(
        &mut self,
        family: SemanticIrPlane,
        row: CanonicalSemanticPlaneRecordView<'_>,
        _scratch: CanonicalPlaneRowScratchObservation,
    ) -> Result<(), Self::Error> {
        self.sink
            .write_row(family, row)
            .map_err(CanonicalPlaneRowStreamError::Sink)
    }
}

fn encode_and_visit_canonical_plane_rows<Reader, Encoder, Visitor>(
    reader: &Reader,
    encoder: &Encoder,
    maximum_rows: Option<u64>,
    maximum_plan_rows: Option<u64>,
    maximum_references: Option<u64>,
    maximum_inline_row_bytes: usize,
    maximum_row_payload_bytes: Option<usize>,
    jumbo_limits: crate::ir::JumboRopeLimits,
    mut jumbo_sink: Option<&mut dyn JumboRopeObjectSink<Error = SemanticPlaneRecordError>>,
    visitor: &mut Visitor,
) -> Result<CanonicalPlaneRowEncodingMetrics, Visitor::Error>
where
    Reader: SemanticReader + ?Sized,
    Encoder: CanonicalPlaneRowEncoder + ?Sized,
    Visitor: CanonicalPlaneEncodedRowVisitor + ?Sized,
{
    if !matches!(encoder.kind(), SemanticPlaneKind::Ir(_)) {
        return Err(SemanticPlaneRecordError::IrKindRequired.into());
    }
    let SemanticPlaneKind::Ir(family) = encoder.kind() else {
        return Err(SemanticPlaneRecordError::IrKindRequired.into());
    };
    if maximum_inline_row_bytes == 0
        || maximum_inline_row_bytes > crate::ir::MAX_SEMANTIC_SEGMENT_BYTES
    {
        return Err(SemanticPlaneRecordError::InvalidInlineRowCeiling {
            observed: maximum_inline_row_bytes,
            maximum: crate::ir::MAX_SEMANTIC_SEGMENT_BYTES,
        }
        .into());
    }
    if let Some(maximum_row_payload_bytes) = maximum_row_payload_bytes
        && (maximum_row_payload_bytes == 0
            || maximum_row_payload_bytes > crate::ir::MAX_SEMANTIC_SEGMENT_BYTES)
    {
        return Err(SemanticPlaneRecordError::InvalidRowPayloadCeiling {
            observed: maximum_row_payload_bytes,
            maximum: crate::ir::MAX_SEMANTIC_SEGMENT_BYTES,
        }
        .into());
    }

    let plan = encoder.build_plan_with_limits(
        reader,
        maximum_plan_rows.unwrap_or(u64::MAX),
        maximum_references.unwrap_or(u64::MAX),
    )?;
    let mut keys = match maximum_rows {
        Some(maximum_rows) => {
            CanonicalSemanticPlaneKeySink::with_row_limit(encoder.kind(), maximum_rows)
        }
        None => CanonicalSemanticPlaneKeySink::new(encoder.kind()),
    };
    encoder.collect_keys(reader, &plan, &mut keys)?;
    keys.rows
        .sort_unstable_by(|left, right| left.key.cmp(&right.key));
    if keys.rows.windows(2).any(|pair| pair[0].key == pair[1].key) {
        return Err(SemanticPlaneRecordError::StableKeyCollision.into());
    }
    let key_capacity = keys
        .rows
        .capacity()
        .checked_mul(size_of::<CanonicalSemanticPlaneRowKey<Encoder::Handle>>())
        .and_then(|bytes| u64::try_from(bytes).ok())
        .ok_or(SemanticPlaneRecordError::MetricsOverflow)?;
    let key_sort_rows =
        u64::try_from(keys.rows.len()).map_err(|_| SemanticPlaneRecordError::MetricsOverflow)?;

    let mut row_scratch = Vec::new();
    let mut row_count = 0_u64;
    let mut peak_row_scratch = 0_usize;
    let mut peak_jumbo_rope_scratch = 0_u64;
    let mut peak_tracked_row_scratch_upper_bound = 0_u64;
    for row in &keys.rows {
        visitor.before_row(row.key, key_capacity)?;
        row_scratch.clear();
        let mut row_jumbo_scratch = 0_u64;
        let row_sink = jumbo_sink.as_mut().map(|sink| {
            &mut **sink as &mut dyn JumboRopeObjectSink<Error = SemanticPlaneRecordError>
        });
        let tag = encoder.encode_row_with_jumbo_measured_for_segment_limit(
            reader,
            &plan,
            row.handle,
            row_sink,
            maximum_inline_row_bytes,
            jumbo_limits,
            &mut row_jumbo_scratch,
            &mut row_scratch,
        )?;
        u32::try_from(row_scratch.len()).map_err(|_| SemanticPlaneRecordError::RowTooLarge)?;
        validate_record_with_row_limit(
            encoder.kind(),
            row.key,
            tag,
            &row_scratch,
            maximum_inline_row_bytes,
        )?;
        if let Some(maximum_row_payload_bytes) = maximum_row_payload_bytes
            && row_scratch.len() > maximum_row_payload_bytes
        {
            return Err(SemanticPlaneRecordError::RowPayloadExceedsCeiling {
                observed: row_scratch.len(),
                maximum: maximum_row_payload_bytes,
            }
            .into());
        }
        peak_tracked_row_scratch_upper_bound =
            peak_tracked_row_scratch_upper_bound.max(tracked_family_scratch_bytes(
                key_capacity,
                row_scratch.capacity(),
                0,
                row_jumbo_scratch,
            )?);
        let scratch = CanonicalPlaneRowScratchObservation {
            row_index_capacity_bytes: key_capacity,
            row_scratch_capacity_bytes: row_scratch.capacity(),
            jumbo_rope_scratch_bytes: row_jumbo_scratch,
        };
        visitor.write_row(
            family,
            CanonicalSemanticPlaneRecordView {
                key: row.key,
                tag,
                payload: &row_scratch,
            },
            scratch,
        )?;
        peak_row_scratch = peak_row_scratch.max(row_scratch.capacity());
        peak_jumbo_rope_scratch = peak_jumbo_rope_scratch.max(row_jumbo_scratch);
        row_count = row_count
            .checked_add(1)
            .ok_or(SemanticPlaneRecordError::MetricsOverflow)?;
    }

    Ok(CanonicalPlaneRowEncodingMetrics {
        row_count,
        row_encode_calls: row_count,
        plan_build_calls: 1,
        key_collection_calls: 1,
        key_sort_rows,
        peak_row_scratch_capacity_bytes: u64::try_from(peak_row_scratch)
            .map_err(|_| SemanticPlaneRecordError::MetricsOverflow)?,
        peak_jumbo_rope_scratch_bytes: peak_jumbo_rope_scratch,
        peak_tracked_row_scratch_upper_bound_bytes: peak_tracked_row_scratch_upper_bound,
        row_index_capacity_bytes: key_capacity,
    })
}

fn stream_canonical_plane_family_inner<Reader, Encoder, Sink>(
    reader: &Reader,
    encoder: &Encoder,
    input: SemanticInputWitness,
    maximum_bytes: usize,
    boundary_policy: Option<CanonicalPlaneSegmentBoundaryPolicy>,
    maximum_rows: Option<u64>,
    maximum_plan_rows: Option<u64>,
    maximum_references: Option<u64>,
    jumbo_limits: crate::ir::JumboRopeLimits,
    jumbo_sink: Option<&mut dyn JumboRopeObjectSink<Error = SemanticPlaneRecordError>>,
    sink: &mut Sink,
) -> Result<CanonicalPlaneEncodingMetrics, CanonicalPlaneStreamError<Sink::Error>>
where
    Reader: SemanticReader + ?Sized,
    Encoder: CanonicalPlaneRowEncoder + ?Sized,
    Sink: CanonicalSemanticPlaneSegmentSink + ?Sized,
{
    let kind = encoder.kind();
    let SemanticPlaneKind::Ir(family) = kind else {
        return Err(SemanticPlaneRecordError::IrKindRequired.into());
    };
    if maximum_bytes == 0 || maximum_bytes > crate::ir::MAX_SEMANTIC_SEGMENT_BYTES {
        return Err(SemanticPlaneRecordError::InvalidByteCeiling {
            observed: maximum_bytes,
            maximum: crate::ir::MAX_SEMANTIC_SEGMENT_BYTES,
        }
        .into());
    }

    let maximum_inline_row_bytes = boundary_policy
        .map(CanonicalPlaneSegmentBoundaryPolicy::maximum_bytes)
        .unwrap_or(crate::ir::MAX_SEMANTIC_SEGMENT_BYTES);
    let mut assembler = CanonicalPlaneSegmentAssembler {
        kind,
        family,
        input,
        maximum_bytes,
        boundary_policy,
        sink,
        segment_bytes: Vec::new(),
        current_prefix: None,
        first_key: [0; 32],
        last_key: [0; 32],
        segment_rows: 0,
        segment_count: 0,
        output_bytes: 0,
        peak_segment_scratch: 0,
        peak_tracked_scratch_upper_bound: 0,
        anchor_hash_rows: 0,
        anchor_key_hash_bytes: 0,
    };
    let row_metrics = encode_and_visit_canonical_plane_rows(
        reader,
        encoder,
        maximum_rows,
        maximum_plan_rows,
        maximum_references,
        maximum_inline_row_bytes,
        None,
        jumbo_limits,
        jumbo_sink,
        &mut assembler,
    )?;
    assembler.finish(row_metrics)
}

struct CanonicalPlaneSegmentAssembler<'sink, Sink: ?Sized> {
    kind: SemanticPlaneKind,
    family: SemanticIrPlane,
    input: SemanticInputWitness,
    maximum_bytes: usize,
    boundary_policy: Option<CanonicalPlaneSegmentBoundaryPolicy>,
    sink: &'sink mut Sink,
    segment_bytes: Vec<u8>,
    current_prefix: Option<u8>,
    first_key: [u8; 32],
    last_key: [u8; 32],
    segment_rows: u32,
    segment_count: u64,
    output_bytes: u64,
    peak_segment_scratch: usize,
    peak_tracked_scratch_upper_bound: u64,
    anchor_hash_rows: u64,
    anchor_key_hash_bytes: u64,
}

impl<Sink> CanonicalPlaneSegmentAssembler<'_, Sink>
where
    Sink: CanonicalSemanticPlaneSegmentSink + ?Sized,
{
    fn emit_current(&mut self) -> Result<(), CanonicalPlaneStreamError<Sink::Error>> {
        if self.segment_rows == 0 {
            return Ok(());
        }
        emit_stream_segment(
            self.kind,
            self.input,
            self.first_key,
            self.last_key,
            self.segment_rows,
            &mut self.segment_bytes,
            self.sink,
        )?;
        account_segment(
            &self.segment_bytes,
            &mut self.segment_count,
            &mut self.output_bytes,
        )?;
        self.segment_bytes.clear();
        self.segment_rows = 0;
        Ok(())
    }

    fn finish(
        mut self,
        row_metrics: CanonicalPlaneRowEncodingMetrics,
    ) -> Result<CanonicalPlaneEncodingMetrics, CanonicalPlaneStreamError<Sink::Error>> {
        self.emit_current()?;
        Ok(CanonicalPlaneEncodingMetrics {
            row_count: row_metrics.row_count,
            segment_count: self.segment_count,
            output_bytes: self.output_bytes,
            row_encode_calls: row_metrics.row_encode_calls,
            plan_build_calls: row_metrics.plan_build_calls,
            key_collection_calls: row_metrics.key_collection_calls,
            key_sort_rows: row_metrics.key_sort_rows,
            anchor_hash_rows: self.anchor_hash_rows,
            peak_row_scratch_capacity_bytes: row_metrics.peak_row_scratch_capacity_bytes,
            peak_segment_scratch_capacity_bytes: u64::try_from(self.peak_segment_scratch)
                .map_err(|_| SemanticPlaneRecordError::MetricsOverflow)?,
            peak_jumbo_rope_scratch_bytes: row_metrics.peak_jumbo_rope_scratch_bytes,
            peak_tracked_scratch_upper_bound_bytes: self
                .peak_tracked_scratch_upper_bound
                .max(row_metrics.peak_tracked_row_scratch_upper_bound_bytes),
            row_index_capacity_bytes: row_metrics.row_index_capacity_bytes,
            anchor_key_hash_bytes: self.anchor_key_hash_bytes,
        })
    }
}

impl<Sink> CanonicalPlaneEncodedRowVisitor for CanonicalPlaneSegmentAssembler<'_, Sink>
where
    Sink: CanonicalSemanticPlaneSegmentSink + ?Sized,
{
    type Error = CanonicalPlaneStreamError<Sink::Error>;

    fn before_row(
        &mut self,
        key: [u8; 32],
        _row_index_capacity_bytes: u64,
    ) -> Result<(), Self::Error> {
        let prefix = prefix_value(&key, INITIAL_PREFIX_BITS);
        let anchor_cut = if let Some(policy) = self.boundary_policy {
            if self.segment_rows > 0 && policy.hashes_candidate(self.segment_bytes.len()) {
                self.anchor_hash_rows = self
                    .anchor_hash_rows
                    .checked_add(1)
                    .ok_or(SemanticPlaneRecordError::MetricsOverflow)?;
                self.anchor_key_hash_bytes = self
                    .anchor_key_hash_bytes
                    .checked_add(32)
                    .ok_or(SemanticPlaneRecordError::MetricsOverflow)?;
            }
            self.segment_rows > 0 && policy.cuts_before(self.family, self.segment_bytes.len(), &key)
        } else {
            false
        };
        let legacy_prefix_cut = self.boundary_policy.is_none()
            && self.segment_rows > 0
            && self.current_prefix != Some(prefix);
        if legacy_prefix_cut || anchor_cut {
            self.emit_current()?;
        }
        self.current_prefix = Some(prefix);
        Ok(())
    }

    fn write_row(
        &mut self,
        _family: SemanticIrPlane,
        row: CanonicalSemanticPlaneRecordView<'_>,
        scratch: CanonicalPlaneRowScratchObservation,
    ) -> Result<(), Self::Error> {
        self.peak_tracked_scratch_upper_bound =
            self.peak_tracked_scratch_upper_bound
                .max(tracked_family_scratch_bytes(
                    scratch.row_index_capacity_bytes,
                    scratch.row_scratch_capacity_bytes,
                    self.segment_bytes.capacity(),
                    scratch.jumbo_rope_scratch_bytes,
                )?);

        let row_length = u32::try_from(row.payload().len())
            .map_err(|_| SemanticPlaneRecordError::RowTooLarge)?;
        let framed_row_length = RECORD_HEADER_BYTES
            .checked_add(row.payload().len())
            .ok_or(SemanticPlaneRecordError::RowTooLarge)?;
        let projected = if self.segment_rows == 0 {
            HEADER_BYTES.checked_add(framed_row_length)
        } else {
            self.segment_bytes.len().checked_add(framed_row_length)
        }
        .ok_or(SemanticPlaneRecordError::RowTooLarge)?;
        if projected > self.maximum_bytes {
            if self.segment_rows == 0 {
                return Err(SemanticPlaneRecordError::OversizedRow {
                    observed: HEADER_BYTES.saturating_add(framed_row_length),
                    maximum: self.maximum_bytes,
                }
                .into());
            }
            self.emit_current()?;
            let projected = HEADER_BYTES
                .checked_add(framed_row_length)
                .ok_or(SemanticPlaneRecordError::RowTooLarge)?;
            if projected > self.maximum_bytes {
                return Err(SemanticPlaneRecordError::OversizedRow {
                    observed: projected,
                    maximum: self.maximum_bytes,
                }
                .into());
            }
        }
        if self.segment_rows == 0 {
            self.first_key = row.key();
            begin_segment(self.kind, &mut self.segment_bytes)?;
        }
        self.segment_bytes
            .try_reserve_exact(framed_row_length)
            .map_err(SemanticPlaneRecordError::Allocation)?;
        self.segment_bytes.extend_from_slice(&row.key());
        self.segment_bytes.push(row.tag());
        self.segment_bytes
            .extend_from_slice(&row_length.to_be_bytes());
        self.segment_bytes.extend_from_slice(row.payload());
        self.peak_tracked_scratch_upper_bound =
            self.peak_tracked_scratch_upper_bound
                .max(tracked_family_scratch_bytes(
                    scratch.row_index_capacity_bytes,
                    scratch.row_scratch_capacity_bytes,
                    self.segment_bytes.capacity(),
                    0,
                )?);
        self.last_key = row.key();
        self.segment_rows =
            self.segment_rows
                .checked_add(1)
                .ok_or(SemanticPlaneRecordError::RowCount {
                    expected: u32::MAX,
                    observed: u32::MAX,
                })?;
        self.peak_segment_scratch = self.peak_segment_scratch.max(self.segment_bytes.capacity());
        Ok(())
    }
}

fn tracked_family_scratch_bytes(
    key_index: u64,
    row_capacity: usize,
    segment_capacity: usize,
    jumbo_writer: u64,
) -> Result<u64, SemanticPlaneRecordError> {
    key_index
        .checked_add(
            u64::try_from(row_capacity).map_err(|_| SemanticPlaneRecordError::MetricsOverflow)?,
        )
        .and_then(|bytes| {
            u64::try_from(segment_capacity)
                .ok()
                .and_then(|segment| bytes.checked_add(segment))
        })
        .and_then(|bytes| bytes.checked_add(jumbo_writer))
        .ok_or(SemanticPlaneRecordError::MetricsOverflow)
}

#[derive(Default)]
struct OwnedPlaneSegments {
    segments: Vec<CanonicalSemanticPlaneSegmentPayload>,
}

impl CanonicalSemanticPlaneSegmentSink for OwnedPlaneSegments {
    type Error = SemanticPlaneRecordError;

    fn write_segment(
        &mut self,
        segment: CanonicalSemanticPlaneSegmentRef<'_>,
    ) -> Result<(), Self::Error> {
        let mut bytes = Vec::new();
        bytes
            .try_reserve_exact(segment.bytes.len())
            .map_err(SemanticPlaneRecordError::Allocation)?;
        bytes.extend_from_slice(segment.bytes);
        self.segments
            .try_reserve(1)
            .map_err(SemanticPlaneRecordError::Allocation)?;
        self.segments.push(CanonicalSemanticPlaneSegmentPayload {
            kind: segment.kind,
            first_key: segment.first_key,
            last_key: segment.last_key,
            row_count: segment.row_count,
            input: segment.input,
            bytes: bytes.into_boxed_slice(),
        });
        Ok(())
    }
}

fn begin_segment(
    kind: SemanticPlaneKind,
    bytes: &mut Vec<u8>,
) -> Result<(), SemanticPlaneRecordError> {
    bytes
        .try_reserve(HEADER_BYTES)
        .map_err(SemanticPlaneRecordError::Allocation)?;
    bytes.extend_from_slice(&MAGIC);
    bytes.extend_from_slice(&VERSION.to_be_bytes());
    bytes.push(ir_plane_code(kind)?);
    bytes.extend_from_slice(&0_u32.to_be_bytes());
    Ok(())
}

fn emit_stream_segment<Sink: CanonicalSemanticPlaneSegmentSink + ?Sized>(
    kind: SemanticPlaneKind,
    input: SemanticInputWitness,
    first_key: [u8; 32],
    last_key: [u8; 32],
    row_count: u32,
    bytes: &mut [u8],
    sink: &mut Sink,
) -> Result<(), CanonicalPlaneStreamError<Sink::Error>> {
    if bytes.len() < HEADER_BYTES {
        return Err(SemanticPlaneRecordError::Header.into());
    }
    bytes[7..11].copy_from_slice(&row_count.to_be_bytes());
    sink.write_segment(CanonicalSemanticPlaneSegmentRef {
        kind,
        first_key,
        last_key,
        row_count,
        input,
        bytes,
    })
    .map_err(CanonicalPlaneStreamError::Sink)
}

fn account_segment(
    bytes: &[u8],
    segment_count: &mut u64,
    output_bytes: &mut u64,
) -> Result<(), SemanticPlaneRecordError> {
    *segment_count = segment_count
        .checked_add(1)
        .ok_or(SemanticPlaneRecordError::MetricsOverflow)?;
    *output_bytes = output_bytes
        .checked_add(
            u64::try_from(bytes.len()).map_err(|_| SemanticPlaneRecordError::MetricsOverflow)?,
        )
        .ok_or(SemanticPlaneRecordError::MetricsOverflow)?;
    Ok(())
}

/// Opens and validates a canonical row-plane payload against its segment
/// descriptor and exact content-derived ID. This compatibility entry point
/// applies the global jumbo spill threshold; use the row-limit variant when
/// decoding a family governed by a smaller manifest policy.
pub fn decode_semantic_plane_segment<'bytes>(
    kind: SemanticPlaneKind,
    descriptor: &SemanticPlaneSegment,
    bytes: &'bytes [u8],
) -> Result<CanonicalSemanticPlaneSegmentView<'bytes>, SemanticPlaneRecordError> {
    decode_semantic_plane_segment_with_row_limit(
        kind,
        descriptor,
        bytes,
        crate::ir::MAX_SEMANTIC_SEGMENT_BYTES,
    )
}

/// Decodes a segment using the family policy's canonical jumbo spill
/// threshold. The policy must come from trusted manifest metadata.
pub fn decode_semantic_plane_segment_with_row_limit<'bytes>(
    kind: SemanticPlaneKind,
    descriptor: &SemanticPlaneSegment,
    bytes: &'bytes [u8],
    maximum_inline_row_bytes: usize,
) -> Result<CanonicalSemanticPlaneSegmentView<'bytes>, SemanticPlaneRecordError> {
    if !matches!(kind, SemanticPlaneKind::Ir(_)) {
        return Err(SemanticPlaneRecordError::IrKindRequired);
    }
    descriptor.admit(kind, bytes)?;
    decode_semantic_plane_segment_structure(kind, descriptor, bytes, maximum_inline_row_bytes)
}

/// Reopens every segment in a complete family and verifies its committed
/// stable-key ramp cuts, target behavior, and hard encoded-size ceiling.
///
/// Per-segment decoding alone cannot prove that a family used canonical
/// boundaries. This verifier walks all rows in order, recomputes every
/// expected anchor/target/forced cut, and compares those cuts with the supplied
/// segment starts. The policy must come from trusted, durably committed
/// metadata; callers must not infer it from the segment bytes. Streaming V2
/// admission should use [`CanonicalSemanticPlaneBoundaryFamilyVerifier`]
/// directly so it need not retain the family payload inventory. Each payload
/// is decoded using the supplied policy's maximum as its canonical jumbo
/// spill threshold.
pub fn verify_canonical_semantic_plane_segment_boundaries(
    kind: SemanticPlaneKind,
    descriptors: &[SemanticPlaneSegment],
    payloads: &[&[u8]],
    policy: CanonicalPlaneSegmentBoundaryPolicy,
    expected_rows: u64,
) -> Result<(), SemanticPlaneRecordError> {
    let SemanticPlaneKind::Ir(family) = kind else {
        return Err(SemanticPlaneRecordError::IrKindRequired);
    };
    if descriptors.len() != payloads.len() {
        return Err(SemanticPlaneRecordError::SegmentCount {
            expected: descriptors.len(),
            observed: payloads.len(),
        });
    }
    let mut verifier =
        super::segment_boundary_policy::CanonicalSemanticPlaneBoundaryFamilyVerifier::begin_family(
            family, policy,
        );
    for (descriptor, payload) in descriptors.iter().zip(payloads) {
        let view = decode_semantic_plane_segment_with_row_limit(
            kind,
            descriptor,
            payload,
            policy.maximum_bytes(),
        )?;
        verifier.push_segment(view)?;
    }
    verifier.finish(
        expected_rows,
        u64::try_from(descriptors.len()).map_err(|_| SemanticPlaneRecordError::MetricsOverflow)?,
    )
}

/// Checks the row-plane grammar after the caller has established exact
/// content identity. Producer validation uses this helper because its
/// descriptor was just built from the same bytes; fetched/untrusted payloads
/// must enter through `decode_semantic_plane_segment` and re-admit the claim.
fn decode_semantic_plane_segment_structure<'bytes>(
    kind: SemanticPlaneKind,
    descriptor: &SemanticPlaneSegment,
    bytes: &'bytes [u8],
    maximum_inline_row_bytes: usize,
) -> Result<CanonicalSemanticPlaneSegmentView<'bytes>, SemanticPlaneRecordError> {
    decode_semantic_plane_segment_structure_with_record_visitor(
        kind,
        descriptor,
        bytes,
        maximum_inline_row_bytes,
        |_| Ok(()),
    )
}

/// Strictly parses one segment and lends each grammar-checked record to a
/// short-lived visitor before advancing the byte cursor. Aggregate admission
/// uses this to consume boundary facts during the structural pass instead of
/// reopening the validated view for another row walk.
pub(crate) fn decode_semantic_plane_segment_structure_with_record_visitor<'bytes, Visitor>(
    kind: SemanticPlaneKind,
    descriptor: &SemanticPlaneSegment,
    bytes: &'bytes [u8],
    maximum_inline_row_bytes: usize,
    mut visitor: Visitor,
) -> Result<CanonicalSemanticPlaneSegmentView<'bytes>, SemanticPlaneRecordError>
where
    Visitor:
        FnMut(CanonicalSemanticPlaneRecordView<'bytes>) -> Result<(), SemanticPlaneRecordError>,
{
    if bytes.len() < HEADER_BYTES || bytes[..4] != MAGIC {
        return Err(SemanticPlaneRecordError::Header);
    }
    let version = u16::from_be_bytes([bytes[4], bytes[5]]);
    if version != VERSION {
        return Err(SemanticPlaneRecordError::Version(version));
    }
    if bytes[6] != ir_plane_code(kind)? {
        return Err(SemanticPlaneRecordError::PlaneKind);
    }
    let count = u32::from_be_bytes(
        bytes[7..11]
            .try_into()
            .map_err(|_| SemanticPlaneRecordError::Header)?,
    );
    if count != descriptor.row_count() {
        return Err(SemanticPlaneRecordError::RowCount {
            expected: descriptor.row_count(),
            observed: count,
        });
    }
    let rows = &bytes[HEADER_BYTES..];
    let mut cursor = CanonicalSemanticPlaneRecordCursor {
        bytes: rows,
        next: 0,
        count,
    };
    let mut first = None;
    let mut previous = None;
    while let Some(row) = cursor.next() {
        if previous.is_some_and(|key| key >= row.key) {
            return Err(SemanticPlaneRecordError::RecordOrder);
        }
        match validate_record_with_row_limit(
            kind,
            row.key,
            row.tag,
            row.payload,
            maximum_inline_row_bytes,
        ) {
            Ok(()) => {}
            Err(SemanticPlaneRecordError::StableKeyMismatch) => {
                return Err(SemanticPlaneRecordError::StableKeyMismatchAt { key: row.key });
            }
            Err(error) => return Err(error),
        }
        visitor(row)?;
        first.get_or_insert(row.key);
        previous = Some(row.key);
    }
    if cursor.next != count {
        return Err(SemanticPlaneRecordError::Truncated);
    }
    if !cursor.bytes.is_empty() {
        return Err(SemanticPlaneRecordError::TrailingBytes);
    }
    let (Some(first), Some(last)) = (first, previous) else {
        return Err(SemanticPlaneRecordError::RowCount {
            expected: descriptor.row_count(),
            observed: 0,
        });
    };
    if first != *descriptor.first_key() || last != *descriptor.last_key() {
        return Err(SemanticPlaneRecordError::KeyRange);
    }
    Ok(CanonicalSemanticPlaneSegmentView {
        kind,
        first_key: first,
        last_key: last,
        row_count: count,
        rows,
    })
}

/// Opaque proof that every jumbo descriptor in one complete semantic-plane
/// family resolves to an exact, content-verified leaf and interior closure.
/// The commitment binds the ordered SPIR segment IDs and row-to-descriptor
/// associations, so a publication verifier can require this token alongside
/// its regular seven-family inventory.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct VerifiedJumboPlaneClosure {
    family: SemanticIrPlane,
    segment_count: u64,
    jumbo_value_count: u64,
    commitment: [u8; 32],
}

impl VerifiedJumboPlaneClosure {
    /// The exact typed family whose row references were checked.
    #[must_use]
    pub const fn family(self) -> SemanticIrPlane {
        self.family
    }

    /// Number of ordered semantic segments covered by this proof.
    #[must_use]
    pub const fn segment_count(self) -> u64 {
        self.segment_count
    }

    /// Number of jumbo value descriptors whose object closures were checked.
    #[must_use]
    pub const fn jumbo_value_count(self) -> u64 {
        self.jumbo_value_count
    }

    /// Commitment to the exact family segments and their ordered descriptors.
    #[must_use]
    pub const fn commitment(self) -> [u8; 32] {
        self.commitment
    }
}

/// Reopens the complete family payloads, validates their typed rows, and
/// verifies every referenced jumbo object closure before minting a proof
/// token. The scan retains no whole jumbo field and reads stored content into
/// one fixed maximum-leaf buffer at a time. This legacy helper uses the global
/// `MAX_SEMANTIC_SEGMENT_BYTES` threshold when deciding whether a jumbo row is
/// canonical; policy-bound manifests should use
/// [`verify_jumbo_plane_family_closures_with_policy`].
pub fn verify_jumbo_plane_family_closures<S>(
    kind: SemanticPlaneKind,
    descriptors: &[SemanticPlaneSegment],
    payloads: &[&[u8]],
    source: &mut S,
) -> Result<VerifiedJumboPlaneClosure, SemanticPlaneRecordError>
where
    S: JumboRopeObjectSource + ?Sized,
    S::Error: core::fmt::Display,
{
    verify_jumbo_plane_family_closures_with_row_limit(
        kind,
        descriptors,
        payloads,
        crate::ir::MAX_SEMANTIC_SEGMENT_BYTES,
        source,
    )
}

/// Reopens the complete family payloads and verifies jumbo closures under the
/// exact family policy committed by a trusted manifest. The policy's maximum
/// bytes define the canonical spill threshold for Docs and SourceProvenance
/// rows, matching the policy-aware producer.
pub fn verify_jumbo_plane_family_closures_with_policy<S>(
    kind: SemanticPlaneKind,
    descriptors: &[SemanticPlaneSegment],
    payloads: &[&[u8]],
    policy: CanonicalPlaneSegmentBoundaryPolicy,
    source: &mut S,
) -> Result<VerifiedJumboPlaneClosure, SemanticPlaneRecordError>
where
    S: JumboRopeObjectSource + ?Sized,
    S::Error: core::fmt::Display,
{
    verify_jumbo_plane_family_closures_with_row_limit(
        kind,
        descriptors,
        payloads,
        policy.maximum_bytes(),
        source,
    )
}

pub(super) fn jumbo_descriptors_for_record_with_row_limit(
    family: SemanticIrPlane,
    record: CanonicalSemanticPlaneRecordView<'_>,
    maximum_inline_row_bytes: usize,
) -> Result<[Option<crate::ir::CheckedJumboValueDescriptor>; 3], SemanticPlaneRecordError> {
    let mut descriptors = [None; 3];
    match family {
        SemanticIrPlane::Core => {
            if let Some(core) = declarations::core_jumbo_descriptors_for_record_with_row_limit(
                record,
                maximum_inline_row_bytes,
            )? {
                descriptors[0] = core.name;
                descriptors[1] = Some(core.members);
                descriptors[2] = Some(core.attributes);
            }
        }
        SemanticIrPlane::Documentation => {
            descriptors[0] = declarations::jumbo_descriptor_for_record_with_row_limit(
                record,
                maximum_inline_row_bytes,
            )?;
        }
        SemanticIrPlane::SourceProvenance => {
            descriptors[0] = source_provenance::jumbo_descriptor_for_record_with_row_limit(
                record,
                maximum_inline_row_bytes,
            )?;
        }
        _ => {}
    }
    Ok(descriptors)
}

fn verify_jumbo_plane_family_closures_with_row_limit<S>(
    kind: SemanticPlaneKind,
    descriptors: &[SemanticPlaneSegment],
    payloads: &[&[u8]],
    maximum_inline_row_bytes: usize,
    source: &mut S,
) -> Result<VerifiedJumboPlaneClosure, SemanticPlaneRecordError>
where
    S: JumboRopeObjectSource + ?Sized,
    S::Error: core::fmt::Display,
{
    let SemanticPlaneKind::Ir(family) = kind else {
        return Err(SemanticPlaneRecordError::IrKindRequired);
    };
    if descriptors.len() != payloads.len() {
        return Err(SemanticPlaneRecordError::SegmentCount {
            expected: descriptors.len(),
            observed: payloads.len(),
        });
    }
    let mut hasher = blake3::Hasher::new_derive_key("backend.semantic.ir.jumbo-plane-closure.v1");
    hasher.update(&[ir_plane_code(kind)?]);
    hasher.update(
        &u64::try_from(descriptors.len())
            .map_err(|_| SemanticPlaneRecordError::MetricsOverflow)?
            .to_be_bytes(),
    );
    let mut jumbo_value_count = 0_u64;
    for (index, (descriptor, payload)) in descriptors.iter().zip(payloads).enumerate() {
        if descriptor.first_key() > descriptor.last_key()
            || index > 0 && descriptors[index - 1].last_key() >= descriptor.first_key()
        {
            return Err(SemanticPlaneRecordError::RecordOrder);
        }
        let segment_id = descriptor.admit(kind, payload)?;
        let view = decode_semantic_plane_segment_with_row_limit(
            kind,
            descriptor,
            payload,
            maximum_inline_row_bytes,
        )?;
        hasher.update(segment_id.as_bytes());
        for record in view.records() {
            let descriptors = jumbo_descriptors_for_record_with_row_limit(
                family,
                record,
                maximum_inline_row_bytes,
            )?;
            for jumbo in descriptors.into_iter().flatten() {
                let verified = match (family, jumbo.encoding()) {
                    (SemanticIrPlane::Documentation, _) => {
                        let mut validator = declarations::DocsWireValidator::new();
                        let verified = jumbo
                            .admit_stored_closure_to(source, &mut validator)
                            .map_err(map_jumbo_source_error)?;
                        validator.finish()?;
                        verified
                    }
                    (
                        SemanticIrPlane::Core,
                        crate::ir::JumboValueEncoding::CoreMemberIdentityList,
                    ) => {
                        let mut validator =
                            declarations::CoreMembersWireValidator::new(1_000_000, None);
                        let verified = jumbo
                            .admit_stored_closure_to(source, &mut validator)
                            .map_err(map_jumbo_source_error)?;
                        validator.finish()?;
                        verified
                    }
                    (SemanticIrPlane::Core, crate::ir::JumboValueEncoding::CoreAttributeList) => {
                        let mut validator = declarations::CoreAttributesWireValidator::new();
                        let verified = jumbo
                            .admit_stored_closure_to(source, &mut validator)
                            .map_err(map_jumbo_source_error)?;
                        validator.finish()?;
                        verified
                    }
                    _ => jumbo
                        .admit_stored_closure(source)
                        .map_err(map_jumbo_source_error)?,
                };
                hasher.update(&record.key());
                hasher.update(verified.descriptor_id().as_bytes());
                jumbo_value_count = jumbo_value_count
                    .checked_add(1)
                    .ok_or(SemanticPlaneRecordError::MetricsOverflow)?;
            }
        }
    }
    Ok(VerifiedJumboPlaneClosure {
        family,
        segment_count: u64::try_from(descriptors.len())
            .map_err(|_| SemanticPlaneRecordError::MetricsOverflow)?,
        jumbo_value_count,
        commitment: *hasher.finalize().as_bytes(),
    })
}

fn map_jumbo_source_error<E: core::fmt::Display>(
    error: crate::ir::JumboOperationError<E>,
) -> SemanticPlaneRecordError {
    match error {
        crate::ir::JumboOperationError::Rope(error) => SemanticPlaneRecordError::JumboRope(error),
        crate::ir::JumboOperationError::Store(error) => {
            SemanticPlaneRecordError::JumboObjectStore(error.to_string())
        }
        crate::ir::JumboOperationError::Input(_) | crate::ir::JumboOperationError::Output(_) => {
            SemanticPlaneRecordError::JumboStream
        }
    }
}

/// Re-encodes one complete row family from a checked reader and compares it
/// with the exact descriptor/payload inventory. Missing ranges and
/// self-consistent manifest omissions fail the segment-count or byte check.
pub fn verify_semantic_plane_family_against_reader<Reader, Encoder>(
    reader: &Reader,
    encoder: &Encoder,
    input: SemanticInputWitness,
    descriptors: &[SemanticPlaneSegment],
    payloads: &[&[u8]],
    maximum_bytes: usize,
) -> Result<(), SemanticPlaneRecordError>
where
    Reader: SemanticReader + ?Sized,
    Encoder: CanonicalPlaneRowEncoder + ?Sized,
{
    if descriptors.len() != payloads.len() {
        return Err(SemanticPlaneRecordError::SegmentCount {
            expected: descriptors.len(),
            observed: payloads.len(),
        });
    }
    let mut sink = FamilyOracleSink {
        kind: encoder.kind(),
        descriptors,
        payloads,
        next: 0,
    };
    stream_canonical_plane_family(reader, encoder, input, maximum_bytes, &mut sink)
        .map_err(unwrap_oracle_sink_error)?;
    if sink.next != descriptors.len() {
        return Err(SemanticPlaneRecordError::SegmentCount {
            expected: sink.next,
            observed: descriptors.len(),
        });
    }
    Ok(())
}

struct FamilyOracleSink<'a> {
    kind: SemanticPlaneKind,
    descriptors: &'a [SemanticPlaneSegment],
    payloads: &'a [&'a [u8]],
    next: usize,
}

impl CanonicalSemanticPlaneSegmentSink for FamilyOracleSink<'_> {
    type Error = SemanticPlaneRecordError;

    fn write_segment(
        &mut self,
        expected: CanonicalSemanticPlaneSegmentRef<'_>,
    ) -> Result<(), Self::Error> {
        let (Some(descriptor), Some(payload)) = (
            self.descriptors.get(self.next),
            self.payloads.get(self.next),
        ) else {
            return Err(SemanticPlaneRecordError::SegmentCount {
                expected: self.next + 1,
                observed: self.descriptors.len(),
            });
        };
        let expected_id = expected
            .metadata()?
            .admitted_id()
            .ok_or(SemanticPlaneRecordError::MissingAdmittedId)?;
        if expected.kind() != self.kind
            || expected.first_key() != *descriptor.first_key()
            || expected.last_key() != *descriptor.last_key()
            || expected.row_count() != descriptor.row_count()
            || expected.bytes() != *payload
            || descriptor.admit(self.kind, payload)? != expected_id
        {
            return Err(SemanticPlaneRecordError::PlaneOracleMismatch);
        }
        self.next += 1;
        Ok(())
    }
}

fn unwrap_oracle_sink_error(
    error: CanonicalPlaneStreamError<SemanticPlaneRecordError>,
) -> SemanticPlaneRecordError {
    match error {
        CanonicalPlaneStreamError::Encoding(error) | CanonicalPlaneStreamError::Sink(error) => {
            error
        }
    }
}

/// Exact stable declaration key, ordered lexicographically by its canonical
/// `family || variant` identity bytes. The enclosing plane scopes the key.
#[must_use]
pub fn declaration_plane_key(_kind: SemanticPlaneKind, identity: DeclarationIdentity) -> [u8; 32] {
    let mut key = [0_u8; 32];
    key[..16].copy_from_slice(identity.family.as_bytes());
    key[16..].copy_from_slice(identity.variant.as_bytes());
    key
}

/// Closed typed serialization failure for canonical row-plane production.
#[derive(Debug, Error)]
pub enum SemanticPlaneRecordError {
    /// Segment metadata or payload admission failed.
    #[error(transparent)]
    Manifest(#[from] crate::ir::SemanticManifestError),
    /// Allocation could not be reserved.
    #[error("canonical plane row allocation failed")]
    Allocation(#[source] alloc::collections::TryReserveError),
    /// Only typed IR row families can use the `SPIR` framing.
    #[error("canonical row payloads require an IR plane")]
    IrKindRequired,
    /// The requested bounded segment size is outside the supported limit.
    #[error("canonical segment ceiling {observed} is invalid; maximum is {maximum}")]
    InvalidByteCeiling {
        /// Requested segment byte ceiling.
        observed: usize,
        /// Maximum supported segment byte ceiling.
        maximum: usize,
    },
    /// The row payload output ceiling is outside the supported wire bound.
    #[error("canonical row payload ceiling {observed} is invalid; maximum is {maximum}")]
    InvalidRowPayloadCeiling {
        /// Requested row-payload ceiling in bytes.
        observed: usize,
        /// Maximum supported row-payload ceiling in bytes.
        maximum: usize,
    },
    /// One encoded typed-row payload exceeded the row sink's admission bound.
    #[error("canonical row payload has {observed} bytes; sink ceiling is {maximum}")]
    RowPayloadExceedsCeiling {
        /// Encoded row payload length in bytes.
        observed: usize,
        /// Active row-sink admission ceiling in bytes.
        maximum: usize,
    },
    /// The canonical inline threshold for a row family is outside the wire bound.
    #[error("canonical inline-row ceiling {observed} is invalid; maximum is {maximum}")]
    InvalidInlineRowCeiling {
        /// Requested inline-row threshold in bytes.
        observed: usize,
        /// Maximum supported inline-row threshold in bytes.
        maximum: usize,
    },
    /// Stable-key policy byte range is invalid or exceeds the closed u32 wire form.
    #[error("canonical segment boundary range {minimum}..={target}..={maximum} is invalid")]
    InvalidSegmentBoundaryRange {
        /// Minimum segment size in bytes.
        minimum: usize,
        /// Target segment size in bytes.
        target: usize,
        /// Maximum segment size in bytes.
        maximum: usize,
    },
    /// A boundary-policy parameter could not be represented in its wire type.
    #[error("canonical segment boundary policy parameter is out of range")]
    BoundaryPolicyRange,
    /// One or more family segments do not follow the selected canonical cut policy.
    #[error("semantic plane segment boundaries are not canonical for the supplied policy")]
    NonCanonicalSegmentBoundary,
    /// Two logical rows produced the same stable key.
    #[error("canonical plane rows have a stable-key collision")]
    StableKeyCollision,
    /// One typed row cannot fit below the selected segment byte ceiling.
    #[error("one canonical row needs {observed} bytes; segment ceiling is {maximum}")]
    OversizedRow {
        /// Minimum single-segment size for this encoded row, including header and framing, in bytes.
        observed: usize,
        /// Maximum encoded row size permitted by the active segment policy, in bytes.
        maximum: usize,
    },
    /// A family row length cannot be represented in the canonical u32 cell.
    #[error("canonical row length exceeds the u32 wire limit")]
    RowTooLarge,
    /// Stable-key collection exceeded the active aggregate family-row budget.
    #[error("canonical family row count exceeds the aggregate limit {maximum}")]
    RowBudgetExceeded {
        /// Maximum number of canonical rows admitted across the family window.
        maximum: u64,
    },
    /// Typed graph edge/root facts exceeded the active reference-work budget.
    #[error("canonical type plan reference facts exceed the aggregate limit {maximum}")]
    ReferenceBudgetExceeded {
        /// Maximum number of typed graph references admitted in the plan.
        maximum: u64,
    },
    /// A typed row refers to a missing canonical-reader coordinate.
    #[error("canonical row refers to a missing reader fact")]
    ReaderReference,
    /// A typed row tag or closed field grammar is invalid.
    #[error("canonical typed row grammar is invalid")]
    RowGrammar,
    /// A typed row contains bytes after its exact grammar.
    #[error("canonical typed row has trailing bytes")]
    RowTrailingBytes,
    /// A jumbo descriptor, proof, or stored object failed verification.
    #[error(transparent)]
    JumboRope(#[from] crate::ir::JumboRopeError),
    /// An oversized canonical field needs an object sink to produce its row.
    #[error("jumbo semantic value encoding requires an object sink")]
    JumboObjectStoreRequired,
    /// A jumbo object store rejected a leaf or interior write.
    #[error("jumbo semantic value object store failed: {0}")]
    JumboObjectStore(String),
    /// A jumbo object store returned a typed corruption or authority error.
    #[error("jumbo semantic value object store rejected invalid state: {0}")]
    JumboObjectStoreIntegrity(String),
    /// A jumbo object store is temporarily unavailable while admitting an
    /// object. Callers may retry after the store or its durability layer
    /// recovers; this does not certify that any partial output is publishable.
    #[error("jumbo semantic value object store is temporarily unavailable: {0}")]
    JumboObjectStoreUnavailable(String),
    /// The configured object-store bound cannot admit this object or plan.
    #[error("jumbo semantic value object-store limit was exceeded: {0}")]
    JumboObjectStoreLimit(String),
    /// A jumbo stream failed while serializing a canonical field.
    #[error("jumbo semantic value stream failed")]
    JumboStream,
    /// A streaming jumbo documentation row exceeded its bounded link census.
    #[error("jumbo documentation references exceed the aggregate verifier budget")]
    JumboReferenceLimitExceeded,
    /// Stable row key does not commit the typed identity in its payload.
    #[error("canonical row key does not match its typed identity")]
    StableKeyMismatch,
    /// Stable row key mismatch found while parsing a segment. The key is
    /// retained so family-scoped verifier diagnostics can identify the row
    /// without exposing its potentially sensitive payload.
    #[error("canonical row key {key:?} does not match its typed identity")]
    StableKeyMismatchAt {
        /// Claimed 32-byte stable key whose payload identity did not match.
        key: [u8; 32],
    },
    /// This build has no strict decoder for the selected family yet.
    #[error("canonical decoder for this semantic plane family is unavailable")]
    UnsupportedFamily,
    /// A typed anonymous dependency graph contains an invalid back-edge.
    #[error("typed semantic dependency graph contains a cycle")]
    TypedDependencyCycle,
    /// A measured byte count overflowed its fixed-width metrics field.
    #[error("canonical plane encoding metrics overflow")]
    MetricsOverflow,
    /// A payload does not carry the exact canonical `SPIR` header.
    #[error("canonical row payload header is invalid")]
    Header,
    /// The payload uses an unknown record grammar version.
    #[error("canonical row payload version {0} is unsupported")]
    Version(u16),
    /// The payload plane tag differs from the selected plane.
    #[error("canonical row payload plane tag differs from its descriptor")]
    PlaneKind,
    /// A row count differs from the segment descriptor.
    #[error("canonical row payload has {observed} rows; descriptor claims {expected}")]
    RowCount {
        /// Row count claimed by the segment descriptor.
        expected: u32,
        /// Row count decoded from the segment payload.
        observed: u32,
    },
    /// Row keys are duplicate or not in strict canonical order.
    #[error("canonical row keys are not in strict ascending order")]
    RecordOrder,
    /// The first or last decoded row does not match the segment key range.
    #[error("canonical row key range differs from the segment descriptor")]
    KeyRange,
    /// Record framing ended before the declared row count.
    #[error("canonical row payload record is truncated")]
    Truncated,
    /// Canonical row payload has bytes after its declared rows.
    #[error("canonical row payload has trailing bytes")]
    TrailingBytes,
    /// The IR plane does not have a closed wire code.
    #[error("IR plane has no canonical row wire code")]
    UnknownPlane,
    /// Supplied payload cardinality differs from canonical family output.
    #[error("plane inventory has {observed} segments; expected {expected}")]
    SegmentCount {
        /// Number of canonical segments produced from the reader.
        expected: usize,
        /// Number of segments supplied by the caller.
        observed: usize,
    },
    /// Complete-family segment count differs from its durable policy claim.
    #[error("boundary verifier observed {observed} segments; manifest claims {expected}")]
    BoundaryFamilySegmentCount {
        /// Segment count committed by the boundary-policy claim.
        expected: u64,
        /// Segment count observed by the complete-family verifier.
        observed: u64,
    },
    /// Complete-family row count differs from its durable policy claim.
    #[error("boundary verifier observed {observed} rows; manifest claims {expected}")]
    BoundaryFamilyRowCount {
        /// Row count committed by the boundary-policy claim.
        expected: u64,
        /// Row count observed by the complete-family verifier.
        observed: u64,
    },
    /// Stable payload bytes differ from the complete reader projection.
    #[error("versioned plane payload differs from its canonical reader projection")]
    PlaneOracleMismatch,
    /// A generated in-memory segment did not carry an admitted content ID.
    #[error("canonical row payload did not produce an admitted segment ID")]
    MissingAdmittedId,
}

fn prefix_value(key: &[u8; 32], bits: u16) -> u8 {
    key[0] >> (8 - bits as u8)
}

fn validate_record_with_row_limit(
    kind: SemanticPlaneKind,
    key: [u8; 32],
    tag: u8,
    payload: &[u8],
    maximum_inline_row_bytes: usize,
) -> Result<(), SemanticPlaneRecordError> {
    match kind {
        SemanticPlaneKind::Ir(SemanticIrPlane::Core | SemanticIrPlane::Documentation) => {
            declarations::validate_record_with_row_limit(
                kind,
                key,
                tag,
                payload,
                maximum_inline_row_bytes,
            )
        }
        SemanticPlaneKind::Ir(SemanticIrPlane::Relations) => {
            relations::validate_record(key, tag, payload)
        }
        SemanticPlaneKind::Ir(SemanticIrPlane::Occurrences) => {
            occurrences::validate_record(key, tag, payload)
        }
        SemanticPlaneKind::Ir(SemanticIrPlane::SourceProvenance) => {
            source_provenance::validate_record_with_row_limit(
                kind,
                key,
                tag,
                payload,
                maximum_inline_row_bytes,
            )
        }
        SemanticPlaneKind::Ir(SemanticIrPlane::Types) => {
            types::validate_record(kind, key, tag, payload)
        }
        SemanticPlaneKind::Ir(SemanticIrPlane::LanguageExtensions(_)) => {
            extensions::validate_record(kind, key, tag, payload)
        }
        _ => Err(SemanticPlaneRecordError::UnsupportedFamily),
    }
}

fn ir_plane_code(kind: SemanticPlaneKind) -> Result<u8, SemanticPlaneRecordError> {
    let SemanticPlaneKind::Ir(plane) = kind else {
        return Err(SemanticPlaneRecordError::IrKindRequired);
    };
    Ok(match plane {
        SemanticIrPlane::Core => 1,
        SemanticIrPlane::Types => 2,
        SemanticIrPlane::Relations => 3,
        SemanticIrPlane::Occurrences => 4,
        SemanticIrPlane::Documentation => 5,
        SemanticIrPlane::SourceProvenance => 6,
        SemanticIrPlane::LanguageExtensions(_) => 7,
    })
}

#[cfg(test)]
mod tests {
    use alloc::{collections::BTreeMap, format, string::String, vec::Vec};
    use std::io::Write as _;

    use crate::ir::jumbo_rope::{
        JUMBO_ROPE_MAX_LEAF_BYTES, JumboRopeLeafRef, JumboRopeNode, JumboRopeObjectId,
        JumboRopeObjectSink, JumboRopeObjectSource, ROPE_NODE_WIRE_BYTES,
    };
    use crate::ir::{
        BorrowedTree, CheckedJumboValueDescriptor, Confidence, CorePayloadHash,
        DeclarationFamilyId, DocInput, EntityAuthorityFacts, EntityVersion, FactAvailability, Ir,
        IrBuilder, ItemKind, JumboValueEncoding, LinkKind, OccurrenceAuthorityFacts,
        ParentageAuthority, SemanticInputWitness, SemanticIrPlane, SemanticPlaneKind,
        SemanticPlaneSegment, SemanticReader, SemanticSegmentId, SourceSpan, TreeEntityId,
        TreeItemInput, TreeLinkInput, TreeLinkTarget, VariantFingerprint, Visibility,
    };
    use backend_version::ScopeRoot;

    use super::*;

    fn image(count: usize, edited: Option<usize>) -> Ir {
        let names: Vec<String> = (0..count).map(|index| format!("n{index:03}")).collect();
        let docs: Vec<String> = (0..count)
            .map(|index| {
                if edited == Some(index) {
                    format!("β{index:03}")
                } else {
                    format!("α{index:03}")
                }
            })
            .collect();
        let doc_inputs: Vec<[DocInput<'_>; 1]> =
            docs.iter().map(|text| [DocInput::Text(text)]).collect();
        let versions: Vec<EntityVersion> = (0..count)
            .map(|index| EntityVersion {
                family: DeclarationFamilyId::from_raw([(index + 1) as u8; 16]),
                variant: VariantFingerprint::from_raw([(index + 2) as u8; 16]),
                core_payload: CorePayloadHash::from_raw([(index + 3) as u8; 16]),
            })
            .collect();
        let authority = EntityAuthorityFacts {
            parentage: ParentageAuthority::Root,
            members: FactAvailability::Captured,
            documentation: FactAvailability::Captured,
            visibility: FactAvailability::Captured,
            attributes: FactAvailability::Captured,
            ..EntityAuthorityFacts::default()
        };
        let items: Vec<TreeItemInput<'_>> = (0..count)
            .map(|index| TreeItemInput {
                name: names[index].as_bytes(),
                anonymous_callable_anchor: None,
                kind: ItemKind::Function,
                visibility: Visibility::Public,
                authority,
                parent: None,
                semantic_type: None,
                members: &[],
                docs: &doc_inputs[index],
                attributes: &[],
                source: None,
                extension: None,
            })
            .collect();
        let mut builder = IrBuilder::new();
        builder
            .add_borrowed_tree(BorrowedTree {
                versions: &versions,
                items: &items,
                links: &[],
            })
            .expect("fixture tree is valid");
        builder.finish().expect("fixture IR is valid")
    }

    fn graph_image(reverse_occurrences: bool, edit_other_site: bool) -> Ir {
        let mut builder = IrBuilder::new();
        let file = builder
            .intern_atom(b"src/graph.rs")
            .expect("source path interns");
        let span = |start, end| SourceSpan::new(file, start, end).expect("valid source span");
        let versions = [
            EntityVersion {
                family: DeclarationFamilyId::from_raw([0x11; 16]),
                variant: VariantFingerprint::from_raw([0x12; 16]),
                core_payload: CorePayloadHash::from_raw([0x13; 16]),
            },
            EntityVersion {
                family: DeclarationFamilyId::from_raw([0x21; 16]),
                variant: VariantFingerprint::from_raw([0x22; 16]),
                core_payload: CorePayloadHash::from_raw([0x23; 16]),
            },
        ];
        let authority = EntityAuthorityFacts {
            parentage: ParentageAuthority::Root,
            members: FactAvailability::Captured,
            documentation: FactAvailability::Captured,
            visibility: FactAvailability::Captured,
            attributes: FactAvailability::Captured,
            ..EntityAuthorityFacts::default()
        };
        let items = [
            TreeItemInput {
                name: b"caller",
                anonymous_callable_anchor: None,
                kind: ItemKind::Function,
                visibility: Visibility::Public,
                authority,
                parent: None,
                semantic_type: None,
                members: &[],
                docs: &[],
                attributes: &[],
                source: None,
                extension: None,
            },
            TreeItemInput {
                name: b"callee",
                anonymous_callable_anchor: None,
                kind: ItemKind::Function,
                visibility: Visibility::Public,
                authority,
                parent: None,
                semantic_type: None,
                members: &[],
                docs: &[],
                attributes: &[],
                source: None,
                extension: None,
            },
        ];
        let duplicate = TreeLinkInput {
            from: TreeEntityId::new(0),
            target: TreeLinkTarget::Local(TreeEntityId::new(1)),
            kind: LinkKind::Calls,
            confidence: Confidence::Compiler,
            authority: OccurrenceAuthorityFacts {
                source: FactAvailability::Captured,
            },
            source: Some(span(3, 8)),
        };
        let other_site = TreeLinkInput {
            confidence: Confidence::Heuristic,
            source: Some(span(
                13 + if edit_other_site { 1 } else { 0 },
                18 + if edit_other_site { 1 } else { 0 },
            )),
            ..duplicate
        };
        let links = if reverse_occurrences {
            [other_site, duplicate, duplicate]
        } else {
            [duplicate, duplicate, other_site]
        };
        builder
            .add_borrowed_tree(BorrowedTree {
                versions: &versions,
                items: &items,
                links: &links,
            })
            .expect("fixture graph is valid");
        builder.finish().expect("fixture IR is valid")
    }

    fn representative_image(reverse_paths: bool) -> Ir {
        let mut builder = IrBuilder::new();
        let (path_z, path_a) = if reverse_paths {
            let z = builder.intern_atom(b"z.rs").expect("z path interns");
            let a = builder.intern_atom(b"a.rs").expect("a path interns");
            (z, a)
        } else {
            let a = builder.intern_atom(b"a.rs").expect("a path interns");
            let z = builder.intern_atom(b"z.rs").expect("z path interns");
            (z, a)
        };
        let versions = [
            EntityVersion {
                family: DeclarationFamilyId::from_raw([0x31; 16]),
                variant: VariantFingerprint::from_raw([0x32; 16]),
                core_payload: CorePayloadHash::from_raw([0x33; 16]),
            },
            EntityVersion {
                family: DeclarationFamilyId::from_raw([0x41; 16]),
                variant: VariantFingerprint::from_raw([0x42; 16]),
                core_payload: CorePayloadHash::from_raw([0x43; 16]),
            },
        ];
        let authority = EntityAuthorityFacts {
            parentage: ParentageAuthority::Root,
            members: FactAvailability::Captured,
            documentation: FactAvailability::Captured,
            visibility: FactAvailability::Captured,
            attributes: FactAvailability::Captured,
            ..EntityAuthorityFacts::default()
        };
        let items = [
            TreeItemInput {
                name: b"from",
                anonymous_callable_anchor: None,
                kind: ItemKind::Function,
                visibility: Visibility::Public,
                authority,
                parent: None,
                semantic_type: None,
                members: &[],
                docs: &[],
                attributes: &[],
                source: None,
                extension: None,
            },
            TreeItemInput {
                name: b"to",
                anonymous_callable_anchor: None,
                kind: ItemKind::Function,
                visibility: Visibility::Public,
                authority,
                parent: None,
                semantic_type: None,
                members: &[],
                docs: &[],
                attributes: &[],
                source: None,
                extension: None,
            },
        ];
        let link = |file, start| TreeLinkInput {
            from: TreeEntityId::new(0),
            target: TreeLinkTarget::Local(TreeEntityId::new(1)),
            kind: LinkKind::Calls,
            confidence: Confidence::Compiler,
            authority: OccurrenceAuthorityFacts {
                source: FactAvailability::Captured,
            },
            source: Some(SourceSpan::new(file, start, start + 1).expect("valid source span")),
        };
        let z_source = link(path_z, 3);
        let a_source = link(path_a, 9);
        let links = if reverse_paths {
            [a_source, z_source]
        } else {
            [z_source, a_source]
        };
        builder
            .add_borrowed_tree(BorrowedTree {
                versions: &versions,
                items: &items,
                links: &links,
            })
            .expect("fixture graph is valid");
        builder.finish().expect("fixture IR is valid")
    }

    fn witness() -> SemanticInputWitness {
        SemanticInputWitness::claimed([0xA1; 32], ScopeRoot::from_bytes([0xB2; 32]))
    }

    fn jumbo_docs_image(text: &str) -> Ir {
        let version = EntityVersion {
            family: DeclarationFamilyId::from_raw([0x51; 16]),
            variant: VariantFingerprint::from_raw([0x52; 16]),
            core_payload: CorePayloadHash::from_raw([0x53; 16]),
        };
        let docs = [DocInput::Text(text)];
        let item = TreeItemInput {
            name: b"jumbo_docs",
            anonymous_callable_anchor: None,
            kind: ItemKind::Function,
            visibility: Visibility::Public,
            authority: EntityAuthorityFacts {
                parentage: ParentageAuthority::Root,
                members: FactAvailability::Captured,
                documentation: FactAvailability::Captured,
                visibility: FactAvailability::Captured,
                attributes: FactAvailability::Captured,
                ..EntityAuthorityFacts::default()
            },
            parent: None,
            semantic_type: None,
            members: &[],
            docs: &docs,
            attributes: &[],
            source: None,
            extension: None,
        };
        let mut builder = IrBuilder::new();
        builder
            .add_borrowed_tree(BorrowedTree {
                versions: &[version],
                items: &[item],
                links: &[],
            })
            .expect("jumbo documentation tree is valid");
        builder.finish().expect("jumbo documentation IR is valid")
    }

    fn small_then_jumbo_docs_image(text: &str) -> (Ir, [u8; 32], [u8; 32]) {
        let versions = [
            EntityVersion {
                family: DeclarationFamilyId::from_raw([0x71; 16]),
                variant: VariantFingerprint::from_raw([0x72; 16]),
                core_payload: CorePayloadHash::from_raw([0x73; 16]),
            },
            EntityVersion {
                family: DeclarationFamilyId::from_raw([0x81; 16]),
                variant: VariantFingerprint::from_raw([0x82; 16]),
                core_payload: CorePayloadHash::from_raw([0x83; 16]),
            },
        ];
        let kind = SemanticPlaneKind::Ir(SemanticIrPlane::Documentation);
        let keys = [
            declaration_plane_key(kind, versions[0].identity()),
            declaration_plane_key(kind, versions[1].identity()),
        ];
        let large_is_first = keys[0] > keys[1];
        let docs = if large_is_first {
            [[DocInput::Text(text)], [DocInput::Text("small")]]
        } else {
            [[DocInput::Text("small")], [DocInput::Text(text)]]
        };
        let authority = EntityAuthorityFacts {
            parentage: ParentageAuthority::Root,
            members: FactAvailability::Captured,
            documentation: FactAvailability::Captured,
            visibility: FactAvailability::Captured,
            attributes: FactAvailability::Captured,
            ..EntityAuthorityFacts::default()
        };
        let items = [
            TreeItemInput {
                name: b"small_first",
                anonymous_callable_anchor: None,
                kind: ItemKind::Function,
                visibility: Visibility::Public,
                authority,
                parent: None,
                semantic_type: None,
                members: &[],
                docs: &docs[0],
                attributes: &[],
                source: None,
                extension: None,
            },
            TreeItemInput {
                name: b"jumbo_second",
                anonymous_callable_anchor: None,
                kind: ItemKind::Function,
                visibility: Visibility::Public,
                authority,
                parent: None,
                semantic_type: None,
                members: &[],
                docs: &docs[1],
                attributes: &[],
                source: None,
                extension: None,
            },
        ];
        let mut builder = IrBuilder::new();
        builder
            .add_borrowed_tree(BorrowedTree {
                versions: &versions,
                items: &items,
                links: &[],
            })
            .expect("two-documentation fixture is valid");
        let small_key = keys[0].min(keys[1]);
        let jumbo_key = keys[0].max(keys[1]);
        (
            builder.finish().expect("two-documentation IR is valid"),
            small_key,
            jumbo_key,
        )
    }

    fn jumbo_source_image(path: &[u8]) -> Ir {
        let mut builder = IrBuilder::new();
        let file = builder
            .intern_atom(path)
            .expect("jumbo source path interns");
        let source = SourceSpan::new(file, 3, 17).expect("jumbo source span is valid");
        let version = EntityVersion {
            family: DeclarationFamilyId::from_raw([0x61; 16]),
            variant: VariantFingerprint::from_raw([0x62; 16]),
            core_payload: CorePayloadHash::from_raw([0x63; 16]),
        };
        let item = TreeItemInput {
            name: b"jumbo_source",
            anonymous_callable_anchor: None,
            kind: ItemKind::Function,
            visibility: Visibility::Public,
            authority: EntityAuthorityFacts {
                parentage: ParentageAuthority::Root,
                source: FactAvailability::Captured,
                source_file: FactAvailability::Captured,
                visibility: FactAvailability::Captured,
                ..EntityAuthorityFacts::default()
            },
            parent: None,
            semantic_type: None,
            members: &[],
            docs: &[],
            attributes: &[],
            source: Some(source),
            extension: None,
        };
        builder
            .add_borrowed_tree(BorrowedTree {
                versions: &[version],
                items: &[item],
                links: &[],
            })
            .expect("jumbo source tree is valid");
        builder.finish().expect("jumbo source IR is valid")
    }

    #[derive(Clone, Default)]
    struct InMemoryJumboObjects {
        leaves: BTreeMap<JumboRopeObjectId, Vec<u8>>,
        interiors: BTreeMap<JumboRopeObjectId, [u8; ROPE_NODE_WIRE_BYTES]>,
        leaf_order: Vec<JumboRopeObjectId>,
    }

    impl JumboRopeObjectSink for InMemoryJumboObjects {
        type Error = SemanticPlaneRecordError;

        fn write_leaf(&mut self, leaf: JumboRopeLeafRef<'_>) -> Result<(), Self::Error> {
            let bytes = leaf.bytes().to_vec();
            if let Some(previous) = self.leaves.insert(leaf.id(), bytes.clone()) {
                assert_eq!(previous, bytes, "content-addressed leaves are immutable");
            }
            self.leaf_order.push(leaf.id());
            Ok(())
        }

        fn write_interior(&mut self, node: &JumboRopeNode) -> Result<(), Self::Error> {
            let id = node.id();
            let bytes = node.encode_wire();
            if let Some(previous) = self.interiors.insert(id, bytes) {
                assert_eq!(previous, bytes, "content-addressed nodes are immutable");
            }
            Ok(())
        }
    }

    impl JumboRopeObjectSource for InMemoryJumboObjects {
        type Error = SemanticPlaneRecordError;

        fn read_leaf(
            &mut self,
            id: JumboRopeObjectId,
            output: &mut [u8; JUMBO_ROPE_MAX_LEAF_BYTES],
        ) -> Result<Option<usize>, Self::Error> {
            let Some(bytes) = self.leaves.get(&id) else {
                return Ok(None);
            };
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
            Ok(self.interiors.get(&id).copied())
        }
    }

    #[derive(Default)]
    struct CapturedFamily {
        rows: Vec<(SemanticPlaneSegment, Vec<u8>)>,
        maximum_payload_bytes: usize,
    }

    impl CanonicalSemanticPlaneSegmentSink for CapturedFamily {
        type Error = SemanticPlaneRecordError;

        fn write_segment(
            &mut self,
            segment: CanonicalSemanticPlaneSegmentRef<'_>,
        ) -> Result<(), Self::Error> {
            self.maximum_payload_bytes = self.maximum_payload_bytes.max(segment.bytes().len());
            self.rows
                .push((segment.metadata()?, segment.bytes().to_vec()));
            Ok(())
        }
    }

    fn verify_captured_jumbo_family(
        family: SemanticIrPlane,
        captured: &CapturedFamily,
        objects: &mut InMemoryJumboObjects,
    ) -> VerifiedJumboPlaneClosure {
        let descriptors: Vec<_> = captured
            .rows
            .iter()
            .map(|(descriptor, _)| *descriptor)
            .collect();
        let payloads: Vec<_> = captured
            .rows
            .iter()
            .map(|(_, payload)| payload.as_slice())
            .collect();
        verify_jumbo_plane_family_closures(
            SemanticPlaneKind::Ir(family),
            &descriptors,
            &payloads,
            objects,
        )
        .expect("typed family and all jumbo closures verify")
    }

    fn verify_captured_jumbo_family_with_policy(
        family: SemanticIrPlane,
        captured: &CapturedFamily,
        policy: CanonicalPlaneSegmentBoundaryPolicy,
        objects: &mut InMemoryJumboObjects,
    ) -> VerifiedJumboPlaneClosure {
        let descriptors: Vec<_> = captured
            .rows
            .iter()
            .map(|(descriptor, _)| *descriptor)
            .collect();
        let payloads: Vec<_> = captured
            .rows
            .iter()
            .map(|(_, payload)| payload.as_slice())
            .collect();
        verify_jumbo_plane_family_closures_with_policy(
            SemanticPlaneKind::Ir(family),
            &descriptors,
            &payloads,
            policy,
            objects,
        )
        .expect("typed family and policy-bound jumbo closures verify")
    }

    fn ids(
        values: &[CanonicalSemanticPlaneSegmentPayload],
    ) -> Result<Vec<SemanticSegmentId>, SemanticPlaneRecordError> {
        values
            .iter()
            .map(|value| {
                value
                    .metadata()?
                    .admitted_id()
                    .ok_or(SemanticPlaneRecordError::MissingAdmittedId)
            })
            .collect()
    }

    #[derive(Clone, Copy)]
    struct CollidingKeys;

    impl CanonicalPlaneRowEncoder for CollidingKeys {
        type Handle = ();
        type Plan = ();

        fn kind(&self) -> SemanticPlaneKind {
            SemanticPlaneKind::Ir(SemanticIrPlane::Core)
        }

        fn build_plan<Reader: SemanticReader + ?Sized>(
            &self,
            _reader: &Reader,
        ) -> Result<Self::Plan, SemanticPlaneRecordError> {
            Ok(())
        }

        fn collect_keys<Reader: SemanticReader + ?Sized>(
            &self,
            _reader: &Reader,
            _plan: &Self::Plan,
            sink: &mut CanonicalSemanticPlaneKeySink<Self::Handle>,
        ) -> Result<(), SemanticPlaneRecordError> {
            sink.push([7; 32], ())?;
            sink.push([7; 32], ())
        }

        fn encode_row<Reader: SemanticReader + ?Sized>(
            &self,
            _reader: &Reader,
            _plan: &Self::Plan,
            _handle: Self::Handle,
            _payload: &mut Vec<u8>,
        ) -> Result<u8, SemanticPlaneRecordError> {
            Ok(0)
        }
    }

    #[derive(Clone, Copy)]
    struct InvalidDocumentationRow;

    impl CanonicalPlaneRowEncoder for InvalidDocumentationRow {
        type Handle = ();
        type Plan = ();

        fn kind(&self) -> SemanticPlaneKind {
            SemanticPlaneKind::Ir(SemanticIrPlane::Documentation)
        }

        fn build_plan<Reader: SemanticReader + ?Sized>(
            &self,
            _reader: &Reader,
        ) -> Result<Self::Plan, SemanticPlaneRecordError> {
            Ok(())
        }

        fn collect_keys<Reader: SemanticReader + ?Sized>(
            &self,
            _reader: &Reader,
            _plan: &Self::Plan,
            sink: &mut CanonicalSemanticPlaneKeySink<Self::Handle>,
        ) -> Result<(), SemanticPlaneRecordError> {
            sink.push([0x31; 32], ())
        }

        fn encode_row<Reader: SemanticReader + ?Sized>(
            &self,
            _reader: &Reader,
            _plan: &Self::Plan,
            _handle: Self::Handle,
            _payload: &mut Vec<u8>,
        ) -> Result<u8, SemanticPlaneRecordError> {
            Ok(u8::MAX)
        }
    }

    #[derive(Default)]
    struct SegmentDigestSink {
        ids: Vec<SemanticSegmentId>,
        maximum_payload_bytes: usize,
    }

    impl CanonicalSemanticPlaneSegmentSink for SegmentDigestSink {
        type Error = SemanticPlaneRecordError;

        fn write_segment(
            &mut self,
            segment: CanonicalSemanticPlaneSegmentRef<'_>,
        ) -> Result<(), Self::Error> {
            let id = segment
                .metadata()?
                .admitted_id()
                .ok_or(SemanticPlaneRecordError::MissingAdmittedId)?;
            self.ids
                .try_reserve(1)
                .map_err(SemanticPlaneRecordError::Allocation)?;
            self.ids.push(id);
            self.maximum_payload_bytes = self.maximum_payload_bytes.max(segment.bytes().len());
            Ok(())
        }
    }

    #[derive(Debug, Eq, PartialEq)]
    struct SegmentSinkFailure;

    #[derive(Default)]
    struct FailingFirstSegment {
        calls: usize,
    }

    impl CanonicalSemanticPlaneSegmentSink for FailingFirstSegment {
        type Error = SegmentSinkFailure;

        fn write_segment(
            &mut self,
            _segment: CanonicalSemanticPlaneSegmentRef<'_>,
        ) -> Result<(), Self::Error> {
            self.calls += 1;
            Err(SegmentSinkFailure)
        }
    }

    #[derive(Default)]
    struct CapturedTypedRows {
        rows: Vec<(SemanticIrPlane, [u8; 32], u8, Vec<u8>)>,
    }

    impl CanonicalSemanticPlaneRowSink for CapturedTypedRows {
        type Error = SemanticPlaneRecordError;

        fn write_row(
            &mut self,
            family: SemanticIrPlane,
            row: CanonicalSemanticPlaneRecordView<'_>,
        ) -> Result<(), Self::Error> {
            let mut payload = Vec::new();
            payload
                .try_reserve_exact(row.payload().len())
                .map_err(SemanticPlaneRecordError::Allocation)?;
            payload.extend_from_slice(row.payload());
            self.rows
                .try_reserve(1)
                .map_err(SemanticPlaneRecordError::Allocation)?;
            self.rows.push((family, row.key(), row.tag(), payload));
            Ok(())
        }
    }

    #[derive(Debug, Eq, PartialEq)]
    struct RowSinkFailure;

    #[derive(Default)]
    struct FailingTypedRows {
        calls: usize,
    }

    impl CanonicalSemanticPlaneRowSink for FailingTypedRows {
        type Error = RowSinkFailure;

        fn write_row(
            &mut self,
            _family: SemanticIrPlane,
            _row: CanonicalSemanticPlaneRecordView<'_>,
        ) -> Result<(), Self::Error> {
            self.calls += 1;
            Err(RowSinkFailure)
        }
    }

    #[test]
    fn borrowed_row_stream_matches_strict_spir_decode_and_reports_full_family_work() {
        let reader = image(64, None);
        let input = witness();
        let policy = CanonicalPlaneSegmentBoundaryPolicy::stable_key_hash_ramp(300, 768, 1024)
            .expect("family boundary policy is valid");
        let mut row_objects = InMemoryJumboObjects::default();
        let mut captured_rows = CapturedTypedRows::default();
        let row_metrics = stream_canonical_plane_family_rows_with_limits(
            &reader,
            &DocumentationRows,
            policy.maximum_bytes(),
            policy.maximum_bytes(),
            128,
            1024,
            4096,
            crate::ir::JumboRopeLimits::default(),
            &mut row_objects,
            &mut captured_rows,
        )
        .expect("bounded row visitor admits the complete documentation family");
        assert_eq!(row_metrics.row_count(), 64);
        assert_eq!(row_metrics.row_encode_calls(), 64);
        assert_eq!(row_metrics.plan_build_calls(), 1);
        assert_eq!(row_metrics.key_collection_calls(), 1);
        assert_eq!(row_metrics.key_sort_rows(), 64);
        assert!(row_metrics.row_index_capacity_bytes() > 0);
        assert!(row_metrics.peak_row_scratch_capacity_bytes() > 0);

        let mut segment_objects = InMemoryJumboObjects::default();
        let mut segments = OwnedPlaneSegments::default();
        let segment_metrics = stream_canonical_plane_family_with_jumbo_and_stable_key_anchors(
            &reader,
            &DocumentationRows,
            input,
            policy,
            &mut segment_objects,
            &mut segments,
        )
        .expect("the production SPIR sink uses the shared row emitter");
        assert_eq!(segment_metrics.plan_build_calls(), 1);
        assert_eq!(segment_metrics.key_collection_calls(), 1);
        assert_eq!(segment_metrics.key_sort_rows(), 64);
        assert_eq!(segment_metrics.row_encode_calls(), 64);
        assert!(
            segments
                .segments
                .iter()
                .all(|segment| segment.bytes().len() <= policy.maximum_bytes())
        );

        let mut decoded_rows = Vec::new();
        for segment in &segments.segments {
            let descriptor = segment.metadata().expect("SPIR segment claim");
            let admitted_id = descriptor
                .admit(segment.kind(), segment.bytes())
                .expect("streaming identity verifier admits the exact bytes");
            assert_eq!(
                Some(admitted_id),
                descriptor.admitted_id(),
                "the persisted claim and independent streaming identity agree"
            );
            let decoded = decode_semantic_plane_segment_with_row_limit(
                segment.kind(),
                &descriptor,
                segment.bytes(),
                policy.maximum_bytes(),
            )
            .expect("independent SPIR decoder admits the emitted segment");
            let mut exact_wire = Vec::new();
            exact_wire.extend_from_slice(b"SPIR");
            exact_wire.extend_from_slice(&2_u16.to_be_bytes());
            exact_wire.push(5);
            exact_wire.extend_from_slice(&decoded.row_count().to_be_bytes());
            for row in decoded.records() {
                let row_payload_length =
                    u32::try_from(row.payload().len()).expect("decoded row length fits u32");
                exact_wire.extend_from_slice(&row.key());
                exact_wire.push(row.tag());
                exact_wire.extend_from_slice(&row_payload_length.to_be_bytes());
                exact_wire.extend_from_slice(row.payload());
                decoded_rows.push((
                    SemanticIrPlane::Documentation,
                    row.key(),
                    row.tag(),
                    row.payload().to_vec(),
                ));
            }
            assert_eq!(
                exact_wire.as_slice(),
                segment.bytes(),
                "canonical decoded rows reconstruct every emitted SPIR byte"
            );

            let mut changed = segment.bytes().to_vec();
            let final_byte = changed.last_mut().expect("SPIR segment is nonempty");
            *final_byte ^= 1;
            assert!(
                descriptor.admit(segment.kind(), &changed).is_err(),
                "one changed byte cannot retain the segment's admitted identity"
            );
        }
        assert_eq!(captured_rows.rows, decoded_rows);
    }

    #[test]
    fn borrowed_row_stream_keeps_policy_bound_jumbo_descriptors_and_scratch_metrics() {
        let policy = CanonicalPlaneSegmentBoundaryPolicy::stable_key_hash_ramp(512, 1024, 4096)
            .expect("family boundary policy is valid");
        let inline_text_limit =
            policy.maximum_bytes() - (HEADER_BYTES + RECORD_HEADER_BYTES + 32 + 1 + 4 + 1 + 4);
        let reader = jumbo_docs_image(&"d".repeat(inline_text_limit + 1));

        let mut row_objects = InMemoryJumboObjects::default();
        let mut captured_rows = CapturedTypedRows::default();
        let row_metrics = stream_canonical_plane_family_rows_with_limits(
            &reader,
            &DocumentationRows,
            policy.maximum_bytes(),
            policy.maximum_bytes(),
            8,
            32,
            64,
            crate::ir::JumboRopeLimits::default(),
            &mut row_objects,
            &mut captured_rows,
        )
        .expect("row visitor externalizes the over-threshold documentation value");
        assert_eq!(row_metrics.row_count(), 1);
        let captured_row = captured_rows.rows.first().expect("captured jumbo row");
        assert_eq!(captured_row.2, declarations::DOCS_JUMBO_TAG);
        assert!(row_metrics.peak_jumbo_rope_scratch_bytes() > 0);

        let mut segment_objects = InMemoryJumboObjects::default();
        let mut segments = OwnedPlaneSegments::default();
        let segment_metrics = stream_canonical_plane_family_with_jumbo_and_stable_key_anchors(
            &reader,
            &DocumentationRows,
            witness(),
            policy,
            &mut segment_objects,
            &mut segments,
        )
        .expect("SPIR and row sinks use the same policy-aware jumbo writer");
        assert!(segment_metrics.peak_jumbo_rope_scratch_bytes() > 0);
        assert_eq!(segments.segments.len(), 1);
        let segment = segments.segments.first().expect("one jumbo segment");
        let descriptor = segment.metadata().expect("jumbo SPIR descriptor");
        let decoded = decode_semantic_plane_segment_with_row_limit(
            segment.kind(),
            &descriptor,
            segment.bytes(),
            policy.maximum_bytes(),
        )
        .expect("strict SPIR decoder admits the policy-bound jumbo row");
        let decoded_row = decoded.records().next().expect("one documentation row");
        assert_eq!(captured_row.1, decoded_row.key());
        assert_eq!(captured_row.2, decoded_row.tag());
        assert_eq!(captured_row.3.as_slice(), decoded_row.payload());
    }

    #[test]
    fn borrowed_row_stream_preserves_budget_grammar_and_sink_failures() {
        let reader = image(2, None);
        let mut jumbo_objects = InMemoryJumboObjects::default();
        let mut captured = CapturedTypedRows::default();
        let boundary = crate::ir::MAX_SEMANTIC_SEGMENT_BYTES;

        assert!(matches!(
            stream_canonical_plane_family_rows_with_limits(
                &reader,
                &DocumentationRows,
                boundary,
                1,
                0,
                1024,
                4096,
                crate::ir::JumboRopeLimits::default(),
                &mut jumbo_objects,
                &mut captured,
            ),
            Err(CanonicalPlaneRowStreamError::Encoding(
                SemanticPlaneRecordError::RowBudgetExceeded { maximum: 0 }
            ))
        ));
        assert!(captured.rows.is_empty());

        assert!(matches!(
            stream_canonical_plane_family_rows_with_limits(
                &reader,
                &DocumentationRows,
                boundary,
                1,
                16,
                1024,
                4096,
                crate::ir::JumboRopeLimits::default(),
                &mut jumbo_objects,
                &mut captured,
            ),
            Err(CanonicalPlaneRowStreamError::Encoding(
                SemanticPlaneRecordError::RowPayloadExceedsCeiling { maximum: 1, .. }
            ))
        ));
        assert!(captured.rows.is_empty());

        let mut invalid_rows = CapturedTypedRows::default();
        assert!(matches!(
            stream_canonical_plane_family_rows_with_limits(
                &reader,
                &InvalidDocumentationRow,
                boundary,
                boundary,
                16,
                1024,
                4096,
                crate::ir::JumboRopeLimits::default(),
                &mut jumbo_objects,
                &mut invalid_rows,
            ),
            Err(CanonicalPlaneRowStreamError::Encoding(
                SemanticPlaneRecordError::RowGrammar
            ))
        ));
        assert!(invalid_rows.rows.is_empty());

        let mut failing = FailingTypedRows::default();
        assert!(matches!(
            stream_canonical_plane_family_rows_with_limits(
                &reader,
                &DocumentationRows,
                boundary,
                boundary,
                16,
                1024,
                4096,
                crate::ir::JumboRopeLimits::default(),
                &mut jumbo_objects,
                &mut failing,
            ),
            Err(CanonicalPlaneRowStreamError::Sink(RowSinkFailure))
        ));
        assert_eq!(failing.calls, 1);

        let mut collision_rows = CapturedTypedRows::default();
        assert!(matches!(
            stream_canonical_plane_family_rows_with_limits(
                &reader,
                &CollidingKeys,
                boundary,
                boundary,
                16,
                1024,
                4096,
                crate::ir::JumboRopeLimits::default(),
                &mut jumbo_objects,
                &mut collision_rows,
            ),
            Err(CanonicalPlaneRowStreamError::Encoding(
                SemanticPlaneRecordError::StableKeyCollision
            ))
        ));
        assert!(collision_rows.rows.is_empty());
    }

    #[test]
    fn segment_sink_failure_at_key_cut_precedes_the_next_rows_jumbo_writes() {
        let text = String::from("j").repeat(32 * 1024);
        let (reader, small_key, jumbo_key) = small_then_jumbo_docs_image(&text);
        assert!(small_key < jumbo_key);
        let policy = CanonicalPlaneSegmentBoundaryPolicy::stable_key_hash_ramp(
            HEADER_BYTES as u32,
            HEADER_BYTES as u32,
            4096,
        )
        .expect("boundary immediately after the first row is valid");
        let mut objects = InMemoryJumboObjects::default();
        let mut sink = FailingFirstSegment::default();
        assert!(matches!(
            stream_canonical_plane_family_with_jumbo_stable_key_anchors_and_limits(
                &reader,
                &DocumentationRows,
                witness(),
                policy,
                2,
                8,
                32,
                crate::ir::JumboRopeLimits::default(),
                &mut objects,
                &mut sink,
            ),
            Err(CanonicalPlaneStreamError::Sink(SegmentSinkFailure))
        ));
        assert_eq!(sink.calls, 1);
        assert!(objects.leaf_order.is_empty());
        assert!(objects.interiors.is_empty());
    }

    #[test]
    fn docs_edits_at_start_middle_and_end_leave_unrelated_segment_ids_stable()
    -> Result<(), SemanticPlaneRecordError> {
        let base = image(128, None);
        let input = witness();
        let core = encode_canonical_plane_family(&base, &CoreDeclarationRows, input, 320)
            .expect("core planes encode");
        let measured =
            encode_canonical_plane_family_measured(&base, &DocumentationRows, input, 320)
                .expect("documentation planes encode");
        let metrics = measured.metrics();
        assert_eq!(metrics.row_count(), 128);
        assert_eq!(metrics.row_encode_calls(), 128);
        assert_eq!(metrics.segment_count() as usize, measured.segments().len());
        assert_eq!(
            metrics.output_bytes() as usize,
            measured
                .segments()
                .iter()
                .map(|segment| segment.bytes().len())
                .sum::<usize>()
        );
        assert!(metrics.output_bytes() > metrics.peak_row_scratch_capacity_bytes());
        assert!(metrics.peak_segment_scratch_capacity_bytes() <= 320);
        let docs = measured.into_segments();

        let mut streaming_sink = SegmentDigestSink::default();
        let streamed_metrics = stream_canonical_plane_family(
            &base,
            &DocumentationRows,
            input,
            320,
            &mut streaming_sink,
        )
        .expect("streamed docs planes encode");
        assert_eq!(
            streaming_sink.ids,
            ids(&docs)?,
            "borrowed stream emits the same canonical payload IDs"
        );
        assert_eq!(streamed_metrics.output_bytes(), metrics.output_bytes());
        assert!(streaming_sink.maximum_payload_bytes <= 320);

        let descriptors: Vec<_> = docs
            .iter()
            .map(|segment| segment.metadata())
            .collect::<Result<_, _>>()?;
        let payloads: Vec<_> = docs.iter().map(|segment| segment.bytes()).collect();
        verify_semantic_plane_family_against_reader(
            &base,
            &DocumentationRows,
            input,
            &descriptors,
            &payloads,
            320,
        )
        .expect("complete family oracle verifies");

        for edited in [0, 64, 127] {
            let target = image(128, Some(edited));
            let next_core =
                encode_canonical_plane_family(&target, &CoreDeclarationRows, input, 320)
                    .expect("core planes re-encode");
            let next_docs = encode_canonical_plane_family(&target, &DocumentationRows, input, 320)
                .expect("documentation planes re-encode");
            assert_eq!(ids(&core)?, ids(&next_core)?);
            let original = ids(&docs)?;
            let changed = ids(&next_docs)?;
            assert_eq!(original.len(), changed.len());
            assert_eq!(
                original.iter().filter(|id| changed.contains(*id)).count(),
                original.len() - 1,
                "only the one stable-key bucket holding declaration {edited} should change"
            );
        }
        Ok(())
    }

    #[test]
    fn stable_key_anchor_cuts_reverify_complete_family_and_reject_valid_but_noncanonical_splits() {
        let base = image(128, None);
        let input = witness();
        let kind = SemanticPlaneKind::Ir(SemanticIrPlane::Documentation);
        let policy = CanonicalPlaneSegmentBoundaryPolicy::stable_key_hash_ramp(300, 768, 1024)
            .expect("stable key boundary policy is valid");
        let mut canonical = OwnedPlaneSegments::default();
        let metrics = stream_canonical_plane_family_with_stable_key_anchors(
            &base,
            &DocumentationRows,
            input,
            policy,
            &mut canonical,
        )
        .expect("stable-key family streams under the committed ramp policy");
        assert_eq!(metrics.row_count(), 128);
        assert_eq!(metrics.row_encode_calls(), 128);
        assert!(metrics.anchor_key_hash_bytes() > 0);
        assert_eq!(
            metrics.anchor_hash_rows() * 32,
            metrics.anchor_key_hash_bytes()
        );
        assert!(
            canonical
                .segments
                .iter()
                .all(|segment| segment.bytes().len() <= policy.maximum_bytes())
        );
        let descriptors = canonical
            .segments
            .iter()
            .map(|segment| segment.metadata().expect("segment descriptor"))
            .collect::<Vec<_>>();
        let payloads = canonical
            .segments
            .iter()
            .map(|segment| segment.bytes())
            .collect::<Vec<_>>();
        verify_canonical_semantic_plane_segment_boundaries(
            kind,
            &descriptors,
            &payloads,
            policy,
            metrics.row_count(),
        )
        .expect("cold family verifier recomputes all anchor and maximum-size cuts");
        assert!(
            descriptors.len() < metrics.row_count() as usize,
            "the source policy groups multiple rows, giving the alternate-policy check a discriminating family"
        );

        let singleton_policy = CanonicalPlaneSegmentBoundaryPolicy::stable_key_hash_ramp(
            HEADER_BYTES as u32,
            HEADER_BYTES as u32,
            1024,
        )
        .expect("singleton policy has valid hard limits");
        let mut singleton_family = OwnedPlaneSegments::default();
        let singleton_metrics = stream_canonical_plane_family_with_stable_key_anchors(
            &image(1, None),
            &DocumentationRows,
            input,
            singleton_policy,
            &mut singleton_family,
        )
        .expect("one record larger than target but below maximum is valid at terminal EOF");
        assert_eq!(singleton_metrics.row_count(), 1);
        assert_eq!(singleton_metrics.segment_count(), 1);
        assert!(singleton_family.segments[0].bytes().len() > singleton_policy.target_bytes());
        let singleton_descriptor = singleton_family.segments[0]
            .metadata()
            .expect("terminal singleton descriptor");
        let singleton_payload = singleton_family.segments[0].bytes();
        verify_canonical_semantic_plane_segment_boundaries(
            kind,
            &[singleton_descriptor],
            &[singleton_payload],
            singleton_policy,
            1,
        )
        .expect("the cold verifier preserves terminal singleton and target-oversize semantics");

        let alternate_policy = CanonicalPlaneSegmentBoundaryPolicy::stable_key_hash_ramp(
            HEADER_BYTES as u32,
            HEADER_BYTES as u32,
            1024,
        )
        .expect("alternate policy is valid");
        assert!(matches!(
            verify_canonical_semantic_plane_segment_boundaries(
                kind,
                &descriptors,
                &payloads,
                alternate_policy,
                metrics.row_count(),
            ),
            Err(SemanticPlaneRecordError::NonCanonicalSegmentBoundary)
        ));

        let singleton_view =
            decode_semantic_plane_segment(kind, &singleton_descriptor, singleton_payload)
                .expect("the large terminal record remains a strict-valid row");
        let row = singleton_view
            .records()
            .next()
            .expect("singleton record exists");
        let observed = HEADER_BYTES + row.encoded_len();
        let smaller_ceiling = u32::try_from(observed - 1).expect("fixture row size fits u32");
        let too_small_policy = CanonicalPlaneSegmentBoundaryPolicy::stable_key_hash_ramp(
            HEADER_BYTES as u32,
            HEADER_BYTES as u32,
            smaller_ceiling,
        )
        .expect("smaller single-row ceiling is valid policy metadata");
        let mut rejected_singleton = OwnedPlaneSegments::default();
        assert!(matches!(
            stream_canonical_plane_family_with_stable_key_anchors(
                &image(1, None),
                &DocumentationRows,
                input,
                too_small_policy,
                &mut rejected_singleton,
            ),
            Err(CanonicalPlaneStreamError::Encoding(
                SemanticPlaneRecordError::OversizedRow { .. }
            ))
        ));

        let mut individually_valid_single_rows = Vec::new();
        for segment in &canonical.segments {
            let descriptor = segment.metadata().expect("canonical segment descriptor");
            let view = decode_semantic_plane_segment(kind, &descriptor, segment.bytes())
                .expect("canonical segment locally decodes");
            for record in view.records() {
                let mut bytes = Vec::new();
                begin_segment(kind, &mut bytes).expect("single-row header fits");
                let row_length = u32::try_from(record.payload().len()).expect("row length fits");
                bytes.extend_from_slice(&record.key());
                bytes.push(record.tag());
                bytes.extend_from_slice(&row_length.to_be_bytes());
                bytes.extend_from_slice(record.payload());
                bytes[7..11].copy_from_slice(&1_u32.to_be_bytes());
                individually_valid_single_rows.push(CanonicalSemanticPlaneSegmentPayload {
                    kind,
                    first_key: record.key(),
                    last_key: record.key(),
                    row_count: 1,
                    input,
                    bytes: bytes.into_boxed_slice(),
                });
            }
        }
        let split_descriptors = individually_valid_single_rows
            .iter()
            .map(|segment| segment.metadata().expect("single-row descriptor"))
            .collect::<Vec<_>>();
        let split_payloads = individually_valid_single_rows
            .iter()
            .map(|segment| segment.bytes())
            .collect::<Vec<_>>();
        for (descriptor, payload) in split_descriptors.iter().zip(&split_payloads) {
            decode_semantic_plane_segment(kind, descriptor, payload)
                .expect("every adversarial range split is individually valid");
        }
        assert!(matches!(
            verify_canonical_semantic_plane_segment_boundaries(
                kind,
                &split_descriptors,
                &split_payloads,
                policy,
                metrics.row_count(),
            ),
            Err(SemanticPlaneRecordError::NonCanonicalSegmentBoundary)
        ));
    }

    #[test]
    fn stable_key_ramp_hash_skew_falls_back_to_the_hard_byte_ceiling() {
        let family = SemanticIrPlane::Documentation;
        let policy = CanonicalPlaneSegmentBoundaryPolicy::stable_key_hash_ramp(256, 512, 512)
            .expect("stable key boundary policy is valid");
        let mut non_anchor_keys = Vec::new();
        for value in 0_u16..4096 {
            let mut key = [0_u8; 32];
            key[30..].copy_from_slice(&value.to_be_bytes());
            if super::super::segment_boundary_policy::stable_key_family_hash(family, &key) >> 60
                == 0xF
            {
                non_anchor_keys.push(key);
            }
        }
        assert!(non_anchor_keys.len() > 128);
        assert!(non_anchor_keys.windows(2).all(|pair| pair[0] < pair[1]));

        let frame_bytes = 120_usize;
        let mut segment_bytes = HEADER_BYTES;
        let mut segment_sizes = Vec::new();
        for key in &non_anchor_keys {
            let projected = segment_bytes + frame_bytes;
            let forced_cut = projected > policy.maximum_bytes();
            let anchor_cut = policy.cuts_before(family, segment_bytes, key);
            assert!(
                !anchor_cut,
                "crafted keys collide in the same non-anchor hash-prefix bucket"
            );
            if forced_cut {
                segment_sizes.push(segment_bytes);
                segment_bytes = HEADER_BYTES;
            }
            segment_bytes += frame_bytes;
            assert!(segment_bytes <= policy.maximum_bytes());
        }
        segment_sizes.push(segment_bytes);
        assert!(segment_sizes.len() > 32);
        assert!(
            segment_sizes
                .iter()
                .all(|size| *size <= policy.maximum_bytes())
        );
    }

    #[test]
    fn stable_boundary_hash_is_domain_separated_by_family_and_full_key() {
        let family = SemanticIrPlane::Documentation;
        let key = [0x5A; 32];
        let changed_key = [0x5B; 32];
        let documentation_hash =
            super::super::segment_boundary_policy::stable_key_family_hash(family, &key);
        assert_ne!(
            documentation_hash,
            super::super::segment_boundary_policy::stable_key_family_hash(
                SemanticIrPlane::Core,
                &key,
            )
        );
        assert_ne!(
            documentation_hash,
            super::super::segment_boundary_policy::stable_key_family_hash(family, &changed_key,)
        );
    }

    #[test]
    fn relation_and_occurrence_rows_are_coordinate_independent_and_preserve_multiplicity()
    -> Result<(), SemanticPlaneRecordError> {
        let forward = graph_image(false, false);
        let reordered = graph_image(true, false);
        let input = witness();
        let relation_segments = encode_canonical_plane_family(&forward, &RelationRows, input, 512)
            .expect("relation rows encode");
        let reordered_relations =
            encode_canonical_plane_family(&reordered, &RelationRows, input, 512)
                .expect("reordered relation rows encode");
        let occurrence_segments =
            encode_canonical_plane_family(&forward, &OccurrenceRows, input, 512)
                .expect("occurrence rows encode");
        let reordered_occurrences =
            encode_canonical_plane_family(&reordered, &OccurrenceRows, input, 512)
                .expect("reordered occurrence rows encode");

        assert_eq!(ids(&relation_segments)?, ids(&reordered_relations)?);
        assert_eq!(ids(&occurrence_segments)?, ids(&reordered_occurrences)?);
        assert_eq!(
            occurrence_segments
                .iter()
                .map(|segment| segment.row_count() as usize)
                .sum::<usize>(),
            3,
            "identical sites remain separate rows"
        );
        assert_eq!(
            relation_segments
                .iter()
                .map(|segment| segment.row_count() as usize)
                .sum::<usize>(),
            1,
            "same logical relation is deduplicated"
        );
        for segment in relation_segments.iter().chain(occurrence_segments.iter()) {
            let descriptor = segment.metadata().expect("segment descriptor");
            let decoded =
                decode_semantic_plane_segment(segment.kind(), &descriptor, segment.bytes())
                    .expect("strict family decoder accepts canonical rows");
            assert_eq!(decoded.row_count(), segment.row_count());
        }

        let relation = &relation_segments[0];
        let mut malformed_relation = relation.bytes().to_vec();
        malformed_relation[HEADER_BYTES + RECORD_HEADER_BYTES + 32] = 2;
        let malformed_relation_descriptor = SemanticPlaneSegment::from_payload_with_witness(
            relation.kind(),
            *relation.first_key(),
            *relation.last_key(),
            relation.row_count(),
            &malformed_relation,
            input,
        )
        .expect("recomputed malformed relation descriptor");
        assert!(matches!(
            decode_semantic_plane_segment(
                relation.kind(),
                &malformed_relation_descriptor,
                &malformed_relation
            ),
            Err(SemanticPlaneRecordError::RowGrammar)
        ));

        let occurrence = &occurrence_segments[0];
        let mut malformed_occurrence = occurrence.bytes().to_vec();
        malformed_occurrence[HEADER_BYTES + RECORD_HEADER_BYTES + 33] = 0;
        let malformed_occurrence_descriptor = SemanticPlaneSegment::from_payload_with_witness(
            occurrence.kind(),
            *occurrence.first_key(),
            *occurrence.last_key(),
            occurrence.row_count(),
            &malformed_occurrence,
            input,
        )
        .expect("recomputed malformed occurrence descriptor");
        assert!(matches!(
            decode_semantic_plane_segment(
                occurrence.kind(),
                &malformed_occurrence_descriptor,
                &malformed_occurrence
            ),
            Err(SemanticPlaneRecordError::RowGrammar)
        ));

        let edited = graph_image(false, true);
        let edited_relations = encode_canonical_plane_family(&edited, &RelationRows, input, 128)
            .expect("relations survive an occurrence-source edit");
        let base_occurrences = encode_canonical_plane_family(&forward, &OccurrenceRows, input, 128)
            .expect("small occurrence segments encode");
        let edited_occurrences =
            encode_canonical_plane_family(&edited, &OccurrenceRows, input, 128)
                .expect("edited occurrence segments encode");
        assert_eq!(ids(&relation_segments)?, ids(&edited_relations)?);
        let base_ids = ids(&base_occurrences)?;
        let edited_ids = ids(&edited_occurrences)?;
        assert_eq!(base_ids.len(), 3);
        assert_eq!(edited_ids.len(), 3);
        assert_eq!(
            base_ids
                .iter()
                .filter(|id| edited_ids.contains(*id))
                .count(),
            2,
            "the changed occurrence has its own segment while unrelated sites retain IDs"
        );
        Ok(())
    }

    #[test]
    fn relation_representative_source_uses_path_bytes_across_atom_reordering() {
        let first = representative_image(false);
        let second = representative_image(true);
        for image in [&first, &second] {
            let (_, relation) = image
                .canonical_links()
                .next()
                .expect("one canonical relation");
            let source = relation.source.expect("representative source retained");
            assert_eq!(
                image.atom(source.file()).expect("source path exists"),
                b"a.rs",
                "lexical source path wins even when its atom coordinate changes"
            );
        }
    }

    #[test]
    fn decoder_rejects_unknown_tag_malformed_utf8_and_forged_row_count()
    -> Result<(), SemanticPlaneRecordError> {
        let ir = image(1, None);
        let kind = SemanticPlaneKind::Ir(SemanticIrPlane::Documentation);
        let payload = &encode_canonical_plane_family(&ir, &DocumentationRows, witness(), 512)
            .expect("small docs row")[0];
        let descriptor = payload.metadata()?;

        let mut bad_tag = payload.bytes().to_vec();
        bad_tag[HEADER_BYTES + 32] = 0xFF;
        let bad_tag_descriptor = SemanticPlaneSegment::from_payload_with_witness(
            kind,
            *payload.first_key(),
            *payload.last_key(),
            payload.row_count(),
            &bad_tag,
            witness(),
        )?;
        assert!(matches!(
            decode_semantic_plane_segment(kind, &bad_tag_descriptor, &bad_tag),
            Err(SemanticPlaneRecordError::RowGrammar)
        ));
        let bad_tag_ref = CanonicalSemanticPlaneSegmentRef {
            kind,
            first_key: *payload.first_key(),
            last_key: *payload.last_key(),
            row_count: payload.row_count(),
            input: payload.input,
            bytes: &bad_tag,
        };
        assert!(matches!(
            bad_tag_ref.validate(),
            Err(SemanticPlaneRecordError::RowGrammar)
        ));

        let mut bad_utf8 = payload.bytes().to_vec();
        bad_utf8[HEADER_BYTES + RECORD_HEADER_BYTES + 42] = 0xFF;
        let bad_utf8_descriptor = SemanticPlaneSegment::from_payload_with_witness(
            kind,
            *payload.first_key(),
            *payload.last_key(),
            payload.row_count(),
            &bad_utf8,
            witness(),
        )?;
        assert!(matches!(
            decode_semantic_plane_segment(kind, &bad_utf8_descriptor, &bad_utf8),
            Err(SemanticPlaneRecordError::RowGrammar)
        ));

        let mut false_count = payload.bytes().to_vec();
        false_count[7..11].copy_from_slice(&2_u32.to_be_bytes());
        let false_count_descriptor = SemanticPlaneSegment::from_payload_with_witness(
            kind,
            *payload.first_key(),
            *payload.last_key(),
            2,
            &false_count,
            witness(),
        )?;
        assert!(matches!(
            decode_semantic_plane_segment(kind, &false_count_descriptor, &false_count),
            Err(SemanticPlaneRecordError::Truncated)
        ));
        assert!(decode_semantic_plane_segment(kind, &descriptor, payload.bytes()).is_ok());
        let validated = payload.validate()?;
        assert_eq!(validated.kind(), kind);
        assert_eq!(validated.row_count(), payload.row_count());
        assert_eq!(validated.bytes(), payload.bytes());
        assert_eq!(
            validated.id(),
            descriptor
                .admitted_id()
                .ok_or(SemanticPlaneRecordError::MissingAdmittedId)?
        );
        Ok(())
    }

    #[test]
    fn duplicate_stable_keys_fail_before_payload_emission() {
        let ir = image(1, None);
        assert!(matches!(
            encode_canonical_plane_family(&ir, &CollidingKeys, witness(), 512),
            Err(SemanticPlaneRecordError::StableKeyCollision)
        ));
    }

    #[test]
    fn unicode_documentation_round_trips_as_exact_typed_payload()
    -> Result<(), SemanticPlaneRecordError> {
        let ir = image(1, None);
        let kind = SemanticPlaneKind::Ir(SemanticIrPlane::Documentation);
        let rows = encode_canonical_plane_family(&ir, &DocumentationRows, witness(), 512)
            .expect("Unicode docs encode");
        let descriptor = rows[0].metadata()?;
        let view = decode_semantic_plane_segment(kind, &descriptor, rows[0].bytes())?;
        assert_eq!(view.row_count(), 1);
        assert_eq!(view.records().len(), 1);
        Ok(())
    }

    #[test]
    fn streamed_documentation_validator_checks_utf8_fragment_grammar() {
        let mut valid = Vec::from([0, 0, 0, 1, 0, 0, 0, 0, 4]);
        valid.extend_from_slice("🧠".as_bytes());
        let mut validator = declarations::DocsWireValidator::new();
        validator
            .write_all(&valid[..10])
            .expect("first block is accepted");
        validator
            .write_all(&valid[10..11])
            .expect("split code point is accepted");
        validator
            .write_all(&valid[11..])
            .expect("last block is accepted");
        validator
            .finish()
            .expect("complete canonical UTF-8 docs validate");

        let malformed = [0, 0, 0, 1, 0, 0, 0, 0, 1, 0xff];
        let mut validator = declarations::DocsWireValidator::new();
        validator
            .write_all(&malformed)
            .expect("validator drains malformed content");
        assert!(matches!(
            validator.finish(),
            Err(declarations::DocsWireValidationError::Grammar)
        ));

        let mut trailing = valid;
        trailing.push(0);
        let mut validator = declarations::DocsWireValidator::new();
        validator
            .write_all(&trailing)
            .expect("validator drains trailing bytes");
        assert!(matches!(
            validator.finish(),
            Err(declarations::DocsWireValidationError::Grammar)
        ));
    }

    #[test]
    fn low_policy_documentation_spills_only_above_its_exact_row_threshold() {
        let policy = CanonicalPlaneSegmentBoundaryPolicy::stable_key_hash_ramp(512, 1024, 4096)
            .expect("small family policy is valid");
        let kind = SemanticPlaneKind::Ir(SemanticIrPlane::Documentation);
        let inline_text_limit =
            policy.maximum_bytes() - (HEADER_BYTES + RECORD_HEADER_BYTES + 32 + 1 + 4 + 1 + 4);

        for (text_len, expected_tag) in [
            (inline_text_limit, declarations::DOCS_TAG),
            (inline_text_limit + 1, declarations::DOCS_JUMBO_TAG),
        ] {
            let text = "d".repeat(text_len);
            let ir = jumbo_docs_image(&text);
            let mut objects = InMemoryJumboObjects::default();
            let mut captured = CapturedFamily::default();
            let metrics = stream_canonical_plane_family_with_jumbo_and_stable_key_anchors(
                &ir,
                &DocumentationRows,
                witness(),
                policy,
                &mut objects,
                &mut captured,
            )
            .expect("policy-aware docs row fits or spills under its family maximum");
            assert_eq!(metrics.row_count(), 1);
            let (descriptor, payload) = &captured.rows[0];
            let view = decode_semantic_plane_segment_with_row_limit(
                kind,
                descriptor,
                payload,
                policy.maximum_bytes(),
            )
            .expect("policy-aware docs decoding uses the committed spill threshold");
            assert_eq!(
                view.records().next().expect("one docs row").tag(),
                expected_tag
            );

            let descriptors = [*descriptor];
            let payloads = [payload.as_slice()];
            verify_canonical_semantic_plane_segment_boundaries(
                kind,
                &descriptors,
                &payloads,
                policy,
                1,
            )
            .expect("family boundary proof decodes under the committed row threshold");

            if expected_tag == declarations::DOCS_JUMBO_TAG {
                assert!(matches!(
                    decode_semantic_plane_segment(kind, descriptor, payload),
                    Err(SemanticPlaneRecordError::RowGrammar)
                ));
                assert!(matches!(
                    verify_jumbo_plane_family_closures(kind, &descriptors, &payloads, &mut objects,),
                    Err(SemanticPlaneRecordError::RowGrammar)
                ));
            }
            let closure = verify_captured_jumbo_family_with_policy(
                SemanticIrPlane::Documentation,
                &captured,
                policy,
                &mut objects,
            );
            assert_eq!(
                closure.jumbo_value_count(),
                if expected_tag == declarations::DOCS_JUMBO_TAG {
                    1
                } else {
                    0
                }
            );
        }
    }

    #[test]
    fn low_policy_source_path_spills_only_above_its_exact_row_threshold() {
        let policy = CanonicalPlaneSegmentBoundaryPolicy::stable_key_hash_ramp(512, 1024, 4096)
            .expect("small family policy is valid");
        let kind = SemanticPlaneKind::Ir(SemanticIrPlane::SourceProvenance);
        let inline_path_limit =
            policy.maximum_bytes() - (HEADER_BYTES + RECORD_HEADER_BYTES + 32 + 1 + 4 + 8);

        for (path_len, expected_tag) in [
            (inline_path_limit, source_provenance::DECLARATION_SOURCE_TAG),
            (
                inline_path_limit + 1,
                source_provenance::DECLARATION_SOURCE_JUMBO_TAG,
            ),
        ] {
            let path = vec![0xa5; path_len];
            let ir = jumbo_source_image(&path);
            let mut objects = InMemoryJumboObjects::default();
            let mut captured = CapturedFamily::default();
            let metrics = stream_canonical_plane_family_with_jumbo_and_stable_key_anchors(
                &ir,
                &SourceProvenanceRows,
                witness(),
                policy,
                &mut objects,
                &mut captured,
            )
            .expect("policy-aware source row fits or spills under its family maximum");
            assert_eq!(metrics.row_count(), 1);
            let (descriptor, payload) = &captured.rows[0];
            let view = decode_semantic_plane_segment_with_row_limit(
                kind,
                descriptor,
                payload,
                policy.maximum_bytes(),
            )
            .expect("policy-aware source decoding uses the committed spill threshold");
            assert_eq!(
                view.records().next().expect("one source row").tag(),
                expected_tag
            );

            let descriptors = [*descriptor];
            let payloads = [payload.as_slice()];
            verify_canonical_semantic_plane_segment_boundaries(
                kind,
                &descriptors,
                &payloads,
                policy,
                1,
            )
            .expect("family boundary proof decodes under the committed row threshold");

            let mut row_objects = InMemoryJumboObjects::default();
            let mut row_sink = CapturedTypedRows::default();
            let row_metrics = stream_canonical_plane_family_rows_with_limits(
                &ir,
                &SourceProvenanceRows,
                policy.maximum_bytes(),
                policy.maximum_bytes(),
                4,
                16,
                64,
                crate::ir::JumboRopeLimits::default(),
                &mut row_objects,
                &mut row_sink,
            )
            .expect("source-provenance row stream honors the family spill threshold");
            assert_eq!(row_metrics.row_count(), 1);
            assert_eq!(
                row_metrics.peak_jumbo_rope_scratch_bytes() > 0,
                expected_tag == source_provenance::DECLARATION_SOURCE_JUMBO_TAG
            );
            let captured_row = row_sink.rows.first().expect("one borrowed source row");
            let decoded_row = view.records().next().expect("one decoded source row");
            assert_eq!(captured_row.0, SemanticIrPlane::SourceProvenance);
            assert_eq!(captured_row.1, decoded_row.key());
            assert_eq!(captured_row.2, expected_tag);
            assert_eq!(captured_row.2, decoded_row.tag());
            assert_eq!(captured_row.3.as_slice(), decoded_row.payload());

            if expected_tag == source_provenance::DECLARATION_SOURCE_JUMBO_TAG {
                assert!(matches!(
                    decode_semantic_plane_segment(kind, descriptor, payload),
                    Err(SemanticPlaneRecordError::RowGrammar)
                ));
                assert!(matches!(
                    verify_jumbo_plane_family_closures(kind, &descriptors, &payloads, &mut objects,),
                    Err(SemanticPlaneRecordError::RowGrammar)
                ));
                let row_view = CanonicalSemanticPlaneRecordView {
                    key: captured_row.1,
                    tag: captured_row.2,
                    payload: &captured_row.3,
                };
                let row_descriptor = source_provenance::jumbo_descriptor_for_record_with_row_limit(
                    row_view,
                    policy.maximum_bytes(),
                )
                .expect("row visitor jumbo descriptor is canonical under the policy")
                .expect("over-threshold path has a jumbo descriptor");
                assert_eq!(row_descriptor.byte_length(), path.len() as u64);
                let verified = row_descriptor
                    .admit_stored_closure(&mut row_objects)
                    .expect("row sink wrote an independently verifiable source closure");
                let mut row_path = Vec::new();
                verified
                    .write_value_to(&mut row_objects, &mut row_path)
                    .expect("row sink source closure reconstructs");
                assert_eq!(row_path, path);
            }
            let closure = verify_captured_jumbo_family_with_policy(
                SemanticIrPlane::SourceProvenance,
                &captured,
                policy,
                &mut objects,
            );
            assert_eq!(
                closure.jumbo_value_count(),
                if expected_tag == source_provenance::DECLARATION_SOURCE_JUMBO_TAG {
                    1
                } else {
                    0
                }
            );
        }
    }

    #[test]
    fn jumbo_documentation_is_one_bounded_row_and_reconstructs_cold() {
        let text = String::from("🧠").repeat(400_000);
        let ir = jumbo_docs_image(&text);
        assert!(matches!(
            encode_canonical_plane_family(
                &ir,
                &DocumentationRows,
                witness(),
                crate::ir::MAX_SEMANTIC_SEGMENT_BYTES,
            ),
            Err(SemanticPlaneRecordError::JumboObjectStoreRequired)
        ));

        let mut objects = InMemoryJumboObjects::default();
        let mut captured = CapturedFamily::default();
        let metrics = stream_canonical_plane_family_with_jumbo(
            &ir,
            &DocumentationRows,
            witness(),
            crate::ir::MAX_SEMANTIC_SEGMENT_BYTES,
            &mut objects,
            &mut captured,
        )
        .expect("jumbo docs encode as a typed row");
        assert_eq!(metrics.row_count(), 1);
        assert!(metrics.peak_row_scratch_capacity_bytes() <= 512);
        assert!(metrics.peak_jumbo_rope_scratch_bytes() > 0);
        assert!(
            metrics.peak_tracked_scratch_upper_bound_bytes()
                >= metrics.peak_jumbo_rope_scratch_bytes()
        );
        assert!(
            metrics.peak_tracked_scratch_upper_bound_bytes()
                <= (JUMBO_ROPE_MAX_LEAF_BYTES + 32 * 1024) as u64,
            "tracked row and rope scratch is bounded to one leaf plus fixed state"
        );
        assert_eq!(
            captured.rows.len(),
            1,
            "descriptor stays in one semantic row"
        );
        assert!(captured.maximum_payload_bytes < 512);
        assert!(captured.maximum_payload_bytes < crate::ir::MAX_SEMANTIC_SEGMENT_BYTES);

        let row_descriptor = captured.rows[0].0;
        let view = decode_semantic_plane_segment(
            SemanticPlaneKind::Ir(SemanticIrPlane::Documentation),
            &row_descriptor,
            &captured.rows[0].1,
        )
        .expect("typed documentation row decodes strictly");
        let record = view.records().next().expect("one documentation row");
        assert_eq!(record.tag(), declarations::DOCS_JUMBO_TAG);
        let descriptor = declarations::jumbo_descriptor_for_record(record)
            .expect("jumbo descriptor context is valid")
            .expect("large documentation value has a descriptor");
        assert_eq!(descriptor.byte_length(), (9 + text.len()) as u64);
        assert_eq!(
            descriptor.family(),
            crate::ir::JumboValueFamily::Documentation
        );
        assert_eq!(descriptor.encoding(), crate::ir::JumboValueEncoding::Bytes);

        let closure =
            verify_captured_jumbo_family(SemanticIrPlane::Documentation, &captured, &mut objects);
        assert_eq!(closure.jumbo_value_count(), 1);
        let verified = descriptor
            .admit_stored_closure(&mut objects)
            .expect("cold storage closure reopens");
        let mut reassembled = Vec::new();
        verified
            .write_value_to(&mut objects, &mut reassembled)
            .expect("complete canonical documentation value streams back");
        assert_eq!(
            u32::from_be_bytes(
                reassembled
                    .get(..4)
                    .expect("fragment count bytes exist")
                    .try_into()
                    .expect("fragment count is fixed width")
            ),
            1
        );
        assert_eq!(reassembled[4], 0);
        assert_eq!(
            u32::from_be_bytes(
                reassembled
                    .get(5..9)
                    .expect("text length bytes exist")
                    .try_into()
                    .expect("text length is fixed width")
            ) as usize,
            text.len()
        );
        assert_eq!(
            core::str::from_utf8(reassembled.get(9..).expect("text bytes exist"))
                .expect("reassembled documentation is UTF-8"),
            text
        );
    }

    #[test]
    fn jumbo_source_provenance_preserves_binary_paths_and_requires_full_cold_closure() {
        let path = vec![0xff; 1536 * 1024];
        let ir = jumbo_source_image(&path);
        let mut objects = InMemoryJumboObjects::default();
        let mut captured = CapturedFamily::default();
        let metrics = stream_canonical_plane_family_with_jumbo(
            &ir,
            &SourceProvenanceRows,
            witness(),
            crate::ir::MAX_SEMANTIC_SEGMENT_BYTES,
            &mut objects,
            &mut captured,
        )
        .expect("jumbo source path encodes as one typed row");
        assert_eq!(metrics.row_count(), 1);
        assert_eq!(captured.rows.len(), 1);
        assert!(captured.maximum_payload_bytes < 512);

        let row_descriptor = captured.rows[0].0;
        let view = decode_semantic_plane_segment(
            SemanticPlaneKind::Ir(SemanticIrPlane::SourceProvenance),
            &row_descriptor,
            &captured.rows[0].1,
        )
        .expect("typed source row decodes strictly");
        let record = view.records().next().expect("one source provenance row");
        let descriptor = source_provenance::jumbo_descriptor_for_record(record)
            .expect("source descriptor context is valid")
            .expect("large source path has a descriptor");
        assert_eq!(descriptor.byte_length(), path.len() as u64);
        assert_eq!(
            descriptor.family(),
            crate::ir::JumboValueFamily::SourceProvenance
        );
        assert_eq!(descriptor.encoding(), crate::ir::JumboValueEncoding::Bytes);
        let closure = verify_captured_jumbo_family(
            SemanticIrPlane::SourceProvenance,
            &captured,
            &mut objects,
        );
        assert_eq!(closure.jumbo_value_count(), 1);

        let verified = descriptor
            .admit_stored_closure(&mut objects)
            .expect("source path closure reopens from cold objects");
        let mut reassembled = Vec::new();
        verified
            .write_value_to(&mut objects, &mut reassembled)
            .expect("binary source path streams back exactly");
        assert_eq!(reassembled, path);

        let missing_leaf = *objects.leaf_order.first().expect("source has leaves");
        objects.leaves.remove(&missing_leaf);
        let descriptors: Vec<_> = captured.rows.iter().map(|(item, _)| *item).collect();
        let payloads: Vec<_> = captured
            .rows
            .iter()
            .map(|(_, payload)| payload.as_slice())
            .collect();
        assert!(
            verify_jumbo_plane_family_closures(
                SemanticPlaneKind::Ir(SemanticIrPlane::SourceProvenance),
                &descriptors,
                &payloads,
                &mut objects,
            )
            .is_err(),
            "a missing leaf prevents family closure proof publication"
        );
    }

    #[test]
    fn source_rows_preserve_exact_path_and_reject_reversed_span() {
        let version = EntityVersion {
            family: DeclarationFamilyId::from_raw([7; 16]),
            variant: VariantFingerprint::from_raw([8; 16]),
            core_payload: CorePayloadHash::from_raw([9; 16]),
        };
        let mut builder = IrBuilder::new();
        let file = builder.intern_atom(b"src/\xff.rs").expect("source atom");
        let source = SourceSpan::new(file, 3, 17).expect("valid half-open span");
        let item = TreeItemInput {
            name: b"source_item",
            anonymous_callable_anchor: None,
            kind: ItemKind::Function,
            visibility: Visibility::Public,
            authority: EntityAuthorityFacts {
                parentage: ParentageAuthority::Root,
                source: FactAvailability::Captured,
                source_file: FactAvailability::Captured,
                visibility: FactAvailability::Captured,
                ..EntityAuthorityFacts::default()
            },
            parent: None,
            semantic_type: None,
            members: &[],
            docs: &[],
            attributes: &[],
            source: Some(source),
            extension: None,
        };
        builder
            .add_borrowed_tree(BorrowedTree {
                versions: &[version],
                items: &[item],
                links: &[],
            })
            .expect("source tree");
        let ir = builder.finish().expect("source image");
        let kind = SemanticPlaneKind::Ir(SemanticIrPlane::SourceProvenance);
        let rows = encode_canonical_plane_family(&ir, &SourceProvenanceRows, witness(), 512)
            .expect("source rows");
        let row = &rows[0];
        let descriptor = row.metadata().expect("descriptor");
        let decoded = decode_semantic_plane_segment(kind, &descriptor, row.bytes())
            .expect("strict source decoder");
        let record = decoded.records().next().expect("one source row");
        assert!(
            record
                .payload()
                .windows(8)
                .any(|bytes| bytes == b"src/\xff.rs")
        );
        let mut invalid = row.bytes().to_vec();
        let last = invalid.len();
        invalid[last - 8..last - 4].copy_from_slice(&18_u32.to_be_bytes());
        let invalid_descriptor = SemanticPlaneSegment::from_payload_with_witness(
            kind,
            *row.first_key(),
            *row.last_key(),
            row.row_count(),
            &invalid,
            witness(),
        )
        .expect("self-consistent envelope");
        assert!(matches!(
            decode_semantic_plane_segment(kind, &invalid_descriptor, &invalid),
            Err(SemanticPlaneRecordError::RowGrammar)
        ));
    }

    #[test]
    fn documentation_edits_do_not_rewrite_source_provenance() -> Result<(), SemanticPlaneRecordError>
    {
        let base = image(128, None);
        let edited = image(128, Some(64));
        let before = encode_canonical_plane_family(&base, &SourceProvenanceRows, witness(), 512)
            .expect("base source rows");
        let after = encode_canonical_plane_family(&edited, &SourceProvenanceRows, witness(), 512)
            .expect("edited source rows");
        assert_eq!(ids(&before)?, ids(&after)?);
        let descriptors: Vec<_> = before
            .iter()
            .map(|segment| segment.metadata().expect("descriptor"))
            .collect();
        let payloads: Vec<_> = before.iter().map(|segment| segment.bytes()).collect();
        verify_semantic_plane_family_against_reader(
            &base,
            &SourceProvenanceRows,
            witness(),
            &descriptors,
            &payloads,
            512,
        )
        .expect("exact source family oracle");
        Ok(())
    }

    #[test]
    fn relation_representative_source_survives_atom_and_observation_reordering()
    -> Result<(), SemanticPlaneRecordError> {
        let first = representative_image(false);
        let second = representative_image(true);
        let before = encode_canonical_plane_family(&first, &SourceProvenanceRows, witness(), 512)
            .expect("first source family");
        let after = encode_canonical_plane_family(&second, &SourceProvenanceRows, witness(), 512)
            .expect("reordered source family");
        assert_eq!(ids(&before)?, ids(&after)?);
        assert_eq!(
            before
                .iter()
                .map(|segment| segment.row_count() as usize)
                .sum::<usize>(),
            3,
            "two declarations and their relation each own one source row"
        );
        let kind = SemanticPlaneKind::Ir(SemanticIrPlane::SourceProvenance);
        let records = before
            .iter()
            .map(|segment| {
                let descriptor = segment.metadata().expect("source descriptor");
                decode_semantic_plane_segment(kind, &descriptor, segment.bytes())
                    .expect("source row grammar")
                    .records()
                    .collect::<Vec<_>>()
            })
            .flatten()
            .collect::<Vec<_>>();
        assert_eq!(
            records.iter().filter(|record| record.tag() == 2).count(),
            1,
            "representative relation source cannot disappear behind occurrences"
        );
        Ok(())
    }

    fn core_fixture_version(index: usize) -> EntityVersion {
        let byte = u8::try_from(index + 1).expect("fixture row index fits u8");
        EntityVersion {
            family: DeclarationFamilyId::from_raw([byte; 16]),
            variant: VariantFingerprint::from_raw([byte.wrapping_add(64); 16]),
            core_payload: CorePayloadHash::from_raw([byte.wrapping_add(128); 16]),
        }
    }

    fn core_members_image(
        member_count: usize,
        replace_member: Option<usize>,
    ) -> (Ir, DeclarationIdentity) {
        let entity_count = member_count + usize::from(replace_member.is_some());
        let versions: Vec<_> = (0..=entity_count).map(core_fixture_version).collect();
        let root_identity = versions[0].identity();
        let mut member_ids: Vec<_> = (1..=member_count)
            .map(|index| TreeEntityId::new(u32::try_from(index).expect("fixture ID fits u32")))
            .collect();
        if let Some(index) = replace_member {
            assert!(index < member_ids.len());
            member_ids[index] =
                TreeEntityId::new(u32::try_from(entity_count).expect("fixture ID fits u32"));
        }
        let names: Vec<Vec<u8>> = (0..=entity_count)
            .map(|index| {
                if index == 0 {
                    b"TestRequests".to_vec()
                } else {
                    format!("member_{index:03}").into_bytes()
                }
            })
            .collect();
        let empty_members: [TreeEntityId; 0] = [];
        let authority = EntityAuthorityFacts {
            parentage: ParentageAuthority::Root,
            members: FactAvailability::Captured,
            documentation: FactAvailability::Captured,
            visibility: FactAvailability::Captured,
            attributes: FactAvailability::Captured,
            ..EntityAuthorityFacts::default()
        };
        let items: Vec<_> = (0..=entity_count)
            .map(|index| TreeItemInput {
                name: &names[index],
                kind: if index == 0 {
                    ItemKind::Record
                } else {
                    ItemKind::Function
                },
                visibility: Visibility::Public,
                authority: if index == 0 {
                    authority
                } else {
                    EntityAuthorityFacts {
                        parentage: ParentageAuthority::Bound(root_identity),
                        ..authority
                    }
                },
                parent: if index == 0 {
                    None
                } else {
                    Some(TreeEntityId::new(0))
                },
                semantic_type: None,
                members: if index == 0 {
                    &member_ids
                } else {
                    &empty_members
                },
                docs: &[],
                attributes: &[],
                source: None,
                extension: None,
            })
            .collect();
        let mut builder = IrBuilder::new();
        builder
            .add_borrowed_tree(BorrowedTree {
                versions: &versions,
                items: &items,
                links: &[],
            })
            .expect("Core member fixture is valid");
        (
            builder.finish().expect("Core member IR is valid"),
            root_identity,
        )
    }

    fn core_attribute_image(attribute: &[u8]) -> (Ir, DeclarationIdentity) {
        let versions = [core_fixture_version(0), core_fixture_version(1)];
        let root_identity = versions[0].identity();
        let members = [TreeEntityId::new(1)];
        let attributes: [&[u8]; 1] = [attribute];
        let empty_members: [TreeEntityId; 0] = [];
        let authority = EntityAuthorityFacts {
            parentage: ParentageAuthority::Root,
            members: FactAvailability::Captured,
            documentation: FactAvailability::Captured,
            visibility: FactAvailability::Captured,
            attributes: FactAvailability::Captured,
            ..EntityAuthorityFacts::default()
        };
        let items = [
            TreeItemInput {
                name: b"Package",
                kind: ItemKind::Module,
                visibility: Visibility::Public,
                authority,
                parent: None,
                semantic_type: None,
                members: &members,
                docs: &[],
                attributes: &[],
                source: None,
                extension: None,
            },
            TreeItemInput {
                name: b"test_normalized_versions",
                kind: ItemKind::Function,
                visibility: Visibility::Public,
                authority: EntityAuthorityFacts {
                    parentage: ParentageAuthority::Bound(root_identity),
                    ..authority
                },
                parent: Some(TreeEntityId::new(0)),
                semantic_type: None,
                members: &empty_members,
                docs: &[],
                attributes: &attributes,
                source: None,
                extension: None,
            },
        ];
        let mut builder = IrBuilder::new();
        builder
            .add_borrowed_tree(BorrowedTree {
                versions: &versions,
                items: &items,
                links: &[],
            })
            .expect("Core attribute fixture is valid");
        (
            builder.finish().expect("Core attribute IR is valid"),
            root_identity,
        )
    }

    fn core_row<'a>(
        captured: &'a CapturedFamily,
        key: [u8; 32],
        maximum: usize,
    ) -> (CanonicalSemanticPlaneRecordView<'a>, usize) {
        let kind = SemanticPlaneKind::Ir(SemanticIrPlane::Core);
        for (descriptor, payload) in &captured.rows {
            let view =
                decode_semantic_plane_segment_with_row_limit(kind, descriptor, payload, maximum)
                    .expect("captured Core segment is canonical");
            for record in view.records() {
                if record.key() == key {
                    let framed_size = HEADER_BYTES + RECORD_HEADER_BYTES + record.payload().len();
                    return (record, framed_size);
                }
            }
        }
        panic!("requested Core declaration key was not emitted")
    }

    fn core_capture(ir: &Ir, maximum: usize) -> (CapturedFamily, InMemoryJumboObjects) {
        let policy = CanonicalPlaneSegmentBoundaryPolicy::stable_key_hash_ramp(
            512,
            1024,
            u32::try_from(maximum).expect("fixture row limit fits u32"),
        )
        .expect("Core policy is valid");
        let mut captured = CapturedFamily::default();
        let mut objects = InMemoryJumboObjects::default();
        stream_canonical_plane_family_with_jumbo_and_stable_key_anchors(
            ir,
            &CoreDeclarationRows,
            witness(),
            policy,
            &mut objects,
            &mut captured,
        )
        .expect("Core rows fit or spill under the configured maximum");
        (captured, objects)
    }

    fn legacy_core_row_size(ir: &Ir, identity: DeclarationIdentity) -> usize {
        let rows = encode_canonical_plane_family(ir, &CoreDeclarationRows, witness(), 16_384)
            .expect("unspilled source image fits the diagnostic segment");
        let key = declaration_plane_key(SemanticPlaneKind::Ir(SemanticIrPlane::Core), identity);
        for row in rows {
            let descriptor = row.metadata().expect("Core segment descriptor");
            let view = decode_semantic_plane_segment(
                SemanticPlaneKind::Ir(SemanticIrPlane::Core),
                &descriptor,
                row.bytes(),
            )
            .expect("legacy Core rows remain readable");
            for record in view.records() {
                if record.key() == key {
                    assert_eq!(record.tag(), 1);
                    return HEADER_BYTES + RECORD_HEADER_BYTES + record.payload().len();
                }
            }
        }
        panic!("legacy Core row was not emitted")
    }

    fn jumbo_wire_value(
        descriptor: CheckedJumboValueDescriptor,
        objects: &mut InMemoryJumboObjects,
    ) -> Vec<u8> {
        let mut value = Vec::new();
        descriptor
            .admit_stored_closure_to(objects, &mut value)
            .expect("typed Core value closure is complete");
        value
    }

    fn identity_wire(identity: DeclarationIdentity) -> [u8; 32] {
        let mut bytes = [0; 32];
        bytes[..16].copy_from_slice(identity.family.as_bytes());
        bytes[16..].copy_from_slice(identity.variant.as_bytes());
        bytes
    }

    #[test]
    fn core_member_overflow_preserves_the_6037_byte_tag_one_row_losslessly() {
        let (ir, root_identity) = core_members_image(185, None);
        assert_eq!(legacy_core_row_size(&ir, root_identity), 6037);
        let (captured, mut objects) = core_capture(&ir, 4096);
        let key =
            declaration_plane_key(SemanticPlaneKind::Ir(SemanticIrPlane::Core), root_identity);
        let (record, jumbo_row_bytes) = core_row(&captured, key, 4096);
        assert_eq!(record.tag(), declarations::CORE_JUMBO_TAG);
        assert!(jumbo_row_bytes <= 4096);
        let fields = declarations::core_jumbo_descriptors_for_record_with_row_limit(record, 4096)
            .expect("Core field visitor accepts exact descriptors")
            .expect("overflow row has typed fields");
        assert!(fields.name.is_none());
        assert_eq!(
            fields.members.encoding(),
            JumboValueEncoding::CoreMemberIdentityList
        );
        assert_eq!(
            fields.attributes.encoding(),
            JumboValueEncoding::CoreAttributeList
        );
        assert_eq!(fields.members.byte_length(), 4 + 185 * 32);
        assert_eq!(fields.attributes.byte_length(), 4);

        let member_wire = jumbo_wire_value(fields.members, &mut objects);
        assert_eq!(
            u32::from_be_bytes(member_wire[..4].try_into().unwrap()),
            185
        );
        let decoded: Vec<[u8; 32]> = member_wire[4..]
            .chunks_exact(32)
            .map(|chunk| chunk.try_into().unwrap())
            .collect();
        let expected = (1..=185)
            .map(|index| identity_wire(core_fixture_version(index).identity()))
            .collect::<Vec<_>>();
        assert_eq!(decoded, expected);

        let policy = CanonicalPlaneSegmentBoundaryPolicy::stable_key_hash_ramp(512, 1024, 4096)
            .expect("Core policy is valid");
        let closure = verify_captured_jumbo_family_with_policy(
            SemanticIrPlane::Core,
            &captured,
            policy,
            &mut objects,
        );
        assert_eq!(closure.jumbo_value_count(), 2);
    }

    #[test]
    fn core_overflow_is_field_local_and_preserves_arbitrary_attribute_bytes() {
        let (base_ir, root_identity) = core_members_image(185, None);
        let (edited_ir, edited_identity) = core_members_image(185, Some(17));
        assert_eq!(root_identity, edited_identity);
        let (base, mut base_objects) = core_capture(&base_ir, 4096);
        let (edited, mut edited_objects) = core_capture(&edited_ir, 4096);
        let key =
            declaration_plane_key(SemanticPlaneKind::Ir(SemanticIrPlane::Core), root_identity);
        let (base_record, _) = core_row(&base, key, 4096);
        let (edited_record, _) = core_row(&edited, key, 4096);
        let base_fields =
            declarations::core_jumbo_descriptors_for_record_with_row_limit(base_record, 4096)
                .expect("base descriptors")
                .expect("base Core overflow");
        let edited_fields =
            declarations::core_jumbo_descriptors_for_record_with_row_limit(edited_record, 4096)
                .expect("edited descriptors")
                .expect("edited Core overflow");
        assert_ne!(base_fields.members.id(), edited_fields.members.id());
        assert_eq!(base_fields.attributes.id(), edited_fields.attributes.id());
        assert_eq!(
            jumbo_wire_value(base_fields.attributes, &mut base_objects),
            jumbo_wire_value(edited_fields.attributes, &mut edited_objects),
            "one member edit rewrites only the member-list value"
        );

        let invalid_utf8 = [0xf0, 0x9f, 0x8d, 0x89, 0xff];
        let mut wire = Vec::from([0, 0, 0, 1, 0, 0, 0, 0, 5]);
        wire.extend_from_slice(&invalid_utf8);
        let mut validator = declarations::CoreAttributesWireValidator::new();
        validator
            .write_all(&wire[..11])
            .expect("split value prefix accepted");
        validator
            .write_all(&wire[11..12])
            .expect("split multibyte byte accepted");
        validator
            .write_all(&wire[12..])
            .expect("arbitrary atom bytes accepted");
        validator
            .finish()
            .expect("Core attribute grammar does not impose UTF-8");
    }

    #[test]
    fn core_attribute_overflow_preserves_the_4314_byte_tag_one_row_and_raw_value() {
        let attribute = vec![b'a'; 4117];
        let (ir, _root_identity) = core_attribute_image(&attribute);
        let child_identity = core_fixture_version(1).identity();
        assert_eq!(legacy_core_row_size(&ir, child_identity), 4314);
        let (captured, mut objects) = core_capture(&ir, 4096);
        let key =
            declaration_plane_key(SemanticPlaneKind::Ir(SemanticIrPlane::Core), child_identity);
        let (record, row_size) = core_row(&captured, key, 4096);
        assert_eq!(record.tag(), declarations::CORE_JUMBO_TAG);
        assert!(row_size <= 4096);
        let fields = declarations::core_jumbo_descriptors_for_record_with_row_limit(record, 4096)
            .expect("Core descriptors")
            .expect("Core tag two");
        assert!(fields.name.is_none());
        assert_eq!(fields.members.byte_length(), 4);
        assert_eq!(fields.attributes.byte_length(), 4 + 4 + 4117);
        let attribute_wire = jumbo_wire_value(fields.attributes, &mut objects);
        assert_eq!(
            u32::from_be_bytes(attribute_wire[..4].try_into().unwrap()),
            1
        );
        assert_eq!(
            u32::from_be_bytes(attribute_wire[4..8].try_into().unwrap()),
            4117
        );
        assert_eq!(&attribute_wire[8..], attribute.as_slice());

        let policy = CanonicalPlaneSegmentBoundaryPolicy::stable_key_hash_ramp(512, 1024, 4096)
            .expect("Core policy is valid");
        let closure = verify_captured_jumbo_family_with_policy(
            SemanticIrPlane::Core,
            &captured,
            policy,
            &mut objects,
        );
        assert_eq!(closure.jumbo_value_count(), 2);
    }

    #[test]
    fn core_wire_validators_reject_partial_members_attributes_and_trailing_bytes() {
        for wire in [Vec::from([0, 0, 0, 1]), Vec::from([0, 0, 0, 1, 0])] {
            let mut validator = declarations::CoreMembersWireValidator::new(1, None);
            validator
                .write_all(&wire)
                .expect("member bytes are consumed");
            assert!(matches!(
                validator.finish(),
                Err(SemanticPlaneRecordError::RowGrammar)
            ));
        }
        let mut attributes = declarations::CoreAttributesWireValidator::new();
        attributes
            .write_all(&[0, 0, 0, 1, 0, 0, 0, 2, 0xff])
            .expect("attribute bytes are consumed");
        assert!(matches!(
            attributes.finish(),
            Err(SemanticPlaneRecordError::RowGrammar)
        ));
        let mut trailing = declarations::CoreMembersWireValidator::new(1, None);
        trailing
            .write_all(&[0, 0, 0, 0, 0])
            .expect("trailing bytes are consumed");
        assert!(matches!(
            trailing.finish(),
            Err(SemanticPlaneRecordError::RowGrammar)
        ));
    }

    fn core_named_image(name: &[u8]) -> (Ir, DeclarationIdentity) {
        let version = core_fixture_version(0);
        let item = TreeItemInput {
            name,
            kind: ItemKind::Function,
            visibility: Visibility::Public,
            authority: EntityAuthorityFacts {
                parentage: ParentageAuthority::Root,
                members: FactAvailability::Captured,
                documentation: FactAvailability::Captured,
                visibility: FactAvailability::Captured,
                attributes: FactAvailability::Captured,
                ..EntityAuthorityFacts::default()
            },
            parent: None,
            semantic_type: None,
            members: &[],
            docs: &[],
            attributes: &[],
            source: None,
            extension: None,
        };
        let mut builder = IrBuilder::new();
        builder
            .add_borrowed_tree(BorrowedTree {
                versions: &[version],
                items: &[item],
                links: &[],
            })
            .expect("Core name fixture is valid");
        (
            builder.finish().expect("Core name IR is valid"),
            version.identity(),
        )
    }

    #[test]
    fn core_name_uses_ordinal_zero_only_when_the_inline_tag_two_row_would_overflow() {
        let name = vec![0xff; 5000];
        let (ir, identity) = core_named_image(&name);
        assert!(legacy_core_row_size(&ir, identity) > 4096);
        let (captured, mut objects) = core_capture(&ir, 4096);
        let key = declaration_plane_key(SemanticPlaneKind::Ir(SemanticIrPlane::Core), identity);
        let (record, framed_size) = core_row(&captured, key, 4096);
        assert_eq!(record.tag(), declarations::CORE_JUMBO_TAG);
        assert!(framed_size <= 4096);
        let fields = declarations::core_jumbo_descriptors_for_record_with_row_limit(record, 4096)
            .expect("Core field visitor accepts the canonical large name")
            .expect("large Core row uses typed fields");
        let name_descriptor = fields.name.expect("large name has ordinal zero descriptor");
        assert_eq!(name_descriptor.encoding(), JumboValueEncoding::Bytes);
        assert_eq!(name_descriptor.byte_length(), name.len() as u64);
        assert_eq!(jumbo_wire_value(name_descriptor, &mut objects), name);

        let mut malformed = record.payload().to_vec();
        let member_offset = malformed.len() - 2 * crate::ir::JUMBO_VALUE_DESCRIPTOR_WIRE_BYTES;
        malformed[member_offset + 7..member_offset + 11].copy_from_slice(&99_u32.to_be_bytes());
        let kind = SemanticPlaneKind::Ir(SemanticIrPlane::Core);
        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"SPIR");
        bytes.extend_from_slice(&VERSION.to_be_bytes());
        bytes.push(ir_plane_code(kind).expect("Core plane code"));
        bytes.extend_from_slice(&1_u32.to_be_bytes());
        bytes.extend_from_slice(&key);
        bytes.push(record.tag());
        bytes.extend_from_slice(&(malformed.len() as u32).to_be_bytes());
        bytes.extend_from_slice(&malformed);
        let descriptor =
            SemanticPlaneSegment::from_payload_with_witness(kind, key, key, 1, &bytes, witness())
                .expect("malformed field context still has a self-consistent segment hash");
        assert!(matches!(
            decode_semantic_plane_segment_with_row_limit(
                SemanticPlaneKind::Ir(SemanticIrPlane::Core),
                &descriptor,
                &bytes,
                4096,
            ),
            Err(SemanticPlaneRecordError::RowGrammar)
        ));
    }
}
