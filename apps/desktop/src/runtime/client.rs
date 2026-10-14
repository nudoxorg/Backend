//! The production engine adapter for the desktop actor.
//!
//! This is the only place where the UI runtime knows that the local-first
//! producer happens to be a `backend_client::Session`.  The actor owns this
//! adapter on its worker thread; the GPUI thread receives only versioned DTOs
//! and never touches a socket, a reply codec, or a registry record.

use super::actor::{CancellationToken, CancellationWake, EngineClient, EngineDto, EngineFault, EngineRequest, ProjectDto};
use super::liveness::{report_if_dead, transport_break};
use super::owner::OwnerFault;
use crate::core::{FaultCode, LocalProjectId, VersionedRoot};
use crate::model::{ObjectId, PackageSummary};
use backend_client::{ClientError, LocalSubscriptionTransport, Session, TransportInterrupt};
use backend_library::{RegistryDownloadCount, RowId, SurfaceCommand, SurfaceReply};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

const DESKTOP_CONNECT_TIMEOUT: Duration = Duration::from_secs(1);

/// Pause before each retry of a root bootstrap whose transport broke. Cold
/// start overlaps the owner's busiest seconds, so a stalled or closed stream
/// is retried on a fresh authenticated socket after a bounded, growing pause;
/// one entry per retry, so the first attempt plus three retries at most.
const BOOTSTRAP_RETRY_BACKOFF: [Duration; 3] = [
    Duration::from_millis(100),
    Duration::from_millis(250),
    Duration::from_millis(500),
];

/// A worker-owned local service session with typed read replay.
pub struct LocalEngineClient {
    endpoint: PathBuf,
    project: LocalProjectId,
    session: Option<Session>,
    subscription: Option<LocalSubscriptionTransport>,
    /// Attached generation admitted for this actor request and its sockets.
    attached_epoch: Option<super::owner::Epoch>,
    active_cancel: Option<CancellationToken>,
    /// The owner this client waits for, on the actor thread (I1).
    gate: Option<super::owner::OwnerGate>,
}

// A transport failure anywhere in a bootstrap (setup, a page exchange, or a
// release that could not keep its root) requires a fresh authenticated stream;
// malformed proofs and acknowledgements must never be retried as connectivity.
fn bootstrap_transport_break(error: &backend_client::LocalSubscriptionExchangeError) -> bool {
    use backend_client::LocalSubscriptionExchangeError;
    use backend_replication::LocalControlExchangeFailure;
    match error {
        LocalSubscriptionExchangeError::Setup(error)
        | LocalSubscriptionExchangeError::Invalid(error) => transport_break(error),
        LocalSubscriptionExchangeError::Exchange(error) => matches!(
            error.failure,
            LocalControlExchangeFailure::Stalled
                | LocalControlExchangeFailure::Closed
                | LocalControlExchangeFailure::Io(_)
        ),
        LocalSubscriptionExchangeError::Rejected { .. } => false,
    }
}

impl LocalEngineClient {
    fn current_cancel(&self) -> Result<CancellationToken, EngineFault> {
        self.active_cancel.clone().ok_or_else(|| EngineFault::Failed(
            crate::core::ErrorValue::new(FaultCode::Protocol, "the local request has no cancellation scope"),
        ))
    }

    /// Creates a client without opening a socket on the caller's thread.
    #[must_use]
    pub fn new(endpoint: impl AsRef<Path>, project: LocalProjectId) -> Self {
        Self {
            endpoint: endpoint.as_ref().to_path_buf(),
            project,
            session: None,
            subscription: None,
            attached_epoch: None,
            active_cancel: None,
            gate: None,
        }
    }

    /// A client whose requests wait, on the actor thread, for the owner to
    /// answer; a request cancelled while it waited is not run.
    #[must_use]
    pub fn gated(
        endpoint: impl AsRef<Path>,
        project: LocalProjectId,
        gate: super::owner::OwnerGate,
    ) -> Self {
        Self {
            gate: Some(gate),
            ..Self::new(endpoint, project)
        }
    }


    fn session(&mut self) -> Result<&mut Session, EngineFault> {
        if self.session.is_none() {
            self.session = Some(Session::connect_with_timeouts(&self.endpoint, DESKTOP_CONNECT_TIMEOUT, Duration::from_secs(30))
                .map_err(|error| self.client_fault(error))?);
        }
        Ok(self.session.as_mut().expect("session installed"))
    }

    fn subscription(&mut self) -> Result<&mut LocalSubscriptionTransport, EngineFault> {
        // A stalled exchange retires its socket and interrupt together. The
        // next request must authenticate a replacement before registering its
        // cancellation wake; bootstrap's own reconnect happens too late.
        if self.subscription.as_ref().is_none_or(|transport| {
            transport.interrupt_handle().is_none()
        }) {
            self.subscription = None;
            self.subscription =
                Some(LocalSubscriptionTransport::connect_timeout(&self.endpoint, DESKTOP_CONNECT_TIMEOUT)
                    .map_err(|error| self.client_fault(error))?);
        }
        Ok(self.subscription.as_mut().expect("subscription installed"))
    }

    fn client_fault(&self, error: ClientError) -> EngineFault {
        if !self.active_cancel.as_ref().is_some_and(CancellationToken::is_cancelled) {
            let _ = report_if_dead(self.gate.as_ref(), self.attached_epoch, &self.endpoint, &error);
        }
        fault(error)
    }

    fn bootstrap_fault(
        &self,
        error: backend_client::LocalSubscriptionExchangeError,
    ) -> EngineFault {
        if self
            .active_cancel
            .as_ref()
            .is_some_and(CancellationToken::is_cancelled)
        {
            return EngineFault::Cancelled;
        }
        if let backend_client::LocalSubscriptionExchangeError::Exchange(exchange) = &error {
            if exchange.failure == backend_replication::LocalControlExchangeFailure::Cancelled {
                return EngineFault::Cancelled;
            }
            if bootstrap_transport_break(&error) {
                // A failed release does not prove the owner died. Preserve the
                // exact exchange diagnostic after the bounded retries.
                return EngineFault::Failed(crate::core::ErrorValue::new(
                    FaultCode::Transport,
                    error.to_string(),
                ));
            }
        }
        self.client_fault(error.into_client_error())
    }

    fn bootstrap_root(
        &mut self,
        use_publication: bool,
    ) -> Result<(Arc<backend_library::ViewRoot>, backend_library::Cursor), EngineFault> {
        let attachment = self.attached_epoch;
        let cancel = self.current_cancel()?;
        let publication = if use_publication {
            self.gate
                .as_ref()
                .zip(attachment)
                .and_then(|(gate, attachment)| gate.publication(attachment))
        } else {
            None
        };
        if let Some(publication) = publication {
            // Explicit user refreshes also reach this path. A cheap admitted
            // revision prevents returning an observer cache behind the owner.
            let session = self.session()?;
            let _wake = cancel_wake(&cancel, session.interrupt_handle())?;
            if cancel.is_cancelled() {
                return Err(EngineFault::Cancelled);
            }
            let revision = session
                .revision()
                .map_err(|error| self.client_fault(error))?;
            if cancel.is_cancelled() {
                return Err(EngineFault::Cancelled);
            }
            if revision.cursor() == publication.1 {
                return Ok(publication);
            }
        }
        let mut retries = 0;
        let admitted = loop {
            let result = {
                let subscription = self.subscription()?;
                let _wake = cancel_wake(&cancel, subscription.interrupt_handle())?;
                if cancel.is_cancelled() {
                    return Err(EngineFault::Cancelled);
                }
                subscription.bootstrap_root_with_exchange_error()
            };
            if cancel.is_cancelled() {
                self.subscription = None;
                return Err(EngineFault::Cancelled);
            }
            match result {
                Ok(root) => break root,
                Err(error) if bootstrap_transport_break(&error) => {
                    // The next attempt authenticates a fresh stream.
                    self.subscription = None;
                    let Some(&delay) = BOOTSTRAP_RETRY_BACKOFF.get(retries) else {
                        return Err(self.bootstrap_fault(error));
                    };
                    super::trace::mark(
                        "root.bootstrap-retry",
                        format_args!("attempt {} after {} ms: {error}", retries + 2, delay.as_millis()),
                    );
                    if !self.pause_before_retry(&cancel, delay) {
                        // Cancelled, or the attached owner changed while it
                        // waited; a replacement owner asks its own Root.
                        return Err(if cancel.is_cancelled() {
                            EngineFault::Cancelled
                        } else {
                            EngineFault::Superseded
                        });
                    }
                    retries += 1;
                }
                Err(error) => return Err(self.bootstrap_fault(error)),
            }
        };
        let (view, cursor) = admitted;
        if self.subscription.as_ref().is_some_and(|transport| {
            transport.authenticated_peer().is_none()
        }) {
            // A fully checked root can survive an acknowledged release whose
            // socket was retired. It does not authorize reuse of that socket.
            self.subscription = None;
            super::trace::mark("root.bootstrap-retired-stream", "verified-root-preserved");
        }
        let view = Arc::new(view);
        if let (Some(gate), Some(attachment)) = (&self.gate, attachment) {
            if gate.publish_view(attachment, Arc::clone(&view), cursor)
                != super::owner::PublicationAdmission::Admitted
            {
                return Err(EngineFault::Superseded);
            }
        }
        Ok((view, cursor))
    }

    /// Waits out one retry backoff. Ends early with `false` when the request
    /// is cancelled or the owner generation this client attached to is gone.
    fn pause_before_retry(&self, cancel: &CancellationToken, delay: Duration) -> bool {
        match &self.gate {
            Some(gate) => gate.retry_pause(self.attached_epoch, cancel, delay),
            None => pause_unless_cancelled(cancel, delay),
        }
    }

    fn request_root(&mut self, request: &EngineRequest) -> Result<EngineDto, EngineFault> {
        let EngineRequest::Root {
            request: request_id,
            basis,
            project: captured_project,
            ..
        } = request
        else {
            unreachable!("root adapter called with a non-root request")
        };
        super::trace::mark("root.bootstrap-entered", "worker");
        let (view, revision) = self.bootstrap_root(true).map_err(|error| {
            let category = match &error {
                EngineFault::Cancelled => "cancelled".to_owned(),
                EngineFault::Superseded => "attachment-or-root-superseded".to_owned(),
                EngineFault::Failed(fault) => format!("fault:{:?}:{}", fault.code(), fault.message()),
                _ => "unexpected-adapter-fault".to_owned(),
            };
            super::trace::mark("root.bootstrap-failed", category);
            error
        })?;
        super::trace::mark("root.bootstrap-admitted", "exact-attachment");
        if revision.root() != view.root() {
            return Err(EngineFault::Failed(crate::core::ErrorValue::new(
                FaultCode::Protocol,
                "the local service returned mismatched root and revision identities",
            )));
        }
        let key =
            VersionedRoot::from_revision(basis.producer_epoch(), revision, basis.observation());
        let packages = view
            .rows()
            .iter()
            .filter_map(|row| match row.id {
                RowId::Package(key) => {
                    crate::core::PackageId::try_from_backend(key, &row.label).ok()
                }
                RowId::Symbol(_) | RowId::Object(_) => None,
            })
            .collect::<Vec<_>>();
        let context = captured_project.as_ref().unwrap_or(&self.project);
        let project = Some(ProjectDto {
            id: context.clone(),
            label: Arc::from(context.as_str()),
            packages: packages.into(),
        });
        let catalog = Some(self.catalog()?);
        Ok(EngineDto::Root {
            request: *request_id,
            basis: *basis,
            key,
            revision,
            delta: None,
            project,
            catalog,
        })
    }

    fn request_connection_probe(&mut self, request: &EngineRequest) -> Result<EngineDto, EngineFault> {
        let EngineRequest::ConnectionProbe { request, basis, .. } = request else {
            unreachable!("connection adapter called with a non-probe request")
        };
        // This is the same authenticated Revision command the owner uses for
        // liveness. It cannot fail because Explore, bootstrap or a product
        // capability is unavailable. The ordinary read retry and cancellation
        // rules still apply to a broken local socket.
        let revision = self.with_session_read(Session::revision)?;
        Ok(EngineDto::ConnectionProbe {
            request: *request,
            basis: *basis,
            revision: revision.cursor(),
        })
    }

    fn catalog(&mut self) -> Result<Arc<[PackageSummary]>, EngineFault> {
        let reply = self
            .with_session_read(|session| {
                session.surface(SurfaceCommand::Explore {
                    query: None,
                    limit: 64,
                })
            });
        Self::catalog_from_reply(reply)
    }

    fn catalog_from_reply(
        reply: Result<SurfaceReply, EngineFault>,
    ) -> Result<Arc<[PackageSummary]>, EngineFault> {
        let SurfaceReply::Explored(records) = reply? else {
            return Err(EngineFault::Failed(crate::core::ErrorValue::new(
                FaultCode::Protocol,
                "the local service returned a non-Explore reply for the catalog request",
            )));
        };
        Ok(
            records
                .iter()
                .filter_map(package_summary)
                .collect::<Vec<_>>()
                .into(),
        )
    }

    fn request_surface(&mut self, request: &EngineRequest) -> Result<EngineDto, EngineFault> {
        let EngineRequest::Surface {
            request: request_id,
            command,
            basis,
            ..
        } = request
        else {
            unreachable!("surface adapter called with a non-surface request")
        };
        let reply = if super::reads::read_only(command) {
            self.with_session_read(|session| session.surface(command.clone()))?
        } else {
            self.with_session_once(|session| session.surface(command.clone()))?
        };
        Ok(EngineDto::Surface {
            request: *request_id,
            basis: *basis,
            command: command.clone(),
            reply,
        })
    }

    fn request_index(&mut self, request: &EngineRequest) -> Result<EngineDto, EngineFault> {
        let EngineRequest::IndexProject { request: request_id, project, operation, basis, owner, .. } = request else {
            unreachable!("index adapter called with a non-index request")
        };
        let owner = owner.as_ref().filter(|owner| owner.is_gated()).ok_or_else(|| EngineFault::IndexNotSent {
            project: project.clone(), error: crate::core::ErrorValue::new(FaultCode::Protocol,
                "This index request has no certified local owner lifetime. Nothing was sent."),
        })?;
        if !operation.belongs_to(project) {
            return Err(EngineFault::IndexNotSent { project: project.clone(), error: crate::core::ErrorValue::new(
                FaultCode::Protocol, "The saved index operation does not belong to this project.") });
        }
        let mut session = Session::connect_with_timeouts(&self.endpoint, DESKTOP_CONNECT_TIMEOUT, Duration::from_secs(30))
            .map_err(|error| EngineFault::IndexNotSent { project: project.clone(), error: crate::core::ErrorValue::new(
                FaultCode::Transport, format!("The first-send connection could not be opened: {error}. Nothing was sent.")) })?;
        let cancel = self.current_cancel()?;
        let result = {
            let _wake = cancel_wake(&cancel, session.interrupt_handle())
                .map_err(|error| EngineFault::IndexNotSent { project: project.clone(), error: crate::core::ErrorValue::new(
                    FaultCode::Transport, format!("The first-send connection could not be cancelled safely: {error:?}. Nothing was sent.")) })?;
            {
                let certified = owner.ready_for_send(*basis, &cancel).map_err(|message| EngineFault::IndexNotSent {
                    project: project.clone(), error: crate::core::ErrorValue::new(FaultCode::Transport, message),
                })?;
                let revision = session.revision().map_err(|error| EngineFault::IndexNotSent {
                    project: project.clone(), error: crate::core::ErrorValue::new(FaultCode::Transport,
                        format!("The first-send connection could not confirm its index authority: {error}. Nothing was sent.")),
                })?;
                let socket = VersionedRoot::from_revision(certified.producer_epoch(), revision.cursor(), 0);
                if !super::actor::admits_mutation_root(certified, socket) {
                    return Err(EngineFault::IndexNotSent { project: project.clone(), error: crate::core::ErrorValue::new(
                        FaultCode::Protocol, "The first-send connection did not match the current index producer stream. Nothing was sent.") });
                }
                // A later ordinary publication may overtake this revision
                // read. Recheck lifetime/readiness against the captured basis;
                // do not mistake that later observation for a socket regression.
                owner.ready_for_send(*basis, &cancel).map_err(|message| EngineFault::IndexNotSent {
                    project: project.clone(), error: crate::core::ErrorValue::new(FaultCode::Transport, message),
                })?;
            }
            if cancel.is_cancelled() { return Err(EngineFault::IndexNotSent { project: project.clone(),
                error: crate::core::ErrorValue::new(FaultCode::Transport, "This index request stopped before its first send. Nothing was sent.") }); }
            // Exactly one mutation send. Every interrupted answer is reconciled
            // by this persisted key; it never starts a new request implicitly.
            session.start_index_operation(operation.key, operation.package.clone(), operation.execution_intent)
        };
        let observation = match result {
            Ok(observation) => observation,
            Err(error) => {
                let _ = self.client_fault(error);
                return Err(EngineFault::IndexUnconfirmed { project: project.clone() });
            }
        };
        self.session = Some(session);
        self.index_observation(*request_id, *basis, project, operation, observation)
    }

    fn request_index_status(&mut self, request: &EngineRequest) -> Result<EngineDto, EngineFault> {
        let EngineRequest::IndexOperationStatus { request: request_id, project, operation, basis, .. } = request else {
            unreachable!("index status adapter called with a non-status request")
        };
        // Status is read-only and may reconnect once. It never calls Start,
        // even when the owner reports Unknown or Unresolved.
        let observation = self.with_session_read(|session| session.index_operation_status(operation.key))
            .map_err(|_| EngineFault::IndexUnconfirmed { project: project.clone() })?;
        self.index_observation(*request_id, *basis, project, operation, observation)
    }

    fn index_observation(
        &mut self,
        request: crate::navigation::RequestId,
        basis: VersionedRoot,
        project: &LocalProjectId,
        operation: &crate::model::IndexOperationClaim,
        observation: backend_library::IndexOperationObservation,
    ) -> Result<EngineDto, EngineFault> {
        if !operation.belongs_to(project) || !operation.admits_observation(&observation) {
            return Err(EngineFault::IndexUnconfirmed {
                project: project.clone(),
            });
        }
        Ok(EngineDto::IndexOperation {
            request,
            basis,
            project: project.clone(),
            operation: operation.clone(),
            observation,
        })
    }

    /// A read may be retried once after a transport break, using a fresh
    /// session. The owner generation is checked again before that retry.
    fn with_session_read<T>(
        &mut self,
        mut operation: impl FnMut(&mut Session) -> Result<T, ClientError>,
    ) -> Result<T, EngineFault> {
        for attempt in 0..2 {
            let cancel = self.current_cancel()?;
            if cancel.is_cancelled() {
                return Err(EngineFault::Cancelled);
            }
            let result = {
                let session = self.session()?;
                let _wake = cancel_wake(&cancel, session.interrupt_handle())?;
                if cancel.is_cancelled() { return Err(EngineFault::Cancelled); }
                operation(session)
            };
            if cancel.is_cancelled() {
                self.session = None;
                return Err(EngineFault::Cancelled);
            }
            match result {
                Ok(value) => return Ok(value),
                Err(error) if attempt == 0 && transport_break(&error) => {
                    self.session = None;
                    let _ = self.client_fault(error);
                    if let Some(gate) = &self.gate {
                        gate.wait_cancelled(&cancel)
                            .map_err(owner_fault)?;
                        if gate.ready_epoch() != self.attached_epoch {
                            return Err(EngineFault::Superseded);
                        }
                    }
                }
                Err(error) if transport_break(&error) => {
                    self.session = None;
                    return Err(self.client_fault(error));
                }
                Err(error) => return Err(self.client_fault(error)),
            }
        }
        unreachable!("one read attempt and one retry return above")
    }

    /// A disconnect after a command's submission leaves a mutation's outcome
    /// ambiguous. Only a later explicit request can retry it.
    fn with_session_once<T>(
        &mut self,
        operation: impl FnOnce(&mut Session) -> Result<T, ClientError>,
    ) -> Result<T, EngineFault> {
        let cancel = self.current_cancel()?;
        let result = {
            let session = self.session()?;
            let _wake = cancel_wake(&cancel, session.interrupt_handle())?;
            if cancel.is_cancelled() { return Err(EngineFault::Cancelled); }
            operation(session)
        };
        if cancel.is_cancelled() {
            self.session = None;
            return match result {
                Ok(value) => Ok(value),
                Err(_) => Err(EngineFault::MutationUnconfirmed),
            };
        }
        match result {
            Ok(value) => Ok(value),
            Err(error) if transport_break(&error) => {
                self.session = None;
                let _ = self.client_fault(error);
                Err(EngineFault::MutationUnconfirmed)
            }
            Err(error) => Err(self.client_fault(error)),
        }
    }
}

/// A timed wait for a client with no owner gate, woken at once by `cancel`.
/// Returns `false` when it was cancelled before `delay` elapsed.
fn pause_unless_cancelled(cancel: &CancellationToken, delay: Duration) -> bool {
    use std::sync::{Condvar, Mutex, PoisonError};
    let signal = Arc::new((Mutex::new(()), Condvar::new()));
    let woken = Arc::clone(&signal);
    // The callback locks before notifying, so cancellation cannot fall
    // between the predicate check below and the wait.
    let _wake = cancel.on_cancel(move || {
        let _guard = woken.0.lock().unwrap_or_else(PoisonError::into_inner);
        woken.1.notify_all();
    });
    let deadline = std::time::Instant::now() + delay;
    let mut guard = signal.0.lock().unwrap_or_else(PoisonError::into_inner);
    loop {
        if cancel.is_cancelled() {
            return false;
        }
        let left = deadline.saturating_duration_since(std::time::Instant::now());
        if left.is_zero() {
            return true;
        }
        guard = signal
            .1
            .wait_timeout(guard, left)
            .unwrap_or_else(PoisonError::into_inner)
            .0;
    }
}

fn cancel_wake(
    cancel: &CancellationToken,
    interrupt: Option<TransportInterrupt>,
) -> Result<CancellationWake, EngineFault> {
    let Some(interrupt) = interrupt else {
        return Err(EngineFault::Failed(crate::core::ErrorValue::new(
            FaultCode::Transport,
            "the local connection cannot be interrupted safely",
        )));
    };
    Ok(cancel.on_cancel(move || interrupt.interrupt()))
}

impl EngineClient for LocalEngineClient {

    fn operation_observer(&self) -> Option<Box<dyn EngineClient>> {
        let mut observer = Self::new(&self.endpoint, self.project.clone());
        observer.gate = self.gate.clone();
        Some(Box::new(observer))
    }

    fn execute(&mut self, request: &EngineRequest) -> Result<EngineDto, EngineFault> {
        self.active_cancel = Some(request.cancellation().clone());
        let _lifetime = match request {
            EngineRequest::IndexProject { owner: Some(owner), project, basis, .. } => {
                let lifetime = owner.bind_cancellation(request.cancellation());
                if let Err(message) = owner.ready_for_send(*basis, request.cancellation()) {
                    self.active_cancel = None;
                    return Err(EngineFault::IndexNotSent { project: project.clone(),
                        error: crate::core::ErrorValue::new(FaultCode::Transport, message) });
                }
                lifetime
            }
            _ => None,
        };
        if let Some(gate) = &self.gate {
            if let Err(error) = gate.wait_cancelled(request.cancellation()) {
                self.active_cancel = None;
                return Err(match request {
                    EngineRequest::IndexProject { project, .. } => EngineFault::IndexNotSent { project: project.clone(),
                        error: crate::core::ErrorValue::new(FaultCode::Transport, format!("The owner stopped before this request's first send: {error}. Nothing was sent.")) },
                    EngineRequest::IndexOperationStatus { project, .. } =>
                        EngineFault::IndexUnconfirmed { project: project.clone() },
                    _ => owner_fault(error),
                });
            }
            // Superseded while it waited (the startup root read, once the
            // owner's own root arrived): not run.
            if request.cancelled() {
                self.active_cancel = None;
                return Err(match request {
                    EngineRequest::IndexProject { project, .. } => EngineFault::IndexNotSent { project: project.clone(),
                        error: crate::core::ErrorValue::new(FaultCode::Transport, "This index request stopped before its first send. Nothing was sent.") },
                    _ => EngineFault::Cancelled,
                });
            }
            // An attached owner may restart under the same root. Discard
            // sockets admitted by its prior serving generation before this
            // request can use them.
            let epoch = gate.ready_epoch();
            if self.attached_epoch != epoch {
                self.session = None;
                self.subscription = None;
            }
            self.attached_epoch = epoch;
        }
        let result = match request {
            EngineRequest::ConnectionProbe { .. } => self.request_connection_probe(request),
            EngineRequest::Root { .. } => self.request_root(request),
            EngineRequest::Surface { .. } => self.request_surface(request),
            EngineRequest::IndexProject { .. } => self.request_index(request),
            EngineRequest::IndexOperationStatus { .. } => self.request_index_status(request),
            EngineRequest::Object {
                request,
                basis,
                object,
                delta,
                ..
            } => Ok(EngineDto::Object {
                request: *request,
                basis: *basis,
                object: *object,
                delta: *delta,
            }),
        };
        // Request wakes can permanently interrupt either cached transport,
        // including early returns from the publication revision shortcut or
        // subscription setup. Retire both after their request-local wakes
        // have dropped; the next request must authenticate fresh sockets.
        // This does not erase or replay the result of an admitted mutation.
        if request.cancelled() {
            self.session = None;
            self.subscription = None;
        }
        self.active_cancel = None;
        let owner_changed = self.attached_epoch.is_some()
            && self.gate.as_ref().and_then(super::owner::OwnerGate::ready_epoch)
                != self.attached_epoch;
        let result = match (request, result) {
            (EngineRequest::IndexProject { project, .. } | EngineRequest::IndexOperationStatus { project, .. },
                Err(EngineFault::Cancelled | EngineFault::Superseded | EngineFault::Failed(_))) =>
                Err(EngineFault::IndexUnconfirmed { project: project.clone() }),
            (_, result) => result,
        };
        owner_change_result(request, result, owner_changed)
    }
}

/// A replacement owner supersedes reads, but cannot erase a mutation receipt.
/// An uncertain mutation remains uncertain; the caller must reconcile it by
/// the exact operation key before a new submission.
fn owner_change_result(
    request: &EngineRequest,
    result: Result<EngineDto, EngineFault>,
    owner_changed: bool,
) -> Result<EngineDto, EngineFault> {
    if !owner_changed {
        return result;
    }
    match (request, result) {
        (EngineRequest::IndexProject { .. }, Err(error @ EngineFault::IndexNotSent { .. })) => {
            Err(error)
        }
        (
            EngineRequest::IndexProject { .. } | EngineRequest::IndexOperationStatus { .. },
            Ok(dto @ EngineDto::IndexOperation { .. }),
        ) if matches!(&dto, EngineDto::IndexOperation { observation: backend_library::IndexOperationObservation::Known(status), .. }
                    if matches!(status.state, backend_library::IndexOperationState::Published(_) | backend_library::IndexOperationState::PartiallyPublished { .. } | backend_library::IndexOperationState::Failed { .. })) =>
        {
            Ok(dto)
        }
        (EngineRequest::IndexOperationStatus { project, .. }, _) => {
            Err(EngineFault::IndexUnconfirmed {
                project: project.clone(),
            })
        }
        (EngineRequest::IndexProject { .. }, Err(EngineFault::IndexCancelled { project })) => {
            Err(EngineFault::IndexCancelled { project })
        }
        (EngineRequest::IndexProject { project, .. }, _) => Err(EngineFault::IndexUnconfirmed {
            project: project.clone(),
        }),
        (EngineRequest::Surface { command, .. }, Ok(dto)) if !super::reads::read_only(command) => {
            Ok(dto)
        }
        (EngineRequest::Surface { command, .. }, _) if !super::reads::read_only(command) => {
            Err(EngineFault::MutationUnconfirmed)
        }
        _ => Err(EngineFault::Superseded),
    }
}

fn project_dto(
    id: LocalProjectId,
    label: Arc<str>,
    view: &backend_library::ViewRoot,
) -> ProjectDto {
    let packages = view
        .rows()
        .iter()
        .filter_map(|row| match row.id {
            RowId::Package(key) => crate::core::PackageId::try_from_backend(key, &row.label).ok(),
            RowId::Symbol(_) | RowId::Object(_) => None,
        })
        .collect::<Vec<_>>();
    ProjectDto {
        id,
        label,
        packages: packages.into(),
    }
}

fn project_label(project: &LocalProjectId) -> Arc<str> {
    project
        .path()
        .file_name()
        .and_then(|name| name.to_str())
        .filter(|name| !name.is_empty())
        .map(Arc::<str>::from)
        .unwrap_or_else(|| Arc::from(project.as_str()))
}

pub(crate) fn package_summary(
    record: &backend_library::RegistryPackageRecord,
) -> Option<PackageSummary> {
    let coordinate = crate::core::PackageId::new(record.coordinate.as_str()).ok()?;
    let downloads = match &record.downloads {
        RegistryDownloadCount::Exact(value) => format!("{value} downloads"),
        RegistryDownloadCount::Approximate(value) => format!("~{value} downloads"),
        RegistryDownloadCount::Unavailable(value) => format!("downloads {value:?}"),
    };
    Some(PackageSummary {
        object: ObjectId::from_backend(backend_library::object_version(
            record.coordinate.as_str().as_bytes(),
        )),
        coordinate,
        name: Arc::from(record.name.as_str()),
        version: Arc::from(record.version.as_str()),
        ecosystem: Arc::from(record.ecosystem.as_str()),
        bytes: record.bytes,
        standing: Arc::from(format!("{:?}", record.standing)),
        downloads: Arc::from(downloads),
        advisory: Arc::from(format!("{:?}", record.advisory)),
    })
}

fn fault(error: ClientError) -> EngineFault {
    let code = match error {
        ClientError::RequestCancelled(_) => return EngineFault::Cancelled,
        ClientError::Protocol(_) | ClientError::IncoherentView => FaultCode::Protocol,
        ClientError::Startup(_)
        | ClientError::Disconnected(_)
        | ClientError::QueryPreparationDisconnected { .. }
        | ClientError::MutationOutcomeUnknown(_)
        | ClientError::IndexOperationDeliveryUnknown { .. }
        | ClientError::Io(_)
        | ClientError::Transport(_)
        | ClientError::RemoteDeadlineExceeded => FaultCode::Transport,
        ClientError::CommandFailed(_) => FaultCode::Missing,
        ClientError::BasisMismatch { .. }
        | ClientError::FreshnessMismatch
        | ClientError::RequestMismatch { .. }
        | ClientError::CursorMismatch
        | ClientError::StaleCursor
        | ClientError::StaleSelection
        | ClientError::StaleRemoteRoot { .. }
        | ClientError::StaleRemoteCapability
        | ClientError::RemoteCapabilityRevoked => FaultCode::Cancelled,
    };
    EngineFault::Failed(crate::core::ErrorValue::new(code, error.to_string()))
}

fn owner_fault(fault: OwnerFault) -> EngineFault {
    if fault == OwnerFault::Cancelled {
        EngineFault::Cancelled
    } else {
        EngineFault::Failed(crate::core::ErrorValue::new(
            FaultCode::Transport,
            format!("the index could not start: {fault}"),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::navigation::RequestId;

    #[test]
    fn bootstrap_retry_preserves_transport_cause_and_never_retries_invalid_or_cancelled_frames() {
        use backend_client::LocalSubscriptionExchangeError;
        use backend_replication::{
            LocalControlError, LocalControlExchangeError, LocalControlExchangeFailure,
            LocalControlExchangePhase, LocalControlExchangeProgress,
        };
        let project = LocalProjectId::new("bootstrap-policy").expect("project");
        let mut client = LocalEngineClient::new("unopened-bootstrap-policy", project);
        let cancel = CancellationToken::new();
        client.active_cancel = Some(cancel.clone());
        for (failure, retry) in [
            (LocalControlExchangeFailure::Stalled, true),
            (LocalControlExchangeFailure::Closed, true),
            (
                LocalControlExchangeFailure::Io(std::io::ErrorKind::ConnectionReset),
                true,
            ),
            (LocalControlExchangeFailure::Cancelled, false),
            (
                LocalControlExchangeFailure::Protocol(LocalControlError::Invalid("wrong lease")),
                false,
            ),
        ] {
            let error = LocalSubscriptionExchangeError::Exchange(LocalControlExchangeError {
                failure,
                progress: LocalControlExchangeProgress {
                    request_id: 2,
                    phase: LocalControlExchangePhase::ReadingHeader,
                    write_offset: 30,
                    header_offset: 0,
                    body_len: None,
                    body_offset: 0,
                    deadline: std::time::Instant::now(),
                    elapsed: Duration::from_millis(50),
                },
            });
            assert_eq!(bootstrap_transport_break(&error), retry);
            let diagnostic = error.to_string();
            let fault = client.bootstrap_fault(error);
            match (failure, fault) {
                (LocalControlExchangeFailure::Cancelled, EngineFault::Cancelled) => {}
                (_, EngineFault::Failed(value)) => {
                    assert_eq!(
                        value.code(),
                        if retry {
                            FaultCode::Transport
                        } else {
                            FaultCode::Protocol
                        }
                    );
                    assert_eq!(value.message(), diagnostic);
                }
                other => panic!("unexpected bootstrap fault: {other:?}"),
            }
        }
        for error in [
            LocalSubscriptionExchangeError::Invalid(ClientError::Protocol(
                "wrong release acknowledgement".into(),
            )),
            LocalSubscriptionExchangeError::Rejected {
                request_id: 2,
                message: "unknown lease".into(),
            },
        ] {
            assert!(!bootstrap_transport_break(&error));
            cancel.cancel();
            assert!(
                matches!(client.bootstrap_fault(error), EngineFault::Cancelled),
                "original cancellation wins over every terminal release outcome"
            );
        }
        assert!(client.subscription.is_none());
        assert!(client.session.is_none());
    }

    #[cfg(unix)]
    #[test]
    fn a_stalled_bootstrap_socket_is_reauthenticated_before_its_next_cancel_wake() {
        use backend_replication::{
            LocalControlExchangeDecision, LocalControlLimits, LocalControlRequest,
            LocalControlResponse, LocalSubscriptionOperation, LocalSubscriptionRequest,
            decode_request, encode_response, read_frame, write_frame,
        };
        use std::os::unix::fs::PermissionsExt as _;
        use std::os::unix::net::{UnixListener, UnixStream};
        use std::time::Instant;

        struct Endpoint(PathBuf);
        impl Drop for Endpoint {
            fn drop(&mut self) {
                let _ = std::fs::remove_file(&self.0);
            }
        }
        fn accept(listener: &UnixListener) -> UnixStream {
            let until = Instant::now() + Duration::from_secs(2);
            loop {
                match listener.accept() {
                    Ok((stream, _)) => {
                        // macOS accepts inherit the listener's nonblocking
                        // flag; the owner uses bounded blocking frame reads.
                        stream.set_nonblocking(false).expect("blocking accepted stream");
                        return stream;
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        assert!(Instant::now() < until, "replacement socket never connected");
                        std::thread::sleep(Duration::from_millis(2));
                    }
                    Err(error) => panic!("accept authenticated client: {error}"),
                }
            }
        }

        let endpoint = Endpoint(PathBuf::from(format!(
            "/tmp/nudox-bootstrap-retry-{}-{}.sock",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock")
                .as_nanos(),
        )));
        let listener = UnixListener::bind(&endpoint.0).expect("private listener");
        std::fs::set_permissions(&endpoint.0, std::fs::Permissions::from_mode(0o600))
            .expect("private endpoint");
        listener.set_nonblocking(true).expect("bounded accept");
        let server = std::thread::spawn(move || {
            let limits = LocalControlLimits::default();
            let mut stalled = accept(&listener);
            stalled
                .set_read_timeout(Some(Duration::from_secs(1)))
                .expect("bounded read");
            let first = read_frame(&mut stalled, limits).expect("initial request reached owner");
            assert!(matches!(
                decode_request(&first, limits),
                Ok(LocalControlRequest::Subscription(_))
            ));
            // Keep the first connection open without answering its header.
            let mut recovered = accept(&listener);
            recovered
                .set_read_timeout(Some(Duration::from_secs(1)))
                .expect("bounded read");
            let next = read_frame(&mut recovered, limits).expect("new bootstrap reached owner");
            let LocalControlRequest::Subscription(request) =
                decode_request(&next, limits).expect("correlated bootstrap frame")
            else {
                panic!("bootstrap subscription");
            };
            let reply = encode_response(
                &LocalControlResponse::Rejected {
                    request_id: request.request_id,
                    message: "recovery endpoint answered".to_owned(),
                },
                limits,
            )
            .expect("bounded reply");
            write_frame(&mut recovered, &reply, limits).expect("answer replacement socket");
        });
        let mut transport = LocalSubscriptionTransport::connect_with_timeouts(
            &endpoint.0,
            Duration::from_secs(1),
            Duration::from_millis(10),
        )
        .expect("initial authenticated connection");
        let first = LocalControlRequest::Subscription(LocalSubscriptionRequest {
            request_id: 1,
            operation: LocalSubscriptionOperation::Open {
                cursor: Box::new([]),
                credit: 1,
                lease_ms: 1_000,
            },
        });
        assert!(
            transport
                .request_with_tick(&first, Instant::now() + Duration::from_millis(80), |_| {
                    LocalControlExchangeDecision::Continue
                })
                .is_err()
        );
        assert!(
            transport.interrupt_handle().is_none(),
            "stall retired the interrupt"
        );

        let project = LocalProjectId::from_path(Path::new("/tmp")).expect("project");
        let mut client = LocalEngineClient::new(&endpoint.0, project);
        client.subscription = Some(transport);
        client.active_cancel = Some(CancellationToken::new());
        let result = client.bootstrap_root(false);
        server.join().expect("bounded recovery owner");
        assert!(
            matches!(result, Err(EngineFault::Failed(error))
            if error.code() == FaultCode::Protocol && error.message().contains("recovery endpoint answered")),
            "the next bootstrap must reach the reauthenticated owner, rather than fail on a missing interrupt"
        );
        assert!(
            client
                .subscription
                .as_ref()
                .expect("replacement installed")
                .interrupt_handle()
                .is_some()
        );
    }

    /// A private owner endpoint that serves each accepted connection with
    /// `serve(index, stream)`, and records how many connections arrived.
    #[cfg(unix)]
    struct FlakyOwner {
        path: PathBuf,
        accepted: Arc<std::sync::atomic::AtomicUsize>,
        done: Option<std::thread::JoinHandle<()>>,
        stop: Arc<std::sync::atomic::AtomicBool>,
    }

    #[cfg(unix)]
    impl FlakyOwner {
        fn start(
            serve: impl Fn(usize, std::os::unix::net::UnixStream) + Send + 'static,
        ) -> Self {
            use std::os::unix::fs::PermissionsExt as _;
            use std::sync::atomic::Ordering;
            let path = PathBuf::from(format!(
                "/tmp/nudox-bootstrap-backoff-{}-{}.sock",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .expect("clock")
                    .as_nanos(),
            ));
            let listener = std::os::unix::net::UnixListener::bind(&path).expect("private listener");
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))
                .expect("private endpoint");
            listener.set_nonblocking(true).expect("bounded accept");
            let accepted = Arc::new(std::sync::atomic::AtomicUsize::new(0));
            let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
            let (count, halt) = (Arc::clone(&accepted), Arc::clone(&stop));
            let done = std::thread::spawn(move || {
                while !halt.load(Ordering::SeqCst) {
                    match listener.accept() {
                        Ok((stream, _)) => {
                            stream.set_nonblocking(false).expect("blocking accepted stream");
                            stream
                                .set_read_timeout(Some(Duration::from_secs(2)))
                                .expect("bounded owner read");
                            let index = count.fetch_add(1, Ordering::SeqCst);
                            serve(index, stream);
                        }
                        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                            std::thread::sleep(Duration::from_millis(2));
                        }
                        Err(error) => panic!("accept authenticated client: {error}"),
                    }
                }
            });
            Self { path, accepted, done: Some(done), stop }
        }

        fn accepted(&self) -> usize {
            self.accepted.load(std::sync::atomic::Ordering::SeqCst)
        }
    }

    #[cfg(unix)]
    impl Drop for FlakyOwner {
        fn drop(&mut self) {
            self.stop.store(true, std::sync::atomic::Ordering::SeqCst);
            if let Some(done) = self.done.take() {
                let _ = done.join();
            }
            let _ = std::fs::remove_file(&self.path);
        }
    }

    /// Reads the Open frame the client sent and drops the stream unanswered:
    /// the client sees the owner close mid-bootstrap.
    #[cfg(unix)]
    fn close_after_open(mut stream: std::os::unix::net::UnixStream) {
        use backend_replication::{LocalControlLimits, read_frame};
        let _ = read_frame(&mut stream, LocalControlLimits::default());
    }

    #[cfg(unix)]
    fn answer_open_with_rejection(mut stream: std::os::unix::net::UnixStream) {
        use backend_replication::{
            LocalControlLimits, LocalControlRequest, LocalControlResponse, decode_request,
            encode_response, read_frame, write_frame,
        };
        let limits = LocalControlLimits::default();
        let frame = read_frame(&mut stream, limits).expect("bootstrap reached the owner");
        let LocalControlRequest::Subscription(request) =
            decode_request(&frame, limits).expect("correlated bootstrap frame")
        else {
            panic!("bootstrap subscription");
        };
        let reply = encode_response(
            &LocalControlResponse::Rejected {
                request_id: request.request_id,
                message: "recovery endpoint answered".to_owned(),
            },
            limits,
        )
        .expect("bounded reply");
        write_frame(&mut stream, &reply, limits).expect("answer the fresh socket");
    }

    #[cfg(unix)]
    #[test]
    fn a_broken_bootstrap_transport_is_retried_on_fresh_sockets_with_bounded_backoff() {
        let owner = FlakyOwner::start(|index, stream| {
            if index < 2 {
                close_after_open(stream);
            } else {
                answer_open_with_rejection(stream);
            }
        });
        let project = LocalProjectId::from_path(Path::new("/tmp")).expect("project");
        let mut client = LocalEngineClient::new(&owner.path, project);
        client.active_cancel = Some(CancellationToken::new());
        let started = std::time::Instant::now();
        let result = client.bootstrap_root(false);
        let elapsed = started.elapsed();
        assert!(
            matches!(&result, Err(EngineFault::Failed(error))
                if error.code() == FaultCode::Protocol && error.message().contains("recovery endpoint answered")),
            "the third socket's answer, not the earlier closes, is the outcome: {result:?}"
        );
        assert_eq!(owner.accepted(), 3, "two retries reached fresh authenticated sockets");
        assert!(
            elapsed >= BOOTSTRAP_RETRY_BACKOFF[0] + BOOTSTRAP_RETRY_BACKOFF[1],
            "the retries wait their backoff: {elapsed:?}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_bootstrap_transport_that_never_recovers_fails_as_transport_after_the_last_backoff() {
        let owner = FlakyOwner::start(|_, stream| close_after_open(stream));
        let project = LocalProjectId::from_path(Path::new("/tmp")).expect("project");
        let mut client = LocalEngineClient::new(&owner.path, project);
        client.active_cancel = Some(CancellationToken::new());
        let started = std::time::Instant::now();
        let result = client.bootstrap_root(false);
        let elapsed = started.elapsed();
        assert!(
            matches!(&result, Err(EngineFault::Failed(error)) if error.code() == FaultCode::Transport),
            "exhausted retries keep the transport cause: {result:?}"
        );
        assert_eq!(owner.accepted(), 1 + BOOTSTRAP_RETRY_BACKOFF.len());
        assert!(elapsed >= BOOTSTRAP_RETRY_BACKOFF.iter().sum::<Duration>(), "{elapsed:?}");
    }

    #[cfg(unix)]
    #[test]
    fn cancellation_ends_the_bootstrap_backoff_at_once_without_another_attempt() {
        let owner = FlakyOwner::start(|_, stream| close_after_open(stream));
        let project = LocalProjectId::from_path(Path::new("/tmp")).expect("project");
        let mut client = LocalEngineClient::new(&owner.path, project);
        let cancel = CancellationToken::new();
        client.active_cancel = Some(cancel.clone());
        let canceller = std::thread::spawn({
            let cancel = cancel.clone();
            move || {
                std::thread::sleep(Duration::from_millis(30));
                cancel.cancel();
            }
        });
        let started = std::time::Instant::now();
        let result = client.bootstrap_root(false);
        canceller.join().expect("canceller");
        assert!(matches!(result, Err(EngineFault::Cancelled)), "{result:?}");
        assert!(
            started.elapsed() < Duration::from_millis(600),
            "cancel ends the backoff instead of running the 850 ms of remaining retries: {:?}",
            started.elapsed()
        );
        assert_eq!(owner.accepted(), 1, "no attempt follows the cancellation");
    }

    #[cfg(unix)]
    #[test]
    fn a_changed_owner_attachment_ends_the_bootstrap_backoff_as_superseded() {
        use super::super::owner::{OwnerGate, OwnerState};
        let owner = FlakyOwner::start(|_, stream| close_after_open(stream));
        let basis = VersionedRoot::unserved();
        let gate = OwnerGate::ready(basis, crate::model::ServiceMode::Attached);
        let project = LocalProjectId::from_path(Path::new("/tmp")).expect("project");
        let mut client = LocalEngineClient::gated(&owner.path, project, gate.clone());
        client.attached_epoch = gate.ready_epoch();
        client.active_cancel = Some(CancellationToken::new());
        let replacement = gate.clone();
        let replacer = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(30));
            replacement.publish(OwnerState::Starting);
        });
        let started = std::time::Instant::now();
        let result = client.bootstrap_root(false);
        replacer.join().expect("replacer");
        assert!(matches!(result, Err(EngineFault::Superseded)), "{result:?}");
        assert!(started.elapsed() < Duration::from_millis(600), "{:?}", started.elapsed());
        assert_eq!(owner.accepted(), 1);
    }

    #[cfg(unix)]
    #[test]
    fn cancelled_publication_revision_retires_both_cached_sockets_before_same_owner_retry() {
        use backend_replication::{LocalControlLimits, read_frame, write_frame};
        use std::io::Read as _;
        use std::os::unix::fs::PermissionsExt as _;
        use std::os::unix::net::{UnixListener, UnixStream};
        use std::time::Instant;

        struct Endpoint(PathBuf);
        impl Drop for Endpoint {
            fn drop(&mut self) {
                let _ = std::fs::remove_file(&self.0);
                let _ = std::fs::remove_dir(self.0.parent().expect("private parent"));
            }
        }
        let directory = std::env::temp_dir().join(format!(
            "ndr-{}-{:x}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock")
                .as_nanos(),
        ));
        std::fs::create_dir(&directory).expect("fresh private endpoint directory");
        std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o700))
            .expect("private directory");
        let endpoint = Endpoint(directory.join("s"));
        let listener = UnixListener::bind(&endpoint.0).expect("replacement listener");
        std::fs::set_permissions(&endpoint.0, std::fs::Permissions::from_mode(0o600))
            .expect("private endpoint");
        listener.set_nonblocking(true).expect("bounded accept");
        let (command, mut command_owner) = UnixStream::pair().expect("command socket pair");
        let (subscription, mut subscription_owner) =
            UnixStream::pair().expect("subscription socket pair");
        for stream in [&command_owner, &subscription_owner] {
            stream.set_read_timeout(Some(Duration::from_secs(2))).expect("bounded owner read");
        }
        let cancel = CancellationToken::new();
        let owner_cancel = cancel.clone();
        let server = std::thread::spawn(move || {
            let limits = LocalControlLimits::default();
            let body = read_frame(&mut command_owner, limits).expect("cached revision reached owner");
            let request = backend_library::decode_command_body(&body).expect("revision request");
            assert!(matches!(request.command, backend_library::Command::Revision));
            // The frame proves the request's real interrupt wake is installed.
            owner_cancel.cancel();
            let mut byte = [0];
            assert_eq!(command_owner.read(&mut byte).expect("interrupted command EOF"), 0);
            assert_eq!(subscription_owner.read(&mut byte).expect("retired subscription EOF"), 0);

            let until = Instant::now() + Duration::from_secs(2);
            let mut fresh = loop {
                match listener.accept() {
                    Ok((stream, _)) => break stream,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        assert!(Instant::now() < until, "same-owner retry never authenticated a fresh stream");
                        std::thread::sleep(Duration::from_millis(2));
                    }
                    Err(error) => panic!("accept fresh stream: {error}"),
                }
            };
            fresh.set_nonblocking(false).expect("blocking accepted stream");
            fresh.set_read_timeout(Some(Duration::from_secs(2))).expect("bounded fresh read");
            let body = read_frame(&mut fresh, limits).expect("fresh revision reached owner");
            let request = backend_library::decode_command_body(&body).expect("fresh revision request");
            assert!(matches!(request.command, backend_library::Command::Revision));
            // A correlated refusal proves fresh physical transport, without
            // inventing a producer certificate or claiming root admission.
            let reply = backend_library::ReplyDto::new(
                request.request_id,
                backend_library::CommandReply::Error("fresh authenticated owner answered".into()),
            );
            write_frame(&mut fresh, &serde_json::to_vec(&reply).expect("reply body"), limits)
                .expect("fresh owner reply");
        });

        let view = super::super::owner::publication_tests::view();
        let cursor = backend_library::Cursor::for_view_root_at(&view, 0);
        let basis = VersionedRoot::from_revision(1, cursor, 0);
        let gate = super::super::owner::OwnerGate::ready(basis, crate::model::ServiceMode::Attached);
        let attachment = gate.ready_epoch().expect("ready attachment");
        assert_eq!(gate.publish_view(attachment, view, cursor), super::super::owner::PublicationAdmission::Admitted);
        let mut client = LocalEngineClient::gated(
            &endpoint.0,
            LocalProjectId::from_path(&directory).expect("project"),
            gate.clone(),
        );
        client.attached_epoch = Some(attachment);
        client.session = Some(Session::from_transport(
            endpoint.0.clone(),
            backend_client::UnixCommandTransport::from_stream(command),
        ));
        client.subscription = Some(LocalSubscriptionTransport::from_stream(subscription));
        assert!(client.execute(&EngineRequest::Root {
            project: None, request: RequestId::new(1), basis, cancel: cancel.clone(),
        }).is_err());
        assert!(cancel.is_cancelled(), "owner cancelled the actual blocked revision");
        assert!(client.active_cancel.is_none());
        assert!(client.session.is_none(), "the interrupted command session cannot be reused");
        assert!(client.subscription.is_none(), "the same request retires its other cached transport");
        assert_eq!(gate.ready_epoch(), Some(attachment), "cancellation did not retire the live owner");

        let fresh = client.execute(&EngineRequest::Root {
            project: None, request: RequestId::new(2), basis, cancel: CancellationToken::new(),
        });
        server.join().expect("bounded physical socket owner");
        assert!(matches!(fresh, Err(EngineFault::Failed(error))
            if error.code() == FaultCode::Protocol && error.message().contains("fresh authenticated owner answered")));
        assert_eq!(gate.ready_epoch(), Some(attachment));
    }

    #[test]
    fn production_observer_has_a_distinct_unopened_session() {
        let original = LocalProjectId::new("/fixture/original-root-context").expect("project");
        let client = LocalEngineClient::new("/unused-no-socket-connect", original);
        assert!(
            client.operation_observer().is_some(),
            "production receipt session"
        );
        assert!(
            client.session.is_none(),
            "factory opens no foreground socket"
        );
    }

    #[test]
    fn a_real_adapter_refuses_missing_and_fixture_lifetimes_before_connection() -> Result<(), Box<dyn std::error::Error>> {
        let project = LocalProjectId::new("/fixture/no-local-owner-lifetime")?;
        let mut client = LocalEngineClient::new("/fixture/no-local-owner.sock", project.clone());
        for owner in [None, super::super::actor::IndexMutationLease::capture(None, None)] {
            let request = EngineRequest::IndexProject {
                owner, project: project.clone(), request: RequestId::new(0x92),
                operation: crate::model::index_operation::tests::claim(&project, 0x92),
                basis: VersionedRoot::unserved(), cancel: CancellationToken::new(),
            };
            assert!(matches!(client.execute(&request), Err(EngineFault::IndexNotSent { error, .. })
                if error.code() == FaultCode::Protocol));
            assert!(client.active_cancel.is_none());
            assert!(client.session.is_none());
        }
        Ok(())
    }

    #[test]
    fn owner_replacement_preserves_a_confirmed_index_and_blocks_an_ambiguous_one() {
        let project = LocalProjectId::new("/tmp/owner-change-project").expect("project");
        let basis = VersionedRoot::unserved();
        let request_id = RequestId::new(17);
        let request = EngineRequest::IndexProject {
            owner: None,
            operation: crate::model::index_operation::tests::claim(&project, 0x51),
            request: request_id, project: project.clone(), basis,
            cancel: CancellationToken::new(),
        };
        let operation = crate::model::index_operation::tests::claim(&project, 0x51);
        let confirmed = EngineDto::IndexOperation {
            request: request_id, basis, project: project.clone(),
            observation: crate::model::index_operation::tests::published(&operation), operation,
        };
        assert!(matches!(
            owner_change_result(&request, Ok(confirmed), true),
            Ok(EngineDto::IndexOperation { project: received, .. }) if received == project
        ));
        let legacy = EngineDto::Index { request: request_id, basis, key: basis, revision: basis.revision(),
            delta: None, project: project.clone(), project_state: None, catalog: None, files_indexed: None };
        assert!(matches!(owner_change_result(&request, Ok(legacy), true),
            Err(EngineFault::IndexUnconfirmed { .. })), "an unkeyed publication cannot settle this operation");
        assert!(matches!(
            owner_change_result(&request, Err(EngineFault::Cancelled), true),
            Err(EngineFault::IndexUnconfirmed { project: received }) if received == project
        ));
        assert!(matches!(
            owner_change_result(&request, Err(EngineFault::IndexCancelled { project: project.clone() }), true),
            Err(EngineFault::IndexCancelled { project: received }) if received == project
        ));
    }

    #[test]
    fn catalog_explore_failure_is_preserved_as_a_typed_fault() {
        let expected = fault(ClientError::CommandFailed(
            backend_library::CommandFailure::NotFound,
        ));
        let observed = LocalEngineClient::catalog_from_reply(Err(expected.clone()))
            .expect_err("Explore failure must not become an absent catalog");
        assert_eq!(observed, expected);
    }

    #[test]
    fn catalog_rejects_a_reply_from_the_wrong_surface_command() {
        let observed = LocalEngineClient::catalog_from_reply(Ok(SurfaceReply::Projects(
            Box::new([]),
        )))
        .expect_err("a non-Explore reply is a protocol fault");
        assert!(matches!(
            observed,
            EngineFault::Failed(error) if error.code() == FaultCode::Protocol
        ));
    }
}
