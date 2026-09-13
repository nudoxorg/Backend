//! The whole engine's state, small enough to read in one glance.
//!
//! Today's status answer is a dump of every capability record — roughly 25
//! JSON objects, several thousand tokens, and no sentence a reader can act on.
//! Almost all of it is the same fact repeated: a slot with no manifest. So the
//! model rolls the inventory up into counts per lifecycle with the
//! unavailability reasons gathered behind them:
//!
//! ```text
//! oracles 0/18 ready · 18 no-manifest · embedding unconfigured
//! ```
//!
//! The individual rows are still reachable — `health` in a machine format
//! carries them — but the *answer* is a handful of lines.

use crate::coverage::CoverageLine;
use crate::identity::{KeyTag, ProjectRef};
use backend_library::{
    CapabilityFamily, CapabilityInventory, CapabilityLifecycle, CapabilityStatus,
    CapabilityUnavailable, HealthReport,
};
use core::fmt;

/// A count of capability slots.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct SlotCount(u32);

impl SlotCount {
    /// Retains one slot count.
    #[must_use]
    pub const fn new(count: u32) -> Self {
        Self(count)
    }

    /// Returns the slot count.
    #[must_use]
    pub const fn get(self) -> u32 {
        self.0
    }

    fn increment(&mut self) {
        self.0 = self.0.saturating_add(1);
    }
}

impl fmt::Display for SlotCount {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}", self.0)
    }
}

/// A monotonic owner sequence position.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct Sequence(u64);

impl Sequence {
    /// Retains one owner sequence position.
    #[must_use]
    pub const fn new(sequence: u64) -> Self {
        Self(sequence)
    }

    /// Returns the sequence position.
    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }
}

impl fmt::Display for Sequence {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}", self.0)
    }
}

/// A bounded published-row count.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct PublishedRows(u64);

impl PublishedRows {
    /// Retains one published-row count.
    #[must_use]
    pub const fn new(rows: u64) -> Self {
        Self(rows)
    }

    /// Returns the published-row count.
    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }
}

impl fmt::Display for PublishedRows {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}", self.0)
    }
}

/// One rolled-up family of capability slots.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct FamilyRollup {
    ready: SlotCount,
    total: SlotCount,
}

impl FamilyRollup {
    /// Returns how many slots reported a fresh readiness proof.
    #[must_use]
    pub const fn ready(self) -> SlotCount {
        self.ready
    }

    /// Returns how many slots this family declares.
    #[must_use]
    pub const fn total(self) -> SlotCount {
        self.total
    }

    /// Returns whether the family declares no slot at all.
    #[must_use]
    pub const fn is_empty(self) -> bool {
        self.total.get() == 0
    }

    fn observe(&mut self, lifecycle: CapabilityLifecycle) {
        self.total.increment();
        if matches!(lifecycle, CapabilityLifecycle::Ready) {
            self.ready.increment();
        }
    }
}

impl fmt::Display for FamilyRollup {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}/{} ready", self.ready, self.total)
    }
}

/// How many slots reported one unavailability reason.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct ReasonRollup {
    reason: CapabilityUnavailable,
    count: SlotCount,
}

impl ReasonRollup {
    /// Returns the reason.
    #[must_use]
    pub const fn reason(self) -> CapabilityUnavailable {
        self.reason
    }

    /// Returns how many slots reported it.
    #[must_use]
    pub const fn count(self) -> SlotCount {
        self.count
    }

    /// Returns the stable kebab-case reason name.
    #[must_use]
    pub const fn name(self) -> &'static str {
        unavailable_name(self.reason)
    }
}

/// Returns the stable kebab-case name of one unavailability reason.
#[must_use]
pub const fn unavailable_name(reason: CapabilityUnavailable) -> &'static str {
    match reason {
        CapabilityUnavailable::NoManifest => "no-manifest",
        CapabilityUnavailable::NotInstalled => "not-installed",
        CapabilityUnavailable::UnsupportedTarget => "unsupported-target",
        CapabilityUnavailable::UnsupportedAbi => "unsupported-abi",
        CapabilityUnavailable::MissingDependency => "missing-dependency",
        CapabilityUnavailable::ProbeFailed => "probe-failed",
    }
}

/// The state of the embedding lane, which has exactly one slot.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum EmbeddingState {
    /// A recipe is configured and its slot reported ready.
    Ready,
    /// A recipe is configured and its slot is not ready yet.
    Configured,
    /// No vector-space recipe is configured in this deployment.
    #[default]
    Unconfigured,
    /// The inventory declared no embedding slot.
    Absent,
}

impl EmbeddingState {
    /// Returns the stable lowercase name.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Ready => "ready",
            Self::Configured => "configured",
            Self::Unconfigured => "unconfigured",
            Self::Absent => "absent",
        }
    }
}

/// The whole capability inventory, rolled up.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct CapabilitySummary {
    frontends: FamilyRollup,
    oracles: FamilyRollup,
    embedding: EmbeddingState,
    reasons: Box<[ReasonRollup]>,
}

impl CapabilitySummary {
    /// Rolls one bounded capability inventory up into counts.
    #[must_use]
    pub fn from_inventory(inventory: &CapabilityInventory) -> Self {
        let mut frontends = FamilyRollup::default();
        let mut oracles = FamilyRollup::default();
        let mut embedding = EmbeddingState::Absent;
        let mut reasons: Vec<ReasonRollup> = Vec::new();
        for status in inventory.as_slice() {
            match status.family() {
                CapabilityFamily::StructuralFrontend { .. } => {
                    frontends.observe(status.lifecycle());
                }
                CapabilityFamily::LanguageOracle { .. } => oracles.observe(status.lifecycle()),
                CapabilityFamily::Embedding { recipe } => {
                    embedding = embedding_state(recipe.is_some(), status);
                }
            }
            if let CapabilityLifecycle::Unavailable(reason) = status.lifecycle() {
                count_reason(&mut reasons, reason);
            }
        }
        reasons.sort_by(|left, right| {
            right
                .count
                .cmp(&left.count)
                .then_with(|| left.name().cmp(right.name()))
        });
        Self {
            frontends,
            oracles,
            embedding,
            reasons: reasons.into_boxed_slice(),
        }
    }

    /// Returns the structural frontend rollup.
    #[must_use]
    pub const fn frontends(&self) -> FamilyRollup {
        self.frontends
    }

    /// Returns the language oracle rollup.
    #[must_use]
    pub const fn oracles(&self) -> FamilyRollup {
        self.oracles
    }

    /// Returns the embedding lane state.
    #[must_use]
    pub const fn embedding(&self) -> EmbeddingState {
        self.embedding
    }

    /// Returns the unavailability reasons, most common first.
    #[must_use]
    pub fn reasons(&self) -> &[ReasonRollup] {
        &self.reasons
    }

    /// Renders the one-line capability summary shared by every surface.
    #[must_use]
    pub fn render(&self) -> String {
        let mut parts = Vec::with_capacity(4);
        if !self.frontends.is_empty() {
            parts.push(format!("frontends {}", self.frontends));
        }
        if !self.oracles.is_empty() {
            parts.push(format!("oracles {}", self.oracles));
        }
        for rollup in &self.reasons {
            parts.push(format!("{} {}", rollup.count, rollup.name()));
        }
        parts.push(format!("embedding {}", self.embedding.name()));
        parts.join(" · ")
    }
}

fn embedding_state(configured: bool, status: &CapabilityStatus) -> EmbeddingState {
    match (configured, status.lifecycle()) {
        (true, CapabilityLifecycle::Ready) => EmbeddingState::Ready,
        (true, _) => EmbeddingState::Configured,
        (false, _) => EmbeddingState::Unconfigured,
    }
}

fn count_reason(reasons: &mut Vec<ReasonRollup>, reason: CapabilityUnavailable) {
    if let Some(existing) = reasons.iter_mut().find(|row| row.reason == reason) {
        existing.count.increment();
        return;
    }
    reasons.push(ReasonRollup {
        reason,
        count: SlotCount::new(1),
    });
}

/// The engine's whole state at one immutable revision.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Status {
    project: Option<ProjectRef>,
    revision: KeyTag,
    source: KeyTag,
    sequence: Sequence,
    coverage: CoverageLine,
    rows: PublishedRows,
    capabilities: CapabilitySummary,
}

impl Status {
    /// Lowers one bounded readiness report into a readable status.
    #[must_use]
    pub fn from_report(report: &HealthReport, project: Option<ProjectRef>) -> Self {
        let revision = report.revision();
        Self {
            project,
            revision: KeyTag::from_key(revision.root().as_bytes()),
            source: KeyTag::from_key(report.basis().object.as_bytes()),
            sequence: Sequence::new(revision.cursor().sequence()),
            coverage: CoverageLine::new(report.coverage(), Some(report.row_count())),
            rows: PublishedRows::new(report.row_count()),
            capabilities: CapabilitySummary::from_inventory(report.capabilities()),
        }
    }

    /// Returns the active project, when one is selected.
    #[must_use]
    pub const fn project(&self) -> Option<&ProjectRef> {
        self.project.as_ref()
    }

    /// Returns the current immutable view revision.
    #[must_use]
    pub const fn revision(&self) -> KeyTag {
        self.revision
    }

    /// Returns the source object the view is based on.
    #[must_use]
    pub const fn source(&self) -> KeyTag {
        self.source
    }

    /// Returns the owner's subscription sequence position.
    #[must_use]
    pub const fn sequence(&self) -> Sequence {
        self.sequence
    }

    /// Returns the honest lane coverage.
    #[must_use]
    pub const fn coverage(&self) -> CoverageLine {
        self.coverage
    }

    /// Returns how many rows the visible root committed.
    #[must_use]
    pub const fn rows(&self) -> PublishedRows {
        self.rows
    }

    /// Returns the rolled-up capability inventory.
    #[must_use]
    pub const fn capabilities(&self) -> &CapabilitySummary {
        &self.capabilities
    }

    /// Returns the single-word readiness summary.
    #[must_use]
    pub fn readiness(&self) -> &'static str {
        self.coverage.readiness()
    }
}
