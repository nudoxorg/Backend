//! Payload encoding for one replication message family.

use super::execution_payload::{read_resources, write_resources};
use crate::codec::primitives::{
    Reader, Writer, read_identity, read_version_range, write_identity, write_version_range,
};
use crate::{
    CapabilityManifest, ReplicationError, SchemaDescriptor, TransportLimits, WirePackClaim,
};

pub(super) fn write_pack(
    writer: &mut Writer,
    pack: &WirePackClaim,
) -> Result<(), ReplicationError> {
    writer.fixed(&pack.id)?;
    writer.fixed(&pack.layout)?;
    writer.bytes(&pack.bytes)?;
    writer
        .u32(u32::try_from(pack.locations.len()).map_err(|_| ReplicationError::MessageTooLarge)?)?;
    for (key, &(offset, length)) in &pack.locations {
        writer.bytes(key)?;
        writer.u32(offset)?;
        writer.u32(length)?;
    }
    Ok(())
}

pub(super) fn read_pack(
    reader: &mut Reader<'_>,
    limits: TransportLimits,
) -> Result<WirePackClaim, ReplicationError> {
    let id = reader.fixed()?;
    let layout = reader.fixed()?;
    let bytes = reader.bytes(limits.max_frame)?;
    let count = reader.count(limits.max_objects)?;
    let mut locations = std::collections::BTreeMap::new();
    for _ in 0..count {
        let key = reader.bytes(limits.max_key_bytes)?;
        let offset = reader.u32()?;
        let length = reader.u32()?;
        if locations.insert(key, (offset, length)).is_some() {
            return Err(ReplicationError::DuplicateKey);
        }
    }
    Ok(WirePackClaim {
        id,
        layout,
        bytes,
        locations,
    })
}

pub(super) fn write_capabilities(
    writer: &mut Writer,
    capabilities: &CapabilityManifest,
) -> Result<(), ReplicationError> {
    write_version_range(writer, capabilities.protocol)?;
    writer.u32(
        u32::try_from(capabilities.schemas.len()).map_err(|_| ReplicationError::MessageTooLarge)?,
    )?;
    for schema in &capabilities.schemas {
        writer.u8(schema.domain)?;
        writer.u16(schema.type_id)?;
        write_version_range(writer, schema.versions)?;
    }
    writer.u32(
        u32::try_from(capabilities.recipes.len()).map_err(|_| ReplicationError::MessageTooLarge)?,
    )?;
    for recipe in &capabilities.recipes {
        write_identity(writer, recipe.recipe)?;
        write_version_range(writer, recipe.versions)?;
    }
    writer.u64(capabilities.max_object)?;
    writer.u32(capabilities.max_chunk)?;
    writer.u32(capabilities.max_frame)?;
    writer.u32(capabilities.max_ranges)?;
    write_resources(writer, capabilities.max_resources)
}

pub(super) fn read_capabilities(
    reader: &mut Reader<'_>,
    limits: TransportLimits,
) -> Result<CapabilityManifest, ReplicationError> {
    let protocol = read_version_range(reader)?;
    let schemas_len = reader.count(limits.max_capabilities)?;
    let mut schemas = Vec::with_capacity(schemas_len);
    for _ in 0..schemas_len {
        schemas.push(SchemaDescriptor {
            domain: reader.u8()?,
            type_id: reader.u16()?,
            versions: read_version_range(reader)?,
        });
    }
    let recipes_len = reader.count(limits.max_capabilities)?;
    let mut recipes = Vec::with_capacity(recipes_len);
    for _ in 0..recipes_len {
        recipes.push(crate::RecipeCapability {
            recipe: read_identity(reader)?,
            versions: read_version_range(reader)?,
        });
    }
    Ok(CapabilityManifest {
        protocol,
        schemas,
        recipes,
        max_object: reader.u64()?,
        max_chunk: reader.u32()?,
        max_frame: reader.u32()?,
        max_ranges: reader.u32()?,
        max_resources: read_resources(reader)?,
    })
}
