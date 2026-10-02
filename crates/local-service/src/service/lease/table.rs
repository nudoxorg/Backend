//! The set of leases the owner retains, and when it stops retaining them.

use super::identity::OwnerLeaseIdentity;
use super::limits::SubscriptionLeaseLimits;
use super::state::{Failure, Lease, LeaseRefusal, ReleaseReason};
use crate::protocol::ProtocolError;
use backend_client::lease_contract::LeaseMs;
use backend_client::monotonic::{Deadline, MonotonicClock, SystemClock};
use backend_engine::LocalSubscriptionId;
use std::collections::BTreeMap;
use std::fmt;
use std::sync::Arc;
use std::time::Instant;

/// How many leases were released, by reason. Each release names exactly one
/// reason, so the total is the number of leases ever released.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) struct ReleaseCounts([u64; ReleaseReason::ALL.len()]);

impl ReleaseCounts {
    pub(crate) fn record(&mut self, reason: ReleaseReason) {
        if let Some(count) = self.0.get_mut(reason.index()) {
            *count = count.saturating_add(1);
        }
    }

    /// Leases released for `reason`.
    #[cfg(test)]
    pub(crate) fn count(&self, reason: ReleaseReason) -> u64 {
        self.0.get(reason.index()).copied().unwrap_or(0)
    }

    /// Leases released for any reason.
    #[cfg(test)]
    pub(crate) fn total(&self) -> u64 {
        self.0.iter().copied().fold(0, u64::saturating_add)
    }
}

/// The retained leases plus the earliest instant any of them could be due.
///
/// `earliest` is a lower bound, never an exact minimum: it only moves down
/// when a lease is stored and is recomputed exactly when a sweep runs. A
/// lease that was renewed or released therefore leaves the bound stale-early,
/// which costs one extra scan at that instant and can never reclaim anything
/// early. That makes the idle owner poll a single comparison.
pub(crate) struct LeaseTable {
    clock: Arc<dyn MonotonicClock>,
    limits: SubscriptionLeaseLimits,
    entries: BTreeMap<LocalSubscriptionId, Lease>,
    earliest: Option<Deadline>,
    identity: OwnerLeaseIdentity,
    released: ReleaseCounts,
    closed: bool,
}

impl fmt::Debug for LeaseTable {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("LeaseTable")
            .field("active", &self.entries.len())
            .field("earliest", &self.earliest)
            .field("limits", &self.limits)
            .field("released", &self.released)
            .field("closed", &self.closed)
            .field(
                "boot_nonce_initialized",
                &self.identity.boot_nonce.is_some(),
            )
            .field("next_lease_nonce", &self.identity.next_nonce)
            .finish()
    }
}

impl Default for LeaseTable {
    fn default() -> Self {
        Self::new(SubscriptionLeaseLimits::default(), Arc::new(SystemClock))
    }
}

impl LeaseTable {
    pub(crate) fn new(limits: SubscriptionLeaseLimits, clock: Arc<dyn MonotonicClock>) -> Self {
        Self {
            clock,
            limits,
            entries: BTreeMap::new(),
            earliest: None,
            identity: OwnerLeaseIdentity::default(),
            released: ReleaseCounts::default(),
            closed: false,
        }
    }

    pub(crate) fn set_limits(&mut self, limits: SubscriptionLeaseLimits) {
        self.limits = limits;
    }

    #[cfg(test)]
    pub(crate) fn set_clock(&mut self, clock: Arc<dyn MonotonicClock>) {
        self.clock = clock;
    }

    /// Reads the table's clock. Operations read it once to decide and once
    /// more to commit, so work that outlives a lease cannot extend it.
    pub(crate) fn now(&self) -> Instant {
        self.clock.now()
    }

    pub(crate) const fn limits(&self) -> SubscriptionLeaseLimits {
        self.limits
    }

    #[cfg(test)]
    pub(crate) fn identity(&mut self) -> &mut OwnerLeaseIdentity {
        &mut self.identity
    }

    /// Grants exactly the requested term or refuses it.
    pub(crate) fn grant_term(&self, requested_ms: u64) -> Result<LeaseMs, LeaseRefusal> {
        self.limits
            .grant_term(requested_ms)
            .ok_or(LeaseRefusal::LeaseBounds)
    }

    /// Allocates a lease identity no retained lease already holds.
    pub(crate) fn allocate_id(
        &mut self,
        request_id: u64,
        cursor: &[u8],
    ) -> Result<LocalSubscriptionId, ProtocolError> {
        let entries = &self.entries;
        self.identity
            .allocate(request_id, cursor, |lease| entries.contains_key(lease))
    }

    /// Refuses early, before any daemon round trip is spent, when no lease
    /// could be stored. [`Self::install`] enforces the same rule itself.
    pub(crate) fn reserve(&mut self, at: Instant) -> Result<(), LeaseRefusal> {
        if self.closed {
            return Err(LeaseRefusal::OwnerClosed);
        }
        self.reclaim_due(at);
        if self.entries.len() >= self.limits.max_active() {
            return Err(LeaseRefusal::CapacityFull);
        }
        Ok(())
    }

    /// Stores a newly granted lease. The table can never hold more than its
    /// limit, nor accept a lease after it closed, whatever its callers do.
    pub(crate) fn install(
        &mut self,
        id: LocalSubscriptionId,
        lease: Lease,
    ) -> Result<(), LeaseRefusal> {
        if self.closed {
            return Err(LeaseRefusal::OwnerClosed);
        }
        if self.entries.len() >= self.limits.max_active() && !self.entries.contains_key(&id) {
            return Err(LeaseRefusal::CapacityFull);
        }
        self.note(lease.retained_until());
        self.entries.insert(id, lease);
        Ok(())
    }

    /// Stores the successor of a retained lease. A successor for a lease that
    /// is gone is dropped: an operation that outlived its lease must not
    /// resurrect it.
    pub(crate) fn replace(&mut self, id: LocalSubscriptionId, lease: Lease) -> bool {
        if self.closed || !self.entries.contains_key(&id) {
            return false;
        }
        self.note(lease.retained_until());
        self.entries.insert(id, lease);
        true
    }

    /// The retained lease `id`, if it is still within its term and window at
    /// `at`. A lease found due is released here, so the caller learns *why*
    /// and the retained root is gone before the error is sent.
    pub(crate) fn active(
        &mut self,
        id: LocalSubscriptionId,
        at: Instant,
    ) -> Result<&Lease, Failure> {
        let due = match self.entries.get(&id) {
            None => return Err(LeaseRefusal::UnknownLease.into()),
            Some(lease) => lease.release_due(at),
        };
        if let Some(reason) = due {
            self.release(id, reason);
            return Err(reason.holder_error().into());
        }
        self.entries
            .get(&id)
            .ok_or_else(|| LeaseRefusal::UnknownLease.into())
    }

    /// Releases `id`, dropping everything retained for it. Returns whether a
    /// lease was released; a second release of the same lease is a no-op, so
    /// a root is released exactly once.
    pub(crate) fn release(&mut self, id: LocalSubscriptionId, reason: ReleaseReason) -> bool {
        let released = self.entries.remove(&id).is_some();
        if released {
            self.released.record(reason);
        }
        released
    }

    /// Releases every lease whose term or reset window is due at `at`, and
    /// returns how many. When nothing can be due this is one comparison.
    pub(crate) fn reclaim_due(&mut self, at: Instant) -> usize {
        if self.earliest.is_none_or(|earliest| !earliest.is_due(at)) {
            return 0;
        }
        let due: Vec<(LocalSubscriptionId, ReleaseReason)> = self
            .entries
            .iter()
            .filter_map(|(id, lease)| lease.release_due(at).map(|reason| (*id, reason)))
            .collect();
        for (id, reason) in &due {
            self.release(*id, *reason);
        }
        self.earliest = self.entries.values().map(Lease::retained_until).min();
        due.len()
    }

    /// Releases everything and refuses all later leases. Called before the
    /// owner joins its deferred workers, so a slow join cannot keep reset
    /// roots pinned.
    pub(crate) fn close(&mut self) {
        self.closed = true;
        for id in self.entries.keys().copied().collect::<Vec<_>>() {
            self.release(id, ReleaseReason::OwnerClosed);
        }
        self.earliest = None;
    }

    fn note(&mut self, deadline: Deadline) {
        self.earliest = Some(
            self.earliest
                .map_or(deadline, |current| current.min(deadline)),
        );
    }

    #[cfg(test)]
    pub(crate) const fn released(&self) -> ReleaseCounts {
        self.released
    }

    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.entries.len()
    }

    #[cfg(test)]
    pub(crate) fn get(&self, id: LocalSubscriptionId) -> Option<&Lease> {
        self.entries.get(&id)
    }

    #[cfg(test)]
    pub(crate) fn earliest(&self) -> Option<Deadline> {
        self.earliest
    }

    #[cfg(test)]
    pub(crate) fn ids(&self) -> Vec<LocalSubscriptionId> {
        self.entries.keys().copied().collect()
    }

    /// The true earliest retention deadline, for checking the lower bound.
    #[cfg(test)]
    pub(crate) fn exact_earliest(&self) -> Option<Deadline> {
        self.entries.values().map(Lease::retained_until).min()
    }
}
