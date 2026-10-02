//! The owner's retention policy for durable subscription leases.

use crate::protocol::ProtocolError;
use backend_client::lease_contract::{
    BOOTSTRAP_LEASE, LeaseMs, PUBLICATION_LEASE, ResetPages, max_reset_time,
};
use std::fmt;
use std::time::Duration;

/// Most leases any configuration may let the owner retain.
const HARD_MAX_ACTIVE: usize = 1024;
/// Longest lease term any configuration may grant: one hour.
const HARD_MAX_TERM: LeaseMs = LeaseMs::constant(60 * 60 * 1000);
/// Most pages any configuration may let one reset hydrate.
const HARD_MAX_RESET_PAGES: u16 = 4096;
/// Longest absolute reset window any configuration may allow: four hours.
const HARD_MAX_RESET_WINDOW: LeaseMs = LeaseMs::constant(4 * 60 * 60 * 1000);

const DEFAULT_ACTIVE: usize = 256;
const DEFAULT_TERM: LeaseMs = LeaseMs::constant(5 * 60 * 1000);
const DEFAULT_RESET_WINDOW: LeaseMs = LeaseMs::constant(90 * 60 * 1000);

// The defaults must admit everything the observer asks for. These are the
// compile-time halves of the producer/observer agreement; the observer's own
// constants live in `backend_client::lease_contract` and are not repeated here.
const _: () = {
    assert!(PUBLICATION_LEASE.get() <= DEFAULT_TERM.get());
    assert!(BOOTSTRAP_LEASE.get() <= DEFAULT_TERM.get());
    assert!(DEFAULT_TERM.get() <= HARD_MAX_TERM.get());
    assert!(DEFAULT_RESET_WINDOW.get() <= HARD_MAX_RESET_WINDOW.get());
    assert!(ResetPages::LIMIT.get() <= HARD_MAX_RESET_PAGES);
    // A reset the observer is willing to wait for must also be one the owner
    // is willing to keep serving.
    assert!(max_reset_time().as_millis() <= DEFAULT_RESET_WINDOW.get() as u128);
};

/// Which owner retention bound was refused.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LeaseLimitError {
    /// The active-lease count is zero or above the hard ceiling.
    ActiveLeases,
    /// The lease term is zero, not whole milliseconds, or above the hard ceiling.
    LeaseTerm,
    /// The reset page budget is zero or above the hard ceiling.
    ResetPages,
    /// The reset window is zero, not whole milliseconds, or above the hard ceiling.
    ResetWindow,
}

impl fmt::Display for LeaseLimitError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::ActiveLeases => "subscription lease count limit is out of range",
            Self::LeaseTerm => "subscription lease term limit is out of range",
            Self::ResetPages => "subscription reset page limit is out of range",
            Self::ResetWindow => "subscription reset window limit is out of range",
        })
    }
}

impl std::error::Error for LeaseLimitError {}

impl From<LeaseLimitError> for ProtocolError {
    fn from(_: LeaseLimitError) -> Self {
        Self::InvalidLimits
    }
}

/// Owner-local bounds on retained subscription roots and lease lifetimes.
///
/// A lease lives for the term the observer negotiated and is renewed only by an
/// operation that proves liveness (see the lease module). A reset additionally
/// carries two bounds the observer cannot extend: a page budget and an absolute
/// window. The defaults admit every request `backend-client` makes.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SubscriptionLeaseLimits {
    active: usize,
    term: LeaseMs,
    reset_pages: ResetPages,
    reset_window: LeaseMs,
}

impl Default for SubscriptionLeaseLimits {
    fn default() -> Self {
        Self {
            active: DEFAULT_ACTIVE,
            term: DEFAULT_TERM,
            // The observer's own cap: the same constant bounds both ends.
            reset_pages: ResetPages::LIMIT,
            reset_window: DEFAULT_RESET_WINDOW,
        }
    }
}

impl SubscriptionLeaseLimits {
    /// Sets finite owner retention bounds within the hard ceilings: 1,024
    /// leases, a one-hour term, 4,096 reset pages and a four-hour reset window.
    /// Durations must be whole milliseconds, the wire's unit.
    ///
    /// # Errors
    /// Returns the first bound that is zero, fractional or above its ceiling.
    pub fn new(
        max_active: usize,
        max_term: Duration,
        max_reset_pages: usize,
        max_reset_window: Duration,
    ) -> Result<Self, LeaseLimitError> {
        if !(1..=HARD_MAX_ACTIVE).contains(&max_active) {
            return Err(LeaseLimitError::ActiveLeases);
        }
        let max_term = whole_millis(max_term, HARD_MAX_TERM).ok_or(LeaseLimitError::LeaseTerm)?;
        let max_reset_pages = u16::try_from(max_reset_pages)
            .ok()
            .filter(|pages| *pages <= HARD_MAX_RESET_PAGES)
            .and_then(ResetPages::new)
            .ok_or(LeaseLimitError::ResetPages)?;
        let max_reset_window = whole_millis(max_reset_window, HARD_MAX_RESET_WINDOW)
            .ok_or(LeaseLimitError::ResetWindow)?;
        Ok(Self {
            active: max_active,
            term: max_term,
            reset_pages: max_reset_pages,
            reset_window: max_reset_window,
        })
    }

    /// Returns the most leases the owner retains at once.
    #[must_use]
    pub const fn max_active(self) -> usize {
        self.active
    }

    /// Returns the longest term the owner grants.
    #[must_use]
    pub const fn max_term(self) -> Duration {
        self.term.duration()
    }

    /// Returns the most pages one reset may hydrate, counting the first.
    #[must_use]
    pub const fn max_reset_pages(self) -> u16 {
        self.reset_pages.get()
    }

    /// Returns the absolute window a reset may stay retained.
    #[must_use]
    pub const fn max_reset_window(self) -> Duration {
        self.reset_window.duration()
    }

    /// Grants the requested term, or refuses it: the owner never silently
    /// shortens a term, because not every response says what was granted.
    pub(crate) fn grant_term(self, requested_ms: u64) -> Option<LeaseMs> {
        LeaseMs::new(requested_ms).filter(|term| *term <= self.term)
    }

    pub(crate) const fn reset(self) -> ResetLimits {
        ResetLimits {
            pages: self.reset_pages,
            window: self.reset_window,
        }
    }
}

/// The bounds one reset hydration runs under.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct ResetLimits {
    pub(crate) pages: ResetPages,
    pub(crate) window: LeaseMs,
}

fn whole_millis(duration: Duration, ceiling: LeaseMs) -> Option<LeaseMs> {
    if !duration.subsec_nanos().is_multiple_of(1_000_000) {
        return None;
    }
    u64::try_from(duration.as_millis())
        .ok()
        .and_then(LeaseMs::new)
        .filter(|term| *term <= ceiling)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SECOND: Duration = Duration::from_secs(1);

    #[test]
    fn the_defaults_are_the_documented_policy_and_admit_the_observer() {
        let limits = SubscriptionLeaseLimits::default();
        assert_eq!(limits.max_active(), 256);
        assert_eq!(limits.max_term(), Duration::from_mins(5));
        assert_eq!(limits.max_reset_pages(), 2048);
        assert_eq!(limits.max_reset_window(), Duration::from_mins(90));
        assert_eq!(
            limits.grant_term(PUBLICATION_LEASE.get()),
            Some(PUBLICATION_LEASE)
        );
        assert_eq!(
            limits.grant_term(BOOTSTRAP_LEASE.get()),
            Some(BOOTSTRAP_LEASE)
        );
    }

    #[test]
    fn the_owner_page_cap_is_the_observer_page_cap() {
        assert_eq!(
            SubscriptionLeaseLimits::default().reset().pages,
            ResetPages::LIMIT
        );
    }

    #[test]
    fn a_term_is_granted_exactly_or_refused_never_shortened() {
        let limits = SubscriptionLeaseLimits::new(1, SECOND, 1, SECOND).expect("limits");
        assert_eq!(limits.grant_term(1), LeaseMs::new(1));
        assert_eq!(limits.grant_term(1000), LeaseMs::new(1000));
        assert_eq!(limits.grant_term(1001), None);
        assert_eq!(limits.grant_term(0), None);
        assert_eq!(limits.grant_term(u64::MAX), None);
    }

    #[test]
    fn every_ceiling_is_inclusive_and_one_past_it_is_refused() {
        let at_ceiling = SubscriptionLeaseLimits::new(
            1024,
            Duration::from_hours(1),
            4096,
            Duration::from_hours(4),
        )
        .expect("ceilings are admitted");
        assert_eq!(at_ceiling.max_active(), 1024);
        assert_eq!(at_ceiling.max_reset_pages(), 4096);
        let refused = |active, term, pages, window| {
            SubscriptionLeaseLimits::new(active, term, pages, window).expect_err("refused")
        };
        assert_eq!(
            refused(1025, SECOND, 1, SECOND),
            LeaseLimitError::ActiveLeases
        );
        assert_eq!(
            refused(1, Duration::from_millis(60 * 60 * 1000 + 1), 1, SECOND),
            LeaseLimitError::LeaseTerm
        );
        assert_eq!(
            refused(1, SECOND, 4097, SECOND),
            LeaseLimitError::ResetPages
        );
        assert_eq!(
            refused(1, SECOND, 1, Duration::from_millis(4 * 60 * 60 * 1000 + 1)),
            LeaseLimitError::ResetWindow
        );
    }

    #[test]
    fn zero_fractional_and_enormous_bounds_are_refused() {
        let refused = |active, term, pages, window| {
            SubscriptionLeaseLimits::new(active, term, pages, window).expect_err("refused")
        };
        assert_eq!(refused(0, SECOND, 1, SECOND), LeaseLimitError::ActiveLeases);
        assert_eq!(
            refused(usize::MAX, SECOND, 1, SECOND),
            LeaseLimitError::ActiveLeases
        );
        assert_eq!(
            refused(1, Duration::ZERO, 1, SECOND),
            LeaseLimitError::LeaseTerm
        );
        assert_eq!(
            refused(1, Duration::from_micros(1500), 1, SECOND),
            LeaseLimitError::LeaseTerm
        );
        assert_eq!(
            refused(1, Duration::MAX, 1, SECOND),
            LeaseLimitError::LeaseTerm
        );
        assert_eq!(refused(1, SECOND, 0, SECOND), LeaseLimitError::ResetPages);
        assert_eq!(
            refused(1, SECOND, usize::MAX, SECOND),
            LeaseLimitError::ResetPages
        );
        assert_eq!(
            refused(1, SECOND, 1, Duration::ZERO),
            LeaseLimitError::ResetWindow
        );
        assert_eq!(
            refused(1, SECOND, 1, Duration::MAX),
            LeaseLimitError::ResetWindow
        );
    }

    #[test]
    fn a_refused_limit_maps_to_the_wire_invalid_limits_error() {
        assert_eq!(
            ProtocolError::from(LeaseLimitError::ResetPages),
            ProtocolError::InvalidLimits
        );
    }
}
