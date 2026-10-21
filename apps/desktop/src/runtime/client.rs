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

// A lease-release transport failure requires a fresh authenticated stream;
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
                // exact exchange diagnostic after the one bounded retry.
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
            // Its command socket may have retired independently of the
            // healthy publication stream. Use ordinary read reconnection
            // before treating this Root as a terminal bootstrap failure.
            let revision = self.with_session_read(Session::revision)?;
            if revision.cursor() == publication.1 {
                return Ok(publication);
            }
        }
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
        let admitted = match result {
            Ok(root) => Ok(root),
            Err(error) if bootstrap_transport_break(&error) => {
                self.subscription = None;
                if self
                    .active_cancel
                    .as_ref()
                    .is_some_and(CancellationToken::is_cancelled)
                {
                    return Err(EngineFault::Cancelled);
                }
                let retry = {
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
                retry.map_err(|error| self.bootstrap_fault(error))
            }
            Err(error) => Err(self.bootstrap_fault(error)),
        }?;
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
        let context = backend_client::RequestContext::new();
        context
            .read_deadline(Duration::from_secs(30))
            .map_err(fault)?;
        let EngineRequest::IndexProject {
            request: request_id,
            project,
            operation,
            basis,
            owner,
            ..
        } = request
        else {
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
        let cancel = self.current_cancel()?;
        let mut session = self.index_session(&context, &cancel)
            .map_err(|error| EngineFault::IndexNotSent { project: project.clone(), error: crate::core::ErrorValue::new(
                FaultCode::Transport, format!("The first-send connection could not be opened: {error}. Nothing was sent.")) })?;
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
            let result = session
                .set_request_context(Some(context.clone()))
                .and_then(|()| {
                    session.start_index_operation(
                        operation.key,
                        operation.package.clone(),
                        operation.execution_intent,
                    )
                });
            let detached = session.set_request_context(None);
            result.and_then(|observation| detached.map(|()| observation))
        };
        let observation = match result {
            Ok(observation) => observation,
            Err(_) => {
                // A missing index receipt is not evidence that the owner died.
                // Publication recovery observes liveness within its own scope.
                return Err(EngineFault::IndexUnconfirmed { project: project.clone() });
            }
        };
        self.session = Some(session);
        self.index_observation(
            *request_id,
            *basis,
            project,
            operation,
            observation,
            &context,
        )
    }

    fn request_index_cancel(&mut self, request: &EngineRequest) -> Result<EngineDto, EngineFault> {
        let EngineRequest::CancelIndexOperation { owner, project, operation, ticket, basis, request: request_id, .. } = request else {
            unreachable!("index cancel adapter called with another request")
        };
        let unconfirmed = || EngineFault::IndexUnconfirmed { project: project.clone() };
        let owner = owner.as_ref().filter(|owner| owner.is_gated()).ok_or_else(unconfirmed)?;
        if !operation.belongs_to(project) || operation.cancellation_ticket().as_ref() != Some(ticket) {
            return Err(unconfirmed());
        }
        let context = backend_client::RequestContext::new();
        context.read_deadline(Duration::from_secs(30)).map_err(|_| unconfirmed())?;
        let before = self.with_index_read(&context, |session| {
            session.set_request_context(Some(context.clone()))?;
            let result = session.index_operation_status(operation.key);
            session.set_request_context(None)?;
            result
        }).map_err(|_| unconfirmed())?;
        if !admit_index_cancel_preflight(operation, ticket, &before).map_err(|_| unconfirmed())? {
            // Publication or another terminal result won the race. Never send
            // Cancel to a finished/replaced job, and retain its exact receipt.
            return self.index_observation(*request_id, *basis, project, operation, before, &context);
        }
        let cancel = self.current_cancel()?;
        let mut session = self.index_session(&context, &cancel).map_err(|_| unconfirmed())?;
        let receipt = {
            let _wake = cancel_wake(&cancel, session.interrupt_handle()).map_err(|_| unconfirmed())?;
            owner.ready_for_send(*basis, &cancel).map_err(|_| unconfirmed())?;
            context.check().map_err(|_| unconfirmed())?;
            // Exactly one mutation send. A transport break never replays Cancel,
            // declares the owner dead, or means the operation was cancelled.
            session.cancel_index_job(ticket.clone()).map_err(|_| unconfirmed())?
        };
        if receipt.ticket != *ticket { return Err(unconfirmed()); }
        self.session = Some(session);
        let after = self.with_index_read(&context, |session| {
            session.set_request_context(Some(context.clone()))?;
            let result = session.index_operation_status(operation.key);
            session.set_request_context(None)?;
            result
        }).map_err(|_| unconfirmed())?;
        if !admit_index_cancel_preflight(operation, ticket, &after).map_err(|_| unconfirmed())? {
            return self.index_observation(*request_id, *basis, project, operation, after, &context);
        }
        if matches!(receipt.status, backend_library::IndexCancelStatus::Unknown) { return Err(unconfirmed()); }
        // Requested is not terminal. Mapping keeps Cancelling while subsequent
        // exact operation observations run; only an owner terminal settles it.
        self.index_observation(*request_id, *basis, project, operation, after, &context)
    }

    fn request_index_status(&mut self, request: &EngineRequest) -> Result<EngineDto, EngineFault> {
        let context = backend_client::RequestContext::new();
        context
            .read_deadline(Duration::from_secs(30))
            .map_err(fault)?;
        let EngineRequest::IndexOperationStatus {
            request: request_id,
            project,
            operation,
            basis,
            ..
        } = request
        else {
            unreachable!("index status adapter called with a non-status request")
        };
        // Status is read-only and may reconnect once. It never calls Start,
        // even when the owner reports Unknown or Unresolved.
        let observation = self
            .with_index_read(&context, |session| {
                session.set_request_context(Some(context.clone()))?;
                let result = session.index_operation_status(operation.key);
                session.set_request_context(None)?;
                result
            })
            .map_err(|_| EngineFault::IndexUnconfirmed {
                project: project.clone(),
            })?;
        self.index_observation(
            *request_id,
            *basis,
            project,
            operation,
            observation,
            &context,
        )
    }

    fn index_observation(
        &mut self,
        request: crate::navigation::RequestId,
        basis: VersionedRoot,
        project: &LocalProjectId,
        operation: &crate::model::IndexOperationClaim,
        observation: backend_library::IndexOperationObservation,
        context: &backend_client::RequestContext,
    ) -> Result<EngineDto, EngineFault> {
        if !operation.belongs_to(project) || !operation.admits_observation(&observation) {
            return Err(EngineFault::IndexUnconfirmed {
                project: project.clone(),
            });
        }
        let observed =
            backend_client::observe_index_operation_progress(observation, context, |command| {
                self.with_index_read(context, |session| {
                    session.set_request_context(Some(context.clone()))?;
                    let result = session.surface(command.clone());
                    session.set_request_context(None)?;
                    result
                })
                .map_err(|error| backend_client::ClientError::Protocol(format!("{error:?}")))
            })
            .map_err(|_| EngineFault::IndexUnconfirmed {
                project: project.clone(),
            })?;
        if !operation.admits_observation(&observed.observation) {
            return Err(EngineFault::IndexUnconfirmed {
                project: project.clone(),
            });
        }
        let mut operation = operation.clone();
        operation.profile_progress = observed.profile_progress;
        Ok(EngineDto::IndexOperation {
            request,
            basis,
            project: project.clone(),
            operation,
            observation: observed.observation,
        })
    }

    fn index_session(
        &self,
        context: &backend_client::RequestContext,
        cancel: &CancellationToken,
    ) -> Result<Session, ClientError> {
        // Dial/authentication precede the socket-specific desktop wake. Link
        // this request before connecting; on_cancel also closes the early race.
        let waiting = context.clone();
        let _wake = cancel.on_cancel(move || {
            waiting.cancel(backend_client::RequestCancellationReason::Explicit);
        });
        let transport = backend_client::UnixCommandTransport::connect_with_context(
            &self.endpoint,
            DESKTOP_CONNECT_TIMEOUT,
            Duration::from_secs(30),
            Some(context),
        )?;
        let mut session = Session::from_transport(self.endpoint.clone(), transport);
        session.set_request_context(Some(context.clone()))?;
        Ok(session)
    }

    /// Index status and progress share their original request lifetime across
    /// connection and retry. The publication observer owns owner-loss recovery;
    /// these reads must not start an independent liveness RPC or recovery wait.
    fn with_index_read<T>(
        &mut self,
        context: &backend_client::RequestContext,
        mut operation: impl FnMut(&mut Session) -> Result<T, ClientError>,
    ) -> Result<T, EngineFault> {
        for attempt in 0..2 {
            let cancel = self.current_cancel()?;
            if cancel.is_cancelled() {
                self.session = None;
                return Err(EngineFault::Cancelled);
            }
            if let Err(error) = context.check() {
                self.session = None;
                return Err(fault(error));
            }
            if self.session.is_none() {
                self.session = Some(self.index_session(context, &cancel).map_err(fault)?);
            }
            let _wake = cancel_wake(
                &cancel,
                self.session.as_ref().expect("session installed").interrupt_handle(),
            )?;
            if cancel.is_cancelled() {
                self.session = None;
                return Err(EngineFault::Cancelled);
            }
            let result = operation(self.session.as_mut().expect("session installed"));
            if cancel.is_cancelled() {
                self.session = None;
                return Err(EngineFault::Cancelled);
            }
            match result {
                Ok(value) => return Ok(value),
                Err(error) => {
                    if let Err(stopped) = context.check() {
                        self.session = None;
                        return Err(fault(stopped));
                    }
                    if transport_break(&error) {
                        self.session = None;
                        if attempt == 0 {
                            if self.gate.as_ref().is_some_and(|gate| {
                                gate.ready_epoch() != self.attached_epoch
                            }) {
                                return Err(EngineFault::Superseded);
                            }
                            continue;
                        }
                    }
                    return Err(fault(error));
                }
            }
        }
        unreachable!("one index read attempt and one retry return above")
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

/// Re-read the exact durable payload and owner ticket before cancellation.
/// A terminal receipt wins; unknown, accepted-only or replaced tickets refuse.
fn admit_index_cancel_preflight(
    operation: &crate::model::IndexOperationClaim,
    ticket: &backend_library::IndexJobTicket,
    observation: &backend_library::IndexOperationObservation,
) -> Result<bool, ClientError> {
    use backend_library::{IndexOperationObservation, IndexOperationState};
    if !operation.admits_observation(observation) {
        return Err(ClientError::Protocol("cancel observation changed its exact operation".into()));
    }
    match observation {
        IndexOperationObservation::Known(status) => match &status.state {
            IndexOperationState::Active { ticket: active, .. } if active == ticket => Ok(true),
            IndexOperationState::Published(_) | IndexOperationState::PartiallyPublished { .. }
                | IndexOperationState::Failed { .. } => Ok(false),
            _ => Err(ClientError::Protocol("cancel observation has no matching current ticket".into())),
        },
        _ => Err(ClientError::Protocol("cancel observation has no exact active job".into())),
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
            EngineRequest::CancelIndexOperation { owner, project, basis, .. } => {
                let Some(owner) = owner else {
                    self.active_cancel = None;
                    return Err(EngineFault::IndexUnconfirmed { project: project.clone() });
                };
                let lifetime = owner.bind_cancellation(request.cancellation());
                if owner.ready_for_send(*basis, request.cancellation()).is_err() {
                    self.active_cancel = None;
                    return Err(EngineFault::IndexUnconfirmed { project: project.clone() });
                }
                lifetime
            }
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
                    EngineRequest::IndexOperationStatus { project, .. }
                    | EngineRequest::CancelIndexOperation { project, .. } => EngineFault::IndexUnconfirmed { project: project.clone() },
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
            EngineRequest::CancelIndexOperation { .. } => self.request_index_cancel(request),
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
            (EngineRequest::IndexProject { project, .. } | EngineRequest::IndexOperationStatus { project, .. }
                | EngineRequest::CancelIndexOperation { project, .. },
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
            EngineRequest::IndexProject { .. } | EngineRequest::IndexOperationStatus { .. }
                | EngineRequest::CancelIndexOperation { .. },
            Ok(dto @ EngineDto::IndexOperation { .. }),
        ) if matches!(&dto, EngineDto::IndexOperation { observation: backend_library::IndexOperationObservation::Known(status), .. }
                    if matches!(status.state, backend_library::IndexOperationState::Published(_) | backend_library::IndexOperationState::PartiallyPublished { .. } | backend_library::IndexOperationState::Failed { .. })) =>
        {
            Ok(dto)
        }
        (EngineRequest::IndexOperationStatus { project, .. } | EngineRequest::CancelIndexOperation { project, .. }, _) => {
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
        ClientError::OwnerUpdate { found, supported, ref detail } => {
            return EngineFault::Failed(crate::core::ErrorValue::owner_update(found, supported, detail));
        }
        ClientError::Protocol(_) | ClientError::IncoherentView => FaultCode::Protocol,
        ClientError::Startup(_)
        | ClientError::Disconnected(_)
        | ClientError::QueryPreparationDisconnected { .. }
        | ClientError::MutationOutcomeUnknown(_)
        | ClientError::IndexOperationDeliveryUnknown { .. }
        | ClientError::Io(_)
        | ClientError::Transport(_)
        | ClientError::ResponseTooLarge { .. }
        | ClientError::ResponseFrameTooLarge { .. }
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

#[cfg(all(test, unix))]
#[path = "client/publication_revision_tests.rs"]
mod publication_revision_tests;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn owner_update_is_manual_action_without_transport_or_cancellation_reclassification() {
        let update = ClientError::OwnerUpdate {
            found: 36,
            supported: 37,
            detail: "custody could not be proven".into(),
        };
        let EngineFault::Failed(error) = fault(update) else { panic!("manual update is a visible failure"); };
        assert_eq!(error.code(), FaultCode::OwnerUpdate);
        assert!(error.message().contains("36 → 37"));
        assert!(error.message().contains("Restart the previous NuDox owner, then retry."));
        assert!(error.message().contains("custody could not be proven"));
        assert!(matches!(fault(ClientError::RequestCancelled(backend_client::RequestCancellationReason::Explicit)), EngineFault::Cancelled));
        assert!(matches!(fault(ClientError::Protocol("bad frame".into())), EngineFault::Failed(error) if error.code() == FaultCode::Protocol));
        assert!(matches!(fault(ClientError::Io("unreachable".into())), EngineFault::Failed(error) if error.code() == FaultCode::Transport));
    }

    use crate::navigation::RequestId;

    #[cfg(unix)]
    #[test]
    fn index_reads_keep_expiry_cancellation_and_owner_fences_without_a_liveness_probe() {
        use std::os::unix::fs::PermissionsExt as _;
        use std::os::unix::net::{UnixListener, UnixStream};
        use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
        use std::time::Instant;

        for case in ["expired", "cancelled", "suspended", "healthy", "protocol", "owner_update"] {
            let directory = std::env::temp_dir().join(format!(
                "ndi-{}-{:x}", std::process::id(),
                std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)
                    .expect("clock").as_nanos(),
            ));
            std::fs::create_dir(&directory).expect("fresh private endpoint directory");
            std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o700))
                .expect("private directory");
            let endpoint = directory.join("s");
            let listener = UnixListener::bind(&endpoint).expect("probe detector");
            std::fs::set_permissions(&endpoint, std::fs::Permissions::from_mode(0o600))
                .expect("private endpoint");
            listener.set_nonblocking(true).expect("bounded accept");
            let connections = Arc::new(AtomicUsize::new(0));
            let finished = Arc::new(AtomicBool::new(false));
            let observed_connections = Arc::clone(&connections);
            let observer_finished = Arc::clone(&finished);
            let detector = std::thread::spawn(move || {
                let until = Instant::now() + Duration::from_secs(2);
                while Instant::now() < until {
                    match listener.accept() {
                        Ok((stream, _)) => {
                            observed_connections.fetch_add(1, Ordering::AcqRel);
                            // A regression's liveness RPC fails promptly rather
                            // than leaving this test in its independent I/O wait.
                            drop(stream);
                        }
                        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                            if observer_finished.load(Ordering::Acquire) { break; }
                            std::thread::sleep(Duration::from_millis(1));
                        }
                        Err(error) => panic!("probe detector: {error}"),
                    }
                }
            });
            let view = super::super::owner::publication_tests::view();
            let cursor = backend_library::Cursor::for_view_root_at(&view, 0);
            let basis = VersionedRoot::from_revision(1, cursor, 0);
            let gate = super::super::owner::OwnerGate::ready(basis, crate::model::ServiceMode::Attached);
            let attachment = gate.ready_epoch().expect("ready attachment");
            let mut client = LocalEngineClient::gated(
                &endpoint, LocalProjectId::from_path(&directory).expect("project"), gate.clone(),
            );
            let (command, _owner) = UnixStream::pair().expect("cached command socket");
            client.session = Some(Session::from_transport(
                endpoint.clone(), backend_client::UnixCommandTransport::from_stream(command),
            ));
            client.active_cancel = Some(CancellationToken::new());
            client.attached_epoch = Some(attachment);
            let context = backend_client::RequestContext::new();
            let deadline = context.read_deadline(if case == "expired" { Duration::from_millis(50) } else { Duration::from_secs(2) }).expect("original budget");
            let mut attempts = 0;
            let result = client.with_index_read(&context, |_| {
                attempts += 1;
                match case {
                    "expired" => {
                        std::thread::sleep(deadline.saturating_duration_since(Instant::now())
                            + Duration::from_millis(1));
                        Err(ClientError::RemoteDeadlineExceeded)
                    }
                    "cancelled" => {
                        context.cancel(backend_client::RequestCancellationReason::Explicit);
                        Err(ClientError::Disconnected(std::io::ErrorKind::ConnectionReset))
                    }
                    "suspended" => {
                        gate.suspend_observation(attachment).expect("rotate attachment");
                        Err(ClientError::Disconnected(std::io::ErrorKind::ConnectionReset))
                    }
                    "healthy" => Ok(7),
                    "protocol" => Err(ClientError::Protocol("correlated refusal".into())),
                    "owner_update" => Err(ClientError::OwnerUpdate {
                        found: 36,
                        supported: 37,
                        detail: "manual restart required".into(),
                    }),
                    _ => unreachable!(),
                }
            });
            finished.store(true, Ordering::Release);
            detector.join().expect("bounded probe detector");
            assert_eq!(connections.load(Ordering::Acquire), 0, "{case}: no extra liveness socket");
            assert_eq!(attempts, 1, "{case}: no retry after stop or authority change");
            match case {
                "expired" => assert!(matches!(result, Err(EngineFault::Failed(ref error))
                    if error.code() == FaultCode::Transport)),
                "cancelled" => assert!(matches!(result, Err(EngineFault::Cancelled))),
                "suspended" => assert!(matches!(result, Err(EngineFault::Superseded))),
                "healthy" => assert!(matches!(result, Ok(7))),
                "owner_update" => assert!(matches!(result, Err(EngineFault::Failed(ref error))
                    if error.code() == FaultCode::OwnerUpdate
                        && error.message().contains("36 → 37")
                        && error.message().contains("Restart the previous NuDox owner, then retry.")
                        && error.message().contains("manual restart required"))),
                "protocol" => assert!(matches!(result, Err(EngineFault::Failed(ref error))
                    if error.code() == FaultCode::Protocol && error.message() == "protocol: correlated refusal")),
                _ => unreachable!(),
            }
            if case != "suspended" {
                assert_eq!(gate.ready_epoch(), Some(attachment), "{case}: read failure cannot declare owner dead");
            }
            assert_eq!(client.session.is_some(), matches!(case, "healthy" | "protocol" | "owner_update"));
            drop(client);
            std::fs::remove_file(endpoint).expect("remove endpoint");
            std::fs::remove_dir(directory).expect("remove private directory");
        }
    }

    #[cfg(unix)]
    #[test]
    fn index_read_retires_the_cached_session_when_cancelled_during_wake_registration() {
        use std::os::unix::net::UnixStream;

        struct CancelAtWake {
            transport: backend_client::UnixCommandTransport,
            cancel: CancellationToken,
            cancel_at_lookup: bool,
        }
        impl backend_client::CommandTransport for CancelAtWake {
            fn request(&mut self, _: backend_library::CommandDto)
                -> Result<backend_library::ReplyDto, ClientError>
            {
                Err(ClientError::Protocol("no fixture command is admitted".into()))
            }
            fn interrupt_handle(&self) -> Option<TransportInterrupt> {
                if self.cancel_at_lookup { self.cancel.cancel(); }
                backend_client::CommandTransport::interrupt_handle(&self.transport)
            }
        }
        for cancel_at_lookup in [true, false] {
            let (stream, _owner) = UnixStream::pair().expect("cached owned socket");
            let transport = backend_client::UnixCommandTransport::from_stream(stream);
            assert!(
                backend_client::CommandTransport::interrupt_handle(&transport).is_some(),
                "real cached transport owns its public interrupt handle"
            );
            let cancel = CancellationToken::new();
            let mut client = LocalEngineClient::new(
                "unused-index-wake-endpoint", LocalProjectId::new("index-wake").expect("project"),
            );
            client.active_cancel = Some(cancel.clone());
            client.session = Some(Session::from_transport("unused-index-wake-endpoint", CancelAtWake {
                transport, cancel, cancel_at_lookup,
            }));
            let context = backend_client::RequestContext::new();
            context.read_deadline(Duration::from_secs(2)).expect("original scope");
            let mut invoked = false;
            let result = client.with_index_read(&context, |_| {
                invoked = true;
                Ok(7)
            });
            if cancel_at_lookup {
                assert!(matches!(result, Err(EngineFault::Cancelled)));
                assert!(!invoked, "no callback after the immediate cancellation wake");
                assert!(client.session.is_none(), "interrupted cached transport is retired");
            } else {
                assert!(matches!(result, Ok(7)));
                assert!(invoked);
                assert!(client.session.is_some(), "healthy cached transport is retained");
            }
        }
    }

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
