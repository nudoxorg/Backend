//! Constructors and accessors for untrusted semantic coverage claims.

use super::{
    AuthorityEpoch, ExecutionScopeId, ObjectVersion, Relation, RevocationVersion,
    SemanticCoverageAdmissionError, SemanticCoverageSchema, SemanticCoverageState,
    UntrustedSemanticCoverageClaim, WireAuthority, WireIdentity,
};

impl UntrustedSemanticCoverageClaim {
    /// Creates an untrusted claim with a partial state.
    ///
    /// New claims default to partial so an omitted state can never turn into a
    /// complete publication capability by accident.
    #[must_use]
    pub fn new<R: Relation>(
        identity: &backend_execution::VersionedWorkIdentity<R>,
        scope: u64,
        witness: impl Into<Box<[u8]>>,
        authority_epoch: AuthorityEpoch,
        revocation_version: RevocationVersion,
    ) -> Self {
        Self::with_state(
            identity,
            scope,
            witness,
            authority_epoch,
            revocation_version,
            SemanticCoverageState::Partial,
        )
    }

    /// Creates a producer claim that asserts complete coverage.
    ///
    /// The result remains untrusted.  The explicit name makes it clear that
    /// this is suitable for parsing hostile producer input and is not the
    /// capability consumed by completion APIs.
    #[must_use]
    pub fn complete_claim<R: Relation>(
        identity: &backend_execution::VersionedWorkIdentity<R>,
        scope: u64,
        witness: impl Into<Box<[u8]>>,
        authority_epoch: AuthorityEpoch,
        revocation_version: RevocationVersion,
    ) -> Self {
        Self::with_state(
            identity,
            scope,
            witness,
            authority_epoch,
            revocation_version,
            SemanticCoverageState::Complete,
        )
    }

    /// Creates a partial producer claim.
    #[must_use]
    pub fn partial<R: Relation>(
        identity: &backend_execution::VersionedWorkIdentity<R>,
        scope: u64,
        witness: impl Into<Box<[u8]>>,
        authority_epoch: AuthorityEpoch,
        revocation_version: RevocationVersion,
    ) -> Self {
        Self::with_state(
            identity,
            scope,
            witness,
            authority_epoch,
            revocation_version,
            SemanticCoverageState::Partial,
        )
    }

    /// Creates an unavailable producer claim.
    #[must_use]
    pub fn unavailable<R: Relation>(
        identity: &backend_execution::VersionedWorkIdentity<R>,
        scope: u64,
        witness: impl Into<Box<[u8]>>,
        authority_epoch: AuthorityEpoch,
        revocation_version: RevocationVersion,
    ) -> Self {
        Self::with_state(
            identity,
            scope,
            witness,
            authority_epoch,
            revocation_version,
            SemanticCoverageState::Unavailable,
        )
    }

    /// Creates an unsupported producer claim.
    #[must_use]
    pub fn unsupported<R: Relation>(
        identity: &backend_execution::VersionedWorkIdentity<R>,
        scope: u64,
        witness: impl Into<Box<[u8]>>,
        authority_epoch: AuthorityEpoch,
        revocation_version: RevocationVersion,
    ) -> Self {
        Self::with_state(
            identity,
            scope,
            witness,
            authority_epoch,
            revocation_version,
            SemanticCoverageState::Unsupported,
        )
    }

    /// Creates a raw claim with an explicitly asserted state.
    ///
    /// This constructor is intentionally untrusted.  It is useful at wire
    /// and producer seams where the state is supplied by another authority.
    #[must_use]
    pub fn with_state<R: Relation>(
        identity: &backend_execution::VersionedWorkIdentity<R>,
        scope: u64,
        witness: impl Into<Box<[u8]>>,
        authority_epoch: AuthorityEpoch,
        revocation_version: RevocationVersion,
        state: SemanticCoverageState,
    ) -> Self {
        let witness = witness.into();
        let mut material = Vec::new();
        material.extend_from_slice(b"backend.engine.semantic-coverage.v1\0");
        material.extend_from_slice(&identity.work_key().to_bytes());
        material.extend_from_slice(&identity.read_manifest.to_bytes());
        material.extend_from_slice(&identity.authority.to_bytes());
        material.extend_from_slice(&scope.to_be_bytes());
        material.extend_from_slice(&authority_epoch.0.to_be_bytes());
        material.extend_from_slice(&revocation_version.0.to_be_bytes());
        material.push(state as u8);
        material.extend_from_slice(&(witness.len() as u64).to_be_bytes());
        material.extend_from_slice(&witness);
        Self {
            identity: ObjectVersion::from_value(material.as_slice()),
            scope,
            read_manifest: identity.read_manifest,
            authority: identity.authority,
            authority_epoch,
            revocation_version,
            witness,
            state,
        }
    }

    /// Returns the typed witness identity.
    #[must_use]
    pub const fn identity(&self) -> ObjectVersion<SemanticCoverageSchema> {
        self.identity
    }

    /// Returns the requested semantic scope.
    #[must_use]
    pub const fn scope(&self) -> u64 {
        self.scope
    }

    /// Returns the exact read-manifest identity.
    #[must_use]
    pub const fn read_manifest(&self) -> backend_execution::ReadManifestId {
        self.read_manifest
    }

    /// Returns the authority revision.
    #[must_use]
    pub const fn authority(&self) -> backend_execution::AuthorityVersion {
        self.authority
    }

    /// Returns the authority epoch.
    #[must_use]
    pub const fn authority_epoch(&self) -> AuthorityEpoch {
        self.authority_epoch
    }

    /// Returns the revocation observation.
    #[must_use]
    pub const fn revocation_version(&self) -> RevocationVersion {
        self.revocation_version
    }

    /// Returns the producer's asserted coverage state.
    #[must_use]
    pub const fn state(&self) -> SemanticCoverageState {
        self.state
    }

    /// Returns producer evidence bytes.
    #[must_use]
    pub fn witness(&self) -> &[u8] {
        &self.witness
    }

    /// Returns whether the producer asserted complete semantic coverage.
    ///
    /// This is deliberately only an assertion on an untrusted claim.  It must
    /// never be used as a publication gate; completion APIs accept only
    /// [`CompleteSemanticCoverage`].
    #[must_use]
    pub const fn asserted_complete(&self) -> bool {
        self.state.is_complete()
    }

    /// Compatibility spelling for the producer's completeness assertion.
    ///
    /// This remains an untrusted claim and cannot be passed to a completion
    /// API; use [`CompleteSemanticCoverage::is_complete`] for an admitted
    /// capability.
    #[must_use]
    pub const fn is_complete(&self) -> bool {
        self.asserted_complete()
    }

    /// Checks that this witness is bound to one exact scheduled identity.
    #[must_use]
    pub fn binds<R: Relation>(
        &self,
        identity: &backend_execution::VersionedWorkIdentity<R>,
    ) -> bool {
        self.scope != 0
            && self.read_manifest == identity.read_manifest
            && self.authority == identity.authority
            && self.identity
                == Self::with_state(
                    identity,
                    self.scope,
                    self.witness.clone(),
                    self.authority_epoch,
                    self.revocation_version,
                    self.state,
                )
                .identity
    }

    /// Converts this witness into an untrusted wire claim.
    ///
    /// # Errors
    ///
    /// Returns [`SemanticCoverageAdmissionError::InvalidScope`] when the
    /// producer supplied the reserved zero legacy scope.
    pub fn wire(
        &self,
    ) -> Result<backend_replication::WireSemanticCoverage, SemanticCoverageAdmissionError> {
        let scope = ExecutionScopeId::try_from(self.scope)
            .map_err(|_| SemanticCoverageAdmissionError::InvalidScope)?;
        Ok(backend_replication::WireSemanticCoverage {
            identity: WireIdentity::from_typed(&self.identity),
            scope,
            read_manifest: WireIdentity::from_typed(&self.read_manifest),
            authority: WireAuthority::from_typed(&self.authority, self.authority_epoch),
        })
    }
}
