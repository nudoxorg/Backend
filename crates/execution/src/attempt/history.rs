//! Attempt history, monotonic owner epochs, and publication acceptance.

use super::{AttemptFence, AttemptLease, AttemptState, ResultReceipt};
use crate::types::IdentityBinding;
use crate::{AdmittedWork, OutputVersion, VersionedWorkIdentity, WorkKey};
use backend_version::Relation;
use std::collections::{BTreeMap, VecDeque};
use std::fmt;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use super::{MANAGER_NONCE, PROCESS_FENCE_SECRET};

/// Bound owner history independently of the number of distinct work keys
/// observed by a long-running process.  Active leases are always retained;
/// terminal history can be forgotten after the fence has already become
/// invalid.  Fence sequence numbers remain process-unique, so forgetting an
/// ordinal never makes an old result valid for a replacement attempt.
const MAX_ATTEMPT_HISTORY: usize = 8192;

/// Reasons a receipt or lease operation was rejected.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AttemptError {
    /// Another active or terminal attempt owns the key.
    Busy,
    /// The receipt or operation referred to an old fence/ordinal.
    Stale,
    /// No attempt record exists for the key.
    Missing,
    /// The lease deadline elapsed before the operation.
    Expired,
    /// The requested state transition is not valid from the current state.
    InvalidTransition,
    /// A result has already been accepted for this key.
    AlreadyAccepted,
    /// Epoch or ordinal arithmetic would regress or overflow.
    EpochRegression,
    /// A checked deadline, ordinal, or fence sequence overflowed.
    Overflow,
    /// An owner-local clock observation moved backwards.
    TimeRegression,
    /// The receipt's semantic key does not match its identity.
    KeyMismatch,
    /// The echoed relation input root differs from the bound identity.
    InputMismatch,
    /// The echoed read manifest differs from the bound identity.
    ReadManifestMismatch,
    /// The echoed authority differs from the bound identity.
    AuthorityMismatch,
    /// The echoed recipe differs from the bound identity.
    RecipeMismatch,
    /// The echoed output contract differs from the bound identity.
    OutputEquivalenceMismatch,
    /// The worker returned an incomplete result for an acceptance path.
    IncompleteCoverage,
    /// A result failed the output validation hook.
    InvalidResult,
    /// Engine authority/revocation evidence rejected the result.
    AuthorityRejected,
}

impl fmt::Display for AttemptError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "attempt error: {self:?}")
    }
}

impl std::error::Error for AttemptError {}

#[derive(Clone, Debug)]
struct AttemptRecord {
    lease: AttemptLease,
    accepted: Option<OutputVersion>,
}

#[derive(Clone, Copy, Debug)]
struct AttemptHistory {
    binding: IdentityBinding,
    owner_epoch: u64,
    ordinal: u32,
    observed_at: u64,
}

/// One-fence owner of active attempts. All transitions are serialized by the
/// manager mutex; late workers can never win a compare-and-swap race.
pub struct AttemptManager {
    active: Mutex<BTreeMap<WorkKey, AttemptRecord>>,
    history: Mutex<BTreeMap<WorkKey, AttemptHistory>>,
    history_order: Mutex<VecDeque<WorkKey>>,
    /// FIFO of terminal fences. Keeping terminal records bounded prevents a
    /// long-lived scheduler from accumulating one tombstone per completed
    /// work key while still retaining enough recent state to reject stragglers.
    terminal_order: Mutex<VecDeque<(WorkKey, AttemptFence)>>,
    secret: [u8; 32],
    incarnation: [u8; 32],
    next_fence: AtomicU64,
    lease_duration: u64,
}

impl fmt::Debug for AttemptManager {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let active = self
            .active
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        f.debug_struct("AttemptManager")
            .field("attempts", &active.len())
            .field("lease_duration", &self.lease_duration)
            .finish_non_exhaustive()
    }
}

impl AttemptManager {
    /// Default owner-local logical lease duration.
    pub const DEFAULT_LEASE_DURATION: u64 = 1;

    /// Creates an attempt manager with a one-tick heartbeat deadline.
    #[must_use]
    pub fn new() -> Self {
        Self::with_lease_duration(Self::DEFAULT_LEASE_DURATION)
    }

    /// Creates an attempt manager with a checked positive lease duration.
    #[must_use]
    pub fn with_lease_duration(lease_duration: u64) -> Self {
        let nonce = MANAGER_NONCE.fetch_add(1, Ordering::Relaxed);
        let process_secret = PROCESS_FENCE_SECRET.get_or_init(|| {
            let timestamp = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_or(0, |duration| duration.as_nanos());
            let mut h = blake3::Hasher::new();
            h.update(b"backend.execution.process-fence-secret.v1\0");
            h.update(&timestamp.to_be_bytes());
            h.update(&std::process::id().to_be_bytes());
            *h.finalize().as_bytes()
        });
        let mut secret_hasher = blake3::Hasher::new();
        secret_hasher.update(b"backend.execution.manager-fence-secret.v1\0");
        secret_hasher.update(process_secret);
        secret_hasher.update(&nonce.to_be_bytes());
        let secret = *secret_hasher.finalize().as_bytes();
        Self {
            active: Mutex::new(BTreeMap::new()),
            history: Mutex::new(BTreeMap::new()),
            history_order: Mutex::new(VecDeque::new()),
            terminal_order: Mutex::new(VecDeque::new()),
            secret,
            incarnation: *process_secret,
            next_fence: AtomicU64::new(1),
            lease_duration: lease_duration.max(1),
        }
    }

    /// Returns the process incarnation bound into every attempt fence.
    /// Remote receipt stores should persist and require this value so a
    /// restart revokes all claims from the prior owner process.
    #[must_use]
    pub const fn incarnation(&self) -> [u8; 32] {
        self.incarnation
    }

    /// Starts an attempt bound to all fields of a typed work identity.
    /// # Errors
    ///
    /// Returns [`AttemptError::Busy`] when the key is already active,
    /// [`AttemptError::AlreadyAccepted`] after publication, or an identity,
    /// epoch, clock, deadline, or fence arithmetic error.
    pub fn start<R: Relation>(
        &self,
        identity: VersionedWorkIdentity<R>,
        epoch: u64,
        now: u64,
    ) -> Result<AttemptLease, AttemptError> {
        let ticket = AdmittedWork::new(identity);
        let binding = IdentityBinding::from_ticket(&ticket);
        self.start_inner(binding, epoch, now)
    }

    fn start_inner(
        &self,
        binding: IdentityBinding,
        epoch: u64,
        now: u64,
    ) -> Result<AttemptLease, AttemptError> {
        let key = binding.key;
        let mut active = self
            .active
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(previous) = active.get(&key) {
            match previous.lease.state {
                AttemptState::Active => return Err(AttemptError::Busy),
                AttemptState::Accepted => return Err(AttemptError::AlreadyAccepted),
                AttemptState::Frozen
                | AttemptState::Cancelled
                | AttemptState::Expired
                | AttemptState::Quarantined => {}
            }
        }
        let mut history = self
            .history
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let ordinal = if let Some(previous) = history.get(&key) {
            if previous.binding != binding {
                return Err(AttemptError::KeyMismatch);
            }
            if epoch < previous.owner_epoch {
                return Err(AttemptError::EpochRegression);
            }
            if now < previous.observed_at {
                return Err(AttemptError::TimeRegression);
            }
            previous
                .ordinal
                .checked_add(1)
                .ok_or(AttemptError::Overflow)?
        } else {
            0
        };
        let expires_at = now
            .checked_add(self.lease_duration)
            .ok_or(AttemptError::Overflow)?;
        let fence = self.mint_fence(key, epoch, ordinal)?;
        let lease = AttemptLease {
            key,
            owner_epoch: epoch,
            ordinal,
            fence,
            incarnation: self.incarnation,
            expires_at,
            observed_at: now,
            state: AttemptState::Active,
            binding,
        };
        // All rejection paths above are read-only. Commit replacement only
        // after binding, monotonicity, expiry, and fence admission succeed.
        active.remove(&key);
        active.insert(
            key,
            AttemptRecord {
                lease: lease.clone(),
                accepted: None,
            },
        );
        let new_history = !history.contains_key(&key);
        history.insert(
            key,
            AttemptHistory {
                binding,
                owner_epoch: epoch,
                ordinal,
                observed_at: now,
            },
        );
        let mut history_order = self
            .history_order
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if new_history {
            history_order.push_back(key);
        }
        let mut terminal_order = self
            .terminal_order
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        Self::prune_terminal(&mut active, &mut terminal_order);
        Self::prune_history(&active, &mut history, &mut history_order);
        Ok(lease)
    }

    /// Takes over an expired attempt with a monotonic owner epoch and ordinal.
    /// # Errors
    ///
    /// Returns [`AttemptError`] when the current lease is not expired, the
    /// owner epoch or clock regresses, the identity differs, or checked
    /// deadline/fence arithmetic overflows.
    pub fn take_over<R: Relation>(
        &self,
        identity: VersionedWorkIdentity<R>,
        epoch: u64,
        now: u64,
    ) -> Result<AttemptLease, AttemptError> {
        let binding = IdentityBinding::from(identity);
        let key = binding.key;
        let mut active = self
            .active
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let old = active.get(&key).ok_or(AttemptError::Missing)?.clone();
        if old.lease.binding != binding {
            return Err(AttemptError::KeyMismatch);
        }
        match old.lease.state {
            AttemptState::Active | AttemptState::Expired => {}
            AttemptState::Accepted => return Err(AttemptError::AlreadyAccepted),
            AttemptState::Frozen | AttemptState::Cancelled | AttemptState::Quarantined => {
                return Err(AttemptError::InvalidTransition);
            }
        }
        if !old.lease.is_expired_at(now) {
            return Err(AttemptError::Busy);
        }
        if epoch < old.lease.owner_epoch {
            return Err(AttemptError::EpochRegression);
        }
        if now < old.lease.observed_at {
            return Err(AttemptError::TimeRegression);
        }
        if old.lease.ordinal == u32::MAX {
            return Err(AttemptError::Overflow);
        }
        let expires_at = now
            .checked_add(self.lease_duration)
            .ok_or(AttemptError::Overflow)?;
        let ordinal = old.lease.ordinal + 1;
        let fence = self.mint_fence(key, epoch, ordinal)?;
        let lease = AttemptLease {
            key,
            owner_epoch: epoch,
            ordinal,
            fence,
            incarnation: self.incarnation,
            expires_at,
            observed_at: now,
            state: AttemptState::Active,
            binding,
        };
        active.insert(
            key,
            AttemptRecord {
                lease: lease.clone(),
                accepted: None,
            },
        );
        let new_history = {
            let history = self
                .history
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            !history.contains_key(&key)
        };
        self.history
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(
                key,
                AttemptHistory {
                    binding,
                    owner_epoch: epoch,
                    ordinal,
                    observed_at: now,
                },
            );
        let mut active = active;
        let mut history = self
            .history
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut history_order = self
            .history_order
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if new_history {
            history_order.push_back(key);
        }
        let mut terminal_order = self
            .terminal_order
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        Self::prune_terminal(&mut active, &mut terminal_order);
        Self::prune_history(&active, &mut history, &mut history_order);
        Ok(lease)
    }

    /// Renews an active lease before its owner-local deadline.
    /// # Errors
    ///
    /// Returns [`AttemptError`] when the supplied lease is stale, terminal,
    /// expired, time-regressed, or its renewed deadline overflows.
    pub fn heartbeat(&self, lease: &AttemptLease, now: u64) -> Result<AttemptLease, AttemptError> {
        let mut active = self
            .active
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let record = active.get_mut(&lease.key).ok_or(AttemptError::Missing)?;
        Self::check_current(&record.lease, lease)?;
        if now < record.lease.observed_at {
            return Err(AttemptError::TimeRegression);
        }
        if record.lease.is_expired_at(now) {
            record.lease.state = AttemptState::Expired;
            return Err(AttemptError::Expired);
        }
        let expires_at = now
            .checked_add(self.lease_duration)
            .ok_or(AttemptError::Overflow)?;
        record.lease.expires_at = expires_at;
        record.lease.observed_at = now;
        Ok(record.lease.clone())
    }

    /// Marks an active attempt expired when its owner observes the deadline.
    /// # Errors
    ///
    /// Returns [`AttemptError`] when the key is missing, already terminal,
    /// time-regressed, or has not reached its observed deadline.
    pub fn expire(&self, key: WorkKey, now: u64) -> Result<(), AttemptError> {
        let mut active = self
            .active
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let record = active.get_mut(&key).ok_or(AttemptError::Missing)?;
        if record.lease.state != AttemptState::Active {
            return Err(if record.lease.state == AttemptState::Accepted {
                AttemptError::AlreadyAccepted
            } else {
                AttemptError::InvalidTransition
            });
        }
        if now < record.lease.observed_at {
            return Err(AttemptError::TimeRegression);
        }
        if !record.lease.is_expired_at(now) {
            return Err(AttemptError::Busy);
        }
        record.lease.state = AttemptState::Expired;
        self.terminal_order
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push_back((key, record.lease.fence));
        Ok(())
    }

    /// Accepts one complete result at the caller's owner-local clock and
    /// permanently closes its publication slot.
    /// # Errors
    ///
    /// Returns [`AttemptError`] when the receipt is stale, expired, incomplete,
    /// mismatched with the current identity, or another result won first.
    pub fn accept<R: Relation>(
        &self,
        receipt: &ResultReceipt<R>,
        now: u64,
    ) -> Result<OutputVersion, AttemptError> {
        self.accept_inner(receipt, now)
    }

    pub(crate) fn validate_for_scheduler<R: Relation>(
        &self,
        receipt: &ResultReceipt<R>,
        now: u64,
    ) -> Result<(), AttemptError> {
        let active = self
            .active
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let record = active.get(&receipt.key).ok_or(AttemptError::Missing)?;
        if record.lease.state == AttemptState::Accepted {
            return Err(AttemptError::AlreadyAccepted);
        }
        if now < record.lease.observed_at {
            return Err(AttemptError::TimeRegression);
        }
        if record.lease.is_expired_at(now) {
            return Err(AttemptError::Expired);
        }
        Self::validate_receipt(record, receipt)?;
        if !receipt.coverage.is_complete() {
            return Err(AttemptError::IncompleteCoverage);
        }
        Ok(())
    }

    pub(crate) fn replace_for_fallback<R: Relation>(
        &self,
        identity: VersionedWorkIdentity<R>,
        old: &AttemptLease,
        now: u64,
    ) -> Result<AttemptLease, AttemptError> {
        let binding = IdentityBinding::from(identity);
        let key = binding.key;
        let mut active = self
            .active
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let record = active.get_mut(&key).ok_or(AttemptError::Missing)?;
        if record.lease.fence != old.fence || record.lease.ordinal != old.ordinal {
            return Err(AttemptError::Stale);
        }
        if record.lease.binding != binding {
            return Err(AttemptError::KeyMismatch);
        }
        if record.lease.state != AttemptState::Active {
            return Err(match record.lease.state {
                AttemptState::Accepted => AttemptError::AlreadyAccepted,
                _ => AttemptError::InvalidTransition,
            });
        }
        if now < record.lease.observed_at {
            return Err(AttemptError::TimeRegression);
        }
        let ordinal = record
            .lease
            .ordinal
            .checked_add(1)
            .ok_or(AttemptError::Overflow)?;
        let expires_at = now
            .checked_add(self.lease_duration)
            .ok_or(AttemptError::Overflow)?;
        let fence = self.mint_fence(key, record.lease.owner_epoch, ordinal)?;
        let lease = AttemptLease {
            key,
            owner_epoch: record.lease.owner_epoch,
            ordinal,
            fence,
            incarnation: self.incarnation,
            expires_at,
            observed_at: now,
            state: AttemptState::Active,
            binding,
        };
        *record = AttemptRecord {
            lease: lease.clone(),
            accepted: None,
        };
        self.history
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(
                key,
                AttemptHistory {
                    binding,
                    owner_epoch: lease.owner_epoch,
                    ordinal,
                    observed_at: now,
                },
            );
        let mut history = self
            .history
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut history_order = self
            .history_order
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        Self::prune_history(&active, &mut history, &mut history_order);
        Ok(lease)
    }

    fn accept_inner<R: Relation>(
        &self,
        receipt: &ResultReceipt<R>,
        now: u64,
    ) -> Result<OutputVersion, AttemptError> {
        let mut active = self
            .active
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let record = active.get_mut(&receipt.key).ok_or(AttemptError::Missing)?;
        if record.lease.state == AttemptState::Accepted {
            return Err(AttemptError::AlreadyAccepted);
        }
        if now < record.lease.observed_at {
            return Err(AttemptError::TimeRegression);
        }
        if record.lease.is_expired_at(now) {
            record.lease.state = AttemptState::Expired;
            return Err(AttemptError::Expired);
        }
        Self::validate_receipt(record, receipt)?;
        if !receipt.coverage.is_complete() {
            return Err(AttemptError::IncompleteCoverage);
        }
        record.accepted = Some(receipt.result);
        record.lease.state = AttemptState::Accepted;
        self.terminal_order
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push_back((receipt.key, record.lease.fence));
        Ok(receipt.result)
    }

    fn validate_receipt<R: Relation>(
        record: &AttemptRecord,
        receipt: &ResultReceipt<R>,
    ) -> Result<(), AttemptError> {
        if record.lease.fence != receipt.fence || record.lease.ordinal != receipt.ordinal {
            return Err(AttemptError::Stale);
        }
        if receipt.authority_evidence.incarnation != record.lease.incarnation {
            return Err(AttemptError::Stale);
        }
        if record.lease.state != AttemptState::Active {
            return Err(match record.lease.state {
                AttemptState::Accepted => AttemptError::AlreadyAccepted,
                AttemptState::Expired => AttemptError::Expired,
                _ => AttemptError::InvalidTransition,
            });
        }
        if receipt.identity.work_key() != receipt.key {
            return Err(AttemptError::KeyMismatch);
        }
        if receipt.input != receipt.identity.input {
            return Err(AttemptError::InputMismatch);
        }
        if receipt.recipe != receipt.identity.recipe {
            return Err(AttemptError::RecipeMismatch);
        }
        if receipt.read_manifest != receipt.identity.read_manifest {
            return Err(AttemptError::ReadManifestMismatch);
        }
        if receipt.authority != receipt.identity.authority {
            return Err(AttemptError::AuthorityMismatch);
        }
        if receipt.output_equivalence != receipt.identity.output_equivalence {
            return Err(AttemptError::OutputEquivalenceMismatch);
        }
        let binding = record.lease.binding;
        if binding.relation_domain != R::DOMAIN
            || binding.relation_type != R::TYPE
            || binding.relation_version != R::VERSION
        {
            return Err(AttemptError::InputMismatch);
        }
        if binding.input != receipt.input.to_bytes() {
            return Err(AttemptError::InputMismatch);
        }
        if binding.recipe != receipt.recipe.to_bytes() {
            return Err(AttemptError::RecipeMismatch);
        }
        if binding.read_manifest != receipt.read_manifest.to_bytes() {
            return Err(AttemptError::ReadManifestMismatch);
        }
        if binding.authority != receipt.authority.to_bytes() {
            return Err(AttemptError::AuthorityMismatch);
        }
        if binding.output_equivalence != receipt.output_equivalence.to_bytes() {
            return Err(AttemptError::OutputEquivalenceMismatch);
        }
        if receipt.authority_evidence.key != record.lease.key
            || receipt.authority_evidence.authority != receipt.authority
            || receipt.authority_evidence.output != receipt.result
            || receipt.authority_evidence.coverage != receipt.coverage
            || receipt.authority_evidence.ordinal != receipt.ordinal
            || receipt.authority_evidence.fence != receipt.fence
        {
            return Err(AttemptError::AuthorityRejected);
        }
        Ok(())
    }

    /// Transitions an active attempt to a publication-fencing terminal state.
    /// # Errors
    ///
    /// Returns [`AttemptError::InvalidTransition`] unless `state` is one of
    /// the explicit cancellation/quarantine terminal states, or when the key
    /// or fence is stale.
    pub fn transition(
        &self,
        key: WorkKey,
        fence: AttemptFence,
        state: AttemptState,
    ) -> Result<(), AttemptError> {
        if !matches!(
            state,
            AttemptState::Frozen | AttemptState::Cancelled | AttemptState::Quarantined
        ) {
            return Err(AttemptError::InvalidTransition);
        }
        let mut active = self
            .active
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let record = active.get_mut(&key).ok_or(AttemptError::Missing)?;
        if record.lease.fence != fence {
            return Err(AttemptError::Stale);
        }
        if record.lease.state != AttemptState::Active {
            return Err(AttemptError::InvalidTransition);
        }
        record.lease.state = state;
        self.terminal_order
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push_back((key, record.lease.fence));
        Ok(())
    }

    /// Returns the current lease snapshot, including terminal records retained
    /// to reject late output.
    #[must_use]
    pub fn current(&self, key: WorkKey) -> Option<AttemptLease> {
        self.active
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(&key)
            .map(|record| record.lease.clone())
    }

    /// Removes a terminal attempt after its receipt and resources are no
    /// longer needed. Active attempts cannot be retired.
    /// # Errors
    ///
    /// Returns [`AttemptError::Missing`] when no record exists or
    /// [`AttemptError::Busy`] while the attempt is still active.
    pub fn retire(&self, key: WorkKey) -> Result<(), AttemptError> {
        let mut active = self
            .active
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let Some(record) = active.get(&key) else {
            return Err(AttemptError::Missing);
        };
        if !record.lease.state.is_terminal() {
            return Err(AttemptError::Busy);
        }
        active.remove(&key);
        Ok(())
    }

    /// Retires a terminal attempt and forgets its owner-history row after the
    /// corresponding reusable publication has been evicted.  This keeps the
    /// stale-fence history tied to the same retention lifecycle as the
    /// accepted output; a fresh fence still comes from the manager's
    /// process-unique sequence when the key is scheduled again.
    ///
    /// # Errors
    ///
    /// Returns [`AttemptError::Missing`] when no attempt is retained or
    /// [`AttemptError::Busy`] when the attempt has not reached a terminal
    /// state.
    pub fn retire_and_forget(&self, key: WorkKey) -> Result<(), AttemptError> {
        self.retire(key)?;
        self.history
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(&key);
        self.history_order
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .retain(|queued| *queued != key);
        Ok(())
    }

    fn prune_history(
        active: &BTreeMap<WorkKey, AttemptRecord>,
        history: &mut BTreeMap<WorkKey, AttemptHistory>,
        order: &mut VecDeque<WorkKey>,
    ) {
        let mut examined = 0_usize;
        while history.len() > MAX_ATTEMPT_HISTORY && examined < order.len() {
            let Some(key) = order.pop_front() else { break };
            examined += 1;
            if active
                .get(&key)
                .is_some_and(|record| record.lease.state == AttemptState::Active)
            {
                order.push_back(key);
            } else {
                history.remove(&key);
            }
        }
    }

    fn prune_terminal(
        active: &mut BTreeMap<WorkKey, AttemptRecord>,
        order: &mut VecDeque<(WorkKey, AttemptFence)>,
    ) {
        while order.len() > MAX_ATTEMPT_HISTORY {
            let Some((key, fence)) = order.pop_front() else {
                break;
            };
            if active.get(&key).is_some_and(|record| {
                record.lease.fence == fence && record.lease.state.is_terminal()
            }) {
                active.remove(&key);
            }
        }
    }

    fn check_current(current: &AttemptLease, supplied: &AttemptLease) -> Result<(), AttemptError> {
        if current.fence != supplied.fence || current.ordinal != supplied.ordinal {
            return Err(AttemptError::Stale);
        }
        if current.state != AttemptState::Active {
            return Err(if current.state == AttemptState::Accepted {
                AttemptError::AlreadyAccepted
            } else {
                AttemptError::InvalidTransition
            });
        }
        Ok(())
    }

    fn mint_fence(
        &self,
        key: WorkKey,
        epoch: u64,
        ordinal: u32,
    ) -> Result<AttemptFence, AttemptError> {
        let sequence = self
            .next_fence
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |value| {
                value.checked_add(1)
            })
            .map_err(|_| AttemptError::Overflow)?;
        let mut h = blake3::Hasher::new();
        h.update(b"backend.execution.attempt-fence.v1\0");
        h.update(&self.secret);
        h.update(&sequence.to_be_bytes());
        h.update(key.as_bytes());
        h.update(&epoch.to_be_bytes());
        h.update(&ordinal.to_be_bytes());
        Ok(AttemptFence::from_bytes(*h.finalize().as_bytes()))
    }
}

impl Default for AttemptManager {
    fn default() -> Self {
        Self::new()
    }
}
