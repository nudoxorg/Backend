//! The one-line honest coverage statement every surface prints.
//!
//! The engine reports coverage as a bounded slice of [`Coverage`] values: one
//! unlabelled [`Coverage::Complete`] standing for the lanes that covered their
//! declared scope, plus one labelled entry per lane that did not. A reader
//! needs the opposite shape — one mark per lane, in a fixed order — so this
//! module folds the slice into four [`LaneCoverage`] rows and renders them as
//! a single `~lanes` line:
//!
//! ```text
//! ~lanes exact✓12 names✓12 graph◐3/7 semantic✗ unconfigured
//! ```
//!
//! A lane that the reply never mentioned renders `·`: not covered, not
//! failed, simply not observed. That distinction is the whole point — an empty
//! complete result and an unavailable lane must never look the same.

use backend_library::{Coverage, Lane, Reason};
use core::fmt;
use core::fmt::Write as _;

/// The state one retrieval lane reported.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum LaneState {
    /// The lane covered its declared source scope.
    Complete,
    /// The lane completed a bounded portion of its scope.
    Partial {
        /// Completed shards.
        completed: LaneShards,
        /// Declared shards.
        total: LaneShards,
    },
    /// The lane could not produce an authoritative result.
    Unavailable {
        /// Typed reason the lane produced nothing.
        reason: Reason,
    },
    /// The reply said nothing about this lane.
    Unobserved,
}

/// A bounded shard count inside one lane's coverage.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct LaneShards(u16);

impl LaneShards {
    /// Retains one shard count reported by a lane.
    #[must_use]
    pub const fn new(count: u16) -> Self {
        Self(count)
    }

    /// Returns the shard count.
    #[must_use]
    pub const fn get(self) -> u16 {
        self.0
    }
}

impl fmt::Display for LaneShards {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}", self.0)
    }
}

/// One lane and the state it reported.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct LaneCoverage {
    lane: Lane,
    state: LaneState,
}

impl LaneCoverage {
    /// Returns the lane this row describes.
    #[must_use]
    pub const fn lane(self) -> Lane {
        self.lane
    }

    /// Returns the lane's reported state.
    #[must_use]
    pub const fn state(self) -> LaneState {
        self.state
    }

    /// Returns the stable lowercase lane name shared by every surface.
    #[must_use]
    pub const fn name(self) -> &'static str {
        lane_name(self.lane)
    }

    /// Returns the mark that follows the lane name.
    #[must_use]
    pub fn mark(self, rows: Option<u64>) -> String {
        match self.state {
            LaneState::Complete => rows.map_or_else(|| "✓".to_owned(), |rows| format!("✓{rows}")),
            LaneState::Partial { completed, total } => format!("◐{completed}/{total}"),
            LaneState::Unavailable { reason } => format!("✗ {}", reason_name(reason)),
            LaneState::Unobserved => "·".to_owned(),
        }
    }

    /// Returns whether this lane refused to answer.
    #[must_use]
    pub const fn is_unavailable(self) -> bool {
        matches!(self.state, LaneState::Unavailable { .. })
    }
}

/// Every retrieval lane's state, folded from one reply's coverage slice.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct CoverageLine {
    lanes: [LaneCoverage; 4],
    rows: Option<RowCount>,
}

/// The number of rows a complete lane stands behind.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct RowCount(u64);

impl RowCount {
    /// Retains one observed row count.
    #[must_use]
    pub const fn new(count: u64) -> Self {
        Self(count)
    }

    /// Returns the observed row count.
    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }
}

impl CoverageLine {
    /// Folds one reply's coverage slice into a per-lane statement.
    #[must_use]
    pub fn new(coverage: &[Coverage], rows: Option<u64>) -> Self {
        let complete = coverage.iter().any(|entry| entry.is_complete());
        let lanes = [Lane::Exact, Lane::Names, Lane::Graph, Lane::Semantic]
            .map(|lane| LaneCoverage {
                lane,
                state: lane_state(coverage, lane, complete),
            });
        Self {
            lanes,
            rows: rows.map(RowCount::new),
        }
    }

    /// Returns every lane in stable display order.
    #[must_use]
    pub const fn lanes(&self) -> &[LaneCoverage; 4] {
        &self.lanes
    }

    /// Returns the row count a complete lane stands behind, when one is known.
    #[must_use]
    pub const fn rows(&self) -> Option<RowCount> {
        self.rows
    }

    /// Returns whether any lane refused to answer.
    #[must_use]
    pub fn has_unavailable(&self) -> bool {
        self.lanes.iter().any(|lane| lane.is_unavailable())
    }

    /// Returns whether any lane failed work it was asked to do.
    ///
    /// A lane the deployment never configured is unavailable without having
    /// failed. It is still rendered `✗ unconfigured` on the `~lanes` line, and
    /// [`Self::has_unavailable`] still reports it; it just does not make the
    /// one-word summary claim that something went wrong.
    #[must_use]
    pub fn has_failed_lane(&self) -> bool {
        self.lanes.iter().any(|lane| match lane.state {
            LaneState::Unavailable { reason } => !reason.is_outside_declared_scope(),
            LaneState::Complete | LaneState::Partial { .. } | LaneState::Unobserved => false,
        })
    }

    /// Returns whether every lane covered its declared scope.
    ///
    /// A lane the deployment never configured has no declared scope to cover,
    /// so it neither completes nor withholds completeness.
    #[must_use]
    pub fn is_complete(&self) -> bool {
        self.lanes.iter().all(|lane| match lane.state {
            LaneState::Complete => true,
            LaneState::Unavailable { reason } => reason.is_outside_declared_scope(),
            LaneState::Partial { .. } | LaneState::Unobserved => false,
        })
    }

    /// Returns the single-word readiness summary.
    #[must_use]
    pub fn readiness(&self) -> &'static str {
        if self.has_failed_lane() {
            "unavailable"
        } else if self
            .lanes
            .iter()
            .any(|lane| matches!(lane.state, LaneState::Partial { .. }))
        {
            "indexing"
        } else if self.is_complete() {
            "ready"
        } else {
            "unknown"
        }
    }

    /// Renders the shared `~lanes` line.
    #[must_use]
    pub fn render(&self) -> String {
        let rows = self.rows.map(RowCount::get);
        let mut line = String::from("~lanes");
        for lane in &self.lanes {
            let _ = write!(line, " {}{}", lane.name(), lane.mark(rows));
        }
        line
    }
}

impl fmt::Display for CoverageLine {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.render())
    }
}

fn lane_state(coverage: &[Coverage], lane: Lane, complete: bool) -> LaneState {
    for entry in coverage {
        match *entry {
            Coverage::Partial {
                lane: reported,
                completed,
                total,
            } if reported == lane => {
                return LaneState::Partial {
                    completed: LaneShards::new(completed),
                    total: LaneShards::new(total),
                };
            }
            Coverage::Unavailable {
                lane: reported,
                reason,
            } if reported == lane => return LaneState::Unavailable { reason },
            _ => {}
        }
    }
    if complete {
        LaneState::Complete
    } else {
        LaneState::Unobserved
    }
}

/// Returns the stable lowercase name of one retrieval lane.
#[must_use]
pub const fn lane_name(lane: Lane) -> &'static str {
    match lane {
        Lane::Exact => "exact",
        Lane::Names => "names",
        Lane::Graph => "graph",
        Lane::Semantic => "semantic",
    }
}

/// Returns the stable lowercase name of one lane failure reason.
#[must_use]
pub const fn reason_name(reason: Reason) -> &'static str {
    match reason {
        Reason::NoIndex => "no-index",
        Reason::Unconfigured => "unconfigured",
        Reason::Offline => "offline",
        Reason::Cancelled => "cancelled",
        Reason::Incomplete => "incomplete",
    }
}
