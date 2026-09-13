//! Platform host for the one local product owner.

use backend_local_service::{EmbeddedLocalService, ProcessConfig, ProcessError};
use backend_runtime::{RuntimeError, WorkspacePaths};
use std::fmt;
use std::path::Path;

/// How this GUI reached the local service.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HostMode {
    /// This process owns the service and its durable workspace lease.
    Embedded,
    /// A service already owned the selected workspace, so this GUI attached.
    Attached,
}

/// Desktop lifecycle root for the local-first product.
pub struct DesktopHost {
    paths: WorkspacePaths,
    embedded: Option<EmbeddedLocalService>,
}

impl DesktopHost {
    /// Discovers the current project and becomes its owner when no owner is live.
    ///
    /// # Errors
    /// Returns an error when paths, credentials, composition, or binding fail.
    pub fn start() -> Result<Self, HostError> {
        let paths = WorkspacePaths::discover(None, None, None).map_err(HostError::Runtime)?;
        Self::start_with_paths(paths)
    }

    /// Starts from explicit host-selected paths.
    ///
    /// Platform packages use this seam when their sandbox or application data
    /// root differs from the shell environment. The service and wire behavior
    /// remain identical after path selection.
    ///
    /// # Errors
    /// Returns an error when credentials, composition, or binding fail.
    pub fn start_with_paths(paths: WorkspacePaths) -> Result<Self, HostError> {
        paths.initialize().map_err(HostError::Runtime)?;
        if std::os::unix::net::UnixStream::connect(paths.endpoint()).is_ok() {
            return Ok(Self {
                paths,
                embedded: None,
            });
        }
        let args = [
            "--endpoint".to_owned(),
            paths.endpoint().to_string_lossy().into_owned(),
            "--workspace".to_owned(),
            paths.data().to_string_lossy().into_owned(),
            "--authority-secret-file".to_owned(),
            paths.authority_secret().to_string_lossy().into_owned(),
            "--profile".to_owned(),
            "builtin".to_owned(),
        ];
        let config = ProcessConfig::parse(args).map_err(HostError::Service)?;
        let embedded = EmbeddedLocalService::start(config).map_err(HostError::Service)?;
        Ok(Self {
            paths,
            embedded: Some(embedded),
        })
    }

    /// Returns the endpoint used by GUI, CLI, and MCP.
    #[must_use]
    pub fn endpoint(&self) -> &Path {
        self.paths.endpoint()
    }

    /// Returns the project selected by zero-configuration discovery.
    #[must_use]
    pub fn project(&self) -> &Path {
        self.paths.project()
    }

    /// Returns whether this process owns or attached to the service.
    #[must_use]
    pub const fn mode(&self) -> HostMode {
        if self.embedded.is_some() {
            HostMode::Embedded
        } else {
            HostMode::Attached
        }
    }
}

/// Failure to compose the native application's local service root.
#[derive(Debug)]
pub enum HostError {
    /// Project discovery or private credential setup failed.
    Runtime(RuntimeError),
    /// The compiled service could not start.
    Service(ProcessError),
}

impl fmt::Display for HostError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Runtime(error) => error.fmt(formatter),
            Self::Service(error) => error.fmt(formatter),
        }
    }
}

impl std::error::Error for HostError {}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use super::*;
    use backend_client::Session;
    use std::fs;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn one_gui_embeds_and_every_other_surface_attaches_to_that_owner() {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let project = std::env::temp_dir().join(format!(
            "backend-desktop-host-{}-{nonce}",
            std::process::id()
        ));
        let data = project.join("state");
        let endpoint = std::env::temp_dir().join(format!(
            "backend-desktop-host-{}-{nonce}.sock",
            std::process::id()
        ));
        fs::create_dir_all(project.join("src")).expect("create project");
        fs::write(
            project.join("Cargo.toml"),
            b"[package]\nname='embedded-proof'\nversion='0.1.0'\nedition='2024'\n",
        )
        .expect("write manifest");
        fs::write(project.join("src/lib.rs"), b"pub struct EmbeddedProof;\n")
            .expect("write source");
        let paths =
            WorkspacePaths::discover(Some(project.clone()), Some(data), Some(endpoint.clone()))
                .expect("explicit paths");

        let owner = DesktopHost::start_with_paths(paths.clone()).expect("embedded owner");
        assert_eq!(owner.mode(), HostMode::Embedded);
        let attached = DesktopHost::start_with_paths(paths).expect("attached surface");
        assert_eq!(attached.mode(), HostMode::Attached);

        let mut session = Session::connect(&endpoint).expect("shared client session");
        session
            .index(&project.to_string_lossy())
            .expect("index through embedded owner");
        let revision = session.revision().expect("read embedded revision");
        assert_ne!(
            revision.root,
            backend_library::view_state_root(&[]),
            "indexing must advance the visible revision"
        );
        let readiness = session.health().expect("read bounded owner health");
        assert_ne!(
            readiness.revision().root(),
            backend_library::view_state_root(&[]),
            "bounded health must report an indexed revision"
        );

        drop(attached);
        drop(session);
        drop(owner);
        fs::remove_dir_all(project).expect("remove project");
    }
}
