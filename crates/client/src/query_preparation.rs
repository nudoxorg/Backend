//! Wait for preparation while preserving the original read and its deadline.
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
    /// Requested immutable view for basis-bearing reads; observed current local
    /// view for IndexSearch readiness, granting no result or cursor authority.
    pub basis: ViewRevision,
    /// Whether this selection is preparing or superseded work is retiring.
    pub state: QueryPreparationState,
    /// Time spent exchanging the pinned query after revision selection.
    pub elapsed: Duration,
    /// Remaining read budget; revision selection has already consumed its share.
    pub remaining: Duration,
}

impl Session {
    /// IndexSearch has no requested basis: readiness only observes the current
    /// local plane. Retain the original opaque cursor and let the owner admit
    /// or refuse it against the final complete composite search snapshot.
    pub(super) fn send_index_search_ready(
        &mut self,
        command: SurfaceCommand,
    ) -> Result<ReplyDto, ClientError> {
        if !matches!(&command, SurfaceCommand::IndexSearch { .. }) {
            return Err(ClientError::Protocol(
                "index-search wait requires IndexSearch".to_owned(),
            ));
        }
        let original = Command::Surface(command);
        let deadline =
            QueryReadDeadline::new(CLIENT_REQUEST_TIMEOUT, self.request_context.as_ref())?;
        let interrupt = self.interrupt_handle();
        let started = Instant::now();
        let mut last_progress = None;
        let mut pacing = Backoff::new(POLL_FIRST, POLL_CEILING);
        loop {
            self.check_request_context()?;
            if interrupt
                .as_ref()
                .is_some_and(|handle| handle.0.interrupted.load(Ordering::Acquire))
            {
                return Err(cancelled());
            }
            let remaining = deadline.remaining()?;
            match self.send_success_with_budget(original.clone(), None, Some(remaining)) {
                // Strict transport admission already established this failure
                // belongs to this exact command. No result or cursor is rebased.
                Err(ClientError::CommandFailed(CommandFailure::QueryPreparation {
                    basis,
                    state:
                        state @ (QueryPreparationState::Preparing | QueryPreparationState::Retiring),
                })) => {
                    self.check_request_context()?;
                    let elapsed = started.elapsed();
                    let remaining = deadline.remaining()?;
                    if last_progress.is_none_or(
                        |(previous_basis, previous_state, at): (
                            ViewRevision,
                            QueryPreparationState,
                            Duration,
                        )| {
                            previous_basis != basis
                                || previous_state != state
                                || elapsed.saturating_sub(at) >= Duration::from_secs(1)
                        },
                    ) {
                        let progress = QueryPreparationProgress {
                            basis,
                            state,
                            elapsed,
                            remaining,
                        };
                        if let Some(context) = &self.request_context {
                            context.preparation_progress(progress);
                        }
                        if let Some(observer) = &mut self.preparation_progress
                            && !observer(progress)
                        {
                            return Err(cancelled());
                        }
                        last_progress = Some((basis, state, elapsed));
                    }
                    self.check_request_context()?;
                    let pause = pacing.step().min(deadline.remaining()?);
                    self.pause_interruptibly(pause, interrupt.as_ref())?;
                }
                result => return result,
            }
        }
    }

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
                    // The PC5 negotiation was bound to the retired stream;
                    // the reopened peer must use strict full-proof PC4 until
                    // a new Session explicitly probes it.
                    self.outline_owner_witness_route = false;
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

#[cfg(test)]
mod index_search_tests {
    use super::*;
    use std::collections::VecDeque;
    use std::sync::{Arc, Mutex, mpsc};

    enum Step {
        Reply(CommandReply),
        FatalTransport,
    }
    type Frames = Arc<Mutex<Vec<(Command, Duration)>>>;
    struct Scripted {
        steps: VecDeque<Step>,
        frames: Frames,
        entered: Option<mpsc::SyncSender<()>>,
    }
    impl CommandTransport for Scripted {
        fn request(&mut self, request: CommandDto) -> Result<ReplyDto, ClientError> {
            self.request_with_budget(request, CLIENT_REQUEST_TIMEOUT)
        }
        fn request_with_budget(
            &mut self,
            request: CommandDto,
            remaining: Duration,
        ) -> Result<ReplyDto, ClientError> {
            self.frames
                .lock()
                .expect("frame log")
                .push((request.command.clone(), remaining));
            if let Some(entered) = self.entered.take() {
                let _ = entered.send(());
            }
            let reply = match self
                .steps
                .pop_front()
                .unwrap_or_else(|| Step::Reply(preparing(QueryPreparationState::Preparing)))
            {
                Step::Reply(reply) => ReplyDto::new(request.request_id, reply),
                Step::FatalTransport => {
                    return Err(ClientError::Protocol("fatal transport fixture".to_owned()));
                }
            };
            // The fixture observes the same shared strict command/reply admission.
            admit_reply(&request, reply)
        }
    }
    fn preparing(state: QueryPreparationState) -> CommandReply {
        CommandReply::Failed(CommandFailure::QueryPreparation {
            basis: ViewRevision::from(backend_library::view_state_root(&[])),
            state,
        })
    }
    fn query(cursor: Option<&str>) -> SurfaceCommand {
        SurfaceCommand::IndexSearch {
            query: backend_library::ProductText::from_static("needle"),
            limit: 3,
            cursor: cursor.map(|value| {
                backend_library::IndexSearchCursor::new(value)
                    .expect("bounded opaque original cursor")
            }),
        }
    }
    fn exact_page() -> CommandReply {
        CommandReply::Surface(SurfaceReply::IndexSearchPage(
            backend_library::IndexSearchPage {
                snapshot: [73; 32],
                evaluated_at_millis: 1,
                hits: vec![backend_library::RegistrySearchHit::LocalDeclaration(
                    backend_library::LocalDeclarationSearchRecord {
                        coordinate: backend_library::ProductText::from_static(
                            "/captured/pkg::src/x.ts:7::needle",
                        ),
                        name: backend_library::ProductText::from_static("needle"),
                        source: backend_library::LocalDeclarationSource::Captured {
                            path: backend_library::ProductText::from_static("src/x.ts"),
                            line: std::num::NonZeroU32::new(7).expect("line"),
                        },
                    },
                )]
                .into_boxed_slice(),
                next_cursor: None,
                result_count: backend_library::IndexSearchResultCount::Exact(1),
            },
        ))
    }
    fn scripted_session(steps: Vec<Step>) -> (Session, Frames) {
        let frames = Arc::new(Mutex::new(Vec::new()));
        let session = Session::from_transport(
            "scripted-index-search",
            Scripted {
                steps: steps.into(),
                frames: Arc::clone(&frames),
                entered: None,
            },
        );
        (session, frames)
    }

    #[test]
    fn index_search_preparation_retries_exact_original_under_one_deadline_to_complete_result() {
        let (mut session, frames) = scripted_session(vec![
            Step::Reply(preparing(QueryPreparationState::Preparing)),
            Step::Reply(preparing(QueryPreparationState::Retiring)),
            Step::Reply(exact_page()),
        ]);
        let context = RequestContext::new();
        context
            .read_deadline(Duration::from_secs(2))
            .expect("one existing absolute deadline");
        session
            .set_request_context(Some(context))
            .expect("admitted scope");
        let original = query(Some("original_cursor_bytes"));
        let reply = session
            .surface(original.clone())
            .expect("complete final result");
        let SurfaceReply::IndexSearchPage(page) = reply else {
            panic!("exact index page");
        };
        assert_eq!(
            page.hits.len(),
            1,
            "readiness never becomes an empty successful page"
        );
        assert_eq!(page.snapshot, [73; 32]);
        let frames = frames.lock().expect("frames");
        assert_eq!(frames.len(), 3);
        for (command, budget) in frames.iter() {
            assert_eq!(command, &Command::Surface(original.clone()));
            assert!(*budget <= Duration::from_secs(2));
        }
        assert!(
            frames.windows(2).all(|pair| pair[1].1 < pair[0].1),
            "polls spend the original budget instead of renewing it"
        );
    }

    #[test]
    fn index_search_preparation_deadline_is_not_renewed_or_returned_as_empty_success() {
        let (mut session, frames) = scripted_session(Vec::new());
        let context = RequestContext::new();
        context
            .read_deadline(Duration::from_millis(80))
            .expect("explicit original scope");
        session
            .set_request_context(Some(context))
            .expect("admitted scope");
        assert!(matches!(
            session.surface(query(None)),
            Err(ClientError::RemoteDeadlineExceeded)
        ));
        let frames = frames.lock().expect("frames");
        assert_eq!(
            frames.len(),
            1,
            "paced pending read cannot spin before its original deadline"
        );
        assert!(frames[0].1 <= Duration::from_millis(80));
    }

    #[test]
    fn index_search_preparation_cancellation_interrupts_pacing_without_another_send() {
        let frames = Arc::new(Mutex::new(Vec::new()));
        let (entered, observed) = mpsc::sync_channel(1);
        let context = RequestContext::new();
        context
            .read_deadline(Duration::from_secs(2))
            .expect("bounded original scope");
        let mut session = Session::from_transport(
            "cancelled-index-search",
            Scripted {
                steps: VecDeque::new(),
                frames: Arc::clone(&frames),
                entered: Some(entered),
            },
        );
        session
            .set_request_context(Some(context.clone()))
            .expect("scope");
        let waiter = std::thread::spawn(move || session.surface(query(None)));
        observed
            .recv_timeout(Duration::from_secs(1))
            .expect("first exact preparation observation");
        std::thread::sleep(Duration::from_millis(25)); // Inside the first100ms pace.
        context.cancel(RequestCancellationReason::Explicit);
        assert!(matches!(
            waiter.join().expect("cancelled waiter"),
            Err(ClientError::RequestCancelled(
                RequestCancellationReason::Explicit
            ))
        ));
        assert_eq!(
            frames.lock().expect("frames").len(),
            1,
            "cancelled pacing cannot send another owner request"
        );
    }

    #[test]
    fn index_search_preparation_propagates_fatal_and_stale_cursor_refusals_without_rebase() {
        for (fatal, kind) in [
            (Step::FatalTransport, 0),
            (
                Step::Reply(CommandReply::Failed(CommandFailure::InvalidQuery(
                    "invalid original cursor".to_owned(),
                ))),
                1,
            ),
            (
                Step::Reply(CommandReply::Failed(CommandFailure::CursorMismatch)),
                2,
            ),
        ] {
            let (mut session, frames) = scripted_session(vec![fatal]);
            let result = session.surface(query(Some("stale_original_bytes")));
            assert!(
                match (kind, result) {
                    (0, Err(ClientError::Protocol(detail))) => detail == "fatal transport fixture",
                    (1, Err(ClientError::CommandFailed(CommandFailure::InvalidQuery(detail)))) =>
                        detail == "invalid original cursor",
                    (2, Err(ClientError::CommandFailed(CommandFailure::CursorMismatch))) => true,
                    _ => false,
                },
                "fatal class and detail must propagate unchanged"
            );
            assert_eq!(
                frames.lock().expect("frames").len(),
                1,
                "only typed preparation is retryable"
            );
        }
        let original = query(Some("stale_original_bytes"));
        let (mut session, frames) = scripted_session(vec![
            Step::Reply(preparing(QueryPreparationState::Retiring)),
            Step::Reply(CommandReply::Failed(CommandFailure::InvalidQuery(
                "cursor does not match selected snapshot".to_owned(),
            ))),
        ]);
        assert!(matches!(session.surface(original.clone()),
            Err(ClientError::CommandFailed(CommandFailure::InvalidQuery(detail)))
                if detail == "cursor does not match selected snapshot"));
        let frames = frames.lock().expect("frames");
        assert_eq!(frames.len(), 2);
        assert!(
            frames
                .iter()
                .all(|(command, _)| command == &Command::Surface(original.clone())),
            "preparation cannot clear or repair an issued opaque cursor"
        );
        drop(frames);
        let (mut session, frames) = scripted_session(vec![Step::Reply(preparing(
            QueryPreparationState::Preparing,
        ))]);
        assert!(matches!(
            session.surface(SurfaceCommand::Projects),
            Err(ClientError::Protocol(_))
        ));
        assert_eq!(
            frames.lock().expect("frames").len(),
            1,
            "an unrelated surface command never enters the new wait loop"
        );
    }
    #[test]
    fn index_search_preparation_observers_report_state_and_cancel_only_the_read() {
        let (mut session, frames) = scripted_session(vec![
            Step::Reply(preparing(QueryPreparationState::Preparing)),
            Step::Reply(preparing(QueryPreparationState::Preparing)),
            Step::Reply(preparing(QueryPreparationState::Retiring)),
            Step::Reply(exact_page()),
        ]);
        let context = RequestContext::new();
        context
            .read_deadline(Duration::from_secs(2))
            .expect("one original observer scope");
        let (sender, receiver) = mpsc::sync_channel(4);
        context.set_preparation_observer(sender);
        session
            .set_request_context(Some(context))
            .expect("context observer");
        let observations = Arc::new(Mutex::new(Vec::new()));
        let observed = Arc::clone(&observations);
        session.observe_query_preparation(move |progress| {
            observed
                .lock()
                .expect("session observations")
                .push(progress);
            true
        });
        let result = session
            .surface(query(None))
            .expect("complete observed result");
        assert!(matches!(result, SurfaceReply::IndexSearchPage(page) if page.hits.len() == 1));
        let context_events = receiver.try_iter().collect::<Vec<_>>();
        let session_events = observations.lock().expect("observations");
        assert_eq!(
            context_events.as_slice(),
            session_events.as_slice(),
            "MCP context and CLI session observers see the same bounded observation"
        );
        assert_eq!(
            context_events.first().expect("initial state").state,
            QueryPreparationState::Preparing
        );
        assert_eq!(
            context_events.last().expect("changed state").state,
            QueryPreparationState::Retiring
        );
        assert!(context_events.len() >= 2 && context_events.len() <= 3);
        assert!(
            context_events
                .iter()
                .all(|event| event.basis == ViewRevision::from(backend_library::view_state_root(&[]))
                    && event.remaining <= Duration::from_secs(2))
        );
        for pair in context_events.windows(2) {
            assert!(pair[1].elapsed >= pair[0].elapsed && pair[1].remaining <= pair[0].remaining);
            if pair[1].state == pair[0].state && pair[1].basis == pair[0].basis {
                assert!(
                    pair[1].elapsed.saturating_sub(pair[0].elapsed) >= Duration::from_secs(1),
                    "unchanged state is throttled rather than emitted on every poll"
                );
            }
        }
        assert_eq!(frames.lock().expect("frames").len(), 4);
        drop(session_events);

        let (mut session, frames) = scripted_session(vec![
            Step::Reply(preparing(QueryPreparationState::Preparing)),
            Step::Reply(exact_page()),
        ]);
        let context = RequestContext::new();
        context
            .read_deadline(Duration::from_secs(2))
            .expect("retained caller scope");
        session
            .set_request_context(Some(context.clone()))
            .expect("scope");
        session.observe_query_preparation(|_| false);
        let original = query(Some("observer_original_cursor"));
        assert!(matches!(session.surface(original.clone()),
            Err(ClientError::CommandFailed(CommandFailure::InvalidQuery(detail)))
                if detail == "query preparation wait cancelled"));
        assert_eq!(frames.lock().expect("frames").len(), 1);
        context
            .check()
            .expect("observer cancellation must not cancel the caller's scope");
        session.observe_query_preparation(|_| true);
        assert!(matches!(
            session.surface(original.clone()),
            Ok(SurfaceReply::IndexSearchPage(_))
        ));
        let frames = frames.lock().expect("frames");
        assert_eq!(frames.len(), 2);
        assert!(
            frames
                .iter()
                .all(|(command, _)| command == &Command::Surface(original.clone()))
        );
        assert!(
            frames[1].1 < frames[0].1,
            "resumed read keeps the caller's original deadline"
        );
    }
}
