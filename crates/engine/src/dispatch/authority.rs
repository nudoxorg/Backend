//! Authority verification, replay tracking, and publication transactions.

use super::DispatchError;
use super::coverage::CompleteSemanticCoverage;
use super::retention::OutputAdmissionValidator;
use backend_execution::{
    AttemptLease, AuthorityVersion, OutputVersion, ResultCoverage, ReuseContext, WorkKey,
};
use backend_replication::{
    Attestation, AttestationClass, AttestationMaterial, AttestationMaterialView,
    AttestationVerifier, ReplicationError,
};
use backend_version::Relation;
use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex};

/// Hard bound for accepted-authority metadata. A full map fails closed until
/// the owner explicitly invalidates entries; metadata therefore cannot grow
/// independently of the bounded reusable-output policy.
pub(super) const MAX_ACCEPTED_AUTHORITY_ENTRIES: usize = 4096;
/// Hard bound for pending authority candidates. Pending entries are released
/// by their affine reservations, but the bound still prevents an admission
/// flood from growing the per-work index without limit.
pub(super) const MAX_PENDING_AUTHORITY_ENTRIES: usize = 4096;
/// Hard bound for pending replay statements.
pub(super) const MAX_PENDING_REPLAY_ENTRIES: usize = 4096;
/// Hard bound for committed replay statements. Entries are owned by the
/// corresponding retained publication and retired with that publication. The
/// statement itself includes attempt/fence material, so retirement cannot
/// reopen a replay for a different attempt.
pub(super) const MAX_COMMITTED_REPLAY_ENTRIES: usize = 8192;
/// Hard bound for deterministic disagreement evidence. A work key remains
/// quarantined until an independently locally verifiable recomputation or an
/// explicit owner resolution clears the row.
pub(super) const MAX_QUARANTINED_AUTHORITY_ENTRIES: usize = 4096;
/// Hard bound for engine-admitted authority freshness transitions.  A full
/// transition table fails closed so retiring an old identity can never let a
/// stale worker roll a revoked authority back into the reusable cache.
pub(super) const MAX_AUTHORITY_TRANSITIONS: usize = 4096;

/// Remote trust policy. A memo cannot mint a scheduler publication capability.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RemoteAuthorityPolicy {
    /// Require a configured signed executor statement.
    Signed,
    /// Require independently checked quorum/audit evidence.
    Quorum,
    /// Accept trusted signed or audited evidence.
    SignedOrQuorum,
    /// Do not publish remote results.
    LocalOnly,
}

impl RemoteAuthorityPolicy {
    pub(super) const fn allows(self, class: AttestationClass) -> bool {
        match self {
            Self::Signed => matches!(class, AttestationClass::TrustedSigned),
            Self::Quorum => matches!(class, AttestationClass::QuorumAudited),
            Self::SignedOrQuorum => matches!(
                class,
                AttestationClass::TrustedSigned | AttestationClass::QuorumAudited
            ),
            Self::LocalOnly => false,
        }
    }
}

/// Deterministic keyed BLAKE3 verifier for trusted executor statements.
#[derive(Clone, Debug, Default)]
pub struct Blake3AuthorityVerifier {
    keys: BTreeMap<[u8; 32], [u8; 32]>,
}

/// Fail-closed authority verifier used by an unconfigured owner type.
#[derive(Clone, Copy, Debug, Default)]
pub struct UnconfiguredAuthorityVerifier;

impl AttestationVerifier for UnconfiguredAuthorityVerifier {
    fn verify(
        &self,
        _attestation: Option<Attestation>,
        _material: &AttestationMaterial,
    ) -> Result<AttestationClass, ReplicationError> {
        Err(ReplicationError::InvalidAttestation)
    }
}

impl Blake3AuthorityVerifier {
    /// Creates an empty verifier.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Registers one authority identity and verifier-owned key.
    pub fn insert_key(&mut self, authority: [u8; 32], secret: [u8; 32]) {
        self.keys.insert(authority, secret);
    }

    /// Produces the exact statement format expected by this verifier.
    #[must_use]
    pub fn sign(&self, authority: [u8; 32], material: &AttestationMaterial) -> Option<Attestation> {
        self.keys
            .get(&authority)
            .map(|secret| sign_with_key(secret, material))
    }
}

impl AttestationVerifier for Blake3AuthorityVerifier {
    fn verify(
        &self,
        attestation: Option<Attestation>,
        material: &AttestationMaterial,
    ) -> Result<AttestationClass, ReplicationError> {
        let Some(attestation) = attestation else {
            return Err(ReplicationError::AttestationRequired);
        };
        let authority = material.authority.id.as_bytes();
        let Some(secret) = self.keys.get(&authority) else {
            return Err(ReplicationError::InvalidAttestation);
        };
        if sign_with_key(secret, material) == attestation {
            Ok(AttestationClass::TrustedSigned)
        } else {
            Err(ReplicationError::InvalidAttestation)
        }
    }

    fn verify_view(
        &self,
        attestation: Option<Attestation>,
        material: &AttestationMaterialView<'_>,
    ) -> Result<AttestationClass, ReplicationError> {
        let Some(attestation) = attestation else {
            return Err(ReplicationError::AttestationRequired);
        };
        let authority = material.authority.id.as_bytes();
        let Some(secret) = self.keys.get(&authority) else {
            return Err(ReplicationError::InvalidAttestation);
        };
        if sign_view_with_key(secret, material) == attestation {
            Ok(AttestationClass::TrustedSigned)
        } else {
            Err(ReplicationError::InvalidAttestation)
        }
    }
}

fn sign_with_key(secret: &[u8; 32], material: &AttestationMaterial) -> Attestation {
    sign_view_with_key(secret, &material.as_view())
}

fn sign_view_with_key(secret: &[u8; 32], material: &AttestationMaterialView<'_>) -> Attestation {
    material.keyed_authority_statement(secret)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct AcceptedAuthority {
    /// Authority identity that established this publication.  The reverse
    /// index uses this exact typed identity to fence all retained outputs on
    /// an engine-admitted authority transition.
    pub(super) authority: AuthorityVersion,
    pub(super) class: AttestationClass,
    pub(super) output: OutputVersion,
    /// Replay statement that established this publication. Retaining it with
    /// the output lets a later disagreement preserve both identities as
    /// quarantine evidence instead of silently electing a rank winner.
    pub(super) statement_id: [u8; 32],
    /// Semantic registration generation retained with this publication.
    pub(super) semantic_generation: Option<u64>,
    /// Authority epoch bound to the accepted publication. Reuse must not
    /// survive an engine-observed authority rotation.
    pub(super) authority_epoch: u64,
    /// Revocation observation bound to the accepted publication. A newer
    /// admitted claim invalidates this publication before lookup reuse.
    pub(super) revocation_version: u64,
    /// Only manifest-backed semantic capabilities may enter the reusable
    /// lookup. Claim-only coverage is an accepted one-shot result.
    pub(super) cacheable: bool,
    /// Exact dependency-manifest version bound to this publication. The work
    /// key alone does not identify a semantic closure, so a later capability
    /// with a changed manifest must retire this entry before planning.
    pub(super) semantic_manifest: Option<[u8; 32]>,
    /// Exact admitted coverage identity and scope. These fields prevent a
    /// capability with a changed witness from reusing a byte-identical output
    /// under the same execution key.
    pub(super) semantic_identity: [u8; 32],
    pub(super) semantic_scope: u64,
    pub(super) semantic_read_manifest: [u8; 32],
    /// The engine-issued context from the winning publication.  Reuse is
    /// authorized only by this retained capability after the dispatcher has
    /// independently confirmed its freshness record.
    pub(super) reuse_context: Option<ReuseContext>,
}

#[derive(Debug, Default)]
pub(super) struct ReplayState {
    pub(super) committed: Mutex<BTreeMap<[u8; 32], WorkKey>>,
    /// Direct owner projection for lifecycle retirement.  The statement map
    /// remains keyed by replay identity for duplicate detection, while this
    /// reverse index makes eviction/invalidation of one `WorkKey` O(log N)
    /// instead of retaining over every committed statement.
    pub(super) committed_by_work: Mutex<BTreeMap<WorkKey, [u8; 32]>>,
    pub(super) pending: Mutex<BTreeSet<[u8; 32]>>,
}

impl ReplayState {
    pub(super) fn reserve(
        self: &Arc<Self>,
        key: WorkKey,
        id: [u8; 32],
        cacheable: bool,
    ) -> Result<ReplayReservation, DispatchError> {
        let mut pending = self
            .pending
            .lock()
            .map_err(|_| DispatchError::AuthorityRejected)?;
        let committed = self
            .committed
            .lock()
            .map_err(|_| DispatchError::AuthorityRejected)?;
        if pending.contains(&id) || committed.contains_key(&id) {
            return Err(DispatchError::AuthorityRejected);
        }
        if pending.len() >= MAX_PENDING_REPLAY_ENTRIES {
            return Err(DispatchError::AuthorityRejected);
        }
        if committed.len() >= MAX_COMMITTED_REPLAY_ENTRIES && !cacheable {
            return Err(DispatchError::AuthorityRejected);
        }
        pending.insert(id);
        Ok(ReplayReservation {
            state: Arc::clone(self),
            key,
            id,
            committed: false,
        })
    }

    fn commit(&self, key: WorkKey, id: [u8; 32]) {
        let mut pending = self
            .pending
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        pending.remove(&id);
        drop(pending);
        let mut committed = self
            .committed
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        // One active publication owns replay evidence for a work key. A
        // replacement publication retires the prior statement at the same
        // linearization point, preventing stale metadata from wedging churn.
        let mut committed_by_work = self
            .committed_by_work
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(previous) = committed_by_work.insert(key, id) {
            committed.remove(&previous);
        }
        if let Some(previous_owner) = committed.insert(id, key)
            && previous_owner != key
            && committed_by_work.get(&previous_owner).copied() == Some(id)
        {
            committed_by_work.remove(&previous_owner);
        }
    }

    fn release(&self, id: [u8; 32]) {
        self.pending
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(&id);
    }

    /// Retires replay statements whose publication is no longer retained.
    /// Replay evidence for a retired work key is no longer needed: a future
    /// attempt has a new fence/ordinal and therefore a distinct statement id.
    pub(super) fn release_committed_for_key(&self, key: WorkKey) {
        // Keep the lock order identical to `commit`: pending (when present),
        // committed statement map, then the direct owner projection.  The
        // reverse projection is an index under the same state owner; taking
        // it first would deadlock a replacement commit racing eviction.
        let mut committed = self
            .committed
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let statement = self
            .committed_by_work
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(&key);
        let Some(statement) = statement else {
            return;
        };
        committed.remove(&statement);
    }
}

#[derive(Debug)]
pub(super) struct ReplayReservation {
    state: Arc<ReplayState>,
    key: WorkKey,
    id: [u8; 32],
    committed: bool,
}

impl ReplayReservation {
    fn commit(&mut self) {
        self.state.commit(self.key, self.id);
        self.committed = true;
    }
}

impl Drop for ReplayReservation {
    fn drop(&mut self) {
        if !self.committed {
            self.state.release(self.id);
        }
    }
}

#[derive(Debug, Default)]
pub(super) struct PendingAuthorityState {
    state: Mutex<PendingAuthorityIndex>,
}

#[derive(Debug, Default)]
struct PendingAuthorityIndex {
    entries: BTreeMap<WorkKey, BTreeMap<[u8; 32], PendingAuthority>>,
    count: usize,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct PendingAuthority {
    class: AttestationClass,
    output: OutputVersion,
}

impl PendingAuthorityState {
    #[cfg(test)]
    pub(super) fn is_empty(&self) -> bool {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .entries
            .is_empty()
    }

    pub(super) fn reserve(
        self: &Arc<Self>,
        key: WorkKey,
        id: [u8; 32],
        class: AttestationClass,
        output: OutputVersion,
    ) -> Result<AuthorityReservation, DispatchError> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| DispatchError::AuthorityRejected)?;
        if state.count >= MAX_PENDING_AUTHORITY_ENTRIES {
            return Err(DispatchError::AuthorityRejected);
        }
        if let Some(pending) = state.entries.get(&key) {
            if pending.contains_key(&id) {
                return Err(DispatchError::AuthorityRejected);
            }
            for previous in pending.values() {
                check_authority_pair(previous.class, previous.output, class, output)?;
            }
        }
        let next_count = state
            .count
            .checked_add(1)
            .ok_or(DispatchError::AuthorityRejected)?;
        state
            .entries
            .entry(key)
            .or_default()
            .insert(id, PendingAuthority { class, output });
        state.count = next_count;
        Ok(AuthorityReservation {
            state: Arc::clone(self),
            key,
            id,
            committed: false,
        })
    }

    fn commit(&self, key: WorkKey, id: [u8; 32]) {
        self.release(key, id);
    }

    fn release(&self, key: WorkKey, id: [u8; 32]) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let (removed, empty) = state
            .entries
            .get_mut(&key)
            .map_or((false, false), |pending| {
                let removed = pending.remove(&id).is_some();
                (removed, pending.is_empty())
            });
        if empty {
            state.entries.remove(&key);
        }
        if removed {
            // Reservation drops are infallible cleanup. The checked branch
            // preserves the counter if a corrupted state is observed instead
            // of wrapping it below zero.
            if let Some(next_count) = state.count.checked_sub(1) {
                state.count = next_count;
            }
        }
    }

    /// Returns one pending candidate with a different deterministic output.
    /// The caller owns the authority transaction, so this observation and the
    /// subsequent quarantine decision are linearized with publication.
    pub(super) fn conflicting_candidate(
        &self,
        key: WorkKey,
        output: OutputVersion,
    ) -> Option<([u8; 32], OutputVersion)> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .entries
            .get(&key)
            .and_then(|pending| {
                pending
                    .iter()
                    .find(|(_, candidate)| candidate.output != output)
                    .map(|(statement, candidate)| (*statement, candidate.output))
            })
    }

    /// Fences all pending authority reservations for a conflicted work key.
    /// Existing reservation drops remain harmless after this owner-side
    /// cleanup, while no candidate can be committed after quarantine.
    pub(super) fn quarantine(&self, key: WorkKey) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(pending) = state.entries.remove(&key)
            && let Some(next_count) = state.count.checked_sub(pending.len())
        {
            state.count = next_count;
        }
    }
}

/// Evidence retained for a deterministic output disagreement. The two
/// statement/output pairs are intentionally fixed-width and bounded so the
/// quarantine map cannot become an unaccounted byte store.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct AuthorityConflictEvidence {
    pub(crate) first_statement: [u8; 32],
    pub(crate) first_output: [u8; 32],
    pub(crate) second_statement: [u8; 32],
    pub(crate) second_output: [u8; 32],
}

#[derive(Debug)]
pub(super) struct AuthorityReservation {
    state: Arc<PendingAuthorityState>,
    key: WorkKey,
    id: [u8; 32],
    committed: bool,
}

impl AuthorityReservation {
    fn commit(&mut self) {
        self.state.commit(self.key, self.id);
        self.committed = true;
    }
}

impl Drop for AuthorityReservation {
    fn drop(&mut self) {
        if !self.committed {
            self.state.release(self.key, self.id);
        }
    }
}

#[derive(Debug)]
pub(crate) struct AdmissionReservation {
    pub(super) authority: AuthorityReservation,
    pub(super) replay: ReplayReservation,
}

impl AdmissionReservation {
    pub(super) fn commit(&mut self) {
        self.authority.commit();
        self.replay.commit();
    }
}

fn check_authority_value(
    previous: &AcceptedAuthority,
    class: AttestationClass,
    output: OutputVersion,
) -> Result<(), DispatchError> {
    check_authority_pair(previous.class, previous.output, class, output)
}

fn check_authority_pair(
    previous_class: AttestationClass,
    previous_output: OutputVersion,
    class: AttestationClass,
    output: OutputVersion,
) -> Result<(), DispatchError> {
    // A deterministic work key has one canonical output. A stronger trust
    // class cannot silently replace a different result: disagreement is
    // evidence that the route must be quarantined/recomputed, regardless of
    // which candidate carries the higher rank.
    if previous_output != output {
        return Err(DispatchError::AuthorityConflict);
    }
    if authority_rank(previous_class) > authority_rank(class) {
        return Err(DispatchError::AuthorityRejected);
    }
    Ok(())
}

pub(super) fn check_authority(
    accepted: &BTreeMap<WorkKey, AcceptedAuthority>,
    key: WorkKey,
    class: AttestationClass,
    output: OutputVersion,
) -> Result<(), DispatchError> {
    if let Some(previous) = accepted.get(&key) {
        check_authority_value(previous, class, output)?;
    }
    Ok(())
}

fn authority_rank(class: AttestationClass) -> u8 {
    match class {
        AttestationClass::UntrustedMemo => 0,
        AttestationClass::TrustedSigned => 1,
        AttestationClass::QuorumAudited => 2,
        AttestationClass::LocallyVerifiable => 3,
    }
}

#[cfg(test)]
pub(super) fn statement_id(material: &AttestationMaterial) -> [u8; 32] {
    statement_id_view(&material.as_view())
}

/// Derives a replay identity directly from borrowed attestation material.
///
/// Remote admission uses this view so replay bookkeeping cannot allocate a
/// second copy of a large canonical output merely to hash the statement.
pub(super) fn statement_id_view(material: &AttestationMaterialView<'_>) -> [u8; 32] {
    material.canonical_digest(b"backend.engine.authority.replay.v1\0")
}

pub(super) struct LocalAuthorityVerifier {
    pub(super) minimum_epoch: u64,
    pub(super) minimum_revocation: u64,
}

impl<R: Relation> backend_execution::AuthorityVerifier<R> for LocalAuthorityVerifier {
    fn verify_authority(
        &self,
        identity: &backend_execution::VersionedWorkIdentity<R>,
        _lease: &AttemptLease,
        claim: &backend_execution::UntrustedAuthorityClaim,
        _admission: &backend_execution::OutputAdmission,
    ) -> Result<(), backend_execution::AuthorityValidationError> {
        if claim.authority != identity.authority.to_bytes()
            || claim.authority_epoch < self.minimum_epoch
            || claim.revocation_version < self.minimum_revocation
        {
            return Err(backend_execution::AuthorityValidationError::BindingMismatch);
        }
        Ok(())
    }
}

pub(super) struct RemoteAuthorityVerifier {
    pub(super) minimum_epoch: u64,
    pub(super) minimum_revocation: u64,
    pub(super) attestation_required: bool,
}

impl<R: Relation> backend_execution::AuthorityVerifier<R> for RemoteAuthorityVerifier {
    fn verify_authority(
        &self,
        identity: &backend_execution::VersionedWorkIdentity<R>,
        _lease: &AttemptLease,
        claim: &backend_execution::UntrustedAuthorityClaim,
        _admission: &backend_execution::OutputAdmission,
    ) -> Result<(), backend_execution::AuthorityValidationError> {
        if claim.authority != identity.authority.to_bytes()
            || claim.authority_epoch < self.minimum_epoch
            || claim.revocation_version < self.minimum_revocation
            || (self.attestation_required && claim.attestation.is_none())
        {
            return Err(backend_execution::AuthorityValidationError::Unauthorized);
        }
        Ok(())
    }
}

pub(super) struct LowerOutputValidator<'a, V> {
    pub(super) validator: &'a V,
    pub(super) semantic: &'a CompleteSemanticCoverage,
    /// The canonical owner already retained by the dispatch admission path.
    /// `BoundOutputValidator` receives a slice for compatibility with the
    /// lower execution crate, so this optional owner lets us route validation
    /// back through `validate_shared` when the slice is that exact allocation.
    /// Without this bridge a slice-only adapter would clone a remote result
    /// once more while constructing `ResultReceipt`.
    pub(super) canonical_owner: Option<&'a Arc<Vec<u8>>>,
}

impl<R, V> backend_execution::BoundOutputValidator<R> for LowerOutputValidator<'_, V>
where
    R: Relation,
    V: OutputAdmissionValidator,
{
    fn validate_bound_output(
        &self,
        identity: &backend_execution::VersionedWorkIdentity<R>,
        output: OutputVersion,
        canonical_bytes: &[u8],
        coverage: ResultCoverage,
    ) -> Result<(), backend_execution::OutputValidationError> {
        if coverage != ResultCoverage::Complete || !self.semantic.binds(identity) {
            return Err(backend_execution::OutputValidationError::ContractMismatch);
        }
        let validation = self
            .canonical_owner
            .filter(|owner| {
                owner.len() == canonical_bytes.len()
                    && owner.as_slice().as_ptr() == canonical_bytes.as_ptr()
            })
            .map_or_else(
                || {
                    OutputAdmissionValidator::validate(
                        self.validator,
                        output,
                        canonical_bytes,
                        self.semantic,
                    )
                },
                |owner| {
                    OutputAdmissionValidator::validate_shared(
                        self.validator,
                        output,
                        owner,
                        self.semantic,
                    )
                },
            );
        validation
            .map(|_| ())
            .map_err(|_| backend_execution::OutputValidationError::ContractMismatch)
    }
}
