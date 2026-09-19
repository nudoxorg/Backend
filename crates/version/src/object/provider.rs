//! Defines provider behavior for `backend_version::object`, whose purpose is to describe canonical objects, providers, and residency.
//! This module owns the provider invariants and typed state transitions.
//! Its narrow surface prevents representation and policy details from leaking outward.
use core::num::NonZeroU64;
use core::ops::Deref;

use thiserror::Error;

/// Compact provider index in the fixed 64-provider promise domain.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
#[repr(transparent)]
pub struct ProviderId(u8);

impl TryFrom<u8> for ProviderId {
    type Error = ProviderIdError;

    /// Validates a compact provider index.
    fn try_from(raw: u8) -> Result<Self, Self::Error> {
        if raw < 64 {
            Ok(Self(raw))
        } else {
            Err(ProviderIdError::OutOfRange { raw })
        }
    }
}

impl Deref for ProviderId {
    type Target = u8;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

/// Provider-index construction failure.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum ProviderIdError {
    /// The fixed promise domain has only 64 provider slots.
    #[error("provider index {raw} is outside the fixed 64-provider domain")]
    OutOfRange {
        /// Rejected provider index.
        raw: u8,
    },
}

/// Non-empty bounded provider set.
///
/// Its representation is private and every public constructor sets at least
/// one bit, so a promised locality cannot be unfulfillable.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
#[repr(transparent)]
pub struct ProviderSet(u64);

/// Rejected raw provider-set bits.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum ProviderSetError {
    /// A promise must name at least one eligible provider.
    #[error("provider-set bits must name at least one provider")]
    Empty,
}

impl ProviderSet {
    /// Creates a singleton provider set.
    #[must_use]
    pub const fn only(provider: ProviderId) -> Self {
        Self(1_u64 << provider.0)
    }

    /// Adds one provider while preserving non-emptiness.
    #[must_use]
    pub const fn with(self, provider: ProviderId) -> Self {
        Self(self.0 | Self::only(provider).0)
    }

    /// Returns whether this promise names the provider.
    #[must_use]
    pub const fn contains(self, provider: ProviderId) -> bool {
        (self.0 & Self::only(provider).0) != 0
    }

    /// Projects a non-zero big-endian wire cell into host-order provider bits.
    #[must_use]
    pub const fn from_be(bits: NonZeroU64) -> Self {
        Self(bits.get().to_be())
    }
}

impl TryFrom<u64> for ProviderSet {
    type Error = ProviderSetError;

    /// Decodes a provider set only when it preserves promise non-emptiness.
    fn try_from(bits: u64) -> Result<Self, Self::Error> {
        if bits == 0 {
            Err(ProviderSetError::Empty)
        } else {
            Ok(Self(bits))
        }
    }
}

impl From<NonZeroU64> for ProviderSet {
    /// Converts a non-zero provider bitmap after its non-emptiness proof.
    fn from(bits: NonZeroU64) -> Self {
        Self(bits.get())
    }
}

impl Deref for ProviderSet {
    type Target = u64;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

#[cfg(test)]
mod tests {
    use super::{ProviderId, ProviderSet, ProviderSetError};

    #[test]
    fn raw_provider_bits_preserve_promise_nonemptiness() {
        assert_eq!(ProviderSet::try_from(0), Err(ProviderSetError::Empty));
        let provider = ProviderId::try_from(63);
        assert_eq!(
            provider.map(ProviderSet::only).map(|set| *set),
            Ok(1_u64 << 63)
        );
        assert_eq!(
            ProviderSet::try_from(1_u64 << 63).map(|set| *set),
            Ok(1_u64 << 63)
        );
    }
}
