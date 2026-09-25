//! [`FollowerDriver`]: batches a source's [`CatalogOp`]s through the single
//! catalog writer and commits once per batch (INDEX-PLAN ID-4), advancing the
//! watermark **only** after the commit succeeds.
//!
//! # The atomicity contract (adversarial requirement)
//!
//! A batch is all-or-nothing:
//!
//! 1. `apply_ops` runs the whole `ops` slice in **one** engine transaction
//!    (INDEX-PLAN ID-3): if any op fails, every staged row rolls back and the
//!    call returns `Err`.
//! 2. On that error the driver returns without calling `commit_batch` and
//!    **without advancing the watermark**, so the catalog is untouched and the
//!    feed re-delivers the batch on the next poll (ops are idempotent upserts).
//! 3. Only after a successful `apply_ops` **and** a successful `commit_batch`
//!    does the driver persist `next_watermark`. A crash between commit and
//!    watermark-persist re-delivers one batch — safe, because the ops upsert.
//!
//! The driver is generic over the versioning engine `E` rather than naming
//! [`crate::engine::Configured`], so it is written against the *contract* and
//! not against whichever engine a build selected. Every build and every test
//! instantiates it with the real DoltLite writer.

use crate::{
    engine::{CatalogEngine, VersioningEngine},
    protocol::{CatalogOp, VersionDelta, VersionRecordWire},
    store::{Catalog, MetaStore, writer::CatalogWriter},
};

use crate::ingest::{
    follower::{Follower, FollowerError},
    git::GitRepository,
    homebrew::HomebrewFollower,
    monitor::{GitMonitor, MonitorError, TickOutcome},
    transport::HttpTransport,
    watermark::{FileWatermarkStore, GitWatermark, WatermarkError, WatermarkStore},
};

/// Add advisory listings for catalog versions an OSV event range covers.
pub fn expand_advisory_ranges<E: CatalogEngine>(
    engine: &E,
    ops: &[CatalogOp],
) -> Result<Vec<CatalogOp>, crate::store::MetaError> {
    use crate::{
        ingest::advisory::{AdvisorySource, range_listings_for},
        store::read::version_snapshots,
    };

    let mut expanded = Vec::with_capacity(ops.len());
    for op in ops {
        expanded.push(op.clone());
        let CatalogOp::UpsertAdvisory { advisory } = op else {
            continue;
        };
        let Some(stem) = advisory.stem_id else {
            continue;
        };
        let snaps = version_snapshots(engine, stem)?;
        let known: Vec<_> = snaps
            .iter()
            .map(|snap| (snap.version_id, snap.version_canonical.as_str()))
            .collect();
        let source = AdvisorySource {
            upstream_id: advisory.upstream_id.clone(),
            stem_id: advisory.stem_id,
            version_range: advisory.version_range.clone(),
            severity: advisory.severity.clone(),
            summary: advisory.summary.clone(),
            url: advisory.url.clone(),
            valid_from: advisory.valid_from,
            valid_to: advisory.valid_to,
            recorded_at: advisory.recorded_at,
            affected_name: None,
            affected_ecosystem: None,
        };
        expanded.extend(range_listings_for(&source, &known));
    }
    Ok(expanded)
}

/// What one drive step did — for logging and test assertions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DriveOutcome {
    /// The batch committed; `applied` ops and a watermark advance were
    /// persisted.
    Committed {
        /// How many ops were applied in the batch.
        applied: usize,
        /// Whether the feed reported itself caught up.
        caught_up: bool,
    },
    /// The feed answered `304`/no-change: no catalog write, watermark clock
    /// advanced (crawl time only).
    NoChange,
}

/// Why a drive step failed.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// The follower's poll failed (transport / parse / mapping).
    #[error(transparent)]
    Follower(#[from] FollowerError),
    /// The git monitor could not read or enumerate the remote.
    #[error(transparent)]
    Monitor(#[from] MonitorError),
    /// Applying or committing the op batch failed. The watermark was **not**
    /// advanced; the batch re-delivers next poll.
    #[error("catalog batch for feed {feed} failed to commit: {message}")]
    Commit {
        /// The feed whose batch failed.
        feed: String,
        /// The underlying store/engine error message.
        message: String,
    },
    /// Reading or persisting the watermark failed.
    #[error(transparent)]
    Watermark(#[from] WatermarkError),
}

/// Backwards-compatible alias: the drive error (now [`Error`]).
pub use self::Error as DriveError;

/// What one git-monitor drive step did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GitDriveOutcome {
    /// The remote ref digest was unchanged; only the checked-at timestamp
    /// moved.
    NoChange,
    /// A changed ref set was committed and its watermark persisted.
    Committed {
        /// Number of catalog operations applied in the atomic batch.
        applied: usize,
    },
}

/// The live Homebrew composition used by the `ingest` process.
///
/// It owns the real HTTP follower and durable watermark store while borrowing
/// the catalog's single writer. Constructing a new value after a process
/// restart reopens the same watermark files and resumes with its last ETag.
pub struct HomebrewIngestor<'writer, Engine>
where
    Engine: VersioningEngine + Send + Sync,
{
    writer: &'writer CatalogWriter<Engine>,
    watermarks: FileWatermarkStore,
    follower: HomebrewFollower<HttpTransport>,
}

impl<'writer, Engine> HomebrewIngestor<'writer, Engine>
where
    Engine: VersioningEngine + Send + Sync,
{
    /// Open the durable watermark store and configure a live HTTP follower.
    pub fn new(
        writer: &'writer CatalogWriter<Engine>,
        watermark_dir: impl AsRef<std::path::Path>,
        formula_url: impl Into<String>,
    ) -> Result<Self, WatermarkError> {
        Ok(Self {
            writer,
            watermarks: FileWatermarkStore::open(watermark_dir)?,
            follower: HomebrewFollower::with_url(HttpTransport::new(), formula_url),
        })
    }

    /// Poll Homebrew once, committing catalog ops before advancing its ETag.
    pub fn drive_once(&mut self, now_unix_ms: i64) -> Result<DriveOutcome, Error> {
        FollowerDriver::new(self.writer, &self.watermarks).drive_once(&self.follower, now_unix_ms)
    }

    /// The follower's requested steady-state cadence.
    pub fn cadence(&self) -> crate::ingest::follower::PollCadence {
        self.follower.cadence()
    }
}

/// Drives followers against a catalog writer with per-batch commits (ID-4).
pub struct FollowerDriver<'writer, Engine, Watermarks>
where
    Engine: VersioningEngine + Send + Sync,
    Watermarks: WatermarkStore,
{
    writer: &'writer CatalogWriter<Engine>,
    watermarks: &'writer Watermarks,
    facts: Option<&'writer std::sync::Mutex<crate::engine::turso_vc::VersionedCatalog>>,
}

impl<'writer, Engine, Watermarks> FollowerDriver<'writer, Engine, Watermarks>
where
    Engine: VersioningEngine + Send + Sync,
    Watermarks: WatermarkStore,
{
    /// Wrap a writer handle and a watermark store.
    pub fn new(writer: &'writer CatalogWriter<Engine>, watermarks: &'writer Watermarks) -> Self {
        Self {
            writer,
            watermarks,
            facts: None,
        }
    }

    /// Also version each committed package on `facts`.
    ///
    /// The SQL commit still happens first. A versioned write that fails leaves
    /// the watermark where it was, so the next poll retries the upserts.
    #[must_use]
    pub fn with_facts(
        mut self,
        facts: &'writer std::sync::Mutex<crate::engine::turso_vc::VersionedCatalog>,
    ) -> Self {
        self.facts = Some(facts);
        self
    }

    /// Drive one poll of `follower`: read its watermark, poll, apply the batch
    /// atomically, commit, then persist the watermark. See the module contract.
    pub fn drive_once(
        &self,
        follower: &dyn Follower,
        now_unix_ms: i64,
    ) -> Result<DriveOutcome, Error> {
        let feed = follower.feed_id().to_owned();
        let previous = self.watermarks.feed_watermark(&feed)?;

        let batch = follower.poll(previous.as_ref(), now_unix_ms)?;

        if batch.ops.is_empty() {
            // No-change / caught-up-with-nothing-new: advance only the crawl
            // clock. No catalog transaction, so nothing to commit.
            self.watermarks.put_feed_watermark(&batch.next_watermark)?;
            return Ok(DriveOutcome::NoChange);
        }

        // ── Atomic catalog batch (ID-3/ID-4) ─────────────────────────────────
        // apply_ops stages the whole batch in one transaction; a single bad op
        // rolls it all back and we bail *before* committing or advancing the
        // watermark.
        let ops = expand_advisory_ranges(self.writer.engine(), &batch.ops).map_err(|error| {
            Error::Commit {
                feed: feed.clone(),
                message: error.to_string(),
            }
        })?;
        let report = self.writer.apply_ops(&ops).map_err(|error| Error::Commit {
            feed: feed.clone(),
            message: error.to_string(),
        })?;

        self.writer
            .commit_batch(&format!("ingestor: {feed} batch ({} ops)", report.applied))
            .map_err(|error| Error::Commit {
                feed: feed.clone(),
                message: error.to_string(),
            })?;
        self.remember_facts(&ops, &feed)?;

        // Only now that the batch is durable do we advance the watermark.
        self.watermarks.put_feed_watermark(&batch.next_watermark)?;

        Ok(DriveOutcome::Committed {
            applied: report.applied,
            caught_up: batch.caught_up,
        })
    }

    /// Drive one git monitor tick through the catalog writer.
    ///
    /// This is the git equivalent of [`Self::drive_once`]: it reads the durable
    /// git watermark, lets [`GitMonitor`] compute a delta, commits the complete
    /// `SourceMoved` + version changeset atomically, and only then persists the
    /// driver watermark. An unchanged poll performs no catalog write and emits
    /// no outbox work.
    pub fn drive_git_once<Repository: GitRepository>(
        &self,
        monitor: &GitMonitor<Repository>,
        stem: crate::ids::PackageStemId,
        repo_slug: &str,
        repo_url: &str,
        checked_at: i64,
        commit_time: u64,
    ) -> Result<GitDriveOutcome, Error> {
        let previous = self
            .watermarks
            .git_watermark(stem)?
            .and_then(|watermark| watermark.last_rev);
        let prior = self.version_revs(stem)?;
        let outcome = monitor.tick(
            stem,
            repo_slug,
            repo_url,
            previous.as_deref(),
            &prior,
            checked_at,
            commit_time,
        )?;

        match outcome {
            TickOutcome::Unchanged { rev } => {
                self.watermarks.put_git_watermark(&GitWatermark {
                    stem_id: stem,
                    last_rev: Some(rev),
                    last_checked_at: checked_at,
                    last_error: None,
                })?;
                Ok(GitDriveOutcome::NoChange)
            }
            TickOutcome::Moved { rev, ops } => {
                let ops = self.reconcile_version_ops(stem, ops)?;
                let report = self.writer.apply_ops(&ops).map_err(|error| Error::Commit {
                    feed: repo_url.to_owned(),
                    message: error.to_string(),
                })?;
                self.writer
                    .commit_batch(&format!(
                        "git monitor: {repo_slug} ({} ops)",
                        report.applied
                    ))
                    .map_err(|error| Error::Commit {
                        feed: repo_url.to_owned(),
                        message: error.to_string(),
                    })?;
                self.remember_facts(&ops, repo_url)?;
                self.watermarks.put_git_watermark(&GitWatermark {
                    stem_id: stem,
                    last_rev: Some(rev),
                    last_checked_at: checked_at,
                    last_error: None,
                })?;
                Ok(GitDriveOutcome::Committed {
                    applied: report.applied,
                })
            }
        }
    }

    fn remember_facts(&self, ops: &[CatalogOp], feed: &str) -> Result<(), Error> {
        let Some(facts) = self.facts else {
            return Ok(());
        };
        let mut catalog = facts
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        for record in crate::edge_project::records_from_ops(ops) {
            catalog.put_record(&record).map_err(|error| Error::Commit {
                feed: feed.to_owned(),
                message: error.to_string(),
            })?;
        }
        Ok(())
    }

    /// Compare the monitor's current version snapshot with the durable catalog
    /// and emit only typed add/change/remove deltas.
    fn reconcile_version_ops(
        &self,
        stem: crate::ids::PackageStemId,
        ops: Vec<CatalogOp>,
    ) -> Result<Vec<CatalogOp>, Error> {
        use std::collections::BTreeMap;

        let existing = self
            .writer
            .version_snapshots(stem)
            .map_err(|error| Error::Commit {
                feed: format!("stem {stem}"),
                message: error.to_string(),
            })?;
        let existing: BTreeMap<_, _> = existing
            .into_iter()
            .map(|version| (version.version_canonical.clone(), version))
            .collect();
        let mut result = Vec::with_capacity(ops.len());

        for op in ops {
            match op {
                CatalogOp::UpsertVersion {
                    coordinates,
                    published_at,
                    toolchain,
                    license,
                    edges,
                    facets,
                    source,
                } if coordinates.stem_id == stem => {
                    let source_rev = source.as_ref().and_then(|source| source.source_rev.clone());
                    let record = VersionRecordWire {
                        coordinates,
                        published_at,
                        toolchain,
                        license,
                        edges,
                        facets,
                        source,
                    };
                    let canonical = record.coordinates.version_canonical.clone();
                    let delta = match existing.get(&canonical) {
                        None => VersionDelta::Added { version: record },
                        Some(previous) if previous.source_rev != source_rev => {
                            VersionDelta::Changed { version: record }
                        }
                        Some(_) => continue,
                    };
                    result.push(CatalogOp::VersionDelta { delta });
                }
                other => result.push(other),
            }
        }

        Ok(result)
    }

    fn version_revs(
        &self,
        stem: crate::ids::PackageStemId,
    ) -> Result<std::collections::BTreeMap<String, Option<String>>, Error> {
        let existing = self
            .writer
            .version_snapshots(stem)
            .map_err(|error| Error::Commit {
                feed: format!("stem {stem}"),
                message: error.to_string(),
            })?;
        Ok(existing
            .into_iter()
            .map(|version| (version.version_canonical, version.source_rev))
            .collect())
    }
}
