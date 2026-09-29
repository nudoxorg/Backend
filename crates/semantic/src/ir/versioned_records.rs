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
    DeclarationIdentity, SemanticInputWitness, SemanticIrPlane, SemanticPlaneKind,
    SemanticPlaneSegment, SemanticReader,
};

const MAGIC: [u8; 4] = *b"SPIR";
const VERSION: u16 = 1;
const HEADER_BYTES: usize = 4 + 2 + 1 + 4;
const RECORD_HEADER_BYTES: usize = 32 + 1 + 4;
const INITIAL_PREFIX_BITS: u16 = 8;

mod declarations;
pub use declarations::{CoreDeclarationRows, DocumentationRows, encode_declaration_planes};

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

/// One segment borrowed only for the duration of a streaming sink call.
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
    /// Allocated capacity of the compact key/handle/length inventory.
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

/// Encodes a row family and lends each bounded output segment to `sink` before
/// reusing its segment buffer. The encoder retains only the sorted compact
/// key/handle index, one row scratch buffer, and one segment payload buffer.
/// Rows are encoded once; the sink can durably write each segment before the
/// next one is generated.
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
        return Err(SemanticPlaneRecordError::StableKeyCollision);
    }
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
        let tag = encoder.encode_row(reader, &plan, row.handle, &mut row_scratch)?;
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
    let key_capacity = keys
        .rows
        .capacity()
        .checked_mul(core::mem::size_of::<
            CanonicalSemanticPlaneRowKey<Encoder::Handle>,
        >())
        .and_then(|bytes| u64::try_from(bytes).ok())
        .ok_or(SemanticPlaneRecordError::MetricsOverflow)?;
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
        row_index_capacity_bytes: key_capacity,
    })
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
    use alloc::{format, string::String, vec::Vec};

    use crate::ir::{
        BorrowedTree, CorePayloadHash, DeclarationFamilyId, DocInput, EntityAuthorityFacts,
        EntityVersion, FactAvailability, Ir, IrBuilder, ItemKind, ParentageAuthority,
        SemanticInputWitness, SemanticIrPlane, SemanticPlaneKind, SemanticPlaneSegment,
        SemanticSegmentId, TreeItemInput, VariantFingerprint, Visibility,
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

    fn witness() -> SemanticInputWitness {
        SemanticInputWitness::claimed([0xA1; 32], ScopeRoot::from_bytes([0xB2; 32]))
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
}
