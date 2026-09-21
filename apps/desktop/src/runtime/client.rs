//! The production engine adapter for the desktop actor.
//!
//! This is the only place where the UI runtime knows that the local-first
//! producer happens to be a `backend_client::Session`.  The actor owns this
//! adapter on its worker thread; the GPUI thread receives only versioned DTOs
//! and never touches a socket, a reply codec, or a registry record.

use super::actor::{EngineClient, EngineDto, EngineFault, EngineRequest, ProjectDto};
use crate::core::{FaultCode, ProjectId, VersionedRoot};
use crate::model::{ObjectId, PackageSummary};
use backend_client::{ClientError, Session};
use backend_library::{RegistryDownloadCount, RowId, SurfaceCommand, SurfaceReply};
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// A worker-owned, reconnecting local service session.
pub struct LocalEngineClient {
    endpoint: PathBuf,
    project_label: Arc<str>,
    session: Option<Session>,
}

impl LocalEngineClient {
    /// Creates a client without opening a socket on the caller's thread.
    #[must_use]
    pub fn new(endpoint: impl AsRef<Path>, project_label: impl Into<Arc<str>>) -> Self {
        Self {
            endpoint: endpoint.as_ref().to_path_buf(),
            project_label: project_label.into(),
            session: None,
        }
    }

    fn session(&mut self) -> Result<&mut Session, EngineFault> {
        if self.session.is_none() {
            self.session = Some(Session::connect(&self.endpoint).map_err(fault)?);
        }
        Ok(self.session.as_mut().expect("session installed"))
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
        let view = self.with_reconnect(|session| session.view())?;
        let key = VersionedRoot::new(view.root(), basis.producer_epoch)
            .with_generation(basis.generation.saturating_add(1))
            .observed_at(basis.observation.saturating_add(1));
        let packages = view
            .rows()
            .iter()
            .filter_map(|row| match row.id {
                RowId::Package(_) => crate::core::PackageId::new(&row.label).ok(),
                RowId::Symbol(_) | RowId::Object(_) => None,
            })
            .collect::<Vec<_>>();
        let Some(one) = std::num::NonZeroU64::new(1) else {
            unreachable!("literal one is non-zero")
        };
        let project = ProjectDto {
            id: ProjectId::from_backend(backend_library::ProjectId::new(one)),
            label: Arc::clone(&self.project_label),
            packages: packages.into(),
        };
        let catalog = self.catalog();
        Ok(EngineDto::Root {
            request: *request_id,
            basis: *basis,
            key,
            delta: None,
            project: Some(project),
            catalog,
        })
    }

    fn catalog(&mut self) -> Option<Arc<[PackageSummary]>> {
        let reply = self
            .with_reconnect(|session| {
                session.surface(SurfaceCommand::Explore {
                    query: None,
                    limit: 64,
                })
            })
            .ok()?;
        let SurfaceReply::Explored(records) = reply else {
            return None;
        };
        Some(
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
        let reply = self.with_reconnect(|session| session.surface(command.clone()))?;
        Ok(EngineDto::Surface {
            request: *request_id,
            basis: *basis,
            command: command.clone(),
            reply,
        })
    }

    fn with_reconnect<T>(
        &mut self,
        operation: impl FnOnce(&mut Session) -> Result<T, ClientError>,
    ) -> Result<T, EngineFault> {
        let result = operation(self.session()?);
        match result {
            Ok(value) => Ok(value),
            Err(ClientError::Disconnected(_)) => {
                let session = self.session()?;
                session.reconnect().map_err(fault)?;
                // The operation is pure/read-only at this boundary. A caller
                // that submitted a mutation gets the original error instead
                // of silently replaying it.
                Err(EngineFault::Failed(crate::core::ErrorValue::new(
                    FaultCode::Transport,
                    "local service connection was renewed; retry the request",
                )))
            }
            Err(error) => Err(fault(error)),
        }
    }
}

impl EngineClient for LocalEngineClient {
    fn execute(&mut self, request: &EngineRequest) -> Result<EngineDto, EngineFault> {
        match request {
            EngineRequest::Root { .. } => self.request_root(request),
            EngineRequest::Surface { .. } => self.request_surface(request),
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
        }
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
            ClientError::Disconnected(_) | ClientError::Io(_) | ClientError::Transport(_) => {
                FaultCode::Transport
            }
            ClientError::CommandFailed(_) => FaultCode::Missing,
            ClientError::BasisMismatch { .. }
            | ClientError::FreshnessMismatch
            | ClientError::RequestMismatch { .. }
            | ClientError::CursorMismatch
            | ClientError::StaleCursor => FaultCode::Cancelled,
        },
        error.to_string(),
    ))
}
