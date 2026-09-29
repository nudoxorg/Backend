//! Coordinate-free source rows for the canonical semantic plane.
//!
//! Every declaration and canonical relation contributes exactly one row. The
//! rows commit captured source evidence or its explicit absence; a missing
//! row cannot masquerade as an unavailable source field to the family oracle.

use alloc::vec::Vec;

use super::{
    relations::relation_key,
    wire::{Cursor, encode_identity, put_bytes, read_identity},
};
use crate::ir::{
    CanonicalPlaneRowEncoder, CanonicalSemanticPlaneKeySink, DeclarationIdentity, FactAvailability,
    LinkId, SemanticIrPlane, SemanticPlaneKind, SemanticPlaneRecordError, SemanticReader,
};

const DECLARATION_SOURCE_TAG: u8 = 1;
const RELATION_SOURCE_TAG: u8 = 2;
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
        match handle {
            SourceProvenanceHandle::Declaration(identity) => {
                encode_declaration_source(reader, identity, out)?;
                Ok(DECLARATION_SOURCE_TAG)
            }
            SourceProvenanceHandle::Relation(id) => {
                let link = reader
                    .link(id)
                    .ok_or(SemanticPlaneRecordError::ReaderReference)?;
                out.extend_from_slice(&relation_key(reader, link)?);
                match link.source {
                    None => out.push(0),
                    Some(source) => {
                        out.push(1);
                        put_bytes(
                            out,
                            reader
                                .atom(source.file())
                                .ok_or(SemanticPlaneRecordError::ReaderReference)?,
                        )?;
                        out.extend_from_slice(&source.start().to_be_bytes());
                        out.extend_from_slice(&source.end().to_be_bytes());
                    }
                }
                Ok(RELATION_SOURCE_TAG)
            }
        }
    }
}

fn encode_declaration_source<Reader: SemanticReader + ?Sized>(
    reader: &Reader,
    identity: DeclarationIdentity,
    out: &mut Vec<u8>,
) -> Result<(), SemanticPlaneRecordError> {
    let entity = reader
        .entity_by_identity(identity)
        .ok_or(SemanticPlaneRecordError::ReaderReference)?;
    encode_identity(identity, out);
    if entity.authority.source != entity.authority.source_file {
        return Err(SemanticPlaneRecordError::RowGrammar);
    }
    match (entity.authority.source, entity.source) {
        (FactAvailability::Unavailable, None) => out.push(0),
        (FactAvailability::Captured, Some(source)) => {
            out.push(1);
            put_bytes(
                out,
                reader
                    .atom(source.file())
                    .ok_or(SemanticPlaneRecordError::ReaderReference)?,
            )?;
            out.extend_from_slice(&source.start().to_be_bytes());
            out.extend_from_slice(&source.end().to_be_bytes());
        }
        _ => return Err(SemanticPlaneRecordError::RowGrammar),
    }
    Ok(())
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
        DECLARATION_SOURCE_TAG => {
            let identity = read_identity(&mut cursor)?;
            super::declaration_plane_key(kind, identity)
        }
        RELATION_SOURCE_TAG => {
            let relation: [u8; 32] = cursor
                .take(32)?
                .try_into()
                .map_err(|_| SemanticPlaneRecordError::Truncated)?;
            relation_source_row_key(relation)
        }
        _ => return Err(SemanticPlaneRecordError::RowGrammar),
    };
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
    if !cursor.is_empty() {
        return Err(SemanticPlaneRecordError::RowTrailingBytes);
    }
    if expected_key != key {
        return Err(SemanticPlaneRecordError::StableKeyMismatch);
    }
    Ok(())
}
