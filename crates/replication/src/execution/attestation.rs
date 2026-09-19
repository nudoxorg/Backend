//! Attestation statements and semantic coverage proofs.

use super::{
    AttemptId, AuthorityExpectation, CancellationId, ExpectedIdentity, Fence, ReplicationError,
    RevocationVersion, SparseCoverage, TransportLimits, WireAuthority, WireAuthorityPolicy,
    WireIdentity, WorkspaceRootClaim, canonical_attestation_bytes, canonical_attestation_write,
};

/// A fixed width authentication or attestation statement.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct Attestation(pub [u8; 64]);

/// Trust class returned by a caller-owned attestation verifier.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum AttestationClass {
    /// The verifier recomputed the complete statement locally.
    LocallyVerifiable,
    /// The verifier accepted a statement under a configured signing key.
    TrustedSigned,
    /// The verifier accepted an independently checked quorum or audit.
    QuorumAudited,
    /// A memo that is useful for diagnostics but cannot authorize publication.
    UntrustedMemo,
}
impl AttestationClass {
    /// Returns whether this trust class can authorize a publishable result.
    #[must_use]
    pub const fn is_publishable(self) -> bool {
        !matches!(self, Self::UntrustedMemo)
    }
}

/// A multidimensional bounded resource envelope carried by execution wire
/// messages. The fields intentionally remain generic so replication does not
/// depend on execution's scheduler types.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct ResourceEnvelope {
    /// CPU allowance in milliseconds.
    pub cpu_millis: u64,
    /// Resident or mutable scratch memory allowance in bytes.
    pub memory_bytes: u64,
    /// Network transfer allowance in bytes.
    pub network_bytes: u64,
    /// Durable temporary-storage allowance in bytes.
    pub storage_bytes: u64,
    /// Materialized output allowance in bytes.
    pub output_bytes: u64,
    /// Maximum number of worker processes.
    pub processes: u32,
    /// Wall-clock allowance in milliseconds.
    pub wall_millis: u64,
}
impl ResourceEnvelope {
    /// Largest fixed-width compute declaration. Negotiation intersects this
    /// value with both peer advertisements and transport output/process
    /// limits before it can admit a request.
    pub const UNBOUNDED: Self = Self {
        cpu_millis: u64::MAX,
        memory_bytes: u64::MAX,
        network_bytes: u64::MAX,
        storage_bytes: u64::MAX,
        output_bytes: u64::MAX,
        processes: u32::MAX,
        wall_millis: u64::MAX,
    };

    /// Returns an empty envelope for callers that explicitly permit no work.
    #[must_use]
    pub const fn zero() -> Self {
        Self {
            cpu_millis: 0,
            memory_bytes: 0,
            network_bytes: 0,
            storage_bytes: 0,
            output_bytes: 0,
            processes: 0,
            wall_millis: 0,
        }
    }

    /// Validates every dimension against the negotiated bounded budget.
    ///
    /// # Errors
    ///
    /// Returns [`ReplicationError::MessageTooLarge`] when the declared output
    /// exceeds the transport's bounded object budget or the process count
    /// exceeds the input-count allocation budget. Other compute dimensions
    /// are fixed-width values and are checked against negotiated worker
    /// resources by [`Self::fits_within`].
    pub fn validate(self, limits: TransportLimits) -> Result<(), ReplicationError> {
        limits.validate()?;
        if self.output_bytes > limits.max_object
            || u64::from(self.processes) > limits.max_inputs as u64
        {
            return Err(ReplicationError::MessageTooLarge);
        }
        Ok(())
    }

    /// Returns whether every resource dimension fits an advertised ceiling.
    #[must_use]
    pub const fn fits_within(self, limit: Self) -> bool {
        self.cpu_millis <= limit.cpu_millis
            && self.memory_bytes <= limit.memory_bytes
            && self.network_bytes <= limit.network_bytes
            && self.storage_bytes <= limit.storage_bytes
            && self.output_bytes <= limit.output_bytes
            && self.processes <= limit.processes
            && self.wall_millis <= limit.wall_millis
    }

    /// Computes the common resource ceiling for two peers.
    #[must_use]
    pub fn intersect(self, peer: Self) -> Self {
        Self {
            cpu_millis: self.cpu_millis.min(peer.cpu_millis),
            memory_bytes: self.memory_bytes.min(peer.memory_bytes),
            network_bytes: self.network_bytes.min(peer.network_bytes),
            storage_bytes: self.storage_bytes.min(peer.storage_bytes),
            output_bytes: self.output_bytes.min(peer.output_bytes),
            processes: self.processes.min(peer.processes),
            wall_millis: self.wall_millis.min(peer.wall_millis),
        }
    }

    /// Clamps a compute ceiling to the allocation-bearing transport fields.
    #[must_use]
    pub fn clamp_transport(self, limits: TransportLimits) -> Self {
        let process_limit = u32::try_from(limits.max_inputs).unwrap_or(u32::MAX);
        Self {
            output_bytes: self.output_bytes.min(limits.max_object),
            processes: self.processes.min(process_limit),
            ..self
        }
    }

    /// Returns the fixed encoded size of this envelope.
    #[must_use]
    pub const fn wire_size() -> usize {
        6 * 8 + 4
    }
}

/// An opaque semantic coverage claim bound to the request scope, read
/// manifest, and authority. It is deliberately separate from byte ranges:
/// replication transports this claim but never decides semantic completeness.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WireSemanticCoverage {
    /// Opaque execution-owned coverage identity.
    pub identity: WireIdentity,
    /// Requested semantic scope.
    pub scope: u64,
    /// Exact read manifest that defines the semantic dependency boundary.
    pub read_manifest: WireIdentity,
    /// Authority claim under which the scope was observed.
    pub authority: WireAuthority,
}
impl WireSemanticCoverage {
    /// Checks bounded structure without granting semantic trust.
    ///
    /// # Errors
    ///
    /// Returns a limit error when the transport budget is invalid.
    pub fn validate(&self, limits: TransportLimits) -> Result<(), ReplicationError> {
        limits.validate().map(|_| ())
    }

    /// Admits this claim by exact comparison with caller-owned semantic
    /// coverage material and the current revocation observation.
    ///
    /// # Errors
    ///
    /// Returns an identity, authority, or revocation error when any binding
    /// differs from the caller's expected semantic coverage.
    pub fn admit_against(
        &self,
        expected: &SemanticCoverageExpectation,
        observed_revocation: RevocationVersion,
    ) -> Result<(), ReplicationError> {
        if self.scope != expected.scope {
            return Err(ReplicationError::IdentityMismatch);
        }
        expected.identity.matches(self.identity)?;
        expected.read_manifest.matches(self.read_manifest)?;
        expected
            .authority
            .admit(self.authority, observed_revocation)
    }
}

/// Caller-owned typed semantic coverage material used for exact result
/// admission. The execution crate creates its identities and semantics; this
/// transport type only holds the expected identity/context and bindings.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SemanticCoverageExpectation {
    /// Expected semantic coverage identity.
    pub identity: ExpectedIdentity,
    /// Expected requested scope.
    pub scope: u64,
    /// Expected read manifest identity.
    pub read_manifest: ExpectedIdentity,
    /// Expected authority policy.
    pub authority: AuthorityExpectation,
}
impl SemanticCoverageExpectation {
    /// Validates the bounded transport envelope around this opaque claim.
    ///
    /// Semantic meaning and completeness remain owned by execution; this
    /// check only ensures the negotiated limits are usable.
    ///
    /// # Errors
    ///
    /// Returns [`ReplicationError::InvalidLimits`] when the transport limits
    /// are not usable.
    pub fn validate(&self, limits: TransportLimits) -> Result<(), ReplicationError> {
        limits.validate().map(|_| ())
    }
}

/// Complete request/result material supplied to an attestation verifier.
/// Every field that can affect execution or publication is represented so a
/// verifier cannot accidentally sign only an output digest.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AttestationMaterial {
    /// Attempt identity.
    pub attempt: AttemptId,
    /// Cancellation identity observed by the worker.
    pub cancellation: CancellationId,
    /// Recipe identity claim.
    pub recipe: WireIdentity,
    /// Work identity claim.
    pub work_key: WireIdentity,
    /// Source workspace root claim.
    pub input_basis: WorkspaceRootClaim,
    /// Input object/facet claims.
    pub inputs: Vec<WireIdentity>,
    /// Read-manifest claim.
    pub read_manifest: WireIdentity,
    /// Requested scope.
    pub scope: u64,
    /// Full multidimensional resource envelope.
    pub resources: ResourceEnvelope,
    /// Exact attempt fence.
    pub fence: Fence,
    /// Output identity claim.
    pub output: WireIdentity,
    /// Complete output bytes.
    pub output_bytes: Vec<u8>,
    /// Byte transfer coverage for those output bytes.
    pub byte_coverage: SparseCoverage,
    /// Opaque semantic coverage claim.
    pub semantic_coverage: WireSemanticCoverage,
    /// Requested authority policy.
    pub authority_policy: WireAuthorityPolicy,
    /// Observed authority claim.
    pub authority: WireAuthority,
    /// Revocation observation.
    pub revocation_version: RevocationVersion,
    /// Receipt identity claim.
    pub receipt: WireIdentity,
}
impl AttestationMaterial {
    /// Returns a borrowing view over this statement.
    ///
    /// The view is useful for verifiers that hash or authenticate the
    /// statement directly.  In particular, it does not clone the output
    /// buffer while building the authenticated material.
    #[must_use]
    pub fn as_view(&self) -> AttestationMaterialView<'_> {
        AttestationMaterialView {
            attempt: self.attempt,
            cancellation: self.cancellation,
            recipe: self.recipe,
            work_key: self.work_key,
            input_basis: self.input_basis,
            inputs: &self.inputs,
            read_manifest: self.read_manifest,
            scope: self.scope,
            resources: self.resources,
            fence: self.fence,
            output: self.output,
            output_bytes: &self.output_bytes,
            byte_coverage: &self.byte_coverage,
            semantic_coverage: &self.semantic_coverage,
            authority_policy: self.authority_policy,
            authority: self.authority,
            revocation_version: self.revocation_version,
            receipt: self.receipt,
        }
    }

    /// Encodes the complete statement using length-delimited canonical fields.
    ///
    /// The result is intended for an injected signature/MAC verifier and is
    /// not itself a trust proof.
    #[must_use]
    pub fn canonical_bytes(&self) -> Vec<u8> {
        self.as_view().canonical_bytes()
    }
}

/// A borrowing view of complete attestation material.
///
/// This type deliberately borrows the output bytes and transfer coverage from
/// the wire result.  A verifier that only needs to hash or inspect the
/// statement can therefore avoid creating an owned `Vec<u8>` for a multi-
/// megabyte result.  Callers must still treat every claim as untrusted until
/// the normal exact admission path succeeds.
#[derive(Debug, Eq, PartialEq)]
pub struct AttestationMaterialView<'a> {
    /// Attempt identity.
    pub attempt: AttemptId,
    /// Cancellation identity observed by the worker.
    pub cancellation: CancellationId,
    /// Recipe identity claim.
    pub recipe: WireIdentity,
    /// Work identity claim.
    pub work_key: WireIdentity,
    /// Source workspace root claim.
    pub input_basis: WorkspaceRootClaim,
    /// Input object/facet claims.
    pub inputs: &'a [WireIdentity],
    /// Read-manifest claim.
    pub read_manifest: WireIdentity,
    /// Requested scope.
    pub scope: u64,
    /// Full multidimensional resource envelope.
    pub resources: ResourceEnvelope,
    /// Exact attempt fence.
    pub fence: Fence,
    /// Output identity claim.
    pub output: WireIdentity,
    /// Complete output bytes borrowed from the wire result.
    pub output_bytes: &'a [u8],
    /// Byte transfer coverage borrowed from the wire result.
    pub byte_coverage: &'a SparseCoverage,
    /// Opaque semantic coverage claim.
    pub semantic_coverage: &'a WireSemanticCoverage,
    /// Requested authority policy.
    pub authority_policy: WireAuthorityPolicy,
    /// Observed authority claim.
    pub authority: WireAuthority,
    /// Revocation observation.
    pub revocation_version: RevocationVersion,
    /// Receipt identity claim.
    pub receipt: WireIdentity,
}
impl AttestationMaterialView<'_> {
    /// Encodes the complete statement without copying the borrowed output.
    #[must_use]
    pub fn canonical_bytes(&self) -> Vec<u8> {
        canonical_attestation_bytes(self)
    }

    /// Hashes the complete canonical statement with a caller supplied domain
    /// prefix without allocating a second buffer for the output bytes.
    ///
    /// The prefix is written before the canonical statement, matching the
    /// layout used by keyed statement signers.  This is useful for replay
    /// keys and other derived identifiers that must bind every attestation
    /// field while keeping a large wire output borrowed.
    #[must_use]
    pub fn canonical_digest(&self, domain_prefix: &[u8]) -> [u8; 32] {
        let mut hasher = blake3::Hasher::new();
        hasher.update(domain_prefix);
        canonical_attestation_write(self, &mut hasher);
        *hasher.finalize().as_bytes()
    }

    /// Hashes the complete canonical statement with a keyed BLAKE3 domain
    /// without materializing the canonical bytes.  The key is supplied by the
    /// caller's authority verifier and is never retained by this view.
    #[must_use]
    pub fn keyed_canonical_digest(&self, key: &[u8; 32], domain_prefix: &[u8]) -> [u8; 32] {
        let mut hasher = blake3::Hasher::new_keyed(key);
        hasher.update(domain_prefix);
        canonical_attestation_write(self, &mut hasher);
        *hasher.finalize().as_bytes()
    }

    /// Produces the canonical two-lane authority statement used by both
    /// worker signers and owner verifiers.  Keeping the domains and the
    /// canonical field writer here prevents those implementations from
    /// silently authenticating different material while still leaving key
    /// ownership with the caller.
    #[must_use]
    pub fn keyed_authority_statement(&self, key: &[u8; 32]) -> Attestation {
        let first = self.keyed_canonical_digest(key, AUTHORITY_STATEMENT_DOMAIN);
        let second = self.keyed_canonical_digest(key, AUTHORITY_CONFIRM_DOMAIN);
        let mut signature = [0; 64];
        signature[..32].copy_from_slice(&first);
        signature[32..].copy_from_slice(&second);
        Attestation(signature)
    }

    /// Materializes an owned compatibility value for legacy verifiers.
    ///
    /// New verifiers should use [`Self::canonical_bytes`] through
    /// [`AttestationVerifier::verify_view`] so large output buffers remain
    /// borrowed.
    #[must_use]
    pub fn to_owned_material(&self) -> AttestationMaterial {
        AttestationMaterial {
            attempt: self.attempt,
            cancellation: self.cancellation,
            recipe: self.recipe,
            work_key: self.work_key,
            input_basis: self.input_basis,
            inputs: self.inputs.to_vec(),
            read_manifest: self.read_manifest,
            scope: self.scope,
            resources: self.resources,
            fence: self.fence,
            output: self.output,
            output_bytes: self.output_bytes.to_vec(),
            byte_coverage: self.byte_coverage.clone(),
            semantic_coverage: self.semantic_coverage.clone(),
            authority_policy: self.authority_policy,
            authority: self.authority,
            revocation_version: self.revocation_version,
            receipt: self.receipt,
        }
    }
}

/// Domain-separated lanes for the shared authority statement grammar.
pub const AUTHORITY_STATEMENT_DOMAIN: &[u8] = b"backend.engine.authority.statement.v1\0";
/// Confirmation lane for the shared authority statement grammar.
pub const AUTHORITY_CONFIRM_DOMAIN: &[u8] = b"backend.engine.authority.statement.v1/confirm\0";

/// Caller-owned trust capability for authenticated execution statements.
/// Implementations may perform local recomputation, signature verification,
/// or quorum/audit checks without adding a crypto dependency to replication.
pub trait AttestationVerifier {
    /// Verifies the supplied statement over complete request/result material.
    ///
    /// # Errors
    ///
    /// Returns [`ReplicationError::InvalidAttestation`] when the statement is
    /// absent, malformed, or fails the verifier's configured policy.
    fn verify(
        &self,
        attestation: Option<Attestation>,
        material: &AttestationMaterial,
    ) -> Result<AttestationClass, ReplicationError>;

    /// Verifies complete material through a borrowing view.
    ///
    /// The default keeps source compatibility with existing verifiers by
    /// materializing the old owned form. Verifiers handling large results
    /// should override this method and authenticate `material` directly;
    /// consuming admission calls this method specifically so that override
    /// can preserve the wire output allocation.
    ///
    /// # Errors
    ///
    /// Returns the verifier's admission error when the authenticated
    /// statement is absent or invalid.
    fn verify_view(
        &self,
        attestation: Option<Attestation>,
        material: &AttestationMaterialView<'_>,
    ) -> Result<AttestationClass, ReplicationError> {
        self.verify(attestation, &material.to_owned_material())
    }
}
