//! Receipt admission for one fenced attempt owner.

use super::{
    AttemptError, AttemptHistory, AttemptLease, AttemptManager, AttemptRecord, AttemptState,
    ResultReceipt,
};
use crate::types::IdentityBinding;
use crate::{OutputVersion, VersionedWorkIdentity};
use backend_version::Relation;

impl AttemptManager {
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
}
