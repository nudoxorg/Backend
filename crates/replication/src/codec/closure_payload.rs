//! Payload encoding for one replication message family.

use crate::codec::primitives::{
    Reader, Writer, read_authority, read_identity, write_authority, write_identity,
};
use crate::{
    ClosureNeedRequest, ClosurePageRequest, ClosurePageResponse, ClosureRootAck, ClosureRootOffer,
    MerkleChild, MerkleLeafEntry, MerklePage, MerklePageBody, MerklePageRequest, NodeDigest,
    PageCursor, ReplicationError, TransportLimits,
};

pub(super) fn write_closure_page_request(
    writer: &mut Writer,
    envelope: &ClosurePageRequest,
) -> Result<(), ReplicationError> {
    writer.u64(envelope.correlation)?;
    let request = envelope.request;
    writer.u32(request.root.schema())?;
    writer.fixed(&request.root.digest().0)?;
    writer.fixed(&request.node.0)?;
    writer.u32(request.cursor.offset)?;
    writer.u16(request.max_items)
}

pub(super) fn write_closure_root_offer(
    writer: &mut Writer,
    offer: &ClosureRootOffer,
) -> Result<(), ReplicationError> {
    writer.u64(offer.correlation)?;
    writer.fixed(&offer.workspace.as_bytes())?;
    writer.bytes(&offer.workspace_manifest)?;
    writer.u32(offer.root.schema())?;
    writer.fixed(&offer.root.digest().0)?;
    write_authority(writer, offer.authority)
}

pub(super) fn read_closure_root_offer(
    reader: &mut Reader<'_>,
    limits: TransportLimits,
) -> Result<ClosureRootOffer, ReplicationError> {
    Ok(ClosureRootOffer {
        correlation: reader.u64()?,
        workspace: crate::WorkspaceRootClaim::from_bytes(reader.fixed()?),
        workspace_manifest: reader.bytes(limits.max_frame)?,
        root: crate::MerkleRoot::new(reader.u32()?, NodeDigest(reader.fixed()?)),
        authority: read_authority(reader)?,
    })
}

pub(super) fn write_closure_root_ack(
    writer: &mut Writer,
    ack: &ClosureRootAck,
) -> Result<(), ReplicationError> {
    writer.u64(ack.correlation)?;
    writer.u32(ack.root.schema())?;
    writer.fixed(&ack.root.digest().0)?;
    writer.u8(u8::from(ack.warm))?;
    writer.u8(u8::from(ack.next.is_some()))?;
    if let Some(next) = ack.next {
        writer.u32(next)?;
    }
    writer.u32(u32::try_from(ack.missing.len()).map_err(|_| ReplicationError::MessageTooLarge)?)?;
    for identity in &ack.missing {
        write_identity(writer, *identity)?;
    }
    Ok(())
}

pub(super) fn read_closure_root_ack(
    reader: &mut Reader<'_>,
    limits: TransportLimits,
) -> Result<ClosureRootAck, ReplicationError> {
    let correlation = reader.u64()?;
    let schema = reader.u32()?;
    let digest = NodeDigest(reader.fixed()?);
    let warm = match reader.u8()? {
        0 => false,
        1 => true,
        _ => return Err(ReplicationError::InvalidWire),
    };
    let next = match reader.u8()? {
        0 => None,
        1 => Some(reader.u32()?),
        _ => return Err(ReplicationError::InvalidWire),
    };
    let count = reader.count(limits.max_objects)?;
    let mut missing = Vec::with_capacity(count);
    for _ in 0..count {
        missing.push(read_identity(reader)?);
    }
    Ok(ClosureRootAck {
        correlation,
        root: crate::MerkleRoot::new(schema, digest),
        warm,
        missing,
        next,
    })
}

pub(super) fn write_closure_need_request(
    writer: &mut Writer,
    request: &ClosureNeedRequest,
) -> Result<(), ReplicationError> {
    writer.u64(request.correlation)?;
    writer.u32(request.root.schema())?;
    writer.fixed(&request.root.digest().0)?;
    writer.u32(request.cursor)
}

pub(super) fn read_closure_need_request(
    reader: &mut Reader<'_>,
) -> Result<ClosureNeedRequest, ReplicationError> {
    Ok(ClosureNeedRequest {
        correlation: reader.u64()?,
        root: crate::MerkleRoot::new(reader.u32()?, NodeDigest(reader.fixed()?)),
        cursor: reader.u32()?,
    })
}

pub(super) fn read_closure_page_request(
    reader: &mut Reader<'_>,
    _limits: TransportLimits,
) -> Result<ClosurePageRequest, ReplicationError> {
    Ok(ClosurePageRequest {
        correlation: reader.u64()?,
        request: MerklePageRequest {
            root: crate::MerkleRoot::new(reader.u32()?, NodeDigest(reader.fixed()?)),
            node: NodeDigest(reader.fixed()?),
            cursor: PageCursor {
                offset: reader.u32()?,
            },
            max_items: reader.u16()?,
        },
    })
}

pub(super) fn write_closure_page_response(
    writer: &mut Writer,
    envelope: &ClosurePageResponse,
) -> Result<(), ReplicationError> {
    writer.u64(envelope.correlation)?;
    writer.bytes(&envelope.proof)?;
    let page = &envelope.page;
    writer.u32(page.root.schema())?;
    writer.fixed(&page.root.digest().0)?;
    writer.fixed(&page.node.0)?;
    writer.u16(page.level)?;
    writer.u32(page.cursor.offset)?;
    match page.next {
        Some(cursor) => {
            writer.u8(1)?;
            writer.u32(cursor.offset)?;
        }
        None => writer.u8(0)?,
    }
    match &page.body {
        MerklePageBody::Branch(children) => {
            writer.u8(0)?;
            writer.u32(
                u32::try_from(children.len()).map_err(|_| ReplicationError::MessageTooLarge)?,
            )?;
            for child in children {
                writer.bytes(&child.first_key)?;
                match &child.end_key {
                    Some(end) => {
                        writer.u8(1)?;
                        writer.bytes(end)?;
                    }
                    None => writer.u8(0)?,
                }
                writer.fixed(&child.digest.0)?;
                writer.u16(child.level)?;
                writer.u64(child.row_count)?;
            }
        }
        MerklePageBody::Leaf(entries) => {
            writer.u8(1)?;
            writer.u32(
                u32::try_from(entries.len()).map_err(|_| ReplicationError::MessageTooLarge)?,
            )?;
            for entry in entries {
                writer.bytes(&entry.key)?;
                writer.fixed(&entry.key_id)?;
                writer.fixed(&entry.version)?;
                writer.u64(entry.len)?;
            }
        }
    }
    Ok(())
}

pub(super) fn read_closure_page_response(
    reader: &mut Reader<'_>,
    limits: TransportLimits,
) -> Result<ClosurePageResponse, ReplicationError> {
    let correlation = reader.u64()?;
    let proof = reader.bytes(limits.max_frame)?;
    let root = crate::MerkleRoot::new(reader.u32()?, NodeDigest(reader.fixed()?));
    let node = NodeDigest(reader.fixed()?);
    let level = reader.u16()?;
    let cursor = PageCursor {
        offset: reader.u32()?,
    };
    let next = match reader.u8()? {
        0 => None,
        1 => Some(PageCursor {
            offset: reader.u32()?,
        }),
        _ => return Err(ReplicationError::InvalidWire),
    };
    let body = match reader.u8()? {
        0 => {
            let count = reader.count(limits.max_objects)?;
            let mut children = Vec::with_capacity(count);
            for _ in 0..count {
                let first_key = reader.bytes(limits.max_key_bytes)?;
                let end_key = match reader.u8()? {
                    0 => None,
                    1 => Some(reader.bytes(limits.max_key_bytes)?),
                    _ => return Err(ReplicationError::InvalidWire),
                };
                children.push(MerkleChild {
                    first_key,
                    end_key,
                    digest: NodeDigest(reader.fixed()?),
                    level: reader.u16()?,
                    row_count: reader.u64()?,
                });
            }
            MerklePageBody::Branch(children)
        }
        1 => {
            let count = reader.count(limits.max_objects)?;
            let mut entries = Vec::with_capacity(count);
            for _ in 0..count {
                entries.push(MerkleLeafEntry {
                    key: reader.bytes(limits.max_key_bytes)?,
                    key_id: reader.fixed()?,
                    version: reader.fixed()?,
                    len: reader.u64()?,
                });
            }
            MerklePageBody::Leaf(entries)
        }
        _ => return Err(ReplicationError::InvalidWire),
    };
    Ok(ClosurePageResponse {
        correlation,
        proof,
        page: MerklePage {
            root,
            node,
            level,
            cursor,
            next,
            body,
        },
    })
}
