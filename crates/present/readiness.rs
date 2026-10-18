//! The one derivation of "how ready is it" that every surface shares.
//!
//! `status`, `health` and `packages` used to answer the same question from
//! three separate reads: the lane line came from the health report's coverage,
//! the project row from a package row's lifecycle, and the row count from
//! whichever reply happened to carry symbol rows. They could and did disagree
//! — `status` said `539 row(s)` while `packages` said `indexing · 0 row(s)`.
//!
//! [`PublishedFacts`] is the single value both read from: the lane coverage and
//! the published row count of one immutable revision. [`ReadinessState`] is the
//! single vocabulary they print. The owner derives the project row's lifecycle
//! and the lane coverage from the same capture facts, so a project row and the
//! lane line it sits beside differ only in granularity, never in meaning.
//!
//! The words are closed and each has one meaning:
//!
//! * `ready` — every lane covered its declared scope.
//! * `partial` — work finished and published readable rows, with explicit
//!   source gaps that waiting cannot close (for example a source the configured
//!   compiler program excludes). Usable, and never called failure.
//! * `indexing` — work is running; the counts still move.
//! * `unavailable` — requested work was refused, cancelled or interrupted, and
//!   no readable result stands for it.
//! * `unknown` — the reply did not observe every lane.

use crate::coverage::CoverageLine;
use crate::fault::{Affordance, Cause, CauseSlug, Fault, FaultSlug, Operand};
use crate::identity::Coordinate;
use crate::shelf::{Readiness, RowCount};
use backend_library::{Coverage, HealthReport, Lane, RowState};

/// The closed readiness vocabulary shared by every surface.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum ReadinessState {
    /// Every lane covered its declared scope.
    Ready,
    /// Work finished with explicit, settled source gaps; rows are readable.
    Partial,
    /// Work is running and its counts still move.
    Indexing,
    /// Requested work did not finish and no readable result stands for it.
    Unavailable,
    /// The reply did not observe every lane.
    Unknown,
}

impl ReadinessState {
    /// Returns the stable lowercase word every surface prints.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Ready => "ready",
            Self::Partial => "partial",
            Self::Indexing => "indexing",
            Self::Unavailable => "unavailable",
            Self::Unknown => "unknown",
        }
    }
}

/// What the owner published at one immutable revision: the lane coverage it
/// stood behind and the number of rows it committed.
///
/// Both numbers come from the last readable publication. A reply that did not
/// carry the count leaves it absent; nothing here ever turns "this reply did
/// not say" into zero.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PublishedFacts {
    lanes: Option<CoverageLine>,
    rows: Option<RowCount>,
}

impl PublishedFacts {
    /// Facts from a reply that observed neither coverage nor a row count.
    #[must_use]
    pub const fn unread() -> Self {
        Self {
            lanes: None,
            rows: None,
        }
    }

    /// Facts from one coverage slice and, when known, the published row count.
    #[must_use]
    pub fn new(coverage: &[Coverage], rows: Option<u64>) -> Self {
        Self {
            lanes: Some(CoverageLine::new(coverage, rows)),
            rows: rows.map(RowCount::new),
        }
    }

    /// Facts from the owner's bounded readiness report.
    #[must_use]
    pub fn from_health(report: &HealthReport) -> Self {
        Self::new(report.coverage(), Some(report.row_count()))
    }

    /// Returns the folded lane coverage, when the reply carried any.
    #[must_use]
    pub const fn lanes(&self) -> Option<&CoverageLine> {
        self.lanes.as_ref()
    }

    /// Returns the published row count, when the reply carried one.
    #[must_use]
    pub const fn rows(&self) -> Option<RowCount> {
        self.rows
    }
}

/// Derives one project's readiness from its package row and the facts of the
/// same revision.
///
/// The owner's [`RowState`] is the authority and the only thing consulted to
/// decide the word. A declaration count must never decide it: the shelf reply
/// carries package rows and no declarations, so a count of zero means "this
/// reply did not say", not "nothing is indexed". Reading it as the latter is
/// how a project with eighteen readable declarations once rendered as
/// `○ polyglot requested` beside a `health` line that said `ready · 18 row(s)`.
///
/// `sole_project` says whether the revision holds exactly one project. Lane
/// coverage and the row count describe the whole workspace, so they may be
/// attributed to a project only when no other project could share them; with
/// several projects the per-project entry stays silent and the shelf footer
/// carries the workspace statement instead.
#[must_use]
pub(crate) fn project_readiness(
    state: RowState,
    label: &str,
    facts: &PublishedFacts,
    sole_project: bool,
) -> Readiness {
    match state {
        RowState::Ready => Readiness::Ready,
        RowState::Loading => Readiness::Indexing {
            rows: if sole_project { facts.rows() } else { None },
        },
        RowState::Failed => Readiness::Failed {
            fault: unfinished_index(label, if sole_project { facts.lanes() } else { None }),
        },
    }
}

/// Returns the lanes which finished with settled source gaps.
#[must_use]
pub(crate) fn gapped_lanes(facts: &PublishedFacts) -> Vec<Lane> {
    facts.lanes().map_or_else(Vec::new, |line| {
        line.lanes()
            .iter()
            .filter(|lane| matches!(lane.state(), crate::coverage::LaneState::Gapped))
            .map(|lane| lane.lane())
            .collect()
    })
}

/// The fault for a project whose last index did not publish a usable result.
///
/// The package row says only that the last index did not finish; it does not
/// say why, and this sentence does not guess. It names every way that happens,
/// leaves the exact cause to the durable operation record, and offers the one
/// action that always applies: index the project again.
fn unfinished_index(label: &str, lanes: Option<&CoverageLine>) -> Fault {
    let cancelled = lanes.is_some_and(|line| {
        line.lanes().iter().any(|lane| {
            matches!(
                lane.state(),
                crate::coverage::LaneState::Unavailable {
                    reason: backend_library::Reason::Cancelled
                }
            )
        })
    });
    let sentence = if cancelled {
        "indexing was cancelled before it published a readable result; index the project again to retry"
    } else {
        "indexing did not finish; no matching retained compiler diagnostic was read for this revision; inspect the index operation or index the project again to retry"
    };
    Fault::new(
        FaultSlug::Rejected,
        Operand::Coordinate(Coordinate::new(label)),
        Cause::new(CauseSlug::Refused, sentence),
        Affordance::Reindex {
            path: label.to_owned(),
        },
    )
}

#[cfg(test)]
#[allow(clippy::panic)]
mod tests {
    use super::*;
    use backend_library::Reason;

    fn facts(coverage: &[Coverage], rows: u64) -> PublishedFacts {
        PublishedFacts::new(coverage, Some(rows))
    }

    #[test]
    fn each_word_has_one_meaning_and_one_spelling() {
        let names = [
            ReadinessState::Ready,
            ReadinessState::Partial,
            ReadinessState::Indexing,
            ReadinessState::Unavailable,
            ReadinessState::Unknown,
        ]
        .map(ReadinessState::name);
        assert_eq!(
            names,
            ["ready", "partial", "indexing", "unavailable", "unknown"]
        );
    }

    #[test]
    fn a_settled_gap_reads_partial_and_never_indexing_or_unavailable() {
        let line = facts(
            &[
                Coverage::Complete,
                Coverage::Gapped {
                    lane: Lane::Semantic,
                },
            ],
            161,
        );
        let state = line.lanes().map(CoverageLine::state);
        assert_eq!(state, Some(ReadinessState::Partial));
        assert_eq!(gapped_lanes(&line), vec![Lane::Semantic]);
    }

    #[test]
    fn a_moving_fraction_reads_indexing_and_a_refusal_reads_unavailable() {
        let moving = facts(
            &[
                Coverage::Complete,
                Coverage::Partial {
                    lane: Lane::Semantic,
                    completed: 0,
                    total: 2,
                },
            ],
            539,
        );
        assert_eq!(
            moving.lanes().map(CoverageLine::state),
            Some(ReadinessState::Indexing)
        );
        let refused = facts(
            &[
                Coverage::Complete,
                Coverage::Unavailable {
                    lane: Lane::Semantic,
                    reason: Reason::Incomplete,
                },
            ],
            539,
        );
        assert_eq!(
            refused.lanes().map(CoverageLine::state),
            Some(ReadinessState::Unavailable)
        );
    }

    #[test]
    fn indexing_carries_the_published_count_only_when_it_is_known_and_attributable() {
        let known = facts(&[Coverage::Complete], 539);
        let Readiness::Indexing { rows } = project_readiness(RowState::Loading, "/p", &known, true)
        else {
            panic!("a loading row reads indexing");
        };
        assert_eq!(rows.map(RowCount::get), Some(539));
        let Readiness::Indexing { rows } =
            project_readiness(RowState::Loading, "/p", &known, false)
        else {
            panic!("a loading row reads indexing");
        };
        assert_eq!(rows, None, "a workspace count is not one project's count");
        let Readiness::Indexing { rows } =
            project_readiness(RowState::Loading, "/p", &PublishedFacts::unread(), true)
        else {
            panic!("a loading row reads indexing");
        };
        assert_eq!(rows, None, "an unread count is never printed as zero");
    }

    #[test]
    fn a_failed_row_offers_the_retry_and_never_claims_a_specific_cause() {
        let Readiness::Failed { fault } =
            project_readiness(RowState::Failed, "/p", &PublishedFacts::unread(), true)
        else {
            panic!("a failed row reads failed");
        };
        assert_eq!(
            fault.affordance(),
            &Affordance::Reindex {
                path: "/p".to_owned()
            }
        );
        assert!(
            fault
                .cause()
                .sentence()
                .contains("no matching retained compiler diagnostic was read")
        );
    }
}
