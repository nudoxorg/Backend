//! Typed result expectations, wire results, and bounded output admission.

use super::{
    AttemptId, Attestation, CancellationId, ExecutionScopeId, ExpectedIdentity, Fence,
    ReplicationError, ResourceEnvelope, RevocationVersion, SemanticCoverageExpectation,
    SparseCoverage, TransportLimits, WireAuthority, WireAuthorityPolicy, WireIdentity,
    WireSemanticCoverage, WorkspaceRootClaim,
};
use std::sync::Arc;

mod admit;

/// Caller-owned typed output and receipt identities for result admission.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ExecutionResultExpectation {
    /// Expected output object/facet identity.
    pub output: ExpectedIdentity,
    /// Expected result receipt identity.
    pub receipt: ExpectedIdentity,
    /// Exact canonical output length already checked by the execution owner.
    ///
    /// The bytes themselves remain owned by the wire result. Keeping only the
    /// length here prevents dispatch from cloning a large output solely to
    /// perform exact admission.
    pub output_len: u64,
    /// Byte/range coverage expected for the transferred output bytes.
    pub byte_coverage: SparseCoverage,
    /// Opaque execution-owned semantic coverage bound to the request.
    pub semantic_coverage: SemanticCoverageExpectation,
}
impl ExecutionResultExpectation {
    /// Creates an output expectation from its typed identity and exact wire
    /// length. The identity is normally produced from execution's typed
    /// `OutputVersion` with [`ExpectedIdentity::from_typed`].
    #[must_use]
    pub const fn new(
        output: ExpectedIdentity,
        receipt: ExpectedIdentity,
        output_len: u64,
        byte_coverage: SparseCoverage,
        semantic_coverage: SemanticCoverageExpectation,
    ) -> Self {
        Self {
            output,
            receipt,
            output_len,
            byte_coverage,
            semantic_coverage,
        }
    }

    /// Creates an expectation from a borrowed output slice without retaining
    /// or copying that slice.
    #[must_use]
    pub fn from_output_bytes(
        output: ExpectedIdentity,
        receipt: ExpectedIdentity,
        output_bytes: &[u8],
        byte_coverage: SparseCoverage,
        semantic_coverage: SemanticCoverageExpectation,
    ) -> Self {
        Self::new(
            output,
            receipt,
            output_bytes.len() as u64,
            byte_coverage,
            semantic_coverage,
        )
    }

    /// Returns a borrowing view of this expectation.
    #[must_use]
    pub fn as_ref(&self) -> ExecutionResultExpectationRef<'_> {
        ExecutionResultExpectationRef {
            output: self.output,
            receipt: self.receipt,
            output_len: self.output_len,
            byte_coverage: &self.byte_coverage,
            semantic_coverage: &self.semantic_coverage,
        }
    }

    fn validate(&self, limits: TransportLimits) -> Result<(), ReplicationError> {
        limits.validate()?;
        if self.output_len > limits.max_object {
            return Err(ReplicationError::ObjectTooLarge);
        }
        if self.byte_coverage.ranges().len() > limits.max_ranges {
            return Err(ReplicationError::CoverageLimit);
        }
        for range in self.byte_coverage.ranges() {
            if range.end()? > self.output_len {
                return Err(ReplicationError::Range);
            }
        }
        self.semantic_coverage.validate(limits)
    }
}

/// Borrowed result expectation used by consuming admission paths.
///
/// The wire result already owns its canonical bytes and transfer coverage.
/// Borrowing those fields here lets an owner perform exact comparison without
/// allocating a second copy before the result is moved into its admitted
/// `Arc` capability.
#[derive(Debug)]
pub struct ExecutionResultExpectationRef<'a> {
    /// Expected output object/facet identity.
    pub output: ExpectedIdentity,
    /// Expected result receipt identity.
    pub receipt: ExpectedIdentity,
    /// Exact canonical output length expected from the wire result.
    pub output_len: u64,
    /// Byte/range coverage borrowed from the wire result.
    pub byte_coverage: &'a SparseCoverage,
    /// Semantic coverage expectation borrowed from the caller's contract.
    pub semantic_coverage: &'a SemanticCoverageExpectation,
}

impl ExecutionResultExpectationRef<'_> {
    fn validate(&self, limits: TransportLimits) -> Result<(), ReplicationError> {
        limits.validate()?;
        if self.output_len > limits.max_object {
            return Err(ReplicationError::ObjectTooLarge);
        }
        if self.byte_coverage.ranges().len() > limits.max_ranges {
            return Err(ReplicationError::CoverageLimit);
        }
        for range in self.byte_coverage.ranges() {
            if range.end()? > self.output_len {
                return Err(ReplicationError::Range);
            }
        }
        self.semantic_coverage.validate(limits)
    }
}

/// A wire execution result/receipt with explicitly untrusted identities.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WireRecipeResult {
    /// Attempt identity.
    pub attempt: AttemptId,
    /// Untrusted recipe identity.
    pub recipe: WireIdentity,
    /// Untrusted semantic work identity.
    pub work_key: WireIdentity,
    /// Untrusted source basis root.
    pub input_basis: WorkspaceRootClaim,
    /// Untrusted input object/facet identities.
    pub inputs: Vec<WireIdentity>,
    /// Untrusted read-manifest identity.
    pub read_manifest: WireIdentity,
    /// Untrusted output object/facet identity.
    pub output: WireIdentity,
    /// Canonical output bytes carried by the untrusted result envelope.
    ///
    /// The shared immutable owner lets decode, attestation verification,
    /// admission, and lower receipt publication retain one allocation.
    pub output_bytes: Arc<Vec<u8>>,
    /// Scope echoed from the request.
    pub scope: ExecutionScopeId,
    /// Full resource envelope echoed from the request.
    pub resources: ResourceEnvelope,
    /// Byte/range coverage for the output bytes. This never claims semantic
    /// completeness.
    pub byte_coverage: SparseCoverage,
    /// Opaque semantic coverage claim bound to scope/read/authority.
    pub semantic_coverage: WireSemanticCoverage,
    /// Untrusted worker authority.
    pub authority: WireAuthority,
    /// Authority policy echoed from the request.
    pub authority_policy: WireAuthorityPolicy,
    /// Revocation observation.
    pub revocation_version: RevocationVersion,
    /// Publication fence echoed from the request.
    pub fence: Fence,
    /// Cancellation identity echoed from the request.
    pub cancellation: CancellationId,
    /// Optional authenticated worker statement.
    pub attestation: Option<Attestation>,
    /// Untrusted receipt statement identity.
    pub receipt: WireIdentity,
}
/// Generic name for a wire execution result receipt.
pub type WireExecutionResult = WireRecipeResult;
