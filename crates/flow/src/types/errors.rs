//! Checked flow errors shared by time, row, batch, and operator facades.

use std::fmt;

/// Errors raised while admitting or advancing flow state.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FlowError {
    /// An upper frontier moved backwards.
    FrontierRegressed,
    /// An empty or otherwise invalid frontier was supplied.
    InvalidFrontier,
    /// Since was above upper.
    SinceBeyondUpper,
    /// Since moved backwards.
    SinceRegressed,
    /// A zero signed update was supplied.
    ZeroDiff,
    /// A signed batch failed verification.
    InvalidBatch,
    /// Checked arithmetic overflowed.
    Overflow,
    /// A dependency cycle was admitted.
    DependencyCycle,
    /// A subscriber requested a sequence after the producer.
    Gap,
    /// A subscriber root is no longer retained/current.
    ResetRequired,
    /// A result was not complete at its declared frontier.
    IncompleteFrontier,
    /// A pin was outside the retained frontier.
    InvalidPin,
    /// The bounded observation-lease registry is full or cannot be shrunk
    /// below its current number of leases.
    PinCapacity,
    /// Compaction cannot move past an active pin.
    PinnedFrontier,
    /// The compaction budget was invalid or insufficient for a requested step.
    InvalidCompactionBudget,
    /// A subscriber queue limit was zero and could not hold a reset event.
    InvalidSubscriptionLimit,
    /// A canonical relation root could not be built.
    InvalidRoot,
    /// The requested snapshot predates the retained since frontier.
    ObservationOutsideRetention,
    /// The requested snapshot is beyond the current upper frontier.
    ObservationBeyondUpper,
    /// A reduction received distinct payloads for one unique key.
    ConflictingReduceValue,
    /// A factor derivative order was zero.
    InvalidFactorOrder,
    /// A bounded flow operation exceeded its explicit work budget.
    RecursionWorkLimit,
    /// Retained runs exceeded the debt bound while a merge was stalled.
    CompactionBackpressure,
}

impl fmt::Display for FlowError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::FrontierRegressed => "frontier regressed",
            Self::InvalidFrontier => "invalid execution frontier",
            Self::SinceBeyondUpper => "since frontier is beyond upper",
            Self::SinceRegressed => "since frontier regressed",
            Self::ZeroDiff => "zero signed update",
            Self::InvalidBatch => "invalid signed batch",
            Self::Overflow => "checked flow arithmetic overflow",
            Self::DependencyCycle => "dependency cycle",
            Self::Gap => "subscriber sequence gap",
            Self::ResetRequired => "subscriber reset required",
            Self::IncompleteFrontier => "incomplete frontier",
            Self::InvalidPin => "invalid trace pin",
            Self::PinCapacity => "observation pin capacity exhausted",
            Self::PinnedFrontier => "active pin prevents compaction",
            Self::InvalidCompactionBudget => "invalid compaction budget",
            Self::InvalidSubscriptionLimit => "invalid subscription limit",
            Self::InvalidRoot => "invalid canonical arrangement root",
            Self::ObservationOutsideRetention => "observation predates retained frontier",
            Self::ObservationBeyondUpper => "observation is beyond upper frontier",
            Self::ConflictingReduceValue => "conflicting reduce values",
            Self::InvalidFactorOrder => "invalid factor derivative order",
            Self::RecursionWorkLimit => "recursive rederivation work limit",
            Self::CompactionBackpressure => "compaction backpressure",
        })
    }
}

impl std::error::Error for FlowError {}
