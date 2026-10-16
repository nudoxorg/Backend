//! Authenticated read recovery; durable starts reconcile only their saved key.

use backend_client::{
    Backoff, ClientError, IndexJournalDispatch, JournaledIndexStart, RequestContext, Session,
};
use backend_library::{SurfaceCommand, SurfaceReply};
use backend_runtime::WorkspacePaths;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// First pause after the owner connection is lost; doubles up to the ceiling.
const BACKOFF_FIRST: Duration = Duration::from_millis(25);
/// Ceiling for dialing a replacement: a dying owner closes within a moment.
const REPLACEMENT_BACKOFF_CEILING: Duration = Duration::from_millis(500);
/// Ceiling while re-observing a saved key: a restarting owner may take longer.
const REOBSERVE_BACKOFF_CEILING: Duration = Duration::from_secs(1);
/// Cancellation and deadline are observed at least this often during a pause.
const PAUSE_SLICE: Duration = Duration::from_millis(25);

#[derive(Clone, Copy)]
pub(crate) enum RecoverableRead {
    Health,
    UnselectedSearch,
}

pub(crate) struct OwnerRecovery {
    used: bool,
    scope: RequestContext,
    paths: WorkspacePaths,
    progress: Option<std::sync::mpsc::SyncSender<String>>,
    connect: Box<dyn FnMut(RequestContext) -> Result<Session, ClientError>>,
}

impl OwnerRecovery {
    pub(crate) fn new(paths: &WorkspacePaths, passive: bool, scope: RequestContext) -> Self {
        let paths = paths.clone();
        let progress = crate::index_wait::progress_sink();
        let reporter = progress.clone();
        Self {
            used: false,
            scope,
            paths: paths.clone(),
            progress,
            connect: Box::new(move |context| {
                connect_replacement(&paths, passive, context, reporter.clone())
            }),
        }
    }

    #[cfg(test)]
    pub(crate) fn from_connector(
        paths: WorkspacePaths,
        scope: RequestContext,
        connect: impl FnMut(RequestContext) -> Result<Session, ClientError> + 'static,
    ) -> Self {
        Self {
            used: false,
            scope,
            paths,
            progress: None,
            connect: Box::new(connect),
        }
    }

    /// Initial dial/authentication shares the original startup deadline.
    /// No command has been sent, so peer loss here permits one fresh dial.
    pub(crate) fn connect_initial(
        &mut self,
        endpoint: &std::path::Path,
    ) -> Result<Session, ClientError> {
        self.scope
            .read_deadline(backend_runtime::COMMAND_STARTUP_TIMEOUT)?;
        let mut session = match Session::connect_with_context(endpoint, self.scope.clone()) {
            Ok(session) => session,
            Err(error) if error.is_connection_lost() => {
                self.used = true;
                (self.connect)(self.scope.clone())?
            }
            Err(error) => return Err(error),
        };
        // Startup scope does not impose a separate deadline on a mutation.
        session.set_request_context(None)?;
        self.scope.finish();
        self.observe_preparation(&mut session);
        Ok(session)
    }

    pub(crate) fn read<T>(
        &mut self,
        session: &mut Session,
        kind: RecoverableRead,
        mut read: impl FnMut(&mut Session) -> Result<T, ClientError>,
    ) -> Result<T, ClientError> {
        let context = RequestContext::new();
        match kind {
            RecoverableRead::UnselectedSearch => {
                context.begin_default_search(backend_client::CLIENT_REQUEST_TIMEOUT)?
            }
            RecoverableRead::Health => {
                context.read_deadline(backend_client::CLIENT_REQUEST_TIMEOUT)?;
            }
        }
        session.set_request_context(Some(context.clone()))?;
        let result = match read(session) {
            Err(error) if error.is_connection_lost() => {
                self.replace_and_repeat(session, &context, &mut read)
            }
            result => result,
        };
        let detached = session.set_request_context(None);
        context.finish();
        detached?;
        result
    }

    pub(crate) fn surface(
        &mut self,
        session: &mut Session,
        command: SurfaceCommand,
        context: Option<&RequestContext>,
    ) -> Result<SurfaceReply, ClientError> {
        self.surface_scoped(session, command, None, context)
    }

    pub(crate) fn journal(
        &mut self,
        session: &mut Session,
        dispatch: IndexJournalDispatch<'_>,
        context: Option<&RequestContext>,
    ) -> Result<SurfaceReply, ClientError> {
        match dispatch {
            IndexJournalDispatch::Observe(operation_key) => self.surface(
                session,
                SurfaceCommand::IndexOperationStatus { operation_key },
                context,
            ),
            IndexJournalDispatch::Start(start) => {
                start.admit_workspace(&self.paths)?;
                self.surface_scoped(session, start.command(), Some(&start), context)
            }
        }
    }

    fn surface_scoped(
        &mut self,
        session: &mut Session,
        command: SurfaceCommand,
        held_start: Option<&JournaledIndexStart<'_>>,
        context: Option<&RequestContext>,
    ) -> Result<SurfaceReply, ClientError> {
        if let Some(scope) = context {
            scope.check()?;
            session.set_request_context(Some(scope.clone()))?;
        }
        let owned_scope = if context.is_none()
            && (matches!(command, SurfaceCommand::IndexOperationStatus { .. })
                || held_start.is_some())
        {
            let scope = RequestContext::new();
            if held_start.is_some() {
                // Establish the original mutation-derived total before send.
                // Do not bind it over the first exchange's existing policy,
                // and do not mint a fresh total after ambiguous delivery.
                scope.read_deadline(backend_client::MAX_INDEX_OPERATION_WAIT)?;
            } else {
                scope.read_deadline(backend_client::CLIENT_REQUEST_TIMEOUT)?;
                session.set_request_context(Some(scope.clone()))?;
            }
            Some(scope)
        } else {
            None
        };
        let result = self.surface_inner(
            session,
            command.clone(),
            held_start,
            context.or(owned_scope.as_ref()),
        );
        let result = match (held_start, &command, result) {
            (Some(_), SurfaceCommand::IndexOperationStart { operation_key, .. }, Err(cause))
                if !matches!(
                    cause,
                    ClientError::RequestCancelled(_) | ClientError::CommandFailed(_)
                ) =>
            {
                Err(ClientError::IndexOperationDeliveryUnknown {
                    operation_key: *operation_key,
                    cause: Box::new(cause),
                })
            }
            (_, _, result) => result,
        };
        if let Some(scope) = owned_scope {
            let detached = session.set_request_context(None);
            scope.finish();
            detached?;
        }
        result
    }

    fn surface_inner(
        &mut self,
        session: &mut Session,
        command: SurfaceCommand,
        held_start: Option<&JournaledIndexStart<'_>>,
        context: Option<&RequestContext>,
    ) -> Result<SurfaceReply, ClientError> {
        let result = session.surface(command.clone());
        let Err(error) = result else {
            return result;
        };
        let durable_start = held_start.is_some();
        let status_read = matches!(command, SurfaceCommand::IndexOperationStatus { .. });
        if status_read && let Some(scope) = context {
            return self.reobserve_saved_operation(session, command, scope, error);
        }
        if !error.is_connection_lost() || !(durable_start || status_read) {
            return Err(
                if !durable_start
                    && backend_library::command_spec(command.id()).mutation
                        == backend_library::CommandMutation::Write
                {
                    match error {
                        ClientError::Disconnected(kind) => {
                            ClientError::MutationOutcomeUnknown(kind)
                        }
                        error => error,
                    }
                } else {
                    error
                },
            );
        }
        // Waiting has its own already-installed absolute scope. A reconnect
        // uses that exact scope, never a fresh timeout per conditional slice.
        let scope = context.cloned().ok_or_else(|| {
            ClientError::Protocol(
                "owner replacement requires the original command scope".to_owned(),
            )
        })?;
        match held_start {
            Some(start) => self.replace_and_repeat(session, &scope, |session| {
                start.reconcile_after_reconnect(|command| session.surface(command))
            }),
            None => {
                self.replace_and_repeat(session, &scope, |session| session.surface(command.clone()))
            }
        }
    }

    /// A receipt read cannot duplicate owner work. Frame-budget retirement
    /// may happen repeatedly during one long wait, so re-authenticate and read
    /// the identical saved key under the original absolute caller scope.
    fn reobserve_saved_operation(
        &mut self,
        session: &mut Session,
        command: SurfaceCommand,
        scope: &RequestContext,
        mut error: ClientError,
    ) -> Result<SurfaceReply, ClientError> {
        let mut backoff = pacing(REOBSERVE_BACKOFF_CEILING);
        loop {
            scope.check()?;
            if !is_owner_gap(&error) {
                return Err(error);
            }
            // Pace repeated loss without resetting the deadline; cancellation
            // remains observable during the backoff. A frame-budget retirement
            // pays only the first step, while an owner that is restarting
            // backs off toward the ceiling until the original scope ends.
            pause(&mut backoff, scope)?;
            match self.replace(session, scope.clone()) {
                Ok(()) => {}
                Err(next) if is_owner_gap(&next) => {
                    error = next;
                    continue;
                }
                Err(next) => return Err(next),
            }
            match session.surface(command.clone()) {
                Ok(reply) => return Ok(reply),
                Err(next) => error = next,
            }
        }
    }

    /// Replaces the connection and repeats an idempotent `attempt` on it,
    /// again on a fresh connection each time the replacement is itself lost.
    ///
    /// A replacement can be the last connection a dying owner accepts: its
    /// listener outlives the dial for a moment and then closes, so the first
    /// request on it is lost too. That is not an answer. The attempt is a read
    /// or a same-key reconciliation, which any number of repeats leaves
    /// unchanged, and the loop ends at the request scope.
    fn replace_and_repeat<T>(
        &mut self,
        session: &mut Session,
        scope: &RequestContext,
        mut attempt: impl FnMut(&mut Session) -> Result<T, ClientError>,
    ) -> Result<T, ClientError> {
        let mut backoff = pacing(REPLACEMENT_BACKOFF_CEILING);
        loop {
            self.replace(session, scope.clone())?;
            match attempt(session) {
                Err(error) if error.is_connection_lost() => pause(&mut backoff, scope)?,
                result => return result,
            }
        }
    }

    fn replace(
        &mut self,
        session: &mut Session,
        context: RequestContext,
    ) -> Result<(), ClientError> {
        context.read_deadline(backend_runtime::COMMAND_STARTUP_TIMEOUT)?;
        self.used = true;
        // Retire the old socket binding before authenticating a replacement.
        // A failed connection never supplies authority to replay a command.
        session.set_request_context(None)?;
        let mut replacement = (self.connect)(context)?;
        self.observe_preparation(&mut replacement);
        *session = replacement;
        Ok(())
    }

    pub(crate) fn detach(&self, session: &mut Session) -> Result<(), ClientError> {
        session.set_request_context(None)
    }

    pub(crate) fn progress_sender(&self) -> Option<std::sync::mpsc::SyncSender<String>> {
        self.progress.clone()
    }

    fn observe_preparation(&self, session: &mut Session) {
        let progress = self.progress.clone();
        session.observe_query_preparation(move |observation| {
            if let Some(progress) = &progress {
                let _ = progress.try_send(format!(
                    "search preparation: {}",
                    observation.state.as_str()
                ));
            }
            true
        });
    }
}

impl Drop for OwnerRecovery {
    fn drop(&mut self) {
        self.scope.finish();
    }
}

/// A gap in owner availability that a bounded wait can cross: the connection
/// was lost, an owner candidate is still starting, or the live owner did not
/// answer inside its read window. None of these says anything about a
/// command's outcome, so a keyed status read (which changes nothing) may
/// simply ask again on a fresh connection to the *existing* listener. A slow
/// owner is never replaced: only an absent one is (`connect_replacement_with`).
fn is_owner_gap(error: &ClientError) -> bool {
    error.is_owner_gap()
}

fn pacing(ceiling: Duration) -> Backoff {
    Backoff::new(BACKOFF_FIRST, ceiling)
}

/// Sleeps for the next step of `backoff`. The step never outlasts the scope's
/// remaining budget, and cancellation and the deadline are observed
/// throughout, so a pause can only end in `Ok` or in the scope's own terminal
/// error.
fn pause(backoff: &mut Backoff, scope: &RequestContext) -> Result<(), ClientError> {
    let step = backoff.step();
    let started = Instant::now();
    loop {
        let remaining = scope.remaining_read_budget()?.ok_or_else(|| {
            ClientError::Protocol(
                "owner replacement requires the original bounded request scope".to_owned(),
            )
        })?;
        let left = step.saturating_sub(started.elapsed());
        if left.is_zero() {
            return Ok(());
        }
        std::thread::sleep(left.min(remaining).min(PAUSE_SLICE));
    }
}

fn connect_replacement(
    paths: &WorkspacePaths,
    passive: bool,
    context: RequestContext,
    progress: Option<std::sync::mpsc::SyncSender<String>>,
) -> Result<Session, ClientError> {
    connect_replacement_with(
        &context,
        paths.endpoint(),
        |endpoint| Session::connect_with_context(endpoint, context.clone()),
        || bootstrap_owner(paths, passive, &context, progress.as_ref()),
    )
}

fn connect_replacement_with(
    context: &RequestContext,
    endpoint: &Path,
    dial: impl FnMut(&Path) -> Result<Session, ClientError>,
    bootstrap: impl FnMut() -> Result<PathBuf, ClientError>,
) -> Result<Session, ClientError> {
    backend_client::connect_local_owner_with(context, endpoint, dial, bootstrap)
}

/// Starts (or, when `passive`, only finds) the owner for an absent endpoint.
fn bootstrap_owner(
    paths: &WorkspacePaths,
    passive: bool,
    context: &RequestContext,
    progress: Option<&std::sync::mpsc::SyncSender<String>>,
) -> Result<PathBuf, ClientError> {
    context.check()?;
    let remaining = context.remaining_read_budget()?.ok_or_else(|| {
        ClientError::Protocol(
            "owner replacement requires the original bounded request scope".to_owned(),
        )
    })?;
    if passive {
        return backend_runtime::connect_existing_locald(paths)
            .map_err(|error| ClientError::Startup(error.into()));
    }
    #[cfg(unix)]
    {
        match backend_runtime::start_locald_cancellable(
            paths,
            remaining.min(backend_runtime::COMMAND_STARTUP_TIMEOUT),
            |phase| {
                if let Some(progress) = progress {
                    let _ = progress.try_send(format!("local startup: {}", phase.as_str()));
                }
            },
            || context.check().is_ok(),
        )
        .map_err(|error| ClientError::Startup(error.into()))?
        {
            backend_runtime::LocaldStartup::Ready(endpoint) => Ok(endpoint),
            backend_runtime::LocaldStartup::Pending(pending) => {
                context.check()?;
                Err(ClientError::Startup(
                    backend_runtime::RuntimeError::StartupPending(pending).into(),
                ))
            }
        }
    }
    #[cfg(not(unix))]
    {
        // Do not promise a cancellable bootstrap on a platform whose
        // existing owner setup remains synchronous. Authenticate an
        // already available replacement without launching another owner.
        let _ = (remaining, progress);
        backend_runtime::connect_existing_locald(paths)
            .map_err(|error| ClientError::Startup(error.into()))
    }
}

#[cfg(all(test, unix))]
#[path = "tests/owner_recovery.rs"]
mod tests;
