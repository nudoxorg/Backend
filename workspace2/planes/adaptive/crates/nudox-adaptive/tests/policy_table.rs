//! Table and permutation attacks for the deterministic policy boundary.

use nudox_adaptive::{
    BatteryState, BudgetAmount, BundleFact, BundleResidence, ByteCount, CapabilityDomain,
    CapabilityKind, ContentId, Demand, DemandLevel, DuplicateInput, ExecutionPhase, ExecutionPoll,
    ExecutionRequest, ExecutionTerminal, FactKey, GenerationId, IndexSnapshotId, InputClass,
    LatencyMicros, LocalFact, MAX_REMOTE_FACTS, ObjectDomain, OperationBudget, Overload,
    OverloadSubject, Pin, PlacementAction, PolicyDecision, PolicyError, PolicyInput, Pressure,
    RecoveryCause, RemoteFact, RemoteHealth, ResourceBudget, ResourceClass, Retention, RetryBudget,
    StorageTier, next_action,
};

fn pin() -> Pin {
    Pin {
        generation: GenerationId::from_digest([41; 32]),
        snapshot: IndexSnapshotId::from_digest([43; 32]),
    }
}

fn key(fact: u8) -> FactKey {
    FactKey {
        pin: pin(),
        object: ContentId::<ObjectDomain>::from_digest([fact; 32]),
    }
}

fn bundle(seed: u8) -> ContentId<CapabilityDomain> {
    ContentId::from_digest([seed; 32])
}

fn execution_action() -> PlacementAction {
    PlacementAction::FetchToRam {
        key: key(4),
        bytes: ByteCount::from(16),
    }
}

fn budget() -> ResourceBudget {
    ResourceBudget {
        ram_free: ByteCount::from(128),
        nvme_free: ByteCount::from(128),
        operations: OperationBudget::from(1),
        retries: RetryBudget::from(1),
        memory_pressure: Pressure::Relaxed,
        storage_pressure: Pressure::Relaxed,
        battery: BatteryState::Normal,
    }
}

fn healthy() -> RemoteHealth {
    RemoteHealth::Healthy {
        observed: pin(),
        latency: LatencyMicros::from(10),
    }
}

fn demand(fact: u8) -> Demand {
    Demand {
        key: key(fact),
        level: DemandLevel::Hot,
        latency_budget: LatencyMicros::from(10),
    }
}

fn remote(fact: u8) -> RemoteFact {
    RemoteFact {
        key: key(fact),
        bytes: ByteCount::from(16),
    }
}

#[test]
fn fact_slice_permutations_keep_the_same_first_fetch() {
    let remote_forward = [remote(9), remote(2)];
    let remote_reverse = [remote(2), remote(9)];
    let demand_forward = [demand(9), demand(2)];
    let demand_reverse = [demand(2), demand(9)];
    let expected = PolicyDecision::Act(PlacementAction::FetchToRam {
        key: key(2),
        bytes: ByteCount::from(16),
    });
    for (remote_facts, demand_facts) in [
        (&remote_forward[..], &demand_forward[..]),
        (&remote_forward[..], &demand_reverse[..]),
        (&remote_reverse[..], &demand_forward[..]),
        (&remote_reverse[..], &demand_reverse[..]),
    ] {
        let input = PolicyInput {
            pin: pin(),
            local: &[],
            remote: remote_facts,
            demand: demand_facts,
            bundles: &[],
            remote_health: healthy(),
            budget: budget(),
        };
        assert_eq!(next_action(&input), expected);
    }
}

#[test]
fn local_permutations_keep_the_same_preservation_action() {
    let local_forward = [
        LocalFact {
            key: key(9),
            tier: StorageTier::Ram,
            bytes: ByteCount::from(4),
            retention: Retention::Idle,
        },
        LocalFact {
            key: key(2),
            tier: StorageTier::Ram,
            bytes: ByteCount::from(3),
            retention: Retention::Idle,
        },
    ];
    let local_reverse = [local_forward[1], local_forward[0]];
    let expected_preserve = PolicyDecision::Act(PlacementAction::PreserveToNvme {
        key: key(2),
        bytes: ByteCount::from(3),
    });
    for local in [&local_forward[..], &local_reverse[..]] {
        let input = PolicyInput {
            pin: pin(),
            local,
            remote: &[],
            demand: &[],
            bundles: &[],
            remote_health: healthy(),
            budget: ResourceBudget {
                memory_pressure: Pressure::Elevated,
                ..budget()
            },
        };
        assert_eq!(next_action(&input), expected_preserve);
    }
}

#[test]
fn bundle_permutations_keep_the_same_acquisition_action() {
    let bundle_forward = [
        BundleFact {
            capability: CapabilityKind::Model,
            bundle: bundle(9),
            residence: BundleResidence::Available,
        },
        BundleFact {
            capability: CapabilityKind::Analyzer,
            bundle: bundle(2),
            residence: BundleResidence::Available,
        },
    ];
    let bundle_reverse = [bundle_forward[1], bundle_forward[0]];
    let expected_acquire = PolicyDecision::Act(PlacementAction::AcquireBundle {
        capability: CapabilityKind::Analyzer,
        bundle: bundle(2),
    });
    for bundles in [&bundle_forward[..], &bundle_reverse[..]] {
        let input = PolicyInput {
            pin: pin(),
            local: &[],
            remote: &[],
            demand: &[],
            bundles,
            remote_health: RemoteHealth::Outage,
            budget: budget(),
        };
        assert_eq!(next_action(&input), expected_acquire);
    }
}

#[test]
fn local_bundle_and_remote_input_fail_with_exact_authority_errors() {
    let duplicate_local = [
        LocalFact {
            key: key(7),
            tier: StorageTier::Ram,
            bytes: ByteCount::from(1),
            retention: Retention::Idle,
        },
        LocalFact {
            key: key(7),
            tier: StorageTier::Ram,
            bytes: ByteCount::from(2),
            retention: Retention::Pinned,
        },
    ];
    let duplicate_input = PolicyInput {
        pin: pin(),
        local: &duplicate_local,
        remote: &[],
        demand: &[],
        bundles: &[],
        remote_health: healthy(),
        budget: budget(),
    };
    assert_eq!(
        next_action(&duplicate_input),
        PolicyDecision::Rejected(PolicyError::Duplicate(DuplicateInput::Local {
            key: key(7),
            tier: StorageTier::Ram,
        }))
    );

    let foreign_key = FactKey {
        pin: Pin {
            generation: GenerationId::from_digest([41; 32]),
            snapshot: IndexSnapshotId::from_digest([99; 32]),
        },
        object: ContentId::<ObjectDomain>::from_digest([7; 32]),
    };
    let mismatched_remote = [RemoteFact {
        key: foreign_key,
        bytes: ByteCount::from(16),
    }];
    let mismatched_input = PolicyInput {
        remote: &mismatched_remote,
        ..duplicate_input
    };
    assert_eq!(
        next_action(&mismatched_input),
        PolicyDecision::Rejected(PolicyError::PinMismatch {
            class: InputClass::Remote,
            expected: pin(),
            observed: foreign_key,
        })
    );
}

#[test]
fn bounds_and_byte_overload_leave_the_candidate_identifiable() {
    let remote_facts = [
        remote(1),
        remote(2),
        remote(3),
        remote(4),
        remote(5),
        remote(6),
        remote(7),
        remote(8),
        remote(9),
    ];
    let bounded_input = PolicyInput {
        pin: pin(),
        local: &[],
        remote: &remote_facts,
        demand: &[],
        bundles: &[],
        remote_health: healthy(),
        budget: budget(),
    };
    assert_eq!(
        next_action(&bounded_input),
        PolicyDecision::Rejected(PolicyError::TooManyFacts {
            class: InputClass::Remote,
            limit: MAX_REMOTE_FACTS,
            observed: remote_facts.len(),
        })
    );

    let fetch = [remote(5)];
    let hot = [demand(5)];
    let overloaded = PolicyInput {
        remote: &fetch,
        demand: &hot,
        budget: ResourceBudget {
            ram_free: ByteCount::from(15),
            ..budget()
        },
        ..bounded_input
    };
    assert_eq!(
        next_action(&overloaded),
        PolicyDecision::Overloaded(Overload {
            subject: OverloadSubject::Fact(key(5)),
            resource: ResourceClass::Ram,
            needed: BudgetAmount::Bytes(ByteCount::from(16)),
            available: BudgetAmount::Bytes(ByteCount::from(15)),
        })
    );
}

#[test]
fn contraction_evicts_before_releasing_an_active_bundle() {
    let local = [LocalFact {
        key: key(6),
        tier: StorageTier::Nvme,
        bytes: ByteCount::from(24),
        retention: Retention::Idle,
    }];
    let bundles = [BundleFact {
        capability: CapabilityKind::Analyzer,
        bundle: bundle(15),
        residence: BundleResidence::Active,
    }];
    let eviction_input = PolicyInput {
        pin: pin(),
        local: &local,
        remote: &[],
        demand: &[],
        bundles: &bundles,
        remote_health: healthy(),
        budget: ResourceBudget {
            storage_pressure: Pressure::Critical,
            ..budget()
        },
    };
    assert_eq!(
        next_action(&eviction_input),
        PolicyDecision::Act(PlacementAction::EvictFromNvme {
            key: key(6),
            bytes: ByteCount::from(24),
        })
    );

    let release_input = PolicyInput {
        local: &[],
        ..eviction_input
    };
    assert_eq!(
        next_action(&release_input),
        PolicyDecision::Act(PlacementAction::ReleaseBundle {
            capability: CapabilityKind::Analyzer,
            bundle: bundle(15),
        })
    );
}

#[test]
fn inconsistency_retains_the_observed_pin_and_stops_at_the_retry_budget() {
    let observed = Pin {
        generation: GenerationId::from_digest([41; 32]),
        snapshot: IndexSnapshotId::from_digest([45; 32]),
    };
    let one_retry = PolicyInput {
        pin: pin(),
        local: &[],
        remote: &[],
        demand: &[],
        bundles: &[],
        remote_health: RemoteHealth::Inconsistent { observed },
        budget: budget(),
    };
    assert_eq!(
        next_action(&one_retry),
        PolicyDecision::Act(PlacementAction::RetryRemote {
            pin: pin(),
            cause: RecoveryCause::Inconsistent { observed },
            retries_remaining: RetryBudget::from(0),
        })
    );

    let exhausted = PolicyInput {
        budget: ResourceBudget {
            retries: RetryBudget::from(0),
            ..budget()
        },
        ..one_retry
    };
    assert_eq!(
        next_action(&exhausted),
        PolicyDecision::RecoveryExhausted {
            pin: pin(),
            cause: RecoveryCause::Inconsistent { observed },
        }
    );
}

#[test]
fn execution_vocabulary_keeps_one_action_owner_through_pending_item_and_terminals() {
    let expected = execution_action();
    let pending_keeps_action = match ExecutionRequest::new(execution_action()).pending() {
        ExecutionPoll::Pending(request) => request.action() == &expected,
        ExecutionPoll::Item(_) | ExecutionPoll::Terminal(_) => false,
    };
    assert!(pending_keeps_action);

    let completed_keeps_action = match ExecutionRequest::new(execution_action()).item() {
        ExecutionPoll::Item(item) => match item.finish() {
            ExecutionPoll::Terminal(ExecutionTerminal::Completed { request }) => {
                request.action() == &expected
            }
            ExecutionPoll::Pending(_) | ExecutionPoll::Item(_) | ExecutionPoll::Terminal(_) => {
                false
            }
        },
        ExecutionPoll::Pending(_) | ExecutionPoll::Terminal(_) => false,
    };
    assert!(completed_keeps_action);

    let failed_keeps_action_and_phase = match ExecutionRequest::new(execution_action())
        .fail(ExecutionPhase::Remote)
    {
        ExecutionPoll::Terminal(ExecutionTerminal::Failed { request, phase }) => {
            request.action() == &expected && phase == ExecutionPhase::Remote
        }
        ExecutionPoll::Pending(_) | ExecutionPoll::Item(_) | ExecutionPoll::Terminal(_) => false,
    };
    assert!(failed_keeps_action_and_phase);

    let cancelled_keeps_action = match ExecutionRequest::new(execution_action()).cancel() {
        ExecutionPoll::Terminal(terminal @ ExecutionTerminal::Cancelled { .. }) => {
            terminal.action() == &expected
        }
        ExecutionPoll::Pending(_) | ExecutionPoll::Item(_) | ExecutionPoll::Terminal(_) => false,
    };
    assert!(cancelled_keeps_action);
}
