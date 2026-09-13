use crate::crates_io::PollResult;
use crate::{ArchiveChecksum, CratesIoAdapter, RegistryError, RegistryTransport};
use backend_semantic::ir::PackageLineage;
use server_index_catalog::{
    CatalogPageError, CatalogPageOperation, CatalogPublication, FeedCheckpoint,
    FeedContentChecksum, TursoCatalog,
};
use server_index_ingest::Checkpoint;
use server_index_vocabulary::{
    PackageCoordinate, PackageVersion, VerifiedCanonicalEntityLocator, VerifiedSemanticPublication,
};
use std::{collections::BTreeSet, sync::atomic::AtomicBool};
use thiserror::Error;

/// One registry record whose package source bytes have been verified but not materialized.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VerifiedArchive<'entry> {
    pub(crate) package: &'entry str,
    pub(crate) version: &'entry str,
    pub(crate) checksum: ArchiveChecksum,
    pub(crate) bytes: Vec<u8>,
}

impl VerifiedArchive<'_> {
    /// Returns the registry-validated Cargo package name.
    pub fn package(&self) -> &str {
        self.package
    }
    /// Returns the registry-validated version spelling.
    pub fn version(&self) -> &str {
        self.version
    }
    /// Returns the verified SHA-256 content address.
    pub const fn checksum(&self) -> ArchiveChecksum {
        self.checksum
    }
    /// Borrows exactly the checksum-verified archive bytes.
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }
}

/// Compiler materialization output, admitting only compiler-verified semantic locators.
///
/// ```compile_fail
/// use server_index_acquire::MaterializedPublication;
///
/// fn rename(output: MaterializedPublication) {
///     let _ = output.package;
/// }
/// ```
#[derive(Clone, Debug)]
pub struct MaterializedPublication {
    /// Reopened semantic publication proof produced by compilation/publication.
    pub publication: VerifiedSemanticPublication,
    /// Locators verified by reopening the canonical semantic image.
    pub entities: Vec<VerifiedCanonicalEntityLocator>,
}

impl MaterializedPublication {
    fn catalog_publication<'entry>(
        &'entry self,
        entry: &'entry crate::crates_io::SparseEntry,
    ) -> Result<CatalogPublication<'entry>, RegistryError> {
        let lineage =
            PackageLineage::new("cargo", &entry.package).map_err(|_| RegistryError::Coordinate)?;
        let version = PackageVersion::new(&entry.version).map_err(|_| RegistryError::Coordinate)?;
        Ok(CatalogPublication {
            package: PackageCoordinate::new(lineage, version),
            publication: self.publication,
            entities: &self.entities,
        })
    }
}

/// Explicit bridge to the compiler/materialization lane. Acquisition never fabricates locators.
pub trait RegistryMaterializer {
    /// Materializes one verified source archive into compiler-verified semantic image locators.
    type Error: std::error::Error + 'static;
    /// Produces the only catalog-admissible semantic publication from the verified archive.
    fn materialize(
        &mut self,
        archive: VerifiedArchive<'_>,
    ) -> Result<MaterializedPublication, Self::Error>;
}

/// Executes verified acquisition, injected materialization, deterministic reconciliation projection,
/// and the catalog's atomic per-feed page apply.
pub struct RegistryIngestor<T, M> {
    adapter: CratesIoAdapter<T>,
    materializer: M,
}

/// Outcome of one feed attempt.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum IngestOutcome {
    /// A conditional request established that no page changed.
    NoChange,
    /// A page was atomically published and its own feed checkpoint advanced.
    Applied(FeedCheckpoint),
}

/// Errors across materialization and catalog commit. No source state advances on either failure.
#[derive(Debug, Error)]
pub enum IngestError<E> {
    /// Acquisition/source verification failed.
    #[error(transparent)]
    Registry(#[from] RegistryError),
    /// The injected compiler/materializer rejected the verified archive.
    #[error(transparent)]
    Materialization(E),
    /// Catalog application failed and its transaction rolled back.
    #[error(transparent)]
    Catalog(#[from] CatalogPageError),
}

impl<T: RegistryTransport, M: RegistryMaterializer> RegistryIngestor<T, M> {
    /// Pairs an owned adapter and explicit materialization lane.
    pub const fn new(adapter: CratesIoAdapter<T>, materializer: M) -> Self {
        Self {
            adapter,
            materializer,
        }
    }

    /// Polls one source page and commits only after every archive has verified and materialized.
    pub async fn poll_and_apply(
        &mut self,
        catalog: &mut TursoCatalog,
        cancelled: &AtomicBool,
    ) -> Result<IngestOutcome, IngestError<M::Error>> {
        let current = catalog
            .feed_checkpoint(self.adapter.feed().clone())
            .await
            .map_err(CatalogPageError::from)?;
        let PollResult::Page(page) = self.adapter.poll(&current, cancelled)? else {
            return Ok(IngestOutcome::NoChange);
        };
        let stored = catalog
            .feed_observations(self.adapter.feed())
            .await
            .map_err(CatalogPageError::from)?;
        let active_observations: BTreeSet<_> = stored
            .iter()
            .filter(|prior| prior.active)
            .map(|prior| (&*prior.package, &*prior.version, prior.checksum))
            .collect();
        let mut materialized = Vec::with_capacity(page.entries.len());
        for entry in &page.entries {
            if !entry.active {
                continue;
            }
            if active_observations.contains(&(
                entry.package.as_str(),
                entry.version.as_str(),
                FeedContentChecksum::from_bytes(entry.checksum.as_bytes()),
            )) {
                continue;
            }
            let archive = self.adapter.fetch_archive(entry, cancelled)?;
            let row = self
                .materializer
                .materialize(archive)
                .map_err(IngestError::Materialization)?;
            materialized.push((entry, row));
        }
        let operations = reconciled_operations(&materialized)?;
        let mut observations = Vec::with_capacity(page.entries.len());
        for entry in &page.entries {
            let lineage = PackageLineage::new("cargo", &entry.package)
                .map_err(|_| RegistryError::Coordinate)?;
            let version =
                PackageVersion::new(&entry.version).map_err(|_| RegistryError::Coordinate)?;
            observations.push(server_index_catalog::FeedObservation {
                coordinate: PackageCoordinate::new(lineage, version),
                checksum: FeedContentChecksum::from_bytes(entry.checksum.as_bytes()),
                active: entry.active,
            });
        }
        let cycle = if current.snapshot.as_ref() == Some(&page.snapshot) {
            current.cycle
        } else {
            current.cycle.checked_add(1).ok_or(RegistryError::Cursor)?
        };
        let next = FeedCheckpoint {
            feed: current.feed.clone(),
            checkpoint: increment(current.checkpoint)?,
            cursor: Some(page.next_cursor),
            validator: page.validator,
            cycle,
            snapshot: (!page.complete).then_some(page.snapshot),
            offset: if page.complete { 0 } else { page.next_offset },
        };
        let applied = catalog
            .apply_feed_snapshot_page(&current, &next, &operations, &observations, page.complete)
            .await?;
        Ok(IngestOutcome::Applied(applied))
    }
}

fn increment(current: Checkpoint) -> Result<Checkpoint, RegistryError> {
    Ok(Checkpoint {
        sequence: current.sequence,
        page: current.page.checked_add(1).ok_or(RegistryError::Cursor)?,
    })
}

fn reconciled_operations<'entry>(
    rows: &'entry [(
        &'entry crate::crates_io::SparseEntry,
        MaterializedPublication,
    )],
) -> Result<Vec<CatalogPageOperation<'entry>>, RegistryError> {
    rows.iter()
        .map(|(entry, row)| {
            row.catalog_publication(entry)
                .map(CatalogPageOperation::Publish)
        })
        .collect()
}
