//! The numbers both ends of the durable publication-lease protocol agree on.
//!
//! The owner reclaims leases and reset roots it granted; the observer gives up
//! on a reset it will not wait for. The two rules only compose when they are
//! the same numbers: an owner that reclaims a reset sooner than an observer is
//! willing to wait kills legitimate hydrations, and an owner that allows fewer
//! pages than the observer's row cap makes the cap unreachable. Each such
//! limit is therefore spelled here exactly once and imported by
//! `backend-local-service` and by [`crate::LocalSubscriptionTransport`].
//!
//! Limits that are the owner's own policy (how many leases it retains, the
//! longest term it will grant) live with the owner; this module holds only
//! what the observer also relies on, plus the compile-time proof that the
//! owner's defaults admit every request the observer makes.

use std::num::{NonZeroU16, NonZeroU64};
use std::time::Duration;

/// A lease duration in whole milliseconds, as carried by `Open`, `Resume` and
/// `Renew` requests and by their responses.
///
/// Whole milliseconds are the wire's unit, so a value of this type is exactly
/// what a peer can express: there is no sub-millisecond remainder for an owner
/// and an observer to round differently.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct LeaseMs(NonZeroU64);

impl LeaseMs {
    /// Returns the lease term for a nonzero millisecond count.
    #[must_use]
    pub const fn new(milliseconds: u64) -> Option<Self> {
        match NonZeroU64::new(milliseconds) {
            Some(milliseconds) => Some(Self(milliseconds)),
            None => None,
        }
    }

    /// Returns the term in milliseconds; never zero.
    #[must_use]
    pub const fn get(self) -> u64 {
        self.0.get()
    }

    /// Returns the term as a duration.
    #[must_use]
    pub const fn duration(self) -> Duration {
        Duration::from_millis(self.0.get())
    }

    /// Returns how long a quiet holder may wait before renewing: half the
    /// term, so one lost renewal still leaves a second chance before expiry.
    #[must_use]
    pub const fn renewal_interval(self) -> Duration {
        Duration::from_millis(self.0.get() / 2 + self.0.get() % 2)
    }

    /// Builds a protocol constant. Use it only to initialize a `const`: a zero
    /// literal then fails the build, not a run.
    ///
    /// # Panics
    /// Panics when `milliseconds` is zero, which in a `const` initializer is a
    /// compile-time error.
    #[must_use]
    #[allow(
        clippy::panic,
        reason = "evaluated at compile time: a zero protocol constant is a build error"
    )]
    pub const fn constant(milliseconds: u64) -> Self {
        match Self::new(milliseconds) {
            Some(term) => term,
            None => panic!("protocol lease constant must be nonzero"),
        }
    }
}

/// Event and page credit the publication observer grants per request.
pub const PUBLICATION_CREDIT: usize = 64;

/// Lease term the publication observer requests.
///
/// The owner either grants exactly this term or refuses the request: the
/// response to a first reset page carries no term field, so an owner that
/// silently shortened the grant would leave the observer unable to know it.
pub const PUBLICATION_LEASE: LeaseMs = LeaseMs::constant(10_000);

/// Lease term a one-shot bootstrap hydration requests.
pub const BOOTSTRAP_LEASE: LeaseMs = LeaseMs::constant(30_000);

/// Most rows one reset may describe before the observer refuses to hydrate it.
pub const MAX_RESET_ROWS: u64 = 131_072;

/// Time every reset is allowed before its first descriptor fixes the rest.
pub const RESET_BASE_TIME: Duration = Duration::from_secs(10);

/// Time added to a reset's allowance for each page it needs.
pub const RESET_PAGE_TIME: Duration = Duration::from_secs(2);

/// Pages in one reset hydration; never zero.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct ResetPages(NonZeroU16);

impl ResetPages {
    /// The most pages the observer admits: [`MAX_RESET_ROWS`] at
    /// [`PUBLICATION_CREDIT`] rows a page.
    pub const LIMIT: Self = ResetRows(MAX_RESET_ROWS).pages();

    /// Returns a page count for a nonzero value.
    #[must_use]
    pub const fn new(pages: u16) -> Option<Self> {
        match NonZeroU16::new(pages) {
            Some(pages) => Some(Self(pages)),
            None => None,
        }
    }

    /// Returns the count; never zero.
    #[must_use]
    pub const fn get(self) -> u16 {
        self.0.get()
    }

    /// Returns the time a reset of this many pages is allowed in total:
    /// [`RESET_BASE_TIME`] plus [`RESET_PAGE_TIME`] per page.
    #[must_use]
    #[allow(
        clippy::cast_lossless,
        reason = "`u32::from` is not callable in a const fn; u16 -> u32 is lossless"
    )]
    pub const fn allowance(self) -> Duration {
        RESET_BASE_TIME.saturating_add(RESET_PAGE_TIME.saturating_mul(self.0.get() as u32))
    }
}

/// Rows of one reset, within [`MAX_RESET_ROWS`].
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct ResetRows(u64);

impl ResetRows {
    /// Admits a descriptor's row count, or returns `None` when it is over the
    /// observer's cap.
    #[must_use]
    pub const fn admit(rows: u64) -> Option<Self> {
        if rows > MAX_RESET_ROWS {
            None
        } else {
            Some(Self(rows))
        }
    }

    /// Returns the row count.
    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }

    /// Returns the pages a producer serving [`PUBLICATION_CREDIT`] rows at a
    /// time needs for these rows. An empty root still takes one page, because
    /// the descriptor itself travels on the first.
    #[must_use]
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_lossless,
        reason = "the row cap bounds the page count far below u16::MAX; `From` is not const"
    )]
    pub const fn pages(self) -> ResetPages {
        let pages = self.0.div_ceil(PUBLICATION_CREDIT as u64);
        let pages = if pages == 0 { 1 } else { pages };
        // `admit` bounds `self.0`, so this cannot exceed `u16::MAX`; the
        // `min` keeps the conversion total for a hand-built value anyway.
        let pages = if pages > u16::MAX as u64 {
            u16::MAX as u64
        } else {
            pages
        };
        match NonZeroU16::new(pages as u16) {
            Some(pages) => ResetPages(pages),
            None => ResetPages(NonZeroU16::MIN),
        }
    }
}

/// The longest any admitted reset may take: the allowance of a reset at the
/// row cap.
#[must_use]
pub const fn max_reset_time() -> Duration {
    ResetPages::LIMIT.allowance()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_observer_cap_is_two_thousand_forty_eight_pages_of_sixty_four_rows() {
        assert_eq!(ResetPages::LIMIT.get(), 2_048);
        assert_eq!(MAX_RESET_ROWS, 2_048 * 64);
        assert_eq!(max_reset_time(), Duration::from_secs(10 + 2 * 2_048));
    }

    #[test]
    fn rows_map_to_ceiling_pages_with_a_floor_of_one() {
        let pages = |rows: u64| {
            ResetRows::admit(rows)
                .expect("within the cap")
                .pages()
                .get()
        };
        assert_eq!(pages(0), 1);
        assert_eq!(pages(1), 1);
        assert_eq!(pages(64), 1);
        assert_eq!(pages(65), 2);
        assert_eq!(pages(128), 2);
        assert_eq!(pages(MAX_RESET_ROWS - 1), 2_048);
        assert_eq!(pages(MAX_RESET_ROWS), 2_048);
    }

    #[test]
    fn a_reset_over_the_row_cap_is_refused_not_clamped() {
        assert_eq!(ResetRows::admit(MAX_RESET_ROWS + 1), None);
        assert_eq!(ResetRows::admit(u64::MAX), None);
    }

    #[test]
    fn the_allowance_is_ten_seconds_plus_two_per_page() {
        let allowance = |pages: u16| ResetPages::new(pages).expect("nonzero").allowance();
        assert_eq!(allowance(1), Duration::from_secs(12));
        assert_eq!(allowance(2), Duration::from_secs(14));
        assert_eq!(allowance(u16::MAX), Duration::from_secs(10 + 2 * 65_535));
    }

    #[test]
    fn the_renewal_interval_is_half_the_term_rounded_up() {
        let interval = |ms: u64| LeaseMs::new(ms).expect("nonzero").renewal_interval();
        assert_eq!(interval(10_000), Duration::from_millis(5_000));
        assert_eq!(interval(3), Duration::from_millis(2));
        assert_eq!(interval(1), Duration::from_millis(1));
        assert_eq!(LeaseMs::new(0), None);
        assert_eq!(ResetPages::new(0), None);
    }
}
