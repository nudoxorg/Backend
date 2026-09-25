//! One retrieval hit and the signals the cascade reads.

use heart::ecosystem::Language;
use smol_str::SmolStr;

use super::super::{gates::GateFlags, popularity::PopularitySignals};

/// One retrieval hit plus the signals ranking needs.
///
/// The `item` field is opaque payload — ranking never inspects it.
#[derive(Debug, Clone)]
pub struct Candidate<T> {
    /// Caller's record / id.  Opaque to the ranking module.
    pub item: T,
    /// Canonical name (used for exact/contains bonus and tiebreaking).
    pub name: String,
    /// Raw BM25 relevance score from tantivy.
    pub bm25: f32,
    /// Quality signal in `0.0..=1.0` (analogous to `crate_base_score` /
    /// `crate_score` in lib.rs).
    pub quality: f32,
    /// Monthly download count, calibrated by the ecosystem's `downloads_scale`.
    ///
    /// `None` when the ecosystem has no download source (see
    /// `SearchNorms::downloads_scale`). Downloads-driven ranking stages
    /// (pull-up, bubble) use the **fairness floor** for `None` candidates
    /// so they are never penalized relative to ecosystems that have data.
    pub downloads: Option<u64>,
    /// Corpus-wide reverse-dependency count from the periodic sweep, if
    /// computed. The ecosystem-fair popularity signal: same methodology in
    /// every registry, no upstream API, harder to game than downloads.
    pub dependents: Option<u32>,
    /// Per-ecosystem popularity percentile in `0.0..=1.0` when the offline CDF
    /// job has filled it. Preferred over raw downloads in
    /// [`Self::popularity_weight`] when present.
    pub popularity_pct: Option<f32>,
    /// This version is withdrawn/yanked on its registry — graded demotion,
    /// never a binary visibility cut (a deprecated-but-only-option must
    /// still surface).
    pub withdrawn: bool,
    /// Typosquat / name land-grab suspect — skips exact-name bonus and applies
    /// the squat gate factor after fuse.
    pub squat_suspect: bool,
    /// Known or flagged malware — buried via the malware gate factor after
    /// fuse.
    pub malware: bool,
    /// Declared repository path contains the package name (soft signal).
    pub verified_repo: bool,
    /// The ecosystem this candidate belongs to; used to select the correct
    /// per-ecosystem `strip_conventions` function for the contains-name bonus
    /// (R1).
    pub ecosystem: Language,
    /// Normalised keywords for the diversity pass.
    pub keywords: Vec<SmolStr>,
}

impl<T> Candidate<T> {
    /// Safety flags for hard spam / squat / malware gates.
    #[must_use]
    pub fn gate_flags(&self) -> GateFlags {
        GateFlags {
            squat_suspect: self.squat_suspect,
            malware: self.malware,
            verified_repo: self.verified_repo,
        }
    }

    /// Popularity signals for this candidate (calibrated downloads already on
    /// [`Self::downloads`]; percentile left `None` until the offline job fills
    /// it).
    pub fn popularity_signals(&self) -> PopularitySignals {
        PopularitySignals {
            downloads: self.downloads,
            dependents: self.dependents,
            popularity_pct: self.popularity_pct,
        }
    }

    /// The popularity weight for downloads-calibrated stages.
    ///
    /// Delegates to [`PopularitySignals::effective_weight`] so percentile /
    /// dependents / downloads / floor logic lives in one place. With neither
    /// signal, the stage's fairness floor applies (missing data is never a
    /// zero-penalty vs packages at the floor).
    pub fn popularity_weight(&self, floor: u64) -> u64 {
        let weight = self.popularity_signals().effective_weight(floor);
        if self.downloads.is_none() && self.dependents.is_none() {
            static ONCE: std::sync::Once = std::sync::Once::new();
            ONCE.call_once(|| {
                tracing::debug!(
                    "ranking: downloads None for at least one candidate (ecosystem {:?}); \
					 using fairness floor {} — this is expected for ecosystems without a \
					 download-count endpoint",
                    self.ecosystem,
                    floor,
                );
            });
        }
        weight
    }
}
