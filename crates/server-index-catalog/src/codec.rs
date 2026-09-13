//! Scalar decoding for durable Turso cells.

use compiler_ir::DeclarationIdentity;
use heart_identity::{ContentId, GenerationId};
use server_index_vocabulary::{
    CanonicalEntityLocator, IndexLocatorFacts, SemanticImageExtent, SemanticImageLocator,
};
use turso::{Connection, Value};

use super::{CatalogError, CatalogRecord};

pub(super) fn integer(value: Value) -> Result<i64, CatalogError> {
    match value {
        Value::Integer(value) => Ok(value),
        _ => Err(CatalogError::CorruptScalar),
    }
}

pub(super) fn blob(value: Value, width: usize) -> Result<Vec<u8>, CatalogError> {
    match value {
        Value::Blob(value) if value.len() == width => Ok(value),
        _ => Err(CatalogError::CorruptBlob),
    }
}

pub(super) async fn load_entities(
    connection: &Connection,
    record: &mut CatalogRecord,
) -> Result<(), CatalogError> {
    let mut rows = connection.query("SELECT ordinal,family,variant,image FROM catalog_entities WHERE sequence=?1 ORDER BY ordinal", (record.sequence,)).await?;
    while let Some(row) = rows.next().await? {
        let ordinal =
            u32::try_from(integer(row.get_value(0)?)?).map_err(|_| CatalogError::CorruptLocator)?;
        let family: [u8; 16] = blob(row.get_value(1)?, 16)?
            .try_into()
            .map_err(|_| CatalogError::CorruptBlob)?;
        let variant: [u8; 16] = blob(row.get_value(2)?, 16)?
            .try_into()
            .map_err(|_| CatalogError::CorruptBlob)?;
        let entity_image = blob(row.get_value(3)?, 32)?;
        if entity_image.as_slice() != record.image.identity.as_ref() {
            return Err(CatalogError::EntityImageMismatch);
        }
        record.entities.push(CanonicalEntityLocator::new(
            record.image,
            ordinal,
            DeclarationIdentity {
                family: compiler_ir::DeclarationFamilyId::from_raw(family),
                variant: compiler_ir::VariantFingerprint::from_raw(variant),
            },
        ));
    }
    Ok(())
}

pub(super) fn record(row: turso::Row) -> Result<CatalogRecord, CatalogError> {
    let text = |index| match row.get_value(index)? {
        Value::Text(value) => Ok(value),
        _ => Err(CatalogError::CorruptScalar),
    };
    let generation = GenerationId::try_from(blob(row.get_value(4)?, 32)?.as_slice())
        .map_err(|_| CatalogError::CorruptLocator)?;
    let snapshot = ContentId::try_from(blob(row.get_value(5)?, 32)?.as_slice())
        .map_err(|_| CatalogError::CorruptLocator)?;
    let publication = ContentId::try_from(blob(row.get_value(6)?, 32)?.as_slice())
        .map_err(|_| CatalogError::CorruptLocator)?;
    let image_identity =
        heart_identity::ArtifactId::try_from(blob(row.get_value(7)?, 32)?.as_slice())
            .map_err(|_| CatalogError::CorruptLocator)?;
    let offset =
        u64::try_from(integer(row.get_value(8)?)?).map_err(|_| CatalogError::CorruptLocator)?;
    let length =
        u32::try_from(integer(row.get_value(9)?)?).map_err(|_| CatalogError::CorruptLocator)?;
    let extent =
        SemanticImageExtent::new(offset, length).map_err(|_| CatalogError::CorruptLocator)?;
    Ok(CatalogRecord {
        sequence: integer(row.get_value(0)?)?,
        ecosystem: text(1)?,
        package: text(2)?,
        version: text(3)?,
        authority: IndexLocatorFacts::new(generation, snapshot, publication),
        image: SemanticImageLocator::new(image_identity, extent),
        entities: Vec::new(),
    })
}
