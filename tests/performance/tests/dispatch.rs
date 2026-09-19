//! Structural contracts for the owner dispatch boundary.
//!
//! The lower scheduler tests in `structural.rs` exercise admission arithmetic.
//! These checks go through the engine dispatcher, where semantic admission,
//! publication, and reusable lookup share one owner transaction.  The tests
//! intentionally use state transitions and retained allocation identity; a
//! clock is never the oracle.

#![forbid(unsafe_code)]

use backend_engine::Blake3AuthorityVerifier;
use backend_engine::dispatch::{
    DispatchCompletion, DispatchError, DispatchPlan, Dispatcher, OutputAdmissionValidator,
    RemoteAuthorityPolicy, RetainedOutput, SemanticCoverageAdmissionError, SemanticCoverageBinding,
    SemanticCoverageValidator, UntrustedSemanticCoverageClaim,
};
use backend_execution::{
    AuthorityVersion, Budget, CompletionCost, CostObservation, CostSnapshot, LocalCapability,
    LocalState, ObservationError, OutputEquivalence, PlacementClass, ReadManifestId, RecipeId,
    RemoteCapability, RemoteState, ResourceVector, ScheduleRequest, VersionedWorkIdentity,
};
use backend_replication::{
    AuthorityEpoch, NegotiatedCapabilities, RevocationVersion, TransportLimits,
};
use backend_semantic::DependencyManifest;
use backend_version::{CoverageWitness, Relation, RelationState, partial_coverage};
use std::sync::Arc;

type TestResult = Result<(), Box<dyn std::error::Error>>;

#[derive(Debug)]
struct DispatchRelation;

impl Relation for DispatchRelation {
    const DOMAIN: u8 = 0xa1;
    const TYPE: u16 = 1;
    type Key = u64;
    type Value = u64;

    fn encode_key(value: &u64, out: &mut Vec<u8>) {
        out.extend_from_slice(&value.to_be_bytes());
    }

    fn encode_value(value: &u64, out: &mut Vec<u8>) {
        out.extend_from_slice(&value.to_be_bytes());
    }
}

#[derive(Clone, Copy, Debug, Default)]
struct AcceptingAuthority;

impl OutputAdmissionValidator for AcceptingAuthority {
    fn validate(
        &self,
        output: backend_execution::OutputVersion,
        bytes: &[u8],
        semantic: &backend_engine::dispatch::CompleteSemanticCoverage,
    ) -> Result<RetainedOutput, DispatchError> {
        if !semantic.is_complete() || backend_execution::OutputVersion::from_value(bytes) != output
        {
            return Err(DispatchError::OutputMismatch);
        }
        Ok(RetainedOutput::from_bytes(output, bytes))
    }

    fn validate_shared(
        &self,
        output: backend_execution::OutputVersion,
        bytes: &Arc<Vec<u8>>,
        semantic: &backend_engine::dispatch::CompleteSemanticCoverage,
    ) -> Result<RetainedOutput, DispatchError> {
        if !semantic.is_complete()
            || backend_execution::OutputVersion::from_value(bytes.as_slice()) != output
        {
            return Err(DispatchError::OutputMismatch);
        }
        Ok(RetainedOutput::from_shared(output, Arc::clone(bytes)))
    }
}

impl SemanticCoverageValidator for AcceptingAuthority {
    fn validate(
        &self,
        binding: &SemanticCoverageBinding,
        claim: &UntrustedSemanticCoverageClaim,
    ) -> Result<(), SemanticCoverageAdmissionError> {
        if claim.asserted_complete()
            && claim.scope() == 1
            && claim.read_manifest() == binding.read_manifest()
            && claim.authority() == binding.authority()
        {
            Ok(())
        } else {
            Err(SemanticCoverageAdmissionError::Rejected)
        }
    }

    fn validate_manifest(
        &self,
        _binding: &SemanticCoverageBinding,
        _claim: &UntrustedSemanticCoverageClaim,
        manifest: &DependencyManifest,
    ) -> Result<(), SemanticCoverageAdmissionError> {
        manifest
            .facts()
            .is_empty()
            .then_some(())
            .ok_or(SemanticCoverageAdmissionError::Rejected)
    }
}

fn identity(
    seed: u64,
) -> Result<VersionedWorkIdentity<DispatchRelation>, Box<dyn std::error::Error>> {
    let state = RelationState::<DispatchRelation>::from_entries(
        [(seed, seed)],
        CoverageWitness::Partial(partial_coverage(seed)),
    )?;
    Ok(VersionedWorkIdentity::new(
        RecipeId::from_value(b"performance-dispatch-recipe"),
        state.root(),
        ReadManifestId::from_value(b"performance-dispatch-reads"),
        AuthorityVersion::from_value(b"performance-dispatch-authority"),
        OutputEquivalence::from_value(b"performance-dispatch-output"),
    ))
}

fn request(
    identity: VersionedWorkIdentity<DispatchRelation>,
    local: LocalState,
    remote: RemoteState,
    class: PlacementClass,
) -> Result<ScheduleRequest<DispatchRelation>, Box<dyn std::error::Error>> {
    let local = LocalCapability::admit(&identity, local, 0, 1_000, &accept_local)?;
    let remote = RemoteCapability::admit(
        &identity,
        &NegotiatedCapabilities {
            protocol: 1,
            schemas: Vec::new(),
            recipes: Vec::new(),
            limits: TransportLimits::default(),
            max_resources: backend_replication::ResourceEnvelope::UNBOUNDED,
        },
        0,
        1_000,
        &AcceptRemote(remote),
    )?;
    let costs = CostSnapshot::admit(
        &identity,
        CostObservation {
            local: 1,
            remote: CompletionCost {
                execution: 1,
                ..CompletionCost::default()
            },
            observed_at: 0,
            expires_at: 1_000,
            confidence_per_mille: 1_000,
        },
        &accept_cost,
    )?;
    Ok(
        ScheduleRequest::new(identity, 1, 1, class, local, remote, costs)
            .with_charge(32, ResourceVector::zero())
            .pure(true),
    )
}

// These callbacks intentionally keep the fallible verifier ABI even though
// this fixture accepts every observation; production verifiers may reject.
#[allow(
    clippy::unnecessary_wraps,
    reason = "preserve the verifier callback ABI"
)]
fn accept_local(
    _: &VersionedWorkIdentity<DispatchRelation>,
    _: LocalState,
) -> Result<(), ObservationError> {
    Ok(())
}

struct AcceptRemote(RemoteState);

impl backend_execution::RemoteCapabilityVerifier<DispatchRelation> for AcceptRemote {
    fn verify_remote(
        &self,
        _: &VersionedWorkIdentity<DispatchRelation>,
        _: &NegotiatedCapabilities,
    ) -> Result<RemoteState, ObservationError> {
        Ok(self.0)
    }
}

#[allow(
    clippy::unnecessary_wraps,
    reason = "preserve the verifier callback ABI"
)]
fn accept_cost(
    _: &VersionedWorkIdentity<DispatchRelation>,
    _: &CostObservation,
) -> Result<(), ObservationError> {
    Ok(())
}

fn dispatcher() -> Dispatcher<AcceptingAuthority, Blake3AuthorityVerifier> {
    Dispatcher::new(
        Budget {
            operations: 512,
            bytes: 64 * 1024,
            ..Budget::zero()
        },
        AcceptingAuthority,
        Blake3AuthorityVerifier::default(),
        RemoteAuthorityPolicy::LocalOnly,
    )
}

fn semantic(
    dispatcher: &Dispatcher<AcceptingAuthority, Blake3AuthorityVerifier>,
    identity: &VersionedWorkIdentity<DispatchRelation>,
) -> Result<backend_engine::dispatch::CompleteSemanticCoverage, DispatchError> {
    dispatcher.admit_semantic_with_manifest(
        identity,
        UntrustedSemanticCoverageClaim::complete_claim(
            identity,
            1,
            b"performance-dispatch-witness".to_vec().into_boxed_slice(),
            AuthorityEpoch(1),
            RevocationVersion(1),
        ),
        DependencyManifest::new(Vec::new()).map_err(|_| DispatchError::OutputContract)?,
    )
}

#[test]
fn cold_local_route_never_enters_remote_dispatch() -> TestResult {
    let dispatcher = dispatcher();
    let request = request(
        identity(1)?,
        LocalState::Ready,
        RemoteState::Unavailable,
        PlacementClass::LocalPreferred,
    )?;
    let plan = dispatcher.plan(request)?;
    match plan {
        DispatchPlan::Scheduled(schedule) => {
            assert_eq!(
                schedule.decision(),
                backend_execution::PlacementDecision::Local
            );
        }
        DispatchPlan::Reused(_) | DispatchPlan::Waiting(_) => {
            return Err("cold work must be a local schedule".into());
        }
    }
    Ok(())
}

#[test]
fn warm_dispatch_requires_owner_reuse_context_without_mutating_publication() -> TestResult {
    let dispatcher = dispatcher();
    let identity = identity(2)?;
    let semantic = semantic(&dispatcher, &identity)?;
    let request = request(
        identity,
        LocalState::Ready,
        RemoteState::Unavailable,
        PlacementClass::LocalPreferred,
    )?;
    let bytes = Arc::new(b"performance-warm-output".to_vec());
    let first = dispatcher.complete_local_shared(
        dispatcher.plan(request)?,
        Arc::clone(&bytes),
        semantic,
        1,
    )?;
    let first = match first {
        DispatchCompletion::Accepted(receipt) => receipt,
        DispatchCompletion::Reused(_) | DispatchCompletion::Waiting(_) => {
            return Err("first dispatch did not publish".into());
        }
    };
    let retained = dispatcher.scheduler().output_lookup().retained_bytes();
    let available = dispatcher.scheduler().admission().available();
    let reusable = first
        .reusable()
        .ok_or("manifest-backed completion did not retain reuse context")?;
    let warm = dispatcher
        .scheduler()
        .output_lookup()
        .lookup(first.key(), &reusable.context())
        .ok_or("published output was not reusable with its exact context")?;
    assert_eq!(warm.output(), first.output());
    assert_eq!(dispatcher.scheduler().output_lookup().len(), 1);
    assert_eq!(
        dispatcher.scheduler().output_lookup().retained_bytes(),
        retained
    );
    assert_eq!(dispatcher.scheduler().admission().available(), available);
    assert!(Arc::ptr_eq(&bytes, &warm.canonical_bytes_arc()));

    // Plain planning has no engine-issued reuse context. It must fail closed
    // while leaving the immutable publication available to the owner path.
    for _ in 0..256 {
        assert!(matches!(
            dispatcher.plan(request),
            Err(DispatchError::ReuseContextRequired)
        ));
        assert_eq!(dispatcher.scheduler().output_lookup().len(), 1);
        assert_eq!(
            dispatcher.scheduler().output_lookup().retained_bytes(),
            retained
        );
        assert_eq!(dispatcher.scheduler().admission().available(), available);
    }
    Ok(())
}
