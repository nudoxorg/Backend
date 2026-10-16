//! Wait only for preparation of the exact immutable read already submitted.
//! The worker belongs to the selected owner, not to one waiting connection.

use super::*;
use backend_library::{QueryPreparationState, ViewRevision};
use std::time::Instant;

/// First wait between polls of one preparing read. The wait grows to
/// [`POLL_CEILING`] so that a long preparation spends a handful of frames,
/// not the owner's whole per-connection budget (256 polls at 100 ms is 27 s).
const POLL_FIRST: Duration = Duration::from_millis(100);
/// Longest wait between polls of one preparing read.
const POLL_CEILING: Duration = Duration::from_millis(500);
/// First wait before dialing again when a replacement connection is lost too.
const REOPEN_FIRST: Duration = Duration::from_millis(25);
/// Longest wait between attempts to reopen the connection.
const REOPEN_CEILING: Duration = Duration::from_millis(500);

/// One deadline spans revision selection and every exchange of this read.
pub(super) struct QueryReadDeadline {
    deadline: Instant,
}
impl QueryReadDeadline {
    pub(super) fn new(
        budget: Duration,
        context: Option<&RequestContext>,
    ) -> Result<Self, ClientError> {
        let deadline = match context {
            Some(context) => context.read_deadline(budget)?.min(Instant::now() + budget),
            None => Instant::now() + budget,
        };
        Ok(Self { deadline })
    }
    pub(super) fn remaining(&self) -> Result<Duration, ClientError> {
        let remaining = self.deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            Err(ClientError::RemoteDeadlineExceeded)
        } else {
            Ok(remaining)
        }
    }
}

/// An admitted preparation observation for the original pinned read.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct QueryPreparationProgress {
    /// The immutable view requested by this read.
    pub basis: ViewRevision,
    /// Whether this selection is preparing or superseded work is retiring.
    pub state: QueryPreparationState,
    /// Time spent exchanging the pinned query after revision selection.
    pub elapsed: Duration,
    /// Remaining read budget; revision selection has already consumed its share.
    pub remaining: Duration,
}

impl Session {
    /// Observes preparation without changing machine-readable result output.
    /// Returning false cancels only this read wait, never the shared worker.
    /// This observer runs on the caller and must not block; arbitrary callback
    /// code is not preempted. MCP's production observer uses a bounded try-send.
    pub fn observe_query_preparation(
        &mut self,
        observer: impl FnMut(QueryPreparationProgress) -> bool + Send + 'static,
    ) {
        self.preparation_progress = Some(Box::new(observer));
    }

    pub(super) fn send_query_ready(
        &mut self,
        command: Command,
        certificate: Option<WireCertificate>,
    ) -> Result<ReplyDto, ClientError> {
        if matches!(&command, Command::Search(_) | Command::GraphQuery(_)) {
            self.send_query_ready_with_budget(command, certificate, CLIENT_REQUEST_TIMEOUT)
        } else {
            self.send(command, certificate)
        }
    }

    pub(super) fn send_query_ready_with_budget(
        &mut self,
        command: Command,
        certificate: Option<WireCertificate>,
        budget: Duration,
    ) -> Result<ReplyDto, ClientError> {
        let basis = match &command {
            Command::Search(query) => query.basis(),
            Command::GraphQuery(query)
                if query.control() != backend_library::GraphQueryControl::Cancel =>
            {
                query.page().basis()
            }
            // Mutations, controls and other reads are submitted exactly once.
            _ => return self.send_with_budget(command, certificate, budget),
        };
        let started = Instant::now();
        let search_scope = matches!(&command, Command::Search(_))
            .then(|| self.search_wait_scope.clone())
            .flatten();
        let remaining_budget = || -> Result<Duration, ClientError> {
            let remaining = search_scope
                .as_ref()
                .map(RequestContext::remaining_read_budget)
                .transpose()?
                .flatten()
                .unwrap_or_else(|| budget.saturating_sub(started.elapsed()));
            Ok(remaining)
        };
        let mut interrupt = self.interrupt_handle();
        let mut last_progress = None;
        let mut poll_pacing = Backoff::new(POLL_FIRST, POLL_CEILING);
        loop {
            self.check_request_context()?;
            if interrupt
                .as_ref()
                .is_some_and(|handle| handle.0.interrupted.load(Ordering::Acquire))
            {
                return Err(cancelled());
            }
            let remaining = remaining_budget()?;
            if remaining.is_zero() {
                return Err(ClientError::RemoteDeadlineExceeded);
            }
            // The owner closes a connection once it has served its frame
            // budget, whatever the next frame is. Polling is what spends it,
            // so a spent connection is replaced before the poll is written,
            // not after the owner has hung up on it. A failed attempt here
            // changes nothing: the send below reports the real condition.
            if self.connection_frames_remaining() == Some(0)
                && matches!(self.reopen_pinned_connection(&remaining_budget), Ok(true))
            {
                interrupt = self.interrupt_handle();
            }
            let request_id = self.next_request_id;
            self.next_request_id = request_id
                .checked_add(1)
                .ok_or_else(|| ClientError::Protocol("request identity exhausted".to_owned()))?;
            let mut request = CommandDto::new(request_id, command.clone());
            if let Some(certificate) = &certificate {
                request = request.with_certificate(certificate.clone());
            }
            let remaining = remaining_budget()?;
            if remaining.is_zero() {
                return Err(ClientError::RemoteDeadlineExceeded);
            }
            let remaining = self
                .request_context
                .as_ref()
                .map(RequestContext::remaining_read_budget)
                .transpose()?
                .flatten()
                .map_or(remaining, |budget| remaining.min(budget));
            // No revision refresh or reconnect: query, credit, manifest,
            // cursor, source and certificate stay exactly as originally bound.
            let result = match &search_scope {
                Some(scope) => self
                    .transport
                    .request_with_search_admission(request, remaining, scope),
                None => self.transport.request_with_budget(request, remaining),
            };
            let reply = match self.request_result(result) {
                Ok(reply) => reply,
                Err(error @ ClientError::Disconnected(kind)) => {
                    // The pinned request is idempotent and carries its own
                    // basis, so it is simply asked again on a fresh connection
                    // to the *existing* listener; the revision is never
                    // refreshed and no owner is started. Only a peer that
                    // closed or stopped answering qualifies, and only while the
                    // read's own deadline has budget left.
                    // An exchange that ended because the read's own budget did is
                    // reported as that loss, as it always was, not retried.
                    let reopen = error.is_owner_gap()
                        && remaining_budget().is_ok_and(|remaining| !remaining.is_zero());
                    if reopen && self.reopen_pinned_connection(&remaining_budget)? {
                        interrupt = self.interrupt_handle();
                        continue;
                    }
                    return Err(ClientError::QueryPreparationDisconnected { basis, kind });
                }
                Err(error) => return Err(error),
            };
            let CommandReply::Failed(CommandFailure::QueryPreparation {
                basis: observed,
                state,
            }) = &reply.reply
            else {
                return Ok(reply);
            };
            if *observed != basis {
                return Err(ClientError::Protocol(
                    "query preparation changed the requested basis".to_owned(),
                ));
            }
            let elapsed = started.elapsed();
            let remaining = remaining_budget()?;
            // Preserve the actual typed state when preparation outlives the
            // wait. A deadline is not an empty successful search.
            if remaining.is_zero() {
                return Ok(reply);
            }
            if last_progress.is_none_or(|(previous, at): (QueryPreparationState, Duration)| {
                previous != *state || elapsed.saturating_sub(at) >= Duration::from_secs(1)
            }) {
                if let Some(context) = &self.request_context {
                    context.preparation_progress(QueryPreparationProgress {
                        basis,
                        state: *state,
                        elapsed,
                        remaining,
                    });
                }
                if let Some(observer) = &mut self.preparation_progress
                    && !observer(QueryPreparationProgress {
                        basis,
                        state: *state,
                        elapsed,
                        remaining,
                    })
                {
                    return Err(cancelled());
                }
                last_progress = Some((*state, elapsed));
            }
            if remaining_budget()?.is_zero() {
                return Ok(reply);
            }
            // Short interruptible intervals leave cancellation responsive and
            // avoid turning concurrent waiting callers into a hot poll loop.
            let pause = poll_pacing.step().min(remaining_budget()?);
            self.pause_interruptibly(pause, interrupt.as_ref())?;
            if remaining_budget()?.is_zero() {
                return Ok(reply);
            }
        }
    }
}

impl Session {
    /// Sleeps `pause`, waking every few milliseconds so cancellation and
    /// caller interruption stay responsive.
    fn pause_interruptibly(
        &self,
        pause: Duration,
        interrupt: Option<&TransportInterrupt>,
    ) -> Result<(), ClientError> {
        let wake = Instant::now() + pause;
        while Instant::now() < wake {
            self.check_request_context()?;
            if interrupt.is_some_and(|handle| handle.0.interrupted.load(Ordering::Acquire)) {
                return Err(cancelled());
            }
            std::thread::sleep(
                Duration::from_millis(10).min(wake.saturating_duration_since(Instant::now())),
            );
        }
        Ok(())
    }

    /// Replaces a spent or lost connection so the pinned request can be asked
    /// again unchanged.
    ///
    /// Returns `Ok(false)` when this transport cannot reopen (the caller keeps
    /// the original loss), or when no owner is listening at all: with nobody
    /// to ask there is nothing to re-ask, and starting an owner belongs to the
    /// caller, which also chooses a fresh revision. A replacement that is
    /// itself lost, or a slow listener, is waited out with a growing pause
    /// until the read's deadline.
    fn reopen_pinned_connection(
        &mut self,
        remaining_budget: &dyn Fn() -> Result<Duration, ClientError>,
    ) -> Result<bool, ClientError> {
        let mut pacing = Backoff::new(REOPEN_FIRST, REOPEN_CEILING);
        loop {
            self.check_request_context()?;
            let remaining = remaining_budget()?;
            if remaining.is_zero() {
                return Err(ClientError::RemoteDeadlineExceeded);
            }
            let context = self.request_context.clone();
            match self.transport.reconnect_within(remaining, context.as_ref()) {
                Ok(true) => {
                    // Request numbering belongs to the connection.
                    self.next_request_id = 1;
                    return Ok(true);
                }
                Ok(false) => return Ok(false),
                Err(error) if error.is_owner_absent() => return Ok(false),
                Err(error) if error.is_owner_gap() => {
                    let interrupt = self.interrupt_handle();
                    self.pause_interruptibly(
                        pacing.step().min(remaining_budget()?),
                        interrupt.as_ref(),
                    )?;
                }
                Err(error) => return Err(error),
            }
        }
    }
}

fn cancelled() -> ClientError {
    ClientError::CommandFailed(CommandFailure::InvalidQuery(
        "query preparation wait cancelled".to_owned(),
    ))
}
