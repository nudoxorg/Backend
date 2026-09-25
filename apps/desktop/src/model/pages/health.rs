//! Health read model: per-lane coverage and ingest progress (gems and seams).

use std::sync::Arc;

/// One language row of the ingest report.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LanguageProgress {
    /// Language name.
    pub language: Arc<str>,
    /// Files that published declarations.
    pub files: u64,
    /// Declarations published.
    pub declarations: u64,
}

/// One unavailable-file reason and how many files it covers.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FaultProgress {
    /// Typed reason, spelled by the producer's enum.
    pub reason: Arc<str>,
    /// Files with this terminal.
    pub files: u64,
}

/// Typed ingest counts for the visible revision.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct IngestModel {
    /// Files the revision selected.
    pub files_discovered: u64,
    /// Files that published declarations.
    pub files_indexed: u64,
    /// Files that published none, with a typed reason.
    pub files_unavailable: u64,
    /// Declarations published.
    pub declarations: u64,
    /// Per-language rows.
    pub languages: Arc<[LanguageProgress]>,
    /// Per-reason unavailable rows.
    pub faults: Arc<[FaultProgress]>,
}

impl IngestModel {
    /// Returns how many of the gem's 12 facets are filled, or `None` when the
    /// revision selected no files (nothing indexed yet is not "complete").
    #[must_use]
    pub fn facets(&self) -> Option<u8> {
        if self.files_discovered == 0 {
            return None;
        }
        let done = self.files_indexed.saturating_add(self.files_unavailable);
        let twelfths = done.saturating_mul(12) / self.files_discovered;
        Some(u8::try_from(twelfths.min(12)).unwrap_or(12))
    }
}

/// Everything the gems, seams, and status bar render about the owner.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HealthModel {
    /// Per-lane coverage (exact, names, graph, semantic).
    pub lanes: backend_present::CoverageLine,
    /// Rows committed by the visible root.
    pub rows: u64,
    /// Ingest counts.
    pub ingest: IngestModel,
    /// Capabilities the owner reports as ready, by family spelling.
    pub ready_capabilities: Arc<[Arc<str>]>,
    /// Capabilities the owner reports as not ready, with their state.
    pub missing_capabilities: Arc<[MissingCapability]>,
}

/// One capability the owner declared but cannot run yet.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MissingCapability {
    /// Capability family, as the owner spells it.
    pub family: Arc<str>,
    /// Lifecycle state (for example `Unavailable(NoManifest)`).
    pub state: Arc<str>,
}
