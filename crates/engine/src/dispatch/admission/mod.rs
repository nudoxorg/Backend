//! Semantic, local, and remote admission paths.

use super::authority::{
    AcceptedAuthority, AdmissionReservation, LocalAuthorityVerifier, LowerOutputValidator,
    RemoteAuthorityVerifier, check_authority, statement_id_view,
};
use super::coverage::{
    CompleteSemanticCoverage, SemanticCoverageAdmissionError, SemanticCoverageBinding,
    SemanticCoverageValidator, UntrustedSemanticCoverageClaim,
};
use super::protocol::{
    DispatchCompletion, DispatchPlan, DispatchTicket, FenceBinding, PendingRemoteEnvelope,
    RemoteAdmission, RemoteDispatchContract, request_expectation, wire_request, worker_receipt_id,
};
use super::retention::OutputAdmissionValidator;
pub(super) use super::{ActiveSemanticRollback, FreshnessRollback};
use super::{DispatchError, Dispatcher};
use backend_execution::{
    HedgeSide, OutputVersion, ResultCoverage, ResultReceipt, Scheduled, UntrustedResultReceipt,
};
use backend_replication::{
    AttestationClass, AttestationVerifier, ExecutionResultExpectation, ExpectedIdentity,
    ReplicationError, WireRecipeRequest, WireRecipeResult,
};
use backend_version::Relation;
use std::num::NonZeroU64;
use std::sync::Arc;

pub(super) fn local_statement_id(
    semantic: &CompleteSemanticCoverage,
    output: OutputVersion,
) -> [u8; 32] {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"backend.engine.authority.local-statement.v1\0");
    hasher.update(&semantic.identity().to_bytes());
    hasher.update(&semantic.scope().to_be_bytes());
    hasher.update(&semantic.read_manifest().to_bytes());
    hasher.update(&semantic.authority_epoch().0.to_be_bytes());
    hasher.update(&semantic.revocation_version().0.to_be_bytes());
    hasher.update(&output.to_bytes());
    *hasher.finalize().as_bytes()
}

mod local;
mod remote;
mod semantic;
mod ticket;
