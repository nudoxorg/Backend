//! Ephemeral demand leases with cancellation-safe release.
//!
//! `DemandGraph` is intentionally a plain deterministic graph for planners
//! and tests.  Live clients need a small ownership wrapper so a dropped or
//! cancelled subscription cannot leave a demand row retained indefinitely.

use super::{Demand, DemandGraph, WorkKey};
use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

/// Thread-safe owner of ephemeral demand rows.
#[derive(Clone, Debug, Default)]
pub struct DemandStore {
    inner: Arc<Mutex<DemandGraph>>,
    expirations: Arc<Mutex<BTreeMap<(u64, u64), Instant>>>,
}

impl DemandStore {
    /// Creates an empty demand store.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Retains one demand row and returns a cancellation lease.
    ///
    /// The caller's consumer ID is the lease identity.  Reusing a consumer
    /// ID replaces its previous demand according to [`DemandGraph`] semantics;
    /// callers should keep IDs unique for concurrently live subscriptions.
    #[must_use = "retain the demand lease while the client remains interested"]
    pub fn lease(&self, demand: Demand) -> DemandLease {
        let consumer = demand.consumer;
        let token = self.with_mut(|graph| graph.add_demand_with_token(demand));
        DemandLease {
            store: self.clone(),
            consumer,
            token,
            active: true,
            expires_at: None,
        }
    }

    /// Retains one demand row with a bounded lease lifetime.
    #[must_use = "retain the demand lease while the client remains interested"]
    pub fn lease_for(&self, demand: Demand, lifetime: Duration) -> DemandLease {
        let mut lease = self.lease(demand);
        let expires_at = Instant::now().checked_add(lifetime);
        lease.expires_at = expires_at;
        if let Some(expires_at) = expires_at {
            self.expirations
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .insert((lease.consumer, lease.token), expires_at);
        }
        lease
    }

    /// Reaps expired demand leases and returns the number released.
    pub fn reap_expired(&self, now: Instant) -> usize {
        let expired = {
            let mut expirations = self
                .expirations
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let keys = expirations
                .iter()
                .filter_map(|(key, expires_at)| (*expires_at <= now).then_some(*key))
                .collect::<Vec<_>>();
            for key in &keys {
                expirations.remove(key);
            }
            keys
        };
        let mut graph = self
            .inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        expired
            .into_iter()
            .filter(|(consumer, token)| graph.release_demand_token(*consumer, *token))
            .count()
    }

    /// Releases a lease only when its owner/fence pair is still current.
    #[must_use]
    pub fn release_fence(&self, fence: DemandLeaseFence) -> bool {
        let released =
            self.with_mut(|graph| graph.release_demand_token(fence.consumer, fence.token));
        if released {
            self.expirations
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .remove(&(fence.consumer, fence.token));
        }
        released
    }

    /// Runs a read-only operation against the demand graph.
    pub fn with<R>(&self, operation: impl FnOnce(&DemandGraph) -> R) -> R {
        let graph = self
            .inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        operation(&graph)
    }

    /// Runs a mutable operation against the demand graph.
    pub fn with_mut<R>(&self, operation: impl FnOnce(&mut DemandGraph) -> R) -> R {
        let mut graph = self
            .inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        operation(&mut graph)
    }
}

/// RAII ownership of one ephemeral demand row.
#[must_use = "retain the demand lease while the client remains interested"]
pub struct DemandLease {
    store: DemandStore,
    consumer: u64,
    token: u64,
    active: bool,
    expires_at: Option<Instant>,
}

/// Owner/fence identity for one demand lease generation.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct DemandLeaseFence {
    consumer: u64,
    token: u64,
}

impl DemandLeaseFence {
    /// Returns the owner identity associated with this fence.
    #[must_use]
    pub const fn consumer(self) -> u64 {
        self.consumer
    }

    /// Returns the lease generation used to reject stale cancellation.
    #[must_use]
    pub const fn token(self) -> u64 {
        self.token
    }
}

impl std::fmt::Debug for DemandLease {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("DemandLease")
            .field("consumer", &self.consumer)
            .field("token", &self.token)
            .field("expires_at", &self.expires_at)
            .field("active", &self.active)
            .finish_non_exhaustive()
    }
}

impl DemandLease {
    /// Returns the consumer ID owned by this lease.
    #[must_use]
    pub const fn consumer(&self) -> u64 {
        self.consumer
    }

    /// Returns whether this lease still retains a demand row.
    #[must_use]
    pub const fn is_active(&self) -> bool {
        self.active
    }

    /// Returns the owner/fence pair used for cancellation-safe release.
    #[must_use]
    pub const fn fence(&self) -> DemandLeaseFence {
        DemandLeaseFence {
            consumer: self.consumer,
            token: self.token,
        }
    }

    /// Returns whether this lease has expired at `now`.
    #[must_use]
    pub fn is_expired(&self, now: Instant) -> bool {
        self.expires_at.is_some_and(|expires_at| expires_at <= now)
    }

    /// Extends a bounded lease from the current instant.
    pub fn renew(&mut self, lifetime: Duration) -> bool {
        if !self.active {
            return false;
        }
        if !self
            .store
            .with(|graph| graph.owns_demand_token(self.consumer, self.token))
        {
            self.active = false;
            return false;
        }
        let Some(expires_at) = Instant::now().checked_add(lifetime) else {
            return false;
        };
        self.expires_at = Some(expires_at);
        self.store
            .expirations
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert((self.consumer, self.token), expires_at);
        true
    }

    /// Returns whether the leased row currently demands a work key.
    #[must_use]
    pub fn is_demanded(&self, work: WorkKey) -> bool {
        self.store.with(|graph| graph.is_demanded(work))
    }

    /// Releases the demand immediately and returns whether a row was removed.
    #[must_use]
    pub fn release(mut self) -> bool {
        self.release_inner()
    }

    fn release_inner(&mut self) -> bool {
        if !self.active {
            return false;
        }
        self.active = false;
        self.store
            .expirations
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(&(self.consumer, self.token));
        self.store
            .with_mut(|graph| graph.release_demand_token(self.consumer, self.token))
    }
}

impl Drop for DemandLease {
    fn drop(&mut self) {
        let _ = self.release_inner();
    }
}
