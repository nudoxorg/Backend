//! Producer observation and complete-scope admission for compiled profiles.

use crate::workspace::WorkspaceSnapshot;
use backend_version::{
    AdmittedProducerObservation, AuthorityScopeClaim, CoverageWitness, ObjectVersion,
    ProducerObservationClaims, ProducerObservationVerifier, Schema, ScopeRoot,
    UntrustedProducerObservation, WorkspaceRoot, admit_complete_scope, admit_producer_observation,
};
use std::sync::Arc;

/// Authority-bound producer admission for a workspace-backed view.
///
/// The producer identity commits to the selected workspace root and source
/// object. Context and evidence retain the checked commit and manifest, so a
/// caller cannot mint an observation from a bare matching digest.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WorkspaceViewProducerAdmission {
    workspace: WorkspaceRoot,
    source: ScopeRoot,
    identity: [u8; backend_version::ID_BYTES],
    context: [u8; backend_version::ID_BYTES],
    evidence: Arc<[u8]>,
}

impl WorkspaceViewProducerAdmission {
    /// Creates a verifier from an actually admitted workspace head and source
    /// object.
    #[must_use]
    pub fn from_snapshot(
        snapshot: &WorkspaceSnapshot,
        source: backend_library::SemanticObject,
    ) -> Self {
        let workspace = snapshot.root();
        let context = snapshot.commit().id().to_bytes();
        let commit_bytes = snapshot.commit().encode();
        let mut evidence_bytes = Vec::with_capacity(64 + commit_bytes.len());
        evidence_bytes.extend_from_slice(b"WVP1");
        evidence_bytes.extend_from_slice(&snapshot.manifest().encode());
        evidence_bytes.extend_from_slice(&commit_bytes);
        let evidence: Arc<[u8]> = Arc::from(evidence_bytes);
        let source_scope = ScopeRoot::from_bytes(source.to_bytes());
        let mut preimage = Vec::with_capacity(160 + evidence.len());
        preimage.extend_from_slice(b"backend.workspace.view-producer.v1\0");
        preimage.extend_from_slice(workspace.as_bytes());
        preimage.extend_from_slice(source.as_bytes());
        preimage.extend_from_slice(&context);
        preimage.extend_from_slice(&evidence);
        let identity = *blake3::hash(&preimage).as_bytes();
        Self {
            workspace,
            source: source_scope,
            identity,
            context,
            evidence,
        }
    }

    /// Creates the bounded observation emitted by this admitted workspace
    /// producer.
    #[must_use]
    pub fn observation(&self) -> UntrustedProducerObservation {
        UntrustedProducerObservation::new(
            self.identity,
            self.source,
            self.context,
            self.evidence.to_vec(),
        )
    }

    /// Admits the producer observation through this verifier.
    /// # Errors
    ///
    /// Returns an error when validation, persistence, or admission of the
    /// supplied value fails.
    pub fn admit(&self) -> Result<AdmittedProducerObservation, String> {
        admit_producer_observation(self.observation(), self).map_err(|error| error.to_string())
    }

    /// Returns the workspace root bound to this producer session.
    #[must_use]
    pub const fn workspace_root(&self) -> WorkspaceRoot {
        self.workspace
    }
}

impl ProducerObservationVerifier for WorkspaceViewProducerAdmission {
    type Error = &'static str;

    fn verify(
        &self,
        observation: &UntrustedProducerObservation,
    ) -> Result<ProducerObservationClaims, Self::Error> {
        let expected = self.observation();
        if observation == &expected {
            Ok(ProducerObservationClaims::new(
                expected.producer_identity(),
                expected.scope_root(),
                expected.context(),
                *blake3::hash(expected.evidence()).as_bytes(),
            ))
        } else {
            Err("workspace producer observation does not match admitted source")
        }
    }
}

/// The owner-side verifier for the compiled builtin's complete scope.
///
/// All four observation fields are derived from the authority version and are
/// compared before the lower version crate mints its opaque admission token.
struct BuiltinCoverageVerifier<T: Schema> {
    authority: ObjectVersion<T>,
}

impl<T: Schema> BuiltinCoverageVerifier<T> {
    fn scope(&self) -> ScopeRoot {
        AuthorityScopeClaim::from_object_version(self.authority).scope_root()
    }

    fn context(&self) -> [u8; backend_version::ID_BYTES] {
        let mut bytes = Vec::with_capacity(64);
        bytes.extend_from_slice(b"backend.engine.builtin.coverage.context.v1\0");
        bytes.extend_from_slice(&self.authority.to_bytes());
        *blake3::hash(&bytes).as_bytes()
    }

    fn identity(&self) -> [u8; backend_version::ID_BYTES] {
        let mut bytes = Vec::with_capacity(96);
        bytes.extend_from_slice(b"backend.engine.builtin.coverage.producer.v1\0");
        bytes.extend_from_slice(&self.authority.to_bytes());
        bytes.extend_from_slice(self.scope().as_bytes());
        *blake3::hash(&bytes).as_bytes()
    }

    fn observation(&self) -> UntrustedProducerObservation {
        let scope = self.scope();
        let context = self.context();
        let mut evidence = Vec::with_capacity(128);
        evidence.extend_from_slice(b"backend.engine.builtin.coverage.evidence.v1\0");
        evidence.extend_from_slice(&self.authority.to_bytes());
        evidence.extend_from_slice(scope.as_bytes());
        evidence.extend_from_slice(&context);
        UntrustedProducerObservation::new(self.identity(), scope, context, evidence)
    }
}

impl<T: Schema> ProducerObservationVerifier for BuiltinCoverageVerifier<T> {
    type Error = &'static str;

    fn verify(
        &self,
        observation: &UntrustedProducerObservation,
    ) -> Result<ProducerObservationClaims, Self::Error> {
        let expected = self.observation();
        if observation.producer_identity() == expected.producer_identity()
            && observation.scope_root() == expected.scope_root()
            && observation.context() == expected.context()
            && observation.evidence() == expected.evidence()
        {
            Ok(ProducerObservationClaims::new(
                expected.producer_identity(),
                expected.scope_root(),
                expected.context(),
                *blake3::hash(expected.evidence()).as_bytes(),
            ))
        } else {
            Err("builtin authority observation mismatch")
        }
    }
}

pub(crate) fn authorized_coverage<T: Schema>(
    authority: ObjectVersion<T>,
) -> Result<backend_version::AuthorizedCompleteCoverage, String> {
    let declaration = AuthorityScopeClaim::from_object_version(authority);
    let verifier = BuiltinCoverageVerifier { authority };
    let observation = admit_producer_observation(verifier.observation(), &verifier)
        .map_err(|error| error.to_string())?;
    admit_complete_scope(declaration, observation).map_err(|error| error.to_string())
}

pub(crate) fn complete_coverage<T: Schema>(
    authority: ObjectVersion<T>,
) -> Result<CoverageWitness, String> {
    authorized_coverage(authority).map(CoverageWitness::Complete)
}

/// Derives complete coverage from an authority capability admitted by the
/// authenticated worker session.
///
/// A bare object version is insufficient: callers must retain the opaque
/// capability produced by authority-policy admission, and its wire identity
/// must bind the same typed authority version.
/// # Errors
///
/// Returns an error when the session capability belongs to another authority
/// or when complete-scope evidence admission fails.
pub fn coverage_from_admitted_authority<T: Schema>(
    authority: ObjectVersion<T>,
    admitted: &backend_replication::AdmittedAuthority,
) -> Result<CoverageWitness, String> {
    if admitted.claim().id.as_bytes() != authority.to_bytes() {
        return Err("admitted authority identity mismatch".to_owned());
    }
    complete_coverage(authority)
}
