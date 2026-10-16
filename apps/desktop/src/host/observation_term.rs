//! Subscription lifetime, producer freshness, and active recovery are separate.
//! Successful control I/O ends work without certifying the producer. Retries
//! retain the same active term; authenticated reset ceilings live in hydration.

use std::time::{Duration, Instant};

#[derive(Default)]
pub(super) struct ObservationLifetime {
    producer_confirmed_at: Option<Instant>,
    work: ObservationWork,
}

#[derive(Default)]
enum ObservationWork {
    #[default]
    Idle,
    Active { deadline: Instant },
}

impl ObservationLifetime {
    pub(super) fn has_confirmation(&self) -> bool {
        self.producer_confirmed_at.is_some()
    }

    /// The first observation gets an initial freshness interval. Only complete
    /// producer admission may subsequently move this instant.
    pub(super) fn producer_anchor(&mut self, now: Instant) -> Instant {
        *self.producer_confirmed_at.get_or_insert(now)
    }

    /// Begins work after idle time, or keeps the original reconnect ceiling.
    pub(super) fn ordinary_deadline(&mut self, now: Instant, allowance: Duration) -> Instant {
        match self.work {
            ObservationWork::Idle => {
                let deadline = now + allowance;
                self.work = ObservationWork::Active { deadline };
                deadline
            }
            ObservationWork::Active { deadline } => deadline,
        }
    }

    pub(super) fn complete_control(&mut self) {
        self.work = ObservationWork::Idle;
    }

    pub(super) fn confirm_producer(&mut self, now: Instant) {
        self.producer_confirmed_at = Some(now);
        self.complete_control();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn long_idle_subscription_does_not_spend_the_next_active_work_term() {
        let mut lifetime = ObservationLifetime::default();
        let certified = Instant::now();
        lifetime.confirm_producer(certified);
        let allowance = Duration::from_secs(30);
        let active = certified + Duration::from_secs(120);
        assert_eq!(lifetime.ordinary_deadline(active, allowance), active + allowance);
        assert_eq!(lifetime.producer_anchor(active), certified,
            "idle time still ages producer freshness");
    }

    #[test]
    fn reconnects_do_not_restart_consumed_active_recovery() {
        let mut lifetime = ObservationLifetime::default();
        let now = Instant::now();
        let allowance = Duration::from_secs(30);
        let original = lifetime.ordinary_deadline(now, allowance);
        for elapsed in [1, 10, 29, 30, 60] {
            assert_eq!(lifetime.ordinary_deadline(now + Duration::from_secs(elapsed), allowance), original);
        }
    }

    #[test]
    fn indefinitely_successful_renewals_never_certify_producer_freshness() {
        let mut lifetime = ObservationLifetime::default();
        let confirmed = Instant::now();
        lifetime.confirm_producer(confirmed);
        for seconds in 1..=1_000 {
            let now = confirmed + Duration::from_secs(seconds);
            assert_eq!(lifetime.ordinary_deadline(now, Duration::from_secs(30)), now + Duration::from_secs(30));
            lifetime.complete_control();
            assert_eq!(lifetime.producer_anchor(now), confirmed);
        }
        let admitted = confirmed + Duration::from_secs(1_001);
        lifetime.confirm_producer(admitted);
        assert_eq!(lifetime.producer_anchor(admitted), admitted);
    }
}
