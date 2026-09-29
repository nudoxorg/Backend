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

use thiserror::Error;

use crate::ir::{
    DeclarationIdentity, JumboRopeObjectSink, JumboRopeObjectSource, SemanticInputWitness,
    SemanticIrPlane, SemanticPlaneKind, SemanticPlaneSegment, SemanticReader, SemanticSegmentId,
};

const MAGIC: [u8; 4] = *b"SPIR";
const VERSION: u16 = 2;
const HEADER_BYTES: usize = 4 + 2 + 1 + 4;
const RECORD_HEADER_BYTES: usize = 32 + 1 + 4;
const INITIAL_PREFIX_BITS: u16 = 8;

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
pub(super) use extensions::{
    LanguageExtensionFamilyValidationError,
    validate_language_extension_family_v2_with_limits_detailed,
};
pub use occurrences::{OccurrenceHandle, OccurrenceRows};
pub use relations::RelationRows;
pub use source_provenance::{SourceProvenanceHandle, SourceProvenanceRows};
pub use types::{
    CheckedTypesFamilyV2, TypedRecordPlan, TypesClosureSemantics, TypesFamilyVerificationLimitsV2,
    TypesReferenceV2, TypesRowDomainV2, TypesRowHandle, TypesRows, validate_types_family_v2,
    validate_types_family_v2_with_limits,
};
pub(super) use types::{TypesFamilyValidationError, validate_types_family_v2_with_limits_detailed};

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
    rows: Vec<CanonicalSemanticPlaneRowKey<Handle>>,
}

impl<Handle: Copy> CanonicalSemanticPlaneKeySink<Handle> {
    /// Starts collecting keys for one exact IR plane.
    #[must_use]
    pub fn new(kind: SemanticPlaneKind) -> Self {
        Self {
            kind,
            rows: Vec::new(),
        }
    }

    /// Adds one stable row key and its non-persisted reader handle.
    pub fn push(&mut self, key: [u8; 32], handle: Handle) -> Result<(), SemanticPlaneRecordError> {
        self.rows
            .try_reserve(1)
            .map_err(SemanticPlaneRecordError::Allocation)?;
        self.rows.push(CanonicalSemanticPlaneRowKey { key, handle });
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
        let descriptor = self.metadata()?;
        let view = decode_semantic_plane_segment_structure(self.kind, &descriptor, self.bytes)?;
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
    stream_canonical_plane_family_inner(reader, encoder, input, maximum_bytes, None, sink)
}

/// Encodes one complete row family while persisting jumbo field values through
/// the caller's existing object CAS. The typed descriptor remains one SPIR
/// semantic row; rope leaves and interior nodes are separate CAS objects.
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
        Some(jumbo_sink),
        sink,
    )
}

fn stream_canonical_plane_family_inner<Reader, Encoder, Sink>(
    reader: &Reader,
    encoder: &Encoder,
    input: SemanticInputWitness,
    maximum_bytes: usize,
    mut jumbo_sink: Option<&mut dyn JumboRopeObjectSink<Error = SemanticPlaneRecordError>>,
    sink: &mut Sink,
) -> Result<CanonicalPlaneEncodingMetrics, CanonicalPlaneStreamError<Sink::Error>>
where
    Reader: SemanticReader + ?Sized,
    Encoder: CanonicalPlaneRowEncoder + ?Sized,
    Sink: CanonicalSemanticPlaneSegmentSink + ?Sized,
{
    if !matches!(encoder.kind(), SemanticPlaneKind::Ir(_)) {
        return Err(SemanticPlaneRecordError::IrKindRequired.into());
    }
    if maximum_bytes == 0 || maximum_bytes > crate::ir::MAX_SEMANTIC_SEGMENT_BYTES {
        return Err(SemanticPlaneRecordError::InvalidByteCeiling {
            observed: maximum_bytes,
            maximum: crate::ir::MAX_SEMANTIC_SEGMENT_BYTES,
        }
        .into());
    }
    let plan = encoder.build_plan(reader)?;
    let mut keys = CanonicalSemanticPlaneKeySink::new(encoder.kind());
    encoder.collect_keys(reader, &plan, &mut keys)?;
    keys.rows
        .sort_unstable_by(|left, right| left.key.cmp(&right.key));
    if keys.rows.windows(2).any(|pair| pair[0].key == pair[1].key) {
        return Err(SemanticPlaneRecordError::StableKeyCollision.into());
    }
    let key_capacity = keys
        .rows
        .capacity()
        .checked_mul(core::mem::size_of::<
            CanonicalSemanticPlaneRowKey<Encoder::Handle>,
        >())
        .and_then(|bytes| u64::try_from(bytes).ok())
        .ok_or(SemanticPlaneRecordError::MetricsOverflow)?;
    let mut row_scratch = Vec::new();
    let mut segment_bytes = Vec::new();
    let mut current_prefix = None;
    let mut first_key = [0; 32];
    let mut last_key = [0; 32];
    let mut segment_rows = 0_u32;
    let mut row_count = 0_u64;
    let mut segment_count = 0_u64;
    let mut output_bytes = 0_u64;
    let mut peak_row_scratch = 0_usize;
    let mut peak_segment_scratch = 0_usize;
    let mut peak_jumbo_rope_scratch = 0_u64;
    let mut peak_tracked_scratch_upper_bound = 0_u64;
    for row in &keys.rows {
        let prefix = prefix_value(&row.key, INITIAL_PREFIX_BITS);
        if segment_rows > 0 && current_prefix != Some(prefix) {
            emit_stream_segment(
                encoder.kind(),
                input,
                first_key,
                last_key,
                segment_rows,
                &mut segment_bytes,
                sink,
            )?;
            account_segment(&segment_bytes, &mut segment_count, &mut output_bytes)?;
            segment_bytes.clear();
            segment_rows = 0;
        }
        current_prefix = Some(prefix);
        row_scratch.clear();
        let mut row_jumbo_scratch = 0_u64;
        let row_sink = jumbo_sink.as_mut().map(|sink| {
            &mut **sink as &mut dyn JumboRopeObjectSink<Error = SemanticPlaneRecordError>
        });
        let tag = encoder.encode_row_with_jumbo_measured(
            reader,
            &plan,
            row.handle,
            row_sink,
            &mut row_jumbo_scratch,
            &mut row_scratch,
        )?;
        peak_jumbo_rope_scratch = peak_jumbo_rope_scratch.max(row_jumbo_scratch);
        peak_tracked_scratch_upper_bound =
            peak_tracked_scratch_upper_bound.max(tracked_family_scratch_bytes(
                key_capacity,
                row_scratch.capacity(),
                segment_bytes.capacity(),
                row_jumbo_scratch,
            )?);
        let row_length =
            u32::try_from(row_scratch.len()).map_err(|_| SemanticPlaneRecordError::RowTooLarge)?;
        validate_record(encoder.kind(), row.key, tag, &row_scratch)?;
        let framed_row_length = RECORD_HEADER_BYTES
            .checked_add(row_scratch.len())
            .ok_or(SemanticPlaneRecordError::RowTooLarge)?;
        let projected = if segment_rows == 0 {
            HEADER_BYTES.checked_add(framed_row_length)
        } else {
            segment_bytes.len().checked_add(framed_row_length)
        }
        .ok_or(SemanticPlaneRecordError::RowTooLarge)?;
        if projected > maximum_bytes {
            if segment_rows == 0 {
                return Err(SemanticPlaneRecordError::OversizedRow {
                    observed: HEADER_BYTES.saturating_add(framed_row_length),
                    maximum: maximum_bytes,
                }
                .into());
            }
            emit_stream_segment(
                encoder.kind(),
                input,
                first_key,
                last_key,
                segment_rows,
                &mut segment_bytes,
                sink,
            )?;
            account_segment(&segment_bytes, &mut segment_count, &mut output_bytes)?;
            segment_bytes.clear();
            segment_rows = 0;
            let projected = HEADER_BYTES
                .checked_add(framed_row_length)
                .ok_or(SemanticPlaneRecordError::RowTooLarge)?;
            if projected > maximum_bytes {
                return Err(SemanticPlaneRecordError::OversizedRow {
                    observed: projected,
                    maximum: maximum_bytes,
                }
                .into());
            }
        }
        if segment_rows == 0 {
            first_key = row.key;
            begin_segment(encoder.kind(), &mut segment_bytes)?;
        }
        segment_bytes
            .try_reserve_exact(framed_row_length)
            .map_err(SemanticPlaneRecordError::Allocation)?;
        segment_bytes.extend_from_slice(&row.key);
        segment_bytes.push(tag);
        segment_bytes.extend_from_slice(&row_length.to_be_bytes());
        segment_bytes.extend_from_slice(&row_scratch);
        peak_tracked_scratch_upper_bound =
            peak_tracked_scratch_upper_bound.max(tracked_family_scratch_bytes(
                key_capacity,
                row_scratch.capacity(),
                segment_bytes.capacity(),
                0,
            )?);
        last_key = row.key;
        segment_rows = segment_rows
            .checked_add(1)
            .ok_or(SemanticPlaneRecordError::RowCount {
                expected: u32::MAX,
                observed: u32::MAX,
            })?;
        row_count = row_count
            .checked_add(1)
            .ok_or(SemanticPlaneRecordError::MetricsOverflow)?;
        peak_row_scratch = peak_row_scratch.max(row_scratch.capacity());
        peak_segment_scratch = peak_segment_scratch.max(segment_bytes.capacity());
    }
    if segment_rows > 0 {
        emit_stream_segment(
            encoder.kind(),
            input,
            first_key,
            last_key,
            segment_rows,
            &mut segment_bytes,
            sink,
        )?;
        account_segment(&segment_bytes, &mut segment_count, &mut output_bytes)?;
    }
    let row_scratch_capacity =
        u64::try_from(peak_row_scratch).map_err(|_| SemanticPlaneRecordError::MetricsOverflow)?;
    let segment_scratch_capacity = u64::try_from(peak_segment_scratch)
        .map_err(|_| SemanticPlaneRecordError::MetricsOverflow)?;
    Ok(CanonicalPlaneEncodingMetrics {
        row_count,
        segment_count,
        output_bytes,
        row_encode_calls: row_count,
        peak_row_scratch_capacity_bytes: row_scratch_capacity,
        peak_segment_scratch_capacity_bytes: segment_scratch_capacity,
        peak_jumbo_rope_scratch_bytes: peak_jumbo_rope_scratch,
        peak_tracked_scratch_upper_bound_bytes: peak_tracked_scratch_upper_bound,
        row_index_capacity_bytes: key_capacity,
    })
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
/// descriptor and exact content-derived ID.
pub fn decode_semantic_plane_segment<'bytes>(
    kind: SemanticPlaneKind,
    descriptor: &SemanticPlaneSegment,
    bytes: &'bytes [u8],
) -> Result<CanonicalSemanticPlaneSegmentView<'bytes>, SemanticPlaneRecordError> {
    if !matches!(kind, SemanticPlaneKind::Ir(_)) {
        return Err(SemanticPlaneRecordError::IrKindRequired);
    }
    descriptor.admit(kind, bytes)?;
    decode_semantic_plane_segment_structure(kind, descriptor, bytes)
}

/// Checks the row-plane grammar after the caller has established exact
/// content identity. Producer validation uses this helper because its
/// descriptor was just built from the same bytes; fetched/untrusted payloads
/// must enter through `decode_semantic_plane_segment` and re-admit the claim.
fn decode_semantic_plane_segment_structure<'bytes>(
    kind: SemanticPlaneKind,
    descriptor: &SemanticPlaneSegment,
    bytes: &'bytes [u8],
) -> Result<CanonicalSemanticPlaneSegmentView<'bytes>, SemanticPlaneRecordError> {
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
        validate_record(kind, row.key, row.tag, row.payload)?;
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
/// one fixed maximum-leaf buffer at a time.
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
        let view = decode_semantic_plane_segment(kind, descriptor, payload)?;
        hasher.update(segment_id.as_bytes());
        for record in view.records() {
            let jumbo = match family {
                SemanticIrPlane::Documentation => {
                    declarations::jumbo_descriptor_for_record(record)?
                }
                SemanticIrPlane::SourceProvenance => {
                    source_provenance::jumbo_descriptor_for_record(record)?
                }
                _ => None,
            };
            let Some(jumbo) = jumbo else {
                continue;
            };
            let verified = if family == SemanticIrPlane::Documentation {
                let mut validator = declarations::DocsWireValidator::new();
                let verified = jumbo
                    .admit_stored_closure_to(source, &mut validator)
                    .map_err(map_jumbo_source_error)?;
                validator.finish()?;
                verified
            } else {
                jumbo
                    .admit_stored_closure(source)
                    .map_err(map_jumbo_source_error)?
            };
            hasher.update(&record.key());
            hasher.update(verified.descriptor_id().as_bytes());
            jumbo_value_count = jumbo_value_count
                .checked_add(1)
                .ok_or(SemanticPlaneRecordError::MetricsOverflow)?;
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
    InvalidByteCeiling { observed: usize, maximum: usize },
    /// Two logical rows produced the same stable key.
    #[error("canonical plane rows have a stable-key collision")]
    StableKeyCollision,
    /// One typed row cannot fit below the selected segment byte ceiling.
    #[error("one canonical row needs {observed} bytes; segment ceiling is {maximum}")]
    OversizedRow { observed: usize, maximum: usize },
    /// A family row length cannot be represented in the canonical u32 cell.
    #[error("canonical row length exceeds the u32 wire limit")]
    RowTooLarge,
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
    /// A jumbo stream failed while serializing a canonical field.
    #[error("jumbo semantic value stream failed")]
    JumboStream,
    /// A streaming jumbo documentation row exceeded its bounded link census.
    #[error("jumbo documentation references exceed the aggregate verifier budget")]
    JumboReferenceLimitExceeded,
    /// Stable row key does not commit the typed identity in its payload.
    #[error("canonical row key does not match its typed identity")]
    StableKeyMismatch,
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
    RowCount { expected: u32, observed: u32 },
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
    SegmentCount { expected: usize, observed: usize },
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

fn validate_record(
    kind: SemanticPlaneKind,
    key: [u8; 32],
    tag: u8,
    payload: &[u8],
) -> Result<(), SemanticPlaneRecordError> {
    match kind {
        SemanticPlaneKind::Ir(SemanticIrPlane::Core | SemanticIrPlane::Documentation) => {
            declarations::validate_record(kind, key, tag, payload)
        }
        SemanticPlaneKind::Ir(SemanticIrPlane::Relations) => {
            relations::validate_record(key, tag, payload)
        }
        SemanticPlaneKind::Ir(SemanticIrPlane::Occurrences) => {
            occurrences::validate_record(key, tag, payload)
        }
        SemanticPlaneKind::Ir(SemanticIrPlane::SourceProvenance) => {
            source_provenance::validate_record(kind, key, tag, payload)
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
        BorrowedTree, Confidence, CorePayloadHash, DeclarationFamilyId, DocInput,
        EntityAuthorityFacts, EntityVersion, FactAvailability, Ir, IrBuilder, ItemKind, LinkKind,
        OccurrenceAuthorityFacts, ParentageAuthority, SemanticCoreReader, SemanticInputWitness,
        SemanticIrPlane, SemanticPlaneKind, SemanticPlaneSegment, SemanticReader,
        SemanticSegmentId, SourceSpan, TreeEntityId, TreeItemInput, TreeLinkInput, TreeLinkTarget,
        VariantFingerprint, Visibility,
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

    fn ids(values: &[CanonicalSemanticPlaneSegmentPayload]) -> Vec<SemanticSegmentId> {
        values
            .iter()
            .map(|value| value.metadata().unwrap().admitted_id().unwrap())
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

    #[test]
    fn docs_edits_at_start_middle_and_end_leave_unrelated_segment_ids_stable() {
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
            ids(&docs),
            "borrowed stream emits the same canonical payload IDs"
        );
        assert_eq!(streamed_metrics.output_bytes(), metrics.output_bytes());
        assert!(streaming_sink.maximum_payload_bytes <= 320);

        let descriptors: Vec<_> = docs
            .iter()
            .map(|segment| segment.metadata().unwrap())
            .collect();
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
            assert_eq!(ids(&core), ids(&next_core));
            let original = ids(&docs);
            let changed = ids(&next_docs);
            assert_eq!(original.len(), changed.len());
            assert_eq!(
                original.iter().filter(|id| changed.contains(*id)).count(),
                original.len() - 1,
                "only the one stable-key bucket holding declaration {edited} should change"
            );
        }
    }

    #[test]
    fn relation_and_occurrence_rows_are_coordinate_independent_and_preserve_multiplicity() {
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

        assert_eq!(ids(&relation_segments), ids(&reordered_relations));
        assert_eq!(ids(&occurrence_segments), ids(&reordered_occurrences));
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
        assert_eq!(ids(&relation_segments), ids(&edited_relations));
        let base_ids = ids(&base_occurrences);
        let edited_ids = ids(&edited_occurrences);
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
    fn decoder_rejects_unknown_tag_malformed_utf8_and_forged_row_count() {
        let ir = image(1, None);
        let kind = SemanticPlaneKind::Ir(SemanticIrPlane::Documentation);
        let payload = &encode_canonical_plane_family(&ir, &DocumentationRows, witness(), 512)
            .expect("small docs row")[0];
        let descriptor = payload.metadata().unwrap();

        let mut bad_tag = payload.bytes().to_vec();
        bad_tag[HEADER_BYTES + 32] = 0xFF;
        let bad_tag_descriptor = SemanticPlaneSegment::from_payload_with_witness(
            kind,
            *payload.first_key(),
            *payload.last_key(),
            payload.row_count(),
            &bad_tag,
            witness(),
        )
        .unwrap();
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
        )
        .unwrap();
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
        )
        .unwrap();
        assert!(matches!(
            decode_semantic_plane_segment(kind, &false_count_descriptor, &false_count),
            Err(SemanticPlaneRecordError::Truncated)
        ));
        assert!(decode_semantic_plane_segment(kind, &descriptor, payload.bytes()).is_ok());
        let validated = payload
            .validate()
            .expect("canonical local segment validates");
        assert_eq!(validated.kind(), kind);
        assert_eq!(validated.row_count(), payload.row_count());
        assert_eq!(validated.bytes(), payload.bytes());
        assert_eq!(
            validated.id(),
            descriptor.admitted_id().expect("descriptor hashes payload")
        );
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
    fn unicode_documentation_round_trips_as_exact_typed_payload() {
        let ir = image(1, None);
        let kind = SemanticPlaneKind::Ir(SemanticIrPlane::Documentation);
        let rows = encode_canonical_plane_family(&ir, &DocumentationRows, witness(), 512)
            .expect("Unicode docs encode");
        let descriptor = rows[0].metadata().unwrap();
        let view = decode_semantic_plane_segment(kind, &descriptor, rows[0].bytes())
            .expect("strict UTF-8 docs reopen");
        assert_eq!(view.row_count(), 1);
        assert_eq!(view.records().len(), 1);
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
    fn documentation_edits_do_not_rewrite_source_provenance() {
        let base = image(128, None);
        let edited = image(128, Some(64));
        let before = encode_canonical_plane_family(&base, &SourceProvenanceRows, witness(), 512)
            .expect("base source rows");
        let after = encode_canonical_plane_family(&edited, &SourceProvenanceRows, witness(), 512)
            .expect("edited source rows");
        assert_eq!(ids(&before), ids(&after));
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
    }

    #[test]
    fn relation_representative_source_survives_atom_and_observation_reordering() {
        let first = representative_image(false);
        let second = representative_image(true);
        let before = encode_canonical_plane_family(&first, &SourceProvenanceRows, witness(), 512)
            .expect("first source family");
        let after = encode_canonical_plane_family(&second, &SourceProvenanceRows, witness(), 512)
            .expect("reordered source family");
        assert_eq!(ids(&before), ids(&after));
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
    }
}
