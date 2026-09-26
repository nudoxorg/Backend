//! Durable engine session: open a workspace owner and drive its daemon.
//!
//! Callers keep [`Engine`] at the crate root. Opening, planning, local
//! completion, and the pending-remote path all go through this session so
//! the composition root stays a list of modules and façades.

use crate::daemon::{self, Daemon, DaemonConfig, DaemonError, DaemonHandle};
use crate::dispatch::{
    CompleteSemanticCoverage, DispatchCompletion, DispatchError, DispatchJournal,
    DispatchJournalLimits, DispatchPlan, Dispatcher, OutputAdmissionValidator, PendingRemoteKey,
    RemoteDispatchContract, RemoteTransport, SemanticCoverageValidator, SemanticInvalidationReport,
    UnconfiguredAuthorityVerifier, UnconfiguredOutputValidator, UntrustedSemanticCoverageClaim,
};
use crate::fault::Faults;
use crate::workspace::{WorkspaceError, WorkspaceHead, WorkspaceModel, WorkspaceOwner};
use backend_execution::{ScheduleRequest, VersionedWorkIdentity};
use backend_replication::{
    AttestationVerifier, CapabilityManifest, NegotiatedCapabilities, TransportMessage,
};
use backend_semantic::DependencyManifest;
use backend_store::RelationAdmissionRegistry;
use backend_version::Relation;

/// Generic engine composition around one daemon owner.
pub struct Engine<M, V = UnconfiguredOutputValidator, A = UnconfiguredAuthorityVerifier>
where
    M: WorkspaceModel,
    V: OutputAdmissionValidator + SemanticCoverageValidator,
    A: AttestationVerifier + Send + Sync + 'static,
{
    daemon: Daemon<M, V, A>,
}

/// Returns the owner clock persisted in durable lease and dispatch records.
///
/// `Instant` is intentionally unsuitable here: it is process-relative and
/// restarts at zero after a reopen, which would make an expired remote lease
/// appear fresh.  The durable owner protocol uses the wall-clock epoch in
/// milliseconds so a new process observes the same time domain as the one
/// that admitted the attempt.
fn owner_wall_clock_millis() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(1, |duration| {
            u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
        })
        .max(1)
}

impl<M, V, A> std::fmt::Debug for Engine<M, V, A>
where
    M: WorkspaceModel,
    V: OutputAdmissionValidator + SemanticCoverageValidator,
    A: AttestationVerifier + Send + Sync + 'static,
{
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Engine")
            .field("daemon", &self.daemon)
            .finish()
    }
}

impl<M, V, A> Engine<M, V, A>
where
    M: WorkspaceModel,
    V: OutputAdmissionValidator + SemanticCoverageValidator,
    A: AttestationVerifier + Send + Sync + 'static,
{
    /// Opens the durable local engine and acquires workspace ownership.
    ///
    /// # Errors
    ///
    /// Returns an error when ownership, durable journals, or the composed
    /// daemon cannot be opened and recovered.
    pub fn open(
        directory: impl AsRef<std::path::Path>,
        model: M,
        genesis: WorkspaceHead,
        dispatcher: Dispatcher<V, A>,
        config: DaemonConfig,
    ) -> Result<Self, WorkspaceError> {
        let directory = directory.as_ref().to_owned();
        let owner = WorkspaceOwner::open(&directory, model, genesis)?;
        let (dispatch_journal, _recovery) = DispatchJournal::open(
            directory.join(daemon::DISPATCH_JOURNAL_FILE),
            DispatchJournalLimits::default(),
        )
        .map_err(|error| WorkspaceError::Io(error.to_string()))?;
        let now = owner_wall_clock_millis();
        let daemon =
            Daemon::new_with_dispatch_journal(owner, dispatcher, config, dispatch_journal, now)
                .map_err(|error| WorkspaceError::Io(error.to_string()))?;
        Ok(Self { daemon })
    }

    /// Opens the durable engine with an explicit canonical relation registry.
    /// Process compositions that persist custom relation roots use this path
    /// so the store and recovery validate the same relation grammar.
    ///
    /// # Errors
    ///
    /// Returns an error when ownership, relation admission, durable journals,
    /// or the composed daemon cannot be opened and recovered.
    pub fn open_with_relation_registry(
        directory: impl AsRef<std::path::Path>,
        model: M,
        genesis: WorkspaceHead,
        dispatcher: Dispatcher<V, A>,
        config: DaemonConfig,
        relation_registry: RelationAdmissionRegistry,
    ) -> Result<Self, WorkspaceError> {
        let directory = directory.as_ref().to_owned();
        let owner =
            WorkspaceOwner::open_with_registry(&directory, model, genesis, relation_registry)?;
        let (dispatch_journal, _recovery) = DispatchJournal::open(
            directory.join(daemon::DISPATCH_JOURNAL_FILE),
            DispatchJournalLimits::default(),
        )
        .map_err(|error| WorkspaceError::Io(error.to_string()))?;
        let now = owner_wall_clock_millis();
        let daemon =
            Daemon::new_with_dispatch_journal(owner, dispatcher, config, dispatch_journal, now)
                .map_err(|error| WorkspaceError::Io(error.to_string()))?;
        Ok(Self { daemon })
    }

    /// Opens the engine with production fault seams used by crash-boundary
    /// tests. The seams remain inside the owner's filesystem operations.
    ///
    /// # Errors
    ///
    /// Returns an error when ownership, fault-aware journals, or the composed
    /// daemon cannot be opened and recovered.
    pub fn open_with_faults(
        directory: impl AsRef<std::path::Path>,
        model: M,
        genesis: WorkspaceHead,
        dispatcher: Dispatcher<V, A>,
        config: DaemonConfig,
        faults: std::sync::Arc<Faults>,
    ) -> Result<Self, WorkspaceError> {
        let directory = directory.as_ref().to_owned();
        let owner = WorkspaceOwner::open_with_faults(&directory, model, genesis, faults)?;
        let (dispatch_journal, _recovery) = DispatchJournal::open_with_faults(
            directory.join(daemon::DISPATCH_JOURNAL_FILE),
            DispatchJournalLimits::default(),
            owner.faults(),
        )
        .map_err(|error| WorkspaceError::Io(error.to_string()))?;
        let now = owner_wall_clock_millis();
        let daemon =
            Daemon::new_with_dispatch_journal(owner, dispatcher, config, dispatch_journal, now)
                .map_err(|error| WorkspaceError::Io(error.to_string()))?;
        Ok(Self { daemon })
    }

    /// Returns a bounded request handle.
    #[must_use]
    pub fn handle(&self) -> DaemonHandle<M::Intent> {
        self.daemon.handle()
    }

    /// Runs one fair owner-loop operation.
    pub fn serve_one(&mut self) -> bool {
        self.daemon.serve_one()
    }

    fn owner_now() -> u64 {
        owner_wall_clock_millis()
    }

    /// Plans one local-first execution while retaining the scheduler's
    /// interner, resource, cancellation, and fence guards in the returned
    /// plan. Applications should use the paired completion methods below so
    /// those guards cannot be bypassed by a second owner.
    ///
    /// # Errors
    ///
    /// Returns an error when the request cannot be admitted by the bounded
    /// scheduler or its exact identity and resource envelope is invalid.
    pub fn plan<R: Relation>(
        &self,
        request: ScheduleRequest<R>,
    ) -> Result<DispatchPlan<R>, DispatchError> {
        self.daemon.dispatcher().plan(request)
    }

    /// Plans with an owner-admitted semantic capability. Reuse is available
    /// only when the capability's exact dependency binding matches the
    /// retained publication; callers cannot manufacture a reuse context.
    ///
    /// # Errors
    ///
    /// Returns an error when semantic admission or freshness validation
    /// fails, a retained binding is inconsistent, or bounded scheduling
    /// cannot admit the request.
    pub fn plan_with_semantic<R: Relation>(
        &self,
        request: ScheduleRequest<R>,
        semantic: &CompleteSemanticCoverage,
    ) -> Result<DispatchPlan<R>, DispatchError> {
        self.daemon
            .dispatcher()
            .plan_with_semantic(request, semantic)
    }

    /// Converts an untrusted producer coverage claim into the semantic
    /// capability required by local or remote result admission.
    ///
    /// # Errors
    ///
    /// Returns an error when the claim does not match the exact work identity
    /// or the configured authority rejects it.
    pub fn admit_semantic<R: Relation>(
        &self,
        identity: &VersionedWorkIdentity<R>,
        claim: UntrustedSemanticCoverageClaim,
    ) -> Result<CompleteSemanticCoverage, DispatchError> {
        self.daemon.dispatcher().admit_semantic(identity, claim)
    }

    /// Admits a complete dependency manifest and retains it with the opaque
    /// semantic capability until publication. The dispatcher registers the
    /// manifest under the exact execution work key transactionally.
    ///
    /// # Errors
    ///
    /// Returns an error when the claim or dependency manifest fails exact
    /// semantic admission.
    pub fn admit_semantic_with_manifest<R: Relation>(
        &self,
        identity: &VersionedWorkIdentity<R>,
        claim: UntrustedSemanticCoverageClaim,
        manifest: DependencyManifest,
    ) -> Result<CompleteSemanticCoverage, DispatchError> {
        self.daemon
            .dispatcher()
            .admit_semantic_with_manifest(identity, claim, manifest)
    }

    /// Invalidates retained execution outputs whose admitted dependency facts
    /// intersect the supplied semantic changes. The report contains exact
    /// work keys removed from reuse and quarantined while active.
    ///
    /// # Errors
    ///
    /// Returns an error when invalidation cannot update the bounded semantic
    /// registry.
    pub fn invalidate_semantic_changes(
        &self,
        changes: &[backend_semantic::DependencyChange],
    ) -> Result<SemanticInvalidationReport, DispatchError> {
        self.daemon
            .dispatcher()
            .invalidate_semantic_changes(changes)
    }

    /// Publishes a locally computed output through exact semantic, recipe,
    /// schema/CAS, cancellation, fence, and scheduler admission.
    ///
    /// # Errors
    ///
    /// Returns an error when the plan, semantic capability, output bytes, or
    /// scheduler ownership fails exact admission.
    pub fn complete_local<R: Relation>(
        &mut self,
        plan: DispatchPlan<R>,
        bytes: &[u8],
        semantic: CompleteSemanticCoverage,
        now: u64,
    ) -> Result<DispatchCompletion, DispatchError> {
        self.daemon
            .complete_local(plan, bytes, semantic, now)
            .map_err(|error| DispatchError::Workspace(error.to_string()))
    }

    /// Allocation-preserving local completion for worker/CAS buffers already
    /// held by an immutable Arc owner.
    ///
    /// # Errors
    ///
    /// Returns an error when the plan, semantic capability, output bytes, or
    /// scheduler ownership fails exact admission.
    pub fn complete_local_shared<R: Relation>(
        &mut self,
        plan: DispatchPlan<R>,
        bytes: std::sync::Arc<Vec<u8>>,
        semantic: CompleteSemanticCoverage,
        now: u64,
    ) -> Result<DispatchCompletion, DispatchError> {
        self.daemon
            .complete_local_shared(plan, bytes, semantic, now)
            .map_err(|error| DispatchError::Workspace(error.to_string()))
    }

    /// Plans through the owner-controlled durable reuse path. The semantic
    /// capability is checked against the current recipe/scope authority, the
    /// selected workspace catalog is queried for an exact binding, and any
    /// recovered generation is restored before a reusable plan is returned.
    /// Catalog misses fall through to ordinary local-first routing.
    ///
    /// # Errors
    ///
    /// Returns an error when semantic admission, catalog validation, route
    /// scheduling, or durable publication fails.
    pub fn plan_with_durable_reuse<R: Relation>(
        &mut self,
        request: ScheduleRequest<R>,
        semantic: CompleteSemanticCoverage,
        now: u64,
    ) -> Result<DispatchPlan<R>, DaemonError> {
        self.daemon.plan_with_durable_reuse(request, semantic, now)
    }

    /// Composes planning, negotiated remote admission, daemon registration,
    /// and one request send into the sole pending-remote path. The returned
    /// key is only a correlation capability; the daemon retains the affine
    /// plan, contract, cancellation, and scheduler reservations.
    ///
    /// # Errors
    ///
    /// Returns an error when planning, capability negotiation, pending-ticket
    /// admission, or request transmission fails.
    pub fn dispatch_remote<R: Relation + Send>(
        &mut self,
        request: ScheduleRequest<R>,
        contract: RemoteDispatchContract,
        capabilities: &CapabilityManifest,
    ) -> Result<PendingRemoteKey, DaemonError> {
        let plan = self.daemon.plan_with_durable_reuse(
            request,
            contract.semantic.clone(),
            Self::owner_now(),
        )?;
        self.daemon
            .dispatch_remote_pending(plan, contract, capabilities)
    }

    /// Completes the next authenticated result for a daemon-owned pending
    /// remote key. Control frames are retained and reported by the daemon.
    ///
    /// # Errors
    ///
    /// Returns an error when no pending result is available or its exact
    /// request, authority, and output evidence fails admission.
    pub fn receive_remote(&mut self, now: u64) -> Result<DispatchCompletion, DaemonError> {
        self.daemon.receive_remote(now)
    }

    /// Retries a daemon-owned pending request after transport replacement.
    /// A failed retry returns [`DaemonError::RemoteSendPending`] with the same
    /// key, preserving the affine fallback/cancellation capability.
    ///
    /// # Errors
    ///
    /// Returns an error when the pending key is unknown or the replacement
    /// transport rejects the exact retained request.
    pub fn resend_remote_pending(
        &mut self,
        key: PendingRemoteKey,
        capabilities: &CapabilityManifest,
    ) -> Result<(), DaemonError> {
        self.daemon.resend_remote_pending(key, capabilities)
    }

    /// Transfers a planned remote attempt into owner-held fallback custody
    /// without transmitting a recipe request. The returned key is consumed
    /// by the same cancellation, fallback, and publication APIs as a sent
    /// attempt.
    ///
    /// # Errors
    ///
    /// Returns an error when ticket, journal, or bounded pending-relation
    /// admission fails.
    pub fn prepare_remote_pending<R: Relation + Send>(
        &mut self,
        plan: DispatchPlan<R>,
        contract: RemoteDispatchContract,
    ) -> Result<PendingRemoteKey, DaemonError> {
        self.daemon.prepare_remote_pending(plan, contract)
    }

    /// Cancels one exact pending remote attempt and propagates its full
    /// work/attempt/fence/cancellation identity to the peer.
    ///
    /// # Errors
    ///
    /// Returns an error when the pending key is unknown or cancellation could
    /// not be transmitted to the bound peer.
    pub fn cancel_remote_pending(&mut self, key: PendingRemoteKey) -> Result<(), DaemonError> {
        self.daemon.cancel_remote_pending(key)
    }

    /// Switches one pending remote attempt to its owner-held local fallback,
    /// preserving the same semantic and output admission path.
    ///
    /// # Errors
    ///
    /// Returns an error when the pending key is unknown or fallback output
    /// fails exact local admission.
    pub fn fallback_remote_pending(
        &mut self,
        key: PendingRemoteKey,
        bytes: std::sync::Arc<Vec<u8>>,
        now: u64,
    ) -> Result<DispatchCompletion, DaemonError> {
        self.daemon.fallback_remote_pending(key, bytes, now)
    }

    /// Activates the same owner-held fallback immediately after a terminal
    /// remote failure while preserving every publication and authority fence.
    ///
    /// # Errors
    ///
    /// Returns an error when the pending key is unknown or fallback output
    /// fails exact local admission.
    pub fn fail_remote_pending(
        &mut self,
        key: PendingRemoteKey,
        bytes: std::sync::Arc<Vec<u8>>,
        now: u64,
    ) -> Result<DispatchCompletion, DaemonError> {
        self.daemon.fail_remote_pending(key, bytes, now)
    }

    /// Installs the daemon-owned remote transport path.
    pub fn set_remote_transport(&mut self, transport: Box<dyn RemoteTransport>) {
        self.daemon.set_remote_transport(transport);
    }

    /// Sends one bounded control prelude through the daemon-owned negotiated
    /// transport. Recipe results remain correlated through pending tickets.
    ///
    /// # Errors
    ///
    /// Returns an error when the control frame fails capability or transport
    /// admission.
    pub fn send_remote_control(
        &mut self,
        message: TransportMessage,
        capabilities: &CapabilityManifest,
    ) -> Result<NegotiatedCapabilities, DaemonError> {
        self.daemon.send_remote_control(message, capabilities)
    }

    /// Receives one bounded control response from the daemon-owned transport.
    ///
    /// # Errors
    ///
    /// Returns an error when the transport is unavailable or the response
    /// fails bounded frame admission.
    pub fn receive_remote_control(&mut self) -> Result<TransportMessage, DaemonError> {
        self.daemon.receive_remote_control()
    }

    /// Borrows the complete composed daemon.
    #[must_use]
    pub const fn daemon(&self) -> &Daemon<M, V, A> {
        &self.daemon
    }

    /// Mutably borrows the daemon for owner-loop setup and shutdown.
    #[must_use]
    pub fn daemon_mut(&mut self) -> &mut Daemon<M, V, A> {
        &mut self.daemon
    }
}
