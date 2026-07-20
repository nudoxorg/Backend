//! The single-writer handle (INDEX-PLAN ID-1, ID-4).
//!
//! [`CatalogWriter`] owns a [`VersioningEngine`] positioned on `main`. It is the
//! **only** type in the crate that mutates the catalog: [`MetaStore`] is
//! implemented for `CatalogWriter` and nothing else, so it is impossible at the
//! type level to write outside the writer handle. Reads (the [`Catalog`] trait)
//! are also available on the writer for convenience, delegating to
//! [`read`](crate::store::read).
//!
//! Durability is an explicit batch heartbeat: callers stage mutations with
//! [`MetaStore::apply_ops`] / [`MetaStore::register_generation`] and then call
//! [`CatalogWriter::commit_batch`] after an ingest batch, listing sweep, forge
//! seal, git-monitor update, bakery pass, or timer (ID-4).

use heart::query::AsOf;

use crate::engine::{CommitHash, EngineError, VersioningEngine};

use super::read::{self, CatalogAsOf};
use super::{
    ApplyReport, Catalog, CatalogCursor, ChangedPage, GenerationRegistration, MetaError, MetaStore,
};
use crate::enums::SinkKind;
use crate::ids::PackageStemId;
use crate::protocol::CatalogOp;
use crate::tables::outbox::OutboxRow;
use crate::tables::packages::PackageRow;

/// The sovereign catalog writer on `main`.
///
/// Generic over the versioning engine `E` so the same logic runs against the
/// real DoltLite binding and the in-memory test fake.
pub struct CatalogWriter<E: VersioningEngine> {
    engine: E,
}

impl<E: VersioningEngine> CatalogWriter<E> {
    /// Wrap an engine already positioned on `main`. The caller (deployment
    /// bootstrap) is responsible for opening the engine and, on first launch,
    /// running migrations (see [`crate::migrations`]).
    pub fn new(engine: E) -> Self {
        Self { engine }
    }

    /// Borrow the underlying engine (read-only helpers, tests).
    pub fn engine(&self) -> &E {
        &self.engine
    }

    /// The batch heartbeat commit (ID-4): stage the working set and commit it
    /// with `message`, returning the new head. Callers invoke this after an
    /// ingest batch so a crash never loses more than one un-committed batch.
    pub fn commit_batch(&self, message: &str) -> Result<CommitHash, EngineError> {
        self.engine.dolt_add_all()?;
        self.engine.dolt_commit(message)
    }
}

impl<E: VersioningEngine + Send + Sync> MetaStore for CatalogWriter<E> {
    fn apply_ops(&self, ops: &[CatalogOp]) -> Result<ApplyReport, MetaError> {
        super::apply::apply_ops(&self.engine, ops)
    }

    fn register_generation(
        &self,
        registration: GenerationRegistration,
    ) -> Result<(), MetaError> {
        super::apply::register_generation(&self.engine, registration)
    }

    fn outbox_claim(
        &self,
        sink: SinkKind,
        limit: usize,
    ) -> Result<Vec<OutboxRow>, MetaError> {
        read::outbox_claim(&self.engine, sink, limit)
    }

    fn advance_sink_watermark(
        &self,
        sink: SinkKind,
        last_seq: i64,
        updated_at: i64,
    ) -> Result<(), MetaError> {
        super::apply::advance_sink_watermark(&self.engine, sink, last_seq, updated_at)
    }

    fn current_sink_watermark(&self, sink: SinkKind) -> Result<i64, MetaError> {
        read::current_watermark(&self.engine, sink)
    }

    fn outbox_head(&self, sink: SinkKind) -> Result<i64, MetaError> {
        read::outbox_head(&self.engine, sink)
    }
}

impl<E: VersioningEngine> Catalog for CatalogWriter<E>
where
    E: Send + Sync,
{
    fn get_package(
        &self,
        stem: PackageStemId,
    ) -> Result<Option<PackageRow>, MetaError> {
        read::get_package(&self.engine, stem)
    }

    fn changed_since(&self, cursor: CatalogCursor) -> Result<ChangedPage, MetaError> {
        read::changed_since(&self.engine, cursor)
    }

    fn at(&self, as_of: &AsOf) -> Result<CatalogAsOf, MetaError> {
        read::at(&self.engine, as_of)
    }
}
