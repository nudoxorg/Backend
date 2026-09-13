//! Owner identity claims used to mint and admit leases.

use crate::{ReplicationError, TypedIdentity, WireIdentity};
use std::{fmt, marker::PhantomData};

/// A caller-owned owner identity branded by `K`.
///
/// The marker prevents a lease minted for one owner class from being passed to
/// a different owner API by accident.  The identity bytes can only enter
/// through a typed backend-version identity.
pub struct OwnerId<K> {
    identity: WireIdentity,
    marker: PhantomData<fn() -> K>,
}

impl<K> Copy for OwnerId<K> {}
impl<K> Clone for OwnerId<K> {
    fn clone(&self) -> Self {
        *self
    }
}
impl<K> fmt::Debug for OwnerId<K> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("OwnerId")
            .field("identity", &self.identity)
            .finish()
    }
}
impl<K> PartialEq for OwnerId<K> {
    fn eq(&self, other: &Self) -> bool {
        self.identity == other.identity
    }
}
impl<K> Eq for OwnerId<K> {}
impl<K> std::hash::Hash for OwnerId<K> {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.identity.hash(state);
    }
}

impl<K> OwnerId<K> {
    /// Encodes one caller-owned typed identity as an owner capability.
    #[must_use]
    pub fn from_typed<T: TypedIdentity>(identity: &T) -> Self {
        Self {
            identity: WireIdentity::from_typed(identity),
            marker: PhantomData,
        }
    }

    /// Returns the owner claim for a wire checkpoint comparison.
    #[must_use]
    pub const fn claim(self) -> WireIdentity {
        self.identity
    }

    /// Returns whether the owner claim has no identity bytes.
    #[must_use]
    pub fn is_zero(self) -> bool {
        self.identity.as_bytes() == [0; 32]
    }

    /// Checks an untrusted wire owner against this caller-owned owner.
    ///
    /// # Errors
    ///
    /// Returns [`ReplicationError::IdentityContext`] for a different owner
    /// schema or [`ReplicationError::IdentityMismatch`] for different bytes.
    pub fn admit(self, claim: WireIdentity) -> Result<(), ReplicationError> {
        if claim.context() != self.identity.context() {
            return Err(ReplicationError::IdentityContext);
        }
        if claim.as_bytes() != self.identity.as_bytes() {
            return Err(ReplicationError::IdentityMismatch);
        }
        Ok(())
    }
}
