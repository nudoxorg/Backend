//! Coordinate-free source rows for the canonical semantic plane.
//!
//! Every declaration contributes exactly one row. The row therefore commits
//! both captured source evidence and its explicit absence; a missing row cannot
//! be mistaken for an unavailable source field by a complete-family oracle.

use alloc::vec::Vec;

use super::wire::{Cursor, encode_identity, put_bytes, read_identity};
use crate::ir::{
    CanonicalPlaneRowEncoder, CanonicalSemanticPlaneKeySink, DeclarationIdentity, FactAvailability,
    SemanticIrPlane, SemanticPlaneKind, SemanticPlaneRecordError, SemanticReader,
};

const SOURCE_TAG: u8 = 1;

/// Stable-key producer for the source provenance of every declaration.
#[derive(Clone, Copy, Debug, Default)]
pub struct SourceProvenanceRows;

impl CanonicalPlaneRowEncoder for SourceProvenanceRows {
    type Handle = DeclarationIdentity;
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
        Ok(SOURCE_TAG)
    }
}

pub(super) fn validate_record(
    kind: SemanticPlaneKind,
    key: [u8; 32],
    tag: u8,
    payload: &[u8],
) -> Result<(), SemanticPlaneRecordError> {
    if kind != SemanticPlaneKind::Ir(SemanticIrPlane::SourceProvenance) || tag != SOURCE_TAG {
        return Err(SemanticPlaneRecordError::RowGrammar);
    }
    let mut cursor = Cursor::new(payload);
    let identity = read_identity(&mut cursor)?;
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
    if super::declaration_plane_key(kind, identity) != key {
        return Err(SemanticPlaneRecordError::StableKeyMismatch);
    }
    Ok(())
}
