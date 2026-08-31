//! Exercises the `heart-adaptive` tests outage-recovery contract through its observable boundary.
//! The cases target malformed, partial, reordered, and resource-constrained behavior.
//! Assertions retain exact typed causes so regressions cannot pass through lossy errors.
//! The outage path preserves local data, then expands local capability, then retries remotely.

use heart_adaptive::{
    BatteryState, BundleFact, BundleState, ByteCount, CapabilityKind, ContentId, Demand,
    DemandLevel, FactKey, GenerationId, IndexSnapshotId, LatencyMicros, LocalFact, ObjectDomain,
    OperationBudget, Pin, PlacementAction, PolicyDecision, PolicyInput, Pressure, RecoveryCause,
    RemoteFact, RemoteHealth, ResourceBudget, Retention, RetryBudget, StorageTier, next_action,
};

fn pin() -> Pin {
    Pin {
        generation: GenerationId::from_digest([7; 32]),
        snapshot: IndexSnapshotId::from_digest([11; 32]),
    }
}

fn key(fact: u8) -> FactKey {
    FactKey {
        pin: pin(),
        object: ContentId::<ObjectDomain>::from_digest([fact; 32]),
    }
}

fn bundle(seed: u8) -> ContentId<heart_adaptive::CapabilityDomain> {
    ContentId::from_digest([seed; 32])
}

fn budget(memory_pressure: Pressure) -> ResourceBudget {
    ResourceBudget {
        ram_free: ByteCount::from(64),
        nvme_free: ByteCount::from(64),
        operations: OperationBudget::from(2),
        retries: RetryBudget::from(2),
        memory_pressure,
        storage_pressure: Pressure::Relaxed,
        battery: BatteryState::Normal,
    }
}

fn assert_action(input: &PolicyInput<'_>, expected: PlacementAction) {
    assert_eq!(next_action(input), PolicyDecision::Act(expected));
}

struct OutageFixture {
    cold: LocalFact,
    remote: RemoteFact,
    demand: Demand,
    analyzer: BundleFact,
}

impl OutageFixture {
    fn new() -> Self {
        Self {
            cold: LocalFact {
                key: key(3),
                tier: StorageTier::Ram,
                bytes: ByteCount::from(48),
                retention: Retention::Idle,
            },
            remote: RemoteFact {
                key: key(9),
                bytes: ByteCount::from(32),
            },
            demand: Demand {
                key: key(9),
                level: DemandLevel::Hot,
                latency_budget: LatencyMicros::from(500),
            },
            analyzer: BundleFact {
                capability: CapabilityKind::Analyzer,
                bundle: bundle(13),
                state: BundleState::AvailableRequired,
            },
        }
    }

    fn input<'facts>(
        &'facts self,
        local: &'facts [LocalFact],
        bundles: &'facts [BundleFact],
        memory_pressure: Pressure,
    ) -> PolicyInput<'facts> {
        PolicyInput {
            pin: pin(),
            local,
            remote: core::slice::from_ref(&self.remote),
            demand: core::slice::from_ref(&self.demand),
            bundles,
            remote_health: RemoteHealth::Outage,
            budget: budget(memory_pressure),
        }
    }
}

#[test]
fn outage_recovery_preserves_then_acquires_then_retries() {
    let fixture = OutageFixture::new();
    let local = [fixture.cold];
    let available = [fixture.analyzer];
    let preserve_input = fixture.input(&local, &available, Pressure::Elevated);
    assert_action(
        &preserve_input,
        PlacementAction::PreserveToNvme {
            key: key(3),
            bytes: ByteCount::from(48),
        },
    );

    let recovered_local = [LocalFact {
        tier: StorageTier::Nvme,
        ..fixture.cold
    }];
    let acquire_input = fixture.input(&recovered_local, &available, Pressure::Relaxed);
    assert_action(
        &acquire_input,
        PlacementAction::AcquireBundle {
            capability: CapabilityKind::Analyzer,
            bundle: bundle(13),
        },
    );

    let active = [BundleFact {
        state: BundleState::ActiveInUse,
        ..available[0]
    }];
    let retry_input = PolicyInput {
        bundles: &active,
        ..acquire_input
    };
    assert_action(
        &retry_input,
        PlacementAction::RetryRemote {
            pin: pin(),
            cause: RecoveryCause::Outage,
            retries_remaining: RetryBudget::from(1),
        },
    );
}
