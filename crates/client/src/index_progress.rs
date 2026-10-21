//! Conditional reads of existing index receipts. No mutation, owner wait, or
//! retained server-side observation cache is introduced by this policy.

use crate::ClientError;
#[cfg(any(unix, windows))]
use crate::RequestContext;
use backend_library::{
    IndexJobObservation, IndexJobStage, IndexJobTicket, IndexOperationKey,
    IndexOperationObservation, IndexOperationState, IndexOperationStatus, SurfaceCommand,
    SurfaceReply,
};
use serde::{Deserialize, Serialize};
use std::time::Duration;
#[cfg(any(unix, windows))]
use std::time::Instant;

/// Maximum duration of one conditional observation, within its original read scope.
pub const MAX_INDEX_PROGRESS_WAIT_MILLIS: u64 = 10_000;
/// Caller observation ceiling for a durable operation, separate from owner work.
pub const MAX_INDEX_OPERATION_WAIT: Duration = crate::CLIENT_MUTATION_TIMEOUT;
const PROBE_INTERVAL: Duration = Duration::from_millis(500);
/// Minimum distance between the starts of two consecutive reads of one wait.
/// A fast-changing operation answers every conditional read at once, and each
/// read spends a frame of the owner's per-connection budget, so an operation
/// that changes continuously must not turn the wait into a busy loop.
const MIN_READ_SPACING: Duration = Duration::from_millis(100);
const CANCELLATION_INTERVAL: Duration = Duration::from_millis(25);

/// Commitment to the complete admitted active status, including its owner
/// ticket, exact request, source generation, coverage and profile outcomes.
/// This is an observation reference, never authority to restart work.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct IndexObservationFingerprint([u8; 32]);

impl TryFrom<String> for IndexObservationFingerprint {
    type Error = &'static str;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        if value.len() != 64
            || !value
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        {
            return Err(
                "after_observation must be the exact 64-character lowercase observation fingerprint",
            );
        }
        blake3::Hash::from_hex(value)
            .map(|hash| Self(*hash.as_bytes()))
            .map_err(|_| "invalid observation fingerprint")
    }
}

impl From<IndexObservationFingerprint> for String {
    fn from(value: IndexObservationFingerprint) -> Self {
        blake3::Hash::from_bytes(value.0).to_hex().to_string()
    }
}

impl IndexObservationFingerprint {
    /// Only an admitted active status has an issued owner witness. Accepted,
    /// terminal and ambiguous evidence cannot produce a conditional cursor.
    /// # Errors
    /// Returns a protocol error if the complete status fails admission/encoding.
    pub fn active(status: &IndexOperationStatus) -> Result<Option<Self>, ClientError> {
        if !matches!(status.state, IndexOperationState::Active { .. }) {
            return Ok(None);
        }
        SurfaceReply::IndexOperationStatus(IndexOperationObservation::Known(status.clone()))
            .admit(backend_library::CommandId::IndexProgress)
            .map_err(|error| ClientError::Protocol(error.to_string()))?;
        Self::hash_active(status).map(Some)
    }

    // The progress reader already admitted its borrowed full reply. Hash it
    // without cloning a captured source receipt on every inner probe.
    fn hash_active(status: &IndexOperationStatus) -> Result<Self, ClientError> {
        struct Digest(blake3::Hasher);
        impl std::io::Write for Digest {
            fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
                self.0.update(bytes);
                Ok(bytes.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        let mut digest = Digest(blake3::Hasher::new());
        digest.0.update(b"nudox/index-active-observation/v1\0");
        serde_json::to_writer(&mut digest, status)
            .map_err(|error| ClientError::Protocol(error.to_string()))?;
        Ok(Self(*digest.0.finalize().as_bytes()))
    }
}

/// A still-pending reference to the caller's previous complete observation.
/// No publication or terminal outcome is inferred from unchanged progress.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(tag = "identity", rename_all = "kebab-case")]
pub enum IndexProgressReference {
    /// The same complete durable status and actual issued owner ticket.
    Operation {
        /// Exact caller-owned operation key.
        operation_key: IndexOperationKey,
        /// Actual owner/job/package identity from the admitted active status.
        ticket: IndexJobTicket,
        /// Full exact status commitment supplied on the conditional read.
        after_observation: IndexObservationFingerprint,
    },
    /// No events beyond this exact job's existing sequence cursor.
    Ticket {
        /// Exact issued job identity.
        ticket: IndexJobTicket,
        /// Last consumed event sequence, not a terminal-state cursor.
        after_sequence: u64,
        /// Current stage, observed again at the end of the bounded wait.
        stage: IndexJobStage,
    },
}

/// A bounded read of an existing command, not a new owner protocol.
#[derive(Clone, Debug)]
pub struct IndexProgressRead {
    command: SurfaceCommand,
    after_observation: Option<IndexObservationFingerprint>,
    wait: Duration,
}

impl IndexProgressRead {
    /// Admits the additive conditional options before any owner call.
    /// Zero wait is a conditional immediate read; omitted options keep old
    /// full/immediate clients on their established path.
    /// # Errors
    /// Refuses other commands, mixed identities, or a wait beyond the fixed bound.
    pub fn new(
        command: SurfaceCommand,
        after_observation: Option<IndexObservationFingerprint>,
        wait_millis: u64,
    ) -> Result<Self, ClientError> {
        command
            .admit()
            .map_err(|error| ClientError::Protocol(error.to_string()))?;
        match &command {
            SurfaceCommand::IndexOperationStatus { .. } if wait_millis == 0 || after_observation.is_some() => {},
            SurfaceCommand::IndexProgress { .. } if after_observation.is_none() => {},
            _ => return Err(ClientError::Protocol("conditional progress needs either an exact operation observation or a ticket/sequence; it cannot start or cancel work".to_owned())),
        }
        if wait_millis > MAX_INDEX_PROGRESS_WAIT_MILLIS {
            return Err(ClientError::Protocol(
                "wait_millis must be between 0 and 10000".to_owned(),
            ));
        }
        Ok(Self {
            command,
            after_observation,
            wait: Duration::from_millis(wait_millis),
        })
    }

    /// Existing immediate owner command used for every read.
    #[must_use]
    pub fn command(&self) -> &SurfaceCommand {
        &self.command
    }

    fn unchanged(
        &self,
        reply: &SurfaceReply,
    ) -> Result<Option<IndexProgressReference>, ClientError> {
        reply
            .admit(self.command.id())
            .map_err(|error| ClientError::Protocol(error.to_string()))?;
        match (&self.command, reply) {
            (
                SurfaceCommand::IndexOperationStatus { operation_key },
                SurfaceReply::IndexOperationStatus(observation),
            ) => {
                let actual_key = match observation {
                    IndexOperationObservation::Known(status) => status.operation_key,
                    IndexOperationObservation::Unknown { operation_key }
                    | IndexOperationObservation::OutsideReceiptWindow { operation_key, .. } => {
                        *operation_key
                    }
                };
                if actual_key != *operation_key {
                    return Err(ClientError::Protocol(
                        "progress reply changed operation key".to_owned(),
                    ));
                }
                if let IndexOperationObservation::Known(status) = observation
                    && let IndexOperationState::Active { ticket, .. } = &status.state
                    && let Some(after) = self.after_observation
                    && IndexObservationFingerprint::hash_active(status)? == after
                {
                    return Ok(Some(IndexProgressReference::Operation {
                        operation_key: *operation_key,
                        ticket: ticket.clone(),
                        after_observation: after,
                    }));
                }
                Ok(None)
            }
            (
                SurfaceCommand::IndexProgress {
                    ticket,
                    after_sequence,
                },
                SurfaceReply::IndexProgress(observation),
            ) => {
                let actual_ticket = match observation {
                    IndexJobObservation::Pending(page) => &page.ticket,
                    IndexJobObservation::Terminal(terminal) => &terminal.ticket,
                    IndexJobObservation::Unknown { ticket, .. } => ticket,
                };
                if actual_ticket != ticket {
                    return Err(ClientError::Protocol(
                        "progress reply changed owner ticket".to_owned(),
                    ));
                }
                if let IndexJobObservation::Pending(page) = observation
                    && page.events.is_empty()
                    && !page.truncated
                    && !page.has_more
                    && page.next_sequence == *after_sequence
                {
                    return Ok(Some(IndexProgressReference::Ticket {
                        ticket: ticket.clone(),
                        after_sequence: *after_sequence,
                        stage: page.stage,
                    }));
                }
                Ok(None)
            }
            _ => Err(ClientError::Protocol(
                "progress reply changed shape".to_owned(),
            )),
        }
    }
}

/// Complete changed facts or an explicitly still-pending observation reference.
#[derive(Debug)]
pub enum IndexProgressUpdate {
    /// Always used for terminal, unknown, unresolved, replacement and changed facts.
    Changed(SurfaceReply),
    /// Only after an actual unchanged pending read at the wait boundary.
    Unchanged(IndexProgressReference),
}

#[path = "index_compiler_progress.rs"]
mod compiler_progress;
pub use compiler_progress::{IndexCompilerProgressUpdate, IndexOperationProgress};
#[cfg(any(unix, windows))]
pub use compiler_progress::{observe_index_operation_progress, read_index_operation_progress};

/// Why an exact-operation wait ended without an admitted terminal receipt.
#[derive(Debug)]
pub enum IndexOperationWaitStop {
    /// Unknown, unresolved or expired evidence cannot establish completion.
    Uncertain,
    /// The original publication worker could not finish. Stop this caller's
    /// wait without changing or cancelling the active durable operation.
    PublicationPending {
        /// Bounded owner-reported publication error.
        detail: backend_library::ProductText,
        /// The owner retained only the bounded prefix of the original detail.
        detail_truncated: bool,
    },
    /// The original caller deadline, cancellation, transport or proof failed.
    Client(ClientError),
}

/// The last complete admitted observation remains available on every exit.
#[derive(Debug)]
pub enum IndexOperationWaitResult {
    /// Published, partially published or failed; never global readiness.
    Terminal(IndexOperationObservation),
    /// Waiting ended without changing the durable operation's outcome.
    Stopped {
        /// Last admitted evidence for the original exact request.
        observation: IndexOperationObservation,
        /// Caller failure or uncertain owner evidence, not an index failure.
        reason: IndexOperationWaitStop,
    },
}

/// Observes only the exact durable request accepted by the original reply.
/// Uses the existing conditional reader and caller's absolute context; it
/// never starts, reconnects, cancels or replaces an index operation. Observers
/// should be nonblocking; arbitrary callbacks cannot be preempted.
/// # Errors
/// Refuses an initial observation that fails typed reply admission.
#[cfg(any(unix, windows))]
pub fn wait_index_operation(
    initial: IndexOperationObservation,
    context: &RequestContext,
    observe: impl FnMut(SurfaceCommand) -> Result<SurfaceReply, ClientError>,
    progress: impl FnMut(&IndexOperationObservation),
) -> Result<IndexOperationWaitResult, ClientError> {
    wait_operation_with_clock(
        initial,
        context,
        observe,
        progress,
        &mut WallClock(Instant::now()),
    )
}

#[cfg(any(unix, windows))]
fn wait_operation_with_clock(
    observation: IndexOperationObservation,
    context: &RequestContext,
    observe: impl FnMut(SurfaceCommand) -> Result<SurfaceReply, ClientError>,
    mut progress: impl FnMut(&IndexOperationObservation),
    clock: &mut impl ProgressClock,
) -> Result<IndexOperationWaitResult, ClientError> {
    wait_operation_with_profiles(
        observation,
        context,
        observe,
        |observation, _| progress(observation),
        clock,
        false,
    )
}

/// Waits for the same exact durable outcome while reporting admitted compiler milestones.
/// # Errors
/// Uses the same admission and original absolute deadline as `wait_index_operation`.
#[cfg(any(unix, windows))]
pub fn wait_index_operation_with_progress(
    initial: IndexOperationObservation,
    context: &RequestContext,
    observe: impl FnMut(SurfaceCommand) -> Result<SurfaceReply, ClientError>,
    progress: impl FnMut(&IndexOperationObservation, Option<&backend_library::IndexProgressPage>),
) -> Result<IndexOperationWaitResult, ClientError> {
    wait_operation_with_profiles(
        initial,
        context,
        observe,
        progress,
        &mut WallClock(Instant::now()),
        true,
    )
}

#[cfg(any(unix, windows))]
fn wait_operation_with_profiles(
    mut observation: IndexOperationObservation,
    context: &RequestContext,
    mut observe: impl FnMut(SurfaceCommand) -> Result<SurfaceReply, ClientError>,
    mut progress: impl FnMut(&IndexOperationObservation, Option<&backend_library::IndexProgressPage>),
    clock: &mut impl ProgressClock,
    include_profiles: bool,
) -> Result<IndexOperationWaitResult, ClientError> {
    let mut profile_progress = None;
    SurfaceReply::IndexOperationStatus(observation.clone())
        .admit(backend_library::CommandId::IndexProgress)
        .map_err(|error| ClientError::Protocol(error.to_string()))?;
    let identity = match &observation {
        IndexOperationObservation::Known(status) => Some((
            status.operation_key,
            status.package.clone(),
            status.execution_intent,
            status.request_digest,
        )),
        _ => None,
    };
    let mut last_read_started: Option<Duration> = None;
    loop {
        progress(&observation, profile_progress.as_ref());
        let status = match &observation {
            IndexOperationObservation::Known(status) => status,
            _ => {
                return Ok(IndexOperationWaitResult::Stopped {
                    observation,
                    reason: IndexOperationWaitStop::Uncertain,
                });
            }
        };
        match status.state {
            IndexOperationState::Published(_)
            | IndexOperationState::PartiallyPublished { .. }
            | IndexOperationState::Failed { .. } => {
                return Ok(IndexOperationWaitResult::Terminal(observation));
            }
            IndexOperationState::Unresolved { .. } => {
                return Ok(IndexOperationWaitResult::Stopped {
                    observation,
                    reason: IndexOperationWaitStop::Uncertain,
                });
            }
            IndexOperationState::Accepted | IndexOperationState::Active { .. } => {}
        }
        if include_profiles && let Some(page) = &profile_progress {
            IndexObservationFingerprint::active_with_profile_progress(status, Some(page))?;
            if let Some(backend_library::IndexJobProgressEvent {
                kind:
                    backend_library::IndexJobProgressKind::PublicationPending {
                        detail,
                        detail_truncated,
                    },
                ..
            }) = page.events.last()
                && !page.has_more
            {
                // A bounded observation can end on historical pending while
                // newer retry progress remains beyond its cursor. Stop only
                // after the current owner history tail is fully observed.
                return Ok(IndexOperationWaitResult::Stopped {
                    observation,
                    reason: IndexOperationWaitStop::PublicationPending {
                        detail: detail.clone(),
                        detail_truncated: *detail_truncated,
                    },
                });
            }
        }
        // A preinstalled shorter absolute scope wins; this never renews it.
        // An already admitted terminal receipt needs no further waiting.
        if let Err(error) = context.read_deadline(MAX_INDEX_OPERATION_WAIT) {
            return Ok(IndexOperationWaitResult::Stopped {
                observation,
                reason: IndexOperationWaitStop::Client(error),
            });
        }
        let after = if include_profiles {
            IndexObservationFingerprint::active_with_profile_progress(
                status,
                profile_progress.as_ref(),
            )?
        } else {
            IndexObservationFingerprint::active(status)?
        };
        let accepted = matches!(status.state, IndexOperationState::Accepted);
        let query = IndexProgressRead::new(
            SurfaceCommand::IndexOperationStatus {
                operation_key: status.operation_key,
            },
            after,
            if accepted {
                0
            } else {
                MAX_INDEX_PROGRESS_WAIT_MILLIS
            },
        )?;
        // Space the reads, not just the unchanged ones: this is after the
        // terminal checks above, so a finished operation is never delayed.
        if let Some(previous) = last_read_started {
            let since = clock.elapsed().saturating_sub(previous);
            if let Some(gap) = MIN_READ_SPACING.checked_sub(since)
                && let Err(error) = clock.wait(gap, context)
            {
                return Ok(IndexOperationWaitResult::Stopped {
                    observation,
                    reason: IndexOperationWaitStop::Client(error),
                });
            }
        }
        last_read_started = Some(clock.elapsed());
        // Install no new deadline: the caller must retain one absolute wait
        // scope, including all conditional probes and replacement connections.
        let result = if include_profiles {
            compiler_progress::read_operation_with_clock(&query, context, &mut observe, clock)
        } else {
            read_with_clock(&query, context, &mut observe, clock).and_then(|update| match update {
                IndexProgressUpdate::Changed(SurfaceReply::IndexOperationStatus(observation)) => {
                    Ok(IndexCompilerProgressUpdate::Changed(
                        IndexOperationProgress {
                            observation,
                            profile_progress: None,
                        },
                    ))
                }
                IndexProgressUpdate::Unchanged(reference) => {
                    Ok(IndexCompilerProgressUpdate::Unchanged(reference))
                }
                _ => Err(ClientError::Protocol(
                    "operation wait reply changed shape".to_owned(),
                )),
            })
        };
        let changed = match result {
            Ok(IndexCompilerProgressUpdate::Changed(changed)) => {
                profile_progress = changed.profile_progress;
                changed.observation
            }
            Ok(IndexCompilerProgressUpdate::Unchanged(_)) => {
                // The preceding slice already read this exact boundary.
                // Retain the existing cadence before opening the next slice.
                if let Err(error) = clock.wait(PROBE_INTERVAL, context) {
                    return Ok(IndexOperationWaitResult::Stopped {
                        observation,
                        reason: IndexOperationWaitStop::Client(error),
                    });
                }
                continue;
            }
            Err(error) => {
                return Ok(IndexOperationWaitResult::Stopped {
                    observation,
                    reason: IndexOperationWaitStop::Client(error),
                });
            }
        };
        let same = match (&changed, &identity) {
            (IndexOperationObservation::Known(status), Some((key, package, intent, digest))) => {
                status.operation_key == *key
                    && status.package == *package
                    && status.execution_intent == *intent
                    && status.request_digest == *digest
            }
            (IndexOperationObservation::Unknown { operation_key }, Some((key, ..))) => {
                operation_key == key
            }
            (
                IndexOperationObservation::OutsideReceiptWindow {
                    operation_key,
                    request_digest,
                },
                Some((key, _, _, digest)),
            ) => operation_key == key && request_digest == digest,
            _ => false,
        };
        if !same {
            return Ok(IndexOperationWaitResult::Stopped {
                observation,
                reason: IndexOperationWaitStop::Client(ClientError::Protocol(
                    "operation wait evidence changed its original request identity".to_owned(),
                )),
            });
        }
        observation = changed;
        if accepted
            && matches!(&observation, IndexOperationObservation::Known(status) if matches!(status.state, IndexOperationState::Accepted))
        {
            if let Err(error) = clock.wait(PROBE_INTERVAL, context) {
                return Ok(IndexOperationWaitResult::Stopped {
                    observation,
                    reason: IndexOperationWaitStop::Client(error),
                });
            }
        }
    }
}

#[cfg(any(unix, windows))]
trait ProgressClock {
    fn elapsed(&self) -> Duration;
    fn wait(&mut self, duration: Duration, context: &RequestContext) -> Result<(), ClientError>;
}

#[cfg(any(unix, windows))]
struct WallClock(Instant);
#[cfg(any(unix, windows))]
impl ProgressClock for WallClock {
    fn elapsed(&self) -> Duration {
        self.0.elapsed()
    }
    fn wait(&mut self, duration: Duration, context: &RequestContext) -> Result<(), ClientError> {
        let started = Instant::now();
        loop {
            context.check()?;
            let remaining = duration.saturating_sub(started.elapsed());
            if remaining.is_zero() {
                return Ok(());
            }
            std::thread::sleep(remaining.min(CANCELLATION_INTERVAL));
        }
    }
}

/// Waits on the caller actor, never the owner control loop. Every read uses
/// the same original context/deadline and exact key/ticket; no reconnect,
/// index mutation, new request lifetime or hidden terminal inference occurs.
/// # Errors
/// Propagates exact cancellation, deadline, transport and admission failures.
#[cfg(any(unix, windows))]
pub fn read_index_progress(
    query: &IndexProgressRead,
    context: &RequestContext,
    observe: impl FnMut(SurfaceCommand) -> Result<SurfaceReply, ClientError>,
) -> Result<IndexProgressUpdate, ClientError> {
    context.read_deadline(crate::CLIENT_REQUEST_TIMEOUT)?;
    read_with_clock(query, context, observe, &mut WallClock(Instant::now()))
}

#[cfg(any(unix, windows))]
fn read_with_clock(
    query: &IndexProgressRead,
    context: &RequestContext,
    mut observe: impl FnMut(SurfaceCommand) -> Result<SurfaceReply, ClientError>,
    clock: &mut impl ProgressClock,
) -> Result<IndexProgressUpdate, ClientError> {
    let started = clock.elapsed();
    loop {
        context.check()?;
        let result = observe(query.command.clone());
        context.check()?;
        let reply = result?;
        let Some(reference) = query.unchanged(&reply)? else {
            return Ok(IndexProgressUpdate::Changed(reply));
        };
        let remaining = query
            .wait
            .saturating_sub(clock.elapsed().saturating_sub(started));
        if remaining.is_zero() {
            return Ok(IndexProgressUpdate::Unchanged(reference));
        }
        clock.wait(remaining.min(PROBE_INTERVAL), context)?;
    }
}

#[cfg(all(test, any(unix, windows)))]
#[expect(
    clippy::expect_used,
    clippy::panic,
    reason = "typed progress fixture laws"
)]
mod tests {
    use super::*;
    use crate::RequestCancellationReason;
    use std::cell::Cell;
    use std::rc::Rc;

    struct Clock {
        now: Rc<Cell<Duration>>,
        started: Duration,
    }
    impl Clock {
        fn new(now: Rc<Cell<Duration>>) -> Self {
            Self {
                started: now.get(),
                now,
            }
        }
    }
    impl ProgressClock for Clock {
        fn elapsed(&self) -> Duration {
            self.now.get() - self.started
        }
        fn wait(
            &mut self,
            duration: Duration,
            context: &RequestContext,
        ) -> Result<(), ClientError> {
            context.check()?;
            self.now.set(self.now.get() + duration);
            Ok(())
        }
    }

    fn waveform() -> serde_json::Value {
        serde_json::from_str(include_str!("fixtures/go05-progress-waveform.json"))
            .expect("actual retained waveform")
    }
    fn active() -> IndexOperationStatus {
        let reply: SurfaceReply = serde_json::from_value(waveform()["runs"][1]["reply"].clone())
            .expect("actual active status");
        let SurfaceReply::IndexOperationStatus(IndexOperationObservation::Known(status)) = reply
        else {
            panic!("actual known receipt")
        };
        status
    }

    #[test]
    fn durable_wait_observes_one_exact_request_through_later_terminal_failure() {
        let status = active();
        let terminal: SurfaceReply = serde_json::from_value(waveform()["runs"][2]["reply"].clone())
            .expect("retained exact refusal");
        let SurfaceReply::IndexOperationStatus(expected) = &terminal else {
            panic!("terminal operation");
        };
        let now = Rc::new(Cell::new(Duration::ZERO));
        let mut reads = 0;
        let mut progress = Vec::new();
        let result = wait_operation_with_clock(
            IndexOperationObservation::Known(status.clone()),
            &RequestContext::new(),
            |command| {
                assert!(matches!(command, SurfaceCommand::IndexOperationStatus { operation_key } if operation_key == status.operation_key));
                reads += 1;
                Ok(if now.get() >= Duration::from_secs(21) { terminal.clone() } else { keyed_reply(status.clone()) })
            },
            |observation| progress.push(observation.clone()),
            &mut Clock::new(now.clone()),
        ).expect("admitted wait");
        assert!(
            matches!(result, IndexOperationWaitResult::Terminal(actual) if &actual == expected)
        );
        assert_eq!(now.get(), Duration::from_secs(21));
        assert_eq!(
            reads, 43,
            "each later conditional slice retains bounded 500ms pacing"
        );
        assert_eq!(progress.last(), Some(expected));
        assert!(progress.iter().all(|observation| matches!(observation, IndexOperationObservation::Known(actual) if actual.operation_key == status.operation_key && actual.request_digest == status.request_digest)));
    }

    /// Every conditional read of a fast-changing operation answers at once,
    /// and each read spends a frame of the owner's per-connection budget. The
    /// wait therefore starts consecutive reads at least the minimum spacing
    /// apart, and never delays the terminal reply it is waiting for.
    #[test]
    fn a_fast_changing_operation_cannot_burn_frames_faster_than_the_minimum_spacing() {
        let status = active();
        let terminal: SurfaceReply = serde_json::from_value(waveform()["runs"][2]["reply"].clone())
            .expect("retained exact refusal");
        let IndexOperationState::Active { ticket, stage } = status.state.clone() else {
            panic!("active fixture");
        };
        let stages = [
            IndexJobStage::Acquiring,
            IndexJobStage::Staging,
            IndexJobStage::Scanning,
            IndexJobStage::Compiling,
            IndexJobStage::Publishing,
        ];
        let now = Rc::new(Cell::new(Duration::ZERO));
        let started = Rc::new(std::cell::RefCell::new(Vec::<Duration>::new()));
        let mut previous = stage;
        let mut reads = 0usize;
        let result = wait_operation_with_clock(
            IndexOperationObservation::Known(status.clone()),
            &RequestContext::new(),
            |_| {
                started.borrow_mut().push(now.get());
                reads += 1;
                if reads > 8 {
                    return Ok(terminal.clone());
                }
                // A different stage from the previous observation each time.
                let next = stages
                    .into_iter()
                    .find(|candidate| *candidate != previous)
                    .expect("another stage");
                previous = next;
                let mut changed = status.clone();
                changed.state = IndexOperationState::Active {
                    ticket: ticket.clone(),
                    stage: next,
                };
                Ok(keyed_reply(changed))
            },
            |_| {},
            &mut Clock::new(now.clone()),
        )
        .expect("admitted wait");
        assert!(matches!(result, IndexOperationWaitResult::Terminal(_)));
        let started = started.borrow();
        assert_eq!(started.len(), 9);
        for pair in started.windows(2) {
            assert!(
                pair[1] - pair[0] >= MIN_READ_SPACING,
                "reads began {:?} apart",
                pair[1] - pair[0]
            );
        }
        assert_eq!(
            now.get(),
            MIN_READ_SPACING * 8,
            "exactly the spacing, no extra delay once the reply is terminal"
        );
    }

    #[test]
    fn durable_wait_stop_retains_last_admitted_identity_without_mutation() {
        let status = active();
        for error in [
            ClientError::Disconnected(std::io::ErrorKind::ConnectionReset),
            ClientError::RemoteDeadlineExceeded,
        ] {
            let mut error = Some(error);
            let mut reads = 0;
            let result = wait_index_operation(
                IndexOperationObservation::Known(status.clone()),
                &RequestContext::new(),
                |_| {
                    reads += 1;
                    Err(error.take().expect("one exact read"))
                },
                |_| {},
            )
            .expect("initial admission");
            assert!(
                matches!(result, IndexOperationWaitResult::Stopped { observation: IndexOperationObservation::Known(actual), reason: IndexOperationWaitStop::Client(_) } if actual == status)
            );
            assert_eq!(reads, 1);
        }
        for reason in [
            RequestCancellationReason::Explicit,
            RequestCancellationReason::ClientDisconnected,
            RequestCancellationReason::Shutdown,
        ] {
            let context = RequestContext::new();
            context.cancel(reason);
            let result = wait_index_operation(
                IndexOperationObservation::Known(status.clone()),
                &context,
                |_| panic!("cancelled wait cannot read or start"),
                |_| {},
            )
            .expect("initial admission");
            assert!(
                matches!(result, IndexOperationWaitResult::Stopped { observation: IndexOperationObservation::Known(actual), reason: IndexOperationWaitStop::Client(ClientError::RequestCancelled(actual_reason)) } if actual == status && actual_reason == reason)
            );
        }
        let context = RequestContext::new();
        assert!(context.read_deadline(Duration::ZERO).is_err());
        let result = wait_index_operation(
            IndexOperationObservation::Known(status.clone()),
            &context,
            |_| panic!("expired scope cannot renew"),
            |_| {},
        )
        .expect("initial admission");
        assert!(
            matches!(result, IndexOperationWaitResult::Stopped { observation: IndexOperationObservation::Known(actual), reason: IndexOperationWaitStop::Client(ClientError::RemoteDeadlineExceeded) } if actual == status)
        );
    }

    #[test]
    fn durable_wait_refuses_foreign_same_key_request_and_stops_on_uncertain_evidence() {
        let status = active();
        let other =
            backend_library::PackageReference::parse("/work/unrelated").expect("other project");
        let foreign = IndexOperationStatus::new(
            status.operation_key,
            other.clone(),
            status.execution_intent,
            IndexOperationState::Active {
                ticket: IndexJobTicket::new(std::num::NonZeroU64::MIN, [9; 16], other),
                stage: IndexJobStage::Compiling,
            },
        );
        let result = wait_index_operation(
            IndexOperationObservation::Known(status.clone()),
            &RequestContext::new(),
            |_| Ok(keyed_reply(foreign.clone())),
            |_| {},
        )
        .expect("initial admission");
        assert!(
            matches!(result, IndexOperationWaitResult::Stopped { observation: IndexOperationObservation::Known(actual), reason: IndexOperationWaitStop::Client(ClientError::Protocol(_)) } if actual == status)
        );
        let mut unresolved = status.clone();
        unresolved.state = IndexOperationState::Unresolved {
            reason: backend_library::IndexOperationUnresolvedReason::WorkspaceEvidenceMismatch,
            detail: backend_library::ProductText::from_static("exact publication cannot be proven"),
        };
        for uncertain in [
            IndexOperationObservation::Known(unresolved),
            IndexOperationObservation::Unknown {
                operation_key: status.operation_key,
            },
            IndexOperationObservation::OutsideReceiptWindow {
                operation_key: status.operation_key,
                request_digest: status.request_digest,
            },
        ] {
            let mut reads = 0;
            let result = wait_index_operation(
                IndexOperationObservation::Known(status.clone()),
                &RequestContext::new(),
                |_| {
                    reads += 1;
                    Ok(SurfaceReply::IndexOperationStatus(uncertain.clone()))
                },
                |_| {},
            )
            .expect("exact uncertainty");
            assert!(
                matches!(result, IndexOperationWaitResult::Stopped { observation, reason: IndexOperationWaitStop::Uncertain } if observation == uncertain)
            );
            assert_eq!(reads, 1, "uncertainty cannot authorize any implicit start");
        }
    }

    #[test]
    fn durable_wait_accepts_already_admitted_terminal_at_zero_wait_budget() {
        let terminal: SurfaceReply = serde_json::from_value(waveform()["runs"][2]["reply"].clone())
            .expect("actual terminal refusal");
        let SurfaceReply::IndexOperationStatus(observation) = terminal else {
            panic!("operation");
        };
        let context = RequestContext::new();
        assert!(context.read_deadline(Duration::ZERO).is_err());
        let result = wait_index_operation(
            observation.clone(),
            &context,
            |_| panic!("terminal needs no exchange"),
            |_| {},
        )
        .expect("admitted terminal");
        assert!(
            matches!(result, IndexOperationWaitResult::Terminal(actual) if actual == observation)
        );
    }
    fn query(status: &IndexOperationStatus, wait: u64) -> IndexProgressRead {
        IndexProgressRead::new(
            SurfaceCommand::IndexOperationStatus {
                operation_key: status.operation_key,
            },
            IndexObservationFingerprint::active(status).expect("complete commitment"),
            wait,
        )
        .expect("read-only query")
    }
    fn keyed_reply(status: IndexOperationStatus) -> SurfaceReply {
        SurfaceReply::IndexOperationStatus(IndexOperationObservation::Known(status))
    }

    #[test]
    fn conditional_progress_wait_is_bounded_and_returns_only_an_actual_pending_reference() {
        let status = active();
        let query = query(&status, MAX_INDEX_PROGRESS_WAIT_MILLIS);
        let now = Rc::new(Cell::new(Duration::ZERO));
        let mut reads = 0;
        let result = read_with_clock(&query, &RequestContext::new(), |command| {
            assert!(matches!(command, SurfaceCommand::IndexOperationStatus { operation_key } if operation_key == status.operation_key));
            reads += 1; Ok(keyed_reply(status.clone()))
        }, &mut Clock::new(now.clone())).expect("bounded unchanged wait");
        let IndexProgressUpdate::Unchanged(IndexProgressReference::Operation {
            operation_key,
            ticket,
            after_observation,
        }) = result
        else {
            panic!("honest pending reference")
        };
        assert_eq!(operation_key, status.operation_key);
        let IndexOperationState::Active {
            ticket: expected_ticket,
            ..
        } = status.state
        else {
            panic!("active")
        };
        assert_eq!(ticket, expected_ticket);
        assert_eq!(Some(after_observation), query.after_observation);
        assert_eq!(
            now.get(),
            Duration::from_millis(MAX_INDEX_PROGRESS_WAIT_MILLIS)
        );
        assert_eq!(
            reads, 21,
            "one immediate read then at most 20 bounded probes, including final boundary"
        );
    }

    #[test]
    fn owner_replacement_and_any_full_source_change_cannot_compare_unchanged() {
        let status = active();
        let query = query(&status, MAX_INDEX_PROGRESS_WAIT_MILLIS);
        let mut replaced = status.clone();
        let IndexOperationState::Active { ticket, .. } = &mut replaced.state else {
            panic!("active")
        };
        *ticket = IndexJobTicket::new(std::num::NonZeroU64::MIN, [9; 16], status.package.clone());
        let mut changed_source = serde_json::to_value(&status).expect("exact source status");
        changed_source["source_capture"]["workspace_root"] = serde_json::json!("a7".repeat(32));
        let changed_source: IndexOperationStatus =
            serde_json::from_value(changed_source).expect("changed exact source root");
        for changed in [replaced, changed_source] {
            let expected = keyed_reply(changed);
            let now = Rc::new(Cell::new(Duration::ZERO));
            let mut reads = 0;
            let result = read_with_clock(
                &query,
                &RequestContext::new(),
                |_| {
                    reads += 1;
                    Ok(expected.clone())
                },
                &mut Clock::new(now.clone()),
            )
            .expect("changed full observation");
            assert!(matches!(result, IndexProgressUpdate::Changed(reply) if reply == expected));
            assert_eq!(reads, 1);
            assert_eq!(now.get(), Duration::ZERO);
        }
    }

    #[test]
    fn terminal_unknown_unresolved_and_wrong_key_never_become_unchanged() {
        let status = active();
        let query = query(&status, MAX_INDEX_PROGRESS_WAIT_MILLIS);
        let mut unresolved = status.clone();
        unresolved.state = IndexOperationState::Unresolved {
            reason:
                backend_library::IndexOperationUnresolvedReason::SemanticWorkInterruptedAfterCapture,
            detail: backend_library::ProductText::from_static("owner stopped before commit"),
        };
        let terminal: SurfaceReply = serde_json::from_value(waveform()["runs"][2]["reply"].clone())
            .expect("actual terminal refusal");
        for reply in [
            terminal,
            keyed_reply(unresolved),
            SurfaceReply::IndexOperationStatus(IndexOperationObservation::Unknown {
                operation_key: status.operation_key,
            }),
            SurfaceReply::IndexOperationStatus(IndexOperationObservation::OutsideReceiptWindow {
                operation_key: status.operation_key,
                request_digest: status.request_digest,
            }),
        ] {
            let mut reads = 0;
            let result = read_with_clock(
                &query,
                &RequestContext::new(),
                |_| {
                    reads += 1;
                    Ok(reply.clone())
                },
                &mut Clock::new(Rc::new(Cell::new(Duration::ZERO))),
            )
            .expect("complete changed facts");
            assert!(matches!(result, IndexProgressUpdate::Changed(actual) if actual == reply));
            assert_eq!(reads, 1);
        }
        let foreign = SurfaceReply::IndexOperationStatus(IndexOperationObservation::Unknown {
            operation_key: IndexOperationKey::from_bytes([7; 32]).expect("foreign key"),
        });
        assert!(matches!(
            query.unchanged(&foreign),
            Err(ClientError::Protocol(_))
        ));
    }

    #[test]
    fn ticket_sequence_is_a_progress_cursor_and_same_sequence_terminal_is_always_full() {
        let status = active();
        let IndexOperationState::Active { ticket, stage } = status.state else {
            panic!("active")
        };
        let query = IndexProgressRead::new(
            SurfaceCommand::IndexProgress {
                ticket: ticket.clone(),
                after_sequence: 17,
            },
            None,
            10000,
        )
        .expect("ticket cursor");
        let pending = backend_library::IndexProgressPage {
            ticket: ticket.clone(),
            stage,
            events: Box::new([]),
            next_sequence: 17,
            truncated: false,
            has_more: false,
        };
        assert!(
            query
                .unchanged(&SurfaceReply::IndexProgress(IndexJobObservation::Pending(
                    pending.clone()
                )))
                .expect("pending")
                .is_some()
        );
        let terminal = SurfaceReply::IndexProgress(IndexJobObservation::Terminal(
            backend_library::IndexJobTerminal {
                ticket: ticket.clone(),
                outcome: backend_library::IndexJobOutcome::Cancelled,
            },
        ));
        assert!(
            query
                .unchanged(&terminal)
                .expect("terminal with no new progress event")
                .is_none()
        );
        let unknown = SurfaceReply::IndexProgress(IndexJobObservation::Unknown {
            ticket,
            current_owner_epoch: [4; 16],
        });
        assert!(query.unchanged(&unknown).expect("new owner").is_none());
        let mut truncated = pending;
        truncated.truncated = true;
        assert!(
            query
                .unchanged(&SurfaceReply::IndexProgress(IndexJobObservation::Pending(
                    truncated
                )))
                .expect("retention gap")
                .is_none()
        );
    }

    #[test]
    fn cancellation_and_original_deadline_take_precedence_without_mutation_or_reset() {
        let status = active();
        let query = query(&status, 10000);
        for reason in [
            RequestCancellationReason::Explicit,
            RequestCancellationReason::ClientDisconnected,
            RequestCancellationReason::Shutdown,
        ] {
            let context = RequestContext::new();
            let cancel = context.clone();
            let mut reads = 0;
            let result = read_with_clock(
                &query,
                &context,
                |_| {
                    reads += 1;
                    cancel.cancel(reason);
                    Err(ClientError::Disconnected(std::io::ErrorKind::BrokenPipe))
                },
                &mut Clock::new(Rc::new(Cell::new(Duration::ZERO))),
            );
            assert!(
                matches!(result, Err(ClientError::RequestCancelled(actual)) if actual == reason)
            );
            assert_eq!(reads, 1);
        }
        let context = RequestContext::new();
        assert!(context.read_deadline(Duration::ZERO).is_err());
        let mut reads = 0;
        let result = read_index_progress(&query, &context, |_| {
            reads += 1;
            Ok(keyed_reply(status.clone()))
        });
        assert!(matches!(result, Err(ClientError::RemoteDeadlineExceeded)));
        assert_eq!(reads, 0);
    }

    #[test]
    fn missing_reference_is_an_immediate_complete_read_and_invalid_options_never_dispatch() {
        let status = active();
        let command = SurfaceCommand::IndexOperationStatus {
            operation_key: status.operation_key,
        };
        let query = IndexProgressRead::new(command.clone(), None, 0)
            .expect("lost caller state can request full snapshot");
        let reply = keyed_reply(status.clone());
        let result = read_index_progress(&query, &RequestContext::new(), |_| Ok(reply.clone()))
            .expect("full read");
        assert!(matches!(result, IndexProgressUpdate::Changed(actual) if actual == reply));
        assert!(IndexProgressRead::new(command.clone(), None, 1).is_err());
        assert!(
            IndexProgressRead::new(
                command,
                IndexObservationFingerprint::active(&status).expect("fingerprint"),
                10001
            )
            .is_err()
        );
        assert!(
            IndexProgressRead::new(
                SurfaceCommand::IndexStart {
                    package: status.package,
                    execution_intent: status.execution_intent
                },
                None,
                0
            )
            .is_err()
        );
        for text in ["00".repeat(31), "AA".repeat(32), "gg".repeat(32)] {
            assert!(IndexObservationFingerprint::try_from(text).is_err());
        }
    }

    #[test]
    fn actual_go05_waveform_replay_preserves_full_refusal_with_fewer_caller_observations() {
        let waveform = waveform();
        assert_eq!(waveform["poll_count"], 800);
        let runs = waveform["runs"]
            .as_array()
            .expect("three actual complete states")
            .iter()
            .map(|run| {
                (
                    Duration::from_millis(run["at_millis"].as_u64().expect("actual phase time")),
                    serde_json::from_value::<SurfaceReply>(run["reply"].clone())
                        .expect("actual typed receipt"),
                )
            })
            .collect::<Vec<_>>();
        assert_eq!(runs.len(), 3);
        let status = match &runs[0].1 {
            SurfaceReply::IndexOperationStatus(IndexOperationObservation::Known(status)) => status,
            _ => panic!("actual active start"),
        };
        let command = SurfaceCommand::IndexOperationStatus {
            operation_key: status.operation_key,
        };
        let now = Rc::new(Cell::new(Duration::ZERO));
        let mut after = None;
        let mut caller_observations = 0;
        let mut complete = 0;
        loop {
            caller_observations += 1;
            assert!(
                caller_observations <= 50,
                "no empty full-response busy loop"
            );
            let query = IndexProgressRead::new(
                command.clone(),
                after,
                if after.is_some() { 10000 } else { 0 },
            )
            .expect("verbatim bounded next observation");
            let update = read_with_clock(
                &query,
                &RequestContext::new(),
                |_| {
                    Ok(runs
                        .iter()
                        .rev()
                        .find(|(at, _)| *at <= now.get())
                        .expect("actual phase")
                        .1
                        .clone())
                },
                &mut Clock::new(now.clone()),
            )
            .expect("waveform read");
            if let IndexProgressUpdate::Changed(reply) = update {
                complete += 1;
                let SurfaceReply::IndexOperationStatus(IndexOperationObservation::Known(status)) =
                    &reply
                else {
                    panic!("complete receipt")
                };
                if matches!(status.state, IndexOperationState::Failed { .. }) {
                    assert_eq!(
                        reply, runs[2].1,
                        "actual compiler failure/source/key/coverage retained exactly"
                    );
                    break;
                }
                after = IndexObservationFingerprint::active(status)
                    .expect("entire changed active receipt");
            }
        }
        assert_eq!(complete, 3);
        assert!(caller_observations < 50);
        assert!(now.get() >= runs[2].0 && now.get() < runs[2].0 + PROBE_INTERVAL);
        // Deterministic replay proves interaction count, not current installed
        // runtime speed, actual cl100k counts, or an inference capability.
    }
}
