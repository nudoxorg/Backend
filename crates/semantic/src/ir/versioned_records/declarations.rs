//! Canonical declaration and documentation record families for SPIR.

use alloc::{boxed::Box, vec::Vec};
use std::io::{self, Write};

use super::wire::{
    Cursor, encode_identity, put_bytes, put_u32, read_checked_jumbo_descriptor, read_identity,
    validate_jumbo_row_size,
};
use crate::ir::{
    CanonicalPlaneRowEncoder, CanonicalSemanticPlaneKeySink, CheckedJumboValueDescriptor,
    DeclarationIdentity, DocFragment, DocId, EntityAuthorityFacts, FactAvailability,
    JUMBO_VALUE_DESCRIPTOR_WIRE_BYTES, JumboRopeObjectSink, JumboRopeStreamWriter,
    JumboValueContext, JumboValueEncoding, JumboValueFamily, LinkTarget, ParentageAuthority,
    SemanticEntity, SemanticPlaneKind, SemanticPlaneRecordError, SemanticReader,
};

pub(super) const CORE_TAG: u8 = 1;
pub(super) const CORE_ANONYMOUS_CALLABLE_TAG: u8 = 2;
pub(super) const CORE_JUMBO_TAG: u8 = 3;
pub(super) const CORE_ANONYMOUS_CALLABLE_JUMBO_TAG: u8 = 4;
pub(super) const DOCS_TAG: u8 = 2;
pub(super) const DOCS_JUMBO_TAG: u8 = 3;

enum DocsTextAfter {
    Fragment,
    LinkTarget,
}

enum DocsWireState {
    FragmentCount {
        bytes: [u8; 4],
        used: usize,
    },
    FragmentTag,
    TextLength {
        bytes: [u8; 4],
        used: usize,
        after: DocsTextAfter,
    },
    TextBody {
        remaining: u32,
        utf8: crate::ir::jumbo_rope::Utf8Validator,
        after: DocsTextAfter,
    },
    LinkTargetKind,
    LinkTargetIdentity {
        kind: u8,
        bytes: [u8; 32],
        used: usize,
    },
    Done,
    Failed,
}

/// Incremental validator for a canonical documentation blob. Link identities
/// are retained only up to the caller's reference budget. Invalid streams are
/// drained by the writer and reported by `finish`, so the complete object
/// closure is still checked before a caller can mint its proof.
pub(super) struct DocsWireValidator {
    state: DocsWireState,
    fragments_remaining: u32,
    reference_limit: u64,
    reference_count: u64,
    local_references: Vec<[u8; 32]>,
    external_references: Vec<[u8; 32]>,
    allocation_error: Option<alloc::collections::TryReserveError>,
}

pub(crate) struct DocsWireReferences {
    pub(super) local: Vec<[u8; 32]>,
    pub(super) external: Vec<[u8; 32]>,
}

#[derive(Debug)]
pub(super) enum DocsWireValidationError {
    Grammar,
    ReferenceLimitExceeded,
    Allocation(alloc::collections::TryReserveError),
}

impl From<DocsWireValidationError> for SemanticPlaneRecordError {
    fn from(error: DocsWireValidationError) -> Self {
        match error {
            DocsWireValidationError::Grammar => Self::RowGrammar,
            DocsWireValidationError::ReferenceLimitExceeded => Self::JumboReferenceLimitExceeded,
            DocsWireValidationError::Allocation(error) => Self::Allocation(error),
        }
    }
}

impl DocsWireValidator {
    pub(super) const fn new() -> Self {
        Self::with_reference_limit(1_000_000)
    }

    pub(super) const fn with_reference_limit(reference_limit: u64) -> Self {
        Self {
            state: DocsWireState::FragmentCount {
                bytes: [0; 4],
                used: 0,
            },
            fragments_remaining: 0,
            reference_limit,
            reference_count: 0,
            local_references: Vec::new(),
            external_references: Vec::new(),
            allocation_error: None,
        }
    }

    pub(super) fn finish(self) -> Result<DocsWireReferences, DocsWireValidationError> {
        if !matches!(self.state, DocsWireState::Done) {
            return Err(DocsWireValidationError::Grammar);
        }
        if let Some(error) = self.allocation_error {
            return Err(DocsWireValidationError::Allocation(error));
        }
        if self.reference_count > self.reference_limit {
            return Err(DocsWireValidationError::ReferenceLimitExceeded);
        }
        Ok(DocsWireReferences {
            local: self.local_references,
            external: self.external_references,
        })
    }

    fn record_reference(&mut self, kind: u8, bytes: [u8; 32]) {
        self.reference_count = self.reference_count.saturating_add(1);
        if self.reference_count > self.reference_limit || self.allocation_error.is_some() {
            return;
        }
        let references = if kind == 0 {
            &mut self.local_references
        } else {
            &mut self.external_references
        };
        if let Err(error) = references.try_reserve(1) {
            self.allocation_error = Some(error);
            return;
        }
        references.push(bytes);
    }

    fn after_fragment(&mut self) -> DocsWireState {
        if self.fragments_remaining == 0 {
            return DocsWireState::Failed;
        }
        self.fragments_remaining -= 1;
        if self.fragments_remaining == 0 {
            DocsWireState::Done
        } else {
            DocsWireState::FragmentTag
        }
    }

    fn consume_byte(&mut self, byte: u8) {
        let state = core::mem::replace(&mut self.state, DocsWireState::Failed);
        self.state = match state {
            DocsWireState::FragmentCount { mut bytes, used } => {
                bytes[used] = byte;
                let used = used + 1;
                if used < bytes.len() {
                    DocsWireState::FragmentCount { bytes, used }
                } else {
                    self.fragments_remaining = u32::from_be_bytes(bytes);
                    if self.fragments_remaining == 0 {
                        DocsWireState::Done
                    } else {
                        DocsWireState::FragmentTag
                    }
                }
            }
            DocsWireState::FragmentTag => match byte {
                0 | 1 => DocsWireState::TextLength {
                    bytes: [0; 4],
                    used: 0,
                    after: DocsTextAfter::Fragment,
                },
                2 => DocsWireState::TextLength {
                    bytes: [0; 4],
                    used: 0,
                    after: DocsTextAfter::LinkTarget,
                },
                3 | 4 if self.fragments_remaining > 0 => self.after_fragment(),
                _ => DocsWireState::Failed,
            },
            DocsWireState::TextLength {
                mut bytes,
                used,
                after,
            } => {
                bytes[used] = byte;
                let used = used + 1;
                if used < bytes.len() {
                    DocsWireState::TextLength { bytes, used, after }
                } else {
                    let remaining = u32::from_be_bytes(bytes);
                    if remaining == 0 {
                        match after {
                            DocsTextAfter::Fragment => self.after_fragment(),
                            DocsTextAfter::LinkTarget => DocsWireState::LinkTargetKind,
                        }
                    } else {
                        DocsWireState::TextBody {
                            remaining,
                            utf8: crate::ir::jumbo_rope::Utf8Validator::default(),
                            after,
                        }
                    }
                }
            }
            DocsWireState::TextBody {
                remaining,
                mut utf8,
                after,
            } => {
                if utf8.push(byte).is_err() {
                    DocsWireState::Failed
                } else if remaining > 1 {
                    DocsWireState::TextBody {
                        remaining: remaining - 1,
                        utf8,
                        after,
                    }
                } else if utf8.finish().is_err() {
                    DocsWireState::Failed
                } else {
                    match after {
                        DocsTextAfter::Fragment => self.after_fragment(),
                        DocsTextAfter::LinkTarget => DocsWireState::LinkTargetKind,
                    }
                }
            }
            DocsWireState::LinkTargetKind if byte <= 1 => DocsWireState::LinkTargetIdentity {
                kind: byte,
                bytes: [0; 32],
                used: 0,
            },
            DocsWireState::LinkTargetIdentity {
                kind,
                mut bytes,
                used,
            } if used < bytes.len() => {
                bytes[used] = byte;
                if used + 1 == bytes.len() {
                    self.record_reference(kind, bytes);
                    self.after_fragment()
                } else {
                    DocsWireState::LinkTargetIdentity {
                        kind,
                        bytes,
                        used: used + 1,
                    }
                }
            }
            DocsWireState::Done | DocsWireState::Failed => DocsWireState::Failed,
            _ => DocsWireState::Failed,
        };
    }
}

impl Write for DocsWireValidator {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        for byte in bytes.iter().copied() {
            self.consume_byte(byte);
        }
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// Stable-key encoder for canonical declaration/core rows.
#[derive(Clone, Copy, Debug, Default)]
pub struct CoreDeclarationRows;

impl CanonicalPlaneRowEncoder for CoreDeclarationRows {
    type Handle = DeclarationIdentity;
    type Plan = ();

    fn kind(&self) -> SemanticPlaneKind {
        SemanticPlaneKind::Ir(crate::ir::SemanticIrPlane::Core)
    }

    fn build_plan<Reader: SemanticReader + ?Sized>(
        &self,
        _reader: &Reader,
    ) -> Result<Self::Plan, SemanticPlaneRecordError> {
        Ok(())
    }

    fn collect_keys<Reader: SemanticReader + ?Sized>(
        &self,
        reader: &Reader,
        _plan: &Self::Plan,
        sink: &mut CanonicalSemanticPlaneKeySink<Self::Handle>,
    ) -> Result<(), SemanticPlaneRecordError> {
        for entity in reader.canonical_entities() {
            let identity = entity.version.identity();
            sink.push(
                super::declaration_plane_key(self.kind(), identity),
                identity,
            )?;
        }
        Ok(())
    }

    fn encode_row<Reader: SemanticReader + ?Sized>(
        &self,
        reader: &Reader,
        _plan: &Self::Plan,
        identity: Self::Handle,
        out: &mut Vec<u8>,
    ) -> Result<u8, SemanticPlaneRecordError> {
        let row = reader
            .entity_by_identity(identity)
            .ok_or(SemanticPlaneRecordError::ReaderReference)?;
        encode_core_row(reader, row, out)
    }

    fn encode_row_with_jumbo_measured_for_segment_limit<Reader: SemanticReader + ?Sized>(
        &self,
        reader: &Reader,
        _plan: &Self::Plan,
        identity: Self::Handle,
        jumbo_sink: Option<&mut dyn JumboRopeObjectSink<Error = SemanticPlaneRecordError>>,
        maximum_segment_bytes: usize,
        jumbo_limits: crate::ir::JumboRopeLimits,
        peak_jumbo_scratch_bytes: &mut u64,
        out: &mut Vec<u8>,
    ) -> Result<u8, SemanticPlaneRecordError> {
        let row = reader
            .entity_by_identity(identity)
            .ok_or(SemanticPlaneRecordError::ReaderReference)?;
        let Some(sink) = jumbo_sink else {
            return encode_core_row(reader, row, out);
        };
        encode_core_row_with_jumbo(
            reader,
            row,
            sink,
            maximum_segment_bytes,
            jumbo_limits,
            peak_jumbo_scratch_bytes,
            out,
        )
    }
}

/// Stable-key encoder for canonical documentation rows.
#[derive(Clone, Copy, Debug, Default)]
pub struct DocumentationRows;

impl CanonicalPlaneRowEncoder for DocumentationRows {
    type Handle = DeclarationIdentity;
    type Plan = ();

    fn kind(&self) -> SemanticPlaneKind {
        SemanticPlaneKind::Ir(crate::ir::SemanticIrPlane::Documentation)
    }

    fn build_plan<Reader: SemanticReader + ?Sized>(
        &self,
        _reader: &Reader,
    ) -> Result<Self::Plan, SemanticPlaneRecordError> {
        Ok(())
    }

    fn collect_keys<Reader: SemanticReader + ?Sized>(
        &self,
        reader: &Reader,
        _plan: &Self::Plan,
        sink: &mut CanonicalSemanticPlaneKeySink<Self::Handle>,
    ) -> Result<(), SemanticPlaneRecordError> {
        for entity in reader.canonical_entities() {
            let identity = entity.version.identity();
            sink.push(
                super::declaration_plane_key(self.kind(), identity),
                identity,
            )?;
        }
        Ok(())
    }

    fn encode_row<Reader: SemanticReader + ?Sized>(
        &self,
        reader: &Reader,
        _plan: &Self::Plan,
        identity: Self::Handle,
        out: &mut Vec<u8>,
    ) -> Result<u8, SemanticPlaneRecordError> {
        let mut peak_jumbo_scratch_bytes = 0;
        encode_documentation_row(
            reader,
            identity,
            None,
            crate::ir::MAX_SEMANTIC_SEGMENT_BYTES,
            crate::ir::JumboRopeLimits::default(),
            &mut peak_jumbo_scratch_bytes,
            out,
        )
    }

    fn encode_row_with_jumbo<Reader: SemanticReader + ?Sized>(
        &self,
        reader: &Reader,
        _plan: &Self::Plan,
        identity: Self::Handle,
        jumbo_sink: Option<&mut dyn JumboRopeObjectSink<Error = SemanticPlaneRecordError>>,
        out: &mut Vec<u8>,
    ) -> Result<u8, SemanticPlaneRecordError> {
        let mut peak_jumbo_scratch_bytes = 0;
        encode_documentation_row(
            reader,
            identity,
            jumbo_sink,
            crate::ir::MAX_SEMANTIC_SEGMENT_BYTES,
            crate::ir::JumboRopeLimits::default(),
            &mut peak_jumbo_scratch_bytes,
            out,
        )
    }

    fn encode_row_with_jumbo_measured<Reader: SemanticReader + ?Sized>(
        &self,
        reader: &Reader,
        _plan: &Self::Plan,
        identity: Self::Handle,
        jumbo_sink: Option<&mut dyn JumboRopeObjectSink<Error = SemanticPlaneRecordError>>,
        peak_jumbo_scratch_bytes: &mut u64,
        out: &mut Vec<u8>,
    ) -> Result<u8, SemanticPlaneRecordError> {
        encode_documentation_row(
            reader,
            identity,
            jumbo_sink,
            crate::ir::MAX_SEMANTIC_SEGMENT_BYTES,
            crate::ir::JumboRopeLimits::default(),
            peak_jumbo_scratch_bytes,
            out,
        )
    }

    fn encode_row_with_jumbo_measured_for_segment_limit<Reader: SemanticReader + ?Sized>(
        &self,
        reader: &Reader,
        _plan: &Self::Plan,
        identity: Self::Handle,
        jumbo_sink: Option<&mut dyn JumboRopeObjectSink<Error = SemanticPlaneRecordError>>,
        maximum_segment_bytes: usize,
        jumbo_limits: crate::ir::JumboRopeLimits,
        peak_jumbo_scratch_bytes: &mut u64,
        out: &mut Vec<u8>,
    ) -> Result<u8, SemanticPlaneRecordError> {
        encode_documentation_row(
            reader,
            identity,
            jumbo_sink,
            maximum_segment_bytes,
            jumbo_limits,
            peak_jumbo_scratch_bytes,
            out,
        )
    }
}

fn encode_documentation_row<Reader: SemanticReader + ?Sized>(
    reader: &Reader,
    identity: DeclarationIdentity,
    jumbo_sink: Option<&mut dyn JumboRopeObjectSink<Error = SemanticPlaneRecordError>>,
    maximum_segment_bytes: usize,
    jumbo_limits: crate::ir::JumboRopeLimits,
    peak_jumbo_scratch_bytes: &mut u64,
    out: &mut Vec<u8>,
) -> Result<u8, SemanticPlaneRecordError> {
    let entity = reader
        .entity_by_identity(identity)
        .ok_or(SemanticPlaneRecordError::ReaderReference)?;
    let doc_bytes = documentation_value_length(reader, entity.docs)?;
    let row_overhead = u64::try_from(super::HEADER_BYTES + super::RECORD_HEADER_BYTES + 32 + 1)
        .map_err(|_| SemanticPlaneRecordError::RowTooLarge)?;
    let complete_row_bytes = row_overhead
        .checked_add(doc_bytes)
        .ok_or(SemanticPlaneRecordError::RowTooLarge)?;
    let spill_threshold = if jumbo_sink.is_some() {
        maximum_segment_bytes
    } else {
        crate::ir::MAX_SEMANTIC_SEGMENT_BYTES
    };
    if complete_row_bytes > spill_threshold as u64 {
        let sink = jumbo_sink.ok_or(SemanticPlaneRecordError::JumboObjectStoreRequired)?;
        let owner = super::declaration_plane_key(
            SemanticPlaneKind::Ir(crate::ir::SemanticIrPlane::Documentation),
            identity,
        );
        let context = JumboValueContext::new(
            owner,
            JumboValueFamily::Documentation,
            0,
            JumboValueEncoding::Bytes,
        );
        let mut writer = JumboRopeStreamWriter::new(context, jumbo_limits, sink)
            .map_err(super::map_jumbo_operation_error)?;
        visit_documentation_wire_parts(reader, entity.docs, |part| {
            writer.push(part).map_err(super::map_jumbo_operation_error)
        })?;
        let receipt = writer.finish().map_err(super::map_jumbo_operation_error)?;
        *peak_jumbo_scratch_bytes =
            (*peak_jumbo_scratch_bytes).max(receipt.metrics().peak_live_scratch_bytes());
        encode_identity(identity, out);
        out.push(availability(entity.authority.documentation));
        out.extend_from_slice(&receipt.verified().descriptor().encode_wire());
        return Ok(DOCS_JUMBO_TAG);
    }

    encode_identity(identity, out);
    out.push(availability(entity.authority.documentation));
    visit_documentation_wire_parts(reader, entity.docs, |part| {
        out.extend_from_slice(part);
        Ok(())
    })?;
    Ok(DOCS_TAG)
}

fn documentation_value_length<Reader: SemanticReader + ?Sized>(
    reader: &Reader,
    docs: DocId,
) -> Result<u64, SemanticPlaneRecordError> {
    let mut length = 0_u64;
    visit_documentation_wire_parts(reader, docs, |part| {
        length = length
            .checked_add(
                u64::try_from(part.len()).map_err(|_| SemanticPlaneRecordError::RowTooLarge)?,
            )
            .ok_or(SemanticPlaneRecordError::RowTooLarge)?;
        Ok(())
    })?;
    Ok(length)
}

fn visit_documentation_wire_parts<Reader, Emit>(
    reader: &Reader,
    docs_id: DocId,
    mut emit: Emit,
) -> Result<(), SemanticPlaneRecordError>
where
    Reader: SemanticReader + ?Sized,
    Emit: FnMut(&[u8]) -> Result<(), SemanticPlaneRecordError>,
{
    let docs = reader
        .docs(docs_id)
        .ok_or(SemanticPlaneRecordError::ReaderReference)?;
    let count = u32::try_from(docs.len()).map_err(|_| SemanticPlaneRecordError::RowTooLarge)?;
    emit(&count.to_be_bytes())?;
    for fragment in docs {
        match fragment {
            DocFragment::Text(text) => {
                emit(&[0])?;
                let value = reader
                    .text(text)
                    .ok_or(SemanticPlaneRecordError::ReaderReference)?;
                emit_text_parts(value, &mut emit)?;
            }
            DocFragment::Code(text) => {
                emit(&[1])?;
                let value = reader
                    .text(text)
                    .ok_or(SemanticPlaneRecordError::ReaderReference)?;
                emit_text_parts(value, &mut emit)?;
            }
            DocFragment::Link { label, target } => {
                emit(&[2])?;
                let value = reader
                    .text(label)
                    .ok_or(SemanticPlaneRecordError::ReaderReference)?;
                emit_text_parts(value, &mut emit)?;
                match target {
                    LinkTarget::Local(id) => {
                        emit(&[0])?;
                        let target = reader
                            .entity(id)
                            .ok_or(SemanticPlaneRecordError::ReaderReference)?;
                        let mut identity_bytes = [0_u8; 32];
                        let identity = target.version.identity();
                        identity_bytes[..16].copy_from_slice(identity.family.as_bytes());
                        identity_bytes[16..].copy_from_slice(identity.variant.as_bytes());
                        emit(&identity_bytes)?;
                    }
                    LinkTarget::External(id) => {
                        emit(&[1])?;
                        let target = crate::ir::ExternalTargetIdentity::capture(reader, id)
                            .map_err(|_| SemanticPlaneRecordError::ReaderReference)?;
                        emit(target.as_bytes())?;
                    }
                }
            }
            DocFragment::SoftBreak => emit(&[3])?,
            DocFragment::HardBreak => emit(&[4])?,
        }
    }
    Ok(())
}

fn emit_text_parts(
    value: &str,
    emit: &mut impl FnMut(&[u8]) -> Result<(), SemanticPlaneRecordError>,
) -> Result<(), SemanticPlaneRecordError> {
    let length = u32::try_from(value.len()).map_err(|_| SemanticPlaneRecordError::RowTooLarge)?;
    emit(&length.to_be_bytes())?;
    emit(value.as_bytes())
}

/// Encodes the two declaration-owned plane families from one complete reader.
pub fn encode_declaration_planes<Reader: SemanticReader + ?Sized>(
    reader: &Reader,
    input: crate::ir::SemanticInputWitness,
    maximum_bytes: usize,
) -> Result<
    (
        Box<[super::CanonicalSemanticPlaneSegmentPayload]>,
        Box<[super::CanonicalSemanticPlaneSegmentPayload]>,
    ),
    SemanticPlaneRecordError,
> {
    let core =
        super::encode_canonical_plane_family(reader, &CoreDeclarationRows, input, maximum_bytes)?;
    let docs =
        super::encode_canonical_plane_family(reader, &DocumentationRows, input, maximum_bytes)?;
    Ok((core, docs))
}

pub(super) fn validate_record_with_row_limit(
    kind: SemanticPlaneKind,
    key: [u8; 32],
    tag: u8,
    payload: &[u8],
    maximum_inline_row_bytes: usize,
) -> Result<(), SemanticPlaneRecordError> {
    let mut cursor = Cursor::new(payload);
    let identity = match (kind, tag) {
        (
            SemanticPlaneKind::Ir(crate::ir::SemanticIrPlane::Core),
            CORE_TAG | CORE_ANONYMOUS_CALLABLE_TAG,
        ) => {
            let identity = read_identity(&mut cursor)?;
            let name = cursor.bytes32()?; // Name bytes are not promised UTF-8.
            let kind = crate::ir::ItemKind::try_from(cursor.u16()?)
                .map_err(|_| SemanticPlaneRecordError::RowGrammar)?;
            if tag == CORE_ANONYMOUS_CALLABLE_TAG
                && (kind != crate::ir::ItemKind::Function
                    || !crate::ir::AnonymousCallableAnchorView::new(name).is_well_formed())
            {
                return Err(SemanticPlaneRecordError::RowGrammar);
            }
            if cursor.u8()? > 4 {
                return Err(SemanticPlaneRecordError::RowGrammar);
            }
            match cursor.u8()? {
                0 => {}
                1 => {
                    let _ = read_identity(&mut cursor)?;
                }
                _ => return Err(SemanticPlaneRecordError::RowGrammar),
            }
            match cursor.u8()? {
                0 => {}
                1 => {}
                2 => {
                    let _ = read_identity(&mut cursor)?;
                }
                3 => {
                    let _ = cursor.take(16)?;
                }
                _ => return Err(SemanticPlaneRecordError::RowGrammar),
            }
            for _ in 0..8 {
                if cursor.u8()? > 1 {
                    return Err(SemanticPlaneRecordError::RowGrammar);
                }
            }
            let members = cursor.u32()?;
            for _ in 0..members {
                let _ = read_identity(&mut cursor)?;
            }
            let attributes = cursor.u32()?;
            for _ in 0..attributes {
                let _ = cursor.bytes32()?;
            }
            identity
        }
        (
            SemanticPlaneKind::Ir(crate::ir::SemanticIrPlane::Core),
            CORE_JUMBO_TAG | CORE_ANONYMOUS_CALLABLE_JUMBO_TAG,
        ) => {
            let parsed = parse_core_jumbo_row(
                payload,
                maximum_inline_row_bytes,
                tag == CORE_ANONYMOUS_CALLABLE_JUMBO_TAG,
            )?;
            let _ = cursor.take(payload.len())?;
            parsed.identity
        }
        (SemanticPlaneKind::Ir(crate::ir::SemanticIrPlane::Documentation), DOCS_TAG) => {
            let identity = read_identity(&mut cursor)?;
            if cursor.u8()? > 1 {
                return Err(SemanticPlaneRecordError::RowGrammar);
            }
            let fragments = cursor.u32()?;
            for _ in 0..fragments {
                match cursor.u8()? {
                    0 | 1 => {
                        let _ = cursor.utf8()?;
                    }
                    2 => {
                        let _ = cursor.utf8()?;
                        match cursor.u8()? {
                            0 => {
                                let _ = read_identity(&mut cursor)?;
                            }
                            1 => {
                                let _ = cursor.take(32)?;
                            }
                            _ => return Err(SemanticPlaneRecordError::RowGrammar),
                        }
                    }
                    3 | 4 => {}
                    _ => return Err(SemanticPlaneRecordError::RowGrammar),
                }
            }
            identity
        }
        (SemanticPlaneKind::Ir(crate::ir::SemanticIrPlane::Documentation), DOCS_JUMBO_TAG) => {
            let identity = read_identity(&mut cursor)?;
            if cursor.u8()? > 1 {
                return Err(SemanticPlaneRecordError::RowGrammar);
            }
            let owner = super::declaration_plane_key(kind, identity);
            let descriptor = read_checked_jumbo_descriptor(
                &mut cursor,
                owner,
                JumboValueFamily::Documentation,
                0,
                JumboValueEncoding::Bytes,
            )?;
            validate_jumbo_row_size(
                &descriptor,
                super::HEADER_BYTES + super::RECORD_HEADER_BYTES + 32 + 1,
                maximum_inline_row_bytes,
            )?;
            identity
        }
        _ => return Err(SemanticPlaneRecordError::RowGrammar),
    };
    if !cursor.is_empty() {
        return Err(SemanticPlaneRecordError::RowTrailingBytes);
    }
    if super::declaration_plane_key(kind, identity) != key {
        return Err(SemanticPlaneRecordError::StableKeyMismatch);
    }
    Ok(())
}

/// The complete, closed jumbo field set carried by one Core overflow row.
/// This is the single schema visitor used by row validation, closure admission,
/// and the whole-manifest verifier.
#[derive(Clone, Copy)]
pub(super) struct CoreJumboValueDescriptors {
    pub(super) name: Option<CheckedJumboValueDescriptor>,
    pub(super) members: CheckedJumboValueDescriptor,
    pub(super) attributes: CheckedJumboValueDescriptor,
}

impl CoreJumboValueDescriptors {
    pub(super) fn iter(self) -> impl Iterator<Item = CheckedJumboValueDescriptor> {
        self.name.into_iter().chain([self.members, self.attributes])
    }
}

struct ParsedCoreJumboRow {
    identity: DeclarationIdentity,
    name: Option<CheckedJumboValueDescriptor>,
    members: CheckedJumboValueDescriptor,
    attributes: CheckedJumboValueDescriptor,
}

fn parse_core_jumbo_row(
    payload: &[u8],
    maximum_inline_row_bytes: usize,
    anonymous_callable: bool,
) -> Result<ParsedCoreJumboRow, SemanticPlaneRecordError> {
    let mut cursor = Cursor::new(payload);
    let identity = read_identity(&mut cursor)?;
    let owner = super::declaration_plane_key(
        SemanticPlaneKind::Ir(crate::ir::SemanticIrPlane::Core),
        identity,
    );
    let (name_length, name_is_descriptor, name_descriptor) = match cursor.u8()? {
        0 => {
            let name = cursor.bytes32()?;
            if anonymous_callable
                && !crate::ir::AnonymousCallableAnchorView::new(name).is_well_formed()
            {
                return Err(SemanticPlaneRecordError::RowGrammar);
            }
            (
                u64::try_from(name.len()).map_err(|_| SemanticPlaneRecordError::RowTooLarge)?,
                false,
                None,
            )
        }
        1 => {
            if anonymous_callable {
                return Err(SemanticPlaneRecordError::RowGrammar);
            }
            let descriptor = read_checked_jumbo_descriptor(
                &mut cursor,
                owner,
                JumboValueFamily::Core,
                CoreJumboField::Name.ordinal(),
                CoreJumboField::Name.encoding(),
            )?;
            (descriptor.byte_length(), true, Some(descriptor))
        }
        _ => return Err(SemanticPlaneRecordError::RowGrammar),
    };
    let kind = crate::ir::ItemKind::try_from(cursor.u16()?)
        .map_err(|_| SemanticPlaneRecordError::RowGrammar)?;
    if anonymous_callable && kind != crate::ir::ItemKind::Function {
        return Err(SemanticPlaneRecordError::RowGrammar);
    }
    if cursor.u8()? > 4 {
        return Err(SemanticPlaneRecordError::RowGrammar);
    }
    let parent_bytes = match cursor.u8()? {
        0 => 1_u64,
        1 => {
            let _ = read_identity(&mut cursor)?;
            33
        }
        _ => return Err(SemanticPlaneRecordError::RowGrammar),
    };
    let parentage_bytes = match cursor.u8()? {
        0 | 1 => 1_u64,
        2 => {
            let _ = read_identity(&mut cursor)?;
            33
        }
        3 => {
            let _ = cursor.take(16)?;
            17
        }
        _ => return Err(SemanticPlaneRecordError::RowGrammar),
    };
    for _ in 0..8 {
        if cursor.u8()? > 1 {
            return Err(SemanticPlaneRecordError::RowGrammar);
        }
    }
    let members = read_checked_jumbo_descriptor(
        &mut cursor,
        owner,
        JumboValueFamily::Core,
        CoreJumboField::Members.ordinal(),
        CoreJumboField::Members.encoding(),
    )?;
    let attributes = read_checked_jumbo_descriptor(
        &mut cursor,
        owner,
        JumboValueFamily::Core,
        CoreJumboField::Attributes.ordinal(),
        CoreJumboField::Attributes.encoding(),
    )?;
    if members.byte_length() < 4 || attributes.byte_length() < 4 {
        return Err(SemanticPlaneRecordError::RowGrammar);
    }
    if !cursor.is_empty() {
        return Err(SemanticPlaneRecordError::RowTrailingBytes);
    }
    let inline_framed_bytes = u64::try_from(super::HEADER_BYTES + super::RECORD_HEADER_BYTES)
        .map_err(|_| SemanticPlaneRecordError::RowTooLarge)?
        .checked_add(32 + 4)
        .and_then(|size| size.checked_add(name_length))
        .and_then(|size| size.checked_add(2 + 1))
        .and_then(|size| size.checked_add(parent_bytes))
        .and_then(|size| size.checked_add(parentage_bytes))
        .and_then(|size| size.checked_add(8 + members.byte_length() + attributes.byte_length()))
        .ok_or(SemanticPlaneRecordError::RowTooLarge)?;
    let jumbo_framed_bytes = u64::try_from(super::HEADER_BYTES + super::RECORD_HEADER_BYTES)
        .map_err(|_| SemanticPlaneRecordError::RowTooLarge)?
        .checked_add(
            u64::try_from(payload.len()).map_err(|_| SemanticPlaneRecordError::RowTooLarge)?,
        )
        .ok_or(SemanticPlaneRecordError::RowTooLarge)?;
    let maximum = u64::try_from(maximum_inline_row_bytes)
        .map_err(|_| SemanticPlaneRecordError::RowTooLarge)?;
    if inline_framed_bytes <= maximum || jumbo_framed_bytes > maximum {
        return Err(SemanticPlaneRecordError::RowGrammar);
    }
    let inline_name_framed_bytes = u64::try_from(super::HEADER_BYTES + super::RECORD_HEADER_BYTES)
        .map_err(|_| SemanticPlaneRecordError::RowTooLarge)?
        .checked_add(32 + 1 + 4)
        .and_then(|size| size.checked_add(name_length))
        .and_then(|size| size.checked_add(2 + 1 + parent_bytes + parentage_bytes + 8))
        .and_then(|size| size.checked_add(JUMBO_VALUE_DESCRIPTOR_WIRE_BYTES as u64 * 2))
        .ok_or(SemanticPlaneRecordError::RowTooLarge)?;
    if name_is_descriptor == (inline_name_framed_bytes <= maximum) {
        return Err(SemanticPlaneRecordError::RowGrammar);
    }
    if name_descriptor.is_some_and(|descriptor| descriptor.byte_length() != name_length) {
        return Err(SemanticPlaneRecordError::RowGrammar);
    }
    Ok(ParsedCoreJumboRow {
        identity,
        name: name_descriptor,
        members,
        attributes,
    })
}

#[derive(Clone, Copy)]
pub(super) enum CoreJumboField {
    Name,
    Members,
    Attributes,
}

impl CoreJumboField {
    const fn ordinal(self) -> u32 {
        match self {
            Self::Name => 0,
            Self::Members => 1,
            Self::Attributes => 2,
        }
    }

    const fn encoding(self) -> JumboValueEncoding {
        match self {
            Self::Name => JumboValueEncoding::Bytes,
            Self::Members => JumboValueEncoding::CoreMemberIdentityList,
            Self::Attributes => JumboValueEncoding::CoreAttributeList,
        }
    }
}

pub(super) fn core_jumbo_descriptors_for_record_with_row_limit(
    record: super::CanonicalSemanticPlaneRecordView<'_>,
    maximum_inline_row_bytes: usize,
) -> Result<Option<CoreJumboValueDescriptors>, SemanticPlaneRecordError> {
    if !matches!(record.tag(), CORE_JUMBO_TAG | CORE_ANONYMOUS_CALLABLE_JUMBO_TAG) {
        return Ok(None);
    }
    let parsed = parse_core_jumbo_row(
        record.payload(),
        maximum_inline_row_bytes,
        record.tag() == CORE_ANONYMOUS_CALLABLE_JUMBO_TAG,
    )?;
    if record.key()
        != super::declaration_plane_key(
            SemanticPlaneKind::Ir(crate::ir::SemanticIrPlane::Core),
            parsed.identity,
        )
    {
        return Err(SemanticPlaneRecordError::StableKeyMismatch);
    }
    Ok(Some(CoreJumboValueDescriptors {
        name: parsed.name,
        members: parsed.members,
        attributes: parsed.attributes,
    }))
}

/// Extracts a Docs jumbo descriptor under the legacy global spill threshold.
/// Policy-bound callers must use `jumbo_descriptor_for_record_with_row_limit`.
#[cfg(test)]
pub(super) fn jumbo_descriptor_for_record(
    record: super::CanonicalSemanticPlaneRecordView<'_>,
) -> Result<Option<CheckedJumboValueDescriptor>, SemanticPlaneRecordError> {
    jumbo_descriptor_for_record_with_row_limit(record, crate::ir::MAX_SEMANTIC_SEGMENT_BYTES)
}

pub(super) fn jumbo_descriptor_for_record_with_row_limit(
    record: super::CanonicalSemanticPlaneRecordView<'_>,
    maximum_inline_row_bytes: usize,
) -> Result<Option<CheckedJumboValueDescriptor>, SemanticPlaneRecordError> {
    if record.tag() != DOCS_JUMBO_TAG {
        return Ok(None);
    }
    let mut cursor = Cursor::new(record.payload());
    let identity = read_identity(&mut cursor)?;
    if cursor.u8()? > 1 {
        return Err(SemanticPlaneRecordError::RowGrammar);
    }
    let descriptor = read_checked_jumbo_descriptor(
        &mut cursor,
        super::declaration_plane_key(
            SemanticPlaneKind::Ir(crate::ir::SemanticIrPlane::Documentation),
            identity,
        ),
        JumboValueFamily::Documentation,
        0,
        JumboValueEncoding::Bytes,
    )?;
    validate_jumbo_row_size(
        &descriptor,
        super::HEADER_BYTES + super::RECORD_HEADER_BYTES + 32 + 1,
        maximum_inline_row_bytes,
    )?;
    if !cursor.is_empty() {
        return Err(SemanticPlaneRecordError::RowTrailingBytes);
    }
    if record.key()
        != super::declaration_plane_key(
            SemanticPlaneKind::Ir(crate::ir::SemanticIrPlane::Documentation),
            identity,
        )
    {
        return Err(SemanticPlaneRecordError::StableKeyMismatch);
    }
    Ok(Some(descriptor))
}

fn encode_core_row_with_jumbo<Reader: SemanticReader + ?Sized>(
    reader: &Reader,
    row: SemanticEntity,
    sink: &mut dyn JumboRopeObjectSink<Error = SemanticPlaneRecordError>,
    maximum_segment_bytes: usize,
    jumbo_limits: crate::ir::JumboRopeLimits,
    peak_jumbo_scratch_bytes: &mut u64,
    out: &mut Vec<u8>,
) -> Result<u8, SemanticPlaneRecordError> {
    let inline_framed_bytes = core_inline_framed_bytes(reader, row)?;
    let maximum =
        u64::try_from(maximum_segment_bytes).map_err(|_| SemanticPlaneRecordError::RowTooLarge)?;
    if inline_framed_bytes <= maximum {
        return encode_core_row(reader, row, out);
    }

    let name = reader
        .atom(row.name.atom())
        .ok_or(SemanticPlaneRecordError::ReaderReference)?;
    let inline_name_framed_bytes = core_jumbo_framed_bytes(row, name.len(), false)?;
    let name_is_descriptor = inline_name_framed_bytes > maximum;
    let jumbo_framed_bytes = if name_is_descriptor {
        core_jumbo_framed_bytes(row, name.len(), true)?
    } else {
        inline_name_framed_bytes
    };
    if jumbo_framed_bytes > maximum {
        return Err(SemanticPlaneRecordError::RowTooLarge);
    }

    let identity = row.version.identity();
    let owner = super::declaration_plane_key(
        SemanticPlaneKind::Ir(crate::ir::SemanticIrPlane::Core),
        identity,
    );
    encode_identity(identity, out);
    if name_is_descriptor {
        out.push(1);
        let descriptor = stream_core_jumbo_value(
            sink,
            jumbo_limits,
            peak_jumbo_scratch_bytes,
            JumboValueContext::new(
                owner,
                JumboValueFamily::Core,
                CoreJumboField::Name.ordinal(),
                CoreJumboField::Name.encoding(),
            ),
            |push| push(name),
        )?;
        out.extend_from_slice(&descriptor.encode_wire());
    } else {
        out.push(0);
        put_bytes(out, name)?;
    }
    out.extend_from_slice(&u16::from(row.kind).to_be_bytes());
    out.push(row.visibility as u8);
    match row.parent {
        None => out.push(0),
        Some(parent) => {
            out.push(1);
            let parent = reader
                .entity(parent)
                .ok_or(SemanticPlaneRecordError::ReaderReference)?;
            encode_identity(parent.version.identity(), out);
        }
    }
    match row.authority.parentage {
        ParentageAuthority::Unavailable => out.push(0),
        ParentageAuthority::Root => out.push(1),
        ParentageAuthority::Bound(parent) => {
            out.push(2);
            encode_identity(parent, out);
        }
        ParentageAuthority::UnrepresentedAuthorityOwner(owner) => {
            out.push(3);
            out.extend_from_slice(&owner.as_bytes());
        }
    }
    encode_authority(row.authority, out);

    let descriptor = stream_core_jumbo_value(
        sink,
        jumbo_limits,
        peak_jumbo_scratch_bytes,
        JumboValueContext::new(
            owner,
            JumboValueFamily::Core,
            CoreJumboField::Members.ordinal(),
            CoreJumboField::Members.encoding(),
        ),
        |push| visit_core_member_wire_parts(reader, row, push),
    )?;
    out.extend_from_slice(&descriptor.encode_wire());
    let descriptor = stream_core_jumbo_value(
        sink,
        jumbo_limits,
        peak_jumbo_scratch_bytes,
        JumboValueContext::new(
            owner,
            JumboValueFamily::Core,
            CoreJumboField::Attributes.ordinal(),
            CoreJumboField::Attributes.encoding(),
        ),
        |push| visit_core_attribute_wire_parts(reader, row, push),
    )?;
    out.extend_from_slice(&descriptor.encode_wire());
    Ok(match row.name {
        crate::ir::ItemName::Named(_) => CORE_JUMBO_TAG,
        crate::ir::ItemName::AnonymousCallable(_) => CORE_ANONYMOUS_CALLABLE_JUMBO_TAG,
    })
}

fn core_jumbo_framed_bytes(
    row: SemanticEntity,
    name_length: usize,
    name_is_descriptor: bool,
) -> Result<u64, SemanticPlaneRecordError> {
    let parent_bytes = if row.parent.is_some() { 33_u64 } else { 1 };
    let parentage_bytes = match row.authority.parentage {
        ParentageAuthority::Unavailable | ParentageAuthority::Root => 1_u64,
        ParentageAuthority::Bound(_) => 33,
        ParentageAuthority::UnrepresentedAuthorityOwner(_) => 17,
    };
    let name_bytes = if name_is_descriptor {
        1_u64 + JUMBO_VALUE_DESCRIPTOR_WIRE_BYTES as u64
    } else {
        1_u64 + 4 + u64::try_from(name_length).map_err(|_| SemanticPlaneRecordError::RowTooLarge)?
    };
    u64::try_from(super::HEADER_BYTES + super::RECORD_HEADER_BYTES)
        .map_err(|_| SemanticPlaneRecordError::RowTooLarge)?
        .checked_add(32 + name_bytes + 2 + 1 + parent_bytes + parentage_bytes + 8)
        .and_then(|bytes| bytes.checked_add(JUMBO_VALUE_DESCRIPTOR_WIRE_BYTES as u64 * 2))
        .ok_or(SemanticPlaneRecordError::RowTooLarge)
}

fn core_inline_framed_bytes<Reader: SemanticReader + ?Sized>(
    reader: &Reader,
    row: SemanticEntity,
) -> Result<u64, SemanticPlaneRecordError> {
    let name = reader
        .atom(row.name.atom())
        .ok_or(SemanticPlaneRecordError::ReaderReference)?;
    let members = reader
        .entity_list(row.members)
        .ok_or(SemanticPlaneRecordError::ReaderReference)?;
    let attributes = reader
        .atom_list(row.attributes)
        .ok_or(SemanticPlaneRecordError::ReaderReference)?;
    let mut bytes = u64::try_from(super::HEADER_BYTES + super::RECORD_HEADER_BYTES)
        .map_err(|_| SemanticPlaneRecordError::RowTooLarge)?
        .checked_add(32 + 4)
        .and_then(|bytes| bytes.checked_add(u64::try_from(name.len()).ok()?))
        .and_then(|bytes| bytes.checked_add(2 + 1 + 8 + 4 + 4))
        .and_then(|bytes| bytes.checked_add(if row.parent.is_some() { 33 } else { 1 }))
        .and_then(|bytes| {
            bytes.checked_add(match row.authority.parentage {
                ParentageAuthority::Unavailable | ParentageAuthority::Root => 1,
                ParentageAuthority::Bound(_) => 33,
                ParentageAuthority::UnrepresentedAuthorityOwner(_) => 17,
            })
        })
        .and_then(|bytes| bytes.checked_add(u64::try_from(members.len()).ok()?.checked_mul(32)?))
        .ok_or(SemanticPlaneRecordError::RowTooLarge)?;
    for attribute in attributes {
        let value = reader
            .atom(attribute)
            .ok_or(SemanticPlaneRecordError::ReaderReference)?;
        bytes = bytes
            .checked_add(4)
            .and_then(|bytes| bytes.checked_add(u64::try_from(value.len()).ok()?))
            .ok_or(SemanticPlaneRecordError::RowTooLarge)?;
    }
    Ok(bytes)
}

fn stream_core_jumbo_value<Emit>(
    sink: &mut dyn JumboRopeObjectSink<Error = SemanticPlaneRecordError>,
    limits: crate::ir::JumboRopeLimits,
    peak_scratch: &mut u64,
    context: JumboValueContext,
    emit: Emit,
) -> Result<CheckedJumboValueDescriptor, SemanticPlaneRecordError>
where
    Emit: FnOnce(
        &mut dyn FnMut(&[u8]) -> Result<(), SemanticPlaneRecordError>,
    ) -> Result<(), SemanticPlaneRecordError>,
{
    let mut writer = JumboRopeStreamWriter::new(context, limits, sink)
        .map_err(super::map_jumbo_operation_error)?;
    let mut push = |bytes: &[u8]| writer.push(bytes).map_err(super::map_jumbo_operation_error);
    emit(&mut push)?;
    drop(push);
    let receipt = writer.finish().map_err(super::map_jumbo_operation_error)?;
    *peak_scratch = (*peak_scratch).max(receipt.metrics().peak_live_scratch_bytes());
    Ok(*receipt.verified().descriptor())
}

fn visit_core_member_wire_parts<Reader: SemanticReader + ?Sized>(
    reader: &Reader,
    row: SemanticEntity,
    emit: &mut dyn FnMut(&[u8]) -> Result<(), SemanticPlaneRecordError>,
) -> Result<(), SemanticPlaneRecordError> {
    let members = reader
        .entity_list(row.members)
        .ok_or(SemanticPlaneRecordError::ReaderReference)?;
    let count = u32::try_from(members.len()).map_err(|_| SemanticPlaneRecordError::RowTooLarge)?;
    emit(&count.to_be_bytes())?;
    for member in members {
        let member = reader
            .entity(member)
            .ok_or(SemanticPlaneRecordError::ReaderReference)?;
        let identity = member.version.identity();
        emit(identity.family.as_bytes())?;
        emit(identity.variant.as_bytes())?;
    }
    Ok(())
}

fn visit_core_attribute_wire_parts<Reader: SemanticReader + ?Sized>(
    reader: &Reader,
    row: SemanticEntity,
    emit: &mut dyn FnMut(&[u8]) -> Result<(), SemanticPlaneRecordError>,
) -> Result<(), SemanticPlaneRecordError> {
    let attributes = reader
        .atom_list(row.attributes)
        .ok_or(SemanticPlaneRecordError::ReaderReference)?;
    let count =
        u32::try_from(attributes.len()).map_err(|_| SemanticPlaneRecordError::RowTooLarge)?;
    emit(&count.to_be_bytes())?;
    for attribute in attributes {
        let value = reader
            .atom(attribute)
            .ok_or(SemanticPlaneRecordError::ReaderReference)?;
        let length =
            u32::try_from(value.len()).map_err(|_| SemanticPlaneRecordError::RowTooLarge)?;
        emit(&length.to_be_bytes())?;
        emit(value)?;
    }
    Ok(())
}

enum CoreMembersWireState {
    Count {
        bytes: [u8; 4],
        used: usize,
    },
    Identity {
        remaining: u32,
        bytes: [u8; 32],
        used: usize,
    },
    Done,
    Failed,
}

/// Bounded parser for the typed Core member-list value. When requested, exact
/// identities are retained under the caller's shared aggregate reference cap.
pub(super) struct CoreMembersWireValidator<'references> {
    state: CoreMembersWireState,
    reference_limit: u64,
    reference_count: u64,
    references: Option<&'references mut Vec<[u8; 32]>>,
    allocation_error: Option<alloc::collections::TryReserveError>,
    failed: bool,
}

impl<'references> CoreMembersWireValidator<'references> {
    pub(super) const fn new(
        reference_limit: u64,
        references: Option<&'references mut Vec<[u8; 32]>>,
    ) -> Self {
        Self {
            state: CoreMembersWireState::Count {
                bytes: [0; 4],
                used: 0,
            },
            reference_limit,
            reference_count: 0,
            references,
            allocation_error: None,
            failed: false,
        }
    }

    pub(super) fn finish(self) -> Result<u64, SemanticPlaneRecordError> {
        if !matches!(self.state, CoreMembersWireState::Done) || self.failed {
            return Err(SemanticPlaneRecordError::RowGrammar);
        }
        if let Some(error) = self.allocation_error {
            return Err(SemanticPlaneRecordError::Allocation(error));
        }
        if self.reference_count > self.reference_limit {
            return Err(SemanticPlaneRecordError::JumboReferenceLimitExceeded);
        }
        Ok(self.reference_count)
    }

    fn consume_byte(&mut self, byte: u8) {
        self.state = match core::mem::replace(&mut self.state, CoreMembersWireState::Failed) {
            CoreMembersWireState::Count {
                mut bytes,
                mut used,
            } => {
                bytes[used] = byte;
                used += 1;
                if used == bytes.len() {
                    let remaining = u32::from_be_bytes(bytes);
                    if remaining == 0 {
                        CoreMembersWireState::Done
                    } else {
                        CoreMembersWireState::Identity {
                            remaining,
                            bytes: [0; 32],
                            used: 0,
                        }
                    }
                } else {
                    CoreMembersWireState::Count { bytes, used }
                }
            }
            CoreMembersWireState::Identity {
                remaining,
                mut bytes,
                mut used,
            } => {
                bytes[used] = byte;
                used += 1;
                if used == bytes.len() {
                    let next_count = self.reference_count.checked_add(1);
                    if let Some(next_count) = next_count {
                        self.reference_count = next_count;
                        if next_count <= self.reference_limit {
                            if let Some(references) = self.references.as_deref_mut() {
                                match references.try_reserve(1) {
                                    Ok(()) => references.push(bytes),
                                    Err(error) => self.allocation_error = Some(error),
                                }
                            }
                        }
                    } else {
                        self.failed = true;
                    }
                    if remaining == 1 {
                        CoreMembersWireState::Done
                    } else {
                        CoreMembersWireState::Identity {
                            remaining: remaining - 1,
                            bytes: [0; 32],
                            used: 0,
                        }
                    }
                } else {
                    CoreMembersWireState::Identity {
                        remaining,
                        bytes,
                        used,
                    }
                }
            }
            CoreMembersWireState::Done | CoreMembersWireState::Failed => {
                self.failed = true;
                CoreMembersWireState::Failed
            }
        };
    }
}

impl Write for CoreMembersWireValidator<'_> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        for byte in bytes.iter().copied() {
            self.consume_byte(byte);
        }
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

enum CoreAttributesWireState {
    Count {
        bytes: [u8; 4],
        used: usize,
    },
    Length {
        remaining_attributes: u32,
        bytes: [u8; 4],
        used: usize,
    },
    Body {
        remaining_attributes: u32,
        remaining_bytes: u32,
    },
    Done,
    Failed,
}

/// Bounded parser for the typed Core attribute-list value. Atom bytes remain
/// arbitrary bytes, matching the existing tag-1 Core grammar.
pub(super) struct CoreAttributesWireValidator {
    state: CoreAttributesWireState,
    failed: bool,
}

impl CoreAttributesWireValidator {
    pub(super) const fn new() -> Self {
        Self {
            state: CoreAttributesWireState::Count {
                bytes: [0; 4],
                used: 0,
            },
            failed: false,
        }
    }

    pub(super) fn finish(self) -> Result<(), SemanticPlaneRecordError> {
        if matches!(self.state, CoreAttributesWireState::Done) && !self.failed {
            Ok(())
        } else {
            Err(SemanticPlaneRecordError::RowGrammar)
        }
    }

    fn consume_byte(&mut self, byte: u8) {
        self.state = match core::mem::replace(&mut self.state, CoreAttributesWireState::Failed) {
            CoreAttributesWireState::Count {
                mut bytes,
                mut used,
            } => {
                bytes[used] = byte;
                used += 1;
                if used == bytes.len() {
                    let count = u32::from_be_bytes(bytes);
                    if count == 0 {
                        CoreAttributesWireState::Done
                    } else {
                        CoreAttributesWireState::Length {
                            remaining_attributes: count,
                            bytes: [0; 4],
                            used: 0,
                        }
                    }
                } else {
                    CoreAttributesWireState::Count { bytes, used }
                }
            }
            CoreAttributesWireState::Length {
                remaining_attributes,
                mut bytes,
                mut used,
            } => {
                bytes[used] = byte;
                used += 1;
                if used == bytes.len() {
                    let length = u32::from_be_bytes(bytes);
                    if length == 0 {
                        if remaining_attributes == 1 {
                            CoreAttributesWireState::Done
                        } else {
                            CoreAttributesWireState::Length {
                                remaining_attributes: remaining_attributes - 1,
                                bytes: [0; 4],
                                used: 0,
                            }
                        }
                    } else {
                        CoreAttributesWireState::Body {
                            remaining_attributes,
                            remaining_bytes: length,
                        }
                    }
                } else {
                    CoreAttributesWireState::Length {
                        remaining_attributes,
                        bytes,
                        used,
                    }
                }
            }
            CoreAttributesWireState::Body {
                remaining_attributes,
                remaining_bytes,
            } => {
                if remaining_bytes == 1 {
                    if remaining_attributes == 1 {
                        CoreAttributesWireState::Done
                    } else {
                        CoreAttributesWireState::Length {
                            remaining_attributes: remaining_attributes - 1,
                            bytes: [0; 4],
                            used: 0,
                        }
                    }
                } else {
                    CoreAttributesWireState::Body {
                        remaining_attributes,
                        remaining_bytes: remaining_bytes - 1,
                    }
                }
            }
            CoreAttributesWireState::Done | CoreAttributesWireState::Failed => {
                self.failed = true;
                CoreAttributesWireState::Failed
            }
        };
    }
}

impl Write for CoreAttributesWireValidator {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        for byte in bytes.iter().copied() {
            self.consume_byte(byte);
        }
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

fn encode_core_row<Reader: SemanticReader + ?Sized>(
    reader: &Reader,
    row: SemanticEntity,
    out: &mut Vec<u8>,
) -> Result<u8, SemanticPlaneRecordError> {
    let identity = row.version.identity();
    encode_identity(identity, out);
    let (name_atom, tag) = match row.name {
        crate::ir::ItemName::Named(atom) => (atom, CORE_TAG),
        crate::ir::ItemName::AnonymousCallable(atom) => (atom, CORE_ANONYMOUS_CALLABLE_TAG),
    };
    put_bytes(
        out,
        reader
            .atom(name_atom)
            .ok_or(SemanticPlaneRecordError::ReaderReference)?,
    )?;
    out.extend_from_slice(&u16::from(row.kind).to_be_bytes());
    out.push(row.visibility as u8);
    match row.parent {
        None => out.push(0),
        Some(parent) => {
            out.push(1);
            let parent = reader
                .entity(parent)
                .ok_or(SemanticPlaneRecordError::ReaderReference)?;
            encode_identity(parent.version.identity(), out);
        }
    }
    match row.authority.parentage {
        ParentageAuthority::Unavailable => out.push(0),
        ParentageAuthority::Root => out.push(1),
        ParentageAuthority::Bound(parent) => {
            out.push(2);
            encode_identity(parent, out);
        }
        ParentageAuthority::UnrepresentedAuthorityOwner(owner) => {
            out.push(3);
            out.extend_from_slice(&owner.as_bytes());
        }
    }
    encode_authority(row.authority, out);
    let members = reader
        .entity_list(row.members)
        .ok_or(SemanticPlaneRecordError::ReaderReference)?;
    put_u32(out, members.len())?;
    for member in members {
        let member = reader
            .entity(member)
            .ok_or(SemanticPlaneRecordError::ReaderReference)?;
        encode_identity(member.version.identity(), out);
    }
    let attributes = reader
        .atom_list(row.attributes)
        .ok_or(SemanticPlaneRecordError::ReaderReference)?;
    put_u32(out, attributes.len())?;
    for attribute in attributes {
        put_bytes(
            out,
            reader
                .atom(attribute)
                .ok_or(SemanticPlaneRecordError::ReaderReference)?,
        )?;
    }
    Ok(tag)
}

fn encode_authority(authority: EntityAuthorityFacts, out: &mut Vec<u8>) {
    for value in [
        authority.source,
        authority.source_file,
        authority.members,
        authority.semantic_type,
        authority.documentation,
        authority.visibility,
        authority.attributes,
        authority.language_extension,
    ] {
        out.push(availability(value));
    }
}

const fn availability(value: FactAvailability) -> u8 {
    match value {
        FactAvailability::Captured => 1,
        FactAvailability::Unavailable => 0,
    }
}
