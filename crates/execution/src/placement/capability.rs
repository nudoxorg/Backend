//! Checked local and remote capability observations.

use crate::{AuthorityVersion, VersionedWorkIdentity, WorkKey};
use backend_version::Relation;

/// Local input/capability state relevant to placement.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LocalState {
    /// All required inputs are ready and local execution can start now.
    Ready,
    /// The local closure is incomplete.
    MissingInputs,
    /// Local capacity is currently too contended for this work.
    Overloaded,
    /// The local capability is unavailable.
    Unavailable,
}

/// Failure while admitting a local, remote, or cost observation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ObservationError {
    /// The observation window was empty or moved backwards.
    InvalidWindow,
    /// The measured values were outside checked scheduler bounds.
    InvalidMeasurement,
    /// The lower-layer verifier rejected the supplied capability evidence.
    Rejected,
}

impl std::fmt::Display for ObservationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "placement observation error: {self:?}")
    }
}

impl std::error::Error for ObservationError {}

/// A local capability observation authenticated against one exact work key.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct LocalCapability {
    key: WorkKey,
    authority: AuthorityVersion,
    state: LocalState,
    observed_at: u64,
    expires_at: u64,
}

impl LocalCapability {
    /// Admits local closure/capability evidence for one typed identity.
    ///
    /// The verifier is the engine's local closure and reservation authority;
    /// execution stores only the resulting opaque observation and its bounded
    /// validity window.
    ///
    /// # Errors
    ///
    /// Returns [`ObservationError`] when the verifier rejects the local
    /// evidence or the validity window is empty.
    pub fn admit<R: Relation, V: LocalCapabilityVerifier<R>>(
        identity: &VersionedWorkIdentity<R>,
        state: LocalState,
        observed_at: u64,
        expires_at: u64,
        verifier: &V,
    ) -> Result<Self, ObservationError> {
        if expires_at <= observed_at {
            return Err(ObservationError::InvalidWindow);
        }
        verifier
            .verify_local(identity, state)
            .map_err(|_| ObservationError::Rejected)?;
        Ok(Self {
            key: identity.work_key(),
            authority: identity.authority,
            state,
            observed_at,
            expires_at,
        })
    }

    /// Returns the local state proven by the lower-layer verifier.
    #[must_use]
    pub const fn state(&self) -> LocalState {
        self.state
    }

    /// Returns the exact work key to which this observation is bound.
    #[must_use]
    pub const fn key(&self) -> WorkKey {
        self.key
    }

    /// Returns the authority version under which local capability was checked.
    #[must_use]
    pub const fn authority(&self) -> AuthorityVersion {
        self.authority
    }

    /// Returns whether this observation is valid for the supplied key/time.
    #[must_use]
    pub fn valid_for(&self, key: WorkKey, authority: AuthorityVersion, now: u64) -> bool {
        self.key == key
            && self.authority == authority
            && now >= self.observed_at
            && now < self.expires_at
    }
}

/// Engine-owned verifier for local input closure and capability evidence.
pub trait LocalCapabilityVerifier<R: Relation> {
    /// Checks that local inputs/capability are ready for this exact identity.
    ///
    /// # Errors
    ///
    /// Returns [`ObservationError::Rejected`] when the evidence is not
    /// authorized or the requested local state is unavailable.
    fn verify_local(
        &self,
        identity: &VersionedWorkIdentity<R>,
        state: LocalState,
    ) -> Result<(), ObservationError>;
}

impl<R, F> LocalCapabilityVerifier<R> for F
where
    R: Relation,
    F: for<'a> Fn(&'a VersionedWorkIdentity<R>, LocalState) -> Result<(), ObservationError>,
{
    fn verify_local(
        &self,
        identity: &VersionedWorkIdentity<R>,
        state: LocalState,
    ) -> Result<(), ObservationError> {
        self(identity, state)
    }
}

/// Remote capability/health state observed by the scheduler.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RemoteState {
    /// No compatible authorized worker is available.
    Unavailable,
    /// A compatible worker is available but cold.
    Available,
    /// A compatible worker and required arrangement/session are warm.
    Warm,
}

/// A negotiated remote capability observation bound to one exact work key.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RemoteCapability {
    key: WorkKey,
    authority: AuthorityVersion,
    state: RemoteState,
    observed_at: u64,
    expires_at: u64,
}

impl RemoteCapability {
    /// Admits an authenticated replication capability envelope for one typed
    /// identity. The verifier must check the exact recipe/schema/authority
    /// intersection; execution records only the resulting state and window.
    ///
    /// # Errors
    ///
    /// Returns [`ObservationError`] when the verifier rejects the negotiated
    /// envelope or the validity window is empty.
    pub fn admit<R: Relation, V: RemoteCapabilityVerifier<R>>(
        identity: &VersionedWorkIdentity<R>,
        capabilities: &backend_replication::NegotiatedCapabilities,
        observed_at: u64,
        expires_at: u64,
        verifier: &V,
    ) -> Result<Self, ObservationError> {
        if expires_at <= observed_at {
            return Err(ObservationError::InvalidWindow);
        }
        let state = verifier
            .verify_remote(identity, capabilities)
            .map_err(|_| ObservationError::Rejected)?;
        Ok(Self {
            key: identity.work_key(),
            authority: identity.authority,
            state,
            observed_at,
            expires_at,
        })
    }

    /// Returns the state proven by the remote capability verifier.
    #[must_use]
    pub const fn state(&self) -> RemoteState {
        self.state
    }

    /// Returns the exact work key to which this observation is bound.
    #[must_use]
    pub const fn key(&self) -> WorkKey {
        self.key
    }

    /// Returns the authority version under which remote capability was checked.
    #[must_use]
    pub const fn authority(&self) -> AuthorityVersion {
        self.authority
    }

    /// Returns whether this observation is valid for the supplied key/time.
    #[must_use]
    pub fn valid_for(&self, key: WorkKey, authority: AuthorityVersion, now: u64) -> bool {
        self.key == key
            && self.authority == authority
            && now >= self.observed_at
            && now < self.expires_at
    }
}

/// Engine-owned verifier for an authenticated replication capability envelope.
pub trait RemoteCapabilityVerifier<R: Relation> {
    /// Checks exact recipe/schema/data/authority compatibility and returns the
    /// usable remote state.
    ///
    /// # Errors
    ///
    /// Returns [`ObservationError::Rejected`] when the peer cannot execute the
    /// exact requested work under the expected authority.
    fn verify_remote(
        &self,
        identity: &VersionedWorkIdentity<R>,
        capabilities: &backend_replication::NegotiatedCapabilities,
    ) -> Result<RemoteState, ObservationError>;
}

impl<R, F> RemoteCapabilityVerifier<R> for F
where
    R: Relation,
    F: for<'a> Fn(
        &'a VersionedWorkIdentity<R>,
        &'a backend_replication::NegotiatedCapabilities,
    ) -> Result<RemoteState, ObservationError>,
{
    fn verify_remote(
        &self,
        identity: &VersionedWorkIdentity<R>,
        capabilities: &backend_replication::NegotiatedCapabilities,
    ) -> Result<RemoteState, ObservationError> {
        self(identity, capabilities)
    }
}

impl RemoteState {
    /// Returns whether remote dispatch is possible for this observation.
    #[must_use]
    pub const fn is_available(self) -> bool {
        !matches!(self, Self::Unavailable)
    }

    /// Returns whether the worker has reusable warm state.
    #[must_use]
    pub const fn is_warm(self) -> bool {
        matches!(self, Self::Warm)
    }
}

/// Converts a negotiated replication capability envelope into a conservative
/// placement observation. An empty recipe or schema set cannot dispatch any
/// work; a non-empty set is merely an availability signal and the caller must
/// still compare the exact recipe, input, authority, and output contract.
#[must_use]
pub fn remote_state_from_capabilities(
    capabilities: &backend_replication::NegotiatedCapabilities,
) -> RemoteState {
    if capabilities.schemas.is_empty() || capabilities.recipes.is_empty() {
        RemoteState::Unavailable
    } else {
        RemoteState::Available
    }
}
