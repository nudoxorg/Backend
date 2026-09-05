//! Durable, transactional carrier for verified index publication locators.
#![forbid(unsafe_code)]
#![warn(missing_docs)]

mod codec;
mod page;
mod schema;

use compiler_ir::{DeclarationIdentity, PackageLineage};
use server_index_ingest::{Checkpoint, CheckpointFault};
use server_index_vocabulary::{
    CanonicalEntityLocator, IndexLocatorFacts, PackageCoordinate, PackageVersion,
    SemanticImageLocator, VerifiedCanonicalEntityLocator,
};
use std::fmt;
use thiserror::Error;
use turso::{Builder, Connection, Value};

use codec::{blob, integer, load_entities, record};

/// One borrowed, already verified catalog publication.
#[derive(Clone, Copy)]
pub struct CatalogPublication<'a> {
    /// Package coordinate being published.
    pub package: PackageCoordinate<'a>,
    /// Exact compiler/index authorities.
    pub authority: IndexLocatorFacts,
    /// Exact semantic-image identity and extent.
    pub image: SemanticImageLocator,
    /// Entity locators proved against the reopened image.
    pub entities: &'a [VerifiedCanonicalEntityLocator],
}

/// Result of an idempotent publication.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CatalogPublishOutcome {
    /// A new history row was committed.
    Inserted {
        /// Durable immutable history sequence.
        sequence: i64,
    },
    /// The exact current row already existed.
    Unchanged {
        /// Durable immutable history sequence.
        sequence: i64,
    },
}

/// Durable catalog row returned by resolution.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CatalogRecord {
    /// Package ecosystem, retained as exact owned text.
    pub ecosystem: String,
    /// Package name, retained as exact owned text.
    pub package: String,
    /// Exact package version, retained as exact owned text.
    pub version: String,
    /// Durable history sequence.
    pub sequence: i64,
    /// Exact typed authorities retained by the row.
    pub authority: IndexLocatorFacts,
    /// Exact typed image locator retained by the row.
    pub image: SemanticImageLocator,
    /// Entity locators retained by the row. Verification proof remains private to vocabulary.
    pub entities: Vec<CanonicalEntityLocator>,
}

/// Typed failure of opening, publishing, or resolving the durable carrier.
#[derive(Debug, Error)]
pub enum CatalogError {
    /// Underlying Turso operation failed.
    #[error("catalog database: {0}")]
    Database(#[from] turso::Error),
    /// A stored row had the wrong scalar type.
    #[error("catalog corruption: scalar")]
    CorruptScalar,
    /// A stored row had a blob with the wrong width.
    #[error("catalog corruption: blob width")]
    CorruptBlob,
    /// A stored row contained an invalid typed identity or extent.
    #[error("catalog corruption: typed locator")]
    CorruptLocator,
    /// A stored entity row did not match its parent image.
    #[error("catalog entity image mismatch")]
    EntityImageMismatch,
    /// One verified locator was repeated or bound to another image.
    #[error("duplicate or mismatched verified entity locator")]
    DuplicateEntity,
    /// Entity locators were not supplied in strictly increasing declaration order.
    #[error("entity locators are out of canonical declaration order")]
    EntityOutOfOrder {
        /// The preceding declaration identity.
        previous: DeclarationIdentity,
        /// The observed declaration identity.
        observed: DeclarationIdentity,
    },
    /// Unversioned resolution found more than one package version.
    #[error("package lineage resolves ambiguously")]
    Ambiguous,
    /// The requested package row does not exist.
    #[error("catalog row not found")]
    NotFound,
}

/// One bounded mutation in an atomically checkpointed catalog page.
pub enum CatalogPageOperation<'a> {
    /// Publish one verified catalog row.
    Publish(CatalogPublication<'a>),
    /// Remove one coordinate from current resolution while retaining immutable history.
    Remove(PackageCoordinate<'a>),
}

/// Exact failure while applying a checkpointed catalog page.
#[derive(Debug, Error)]
pub enum CatalogPageError {
    /// The requested successor does not strictly follow the caller checkpoint.
    #[error("catalog checkpoint candidate is not higher")]
    Candidate(CheckpointFault),
    /// The database checkpoint differs from the caller's retained checkpoint.
    #[error("catalog checkpoint is stale")]
    Stale {
        /// Checkpoint durably stored by the catalog.
        stored: Checkpoint,
        /// Checkpoint supplied by the caller.
        current: Checkpoint,
    },
    /// The caller exceeded the bounded page capacity.
    #[error("catalog page is too large")]
    TooManyOperations,
    /// One page named the same package coordinate more than once.
    #[error("catalog page repeats a package coordinate")]
    DuplicateCoordinate,
    /// A caller checkpoint exceeds Turso's signed durable integer width.
    #[error("catalog checkpoint exceeds durable width")]
    CheckpointWidth,
    /// A catalog mutation failed and the transaction was rolled back.
    #[error(transparent)]
    Catalog(#[from] CatalogError),
    /// Turso failed while starting, reading, or committing the page transaction.
    #[error("catalog page database: {0}")]
    Database(#[from] turso::Error),
}

/// Single-writer durable Turso catalog.
pub struct TursoCatalog {
    connection: Connection,
}

impl fmt::Debug for TursoCatalog {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TursoCatalog").finish_non_exhaustive()
    }
}

impl TursoCatalog {
    /// Opens (and, on first use, initializes) a local Turso database.
    pub async fn open(path: &str) -> Result<Self, CatalogError> {
        let database = Builder::new_local(path).build().await?;
        let connection = database.connect()?;
        connection.execute_batch(schema::INITIALIZE).await?;
        Ok(Self { connection })
    }

    /// Publishes one row atomically after complete duplicate preflight.
    pub async fn publish(
        &mut self,
        publication: CatalogPublication<'_>,
    ) -> Result<CatalogPublishOutcome, CatalogError> {
        let transaction = self.connection.transaction().await?;
        let outcome = Self::publish_in(&transaction, publication).await?;
        transaction.commit().await?;
        Ok(outcome)
    }

    async fn publish_in(
        tx: &turso::transaction::Transaction<'_>,
        publication: CatalogPublication<'_>,
    ) -> Result<CatalogPublishOutcome, CatalogError> {
        let image = publication.image;
        for (index, entity) in publication.entities.iter().enumerate() {
            let locator = entity.as_locator();
            if locator.image != image {
                return Err(CatalogError::EntityImageMismatch);
            }
            if index != 0 {
                let prior = publication.entities[index - 1].as_locator();
                if prior.declaration == locator.declaration {
                    return Err(CatalogError::DuplicateEntity);
                }
                if prior.declaration > locator.declaration {
                    return Err(CatalogError::EntityOutOfOrder {
                        previous: prior.declaration,
                        observed: locator.declaration,
                    });
                }
            }
        }
        let p = publication.package;
        let a = publication.authority;
        let params = (
            p.lineage.ecosystem.to_owned(),
            p.lineage.name.to_owned(),
            p.version.as_str().to_owned(),
            Value::Blob(a.generation.as_ref().to_vec()),
            Value::Blob(a.snapshot.as_ref().to_vec()),
            Value::Blob(a.publication.as_ref().to_vec()),
            Value::Blob(image.identity.as_ref().to_vec()),
            i64::try_from(image.extent.offset()).map_err(|_| CatalogError::CorruptLocator)?,
            i64::from(image.extent.byte_length()),
        );
        let mut rows = tx.prepare("SELECT c.sequence FROM catalog_current c WHERE c.ecosystem=?1 AND c.package=?2 AND c.version=?3").await?.query((p.lineage.ecosystem, p.lineage.name, p.version.as_str())).await?;
        let outcome = if let Some(row) = rows.next().await? {
            let sequence = integer(row.get_value(0)?)?;
            let mut current = tx.query("SELECT generation,snapshot,publication,image,image_offset,image_length FROM catalog_history WHERE sequence=?1", (sequence,)).await?;
            let current = current.next().await?.ok_or(CatalogError::CorruptScalar)?;
            let same = current.get_value(0)? == params.3
                && current.get_value(1)? == params.4
                && current.get_value(2)? == params.5
                && current.get_value(3)? == params.6
                && current.get_value(4)? == Value::Integer(params.7)
                && current.get_value(5)? == Value::Integer(params.8);
            let same_entities = if same {
                entities_match(tx, sequence, publication.entities).await?
            } else {
                false
            };
            if same_entities {
                CatalogPublishOutcome::Unchanged { sequence }
            } else {
                tx.execute("INSERT INTO catalog_history (ecosystem,package,version,generation,snapshot,publication,image,image_offset,image_length) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9)", params.clone()).await?;
                CatalogPublishOutcome::Inserted {
                    sequence: integer(
                        tx.query("SELECT last_insert_rowid()", ())
                            .await?
                            .next()
                            .await?
                            .ok_or(CatalogError::CorruptScalar)?
                            .get_value(0)?,
                    )?,
                }
            }
        } else {
            tx.execute("INSERT INTO catalog_history (ecosystem,package,version,generation,snapshot,publication,image,image_offset,image_length) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9)", params).await?;
            let mut rows = tx.query("SELECT last_insert_rowid()", ()).await?;
            let row = rows.next().await?.ok_or(CatalogError::CorruptScalar)?;
            CatalogPublishOutcome::Inserted {
                sequence: integer(row.get_value(0)?)?,
            }
        };
        if let CatalogPublishOutcome::Inserted { sequence } = outcome {
            for entity in publication.entities {
                let locator = entity.as_locator();
                tx.execute("INSERT INTO catalog_entities(sequence,ordinal,family,variant,image) VALUES (?1,?2,?3,?4,?5)", (sequence, i64::from(locator.ordinal), Value::Blob(locator.declaration.family.as_bytes().to_vec()), Value::Blob(locator.declaration.variant.as_bytes().to_vec()), Value::Blob(locator.image.identity.as_ref().to_vec()))).await?;
            }
            tx.execute("INSERT INTO catalog_current(ecosystem,package,version,sequence) VALUES (?1,?2,?3,?4) ON CONFLICT(ecosystem,package,version) DO UPDATE SET sequence=excluded.sequence", (p.lineage.ecosystem, p.lineage.name, p.version.as_str(), sequence)).await?;
        }
        Ok(outcome)
    }

    /// Resolves an exact version, or rejects an unversioned ambiguous lineage.
    pub async fn resolve(
        &mut self,
        lineage: PackageLineage<'_>,
        version: Option<PackageVersion<'_>>,
    ) -> Result<CatalogRecord, CatalogError> {
        let ecosystem = lineage.ecosystem;
        let package = lineage.name;
        let mut rows = if let Some(version) = version {
            self.connection.query("SELECT h.sequence,h.ecosystem,h.package,h.version,h.generation,h.snapshot,h.publication,h.image,h.image_offset,h.image_length FROM catalog_current c JOIN catalog_history h ON h.sequence=c.sequence WHERE c.ecosystem=?1 AND c.package=?2 AND c.version=?3", (ecosystem, package, version.as_str())).await?
        } else {
            self.connection.query("SELECT h.sequence,h.ecosystem,h.package,h.version,h.generation,h.snapshot,h.publication,h.image,h.image_offset,h.image_length FROM catalog_current c JOIN catalog_history h ON h.sequence=c.sequence WHERE c.ecosystem=?1 AND c.package=?2 ORDER BY h.sequence DESC LIMIT 1", (ecosystem, package)).await?
        };
        let first = rows.next().await?.ok_or(CatalogError::NotFound)?;
        if version.is_none() {
            let observed_version = match first.get_value(3)? {
                Value::Text(value) => value,
                _ => return Err(CatalogError::CorruptScalar),
            };
            let mut versions = self.connection.query("SELECT version FROM catalog_current WHERE ecosystem=?1 AND package=?2 AND version<>?3 LIMIT 1", (ecosystem, package, observed_version)).await?;
            if versions.next().await?.is_some() {
                return Err(CatalogError::Ambiguous);
            }
        }
        let mut record = record(first)?;
        load_entities(&self.connection, &mut record).await?;
        Ok(record)
    }

    /// Returns all committed history rows for one exact coordinate.
    pub async fn history(
        &mut self,
        coordinate: PackageCoordinate<'_>,
    ) -> Result<Vec<CatalogRecord>, CatalogError> {
        let mut rows = self.connection.query("SELECT sequence,ecosystem,package,version,generation,snapshot,publication,image,image_offset,image_length FROM catalog_history WHERE ecosystem=?1 AND package=?2 AND version=?3 ORDER BY sequence", (coordinate.lineage.ecosystem, coordinate.lineage.name, coordinate.version.as_str())).await?;
        let mut result = Vec::new();
        while let Some(row) = rows.next().await? {
            let mut item = record(row)?;
            load_entities(&self.connection, &mut item).await?;
            result.push(item);
        }
        Ok(result)
    }
}

async fn entities_match(
    transaction: &turso::transaction::Transaction<'_>,
    sequence: i64,
    expected: &[VerifiedCanonicalEntityLocator],
) -> Result<bool, CatalogError> {
    let mut rows = transaction
        .query(
            "SELECT ordinal,family,variant,image FROM catalog_entities WHERE sequence=?1 ORDER BY ordinal",
            (sequence,),
        )
        .await?;
    let mut index = 0;
    while let Some(row) = rows.next().await? {
        let Some(expected) = expected.get(index) else {
            return Ok(false);
        };
        let locator = expected.as_locator();
        let ordinal =
            u32::try_from(integer(row.get_value(0)?)?).map_err(|_| CatalogError::CorruptLocator)?;
        let family: [u8; 16] = blob(row.get_value(1)?, 16)?
            .try_into()
            .map_err(|_| CatalogError::CorruptBlob)?;
        let variant: [u8; 16] = blob(row.get_value(2)?, 16)?
            .try_into()
            .map_err(|_| CatalogError::CorruptBlob)?;
        let image = blob(row.get_value(3)?, 32)?;
        if ordinal != locator.ordinal
            || family != *locator.declaration.family.as_bytes()
            || variant != *locator.declaration.variant.as_bytes()
            || image.as_slice() != locator.image.identity.as_ref()
        {
            return Ok(false);
        }
        index += 1;
    }
    Ok(index == expected.len())
}
