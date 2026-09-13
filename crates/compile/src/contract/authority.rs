//! Authority identities, admission fences, and the frontend contract.

use super::{
    AuthorityEpoch, AuthorityEpochSchema, CommandId, ContractId, DiscoverySnapshot, Extraction,
    FactEvidence, FactKeySchema, FactRecord, FactValueSchema, InputManifest, InputManifestId,
    ProducerId, SessionId, SessionKey, SessionSchema, ToolchainId, typed_of,
};
use backend_version::{
    AuthorityScopeClaim, AuthorizedCompleteCoverage, CoverageWitness, ProducerObservationClaims,
    ProducerObservationVerifier, Schema, ScopeRoot, UntrustedProducerObservation,
    admit_complete_scope, admit_producer_observation,
};
use std::{fmt, sync::Arc};

/// Immutable authority identity supplied by a concrete frontend.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AuthorityIdentity {
    /// Producer implementation identity.
    pub producer: ProducerId,
    /// External tool or runtime identity.
    pub toolchain: ToolchainId,
    /// Protocol and semantic contract identity.
    pub contract: ContractId,
}

impl AuthorityIdentity {
    /// Computes the composite authority identity.
    #[must_use]
    pub fn digest(&self) -> SessionId {
        let mut bytes = Vec::with_capacity(96);
        bytes.extend_from_slice(self.producer.as_bytes());
        bytes.extend_from_slice(self.toolchain.as_bytes());
        bytes.extend_from_slice(self.contract.as_bytes());
        typed_of::<SessionSchema>(&bytes)
    }
}

/// Private fence carried by a complete authority result.
///
/// A backend-version `AuthorizedCompleteCoverage` proves exact scope equality
/// and producer provenance. The compile boundary additionally retains the
/// authority, immutable command, toolchain, revision, and registration epoch
/// that were checked before the coverage token was minted.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct AuthorityFence {
    pub(super) authority: AuthorityIdentity,
    pub(super) manifest: InputManifestId,
    pub(super) revision: u64,
    pub(super) toolchain: ToolchainId,
    pub(super) epoch: AuthorityEpoch,
    pub(super) command: CommandId,
    pub(super) coverage: AuthorizedCompleteCoverage,
}

impl AuthorityFence {
    pub(super) fn evidence(self) -> FactEvidence {
        FactEvidence {
            authority: self.authority.digest(),
            manifest: self.manifest,
            scope: ScopeRoot::from_bytes(self.manifest.to_bytes()),
            revision: self.revision,
            epoch: self.epoch,
        }
    }

    pub(super) fn witness(self) -> CoverageWitness {
        CoverageWitness::Complete(self.coverage)
    }
}

/// A complete coverage capability minted by the checked native authority
/// admission path.
///
/// The type has no public constructor. It is retained by complete snapshots
/// and extractions so a complete coverage label cannot be detached from the
/// command, toolchain, revision, and authority epoch that justified it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct CompleteAuthorityCoverage {
    pub(super) fence: Arc<AuthorityFence>,
}

impl CompleteAuthorityCoverage {
    fn new(fence: &AuthorityFence) -> Self {
        Self {
            fence: Arc::new(*fence),
        }
    }

    pub(super) fn fence(&self) -> &Arc<AuthorityFence> {
        &self.fence
    }
}

impl ProducerObservationVerifier for AuthorityRegistry {
    type Error = AuthorityAdmissionError;

    fn verify(
        &self,
        observation: &UntrustedProducerObservation,
    ) -> Result<ProducerObservationClaims, Self::Error> {
        let expected_scope = ScopeRoot::from_bytes(self.manifest.to_bytes());
        if observation.producer_identity() != self.authority.digest().to_bytes()
            || observation.scope_root() != expected_scope
            || observation.context() != self.command.to_bytes()
            || observation.evidence() != self.evidence_bytes()
        {
            return Err(AuthorityAdmissionError::EvidenceMismatch);
        }
        Ok(ProducerObservationClaims::new(
            self.authority.digest().to_bytes(),
            expected_scope,
            self.command.to_bytes(),
            *blake3::hash(&self.evidence_bytes()).as_bytes(),
        ))
    }
}

/// Admission failures for complete native authority results.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum AuthorityAdmissionError {
    /// The command did not carry the expected authority binding.
    CommandMismatch,
    /// The command executable drifted or could not be verified.
    Process(crate::ProcessError),
    /// The manifest lacks a positive source and an explicit negative/range
    /// boundary, so omitted records cannot be interpreted as deletions.
    OpenManifest,
    /// A complete native result must contain at least one typed fact.
    EmptyFacts,
    /// A typed fact carried evidence for another authority fence.
    EvidenceMismatch,
    /// A fact key was empty and therefore cannot identify a semantic object.
    EmptyFactKey,
    /// The authority response did not match its registered request fence.
    PayloadMismatch,
}

impl fmt::Display for AuthorityAdmissionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::CommandMismatch => formatter.write_str("native command authority fence mismatch"),
            Self::Process(error) => write!(formatter, "native command verification failed: {error}"),
            Self::OpenManifest => formatter.write_str(
                "complete authority manifest must include a source and a negative or range boundary",
            ),
            Self::EmptyFacts => formatter.write_str("complete authority result contains no typed facts"),
            Self::EvidenceMismatch => formatter.write_str("typed fact evidence does not match authority fence"),
            Self::EmptyFactKey => formatter.write_str("complete authority result contains an empty fact key"),
            Self::PayloadMismatch => formatter.write_str("native payload does not match authority fence"),
        }
    }
}

impl std::error::Error for AuthorityAdmissionError {}

/// Independent registry for one immutable authority execution fence.
///
/// A registry is created only after the native command, executable identity,
/// toolchain binding, manifest shape, and revision are checked. It mints a
/// private complete capability only after a non-empty typed result is checked
/// against the same fence.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct AuthorityRegistry {
    authority: AuthorityIdentity,
    manifest: InputManifestId,
    manifest_data: Arc<InputManifest>,
    revision: u64,
    toolchain: ToolchainId,
    epoch: AuthorityEpoch,
    command: CommandId,
}

impl AuthorityRegistry {
    #[cfg(test)]
    pub(crate) fn for_test(authority: AuthorityIdentity, snapshot: &DiscoverySnapshot) -> Self {
        let command = typed_of::<super::CommandSchema>(b"backend-compile-test-command");
        Self {
            authority,
            manifest: snapshot.manifest().digest(),
            manifest_data: Arc::new(snapshot.manifest().clone()),
            revision: snapshot.sequence(),
            toolchain: authority.toolchain,
            epoch: authority_epoch(authority.digest(), snapshot.manifest().digest(), command),
            command,
        }
    }

    pub(crate) fn for_native(
        authority: AuthorityIdentity,
        snapshot: &DiscoverySnapshot,
        command: &crate::SupervisedCommand,
    ) -> Result<Self, AuthorityAdmissionError> {
        if command.toolchain() != Some(authority.toolchain)
            || command.session_key().is_none_or(|key| {
                !key.matches(&authority, snapshot.manifest())
                    || key.manifest() != snapshot.manifest().digest()
            })
            || !command.protocol().is_supported()
            || command.executable_identity().is_none()
        {
            return Err(AuthorityAdmissionError::CommandMismatch);
        }
        command
            .verify_executable()
            .map_err(AuthorityAdmissionError::Process)?;
        if !snapshot.manifest().has_present_source() || !snapshot.manifest().has_closed_boundary() {
            return Err(AuthorityAdmissionError::OpenManifest);
        }
        let command_id = command.identity();
        let epoch = authority_epoch(authority.digest(), snapshot.manifest().digest(), command_id);
        Ok(Self {
            authority,
            manifest: snapshot.manifest().digest(),
            manifest_data: Arc::new(snapshot.manifest().clone()),
            revision: snapshot.sequence(),
            toolchain: authority.toolchain,
            epoch,
            command: command_id,
        })
    }

    pub(crate) const fn authority(&self) -> AuthorityIdentity {
        self.authority
    }

    pub(crate) const fn manifest(&self) -> InputManifestId {
        self.manifest
    }

    pub(crate) fn manifest_ref(&self) -> &InputManifest {
        self.manifest_data.as_ref()
    }

    pub(crate) const fn revision(&self) -> u64 {
        self.revision
    }

    fn evidence_bytes(&self) -> Vec<u8> {
        let mut evidence = Vec::with_capacity(32 * 4 + 8);
        evidence.extend_from_slice(&self.authority.digest().to_bytes());
        evidence.extend_from_slice(&self.toolchain.to_bytes());
        evidence.extend_from_slice(&self.manifest.to_bytes());
        evidence.extend_from_slice(&self.revision.to_be_bytes());
        evidence.extend_from_slice(&self.epoch.to_bytes());
        evidence.extend_from_slice(&self.command.to_bytes());
        evidence
    }

    pub(crate) fn admit_complete_coverage(
        &self,
    ) -> Result<AuthorizedCompleteCoverage, AuthorityAdmissionError> {
        let declaration = AuthorityScopeClaim::from_object_version(self.manifest);
        let observation = UntrustedProducerObservation::new(
            self.authority.digest().to_bytes(),
            ScopeRoot::from_bytes(self.manifest.to_bytes()),
            self.command.to_bytes(),
            self.evidence_bytes(),
        );
        let admitted = admit_producer_observation(observation, self)
            .map_err(|_| AuthorityAdmissionError::EvidenceMismatch)?;
        admit_complete_scope(declaration, admitted)
            .map_err(|_| AuthorityAdmissionError::PayloadMismatch)
    }

    pub(crate) fn complete_records<K: Schema<Value = [u8]>, V: Schema<Value = [u8]>>(
        &self,
        mut records: Vec<FactRecord<K, V>>,
    ) -> Result<(CompleteAuthorityCoverage, Vec<FactRecord<K, V>>), AuthorityAdmissionError> {
        if records.is_empty() {
            return Err(AuthorityAdmissionError::EmptyFacts);
        }
        let evidence = FactEvidence {
            authority: self.authority.digest(),
            manifest: self.manifest,
            scope: ScopeRoot::from_bytes(self.manifest.to_bytes()),
            revision: self.revision,
            epoch: self.epoch,
        };
        for record in &mut records {
            if record.key_bytes.is_empty() {
                return Err(AuthorityAdmissionError::EmptyFactKey);
            }
            record.evidence = match record.evidence {
                None => Some(evidence),
                Some(existing)
                    if existing.authority == evidence.authority
                        && existing.manifest == evidence.manifest
                        && existing.scope == evidence.scope
                        && existing.epoch == evidence.epoch
                        && existing.revision <= evidence.revision =>
                {
                    Some(existing)
                }
                Some(_) => return Err(AuthorityAdmissionError::EvidenceMismatch),
            };
        }
        records.sort_by(|left, right| {
            (left.kind, left.key_bytes.as_slice()).cmp(&(right.kind, right.key_bytes.as_slice()))
        });
        if records
            .windows(2)
            .any(|window| window[0].kind == window[1].kind && window[0].key == window[1].key)
        {
            return Err(AuthorityAdmissionError::EvidenceMismatch);
        }
        let coverage = self.admit_complete_coverage()?;
        let fence = AuthorityFence {
            authority: self.authority,
            manifest: self.manifest,
            revision: self.revision,
            toolchain: self.toolchain,
            epoch: self.epoch,
            command: self.command,
            coverage,
        };
        Ok((CompleteAuthorityCoverage::new(&fence), records))
    }

    pub(crate) fn admit_native_records(
        &self,
        key: SessionKey,
        envelope: &crate::NativeEnvelope<crate::Bound>,
        records: Vec<FactRecord<FactKeySchema, FactValueSchema>>,
    ) -> Result<Extraction, AuthorityError> {
        if !matches!(envelope.coverage(), crate::NativeCoverage::Complete)
            || envelope.session() != key.digest()
            || envelope.manifest() != self.manifest
            || envelope.authority() != self.authority.digest()
            || envelope.revision() != self.revision
            || key.manifest() != self.manifest
            || key.authority() != self.authority.digest()
        {
            return Err(AuthorityError::Extraction(
                AuthorityAdmissionError::PayloadMismatch.to_string(),
            ));
        }
        let (capability, records) = self
            .complete_records(records)
            .map_err(|error| AuthorityError::Extraction(error.to_string()))?;
        Extraction::from_complete_capability(capability, records)
            .map_err(|error| AuthorityError::Extraction(error.to_string()))
    }
}

fn authority_epoch(
    authority: SessionId,
    manifest: InputManifestId,
    command: CommandId,
) -> AuthorityEpoch {
    let mut bytes = Vec::with_capacity(32 * 3);
    bytes.extend_from_slice(authority.as_bytes());
    bytes.extend_from_slice(manifest.as_bytes());
    bytes.extend_from_slice(command.as_bytes());
    typed_of::<AuthorityEpochSchema>(&bytes)
}

/// A frontend-facing authority contract.  Concrete frontends implement this
/// trait in their own crates.
pub trait Authority {
    /// Returns the producer, toolchain, and protocol identity.
    fn identity(&self) -> AuthorityIdentity;
    /// Discovers the complete input manifest for a request.
    ///
    /// # Errors
    ///
    /// Returns [`AuthorityError`] when discovery cannot produce a checked
    /// manifest snapshot.
    fn discover(&self) -> Result<DiscoverySnapshot, AuthorityError>;
    /// Extracts facts for an already-discovered snapshot and session key.
    ///
    /// # Errors
    ///
    /// Returns [`AuthorityError::InvalidSessionKey`] for a mismatched key, or
    /// another [`AuthorityError`] when extraction fails.
    fn extract(
        &self,
        snapshot: &DiscoverySnapshot,
        key: SessionKey,
    ) -> Result<Extraction, AuthorityError>;
}

/// Typed authority boundary failures.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum AuthorityError {
    /// Discovery failed before a complete manifest was produced.
    Discovery(String),
    /// The supplied session key did not match the authority request.
    InvalidSessionKey,
    /// Fact extraction failed.
    Extraction(String),
    /// A supervised authority process failed.
    Process(crate::ProcessError),
}

impl fmt::Display for AuthorityError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Discovery(message) => write!(f, "authority discovery failed: {message}"),
            Self::InvalidSessionKey => f.write_str("authority session key is invalid"),
            Self::Extraction(message) => write!(f, "authority extraction failed: {message}"),
            Self::Process(error) => write!(f, "authority process failed: {error}"),
        }
    }
}

impl std::error::Error for AuthorityError {}

impl From<crate::ProcessError> for AuthorityError {
    fn from(error: crate::ProcessError) -> Self {
        Self::Process(error)
    }
}

impl From<crate::NativeRunnerError> for AuthorityError {
    fn from(error: crate::NativeRunnerError) -> Self {
        match error {
            crate::NativeRunnerError::Process(error) => Self::Process(error),
            other => Self::Extraction(other.to_string()),
        }
    }
}
