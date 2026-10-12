//! Wait only for preparation of the exact immutable read already submitted.
//! The worker belongs to the selected owner, not to one waiting connection.

use super::*;
use backend_library::{QueryPreparationState, ViewRevision};
use std::time::Instant;

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
        let interrupt = self.interrupt_handle();
        let mut last_progress = None;
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
            let reply = self.request_result(result).map_err(|error| match error {
                ClientError::Disconnected(kind) => {
                    ClientError::QueryPreparationDisconnected { basis, kind }
                }
                error => error,
            })?;
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
            let pause = Duration::from_millis(100).min(remaining_budget()?);
            let wake = Instant::now() + pause;
            while Instant::now() < wake {
                self.check_request_context()?;
                if interrupt
                    .as_ref()
                    .is_some_and(|handle| handle.0.interrupted.load(Ordering::Acquire))
                {
                    return Err(cancelled());
                }
                std::thread::sleep(
                    Duration::from_millis(10).min(wake.saturating_duration_since(Instant::now())),
                );
            }
            if remaining_budget()?.is_zero() {
                return Ok(reply);
            }
        }
    }
}

fn cancelled() -> ClientError {
    ClientError::CommandFailed(CommandFailure::InvalidQuery(
        "query preparation wait cancelled".to_owned(),
    ))
}
