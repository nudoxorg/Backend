//! Typed scheduling requests and affine route reservations.

use crate::{
    CostSnapshot, LocalCapability, PlacementClass, RemoteCapability, ResourceVector,
    VersionedWorkIdentity,
};
use backend_version::Relation;

/// A typed request to schedule one dependency-closed pure recipe shard.
pub struct ScheduleRequest<R: Relation> {
    /// Complete semantic work identity.
    pub identity: VersionedWorkIdentity<R>,
    /// Scheduler owner epoch.
    pub epoch: u64,
    /// Owner-local logical time used for lease expiry.
    pub now: u64,
    /// Local/remote policy class.
    pub class: PlacementClass,
    /// Checked local capability and input observation.
    pub local: LocalCapability,
    /// Checked remote capability/health observation.
    pub remote: RemoteCapability,
    /// Checked, bounded route-cost snapshot.
    pub costs: CostSnapshot,
    /// Scratch/output bytes charged to the route.
    pub bytes: u64,
    /// Checked multidimensional demand charged once per execution side.
    pub resources: ResourceVector,
    /// Whether a pure hedge is requested when policy selects one.
    pub hedge: bool,
    /// Whether the recipe is repeatable and safe to duplicate.
    pub pure: bool,
    /// Whether a local fallback reservation is held for deadline-optimized
    /// remote work.
    pub fallback_reserved: bool,
    /// Latest owner-local logical time at which the reserved local fallback
    /// must be activated. `None` means no fallback deadline is promised.
    pub fallback_deadline: Option<u64>,
}

impl<R: Relation> ScheduleRequest<R> {
    /// Builds a request from an identity and three checked observations.
    ///
    /// Optional route charges and policy switches can be added with the
    /// builder methods below. The default request is a non-hedged local
    /// preferred request with no byte or multidimensional charge.
    #[must_use]
    pub const fn new(
        identity: VersionedWorkIdentity<R>,
        epoch: u64,
        now: u64,
        class: PlacementClass,
        local: LocalCapability,
        remote: RemoteCapability,
        costs: CostSnapshot,
    ) -> Self {
        Self {
            identity,
            epoch,
            now,
            class,
            local,
            remote,
            costs,
            bytes: 0,
            resources: ResourceVector::zero(),
            hedge: false,
            pure: false,
            fallback_reserved: false,
            fallback_deadline: None,
        }
    }

    /// Adds the byte and multidimensional charges for the selected route.
    #[must_use]
    pub const fn with_charge(mut self, bytes: u64, resources: ResourceVector) -> Self {
        self.bytes = bytes;
        self.resources = resources;
        self
    }

    /// Marks the recipe as repeatable and permits a hedge when policy selects
    /// one.
    #[must_use]
    pub const fn pure(mut self, enabled: bool) -> Self {
        self.pure = enabled;
        self
    }

    /// Enables hedge admission for this request.
    #[must_use]
    pub const fn hedge(mut self, enabled: bool) -> Self {
        self.hedge = enabled;
        self
    }

    /// Retains local capacity as a bounded remote fallback.
    #[must_use]
    pub const fn with_fallback(mut self, deadline: Option<u64>) -> Self {
        self.fallback_reserved = true;
        self.fallback_deadline = deadline;
        self
    }
}

impl<R: Relation> Copy for ScheduleRequest<R> {}

impl<R: Relation> Clone for ScheduleRequest<R> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<R: Relation> PartialEq for ScheduleRequest<R> {
    fn eq(&self, other: &Self) -> bool {
        self.identity == other.identity
            && self.epoch == other.epoch
            && self.now == other.now
            && self.class == other.class
            && self.local == other.local
            && self.remote == other.remote
            && self.costs == other.costs
            && self.bytes == other.bytes
            && self.resources == other.resources
            && self.hedge == other.hedge
            && self.pure == other.pure
            && self.fallback_reserved == other.fallback_reserved
            && self.fallback_deadline == other.fallback_deadline
    }
}

impl<R: Relation> Eq for ScheduleRequest<R> {}

impl<R: Relation> std::fmt::Debug for ScheduleRequest<R> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ScheduleRequest")
            .field("identity", &self.identity)
            .field("epoch", &self.epoch)
            .field("now", &self.now)
            .field("class", &self.class)
            .field("local", &self.local)
            .field("remote", &self.remote)
            .field("costs", &self.costs)
            .field("bytes", &self.bytes)
            .field("resources", &self.resources)
            .field("hedge", &self.hedge)
            .field("pure", &self.pure)
            .field("fallback_reserved", &self.fallback_reserved)
            .field("fallback_deadline", &self.fallback_deadline)
            .finish()
    }
}
