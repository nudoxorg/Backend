//! Generic execution wire envelopes admitted against caller-owned typed material.

use crate::{
    AttemptId, AuthorityExpectation, CancellationId, ExpectedIdentity, Fence, ReplicationError,
    RevocationVersion, SparseCoverage, TransportLimits, WireAuthority, WireAuthorityPolicy,
    WireIdentity, WorkspaceRoot, WorkspaceRootClaim,
};
use std::sync::Arc;

mod attestation;
pub use attestation::*;
mod request;
pub use request::*;
mod result;
pub use result::*;

/// A bounded authentication or attestation statement that an execution owner
/// may carry in its result envelope.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AdmittedExecutionResult {
    wire: Arc<WireRecipeResult>,
}
impl AdmittedExecutionResult {
    /// Returns the admitted wire envelope for a subsequent verifier or
    /// execution-owned canonical output check.
    #[must_use]
    pub fn wire(&self) -> &WireRecipeResult {
        &self.wire
    }

    /// Returns the current number of owners sharing this admitted envelope.
    ///
    /// This is useful for instrumentation around large result handoffs; it
    /// does not grant any additional execution authority.
    #[must_use]
    pub fn shared_owner_count(&self) -> usize {
        Arc::strong_count(&self.wire)
    }
    /// Consumes the wrapper and returns the still non-publishable envelope.
    #[must_use]
    pub fn into_wire(self) -> WireRecipeResult {
        match Arc::try_unwrap(self.wire) {
            Ok(wire) => wire,
            Err(wire) => (*wire).clone(),
        }
    }
}

/// Result carrying a verifier-approved publishability capability.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PublishableExecutionResult {
    admitted: AdmittedExecutionResult,
    class: AttestationClass,
}
impl PublishableExecutionResult {
    /// Returns the verifier-selected trust class.
    #[must_use]
    pub const fn class(&self) -> AttestationClass {
        self.class
    }
    /// Returns the exact admitted wire result.
    #[must_use]
    pub fn wire(&self) -> &WireRecipeResult {
        self.admitted.wire()
    }
    /// Consumes the publishability capability and returns the admitted result.
    #[must_use]
    pub fn into_admitted(self) -> AdmittedExecutionResult {
        self.admitted
    }
}

trait CanonicalWriter {
    fn write(&mut self, bytes: &[u8]);

    fn write_u64(&mut self, value: u64) {
        self.write(&value.to_be_bytes());
    }

    fn write_identity(&mut self, identity: WireIdentity) {
        self.write(&identity.as_bytes());
        self.write(&[identity.context().class()]);
        self.write(&[identity.context().domain()]);
        self.write(&identity.context().ty().to_be_bytes());
        self.write(&[identity.context().version()]);
    }

    fn write_bytes(&mut self, value: &[u8]) {
        self.write_u64(value.len() as u64);
        self.write(value);
    }

    fn write_resources(&mut self, resources: ResourceEnvelope) {
        self.write_u64(resources.cpu_millis);
        self.write_u64(resources.memory_bytes);
        self.write_u64(resources.network_bytes);
        self.write_u64(resources.storage_bytes);
        self.write_u64(resources.output_bytes);
        self.write(&resources.processes.to_be_bytes());
        self.write_u64(resources.wall_millis);
    }

    fn write_byte_coverage(&mut self, coverage: &SparseCoverage) {
        self.write_u64(coverage.ranges().len() as u64);
        for range in coverage.ranges() {
            self.write_u64(range.start);
            self.write_u64(range.len);
        }
    }

    fn write_semantic_coverage(&mut self, coverage: &WireSemanticCoverage) {
        self.write_identity(coverage.identity);
        self.write(&coverage.scope.as_bytes());
        self.write_identity(coverage.read_manifest);
        self.write_authority(coverage.authority);
    }

    fn write_authority_policy(&mut self, policy: WireAuthorityPolicy) {
        self.write_identity(policy.id);
        self.write_u64(policy.minimum_epoch.0);
        self.write_u64(policy.revocation_version.0);
    }

    fn write_authority(&mut self, authority: WireAuthority) {
        self.write_identity(authority.id);
        self.write_u64(authority.epoch.0);
    }
}

impl CanonicalWriter for Vec<u8> {
    fn write(&mut self, bytes: &[u8]) {
        self.extend_from_slice(bytes);
    }
}

impl CanonicalWriter for blake3::Hasher {
    fn write(&mut self, bytes: &[u8]) {
        self.update(bytes);
    }
}

fn canonical_attestation_write<W: CanonicalWriter>(
    material: &AttestationMaterialView<'_>,
    output: &mut W,
) {
    output.write(b"backend.replication.attestation.v2\0");
    output.write_u64(material.attempt.get());
    output.write(&material.cancellation.as_bytes());
    output.write_identity(material.recipe);
    output.write_identity(material.work_key);
    output.write(&material.input_basis.as_bytes());
    output.write_u64(material.inputs.len() as u64);
    for input in material.inputs {
        output.write_identity(*input);
    }
    output.write_identity(material.read_manifest);
    output.write(&material.scope.as_bytes());
    output.write_resources(material.resources);
    output.write(&material.fence.as_bytes());
    output.write_identity(material.output);
    output.write_bytes(material.output_bytes);
    output.write_byte_coverage(material.byte_coverage);
    output.write_semantic_coverage(material.semantic_coverage);
    output.write_authority_policy(material.authority_policy);
    output.write_authority(material.authority);
    output.write_u64(material.revocation_version.0);
    output.write_identity(material.receipt);
}

fn canonical_attestation_bytes(material: &AttestationMaterialView<'_>) -> Vec<u8> {
    let mut bytes = Vec::new();
    canonical_attestation_write(material, &mut bytes);
    bytes
}

/// Alias emphasizing that a pure recipe result is transported as an opaque
/// receipt envelope and admitted by the execution owner.
pub type PureRecipeReceipt = WireRecipeResult;
