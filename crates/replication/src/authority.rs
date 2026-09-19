//! Opaque authority claims and caller-owned admission policy.

use crate::{
    AuthorityEpoch, ExpectedIdentity, ReplicationError, RevocationVersion, TypedIdentity,
    WireIdentity,
};

/// An authority identity and epoch carried as an untrusted wire claim.
///
/// Replication transports this envelope but does not define the authority
/// schema. The execution or workspace owner must compare it with an
/// [`AuthorityExpectation`] before accepting a result or root.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AuthorityClaim {
    /// Untrusted authority identity with its caller-supplied context.
    pub id: WireIdentity,
    /// Claimed authority epoch.
    pub epoch: AuthorityEpoch,
}
/// Compatibility name for an authority claim at a wire boundary.
pub type WireAuthority = AuthorityClaim;
impl AuthorityClaim {
    /// Creates an authority claim from an already encoded identity.
    #[must_use]
    pub const fn new(id: WireIdentity, epoch: AuthorityEpoch) -> Self {
        Self { id, epoch }
    }

    /// Encodes a caller-owned typed authority identity as an untrusted claim.
    #[must_use]
    pub fn from_typed<T: TypedIdentity>(id: &T, epoch: AuthorityEpoch) -> Self {
        Self::new(WireIdentity::from_typed(id), epoch)
    }
}

/// Authority evidence admitted against an owner-owned expectation.
///
/// This capability is the only authority representation accepted by lease
/// construction. A wire claim can be transported and compared, but cannot
/// itself pin bytes or mint a retention lease.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AdmittedAuthority(AuthorityClaim);
impl AdmittedAuthority {
    /// Returns the authenticated wire representation for protocol signing.
    #[must_use]
    pub const fn claim(self) -> AuthorityClaim {
        self.0
    }
}

/// Caller-owned expected authority material used to admit wire claims.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AuthorityExpectation {
    /// Exact typed authority identity encoded for comparison.
    id: ExpectedIdentity,
    /// Minimum accepted authority epoch.
    pub minimum_epoch: AuthorityEpoch,
    /// Minimum revocation observation required for acceptance.
    pub revocation_version: RevocationVersion,
}
impl AuthorityExpectation {
    /// Builds expected authority material from a caller-owned typed identity.
    #[must_use]
    pub fn from_typed<T: TypedIdentity>(
        id: &T,
        minimum_epoch: AuthorityEpoch,
        revocation_version: RevocationVersion,
    ) -> Self {
        Self {
            id: ExpectedIdentity::from_typed(id),
            minimum_epoch,
            revocation_version,
        }
    }

    /// Admits a wire authority claim against this exact expected identity.
    ///
    /// # Errors
    ///
    /// Returns an identity, epoch, or revocation error when the claim is not
    /// acceptable for the caller's policy.
    pub fn admit(
        self,
        authority: AuthorityClaim,
        observed_revocation: RevocationVersion,
    ) -> Result<(), ReplicationError> {
        self.id.matches(authority.id)?;
        if authority.epoch < self.minimum_epoch {
            return Err(ReplicationError::StaleAuthority);
        }
        if observed_revocation < self.revocation_version {
            return Err(ReplicationError::RevokedAuthority);
        }
        Ok(())
    }

    /// Admits a wire authority and retains the result as lease-capable
    /// evidence. The plain [`Self::admit`] method remains available for
    /// comparison-only protocol paths.
    ///
    /// # Errors
    ///
    /// Returns an identity, epoch, or revocation error when the claim does not
    /// satisfy this expectation.
    pub fn admit_capability(
        self,
        authority: AuthorityClaim,
        observed_revocation: RevocationVersion,
    ) -> Result<AdmittedAuthority, ReplicationError> {
        self.admit(authority, observed_revocation)?;
        Ok(AdmittedAuthority(authority))
    }
}

/// An authority policy claim received from the wire.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct WireAuthorityPolicy {
    /// Untrusted authority identity.
    pub id: WireIdentity,
    /// Minimum accepted epoch.
    pub minimum_epoch: AuthorityEpoch,
    /// Minimum revocation observation required for acceptance.
    pub revocation_version: RevocationVersion,
}
impl WireAuthorityPolicy {
    /// Admits this claim against caller-owned expected policy material.
    ///
    /// # Errors
    ///
    /// Returns an identity, epoch, or revocation error when the claim differs
    /// from the expected execution policy.
    pub fn admit_against(
        self,
        expected: AuthorityExpectation,
        observed_revocation: RevocationVersion,
    ) -> Result<(), ReplicationError> {
        if self.minimum_epoch != expected.minimum_epoch
            || self.revocation_version != expected.revocation_version
        {
            return Err(ReplicationError::StaleAuthority);
        }
        expected.id.matches(self.id)?;
        if observed_revocation < self.revocation_version {
            return Err(ReplicationError::RevokedAuthority);
        }
        Ok(())
    }
}
