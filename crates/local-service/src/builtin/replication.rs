//! Owner-side remote dispatch state machine.
//!
//! This module owns transport reconnect, closure prelude, bounded pending
//! tickets, result demultiplexing, and local fallback. The parent profile
//! supplies the checked model, validators, and canonical recipe helpers.

#[cfg(test)]
use super::profile_descriptor;
use super::{
    Arc, AuthorityClaim, AuthorityEpoch, BuiltinAuthorityVerifier, BuiltinModel, BuiltinProfile,
    BuiltinValidator, ClosureRootOffer, CostSnapshot, DeltaPlanner, DispatchAttemptKey,
    DispatchPlan, DispatchRecoveryAction, Duration, EngineStatus, ExpectedInput, Frame, Instant,
    LocalCapability, LocalState, OutputVersion, PendingRemoteKey, PlacementClass, ProductInput,
    ProductRelation, ProductSourceDeltaFacts, ProductSourceSnapshot, ProfileDescriptor, ProfileIds,
    RebuildScope, Receiver, RefreshChoice, RefreshCost, RelationState, RemoteAuthorityPolicy,
    RemoteCapability, RemoteDispatchContract, RemoteState, ReplicationAdmission, ResourceEnvelope,
    ResourceVector, RevocationVersion, ScheduleRequest, TransportLimits, TransportMessage,
    TryRecvError, UntrustedSemanticCoverageClaim, VersionedWorkIdentity, WireAuthorityPolicy,
    WireIdentity, WireRecipeRequest, WorkspaceRoot, connect_worker, daemon_replicate,
    execution_input_basis, execution_manifest, execution_resources, mpsc,
    product_dependency_manifest, product_input_version, product_source_fixture_with_authority,
    thread, validate_canonical_output,
};
use backend_engine::{
    ProductProjection, ProductProjectionBuilder, WorkspaceRelationNodePage, product_output_bytes,
};

#[path = "replication/admission.rs"]
mod admission;
#[path = "replication/closure.rs"]
mod closure;
#[path = "replication/dispatch.rs"]
mod dispatch;
#[path = "replication/pending.rs"]
mod pending;
#[path = "replication/reconnect.rs"]
mod reconnect;
#[path = "replication/recovery.rs"]
mod recovery;
use pending::PendingIndex;
use reconnect::ReconnectCircuit;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct RefreshObservation {
    pub(super) prior: backend_engine::StateRoot<ProductRelation>,
    pub(super) target: backend_engine::StateRoot<ProductRelation>,
    pub(super) choice: RefreshChoice,
    pub(super) source_changed: bool,
}

fn output_for_relation(
    ids: ProfileIds,
    _input_basis: WorkspaceRoot,
    _relation: &RelationState<ProductRelation>,
) -> Vec<u8> {
    ids.output_prefix.to_vec()
}

/// Computes the deterministic product result from an owner snapshot without
/// retaining a materialized relation in a pending ticket.  The snapshot is a
/// shared checked head; opening its selected relation is lazy and only the
/// root object is needed for the result identity.
fn output_for_snapshot(
    ids: ProfileIds,
    input_basis: WorkspaceRoot,
    snapshot: &ProductSourceSnapshot,
) -> Result<Vec<u8>, String> {
    let projection = project_snapshot(snapshot)?;
    Ok(product_output_bytes(ids, input_basis, projection))
}

/// Streams one checked workspace relation in canonical order. Only a bounded
/// page and a depth-first frontier are retained; the source relation is never
/// materialized as a second map or row vector.
fn project_snapshot(snapshot: &ProductSourceSnapshot) -> Result<ProductProjection, String> {
    const PAGE_ROWS: usize = 256;
    let relation = snapshot.relation();
    let root = relation.root_node().map_err(|error| error.to_string())?;
    let expected_rows = root.row_count();
    let mut builder = ProductProjectionBuilder::new();
    let mut stack = vec![root];
    while let Some(node) = stack.pop() {
        let mut offset = 0_usize;
        let mut children = Vec::new();
        loop {
            match relation
                .node_page(node, offset, PAGE_ROWS)
                .map_err(|error| error.to_string())?
            {
                WorkspaceRelationNodePage::Leaf {
                    entries, has_more, ..
                } => {
                    let count = entries.len();
                    for (key, value) in &entries {
                        builder.push(key, value).map_err(str::to_owned)?;
                    }
                    if !has_more {
                        break;
                    }
                    offset = offset
                        .checked_add(count)
                        .ok_or_else(|| "product leaf cursor overflow".to_owned())?;
                }
                WorkspaceRelationNodePage::Branch {
                    children: page,
                    has_more,
                    ..
                } => {
                    let count = page.len();
                    children.extend(
                        page.iter()
                            .map(backend_engine::WorkspaceRelationChild::handle),
                    );
                    if !has_more {
                        break;
                    }
                    offset = offset
                        .checked_add(count)
                        .ok_or_else(|| "product branch cursor overflow".to_owned())?;
                }
            }
        }
        stack.extend(children.into_iter().rev());
    }
    let projection = builder.finish(relation.root());
    if projection.projects().saturating_add(projection.files()) != expected_rows {
        return Err("product projection row count does not match its admitted root".to_owned());
    }
    Ok(projection)
}

/// Owner-side admission adapter for the compiled worker route. The adapter
/// accepts only the one registered recipe and converts the incoming wire
/// request into a scheduler-owned plan and contract. Results are admitted by
/// the daemon's retained pending envelope before this method reports success.
#[derive(Debug)]
pub(super) struct BuiltinReplication {
    profile: Arc<ProfileDescriptor>,
    limits: TransportLimits,
    capabilities: backend_engine::CapabilityManifest,
    /// Last closure root admitted by the worker. A capability negotiation
    /// cannot mark this warm; only the authenticated root-summary response
    /// does. This option is the sole warm signal and is always tied to the
    /// exact root being dispatched.
    remote_root: Option<backend_engine::MerkleRoot>,
    /// Optional worker endpoint retained for bounded reconnect attempts.
    worker_endpoint: Option<backend_engine::UnixEndpointPath>,
    /// Credential used by the authenticated worker transport handshake.
    worker_secret: [u8; 32],
    /// Whether the current daemon-owned transport is usable.
    transport_ready: bool,
    /// Bounded connect and handshake timeout for worker recovery.
    worker_timeout: Duration,
    /// At most one bounded reconnect attempt may be in flight. The worker
    /// connector performs TCP/Unix authentication and capability negotiation
    /// off the owner thread, then publishes the ready transport here.
    reconnect: Option<Receiver<Result<Box<dyn backend_engine::RemoteTransport>, String>>>,
    /// Bounded circuit controlling retry bursts and cooldown generations.
    /// It permits one connector at a time without turning a finite outage
    /// into permanent process-level disconnection.
    reconnect_circuit: ReconnectCircuit,
    /// Monotonic owner-side closure correlation. A reconnect or replacement
    /// closure can therefore never accept a delayed acknowledgement from an
    /// earlier session.
    next_correlation: u64,
    /// Checked source refresh planner shared by local and remote routes.
    delta_planner: DeltaPlanner,
    /// Last source root observed by this profile. This is a bounded routing
    /// hint; the owner catalog remains the authority for reusable output.
    last_relation_root: Option<backend_engine::StateRoot<ProductRelation>>,
    /// Correlation and deadline index for owner-retained remote tickets.
    /// The index keeps result admission and expiry independent of product
    /// payload size and makes replacement attempts generation-safe.
    pending: PendingIndex,
    pending_dispatch: Option<PendingDispatch>,
    pending_closure: Option<PendingClosure>,
    /// Restart actions are consumed by this owner adapter before it accepts
    /// new worker demand. A recovered remote lease is never left inert.
    recovered: std::collections::VecDeque<DispatchRecoveryAction>,
    /// One recovered attempt is being reconstructed through the normal
    /// local admission path. This capability is owner-thread affine.
    recovery_fallback: Option<DispatchAttemptKey>,
}

#[derive(Debug)]
struct PendingProduct {
    ids: ProfileIds,
    input_basis: WorkspaceRoot,
    /// Shared checked head used to re-open the source relation only if the
    /// owner has to execute a local fallback.
    snapshot: ProductSourceSnapshot,
}

#[derive(Debug)]
struct PendingDispatch {
    plan: DispatchPlan<ProductRelation>,
    contract: RemoteDispatchContract,
    identity: VersionedWorkIdentity<ProductRelation>,
    ids: ProfileIds,
    input_basis: WorkspaceRoot,
    /// Shared checked head retained instead of cloning every relation row.
    snapshot: ProductSourceSnapshot,
}

#[derive(Debug)]
struct RemotePlanRequest {
    plan: DispatchPlan<ProductRelation>,
    contract: RemoteDispatchContract,
    identity: VersionedWorkIdentity<ProductRelation>,
    ids: ProfileIds,
    input_basis: WorkspaceRoot,
    snapshot: ProductSourceSnapshot,
}

#[derive(Debug)]
struct ClosurePlanRequest {
    plan: DispatchPlan<ProductRelation>,
    contract: RemoteDispatchContract,
    identity: VersionedWorkIdentity<ProductRelation>,
    ids: ProfileIds,
    input_basis: WorkspaceRoot,
    snapshot: ProductSourceSnapshot,
    expected_root: backend_engine::MerkleRoot,
}

impl From<PendingDispatch> for RemotePlanRequest {
    fn from(pending: PendingDispatch) -> Self {
        Self {
            plan: pending.plan,
            contract: pending.contract,
            identity: pending.identity,
            ids: pending.ids,
            input_basis: pending.input_basis,
            snapshot: pending.snapshot,
        }
    }
}

#[derive(Debug)]
struct PendingClosure {
    correlation: u64,
    expected_root: backend_engine::MerkleRoot,
    source: Option<crate::reconcile::ProductPageSource<ProductRelation>>,
    input_value: ProductInput,
    input_version: [u8; 32],
    authority: AuthorityClaim,
    frames: Option<Vec<Frame>>,
    next_frame: usize,
    input: Option<Frame>,
    summary_admitted: bool,
    awaiting_complete: bool,
    awaiting_need: bool,
    need_cursor: Option<u32>,
    next_transfer: u64,
    needed_versions: Option<std::collections::BTreeSet<[u8; 32]>>,
    deadline: Instant,
}

impl BuiltinReplication {
    pub(super) fn new(
        limits: TransportLimits,
        profile: Arc<ProfileDescriptor>,
        worker_endpoint: Option<backend_engine::UnixEndpointPath>,
        worker_secret: [u8; 32],
        worker_timeout: Duration,
    ) -> Result<Self, String> {
        let capabilities = execution_manifest(limits, &profile)?;
        Ok(Self {
            profile,
            limits,
            capabilities,
            remote_root: None,
            worker_endpoint,
            worker_secret,
            transport_ready: false,
            worker_timeout,
            reconnect: None,
            reconnect_circuit: ReconnectCircuit::new(Instant::now()),
            next_correlation: 1,
            delta_planner: DeltaPlanner::new(4),
            last_relation_root: None,
            pending: PendingIndex::default(),
            pending_dispatch: None,
            pending_closure: None,
            recovered: std::collections::VecDeque::new(),
            recovery_fallback: None,
        })
    }

    /// Chooses a refresh route from the model's checked lazy-update facts.
    /// Unknown cross-root change cardinality fails closed to a product rebuild;
    /// a warm remote may serve that explicitly expensive scope.
    pub(super) fn refresh_choice(
        &self,
        source: Option<&ProductSourceSnapshot>,
        root: backend_engine::StateRoot<ProductRelation>,
    ) -> Result<RefreshObservation, String> {
        let prior = self.last_relation_root.unwrap_or(root);
        let source_changed = prior != root;
        let adjacent = source.is_some_and(|source| source.transition().binds(prior, root));
        let facts = if adjacent {
            source
                .map(ProductSourceSnapshot::delta_facts)
                .unwrap_or_default()
        } else {
            ProductSourceDeltaFacts::default()
        };
        let changed_items = if prior == root {
            0
        } else {
            usize::try_from(facts.changed_items).map_err(|_| "change count overflow".to_owned())?
        };
        let delta_plan = self.delta_planner.plan(&prior, &root, changed_items);
        let scope = delta_plan.scope().unwrap_or(RebuildScope::Item);
        let row_count = if let Some(source) = source {
            source.retention_facts().relation_rows
        } else {
            0
        };
        let rebuild_bytes = row_count
            .checked_mul(96)
            .ok_or_else(|| "rebuild cost overflow".to_owned())?;
        let cost = RefreshCost {
            delta_bytes: if prior == root || facts.changed_items != 0 {
                facts.changed_bytes
            } else {
                u64::MAX
            },
            rebuild_bytes,
            delta_work: facts.changed_nodes,
            rebuild_work: row_count,
        };
        Ok(RefreshObservation {
            prior,
            target: root,
            choice: backend_engine::choose_refresh(cost, scope),
            source_changed,
        })
    }

    /// Converts checked source refresh facts into the route policy class. A
    /// changed source is remote-optional: the lower scheduler keeps local
    /// work immediately executable during cold start, then the owner cost
    /// model may select remote acceleration after verified samples. An
    /// unchanged source remains local-first because it should normally be a
    /// durable reuse hit before route admission.
    fn placement_for_refresh(
        observation: RefreshObservation,
        retention: backend_engine::ProductSourceRetentionFacts,
    ) -> PlacementClass {
        let local_arrangement_ready =
            retention.relation_rows != 0 && retention.retained_objects != 0;
        if !observation.source_changed && local_arrangement_ready {
            return PlacementClass::LocalPreferred;
        }
        match observation.choice {
            RefreshChoice::Delta | RefreshChoice::Rebuild(_) => PlacementClass::RemoteOptional,
        }
    }

    /// Commits a source observation only after the corresponding plan has
    /// reached a reusable, local-completed, or owner-retained remote state.
    pub(super) fn commit_refresh_observation(&mut self, observation: RefreshObservation) {
        if self.last_relation_root.is_none() || self.last_relation_root == Some(observation.prior) {
            self.last_relation_root = Some(observation.target);
        }
    }

    /// Installs the bounded restart actions transferred from the engine
    /// daemon. The adapter owns their product-specific decoding and local
    /// relation rebind.
    pub(super) fn install_recovered(
        &mut self,
        actions: impl IntoIterator<Item = DispatchRecoveryAction>,
    ) {
        self.recovered.extend(actions);
    }

    fn owner_now() -> u64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(1, |duration| {
                u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
            })
            .max(1)
    }

    fn owner_instant() -> Instant {
        Instant::now()
    }

    fn owner_deadline(&self) -> Instant {
        let now = Self::owner_instant();
        now.checked_add(self.worker_timeout)
            .map_or(now, |deadline| deadline)
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    fn product_replication() -> BuiltinReplication {
        let profile = profile_descriptor(BuiltinProfile::Product).expect("product profile");
        BuiltinReplication::new(
            TransportLimits::default(),
            profile,
            None,
            [0; 32],
            Duration::from_millis(1),
        )
        .expect("replication state")
    }

    fn roots() -> (
        backend_engine::StateRoot<ProductRelation>,
        backend_engine::StateRoot<ProductRelation>,
    ) {
        let profile = profile_descriptor(BuiltinProfile::Product).expect("product profile");
        let first = product_source_fixture_with_authority(false, profile.ids.authority)
            .expect("first source relation");
        let second = product_source_fixture_with_authority(true, profile.ids.authority)
            .expect("second source relation");
        (first.root(), second.root())
    }

    #[test]
    fn refresh_observation_is_pure_for_invalid_request() {
        let mut replication = product_replication();
        let (prior, target) = roots();
        replication.last_relation_root = Some(prior);
        let profile = profile_descriptor(BuiltinProfile::Product).expect("product profile");
        let identity = VersionedWorkIdentity::new(
            profile.ids.recipe,
            target,
            profile.ids.read_manifest,
            profile.ids.authority,
            profile.ids.equivalence,
        );
        let claim = WireIdentity::from_typed(&identity.recipe);
        let invalid = WireRecipeRequest {
            attempt: backend_engine::AttemptId::new(1).expect("attempt"),
            recipe: claim,
            work_key: WireIdentity::from_typed(&identity.work_key()),
            inputs: vec![
                WireIdentity::from_typed(&identity.recipe);
                TransportLimits::default().max_inputs.saturating_add(1)
            ],
            read_manifest: WireIdentity::from_typed(&identity.read_manifest),
            scope: 1,
            authority: WireAuthorityPolicy {
                id: WireIdentity::from_typed(&identity.authority),
                minimum_epoch: AuthorityEpoch(1),
                revocation_version: RevocationVersion(1),
            },
            resources: ResourceEnvelope::default(),
            fence: backend_engine::Fence::from_u64(1).expect("fence"),
            cancellation: backend_engine::CancellationId::new([1; 32]).expect("cancellation"),
            input_basis: backend_engine::WorkspaceRootClaim::from_bytes([0; 32]),
        };
        assert!(invalid.validate(TransportLimits::default()).is_err());
        assert_eq!(replication.last_relation_root, Some(prior));
        let observation = replication
            .refresh_choice(None, target)
            .expect("pure refresh observation");
        assert_eq!(replication.last_relation_root, Some(prior));
        assert_eq!(observation.prior, prior);
        assert_eq!(observation.target, target);
    }

    #[test]
    fn backpressure_observation_does_not_advance_refresh_state() {
        let mut replication = product_replication();
        let (prior, target) = roots();
        replication.last_relation_root = Some(prior);
        let observation = replication
            .refresh_choice(None, target)
            .expect("pure refresh observation");
        // The pending owner slots represent admission backpressure. A
        // planner observation is not committed until a route owns its plan;
        // this pure observation therefore leaves the retained state alone.
        assert_eq!(replication.last_relation_root, Some(prior));
        assert_eq!(observation.prior, prior);
        assert_eq!(observation.target, target);
    }

    #[test]
    fn skipped_intermediate_commit_fails_closed_to_product_rebuild() {
        let mut replication = product_replication();
        let (first, final_root) = roots();
        replication.last_relation_root = Some(first);
        let observation = replication
            .refresh_choice(None, final_root)
            .expect("refresh observation");
        assert_eq!(
            observation.choice,
            RefreshChoice::Rebuild(RebuildScope::Product)
        );
        assert!(observation.source_changed);
        assert_eq!(replication.last_relation_root, Some(first));
    }

    #[test]
    fn committed_observation_advances_only_from_the_expected_prior() {
        let mut replication = product_replication();
        let (prior, target) = roots();
        replication.last_relation_root = Some(prior);
        let observation = RefreshObservation {
            prior: target,
            target: prior,
            choice: RefreshChoice::Rebuild(RebuildScope::Product),
            source_changed: true,
        };
        replication.commit_refresh_observation(observation);
        assert_eq!(replication.last_relation_root, Some(prior));
    }
}
