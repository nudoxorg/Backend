//! Durable owner intent for acknowledging stored compiler results.
//!
//! The journal is a retry hint, never selection authority. Rows are written
//! before compare-and-select; startup must resolve their exact selection
//! tuple against Turso's append-only history before constructing a recovered
//! ACK capability.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::{self, Read};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, SocketAddrV6};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use backend_engine::cluster_transport::{AssignmentScope, EndpointAddr, EndpointId, SecretKey};
use backend_extension_turso::{SelectedGeneration, SupersededAttemptProof};

use super::semantic_authority::PendingRemoteResultProof;
use crate::compiler_trust::TrustedCompilerWorkerPolicy;

const JOURNAL_DIRECTORY: &str = "compiler-pending-stored-acks";
const JOURNAL_FILE: &str = "pending-stored-acks.v4";
const MAGIC: &[u8; 8] = b"BKPSACK4";
const FORMAT_VERSION: u16 = 4;
const CHECKSUM_BYTES: usize = 32;
const MAX_ROWS: usize = 64;
const MAX_FILE_BYTES: usize = 2 * 1024 * 1024;
const MAX_WORKER_OBJECTS: u32 = 100_002;
const MAX_WORKER_BYTES: u64 = 512 * 1024 * 1024;
const MAX_PACKAGE_BYTES: usize = 4096;
const MAX_COORDINATE_BYTES: usize = 4096;
const MAX_PROFILE_BYTES: usize = 256;
const MAX_SOURCE_ROOT_BYTES: usize = 8 * 1024;

/// State of an owner-side pending result ACK. Terminal choices only move
/// forward, so a crash after a Superseded ACK can never turn it into Stored.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum PendingStoredAckState {
    /// The result is durably in the owner CAS; Turso selection may be pending
    /// or its commit outcome may be uncertain.
    AwaitingSelection,
    /// Exact selected-generation history authorizes an idempotent Stored ACK.
    StoredAckPending,
    /// Exact authority proof says this attempt was superseded; only a terminal
    /// Superseded ACK may be retried.
    SupersededAckPending,
    /// The worker durably retired the result and the owner must retry only the
    /// exact retirement confirmation, never resend a different disposition.
    StoredAwaitingRetirementConfirm,
    /// The worker durably retired a superseded result and the owner must retry
    /// only the exact retirement confirmation.
    SupersededAwaitingRetirementConfirm,
}

impl PendingStoredAckState {
    const fn tag(self) -> u8 {
        match self {
            Self::AwaitingSelection => 0,
            Self::StoredAckPending => 1,
            Self::SupersededAckPending => 2,
            Self::StoredAwaitingRetirementConfirm => 3,
            Self::SupersededAwaitingRetirementConfirm => 4,
        }
    }

    fn from_tag(tag: u8) -> Result<Self, PendingStoredAckError> {
        match tag {
            0 => Ok(Self::AwaitingSelection),
            1 => Ok(Self::StoredAckPending),
            2 => Ok(Self::SupersededAckPending),
            3 => Ok(Self::StoredAwaitingRetirementConfirm),
            4 => Ok(Self::SupersededAwaitingRetirementConfirm),
            _ => Err(PendingStoredAckError::Corrupt("state")),
        }
    }
}

/// Product key material needed to reopen the exact Turso namespace.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct PendingStoredAckProductKey {
    pub(crate) package: Box<str>,
    pub(crate) coordinate: Box<str>,
    /// Canonical profile name used by the product semantic authority.
    pub(crate) profile: Box<str>,
}

/// Current trust grant facts for the exact compiler invocation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct PendingStoredAckTrust {
    pub(crate) recipe: [u8; 32],
    pub(crate) profile: [u8; 2],
    pub(crate) stage: u8,
    pub(crate) toolchain: [u8; 32],
    pub(crate) environment: [u8; 32],
    pub(crate) target_platform: [u8; 32],
}

/// All claims required to resolve and retry one exact Stored ACK.
///
/// Every field is checksummed and validated on reopen. Callers still need an
/// exact authority proof before obtaining the sealed `RecoveredAckScope` used
/// by the transport path; current execution trust is required for ordinary
/// ACK retries, while selected Stored cleanup is narrowly proof-gated.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct PendingStoredAckRecord {
    pub(crate) state: PendingStoredAckState,
    pub(crate) owner_endpoint_id: [u8; 32],
    pub(crate) worker_peer: [u8; 32],
    pub(crate) worker_address: SocketAddr,
    pub(crate) namespace_id: [u8; 16],
    pub(crate) product_key: PendingStoredAckProductKey,
    /// Stable transfer work identity derived from every assignment work fact.
    pub(crate) work_id: [u8; 16],
    pub(crate) assignment_attempt: u64,
    pub(crate) assignment_fence: [u8; 32],
    pub(crate) worker_closure_id: [u8; 32],
    pub(crate) worker_object_count: u32,
    pub(crate) worker_payload_bytes: u64,
    pub(crate) worker_bytes_verified: u64,
    /// Original authenticated, non-authoritative receipt required to repeat
    /// exact result admission after a crash before the Turso CAS.
    pub(crate) worker_result_receipt: backend_engine::cluster_transport::ControlResultReceipt,
    pub(crate) turso_generation: u64,
    pub(crate) turso_attempt_id: [u8; 16],
    pub(crate) turso_attempt_epoch: u64,
    pub(crate) turso_fence: [u8; 32],
    pub(crate) input_digest: [u8; 32],
    pub(crate) candidate_id: [u8; 32],
    pub(crate) target_root: [u8; 32],
    pub(crate) selected_closure_id: [u8; 32],
    pub(crate) trust: PendingStoredAckTrust,
    /// The exact input capture kept with AwaitingSelection so cold recovery can
    /// finish a CAS that crashed after intent fsync but before Turso commit.
    pub(crate) captured_work: PendingAckCapturedWork,
}

impl PendingStoredAckRecord {
    /// Stable journal key for idempotent replacement and deletion.
    #[must_use]
    pub(crate) fn id(&self) -> [u8; 32] {
        // A direct address is a mutable route hint, not result identity. A
        // grant may be updated to a new address while the peer and exact
        // assignment remain the same.
        let bytes = self.encode_identity(false);
        *blake3::hash(&bytes).as_bytes()
    }

    fn validate(&self) -> Result<(), PendingStoredAckError> {
        self.captured_work.validate()?;
        if self.owner_endpoint_id == [0; 32]
            || self.worker_peer == [0; 32]
            || self.worker_peer == self.owner_endpoint_id
            || self.namespace_id == [0; 16]
            || self.work_id == [0; 16]
            || self.assignment_attempt == 0
            || self.assignment_fence == [0; 32]
            || self.worker_closure_id == [0; 32]
            || self.worker_object_count == 0
            || self.worker_object_count > MAX_WORKER_OBJECTS
            || self.worker_payload_bytes == 0
            || self.worker_payload_bytes > MAX_WORKER_BYTES
            || self.worker_bytes_verified < self.worker_payload_bytes
            || self.worker_bytes_verified > MAX_WORKER_BYTES
            || self.worker_result_receipt.scope.namespace_id != self.namespace_id
            || self.worker_result_receipt.scope.work_id != self.work_id
            || self.worker_result_receipt.scope.attempt != self.assignment_attempt
            || self.worker_result_receipt.scope.fence != self.assignment_fence
            || self.worker_result_receipt.closure_id != self.worker_closure_id
            || self.worker_result_receipt.object_count != self.worker_object_count
            || self.worker_result_receipt.payload_bytes != self.worker_payload_bytes
            || self.worker_result_receipt.result_grant_pages == 0
            || self.worker_result_receipt.target_root != self.target_root
            || self.worker_result_receipt.pack_id == Some([0; 32])
            || self.turso_generation == 0
            || self.turso_attempt_id == [0; 16]
            || self.turso_attempt_epoch == 0
            || self.turso_fence == [0; 32]
            || self.assignment_attempt != self.turso_attempt_epoch
            || self.assignment_fence != self.turso_fence
            || self.input_digest == [0; 32]
            || self.candidate_id == [0; 32]
            || self.target_root == [0; 32]
            || self.selected_closure_id == [0; 32]
            || self.trust.recipe == [0; 32]
            || self.trust.toolchain == [0; 32]
            || self.trust.environment == [0; 32]
            || self.trust.target_platform == [0; 32]
            || self.captured_work.recipe != self.trust.recipe
            || self.captured_work.turso_epoch != self.turso_attempt_epoch
            || self.captured_work.turso_fence != self.turso_fence
            || self.captured_work.turso_attempt_id != self.turso_attempt_id
            || self.captured_work.turso_input_digest != self.input_digest
            || self.captured_work.input_root != self.input_digest
            || self.captured_work.work_id() != self.work_id
        {
            return Err(PendingStoredAckError::Invalid("empty identity or receipt"));
        }
        EndpointId::from_bytes(&self.owner_endpoint_id)
            .map_err(|_| PendingStoredAckError::Invalid("owner endpoint"))?;
        EndpointId::from_bytes(&self.worker_peer)
            .map_err(|_| PendingStoredAckError::Invalid("worker endpoint"))?;
        let address = self.worker_address;
        if address.port() == 0 || address.ip().is_unspecified() || address.ip().is_multicast() {
            return Err(PendingStoredAckError::Invalid("worker address"));
        }
        check_text(
            self.product_key.package.as_bytes(),
            MAX_PACKAGE_BYTES,
            "package",
        )?;
        check_text(
            self.product_key.coordinate.as_bytes(),
            MAX_COORDINATE_BYTES,
            "coordinate",
        )?;
        check_text(
            self.product_key.profile.as_bytes(),
            MAX_PROFILE_BYTES,
            "profile",
        )?;
        if self.product_key.profile.is_empty()
            || !self
                .product_key
                .profile
                .bytes()
                .all(|byte| byte.is_ascii_graphic())
        {
            return Err(PendingStoredAckError::Invalid("profile"));
        }
        if backend_semantic::vocabulary::LanguageProfile::try_from(self.trust.profile).is_err()
            || backend_semantic::vocabulary::Stage::try_from(self.trust.stage).is_err()
        {
            return Err(PendingStoredAckError::Invalid("trust profile or stage"));
        }
        if parse_profile_code(&self.product_key.profile)? != self.trust.profile
            || self.trust.stage != u8::from(backend_semantic::vocabulary::Stage::LowerIr)
        {
            return Err(PendingStoredAckError::Invalid(
                "product profile and trusted compiler grant differ",
            ));
        }
        if self.state != PendingStoredAckState::AwaitingSelection
            && self.state != PendingStoredAckState::StoredAckPending
            && self.state != PendingStoredAckState::SupersededAckPending
            && self.state != PendingStoredAckState::StoredAwaitingRetirementConfirm
            && self.state != PendingStoredAckState::SupersededAwaitingRetirementConfirm
        {
            return Err(PendingStoredAckError::Invalid("state"));
        }
        Ok(())
    }

    fn encode_identity(&self, include_address: bool) -> Vec<u8> {
        let mut output = Vec::new();
        output.extend_from_slice(&self.owner_endpoint_id);
        output.extend_from_slice(&self.worker_peer);
        if include_address {
            encode_socket_addr(&mut output, self.worker_address);
        }
        output.extend_from_slice(&self.namespace_id);
        encode_text(&mut output, self.product_key.package.as_bytes());
        encode_text(&mut output, self.product_key.coordinate.as_bytes());
        encode_text(&mut output, self.product_key.profile.as_bytes());
        output.extend_from_slice(&self.work_id);
        output.extend_from_slice(&self.assignment_attempt.to_be_bytes());
        output.extend_from_slice(&self.assignment_fence);
        output.extend_from_slice(&self.worker_closure_id);
        output.extend_from_slice(&self.worker_object_count.to_be_bytes());
        output.extend_from_slice(&self.worker_payload_bytes.to_be_bytes());
        output.extend_from_slice(&self.worker_bytes_verified.to_be_bytes());
        encode_control_result_receipt(&mut output, self.worker_result_receipt);
        output.extend_from_slice(&self.turso_generation.to_be_bytes());
        output.extend_from_slice(&self.turso_attempt_id);
        output.extend_from_slice(&self.turso_attempt_epoch.to_be_bytes());
        output.extend_from_slice(&self.turso_fence);
        output.extend_from_slice(&self.input_digest);
        output.extend_from_slice(&self.candidate_id);
        output.extend_from_slice(&self.target_root);
        output.extend_from_slice(&self.selected_closure_id);
        output.extend_from_slice(&self.trust.recipe);
        output.extend_from_slice(&self.trust.profile);
        output.push(self.trust.stage);
        output.extend_from_slice(&self.trust.toolchain);
        output.extend_from_slice(&self.trust.environment);
        output.extend_from_slice(&self.trust.target_platform);
        self.captured_work
            .encode(&mut output)
            .expect("validated selection capture encodes");
        output
    }

    fn encode(&self) -> Result<Vec<u8>, PendingStoredAckError> {
        self.validate()?;
        let mut output = Vec::new();
        output.push(self.state.tag());
        output.extend_from_slice(&self.encode_identity(true));
        Ok(output)
    }

    fn decode(bytes: &[u8]) -> Result<Self, PendingStoredAckError> {
        let mut cursor = Cursor::new(bytes);
        let state = PendingStoredAckState::from_tag(cursor.u8()?)?;
        let owner_endpoint_id = cursor.array()?;
        let worker_peer = cursor.array()?;
        let worker_address = cursor.socket_addr()?;
        let namespace_id = cursor.array()?;
        let product_key = PendingStoredAckProductKey {
            package: cursor.text(MAX_PACKAGE_BYTES, "package")?,
            coordinate: cursor.text(MAX_COORDINATE_BYTES, "coordinate")?,
            profile: cursor.text(MAX_PROFILE_BYTES, "profile")?,
        };
        let work_id = cursor.array()?;
        let assignment_attempt = cursor.u64()?;
        let assignment_fence = cursor.array()?;
        let worker_closure_id = cursor.array()?;
        let worker_object_count = cursor.u32()?;
        let worker_payload_bytes = cursor.u64()?;
        let worker_bytes_verified = cursor.u64()?;
        let worker_result_receipt = decode_control_result_receipt(&mut cursor)?;
        let turso_generation = cursor.u64()?;
        let turso_attempt_id = cursor.array()?;
        let turso_attempt_epoch = cursor.u64()?;
        let turso_fence = cursor.array()?;
        let input_digest = cursor.array()?;
        let candidate_id = cursor.array()?;
        let target_root = cursor.array()?;
        let selected_closure_id = cursor.array()?;
        let trust = PendingStoredAckTrust {
            recipe: cursor.array()?,
            profile: cursor.array()?,
            stage: cursor.u8()?,
            toolchain: cursor.array()?,
            environment: cursor.array()?,
            target_platform: cursor.array()?,
        };
        let captured_work = PendingAckCapturedWork::decode(&mut cursor)?;
        cursor.finish()?;
        let row = Self {
            state,
            owner_endpoint_id,
            worker_peer,
            worker_address,
            namespace_id,
            product_key,
            work_id,
            assignment_attempt,
            assignment_fence,
            worker_closure_id,
            worker_object_count,
            worker_payload_bytes,
            worker_bytes_verified,
            worker_result_receipt,
            turso_generation,
            turso_attempt_id,
            turso_attempt_epoch,
            turso_fence,
            input_digest,
            candidate_id,
            target_root,
            selected_closure_id,
            trust,
            captured_work,
        };
        row.validate()?;
        Ok(row)
    }
}

/// Capacity held before a remote assignment can produce a result requiring
/// durable terminal acknowledgement.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct PendingAckCapacityReservation {
    id: [u8; 32],
}

impl PendingAckCapacityReservation {
    #[must_use]
    pub(crate) const fn id(self) -> [u8; 32] {
        self.id
    }
}

/// In-process guard for one durable capacity reservation. Before Offer may
/// have reached a worker, dropping the guard releases the slot; after that
/// boundary the journal refuses the release and retains the row for recovery.
pub(crate) struct PendingAckReservationLease {
    journal: Arc<Mutex<PendingStoredAckJournal>>,
    reservation: PendingAckCapacityReservation,
}

impl PendingAckReservationLease {
    pub(crate) fn new(
        journal: Arc<Mutex<PendingStoredAckJournal>>,
        reservation: PendingAckCapacityReservation,
    ) -> Self {
        Self {
            journal,
            reservation,
        }
    }

    #[must_use]
    pub(crate) const fn reservation(&self) -> PendingAckCapacityReservation {
        self.reservation
    }
}

impl Drop for PendingAckReservationLease {
    fn drop(&mut self) {
        let _ = self
            .journal
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .release_before_offer(self.reservation);
    }
}

/// Sealed proof that the exact terminal disposition is already durably stored
/// for one assignment/result. Owner transport requires this capability before
/// it can send a terminal ACK frame.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct PreparedResultDisposition {
    journal_id: [u8; 32],
    scope: AssignmentScope,
    expected_peer: EndpointId,
    worker_address: SocketAddr,
    closure_id: [u8; 32],
    disposition: backend_engine::cluster_transport::ResultAckDisposition,
}

impl PreparedResultDisposition {
    #[must_use]
    pub(crate) const fn journal_id(self) -> [u8; 32] {
        self.journal_id
    }

    #[must_use]
    pub(crate) const fn disposition(
        self,
    ) -> backend_engine::cluster_transport::ResultAckDisposition {
        self.disposition
    }

    #[must_use]
    pub(crate) const fn closure_id(self) -> [u8; 32] {
        self.closure_id
    }

    #[must_use]
    pub(crate) const fn expected_peer(self) -> EndpointId {
        self.expected_peer
    }

    #[must_use]
    pub(crate) const fn worker_address(self) -> SocketAddr {
        self.worker_address
    }

    #[must_use]
    pub(crate) const fn scope(self) -> AssignmentScope {
        self.scope
    }

    #[must_use]
    pub(crate) fn matches(
        self,
        result: &super::cluster_dispatch::CompilerResultIdentity,
        disposition: backend_engine::cluster_transport::ResultAckDisposition,
    ) -> bool {
        self.disposition == disposition
            && self.closure_id == result.closure_id
            && self.expected_peer.as_bytes() == result.worker_grant.peer().as_bytes()
            && self.worker_address == result.worker_grant.address()
            && assignment_scope(result.assignment, result.namespace_id)
                .is_ok_and(|scope| scope == self.scope)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct PendingAckAssignmentIdentity {
    pub(crate) owner_endpoint_id: [u8; 32],
    pub(crate) worker_peer: [u8; 32],
    pub(crate) worker_address: SocketAddr,
    pub(crate) namespace_id: [u8; 16],
    pub(crate) product_key: PendingStoredAckProductKey,
    pub(crate) work_id: [u8; 16],
    pub(crate) assignment_attempt: u64,
    pub(crate) assignment_fence: [u8; 32],
    pub(crate) trust: PendingStoredAckTrust,
}

/// Exact full-workspace capture and Turso attempt needed to recover an Offer
/// after owner restart. These facts are hints only until cold code reopens the
/// pinned closure and asks Turso to re-admit the current attempt.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct PendingAckCapturedWork {
    pub(crate) source_root: Box<str>,
    pub(crate) package_lineage: [u8; 32],
    pub(crate) target: [u8; 32],
    pub(crate) recipe: [u8; 32],
    pub(crate) input_root: [u8; 32],
    pub(crate) read_manifest: [u8; 32],
    pub(crate) workspace_snapshot_id: [u8; 32],
    pub(crate) max_output_bytes: u64,
    pub(crate) input_closure_id: [u8; 32],
    pub(crate) manifest_object_id: [u8; 32],
    pub(crate) object_count: u64,
    pub(crate) payload_bytes: u64,
    pub(crate) source_fence_digest: [u8; 32],
    pub(crate) source_observation_revision: [u8; 32],
    pub(crate) source_observation_sequence: u64,
    pub(crate) source_observation_observed_at_ms: u64,
    pub(crate) source_observation_count: u64,
    pub(crate) turso_attempt_id: [u8; 16],
    pub(crate) turso_epoch: u64,
    pub(crate) turso_fence: [u8; 32],
    pub(crate) turso_input_digest: [u8; 32],
    pub(crate) turso_base_generation: u64,
    pub(crate) turso_base_root: Option<[u8; 32]>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum PendingAckReservationStage {
    Unbound,
    BoundBeforeOffer,
    OfferMayBeSent,
}

/// Untrusted facts copied from a durable OfferMayBeSent row. This value is
/// only a lookup/reopen hint; authority, CAS, source fence, and worker trust
/// must be re-admitted before it can revive an assignment.
#[derive(Clone, Debug)]
pub(crate) struct OfferedReservationClaims {
    pub(crate) id: [u8; 32],
    pub(crate) owner_endpoint_id: [u8; 32],
    pub(crate) identity: PendingAckAssignmentIdentity,
    pub(crate) captured_work: PendingAckCapturedWork,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct PendingAckReservationRecord {
    id: [u8; 32],
    stage: PendingAckReservationStage,
    owner_endpoint_id: [u8; 32],
    identity: Option<PendingAckAssignmentIdentity>,
    capture: Option<PendingAckCapturedWork>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum PendingRejectedAckState {
    AckPending,
    AwaitingRetirementConfirm,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct PendingRejectedAckRecord {
    state: PendingRejectedAckState,
    identity: PendingAckAssignmentIdentity,
    closure_id: [u8; 32],
    reason: backend_engine::cluster_transport::ResultRejectReason,
}

/// Typed journal entries keep selection authority separate from a durable
/// owner-side admission rejection and from capacity that has not reached Offer.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum PendingStoredAckEntry {
    Reservation(PendingAckReservationRecord),
    Selection(PendingStoredAckRecord),
    RejectedAdmission(PendingRejectedAckRecord),
}

impl PendingAckAssignmentIdentity {
    fn from_assignment(
        owner_endpoint_id: [u8; 32],
        assignment: backend_engine::application::CompilerAssignment,
        namespace_id: [u8; 16],
        product_key: PendingStoredAckProductKey,
        grant: &crate::compiler_trust::TrustedCompilerWorkerGrant,
    ) -> Result<Self, PendingStoredAckError> {
        let backend_engine::application::CompilerAssignmentRoute::Remote(peer) = assignment.route()
        else {
            return Err(PendingStoredAckError::Invalid("assignment is not remote"));
        };
        let work = assignment.work();
        let token = assignment.token();
        let scope = backend_engine::compiler_cluster_transport::compiler_assignment_scope(
            assignment,
            namespace_id,
        )
        .map_err(|_| PendingStoredAckError::Invalid("assignment scope"))?;
        if owner_endpoint_id == [0; 32]
            || namespace_id != grant.namespace_id()
            || peer.as_bytes() != *grant.peer().as_bytes()
            || *work.recipe().as_ref() != grant.recipe()
            || token.attempt().get() != scope.attempt
            || token.fence().as_bytes() != scope.fence
        {
            return Err(PendingStoredAckError::Invalid(
                "assignment differs from exact worker grant",
            ));
        }
        let identity = Self {
            owner_endpoint_id,
            worker_peer: peer.as_bytes(),
            worker_address: grant.address(),
            namespace_id,
            product_key,
            work_id: scope.work_id,
            assignment_attempt: scope.attempt,
            assignment_fence: scope.fence,
            trust: PendingStoredAckTrust {
                recipe: grant.recipe(),
                profile: <[u8; 2]>::from(grant.profile()),
                stage: u8::from(grant.stage()),
                toolchain: grant.toolchain(),
                environment: grant.environment(),
                target_platform: grant.target_platform(),
            },
        };
        identity.validate()?;
        Ok(identity)
    }

    fn from_result_identity(
        owner_endpoint_id: [u8; 32],
        result: &super::cluster_dispatch::CompilerResultIdentity,
        product_key: PendingStoredAckProductKey,
    ) -> Result<Self, PendingStoredAckError> {
        if result.closure_id == [0; 32] {
            return Err(PendingStoredAckError::Invalid("result closure"));
        }
        Self::from_assignment(
            owner_endpoint_id,
            result.assignment,
            result.namespace_id,
            product_key,
            &result.worker_grant,
        )
    }

    fn validate(&self) -> Result<(), PendingStoredAckError> {
        if self.owner_endpoint_id == [0; 32]
            || self.worker_peer == [0; 32]
            || self.worker_peer == self.owner_endpoint_id
            || self.namespace_id == [0; 16]
            || self.work_id == [0; 16]
            || self.assignment_attempt == 0
            || self.assignment_fence == [0; 32]
            || self.trust.recipe == [0; 32]
            || self.trust.toolchain == [0; 32]
            || self.trust.environment == [0; 32]
            || self.trust.target_platform == [0; 32]
        {
            return Err(PendingStoredAckError::Invalid("assignment identity"));
        }
        EndpointId::from_bytes(&self.owner_endpoint_id)
            .map_err(|_| PendingStoredAckError::Invalid("owner endpoint"))?;
        EndpointId::from_bytes(&self.worker_peer)
            .map_err(|_| PendingStoredAckError::Invalid("worker endpoint"))?;
        if self.worker_address.port() == 0
            || self.worker_address.ip().is_unspecified()
            || self.worker_address.ip().is_multicast()
        {
            return Err(PendingStoredAckError::Invalid("worker address"));
        }
        check_text(
            self.product_key.package.as_bytes(),
            MAX_PACKAGE_BYTES,
            "package",
        )?;
        check_text(
            self.product_key.coordinate.as_bytes(),
            MAX_COORDINATE_BYTES,
            "coordinate",
        )?;
        check_text(
            self.product_key.profile.as_bytes(),
            MAX_PROFILE_BYTES,
            "profile",
        )?;
        if parse_profile_code(&self.product_key.profile)? != self.trust.profile
            || backend_semantic::vocabulary::LanguageProfile::try_from(self.trust.profile).is_err()
            || self.trust.stage != u8::from(backend_semantic::vocabulary::Stage::LowerIr)
        {
            return Err(PendingStoredAckError::Invalid("assignment trust scope"));
        }
        Ok(())
    }

    fn encode(&self, include_address: bool) -> Vec<u8> {
        let mut output = Vec::new();
        output.extend_from_slice(&self.owner_endpoint_id);
        output.extend_from_slice(&self.worker_peer);
        if include_address {
            encode_socket_addr(&mut output, self.worker_address);
        }
        output.extend_from_slice(&self.namespace_id);
        encode_text(&mut output, self.product_key.package.as_bytes());
        encode_text(&mut output, self.product_key.coordinate.as_bytes());
        encode_text(&mut output, self.product_key.profile.as_bytes());
        output.extend_from_slice(&self.work_id);
        output.extend_from_slice(&self.assignment_attempt.to_be_bytes());
        output.extend_from_slice(&self.assignment_fence);
        output.extend_from_slice(&self.trust.recipe);
        output.extend_from_slice(&self.trust.profile);
        output.push(self.trust.stage);
        output.extend_from_slice(&self.trust.toolchain);
        output.extend_from_slice(&self.trust.environment);
        output.extend_from_slice(&self.trust.target_platform);
        output
    }

    fn decode(cursor: &mut Cursor<'_>) -> Result<Self, PendingStoredAckError> {
        let identity = Self {
            owner_endpoint_id: cursor.array()?,
            worker_peer: cursor.array()?,
            worker_address: cursor.socket_addr()?,
            namespace_id: cursor.array()?,
            product_key: PendingStoredAckProductKey {
                package: cursor.text(MAX_PACKAGE_BYTES, "package")?,
                coordinate: cursor.text(MAX_COORDINATE_BYTES, "coordinate")?,
                profile: cursor.text(MAX_PROFILE_BYTES, "profile")?,
            },
            work_id: cursor.array()?,
            assignment_attempt: cursor.u64()?,
            assignment_fence: cursor.array()?,
            trust: PendingStoredAckTrust {
                recipe: cursor.array()?,
                profile: cursor.array()?,
                stage: cursor.u8()?,
                toolchain: cursor.array()?,
                environment: cursor.array()?,
                target_platform: cursor.array()?,
            },
        };
        identity.validate()?;
        Ok(identity)
    }

    fn same_assignment(&self, other: &Self) -> bool {
        self.owner_endpoint_id == other.owner_endpoint_id
            && self.worker_peer == other.worker_peer
            && self.namespace_id == other.namespace_id
            && self.product_key == other.product_key
            && self.work_id == other.work_id
            && self.assignment_attempt == other.assignment_attempt
            && self.assignment_fence == other.assignment_fence
            && self.trust == other.trust
    }
}

impl PendingAckCapturedWork {
    fn work_id(&self) -> [u8; 16] {
        backend_engine::compiler_cluster_transport::compiler_full_workspace_transfer_work_id(
            self.package_lineage,
            self.target,
            self.recipe,
            self.read_manifest,
            self.input_root,
            self.input_closure_id,
            self.manifest_object_id,
            self.max_output_bytes,
        )
    }

    pub(crate) fn reopen_expectation(
        &self,
        work_id: [u8; 16],
        trust: PendingStoredAckTrust,
    ) -> backend_engine::application::CapturedFullWorkspaceV2Expectation {
        backend_engine::application::CapturedFullWorkspaceV2Expectation {
            input_closure_id: self.input_closure_id,
            manifest_object_id: self.manifest_object_id,
            object_count: self.object_count,
            payload_bytes: self.payload_bytes,
            work_id,
            package_lineage: self.package_lineage,
            target: self.target,
            recipe: self.recipe,
            input_root: self.input_root,
            read_manifest: self.read_manifest,
            workspace_snapshot_id: self.workspace_snapshot_id,
            max_output_bytes: self.max_output_bytes,
            source_fence_digest: self.source_fence_digest,
            profile: trust.profile,
            stage: trust.stage,
            toolchain: trust.toolchain,
            environment: trust.environment,
            target_platform: trust.target_platform,
        }
    }

    pub(crate) fn recovery_claim(
        &self,
        namespace: backend_extension_turso::AuthorityNamespace,
    ) -> backend_extension_turso::CandidateAttemptRecoveryClaim {
        backend_extension_turso::CandidateAttemptRecoveryClaim::new(
            namespace,
            self.turso_epoch,
            self.turso_attempt_id,
            self.turso_fence,
            self.turso_input_digest,
            self.turso_base_generation,
            self.turso_base_root,
            self.source_observation_sequence,
        )
    }

    fn from_attempt_capture(
        attempt: &backend_extension_turso::CandidateAttempt,
        capture: &backend_engine::application::CapturedFullWorkspaceV2,
        source_root: &Path,
    ) -> Result<Self, PendingStoredAckError> {
        let claim = capture
            .manifest()
            .identity_claim()
            .map_err(|_| PendingStoredAckError::Invalid("captured V2 identity"))?;
        let observation = attempt.observation();
        let source_observation_revision =
            observation
                .observation()
                .revision()
                .ok_or(PendingStoredAckError::Invalid(
                    "source observation revision",
                ))?;
        if let backend_extension_turso::SourceObservationValue::KnownCount(count) =
            observation.observation().value()
        {
            let source_root = source_root
                .to_str()
                .ok_or(PendingStoredAckError::Invalid("source root encoding"))?;
            let work = Self {
                source_root: source_root.into(),
                package_lineage: claim.scope.package.as_bytes(),
                target: *claim.scope.target.as_ref(),
                recipe: *claim.scope.recipe.as_ref(),
                input_root: *claim.input_root.as_ref(),
                read_manifest: *claim.manifest.as_bytes(),
                workspace_snapshot_id: capture.manifest().workspace_snapshot_id().to_bytes(),
                max_output_bytes: capture.manifest().max_output_bytes(),
                input_closure_id: *capture.closure().as_bytes(),
                manifest_object_id: *capture.manifest_object_id().as_bytes(),
                object_count: capture.object_count(),
                payload_bytes: capture.payload_bytes(),
                source_fence_digest: capture.source_fence_digest(),
                source_observation_revision,
                source_observation_sequence: observation.sequence(),
                source_observation_observed_at_ms: observation.observation().observed_at_ms(),
                source_observation_count: *count,
                turso_attempt_id: *attempt.attempt_id(),
                turso_epoch: attempt.epoch(),
                turso_fence: attempt.fence_bytes(),
                turso_input_digest: *attempt.input_digest(),
                turso_base_generation: attempt.base_generation(),
                turso_base_root: attempt.base_root(),
            };
            work.validate()?;
            Ok(work)
        } else {
            Err(PendingStoredAckError::Invalid(
                "remote capture requires a known source observation",
            ))
        }
    }

    fn validate(&self) -> Result<(), PendingStoredAckError> {
        check_text(
            self.source_root.as_bytes(),
            MAX_SOURCE_ROOT_BYTES,
            "source root",
        )?;
        let root = Path::new(self.source_root.as_ref());
        if !root.is_absolute()
            || root
                .components()
                .any(|component| matches!(component, std::path::Component::ParentDir))
        {
            return Err(PendingStoredAckError::Invalid("source root path"));
        }
        if self.package_lineage == [0; 32]
            || self.target == [0; 32]
            || self.recipe == [0; 32]
            || self.input_root == [0; 32]
            || self.read_manifest == [0; 32]
            || self.workspace_snapshot_id == [0; 32]
            || self.max_output_bytes == 0
            || self.input_closure_id == [0; 32]
            || self.manifest_object_id == [0; 32]
            || self.object_count == 0
            || self.object_count > u64::from(MAX_WORKER_OBJECTS)
            || self.payload_bytes == 0
            || self.payload_bytes > MAX_WORKER_BYTES
            || self.source_fence_digest == [0; 32]
            || self.source_observation_revision == [0; 32]
            || self.source_observation_revision != self.input_root
            || self.turso_input_digest != self.input_root
            || self.source_observation_sequence == 0
            || self.source_observation_observed_at_ms == 0
            || self.source_observation_count == 0
            || self.turso_attempt_id == [0; 16]
            || self.turso_epoch == 0
            || self.turso_fence == [0; 32]
            || self.turso_input_digest == [0; 32]
        {
            return Err(PendingStoredAckError::Invalid("captured work identity"));
        }
        match (self.turso_base_generation, self.turso_base_root) {
            (0, None) => {}
            _ => return Err(PendingStoredAckError::Invalid("fresh capture has base")),
        }
        Ok(())
    }

    fn encode(&self, output: &mut Vec<u8>) -> Result<(), PendingStoredAckError> {
        self.validate()?;
        encode_text(output, self.source_root.as_bytes());
        output.extend_from_slice(&self.package_lineage);
        output.extend_from_slice(&self.target);
        output.extend_from_slice(&self.recipe);
        output.extend_from_slice(&self.input_root);
        output.extend_from_slice(&self.read_manifest);
        output.extend_from_slice(&self.workspace_snapshot_id);
        output.extend_from_slice(&self.max_output_bytes.to_be_bytes());
        output.extend_from_slice(&self.input_closure_id);
        output.extend_from_slice(&self.manifest_object_id);
        output.extend_from_slice(&self.object_count.to_be_bytes());
        output.extend_from_slice(&self.payload_bytes.to_be_bytes());
        output.extend_from_slice(&self.source_fence_digest);
        output.extend_from_slice(&self.source_observation_revision);
        output.extend_from_slice(&self.source_observation_sequence.to_be_bytes());
        output.extend_from_slice(&self.source_observation_observed_at_ms.to_be_bytes());
        output.extend_from_slice(&self.source_observation_count.to_be_bytes());
        output.extend_from_slice(&self.turso_attempt_id);
        output.extend_from_slice(&self.turso_epoch.to_be_bytes());
        output.extend_from_slice(&self.turso_fence);
        output.extend_from_slice(&self.turso_input_digest);
        output.extend_from_slice(&self.turso_base_generation.to_be_bytes());
        match self.turso_base_root {
            None => output.push(0),
            Some(root) => {
                output.push(1);
                output.extend_from_slice(&root);
            }
        }
        Ok(())
    }

    fn decode(cursor: &mut Cursor<'_>) -> Result<Self, PendingStoredAckError> {
        let value = Self {
            source_root: cursor.text(MAX_SOURCE_ROOT_BYTES, "source root")?,
            package_lineage: cursor.array()?,
            target: cursor.array()?,
            recipe: cursor.array()?,
            input_root: cursor.array()?,
            read_manifest: cursor.array()?,
            workspace_snapshot_id: cursor.array()?,
            max_output_bytes: cursor.u64()?,
            input_closure_id: cursor.array()?,
            manifest_object_id: cursor.array()?,
            object_count: cursor.u64()?,
            payload_bytes: cursor.u64()?,
            source_fence_digest: cursor.array()?,
            source_observation_revision: cursor.array()?,
            source_observation_sequence: cursor.u64()?,
            source_observation_observed_at_ms: cursor.u64()?,
            source_observation_count: cursor.u64()?,
            turso_attempt_id: cursor.array()?,
            turso_epoch: cursor.u64()?,
            turso_fence: cursor.array()?,
            turso_input_digest: cursor.array()?,
            turso_base_generation: cursor.u64()?,
            turso_base_root: match cursor.u8()? {
                0 => None,
                1 => Some(cursor.array()?),
                _ => return Err(PendingStoredAckError::Corrupt("base root option")),
            },
        };
        value.validate()?;
        Ok(value)
    }
}

impl PendingAckReservationRecord {
    fn validate(&self) -> Result<(), PendingStoredAckError> {
        if self.id == [0; 32] || self.owner_endpoint_id == [0; 32] {
            return Err(PendingStoredAckError::Invalid("reservation identity"));
        }
        EndpointId::from_bytes(&self.owner_endpoint_id)
            .map_err(|_| PendingStoredAckError::Invalid("owner endpoint"))?;
        match (self.stage, self.identity.as_ref()) {
            (PendingAckReservationStage::Unbound, None) if self.capture.is_none() => Ok(()),
            (
                PendingAckReservationStage::BoundBeforeOffer
                | PendingAckReservationStage::OfferMayBeSent,
                Some(identity),
            ) if identity.owner_endpoint_id == self.owner_endpoint_id => {
                identity.validate()?;
                let capture = self
                    .capture
                    .as_ref()
                    .ok_or(PendingStoredAckError::Invalid("bound capture facts"))?;
                capture.validate()?;
                if capture.recipe != identity.trust.recipe
                    || capture.turso_epoch != identity.assignment_attempt
                    || capture.turso_fence != identity.assignment_fence
                    || capture.workspace_snapshot_id != capture.read_manifest
                {
                    return Err(PendingStoredAckError::Invalid(
                        "capture differs from bound assignment",
                    ));
                }
                Ok(())
            }
            _ => Err(PendingStoredAckError::Invalid("reservation stage")),
        }
    }

    fn encode(&self) -> Result<Vec<u8>, PendingStoredAckError> {
        self.validate()?;
        let mut output = Vec::new();
        output.extend_from_slice(&self.id);
        output.extend_from_slice(&self.owner_endpoint_id);
        match self.stage {
            PendingAckReservationStage::Unbound => output.push(0),
            PendingAckReservationStage::BoundBeforeOffer => {
                output.push(1);
                let identity = self
                    .identity
                    .as_ref()
                    .ok_or(PendingStoredAckError::Invalid("bound reservation identity"))?;
                output.extend_from_slice(&identity.encode(true));
                self.capture
                    .as_ref()
                    .ok_or(PendingStoredAckError::Invalid("bound capture facts"))?
                    .encode(&mut output)?;
            }
            PendingAckReservationStage::OfferMayBeSent => {
                output.push(2);
                let identity = self
                    .identity
                    .as_ref()
                    .ok_or(PendingStoredAckError::Invalid(
                        "offered reservation identity",
                    ))?;
                output.extend_from_slice(&identity.encode(true));
                self.capture
                    .as_ref()
                    .ok_or(PendingStoredAckError::Invalid("offered capture facts"))?
                    .encode(&mut output)?;
            }
        }
        Ok(output)
    }

    fn decode(bytes: &[u8]) -> Result<Self, PendingStoredAckError> {
        let mut cursor = Cursor::new(bytes);
        let id = cursor.array()?;
        let owner_endpoint_id = cursor.array()?;
        let (stage, identity, capture) = match cursor.u8()? {
            0 => (PendingAckReservationStage::Unbound, None, None),
            1 => (
                PendingAckReservationStage::BoundBeforeOffer,
                Some(PendingAckAssignmentIdentity::decode(&mut cursor)?),
                Some(PendingAckCapturedWork::decode(&mut cursor)?),
            ),
            2 => (
                PendingAckReservationStage::OfferMayBeSent,
                Some(PendingAckAssignmentIdentity::decode(&mut cursor)?),
                Some(PendingAckCapturedWork::decode(&mut cursor)?),
            ),
            _ => return Err(PendingStoredAckError::Corrupt("reservation stage")),
        };
        cursor.finish()?;
        let row = Self {
            id,
            stage,
            owner_endpoint_id,
            identity,
            capture,
        };
        row.validate()?;
        Ok(row)
    }
}

impl PendingRejectedAckRecord {
    fn id(&self) -> [u8; 32] {
        let mut bytes = vec![0x52];
        bytes.extend_from_slice(&self.identity.encode(false));
        bytes.extend_from_slice(&self.closure_id);
        bytes.push(reject_reason_tag(self.reason));
        *blake3::hash(&bytes).as_bytes()
    }

    fn validate(&self) -> Result<(), PendingStoredAckError> {
        self.identity.validate()?;
        if self.closure_id == [0; 32]
            || !matches!(
                self.reason,
                backend_engine::cluster_transport::ResultRejectReason::Admission
                    | backend_engine::cluster_transport::ResultRejectReason::Scope
            )
        {
            return Err(PendingStoredAckError::Invalid("rejected result identity"));
        }
        Ok(())
    }

    fn encode(&self) -> Result<Vec<u8>, PendingStoredAckError> {
        self.validate()?;
        let mut output = vec![match self.state {
            PendingRejectedAckState::AckPending => 0,
            PendingRejectedAckState::AwaitingRetirementConfirm => 1,
        }];
        output.extend_from_slice(&self.identity.encode(true));
        output.extend_from_slice(&self.closure_id);
        output.push(reject_reason_tag(self.reason));
        Ok(output)
    }

    fn decode(bytes: &[u8]) -> Result<Self, PendingStoredAckError> {
        let mut cursor = Cursor::new(bytes);
        let state = match cursor.u8()? {
            0 => PendingRejectedAckState::AckPending,
            1 => PendingRejectedAckState::AwaitingRetirementConfirm,
            _ => return Err(PendingStoredAckError::Corrupt("rejection state")),
        };
        let identity = PendingAckAssignmentIdentity::decode(&mut cursor)?;
        let closure_id = cursor.array()?;
        let reason = reject_reason_from_tag(cursor.u8()?)?;
        cursor.finish()?;
        let row = Self {
            state,
            identity,
            closure_id,
            reason,
        };
        row.validate()?;
        Ok(row)
    }
}

fn assignment_scope(
    assignment: backend_engine::application::CompilerAssignment,
    namespace_id: [u8; 16],
) -> Result<AssignmentScope, PendingStoredAckError> {
    backend_engine::compiler_cluster_transport::compiler_assignment_scope(assignment, namespace_id)
        .map_err(|_| PendingStoredAckError::Invalid("assignment scope"))
}

fn assignment_matches_identity(
    assignment: backend_engine::application::CompilerAssignment,
    namespace_id: [u8; 16],
    identity: &PendingAckAssignmentIdentity,
) -> bool {
    let backend_engine::application::CompilerAssignmentRoute::Remote(peer) = assignment.route()
    else {
        return false;
    };
    let Ok(scope) = assignment_scope(assignment, namespace_id) else {
        return false;
    };
    namespace_id == identity.namespace_id
        && peer.as_bytes() == identity.worker_peer
        && *assignment.work().recipe().as_ref() == identity.trust.recipe
        && scope.work_id == identity.work_id
        && scope.attempt == identity.assignment_attempt
        && scope.fence == identity.assignment_fence
}

fn assignment_matches_capture(
    assignment: backend_engine::application::CompilerAssignment,
    capture: &PendingAckCapturedWork,
) -> bool {
    let work = assignment.work();
    let identity = work.input_identity();
    work.package().as_bytes() == capture.package_lineage
        && *work.target().as_ref() == capture.target
        && *work.recipe().as_ref() == capture.recipe
        && *identity.input_root.as_ref() == capture.input_root
        && *identity.manifest.as_bytes() == capture.read_manifest
        && work.selected_base().is_none()
        && work.max_output_bytes() == capture.max_output_bytes
}

fn identity_matches_selection(
    identity: &PendingAckAssignmentIdentity,
    row: &PendingStoredAckRecord,
) -> bool {
    identity.owner_endpoint_id == row.owner_endpoint_id
        && identity.worker_peer == row.worker_peer
        && identity.namespace_id == row.namespace_id
        && identity.product_key == row.product_key
        && identity.work_id == row.work_id
        && identity.assignment_attempt == row.assignment_attempt
        && identity.assignment_fence == row.assignment_fence
        && identity.trust == row.trust
}

fn prepared_selection_disposition(
    row: &PendingStoredAckRecord,
) -> Result<PreparedResultDisposition, PendingStoredAckError> {
    let peer = EndpointId::from_bytes(&row.worker_peer)
        .map_err(|_| PendingStoredAckError::Invalid("worker endpoint"))?;
    let scope = AssignmentScope::new(
        row.namespace_id,
        row.work_id,
        row.assignment_attempt,
        row.assignment_fence,
    )
    .map_err(|_| PendingStoredAckError::Invalid("assignment scope"))?;
    let disposition = match row.state {
        PendingStoredAckState::AwaitingSelection | PendingStoredAckState::StoredAckPending => {
            backend_engine::cluster_transport::ResultAckDisposition::Stored
        }
        PendingStoredAckState::SupersededAckPending => {
            backend_engine::cluster_transport::ResultAckDisposition::Rejected(
                backend_engine::cluster_transport::ResultRejectReason::Scope,
            )
        }
        PendingStoredAckState::StoredAwaitingRetirementConfirm => {
            backend_engine::cluster_transport::ResultAckDisposition::Stored
        }
        PendingStoredAckState::SupersededAwaitingRetirementConfirm => {
            backend_engine::cluster_transport::ResultAckDisposition::Rejected(
                backend_engine::cluster_transport::ResultRejectReason::Scope,
            )
        }
    };
    Ok(PreparedResultDisposition {
        journal_id: row.id(),
        scope,
        expected_peer: peer,
        worker_address: row.worker_address,
        closure_id: row.worker_closure_id,
        disposition,
    })
}

fn prepared_rejected_disposition(
    id: [u8; 32],
    row: &PendingRejectedAckRecord,
) -> Result<PreparedResultDisposition, PendingStoredAckError> {
    let peer = EndpointId::from_bytes(&row.identity.worker_peer)
        .map_err(|_| PendingStoredAckError::Invalid("worker endpoint"))?;
    let scope = AssignmentScope::new(
        row.identity.namespace_id,
        row.identity.work_id,
        row.identity.assignment_attempt,
        row.identity.assignment_fence,
    )
    .map_err(|_| PendingStoredAckError::Invalid("assignment scope"))?;
    Ok(PreparedResultDisposition {
        journal_id: id,
        scope,
        expected_peer: peer,
        worker_address: row.identity.worker_address,
        closure_id: row.closure_id,
        disposition: backend_engine::cluster_transport::ResultAckDisposition::Rejected(row.reason),
    })
}

fn same_rejection(left: &PendingRejectedAckRecord, right: &PendingRejectedAckRecord) -> bool {
    left.identity.same_assignment(&right.identity)
        && left.closure_id == right.closure_id
        && left.reason == right.reason
}

fn reject_reason_tag(reason: backend_engine::cluster_transport::ResultRejectReason) -> u8 {
    use backend_engine::cluster_transport::ResultRejectReason;
    match reason {
        ResultRejectReason::Scope => 0,
        ResultRejectReason::Closure => 1,
        ResultRejectReason::Metadata => 2,
        ResultRejectReason::Admission => 3,
    }
}

fn reject_reason_from_tag(
    tag: u8,
) -> Result<backend_engine::cluster_transport::ResultRejectReason, PendingStoredAckError> {
    use backend_engine::cluster_transport::ResultRejectReason;
    match tag {
        0 => Ok(ResultRejectReason::Scope),
        1 => Ok(ResultRejectReason::Closure),
        2 => Ok(ResultRejectReason::Metadata),
        3 => Ok(ResultRejectReason::Admission),
        _ => Err(PendingStoredAckError::Corrupt("rejection reason")),
    }
}

fn fill_random(bytes: &mut [u8]) -> Result<(), PendingStoredAckError> {
    #[cfg(unix)]
    {
        fs::File::open("/dev/urandom")?.read_exact(bytes)?;
        Ok(())
    }
    #[cfg(windows)]
    {
        backend_platform::win32::random::fill(bytes).map_err(PendingStoredAckError::Io)
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = bytes;
        Err(PendingStoredAckError::Invalid(
            "secure random source unavailable",
        ))
    }
}

impl PendingStoredAckEntry {
    fn id(&self) -> [u8; 32] {
        match self {
            Self::Reservation(row) => row.id,
            Self::Selection(row) => row.id(),
            Self::RejectedAdmission(row) => row.id(),
        }
    }

    fn validate(&self) -> Result<(), PendingStoredAckError> {
        match self {
            Self::Reservation(row) => row.validate(),
            Self::Selection(row) => row.validate(),
            Self::RejectedAdmission(row) => row.validate(),
        }
    }

    fn encode(&self) -> Result<Vec<u8>, PendingStoredAckError> {
        let (tag, body) = match self {
            Self::Selection(row) => (1, row.encode()?),
            Self::RejectedAdmission(row) => (2, row.encode()?),
            Self::Reservation(row) => {
                let tag = match row.stage {
                    PendingAckReservationStage::Unbound => 3,
                    PendingAckReservationStage::BoundBeforeOffer => 4,
                    PendingAckReservationStage::OfferMayBeSent => 5,
                };
                (tag, row.encode()?)
            }
        };
        let mut output = Vec::with_capacity(body.len() + 1);
        output.push(tag);
        output.extend_from_slice(&body);
        Ok(output)
    }

    fn decode(bytes: &[u8]) -> Result<Self, PendingStoredAckError> {
        let (&tag, body) = bytes
            .split_first()
            .ok_or(PendingStoredAckError::Corrupt("entry tag"))?;
        match tag {
            1 => Ok(Self::Selection(PendingStoredAckRecord::decode(body)?)),
            2 => Ok(Self::RejectedAdmission(PendingRejectedAckRecord::decode(
                body,
            )?)),
            3..=5 => {
                let row = PendingAckReservationRecord::decode(body)?;
                let expected_tag = match row.stage {
                    PendingAckReservationStage::Unbound => 3,
                    PendingAckReservationStage::BoundBeforeOffer => 4,
                    PendingAckReservationStage::OfferMayBeSent => 5,
                };
                if tag != expected_tag {
                    return Err(PendingStoredAckError::Corrupt("reservation tag"));
                }
                Ok(Self::Reservation(row))
            }
            _ => Err(PendingStoredAckError::Corrupt("entry tag")),
        }
    }
}

/// Bounded crash-safe journal of remote result acknowledgements and capacity.
#[derive(Debug)]
pub(crate) struct PendingStoredAckJournal {
    path: PathBuf,
    rows: BTreeMap<[u8; 32], PendingStoredAckEntry>,
    /// Process-local guard against retrying an AwaitingSelection row while
    /// its foreground compare-and-select call is still in flight. It is
    /// deliberately not encoded, so cold open always retries durable rows.
    in_flight_selections: BTreeSet<[u8; 32]>,
}

/// Process-local exclusion for a selection intent while its owner is between
/// durable journal prepare and the Turso compare-and-select result. Dropping
/// it makes the durable row retryable on every return path, including unwind.
pub(crate) struct PendingSelectionAttemptGuard {
    journal: Arc<Mutex<PendingStoredAckJournal>>,
    id: [u8; 32],
}

fn gc_retention_closure_ids(
    rows: &BTreeMap<[u8; 32], PendingStoredAckEntry>,
) -> impl Iterator<Item = [u8; 32]> + '_ {
    rows.values()
        .filter_map(|entry| match entry {
            PendingStoredAckEntry::Reservation(row)
                if row.stage == PendingAckReservationStage::OfferMayBeSent =>
            {
                row.capture
                    .as_ref()
                    .map(|capture| (capture.input_closure_id, None))
            }
            PendingStoredAckEntry::Selection(row) => Some((
                row.worker_closure_id,
                Some(row.captured_work.input_closure_id),
            )),
            PendingStoredAckEntry::RejectedAdmission(row) => Some((row.closure_id, None)),
            PendingStoredAckEntry::Reservation(_) => None,
        })
        .flat_map(|(first, second)| std::iter::once(first).chain(second))
}

impl Drop for PendingSelectionAttemptGuard {
    fn drop(&mut self) {
        self.journal
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .in_flight_selections
            .remove(&self.id);
    }
}

impl PendingStoredAckJournal {
    /// Opens or creates the owner-private journal directory below `workspace`.
    pub(crate) fn open(workspace: &Path) -> Result<Self, PendingStoredAckError> {
        let directory = workspace.join(JOURNAL_DIRECTORY);
        backend_platform::durable::ensure_private_directory(&directory)?;
        let path = directory.join(JOURNAL_FILE);
        let mut rows = match backend_platform::durable::open_private_read(&path) {
            Ok(mut file) => {
                let mut bytes = Vec::new();
                file.by_ref()
                    .take((MAX_FILE_BYTES + 1) as u64)
                    .read_to_end(&mut bytes)?;
                if bytes.len() > MAX_FILE_BYTES {
                    return Err(PendingStoredAckError::Limit);
                }
                decode_rows(&bytes)?
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => BTreeMap::new(),
            Err(error) => return Err(error.into()),
        };
        let initial_count = rows.len();
        rows.retain(|_, entry| {
            !matches!(
                entry,
                PendingStoredAckEntry::Reservation(PendingAckReservationRecord {
                    stage: PendingAckReservationStage::Unbound
                        | PendingAckReservationStage::BoundBeforeOffer,
                    ..
                })
            )
        });
        let mut journal = Self {
            path,
            rows,
            in_flight_selections: BTreeSet::new(),
        };
        if journal.rows.len() != initial_count {
            // Unbound capacity and bound-but-not-offered assignments cannot
            // have produced a worker result. Reclaim them after a cold restart.
            journal.publish(journal.rows.clone())?;
        }
        Ok(journal)
    }

    /// Returns the live rows in stable identity order.
    pub(crate) fn rows(&self) -> impl Iterator<Item = (&[u8; 32], &PendingStoredAckRecord)> {
        self.rows.iter().filter_map(|(id, entry)| match entry {
            PendingStoredAckEntry::Selection(row) => Some((id, row)),
            PendingStoredAckEntry::Reservation(_) | PendingStoredAckEntry::RejectedAdmission(_) => {
                None
            }
        })
    }

    /// Returns every closure whose payload can still be needed to recover an
    /// unresolved owner result or ACK. These identities are retention claims
    /// only; callers must still open the local closure index before rooting
    /// them, and they confer no selection or acknowledgement authority.
    pub(crate) fn gc_retention_closure_ids(&self) -> impl Iterator<Item = [u8; 32]> + '_ {
        gc_retention_closure_ids(&self.rows)
    }

    /// Reads closure retention claims from the durable journal without
    /// opening a writable journal or pruning rows. GC calls this while it owns
    /// the exclusive FileStore lease, so a concurrent publisher is either
    /// represented by its durable row or still holds the shared publication
    /// pin that keeps its objects out of the sweep.
    pub(crate) fn read_gc_retention_closure_ids(
        workspace: &Path,
    ) -> Result<Vec<[u8; 32]>, PendingStoredAckError> {
        let path = workspace.join(JOURNAL_DIRECTORY).join(JOURNAL_FILE);
        let mut file = match backend_platform::durable::open_private_read(&path) {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => return Err(error.into()),
        };
        let mut bytes = Vec::new();
        file.by_ref()
            .take((MAX_FILE_BYTES + 1) as u64)
            .read_to_end(&mut bytes)?;
        if bytes.len() > MAX_FILE_BYTES {
            return Err(PendingStoredAckError::Limit);
        }
        let rows = decode_rows(&bytes)?;
        Ok(gc_retention_closure_ids(&rows).collect())
    }

    pub(crate) fn retry_rows(&self) -> impl Iterator<Item = (&[u8; 32], &PendingStoredAckEntry)> {
        self.rows.iter().filter(|(id, entry)| match entry {
            PendingStoredAckEntry::Reservation(row) => {
                row.stage == PendingAckReservationStage::OfferMayBeSent
            }
            PendingStoredAckEntry::Selection(_) => !self.in_flight_selections.contains(*id),
            PendingStoredAckEntry::RejectedAdmission(_) => true,
        })
    }

    /// Marks an already-persisted AwaitingSelection row as owned by a cold
    /// recovery attempt, atomically with respect to the retry iterator.
    pub(crate) fn guard_selection_attempt(
        journal: &Arc<Mutex<Self>>,
        id: [u8; 32],
    ) -> Result<PendingSelectionAttemptGuard, PendingStoredAckError> {
        let mut current = journal
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        match current.rows.get(&id) {
            Some(PendingStoredAckEntry::Selection(row))
                if row.state == PendingStoredAckState::AwaitingSelection => {}
            Some(PendingStoredAckEntry::Selection(_)) => {
                return Err(PendingStoredAckError::TerminalState);
            }
            _ => return Err(PendingStoredAckError::MissingRow),
        }
        if !current.in_flight_selections.insert(id) {
            return Err(PendingStoredAckError::Invalid(
                "selection attempt is already in flight",
            ));
        }
        Ok(PendingSelectionAttemptGuard {
            journal: Arc::clone(journal),
            id,
        })
    }

    /// Persists a selection intent and installs its retry exclusion under one
    /// journal lock, leaving no window where the background retry can race the
    /// foreground compare-and-select.
    pub(crate) fn prepare_selection_guarded(
        journal: &Arc<Mutex<Self>>,
        reservation: PendingAckCapacityReservation,
        row: PendingStoredAckRecord,
    ) -> Result<(PreparedResultDisposition, PendingSelectionAttemptGuard), PendingStoredAckError>
    {
        let id = row.id();
        let mut current = journal
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let prepared = current.prepare_selection(reservation, row)?;
        Ok((
            prepared,
            PendingSelectionAttemptGuard {
                journal: Arc::clone(journal),
                id,
            },
        ))
    }

    /// Copies one offered reservation's persisted claims for cold recovery.
    /// Selection authority remains with Turso and CAS admission.
    pub(crate) fn offered_recovery_claim(
        &self,
        id: &[u8; 32],
    ) -> Result<OfferedReservationClaims, PendingStoredAckError> {
        let PendingStoredAckEntry::Reservation(row) =
            self.rows.get(id).ok_or(PendingStoredAckError::MissingRow)?
        else {
            return Err(PendingStoredAckError::TerminalState);
        };
        row.validate()?;
        if row.stage != PendingAckReservationStage::OfferMayBeSent {
            return Err(PendingStoredAckError::TerminalState);
        }
        Ok(OfferedReservationClaims {
            id: row.id,
            owner_endpoint_id: row.owner_endpoint_id,
            identity: row
                .identity
                .clone()
                .ok_or(PendingStoredAckError::TerminalState)?,
            captured_work: row
                .capture
                .clone()
                .ok_or(PendingStoredAckError::TerminalState)?,
        })
    }

    /// Recreates the opaque capacity token for one durable OfferMayBeSent row.
    /// The token is valid only while that exact row remains in this journal.
    pub(crate) fn reservation_for_recovery(
        &self,
        id: &[u8; 32],
    ) -> Result<PendingAckCapacityReservation, PendingStoredAckError> {
        let PendingStoredAckEntry::Reservation(row) =
            self.rows.get(id).ok_or(PendingStoredAckError::MissingRow)?
        else {
            return Err(PendingStoredAckError::TerminalState);
        };
        row.validate()?;
        if row.stage != PendingAckReservationStage::OfferMayBeSent {
            return Err(PendingStoredAckError::TerminalState);
        }
        Ok(PendingAckCapacityReservation { id: row.id })
    }

    /// Confirms that a cold-recovered candidate is the exact immutable result
    /// whose selection intent was fsynced before the owner crashed. This is a
    /// comparison only; the caller still has to revalidate source, Turso, CAS,
    /// and the V2 result before invoking compare-and-select.
    pub(crate) fn check_awaiting_selection_candidate(
        &self,
        id: &[u8; 32],
        candidate: &backend_extension_turso::CandidateGeneration,
    ) -> Result<(), PendingStoredAckError> {
        let PendingStoredAckEntry::Selection(row) =
            self.rows.get(id).ok_or(PendingStoredAckError::MissingRow)?
        else {
            return Err(PendingStoredAckError::TerminalState);
        };
        row.validate()?;
        let attempt = candidate.attempt();
        if row.state != PendingStoredAckState::AwaitingSelection
            || candidate.candidate_id() != &row.candidate_id
            || candidate.target_root() != &row.target_root
            || candidate.closure_id() != &row.selected_closure_id
            || attempt.namespace().namespace_id() != row.namespace_id
            || attempt.epoch() != row.turso_attempt_epoch
            || attempt.attempt_id() != &row.turso_attempt_id
            || attempt.fence_bytes() != row.turso_fence
            || attempt.input_digest() != &row.input_digest
            || attempt.base_generation().checked_add(1) != Some(row.turso_generation)
            || attempt.base_root() != row.captured_work.turso_base_root
            || attempt.observation().sequence() != row.captured_work.source_observation_sequence
            || attempt.observation().observation().revision()
                != Some(row.captured_work.source_observation_revision)
        {
            return Err(PendingStoredAckError::Invalid(
                "recovered candidate differs from awaiting selection intent",
            ));
        }
        Ok(())
    }

    pub(crate) fn pending_ack_count(&self) -> usize {
        self.rows
            .values()
            .filter(|entry| !matches!(entry, PendingStoredAckEntry::Reservation(_)))
            .count()
    }

    pub(crate) fn capacity_count(&self) -> usize {
        self.rows.len()
    }

    /// Offered assignments whose result/terminal state was not durably
    /// reconciled. These rows are retained and consume capacity after restart.
    pub(crate) fn offered_reservation_count(&self) -> usize {
        self.rows
            .values()
            .filter(|entry| {
                matches!(
                    entry,
                    PendingStoredAckEntry::Reservation(PendingAckReservationRecord {
                        stage: PendingAckReservationStage::OfferMayBeSent,
                        ..
                    })
                )
            })
            .count()
    }

    /// Reserves one bounded durable slot before a compiler assignment can be
    /// offered. The random token is an opaque capability to release or bind
    /// only the row created by this call.
    pub(crate) fn reserve_capacity(
        &mut self,
        owner_endpoint_id: [u8; 32],
    ) -> Result<PendingAckCapacityReservation, PendingStoredAckError> {
        if owner_endpoint_id == [0; 32] {
            return Err(PendingStoredAckError::Invalid("owner endpoint"));
        }
        if self.rows.len() >= MAX_ROWS {
            return Err(PendingStoredAckError::CapacityFull);
        }
        let mut replacement = self.rows.clone();
        loop {
            let mut id = [0; 32];
            fill_random(&mut id)?;
            if id == [0; 32] || replacement.contains_key(&id) {
                continue;
            }
            let row = PendingAckReservationRecord {
                id,
                stage: PendingAckReservationStage::Unbound,
                owner_endpoint_id,
                identity: None,
                capture: None,
            };
            row.validate()?;
            replacement.insert(id, PendingStoredAckEntry::Reservation(row));
            self.publish(replacement)?;
            return Ok(PendingAckCapacityReservation { id });
        }
    }

    /// Binds a preflight reservation to the exact remote scheduler assignment
    /// and persisted worker grant before input admission begins.
    pub(crate) fn bind_assignment(
        &mut self,
        reservation: PendingAckCapacityReservation,
        assignment: backend_engine::application::CompilerAssignment,
        namespace_id: [u8; 16],
        product_key: PendingStoredAckProductKey,
        grant: &crate::compiler_trust::TrustedCompilerWorkerGrant,
        attempt: &backend_extension_turso::CandidateAttempt,
        capture: &backend_engine::application::CapturedFullWorkspaceV2,
        source_root: &Path,
    ) -> Result<(), PendingStoredAckError> {
        let id = reservation.id();
        let current = match self
            .rows
            .get(&id)
            .ok_or(PendingStoredAckError::MissingRow)?
        {
            PendingStoredAckEntry::Reservation(row) => row,
            _ => return Err(PendingStoredAckError::TerminalState),
        };
        let identity = PendingAckAssignmentIdentity::from_assignment(
            current.owner_endpoint_id,
            assignment,
            namespace_id,
            product_key,
            grant,
        )?;
        let captured_work =
            PendingAckCapturedWork::from_attempt_capture(attempt, capture, source_root)?;
        if attempt.namespace().namespace_id() != namespace_id
            || captured_work.turso_epoch != identity.assignment_attempt
            || captured_work.turso_fence != identity.assignment_fence
            || captured_work.recipe != identity.trust.recipe
            || !assignment_matches_capture(assignment, &captured_work)
            || assignment.work().transfer_work_id() != identity.work_id
        {
            return Err(PendingStoredAckError::Invalid(
                "captured input differs from assignment authority",
            ));
        }
        if current.stage == PendingAckReservationStage::BoundBeforeOffer
            && current
                .identity
                .as_ref()
                .is_some_and(|existing| existing.same_assignment(&identity))
            && current.capture.as_ref() == Some(&captured_work)
        {
            return Ok(());
        }
        if current.stage != PendingAckReservationStage::Unbound {
            return Err(PendingStoredAckError::TerminalState);
        }
        let mut replacement = self.rows.clone();
        let PendingStoredAckEntry::Reservation(row) = replacement
            .get_mut(&id)
            .ok_or(PendingStoredAckError::MissingRow)?
        else {
            return Err(PendingStoredAckError::TerminalState);
        };
        row.stage = PendingAckReservationStage::BoundBeforeOffer;
        row.identity = Some(identity);
        row.capture = Some(captured_work);
        row.validate()?;
        self.publish(replacement)
    }

    /// Persists that the first Offer may be visible to the worker. This is a
    /// conservative crash boundary: such a row is never reclaimed by age or
    /// cold-start cleanup because the worker may retain a result.
    pub(crate) fn mark_offer_may_be_sent(
        &mut self,
        reservation: PendingAckCapacityReservation,
        assignment: backend_engine::application::CompilerAssignment,
        namespace_id: [u8; 16],
    ) -> Result<(), PendingStoredAckError> {
        let id = reservation.id();
        let current = match self
            .rows
            .get(&id)
            .ok_or(PendingStoredAckError::MissingRow)?
        {
            PendingStoredAckEntry::Reservation(row) => row,
            _ => return Err(PendingStoredAckError::TerminalState),
        };
        let identity = current
            .identity
            .as_ref()
            .ok_or(PendingStoredAckError::TerminalState)?;
        let capture = current
            .capture
            .as_ref()
            .ok_or(PendingStoredAckError::TerminalState)?;
        if current.stage == PendingAckReservationStage::OfferMayBeSent
            && assignment_matches_identity(assignment, namespace_id, identity)
            && assignment_matches_capture(assignment, capture)
        {
            return Ok(());
        }
        if current.stage != PendingAckReservationStage::BoundBeforeOffer
            || !assignment_matches_identity(assignment, namespace_id, identity)
            || !assignment_matches_capture(assignment, capture)
        {
            return Err(PendingStoredAckError::Invalid("offered assignment differs"));
        }
        let mut replacement = self.rows.clone();
        let PendingStoredAckEntry::Reservation(row) = replacement
            .get_mut(&id)
            .ok_or(PendingStoredAckError::MissingRow)?
        else {
            return Err(PendingStoredAckError::TerminalState);
        };
        row.stage = PendingAckReservationStage::OfferMayBeSent;
        row.validate()?;
        self.publish(replacement)
    }

    /// Releases a reservation only while no Offer could have reached a worker.
    pub(crate) fn release_before_offer(
        &mut self,
        reservation: PendingAckCapacityReservation,
    ) -> Result<(), PendingStoredAckError> {
        let id = reservation.id();
        let current = match self
            .rows
            .get(&id)
            .ok_or(PendingStoredAckError::MissingRow)?
        {
            PendingStoredAckEntry::Reservation(row) => row,
            _ => return Err(PendingStoredAckError::TerminalState),
        };
        if !matches!(
            current.stage,
            PendingAckReservationStage::Unbound | PendingAckReservationStage::BoundBeforeOffer
        ) {
            return Err(PendingStoredAckError::TerminalState);
        }
        let mut replacement = self.rows.clone();
        replacement.remove(&id);
        self.publish(replacement)
    }

    /// Releases an offered reservation only after a validated authenticated
    /// terminal message proves no result is retained for this assignment.
    pub(crate) fn release_after_no_result_terminal(
        &mut self,
        reservation: PendingAckCapacityReservation,
        assignment: backend_engine::application::CompilerAssignment,
        namespace_id: [u8; 16],
    ) -> Result<(), PendingStoredAckError> {
        let id = reservation.id();
        let current = match self
            .rows
            .get(&id)
            .ok_or(PendingStoredAckError::MissingRow)?
        {
            PendingStoredAckEntry::Reservation(row) => row,
            _ => return Err(PendingStoredAckError::TerminalState),
        };
        let Some(identity) = current.identity.as_ref() else {
            return Err(PendingStoredAckError::TerminalState);
        };
        let Some(capture) = current.capture.as_ref() else {
            return Err(PendingStoredAckError::TerminalState);
        };
        if current.stage != PendingAckReservationStage::OfferMayBeSent
            || !assignment_matches_identity(assignment, namespace_id, identity)
            || !assignment_matches_capture(assignment, capture)
        {
            return Err(PendingStoredAckError::Invalid(
                "terminal assignment differs",
            ));
        }
        let mut replacement = self.rows.clone();
        replacement.remove(&id);
        self.publish(replacement)
    }

    /// Reclaims an offered reservation only after a scope-matched worker
    /// recovery status durably fences delayed duplicate Offers as NoResult.
    pub(crate) fn release_after_recovered_no_result(
        &mut self,
        reservation: PendingAckCapacityReservation,
        assignment: backend_engine::application::CompilerAssignment,
        namespace_id: [u8; 16],
        status: &backend_engine::cluster_transport::ControlResultRecoveryStatus,
    ) -> Result<(), PendingStoredAckError> {
        let row = match self
            .rows
            .get(&reservation.id())
            .ok_or(PendingStoredAckError::MissingRow)?
        {
            PendingStoredAckEntry::Reservation(row) => row,
            _ => return Err(PendingStoredAckError::TerminalState),
        };
        let identity = row
            .identity
            .as_ref()
            .ok_or(PendingStoredAckError::TerminalState)?;
        let capture = row
            .capture
            .as_ref()
            .ok_or(PendingStoredAckError::TerminalState)?;
        let scope = assignment_scope(assignment, namespace_id)?;
        let owner = EndpointId::from_bytes(&identity.owner_endpoint_id)
            .map_err(|_| PendingStoredAckError::Invalid("owner endpoint"))?;
        let worker = EndpointId::from_bytes(&identity.worker_peer)
            .map_err(|_| PendingStoredAckError::Invalid("worker endpoint"))?;
        let query =
            backend_engine::cluster_transport::ControlResultRecoveryQuery::new(scope, owner)
                .map_err(|_| PendingStoredAckError::Invalid("recovery query"))?;
        if row.stage != PendingAckReservationStage::OfferMayBeSent
            || !assignment_matches_identity(assignment, namespace_id, identity)
            || !assignment_matches_capture(assignment, capture)
            || !status.matches_query(&query, owner, worker)
            || !matches!(
                status.state(),
                backend_engine::cluster_transport::ControlResultRecoveryState::NoResult
            )
        {
            return Err(PendingStoredAckError::Invalid(
                "recovery status does not prove exact no-result terminal",
            ));
        }
        let mut replacement = self.rows.clone();
        replacement.remove(&reservation.id());
        self.publish(replacement)
    }

    /// Promotes the exact assignment reservation to a selection row in one
    /// atomic journal replacement before Turso compare-and-select.
    pub(crate) fn prepare_selection(
        &mut self,
        reservation: PendingAckCapacityReservation,
        row: PendingStoredAckRecord,
    ) -> Result<PreparedResultDisposition, PendingStoredAckError> {
        if row.state != PendingStoredAckState::AwaitingSelection {
            return Err(PendingStoredAckError::Invalid("selection state"));
        }
        row.validate()?;
        let reservation_id = reservation.id();
        let current = match self
            .rows
            .get(&reservation_id)
            .ok_or(PendingStoredAckError::MissingRow)?
        {
            PendingStoredAckEntry::Reservation(row) => row,
            _ => return Err(PendingStoredAckError::TerminalState),
        };
        let bound = current
            .identity
            .as_ref()
            .ok_or(PendingStoredAckError::TerminalState)?;
        let capture = current
            .capture
            .as_ref()
            .ok_or(PendingStoredAckError::TerminalState)?;
        if current.stage != PendingAckReservationStage::OfferMayBeSent
            || !identity_matches_selection(bound, &row)
            || row.input_digest != capture.turso_input_digest
            || row.turso_attempt_id != capture.turso_attempt_id
            || row.turso_attempt_epoch != capture.turso_epoch
            || row.turso_fence != capture.turso_fence
        {
            return Err(PendingStoredAckError::Invalid(
                "selection differs from reservation",
            ));
        }
        let id = row.id();
        let mut replacement = self.rows.clone();
        if let Some(existing) = replacement.get(&id) {
            if matches!(existing, PendingStoredAckEntry::Selection(existing) if same_identity(existing, &row))
            {
                replacement.remove(&reservation_id);
                self.publish(replacement)?;
                self.in_flight_selections.insert(id);
                return prepared_selection_disposition(&row);
            }
            if id != reservation_id {
                return Err(PendingStoredAckError::Corrupt(
                    "duplicate selection identity",
                ));
            }
        }
        replacement.remove(&reservation_id);
        replacement.insert(id, PendingStoredAckEntry::Selection(row));
        self.publish(replacement)?;
        self.in_flight_selections.insert(id);
        prepared_selection_disposition(match self.rows.get(&id) {
            Some(PendingStoredAckEntry::Selection(row)) => row,
            _ => return Err(PendingStoredAckError::MissingRow),
        })
    }

    /// Promotes a reservation to a durable owner-side rejection before its
    /// terminal ACK. Only Admission and proof-backed Scope are supported.
    /// Duplicate exact results reuse the existing row and do not consume
    /// another capacity slot.
    pub(crate) fn prepare_rejection(
        &mut self,
        reservation: PendingAckCapacityReservation,
        result: &super::cluster_dispatch::CompilerResultIdentity,
        disposition: backend_engine::cluster_transport::ResultAckDisposition,
    ) -> Result<PreparedResultDisposition, PendingStoredAckError> {
        let backend_engine::cluster_transport::ResultAckDisposition::Rejected(reason) = disposition
        else {
            return Err(PendingStoredAckError::Invalid("rejection disposition"));
        };
        if !matches!(
            reason,
            backend_engine::cluster_transport::ResultRejectReason::Admission
                | backend_engine::cluster_transport::ResultRejectReason::Scope
        ) {
            return Err(PendingStoredAckError::Invalid("rejection disposition"));
        }
        let reservation_id = reservation.id();
        let current = match self
            .rows
            .get(&reservation_id)
            .ok_or(PendingStoredAckError::MissingRow)?
        {
            PendingStoredAckEntry::Reservation(row) => row,
            _ => return Err(PendingStoredAckError::TerminalState),
        };
        let bound = current
            .identity
            .as_ref()
            .ok_or(PendingStoredAckError::TerminalState)?;
        let capture = current
            .capture
            .as_ref()
            .ok_or(PendingStoredAckError::TerminalState)?;
        let identity = PendingAckAssignmentIdentity::from_result_identity(
            current.owner_endpoint_id,
            result,
            bound.product_key.clone(),
        )?;
        if current.stage != PendingAckReservationStage::OfferMayBeSent
            || !bound.same_assignment(&identity)
            || !assignment_matches_identity(result.assignment, result.namespace_id, bound)
            || !assignment_matches_capture(result.assignment, capture)
            || result.assignment.work().transfer_work_id() != bound.work_id
        {
            return Err(PendingStoredAckError::Invalid(
                "rejection differs from reservation",
            ));
        }
        let rejection = PendingRejectedAckRecord {
            state: PendingRejectedAckState::AckPending,
            identity,
            closure_id: result.closure_id,
            reason,
        };
        rejection.validate()?;
        let id = rejection.id();
        let mut replacement = self.rows.clone();
        if let Some(existing) = replacement.get(&id) {
            if matches!(existing, PendingStoredAckEntry::RejectedAdmission(existing) if same_rejection(existing, &rejection))
            {
                replacement.remove(&reservation_id);
                self.publish(replacement)?;
                return prepared_rejected_disposition(id, &rejection);
            }
            return Err(PendingStoredAckError::Corrupt(
                "duplicate rejection identity",
            ));
        }
        replacement.remove(&reservation_id);
        replacement.insert(
            id,
            PendingStoredAckEntry::RejectedAdmission(rejection.clone()),
        );
        self.publish(replacement)?;
        prepared_rejected_disposition(id, &rejection)
    }

    /// Converts an exact, still-unselected selection intent to a durable
    /// admission rejection when the pre-CAS source recheck fails. This is
    /// permitted only while the callback has not returned to Turso's CAS;
    /// callers must never use it after an uncertain compare-and-select.
    pub(crate) fn prepare_rejection_from_selection(
        &mut self,
        selection_id: [u8; 32],
        result: &super::cluster_dispatch::CompilerResultIdentity,
        disposition: backend_engine::cluster_transport::ResultAckDisposition,
    ) -> Result<PreparedResultDisposition, PendingStoredAckError> {
        let backend_engine::cluster_transport::ResultAckDisposition::Rejected(reason) = disposition
        else {
            return Err(PendingStoredAckError::Invalid("rejection disposition"));
        };
        if !matches!(
            reason,
            backend_engine::cluster_transport::ResultRejectReason::Admission
                | backend_engine::cluster_transport::ResultRejectReason::Scope
        ) {
            return Err(PendingStoredAckError::Invalid("rejection disposition"));
        }
        let current = match self
            .rows
            .get(&selection_id)
            .ok_or(PendingStoredAckError::MissingRow)?
        {
            PendingStoredAckEntry::Selection(row) => row,
            _ => return Err(PendingStoredAckError::TerminalState),
        };
        if current.state != PendingStoredAckState::AwaitingSelection
            || result.closure_id != current.worker_closure_id
            || result.worker_grant.address() != current.worker_address
            || result.worker_grant.peer().as_bytes() != &current.worker_peer
            || !assignment_matches_identity(
                result.assignment,
                result.namespace_id,
                &PendingAckAssignmentIdentity {
                    owner_endpoint_id: current.owner_endpoint_id,
                    worker_peer: current.worker_peer,
                    worker_address: result.worker_grant.address(),
                    namespace_id: current.namespace_id,
                    product_key: current.product_key.clone(),
                    work_id: current.work_id,
                    assignment_attempt: current.assignment_attempt,
                    assignment_fence: current.assignment_fence,
                    trust: current.trust,
                },
            )
        {
            return Err(PendingStoredAckError::Invalid(
                "preselection rejection differs from pending selection",
            ));
        }
        let identity = PendingAckAssignmentIdentity::from_result_identity(
            current.owner_endpoint_id,
            result,
            current.product_key.clone(),
        )?;
        if !identity_matches_selection(&identity, current) {
            return Err(PendingStoredAckError::Invalid(
                "preselection rejection assignment differs",
            ));
        }
        let rejected = PendingRejectedAckRecord {
            state: PendingRejectedAckState::AckPending,
            identity,
            closure_id: result.closure_id,
            reason,
        };
        rejected.validate()?;
        let rejected_id = rejected.id();
        let mut replacement = self.rows.clone();
        if let Some(existing) = replacement.get(&rejected_id) {
            if !matches!(existing, PendingStoredAckEntry::RejectedAdmission(row) if same_rejection(row, &rejected))
            {
                return Err(PendingStoredAckError::Corrupt(
                    "duplicate preselection rejection identity",
                ));
            }
        }
        replacement.remove(&selection_id);
        replacement.insert(
            rejected_id,
            PendingStoredAckEntry::RejectedAdmission(rejected.clone()),
        );
        self.publish(replacement)?;
        self.in_flight_selections.remove(&selection_id);
        prepared_rejected_disposition(rejected_id, &rejected)
    }

    /// Persists the exact authenticated worker-retirement receipt for a
    /// rejected result before the owner sends its final confirmation.
    pub(crate) fn mark_rejected_awaiting_confirm(
        &mut self,
        result: &super::cluster_dispatch::CompilerResultIdentity,
        retired: &backend_engine::cluster_transport::ControlResultRetired,
    ) -> Result<(), PendingStoredAckError> {
        let mut matching = self.rows.iter().filter_map(|(id, entry)| {
            let PendingStoredAckEntry::RejectedAdmission(row) = entry else {
                return None;
            };
            let identity = PendingAckAssignmentIdentity::from_result_identity(
                row.identity.owner_endpoint_id,
                result,
                row.identity.product_key.clone(),
            )
            .ok()?;
            (row.identity.same_assignment(&identity) && row.closure_id == result.closure_id)
                .then_some((*id, row))
        });
        let (id, row) = matching.next().ok_or(PendingStoredAckError::MissingRow)?;
        if matching.next().is_some() {
            return Err(PendingStoredAckError::Corrupt("ambiguous rejected result"));
        }
        let expected_ack = backend_engine::cluster_transport::ControlResultAck {
            scope: assignment_scope(result.assignment, result.namespace_id)?,
            closure_id: result.closure_id,
            disposition: backend_engine::cluster_transport::ResultAckDisposition::Rejected(
                row.reason,
            ),
        };
        let peer = EndpointId::from_bytes(&row.identity.worker_peer)
            .map_err(|_| PendingStoredAckError::Invalid("worker endpoint"))?;
        if !retired.matches_ack(&expected_ack, peer) {
            return Err(PendingStoredAckError::Invalid(
                "rejection retirement receipt",
            ));
        }
        if row.state == PendingRejectedAckState::AwaitingRetirementConfirm {
            return Ok(());
        }
        let mut replacement = self.rows.clone();
        let PendingStoredAckEntry::RejectedAdmission(row) = replacement
            .get_mut(&id)
            .ok_or(PendingStoredAckError::MissingRow)?
        else {
            return Err(PendingStoredAckError::TerminalState);
        };
        row.state = PendingRejectedAckState::AwaitingRetirementConfirm;
        row.validate()?;
        self.publish(replacement)
    }

    /// Persists a rejected-result retirement receipt using only a journal row
    /// ID captured from this journal and the authenticated receipt. This is
    /// the cold-retry form, where no live `CompilerAssignment` object exists.
    pub(crate) fn mark_rejected_awaiting_confirm_by_id(
        &mut self,
        id: [u8; 32],
        retired: &backend_engine::cluster_transport::ControlResultRetired,
    ) -> Result<(), PendingStoredAckError> {
        let row = match self
            .rows
            .get(&id)
            .ok_or(PendingStoredAckError::MissingRow)?
        {
            PendingStoredAckEntry::RejectedAdmission(row) => row,
            _ => return Err(PendingStoredAckError::TerminalState),
        };
        row.validate()?;
        let expected_ack = backend_engine::cluster_transport::ControlResultAck {
            scope: AssignmentScope::new(
                row.identity.namespace_id,
                row.identity.work_id,
                row.identity.assignment_attempt,
                row.identity.assignment_fence,
            )
            .map_err(|_| PendingStoredAckError::Invalid("assignment scope"))?,
            closure_id: row.closure_id,
            disposition: backend_engine::cluster_transport::ResultAckDisposition::Rejected(
                row.reason,
            ),
        };
        let peer = EndpointId::from_bytes(&row.identity.worker_peer)
            .map_err(|_| PendingStoredAckError::Invalid("worker endpoint"))?;
        if !retired.matches_ack(&expected_ack, peer) {
            return Err(PendingStoredAckError::Invalid(
                "rejection retirement receipt",
            ));
        }
        if row.state == PendingRejectedAckState::AwaitingRetirementConfirm {
            return Ok(());
        }
        let mut replacement = self.rows.clone();
        let PendingStoredAckEntry::RejectedAdmission(row) = replacement
            .get_mut(&id)
            .ok_or(PendingStoredAckError::MissingRow)?
        else {
            return Err(PendingStoredAckError::TerminalState);
        };
        if row.state != PendingRejectedAckState::AckPending {
            return Err(PendingStoredAckError::TerminalState);
        }
        row.state = PendingRejectedAckState::AwaitingRetirementConfirm;
        row.validate()?;
        self.publish(replacement)
    }

    /// Mints confirm-only authority from the journal's retained, validated
    /// AwaitingConfirm row. Callers cannot synthesize this capability from a
    /// cloned or caller-constructed record.
    pub(crate) fn retirement_confirmation_scope(
        &self,
        id: &[u8; 32],
        owner_endpoint_id: [u8; 32],
    ) -> Result<RecoveredAckScope, PendingStoredAckError> {
        match self.rows.get(id).ok_or(PendingStoredAckError::MissingRow)? {
            PendingStoredAckEntry::Selection(row) => {
                RecoveredAckScope::from_retirement_confirmation(row, owner_endpoint_id)
            }
            PendingStoredAckEntry::RejectedAdmission(row) => {
                RecoveredAckScope::from_rejected_retirement_confirmation(row, owner_endpoint_id)
            }
            PendingStoredAckEntry::Reservation(_) => Err(PendingStoredAckError::TerminalState),
        }
    }

    /// Promotes a selected-history-proven pending result to the durable
    /// `StoredAckPending` state before minting the narrow trust-independent
    /// cleanup scope. This is intentionally sourced from the journal's own
    /// validated row so callers cannot substitute peer, address, scope, or
    /// closure facts.
    pub(crate) fn selected_stored_cleanup_scope(
        &mut self,
        id: &[u8; 32],
        owner_endpoint_id: [u8; 32],
        proof: &PendingRemoteResultProof,
    ) -> Result<RecoveredAckScope, PendingStoredAckError> {
        let row = self.selected_cleanup_row(id, owner_endpoint_id)?;
        let PendingRemoteResultProof::SelectedHistory(selected) = proof else {
            return Err(PendingStoredAckError::Invalid(
                "selected cleanup requires immutable selection history",
            ));
        };
        check_selected_history(&row.authority_identity(), selected)?;
        match row.state {
            PendingStoredAckState::StoredAckPending => {}
            PendingStoredAckState::AwaitingSelection => {
                let mut replacement = self.rows.clone();
                let PendingStoredAckEntry::Selection(row) = replacement
                    .get_mut(id)
                    .ok_or(PendingStoredAckError::MissingRow)?
                else {
                    return Err(PendingStoredAckError::TerminalState);
                };
                row.state = PendingStoredAckState::StoredAckPending;
                row.validate()?;
                self.publish(replacement)?;
            }
            _ => return Err(PendingStoredAckError::TerminalState),
        }
        match self.rows.get(id).ok_or(PendingStoredAckError::MissingRow)? {
            PendingStoredAckEntry::Selection(row) => {
                RecoveredAckScope::from_selected_history_cleanup(row, owner_endpoint_id, selected)
            }
            PendingStoredAckEntry::Reservation(_) | PendingStoredAckEntry::RejectedAdmission(_) => {
                Err(PendingStoredAckError::TerminalState)
            }
        }
    }

    /// Returns the only journal state from which selected-history cleanup may
    /// be minted. Kept separate so forbidden reservation/rejection rows are
    /// covered without fabricating an authority proof in tests.
    fn selected_cleanup_row(
        &self,
        id: &[u8; 32],
        owner_endpoint_id: [u8; 32],
    ) -> Result<&PendingStoredAckRecord, PendingStoredAckError> {
        let row = match self.rows.get(id).ok_or(PendingStoredAckError::MissingRow)? {
            PendingStoredAckEntry::Selection(row) => row,
            PendingStoredAckEntry::Reservation(_) | PendingStoredAckEntry::RejectedAdmission(_) => {
                return Err(PendingStoredAckError::TerminalState);
            }
        };
        row.validate()?;
        if owner_endpoint_id != row.owner_endpoint_id {
            return Err(PendingStoredAckError::Invalid("owner endpoint changed"));
        }
        if !matches!(
            row.state,
            PendingStoredAckState::AwaitingSelection | PendingStoredAckState::StoredAckPending
        ) {
            return Err(PendingStoredAckError::TerminalState);
        }
        Ok(row)
    }

    /// Mints a rejection-only scope for an already persisted admission
    /// rejection. The exact durable row permits cleanup after execution-grant
    /// revocation, but carries only `Rejected(Admission)` for its pinned peer,
    /// assignment, and closure. After `ResultRetired` is fsynced, recovery
    /// uses the narrower confirmation-only scope above.
    pub(crate) fn rejected_ack_scope(
        &self,
        id: &[u8; 32],
        owner_endpoint_id: [u8; 32],
    ) -> Result<RecoveredAckScope, PendingStoredAckError> {
        match self.rows.get(id).ok_or(PendingStoredAckError::MissingRow)? {
            PendingStoredAckEntry::RejectedAdmission(row) => {
                RecoveredAckScope::from_rejected_ack(row, owner_endpoint_id)
            }
            _ => Err(PendingStoredAckError::TerminalState),
        }
    }

    /// Durably records a candidate before its Turso compare-and-select call.
    /// Repeating an identical prepare is idempotent; a terminal row cannot be
    /// downgraded to the awaiting-selection state.
    pub(crate) fn prepare(
        &mut self,
        row: PendingStoredAckRecord,
    ) -> Result<[u8; 32], PendingStoredAckError> {
        if row.state != PendingStoredAckState::AwaitingSelection {
            return Err(PendingStoredAckError::Invalid(
                "prepare must start in AwaitingSelection",
            ));
        }
        row.validate()?;
        let id = row.id();
        let mut replacement = self.rows.clone();
        if let Some(existing) = replacement.get(&id) {
            if matches!(existing, PendingStoredAckEntry::Selection(existing) if same_identity(existing, &row))
            {
                return Ok(id);
            }
            return Err(PendingStoredAckError::Corrupt("duplicate row identity"));
        }
        if replacement.len() >= MAX_ROWS {
            return Err(PendingStoredAckError::Limit);
        }
        replacement.insert(id, PendingStoredAckEntry::Selection(row));
        self.publish(replacement)?;
        Ok(id)
    }

    /// Advances an intent only after a fresh authority resolver proves the
    /// exact selected history row or typed superseded attempt proof.
    pub(crate) fn resolve(
        &mut self,
        proof: &RecoveredAckScope,
    ) -> Result<(), PendingStoredAckError> {
        let id = proof.journal_id();
        let state = match proof.kind() {
            RecoveredAckKind::Stored => PendingStoredAckState::StoredAckPending,
            RecoveredAckKind::Superseded => PendingStoredAckState::SupersededAckPending,
            RecoveredAckKind::RejectedAdmission => {
                return Err(PendingStoredAckError::TerminalState);
            }
        };
        let current = match self
            .rows
            .get(&id)
            .ok_or(PendingStoredAckError::MissingRow)?
        {
            PendingStoredAckEntry::Selection(row) => row,
            PendingStoredAckEntry::Reservation(_) | PendingStoredAckEntry::RejectedAdmission(_) => {
                return Err(PendingStoredAckError::TerminalState);
            }
        };
        if current.state == state {
            return Ok(());
        }
        let awaiting_confirm = match (current.state, state) {
            (
                PendingStoredAckState::StoredAwaitingRetirementConfirm,
                PendingStoredAckState::StoredAckPending,
            ) => true,
            (
                PendingStoredAckState::SupersededAwaitingRetirementConfirm,
                PendingStoredAckState::SupersededAckPending,
            ) => true,
            _ => false,
        };
        if awaiting_confirm {
            return Ok(());
        }
        if current.state != PendingStoredAckState::AwaitingSelection {
            return Err(PendingStoredAckError::TerminalState);
        }
        let mut replacement = self.rows.clone();
        let PendingStoredAckEntry::Selection(row) = replacement
            .get_mut(&id)
            .ok_or(PendingStoredAckError::MissingRow)?
        else {
            return Err(PendingStoredAckError::TerminalState);
        };
        row.state = state;
        row.validate()?;
        self.publish(replacement)
    }

    /// Persists the exact worker retirement receipt before the owner sends its
    /// final confirmation. The transport callback is invoked only after the
    /// authenticated peer, assignment scope, closure, and disposition match.
    pub(crate) fn mark_awaiting_retirement_confirmation(
        &mut self,
        proof: &RecoveredAckScope,
        retired: &backend_engine::cluster_transport::ControlResultRetired,
    ) -> Result<(), PendingStoredAckError> {
        let id = proof.journal_id();
        let row = match self
            .rows
            .get(&id)
            .ok_or(PendingStoredAckError::MissingRow)?
        {
            PendingStoredAckEntry::Selection(row) => row,
            PendingStoredAckEntry::Reservation(_) | PendingStoredAckEntry::RejectedAdmission(_) => {
                return Err(PendingStoredAckError::TerminalState);
            }
        };
        let expected_state = match proof.kind() {
            RecoveredAckKind::Stored => PendingStoredAckState::StoredAckPending,
            RecoveredAckKind::Superseded => PendingStoredAckState::SupersededAckPending,
            RecoveredAckKind::RejectedAdmission => {
                return Err(PendingStoredAckError::TerminalState);
            }
        };
        let next_state = match proof.kind() {
            RecoveredAckKind::Stored => PendingStoredAckState::StoredAwaitingRetirementConfirm,
            RecoveredAckKind::Superseded => {
                PendingStoredAckState::SupersededAwaitingRetirementConfirm
            }
            RecoveredAckKind::RejectedAdmission => {
                return Err(PendingStoredAckError::TerminalState);
            }
        };
        let expected_ack = backend_engine::cluster_transport::ControlResultAck {
            scope: proof.scope(),
            closure_id: proof.closure_id(),
            disposition: match proof.kind() {
                RecoveredAckKind::Stored => {
                    backend_engine::cluster_transport::ResultAckDisposition::Stored
                }
                RecoveredAckKind::Superseded => {
                    backend_engine::cluster_transport::ResultAckDisposition::Rejected(
                        backend_engine::cluster_transport::ResultRejectReason::Scope,
                    )
                }
                RecoveredAckKind::RejectedAdmission => {
                    return Err(PendingStoredAckError::TerminalState);
                }
            },
        };
        if !retired.matches_ack(&expected_ack, proof.expected_peer()) {
            return Err(PendingStoredAckError::Invalid("retirement receipt scope"));
        }
        if row.state == next_state {
            return Ok(());
        }
        if row.state != expected_state {
            return Err(PendingStoredAckError::TerminalState);
        }
        let mut replacement = self.rows.clone();
        let PendingStoredAckEntry::Selection(row) = replacement
            .get_mut(&id)
            .ok_or(PendingStoredAckError::MissingRow)?
        else {
            return Err(PendingStoredAckError::TerminalState);
        };
        row.state = next_state;
        row.validate()?;
        self.publish(replacement)
    }

    /// Deletes a row only after its exact Stored or Superseded ACK completed.
    pub(crate) fn complete_ack(
        &mut self,
        receipt: &super::cluster_dispatch::RecoveredAckReceipt,
    ) -> Result<(), PendingStoredAckError> {
        let id = receipt.journal_id();
        let current = match self
            .rows
            .get(&id)
            .ok_or(PendingStoredAckError::MissingRow)?
        {
            PendingStoredAckEntry::Selection(row) => {
                let expected_state = match receipt.kind() {
                    RecoveredAckKind::Stored => {
                        PendingStoredAckState::StoredAwaitingRetirementConfirm
                    }
                    RecoveredAckKind::Superseded => {
                        PendingStoredAckState::SupersededAwaitingRetirementConfirm
                    }
                    RecoveredAckKind::RejectedAdmission => {
                        return Err(PendingStoredAckError::TerminalState);
                    }
                };
                if row.state != expected_state {
                    return Err(PendingStoredAckError::Unresolved);
                }
                row
            }
            PendingStoredAckEntry::RejectedAdmission(row) => {
                if receipt.kind() != RecoveredAckKind::RejectedAdmission
                    || row.state != PendingRejectedAckState::AwaitingRetirementConfirm
                {
                    return Err(PendingStoredAckError::Unresolved);
                }
                // We remove by id below after checking this terminal row.
                let mut replacement = self.rows.clone();
                replacement.remove(&id);
                return self.publish(replacement);
            }
            PendingStoredAckEntry::Reservation(_) => {
                return Err(PendingStoredAckError::TerminalState);
            }
        };
        let _ = current;
        let mut replacement = self.rows.clone();
        if !matches!(
            replacement.remove(&id),
            Some(PendingStoredAckEntry::Selection(_))
        ) {
            return Err(PendingStoredAckError::TerminalState);
        }
        self.publish(replacement)
    }

    fn publish(
        &mut self,
        replacement: BTreeMap<[u8; 32], PendingStoredAckEntry>,
    ) -> Result<(), PendingStoredAckError> {
        let bytes = encode_rows(&replacement)?;
        backend_platform::durable::write_private_atomic(&self.path, &bytes)?;
        self.rows = replacement;
        Ok(())
    }
}

/// Final network disposition justified by a fresh Turso history proof.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum RecoveredAckKind {
    /// Exact result closure belongs to an immutable selected generation.
    Stored,
    /// Turso proves a newer attempt fenced this result before selection.
    Superseded,
    /// Exact owner-side admission rejection for a checked result that could
    /// not be selected. Its persisted disposition is `Rejected(Admission)`.
    RejectedAdmission,
}

/// Sealed transport scope for one ACK recovered from a private journal row.
///
/// Its constructor validates current worker trust, owner identity, receipt
/// facts, and a non-forgeable locald Turso proof. The journal row alone cannot
/// create this capability.
#[derive(Clone, Debug)]
pub(crate) struct RecoveredAckScope {
    journal_id: [u8; 32],
    owner_endpoint_id: [u8; 32],
    worker_address: EndpointAddr,
    worker_socket_address: SocketAddr,
    expected_peer: EndpointId,
    scope: AssignmentScope,
    closure_id: [u8; 32],
    trust: PendingStoredAckTrust,
    kind: RecoveredAckKind,
    disposition: backend_engine::cluster_transport::ResultAckDisposition,
    awaiting_retirement_confirmation: bool,
    selected_stored_cleanup_only: bool,
}

impl RecoveredAckScope {
    /// Mints only the confirm half of a retirement handshake from a journal
    /// row whose exact authenticated `ResultRetired` receipt was already
    /// matched and fsynced. This capability cannot authorize a fresh ACK or a
    /// new compiler assignment, so it remains usable after trust revocation.
    fn from_retirement_confirmation(
        row: &PendingStoredAckRecord,
        owner_endpoint_id: [u8; 32],
    ) -> Result<Self, PendingStoredAckError> {
        row.validate()?;
        if owner_endpoint_id != row.owner_endpoint_id {
            return Err(PendingStoredAckError::Invalid("owner endpoint changed"));
        }
        let kind = match row.state {
            PendingStoredAckState::StoredAwaitingRetirementConfirm => RecoveredAckKind::Stored,
            PendingStoredAckState::SupersededAwaitingRetirementConfirm => {
                RecoveredAckKind::Superseded
            }
            _ => return Err(PendingStoredAckError::TerminalState),
        };
        let disposition = match kind {
            RecoveredAckKind::Stored => {
                backend_engine::cluster_transport::ResultAckDisposition::Stored
            }
            RecoveredAckKind::Superseded => {
                backend_engine::cluster_transport::ResultAckDisposition::Rejected(
                    backend_engine::cluster_transport::ResultRejectReason::Scope,
                )
            }
            RecoveredAckKind::RejectedAdmission => unreachable!("selection row kind"),
        };
        let expected_peer = EndpointId::from_bytes(&row.worker_peer)
            .map_err(|_| PendingStoredAckError::Invalid("worker endpoint"))?;
        let scope = AssignmentScope::new(
            row.namespace_id,
            row.work_id,
            row.assignment_attempt,
            row.assignment_fence,
        )
        .map_err(|_| PendingStoredAckError::Invalid("assignment scope"))?;
        Ok(Self {
            journal_id: row.id(),
            owner_endpoint_id,
            worker_address: EndpointAddr::new(expected_peer).with_ip_addr(row.worker_address),
            worker_socket_address: row.worker_address,
            expected_peer,
            scope,
            closure_id: row.worker_closure_id,
            trust: row.trust,
            kind,
            disposition,
            awaiting_retirement_confirmation: true,
            selected_stored_cleanup_only: false,
        })
    }

    fn from_rejected_retirement_confirmation(
        row: &PendingRejectedAckRecord,
        owner_endpoint_id: [u8; 32],
    ) -> Result<Self, PendingStoredAckError> {
        row.validate()?;
        if row.state != PendingRejectedAckState::AwaitingRetirementConfirm
            || owner_endpoint_id != row.identity.owner_endpoint_id
        {
            return Err(PendingStoredAckError::TerminalState);
        }
        let expected_peer = EndpointId::from_bytes(&row.identity.worker_peer)
            .map_err(|_| PendingStoredAckError::Invalid("worker endpoint"))?;
        let scope = AssignmentScope::new(
            row.identity.namespace_id,
            row.identity.work_id,
            row.identity.assignment_attempt,
            row.identity.assignment_fence,
        )
        .map_err(|_| PendingStoredAckError::Invalid("assignment scope"))?;
        Ok(Self {
            journal_id: row.id(),
            owner_endpoint_id,
            worker_address: EndpointAddr::new(expected_peer)
                .with_ip_addr(row.identity.worker_address),
            worker_socket_address: row.identity.worker_address,
            expected_peer,
            scope,
            closure_id: row.closure_id,
            trust: row.identity.trust,
            kind: RecoveredAckKind::RejectedAdmission,
            disposition: backend_engine::cluster_transport::ResultAckDisposition::Rejected(
                row.reason,
            ),
            awaiting_retirement_confirmation: true,
            selected_stored_cleanup_only: false,
        })
    }

    fn from_rejected_ack(
        row: &PendingRejectedAckRecord,
        owner_endpoint_id: [u8; 32],
    ) -> Result<Self, PendingStoredAckError> {
        row.validate()?;
        if row.state != PendingRejectedAckState::AckPending
            || owner_endpoint_id != row.identity.owner_endpoint_id
        {
            return Err(PendingStoredAckError::TerminalState);
        }
        let expected_peer = EndpointId::from_bytes(&row.identity.worker_peer)
            .map_err(|_| PendingStoredAckError::Invalid("worker endpoint"))?;
        let scope = AssignmentScope::new(
            row.identity.namespace_id,
            row.identity.work_id,
            row.identity.assignment_attempt,
            row.identity.assignment_fence,
        )
        .map_err(|_| PendingStoredAckError::Invalid("assignment scope"))?;
        Ok(Self {
            journal_id: row.id(),
            owner_endpoint_id,
            worker_address: EndpointAddr::new(expected_peer)
                .with_ip_addr(row.identity.worker_address),
            worker_socket_address: row.identity.worker_address,
            expected_peer,
            scope,
            closure_id: row.closure_id,
            trust: row.identity.trust,
            kind: RecoveredAckKind::RejectedAdmission,
            disposition: backend_engine::cluster_transport::ResultAckDisposition::Rejected(
                row.reason,
            ),
            awaiting_retirement_confirmation: false,
            selected_stored_cleanup_only: false,
        })
    }

    /// Mints a Stored-retirement capability from a durable `StoredAckPending`
    /// row and the exact immutable Turso selected-history proof. This grants
    /// no assignment, publication, or new ACK authority; it only lets the
    /// owner retire the exact already-selected result at the original
    /// authenticated peer/address after its execution grant has been revoked.
    fn from_selected_history_cleanup(
        row: &PendingStoredAckRecord,
        owner_endpoint_id: [u8; 32],
        selected: &SelectedGeneration,
    ) -> Result<Self, PendingStoredAckError> {
        row.validate()?;
        if owner_endpoint_id != row.owner_endpoint_id
            || row.state != PendingStoredAckState::StoredAckPending
        {
            return Err(PendingStoredAckError::TerminalState);
        }
        check_selected_history(&row.authority_identity(), selected)?;
        let expected_peer = EndpointId::from_bytes(&row.worker_peer)
            .map_err(|_| PendingStoredAckError::Invalid("worker endpoint"))?;
        let scope = AssignmentScope::new(
            row.namespace_id,
            row.work_id,
            row.assignment_attempt,
            row.assignment_fence,
        )
        .map_err(|_| PendingStoredAckError::Invalid("assignment scope"))?;
        Ok(Self {
            journal_id: row.id(),
            owner_endpoint_id,
            worker_address: EndpointAddr::new(expected_peer).with_ip_addr(row.worker_address),
            worker_socket_address: row.worker_address,
            expected_peer,
            scope,
            closure_id: row.worker_closure_id,
            trust: row.trust,
            kind: RecoveredAckKind::Stored,
            disposition: backend_engine::cluster_transport::ResultAckDisposition::Stored,
            awaiting_retirement_confirmation: false,
            selected_stored_cleanup_only: true,
        })
    }

    /// Mints a send scope only after the selected-history or superseded proof
    /// has been checked against this private journal claim and current trust.
    pub(crate) fn from_proof(
        row: &PendingStoredAckRecord,
        owner_endpoint_id: [u8; 32],
        trust_policy: &TrustedCompilerWorkerPolicy,
        proof: &PendingRemoteResultProof,
    ) -> Result<Self, PendingStoredAckError> {
        row.validate()?;
        if owner_endpoint_id != row.owner_endpoint_id {
            return Err(PendingStoredAckError::Invalid("owner endpoint changed"));
        }
        if !trust_policy.grants().iter().any(|grant| {
            *grant.peer().as_bytes() == row.worker_peer
                && grant.address() == row.worker_address
                && grant.namespace_id() == row.namespace_id
                && grant.recipe() == row.trust.recipe
                && <[u8; 2]>::from(grant.profile()) == row.trust.profile
                && u8::from(grant.stage()) == row.trust.stage
                && grant.toolchain() == row.trust.toolchain
                && grant.environment() == row.trust.environment
                && grant.target_platform() == row.trust.target_platform
        }) {
            return Err(PendingStoredAckError::Invalid(
                "worker trust was revoked or changed",
            ));
        }
        let identity = row.authority_identity();
        let (kind, expected_closure) = match proof {
            PendingRemoteResultProof::SelectedHistory(selected) => {
                check_selected_history(&identity, selected)?;
                (RecoveredAckKind::Stored, row.worker_closure_id)
            }
            PendingRemoteResultProof::Superseded(superseded) => {
                check_superseded_proof(&identity, superseded)?;
                (RecoveredAckKind::Superseded, row.worker_closure_id)
            }
        };
        let awaiting_retirement_confirmation = match (row.state, kind) {
            (PendingStoredAckState::AwaitingSelection, _) => false,
            (PendingStoredAckState::StoredAckPending, RecoveredAckKind::Stored)
            | (PendingStoredAckState::SupersededAckPending, RecoveredAckKind::Superseded) => false,
            (PendingStoredAckState::StoredAwaitingRetirementConfirm, RecoveredAckKind::Stored)
            | (
                PendingStoredAckState::SupersededAwaitingRetirementConfirm,
                RecoveredAckKind::Superseded,
            ) => true,
            _ => return Err(PendingStoredAckError::TerminalState),
        };
        let expected_peer = EndpointId::from_bytes(&row.worker_peer)
            .map_err(|_| PendingStoredAckError::Invalid("worker peer"))?;
        let scope = AssignmentScope::new(
            row.namespace_id,
            row.work_id,
            row.assignment_attempt,
            row.assignment_fence,
        )
        .map_err(|_| PendingStoredAckError::Invalid("assignment scope"))?;
        Ok(Self {
            journal_id: row.id(),
            owner_endpoint_id,
            worker_address: EndpointAddr::new(expected_peer).with_ip_addr(row.worker_address),
            worker_socket_address: row.worker_address,
            expected_peer,
            scope,
            closure_id: expected_closure,
            trust: row.trust,
            kind,
            disposition: match kind {
                RecoveredAckKind::Stored => {
                    backend_engine::cluster_transport::ResultAckDisposition::Stored
                }
                RecoveredAckKind::Superseded => {
                    backend_engine::cluster_transport::ResultAckDisposition::Rejected(
                        backend_engine::cluster_transport::ResultRejectReason::Scope,
                    )
                }
                RecoveredAckKind::RejectedAdmission => unreachable!("selection proof kind"),
            },
            awaiting_retirement_confirmation,
            selected_stored_cleanup_only: false,
        })
    }

    /// Rechecks every pinned grant dimension immediately before opening the
    /// network stream, so a trust-policy edit after proof minting stops ACK.
    #[must_use]
    pub(crate) fn is_currently_trusted(&self, trust_policy: &TrustedCompilerWorkerPolicy) -> bool {
        trust_policy.grants().iter().any(|grant| {
            grant.peer() == self.expected_peer
                && grant.address() == self.worker_socket_address
                && grant.namespace_id() == self.scope.namespace_id
                && grant.recipe() == self.trust.recipe
                && <[u8; 2]>::from(grant.profile()) == self.trust.profile
                && u8::from(grant.stage()) == self.trust.stage
                && grant.toolchain() == self.trust.toolchain
                && grant.environment() == self.trust.environment
                && grant.target_platform() == self.trust.target_platform
        })
    }

    #[must_use]
    pub(crate) const fn owner_endpoint_id(&self) -> [u8; 32] {
        self.owner_endpoint_id
    }

    /// Durable row identity sealed into the Turso-authorized recovery token.
    #[must_use]
    pub(crate) const fn journal_id(&self) -> [u8; 32] {
        self.journal_id
    }

    #[must_use]
    pub(crate) fn worker_address(&self) -> EndpointAddr {
        self.worker_address.clone()
    }

    #[must_use]
    pub(crate) const fn expected_peer(&self) -> EndpointId {
        self.expected_peer
    }

    #[must_use]
    pub(crate) const fn scope(&self) -> AssignmentScope {
        self.scope
    }

    #[must_use]
    pub(crate) const fn closure_id(&self) -> [u8; 32] {
        self.closure_id
    }

    #[must_use]
    pub(crate) const fn kind(&self) -> RecoveredAckKind {
        self.kind
    }

    /// Exact immutable terminal disposition selected by the journal and
    /// authority proof. The transport layer must not remap it.
    #[must_use]
    pub(crate) const fn disposition(
        &self,
    ) -> backend_engine::cluster_transport::ResultAckDisposition {
        self.disposition
    }

    /// Whether the exact worker retirement receipt is already durable locally,
    /// so recovery must send only the final confirmation.
    #[must_use]
    pub(crate) const fn awaiting_retirement_confirmation(&self) -> bool {
        self.awaiting_retirement_confirmation
    }

    /// Whether this scope authorizes only a Stored retirement backed by an
    /// exact selected-history proof, independent of current execution trust.
    #[must_use]
    pub(crate) const fn selected_stored_cleanup_only(&self) -> bool {
        self.selected_stored_cleanup_only
    }
}

impl PendingStoredAckRecord {
    /// Builds a durable preselection intent from the exact checked worker
    /// receipt, scheduler assignment, and Turso candidate that is about to be
    /// compare-and-selected.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn from_selected_candidate(
        owner_endpoint_id: [u8; 32],
        worker_address: SocketAddr,
        namespace_id: [u8; 16],
        product_key: PendingStoredAckProductKey,
        assignment: backend_engine::application::CompilerAssignment,
        worker_receipt: backend_store::StoredClosureReceipt,
        selected: &backend_extension_turso::CandidateGeneration,
        trust: PendingStoredAckTrust,
        worker_result_receipt: backend_engine::cluster_transport::ControlResultReceipt,
        capture: &backend_engine::application::CapturedFullWorkspaceV2,
        source_root: &Path,
    ) -> Result<Self, PendingStoredAckError> {
        use backend_engine::application::CompilerAssignmentRoute;

        let CompilerAssignmentRoute::Remote(peer) = assignment.route() else {
            return Err(PendingStoredAckError::Invalid("assignment is not remote"));
        };
        let attempt = selected.attempt();
        let token = assignment.token();
        let expected_generation =
            attempt
                .base_generation()
                .checked_add(1)
                .ok_or(PendingStoredAckError::Invalid(
                    "selected generation overflow",
                ))?;
        let scope = backend_engine::compiler_cluster_transport::compiler_assignment_scope(
            assignment,
            namespace_id,
        )
        .map_err(|_| PendingStoredAckError::Invalid("assignment scope"))?;
        if attempt.namespace().namespace_id() != namespace_id
            || attempt.scheduler_attempt_id() != token.attempt().get()
            || attempt.fence_bytes() != token.fence().as_bytes()
        {
            return Err(PendingStoredAckError::Invalid(
                "candidate differs from exact assigned attempt",
            ));
        }
        let captured_work =
            PendingAckCapturedWork::from_attempt_capture(attempt, capture, source_root)?;
        if captured_work.work_id() != scope.work_id {
            return Err(PendingStoredAckError::Invalid(
                "selected assignment differs from captured work identity",
            ));
        }
        let worker_object_count = u32::try_from(worker_receipt.object_count())
            .map_err(|_| PendingStoredAckError::Limit)?;
        let row = Self {
            state: PendingStoredAckState::AwaitingSelection,
            owner_endpoint_id,
            worker_peer: peer.as_bytes(),
            worker_address,
            namespace_id,
            product_key,
            work_id: scope.work_id,
            assignment_attempt: scope.attempt,
            assignment_fence: scope.fence,
            worker_closure_id: *worker_receipt.closure().as_bytes(),
            worker_object_count,
            worker_payload_bytes: worker_receipt.payload_bytes(),
            worker_bytes_verified: worker_receipt.bytes_verified(),
            worker_result_receipt,
            turso_generation: expected_generation,
            turso_attempt_id: *attempt.attempt_id(),
            turso_attempt_epoch: attempt.epoch(),
            turso_fence: attempt.fence_bytes(),
            input_digest: *attempt.input_digest(),
            candidate_id: *selected.candidate_id(),
            target_root: *selected.target_root(),
            selected_closure_id: *selected.closure_id(),
            trust,
            captured_work,
        };
        row.validate()?;
        Ok(row)
    }

    pub(crate) fn authority_identity(
        &self,
    ) -> super::semantic_authority::PendingRemoteResultIdentity {
        super::semantic_authority::PendingRemoteResultIdentity::new(
            self.namespace_id,
            self.turso_attempt_epoch,
            self.turso_attempt_id,
            self.turso_fence,
            self.input_digest,
            self.candidate_id,
            self.target_root,
            self.selected_closure_id,
            self.turso_generation,
            self.worker_closure_id,
            self.worker_object_count,
            self.worker_payload_bytes,
            self.worker_bytes_verified,
        )
    }

    pub(crate) fn assignment_identity(&self) -> PendingAckAssignmentIdentity {
        PendingAckAssignmentIdentity {
            owner_endpoint_id: self.owner_endpoint_id,
            worker_peer: self.worker_peer,
            worker_address: self.worker_address,
            namespace_id: self.namespace_id,
            product_key: self.product_key.clone(),
            work_id: self.work_id,
            assignment_attempt: self.assignment_attempt,
            assignment_fence: self.assignment_fence,
            trust: self.trust,
        }
    }

    /// Reconstructs the exact typed product key from canonical durable text.
    ///
    /// A journal string is only an untrusted lookup hint. Parse and round-trip
    /// every component before it is passed to the locald authority resolver.
    pub(crate) fn product_semantic_key(
        &self,
    ) -> Result<backend_engine::builtin::ProductSemanticPublicationKey, PendingStoredAckError> {
        self.product_key.product_semantic_key()
    }
}

impl PendingStoredAckProductKey {
    pub(crate) fn product_semantic_key(
        &self,
    ) -> Result<backend_engine::builtin::ProductSemanticPublicationKey, PendingStoredAckError> {
        let package = backend_engine::PackageReference::parse(self.package.to_string())
            .map_err(|_| PendingStoredAckError::Invalid("package key"))?;
        if package.as_str() != self.package.as_ref() {
            return Err(PendingStoredAckError::Invalid("noncanonical package key"));
        }
        let coordinate = backend_library::interface::PackageUrl::parse(self.coordinate.to_string())
            .map_err(|_| PendingStoredAckError::Invalid("coordinate key"))?;
        if coordinate.as_str() != self.coordinate.as_ref() {
            return Err(PendingStoredAckError::Invalid(
                "noncanonical coordinate key",
            ));
        }
        let code = parse_profile_code(&self.profile)?;
        let profile = backend_semantic::vocabulary::LanguageProfile::try_from(code)
            .map_err(|_| PendingStoredAckError::Invalid("profile key"))?;
        let canonical_profile = format!("{:02x}{:02x}/lower-ir", code[0], code[1]);
        if canonical_profile != self.profile.as_ref() {
            return Err(PendingStoredAckError::Invalid("noncanonical profile key"));
        }
        backend_engine::builtin::ProductSemanticPublicationKey::new(package, coordinate, profile)
            .map_err(|_| PendingStoredAckError::Invalid("product semantic key"))
    }
}

fn parse_profile_code(profile: &str) -> Result<[u8; 2], PendingStoredAckError> {
    let hex = profile
        .strip_suffix("/lower-ir")
        .ok_or(PendingStoredAckError::Invalid("profile stage"))?;
    if hex.len() != 4 || !hex.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(PendingStoredAckError::Invalid("profile code"));
    }
    let mut code = [0_u8; 2];
    for (index, byte) in code.iter_mut().enumerate() {
        let start = index * 2;
        *byte = u8::from_str_radix(&hex[start..start + 2], 16)
            .map_err(|_| PendingStoredAckError::Invalid("profile code"))?;
    }
    Ok(code)
}

fn check_selected_history(
    expected: &super::semantic_authority::PendingRemoteResultIdentity,
    selected: &SelectedGeneration,
) -> Result<(), PendingStoredAckError> {
    let (attempt_id, epoch) = selected.attempt();
    let (scheduler_epoch, fence) = selected.scheduler_fence();
    if selected.namespace().namespace_id() != expected.namespace_id()
        || selected.generation() != expected.expected_generation()
        || attempt_id != expected.attempt_id()
        || epoch != expected.epoch()
        || scheduler_epoch != expected.epoch()
        || fence != *expected.fence()
        || selected.input_digest() != expected.input_digest()
        || selected.candidate_id() != expected.candidate_id()
        || selected.target_root() != expected.target_root()
        || selected.closure_id() != expected.selected_closure_id()
    {
        return Err(PendingStoredAckError::Invalid(
            "selected history differs from pending result",
        ));
    }
    Ok(())
}

fn check_superseded_proof(
    expected: &super::semantic_authority::PendingRemoteResultIdentity,
    proof: &SupersededAttemptProof,
) -> Result<(), PendingStoredAckError> {
    if proof.namespace().namespace_id() != expected.namespace_id()
        || proof.attempt_id() != expected.attempt_id()
        || proof.epoch() != expected.epoch()
        || proof.fence() != expected.fence()
        || proof.input_digest() != expected.input_digest()
        || proof.current_epoch() <= expected.epoch()
    {
        return Err(PendingStoredAckError::Invalid(
            "superseded proof differs from pending result",
        ));
    }
    Ok(())
}

/// Failure opening, validating, or updating the pending Stored ACK journal.
#[derive(Debug)]
pub(crate) enum PendingStoredAckError {
    /// Filesystem or durability barrier failure.
    Io(io::Error),
    /// A persisted row is malformed, truncated, unsupported, or has a bad checksum.
    Corrupt(&'static str),
    /// A strict byte, field, or row bound was exceeded.
    Limit,
    /// Every reserved slot is occupied by an unresolved assignment/result.
    CapacityFull,
    /// A supplied pending result claim is internally invalid.
    Invalid(&'static str),
    /// The requested journal identity is not present.
    MissingRow,
    /// A terminal ACK state was already selected and cannot be changed.
    TerminalState,
    /// An unresolved selection intent cannot be completed as an ACK.
    Unresolved,
}

impl std::fmt::Display for PendingStoredAckError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(error) => write!(formatter, "pending Stored ACK journal I/O: {error}"),
            Self::Corrupt(field) => {
                write!(formatter, "pending Stored ACK journal is corrupt ({field})")
            }
            Self::Limit => {
                formatter.write_str("pending Stored ACK journal exceeds its fixed bound")
            }
            Self::CapacityFull => formatter.write_str(
                "pending compiler-result journal is full; unresolved remote work needs recovery",
            ),
            Self::Invalid(field) => {
                write!(formatter, "pending Stored ACK claim is invalid ({field})")
            }
            Self::MissingRow => formatter.write_str("pending Stored ACK row is missing"),
            Self::TerminalState => {
                formatter.write_str("pending Stored ACK disposition is terminal")
            }
            Self::Unresolved => formatter.write_str("pending Stored ACK selection is unresolved"),
        }
    }
}

impl std::error::Error for PendingStoredAckError {}

impl From<io::Error> for PendingStoredAckError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

fn same_identity(left: &PendingStoredAckRecord, right: &PendingStoredAckRecord) -> bool {
    let mut left = left.clone();
    let mut right = right.clone();
    left.state = PendingStoredAckState::AwaitingSelection;
    right.state = PendingStoredAckState::AwaitingSelection;
    // Address is only a persisted connection hint. The authenticated peer and
    // exact assignment determine result identity.
    left.worker_address = right.worker_address;
    left == right
}

fn encode_rows(
    rows: &BTreeMap<[u8; 32], PendingStoredAckEntry>,
) -> Result<Vec<u8>, PendingStoredAckError> {
    if rows.len() > MAX_ROWS {
        return Err(PendingStoredAckError::Limit);
    }
    let mut body = Vec::new();
    for (id, entry) in rows {
        entry.validate()?;
        if entry.id() != *id {
            return Err(PendingStoredAckError::Corrupt("row key"));
        }
        let encoded = entry.encode()?;
        let length = u32::try_from(encoded.len()).map_err(|_| PendingStoredAckError::Limit)?;
        body.extend_from_slice(&length.to_be_bytes());
        body.extend_from_slice(&encoded);
    }
    let body_length = u32::try_from(body.len()).map_err(|_| PendingStoredAckError::Limit)?;
    let mut bytes = Vec::with_capacity(8 + 2 + 2 + 4 + body.len() + CHECKSUM_BYTES);
    bytes.extend_from_slice(MAGIC);
    bytes.extend_from_slice(&FORMAT_VERSION.to_be_bytes());
    bytes.extend_from_slice(&(rows.len() as u16).to_be_bytes());
    bytes.extend_from_slice(&body_length.to_be_bytes());
    bytes.extend_from_slice(&body);
    let checksum = blake3::hash(&bytes);
    bytes.extend_from_slice(checksum.as_bytes());
    if bytes.len() > MAX_FILE_BYTES {
        return Err(PendingStoredAckError::Limit);
    }
    Ok(bytes)
}

fn decode_rows(
    bytes: &[u8],
) -> Result<BTreeMap<[u8; 32], PendingStoredAckEntry>, PendingStoredAckError> {
    if bytes.len() < 8 + 2 + 2 + 4 + CHECKSUM_BYTES || bytes.len() > MAX_FILE_BYTES {
        return Err(PendingStoredAckError::Corrupt("length"));
    }
    let checksum_offset = bytes.len() - CHECKSUM_BYTES;
    if blake3::hash(&bytes[..checksum_offset]).as_bytes() != &bytes[checksum_offset..] {
        return Err(PendingStoredAckError::Corrupt("checksum"));
    }
    let mut cursor = Cursor::new(&bytes[..checksum_offset]);
    if cursor.take(8)? != MAGIC {
        return Err(PendingStoredAckError::Corrupt("magic"));
    }
    if cursor.u16()? != FORMAT_VERSION {
        return Err(PendingStoredAckError::Corrupt("version"));
    }
    let row_count = usize::from(cursor.u16()?);
    if row_count > MAX_ROWS {
        return Err(PendingStoredAckError::Limit);
    }
    let body_length = usize::try_from(cursor.u32()?).map_err(|_| PendingStoredAckError::Limit)?;
    if body_length != cursor.remaining() {
        return Err(PendingStoredAckError::Corrupt("body length"));
    }
    let mut rows = BTreeMap::new();
    for _ in 0..row_count {
        let record_length =
            usize::try_from(cursor.u32()?).map_err(|_| PendingStoredAckError::Limit)?;
        if record_length > MAX_FILE_BYTES || record_length > cursor.remaining() {
            return Err(PendingStoredAckError::Corrupt("record length"));
        }
        let entry = PendingStoredAckEntry::decode(cursor.take(record_length)?)?;
        let id = entry.id();
        if rows.insert(id, entry).is_some() {
            return Err(PendingStoredAckError::Corrupt("duplicate row"));
        }
    }
    cursor.finish()?;
    Ok(rows)
}

fn check_text(
    bytes: &[u8],
    maximum: usize,
    field: &'static str,
) -> Result<(), PendingStoredAckError> {
    if bytes.is_empty()
        || bytes.len() > maximum
        || std::str::from_utf8(bytes).is_err()
        || bytes.contains(&0)
    {
        return Err(PendingStoredAckError::Invalid(field));
    }
    Ok(())
}

fn encode_text(output: &mut Vec<u8>, bytes: &[u8]) {
    // The validated field bounds are all below the u16 range.
    output.extend_from_slice(&(bytes.len() as u16).to_be_bytes());
    output.extend_from_slice(bytes);
}

fn encode_socket_addr(output: &mut Vec<u8>, address: SocketAddr) {
    match address {
        SocketAddr::V4(address) => {
            output.push(4);
            output.extend_from_slice(&address.ip().octets());
            output.extend_from_slice(&address.port().to_be_bytes());
        }
        SocketAddr::V6(address) => {
            output.push(6);
            output.extend_from_slice(&address.ip().octets());
            output.extend_from_slice(&address.port().to_be_bytes());
            output.extend_from_slice(&address.flowinfo().to_be_bytes());
            output.extend_from_slice(&address.scope_id().to_be_bytes());
        }
    }
}

fn encode_control_result_receipt(
    output: &mut Vec<u8>,
    receipt: backend_engine::cluster_transport::ControlResultReceipt,
) {
    output.extend_from_slice(&receipt.scope.namespace_id);
    output.extend_from_slice(&receipt.scope.work_id);
    output.extend_from_slice(&receipt.scope.attempt.to_be_bytes());
    output.extend_from_slice(&receipt.scope.fence);
    output.extend_from_slice(&receipt.target_root);
    match receipt.pack_id {
        None => output.push(0),
        Some(pack_id) => {
            output.push(1);
            output.extend_from_slice(&pack_id);
        }
    }
    output.extend_from_slice(&receipt.closure_id);
    output.extend_from_slice(&receipt.object_count.to_be_bytes());
    output.extend_from_slice(&receipt.payload_bytes.to_be_bytes());
    output.extend_from_slice(&receipt.result_grant_pages.to_be_bytes());
}

fn decode_control_result_receipt(
    cursor: &mut Cursor<'_>,
) -> Result<backend_engine::cluster_transport::ControlResultReceipt, PendingStoredAckError> {
    let scope = backend_engine::cluster_transport::AssignmentScope::new(
        cursor.array()?,
        cursor.array()?,
        cursor.u64()?,
        cursor.array()?,
    )
    .map_err(|_| PendingStoredAckError::Corrupt("result receipt scope"))?;
    let target_root = cursor.array()?;
    let pack_id = match cursor.u8()? {
        0 => None,
        1 => Some(cursor.array()?),
        _ => return Err(PendingStoredAckError::Corrupt("result receipt pack option")),
    };
    Ok(backend_engine::cluster_transport::ControlResultReceipt {
        scope,
        target_root,
        pack_id,
        closure_id: cursor.array()?,
        object_count: cursor.u32()?,
        payload_bytes: cursor.u64()?,
        result_grant_pages: cursor.u32()?,
    })
}

struct Cursor<'a> {
    bytes: &'a [u8],
    offset: usize,
}

impl<'a> Cursor<'a> {
    const fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, offset: 0 }
    }

    fn take(&mut self, count: usize) -> Result<&'a [u8], PendingStoredAckError> {
        let end = self
            .offset
            .checked_add(count)
            .ok_or(PendingStoredAckError::Corrupt("offset overflow"))?;
        let result = self
            .bytes
            .get(self.offset..end)
            .ok_or(PendingStoredAckError::Corrupt("truncated record"))?;
        self.offset = end;
        Ok(result)
    }

    fn array<const N: usize>(&mut self) -> Result<[u8; N], PendingStoredAckError> {
        self.take(N)?
            .try_into()
            .map_err(|_| PendingStoredAckError::Corrupt("array"))
    }

    fn u8(&mut self) -> Result<u8, PendingStoredAckError> {
        self.take(1)?
            .first()
            .copied()
            .ok_or(PendingStoredAckError::Corrupt("u8"))
    }

    fn u16(&mut self) -> Result<u16, PendingStoredAckError> {
        Ok(u16::from_be_bytes(self.array()?))
    }

    fn u32(&mut self) -> Result<u32, PendingStoredAckError> {
        Ok(u32::from_be_bytes(self.array()?))
    }

    fn u64(&mut self) -> Result<u64, PendingStoredAckError> {
        Ok(u64::from_be_bytes(self.array()?))
    }

    fn socket_addr(&mut self) -> Result<SocketAddr, PendingStoredAckError> {
        match self.u8()? {
            4 => Ok(SocketAddr::new(
                IpAddr::V4(Ipv4Addr::from(self.array::<4>()?)),
                self.u16()?,
            )),
            6 => {
                let ip = Ipv6Addr::from(self.array::<16>()?);
                let port = self.u16()?;
                let flowinfo = self.u32()?;
                let scope_id = self.u32()?;
                Ok(SocketAddr::V6(SocketAddrV6::new(
                    ip, port, flowinfo, scope_id,
                )))
            }
            _ => return Err(PendingStoredAckError::Corrupt("address family")),
        }
    }

    fn text(
        &mut self,
        maximum: usize,
        field: &'static str,
    ) -> Result<Box<str>, PendingStoredAckError> {
        let length = usize::from(self.u16()?);
        if length > maximum {
            return Err(PendingStoredAckError::Limit);
        }
        let bytes = self.take(length)?;
        check_text(bytes, maximum, field)?;
        let text = std::str::from_utf8(bytes).map_err(|_| PendingStoredAckError::Corrupt(field))?;
        Ok(text.into())
    }

    fn remaining(&self) -> usize {
        self.bytes.len().saturating_sub(self.offset)
    }

    fn finish(&self) -> Result<(), PendingStoredAckError> {
        if self.remaining() == 0 {
            Ok(())
        } else {
            Err(PendingStoredAckError::Corrupt("trailing bytes"))
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, clippy::panic)]

    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT_ROOT: AtomicU64 = AtomicU64::new(0);

    struct TestRoot(PathBuf);

    impl TestRoot {
        fn new() -> Self {
            let id = NEXT_ROOT.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "backend-pending-stored-ack-{}-{id}",
                std::process::id()
            ));
            fs::create_dir(&path).expect("create private test root");
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                fs::set_permissions(&path, fs::Permissions::from_mode(0o700))
                    .expect("set private test root mode");
            }
            Self(path)
        }
    }

    impl Drop for TestRoot {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn record() -> PendingStoredAckRecord {
        let owner_endpoint_id = test_endpoint_id(1);
        let worker_peer = test_endpoint_id(2);
        let mut row = PendingStoredAckRecord {
            state: PendingStoredAckState::AwaitingSelection,
            owner_endpoint_id,
            worker_peer,
            worker_address: "127.0.0.1:4331".parse().expect("socket address"),
            namespace_id: [3; 16],
            product_key: PendingStoredAckProductKey {
                package: "pkg:cargo/authority-cutover@1.0.0".into(),
                coordinate: "pkg:cargo/authority-cutover@1.0.0".into(),
                profile: "0003/lower-ir".into(),
            },
            work_id: [4; 16],
            assignment_attempt: 8,
            assignment_fence: [11; 32],
            worker_closure_id: [7; 32],
            worker_object_count: 3,
            worker_payload_bytes: 4096,
            worker_bytes_verified: 4096,
            worker_result_receipt: backend_engine::cluster_transport::ControlResultReceipt {
                scope: AssignmentScope::new([3; 16], [4; 16], 8, [11; 32]).expect("result scope"),
                target_root: [14; 32],
                pack_id: None,
                closure_id: [7; 32],
                object_count: 3,
                payload_bytes: 4096,
                result_grant_pages: 1,
            },
            turso_generation: 9,
            turso_attempt_id: [10; 16],
            turso_attempt_epoch: 8,
            turso_fence: [11; 32],
            input_digest: [12; 32],
            candidate_id: [13; 32],
            target_root: [14; 32],
            selected_closure_id: [15; 32],
            trust: PendingStoredAckTrust {
                recipe: [16; 32],
                profile: [0, 3],
                stage: 1,
                toolchain: [18; 32],
                environment: [19; 32],
                target_platform: [20; 32],
            },
            captured_work: PendingAckCapturedWork {
                source_root: "/tmp/project".into(),
                package_lineage: [21; 32],
                target: [22; 32],
                recipe: [16; 32],
                input_root: [12; 32],
                read_manifest: [23; 32],
                workspace_snapshot_id: [23; 32],
                max_output_bytes: 1024 * 1024,
                input_closure_id: [24; 32],
                manifest_object_id: [25; 32],
                object_count: 3,
                payload_bytes: 4096,
                source_fence_digest: [26; 32],
                source_observation_revision: [12; 32],
                source_observation_sequence: 1,
                source_observation_observed_at_ms: 1,
                source_observation_count: 1,
                turso_attempt_id: [10; 16],
                turso_epoch: 8,
                turso_fence: [11; 32],
                turso_input_digest: [12; 32],
                turso_base_generation: 0,
                turso_base_root: None,
            },
        };
        row.work_id = row.captured_work.work_id();
        row.worker_result_receipt.scope.work_id = row.work_id;
        row
    }

    fn test_endpoint_id(seed: u8) -> [u8; 32] {
        let endpoint = SecretKey::from_bytes(&[seed; 32]).public();
        *endpoint.as_bytes()
    }

    fn open(root: &TestRoot) -> PendingStoredAckJournal {
        PendingStoredAckJournal::open(&root.0).expect("open journal")
    }

    fn proof_scope(row: &PendingStoredAckRecord, kind: RecoveredAckKind) -> RecoveredAckScope {
        let expected_peer = EndpointId::from_bytes(&row.worker_peer).expect("worker endpoint");
        RecoveredAckScope {
            journal_id: row.id(),
            owner_endpoint_id: row.owner_endpoint_id,
            worker_address: EndpointAddr::new(expected_peer).with_ip_addr(row.worker_address),
            worker_socket_address: row.worker_address,
            expected_peer,
            scope: AssignmentScope::new(
                row.namespace_id,
                row.work_id,
                row.assignment_attempt,
                row.assignment_fence,
            )
            .expect("assignment scope"),
            closure_id: row.worker_closure_id,
            trust: row.trust,
            kind,
            disposition: match kind {
                RecoveredAckKind::Stored => {
                    backend_engine::cluster_transport::ResultAckDisposition::Stored
                }
                RecoveredAckKind::Superseded => {
                    backend_engine::cluster_transport::ResultAckDisposition::Rejected(
                        backend_engine::cluster_transport::ResultRejectReason::Scope,
                    )
                }
                RecoveredAckKind::RejectedAdmission => {
                    backend_engine::cluster_transport::ResultAckDisposition::Rejected(
                        backend_engine::cluster_transport::ResultRejectReason::Admission,
                    )
                }
            },
            awaiting_retirement_confirmation: false,
            selected_stored_cleanup_only: false,
        }
    }

    fn assignment_identity(row: &PendingStoredAckRecord) -> PendingAckAssignmentIdentity {
        PendingAckAssignmentIdentity {
            owner_endpoint_id: row.owner_endpoint_id,
            worker_peer: row.worker_peer,
            worker_address: row.worker_address,
            namespace_id: row.namespace_id,
            product_key: row.product_key.clone(),
            work_id: row.work_id,
            assignment_attempt: row.assignment_attempt,
            assignment_fence: row.assignment_fence,
            trust: row.trust,
        }
    }

    fn captured_work(row: &PendingStoredAckRecord) -> PendingAckCapturedWork {
        row.captured_work.clone()
    }

    #[test]
    fn preselection_intent_survives_restart_and_uncertain_commit_can_be_resolved() {
        let root = TestRoot::new();
        let mut journal = open(&root);
        let row = record();
        let id = journal.prepare(row.clone()).expect("persist before select");
        drop(journal);

        // Simulates a process crash between atomic intent publication and the
        // compare-and-select call. Recovery has a claim to re-query, not ACK.
        let mut reopened = open(&root);
        let (observed_id, observed) = reopened.rows().next().expect("pending row");
        assert_eq!(*observed_id, id);
        assert_eq!(observed, &row);
        assert_eq!(observed.state, PendingStoredAckState::AwaitingSelection);

        // Simulates a commit whose outcome was uncertain to the old process:
        // the fresh authority resolver proved exact selection before this
        // state transition became durable.
        reopened
            .resolve(&proof_scope(&row, RecoveredAckKind::Stored))
            .expect("record proven selection");
        drop(reopened);
        let reopened = open(&root);
        assert_eq!(
            reopened.rows().next().map(|(_, row)| row.state),
            Some(PendingStoredAckState::StoredAckPending)
        );
    }

    #[test]
    fn selection_guard_excludes_retry_only_while_compare_is_in_flight() {
        let root = TestRoot::new();
        let mut journal = open(&root);
        let row = record();
        let reservation_id = [40; 32];
        let input_closure = row.captured_work.input_closure_id;
        let worker_closure = row.worker_closure_id;
        let reservation = PendingAckReservationRecord {
            id: reservation_id,
            stage: PendingAckReservationStage::OfferMayBeSent,
            owner_endpoint_id: row.owner_endpoint_id,
            identity: Some(assignment_identity(&row)),
            capture: Some(captured_work(&row)),
        };
        journal
            .publish(BTreeMap::from([(
                reservation_id,
                PendingStoredAckEntry::Reservation(reservation),
            )]))
            .expect("persist offered reservation");
        assert_eq!(
            journal.gc_retention_closure_ids().collect::<Vec<_>>(),
            vec![input_closure],
            "offered assignment must retain its captured input closure"
        );
        let journal = Arc::new(Mutex::new(journal));

        let (_, guard) = PendingStoredAckJournal::prepare_selection_guarded(
            &journal,
            PendingAckCapacityReservation { id: reservation_id },
            row,
        )
        .expect("persist intent and claim one in-flight compare atomically");
        let mut retained = journal
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .gc_retention_closure_ids()
            .collect::<Vec<_>>();
        retained.sort_unstable();
        let mut expected = vec![input_closure, worker_closure];
        expected.sort_unstable();
        assert_eq!(
            retained, expected,
            "selection intent must retain both captured input and worker result closures"
        );
        assert_eq!(
            journal
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .retry_rows()
                .count(),
            0,
            "sweeper must not race the foreground compare"
        );

        drop(guard);
        assert_eq!(
            journal
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .retry_rows()
                .count(),
            1,
            "a returned compare leaves the durable intent retryable"
        );
    }

    #[test]
    fn journal_round_trips_scoped_ipv6_worker_address_exactly() {
        let root = TestRoot::new();
        let mut value = record();
        value.worker_address = SocketAddr::V6(SocketAddrV6::new(
            "fe80::1234".parse().expect("IPv6 address"),
            4331,
            7,
            12,
        ));
        let expected = value.worker_address;
        let mut journal = open(&root);
        journal.prepare(value).expect("prepare");
        drop(journal);

        let reopened = open(&root);
        assert_eq!(
            reopened.rows().next().map(|(_, row)| row.worker_address),
            Some(expected)
        );
    }

    #[test]
    fn decoded_selection_rejects_non_endpoint_owner_and_worker_keys() {
        let row = record();
        let invalid_public_key = [2; 32];
        assert!(EndpointId::from_bytes(&invalid_public_key).is_err());

        let mut invalid_owner = row.encode().expect("encode valid row");
        invalid_owner[1..33].copy_from_slice(&invalid_public_key);
        assert!(matches!(
            PendingStoredAckRecord::decode(&invalid_owner),
            Err(PendingStoredAckError::Invalid("owner endpoint"))
        ));

        let mut invalid_worker = row.encode().expect("encode valid row");
        invalid_worker[33..65].copy_from_slice(&invalid_public_key);
        assert!(matches!(
            PendingStoredAckRecord::decode(&invalid_worker),
            Err(PendingStoredAckError::Invalid("worker endpoint"))
        ));
    }

    #[test]
    fn durable_product_key_reopens_only_from_canonical_components() {
        let value = record();
        let key = value.product_semantic_key().expect("canonical key reopens");
        assert_eq!(key.package().as_str(), value.product_key.package.as_ref());
        assert_eq!(
            key.coordinate().as_str(),
            value.product_key.coordinate.as_ref()
        );
        assert_eq!(<[u8; 2]>::from(key.profile()), value.trust.profile);

        let mut noncanonical = value;
        noncanonical.product_key.profile = "0003/LOWER-IR".into();
        assert!(matches!(
            noncanonical.product_semantic_key(),
            Err(PendingStoredAckError::Invalid(_))
        ));
    }

    #[test]
    fn terminal_superseded_state_cannot_be_resurrected_as_stored() {
        let root = TestRoot::new();
        let mut journal = open(&root);
        let row = record();
        journal.prepare(row.clone()).expect("prepare");
        journal
            .resolve(&proof_scope(&row, RecoveredAckKind::Superseded))
            .expect("record exact superseded proof");
        assert!(matches!(
            journal.resolve(&proof_scope(&row, RecoveredAckKind::Stored)),
            Err(PendingStoredAckError::TerminalState)
        ));
        drop(journal);
        assert_eq!(
            open(&root).rows().next().map(|(_, row)| row.state),
            Some(PendingStoredAckState::SupersededAckPending)
        );
    }

    #[test]
    fn stored_state_cannot_be_changed_to_a_terminal_rejection() {
        let root = TestRoot::new();
        let mut journal = open(&root);
        let row = record();
        journal.prepare(row.clone()).expect("prepare");
        journal
            .resolve(&proof_scope(&row, RecoveredAckKind::Stored))
            .expect("record exact selected proof");
        assert!(matches!(
            journal.resolve(&proof_scope(&row, RecoveredAckKind::Superseded)),
            Err(PendingStoredAckError::TerminalState)
        ));
        assert!(matches!(
            PendingStoredAckState::from_tag(5),
            Err(PendingStoredAckError::Corrupt("state"))
        ));
    }

    #[test]
    fn awaiting_retirement_confirmation_survives_cold_reopen_without_downgrade() {
        let root = TestRoot::new();
        let mut journal = open(&root);
        let mut row = record();
        journal.prepare(row.clone()).expect("prepare");
        row.state = PendingStoredAckState::StoredAwaitingRetirementConfirm;
        journal
            .publish(BTreeMap::from([(
                row.id(),
                PendingStoredAckEntry::Selection(row.clone()),
            )]))
            .expect("persist awaiting confirmation");
        drop(journal);

        let mut reopened = open(&root);
        assert_eq!(
            reopened.rows().next().map(|(_, row)| row.state),
            Some(PendingStoredAckState::StoredAwaitingRetirementConfirm)
        );
        reopened
            .resolve(&proof_scope(&row, RecoveredAckKind::Stored))
            .expect("repeated selected proof does not downgrade confirm state");
        assert_eq!(
            reopened.rows().next().map(|(_, row)| row.state),
            Some(PendingStoredAckState::StoredAwaitingRetirementConfirm)
        );
    }

    #[test]
    fn retirement_confirmation_survives_worker_trust_revocation() {
        let root = TestRoot::new();
        let mut row = record();
        let peer = EndpointId::from_bytes(&row.worker_peer).expect("worker endpoint");
        let mut trust = TrustedCompilerWorkerPolicy::default();
        trust
            .add(
                crate::compiler_trust::TrustedCompilerWorkerGrant::new(
                    peer,
                    row.worker_address,
                    row.namespace_id,
                    row.trust.recipe,
                    backend_semantic::vocabulary::LanguageProfile::try_from(row.trust.profile)
                        .expect("profile"),
                    backend_semantic::vocabulary::Stage::try_from(row.trust.stage).expect("stage"),
                    row.trust.toolchain,
                    row.trust.environment,
                    row.trust.target_platform,
                )
                .expect("exact worker grant"),
            )
            .expect("trust worker before result retirement");
        assert_eq!(trust.grants().len(), 1);

        let mut journal = open(&root);
        journal.prepare(row.clone()).expect("prepare result intent");
        let mut not_retired = record();
        not_retired.candidate_id = [22; 32];
        journal
            .prepare(not_retired.clone())
            .expect("prepare second result intent");
        journal
            .resolve(&proof_scope(&row, RecoveredAckKind::Stored))
            .expect("selected authority proof");
        let ack_scope = proof_scope(&row, RecoveredAckKind::Stored);
        let retired = backend_engine::cluster_transport::ControlResultRetired::new(
            ack_scope.scope(),
            row.worker_closure_id,
            backend_engine::cluster_transport::ResultAckDisposition::Stored,
            peer,
        )
        .expect("exact durable worker receipt");
        journal
            .mark_awaiting_retirement_confirmation(&ack_scope, &retired)
            .expect("fsync exact retirement receipt");
        assert!(matches!(
            journal.retirement_confirmation_scope(&not_retired.id(), not_retired.owner_endpoint_id),
            Err(PendingStoredAckError::TerminalState)
        ));

        // Revoke after the exact authenticated receipt has been persisted. A
        // confirm-only scope remains available, while the live policy is now
        // deny-all and cannot authorize another Stored ACK.
        trust.revoke_peer(peer).expect("revoke worker");
        assert!(trust.grants().is_empty());
        drop(journal);

        let reopened = open(&root);
        let persisted = reopened
            .rows()
            .find(|(id, _)| **id == row.id())
            .expect("retirement row")
            .1;
        assert_eq!(
            persisted.state,
            PendingStoredAckState::StoredAwaitingRetirementConfirm
        );
        let confirm_only = reopened
            .retirement_confirmation_scope(&row.id(), row.owner_endpoint_id)
            .expect("revoke does not strand exact retirement confirmation");
        assert!(confirm_only.awaiting_retirement_confirmation());
        assert_eq!(confirm_only.kind(), RecoveredAckKind::Stored);
        assert!(!confirm_only.is_currently_trusted(&trust));
    }

    fn superseded_proof(root: &TestRoot) -> SupersededAttemptProof {
        let namespace = backend_extension_turso::AuthorityNamespace::package_metadata(
            "pkg:cargo/pending-stored-cleanup",
            "registry:crates-io",
            "locald",
            "locald-product-v1",
        )
        .expect("valid authority namespace");
        let mut authority = futures_executor::block_on(
            backend_extension_turso::TursoAuthority::open(root.0.join("authority.turso")),
        )
        .expect("open test authority");
        let observation = backend_extension_turso::SourceObservation::new(
            namespace.clone(),
            Some([31; 32]),
            1,
            backend_extension_turso::SourceObservationValue::KnownCount(1),
        )
        .expect("valid source observation");
        let observed = futures_executor::block_on(authority.record_source_observation(observation))
            .expect("record source observation");
        let retired =
            futures_executor::block_on(authority.begin_attempt(&namespace, [32; 32], &observed))
                .expect("begin attempt to supersede");
        let _current =
            futures_executor::block_on(authority.begin_attempt(&namespace, [33; 32], &observed))
                .expect("begin superseding attempt");
        futures_executor::block_on(authority.superseded_attempt_proof(
            &namespace,
            *retired.attempt_id(),
            retired.epoch(),
            retired.fence_bytes(),
        ))
        .expect("read exact superseded proof")
        .expect("later attempt superseded the exact old token")
    }

    #[test]
    fn selected_cleanup_is_unavailable_to_superseded_and_nonselection_rows() {
        let root = TestRoot::new();
        let mut journal = open(&root);
        let selected_row = record();
        let selected_id = journal
            .prepare(selected_row.clone())
            .expect("persist selection intent");
        let superseded = PendingRemoteResultProof::Superseded(superseded_proof(&root));
        assert!(matches!(
            journal.selected_stored_cleanup_scope(
                &selected_id,
                selected_row.owner_endpoint_id,
                &superseded,
            ),
            Err(PendingStoredAckError::Invalid(
                "selected cleanup requires immutable selection history"
            ))
        ));

        let reservation_id = [41; 32];
        let reservation = PendingAckReservationRecord {
            id: reservation_id,
            stage: PendingAckReservationStage::OfferMayBeSent,
            owner_endpoint_id: selected_row.owner_endpoint_id,
            identity: Some(assignment_identity(&selected_row)),
            capture: Some(captured_work(&selected_row)),
        };
        journal
            .publish(BTreeMap::from([(
                reservation_id,
                PendingStoredAckEntry::Reservation(reservation),
            )]))
            .expect("persist offer reservation");
        assert!(matches!(
            journal.selected_cleanup_row(&reservation_id, selected_row.owner_endpoint_id),
            Err(PendingStoredAckError::TerminalState)
        ));
        assert!(matches!(
            journal.selected_stored_cleanup_scope(
                &reservation_id,
                selected_row.owner_endpoint_id,
                &superseded,
            ),
            Err(PendingStoredAckError::TerminalState)
        ));

        let rejected = PendingRejectedAckRecord {
            state: PendingRejectedAckState::AckPending,
            identity: assignment_identity(&selected_row),
            closure_id: selected_row.worker_closure_id,
            reason: backend_engine::cluster_transport::ResultRejectReason::Admission,
        };
        let rejected_id = rejected.id();
        journal
            .publish(BTreeMap::from([(
                rejected_id,
                PendingStoredAckEntry::RejectedAdmission(rejected),
            )]))
            .expect("persist rejection intent");
        assert!(matches!(
            journal.selected_cleanup_row(&rejected_id, selected_row.owner_endpoint_id),
            Err(PendingStoredAckError::TerminalState)
        ));
        assert!(matches!(
            journal.selected_stored_cleanup_scope(
                &rejected_id,
                selected_row.owner_endpoint_id,
                &superseded,
            ),
            Err(PendingStoredAckError::TerminalState)
        ));
    }

    #[test]
    fn persisted_admission_rejection_remains_rejection_only_after_trust_revocation() {
        let root = TestRoot::new();
        let source = record();
        let rejected = PendingRejectedAckRecord {
            state: PendingRejectedAckState::AckPending,
            identity: assignment_identity(&source),
            closure_id: source.worker_closure_id,
            reason: backend_engine::cluster_transport::ResultRejectReason::Admission,
        };
        let id = rejected.id();
        let mut journal = open(&root);
        journal
            .publish(BTreeMap::from([(
                id,
                PendingStoredAckEntry::RejectedAdmission(rejected),
            )]))
            .expect("persist exact admission rejection");
        drop(journal);

        let reopened = open(&root);
        let scope = reopened
            .rejected_ack_scope(&id, source.owner_endpoint_id)
            .expect("durable rejection remains retryable without live execution grant");
        assert_eq!(scope.kind(), RecoveredAckKind::RejectedAdmission);
        assert_eq!(
            scope.disposition(),
            backend_engine::cluster_transport::ResultAckDisposition::Rejected(
                backend_engine::cluster_transport::ResultRejectReason::Admission
            )
        );
        assert_eq!(scope.closure_id(), source.worker_closure_id);
        assert_eq!(scope.scope().namespace_id, source.namespace_id);
    }

    #[test]
    fn unreachable_or_lost_ack_response_retains_row_for_idempotent_retry() {
        let root = TestRoot::new();
        let mut journal = open(&root);
        let row = record();
        journal.prepare(row.clone()).expect("prepare");
        journal
            .resolve(&proof_scope(&row, RecoveredAckKind::Stored))
            .expect("prove selection");

        // A failed connection or a crash after delivery but before local
        // deletion performs no journal mutation; a later process reopens the
        // exact row and can retry the idempotent Stored ACK.
        drop(journal);
        let mut retry = open(&root);
        assert_eq!(
            retry.rows().next().map(|(_, row)| row.state),
            Some(PendingStoredAckState::StoredAckPending)
        );
    }

    #[test]
    fn truncated_and_checksum_corrupt_journals_fail_closed() {
        let root = TestRoot::new();
        let mut journal = open(&root);
        journal.prepare(record()).expect("prepare");
        drop(journal);
        let path = root.0.join(JOURNAL_DIRECTORY).join(JOURNAL_FILE);
        let bytes = fs::read(&path).expect("read journal");

        fs::write(&path, &bytes[..bytes.len() - 1]).expect("truncate");
        assert!(matches!(
            PendingStoredAckJournal::open(&root.0),
            Err(PendingStoredAckError::Corrupt(_))
        ));

        let mut corrupt = bytes;
        let body_offset = 8 + 2 + 2 + 4;
        corrupt[body_offset] ^= 1;
        fs::write(&path, corrupt).expect("corrupt");
        assert!(matches!(
            PendingStoredAckJournal::open(&root.0),
            Err(PendingStoredAckError::Corrupt("checksum"))
        ));
    }

    #[test]
    fn replacement_with_wrong_candidate_identity_does_not_replace_pending_row() {
        let root = TestRoot::new();
        let mut journal = open(&root);
        let mut original = record();
        let id = journal.prepare(original.clone()).expect("prepare");
        original.candidate_id = [21; 32];
        let replacement_id = original.id();
        assert_ne!(id, replacement_id);
        journal
            .prepare(original)
            .expect("distinct selection remains a distinct unresolved intent");
        assert_eq!(journal.rows().count(), 2);
        assert_eq!(
            journal
                .rows()
                .find(|(row_id, _)| **row_id == id)
                .map(|(_, row)| row.candidate_id),
            Some([13; 32])
        );
    }

    #[test]
    fn cold_open_reclaims_only_unoffered_reservations() {
        let root = TestRoot::new();
        let mut journal = open(&root);
        let row = record();
        let unbound = PendingAckReservationRecord {
            id: [31; 32],
            stage: PendingAckReservationStage::Unbound,
            owner_endpoint_id: row.owner_endpoint_id,
            identity: None,
            capture: None,
        };
        let bound = PendingAckReservationRecord {
            id: [32; 32],
            stage: PendingAckReservationStage::BoundBeforeOffer,
            owner_endpoint_id: row.owner_endpoint_id,
            identity: Some(assignment_identity(&row)),
            capture: Some(captured_work(&row)),
        };
        let offered = PendingAckReservationRecord {
            id: [33; 32],
            stage: PendingAckReservationStage::OfferMayBeSent,
            owner_endpoint_id: row.owner_endpoint_id,
            identity: Some(assignment_identity(&row)),
            capture: Some(captured_work(&row)),
        };
        journal
            .publish(BTreeMap::from([
                (unbound.id, PendingStoredAckEntry::Reservation(unbound)),
                (bound.id, PendingStoredAckEntry::Reservation(bound)),
                (offered.id, PendingStoredAckEntry::Reservation(offered)),
            ]))
            .expect("persist all crash-boundary states");
        drop(journal);

        let reopened = open(&root);
        assert_eq!(reopened.capacity_count(), 1);
        assert_eq!(reopened.offered_reservation_count(), 1);
        assert!(reopened.rows().next().is_none());
        assert!(matches!(
            reopened.retry_rows().next(),
            Some((id, PendingStoredAckEntry::Reservation(_))) if *id == [33; 32]
        ));
    }

    #[test]
    fn capacity_is_reserved_durably_and_full_state_is_visible() {
        let root = TestRoot::new();
        let owner = test_endpoint_id(41);
        let mut journal = open(&root);
        for _ in 0..MAX_ROWS {
            journal.reserve_capacity(owner).expect("reserve one slot");
        }
        assert_eq!(journal.capacity_count(), MAX_ROWS);
        assert!(matches!(
            journal.reserve_capacity(owner),
            Err(PendingStoredAckError::CapacityFull)
        ));
        drop(journal);

        // These reservations were never bound to an assignment or offered, so
        // cold startup can safely release them and recover capacity.
        assert_eq!(open(&root).capacity_count(), 0);
    }
}
