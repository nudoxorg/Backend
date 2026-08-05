//! `index::ingest` — registry followers + git monitor + advisory ingestion,
//! feeding the catalog writer over [`CatalogOp`](crate::protocol::CatalogOp)
//! (INDEX-PLAN ID-7, ID-14; REGISTRYLESS-PLAN §7).
//!
//! # Shape
//!
//! This module is **library-first** (INDEX-PLAN ID-7): everything the Remote
//! ingestor process does is a type here, and `ingest/main.rs` is a thin binary
//! that wires those types to a real catalog writer and loops. Embedded
//! deployments link the library directly and drive one tick at a time.
//!
//! # The plane in three layers
//!
//! 1. **Sources of ops.** A [`follower::Follower`] polls one upstream feed (a
//!    registry JSON endpoint, an OSV export) using a [`watermark::FeedWatermark`]
//!    cursor and emits a `Vec<CatalogOp>`. A [`monitor::GitMonitor`] polls one
//!    git stem's refs via a [`git::GitRepository`] adapter and emits
//!    `SourceMoved` + version-enumeration ops. Both are pure over their
//!    transport / adapter, so tests use fixtures.
//! 2. **The driver.** [`driver::FollowerDriver`] batches a source's ops through
//!    a writer handle and calls
//!    [`commit_batch`](crate::store::writer::CatalogWriter::commit_batch) once
//!    per batch (ID-4). A batch is atomic: one bad op ⇒ the whole batch is
//!    dropped and the watermark is **not** advanced (adversarial requirement).
//! 3. **Watermarks.** [`watermark::WatermarkStore`] is the persistence seam for
//!    feed cursors (ETag / last ref) and git high-water marks. The index crate
//!    stores these rows but exposes no read/write path for them yet (see the
//!    crate report); the driver depends on this trait so the seam is swappable.
//!
//! # Style
//!
//! Fully-qualified descriptive names, typed errors (`thiserror`), no `unwrap`
//! outside tests, small files, strong typing (INDEX-PLAN house style).

/// The untrusted-archive extraction plane (moved from `driver::ingest`).
/// Sanitizes hostile archive inputs into content-addressed blobs.
pub mod archive;
pub mod advisory;
pub mod driver;
pub mod enumerate;
pub mod follower;
pub mod git;
pub mod grit;
pub mod homebrew;
pub mod monitor;
pub mod transport;
pub mod watermark;

pub use driver::{DriveOutcome, FollowerDriver};
pub use follower::{Follower, FollowerError, PollCadence};
pub use git::{GitCommandAdapter, GitRepository, GitRepositoryError, LsRemoteRef};
pub use grit::GritAdapter;
pub use monitor::{GitMonitor, MonitorError};

/// The [`git::GitRepository`] implementation monitor/enumeration wiring should
/// construct by default: the in-process grit adapter ([`grit::GritAdapter`]),
/// adopted for its better networking. [`git::GitCommandAdapter`] remains
/// compiled as the subprocess fallback (and the subject of the
/// subprocess-hardening regression tests); switching back is this one alias.
pub type DefaultGitAdapter = grit::GritAdapter;
pub use transport::{FeedRequest, FeedResponse, FeedTransport, TransportError};
pub use watermark::{FeedWatermark, GitWatermark, WatermarkError, WatermarkStore};
