//! Coordinate-free source rows for the canonical semantic plane.
//!
//! Every declaration and canonical relation contributes exactly one row. The
//! rows commit captured source evidence or its explicit absence; a missing
//! row cannot masquerade as an unavailable source field to the family oracle.

use alloc::vec::Vec;

use super::{
    relations::relation_key,
    wire::{
        Cursor, encode_identity, put_bytes, read_checked_jumbo_descriptor, read_identity,
        validate_jumbo_row_size,
    },
};
use crate::ir::{
    CanonicalPlaneRowEncoder, CanonicalSemanticPlaneKeySink, CheckedJumboValueDescriptor,
    DeclarationIdentity, FactAvailability, JumboRopeObjectSink, JumboRopeStreamWriter,
    JumboValueContext, JumboValueEncoding, JumboValueFamily, LinkId, SemanticIrPlane,
    SemanticPlaneKind, SemanticPlaneRecordError, SemanticReader,
};

const DECLARATION_SOURCE_TAG: u8 = 1;
const RELATION_SOURCE_TAG: u8 = 2;
pub(super) const DECLARATION_SOURCE_JUMBO_TAG: u8 = 3;
pub(super) const RELATION_SOURCE_JUMBO_TAG: u8 = 4;
const RELATION_SOURCE_KEY_DOMAIN: &[u8] = b"backend.semantic.ir.relation-source-key.v1\0";

/// Reader-local handle for a declaration source or relation representative.
#[derive(Clone, Copy)]
pub enum SourceProvenanceHandle {
    /// Source evidence on one declaration.
    Declaration(DeclarationIdentity),
    /// Representative source evidence on one canonical graph relation.
    Relation(LinkId),
}

/// Stable-key producer for declaration and relation source provenance.
#[derive(Clone, Copy, Debug, Default)]
pub struct SourceProvenanceRows;

impl CanonicalPlaneRowEncoder for SourceProvenanceRows {
    type Handle = SourceProvenanceHandle;
    type Plan = ();

    fn kind(&self) -> SemanticPlaneKind {
        SemanticPlaneKind::Ir(SemanticIrPlane::SourceProvenance)
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
                SourceProvenanceHandle::Declaration(identity),
            )?;
        }
        for (id, link) in reader.canonical_links() {
            sink.push(
                relation_source_row_key(relation_key(reader, link)?),
                SourceProvenanceHandle::Relation(id),
            )?;
        }
        Ok(())
    }

    fn encode_row<Reader: SemanticReader + ?Sized>(
        &self,
        reader: &Reader,
        _plan: &Self::Plan,
        handle: Self::Handle,
        out: &mut Vec<u8>,
    ) -> Result<u8, SemanticPlaneRecordError> {
        let mut peak_jumbo_scratch_bytes = 0;
        encode_source_row(reader, handle, None, &mut peak_jumbo_scratch_bytes, out)
    }

    fn encode_row_with_jumbo<Reader: SemanticReader + ?Sized>(
        &self,
        reader: &Reader,
        _plan: &Self::Plan,
        handle: Self::Handle,
        jumbo_sink: Option<&mut dyn JumboRopeObjectSink<Error = SemanticPlaneRecordError>>,
        out: &mut Vec<u8>,
    ) -> Result<u8, SemanticPlaneRecordError> {
        let mut peak_jumbo_scratch_bytes = 0;
        encode_source_row(
            reader,
            handle,
            jumbo_sink,
            &mut peak_jumbo_scratch_bytes,
            out,
        )
    }

    fn encode_row_with_jumbo_measured<Reader: SemanticReader + ?Sized>(
        &self,
        reader: &Reader,
        _plan: &Self::Plan,
        handle: Self::Handle,
        jumbo_sink: Option<&mut dyn JumboRopeObjectSink<Error = SemanticPlaneRecordError>>,
        peak_jumbo_scratch_bytes: &mut u64,
        out: &mut Vec<u8>,
    ) -> Result<u8, SemanticPlaneRecordError> {
        encode_source_row(reader, handle, jumbo_sink, peak_jumbo_scratch_bytes, out)
    }
}

fn encode_source_row<Reader: SemanticReader + ?Sized>(
    reader: &Reader,
    handle: SourceProvenanceHandle,
    mut jumbo_sink: Option<&mut dyn JumboRopeObjectSink<Error = SemanticPlaneRecordError>>,
    peak_jumbo_scratch_bytes: &mut u64,
    out: &mut Vec<u8>,
) -> Result<u8, SemanticPlaneRecordError> {
    match handle {
        SourceProvenanceHandle::Declaration(identity) => {
            let entity = reader
                .entity_by_identity(identity)
                .ok_or(SemanticPlaneRecordError::ReaderReference)?;
            encode_identity(identity, out);
            if entity.authority.source != entity.authority.source_file {
                return Err(SemanticPlaneRecordError::RowGrammar);
            }
            match (entity.authority.source, entity.source) {
                (FactAvailability::Unavailable, None) => {
                    out.push(0);
                    Ok(DECLARATION_SOURCE_TAG)
                }
                (FactAvailability::Captured, Some(source)) => {
                    let path = reader
                        .atom(source.file())
                        .ok_or(SemanticPlaneRecordError::ReaderReference)?;
                    let owner = super::declaration_plane_key(
                        SemanticPlaneKind::Ir(SemanticIrPlane::SourceProvenance),
                        identity,
                    );
                    let jumbo = encode_source_value(
                        owner,
                        path,
                        source.start(),
                        source.end(),
                        jumbo_sink,
                        peak_jumbo_scratch_bytes,
                        out,
                    )?;
                    Ok(if jumbo {
                        DECLARATION_SOURCE_JUMBO_TAG
                    } else {
                        DECLARATION_SOURCE_TAG
                    })
                }
                _ => Err(SemanticPlaneRecordError::RowGrammar),
            }
        }
        SourceProvenanceHandle::Relation(id) => {
            let link = reader
                .link(id)
                .ok_or(SemanticPlaneRecordError::ReaderReference)?;
            let relation = relation_key(reader, link)?;
            let owner = relation_source_row_key(relation);
            out.extend_from_slice(&relation);
            match link.source {
                None => {
                    out.push(0);
                    Ok(RELATION_SOURCE_TAG)
                }
                Some(source) => {
                    let path = reader
                        .atom(source.file())
                        .ok_or(SemanticPlaneRecordError::ReaderReference)?;
                    let jumbo = encode_source_value(
                        owner,
                        path,
                        source.start(),
                        source.end(),
                        jumbo_sink,
                        peak_jumbo_scratch_bytes,
                        out,
                    )?;
                    Ok(if jumbo {
                        RELATION_SOURCE_JUMBO_TAG
                    } else {
                        RELATION_SOURCE_TAG
                    })
                }
            }
        }
    }
}

fn encode_source_value(
    owner: [u8; 32],
    path: &[u8],
    start: u32,
    end: u32,
    jumbo_sink: Option<&mut dyn JumboRopeObjectSink<Error = SemanticPlaneRecordError>>,
    peak_jumbo_scratch_bytes: &mut u64,
    out: &mut Vec<u8>,
) -> Result<bool, SemanticPlaneRecordError> {
    let row_overhead =
        u64::try_from(super::HEADER_BYTES + super::RECORD_HEADER_BYTES + 32 + 1 + 4 + 8)
            .map_err(|_| SemanticPlaneRecordError::RowTooLarge)?;
    let complete_row_bytes = row_overhead
        .checked_add(u64::try_from(path.len()).map_err(|_| SemanticPlaneRecordError::RowTooLarge)?)
        .ok_or(SemanticPlaneRecordError::RowTooLarge)?;
    if complete_row_bytes > crate::ir::MAX_SEMANTIC_SEGMENT_BYTES as u64 {
        let sink = jumbo_sink.ok_or(SemanticPlaneRecordError::JumboObjectStoreRequired)?;
        let context = JumboValueContext::new(
            owner,
            JumboValueFamily::SourceProvenance,
            0,
            JumboValueEncoding::Bytes,
        );
        let mut writer =
            JumboRopeStreamWriter::new(context, crate::ir::JumboRopeLimits::default(), sink)
                .map_err(super::map_jumbo_operation_error)?;
        writer
            .push(path)
            .map_err(super::map_jumbo_operation_error)?;
        let receipt = writer.finish().map_err(super::map_jumbo_operation_error)?;
        *peak_jumbo_scratch_bytes =
            (*peak_jumbo_scratch_bytes).max(receipt.metrics().peak_live_scratch_bytes());
        out.push(1);
        out.extend_from_slice(&receipt.verified().descriptor().encode_wire());
        out.extend_from_slice(&start.to_be_bytes());
        out.extend_from_slice(&end.to_be_bytes());
        return Ok(true);
    }
    out.push(1);
    put_bytes(out, path)?;
    out.extend_from_slice(&start.to_be_bytes());
    out.extend_from_slice(&end.to_be_bytes());
    Ok(false)
}

fn relation_source_row_key(relation: [u8; 32]) -> [u8; 32] {
    let mut hasher = blake3::Hasher::new();
    hasher.update(RELATION_SOURCE_KEY_DOMAIN);
    hasher.update(&relation);
    *hasher.finalize().as_bytes()
}

pub(super) fn validate_record(
    kind: SemanticPlaneKind,
    key: [u8; 32],
    tag: u8,
    payload: &[u8],
) -> Result<(), SemanticPlaneRecordError> {
    if kind != SemanticPlaneKind::Ir(SemanticIrPlane::SourceProvenance) {
        return Err(SemanticPlaneRecordError::RowGrammar);
    }
    let mut cursor = Cursor::new(payload);
    let expected_key = match tag {
        DECLARATION_SOURCE_TAG | DECLARATION_SOURCE_JUMBO_TAG => {
            let identity = read_identity(&mut cursor)?;
            super::declaration_plane_key(kind, identity)
        }
        RELATION_SOURCE_TAG | RELATION_SOURCE_JUMBO_TAG => {
            let relation: [u8; 32] = cursor
                .take(32)?
                .try_into()
                .map_err(|_| SemanticPlaneRecordError::Truncated)?;
            relation_source_row_key(relation)
        }
        _ => return Err(SemanticPlaneRecordError::RowGrammar),
    };
    if matches!(
        tag,
        DECLARATION_SOURCE_JUMBO_TAG | RELATION_SOURCE_JUMBO_TAG
    ) {
        if cursor.u8()? != 1 {
            return Err(SemanticPlaneRecordError::RowGrammar);
        }
        let descriptor = read_checked_jumbo_descriptor(
            &mut cursor,
            expected_key,
            crate::ir::JumboValueFamily::SourceProvenance,
            0,
            crate::ir::JumboValueEncoding::Bytes,
        )?;
        validate_jumbo_row_size(
            &descriptor,
            super::HEADER_BYTES + super::RECORD_HEADER_BYTES + 32 + 1 + 4 + 8,
        )?;
        let start = cursor.u32()?;
        let end = cursor.u32()?;
        if start > end {
            return Err(SemanticPlaneRecordError::RowGrammar);
        }
    } else {
        match cursor.u8()? {
            0 => {}
            1 => {
                let _source_file = cursor.bytes32()?;
                let start = cursor.u32()?;
                let end = cursor.u32()?;
                if start > end {
                    return Err(SemanticPlaneRecordError::RowGrammar);
                }
            }
            _ => return Err(SemanticPlaneRecordError::RowGrammar),
        }
    }
    if !cursor.is_empty() {
        return Err(SemanticPlaneRecordError::RowTrailingBytes);
    }
    if expected_key != key {
        return Err(SemanticPlaneRecordError::StableKeyMismatch);
    }
    Ok(())
}

pub(super) fn jumbo_descriptor_for_record(
    record: super::CanonicalSemanticPlaneRecordView<'_>,
) -> Result<Option<CheckedJumboValueDescriptor>, SemanticPlaneRecordError> {
    if !matches!(
        record.tag(),
        DECLARATION_SOURCE_JUMBO_TAG | RELATION_SOURCE_JUMBO_TAG
    ) {
        return Ok(None);
    }
    let mut cursor = Cursor::new(record.payload());
    let _row_owner = cursor.take(32)?;
    if cursor.u8()? != 1 {
        return Err(SemanticPlaneRecordError::RowGrammar);
    }
    let descriptor = read_checked_jumbo_descriptor(
        &mut cursor,
        record.key(),
        crate::ir::JumboValueFamily::SourceProvenance,
        0,
        crate::ir::JumboValueEncoding::Bytes,
    )?;
    validate_jumbo_row_size(
        &descriptor,
        super::HEADER_BYTES + super::RECORD_HEADER_BYTES + 32 + 1 + 4 + 8,
    )?;
    let start = cursor.u32()?;
    let end = cursor.u32()?;
    if start > end {
        return Err(SemanticPlaneRecordError::RowGrammar);
    }
    if !cursor.is_empty() {
        return Err(SemanticPlaneRecordError::RowTrailingBytes);
    }
    Ok(Some(descriptor))
}
