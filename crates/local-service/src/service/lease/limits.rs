//! The owner's retention policy for durable subscription leases.

use crate::protocol::ProtocolError;
use backend_client::lease_contract::{LeaseMs, ResetPages};
use std::time::Duration;

const HARD_MAX_ACTIVE: usize = 1024;
const HARD_MAX_TERM: Duration = Duration::from_secs(60 * 60);
const HARD_MAX_RESET_PAGES: u16 = 4096;
const HARD_MAX_RESET_WINDOW: Duration = Duration::from_secs(4 * 60 * 60);

const DEFAULT_ACTIVE: usize = 256;
const DEFAULT_TERM: Duration = Duration::from_secs(5 * 60);
const DEFAULT_RESET_WINDOW: Duration = Duration::from_secs(90 * 60);

/// Owner-local bounds on retained subscription roots and lease lifetimes.
///
/// A lease lives for the exact term the observer requested and is renewed only
/// by a liveness-proving operation. A reset additionally has a page budget and
/// an absolute window that no operation can extend. The defaults admit every
/// request in `backend-client::lease_contract`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SubscriptionLeaseLimits {
    max_active: usize,
    max_duration: Duration,
    reset_pages: ResetPages,
    max_reset_duration: Duration,
}

impl Default for SubscriptionLeaseLimits {
    fn default() -> Self {
        Self {
            max_active: DEFAULT_ACTIVE,
            max_duration: DEFAULT_TERM,
            reset_pages: ResetPages::LIMIT,
            max_reset_duration: DEFAULT_RESET_WINDOW,
        }
    }
}

impl SubscriptionLeaseLimits {
    /// Sets finite owner retention bounds within the hard ceilings.
    ///
    /// This preserves the service's established API: all invalid policy
    /// values map to [`ProtocolError::InvalidLimits`].
    ///
    /// # Errors
    /// Returns [`ProtocolError::InvalidLimits`] for zero or excessive bounds.
    pub fn new(
        max_active: usize,
        max_duration: Duration,
        max_reset_pages: usize,
        max_reset_duration: Duration,
    ) -> Result<Self, ProtocolError> {
        let reset_pages = u16::try_from(max_reset_pages)
            .ok()
            .and_then(ResetPages::new)
            .filter(|pages| pages.get() <= HARD_MAX_RESET_PAGES)
            .ok_or(ProtocolError::InvalidLimits)?;
        if max_active == 0
            || max_active > HARD_MAX_ACTIVE
            || max_duration.is_zero()
            || max_duration > HARD_MAX_TERM
            || max_reset_duration.is_zero()
            || max_reset_duration > HARD_MAX_RESET_WINDOW
        {
            return Err(ProtocolError::InvalidLimits);
        }
        Ok(Self {
            max_active,
            max_duration,
            reset_pages,
            max_reset_duration,
        })
    }

    pub(crate) const fn max_active(self) -> usize {
        self.max_active
    }

    /// Returns the longest lease term a caller may request.
    #[must_use]
    pub const fn max_term(self) -> Duration {
        self.max_duration
    }

    /// Returns the most pages one reset may hydrate, counting the first.
    #[must_use]
    pub const fn max_reset_pages(self) -> u16 {
        self.reset_pages.get()
    }

    /// Returns the absolute window a reset may stay retained.
    #[must_use]
    pub const fn max_reset_window(self) -> Duration {
        self.max_reset_duration
    }

    /// Grants exactly the requested term or refuses it; it is never silently
    /// shortened because not every response carries a term field.
    pub(crate) fn grant_term(self, requested_ms: u64) -> Option<LeaseMs> {
        LeaseMs::new(requested_ms).filter(|term| term.duration() <= self.max_duration)
    }

    pub(crate) const fn reset(self) -> ResetLimits {
        ResetLimits {
            pages: self.reset_pages,
            window: self.max_reset_duration,
        }
    }
}

/// The bounds one reset hydration runs under.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct ResetLimits {
    pub(crate) pages: ResetPages,
    pub(crate) window: Duration,
}

#[cfg(test)]
mod tests {
    use super::*;
    use backend_client::lease_contract::{
        BOOTSTRAP_LEASE, PUBLICATION_LEASE, max_reset_time,
    };

    const SECOND: Duration = Duration::from_secs(1);

    #[test]
    fn defaults_admit_every_term_and_reset_the_observer_requests() {
        let limits = SubscriptionLeaseLimits::default();
        assert_eq!(limits.max_active, 256);
        assert_eq!(limits.max_term(), Duration::from_secs(5 * 60));
        assert_eq!(limits.max_reset_pages(), 2048);
        assert_eq!(limits.max_reset_window(), Duration::from_secs(90 * 60));
        assert_eq!(limits.grant_term(PUBLICATION_LEASE.get()), Some(PUBLICATION_LEASE));
        assert_eq!(limits.grant_term(BOOTSTRAP_LEASE.get()), Some(BOOTSTRAP_LEASE));
        assert!(max_reset_time() <= limits.max_reset_window());
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
    fn policy_limits_keep_the_existing_protocol_error_boundary() {
        for result in [
            SubscriptionLeaseLimits::new(0, SECOND, 1, SECOND),
            SubscriptionLeaseLimits::new(HARD_MAX_ACTIVE + 1, SECOND, 1, SECOND),
            SubscriptionLeaseLimits::new(1, Duration::ZERO, 1, SECOND),
            SubscriptionLeaseLimits::new(1, HARD_MAX_TERM + SECOND, 1, SECOND),
            SubscriptionLeaseLimits::new(1, SECOND, 0, SECOND),
            SubscriptionLeaseLimits::new(1, SECOND, usize::MAX, SECOND),
            SubscriptionLeaseLimits::new(1, SECOND, 1, Duration::ZERO),
            SubscriptionLeaseLimits::new(1, SECOND, 1, HARD_MAX_RESET_WINDOW + SECOND),
        ] {
            assert_eq!(result, Err(ProtocolError::InvalidLimits));
        }
    }
}
