//! Coverage scope algebra for authoritative and incomplete observations.
//!
//! Version owns scope equality and the handoff from an admitted authority
//! producer. A canonical object version yields a claim; only an authority
//! producer session can turn an independently observed scope into an
//! authoritative complete witness.

use core::fmt;

use crate::ids::{ID_BYTES, ObjectVersion, Schema};

mod witness;

/// Complete or partial coverage state.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Coverage {
    /// The declared scope was completely observed.
    Complete,
    /// The authority returned a bounded subset of the declared scope.
    Partial,
    /// The authority could not provide the requested scope.
    Unavailable,
    /// The authority does not implement the requested operation.
    Unsupported,
    /// The relation is closed over a complete local materialization, without
    /// an authority producer claim. This is sufficient for local delta
    /// algebra but cannot authorize workspace publication.
    Closed,
}

impl Coverage {
    /// Returns whether this state is complete.
    #[must_use]
    pub const fn is_complete(self) -> bool {
        matches!(self, Self::Complete)
    }

    /// Returns whether this state is a complete closed-world relation view.
    #[must_use]
    pub const fn is_closed(self) -> bool {
        matches!(self, Self::Closed)
    }
}

/// Exact equality binding between a claimed and observed scope.
///
/// This type proves only the version algebra `claimed == observed`; it does
/// not attest which authority produced the observation. A higher authority
/// layer must retain its own producer provenance alongside this binding.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ScopeEqualityBinding {
    scope: ScopeRoot,
}

/// Untrusted observation output from an authority producer/session.
///
/// The producer identity, session/epoch context, scope, and evidence are
/// carried together so an authority verifier can check the complete output as
/// one record. This type alone cannot enter authoritative coverage.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct UntrustedProducerObservation {
    producer: [u8; ID_BYTES],
    scope: ScopeRoot,
    context: [u8; ID_BYTES],
    evidence: Vec<u8>,
}

impl UntrustedProducerObservation {
    /// Creates an untrusted producer result for authority-layer verification.
    #[must_use]
    pub fn new(
        producer: [u8; ID_BYTES],
        scope: ScopeRoot,
        context: [u8; ID_BYTES],
        evidence: Vec<u8>,
    ) -> Self {
        Self {
            producer,
            scope,
            context,
            evidence,
        }
    }

    /// Returns the producer identity claim.
    #[must_use]
    pub const fn producer_identity(&self) -> [u8; ID_BYTES] {
        self.producer
    }

    /// Returns the observed scope claim.
    #[must_use]
    pub const fn scope_root(&self) -> ScopeRoot {
        self.scope
    }

    /// Returns the session/epoch/source context claim.
    #[must_use]
    pub const fn context(&self) -> [u8; ID_BYTES] {
        self.context
    }

    /// Returns the producer's opaque evidence bytes for verification.
    #[must_use]
    pub fn evidence(&self) -> &[u8] {
        &self.evidence
    }
}

/// Authority-layer verifier for one complete producer/session observation.
///
/// The authority crate implements this for its already-admitted session or
/// protocol evidence. Version never accepts a raw scope, digest equality, or
/// caller-created wrapper as the verifier result.
pub trait ProducerObservationVerifier {
    /// Verifier-specific rejection detail.
    type Error;

    /// Checks producer identity, session/epoch/source context, scope, and
    /// evidence as one independently admitted authority result.
    ///
    /// # Errors
    ///
    /// Returns the verifier's authority-specific rejection when any part of
    /// the observation is not backed by an admitted session.
    fn verify(
        &self,
        observation: &UntrustedProducerObservation,
    ) -> Result<ProducerObservationClaims, Self::Error>;
}

/// Claims independently established by a producer verifier.
///
/// This is deliberately not an admission receipt. The verifier reports only
/// what its authority evidence established; `backend-version` compares every
/// claim with the observed record before constructing opaque admitted state.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ProducerObservationClaims {
    producer: [u8; ID_BYTES],
    scope: ScopeRoot,
    context: [u8; ID_BYTES],
    evidence_digest: [u8; ID_BYTES],
}

impl ProducerObservationClaims {
    /// Records the independently established producer observation facts.
    #[must_use]
    pub const fn new(
        producer: [u8; ID_BYTES],
        scope: ScopeRoot,
        context: [u8; ID_BYTES],
        evidence_digest: [u8; ID_BYTES],
    ) -> Self {
        Self {
            producer,
            scope,
            context,
            evidence_digest,
        }
    }
}

/// Opaque version-owned observation admitted by an authority verifier.
///
/// Its private fields ensure that complete coverage can only use evidence
/// returned by [`admit_producer_observation`].
///
/// This capability cannot be reconstructed by callers from raw fields:
///
/// ```compile_fail
/// use backend_version::{AdmittedProducerObservation, ProducerObservationIdentity, ScopeRoot};
/// let _ = AdmittedProducerObservation {
///     scope: ScopeRoot::from_u64(1),
///     identity: ProducerObservationIdentity {
///         producer: [0; 32],
///         context: [0; 32],
///         evidence_digest: [0; 32],
///     },
/// };
/// ```
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AdmittedProducerObservation {
    scope: ScopeRoot,
    identity: ProducerObservationIdentity,
}

/// Complete coverage bound to an admitted producer identity and exact scope.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AuthorizedCompleteCoverage {
    scope: ScopeRoot,
    identity: ProducerObservationIdentity,
}

/// Fixed-width identity for one admitted producer observation.
///
/// The identity binds producer, session/epoch/source context, and the digest
/// of the evidence checked by the authority verifier. Its private fields keep
/// callers from manufacturing an admitted or wire-complete coverage value;
/// the enclosing coverage wrappers determine whether the identity is trusted.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ProducerObservationIdentity {
    producer: [u8; ID_BYTES],
    context: [u8; ID_BYTES],
    evidence_digest: [u8; ID_BYTES],
}

impl ProducerObservationIdentity {
    const fn from_parts(
        producer: [u8; ID_BYTES],
        context: [u8; ID_BYTES],
        evidence_digest: [u8; ID_BYTES],
    ) -> Self {
        Self {
            producer,
            context,
            evidence_digest,
        }
    }

    pub(crate) fn append_observation_identity(self, out: &mut Vec<u8>) {
        out.extend_from_slice(&self.producer);
        out.extend_from_slice(&self.context);
        out.extend_from_slice(&self.evidence_digest);
    }

    pub(crate) const fn from_canonical_parts(
        producer: [u8; ID_BYTES],
        context: [u8; ID_BYTES],
        evidence_digest: [u8; ID_BYTES],
    ) -> Self {
        Self::from_parts(producer, context, evidence_digest)
    }

    /// Returns the admitted producer/session identity.
    #[must_use]
    pub const fn producer_identity(self) -> [u8; ID_BYTES] {
        self.producer
    }

    /// Returns the admitted session/epoch/source context.
    #[must_use]
    pub const fn context(self) -> [u8; ID_BYTES] {
        self.context
    }

    /// Returns the digest of the evidence checked by the producer verifier.
    #[must_use]
    pub const fn evidence_digest(self) -> [u8; ID_BYTES] {
        self.evidence_digest
    }
}

/// Authority scope claimed by a canonical authority object version.
///
/// This is a claim only. It does not assert that any observation was made and
/// cannot mint an observation capability.
///
/// ```compile_fail
/// use backend_version::{admit_complete_scope, AuthorityScopeClaim, ObjectVersion, Schema, ScopeRoot};
/// struct S;
/// impl Schema for S {
///     const DOMAIN: u8 = 1;
///     const TYPE: u16 = 1;
///     type Value = u64;
///     fn encode(value: &u64, out: &mut Vec<u8>) { out.extend_from_slice(&value.to_be_bytes()); }
/// }
/// fn self_admit(version: ObjectVersion<S>) {
///     let claim = AuthorityScopeClaim::from_object_version(version);
///     let raw = ScopeRoot::from_bytes(*claim.scope_root().as_bytes());
///     // Equal raw digests still do not carry an admitted producer session.
///     let _ = admit_complete_scope(claim, &raw);
/// }
/// ```
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AuthorityScopeClaim {
    scope: ScopeRoot,
}

/// A scoped non-authority capability.
///
/// This value carries only the scope identity. The enclosing
/// [`CoverageWitness`] variant carries the coverage state, so a scope cannot
/// contain a contradictory or independently forged state label.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct UntrustedCoverageScope {
    scope: ScopeRoot,
}

/// Untrusted complete coverage identity retained from a wire manifest.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct UntrustedCompleteCoverage {
    scope: ScopeRoot,
    identity: ProducerObservationIdentity,
}

/// Exact coverage of a locally closed relation materialization.
///
/// A closed relation is complete for path-copy and delta algebra, but carries
/// no authority producer provenance and therefore cannot close a workspace
/// manifest. The scope root is retained to keep transitions bound to the same
/// relation identity.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ClosedRelationScope {
    scope: ScopeRoot,
}

impl ClosedRelationScope {
    /// Creates a closed relation scope from a checked local scope identity.
    ///
    /// This constructor grants no authority capability; callers must still
    /// provide a matching relation state and cannot use the result to publish
    /// a workspace manifest.
    #[must_use]
    pub const fn from_scope_root(scope: ScopeRoot) -> Self {
        Self { scope }
    }

    /// Returns the exact scope retained by this closed relation.
    #[must_use]
    pub const fn scope_root(self) -> ScopeRoot {
        self.scope
    }
}

/// Canonical identity of the exact authority scope being observed.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ScopeRoot([u8; ID_BYTES]);

impl ScopeRoot {
    /// Encodes a legacy numeric scope in the first eight bytes for
    /// non-authoritative compatibility/partial use.
    #[must_use]
    pub const fn from_u64(scope: u64) -> Self {
        let bytes = scope.to_be_bytes();
        Self([
            bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5], bytes[6], bytes[7], 0, 0,
            0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
        ])
    }

    /// Decodes a fixed-width scope root for untrusted/partial use.
    ///
    /// This constructor never creates a scope claim or complete binding. Pass
    /// a canonical object version through
    /// [`AuthorityScopeClaim::from_object_version`] before binding it.
    #[must_use]
    pub const fn from_bytes(bytes: [u8; ID_BYTES]) -> Self {
        Self(bytes)
    }

    /// Returns the exact scope root bytes.
    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; ID_BYTES] {
        &self.0
    }
}

impl AuthorityScopeClaim {
    /// Records an authority scope claim from an already canonical object
    /// version.
    #[must_use]
    pub fn from_object_version<T: Schema>(version: ObjectVersion<T>) -> Self {
        Self {
            scope: ScopeRoot::from_bytes(version.to_bytes()),
        }
    }

    /// Returns the claimed authority scope.
    #[must_use]
    pub const fn scope_root(self) -> ScopeRoot {
        self.scope
    }
}

/// Failure to bind a claimed scope to an observed scope.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CoverageAdmissionError {
    /// The observed scope differed from the canonical claim.
    ScopeMismatch {
        /// Scope in the canonical claim.
        declared: ScopeRoot,
        /// Scope supplied by the observer.
        observed: ScopeRoot,
    },
}

impl fmt::Display for CoverageAdmissionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "coverage scope binding failed: {self:?}")
    }
}

impl std::error::Error for CoverageAdmissionError {}

/// Binds two scope records for low-level equality algebra only.
///
/// The returned [`ScopeEqualityBinding`] cannot be converted into
/// [`CoverageWitness::Complete`]. Use [`admit_complete_scope`] with an
/// [`AdmittedProducerObservation`] when the result is authoritative.
///
/// # Errors
///
/// Returns [`CoverageAdmissionError::ScopeMismatch`] when the two scopes
/// differ.
pub fn bind_scope_equality(
    declared: AuthorityScopeClaim,
    observed: ScopeRoot,
) -> Result<ScopeEqualityBinding, CoverageAdmissionError> {
    if declared.scope == observed {
        Ok(ScopeEqualityBinding {
            scope: declared.scope,
        })
    } else {
        Err(CoverageAdmissionError::ScopeMismatch {
            declared: declared.scope,
            observed,
        })
    }
}

/// Failure while admitting an untrusted producer observation.
#[derive(Debug, Eq, PartialEq)]
pub enum ProducerObservationAdmissionError<E> {
    /// The authority verifier rejected the producer output.
    Rejected(E),
    /// Verifier evidence did not establish the complete observed record.
    ClaimsMismatch,
}

impl<E: fmt::Display> fmt::Display for ProducerObservationAdmissionError<E> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Rejected(error) => write!(f, "producer observation rejected: {error}"),
            Self::ClaimsMismatch => f.write_str("producer observation claims did not match"),
        }
    }
}

impl<E: fmt::Debug + fmt::Display> std::error::Error for ProducerObservationAdmissionError<E> {}

/// Admits one producer output through an authority-owned verifier.
///
/// The resulting observation is opaque and carries producer identity,
/// context, scope, and a digest of the verified evidence. It is the only
/// input accepted by [`admit_complete_scope`].
///
/// # Errors
///
/// Returns [`ProducerObservationAdmissionError::Rejected`] when the authority
/// verifier does not accept the producer/session output, or
/// [`ProducerObservationAdmissionError::ClaimsMismatch`] when its established
/// claims do not exactly bind the observed producer, scope, context, and
/// evidence bytes.
#[allow(
    clippy::needless_pass_by_value,
    reason = "consuming the untrusted record transfers its verified evidence into the opaque capability"
)]
pub fn admit_producer_observation<V: ProducerObservationVerifier>(
    observation: UntrustedProducerObservation,
    verifier: &V,
) -> Result<AdmittedProducerObservation, ProducerObservationAdmissionError<V::Error>> {
    let claims = verifier
        .verify(&observation)
        .map_err(ProducerObservationAdmissionError::Rejected)?;
    let evidence_digest = *blake3::hash(observation.evidence()).as_bytes();
    if claims.producer != observation.producer
        || claims.scope != observation.scope
        || claims.context != observation.context
        || claims.evidence_digest != evidence_digest
    {
        return Err(ProducerObservationAdmissionError::ClaimsMismatch);
    }
    Ok(AdmittedProducerObservation {
        scope: observation.scope,
        identity: ProducerObservationIdentity::from_parts(
            observation.producer,
            observation.context,
            evidence_digest,
        ),
    })
}

impl AdmittedProducerObservation {
    /// Returns the admitted producer identity.
    #[must_use]
    pub const fn producer_identity(self) -> [u8; ID_BYTES] {
        self.identity.producer_identity()
    }

    /// Returns the independently admitted scope.
    #[must_use]
    pub const fn scope_root(self) -> ScopeRoot {
        self.scope
    }

    /// Returns the admitted session/epoch/source context.
    #[must_use]
    pub const fn context(self) -> [u8; ID_BYTES] {
        self.identity.context()
    }

    /// Returns the digest of the evidence checked by the verifier.
    #[must_use]
    pub const fn evidence_digest(self) -> [u8; ID_BYTES] {
        self.identity.evidence_digest()
    }
}

/// Binds a canonical scope claim to an observation from an admitted producer.
///
/// # Errors
///
/// Returns [`CoverageAdmissionError::ScopeMismatch`] when the observation
/// differs from the claim.
///
/// An authority adapter derives the claim from its canonical authority object,
/// obtains an admitted producer/session from its authority layer, and passes
/// that session here. The returned capability carries both exact scope
/// equality and producer identity.
///
/// ```
/// # use backend_version::{admit_complete_scope, admit_producer_observation, AdmittedProducerObservation, AuthorityScopeClaim, CoverageAdmissionError, ObjectVersion, ProducerObservationVerifier, Schema, ScopeRoot, UntrustedProducerObservation};
/// # struct Authority;
/// # impl Schema for Authority {
/// #     const DOMAIN: u8 = 1;
/// #     const TYPE: u16 = 77;
/// #     type Value = u64;
/// #     fn encode(value: &u64, out: &mut Vec<u8>) { out.extend_from_slice(&value.to_be_bytes()); }
/// # }
/// struct Verifier;
/// impl ProducerObservationVerifier for Verifier {
///     type Error = &'static str;
///     fn verify(&self, observation: &UntrustedProducerObservation) -> Result<backend_version::ProducerObservationClaims, Self::Error> {
///         if observation.producer_identity() == [9; 32]
///             && observation.context() == [4; 32]
///             && observation.evidence() == [1, 2, 3]
///         { Ok(backend_version::ProducerObservationClaims::new(
///             [9; 32], observation.scope_root(), [4; 32],
///             *blake3::hash(&[1, 2, 3]).as_bytes())) }
///         else { Err("unadmitted producer output") }
///     }
/// }
/// let authority_version = ObjectVersion::<Authority>::from_value(&7);
/// let claim = AuthorityScopeClaim::from_object_version(authority_version);
/// let observed_scope = ScopeRoot::from_bytes(authority_version.to_bytes());
/// let observation = UntrustedProducerObservation::new(
///     [9; 32], observed_scope, [4; 32], vec![1, 2, 3]);
/// let admitted = admit_producer_observation(observation, &Verifier)
///     .map_err(|_| CoverageAdmissionError::ScopeMismatch {
///         declared: claim.scope_root(), observed: observed_scope
///     })?;
/// let binding = admit_complete_scope(claim, admitted)?;
/// assert_eq!(binding.scope_root(), admitted.scope_root());
/// # Ok::<(), backend_version::CoverageAdmissionError>(())
/// ```
#[allow(
    clippy::needless_pass_by_value,
    reason = "consuming the opaque Copy capability makes its authority handoff explicit"
)]
pub fn admit_complete_scope(
    declared: AuthorityScopeClaim,
    producer: AdmittedProducerObservation,
) -> Result<AuthorizedCompleteCoverage, CoverageAdmissionError> {
    let observed = producer.scope;
    if declared.scope == observed {
        Ok(AuthorizedCompleteCoverage {
            scope: declared.scope,
            identity: producer.identity,
        })
    } else {
        Err(CoverageAdmissionError::ScopeMismatch {
            declared: declared.scope,
            observed,
        })
    }
}

const fn scope_word(scope: ScopeRoot) -> u64 {
    let bytes = scope.0;
    u64::from_be_bytes([
        bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5], bytes[6], bytes[7],
    ])
}

impl ScopeEqualityBinding {
    /// Returns the exact scope shared by the claim and observation.
    #[must_use]
    pub const fn scope_root(self) -> ScopeRoot {
        self.scope
    }

    /// Returns the legacy numeric projection of the bound scope.
    #[must_use]
    pub const fn scope(self) -> u64 {
        scope_word(self.scope)
    }
}

impl AuthorizedCompleteCoverage {
    /// Returns the exact scope observed by the admitted producer.
    #[must_use]
    pub const fn scope_root(self) -> ScopeRoot {
        self.scope
    }

    /// Returns the admitted producer/session identity.
    #[must_use]
    pub const fn producer_identity(self) -> [u8; ID_BYTES] {
        self.identity.producer_identity()
    }

    /// Returns the admitted session/epoch/source context.
    #[must_use]
    pub const fn context(self) -> [u8; ID_BYTES] {
        self.identity.context()
    }

    /// Returns the digest of the evidence checked by the producer verifier.
    #[must_use]
    pub const fn evidence_digest(self) -> [u8; ID_BYTES] {
        self.identity.evidence_digest()
    }
}

impl UntrustedCompleteCoverage {
    /// Returns the claimed complete scope.
    #[must_use]
    pub const fn scope_root(self) -> ScopeRoot {
        self.scope
    }

    /// Returns the claimed producer identity.
    #[must_use]
    pub const fn producer_identity(self) -> [u8; ID_BYTES] {
        self.identity.producer_identity()
    }

    /// Returns the claimed session/epoch/source context.
    #[must_use]
    pub const fn context(self) -> [u8; ID_BYTES] {
        self.identity.context()
    }

    /// Returns the claimed evidence digest.
    #[must_use]
    pub const fn evidence_digest(self) -> [u8; ID_BYTES] {
        self.identity.evidence_digest()
    }
}

impl UntrustedCoverageScope {
    /// Creates a scoped non-authority capability from an exact scope root.
    #[must_use]
    pub const fn from_scope_root(scope: ScopeRoot) -> Self {
        Self { scope }
    }

    /// Creates a scoped non-authority capability for one numeric scope.
    #[must_use]
    pub const fn new(scope: u64) -> Self {
        Self {
            scope: ScopeRoot::from_u64(scope),
        }
    }

    /// Returns the legacy numeric projection of this scope.
    #[must_use]
    pub const fn scope(self) -> u64 {
        scope_word(self.scope)
    }

    /// Returns the exact 32-byte authority scope.
    #[must_use]
    pub const fn scope_root(self) -> ScopeRoot {
        self.scope
    }
}

/// Checked coverage state carried by relation and workspace manifests.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CoverageWitness {
    /// A scope with an exact claim/observation binding.
    Complete(AuthorizedCompleteCoverage),
    /// A wire-declared complete state that has no admitted producer evidence.
    /// It is intentionally unusable for checked transitions.
    UntrustedComplete(UntrustedCompleteCoverage),
    /// A complete local relation materialization without authority provenance.
    Closed(ClosedRelationScope),
    /// A partial authority result.
    Partial(UntrustedCoverageScope),
    /// An unavailable authority result.
    Unavailable(UntrustedCoverageScope),
    /// An unsupported authority result.
    Unsupported(UntrustedCoverageScope),
}

/// Creates a non-authoritative witness for one explicit scope.
#[must_use]
pub const fn partial_coverage(scope: u64) -> UntrustedCoverageScope {
    UntrustedCoverageScope::new(scope)
}
