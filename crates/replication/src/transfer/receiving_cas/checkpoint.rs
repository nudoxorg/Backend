//! Byte-free receiving checkpoints and their leased restart capabilities.

use super::super::lease::{
    GcRoot, LeaseClaim, LeaseToken, OwnerId, TransferLease, WireGcRoot, validate_root_binding,
};
use super::validation::validate_extent_metadata;
use super::{ExtentId, StagedExtent, WireStagedExtent};
use crate::{
    AdmittedAuthority, AuthorityClaim, ImmutableObjectSchema, ObjectRequest, ReplicationError,
    SparseCoverage, TransferId, TransportLimits, WireIdentity, claim_schema_object_key,
    claim_schema_object_version,
};
use backend_version::{ObjectKey, ObjectVersion, Schema};
use std::fmt;

/// A bounded, byte-free checkpoint for an incremental sparse CAS transfer.
///
/// The checkpoint persists object identities, coverage, and extent handles;
/// the payload remains owned by the CAS. It can therefore survive process
/// restart without serializing or cloning the object itself.
pub struct ReceivingCheckpoint<T: Schema = ImmutableObjectSchema> {
    /// Transfer identity.
    pub transfer: TransferId,
    /// Logical object key.
    pub key: ObjectKey<T>,
    /// Complete object version.
    pub version: ObjectVersion<T>,
    /// Complete canonical byte length.
    pub len: u64,
    /// Authority fence carried by the transfer.
    pub authority: AuthorityClaim,
    /// Sparse retained byte coverage.
    pub coverage: SparseCoverage,
    /// Authenticated staged extent metadata, ordered by sequence.
    pub extents: Vec<StagedExtent>,
}
impl<T: Schema> Clone for ReceivingCheckpoint<T> {
    fn clone(&self) -> Self {
        Self {
            transfer: self.transfer,
            key: self.key,
            version: self.version,
            len: self.len,
            authority: self.authority,
            coverage: self.coverage.clone(),
            extents: self.extents.clone(),
        }
    }
}
impl<T: Schema> fmt::Debug for ReceivingCheckpoint<T> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ReceivingCheckpoint")
            .field("transfer", &self.transfer)
            .field("key", &self.key)
            .field("version", &self.version)
            .field("len", &self.len)
            .field("authority", &self.authority)
            .field("coverage", &self.coverage)
            .field("extents", &self.extents)
            .finish()
    }
}
impl<T: Schema> PartialEq for ReceivingCheckpoint<T> {
    fn eq(&self, other: &Self) -> bool {
        self.transfer == other.transfer
            && self.key == other.key
            && self.version == other.version
            && self.len == other.len
            && self.authority == other.authority
            && self.coverage == other.coverage
            && self.extents == other.extents
    }
}
impl<T: Schema> Eq for ReceivingCheckpoint<T> {}

impl<T: Schema> ReceivingCheckpoint<T> {
    /// Creates a checkpoint after validating placement, chains, coverage, and
    /// the caller's explicit extent metadata bound.
    ///
    /// # Errors
    ///
    /// Returns a bounds, identity, range, replay, or chain error when the
    /// checkpoint metadata is malformed.
    pub fn new(
        request: &ObjectRequest<T>,
        authority: AuthorityClaim,
        coverage: SparseCoverage,
        extents: Vec<StagedExtent>,
        limits: TransportLimits,
        max_extents: usize,
    ) -> Result<Self, ReplicationError> {
        let checkpoint = Self {
            transfer: request.transfer,
            key: request.key,
            version: request.version,
            len: request.len,
            authority,
            coverage,
            extents,
        };
        checkpoint.validate_against(request, authority, limits, max_extents)?;
        Ok(checkpoint)
    }

    /// Validates the byte-free checkpoint against the active request.
    ///
    /// # Errors
    ///
    /// Returns a stale-fence, bounds, identity, range, replay, or chain error
    /// when the checkpoint does not describe `request` exactly.
    pub fn validate_against(
        &self,
        request: &ObjectRequest<T>,
        authority: AuthorityClaim,
        limits: TransportLimits,
        max_extents: usize,
    ) -> Result<(), ReplicationError> {
        self.validate_structure(limits, max_extents)?;
        request.validate(limits)?;
        if self.transfer != request.transfer
            || self.key != request.key
            || self.version != request.version
            || self.len != request.len
            || self.authority != authority
        {
            return Err(ReplicationError::StaleFence);
        }
        Ok(())
    }

    /// Validates the checkpoint's self-contained shape without requiring an
    /// active request. This is the restart-safe structural half of admission.
    pub(crate) fn validate_structure(
        &self,
        limits: TransportLimits,
        max_extents: usize,
    ) -> Result<(), ReplicationError> {
        limits.validate()?;
        if self.transfer.get() == 0 || self.len == 0 {
            return Err(ReplicationError::InvalidIdentifier);
        }
        if self.len > limits.max_object {
            return Err(ReplicationError::ObjectTooLarge);
        }
        if max_extents == 0 || self.extents.len() > max_extents {
            return Err(ReplicationError::CoverageLimit);
        }
        if self.coverage.ranges().len() > limits.max_ranges {
            return Err(ReplicationError::CoverageLimit);
        }
        for range in self.coverage.ranges() {
            if range.end()? > self.len {
                return Err(ReplicationError::Range);
            }
        }
        validate_extent_metadata(
            self.len,
            &self.coverage,
            self.extents.iter().copied(),
            limits,
            max_extents,
        )
    }

    /// Converts extent metadata to a durable claim without touching payloads.
    ///
    /// # Errors
    ///
    /// Returns an identity encoding error when a typed object claim cannot be
    /// represented by the fixed-width wire grammar.
    pub fn to_wire(&self) -> Result<WireReceivingCheckpoint<T>, ReplicationError> {
        Ok(WireReceivingCheckpoint {
            transfer: self.transfer,
            key: claim_schema_object_key(self.key).map_err(ReplicationError::from)?,
            version: claim_schema_object_version(self.version).map_err(ReplicationError::from)?,
            len: self.len,
            authority: self.authority,
            coverage: self.coverage.clone(),
            extents: self.extents.iter().copied().map(Into::into).collect(),
        })
    }
}

/// Durable wire claim for a byte-free receiving checkpoint.
pub struct WireReceivingCheckpoint<T: Schema = ImmutableObjectSchema> {
    /// Transfer identity claim.
    pub transfer: TransferId,
    /// Typed-schema object key claim.
    pub key: crate::SchemaWireObjectKey<T>,
    /// Typed-schema object version claim.
    pub version: crate::SchemaWireObjectVersion<T>,
    /// Complete object length.
    pub len: u64,
    /// Authority fence claim.
    pub authority: AuthorityClaim,
    /// Sparse retained coverage claim.
    pub coverage: SparseCoverage,
    /// Extent metadata; no payload bytes are embedded.
    pub extents: Vec<WireStagedExtent>,
}
impl<T: Schema> Clone for WireReceivingCheckpoint<T> {
    fn clone(&self) -> Self {
        Self {
            transfer: self.transfer,
            key: self.key,
            version: self.version,
            len: self.len,
            authority: self.authority,
            coverage: self.coverage.clone(),
            extents: self.extents.clone(),
        }
    }
}
impl<T: Schema> fmt::Debug for WireReceivingCheckpoint<T> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("WireReceivingCheckpoint")
            .field("transfer", &self.transfer)
            .field("key", &self.key)
            .field("version", &self.version)
            .field("len", &self.len)
            .field("authority", &self.authority)
            .field("coverage", &self.coverage)
            .field("extents", &self.extents)
            .finish()
    }
}
impl<T: Schema> PartialEq for WireReceivingCheckpoint<T> {
    fn eq(&self, other: &Self) -> bool {
        self.transfer == other.transfer
            && self.key == other.key
            && self.version == other.version
            && self.len == other.len
            && self.authority == other.authority
            && self.coverage == other.coverage
            && self.extents == other.extents
    }
}
impl<T: Schema> Eq for WireReceivingCheckpoint<T> {}
impl<T: Schema> WireReceivingCheckpoint<T> {
    /// Validates bounded wire structure and schema contexts.
    ///
    /// # Errors
    ///
    /// Returns a size, context, range, identity, replay, or chain error when
    /// the untrusted checkpoint cannot be represented safely.
    pub fn validate(
        &self,
        limits: TransportLimits,
        max_extents: usize,
    ) -> Result<(), ReplicationError> {
        limits.validate()?;
        if self.transfer.get() == 0 || self.len == 0 {
            return Err(ReplicationError::InvalidIdentifier);
        }
        if self.len > limits.max_object {
            return Err(ReplicationError::ObjectTooLarge);
        }
        if max_extents == 0
            || self.extents.len() > max_extents
            || self.coverage.ranges().len() > limits.max_ranges
        {
            return Err(ReplicationError::CoverageLimit);
        }
        self.key
            .admit_context(backend_version::IdContext::object_key::<T>())?;
        self.version
            .admit_context(backend_version::IdContext::schema::<T>())?;
        for extent in &self.extents {
            let extent: StagedExtent = (*extent).into();
            if extent.id != ExtentId(extent.chain.0) {
                return Err(ReplicationError::IdentityMismatch);
            }
            if extent.len == 0
                || extent.len > limits.max_chunk as u64
                || extent.sequence >= self.len
            {
                return Err(ReplicationError::Range);
            }
        }
        validate_extent_metadata(
            self.len,
            &self.coverage,
            self.extents.iter().copied().map(Into::into),
            limits,
            max_extents,
        )
    }

    /// Admits this wire claim against exact caller-owned request material.
    ///
    /// # Errors
    ///
    /// Returns a stale-fence, identity, bounds, replay, or chain error when
    /// the claim differs from the active request.
    pub fn admit_against(
        self,
        request: &ObjectRequest<T>,
        authority: AuthorityClaim,
        limits: TransportLimits,
        max_extents: usize,
    ) -> Result<ReceivingCheckpoint<T>, ReplicationError> {
        self.validate(limits, max_extents)?;
        let expected_key = claim_schema_object_key(request.key).map_err(ReplicationError::from)?;
        let expected_version =
            claim_schema_object_version(request.version).map_err(ReplicationError::from)?;
        if self.transfer != request.transfer
            || self.len != request.len
            || self.authority != authority
            || self.key != expected_key
            || self.version != expected_version
        {
            return Err(ReplicationError::StaleFence);
        }
        let checkpoint = ReceivingCheckpoint {
            transfer: self.transfer,
            key: request.key,
            version: request.version,
            len: self.len,
            authority: self.authority,
            coverage: self.coverage,
            extents: self.extents.into_iter().map(Into::into).collect(),
        };
        checkpoint.validate_against(request, authority, limits, max_extents)?;
        Ok(checkpoint)
    }
}

/// A byte-free receiving checkpoint protected by the same typed lease and GC
/// root capability as the normal transfer checkpoint.
pub struct LeasedReceivingCheckpoint<T: Schema = ImmutableObjectSchema, K = ()> {
    checkpoint: ReceivingCheckpoint<T>,
    lease: TransferLease<K>,
    roots: Vec<GcRoot<T, K>>,
}
impl<T: Schema, K> Clone for LeasedReceivingCheckpoint<T, K> {
    fn clone(&self) -> Self {
        Self {
            checkpoint: self.checkpoint.clone(),
            lease: self.lease.clone(),
            roots: self.roots.clone(),
        }
    }
}
impl<T: Schema, K> fmt::Debug for LeasedReceivingCheckpoint<T, K> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("LeasedReceivingCheckpoint")
            .field("checkpoint", &self.checkpoint)
            .field("lease", &self.lease)
            .field("roots", &self.roots)
            .finish()
    }
}
impl<T: Schema, K> PartialEq for LeasedReceivingCheckpoint<T, K> {
    fn eq(&self, other: &Self) -> bool {
        self.checkpoint == other.checkpoint
            && self.lease == other.lease
            && self.roots == other.roots
    }
}
impl<T: Schema, K> Eq for LeasedReceivingCheckpoint<T, K> {}
impl<T: Schema, K> LeasedReceivingCheckpoint<T, K> {
    /// Creates a leased checkpoint after validating all extent metadata and
    /// requiring a root pin for the exact object being transferred.
    ///
    /// # Errors
    ///
    /// Returns a checkpoint, lease, root, or transport-limit error when any
    /// extent metadata or GC pin is malformed.
    pub fn new(
        checkpoint: ReceivingCheckpoint<T>,
        lease: TransferLease<K>,
        roots: Vec<GcRoot<T, K>>,
        limits: TransportLimits,
        max_extents: usize,
    ) -> Result<Self, ReplicationError> {
        checkpoint.validate_structure(limits, max_extents)?;
        validate_root_binding(
            checkpoint.transfer,
            checkpoint.key,
            checkpoint.version,
            checkpoint.authority,
            &lease,
            &roots,
            limits,
        )?;
        Ok(Self {
            checkpoint,
            lease,
            roots,
        })
    }

    /// Returns the byte-free checkpoint.
    #[must_use]
    pub const fn checkpoint(&self) -> &ReceivingCheckpoint<T> {
        &self.checkpoint
    }

    /// Returns the lease capability.
    #[must_use]
    pub const fn lease(&self) -> &TransferLease<K> {
        &self.lease
    }

    /// Returns the roots retained for GC protection.
    #[must_use]
    pub fn roots(&self) -> &[GcRoot<T, K>] {
        &self.roots
    }

    /// Validates owner, authority, fence, revocation, expiry, and extents at
    /// reconnect time.
    ///
    /// # Errors
    ///
    /// Returns a stale-fence, expiry, revocation, root, or checkpoint error
    /// when the retained capability is no longer admissible.
    // The explicit request and lease fences stay visible at this restart
    // boundary; each scalar is independently authenticated by the lease.
    #[allow(clippy::too_many_arguments)]
    pub fn validate_at(
        &self,
        request: &ObjectRequest<T>,
        owner: OwnerId<K>,
        authority: AdmittedAuthority,
        observed_revocation: crate::RevocationVersion,
        fence: crate::Fence,
        now: u64,
        limits: TransportLimits,
        max_extents: usize,
    ) -> Result<(), ReplicationError> {
        self.checkpoint
            .validate_against(request, authority.claim(), limits, max_extents)?;
        self.lease.validate_at(
            owner,
            request.transfer,
            authority,
            fence,
            observed_revocation,
            now,
        )?;
        validate_root_binding(
            self.checkpoint.transfer,
            self.checkpoint.key,
            self.checkpoint.version,
            self.checkpoint.authority,
            &self.lease,
            &self.roots,
            limits,
        )
    }

    /// Converts the leased byte-free checkpoint into an untrusted durable
    /// claim. No extent payload is copied.
    ///
    /// # Errors
    ///
    /// Returns an identity encoding error when a typed checkpoint or root
    /// cannot be represented by the fixed-width wire grammar.
    pub fn to_wire(&self) -> Result<WireLeasedReceivingCheckpoint<T>, ReplicationError> {
        let roots = self
            .roots
            .iter()
            .copied()
            .map(GcRoot::to_wire)
            .collect::<Result<Vec<_>, _>>()?;
        Ok(WireLeasedReceivingCheckpoint {
            checkpoint: self.checkpoint.to_wire()?,
            owner: self.lease.owner().claim(),
            authority: self.lease.authority(),
            revocation_version: self.lease.revocation_version(),
            fence: self.lease.fence(),
            issued_at: self.lease.issued_at(),
            expires_at: self.lease.expires_at(),
            token: self.lease.token(),
            roots,
        })
    }
}

/// Untrusted durable claim for a leased byte-free receiving checkpoint.
pub struct WireLeasedReceivingCheckpoint<T: Schema = ImmutableObjectSchema> {
    /// Byte-free receiving checkpoint claim.
    pub checkpoint: WireReceivingCheckpoint<T>,
    /// Owner identity claim.
    pub owner: WireIdentity,
    /// Authority epoch claim.
    pub authority: AuthorityClaim,
    /// Revocation observation bound into the lease.
    pub revocation_version: crate::RevocationVersion,
    /// Publication fence.
    pub fence: crate::Fence,
    /// Monotonic issuance time.
    pub issued_at: u64,
    /// Exclusive monotonic expiry.
    pub expires_at: u64,
    /// Lease token.
    pub token: LeaseToken,
    /// GC roots protected by the lease.
    pub roots: Vec<WireGcRoot<T>>,
}
impl<T: Schema> Clone for WireLeasedReceivingCheckpoint<T> {
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
impl<T: Schema> fmt::Debug for WireLeasedReceivingCheckpoint<T> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("WireLeasedReceivingCheckpoint")
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
impl<T: Schema> PartialEq for WireLeasedReceivingCheckpoint<T> {
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
impl<T: Schema> Eq for WireLeasedReceivingCheckpoint<T> {}
impl<T: Schema> WireLeasedReceivingCheckpoint<T> {
    /// Validates bounded claim shape without admitting owner/object identity.
    ///
    /// # Errors
    ///
    /// Returns a size, identity, expiry, root, or checkpoint error when the
    /// durable claim is malformed under `limits`.
    pub fn validate(
        &self,
        limits: TransportLimits,
        max_extents: usize,
    ) -> Result<(), ReplicationError> {
        self.checkpoint.validate(limits, max_extents)?;
        if self.owner.as_bytes() == [0; 32]
            || self.fence.is_zero()
            || self.expires_at <= self.issued_at
            || self.roots.is_empty()
            || self.roots.len() > limits.max_objects
            || self.checkpoint.authority != self.authority
        {
            return Err(ReplicationError::InvalidIdentifier);
        }
        for root in &self.roots {
            root.key
                .admit_context(backend_version::IdContext::object_key::<T>())?;
            root.version
                .admit_context(backend_version::IdContext::schema::<T>())?;
        }
        Ok(())
    }

    /// Admits owner, lease, root, and typed request bindings after restart.
    ///
    /// # Errors
    ///
    /// Returns a stale-fence, expiry, revocation, identity, root, or bounds
    /// error when the durable claim differs from the active owner and request.
    // Keep every independently authenticated fence explicit at the wire
    // admission boundary rather than hiding them in an untrusted aggregate.
    #[allow(clippy::too_many_arguments)]
    pub fn admit_against<K>(
        self,
        request: &ObjectRequest<T>,
        owner: OwnerId<K>,
        expected_roots: &[GcRoot<T, K>],
        authority: AdmittedAuthority,
        observed_revocation: crate::RevocationVersion,
        fence: crate::Fence,
        now: u64,
        limits: TransportLimits,
        max_extents: usize,
    ) -> Result<LeasedReceivingCheckpoint<T, K>, ReplicationError> {
        self.validate(limits, max_extents)?;
        if self.authority != authority.claim() || self.fence != fence {
            return Err(ReplicationError::StaleFence);
        }
        owner.admit(self.owner)?;
        if self.roots.len() != expected_roots.len() {
            return Err(ReplicationError::IdentityMismatch);
        }
        let checkpoint =
            self.checkpoint
                .admit_against(request, authority.claim(), limits, max_extents)?;
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
            request.transfer,
            authority,
            fence,
            observed_revocation,
            now,
        )?;
        for (claim, expected) in self.roots.iter().zip(expected_roots) {
            let expected_key =
                claim_schema_object_key(expected.key()).map_err(ReplicationError::from)?;
            let expected_version =
                claim_schema_object_version(expected.version()).map_err(ReplicationError::from)?;
            if claim.transfer != expected.transfer()
                || claim.lease != expected.lease()
                || claim.key != expected_key
                || claim.version != expected_version
            {
                return Err(ReplicationError::IdentityMismatch);
            }
        }
        LeasedReceivingCheckpoint::new(
            checkpoint,
            lease,
            expected_roots.to_vec(),
            limits,
            max_extents,
        )
    }
}
