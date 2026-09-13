//! Lane coverage and capability inventories, projected into chips.
//! A chip states one lane's or one capability's honest result in three glyphs.
//! An unavailable lane is drawn unavailable, with its reason, never as zero hits.
//!
//! This is the module that keeps the product from lying by omission. Every
//! search sheet, every status bar, and every empty state reads its chips from
//! here, so "no results" and "the semantic lane is not configured in this
//! build" can never render as the same thing.

use super::fault::{lane_name, reason_name};
use crate::theme::language::Language;
use backend_library::{
    CapabilityFamily, CapabilityInventory, CapabilityLifecycle, CapabilityStatus,
    CapabilityUnavailable, Coverage, Lane, LanguageOracleTask, Reason,
};

/// How a lane or capability resolved.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(crate) enum Standing {
    /// Complete, ready, authoritative.
    Complete,
    /// Bounded, probing, degraded but real.
    Partial,
    /// No authoritative result.
    Absent,
}

impl Standing {
    /// Returns the glyph drawn in the chip.
    pub(crate) const fn glyph(self) -> char {
        match self {
            Self::Complete => '✓',
            Self::Partial => '◐',
            Self::Absent => '✗',
        }
    }
}

/// One lane's honest result.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct LaneChip {
    lane: Lane,
    standing: Standing,
    detail: String,
    explanation: String,
}

impl LaneChip {
    /// Projects one coverage value into a chip.
    pub(crate) fn new(coverage: Coverage) -> Self {
        match coverage {
            Coverage::Complete => Self {
                lane: Lane::Exact,
                standing: Standing::Complete,
                detail: String::new(),
                explanation: "This lane covered its declared scope.".to_owned(),
            },
            Coverage::Partial {
                lane,
                completed,
                total,
            } => Self {
                lane,
                standing: Standing::Partial,
                detail: format!("{completed}/{total}"),
                explanation: format!(
                    "{completed} of {total} shards answered before the bound was reached."
                ),
            },
            Coverage::Unavailable { lane, reason } => Self {
                lane,
                standing: Standing::Absent,
                detail: reason_name(reason).to_owned(),
                explanation: explain(reason).to_owned(),
            },
        }
    }

    /// Returns the lane this chip reports.
    pub(crate) const fn lane(&self) -> Lane {
        self.lane
    }

    /// Returns the lane's reader-facing name.
    pub(crate) const fn name(&self) -> &'static str {
        lane_name(self.lane)
    }

    /// Returns how the lane resolved.
    pub(crate) const fn standing(&self) -> Standing {
        self.standing
    }

    /// Returns the short suffix drawn after the glyph, if any.
    pub(crate) fn detail(&self) -> &str {
        &self.detail
    }

    /// Returns the sentence shown on hover.
    pub(crate) fn explanation(&self) -> &str {
        &self.explanation
    }

    /// Returns the chip as one line of text, as the status bar spells it.
    pub(crate) fn line(&self) -> String {
        if self.detail.is_empty() {
            format!("{} {}", self.name(), self.standing.glyph())
        } else {
            format!("{} {} {}", self.name(), self.standing.glyph(), self.detail)
        }
    }
}

/// Projects every declared lane, filling in lanes the root did not mention.
///
/// A root that reports only `Complete` says nothing about which lanes ran. All
/// four lanes are always drawn so the reader can see which ones this build
/// actually has, rather than inferring it from which chips happen to appear.
pub(crate) fn lane_chips(coverage: &[Coverage]) -> Vec<LaneChip> {
    const LANES: [Lane; 4] = [Lane::Exact, Lane::Names, Lane::Graph, Lane::Semantic];
    let complete = coverage.iter().copied().any(Coverage::is_complete);
    LANES
        .into_iter()
        .map(|lane| {
            coverage
                .iter()
                .copied()
                .find(|entry| mentions(*entry, lane))
                .map_or_else(
                    || fallback(lane, complete),
                    |entry| LaneChip {
                        lane,
                        ..LaneChip::new(entry)
                    },
                )
        })
        .collect()
}

fn mentions(coverage: Coverage, lane: Lane) -> bool {
    match coverage {
        Coverage::Complete => false,
        Coverage::Partial { lane: named, .. } | Coverage::Unavailable { lane: named, .. } => {
            named == lane
        }
    }
}

fn fallback(lane: Lane, complete: bool) -> LaneChip {
    if complete {
        return LaneChip {
            lane,
            standing: Standing::Complete,
            detail: String::new(),
            explanation: "This lane covered its declared scope.".to_owned(),
        };
    }
    LaneChip::new(Coverage::Unavailable {
        lane,
        reason: Reason::NoIndex,
    })
}

const fn explain(reason: Reason) -> &'static str {
    match reason {
        Reason::NoIndex => "Nothing has been indexed for this lane yet.",
        Reason::Unconfigured => "This build has no provider configured for this lane.",
        Reason::Offline => "This lane needs remote work and the host is offline.",
        Reason::Cancelled => "The lane was cancelled before it produced a complete result.",
        Reason::Incomplete => "The source facts this lane needs are not complete.",
    }
}

/// One capability's honest state.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct CapabilityChip {
    name: String,
    role: String,
    language: Option<Language>,
    standing: Standing,
    state: String,
    explanation: String,
}

impl CapabilityChip {
    /// Returns the capability's readable name.
    pub(crate) fn name(&self) -> &str {
        &self.name
    }

    /// Returns what the capability does.
    pub(crate) fn role(&self) -> &str {
        &self.role
    }

    /// Returns the language this capability serves, when it serves one.
    pub(crate) const fn language(&self) -> Option<Language> {
        self.language
    }

    /// Returns how the capability resolved.
    pub(crate) const fn standing(&self) -> Standing {
        self.standing
    }

    /// Returns the lifecycle state name.
    pub(crate) fn state(&self) -> &str {
        &self.state
    }

    /// Returns the sentence shown on hover.
    pub(crate) fn explanation(&self) -> &str {
        &self.explanation
    }
}

/// Projects an inventory into chips, one per declared capability.
pub(crate) fn capability_chips(inventory: &CapabilityInventory) -> Vec<CapabilityChip> {
    inventory.as_slice().iter().map(chip).collect()
}

/// Summarises an inventory as ready, probing, and unavailable counts.
pub(crate) fn capability_totals(inventory: &CapabilityInventory) -> (usize, usize, usize) {
    let mut totals = (0usize, 0usize, 0usize);
    for status in inventory.as_slice() {
        match standing_of(status.lifecycle()) {
            Standing::Complete => totals.0 = totals.0.saturating_add(1),
            Standing::Partial => totals.1 = totals.1.saturating_add(1),
            Standing::Absent => totals.2 = totals.2.saturating_add(1),
        }
    }
    totals
}

fn chip(status: &CapabilityStatus) -> CapabilityChip {
    let (name, role, language) = describe(status.family());
    CapabilityChip {
        name,
        role,
        language,
        standing: standing_of(status.lifecycle()),
        state: lifecycle_name(status.lifecycle()).to_owned(),
        explanation: explain_lifecycle(status.lifecycle()),
    }
}

fn describe(family: CapabilityFamily) -> (String, String, Option<Language>) {
    match family {
        CapabilityFamily::StructuralFrontend { profile } => {
            let language = language_of(&format!("{:?}", profile.language()));
            (
                language.label().to_owned(),
                "structural frontend".to_owned(),
                Some(language),
            )
        }
        CapabilityFamily::LanguageOracle { profile, task } => {
            let language = language_of(&format!("{:?}", profile.language()));
            (
                language.label().to_owned(),
                task_name(task).to_owned(),
                Some(language),
            )
        }
        CapabilityFamily::Embedding { .. } => (
            "Embedding".to_owned(),
            "semantic search model".to_owned(),
            None,
        ),
    }
}

const fn task_name(task: LanguageOracleTask) -> &'static str {
    match task {
        LanguageOracleTask::Parse => "parser oracle",
        LanguageOracleTask::TypeCheck => "type checker",
        LanguageOracleTask::SemanticIndex => "semantic index",
    }
}

fn language_of(debug_name: &str) -> Language {
    match debug_name {
        "Rust" => Language::Rust,
        "Python" => Language::Python,
        "TypeScript" => Language::TypeScript,
        "Go" => Language::Go,
        "Java" => Language::Java,
        "CSharp" => Language::CSharp,
        "Clang" => Language::Clang,
        _ => Language::Other,
    }
}

const fn standing_of(lifecycle: CapabilityLifecycle) -> Standing {
    match lifecycle {
        CapabilityLifecycle::Ready => Standing::Complete,
        CapabilityLifecycle::Probing
        | CapabilityLifecycle::Installed
        | CapabilityLifecycle::Resident
        | CapabilityLifecycle::Active => Standing::Partial,
        CapabilityLifecycle::Unavailable(_) | CapabilityLifecycle::Revoked => Standing::Absent,
    }
}

const fn lifecycle_name(lifecycle: CapabilityLifecycle) -> &'static str {
    match lifecycle {
        CapabilityLifecycle::Unavailable(_) => "unavailable",
        CapabilityLifecycle::Probing => "probing",
        CapabilityLifecycle::Installed => "installed",
        CapabilityLifecycle::Resident => "resident",
        CapabilityLifecycle::Active => "active",
        CapabilityLifecycle::Ready => "ready",
        CapabilityLifecycle::Revoked => "revoked",
    }
}

fn explain_lifecycle(lifecycle: CapabilityLifecycle) -> String {
    match lifecycle {
        CapabilityLifecycle::Unavailable(reason) => unavailable_reason(reason).to_owned(),
        CapabilityLifecycle::Probing => {
            "An admitted authority is running its readiness probe.".to_owned()
        }
        CapabilityLifecycle::Installed => {
            "Verified bytes are installed but not yet resident.".to_owned()
        }
        CapabilityLifecycle::Resident => {
            "Installed bytes hold a residence lease but no runtime owns them.".to_owned()
        }
        CapabilityLifecycle::Active => {
            "A runtime owns the bytes but has no fresh readiness proof.".to_owned()
        }
        CapabilityLifecycle::Ready => "A fresh bounded runtime probe succeeded.".to_owned(),
        CapabilityLifecycle::Revoked => "Runtime authority was explicitly revoked.".to_owned(),
    }
}

const fn unavailable_reason(reason: CapabilityUnavailable) -> &'static str {
    match reason {
        CapabilityUnavailable::NoManifest => "No authenticated manifest is configured.",
        CapabilityUnavailable::NotInstalled => "Artifact acquisition has not completed.",
        CapabilityUnavailable::UnsupportedTarget => {
            "The authenticated target is unsupported on this host."
        }
        CapabilityUnavailable::UnsupportedAbi => "The declared protocol ABI is unsupported.",
        CapabilityUnavailable::MissingDependency => {
            "A required model, runtime, or toolchain dependency is missing."
        }
        CapabilityUnavailable::ProbeFailed => "The live readiness probe failed.",
    }
}
