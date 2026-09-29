//! Canonical declaration and documentation record families for SPIR.

use alloc::{boxed::Box, vec::Vec};

use super::wire::{Cursor, encode_identity, put_bytes, put_text, put_u32, read_identity};
use crate::ir::{
    CanonicalPlaneRowEncoder, CanonicalSemanticPlaneKeySink, DeclarationIdentity, DocFragment,
    EntityAuthorityFacts, FactAvailability, LinkTarget, ParentageAuthority, SemanticEntity,
    SemanticPlaneKind, SemanticPlaneRecordError, SemanticReader,
};

const CORE_TAG: u8 = 1;
const DOCS_TAG: u8 = 2;

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
        encode_core_row(reader, row, out)?;
        Ok(CORE_TAG)
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
        let entity = reader
            .entity_by_identity(identity)
            .ok_or(SemanticPlaneRecordError::ReaderReference)?;
        encode_identity(identity, out);
        out.push(availability(entity.authority.documentation));
        let docs = reader
            .docs(entity.docs)
            .ok_or(SemanticPlaneRecordError::ReaderReference)?;
        put_u32(out, docs.len())?;
        for fragment in docs {
            match fragment {
                DocFragment::Text(text) => {
                    out.push(0);
                    put_text(
                        out,
                        reader
                            .text(text)
                            .ok_or(SemanticPlaneRecordError::ReaderReference)?,
                    )?;
                }
                DocFragment::Code(text) => {
                    out.push(1);
                    put_text(
                        out,
                        reader
                            .text(text)
                            .ok_or(SemanticPlaneRecordError::ReaderReference)?,
                    )?;
                }
                DocFragment::Link { label, target } => {
                    out.push(2);
                    put_text(
                        out,
                        reader
                            .text(label)
                            .ok_or(SemanticPlaneRecordError::ReaderReference)?,
                    )?;
                    match target {
                        LinkTarget::Local(id) => {
                            out.push(0);
                            let target = reader
                                .entity(id)
                                .ok_or(SemanticPlaneRecordError::ReaderReference)?;
                            encode_identity(target.version.identity(), out);
                        }
                        LinkTarget::External(id) => {
                            out.push(1);
                            let target = crate::ir::ExternalTargetIdentity::capture(reader, id)
                                .map_err(|_| SemanticPlaneRecordError::ReaderReference)?;
                            out.extend_from_slice(target.as_bytes());
                        }
                    }
                }
                DocFragment::SoftBreak => out.push(3),
                DocFragment::HardBreak => out.push(4),
            }
        }
        Ok(DOCS_TAG)
    }
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

pub(super) fn validate_record(
    kind: SemanticPlaneKind,
    key: [u8; 32],
    tag: u8,
    payload: &[u8],
) -> Result<(), SemanticPlaneRecordError> {
    let mut cursor = Cursor::new(payload);
    let identity = match (kind, tag) {
        (SemanticPlaneKind::Ir(crate::ir::SemanticIrPlane::Core), CORE_TAG) => {
            let identity = read_identity(&mut cursor)?;
            let _ = cursor.bytes32()?; // Name bytes are not promised UTF-8.
            if !crate::ir::ItemKind::try_from(cursor.u16()?).is_ok() {
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

fn encode_core_row<Reader: SemanticReader + ?Sized>(
    reader: &Reader,
    row: SemanticEntity,
    out: &mut Vec<u8>,
) -> Result<(), SemanticPlaneRecordError> {
    let identity = row.version.identity();
    encode_identity(identity, out);
    put_bytes(
        out,
        reader
            .atom(row.name)
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
    Ok(())
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
