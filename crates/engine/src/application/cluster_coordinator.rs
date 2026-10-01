//! Owner-side compiler-cluster result ABI and coordinator runtime.
//!
//! Wire decoding, CAS admission, and network dispatch live in separate modules so workers can
//! share a stable output ABI while authority remains with the index owner.

mod admission;
mod result_wire;
mod runtime;

pub use self::admission::{
    AdmittedRemoteCompilerCandidate, CheckedRemoteCompilerArtifact, CheckedRemoteCompilerOutput,
    CheckedRemoteCompilerPlane, CheckedRemoteCompilerPlaneArtifact,
    CheckedRemoteCompilerPlaneDescriptor, CheckedRemoteCompilerPlaneSegment,
    admit_remote_compiler_candidate, readmit_remote_compiler_candidate,
};
pub use self::result_wire::{
    CompilerResultClosureIndex, CompilerResultEnvelopeSchema, CompilerResultEnvelopeV1,
    CompilerResultError, CompilerResultMemberRole, CompilerResultMemberV1,
    CompilerResultOutputClaim, CompilerResultOutputSchema, compiler_result_auxiliary_output_claim,
    compiler_result_auxiliary_output_object, compiler_result_envelope_object_from_members,
    compiler_result_envelope_object_from_members_with_planes,
    compiler_result_envelope_object_from_members_with_versioned_planes,
    compiler_result_output_claim, compiler_result_output_object,
    compiler_result_typed_object_claim, compiler_result_versioned_plane_output_claims,
    reopen_compiler_result_envelope,
};
pub use self::runtime::{
    AdmittedSemanticInputWitnessV2, CompilerClusterCoordinator, CompilerInputAdmissionError,
    CompilerInputAdmissionEvidence, CompilerInputAdmissionVerifier, CompilerTrustedExecutionGrant,
    VerifiedCompilerInputAdmission,
};
