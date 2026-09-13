//! Recovered attempt model and restart action vocabulary.

use super::record::{
    AcceptedResultProof, DispatchAttemptKey, DispatchPhase, NotificationCursor, PublicationAck,
    RemoteAttemptIntent, TerminalState, TransferCheckpointRef,
};
use crate::journal::JournalScan;
use blake3::Hasher;
use std::collections::BTreeMap;

mod sealed {
    /// Marker implemented only by capabilities minted inside the engine.
    /// Keeping this trait private prevents an application from manufacturing
    /// a restart policy for an arbitrary epoch or revocation value.
    pub trait RestartAuthority {}
}

/// One recovered attempt with all durable progress needed for a restart.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RecoveredAttempt {
    /// Original admitted remote intent.
    pub intent: RemoteAttemptIntent,
    /// Current durable phase.
    pub phase: DispatchPhase,
    /// Current owner fence. It changes only through a Fenced record.
    pub current_fence: [u8; 32],
    /// Latest transfer/checkpoint progress.
    pub transfer: Option<TransferCheckpointRef>,
    /// Accepted proof retained until owner publication is acknowledged.
    pub accepted: Option<AcceptedResultProof>,
    /// Publication acknowledgement, if present.
    pub publication: Option<PublicationAck>,
    /// Terminal cancellation/fallback state, if present.
    pub terminal: Option<TerminalState>,
    /// Fence epoch/reason, if a takeover was recorded.
    pub fenced: Option<(u64, u16)>,
    /// Latest waiter/subscription cursor.
    pub cursor: Option<NotificationCursor>,
}

impl RecoveredAttempt {
    /// Returns the stable attempt identity.
    #[must_use]
    pub const fn key(&self) -> DispatchAttemptKey {
        self.intent.key
    }

    /// Returns whether the accepted proof still needs owner publication.
    #[must_use]
    pub const fn publication_pending(&self) -> bool {
        self.accepted.is_some() && self.publication.is_none()
    }
}

/// Bounded result of one streaming dispatch replay.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DispatchRecovery {
    pub(super) attempts: BTreeMap<DispatchAttemptKey, RecoveredAttempt>,
    /// Number of complete frames folded.
    pub frames_scanned: usize,
    /// Number of bytes consumed by the scanner.
    pub bytes_scanned: u64,
    /// Largest frame payload allocated by the scanner.
    pub peak_payload_bytes: usize,
    /// Last valid sequence, if one exists.
    pub last_sequence: Option<u64>,
    /// Whether a torn final frame was repaired.
    pub truncated_tail: bool,
}

impl DispatchRecovery {
    /// Returns the number of retained attempts.
    #[must_use]
    pub fn len(&self) -> usize {
        self.attempts.len()
    }

    /// Returns whether no attempts were retained.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.attempts.is_empty()
    }

    /// Looks up one recovered attempt.
    #[must_use]
    pub fn get(&self, key: DispatchAttemptKey) -> Option<&RecoveredAttempt> {
        self.attempts.get(&key)
    }

    /// Iterates recovered attempts in stable key order.
    pub fn iter(&self) -> impl Iterator<Item = (&DispatchAttemptKey, &RecoveredAttempt)> {
        self.attempts.iter()
    }

    /// Returns the bounded recovered map.
    #[must_use]
    pub fn into_attempts(self) -> BTreeMap<DispatchAttemptKey, RecoveredAttempt> {
        self.attempts
    }
}

/// The authority state observed by one restart decision.
///
/// The snapshot is carried into every fencing decision. This prevents a
/// restart from manufacturing `old_epoch + 1` and accidentally publishing
/// under an epoch that is no longer current. The notification watermark is
/// part of the same capability so fallback can resume waiters without
/// resetting their durable cursor to zero.
///
/// ```compile_fail
/// use backend_engine::AuthoritySnapshot;
/// let _forged = AuthoritySnapshot::new(7, 11);
/// ```
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RestartAuthoritySnapshot {
    /// Current owner epoch.
    owner_epoch: u64,
    /// Current authority revocation observation.
    revocation_version: u64,
    /// Current owner notification watermark.
    notification_cursor: u64,
}

impl RestartAuthoritySnapshot {
    const fn from_observation(owner_epoch: u64, revocation_version: u64) -> Self {
        Self {
            owner_epoch,
            revocation_version,
            notification_cursor: 0,
        }
    }

    /// Adds the current notification watermark to this authority capability.
    #[must_use]
    const fn with_notification_cursor(mut self, notification_cursor: u64) -> Self {
        self.notification_cursor = notification_cursor;
        self
    }

    pub(super) const fn owner_epoch(&self) -> u64 {
        self.owner_epoch
    }

    pub(super) const fn revocation_version(&self) -> u64 {
        self.revocation_version
    }

    pub(super) const fn notification_cursor(&self) -> u64 {
        self.notification_cursor
    }
}

/// Compatibility name for the owner-restart snapshot capability. Its
/// constructor remains crate-private.
pub type AuthoritySnapshot = RestartAuthoritySnapshot;

/// Owner-minted restart capability bound to one checked workspace, lease
/// fence, authority epoch, revocation observation, and notification
/// watermark. The constructor is crate-private and the capability carries a
/// domain-separated seal, so a public epoch-shaped value cannot authorize
/// remote work after takeover.
///
/// ```compile_fail
/// use backend_engine::OwnerRestartAuthority;
/// let _forged = OwnerRestartAuthority::mint([1; 32], 7, [2; 32], 11, 0);
/// ```
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct OwnerRestartAuthority {
    snapshot: AuthoritySnapshot,
    workspace_root: [u8; 32],
    owner_fence: [u8; 32],
    seal: [u8; 32],
}

impl OwnerRestartAuthority {
    /// Mints a restart capability from the currently held workspace owner.
    pub(crate) fn mint(
        workspace_root: [u8; 32],
        owner_epoch: u64,
        owner_fence: [u8; 32],
        revocation_version: u64,
        notification_cursor: u64,
    ) -> Self {
        let snapshot = AuthoritySnapshot::from_observation(owner_epoch, revocation_version)
            .with_notification_cursor(notification_cursor);
        let seal = seal_authority(
            workspace_root,
            owner_epoch,
            owner_fence,
            revocation_version,
            notification_cursor,
        );
        Self {
            snapshot,
            workspace_root,
            owner_fence,
            seal,
        }
    }

    fn is_sealed(&self) -> bool {
        self.seal
            == seal_authority(
                self.workspace_root,
                self.snapshot.owner_epoch,
                self.owner_fence,
                self.snapshot.revocation_version,
                self.snapshot.notification_cursor,
            )
            && self.owner_fence != [0; 32]
            && self.workspace_root != [0; 32]
    }

    fn snapshot(self) -> AuthoritySnapshot {
        self.snapshot
    }

    pub(crate) fn matches_owner(
        &self,
        workspace_root: [u8; 32],
        owner_epoch: u64,
        owner_fence: [u8; 32],
    ) -> bool {
        self.is_sealed()
            && self.workspace_root == workspace_root
            && self.snapshot.owner_epoch == owner_epoch
            && self.owner_fence == owner_fence
    }
}

impl sealed::RestartAuthority for OwnerRestartAuthority {}

fn seal_authority(
    workspace_root: [u8; 32],
    owner_epoch: u64,
    owner_fence: [u8; 32],
    revocation_version: u64,
    notification_cursor: u64,
) -> [u8; 32] {
    let mut hasher = Hasher::new();
    hasher.update(b"backend.engine.dispatch.owner-restart-authority.v1\0");
    hasher.update(&workspace_root);
    hasher.update(&owner_epoch.to_be_bytes());
    hasher.update(&owner_fence);
    hasher.update(&revocation_version.to_be_bytes());
    hasher.update(&notification_cursor.to_be_bytes());
    *hasher.finalize().as_bytes()
}

/// A typed result from the authority check used by restart.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DispatchRestartDecision {
    /// The exact admitted owner/revocation pair may retain remote rights.
    Permit(AuthoritySnapshot),
    /// The attempt must be fenced under this current authority and sent to
    /// local fallback.
    Fence {
        /// Current owner capability used to mint the replacement fence.
        current: AuthoritySnapshot,
        /// Stable reason code recorded in the journal.
        reason: u16,
    },
    /// The supplied authority is older than the durable attempt. The caller
    /// must obtain a newer owner snapshot before mutating the journal.
    Defer,
}

/// Restart authority/capability check supplied by the owner.
///
/// Lease expiry is checked by the journal. This trait supplies the current
/// authority snapshot and makes revocation a required part of the decision;
/// a bare boolean cannot accidentally authorize a stale epoch. The sealed
/// supertrait means the only production implementation is
/// [`OwnerRestartAuthority`], which the workspace owner mints.
///
/// ```compile_fail
/// use backend_engine::{DispatchRestartAuthority, DispatchRestartDecision, RecoveredAttempt};
/// struct Forged;
/// impl DispatchRestartAuthority for Forged {
///     fn decide(&self, _: &RecoveredAttempt, _: u64) -> DispatchRestartDecision {
///         DispatchRestartDecision::Defer
///     }
/// }
/// ```
pub trait DispatchRestartAuthority: sealed::RestartAuthority {
    /// Classifies one recovered attempt under the current owner capability.
    fn decide(&self, attempt: &RecoveredAttempt, now: u64) -> DispatchRestartDecision;
}

impl DispatchRestartAuthority for OwnerRestartAuthority {
    fn decide(&self, attempt: &RecoveredAttempt, _now: u64) -> DispatchRestartDecision {
        if !self.is_sealed() {
            return DispatchRestartDecision::Defer;
        }
        // A matching epoch alone is insufficient: the durable intent is
        // bound to the selected workspace root and full-width owner fence
        // that admitted it. A copied capability with stale root/fence must
        // fence the remote attempt before any resend.
        if attempt.intent.root != self.workspace_root || attempt.intent.fence != self.owner_fence {
            return DispatchRestartDecision::Fence {
                current: self.snapshot,
                reason: super::error::RESTART_OWNER_TAKEOVER,
            };
        }
        if self.snapshot.owner_epoch < attempt.intent.owner_epoch {
            return DispatchRestartDecision::Defer;
        }
        if self.snapshot.owner_epoch == attempt.intent.owner_epoch
            && self.snapshot.revocation_version == attempt.intent.revocation_version
        {
            DispatchRestartDecision::Permit(self.snapshot())
        } else {
            DispatchRestartDecision::Fence {
                current: self.snapshot(),
                reason: if self.snapshot.owner_epoch == attempt.intent.owner_epoch {
                    super::error::RESTART_AUTHORITY_REVOKED
                } else {
                    super::error::RESTART_OWNER_TAKEOVER
                },
            }
        }
    }
}

impl NotificationCursor {
    /// Returns the durable watermark represented by waiter/subscription
    /// positions. The larger position is safe for a monotonic owner cursor.
    #[must_use]
    pub const fn watermark(&self) -> u64 {
        if self.waiter >= self.subscription {
            self.waiter
        } else {
            self.subscription
        }
    }
}

/// Safe action selected for one nonterminal attempt after restart.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PublicationRecoveryMode {
    /// The accepted proof was durable, but publication was never marked as started.
    Start,
    /// Publication was marked in flight, so the store outcome must be reconciled first.
    Reconcile,
}

/// Safe action selected for one nonterminal attempt after restart.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DispatchRecoveryAction {
    /// Rebind and resend the exact admitted request.
    Resend {
        /// Recovered intent and transfer cursor.
        attempt: RecoveredAttempt,
    },
    /// Retry owner publication using the durable accepted proof.
    PublishAccepted {
        /// Recovered attempt.
        attempt: RecoveredAttempt,
        /// Proof that must remain available until the owner acknowledges.
        proof: AcceptedResultProof,
        /// Whether recovery may start publication or must reconcile an ambiguous call.
        mode: PublicationRecoveryMode,
    },
    /// Remote rights were fenced and local fallback is now authoritative.
    Fallback {
        /// Recovered attempt after durable fencing/terminal fallback.
        attempt: RecoveredAttempt,
        /// Accepted proof, if one existed before fallback.
        proof: Option<AcceptedResultProof>,
        /// Stable reason for the fallback.
        reason: u16,
    },
}

/// Builds a bounded public recovery report from folded state and scanner metrics.
pub(super) fn recovery_from_state(
    attempts: BTreeMap<DispatchAttemptKey, RecoveredAttempt>,
    scan: &JournalScan<super::record::DispatchLog>,
) -> DispatchRecovery {
    DispatchRecovery {
        attempts,
        frames_scanned: scan.frames_scanned,
        bytes_scanned: scan.bytes_scanned,
        peak_payload_bytes: scan.peak_payload_bytes,
        last_sequence: scan.last_sequence,
        truncated_tail: scan.truncated_tail,
    }
}
