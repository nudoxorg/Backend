//! Zero-configuration discovery and process composition for local clients.
//!
//! Every product surface uses this crate to select the same workspace data
//! root, short Unix endpoint, authority credential, and daemon executable.
//!
//! # Endpoint identity
//!
//! Workspace ownership is a kernel lock on an inode, while the endpoint is
//! derived from a path. Those two facts must not be allowed to disagree: if
//! `<project>/.backend/v2`, `<project>/./.backend/v2`, a trailing-slash
//! spelling, and macOS `/tmp` versus `/private/tmp` hash to four sockets while
//! locking one inode, a second surface binds a socket nobody is listening on
//! and then dies on the lock it could have attached through. Every path is
//! therefore reduced to its filesystem identity by [`normalize_identity`]
//! before it is hashed, so one directory always derives one endpoint.
//!
//! # Daemon lifecycle
//!
//! [`ensure_locald`] spawns `backend-locald` detached, so the spawning CLI is
//! not the daemon's parent for lifetime purposes. Nothing reaps it explicitly.
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

use std::ffi::OsString;
use std::fmt;
use std::fs::{self, OpenOptions};
use std::io::{Read, Write};
use std::path::{Component, Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
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

const STATE_DIRECTORY: &str = ".backend/v2";
const AUTHORITY_FILE: &str = "authority.secret";
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
/// How long to keep waiting after the spawned child has exited.
///
/// A contender that won the owner lease can publish its listener shortly after
/// this child gives up, so an exit is not yet proof that no owner will appear.
/// It is proof that *this* process will not produce one, which is why the
/// window after an exit is short and the window before it is not.
const EXITED_START_GRACE: Duration = Duration::from_secs(5);
const POLL_INTERVAL: Duration = Duration::from_millis(20);
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
        let data = data
            .or_else(|| std::env::var_os(DATA_ENV).map(PathBuf::from))
            .unwrap_or_else(|| project.join(STATE_DIRECTORY));
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
        fs::create_dir_all(&self.data).map_err(RuntimeError::Io)?;
        ensure_authority_secret(&self.authority_secret)
    }
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
#[cfg(any(unix, windows))]
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
    let mut command = Command::new(&executable);
    command
        .arg("--endpoint")
        .arg(paths.endpoint())
        .arg("--workspace")
        .arg(paths.data())
        .arg("--authority-secret-file")
        .arg(paths.authority_secret())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
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
            // A concurrent caller may have won the owner lease while this
            // child was composing. Its listener can appear shortly after the
            // losing child exits, so the wait continues for a short grace
            // window rather than failing on the exit itself.
            child_exit = Some(status.code());
            deadline = Instant::now() + EXITED_START_GRACE;
        }
        if Instant::now() >= deadline {
            return child_exit.map_or_else(
                || Err(RuntimeError::StartTimeout(paths.endpoint.clone())),
                |code| Err(RuntimeError::DaemonExited(code)),
            );
        }
        thread::sleep(POLL_INTERVAL);
    }
}

/// Starts a spawned daemon outside the launching Windows console's control
/// group. Unix has no equivalent setup requirement: its detached child simply
/// inherits no terminal ownership from the parent process.
#[cfg(windows)]
fn detach(command: &mut Command) {
    use std::os::windows::process::CommandExt as _;

    const DETACHED_PROCESS: u32 = 0x0000_0008;
    const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
    command.creation_flags(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP);
}

#[cfg(not(windows))]
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

/// Returns the checkout that owns the conventional `.backend/v2` state path.
///
/// A caller can intentionally choose a state directory elsewhere, so this
/// check is limited to the layout this crate itself derives. That catches a
/// stale MCP/CLI configuration pointing at another checkout while preserving
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
    let sibling = current.with_file_name(format!("backend-locald{}", std::env::consts::EXE_SUFFIX));
    if sibling.is_file() {
        return Ok(sibling);
    }
    Err(RuntimeError::MissingExecutable(sibling))
}

fn ensure_authority_secret(path: &Path) -> Result<(), RuntimeError> {
    if path.exists() {
        return validate_authority_secret(path);
    }
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(RuntimeError::Io)?;
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
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(&temporary).map_err(RuntimeError::Io)?;
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
        Ok(()) => validate_authority_secret(path),
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            validate_authority_secret(path)
        }
        Err(error) => Err(RuntimeError::Io(error)),
    }
}

fn validate_authority_secret(path: &Path) -> Result<(), RuntimeError> {
    let metadata = fs::metadata(path).map_err(RuntimeError::Io)?;
    if !metadata.is_file() || metadata.len() != 32 {
        Err(RuntimeError::InvalidCredential(path.to_path_buf()))
    } else {
        #[cfg(windows)]
        {
            use backend_platform::win32::identity;
            let owner = identity::file_owner(path).map_err(RuntimeError::Io)?;
            if !identity::is_owned_by_current_user(&owner).map_err(RuntimeError::Io)? {
                return Err(RuntimeError::InvalidCredential(path.to_path_buf()));
            }
        }
        Ok(())
    }
}

/// Local composition failure.
#[derive(Debug)]
pub enum RuntimeError {
    /// A filesystem or process operation failed.
    Io(std::io::Error),
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
    /// The daemon exited before accepting clients.
    DaemonExited(Option<i32>),
    /// The daemon did not become ready before the bounded deadline.
    StartTimeout(PathBuf),
    /// Automatic local composition is unavailable on this platform.
    Unsupported,
}

impl fmt::Display for RuntimeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(error) => write!(formatter, "local runtime I/O failed: {error}"),
            Self::InvalidPath(message) => formatter.write_str(message),
            Self::WorkspaceProjectMismatch {
                project,
                workspace,
                workspace_project,
            } => write!(
                formatter,
                "workspace {} belongs to checkout {}, but the configured project is {}; choose --workspace {}/.backend/v2 for that project or remove the workspace override",
                workspace.display(),
                workspace_project.display(),
                project.display(),
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
                "backend-locald was not found at {}; install all backend binaries or set {LOCALD_BIN_ENV}",
                path.display()
            ),
            Self::Spawn { executable, source } => {
                write!(formatter, "start {}: {source}", executable.display())
            }
            Self::DaemonExited(code) => {
                write!(formatter, "backend-locald exited during startup ({code:?})")
            }
            Self::StartTimeout(path) => {
                write!(formatter, "backend-locald did not open {}", path.display())
            }
            Self::Unsupported => {
                formatter.write_str("automatic local runtime requires Unix or Windows")
            }
        }
    }
}

impl std::error::Error for RuntimeError {}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use super::*;

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
        let workspace = second.join(STATE_DIRECTORY);
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
        assert!(message.contains("--workspace"), "{message}");
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
        assert_eq!(
            discovered.data(),
            data.canonicalize().expect("canonical data")
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

    fn test_directory(label: &str) -> PathBuf {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        std::env::temp_dir().join(format!(
            "backend-runtime-{label}-{}-{nonce}",
            std::process::id()
        ))
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
        let root = test_directory("concurrent-secret");
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
            1
        );
        fs::remove_dir_all(root).expect("remove runtime fixture");
    }
}
