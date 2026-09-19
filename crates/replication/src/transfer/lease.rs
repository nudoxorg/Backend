//! Typed retention leases for resumable transfers.
//!
//! A checkpoint is useful only while an owner is still entitled to resume it.
//! These types keep the owner identity, publication fence, authority epoch,
//! revocation observation, and expiry in one private capability.  GC roots
//! carry the same lease token, so an object referenced by a live resume cannot
//! be reclaimed accidentally during reconnect or process restart.

use std::{fmt, marker::PhantomData};

use backend_version::{ObjectKey, ObjectVersion, Schema};

use crate::{
    AdmittedAuthority, AuthorityClaim, Fence, ImmutableObjectSchema, ObjectRequest,
    ReplicationError, RevocationVersion, SchemaWireObjectKey, SchemaWireObjectVersion, TransferId,
    TransportLimits, WireIdentity, claim_schema_object_key, claim_schema_object_version,
};

mod token;
use token::derive_lease_token;
mod root_validation;
pub(crate) use root_validation::{validate_root_binding, validate_roots};

use super::checkpoint::{TransferCheckpoint, WireTransferCheckpoint};

mod claim;
pub use claim::OwnerId;

/// A fixed-width token binding one transfer lease's immutable fields.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct LeaseToken([u8; 32]);
impl LeaseToken {
    /// Returns the fixed-width token used by GC roots and durable claims.
    #[must_use]
    pub const fn as_bytes(self) -> [u8; 32] {
        self.0
    }
}

/// A resumable transfer lease with an explicit owner and expiry.
pub struct TransferLease<K> {
    transfer: TransferId,
    owner: OwnerId<K>,
    authority: AuthorityClaim,
    revocation_version: RevocationVersion,
    fence: Fence,
    issued_at: u64,
    expires_at: u64,
    token: LeaseToken,
}

impl<K> Clone for TransferLease<K> {
    fn clone(&self) -> Self {
        Self {
            transfer: self.transfer,
            owner: self.owner,
            authority: self.authority,
            revocation_version: self.revocation_version,
            fence: self.fence,
            issued_at: self.issued_at,
            expires_at: self.expires_at,
            token: self.token,
        }
    }
}
impl<K> fmt::Debug for TransferLease<K> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("TransferLease")
            .field("transfer", &self.transfer)
            .field("owner", &self.owner)
            .field("authority", &self.authority)
            .field("revocation_version", &self.revocation_version)
            .field("fence", &self.fence)
            .field("issued_at", &self.issued_at)
            .field("expires_at", &self.expires_at)
            .field("token", &self.token)
            .finish()
    }
}
impl<K> PartialEq for TransferLease<K> {
    fn eq(&self, other: &Self) -> bool {
        self.transfer == other.transfer
            && self.owner == other.owner
            && self.authority == other.authority
            && self.revocation_version == other.revocation_version
            && self.fence == other.fence
            && self.issued_at == other.issued_at
            && self.expires_at == other.expires_at
            && self.token == other.token
    }
}
impl<K> Eq for TransferLease<K> {}

impl<K> TransferLease<K> {
    /// Mints a lease for an exact transfer and authority epoch.
    ///
    /// `expires_at` is an owner-supplied monotonic clock value. Persisting a
    /// monotonic deadline rather than a wall-clock timestamp keeps restart and
    /// clock-adjustment policy in the owner while making expiry explicit at
    /// every resume boundary.
    ///
    /// # Errors
    ///
    /// Returns [`ReplicationError::InvalidIdentifier`] for a zero transfer,
    /// zero fence/owner, or non-forward expiry interval.
    pub fn new(
        transfer: TransferId,
        owner: OwnerId<K>,
        authority: AdmittedAuthority,
        revocation_version: RevocationVersion,
        fence: Fence,
        issued_at: u64,
        expires_at: u64,
    ) -> Result<Self, ReplicationError> {
        if transfer.get() == 0 || fence.is_zero() || expires_at <= issued_at || owner.is_zero() {
            return Err(ReplicationError::InvalidIdentifier);
        }
        let token = LeaseToken(derive_lease_token(
            transfer,
            owner.claim(),
            authority.claim(),
            revocation_version,
            fence,
            issued_at,
            expires_at,
        ));
        Ok(Self {
            transfer,
            owner,
            authority: authority.claim(),
            revocation_version,
            fence,
            issued_at,
            expires_at,
            token,
        })
    }

    /// Returns the transfer protected by this lease.
    #[must_use]
    pub const fn transfer(&self) -> TransferId {
        self.transfer
    }

    /// Returns the branded owner capability.
    #[must_use]
    pub const fn owner(&self) -> OwnerId<K> {
        self.owner
    }

    /// Returns the authority claim retained by the lease.
    #[must_use]
    pub const fn authority(&self) -> AuthorityClaim {
        self.authority
    }

    /// Returns the revocation observation bound into this lease.
    #[must_use]
    pub const fn revocation_version(&self) -> RevocationVersion {
        self.revocation_version
    }

    /// Returns the exact publication fence.
    #[must_use]
    pub const fn fence(&self) -> Fence {
        self.fence
    }

    /// Returns the monotonic issuance time.
    #[must_use]
    pub const fn issued_at(&self) -> u64 {
        self.issued_at
    }

    /// Returns the exclusive monotonic expiry.
    #[must_use]
    pub const fn expires_at(&self) -> u64 {
        self.expires_at
    }

    /// Returns the token that must be present on every protected GC root.
    #[must_use]
    pub const fn token(&self) -> LeaseToken {
        self.token
    }

    /// Checks owner, transfer fence, expiry, and revocation freshness.
    ///
    /// # Errors
    ///
    /// Returns [`ReplicationError::StaleFence`] for an owner, transfer, fence,
    /// or time mismatch, or [`ReplicationError::RevokedAuthority`] when the
    /// observed revocation is older than the lease's bound.
    pub fn validate_at(
        &self,
        owner: OwnerId<K>,
        transfer: TransferId,
        authority: AdmittedAuthority,
        fence: Fence,
        observed_revocation: RevocationVersion,
        now: u64,
    ) -> Result<(), ReplicationError> {
        if self.owner != owner
            || self.transfer != transfer
            || self.authority != authority.claim()
            || self.fence != fence
        {
            return Err(ReplicationError::StaleFence);
        }
        if now < self.issued_at || now >= self.expires_at {
            return Err(ReplicationError::StaleFence);
        }
        if observed_revocation < self.revocation_version {
            return Err(ReplicationError::RevokedAuthority);
        }
        Ok(())
    }

    pub(crate) fn from_claim(claim: &LeaseClaim<K>) -> Result<Self, ReplicationError> {
        let lease = Self::new(
            claim.transfer,
            claim.owner,
            claim.authority,
            claim.revocation_version,
            claim.fence,
            claim.issued_at,
            claim.expires_at,
        )?;
        if lease.token != claim.token {
            return Err(ReplicationError::IdentityMismatch);
        }
        Ok(lease)
    }
}

/// The complete immutable field set of an untrusted lease claim.
///
/// Grouping these fields prevents call sites from accidentally swapping two
/// adjacent scalar arguments while keeping token verification in one place.
pub(crate) struct LeaseClaim<K> {
    pub transfer: TransferId,
    pub owner: OwnerId<K>,
    pub authority: AdmittedAuthority,
    pub revocation_version: RevocationVersion,
    pub fence: Fence,
    pub issued_at: u64,
    pub expires_at: u64,
    pub token: LeaseToken,
}

/// A versioned immutable object retained as a GC root by one transfer lease.
pub struct GcRoot<T: Schema = ImmutableObjectSchema, K = ()> {
    transfer: TransferId,
    key: ObjectKey<T>,
    version: ObjectVersion<T>,
    lease: LeaseToken,
    marker: PhantomData<fn() -> K>,
}
impl<T: Schema, K> Copy for GcRoot<T, K> {}
impl<T: Schema, K> Clone for GcRoot<T, K> {
    fn clone(&self) -> Self {
        *self
    }
}
impl<T: Schema, K> fmt::Debug for GcRoot<T, K> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("GcRoot")
            .field("transfer", &self.transfer)
            .field("key", &self.key)
            .field("version", &self.version)
            .field("lease", &self.lease)
            .finish()
    }
}
impl<T: Schema, K> PartialEq for GcRoot<T, K> {
    fn eq(&self, other: &Self) -> bool {
        self.transfer == other.transfer
            && self.key == other.key
            && self.version == other.version
            && self.lease == other.lease
    }
}
impl<T: Schema, K> Eq for GcRoot<T, K> {}
impl<T: Schema, K> GcRoot<T, K> {
    /// Pins one object under the supplied lease token.
    #[must_use]
    pub fn new(lease: &TransferLease<K>, key: ObjectKey<T>, version: ObjectVersion<T>) -> Self {
        Self {
            transfer: lease.transfer,
            key,
            version,
            lease: lease.token,
            marker: PhantomData,
        }
    }

    /// Returns the object transfer identity.
    #[must_use]
    pub const fn transfer(self) -> TransferId {
        self.transfer
    }

    /// Returns the object key.
    #[must_use]
    pub const fn key(self) -> ObjectKey<T> {
        self.key
    }

    /// Returns the complete object version.
    #[must_use]
    pub const fn version(self) -> ObjectVersion<T> {
        self.version
    }

    /// Returns the lease token protecting this root.
    #[must_use]
    pub const fn lease(self) -> LeaseToken {
        self.lease
    }
}

/// A validated transfer checkpoint plus its owner lease and GC roots.
pub struct LeasedTransferCheckpoint<T: Schema = ImmutableObjectSchema, K = ()> {
    checkpoint: TransferCheckpoint<T>,
    lease: TransferLease<K>,
    roots: Vec<GcRoot<T, K>>,
}
impl<T: Schema, K> Clone for LeasedTransferCheckpoint<T, K> {
    fn clone(&self) -> Self {
        Self {
            checkpoint: self.checkpoint.clone(),
            lease: self.lease.clone(),
            roots: self.roots.clone(),
        }
    }
}
impl<T: Schema, K> fmt::Debug for LeasedTransferCheckpoint<T, K> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("LeasedTransferCheckpoint")
            .field("checkpoint", &self.checkpoint)
            .field("lease", &self.lease)
            .field("roots", &self.roots)
            .finish()
    }
}
impl<T: Schema, K> PartialEq for LeasedTransferCheckpoint<T, K> {
    fn eq(&self, other: &Self) -> bool {
        self.checkpoint == other.checkpoint
            && self.lease == other.lease
            && self.roots == other.roots
    }
}
impl<T: Schema, K> Eq for LeasedTransferCheckpoint<T, K> {}
impl<T: Schema, K> LeasedTransferCheckpoint<T, K> {
    /// Creates a leased checkpoint after validating all retained bytes and
    /// requiring a GC root for the transferred object itself.
    ///
    /// # Errors
    ///
    /// Returns a checkpoint, lease, root, or transport-limit error when any
    /// retained chunk or pin is malformed.
    pub fn new(
        checkpoint: TransferCheckpoint<T>,
        lease: TransferLease<K>,
        roots: Vec<GcRoot<T, K>>,
        limits: TransportLimits,
    ) -> Result<Self, ReplicationError> {
        checkpoint.validate(limits)?;
        validate_roots(&checkpoint, &lease, &roots, limits)?;
        Ok(Self {
            checkpoint,
            lease,
            roots,
        })
    }

    /// Validates the checkpoint and lease at a reconnect time.
    ///
    /// # Errors
    ///
    /// Returns a stale-fence, expiry, revocation, root, or checkpoint error
    /// when the retained capability is no longer admissible.
    pub fn validate_at(
        &self,
        owner: OwnerId<K>,
        authority: AdmittedAuthority,
        fence: Fence,
        observed_revocation: RevocationVersion,
        now: u64,
        limits: TransportLimits,
    ) -> Result<(), ReplicationError> {
        self.checkpoint.validate(limits)?;
        self.lease.validate_at(
            owner,
            self.checkpoint.transfer,
            authority,
            fence,
            observed_revocation,
            now,
        )?;
        validate_roots(&self.checkpoint, &self.lease, &self.roots, limits)
    }

    /// Borrows the validated transfer checkpoint.
    #[must_use]
    pub const fn checkpoint(&self) -> &TransferCheckpoint<T> {
        &self.checkpoint
    }

    /// Borrows the lease protecting this checkpoint.
    #[must_use]
    pub const fn lease(&self) -> &TransferLease<K> {
        &self.lease
    }

    /// Borrows all object roots pinned by this checkpoint.
    #[must_use]
    pub fn roots(&self) -> &[GcRoot<T, K>] {
        &self.roots
    }

    /// Converts this typed capability to an untrusted durable claim.
    ///
    /// # Errors
    ///
    /// Returns an identity encoding error when a typed checkpoint cannot be
    /// represented by the fixed-width wire grammar.
    pub fn to_wire(&self) -> Result<WireLeasedTransferCheckpoint<T>, ReplicationError> {
        let roots = self
            .roots
            .iter()
            .copied()
            .map(GcRoot::to_wire)
            .collect::<Result<Vec<_>, _>>()?;
        Ok(WireLeasedTransferCheckpoint {
            checkpoint: self.checkpoint.to_wire()?,
            owner: self.lease.owner.claim(),
            authority: self.lease.authority,
            revocation_version: self.lease.revocation_version,
            fence: self.lease.fence,
            issued_at: self.lease.issued_at,
            expires_at: self.lease.expires_at,
            token: self.lease.token,
            roots,
        })
    }

    /// Consumes this typed capability into a durable wire claim.
    ///
    /// # Errors
    ///
    /// Returns an identity encoding error when a typed checkpoint cannot be
    /// represented by the fixed-width wire grammar.
    pub fn into_wire(self) -> Result<WireLeasedTransferCheckpoint<T>, ReplicationError> {
        let (checkpoint, lease, roots) = self.into_parts();
        let roots = roots
            .into_iter()
            .map(GcRoot::into_wire)
            .collect::<Result<Vec<_>, _>>()?;
        Ok(WireLeasedTransferCheckpoint {
            checkpoint: checkpoint.into_wire()?,
            owner: lease.owner.claim(),
            authority: lease.authority,
            revocation_version: lease.revocation_version,
            fence: lease.fence,
            issued_at: lease.issued_at,
            expires_at: lease.expires_at,
            token: lease.token,
            roots,
        })
    }

    /// Consumes the wrapper and returns its parts for durable serialization.
    #[must_use]
    pub fn into_parts(self) -> (TransferCheckpoint<T>, TransferLease<K>, Vec<GcRoot<T, K>>) {
        (self.checkpoint, self.lease, self.roots)
    }
}

impl<T: Schema, K> GcRoot<T, K> {
    pub(crate) fn to_wire(self) -> Result<WireGcRoot<T>, ReplicationError> {
        Ok(WireGcRoot {
            transfer: self.transfer,
            key: claim_schema_object_key(self.key).map_err(ReplicationError::from)?,
            version: claim_schema_object_version(self.version).map_err(ReplicationError::from)?,
            lease: self.lease,
        })
    }

    pub(crate) fn into_wire(self) -> Result<WireGcRoot<T>, ReplicationError> {
        self.to_wire()
    }
}

/// One untrusted GC-root claim carried in a durable leased checkpoint.
pub struct WireGcRoot<T: Schema = ImmutableObjectSchema> {
    /// Transfer identity owning this root.
    pub transfer: TransferId,
    /// Untrusted object key claim.
    pub key: SchemaWireObjectKey<T>,
    /// Untrusted object version claim.
    pub version: SchemaWireObjectVersion<T>,
    /// Lease token claim.
    pub lease: LeaseToken,
}
impl<T: Schema> Copy for WireGcRoot<T> {}
impl<T: Schema> Clone for WireGcRoot<T> {
    fn clone(&self) -> Self {
        *self
    }
}
impl<T: Schema> fmt::Debug for WireGcRoot<T> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("WireGcRoot")
            .field("transfer", &self.transfer)
            .field("key", &self.key)
            .field("version", &self.version)
            .field("lease", &self.lease)
            .finish()
    }
}
impl<T: Schema> PartialEq for WireGcRoot<T> {
    fn eq(&self, other: &Self) -> bool {
        self.transfer == other.transfer
            && self.key == other.key
            && self.version == other.version
            && self.lease == other.lease
    }
}
impl<T: Schema> Eq for WireGcRoot<T> {}

/// A durable leased checkpoint claim. Every identity remains untrusted until
/// [`Self::admit_against`] checks it against the active request and owner.
pub struct WireLeasedTransferCheckpoint<T: Schema = ImmutableObjectSchema> {
    /// Untrusted object transfer checkpoint.
    pub checkpoint: WireTransferCheckpoint<T>,
    /// Untrusted owner identity claim.
    pub owner: WireIdentity,
    /// Authority claim retained as part of the lease fence.
    pub authority: AuthorityClaim,
    /// Revocation observation bound into the lease.
    pub revocation_version: RevocationVersion,
    /// Exact publication fence.
    pub fence: Fence,
    /// Monotonic issuance time.
    pub issued_at: u64,
    /// Exclusive monotonic expiry.
    pub expires_at: u64,
    /// Lease token claim.
    pub token: LeaseToken,
    /// Untrusted immutable object roots pinned by the lease.
    pub roots: Vec<WireGcRoot<T>>,
}
impl<T: Schema> Clone for WireLeasedTransferCheckpoint<T> {
    fn clone(&self) -> Self {
        Self {
            checkpoint: self.checkpoint.clone(),
            owner: self.owner,
            authority: self.authority,
            revocation_version: self.revocation_version,
            fence: self.fence,
            issued_at: self.issued_at,
            expires_at: self.expires_at,
            token: self.token,
            roots: self.roots.clone(),
        }
    }
}
impl<T: Schema> fmt::Debug for WireLeasedTransferCheckpoint<T> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("WireLeasedTransferCheckpoint")
            .field("checkpoint", &self.checkpoint)
            .field("owner", &self.owner)
            .field("authority", &self.authority)
            .field("revocation_version", &self.revocation_version)
            .field("fence", &self.fence)
            .field("issued_at", &self.issued_at)
            .field("expires_at", &self.expires_at)
            .field("token", &self.token)
            .field("roots", &self.roots)
            .finish()
    }
}
impl<T: Schema> PartialEq for WireLeasedTransferCheckpoint<T> {
    fn eq(&self, other: &Self) -> bool {
        self.checkpoint == other.checkpoint
            && self.owner == other.owner
            && self.authority == other.authority
            && self.revocation_version == other.revocation_version
            && self.fence == other.fence
            && self.issued_at == other.issued_at
            && self.expires_at == other.expires_at
            && self.token == other.token
            && self.roots == other.roots
    }
}
impl<T: Schema> Eq for WireLeasedTransferCheckpoint<T> {}

impl<T: Schema> WireLeasedTransferCheckpoint<T> {
    /// Checks only bounded claim structure.
    ///
    /// # Errors
    ///
    /// Returns a size, identity, expiry, or root-shape error when the durable
    /// claim is malformed under `limits`.
    pub fn validate(&self, limits: TransportLimits) -> Result<(), ReplicationError> {
        self.checkpoint.validate(limits)?;
        if self.owner.as_bytes() == [0; 32]
            || self.fence.is_zero()
            || self.expires_at <= self.issued_at
            || self.roots.is_empty()
            || self.roots.len() > limits.max_objects
        {
            return Err(ReplicationError::InvalidIdentifier);
        }
        if self.checkpoint.authority != self.authority {
            return Err(ReplicationError::StaleFence);
        }
        for root in &self.roots {
            root.key
                .admit_context(backend_version::IdContext::object_key::<T>())?;
            root.version
                .admit_context(backend_version::IdContext::schema::<T>())?;
        }
        Ok(())
    }

    /// Admits this durable claim against the exact active request, owner,
    /// authority, and fence. `expected_roots` is caller-owned typed material;
    /// accepting opaque extra roots would otherwise mint unverified GC pins.
    ///
    /// # Errors
    ///
    /// Returns a stale-fence, expiry, revocation, identity, root, or bounds
    /// error when any durable claim differs from the active capability.
    // These fields are separate trust fences by design; grouping them would
    // make it easier to accidentally reuse a context from another transfer.
    #[allow(clippy::too_many_arguments)]
    pub fn admit_against<K>(
        self,
        request: &ObjectRequest<T>,
        owner: OwnerId<K>,
        expected_roots: &[GcRoot<T, K>],
        authority: AdmittedAuthority,
        observed_revocation: RevocationVersion,
        fence: Fence,
        now: u64,
        limits: TransportLimits,
    ) -> Result<LeasedTransferCheckpoint<T, K>, ReplicationError> {
        self.validate(limits)?;
        if self.authority != authority.claim() || self.fence != fence {
            return Err(ReplicationError::StaleFence);
        }
        owner.admit(self.owner)?;
        if expected_roots.len() != self.roots.len() {
            return Err(ReplicationError::IdentityMismatch);
        }
        let checkpoint = self
            .checkpoint
            .admit_against(request, authority.claim(), limits)?;
        let lease = TransferLease::from_claim(&LeaseClaim {
            transfer: checkpoint.transfer,
            owner,
            authority,
            revocation_version: self.revocation_version,
            fence,
            issued_at: self.issued_at,
            expires_at: self.expires_at,
            token: self.token,
        })?;
        lease.validate_at(
            owner,
            checkpoint.transfer,
            authority,
            fence,
            observed_revocation,
            now,
        )?;
        for (claim, expected) in self.roots.iter().zip(expected_roots) {
            let expected_key =
                claim_schema_object_key(expected.key).map_err(ReplicationError::from)?;
            let expected_version =
                claim_schema_object_version(expected.version).map_err(ReplicationError::from)?;
            if claim.transfer != expected.transfer
                || claim.lease != expected.lease
                || claim.key != expected_key
                || claim.version != expected_version
            {
                return Err(ReplicationError::IdentityMismatch);
            }
        }
        LeasedTransferCheckpoint::new(checkpoint, lease, expected_roots.to_vec(), limits)
    }
}

#[cfg(test)]
#[path = "lease_tests.rs"]
mod lease_tests;
