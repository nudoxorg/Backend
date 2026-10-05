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
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

const DESKTOP_CONNECT_TIMEOUT: Duration = Duration::from_secs(1);

/// Exact published receipts choose the project context shared by the root and
/// status sessions. This small context carries no root or producer authority.
#[derive(Clone, Default)]
pub struct PublishedProjectContext(Arc<Mutex<Option<LocalProjectId>>>);
impl PublishedProjectContext {
    pub(crate) fn project(&self, fallback: &LocalProjectId) -> LocalProjectId {
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .as_ref()
            .unwrap_or(fallback)
            .clone()
    }
    pub(crate) fn record(&self, project: &LocalProjectId) {
        *self.0.lock().unwrap_or_else(PoisonError::into_inner) = Some(project.clone());
    }
}

/// A worker-owned local service session with typed read replay.
pub struct LocalEngineClient {
    endpoint: PathBuf,
    project: LocalProjectId,
    published_project: PublishedProjectContext,
    session: Option<Session>,
    subscription: Option<LocalSubscriptionTransport>,
    /// Attached generation admitted for this actor request and its sockets.
    attached_epoch: Option<super::owner::Epoch>,
    active_cancel: Option<CancellationToken>,
    /// The owner this client waits for, on the actor thread (I1).
    gate: Option<super::owner::OwnerGate>,
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
            published_project: PublishedProjectContext::default(),
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

    pub(crate) fn with_published_project_context(
        mut self,
        context: PublishedProjectContext,
    ) -> Self {
        self.published_project = context;
        self
    }

    fn session(&mut self) -> Result<&mut Session, EngineFault> {
        if self.session.is_none() {
            self.session = Some(Session::connect_with_timeouts(&self.endpoint, DESKTOP_CONNECT_TIMEOUT, Duration::from_secs(30))
                .map_err(|error| self.client_fault(error))?);
        }
        Ok(self.session.as_mut().expect("session installed"))
    }

    fn subscription(&mut self) -> Result<&mut LocalSubscriptionTransport, EngineFault> {
        if self.subscription.is_none() {
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
        let result = {
            let subscription = self.subscription()?;
            let _wake = cancel_wake(&cancel, subscription.interrupt_handle())?;
            if cancel.is_cancelled() {
                return Err(EngineFault::Cancelled);
            }
            subscription.bootstrap_root()
        };
        if cancel.is_cancelled() {
            self.subscription = None;
            return Err(EngineFault::Cancelled);
        }
        let admitted = match result {
            Ok(root) => Ok(root),
            Err(error) if transport_break(&error) => {
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
                    subscription.bootstrap_root()
                };
                if cancel.is_cancelled() {
                    self.subscription = None;
                    return Err(EngineFault::Cancelled);
                }
                retry.map_err(|error| self.client_fault(error))
            }
            Err(error) => Err(self.client_fault(error)),
        }?;
        let (view, cursor) = admitted;
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
            ..
        } = request
        else {
            unreachable!("root adapter called with a non-root request")
        };
        let (view, revision) = self.bootstrap_root(true)?;
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
        let context = self.published_project.project(&self.project);
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
    fn published_project_context(&self) -> Option<PublishedProjectContext> {
        Some(self.published_project.clone())
    }

    fn operation_observer(&self) -> Option<Box<dyn EngineClient>> {
        let mut observer = Self::new(&self.endpoint, self.project.clone())
            .with_published_project_context(self.published_project.clone());
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
            (EngineRequest::IndexProject { .. }, Err(error @ EngineFault::IndexNotSent { .. })) => Err(error),
            (EngineRequest::IndexProject { .. } | EngineRequest::IndexOperationStatus { .. }, Ok(dto @ EngineDto::IndexOperation { .. }))
                if matches!(&dto, EngineDto::IndexOperation { observation: backend_library::IndexOperationObservation::Known(status), .. }
                    if matches!(status.state, backend_library::IndexOperationState::Published(_) | backend_library::IndexOperationState::Failed { .. })) => Ok(dto),
            (EngineRequest::IndexOperationStatus { project, .. }, _) =>
                Err(EngineFault::IndexUnconfirmed { project: project.clone() }),
            (EngineRequest::IndexProject { .. }, Err(EngineFault::IndexCancelled { project })) =>
                Err(EngineFault::IndexCancelled { project }),
            (EngineRequest::IndexProject { project, .. }, _) =>
                Err(EngineFault::IndexUnconfirmed { project: project.clone() }),
            (EngineRequest::Surface { command, .. }, Ok(dto)) if !super::reads::read_only(command) => Ok(dto),
            (EngineRequest::Surface { command, .. }, _) if !super::reads::read_only(command) =>
                Err(EngineFault::MutationUnconfirmed),
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
    EngineFault::Failed(crate::core::ErrorValue::new(
        match error {
            ClientError::Protocol(_) | ClientError::IncoherentView => FaultCode::Protocol,
            ClientError::Disconnected(_) | ClientError::Io(_) | ClientError::Transport(_) | ClientError::RemoteDeadlineExceeded => {
                FaultCode::Transport
            }
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
        },
        error.to_string(),
    ))
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
    fn production_observer_has_a_distinct_session_and_shared_admitted_project_context() {
        let original = LocalProjectId::new("/fixture/original-root-context").expect("project");
        let indexed = LocalProjectId::new("/fixture/new-root-context").expect("project");
        let client = LocalEngineClient::new("/unused-no-socket-connect", original.clone());
        let observer = client
            .operation_observer()
            .expect("production receipt session");
        let root_context = client.published_project_context().expect("root context");
        let status_context = observer
            .published_project_context()
            .expect("observer context");
        assert!(Arc::ptr_eq(&root_context.0, &status_context.0));
        assert!(
            client.session.is_none(),
            "factory creates no foreground socket"
        );
        root_context.record(&indexed); // the runtime admission boundary owns this update
        assert_eq!(client.published_project.project(&original), indexed);
        assert_eq!(status_context.project(&original), indexed);
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
