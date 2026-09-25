//! The transactional outbox — how stored generations fan out to the derived
//! read-plane stores, now backed by the catalog's outbox table
//! (INDEX-PLAN ID-3, §11 loop 3).
//!
//! When a new package generation is emitted, the registry records one outbox
//! row per downstream sink alongside the business write. Derived stores
//! (tantivy text, vector, usage reverse-index) each poll the outbox from their
//! own watermark and materialize what they missed. Delivery is **at-least-
//! once**: a crash between projection and watermark advance redelivers, so
//! every projection is idempotent by `(package, generation)` and the watermark
//! skip-guard makes redelivery observable rather than silent.
//!
//! The old postgres advisory locks are gone: the catalog has a single writer
//! and one logical drainer per sink (INDEX-PLAN ID-1); in-process serialization
//! is a per-sink mutex ([`Outbox::try_lock_sink`]).

use chrono::{DateTime, TimeZone, Utc};
use std::sync::Arc;

use heart::{PackageId, ResolutionState, content::ContentHash};

use crate::{
    engine::VersioningEngine,
    enums::{OutboxOperation, SinkKind as CatalogSinkKind},
    store::{apply, lifecycle, read, writer::CatalogWriter},
};

use crate::{catalog::GlobalStore, error::OutboxError};

/// What the outbox consumer should do when it sees this entry.
///
/// `Upsert` is the default — it means "materialize this package's symbols into
/// the sink". `Delete` is the mirror-tombstone path: the package was Withdrawn
/// by its upstream registry and search visibility should end.
///
/// **CAS / blob data is never touched by a Delete intent.** The mirror keeps
/// full history; only the search-plane projections lose visibility.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum OutboxOp {
    /// Materialize (upsert) the package's symbols into the sink.
    Upsert,
    /// Remove the package's symbols from the sink (search tombstone). CAS
    /// history is retained — mirror keeps full blob lineage.
    Delete,
}

/// A single fan-out intent: "package `X` changed; sink `K` should materialize."
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct OutboxEntry {
    /// The monotonic outbox sequence id — the watermark cursor pollers advance.
    pub id: OutboxSeq,

    /// The package that changed.
    pub package: PackageId,

    /// Which derived sink this intent is for.
    pub kind: SinkKind,

    /// Whether to upsert or delete the package's search projection.
    pub op: OutboxOp,

    /// When the intent was recorded.
    pub created_at: DateTime<Utc>,
}

/// The monotonic sequence position of an outbox entry — pollers store the last
/// one they consumed as their watermark.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize, serde::Deserialize,
)]
pub struct OutboxSeq(pub i64);

/// Which derived read-plane store a fan-out intent targets.
///
/// This is the shared [`heart::DerivedStore`]; the outbox names it `SinkKind`
/// (its role here is a fan-out sink), but it is the *same type* the server's
/// rebuild path uses, so the two can never drift.
pub use heart::DerivedStore as SinkKind;

/// Map the serving vocabulary onto the catalog's sink enum.
///
/// `Graph` maps to `UsageIndex`: the Terminus graph plane is dead
/// (INDEX-PLAN §14) and the third projection is the usage reverse-index.
pub fn catalog_sink(kind: SinkKind) -> CatalogSinkKind {
    match kind {
        SinkKind::Text => CatalogSinkKind::Text,
        SinkKind::Vector => CatalogSinkKind::Vector,
        SinkKind::Graph => CatalogSinkKind::UsageIndex,
    }
}

fn serving_sink(kind: CatalogSinkKind) -> SinkKind {
    match kind {
        CatalogSinkKind::Text => SinkKind::Text,
        CatalogSinkKind::Vector => SinkKind::Vector,
        CatalogSinkKind::UsageIndex => SinkKind::Graph,
    }
}

/// The transactional outbox over the catalog's single writer.
pub struct Outbox<Engine: VersioningEngine> {
    writer: Arc<CatalogWriter<Engine>>,
    /// One in-process guard per sink — the advisory-lock replacement
    /// (single logical drainer per sink; INDEX-PLAN ID-1).
    sink_guards: [Arc<tokio::sync::Mutex<()>>; 3],
}

impl<Engine: VersioningEngine> Clone for Outbox<Engine> {
    fn clone(&self) -> Self {
        Self {
            writer: Arc::clone(&self.writer),
            sink_guards: self.sink_guards.clone(),
        }
    }
}

impl<Engine: VersioningEngine + Send + Sync> Outbox<Engine> {
    /// Wrap the shared catalog writer.
    pub fn new(writer: Arc<CatalogWriter<Engine>>) -> Self {
        Self {
            writer,
            sink_guards: [
                Arc::new(tokio::sync::Mutex::new(())),
                Arc::new(tokio::sync::Mutex::new(())),
                Arc::new(tokio::sync::Mutex::new(())),
            ],
        }
    }

    fn engine(&self) -> &Engine {
        self.writer.engine()
    }

    fn emit(
        &self,
        package: PackageId,
        snapshot: Option<ContentHash>,
        kind: SinkKind,
        op: OutboxOperation,
    ) -> Result<(), OutboxError> {
        apply::emit_outbox_row(
            self.engine(),
            Some(package),
            snapshot
                .map(|hash| crate::ids::GenerationStamp::from_blob(hash.as_bytes()))
                .transpose()
                .map_err(|_| OutboxError::MissingVersion { seq: -1 })?,
            catalog_sink(kind),
            op,
        )
        .map_err(OutboxError::Catalog)
    }

    /// Fan `package`'s `snapshot` out to `kinds` (one row per sink).
    pub async fn append(
        &self,
        package: PackageId,
        snapshot: ContentHash,
        kinds: &[SinkKind],
    ) -> Result<(), OutboxError> {
        for &kind in kinds {
            self.emit(package, Some(snapshot), kind, OutboxOperation::Upsert)?;
        }
        Ok(())
    }

    /// Record that `package` reached `generation` **and** fan it out to every
    /// derived sink that needs a fresh signal. The `Stored` transition
    /// (lifecycle columns + generation row) and the facets write land first;
    /// the Vector/Graph/Text sink rows follow.
    ///
    /// # Text IS re-emitted here (previously it deliberately was not)
    ///
    /// This used to skip Text, reasoning that the one intent left by the
    /// metadata upsert at `ensure_initialized` track time
    /// (`store::apply::apply_run`'s `CatalogOp::UpsertVersion` arm) was
    /// already enough — a second one here would just be a duplicate the
    /// Text/package-search sink has to re-process for nothing.
    ///
    /// That reasoning silently assumed the *first* intent could always be
    /// materialized into something useful. It cannot: `UpsertVersion` fires
    /// at *registration* time, before compilation has produced any symbols.
    /// `runtime::text::poll::Poller::poll_once` calls `symbols_for` for that
    /// intent, gets `NotFound`, maps it to an empty (but well-formed) batch,
    /// and — like any other intent — advances its durable watermark past it.
    /// Nothing re-signals Text once symbols actually land, because nothing
    /// emitted a *second* intent after they did. Net effect: every freshly
    /// registered package was lexically unsearchable forever, silently (no
    /// error, `/readyz` still green) — the outbox watermark advanced before
    /// the work it represented was actually done, the same defect class as
    /// the other watermark bugs already fixed in this store.
    ///
    /// The fix is symmetry with Vector/Graph: by the time `record_stored`
    /// runs, symbols genuinely exist (this is the terminal `Stored`
    /// transition, downstream of compile), so this Text intent — unlike the
    /// registration-time one — is never a no-op. The registration-time emit
    /// stays too: it is still the right signal for a metadata-only update
    /// (e.g. a re-registration that only changes description/keywords) that
    /// never touches `Stored` again. A freshly registered package now gets
    /// two Text intents — one empty (registration, before compile), one real
    /// (`Stored`, after) — never zero and never permanently stuck; see
    /// `tests/pipeline_end_to_end.rs::package_is_parsed_once_and_fanned_out`
    /// (updated) and the dedicated regression test
    /// `tests/text_sink_recovers_after_stored.rs`.
    ///
    /// Each step here is still an idempotent upsert, so a crash mid-sequence
    /// re-converges on the next emit — at-least-once, never divergent.
    pub async fn record_stored(
        &self,
        index: &GlobalStore<Engine>,
        package: PackageId,
        snapshot: ContentHash,
        facets: Option<&crate::metadata::SearchFacets>,
        edges: &[crate::record::DepEdge],
    ) -> Result<Option<crate::edge_project::FeedObservation>, OutboxError> {
        index
            .set_state(package, &ResolutionState::Stored { hash: snapshot })
            .await
            .map_err(OutboxError::Index)?;
        if let Some(facets) = facets {
            let (keywords, quality_ppm, extras) =
                crate::schema::catalog_map::facets_to_row(facets).map_err(OutboxError::Map)?;
            lifecycle::set_facets_quiet(
                self.engine(),
                package,
                keywords.as_deref(),
                quality_ppm,
                extras.as_deref(),
            )
            .map_err(OutboxError::Catalog)?;
            if let Some(row) =
                lifecycle::version_record(self.engine(), package).map_err(OutboxError::Catalog)?
            {
                let ecosystem = crate::schema::codec::ecosystem_from_token(&row.3)?;
                let observed = if edges.is_empty() {
                    crate::edge_project::feed_observation(
                        ecosystem,
                        &row.4,
                        &row.0,
                        &facets.dependencies,
                    )
                } else {
                    crate::edge_project::feed_edges(ecosystem, &row.4, &row.0, edges)
                };
                if let Some(observed) = observed {
                    crate::store::apply::write_edge_snapshot(
                        self.engine(),
                        package,
                        &observed.snapshot,
                    )
                    .map_err(OutboxError::Catalog)?;
                    self.emit_stored(package, snapshot)?;
                    return Ok(Some(observed));
                }
            }
        }
        self.emit_stored(package, snapshot)?;
        Ok(None)
    }

    fn emit_stored(&self, package: PackageId, snapshot: ContentHash) -> Result<(), OutboxError> {
        self.emit(
            package,
            Some(snapshot),
            SinkKind::Text,
            OutboxOperation::Upsert,
        )?;
        self.emit(
            package,
            Some(snapshot),
            SinkKind::Vector,
            OutboxOperation::Upsert,
        )?;
        self.emit(
            package,
            Some(snapshot),
            SinkKind::Graph,
            OutboxOperation::Upsert,
        )?;
        Ok(())
    }

    /// Emit `Delete` tombstones for a withdrawn version across every sink.
    pub async fn emit_withdraw_intents_for_version(
        &self,
        package: PackageId,
        snapshot: ContentHash,
    ) -> Result<(), OutboxError> {
        for kind in [SinkKind::Text, SinkKind::Vector, SinkKind::Graph] {
            self.emit(package, Some(snapshot), kind, OutboxOperation::Delete)?;
        }
        Ok(())
    }

    /// Append a single `Delete` tombstone for one sink.
    pub async fn append_delete(
        &self,
        package: PackageId,
        snapshot: ContentHash,
        kind: SinkKind,
    ) -> Result<(), OutboxError> {
        self.emit(package, Some(snapshot), kind, OutboxOperation::Delete)
    }

    /// Read up to `limit` entries for `kind` with sequence beyond `after`.
    pub async fn read_since(
        &self,
        kind: SinkKind,
        after: OutboxSeq,
        limit: usize,
    ) -> Result<Vec<OutboxEntry>, OutboxError> {
        let rows = read::outbox_read_since(self.engine(), catalog_sink(kind), after.0, limit)
            .map_err(OutboxError::Catalog)?;
        rows.into_iter()
            .map(|row| {
                let package = row
                    .version_id
                    .map(PackageId::from_uuid)
                    .ok_or(OutboxError::MissingVersion { seq: row.seq })?;
                Ok(OutboxEntry {
                    id: OutboxSeq(row.seq),
                    package,
                    kind: serving_sink(row.sink_kind),
                    op: match row.op {
                        OutboxOperation::Upsert => OutboxOp::Upsert,
                        OutboxOperation::Delete => OutboxOp::Delete,
                    },
                    created_at: Utc
                        .timestamp_millis_opt(row.created_at)
                        .single()
                        .unwrap_or_else(Utc::now),
                })
            })
            .collect()
    }

    /// The newest sequence recorded for `kind` (0 when empty).
    pub async fn head(&self, kind: SinkKind) -> Result<OutboxSeq, OutboxError> {
        read::outbox_head(self.engine(), catalog_sink(kind))
            .map(OutboxSeq)
            .map_err(OutboxError::Catalog)
    }

    /// Advance `kind`'s watermark to `seq` (monotonic; regressions are typed
    /// errors from the catalog layer).
    pub async fn advance_watermark(
        &self,
        kind: SinkKind,
        seq: OutboxSeq,
    ) -> Result<(), OutboxError> {
        apply::advance_sink_watermark(
            self.engine(),
            catalog_sink(kind),
            seq.0,
            Utc::now().timestamp_millis(),
        )
        .map_err(OutboxError::Catalog)
    }

    /// Take the in-process drain guard for `kind`, or `None` when another local
    /// drainer holds it. Replaces the pg advisory lock (single process per
    /// deployment drains a sink; INDEX-PLAN single-writer discipline).
    pub async fn try_lock_sink(
        &self,
        kind: SinkKind,
    ) -> Result<Option<SinkLockGuard>, OutboxError> {
        let slot = sink_slot(kind);
        Arc::clone(&self.sink_guards[slot])
            .try_lock_owned()
            .map_or_else(
                |_| Ok(None),
                |permit| {
                    Ok(Some(SinkLockGuard {
                        _permit: permit,
                        kind,
                    }))
                },
            )
    }

    /// Drop outbox rows consumed by every sink. Returns rows removed.
    pub async fn gc_consumed(&self) -> Result<u64, OutboxError> {
        read::outbox_gc(self.engine()).map_err(OutboxError::Catalog)
    }

    /// The stored watermark for `kind` (0 when never advanced).
    pub async fn read_watermark(&self, kind: SinkKind) -> Result<OutboxSeq, OutboxError> {
        read::current_watermark(self.engine(), catalog_sink(kind))
            .map(OutboxSeq)
            .map_err(OutboxError::Catalog)
    }
}

fn sink_slot(kind: SinkKind) -> usize {
    match kind {
        SinkKind::Text => 0,
        SinkKind::Vector => 1,
        SinkKind::Graph => 2,
    }
}

/// Holds the in-process drain right for one sink until dropped or released.
pub struct SinkLockGuard {
    _permit: tokio::sync::OwnedMutexGuard<()>,
    kind: SinkKind,
}

impl SinkLockGuard {
    /// Explicit release (drop also releases; this exists for call-site
    /// clarity).
    pub async fn release(self) -> Result<(), OutboxError> {
        Ok(())
    }

    /// Which sink this guard serializes.
    pub fn kind(&self) -> SinkKind {
        self.kind
    }
}
