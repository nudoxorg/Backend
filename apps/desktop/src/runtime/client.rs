//! The production engine adapter for the desktop actor.
//!
//! This is the only place where the UI runtime knows that the local-first
//! producer happens to be a `backend_client::Session`.  The actor owns this
//! adapter on its worker thread; the GPUI thread receives only versioned DTOs
//! and never touches a socket, a reply codec, or a registry record.

use super::actor::{CancellationToken, EngineClient, EngineDto, EngineFault, EngineRequest, ProjectDto};
use super::liveness::{report_if_dead, transport_break};
use super::owner::OwnerFault;
use crate::core::{FaultCode, LocalProjectId, VersionedRoot};
use crate::model::{ObjectId, PackageSummary};
use backend_client::{ClientError, LocalSubscriptionTransport, Session};
use backend_library::{RegistryDownloadCount, RowId, SurfaceCommand, SurfaceReply};
use std::path::{Path, PathBuf};
use std::sync::Arc;

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

impl LocalEngineClient {
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
            self.session = Some(Session::connect(&self.endpoint).map_err(|error| self.client_fault(error))?);
        }
        Ok(self.session.as_mut().expect("session installed"))
    }

    fn subscription(&mut self) -> Result<&mut LocalSubscriptionTransport, EngineFault> {
        if self.subscription.is_none() {
            self.subscription =
                Some(LocalSubscriptionTransport::connect(&self.endpoint).map_err(|error| self.client_fault(error))?);
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
    ) -> Result<(backend_library::ViewRoot, backend_library::Cursor), EngineFault> {
        match self.subscription()?.bootstrap_root() {
            Ok(root) => Ok(root),
            Err(error) if transport_break(&error) => {
                self.subscription = None;
                if self.active_cancel.as_ref().is_some_and(CancellationToken::is_cancelled) {
                    return Err(EngineFault::Cancelled);
                }
                let retry = self.subscription()?.bootstrap_root();
                retry.map_err(|error| self.client_fault(error))
            }
            Err(error) => Err(self.client_fault(error)),
        }
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
        let (view, revision) = self.bootstrap_root()?;
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
        let project = Some(ProjectDto {
            id: self.project.clone(),
            label: Arc::from(self.project.as_str()),
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
        let EngineRequest::IndexProject {
            request: request_id,
            project,
            basis,
            ..
        } = request
        else {
            unreachable!("index adapter called with a non-index request")
        };
        let mut session = Session::connect(&self.endpoint)
            .map_err(|error| self.client_fault(error))
            .map_err(|error| index_fault(project.clone(), error))?;
        let coordinate =
            project
                .service_coordinate()
                .map_err(|error| EngineFault::IndexFailed {
                    project: project.clone(),
                    error: crate::core::ErrorValue::new(FaultCode::Protocol, error.to_string()),
                })?;
        session
            .index(coordinate)
            .map_err(|error| self.client_fault(error))
            .map_err(|error| index_fault(project.clone(), error))?;
        self.session = Some(session);
        let (view, revision) = self
            .bootstrap_root()
            .map_err(|error| index_fault(project.clone(), error))?;
        if revision.root() != view.root() {
            return Err(EngineFault::IndexFailed {
                project: project.clone(),
                error: crate::core::ErrorValue::new(
                    FaultCode::Protocol,
                    "the local service returned mismatched index and health revisions",
                ),
            });
        }
        // The owner has now admitted the selected project and published its
        // post-index revision. Switch subsequent root/read projections to
        // this local workspace only at that authoritative boundary; a failed
        // or cancelled attempt leaves the last served workspace untouched.
        self.project = project.clone();
        let key =
            VersionedRoot::from_revision(basis.producer_epoch(), revision, basis.observation());
        let project_state = Some(project_dto(project.clone(), project_label(project), &view));
        let catalog = Some(self.catalog()?);
        Ok(EngineDto::Index {
            request: *request_id,
            basis: *basis,
            key,
            revision,
            delta: None,
            project: project.clone(),
            project_state,
            catalog,
            // The current service health report is owner-global. Do not lower
            // it into a per-project count until the owner publishes one.
            files_indexed: None,
        })
    }

    /// A read may be retried once after a transport break, using a fresh
    /// session. The owner generation is checked again before that retry.
    fn with_session_read<T>(
        &mut self,
        mut operation: impl FnMut(&mut Session) -> Result<T, ClientError>,
    ) -> Result<T, EngineFault> {
        for attempt in 0..2 {
            if self.active_cancel.as_ref().is_some_and(CancellationToken::is_cancelled) {
                return Err(EngineFault::Cancelled);
            }
            let result = operation(self.session()?);
            match result {
                Ok(value) => return Ok(value),
                Err(error) if attempt == 0 && transport_break(&error) => {
                    self.session = None;
                    let _ = self.client_fault(error);
                    if let Some(gate) = &self.gate {
                        gate.wait_cancelled(self.active_cancel.as_ref().expect("read has a cancellation token"))
                            .map_err(owner_fault)?;
                        if gate.attached_ready_epoch() != self.attached_epoch {
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
        let result = operation(self.session()?);
        match result {
            Ok(value) => Ok(value),
            Err(error) if transport_break(&error) => {
                self.session = None;
                Err(self.client_fault(error))
            }
            Err(error) => Err(self.client_fault(error)),
        }
    }
}

impl EngineClient for LocalEngineClient {
    fn execute(&mut self, request: &EngineRequest) -> Result<EngineDto, EngineFault> {
        self.active_cancel = Some(request.cancellation().clone());
        if let Some(gate) = &self.gate {
            gate.wait_cancelled(request.cancellation()).map_err(owner_fault)?;
            // Superseded while it waited (the startup root read, once the
            // owner's own root arrived): not run.
            if request.cancelled() {
                return Err(EngineFault::Cancelled);
            }
            // An attached owner may restart under the same root. Discard
            // sockets admitted by its prior serving generation before this
            // request can use them.
            let epoch = gate.attached_ready_epoch();
            if self.attached_epoch != epoch {
                self.session = None;
                self.subscription = None;
            }
            self.attached_epoch = epoch;
        }
        let result = match request {
            EngineRequest::Root { .. } => self.request_root(request),
            EngineRequest::Surface { .. } => self.request_surface(request),
            EngineRequest::IndexProject { .. } => self.request_index(request),
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
        if self.attached_epoch.is_some()
            && self.gate.as_ref().and_then(super::owner::OwnerGate::attached_ready_epoch) != self.attached_epoch
        {
            Err(EngineFault::Superseded)
        } else {
            result
        }
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

fn index_fault(project: LocalProjectId, fault: EngineFault) -> EngineFault {
    match fault {
        EngineFault::Failed(error) => EngineFault::IndexFailed { project, error },
        other => other,
    }
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
        ecosystem: Arc::from(format!("{:?}", record.ecosystem)),
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
