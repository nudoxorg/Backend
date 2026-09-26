//! Deterministic local-first placement from one immutable policy snapshot.

use super::{
    BatteryState, BudgetAmount, BundleFact, BundleState, Demand, DemandLevel, DuplicateInput,
    FactKey, InputClass, LatencyMicros, LocalFact, MAX_BUNDLE_FACTS, MAX_DEMAND_FACTS,
    MAX_LOCAL_FACTS, MAX_REMOTE_FACTS, OperationBudget, Overload, OverloadSubject, PlacementAction,
    PolicyDecision, PolicyError, PolicyInput, Pressure, RecoveryCause, RemoteFact, RemoteHealth,
    ResourceClass, Retention, StorageTier,
};

/// Return the one canonical action for an immutable bounded snapshot.
///
/// Candidate classes are ordered exactly as preservation, safe RAM contraction, bundle
/// acquisition, remote recovery, healthy fetch, `NVMe` eviction, then bundle release. Ties are
/// ordered by [`FactKey`] or by
/// `(CapabilityKind, ContentId<CapabilityDomain>)`, never by caller slice order. The adapter applies an action and
/// returns a new snapshot before this function is called again, so resource and retry credits are
/// neither hidden mutable policy state nor an in-process async queue.
#[must_use]
pub fn next_action(input: &PolicyInput<'_>) -> PolicyDecision {
    if let Some(error) = validate_input(input) {
        return PolicyDecision::Rejected(error);
    }

    if let Some(local) = least_preservation_candidate(input) {
        return admit_preservation(input, local);
    }

    if let Some(local) = least_ram_eviction_candidate(input) {
        return admit_operation(
            input.budget.operations,
            OverloadSubject::Fact(local.key),
            PlacementAction::EvictFromRam {
                key: local.key,
                bytes: local.bytes,
            },
        );
    }

    if let Some(bundle) = least_acquire_candidate(input) {
        return admit_operation(
            input.budget.operations,
            OverloadSubject::Bundle {
                capability: bundle.capability,
                bundle: bundle.bundle,
            },
            PlacementAction::AcquireBundle {
                capability: bundle.capability,
                bundle: bundle.bundle,
            },
        );
    }

    if let Some(cause) = recovery_cause(input) {
        return admit_recovery(input, cause);
    }

    if let Some(remote) = least_fetch_candidate(input) {
        return admit_fetch(input, remote);
    }

    if let Some(local) = least_eviction_candidate(input) {
        return admit_operation(
            input.budget.operations,
            OverloadSubject::Fact(local.key),
            PlacementAction::EvictFromNvme {
                key: local.key,
                bytes: local.bytes,
            },
        );
    }

    if let Some(bundle) = least_release_candidate(input) {
        return admit_operation(
            input.budget.operations,
            OverloadSubject::Bundle {
                capability: bundle.capability,
                bundle: bundle.bundle,
            },
            PlacementAction::ReleaseBundle {
                capability: bundle.capability,
                bundle: bundle.bundle,
            },
        );
    }

    PolicyDecision::NoAction
}

fn validate_input(input: &PolicyInput<'_>) -> Option<PolicyError> {
    let bounded = [
        (InputClass::Local, input.local.len(), MAX_LOCAL_FACTS),
        (InputClass::Remote, input.remote.len(), MAX_REMOTE_FACTS),
        (InputClass::Demand, input.demand.len(), MAX_DEMAND_FACTS),
        (InputClass::Bundle, input.bundles.len(), MAX_BUNDLE_FACTS),
    ];
    for (class, observed, limit) in bounded {
        if observed > limit {
            return Some(PolicyError::TooManyFacts {
                class,
                limit,
                observed,
            });
        }
    }

    let local_mismatch = input
        .local
        .iter()
        .filter(|fact| fact.key.pin != input.pin)
        .min_by_key(|fact| fact.key);
    if let Some(fact) = local_mismatch {
        return Some(PolicyError::PinMismatch {
            class: InputClass::Local,
            expected: input.pin,
            observed: fact.key,
        });
    }
    let remote_mismatch = input
        .remote
        .iter()
        .filter(|fact| fact.key.pin != input.pin)
        .min_by_key(|fact| fact.key);
    if let Some(fact) = remote_mismatch {
        return Some(PolicyError::PinMismatch {
            class: InputClass::Remote,
            expected: input.pin,
            observed: fact.key,
        });
    }
    let demand_mismatch = input
        .demand
        .iter()
        .filter(|fact| fact.key.pin != input.pin)
        .min_by_key(|fact| fact.key);
    if let Some(fact) = demand_mismatch {
        return Some(PolicyError::PinMismatch {
            class: InputClass::Demand,
            expected: input.pin,
            observed: fact.key,
        });
    }

    duplicate_local(input.local)
        .map(PolicyError::Duplicate)
        .or_else(|| duplicate_remote(input.remote).map(PolicyError::Duplicate))
        .or_else(|| duplicate_demand(input.demand).map(PolicyError::Duplicate))
        .or_else(|| duplicate_bundle(input.bundles).map(PolicyError::Duplicate))
}

fn duplicate_local(facts: &[LocalFact]) -> Option<DuplicateInput> {
    let mut duplicate = None;
    for (offset, current) in facts.iter().enumerate() {
        if facts[..offset]
            .iter()
            .any(|previous| previous.key == current.key && previous.tier == current.tier)
        {
            let candidate = DuplicateInput::Local {
                key: current.key,
                tier: current.tier,
            };
            if duplicate_local_is_less(candidate, duplicate) {
                duplicate = Some(candidate);
            }
        }
    }
    duplicate
}

fn duplicate_local_is_less(candidate: DuplicateInput, current: Option<DuplicateInput>) -> bool {
    match current {
        None => true,
        Some(DuplicateInput::Local { key, tier }) => match candidate {
            DuplicateInput::Local {
                key: candidate_key,
                tier: candidate_tier,
            } => (candidate_key, candidate_tier) < (key, tier),
            _ => false,
        },
        Some(_) => false,
    }
}

fn duplicate_remote(facts: &[RemoteFact]) -> Option<DuplicateInput> {
    let mut duplicate = None;
    for (offset, current) in facts.iter().enumerate() {
        if facts[..offset]
            .iter()
            .any(|previous| previous.key == current.key)
        {
            let candidate = DuplicateInput::Remote { key: current.key };
            if duplicate_key_is_less(candidate, duplicate) {
                duplicate = Some(candidate);
            }
        }
    }
    duplicate
}

fn duplicate_demand(facts: &[Demand]) -> Option<DuplicateInput> {
    let mut duplicate = None;
    for (offset, current) in facts.iter().enumerate() {
        if facts[..offset]
            .iter()
            .any(|previous| previous.key == current.key)
        {
            let candidate = DuplicateInput::Demand { key: current.key };
            if duplicate_key_is_less(candidate, duplicate) {
                duplicate = Some(candidate);
            }
        }
    }
    duplicate
}

fn duplicate_key_is_less(candidate: DuplicateInput, current: Option<DuplicateInput>) -> bool {
    match (candidate, current) {
        (DuplicateInput::Remote { key: _ } | DuplicateInput::Demand { key: _ }, None) => true,
        (DuplicateInput::Remote { key: candidate_key }, Some(DuplicateInput::Remote { key }))
        | (DuplicateInput::Demand { key: candidate_key }, Some(DuplicateInput::Demand { key })) => {
            candidate_key < key
        }
        _ => false,
    }
}

fn duplicate_bundle(facts: &[BundleFact]) -> Option<DuplicateInput> {
    let mut duplicate = None;
    for (offset, current) in facts.iter().enumerate() {
        if facts[..offset].iter().any(|previous| {
            previous.capability == current.capability && previous.bundle == current.bundle
        }) {
            let candidate = DuplicateInput::Bundle {
                capability: current.capability,
                bundle: current.bundle,
            };
            if duplicate_bundle_is_less(candidate, duplicate) {
                duplicate = Some(candidate);
            }
        }
    }
    duplicate
}

fn duplicate_bundle_is_less(candidate: DuplicateInput, current: Option<DuplicateInput>) -> bool {
    match current {
        None => true,
        Some(DuplicateInput::Bundle { capability, bundle }) => match candidate {
            DuplicateInput::Bundle {
                capability: candidate_capability,
                bundle: candidate_bundle,
            } => (candidate_capability, candidate_bundle) < (capability, bundle),
            _ => false,
        },
        Some(_) => false,
    }
}

fn least_preservation_candidate(input: &PolicyInput<'_>) -> Option<LocalFact> {
    if input.budget.memory_pressure == Pressure::Relaxed {
        return None;
    }
    input
        .local
        .iter()
        .copied()
        .filter(|local| {
            local.tier == StorageTier::Ram
                && local.retention == Retention::Idle
                && !input.local.iter().any(|candidate| {
                    candidate.key == local.key && candidate.tier == StorageTier::Nvme
                })
        })
        .min_by_key(|local| local.key)
}

fn admit_preservation(input: &PolicyInput<'_>, local: LocalFact) -> PolicyDecision {
    let subject = OverloadSubject::Fact(local.key);
    if !input.budget.operations.available() {
        return operation_overload(subject, input.budget.operations);
    }
    if !local.bytes.fits_within(input.budget.nvme_free) {
        return PolicyDecision::Overloaded(Overload {
            subject,
            resource: ResourceClass::Nvme,
            needed: BudgetAmount::Bytes(local.bytes),
            available: BudgetAmount::Bytes(input.budget.nvme_free),
        });
    }
    PolicyDecision::Act(PlacementAction::PreserveToNvme {
        key: local.key,
        bytes: local.bytes,
    })
}

fn least_ram_eviction_candidate(input: &PolicyInput<'_>) -> Option<LocalFact> {
    if input.budget.memory_pressure == Pressure::Relaxed {
        return None;
    }
    input
        .local
        .iter()
        .copied()
        .filter(|local| {
            local.tier == StorageTier::Ram
                && local.retention == Retention::Idle
                && input.local.iter().any(|candidate| {
                    candidate.key == local.key && candidate.tier == StorageTier::Nvme
                })
        })
        .min_by_key(|local| local.key)
}

fn least_acquire_candidate(input: &PolicyInput<'_>) -> Option<BundleFact> {
    recovery_cause(input)?;
    input
        .bundles
        .iter()
        .copied()
        .filter(|bundle| bundle.state == BundleState::AvailableRequired)
        .min_by_key(|bundle| (bundle.capability, bundle.bundle))
}

fn recovery_cause(input: &PolicyInput<'_>) -> Option<RecoveryCause> {
    match input.remote_health {
        RemoteHealth::Healthy { observed, .. } if observed == input.pin => None,
        RemoteHealth::Healthy { observed, .. } | RemoteHealth::Inconsistent { observed } => {
            Some(RecoveryCause::Inconsistent { observed })
        }
        RemoteHealth::Outage => Some(RecoveryCause::Outage),
    }
}

fn admit_recovery(input: &PolicyInput<'_>, cause: RecoveryCause) -> PolicyDecision {
    if !input.budget.operations.available() {
        return operation_overload(OverloadSubject::Remote(input.pin), input.budget.operations);
    }
    if !input.budget.retries.available() {
        return PolicyDecision::RecoveryExhausted {
            pin: input.pin,
            cause,
        };
    }
    PolicyDecision::Act(PlacementAction::RetryRemote {
        pin: input.pin,
        cause,
        retries_remaining: input.budget.retries.after_one(),
    })
}

fn least_fetch_candidate(input: &PolicyInput<'_>) -> Option<RemoteFact> {
    let RemoteHealth::Healthy { observed, latency } = input.remote_health else {
        return None;
    };
    if observed != input.pin || input.budget.battery == BatteryState::Critical {
        return None;
    }
    input
        .remote
        .iter()
        .copied()
        .filter(|remote| {
            !is_locally_in_ram(input.local, remote.key)
                && has_acceptable_hot_demand(input.demand, remote.key, latency)
        })
        .min_by_key(|remote| remote.key)
}

fn is_locally_in_ram(local: &[LocalFact], key: FactKey) -> bool {
    local
        .iter()
        .any(|fact| fact.key == key && fact.tier == StorageTier::Ram)
}

fn has_acceptable_hot_demand(demand: &[Demand], key: FactKey, latency: LatencyMicros) -> bool {
    demand.iter().any(|candidate| {
        candidate.key == key
            && candidate.level == DemandLevel::Hot
            && latency.no_more_than(candidate.latency_budget)
    })
}

fn admit_fetch(input: &PolicyInput<'_>, remote: RemoteFact) -> PolicyDecision {
    let subject = OverloadSubject::Fact(remote.key);
    if !input.budget.operations.available() {
        return operation_overload(subject, input.budget.operations);
    }
    if !remote.bytes.fits_within(input.budget.ram_free) {
        return PolicyDecision::Overloaded(Overload {
            subject,
            resource: ResourceClass::Ram,
            needed: BudgetAmount::Bytes(remote.bytes),
            available: BudgetAmount::Bytes(input.budget.ram_free),
        });
    }
    PolicyDecision::Act(PlacementAction::FetchToRam {
        key: remote.key,
        bytes: remote.bytes,
    })
}

fn least_eviction_candidate(input: &PolicyInput<'_>) -> Option<LocalFact> {
    if input.budget.storage_pressure != Pressure::Critical {
        return None;
    }
    input
        .local
        .iter()
        .copied()
        .filter(|local| {
            local.tier == StorageTier::Nvme
                && local.retention == Retention::Idle
                && has_eviction_survivor(input, local.key)
        })
        .min_by_key(|local| local.key)
}

fn has_eviction_survivor(input: &PolicyInput<'_>, key: FactKey) -> bool {
    let has_ram_copy = input
        .local
        .iter()
        .any(|candidate| candidate.key == key && candidate.tier == StorageTier::Ram);
    let matching_remote_is_healthy = matches!(
        input.remote_health,
        RemoteHealth::Healthy { observed, .. } if observed == input.pin
    );
    let has_remote_copy =
        matching_remote_is_healthy && input.remote.iter().any(|candidate| candidate.key == key);
    has_ram_copy || has_remote_copy
}

fn least_release_candidate(input: &PolicyInput<'_>) -> Option<BundleFact> {
    let contraction = input.budget.battery == BatteryState::Critical
        || input.budget.memory_pressure == Pressure::Critical
        || input.budget.storage_pressure == Pressure::Critical;
    if !contraction {
        return None;
    }
    input
        .bundles
        .iter()
        .copied()
        .filter(|bundle| bundle.state == BundleState::ActiveIdle)
        .min_by_key(|bundle| (bundle.capability, bundle.bundle))
}

fn admit_operation(
    operations: OperationBudget,
    subject: OverloadSubject,
    action: PlacementAction,
) -> PolicyDecision {
    if operations.available() {
        PolicyDecision::Act(action)
    } else {
        operation_overload(subject, operations)
    }
}

fn operation_overload(subject: OverloadSubject, available: OperationBudget) -> PolicyDecision {
    PolicyDecision::Overloaded(Overload {
        subject,
        resource: ResourceClass::Operations,
        needed: BudgetAmount::Operations(OperationBudget::from(1)),
        available: BudgetAmount::Operations(available),
    })
}
