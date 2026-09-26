use std::collections::BTreeMap;
use std::sync::Arc;

pub(super) const MAX_FACT_OBSERVATIONS: usize = 4_096;

/// Explicit freshness frontier for mutable registry facts.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FactFreshness {
    /// Maximum age of an observed row. Zero revalidates every demand.
    pub max_age_millis: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct FactObservation {
    observed_at_millis: u64,
    policy_epoch: u64,
}

pub(super) fn observed_facts_at(
    observations: &BTreeMap<Arc<str>, FactObservation>,
    coordinate: &str,
    policy_epoch: u64,
) -> Option<u64> {
    observations
        .get(coordinate)
        .filter(|observation| observation.policy_epoch == policy_epoch)
        .map(|observation| observation.observed_at_millis)
}

pub(super) fn remember_fact_observation(
    observations: &mut BTreeMap<Arc<str>, FactObservation>,
    coordinate: &str,
    observed_at_millis: u64,
    policy_epoch: u64,
) {
    if observations.len() >= MAX_FACT_OBSERVATIONS && !observations.contains_key(coordinate) {
        if let Some(oldest) = observations
            .iter()
            .min_by(
                |(coordinate_a, observation_a), (coordinate_b, observation_b)| {
                    observation_a
                        .observed_at_millis
                        .cmp(&observation_b.observed_at_millis)
                        .then_with(|| coordinate_a.cmp(coordinate_b))
                },
            )
            .map(|(coordinate, _)| Arc::clone(coordinate))
        {
            observations.remove(&oldest);
        }
    }
    observations.insert(
        Arc::from(coordinate),
        FactObservation {
            observed_at_millis,
            policy_epoch,
        },
    );
}

impl FactFreshness {
    /// Revalidates facts on every demand.
    #[must_use]
    pub const fn always() -> Self {
        Self { max_age_millis: 0 }
    }

    /// Allows reuse for a bounded interval.
    #[must_use]
    pub const fn max_age_millis(max_age_millis: u64) -> Self {
        Self { max_age_millis }
    }

    pub(super) fn due(self, observed_at_millis: Option<u64>, now_millis: u64) -> bool {
        let Some(observed) = observed_at_millis else {
            return true;
        };
        self.max_age_millis == 0 || now_millis.saturating_sub(observed) >= self.max_age_millis
    }
}
