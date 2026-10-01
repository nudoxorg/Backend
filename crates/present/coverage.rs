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

use backend_library::{
    Coverage, Lane, Reason, SemanticSearchReason, SemanticSearchStatus,
};
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
    semantic_search: Option<SemanticSearchStatus>,
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
            semantic_search: None,
        }
    }

    /// Attaches the independent per-query status of the semantic search lane.
    #[must_use]
    pub const fn with_semantic_search_status(mut self, status: SemanticSearchStatus) -> Self {
        self.semantic_search = Some(status);
        self
    }

    /// Returns the semantic search status, when this is a search result line.
    #[must_use]
    pub const fn semantic_search_status(&self) -> Option<SemanticSearchStatus> {
        self.semantic_search
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

    /// Coverage across separate search pages. A later page cannot turn an
    /// earlier partial or unobserved lane into a claim that the whole query
    /// was complete. Row counts, unlike lane coverage, add across pages.
    #[must_use]
    pub fn across_pages(self, next: Self) -> Self {
        let lanes = std::array::from_fn(|index| {
            let earlier = self.lanes[index];
            let later = next.lanes[index];
            // Unconfigured is outside the declared scope and counts as
            // complete; it must never erase a partial or failed observation.
            let gravity = |state: LaneState| match state {
                LaneState::Complete | LaneState::Unavailable { reason: Reason::Unconfigured } => 0,
                LaneState::Unobserved => 1,
                LaneState::Partial { .. } => 2,
                LaneState::Unavailable { .. } => 3,
            };
            let state = if gravity(earlier.state) >= gravity(later.state) { earlier.state } else { later.state };
            LaneCoverage { lane: earlier.lane, state }
        });
        let rows = match (self.rows, next.rows) {
            (Some(earlier), Some(later)) => earlier.get().checked_add(later.get()).map(RowCount::new),
            _ => None,
        };
        let semantic_search = match (self.semantic_search, next.semantic_search) {
            (Some(earlier), Some(later)) => {
                let severity = |status| match status {
                    SemanticSearchStatus::Available => 0,
                    SemanticSearchStatus::Unavailable {
                        reason: SemanticSearchReason::Unconfigured,
                    } => 1,
                    SemanticSearchStatus::Stale { .. } => 2,
                    SemanticSearchStatus::Unavailable { .. } => 3,
                };
                Some(if severity(earlier) >= severity(later) {
                    earlier
                } else {
                    later
                })
            }
            (earlier, later) => earlier.or(later),
        };
        Self {
            lanes,
            rows,
            semantic_search,
        }
    }

    /// Returns whether any lane refused to answer.
    #[must_use]
    pub fn has_unavailable(&self) -> bool {
        self.lanes.iter().any(|lane| lane.is_unavailable())
            || self
                .semantic_search
                .is_some_and(|status| !matches!(status, SemanticSearchStatus::Available))
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
        }) || self.semantic_search.is_some_and(|status| match status {
            SemanticSearchStatus::Available => false,
            SemanticSearchStatus::Unavailable { reason } => {
                reason != SemanticSearchReason::Unconfigured
            }
            SemanticSearchStatus::Stale { .. } => true,
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
        }) && self.semantic_search.is_none_or(|status| match status {
            SemanticSearchStatus::Available => true,
            SemanticSearchStatus::Unavailable { reason } => {
                reason == SemanticSearchReason::Unconfigured
            }
            SemanticSearchStatus::Stale { .. } => false,
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
        if let Some(status) = self.semantic_search {
            let _ = write!(line, " semantic-search {}", semantic_search_name(status));
        }
        line
    }
}

fn semantic_search_name(status: SemanticSearchStatus) -> String {
    match status {
        SemanticSearchStatus::Available => "available".to_owned(),
        SemanticSearchStatus::Unavailable { reason } => {
            format!("unavailable ({})", semantic_search_reason_name(reason))
        }
        SemanticSearchStatus::Stale { reason } => {
            format!("stale ({})", semantic_search_reason_name(reason))
        }
    }
}

const fn semantic_search_reason_name(reason: SemanticSearchReason) -> &'static str {
    match reason {
        SemanticSearchReason::Unconfigured => "unconfigured",
        SemanticSearchReason::InvalidConfiguration => "invalid-configuration",
        SemanticSearchReason::ProviderUnavailable => "provider-unavailable",
        SemanticSearchReason::NoActiveProjection => "no-active-projection",
        SemanticSearchReason::StaleProjection => "stale-projection",
        SemanticSearchReason::ModelUnavailable => "model-unavailable",
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

#[cfg(test)]
mod page_tests {
    use super::*;

    #[test]
    fn semantic_search_failure_on_any_page_survives_page_combination() {
        let available = CoverageLine::new(&[Coverage::Complete], Some(1))
            .with_semantic_search_status(SemanticSearchStatus::Available);
        let unavailable = CoverageLine::new(&[Coverage::Complete], Some(1))
            .with_semantic_search_status(SemanticSearchStatus::Unavailable {
                reason: SemanticSearchReason::ProviderUnavailable,
            });
        for merged in [
            available.across_pages(unavailable),
            unavailable.across_pages(available),
        ] {
            assert_eq!(
                merged.semantic_search_status(),
                unavailable.semantic_search_status()
            );
            assert!(merged.has_failed_lane());
            assert!(!merged.is_complete());
        }
    }

    #[test]
    fn later_complete_page_cannot_erase_earlier_partial_coverage() {
        let partial = CoverageLine::new(&[Coverage::Partial { lane: Lane::Exact, completed: 1, total: 2 }], Some(2));
        let complete = CoverageLine::new(&[Coverage::Complete], Some(3));
        let merged = partial.across_pages(complete);
        assert!(!merged.is_complete());
        assert!(matches!(merged.lanes()[0].state(), LaneState::Partial { .. }));
        assert_eq!(merged.rows().map(RowCount::get), Some(5));
        assert!(!complete.across_pages(partial).is_complete(), "page order cannot fabricate completeness");
    }

    #[test]
    fn every_page_must_observe_complete_coverage() {
        let complete = CoverageLine::new(&[Coverage::Complete], Some(2));
        let unobserved = CoverageLine::new(&[], Some(1));
        assert!(complete.across_pages(complete).is_complete());
        assert!(!complete.across_pages(unobserved).is_complete());
    }

    #[test]
    fn page_combination_never_erases_incomplete_or_failed_lane() {
        let states = [
            LaneState::Complete,
            LaneState::Unavailable { reason: Reason::Unconfigured },
            LaneState::Unobserved,
            LaneState::Partial { completed: LaneShards::new(1), total: LaneShards::new(2) },
            LaneState::Unavailable { reason: Reason::Offline },
        ];
        let page = |state| {
            let mut complete = CoverageLine::new(&[Coverage::Complete], Some(1));
            complete.lanes[0].state = state;
            complete
        };
        for left in states {
            for right in states {
                let merged = page(left).across_pages(page(right));
                assert_eq!(merged.is_complete(), page(left).is_complete() && page(right).is_complete(),
                    "{left:?} then {right:?}");
                let reverse = page(right).across_pages(page(left));
                assert_eq!(reverse.is_complete(), merged.is_complete(), "{right:?} then {left:?}");
                if matches!(left, LaneState::Unavailable { reason: Reason::Offline })
                    || matches!(right, LaneState::Unavailable { reason: Reason::Offline }) {
                    assert!(merged.has_failed_lane(), "failed lane erased by {left:?} / {right:?}");
                }
            }
        }
    }
}
