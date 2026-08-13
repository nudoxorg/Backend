//! The [`IrSource`] trait and its supporting types.
//!
//! # Design: one trait, two implementations
//!
//! Per LR-5, there are exactly two in-tree `IrSource` implementations:
//!
//! * [`FixtureSource`] — deterministic, no clock, no filesystem. Every test
//!   and dev-mode screen uses this path so that results are reproducible.
//! * [`ProducerSource`] — drives `nudox_producer::produce` on the Tokio
//!   blocking pool. Rust (`nudox-producer-rust`) is the pilot; the other six
//!   languages register behind `ProducerRegistry` keyed by `Language`.
//!
//! The trait returns a [`BoxStream`] rather than a `Vec` so that a 250-package
//! workspace can surface its first `PackageView` before the last producer
//! finishes. The GUI paints incrementally; blocking on the full load would
//! violate LR-10 ("local is the truth; remote is an enrichment that may be
//! absent").
//!
//! # Error isolation
//!
//! A `SourceError` from one package is surfaced as `LoadEvent::Failed` and the
//! stream continues (LR-10). The only terminal error is a stream-level failure
//! that makes it impossible to enumerate packages at all; everything else is
//! per-package and non-fatal.

#[cfg(any(test, feature = "fixtures"))]
pub mod fixtures;
pub mod producer;

use std::sync::Arc;

use futures::stream::BoxStream;
use nudox_ir::change::PackageLineageId;

use crate::package::PackageView;

// ---------------------------------------------------------------------------
// SourceDescriptor
// ---------------------------------------------------------------------------

/// Static description of what an [`IrSource`] covers.
///
/// Returned by [`IrSource::describe`] so the engine can label sources in
/// the UI and in logs without driving them.
#[derive(Debug, Clone)]
pub struct SourceDescriptor {
    /// Human-readable label shown in the GUI's package-list header.
    pub label: String,
    /// The number of packages this source will produce, if known ahead of time.
    ///
    /// `None` means "unknown"; the GUI draws a spinner rather than a progress
    /// bar in that case.
    pub package_count_hint: Option<u32>,
}

// ---------------------------------------------------------------------------
// LoadRequest
// ---------------------------------------------------------------------------

/// A request to load (or reload) a set of packages from an [`IrSource`].
///
/// The `filter` field is currently unused but is present so that future
/// partial-reload requests ("only reload this one package") are not a
/// breaking change.
#[derive(Debug, Clone, Default)]
pub struct LoadRequest {
    /// If non-empty, only load packages whose lineage is in this list.
    ///
    /// An empty slice means "load everything the source knows about".
    pub filter: Vec<PackageLineageId>,
}

// ---------------------------------------------------------------------------
// PackageHint
// ---------------------------------------------------------------------------

/// Lightweight metadata emitted in [`LoadEvent::Discovered`] before the full
/// `IrView` is ready.
///
/// Enough to render a placeholder row in the package list (LR-10: paint the
/// skeleton before the data arrives).
#[derive(Debug, Clone)]
pub struct PackageHint {
    /// The package's display name (usually the ecosystem package name).
    pub display_name: String,
    /// The ecosystem this package belongs to (`"cargo"`, `"npm"`, …).
    pub ecosystem: String,
    /// The version string, if known at discovery time.
    pub version: Option<String>,
}

// ---------------------------------------------------------------------------
// ProduceStage
// ---------------------------------------------------------------------------

/// A coarse progress stage within one package's production pipeline.
///
/// Emitted via [`LoadEvent::Progress`] to drive the pipeline-dot animation
/// in GUI-PLAN §19. `#[non_exhaustive]` so that adding a new stage (e.g. a
/// tree-sitter pass before the oracle) is not a breaking change.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum ProduceStage {
    /// The oracle subprocess has been spawned (or the in-process oracle
    /// has started).
    OracleRunning,
    /// The oracle output is being decoded and lowered into IR declarations.
    Lowering,
    /// The declaration table is being sealed into a `PristineIntroTable`.
    Sealing,
    /// Derived indexes (`PackageIndexes`) are being built from the sealed IR.
    Indexing,
}

// ---------------------------------------------------------------------------
// SourceError
// ---------------------------------------------------------------------------

/// Everything that can go wrong while producing or loading an `IrView`.
///
/// `#[non_exhaustive]` so that new error categories (e.g. network fetch
/// failures for remote packages) can be added without breaking existing
/// `match` arms.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum SourceError {
    /// The language toolchain needed to run this producer was not found on
    /// `PATH` or in the configured toolchain directory.
    ///
    /// This is a soft error: the package is skipped and the stream continues
    /// (LR-10). The GUI shows the package in a "toolchain missing" state.
    #[error("toolchain missing for {language} (package {package})")]
    ToolchainMissing {
        /// The language whose toolchain is absent.
        language: nudox_ir::body::Language,
        /// The package that could not be produced as a result.
        package: PackageLineageId,
    },

    /// The producer ran but its oracle exited with a non-zero status.
    #[error("oracle failed for {package}: {detail}")]
    OracleFailed {
        /// The package being produced when the failure occurred.
        package: PackageLineageId,
        /// A human-readable description of the failure (exit code, stderr
        /// excerpt, …).
        detail: String,
    },

    /// The oracle output could not be decoded or lowered into valid IR.
    #[error("lowering failed for {package}: {detail}")]
    LoweringFailed {
        /// The package being produced.
        package: PackageLineageId,
        /// Description of the structural error found by `Lowering::finish`.
        detail: String,
    },

    /// A fixture or other deterministic source produced an internal error.
    ///
    /// This variant should never appear at runtime with the built-in
    /// `FixtureSource`; it is reserved for user-supplied `IrSource` impls.
    #[error("internal source error: {0}")]
    Internal(String),
}

// ---------------------------------------------------------------------------
// LoadEvent
// ---------------------------------------------------------------------------

/// One event in the `IrSource::load` stream.
///
/// `#[non_exhaustive]` so that future progress events (e.g. `Cancelled`) can
/// be added without breaking existing consumers. Consumers must handle the
/// `_` arm.
#[derive(Debug)]
#[non_exhaustive]
pub enum LoadEvent {
    /// The source has identified a package but has not yet produced its IR.
    ///
    /// Emitted as early as possible so the GUI can paint a placeholder row
    /// before the potentially slow oracle pass starts.
    Discovered {
        /// The stable identity of the package being discovered.
        lineage: PackageLineageId,
        /// Lightweight metadata for the placeholder row.
        hint: PackageHint,
    },

    /// A coarse progress update for one package's production pipeline.
    ///
    /// `done` and `total` are producer-defined; `total = 0` means unknown.
    Progress {
        /// The package being produced.
        lineage: PackageLineageId,
        /// Which pipeline stage is currently running.
        stage: ProduceStage,
        /// How many units of work in `stage` have completed.
        done: u32,
        /// Total units of work in `stage`, or `0` if unknown.
        total: u32,
    },

    /// A package has been fully produced and its indexes built.
    ///
    /// After this event the package is queryable via [`Corpus`].
    ///
    /// [`Corpus`]: crate::corpus::Corpus
    Ready {
        /// The fully built, immutable package view.
        package: Arc<PackageView>,
    },

    /// One package failed; the stream continues with remaining packages.
    ///
    /// Consumers must never treat this as a terminal event — more `Ready` or
    /// `Failed` events may follow.
    Failed {
        /// The package that could not be produced.
        lineage: PackageLineageId,
        /// The error that caused the failure.
        error: SourceError,
    },
}

// ---------------------------------------------------------------------------
// IrSource
// ---------------------------------------------------------------------------

/// Where `IrView`s come from.
///
/// Async and streaming: a 250-package workspace must surface its first package
/// without waiting for the last (LR-10, LR-5). Implementations must be
/// `Send + Sync + 'static` so they can be stored in the engine and polled
/// from any Tokio task.
///
/// # Contract
///
/// * `load` MUST emit `Discovered` before `Ready` for every package.
/// * `load` MUST emit `Failed` (not panic, not hang) when a single package
///   fails; the stream must continue for remaining packages.
/// * The stream is terminated by the sender dropping — there is no explicit
///   `Done` event at the trait level (the engine detects stream termination
///   and emits its own `PackageEvent::LoadComplete`).
pub trait IrSource: Send + Sync + 'static {
    /// Describe this source for UI labels and logging.
    fn describe(&self) -> SourceDescriptor;

    /// Begin loading packages matching `req`.
    ///
    /// Returns a [`BoxStream`] of results. Each `Ok(LoadEvent)` is a progress
    /// update or a completed package; each `Err(SourceError)` is a
    /// stream-level failure (distinct from per-package `LoadEvent::Failed`
    /// which is `Ok`).
    fn load(&self, req: LoadRequest) -> BoxStream<'static, Result<LoadEvent, SourceError>>;
}
