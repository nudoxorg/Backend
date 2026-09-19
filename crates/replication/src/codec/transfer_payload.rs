//! Payload encoding for one replication message family.

use crate::codec::primitives::{
    Reader, Writer, read_authority, read_coverage, read_wire_id, write_authority, write_coverage,
    write_wire_id,
};
use crate::{
    ChunkChain, Frame, ReplicationError, TransferId, TransportLimits, WireNodeRequest,
    WireRangeRequest, WireResumeRequest,
};

pub(super) fn write_node_request<T: backend_version::Schema>(
    writer: &mut Writer,
    request: &WireNodeRequest<T>,
) -> Result<(), ReplicationError> {
    writer.u64(request.transfer.get())?;
    write_wire_id(writer, request.key)?;
    write_wire_id(writer, request.version)?;
    writer.u64(request.len)?;
    write_coverage(writer, &request.resume)
}

pub(super) fn read_node_request(
    reader: &mut Reader<'_>,
    limits: TransportLimits,
) -> Result<WireNodeRequest, ReplicationError> {
    Ok(WireNodeRequest {
        transfer: TransferId::new(reader.u64()?)?,
        key: read_wire_id(reader)?,
        version: read_wire_id(reader)?,
        len: reader.u64()?,
        resume: read_coverage(reader, limits, None)?,
    })
}

pub(super) fn write_range_request(
    writer: &mut Writer,
    request: &WireRangeRequest,
) -> Result<(), ReplicationError> {
    writer.u16(request.relation)?;
    write_wire_id(writer, request.root)?;
    writer.bytes(&request.start)?;
    match &request.end {
        Some(end) => {
            writer.u8(1)?;
            writer.bytes(end)?;
        }
        None => writer.u8(0)?,
    }
    writer.u32(request.limit)?;
    write_coverage(writer, &request.resume)
}

pub(super) fn read_range_request(
    reader: &mut Reader<'_>,
    limits: TransportLimits,
) -> Result<WireRangeRequest, ReplicationError> {
    let relation = reader.u16()?;
    let root = read_wire_id(reader)?;
    let start = reader.bytes(limits.max_key_bytes)?;
    let end = match reader.u8()? {
        0 => None,
        1 => Some(reader.bytes(limits.max_key_bytes)?),
        _ => return Err(ReplicationError::InvalidWire),
    };
    Ok(WireRangeRequest {
        relation,
        root,
        start,
        end,
        limit: reader.u32()?,
        resume: read_coverage(reader, limits, None)?,
    })
}

pub(super) fn write_frame<T: backend_version::Schema>(
    writer: &mut Writer,
    frame: &Frame<T>,
) -> Result<(), ReplicationError> {
    writer.u64(frame.transfer.get())?;
    write_wire_id(writer, frame.key)?;
    write_wire_id(writer, frame.version)?;
    writer.u64(frame.object_len)?;
    writer.u64(frame.offset)?;
    writer.u64(frame.sequence)?;
    writer.fixed(&frame.previous_chain.0)?;
    writer.fixed(&frame.chain.0)?;
    writer.bytes(&frame.payload)?;
    write_authority(writer, frame.authority)
}

pub(super) fn read_frame(
    reader: &mut Reader<'_>,
    limits: TransportLimits,
) -> Result<Frame, ReplicationError> {
    Ok(Frame {
        transfer: TransferId::new(reader.u64()?)?,
        key: read_wire_id(reader)?,
        version: read_wire_id(reader)?,
        object_len: reader.u64()?,
        offset: reader.u64()?,
        sequence: reader.u64()?,
        previous_chain: ChunkChain(reader.fixed()?),
        chain: ChunkChain(reader.fixed()?),
        payload: reader.bytes(limits.max_chunk)?,
        authority: read_authority(reader)?,
    })
}

pub(super) fn write_resume_request<T: backend_version::Schema>(
    writer: &mut Writer,
    request: &WireResumeRequest<T>,
) -> Result<(), ReplicationError> {
    writer.u64(request.transfer.get())?;
    write_wire_id(writer, request.key)?;
    write_wire_id(writer, request.version)?;
    writer.u64(request.len)?;
    write_coverage(writer, &request.coverage)
}

pub(super) fn read_resume_request(
    reader: &mut Reader<'_>,
    limits: TransportLimits,
) -> Result<WireResumeRequest, ReplicationError> {
    Ok(WireResumeRequest {
        transfer: TransferId::new(reader.u64()?)?,
        key: read_wire_id(reader)?,
        version: read_wire_id(reader)?,
        len: reader.u64()?,
        coverage: read_coverage(reader, limits, None)?,
    })
}
