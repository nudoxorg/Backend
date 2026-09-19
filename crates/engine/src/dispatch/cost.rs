//! Bounded, owner-maintained route cost observations.
//!
//! Placement must not be steered by a caller fabricating a one-shot
//! [`backend_execution::CostSnapshot`]. The dispatcher owns this model and
//! replaces the request's estimate with a snapshot derived from verified
//! completion observations. One compact row represents a recipe/size class;
//! fixed local-state and remote-state slots keep independent evidence axes
//! without allocating their Cartesian product.

use backend_execution::{
    CompletionCost, CostObservation, CostSnapshot, CostVerifier, LocalState, ObservationError,
    RemoteState, VersionedWorkIdentity,
};
use backend_version::Relation;
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Mutex;

const MAX_CLASSES: usize = 256;
const MAX_SAMPLES: u32 = 16;
const MAX_TAIL_SAMPLES: usize = 16;
const LOCAL_STATE_COUNT: usize = 4;
const REMOTE_STATE_COUNT: usize = 3;
// Owner observations use Unix milliseconds in the process composition. Keep
// a coarse recipe/size class warm across ordinary indexing and fsync work;
// fixed row/sample caps remain the memory and adversarial-allocation limit.
pub(super) const OBSERVATION_TTL_TICKS: u64 = 5 * 60 * 1_000;
const REMOTE_CIRCUIT_FAILURE_LIMIT: u8 = 3;
const REMOTE_CIRCUIT_TICKS: u64 = 1_000;

#[derive(Clone, Copy, Debug)]
struct InternalCostVerifier;

impl<R: Relation> CostVerifier<R> for InternalCostVerifier {
    fn verify_cost(
        &self,
        _identity: &VersionedWorkIdentity<R>,
        _observation: &CostObservation,
    ) -> Result<(), ObservationError> {
        Ok(())
    }
}

/// Bounded route identity. Local and remote locality are observations about
/// this route, not dimensions of its allocation key.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
struct RouteClass {
    recipe: [u8; 32],
    input_bucket: u8,
}

impl RouteClass {
    fn from_identity<R: Relation>(identity: &VersionedWorkIdentity<R>, bytes: u64) -> Self {
        Self {
            recipe: identity.recipe.to_bytes(),
            input_bucket: input_bucket(bytes),
        }
    }
}

fn input_bucket(bytes: u64) -> u8 {
    match bytes {
        0..=4_096 => 0,
        4_097..=65_536 => 1,
        65_537..=1_048_576 => 2,
        _ => 3,
    }
}

const fn local_state_index(state: LocalState) -> usize {
    match state {
        LocalState::Ready => 0,
        LocalState::MissingInputs => 1,
        LocalState::Overloaded => 2,
        LocalState::Unavailable => 3,
    }
}

const fn remote_state_index(state: RemoteState) -> usize {
    match state {
        RemoteState::Unavailable => 0,
        RemoteState::Available => 1,
        RemoteState::Warm => 2,
    }
}

#[derive(Clone, Copy, Debug, Default)]
struct LocalEvidence {
    cost: u64,
    samples: u32,
    observed_at: u64,
    profile_version: u64,
}

#[derive(Clone, Copy, Debug)]
struct RemoteEvidence {
    cost: CompletionCost,
    samples: u32,
    tail: [u64; MAX_TAIL_SAMPLES],
    tail_len: usize,
    tail_next: usize,
    failures: u8,
    circuit_until: u64,
    observed_at: u64,
    profile_version: u64,
}

impl Default for RemoteEvidence {
    fn default() -> Self {
        Self {
            cost: CompletionCost::default_with_max_remote(),
            samples: 0,
            tail: [u64::MAX; MAX_TAIL_SAMPLES],
            tail_len: 0,
            tail_next: 0,
            failures: 0,
            circuit_until: 0,
            observed_at: 0,
            profile_version: 0,
        }
    }
}

#[derive(Clone, Copy, Debug)]
struct CostEntry {
    local: [LocalEvidence; LOCAL_STATE_COUNT],
    remote: [RemoteEvidence; REMOTE_STATE_COUNT],
    generation: u64,
}

impl CostEntry {
    fn new(generation: u64) -> Self {
        Self {
            local: [LocalEvidence::default(); LOCAL_STATE_COUNT],
            remote: [RemoteEvidence::default(); REMOTE_STATE_COUNT],
            generation,
        }
    }
}

struct ModelState {
    entries: BTreeMap<RouteClass, CostEntry>,
    recency: BTreeSet<(u64, RouteClass)>,
    next_generation: u64,
}

/// Dispatcher-owned route model. All mutation is private to dispatch
/// completion paths; callers receive only a checked, confidence-bounded
/// snapshot for planning.
pub(super) struct RouteCostModel {
    state: Mutex<ModelState>,
    profile_version: u64,
}

impl std::fmt::Debug for RouteCostModel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        f.debug_struct("RouteCostModel")
            .field("classes", &state.entries.len())
            .field("max_classes", &MAX_CLASSES)
            .field("profile_version", &self.profile_version)
            .finish()
    }
}

impl Default for RouteCostModel {
    fn default() -> Self {
        Self::new()
    }
}

impl RouteCostModel {
    pub(super) fn new() -> Self {
        Self {
            state: Mutex::new(ModelState {
                entries: BTreeMap::new(),
                recency: BTreeSet::new(),
                next_generation: 0,
            }),
            profile_version: 1,
        }
    }

    /// Returns an owner-derived checked snapshot. The supplied request cost
    /// is intentionally ignored; it is retained on `ScheduleRequest` only
    /// for source compatibility with lower-layer callers.
    pub(super) fn snapshot<R: Relation>(
        &self,
        identity: &VersionedWorkIdentity<R>,
        bytes: u64,
        local_state: LocalState,
        remote_state: RemoteState,
        now: u64,
    ) -> Result<CostSnapshot, ObservationError> {
        let class = RouteClass::from_identity(identity, bytes);
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let entry = state.entries.get(&class).copied();
        let local = entry
            .map(|entry| entry.local[local_state_index(local_state)])
            .filter(|evidence| self.local_is_fresh(*evidence, now));
        let remote = entry.and_then(|entry| {
            let exact = entry.remote[remote_state_index(remote_state)];
            if self.remote_is_fresh(exact, now) {
                return Some(exact);
            }
            // A successful Available execution is what makes the next
            // adjacent root Warm. Until Warm has evidence of its own, inherit
            // the strictly less favorable Available prior.
            (remote_state == RemoteState::Warm)
                .then_some(entry.remote[remote_state_index(RemoteState::Available)])
                .filter(|evidence| self.remote_is_fresh(*evidence, now))
        });

        let local_cost = local.map_or(0, |evidence| evidence.cost);
        let mut remote_cost = remote
            .map_or_else(CompletionCost::default_with_max_remote, |evidence| {
                evidence.cost
            });
        let mut remote_p95 = remote.map_or(u64::MAX, |evidence| remote_tail_p95(&evidence));
        let local_samples = local.map_or(0, |evidence| evidence.samples);
        let remote_samples = remote.map_or(0, |evidence| evidence.samples);
        let mut confidence = (local_samples.min(remote_samples).min(10) as u16) * 100;

        // Explore after the first observed local completion, then at most once
        // per four local samples until five remote samples exist. Independent
        // evidence slots ensure a Warm observation cannot erase this baseline.
        if remote_samples < 5 && local_samples >= 1 && local_samples % 4 == 1 {
            let estimate = local_cost.saturating_sub(1);
            remote_cost = CompletionCost {
                execution: estimate,
                ..CompletionCost::default()
            };
            remote_p95 = estimate;
            confidence = CostSnapshot::MIN_CONFIDENCE_PER_MILLE;
        }
        if remote.is_some_and(|evidence| now < evidence.circuit_until) {
            remote_cost = CompletionCost::default_with_max_remote();
            remote_p95 = u64::MAX;
            confidence = 0;
        }

        let observed_at = match (local, remote) {
            (Some(local), Some(remote)) => local.observed_at.min(remote.observed_at),
            (Some(local), None) => local.observed_at,
            (None, Some(remote)) => remote.observed_at,
            (None, None) => now,
        };
        if entry.is_some() {
            touch_entry(&mut state, class).ok_or(ObservationError::Rejected)?;
        }
        let expires_at = observed_at
            .checked_add(OBSERVATION_TTL_TICKS)
            .ok_or(ObservationError::InvalidWindow)?;
        drop(state);
        CostSnapshot::admit_with_remote_p95(
            identity,
            CostObservation {
                local: local_cost,
                remote: remote_cost,
                observed_at,
                expires_at,
                confidence_per_mille: confidence.min(1_000),
            },
            remote_p95,
            &InternalCostVerifier,
        )
    }

    fn local_is_fresh(&self, evidence: LocalEvidence, now: u64) -> bool {
        evidence.samples != 0
            && evidence.profile_version == self.profile_version
            && window_is_fresh(evidence.observed_at, now)
    }

    fn remote_is_fresh(&self, evidence: RemoteEvidence, now: u64) -> bool {
        (evidence.samples != 0 || evidence.failures != 0)
            && evidence.profile_version == self.profile_version
            && window_is_fresh(evidence.observed_at, now)
    }

    /// Records a verified local completion. Remote cache locality is
    /// deliberately absent from this update: it cannot change local compute.
    pub(super) fn record_local<R: Relation>(
        &self,
        identity: &VersionedWorkIdentity<R>,
        bytes: u64,
        _remote_state: RemoteState,
        elapsed: u64,
        now: u64,
    ) {
        let class = RouteClass::from_identity(identity, bytes);
        let mut state = self.lock_with_entry(class);
        let Some(entry) = state.entries.get_mut(&class) else {
            return;
        };
        let evidence = &mut entry.local[local_state_index(LocalState::Ready)];
        if self.local_is_fresh(*evidence, now) {
            evidence.cost = ewma(evidence.cost, elapsed.max(1));
            evidence.samples = evidence.samples.saturating_add(1).min(MAX_SAMPLES);
            evidence.observed_at = now;
        } else {
            *evidence = LocalEvidence {
                cost: elapsed.max(1),
                samples: 1,
                observed_at: now,
                profile_version: self.profile_version,
            };
        }
        let _ = touch_entry(&mut state, class);
    }

    /// Records a verified remote completion. Local readiness is deliberately
    /// absent from this update: it cannot change remote transfer or compute.
    pub(super) fn record_remote<R: Relation>(
        &self,
        identity: &VersionedWorkIdentity<R>,
        bytes: u64,
        _local_state: LocalState,
        remote_state: RemoteState,
        cost: CompletionCost,
        now: u64,
    ) {
        let class = RouteClass::from_identity(identity, bytes);
        let mut state = self.lock_with_entry(class);
        let Some(entry) = state.entries.get_mut(&class) else {
            return;
        };
        let evidence = &mut entry.remote[remote_state_index(remote_state)];
        if self.remote_is_fresh(*evidence, now) {
            evidence.cost = if evidence.samples == 0 {
                cost
            } else {
                ewma_cost(evidence.cost, cost)
            };
            evidence.samples = evidence.samples.saturating_add(1).min(MAX_SAMPLES);
            push_remote_tail(evidence, cost.total());
            evidence.failures = 0;
            evidence.circuit_until = 0;
            evidence.observed_at = now;
        } else {
            reset_remote(evidence, Some(cost), now, self.profile_version);
        }
        let _ = touch_entry(&mut state, class);
    }

    /// Records a remote route failure. The circuit belongs to the remote
    /// locality slot and therefore survives unrelated local observations.
    pub(super) fn record_remote_failure<R: Relation>(
        &self,
        identity: &VersionedWorkIdentity<R>,
        bytes: u64,
        _local_state: LocalState,
        remote_state: RemoteState,
        now: u64,
    ) {
        let class = RouteClass::from_identity(identity, bytes);
        let mut state = self.lock_with_entry(class);
        let Some(entry) = state.entries.get_mut(&class) else {
            return;
        };
        let evidence = &mut entry.remote[remote_state_index(remote_state)];
        if !self.remote_is_fresh(*evidence, now) {
            reset_remote(evidence, None, now, self.profile_version);
        } else if evidence.circuit_until != 0 && now >= evidence.circuit_until {
            evidence.failures = 0;
            evidence.circuit_until = 0;
        }
        evidence.failures = evidence
            .failures
            .saturating_add(1)
            .min(REMOTE_CIRCUIT_FAILURE_LIMIT);
        if evidence.failures >= REMOTE_CIRCUIT_FAILURE_LIMIT {
            evidence.circuit_until = now.saturating_add(REMOTE_CIRCUIT_TICKS);
        }
        evidence.observed_at = now;
        evidence.profile_version = self.profile_version;
        let _ = touch_entry(&mut state, class);
    }

    fn lock_with_entry(&self, class: RouteClass) -> std::sync::MutexGuard<'_, ModelState> {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if !state.entries.contains_key(&class) {
            insert_entry(&mut state, class);
        }
        state
    }

    #[cfg(test)]
    pub(super) fn class_count(&self) -> usize {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .entries
            .len()
    }
}

fn window_is_fresh(observed_at: u64, now: u64) -> bool {
    now >= observed_at
        && now
            .checked_sub(observed_at)
            .is_some_and(|elapsed| elapsed < OBSERVATION_TTL_TICKS)
}

fn insert_entry(state: &mut ModelState, class: RouteClass) {
    while state.entries.len() >= MAX_CLASSES {
        let Some((generation, oldest)) = state.recency.pop_first() else {
            break;
        };
        if state
            .entries
            .get(&oldest)
            .is_some_and(|entry| entry.generation == generation)
        {
            state.entries.remove(&oldest);
        }
    }
    let Some(generation) = next_generation(state) else {
        return;
    };
    state.recency.insert((generation, class));
    state.entries.insert(class, CostEntry::new(generation));
}

fn touch_entry(state: &mut ModelState, class: RouteClass) -> Option<()> {
    let old_generation = state.entries.get(&class)?.generation;
    let generation = next_generation(state)?;
    state.recency.remove(&(old_generation, class));
    state.entries.get_mut(&class)?.generation = generation;
    state.recency.insert((generation, class));
    Some(())
}

fn reset_remote(
    evidence: &mut RemoteEvidence,
    cost: Option<CompletionCost>,
    now: u64,
    profile_version: u64,
) {
    *evidence = RemoteEvidence {
        cost: cost.unwrap_or_else(CompletionCost::default_with_max_remote),
        samples: u32::from(cost.is_some()),
        tail: cost.map_or([u64::MAX; MAX_TAIL_SAMPLES], |sample| {
            [sample.total(); MAX_TAIL_SAMPLES]
        }),
        tail_len: usize::from(cost.is_some()),
        tail_next: usize::from(cost.is_some()),
        failures: 0,
        circuit_until: 0,
        observed_at: now,
        profile_version,
    };
}

fn push_remote_tail(evidence: &mut RemoteEvidence, sample: u64) {
    evidence.tail[evidence.tail_next] = sample;
    evidence.tail_next = (evidence.tail_next + 1) % MAX_TAIL_SAMPLES;
    evidence.tail_len = evidence.tail_len.saturating_add(1).min(MAX_TAIL_SAMPLES);
}

fn next_generation(state: &mut ModelState) -> Option<u64> {
    if state.next_generation == u64::MAX {
        // Rebase the bounded recency index before allocating another stamp.
        let keys: Vec<_> = state.entries.keys().copied().collect();
        let mut recency = BTreeSet::new();
        for (index, class) in keys.into_iter().enumerate() {
            let generation = u64::try_from(index).ok()?.checked_add(1)?;
            state.entries.get_mut(&class)?.generation = generation;
            recency.insert((generation, class));
        }
        state.recency = recency;
        state.next_generation = u64::try_from(state.entries.len()).ok()?;
    }
    let next = state.next_generation.checked_add(1)?;
    state.next_generation = next;
    Some(next)
}

fn ewma(old: u64, sample: u64) -> u64 {
    // Reduce before multiplying so weighted terms remain in range even for
    // adversarial clock/cost observations near `u64::MAX`.
    let weighted_old = (old / 8) * 7;
    let weighted_sample = sample / 8;
    let remainder = ((old % 8) * 7 + (sample % 8)) / 8;
    weighted_old
        .checked_add(weighted_sample)
        .and_then(|value| value.checked_add(remainder))
        .unwrap_or(u64::MAX)
}

fn ewma_cost(old: CompletionCost, sample: CompletionCost) -> CompletionCost {
    CompletionCost {
        client_queue: ewma(old.client_queue, sample.client_queue),
        round_trip: ewma(old.round_trip, sample.round_trip),
        input_transfer: ewma(old.input_transfer, sample.input_transfer),
        worker_queue: ewma(old.worker_queue, sample.worker_queue),
        warmup: ewma(old.warmup, sample.warmup),
        execution: ewma(old.execution, sample.execution),
        output_transfer: ewma(old.output_transfer, sample.output_transfer),
        validation: ewma(old.validation, sample.validation),
        contention: ewma(old.contention, sample.contention),
    }
}

fn remote_tail_p95(evidence: &RemoteEvidence) -> u64 {
    if evidence.tail_len == 0 {
        return u64::MAX;
    }
    let mut samples = evidence.tail;
    samples[..evidence.tail_len].sort_unstable();
    let index = (evidence.tail_len - 1) * 95 / 100;
    samples[index]
}

trait ColdRemoteCost {
    fn default_with_max_remote() -> Self;
}

impl ColdRemoteCost for CompletionCost {
    fn default_with_max_remote() -> Self {
        Self {
            execution: u64::MAX,
            ..Self::default()
        }
    }
}
