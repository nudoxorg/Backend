//! The allowance one reset hydration is given, fixed by its first descriptor.
//!
//! An observer cannot know how large a reset is until the first authenticated
//! page arrives, so it starts with the base allowance and lets that page's
//! descriptor fix the rest: 10 s plus 2 s for each page the descriptor's row
//! count needs, with at most 131,072 rows. Progress never moves the deadline.
//! A page that merely arrives, or arrives again, buys no time; only the final
//! certificate and the acknowledgement advance what the observer admits.
//!
//! Socket reads keep their own, independent timeout. This budget is checked
//! between frames, so a single stalled read can overrun it by at most that
//! timeout.

use crate::ClientError;
use crate::lease_contract::{RESET_BASE_TIME, ResetPages, ResetRows};
use crate::monotonic::Deadline;
use std::time::Instant;

/// Why an observer abandons a reset. Closed, so tests assert causes.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ResetFault {
    /// The descriptor claims more rows than the observer will hydrate.
    RowBudget,
    /// A later page's descriptor disagrees with the first on the row count.
    RowsChanged,
    /// The producer served more pages than the descriptor's rows need.
    PageBudget,
    /// The reset's allowance elapsed.
    TimeBudget,
    /// The producer named the continuation it was just asked for.
    RepeatedContinuation,
    /// The monotonic clock cannot represent the reset's deadline.
    ClockUnavailable,
}

impl ResetFault {
    pub(crate) const fn message(self) -> &'static str {
        match self {
            Self::RowBudget => "publication reset exceeds its row budget",
            Self::RowsChanged => "publication reset changed its row budget",
            Self::PageBudget => "publication reset exceeds its page budget",
            Self::TimeBudget => "publication reset exceeded its time budget",
            Self::RepeatedContinuation => "publication reset repeated its continuation",
            Self::ClockUnavailable => "publication reset deadline is unavailable",
        }
    }
}

impl From<ResetFault> for ClientError {
    fn from(fault: ResetFault) -> Self {
        Self::Protocol(fault.message().to_owned())
    }
}

#[derive(Clone, Copy, Debug)]
enum Allowance {
    /// No descriptor yet: only the base allowance applies.
    Provisional { ends: Deadline },
    /// The first authenticated descriptor fixed the whole reset's allowance.
    Fixed {
        rows: ResetRows,
        pages: ResetPages,
        admitted: u16,
        ends: Deadline,
    },
}

/// Time and page accounting for one reset hydration.
#[derive(Clone, Copy, Debug)]
pub(crate) struct ResetBudget {
    started: Instant,
    allowance: Allowance,
}

impl ResetBudget {
    /// Starts the budget at `now` with only the base allowance.
    pub(crate) fn begin(now: Instant) -> Result<Self, ResetFault> {
        let ends = Deadline::after(now, RESET_BASE_TIME).ok_or(ResetFault::ClockUnavailable)?;
        Ok(Self {
            started: now,
            allowance: Allowance::Provisional { ends },
        })
    }

    pub(crate) fn started(&self) -> Instant {
        self.started
    }

    pub(crate) fn deadline(&self) -> Instant {
        match self.allowance {
            Allowance::Provisional { ends } | Allowance::Fixed { ends, .. } => ends.instant(),
        }
    }

    pub(crate) const fn descriptor(&self) -> Option<(u64, u16)> {
        match self.allowance {
            Allowance::Provisional { .. } => None,
            Allowance::Fixed { rows, pages, .. } => {
                Some((rows.get(), pages.get()))
            }
        }
    }

    /// Admits one authenticated page whose descriptor claims `rows`, then
    /// checks the time. The first page fixes the allowance; every later page
    /// must agree with it and fit inside its page count.
    pub(crate) fn admit_page(&mut self, rows: u64, now: Instant) -> Result<(), ResetFault> {
        let rows = ResetRows::admit(rows).ok_or(ResetFault::RowBudget)?;
        match &mut self.allowance {
            Allowance::Provisional { .. } => {
                let pages = rows.pages();
                let ends = Deadline::after(self.started, pages.allowance())
                    .ok_or(ResetFault::ClockUnavailable)?;
                self.allowance = Allowance::Fixed {
                    rows,
                    pages,
                    admitted: 0,
                    ends,
                };
            }
            Allowance::Fixed { rows: fixed, .. } if *fixed != rows => {
                return Err(ResetFault::RowsChanged);
            }
            Allowance::Fixed { .. } => {}
        }
        if let Allowance::Fixed {
            pages, admitted, ..
        } = &mut self.allowance
        {
            if *admitted >= pages.get() {
                return Err(ResetFault::PageBudget);
            }
            *admitted += 1;
        }
        self.check(now)
    }

    /// Fails once the allowance has elapsed.
    pub(crate) fn check(&self, now: Instant) -> Result<(), ResetFault> {
        let ends = match self.allowance {
            Allowance::Provisional { ends } | Allowance::Fixed { ends, .. } => ends,
        };
        if ends.is_due(now) {
            Err(ResetFault::TimeBudget)
        } else {
            Ok(())
        }
    }

    /// The instant the allowance ends. Exposed so tests can assert that no
    /// amount of progress moves it.
    #[cfg(test)]
    pub(crate) fn ends(&self) -> Deadline {
        match self.allowance {
            Allowance::Provisional { ends } | Allowance::Fixed { ends, .. } => ends,
        }
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use crate::lease_contract::{MAX_RESET_ROWS, PUBLICATION_CREDIT};
    use crate::monotonic::{ManualClock, MonotonicClock};
    use std::time::Duration;

    const SECOND: Duration = Duration::from_secs(1);

    fn begin(clock: &ManualClock) -> ResetBudget {
        ResetBudget::begin(clock.now()).expect("representable")
    }

    #[test]
    fn before_a_descriptor_only_the_base_allowance_applies() {
        let clock = ManualClock::new();
        let budget = begin(&clock);
        clock.advance(RESET_BASE_TIME - Duration::from_nanos(1));
        assert_eq!(budget.check(clock.now()), Ok(()));
        clock.advance(Duration::from_nanos(1));
        assert_eq!(budget.check(clock.now()), Err(ResetFault::TimeBudget));
    }

    #[test]
    fn the_first_descriptor_fixes_ten_seconds_plus_two_per_page() {
        // 256 rows at 64 a page is four pages: 10 s + 4 * 2 s = 18 s.
        let clock = ManualClock::new();
        let mut budget = begin(&clock);
        budget.admit_page(256, clock.now()).expect("first page");
        clock.advance(18 * SECOND - Duration::from_nanos(1));
        assert_eq!(budget.check(clock.now()), Ok(()));
        clock.advance(Duration::from_nanos(1));
        assert_eq!(budget.check(clock.now()), Err(ResetFault::TimeBudget));
    }

    #[test]
    fn a_legitimate_reset_may_outlast_the_base_allowance_but_progress_never_moves_the_deadline() {
        let clock = ManualClock::new();
        let mut budget = begin(&clock);
        budget.admit_page(256, clock.now()).expect("descriptor");
        let fixed = budget.ends();
        // 4 pages, 4 s apart: 12 s in all, well past the 10 s base, inside
        // the 18 s the descriptor earned.
        for _ in 0..3 {
            clock.advance(4 * SECOND);
            budget
                .admit_page(256, clock.now())
                .expect("bounded delayed progress");
            assert_eq!(budget.ends(), fixed, "admitting a page moved the deadline");
        }
        clock.advance(6 * SECOND);
        assert_eq!(budget.check(clock.now()), Err(ResetFault::TimeBudget));
    }

    #[test]
    fn a_page_arriving_after_the_allowance_is_refused_even_if_it_is_well_formed() {
        let clock = ManualClock::new();
        let mut budget = begin(&clock);
        budget
            .admit_page(256, clock.now())
            .expect("first of four pages");
        clock.advance(18 * SECOND);
        assert_eq!(
            budget.admit_page(256, clock.now()),
            Err(ResetFault::TimeBudget),
            "a second page inside the page count but past the deadline"
        );
    }

    #[test]
    fn a_descriptor_over_the_row_cap_is_refused_before_it_fixes_anything() {
        let clock = ManualClock::new();
        let mut budget = begin(&clock);
        assert_eq!(
            budget.admit_page(MAX_RESET_ROWS + 1, clock.now()),
            Err(ResetFault::RowBudget)
        );
        assert_eq!(
            budget.admit_page(u64::MAX, clock.now()),
            Err(ResetFault::RowBudget)
        );
        // Still provisional: a sane descriptor is admitted afterwards.
        assert_eq!(budget.admit_page(64, clock.now()), Ok(()));
    }

    #[test]
    fn a_later_page_that_changes_the_row_count_is_refused() {
        let clock = ManualClock::new();
        let mut budget = begin(&clock);
        budget.admit_page(256, clock.now()).expect("descriptor");
        assert_eq!(
            budget.admit_page(257, clock.now()),
            Err(ResetFault::RowsChanged)
        );
        assert_eq!(
            budget.admit_page(255, clock.now()),
            Err(ResetFault::RowsChanged)
        );
    }

    #[test]
    fn a_producer_cannot_serve_more_pages_than_the_rows_need() {
        let clock = ManualClock::new();
        let mut budget = begin(&clock);
        let rows = u64::try_from(PUBLICATION_CREDIT).expect("credit") * 3;
        for _ in 0..3 {
            budget.admit_page(rows, clock.now()).expect("needed page");
        }
        assert_eq!(
            budget.admit_page(rows, clock.now()),
            Err(ResetFault::PageBudget),
            "a replayed or padded page is a fourth page of a three-page reset"
        );
    }

    #[test]
    fn the_largest_admitted_reset_gets_exactly_its_documented_allowance_and_page_count() {
        let clock = ManualClock::new();
        let mut budget = begin(&clock);
        budget
            .admit_page(MAX_RESET_ROWS, clock.now())
            .expect("largest reset");
        for _ in 1..ResetPages::LIMIT.get() {
            budget
                .admit_page(MAX_RESET_ROWS, clock.now())
                .expect("page within the cap");
        }
        assert_eq!(
            budget.admit_page(MAX_RESET_ROWS, clock.now()),
            Err(ResetFault::PageBudget)
        );
        clock.advance(Duration::from_secs(10 + 2 * 2_048) - Duration::from_nanos(1));
        assert_eq!(budget.check(clock.now()), Ok(()));
        clock.advance(Duration::from_nanos(1));
        assert_eq!(budget.check(clock.now()), Err(ResetFault::TimeBudget));
    }

    #[test]
    fn an_empty_reset_is_one_page() {
        let clock = ManualClock::new();
        let mut budget = begin(&clock);
        assert_eq!(budget.admit_page(0, clock.now()), Ok(()));
        assert_eq!(
            budget.admit_page(0, clock.now()),
            Err(ResetFault::PageBudget)
        );
    }

    #[test]
    fn faults_surface_as_the_protocol_errors_hosts_already_treat_as_reacquire() {
        assert_eq!(
            ClientError::from(ResetFault::TimeBudget),
            ClientError::Protocol("publication reset exceeded its time budget".to_owned())
        );
        assert_eq!(
            ClientError::from(ResetFault::RepeatedContinuation),
            ClientError::Protocol("publication reset repeated its continuation".to_owned())
        );
    }
}
