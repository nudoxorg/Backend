//! Zero-configuration discovery and process composition for local clients.
//!
//! Every product surface uses this crate to select the same workspace data
//! root, short Unix endpoint, authority credential, and daemon executable.
//!
//! # Endpoint identity
//!
//! Workspace ownership is a kernel lock on an inode, while the endpoint is
//! derived from a path. Durable state lives in the user's application-state
//! directory under a hash of the normalized project identity, so several
//! projects stay isolated without writing into source checkouts. Every path is
//! reduced to its filesystem identity by [`normalize_identity`] before it is
//! hashed, so alternate spellings always resolve to one data root and endpoint.
//!
//! # Daemon lifecycle
//!
//! [`ensure_locald`] spawns `backend-locald` detached, so the spawning CLI is
//! not the daemon's parent for lifetime purposes. Startup contenders that exit
//! during the readiness wait are reaped through their exact child handles.
//! Instead the daemon reaps itself: its listener carries an idle timeout
//! (ten minutes with no connected client by default, see
//! `backend_local_service::ListenerConfig`) and removes its socket on the way
//! out. A surface keeps a daemon alive simply by staying connected — the idle
//! window only advances while the connection count is zero — and a surface
//! that wants a longer or shorter window passes `--idle-timeout-ms` when it
//! spawns the daemon itself (`0` disables the timeout entirely).
#![deny(unsafe_code)]

extern crate alloc;

/// Shared source-selection policy re-exported for CLI, MCP, and GUI hosts.
pub use backend_discovery::DiscoveryPolicy;

/// Bounded physical-credit admission and one-owner execution.
pub mod server;

#[cfg(unix)]
mod bootstrap;
mod startup;
#[cfg(unix)]
pub use bootstrap::{
    LocaldStartup, SpawnedOwnerBootstrap, StartupPending, StartupPendingAction, StartupPendingCause,
    StartupPhase, report_automatic_startup_failure, start_locald,
};
pub use startup::{STARTUP_DIAGNOSTIC_ENV, StartupDiagnostic, StartupFailureReporter};

use std::ffi::OsString;
use std::fmt;
use std::fs::{self, OpenOptions};
use std::io::{Read, Write};
use std::path::{Component, Path, PathBuf};
use std::process::Command;
#[cfg(any(windows, test))]
use std::process::Stdio;
use std::sync::atomic::{AtomicU64, Ordering};
#[cfg(any(windows, test))]
use std::thread;
use std::time::{Duration, Instant};

/// Environment variable selecting the project whose state is indexed.
pub const PROJECT_ENV: &str = "BACKEND_PROJECT";
/// Environment variable selecting the durable engine data directory.
pub const DATA_ENV: &str = "BACKEND_LOCALD_WORKSPACE";
/// Environment variable selecting the local daemon Unix endpoint.
pub const ENDPOINT_ENV: &str = "BACKEND_LOCALD_ENDPOINT";
/// Environment variable selecting an installed local daemon executable.
pub const LOCALD_BIN_ENV: &str = "BACKEND_LOCALD_BIN";
/// Environment variable selecting the authority credential.
pub const AUTHORITY_SECRET_ENV: &str = "BACKEND_LOCALD_AUTHORITY_SECRET_FILE";
/// Environment variable naming the per-user runtime directory preferred for
/// derived Unix endpoints.
pub const RUNTIME_DIR_ENV: &str = "XDG_RUNTIME_DIR";

const APPLICATION_DIRECTORY: &str = "Nudox";
const PROJECTS_DIRECTORY: &str = "projects";
const AUTHORITY_FILE: &str = "authority.secret";
const AUTHORITY_INITIALIZATION_LOCK: &str = "authority-initialize.lock";
/// How long a daemon that is still running may take to publish its endpoint.
///
/// Opening a cold workspace is bounded by how much the last revision wrote,
/// not by a constant a surface can guess: a one-file project binds in
/// milliseconds and a real crate's workspace has a persisted transition,
/// relation roots, and a projection to re-admit first. A five-second budget
/// looked correct against a demo project and turned a healthy reopen of
/// `memchr` into `backend-locald did not open <socket>` — advice to re-run a
/// command that was already working. A live child is evidence that startup is
/// progressing, so waiting on it is not the same act as waiting on nothing.
const LIVE_START_TIMEOUT: Duration = Duration::from_secs(90);
/// The bounded interval a command-bearing surface may wait for the existing
/// owner to publish its endpoint before returning an unsent-command result.
///
/// A command invocation uses this once: if its candidate loses the workspace
/// owner lease, it waits for the winner instead of asking the user to retry
/// and spawning another contender.
pub const COMMAND_STARTUP_TIMEOUT: Duration = LIVE_START_TIMEOUT;
/// A checked owner-lease contender must await the already-opening winner.
/// This is distinct from a failed owner composition or an unavailable profile.
pub const OWNER_CONTENDED_EXIT_CODE: u8 = 75;
/// How long to keep waiting after the spawned child has exited.
///
/// Ordinary failure exits get a short final endpoint check. A typed owner-lease
/// contention exit retains the original cold-start budget instead: its winner
/// may still be replaying a healthy workspace before binding the listener.
const EXITED_START_GRACE: Duration = Duration::from_secs(5);
const POLL_INTERVAL: Duration = Duration::from_millis(20);
const PASSIVE_CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
/// Domain separator so an endpoint digest can never be confused with another
/// blake3 use of the same path bytes.
const ENDPOINT_DOMAIN: &[u8] = b"backend-v2-local-endpoint\0";
/// Conservative budget for a derived endpoint. The platform `sun_path` limit
/// is 104 bytes on macOS and 108 on Linux; staying under this keeps the
/// preferred runtime directory from producing an unbindable socket.
const MAX_DERIVED_ENDPOINT_BYTES: usize = 100;
/// Total capacity of `sockaddr_un.sun_path`, in bytes, on the strictest
/// platform this crate targets.
///
/// macOS declares `sun_path` as `char[104]` in `<sys/un.h>`; Linux declares it
/// as `char[108]` in `unix(7)`. A workspace's daemon and every client that
/// dials it must agree on one endpoint regardless of which of the two built
/// it, so the smaller of the two capacities is the one this crate enforces
/// everywhere `sockaddr_un` is unix-specific, not per compiled target.
#[cfg(target_os = "linux")]
const SUN_PATH_CAPACITY: usize = 108;
#[cfg(all(unix, not(target_os = "linux")))]
const SUN_PATH_CAPACITY: usize = 104;
/// Bytes available to the endpoint path itself once the NUL terminator that
/// `bind(2)`/`connect(2)` require inside `sun_path` is reserved.
#[cfg(unix)]
const MAX_ENDPOINT_PATH_BYTES: usize = SUN_PATH_CAPACITY - 1;
#[cfg(windows)]
const MAX_ENDPOINT_PATH_BYTES: usize = 100;
static SECRET_TEMP_SEQUENCE: AtomicU64 = AtomicU64::new(0);

/// All local paths selected for one project session.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WorkspacePaths {
    project: PathBuf,
    data: PathBuf,
    private_application_root: Option<ApplicationStateRoot>,
    endpoint: PathBuf,
    authority_secret: PathBuf,
}

impl WorkspacePaths {
    /// Discovers a project session from explicit values, environment, and the
    /// current directory, in that order.
    ///
    /// # Errors
    /// Returns an error when the current directory cannot be read or an
    /// explicit path is empty.
    pub fn discover(
        project: Option<PathBuf>,
        data: Option<PathBuf>,
        endpoint: Option<PathBuf>,
    ) -> Result<Self, RuntimeError> {
        let requested_project =
            project.or_else(|| std::env::var_os(PROJECT_ENV).map(PathBuf::from));
        let project = match requested_project {
            Some(project) => project,
            None => project_root_from(&std::env::current_dir().map_err(RuntimeError::Io)?),
        };
        if project.as_os_str().is_empty() {
            return Err(RuntimeError::InvalidPath("project path is empty"));
        }
        let project = normalize_identity(&project);
        let configured_data = data.or_else(|| std::env::var_os(DATA_ENV).map(PathBuf::from));
        let (data, private_application_root) = match configured_data {
            Some(data) => (data, None),
            None => {
                let state_root = application_state_root()?;
                (
                    default_workspace_path(&project, &state_root.path()),
                    Some(state_root),
                )
            }
        };
        if data.as_os_str().is_empty() {
            return Err(RuntimeError::InvalidPath("data path is empty"));
        }
        // `BACKEND_LOCALD_WORKSPACE` is taken verbatim from a shell, a launch
        // agent, or a test, so it is the likeliest source of a divergent
        // spelling. Reduce it to the same identity the workspace lock uses
        // before anything is derived from it.
        let data = normalize_identity(&data);
        if let Some(workspace_project) = checkout_for_workspace(&data)
            && !same_path_identity(&workspace_project, &project)
        {
            return Err(RuntimeError::WorkspaceProjectMismatch {
                project,
                workspace: data,
                workspace_project,
            });
        }
        let endpoint = endpoint
            .or_else(|| std::env::var_os(ENDPOINT_ENV).map(PathBuf::from))
            .unwrap_or_else(|| default_endpoint(&data));
        if endpoint.as_os_str().is_empty() {
            return Err(RuntimeError::InvalidPath("endpoint path is empty"));
        }
        // Windows canonicalization commonly yields a `\\?\\` verbatim
        // spelling. Normalize the process-boundary path before deriving or
        // comparing any local endpoint so the same socket is addressable from
        // shells, desktop launches, and MCP hosts that use ordinary drive or
        // UNC paths.
        let endpoint = normalize_verbatim_prefix(&endpoint);
        validate_endpoint_length(&endpoint)?;
        let authority_secret = std::env::var_os(AUTHORITY_SECRET_ENV)
            .map_or_else(|| data.join(AUTHORITY_FILE), PathBuf::from);
        Ok(Self {
            project,
            data,
            private_application_root,
            endpoint,
            authority_secret,
        })
    }

    /// Returns the indexed project root.
    #[must_use]
    pub fn project(&self) -> &Path {
        &self.project
    }

    /// Returns the durable engine state directory.
    #[must_use]
    pub fn data(&self) -> &Path {
        &self.data
    }

    /// Returns the short local Unix endpoint.
    #[must_use]
    pub fn endpoint(&self) -> &Path {
        &self.endpoint
    }

    /// Returns the owner-only authority credential path.
    #[must_use]
    pub fn authority_secret(&self) -> &Path {
        &self.authority_secret
    }

    /// Creates the private state directory and authority credential if needed.
    ///
    /// Existing credentials are admitted at their exact 32-byte length and
    /// never replaced.
    ///
    /// # Errors
    /// Returns an error for directory, randomness, permission, or credential
    /// admission failures.
    pub fn initialize(&self) -> Result<(), RuntimeError> {
        self.initialize_data_directory()?;
        ensure_authority_secret(&self.authority_secret)
    }

    /// Creates or verifies this workspace's private data directories without
    /// creating a local-owner authority credential.
    ///
    /// This is useful for local client-only state such as a remote-index
    /// identity, which needs a private workspace but does not start locald.
    ///
    /// # Errors
    /// Returns an error when the workspace directories cannot be admitted as
    /// private state.
    pub fn initialize_data_directory(&self) -> Result<(), RuntimeError> {
        if let Some(application_root) = self.private_application_root.as_ref() {
            initialize_default_state(application_root, &self.data).map_err(|source| {
                RuntimeError::DefaultStateInitialization {
                    path: application_root.path().join(APPLICATION_DIRECTORY), source,
                }
            })?;
        } else {
            backend_platform::durable::ensure_private_directory(&self.data)
                .map_err(RuntimeError::Io)?;
        }
        Ok(())
    }
}

/// An existing user profile/data anchor plus only the platform's known suffix.
/// Discovery freezes this choice; initialization never reads a second environment.
#[derive(Clone, Debug, Eq, PartialEq)]
struct ApplicationStateRoot {
    anchor: PathBuf,
    suffix: &'static [&'static str],
}

impl ApplicationStateRoot {
    fn new(anchor: PathBuf, suffix: &'static [&'static str]) -> Self {
        Self { anchor, suffix }
    }

    fn path(&self) -> PathBuf {
        self.suffix
            .iter()
            .fold(self.anchor.clone(), |path, name| path.join(name))
    }
}

fn initialize_default_state(state: &ApplicationStateRoot, data: &Path) -> std::io::Result<()> {
    let application = backend_platform::OwnedWorkspaceDirectory::under_user_data_application(
        &state.anchor,
        state.suffix,
        APPLICATION_DIRECTORY,
    )?;
    // These paths are the captured default application layout, never an
    // explicit workspace or conventional OS data directory. Keep the pinned
    // parent fences while repairing only each exact owned child.
    let repair_child = |parent: &backend_platform::OwnedWorkspaceDirectory, name: &str| {
        parent.verify_path()?;
        let path = parent.path().join(name);
        backend_platform::durable::ensure_private_application_directory(&path).map_err(
            |error| std::io::Error::new(error.kind(), format!("{}: {error}", path.display())),
        )?;
        parent.verify_path()?;
        parent.child(name)
    };
    let projects = repair_child(&application, PROJECTS_DIRECTORY)?;
    let name = data
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "default project state has no name",
            )
        })?;
    if !same_path_identity(&normalize_identity(&projects.path().join(name)), data) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "default project state is outside the captured application directory",
        ));
    }
    repair_child(&projects, name)?.verify_path()
}
/// Selects the repository boundary from any descendant directory. A Git root
/// wins over nested package manifests so CLI, MCP, and desktop sessions share
/// one durable workspace across a monorepo. Outside Git, the nearest common
/// language manifest is the project boundary.
fn project_root_from(start: &Path) -> PathBuf {
    const MARKERS: &[&str] = &[
        "Cargo.toml",
        "package.json",
        "go.mod",
        "pyproject.toml",
        "pom.xml",
        "build.gradle",
        "build.gradle.kts",
        "CMakeLists.txt",
    ];

    let mut nearest_manifest = None;
    for ancestor in start.ancestors() {
        if ancestor.join(".git").exists() {
            return ancestor.to_path_buf();
        }
        if nearest_manifest.is_none()
            && MARKERS.iter().any(|marker| ancestor.join(marker).is_file())
        {
            nearest_manifest = Some(ancestor.to_path_buf());
        }
    }
    nearest_manifest.unwrap_or_else(|| start.to_path_buf())
}

/// A local daemon that answered at a workspace's selected endpoint.
///
/// Holding this value is proof that a live owner was reachable at the instant
/// it was produced. It carries no capability of its own; a surface uses the
/// endpoint to open its own authenticated session.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LiveEndpoint {
    endpoint: PathBuf,
}

impl LiveEndpoint {
    /// Returns the endpoint published by the live owner.
    #[must_use]
    pub fn endpoint(&self) -> &Path {
        &self.endpoint
    }

    /// Consumes this proof and returns the endpoint path.
    #[must_use]
    pub fn into_endpoint(self) -> PathBuf {
        self.endpoint
    }
}

/// Probes the selected endpoint and reports a live owner without starting one.
///
/// A successful connect is the only evidence this crate accepts that an owner
/// exists. A missing or refusing socket is *not* evidence of the opposite: an
/// owner may hold the workspace lock and not yet have bound its listener, so
/// callers that need certainty follow a `None` with a bounded retry rather
/// than with a failure.
#[cfg(any(unix, windows))]
#[must_use]
pub fn try_attach(paths: &WorkspacePaths) -> Option<LiveEndpoint> {
    backend_platform::local::LocalStream::connect(paths.endpoint())
        .ok()
        .map(|_probe| LiveEndpoint {
            endpoint: paths.endpoint().to_path_buf(),
        })
}

/// Connects to the selected local endpoint with a bounded dial and no
/// composition or startup attempt.
///
/// Unlike [`ensure_locald`], this function never creates workspace state,
/// removes a stale endpoint, locates a daemon executable, or starts a process.
/// It is intended for health probes whose result must describe the configured
/// service rather than cause that service to start. The caller should still
/// authenticate and query the connected owner before treating it as healthy.
///
/// # Errors
/// Returns the original connection error and endpoint path when no owner
/// accepts the connection.
#[cfg(any(unix, windows))]
pub fn connect_existing_locald(paths: &WorkspacePaths) -> Result<PathBuf, RuntimeError> {
    backend_platform::local::connect_timeout(paths.endpoint(), PASSIVE_CONNECT_TIMEOUT)
        .map(|_probe| paths.endpoint().to_path_buf())
        .map_err(|source| RuntimeError::EndpointUnavailable {
            endpoint: paths.endpoint().to_path_buf(),
            source,
        })
}

/// Reports that passive local endpoint connections are unavailable on this
/// platform.
#[cfg(not(any(unix, windows)))]
pub fn connect_existing_locald(_paths: &WorkspacePaths) -> Result<PathBuf, RuntimeError> {
    Err(RuntimeError::Unsupported)
}

/// Reports that endpoint probing is unavailable on platforms without a local
/// stream transport.
#[cfg(not(any(unix, windows)))]
#[must_use]
pub fn try_attach(_paths: &WorkspacePaths) -> Option<LiveEndpoint> {
    None
}

/// Removes an endpoint whose owner is provably gone, and reports whether it
/// did.
///
/// The file is unlinked only when it is a socket that refuses or resets the
/// connection — the kernel states that mean no process is listening. A
/// non-socket, a permission failure, or any other error is left alone so a
/// misconfigured path fails loudly at bind instead of being deleted here.
#[cfg(any(unix, windows))]
fn unlink_dead_endpoint(endpoint: &Path) -> bool {
    use std::io::ErrorKind;

    let Ok(metadata) = fs::symlink_metadata(endpoint) else {
        return false;
    };
    if !is_endpoint_file(&metadata) {
        return false;
    }
    match backend_platform::local::LocalStream::connect(endpoint) {
        Ok(_live) => false,
        Err(error)
            if matches!(
                error.kind(),
                ErrorKind::ConnectionRefused | ErrorKind::NotFound | ErrorKind::ConnectionReset
            ) =>
        {
            fs::remove_file(endpoint).is_ok()
        }
        Err(_) => false,
    }
}

#[cfg(unix)]
fn is_endpoint_file(metadata: &fs::Metadata) -> bool {
    use std::os::unix::fs::FileTypeExt;
    metadata.file_type().is_socket()
}

#[cfg(windows)]
fn is_endpoint_file(metadata: &fs::Metadata) -> bool {
    backend_platform::win32::security::is_endpoint_metadata(metadata)
}

/// Ensures the shared local daemon is accepting connections and returns its
/// selected endpoint.
///
/// Concurrent callers may both attempt startup. The daemon owner lock admits
/// one winner and every caller converges on the same socket.
///
/// The spawned daemon is detached and is never reaped by this process; it
/// retires itself through its own idle timeout. See the module header.
///
/// # Errors
/// Returns an error when setup, executable discovery, process startup, or the
/// bounded readiness wait fails.
#[cfg(windows)]
pub fn ensure_locald(paths: &WorkspacePaths) -> Result<PathBuf, RuntimeError> {
    if let Some(live) = try_attach(paths) {
        return Ok(live.into_endpoint());
    }
    paths.initialize()?;
    if let Some(parent) = paths.endpoint().parent() {
        fs::create_dir_all(parent).map_err(RuntimeError::Io)?;
    }
    // A daemon that was killed leaves its socket behind. Sweeping it here
    // means the child never has to distinguish "a stale file is in my way"
    // from "somebody else is already listening".
    unlink_dead_endpoint(paths.endpoint());
    let executable = locald_executable()?;
    let startup = startup::StartupAttempt::prepare(paths.data()).map_err(RuntimeError::Io)?;
    let mut command = Command::new(&executable);
    // locald derives its configured project from the working directory when
    // no project flag exists, and rejects a workspace owned by a different
    // checkout. Services launch from unrelated working directories, so the
    // spawn must pin the CWD to the owning project.
    command
        .current_dir(paths.project())
        .arg("--endpoint")
        .arg(paths.endpoint())
        .arg("--workspace")
        .arg(paths.data())
        .arg("--authority-secret-file")
        .arg(paths.authority_secret())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    startup.configure(&mut command);
    detach(&mut command);
    let mut child = command
        .spawn()
        .map_err(|source| RuntimeError::Spawn { executable, source })?;
    let mut deadline = Instant::now() + LIVE_START_TIMEOUT;
    let mut child_exit = None;
    loop {
        if backend_platform::local::LocalStream::connect(paths.endpoint()).is_ok() {
            return Ok(paths.endpoint().to_path_buf());
        }
        if child_exit.is_none()
            && let Some(status) = child.try_wait().map_err(RuntimeError::Io)?
        {
            // A losing contender is not a failed startup. Preserve the
            // original cold-open budget for its actual lease winner; an
            // ordinary failed child cannot extend or restart that budget.
            child_exit = Some(status.code());
            deadline = deadline_after_exit(deadline, Instant::now(), status.code());
        }
        if Instant::now() >= deadline {
            return child_exit.map_or_else(
                || Err(RuntimeError::StartTimeout(paths.endpoint.clone())),
                |code| {
                    Err(RuntimeError::DaemonExited {
                        code,
                        diagnostic: startup.failure(),
                    })
                },
            );
        }
        thread::sleep(POLL_INTERVAL);
    }
}

/// Ensures local endpoint readiness with the existing ninety-second budget.
/// Unix performs durable initialization in the selected owner candidate.
/// A pending error does not mean the caller's command was submitted.
///
/// # Errors
/// Returns startup admission, original child failure, or pending readiness.
#[cfg(unix)]
pub fn ensure_locald(paths: &WorkspacePaths) -> Result<PathBuf, RuntimeError> {
    match start_locald(paths, LIVE_START_TIMEOUT, |_| {})? {
        LocaldStartup::Ready(endpoint) => Ok(endpoint),
        LocaldStartup::Pending(pending) => Err(RuntimeError::StartupPending(pending)),
    }
}

fn deadline_after_exit(original: Instant, now: Instant, code: Option<i32>) -> Instant {
    if code == Some(i32::from(OWNER_CONTENDED_EXIT_CODE)) {
        original
    } else {
        original.min(now + EXITED_START_GRACE)
    }
}

/// Gives a shared owner its own cancellation group. A launching client can
/// retire its process group without also terminating the workspace owner.
/// Unix preserves the inherited session; Windows also detaches the console.
#[cfg(windows)]
fn detach(command: &mut Command) {
    use std::os::windows::process::CommandExt as _;

    const DETACHED_PROCESS: u32 = 0x0000_0008;
    const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
    command.creation_flags(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP);
}

#[cfg(unix)]
fn detach(command: &mut Command) {
    use std::os::unix::process::CommandExt as _;
    command.process_group(0);
}

#[cfg(not(any(unix, windows)))]
const fn detach(_command: &mut Command) {}

/// Reports that automatic local daemon composition is unavailable on this platform.
#[cfg(not(any(unix, windows)))]
pub fn ensure_locald(_paths: &WorkspacePaths) -> Result<PathBuf, RuntimeError> {
    Err(RuntimeError::Unsupported)
}

/// Reduces one path to the filesystem identity the workspace lock uses.
///
/// `canonicalize` is authoritative whenever the directory exists: it resolves
/// symlinks, `.` and `..` segments, trailing separators, and the macOS
/// `/tmp` to `/private/tmp` indirection in one step.
///
/// A workspace that has not been created yet has no inode to resolve. Rather
/// than create it — discovery must stay free of side effects, because every
/// surface calls it before it knows whether it will own anything — the longest
/// existing ancestor is canonicalized and the remaining components are
/// appended after lexical reduction. The value therefore already agrees with
/// the canonical form for the part of the path that exists, and converges on
/// it completely as soon as the workspace directory is created.
fn normalize_identity(path: &Path) -> PathBuf {
    let absolute = lexically_absolute(path);
    let mut existing = absolute.as_path();
    let mut suffix: Vec<OsString> = Vec::new();
    loop {
        if let Ok(resolved) = existing.canonicalize() {
            let mut identity = normalize_verbatim_prefix(&resolved);
            for component in suffix.iter().rev() {
                identity.push(component);
            }
            return identity;
        }
        let (Some(parent), Some(name)) = (existing.parent(), existing.file_name()) else {
            return absolute;
        };
        suffix.push(name.to_os_string());
        existing = parent;
    }
}

/// Returns a stable per-project data path beneath the supplied application
/// state root. The project path is hashed after normalization so the directory
/// name does not disclose checkout paths and distinct projects do not share
/// indexes or authority databases.
fn default_workspace_path(project: &Path, state_root: &Path) -> PathBuf {
    let project = normalize_identity(project);
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"backend.runtime.project-state.v1\0");
    hasher.update(project.as_os_str().as_encoded_bytes());
    let digest = hasher.finalize();
    let mut key = String::with_capacity(64);
    for byte in digest.as_bytes() {
        use std::fmt::Write as _;
        let _ = write!(key, "{byte:02x}");
    }
    state_root
        .join(APPLICATION_DIRECTORY)
        .join(PROJECTS_DIRECTORY)
        .join(key)
}

/// Selects the operating system's per-user durable state directory without
/// creating it. Callers share this path across CLI, GUI, MCP, and locald.
fn application_state_root() -> Result<ApplicationStateRoot, RuntimeError> {
    #[cfg(target_os = "macos")]
    {
        let home = std::env::var_os("HOME").map(PathBuf::from).ok_or_else(|| {
            RuntimeError::Io(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "the user's home directory is unavailable",
            ))
        })?;
        return Ok(ApplicationStateRoot::new(home, &["Library", "Application Support"]));
    }

    #[cfg(target_os = "windows")]
    {
        if let Some(local_app_data) = std::env::var_os("LOCALAPPDATA") {
            let path = PathBuf::from(local_app_data);
            if path.is_absolute() {
                return Ok(ApplicationStateRoot::new(path, &[]));
            }
        }
        let profile = std::env::var_os("USERPROFILE")
            .map(PathBuf::from)
            .ok_or_else(|| {
                RuntimeError::Io(std::io::Error::new(
                    std::io::ErrorKind::NotFound,
                    "the user's LocalAppData directory is unavailable",
                ))
            })?;
        return Ok(ApplicationStateRoot::new(profile, &["AppData", "Local"]));
    }

    #[cfg(all(unix, not(target_os = "macos")))]
    {
        if let Some(state) = std::env::var_os("XDG_STATE_HOME") {
            let path = PathBuf::from(state);
            if path.is_absolute() {
                return Ok(ApplicationStateRoot::new(path, &[]));
            }
        }
        if let Some(data) = std::env::var_os("XDG_DATA_HOME") {
            let path = PathBuf::from(data);
            if path.is_absolute() {
                return Ok(ApplicationStateRoot::new(path, &["state"]));
            }
        }
        let home = std::env::var_os("HOME").map(PathBuf::from).ok_or_else(|| {
            RuntimeError::Io(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "the user's XDG state and home directories are unavailable",
            ))
        })?;
        return Ok(ApplicationStateRoot::new(home, &[".local", "state"]));
    }

    #[allow(unreachable_code)]
    Err(RuntimeError::Unsupported)
}

/// Absolutizes and lexically reduces a path without touching the filesystem.
fn lexically_absolute(path: &Path) -> PathBuf {
    let path = normalize_verbatim_prefix(path);
    let absolute = if path.is_absolute() {
        path
    } else {
        std::env::current_dir()
            .unwrap_or_else(|_| PathBuf::from("/"))
            .join(path)
    };
    let mut normalized = PathBuf::new();
    for component in absolute.components() {
        match component {
            Component::Prefix(_) | Component::RootDir => normalized.push(component.as_os_str()),
            Component::CurDir => {}
            Component::ParentDir => {
                if !normalized.pop() {
                    normalized.push(component.as_os_str());
                }
            }
            Component::Normal(part) => normalized.push(part),
        }
    }
    normalized
}

/// Removes Windows' verbatim path marker at the process boundary.
///
/// `canonicalize` deliberately returns `\\?\\` paths on Windows. Those paths
/// are valid for Win32 calls but leak an implementation spelling into package
/// coordinates, MCP instructions, and continuation context. The marker does
/// not identify a different file, so stripping it before lexical identity
/// reduction keeps endpoint ownership stable while giving every surface one
/// display spelling. UNC verbatim paths become ordinary UNC paths.
fn normalize_verbatim_prefix(path: &Path) -> PathBuf {
    #[cfg(windows)]
    {
        let text = path.as_os_str().to_string_lossy();
        // Compare bytes so an unrelated non-ASCII path prefix cannot panic
        // on a slice that splits a UTF-8 code point.
        if text
            .as_bytes()
            .get(..8)
            .is_some_and(|prefix| prefix.eq_ignore_ascii_case(br"\\?\unc\"))
        {
            return PathBuf::from(format!(r"\\{}", &text[8..]));
        }
        if let Some(local) = text.strip_prefix(r"\\?\") {
            // Only a verbatim drive path has a normal Win32 spelling. Keep
            // device namespaces such as `GLOBALROOT` and `Volume{...}`
            // verbatim: dropping their prefix would turn an absolute device
            // path into a relative path and could change which object is
            // addressed.
            let drive = local.as_bytes().get(1).copied() == Some(b':');
            if drive {
                return PathBuf::from(local);
            }
        }
    }
    path.to_path_buf()
}

/// Returns the canonical, user-facing spelling of a project or workspace
/// path. This resolves the existing prefix and strips Windows' `\\?\\` marker
/// without changing the filesystem identity used for ownership.
#[must_use]
pub fn normalize_surface_path(path: impl AsRef<Path>) -> PathBuf {
    normalize_identity(path.as_ref())
}

/// Returns the checkout that owns the legacy conventional `.backend/v2` path.
///
/// A caller can intentionally choose a state directory elsewhere, so this
/// check is limited to the old layout. That catches a stale MCP/CLI
/// configuration pointing at another checkout while preserving
/// explicit shared state locations for hosts that own their own identity
/// policy.
fn checkout_for_workspace(workspace: &Path) -> Option<PathBuf> {
    let version = workspace.file_name()?.to_string_lossy();
    let backend = workspace.parent()?.file_name()?.to_string_lossy();
    if !version.eq_ignore_ascii_case("v2") || !backend.eq_ignore_ascii_case(".backend") {
        return None;
    }
    workspace.parent()?.parent().map(normalize_identity)
}

/// Compares two normalized paths using the host filesystem's identity rules.
/// Windows paths are case-insensitive even when callers spell a drive, UNC
/// share, or directory component differently; Unix paths remain byte-exact.
#[cfg(windows)]
fn same_path_identity(left: &Path, right: &Path) -> bool {
    left.as_os_str()
        .to_string_lossy()
        .eq_ignore_ascii_case(&right.as_os_str().to_string_lossy())
}

#[cfg(not(windows))]
fn same_path_identity(left: &Path, right: &Path) -> bool {
    left == right
}

/// Returns the effective user identity folded into every derived endpoint.
///
/// `/tmp` is shared, so two users indexing the same absolute workspace path
/// would otherwise contend for one 0600 socket that only one of them can open.
#[cfg(unix)]
fn effective_uid() -> u32 {
    rustix::process::geteuid().as_raw()
}

#[cfg(not(unix))]
fn effective_uid() -> u32 {
    #[cfg(windows)]
    {
        // Windows has no numeric effective UID. Fold the authenticated user
        // SID into the endpoint identity so two users sharing a workspace
        // root still derive different owner-protected endpoints.
        if let Ok(user) = backend_platform::win32::identity::current_user() {
            let digest = blake3::hash(user.as_bytes());
            let bytes = digest.as_bytes();
            return u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
        }
    }
    0
}

/// Selects the directory that holds derived endpoints.
///
/// `XDG_RUNTIME_DIR` is preferred because it is per-user, mode `0700`, and
/// cleaned by the session rather than by a `/tmp` sweeper. It is used only
/// when it is absolute and leaves the endpoint inside the platform `sun_path`
/// budget; otherwise `/tmp` remains the answer.
///
/// `TMPDIR` is deliberately *not* consulted. On macOS it names a per-session
/// directory, so two surfaces of the same user can see different values — the
/// exact divergence this derivation exists to remove.
#[cfg(unix)]
fn socket_directory(file_name: &str) -> PathBuf {
    let fallback = PathBuf::from("/tmp");
    let Some(preferred) = std::env::var_os(RUNTIME_DIR_ENV).map(PathBuf::from) else {
        return fallback;
    };
    if preferred.is_absolute()
        && preferred.join(file_name).as_os_str().len() <= MAX_DERIVED_ENDPOINT_BYTES
    {
        preferred
    } else {
        fallback
    }
}

#[cfg(not(unix))]
fn socket_directory(_file_name: &str) -> PathBuf {
    std::env::temp_dir()
}

/// Derives the short Unix endpoint for a workspace's data directory using
/// this crate's hashed, short-prefix scheme — the same one
/// [`WorkspacePaths::discover`] falls back to when no endpoint is supplied
/// *and* `BACKEND_LOCALD_ENDPOINT` is unset.
///
/// Unlike `discover`, this never consults the environment. A caller that
/// needs a workspace-specific endpoint regardless of an ambient
/// `BACKEND_LOCALD_ENDPOINT` left over from another session on a shared
/// machine — a test deriving a fresh, unique, always-short socket per
/// fixture is the motivating case — should call this directly instead of
/// duplicating the hashing scheme or risking a collision with whatever that
/// variable happens to name.
#[must_use]
pub fn derive_endpoint(data: &Path) -> PathBuf {
    default_endpoint(data)
}

/// Derives the endpoint for a workspace from its filesystem identity.
///
/// The digest covers a domain separator, the effective user, and the
/// normalized workspace path, so the same directory always yields the same
/// socket however it was spelled, and two users never collide.
fn default_endpoint(data: &Path) -> PathBuf {
    let identity = normalize_identity(data);
    let mut hasher = blake3::Hasher::new();
    hasher.update(ENDPOINT_DOMAIN);
    hasher.update(&effective_uid().to_be_bytes());
    hasher.update(&[0]);
    hasher.update(identity.as_os_str().as_encoded_bytes());
    let digest = hasher.finalize();
    let mut short = String::with_capacity(24);
    for byte in &digest.as_bytes()[..12] {
        use std::fmt::Write as _;
        let _ = write!(short, "{byte:02x}");
    }
    let file_name = format!("backend-v2-{short}.sock");
    socket_directory(&file_name).join(file_name)
}

/// Rejects an endpoint that could never be bound or connected to.
///
/// `UnixListener`/`UnixStream` copy the path into a fixed-size `sun_path`
/// buffer at the syscall boundary, so an over-long path fails there with a
/// raw `ENAMETOOLONG` `io::Error` that gives a caller no way to tell "this
/// workspace's identity is unreachable" from an ordinary transient I/O fault.
/// Catching it here, against an honestly derived platform limit, turns that
/// into a typed, actionable error before a socket is ever touched.
#[cfg(any(unix, windows))]
fn validate_endpoint_length(endpoint: &Path) -> Result<(), RuntimeError> {
    let bytes = endpoint.as_os_str().len();
    if bytes > MAX_ENDPOINT_PATH_BYTES {
        return Err(RuntimeError::EndpointTooLong {
            endpoint: endpoint.to_path_buf(),
            bytes,
            limit: MAX_ENDPOINT_PATH_BYTES,
        });
    }
    Ok(())
}

#[cfg(not(any(unix, windows)))]
const fn validate_endpoint_length(_endpoint: &Path) -> Result<(), RuntimeError> {
    Ok(())
}

fn locald_executable() -> Result<PathBuf, RuntimeError> {
    if let Some(path) = std::env::var_os(LOCALD_BIN_ENV).map(PathBuf::from) {
        if path.is_file() {
            return Ok(path);
        }
        return Err(RuntimeError::MissingExecutable(path));
    }
    let current = std::env::current_exe().map_err(RuntimeError::Io)?;
    locald_sibling(&current)
}

fn locald_sibling(current: &Path) -> Result<PathBuf, RuntimeError> {
    // macOS retains the launched symlink in current_exe. The stock installer
    // names that link `nudox`, while the matched owner remains in the app bundle.
    let current = fs::canonicalize(current).map_err(RuntimeError::Io)?;
    let sibling = current.with_file_name(format!("backend-locald{}", std::env::consts::EXE_SUFFIX));
    if sibling.is_file() {
        return Ok(sibling);
    }
    Err(RuntimeError::MissingExecutable(sibling))
}

fn ensure_authority_secret(path: &Path) -> Result<(), RuntimeError> {
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt as _;

    // An already admitted credential needs no write access or initializer
    // lock. A concurrently publishing credential is not admitted yet.
    if validate_authority_secret(path).is_ok() {
        return Ok(());
    }
    let parent = path.parent().ok_or_else(|| {
        RuntimeError::Io(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "authority credential needs a parent directory",
        ))
    })?;
    backend_platform::durable::ensure_private_directory(parent).map_err(RuntimeError::Io)?;
    let directory = backend_platform::DirectoryCapability::open(
        &fs::canonicalize(parent).map_err(RuntimeError::Io)?,
    )
    .map_err(RuntimeError::Io)?;
    directory.validate_private().map_err(RuntimeError::Io)?;
    let initialization = directory
        .open_private_file_read_write(AUTHORITY_INITIALIZATION_LOCK, true)
        .map_err(RuntimeError::Io)?;
    initialization.lock().map_err(RuntimeError::Io)?;
    // Independent processes keep the same descriptor lease through staging,
    // no-clobber publication and temporary unlink. No racing initializer can
    // inspect the valid credential's transient two-link publication window.
    match fs::symlink_metadata(path) {
        Ok(_) => return validate_authority_secret(path),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(RuntimeError::Io(error)),
    }
    let mut bytes = [0_u8; 32];
    #[cfg(unix)]
    fs::File::open("/dev/urandom")
        .and_then(|mut source| source.read_exact(&mut bytes))
        .map_err(RuntimeError::Io)?;
    #[cfg(windows)]
    backend_platform::win32::random::fill(&mut bytes).map_err(RuntimeError::Io)?;
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let temporary = path.with_extension(format!(
        "secret.{}.{}.{}.tmp",
        std::process::id(),
        nonce,
        SECRET_TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed)
    ));
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o600);
    }
    let mut file = options.open(&temporary).map_err(RuntimeError::Io)?;
    #[cfg(unix)]
    let staged = file
        .set_permissions(fs::Permissions::from_mode(0o600))
        .and_then(|()| file.write_all(&bytes))
        .and_then(|()| file.sync_all())
        .map_err(RuntimeError::Io);
    #[cfg(not(unix))]
    let staged = file
        .write_all(&bytes)
        .and_then(|()| file.sync_all())
        .map_err(RuntimeError::Io);
    drop(file);
    if let Err(error) = staged {
        let _ = fs::remove_file(&temporary);
        return Err(error);
    }
    #[cfg(windows)]
    if let Err(error) = backend_platform::win32::security::restrict_to_current_user(&temporary) {
        let _ = fs::remove_file(&temporary);
        return Err(RuntimeError::Io(error));
    }
    let published = fs::hard_link(&temporary, path);
    let _ = fs::remove_file(&temporary);
    match published {
        Ok(()) => {
            backend_platform::durable::sync_parent(path).map_err(RuntimeError::Io)?;
            validate_authority_secret(path)
        }
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            validate_authority_secret(path)
        }
        Err(error) => Err(RuntimeError::Io(error)),
    }
}

fn validate_authority_secret(path: &Path) -> Result<(), RuntimeError> {
    let file = backend_platform::durable::open_private_read(path).map_err(RuntimeError::Io)?;
    let metadata = file.metadata().map_err(RuntimeError::Io)?;
    if !metadata.is_file() || metadata.len() != 32 {
        Err(RuntimeError::InvalidCredential(path.to_path_buf()))
    } else {
        Ok(())
    }
}

/// Local composition failure.
#[derive(Debug)]
pub enum RuntimeError {
    /// A filesystem or process operation failed.
    Io(std::io::Error),
    /// The captured platform-default state location could not be initialized.
    DefaultStateInitialization {
        /// Application directory below the selected user profile/data anchor.
        path: PathBuf,
        /// Original filesystem admission or creation cause.
        source: std::io::Error,
    },
    /// One selected path was empty.
    InvalidPath(&'static str),
    /// The configured workspace state belongs to another checkout.
    WorkspaceProjectMismatch {
        /// Checkout selected by the caller.
        project: PathBuf,
        /// State directory selected by the caller.
        workspace: PathBuf,
        /// Checkout inferred from the conventional state layout.
        workspace_project: PathBuf,
    },
    /// The authority credential was not one private 32-byte file.
    InvalidCredential(PathBuf),
    /// The endpoint path is longer than the platform local-socket address can
    /// hold, so it could never be bound or connected to.
    EndpointTooLong {
        /// The path that was rejected.
        endpoint: PathBuf,
        /// Its length in bytes.
        bytes: usize,
        /// The maximum number of path bytes the platform allows, with room
        /// already reserved for the mandatory NUL terminator.
        limit: usize,
    },
    /// No installed daemon executable was found.
    MissingExecutable(PathBuf),
    /// Starting the selected executable failed.
    Spawn {
        /// Executable selected by discovery.
        executable: PathBuf,
        /// Process creation failure.
        source: std::io::Error,
    },
    /// A passive connection could not reach the existing owner endpoint.
    EndpointUnavailable {
        /// Endpoint selected by the caller.
        endpoint: PathBuf,
        /// Original local-socket connection error, including its kind.
        source: std::io::Error,
    },
    /// The daemon exited before accepting clients.
    DaemonExited {
        /// Exit status of the attempted owner process.
        code: Option<i32>,
        /// Original bounded startup cause, when the owner could report one.
        diagnostic: Option<StartupDiagnostic>,
    },
    /// The existing owner candidate is still starting; no user command was submitted.
    #[cfg(unix)]
    StartupPending(StartupPending),
    /// The matched automatic companion protocol could not be admitted.
    #[cfg(unix)]
    StartupChannel(&'static str),
    /// Selected companion does not implement the matched startup prefix/channel.
    #[cfg(unix)]
    CompanionProtocolMismatch,
    /// The daemon did not become ready before the bounded deadline.
    StartTimeout(PathBuf),
    /// Automatic local composition is unavailable on this platform.
    Unsupported,
}

impl fmt::Display for RuntimeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(error) => write!(formatter, "local runtime I/O failed: {error}"),
            Self::DefaultStateInitialization { path, source } => write!(formatter,
                "initialize default state at {}: {source}; the user profile/data directories must be owned by this user and not writable by other users",
                path.display()),
            Self::InvalidPath(message) => formatter.write_str(message),
            Self::WorkspaceProjectMismatch {
                project,
                workspace,
                workspace_project,
            } => write!(
                formatter,
                "workspace {} belongs to checkout {}, but the configured project is {}; choose a workspace owned by that project or remove the workspace override",
                workspace.display(),
                workspace_project.display(),
                project.display(),
            ),
            Self::InvalidCredential(path) => write!(
                formatter,
                "authority credential {} must be one 32-byte file",
                path.display()
            ),
            Self::EndpointTooLong {
                endpoint,
                bytes,
                limit,
            } => write!(
                formatter,
                "endpoint {} is {bytes} bytes, exceeding the {limit}-byte sockaddr_un.sun_path limit",
                endpoint.display()
            ),
            Self::MissingExecutable(path) => write!(
                formatter,
                "backend-locald companion executable was not found at {}; build or install backend-locald beside backend-cli/backend-mcp (cargo build -p backend-locald -p backend-cli -p backend-mcp), or set {LOCALD_BIN_ENV} to its absolute path",
                path.display()
            ),
            Self::Spawn { executable, source } => {
                write!(formatter, "start {}: {source}", executable.display())
            }
            Self::EndpointUnavailable { endpoint, source } => write!(
                formatter,
                "no existing local owner answered at {}: {source}",
                endpoint.display()
            ),
            Self::DaemonExited { code, diagnostic } => {
                write!(formatter, "backend-locald exited during startup ({code:?})")?;
                if let Some(diagnostic) = diagnostic {
                    write!(formatter, ": {diagnostic}")?;
                }
                Ok(())
            }
            #[cfg(unix)]
            Self::StartupPending(pending) => write!(
                formatter,
                "startup pending for candidate {:?} in phase {}; retry the original command; it has not been submitted",
                pending.candidate_pid(), pending.phase().as_str()
            ),
            #[cfg(unix)]
            Self::StartupChannel(cause) => formatter.write_str(cause),
            #[cfg(unix)]
            Self::CompanionProtocolMismatch => formatter.write_str("selected companion rejected the versioned automatic startup protocol; install matched CLI and locald or rebuild them together (cargo build -p backend-locald -p backend-cli -p backend-mcp)"),
            Self::StartTimeout(path) => {
                write!(formatter, "backend-locald did not open {}", path.display())
            }
            Self::Unsupported => {
                formatter.write_str("automatic local runtime requires Unix or Windows")
            }
        }
    }
}

impl std::error::Error for RuntimeError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(source)
            | Self::DefaultStateInitialization { source, .. }
            | Self::EndpointUnavailable { source, .. }
            | Self::Spawn { source, .. } => Some(source),
            #[cfg(unix)]
            Self::StartupPending(_) | Self::StartupChannel(_) | Self::CompanionProtocolMismatch => None,
            Self::InvalidPath(_)
            | Self::WorkspaceProjectMismatch { .. }
            | Self::InvalidCredential(_)
            | Self::EndpointTooLong { .. }
            | Self::MissingExecutable(_)
            | Self::DaemonExited { .. }
            | Self::StartTimeout(_)
            | Self::Unsupported => None,
        }
    }
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn an_owner_contender_keeps_the_cold_open_budget_without_extending_it() {
        let start = Instant::now();
        let original = start + LIVE_START_TIMEOUT;
        let early_exit = start + Duration::from_millis(100);
        let cold_winner = start + Duration::from_secs(18);
        let contender = deadline_after_exit(
            original,
            early_exit,
            Some(i32::from(OWNER_CONTENDED_EXIT_CODE)),
        );
        assert!(
            cold_winner < contender,
            "a healthy cold winner must not inherit a losing child's five-second window"
        );
        assert!(
            cold_winner > deadline_after_exit(original, early_exit, Some(70)),
            "a genuine startup failure retains only its final endpoint-check grace"
        );
        let late_exit = original - Duration::from_millis(10);
        assert_eq!(
            deadline_after_exit(original, late_exit, Some(70)),
            original,
            "late exits never restart the overall startup budget"
        );
        assert_eq!(
            deadline_after_exit(
                original,
                late_exit,
                Some(i32::from(OWNER_CONTENDED_EXIT_CODE))
            ),
            original
        );
    }

    /// Runs the actual owner launcher against an explicitly supplied private
    /// runtime fixture. This is kept out of ordinary unit runs.
    #[cfg(unix)]
    #[test]
    #[ignore = "requires an owned matched runtime fixture and executable"]
    fn real_private_owner_launch() {
        let path = |name| PathBuf::from(std::env::var_os(name).expect("explicit private fixture"));
        let paths = WorkspacePaths::discover(
            Some(path("BACKEND_RUNTIME_TEST_PROJECT")),
            Some(path("BACKEND_RUNTIME_TEST_WORKSPACE")),
            Some(path("BACKEND_RUNTIME_TEST_ENDPOINT")),
        )
        .expect("private fixture paths");
        ensure_locald(&paths).expect("actual owner accepts connections");
    }

    /// A separate subprocess lets the test cancel a real launching process group
    /// without signaling the test runner or another test's children.
    #[cfg(unix)]
    #[test]
    fn unix_detached_owner_survives_launcher_group_cancellation() {
        use rustix::process::{Pid, Signal, getpgid, getsid, kill_process, kill_process_group};
        use std::os::unix::process::CommandExt as _;

        const ROLE: &str = "BACKEND_RUNTIME_DETACH_TEST_ROLE";
        const ROOT: &str = "BACKEND_RUNTIME_DETACH_TEST_ROOT";
        if let Ok(role) = std::env::var(ROLE) {
            if role == "launcher" {
                let root = PathBuf::from(std::env::var_os(ROOT).expect("child root"));
                let mut command = Command::new(std::env::current_exe().expect("test executable"));
                command
                    .args([
                        "--exact",
                        "tests::unix_detached_owner_survives_launcher_group_cancellation",
                    ])
                    .env(ROLE, "owner")
                    .stdin(Stdio::null())
                    .stdout(Stdio::null())
                    .stderr(Stdio::null());
                detach(&mut command);
                let child = command.spawn().expect("spawn detached owner");
                fs::write(root.join("owner.pid.pending"), child.id().to_string())
                    .expect("write owner PID");
                fs::rename(root.join("owner.pid.pending"), root.join("owner.pid"))
                    .expect("publish owner PID");
            }
            let root = PathBuf::from(std::env::var_os(ROOT).expect("child root"));
            let mut tick = 0_u64;
            loop {
                if role == "owner" {
                    tick += 1;
                    fs::write(root.join("owner.alive"), tick.to_string()).expect("owner heartbeat");
                }
                thread::sleep(Duration::from_millis(5));
            }
        }
        let root = test_directory("detach-cancellation");
        fs::create_dir_all(&root).expect("test root");
        let mut launcher = Command::new(std::env::current_exe().expect("test executable"))
            .args([
                "--exact",
                "tests::unix_detached_owner_survives_launcher_group_cancellation",
            ])
            .env(ROLE, "launcher")
            .env(ROOT, &root)
            .process_group(0)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn isolated launcher");
        let deadline = Instant::now() + Duration::from_secs(5);
        let owner = loop {
            if let Ok(value) = fs::read_to_string(root.join("owner.pid")) {
                break Pid::from_raw(value.parse().expect("owner PID")).expect("nonzero owner PID");
            }
            if Instant::now() >= deadline {
                launcher.kill().expect("retire failed launcher");
                launcher.wait().expect("reap failed launcher");
                panic!("launcher did not publish its owner PID");
            }
            thread::sleep(Duration::from_millis(5));
        };
        let launcher_pid = Pid::from_raw(launcher.id().cast_signed()).expect("launcher PID");
        let owner_group = getpgid(Some(owner)).expect("owner group before cancellation");
        let owner_session = getsid(Some(owner)).expect("owner session before cancellation");
        let before_cancel = fs::read_to_string(root.join("owner.alive")).unwrap_or_default();
        kill_process_group(launcher_pid, Signal::TERM).expect("cancel launcher group");
        launcher.wait().expect("reap canceled launcher");
        let deadline = Instant::now() + Duration::from_secs(2);
        let survives = loop {
            if fs::read_to_string(root.join("owner.alive"))
                .is_ok_and(|value| !value.is_empty() && value != before_cancel)
            {
                break true;
            }
            if Instant::now() >= deadline {
                break false;
            }
            thread::sleep(Duration::from_millis(5));
        };
        // Cleanup precedes assertions, including the before-fix failure path.
        let _ = kill_process(owner, Signal::KILL);
        fs::remove_dir_all(root).expect("remove fixture");
        assert_eq!(
            owner_group, owner,
            "owner must lead its own cancellation group"
        );
        assert_eq!(
            owner_session,
            getsid(None).expect("parent session"),
            "Unix group detachment deliberately preserves the session"
        );
        assert!(
            survives,
            "canceling a launching client must preserve its shared owner"
        );
    }

    #[test]
    fn missing_locald_error_names_the_companion_build_and_override() {
        let error = RuntimeError::MissingExecutable(PathBuf::from("/build/bin/backend-locald"));
        let message = error.to_string();
        assert!(message.contains("backend-locald companion executable"));
        assert!(message.contains("cargo build -p backend-locald -p backend-cli -p backend-mcp"));
        assert!(message.contains(LOCALD_BIN_ENV));
    }

    #[cfg(unix)]
    #[test]
    fn locald_sibling_resolves_installed_cli_symlink_to_matched_bundle() {
        let root = test_directory("installed-cli-symlink");
        let bundle = root.join("Applications/Nudox.app/Contents/MacOS");
        let installed = root.join(".local/bin");
        fs::create_dir_all(&bundle).expect("bundle directory");
        fs::create_dir_all(&installed).expect("installation directory");
        let cli = bundle.join("backend-cli");
        let owner = bundle.join("backend-locald");
        fs::write(&cli, b"matched cli").expect("CLI image");
        fs::write(&owner, b"matched owner").expect("owner image");
        let link = installed.join("nudox");
        std::os::unix::fs::symlink(&cli, &link).expect("stock installed CLI link");
        // A same-directory daemon must not supersede the matched bundle owner.
        fs::write(installed.join("backend-locald"), b"other owner").expect("other owner image");
        assert_eq!(
            locald_sibling(&link).expect("discover matched bundle owner"),
            fs::canonicalize(&owner).expect("canonical matched owner")
        );
        fs::remove_dir_all(root).expect("remove installation fixture");
    }

    #[cfg(unix)]
    #[test]
    fn passive_connect_preserves_missing_endpoint_cause_without_creating_state() {
        let root = socket_test_directory("passive-owner-absent");
        let project = root.join("project");
        let workspace = root.join("state");
        let endpoint = root.join("run").join("locald.sock");
        fs::create_dir_all(&project).expect("create project only");
        let paths = WorkspacePaths::discover(
            Some(project),
            Some(workspace.clone()),
            Some(endpoint.clone()),
        )
        .expect("explicit passive paths");

        let error = connect_existing_locald(&paths).expect_err("owner is absent");
        assert!(matches!(
            error,
            RuntimeError::EndpointUnavailable { source, .. }
                if source.kind() == std::io::ErrorKind::NotFound
        ));
        assert!(
            !workspace.exists(),
            "passive connect created workspace state"
        );
        assert!(
            !endpoint.parent().expect("endpoint parent").exists(),
            "passive connect created the endpoint directory"
        );

        fs::remove_dir_all(root).expect("remove passive fixture");
    }

    #[cfg(unix)]
    #[test]
    fn passive_connect_preserves_refused_stale_socket_cause() {
        use std::os::unix::net::UnixListener;

        let root = socket_test_directory("passive-owner-refused");
        fs::create_dir_all(&root).expect("create fixture root");
        let project = root.join("project");
        let workspace = root.join("state");
        let endpoint = root.join("locald.sock");
        fs::create_dir(&project).expect("create project only");
        let paths = WorkspacePaths::discover(
            Some(project),
            Some(workspace.clone()),
            Some(endpoint.clone()),
        )
        .expect("explicit passive paths");
        drop(UnixListener::bind(&endpoint).expect("bind stale socket"));

        let error = connect_existing_locald(&paths).expect_err("stale socket refuses connection");
        assert!(matches!(
            error,
            RuntimeError::EndpointUnavailable { source, .. }
                if source.kind() == std::io::ErrorKind::ConnectionRefused
        ));
        assert!(
            endpoint.exists(),
            "passive connect removed the stale socket while probing it"
        );
        assert!(
            !workspace.exists(),
            "passive connect created workspace state"
        );

        fs::remove_dir_all(root).expect("remove passive fixture");
    }

    #[test]
    fn discover_rejects_an_endpoint_that_overflows_sun_path() {
        let root = test_directory("endpoint-too-long");
        fs::create_dir_all(&root).expect("create fixture root");
        let padding = "x".repeat(MAX_ENDPOINT_PATH_BYTES);
        let endpoint = root.join(format!("{padding}.sock"));
        assert!(
            endpoint.as_os_str().len() > MAX_ENDPOINT_PATH_BYTES,
            "fixture endpoint must actually exceed the limit"
        );

        let error = WorkspacePaths::discover(Some(root.clone()), None, Some(endpoint.clone()))
            .expect_err("an oversized endpoint must be rejected");
        match &error {
            RuntimeError::EndpointTooLong {
                endpoint: rejected,
                bytes,
                limit,
            } => {
                assert_eq!(rejected, &endpoint);
                assert_eq!(*limit, MAX_ENDPOINT_PATH_BYTES);
                assert!(*bytes > *limit);
            }
            other => panic!("expected EndpointTooLong, got {other:?}"),
        }
        let message = error.to_string();
        assert!(
            message.contains("sockaddr_un.sun_path"),
            "rendered message did not name the limit it enforces: {message}"
        );

        fs::remove_dir_all(root).expect("remove runtime fixture");
    }

    #[test]
    fn discover_accepts_an_endpoint_within_sun_path() {
        let root = test_directory("endpoint-short-ok");
        fs::create_dir_all(&root).expect("create fixture root");
        let endpoint = PathBuf::from("/tmp/backend-v2-endpoint-short-ok.sock");
        assert!(endpoint.as_os_str().len() <= MAX_ENDPOINT_PATH_BYTES);

        let discovered = WorkspacePaths::discover(Some(root.clone()), None, Some(endpoint.clone()))
            .expect("a short endpoint must be accepted");
        assert_eq!(discovered.endpoint(), endpoint);

        fs::remove_dir_all(root).expect("remove runtime fixture");
    }

    #[test]
    fn discover_rejects_state_from_a_different_checkout() {
        let first = test_directory("checkout-a");
        let second = test_directory("checkout-b");
        fs::create_dir_all(&first).expect("create first checkout");
        let workspace = second.join(".backend").join("v2");
        fs::create_dir_all(&workspace).expect("create state fixture");
        let endpoint = PathBuf::from("/tmp/backend-v2-checkout-mismatch.sock");

        let error =
            WorkspacePaths::discover(Some(first.clone()), Some(workspace.clone()), Some(endpoint))
                .expect_err("state from another checkout must be refused");
        match &error {
            RuntimeError::WorkspaceProjectMismatch {
                project,
                workspace: observed,
                workspace_project,
            } => {
                assert_eq!(project, &normalize_identity(&first));
                assert_eq!(observed, &normalize_identity(&workspace));
                assert_eq!(workspace_project, &normalize_identity(&second));
            }
            other => panic!("expected workspace mismatch, got {other:?}"),
        }
        let message = error.to_string();
        assert!(message.contains("belongs to checkout"), "{message}");
        assert!(message.contains("choose a workspace owned"), "{message}");
        assert!(
            message.contains("remove the workspace override"),
            "{message}"
        );

        fs::remove_dir_all(first).expect("remove first checkout");
        fs::remove_dir_all(second).expect("remove second checkout");
    }

    #[cfg(windows)]
    #[test]
    fn verbatim_drive_and_unc_paths_share_display_identity_without_device_aliasing() {
        let drive = PathBuf::from(r"C:\workspace\project");
        let extended_drive = PathBuf::from(r"\\?\C:\workspace\project");
        assert_eq!(
            normalize_verbatim_prefix(&extended_drive),
            drive,
            "the \u{005c}\u{005c}?\u{005c} drive marker is a display spelling"
        );

        let unc = PathBuf::from(r"\\server\share\project");
        let extended_unc = PathBuf::from(r"\\?\UNC\server\share\project");
        assert_eq!(normalize_verbatim_prefix(&extended_unc), unc);

        let device = PathBuf::from(r"\\?\GLOBALROOT\Device\HarddiskVolumeShadowCopy1");
        assert_eq!(normalize_verbatim_prefix(&device), device);
    }

    #[test]
    fn derived_endpoints_are_short_stable_and_workspace_specific() {
        let first = default_endpoint(Path::new("/a/very/long/project/data/path"));
        let repeated = default_endpoint(Path::new("/a/very/long/project/data/path"));
        let other = default_endpoint(Path::new("/another/project"));
        assert_eq!(first, repeated);
        assert_ne!(first, other);
        assert!(first.as_os_str().len() < 80);
    }

    #[test]
    fn public_derive_endpoint_matches_the_internal_scheme() {
        // `derive_endpoint` is the pub wrapper callers outside this crate
        // (`tests/journeys`, notably) use to get a workspace-specific socket
        // without going through `discover`'s `BACKEND_LOCALD_ENDPOINT`
        // environment fallback. It must be a plain passthrough to the same
        // hashing scheme, not a second implementation that could drift.
        let workspace = Path::new("/a/very/long/project/data/path");
        assert_eq!(derive_endpoint(workspace), default_endpoint(workspace));
    }

    #[test]
    fn one_workspace_derives_one_endpoint_however_it_is_spelled() {
        let root = test_directory("endpoint-identity");
        let data = root.join("state");
        fs::create_dir_all(&data).expect("create workspace fixture");
        let canonical = default_endpoint(&data);

        // Every spelling below names the same inode. `/tmp` is a symlink to
        // `/private/tmp` on macOS, which is why the platform temporary
        // directory is exercised alongside the lexical variants.
        let spellings = [
            data.join("."),
            root.join(".").join("state"),
            root.join("state").join("nested").join(".."),
            PathBuf::from(format!("{}/", data.display())),
            PathBuf::from(format!("{}//state", root.display())),
        ];
        for spelling in spellings {
            assert_eq!(
                default_endpoint(&spelling),
                canonical,
                "divergent spelling derived a second endpoint: {}",
                spelling.display()
            );
        }

        // Discovery must agree with the raw derivation, including through the
        // verbatim `BACKEND_LOCALD_WORKSPACE` path.
        let discovered = WorkspacePaths::discover(
            Some(root.clone()),
            Some(root.join(".").join("state").join("")),
            None,
        )
        .expect("discover divergent spelling");
        assert_eq!(discovered.endpoint(), canonical);
        // The canonical identity of an existing directory, spelled the way every
        // surface displays it. `canonicalize` yields the verbatim form on Windows.
        let canonical_data =
            normalize_verbatim_prefix(&data.canonicalize().expect("canonical data"));
        assert_eq!(discovered.data(), canonical_data);
        assert!(
            !discovered.data().to_string_lossy().starts_with(r"\\?\"),
            "a verbatim marker must not leak into the discovered workspace path"
        );

        fs::remove_dir_all(root).expect("remove runtime fixture");
    }

    #[test]
    fn an_uncreated_workspace_still_agrees_with_its_created_identity() {
        let root = test_directory("endpoint-deferred");
        fs::create_dir_all(&root).expect("create fixture root");
        let data = root.join("state");
        let before = default_endpoint(&root.join(".").join("state"));
        fs::create_dir_all(&data).expect("create workspace");
        let after = default_endpoint(&data);
        assert_eq!(
            before, after,
            "creating the workspace must not move its endpoint"
        );
        fs::remove_dir_all(root).expect("remove runtime fixture");
    }

    #[test]
    fn default_project_state_is_stable_and_isolated_under_app_data() {
        let root = test_directory("app-data-project-state");
        let state = root.join("state");
        let first = root.join("project-a");
        let second = root.join("project-b");
        fs::create_dir_all(&first).expect("create first project");
        fs::create_dir_all(&second).expect("create second project");

        let first_state = default_workspace_path(&first, &state);
        let first_alias = default_workspace_path(&first.join("."), &state);
        let second_state = default_workspace_path(&second, &state);
        assert_eq!(first_state, first_alias);
        assert_ne!(first_state, second_state);
        assert!(first_state.starts_with(state.join(APPLICATION_DIRECTORY)));
        assert!(second_state.starts_with(state.join(APPLICATION_DIRECTORY)));

        fs::create_dir_all(&state).expect("create OS app-data parent");
        initialize_default_state(&ApplicationStateRoot::new(state.clone(), &[]), &first_state)
            .expect("initialize first project state");
        initialize_default_state(&ApplicationStateRoot::new(state.clone(), &[]), &second_state)
            .expect("initialize second project state");
        initialize_default_state(&ApplicationStateRoot::new(state.clone(), &[]), &first_state)
            .expect("cold reopen first project state");
        assert!(first_state.is_dir());
        assert!(second_state.is_dir());
        assert_ne!(
            default_endpoint(&first_state),
            default_endpoint(&second_state)
        );

        fs::remove_dir_all(root).expect("remove app-data fixture");
    }

    #[test]
    #[cfg(unix)]
    fn empty_home_default_state_creates_only_known_suffix_and_preserves_parent_modes() {
        use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};
        for (platform, suffix) in [
            ("macos", &["Library", "Application Support"][..]),
            ("linux", &[".local", "state"][..]),
            ("windows", &["AppData", "Local"][..]),
        ] {
            for existing in [false, true] {
                for home_mode in [0o700, 0o755] {
                    let root = test_directory(&format!(
                        "default-empty-home-{platform}-{existing}-{home_mode}"
                    ));
                    fs::create_dir(&root).expect("fixture home");
                    fs::set_permissions(&root, fs::Permissions::from_mode(home_mode))
                        .expect("home mode");
                    assert_eq!(
                        fs::read_dir(&root).unwrap().count(),
                        0,
                        "genuinely empty home"
                    );
                    let state = ApplicationStateRoot::new(root.clone(), suffix);
                    if existing {
                        let mut parent = root.clone();
                        for name in suffix {
                            parent.push(name);
                            fs::create_dir(&parent).expect("existing conventional parent");
                            fs::set_permissions(&parent, fs::Permissions::from_mode(0o755))
                                .unwrap();
                        }
                    }
                    // Discovery freezes canonical data identity even through macOS /var aliases.
                    let data = normalize_identity(&default_workspace_path(
                        &root.join("project"),
                        &state.path(),
                    ));
                    let paths = WorkspacePaths {
                        project: root.join("project"),
                        data: data.clone(),
                        private_application_root: Some(state.clone()),
                        endpoint: default_endpoint(&data),
                        authority_secret: data.join(AUTHORITY_FILE),
                    };
                    paths
                        .initialize()
                        .expect("first-run default state and authority");
                    paths.initialize().expect("reopen same state");
                    assert!(data.is_dir());
                    assert_eq!(fs::metadata(paths.authority_secret()).unwrap().len(), 32);
                    assert_eq!(fs::metadata(&root).unwrap().mode() & 0o777, home_mode);
                    let mut parent = root.clone();
                    for name in suffix {
                        parent.push(name);
                        assert_eq!(
                            fs::metadata(&parent).unwrap().mode() & 0o777,
                            if existing { 0o755 } else { 0o700 }
                        );
                    }
                    assert_eq!(
                        fs::metadata(state.path().join(APPLICATION_DIRECTORY))
                            .unwrap()
                            .mode()
                            & 0o777,
                        0o700
                    );
                    fs::remove_dir_all(&root).unwrap();
                }
            }
        }
    }

    #[test]
    #[cfg(unix)]
    fn legacy_default_home_app_and_project_modes_recover_on_cold_reopen() {
        use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};
        for suffix in [
            &["Library", "Application Support"][..],
            &[".local", "state"][..],
        ] {
            let home = test_directory("legacy-default-home");
            fs::create_dir(&home).unwrap();
            fs::set_permissions(&home, fs::Permissions::from_mode(0o755)).unwrap();
            let state = ApplicationStateRoot::new(home.clone(), suffix);
            let data = normalize_identity(&default_workspace_path(
                &home.join("project"),
                &state.path(),
            ));
            let paths = WorkspacePaths {
                project: home.join("project"),
                data: data.clone(),
                private_application_root: Some(state.clone()),
                endpoint: default_endpoint(&data),
                authority_secret: data.join(AUTHORITY_FILE),
            };
            paths
                .initialize()
                .expect("empty HOME creates private default state");
            let credential = fs::read(paths.authority_secret()).unwrap();
            let credential_inode = fs::metadata(paths.authority_secret()).unwrap().ino();
            let endpoint = paths.endpoint().to_path_buf();
            let application = state.path().join(APPLICATION_DIRECTORY);
            let projects = application.join(PROJECTS_DIRECTORY);
            fs::write(data.join("kept-index"), b"historical index bytes").unwrap();
            let unrelated = projects.join("reader-state");
            fs::create_dir(&unrelated).unwrap();
            fs::set_permissions(&unrelated, fs::Permissions::from_mode(0o755)).unwrap();
            for path in [&application, &projects, &data] {
                fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
            }
            for _ in 0..2 {
                paths
                    .initialize()
                    .expect("cold reopen repairs only captured application state");
                for path in [&application, &projects, &data] {
                    assert_eq!(fs::metadata(path).unwrap().mode() & 0o777, 0o700);
                }
                assert_eq!(fs::read(paths.authority_secret()).unwrap(), credential);
                assert_eq!(
                    fs::metadata(paths.authority_secret()).unwrap().ino(),
                    credential_inode
                );
                assert_eq!(paths.endpoint(), endpoint);
                assert_eq!(
                    fs::read(data.join("kept-index")).unwrap(),
                    b"historical index bytes"
                );
                assert_eq!(fs::metadata(&unrelated).unwrap().mode() & 0o777, 0o755);
                assert_eq!(fs::metadata(&home).unwrap().mode() & 0o777, 0o755);
            }
            fs::remove_dir_all(home).unwrap();
        }
    }

    #[test]
    #[cfg(unix)]
    fn default_owned_repair_refuses_links_and_foreign_application_children() {
        use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _, symlink};
        for component in 0..3 {
            let home = test_directory("default-owned-link");
            fs::create_dir(&home).unwrap();
            fs::set_permissions(&home, fs::Permissions::from_mode(0o755)).unwrap();
            let state = ApplicationStateRoot::new(home.clone(), &[]);
            let data = normalize_identity(&default_workspace_path(
                &home.join("project"),
                &state.path(),
            ));
            initialize_default_state(&state, &data).unwrap();
            let app = home.join(APPLICATION_DIRECTORY);
            let projects = app.join(PROJECTS_DIRECTORY);
            let blocked = [&app, &projects, &data][component].to_path_buf();
            let target = home.join("outside");
            fs::rename(&blocked, &target).unwrap();
            fs::set_permissions(&target, fs::Permissions::from_mode(0o755)).unwrap();
            fs::write(target.join("kept"), b"outside bytes").unwrap();
            symlink(&target, &blocked).unwrap();
            assert!(initialize_default_state(&state, &data).is_err());
            assert_eq!(fs::metadata(&target).unwrap().mode() & 0o777, 0o755);
            assert_eq!(fs::read(target.join("kept")).unwrap(), b"outside bytes");
            fs::remove_dir_all(home).unwrap();
        }
        if rustix::process::geteuid().is_root() {
            let home = test_directory("default-owned-foreign");
            fs::create_dir(&home).unwrap();
            fs::set_permissions(&home, fs::Permissions::from_mode(0o755)).unwrap();
            let state = ApplicationStateRoot::new(home.clone(), &[]);
            let data = normalize_identity(&default_workspace_path(
                &home.join("project"),
                &state.path(),
            ));
            let app = home.join(APPLICATION_DIRECTORY);
            fs::create_dir(&app).unwrap();
            fs::set_permissions(&app, fs::Permissions::from_mode(0o755)).unwrap();
            rustix::fs::chown(&app, Some(rustix::fs::Uid::from_raw(1)), None)
                .expect("foreign-owner fixture");
            assert!(initialize_default_state(&state, &data).is_err());
            assert_eq!(fs::metadata(&app).unwrap().mode() & 0o777, 0o755);
            assert!(!data.exists());
            fs::remove_dir_all(home).unwrap();
        }
    }

    #[test]
    #[cfg(unix)]
    fn default_state_refuses_file_link_and_writable_components_without_repair() {
        use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _, symlink};
        for blocker in ["file", "link", "writable"] {
            for component in [0, 1] {
                let root = test_directory(&format!("default-state-blocker-{blocker}-{component}"));
                fs::create_dir(&root).unwrap();
                fs::set_permissions(&root, fs::Permissions::from_mode(0o755)).unwrap();
                let state =
                    ApplicationStateRoot::new(root.clone(), &["Library", "Application Support"]);
                let blocked = if component == 0 {
                    root.join("Library")
                } else {
                    let parent = root.join("Library");
                    fs::create_dir(&parent).unwrap();
                    fs::set_permissions(&parent, fs::Permissions::from_mode(0o755)).unwrap();
                    parent.join("Application Support")
                };
                let outside = root.join("outside");
                match blocker {
                    "file" => fs::write(&blocked, "retained blocker").unwrap(),
                    "link" => {
                        fs::create_dir(&outside).unwrap();
                        symlink(&outside, &blocked).unwrap();
                    }
                    _ => {
                        fs::create_dir(&blocked).unwrap();
                        fs::set_permissions(&blocked, fs::Permissions::from_mode(0o775)).unwrap();
                    }
                }
                let data = default_workspace_path(&root.join("project"), &state.path());
                assert!(initialize_default_state(&state, &data).is_err());
                assert!(!data.exists());
                assert_eq!(fs::metadata(&root).unwrap().mode() & 0o777, 0o755);
                match blocker {
                    "file" => assert_eq!(fs::read_to_string(&blocked).unwrap(), "retained blocker"),
                    "link" => {
                        assert!(
                            fs::symlink_metadata(&blocked)
                                .unwrap()
                                .file_type()
                                .is_symlink()
                        );
                        assert_eq!(fs::read_dir(&outside).unwrap().count(), 0);
                    }
                    _ => assert_eq!(fs::metadata(&blocked).unwrap().mode() & 0o777, 0o775),
                }
                fs::remove_dir_all(&root).unwrap();
            }
        }
        let root = test_directory("default-state-public-anchor-writable");
        fs::create_dir(&root).unwrap();
        fs::set_permissions(&root, fs::Permissions::from_mode(0o775)).unwrap();
        let state = ApplicationStateRoot::new(root.clone(), &[".local", "state"]);
        let data = default_workspace_path(&root.join("project"), &state.path());
        assert!(initialize_default_state(&state, &data).is_err());
        assert_eq!(fs::metadata(&root).unwrap().mode() & 0o777, 0o775);
        assert_eq!(fs::read_dir(&root).unwrap().count(), 0);
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    #[cfg(unix)]
    fn remote_client_data_initialization_does_not_create_owner_authority() {
        use std::os::unix::fs::PermissionsExt;

        let root = test_directory("client-only-data");
        fs::create_dir(&root).expect("create private fixture root");
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700))
            .expect("set private fixture root");
        let data = root.join("client-data");
        let paths = WorkspacePaths::discover(Some(root.clone()), Some(data.clone()), None)
            .expect("discover client-only workspace");

        paths
            .initialize_data_directory()
            .expect("initialize private client data");
        assert!(data.is_dir());
        assert!(
            !paths.authority_secret().exists(),
            "client-only setup must not create a local-owner authority credential"
        );
        fs::remove_dir_all(root).expect("remove client-only fixture");
    }

    #[test]
    #[cfg(unix)]
    fn a_dead_endpoint_is_unlinked_and_a_live_one_is_left_alone() {
        // A bindable endpoint must fit the platform `sun_path` budget, which
        // the per-session macOS temporary directory does not leave room for.
        let root = Path::new("/tmp").join(
            test_directory("dead-endpoint")
                .file_name()
                .unwrap_or_else(|| std::ffi::OsStr::new("backend-runtime-dead-endpoint")),
        );
        fs::create_dir_all(&root).expect("create fixture root");
        let endpoint = root.join("locald.sock");
        let listener =
            std::os::unix::net::UnixListener::bind(&endpoint).expect("bind probe listener");
        assert!(
            !unlink_dead_endpoint(&endpoint),
            "a live endpoint must never be unlinked"
        );
        drop(listener);
        assert!(endpoint.exists(), "closing a listener leaves its socket");
        assert!(
            unlink_dead_endpoint(&endpoint),
            "a refusing socket is provably dead"
        );
        assert!(!endpoint.exists());
        assert!(
            !unlink_dead_endpoint(&endpoint),
            "an absent endpoint is not a removal"
        );
        fs::remove_dir_all(root).expect("remove runtime fixture");
    }

    #[cfg(unix)]
    pub(super) fn socket_test_directory(label: &str) -> PathBuf {
        // Nix and macOS can put TMPDIR beyond sun_path's limit before the
        // fixture adds its name. Keep only socket fixtures on a short root;
        // ordinary path tests still exercise the configured temporary root.
        let directory = test_directory(label);
        Path::new("/tmp").join(directory.file_name().expect("fixture directory name"))
    }

    pub(super) fn test_directory(label: &str) -> PathBuf {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        std::env::temp_dir().join(format!(
            "backend-runtime-{label}-{}-{nonce}",
            std::process::id()
        ))
    }

    /// Creates `path` as a directory only its owner can use.
    ///
    /// Private state is admitted only beneath a private parent, so a fixture
    /// that will hold it must be private itself: mode 0700 on Unix, and the
    /// protected current-user ACL on Windows, where a plain `create_dir`
    /// inherits the (shared) temporary directory's ACL.
    pub(super) fn create_private_fixture(path: &Path) {
        #[cfg(unix)]
        {
            use std::os::unix::fs::{DirBuilderExt as _, PermissionsExt as _};
            fs::DirBuilder::new()
                .mode(0o700)
                .create(path)
                .expect("private fixture directory");
            fs::set_permissions(path, fs::Permissions::from_mode(0o700))
                .expect("fixture remains private under umask");
        }
        #[cfg(windows)]
        backend_platform::durable::ensure_private_child_directory(path)
            .expect("private fixture directory");
    }

    #[test]
    fn git_root_unifies_nested_language_packages() {
        let root = test_directory("git-root");
        let nested = root.join("packages/rust/src");
        fs::create_dir_all(root.join(".git")).expect("git marker");
        fs::create_dir_all(&nested).expect("nested package");
        fs::write(root.join("packages/rust/Cargo.toml"), b"[package]\n").expect("nested manifest");

        assert_eq!(project_root_from(&nested), root);
        fs::remove_dir_all(root).expect("remove runtime fixture");
    }

    #[test]
    fn nearest_manifest_is_the_non_git_project_boundary() {
        let root = test_directory("manifest-root");
        let nested = root.join("src/deep");
        fs::create_dir_all(&nested).expect("nested source");
        fs::write(root.join("pyproject.toml"), b"[project]\n").expect("project manifest");

        assert_eq!(project_root_from(&nested), root);
        fs::remove_dir_all(root).expect("remove runtime fixture");
    }

    #[test]
    fn concurrent_initializers_publish_one_complete_authority_secret() {
        let fixture = test_directory("concurrent-secret");
        create_private_fixture(&fixture);
        let root = fixture.join("state");
        backend_platform::durable::ensure_private_directory(&root)
            .expect("create private secret parent");
        let secret = root.join("authority.secret");
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(24));
        let workers = (0..24)
            .map(|_| {
                let barrier = barrier.clone();
                let secret = secret.clone();
                thread::spawn(move || {
                    barrier.wait();
                    ensure_authority_secret(&secret)
                })
            })
            .collect::<Vec<_>>();
        for worker in workers {
            worker
                .join()
                .expect("initializer thread")
                .expect("publish or admit secret");
        }
        assert_eq!(fs::read(&secret).expect("read secret").len(), 32);
        assert_eq!(
            fs::read_dir(&root)
                .expect("read fixture")
                .filter_map(Result::ok)
                .count(),
            2
        );
        assert!(
            root.join(AUTHORITY_INITIALIZATION_LOCK).is_file(),
            "the stable initializer lease replaces transient credential retries"
        );
        fs::remove_dir_all(fixture).expect("remove runtime fixture");
    }

    #[cfg(unix)]
    #[test]
    fn authority_secret_admission_rejects_symlinks_without_replacing_them() {
        use std::os::unix::fs::symlink;

        let fixture = test_directory("secret-symlink");
        let mut builder = fs::DirBuilder::new();
        use std::os::unix::fs::DirBuilderExt as _;
        builder.mode(0o700);
        builder.create(&fixture).expect("private secret fixture");
        let state = fixture.join("state");
        backend_platform::durable::ensure_private_directory(&state)
            .expect("create private state directory");
        let target = fixture.join("target");
        fs::write(&target, [7_u8; 32]).expect("secret-shaped target");
        use std::os::unix::fs::PermissionsExt as _;
        fs::set_permissions(&target, fs::Permissions::from_mode(0o600))
            .expect("protect target fixture");
        let secret = state.join("authority.secret");
        symlink(&target, &secret).expect("credential symlink");

        assert!(ensure_authority_secret(&secret).is_err());
        assert!(
            fs::symlink_metadata(&secret)
                .expect("rejected credential remains present")
                .file_type()
                .is_symlink()
        );
        assert_eq!(
            fs::read(&target).expect("target remains unchanged"),
            [7_u8; 32]
        );

        fs::remove_dir_all(fixture).expect("remove secret fixture");
    }
}
