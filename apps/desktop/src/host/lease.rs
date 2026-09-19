//! Platform host: become the local owner, or attach to the one that is live.
//! Ownership is a file lock on the workspace; the endpoint is a separate socket.
//! Those two facts can disagree for a moment, and this module is where they meet.
//!
//! The rule is that an owner already holding the workspace lock always wins.
//! A failed connect is not proof that no owner exists — the socket may have been
//! swept out of `/tmp`, or the owner may hold the lock and not yet have bound
//! its listener — so a refusal to embed is answered by waiting for that owner's
//! endpoint rather than by reporting a failure the reader cannot act on.

use backend_local_service::{EmbeddedLocalService, ProcessConfig, ProcessError};
use backend_runtime::{RuntimeError, WorkspacePaths};
use std::fmt;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// How many times an attach is retried after the workspace lock is contended.
const ATTACH_ATTEMPTS: u32 = 40;

/// How long to wait between attach attempts.
const ATTACH_DELAY: Duration = Duration::from_millis(50);

/// How long a liveness probe waits for an owner to complete its handshake.
///
/// Short on purpose. This is not a request; it is the question "is anyone
/// actually serving this path?", and an owner that cannot answer it inside a
/// second is not one this process should hand its startup to.
const PROBE_TIMEOUT: Duration = Duration::from_millis(750);

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
        let paths = super::paths::discover().map_err(HostError::Runtime)?;
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
        if Self::endpoint_is_live(&paths) {
            return Ok(Self {
                paths,
                embedded: None,
            });
        }
        let config = ProcessConfig::parse(Self::owner_arguments(&paths)).map_err(HostError::Service)?;
        match EmbeddedLocalService::start(config) {
            Ok(embedded) => Ok(Self {
                paths,
                embedded: Some(embedded),
            }),
            Err(refusal) => Self::attach_to_contended_owner(paths, refusal),
        }
    }

    fn owner_arguments(paths: &WorkspacePaths) -> [String; 8] {
        [
            "--endpoint".to_owned(),
            paths.endpoint().to_string_lossy().into_owned(),
            "--workspace".to_owned(),
            paths.data().to_string_lossy().into_owned(),
            "--authority-secret-file".to_owned(),
            paths.authority_secret().to_string_lossy().into_owned(),
            "--profile".to_owned(),
            "builtin".to_owned(),
        ]
    }

    /// Returns whether a live owner is answering on the workspace endpoint.
    ///
    /// A successful `connect` is not proof. A socket file left behind by a
    /// dead owner still accepts connections on macOS, and a process that took
    /// that as proof would skip embedding, connect to nothing, and then sit in
    /// a thirty-second read for every bootstrap attempt — minutes of a window
    /// that never opens and never says why. So liveness is proven the only way
    /// it can be: by completing the peer handshake, under a short bound.
    fn endpoint_is_live(paths: &WorkspacePaths) -> bool {
        let endpoint = paths.endpoint();
        let Ok(stream) = std::os::unix::net::UnixStream::connect(endpoint) else {
            return false;
        };
        if stream
            .set_read_timeout(Some(PROBE_TIMEOUT))
            .and_then(|()| stream.set_write_timeout(Some(PROBE_TIMEOUT)))
            .is_err()
        {
            return false;
        }
        backend_replication::AuthenticatedLocalPeer::authenticate(&stream, endpoint).is_ok()
    }

    /// Waits for the owner that refused this process to publish its endpoint.
    ///
    /// Embedding fails when another live process already holds the workspace
    /// lock. That process is the authority, so the only correct answer is to
    /// attach to it; the refusal itself is only reported if it never binds.
    fn attach_to_contended_owner(
        paths: WorkspacePaths,
        refusal: ProcessError,
    ) -> Result<Self, HostError> {
        for _ in 0..ATTACH_ATTEMPTS {
            std::thread::sleep(ATTACH_DELAY);
            if Self::endpoint_is_live(&paths) {
                return Ok(Self {
                    paths,
                    embedded: None,
                });
            }
        }
        Err(HostError::Contended {
            endpoint: paths.endpoint().to_path_buf(),
            data: paths.data().to_path_buf(),
            refusal: Box::new(refusal),
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

    /// Returns the durable workspace directory this window reads and writes beside.
    #[must_use]
    pub fn data(&self) -> &Path {
        self.paths.data()
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
    /// This process could not become the owner, and no owner ever answered.
    ///
    /// Two different situations reach here and the message must not claim to
    /// know which: another live process may hold the workspace lock and be
    /// slow to bind, or the workspace itself may be unreadable by this build.
    /// The service's own refusal is carried verbatim because it is the only
    /// part of this that says which.
    Contended {
        /// Endpoint this process waited on.
        endpoint: PathBuf,
        /// Workspace directory this process tried to own.
        data: PathBuf,
        /// The service's own refusal, retained verbatim.
        refusal: Box<ProcessError>,
    },
}

impl HostError {
    /// Returns the exact operand this failure happened to.
    #[must_use]
    pub fn operand(&self) -> String {
        match self {
            Self::Runtime(_) | Self::Service(_) => "local workspace".to_owned(),
            Self::Contended { endpoint, .. } => endpoint.to_string_lossy().into_owned(),
        }
    }
}

impl fmt::Display for HostError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Runtime(error) => error.fmt(formatter),
            Self::Service(error) => error.fmt(formatter),
            Self::Contended {
                endpoint,
                data,
                refusal,
            } => write!(
                formatter,
                "could not own {} and nothing answered on {}: {refusal}",
                data.display(),
                endpoint.display()
            ),
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
        // `/tmp`, not `temp_dir()`: under nix the latter is long enough that
        // the socket path exceeds `sockaddr_un` and the owner refuses to bind.
        let project = PathBuf::from("/tmp").join(format!(
            "nudox-h-{}-{nonce}",
            std::process::id()
        ));
        let data = project.join("state");
        let endpoint = PathBuf::from("/tmp").join(format!(
            "nudox-h-{}-{nonce}.sock",
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
