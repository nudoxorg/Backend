//! Payload encoding for one replication message family.

use crate::codec::primitives::{
    Reader, Writer, read_authority, read_coverage, read_wire_id, read_workspace, write_authority,
    write_coverage, write_wire_id, write_workspace,
};
use crate::{ReplicationError, TransportLimits, WireRootSummary};

pub(super) fn write_root_summary(
    writer: &mut Writer,
    summary: &WireRootSummary,
) -> Result<(), ReplicationError> {
    writer.u32(summary.schema)?;
    write_workspace(writer, summary.workspace)?;
    writer.u32(
        u32::try_from(summary.relations.len()).map_err(|_| ReplicationError::MessageTooLarge)?,
    )?;
    for relation in &summary.relations {
        writer.u16(relation.relation)?;
        write_wire_id(writer, relation.root)?;
        write_coverage(writer, &relation.coverage)?;
    }
    writer.u32(
        u32::try_from(summary.objects.len()).map_err(|_| ReplicationError::MessageTooLarge)?,
    )?;
    for object in &summary.objects {
        write_wire_id(writer, object.key)?;
        write_wire_id(writer, object.version)?;
        writer.u64(object.len)?;
    }
    write_authority(writer, summary.authority)?;
    write_coverage(writer, &summary.coverage)
}

pub(super) fn read_root_summary(
    reader: &mut Reader<'_>,
    limits: TransportLimits,
) -> Result<WireRootSummary, ReplicationError> {
    let schema = reader.u32()?;
    let workspace = read_workspace(reader)?;
    let relation_len = reader.count(limits.max_objects)?;
    let mut relations = Vec::with_capacity(relation_len);
    for _ in 0..relation_len {
        relations.push(crate::WireRelationSummary {
            relation: reader.u16()?,
            root: read_wire_id(reader)?,
            coverage: read_coverage(reader, limits, None)?,
        });
    }
    let object_len = reader.count(limits.max_objects)?;
    let mut objects = Vec::with_capacity(object_len);
    for _ in 0..object_len {
        objects.push(crate::WireObjectSummary {
            key: read_wire_id(reader)?,
            version: read_wire_id(reader)?,
            len: reader.u64()?,
        });
    }
    Ok(WireRootSummary {
        schema,
        workspace,
        relations,
        objects,
        authority: read_authority(reader)?,
        coverage: read_coverage(reader, limits, None)?,
    })
}
