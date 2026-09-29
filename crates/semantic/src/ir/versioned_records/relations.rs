//! Canonical stable graph-relation records for SPIR.

use alloc::vec::Vec;

use crate::ir::{
    CanonicalPlaneRowEncoder, CanonicalSemanticPlaneKeySink, Confidence, DeclarationIdentity,
    ExternalTargetIdentity, Link, LinkId, LinkKind, LinkTarget, SemanticIrPlane, SemanticPlaneKind,
    SemanticPlaneRecordError, SemanticReader,
};

const RELATION_TAG: u8 = 3;
const RELATION_KEY_DOMAIN: &[u8] = b"backend.semantic.ir.relation-key.v1\0";

/// Stable-key encoder for canonical relation rows.
#[derive(Clone, Copy, Debug, Default)]
pub struct RelationRows;

impl CanonicalPlaneRowEncoder for RelationRows {
    type Handle = LinkId;
    type Plan = ();

    fn kind(&self) -> SemanticPlaneKind {
        SemanticPlaneKind::Ir(SemanticIrPlane::Relations)
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
        for (id, link) in reader.canonical_links() {
            sink.push(relation_key(reader, link)?, id)?;
        }
        Ok(())
    }

    fn encode_row<Reader: SemanticReader + ?Sized>(
        &self,
        reader: &Reader,
        _plan: &Self::Plan,
        id: Self::Handle,
        out: &mut Vec<u8>,
    ) -> Result<u8, SemanticPlaneRecordError> {
        let link = reader
            .link(id)
            .ok_or(SemanticPlaneRecordError::ReaderReference)?;
        encode_relation_payload(reader, link, out)?;
        Ok(RELATION_TAG)
    }
}

/// Coordinate-independent relation identity shared with occurrence rows.
pub(super) fn relation_key<Reader: SemanticReader + ?Sized>(
    reader: &Reader,
    link: Link,
) -> Result<[u8; 32], SemanticPlaneRecordError> {
    let from = reader
        .entity(link.from)
        .ok_or(SemanticPlaneRecordError::ReaderReference)?
        .version
        .identity();
    let (target_kind, target) = target_identity(reader, link.target)?;
    Ok(relation_key_from_parts(
        from,
        target_kind,
        target,
        link.kind,
    ))
}

pub(super) fn relation_key_from_parts(
    from: DeclarationIdentity,
    target_kind: u8,
    target: [u8; 32],
    kind: LinkKind,
) -> [u8; 32] {
    let mut hasher = blake3::Hasher::new();
    hasher.update(RELATION_KEY_DOMAIN);
    hasher.update(from.family.as_bytes());
    hasher.update(from.variant.as_bytes());
    hasher.update(&[target_kind]);
    hasher.update(&target);
    hasher.update(&[kind as u8]);
    *hasher.finalize().as_bytes()
}

fn target_identity<Reader: SemanticReader + ?Sized>(
    reader: &Reader,
    target: LinkTarget,
) -> Result<(u8, [u8; 32]), SemanticPlaneRecordError> {
    match target {
        LinkTarget::Local(id) => {
            let identity = reader
                .entity(id)
                .ok_or(SemanticPlaneRecordError::ReaderReference)?
                .version
                .identity();
            Ok((
                0,
                crate::ir::declaration_plane_key(
                    SemanticPlaneKind::Ir(SemanticIrPlane::Core),
                    identity,
                ),
            ))
        }
        LinkTarget::External(id) => {
            let identity = ExternalTargetIdentity::capture(reader, id)
                .map_err(|_| SemanticPlaneRecordError::ReaderReference)?;
            Ok((1, *identity.as_bytes()))
        }
    }
}

fn encode_relation_payload<Reader: SemanticReader + ?Sized>(
    reader: &Reader,
    link: Link,
    out: &mut Vec<u8>,
) -> Result<(), SemanticPlaneRecordError> {
    let from = reader
        .entity(link.from)
        .ok_or(SemanticPlaneRecordError::ReaderReference)?
        .version
        .identity();
    let (target_kind, target) = target_identity(reader, link.target)?;
    out.extend_from_slice(from.family.as_bytes());
    out.extend_from_slice(from.variant.as_bytes());
    out.push(target_kind);
    out.extend_from_slice(&target);
    out.push(link.kind as u8);
    out.push(link.confidence as u8);
    Ok(())
}

pub(super) fn validate_record(
    key: [u8; 32],
    tag: u8,
    payload: &[u8],
) -> Result<(), SemanticPlaneRecordError> {
    if tag != RELATION_TAG || payload.len() != 67 {
        return Err(SemanticPlaneRecordError::RowGrammar);
    }
    let from = DeclarationIdentity {
        family: crate::ir::DeclarationFamilyId::from_raw(
            payload[..16]
                .try_into()
                .map_err(|_| SemanticPlaneRecordError::Truncated)?,
        ),
        variant: crate::ir::VariantFingerprint::from_raw(
            payload[16..32]
                .try_into()
                .map_err(|_| SemanticPlaneRecordError::Truncated)?,
        ),
    };
    let target_kind = payload[32];
    if target_kind > 1 {
        return Err(SemanticPlaneRecordError::RowGrammar);
    }
    let target: [u8; 32] = payload[33..65]
        .try_into()
        .map_err(|_| SemanticPlaneRecordError::Truncated)?;
    let kind = decode_link_kind(payload[65])?;
    let _confidence = decode_confidence(payload[66])?;
    if relation_key_from_parts(from, target_kind, target, kind) != key {
        return Err(SemanticPlaneRecordError::StableKeyMismatch);
    }
    Ok(())
}

fn decode_link_kind(raw: u8) -> Result<LinkKind, SemanticPlaneRecordError> {
    Ok(match raw {
        0 => LinkKind::Calls,
        1 => LinkKind::MethodCall,
        2 => LinkKind::TypeReference,
        3 => LinkKind::Reads,
        4 => LinkKind::Writes,
        5 => LinkKind::Imports,
        6 => LinkKind::Implements,
        7 => LinkKind::Overrides,
        8 => LinkKind::Reexports,
        9 => LinkKind::Inherits,
        10 => LinkKind::Documents,
        _ => return Err(SemanticPlaneRecordError::RowGrammar),
    })
}

fn decode_confidence(raw: u8) -> Result<Confidence, SemanticPlaneRecordError> {
    Ok(match raw {
        0 => Confidence::Syntactic,
        1 => Confidence::Heuristic,
        2 => Confidence::Indexed,
        3 => Confidence::Imported,
        4 => Confidence::Compiler,
        _ => return Err(SemanticPlaneRecordError::RowGrammar),
    })
}
