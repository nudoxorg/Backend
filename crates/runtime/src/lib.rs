//! Zero-configuration discovery and process composition for local clients.
//!
//! Every product surface uses this crate to select the same workspace data
//! root, short Unix endpoint, authority credential, and daemon executable.
//! Deriving paths performs no I/O; startup is an explicit fallible transition.
#![forbid(unsafe_code)]

use std::fmt;
use std::fs::{self, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
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

const STATE_DIRECTORY: &str = ".backend/v2";
const AUTHORITY_FILE: &str = "authority.secret";
const START_TIMEOUT: Duration = Duration::from_secs(5);
const POLL_INTERVAL: Duration = Duration::from_millis(20);
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
        let project = project.canonicalize().unwrap_or(project);
        let data = data
            .or_else(|| std::env::var_os(DATA_ENV).map(PathBuf::from))
            .unwrap_or_else(|| project.join(STATE_DIRECTORY));
        if data.as_os_str().is_empty() {
            return Err(RuntimeError::InvalidPath("data path is empty"));
        }
        let endpoint = endpoint
            .or_else(|| std::env::var_os(ENDPOINT_ENV).map(PathBuf::from))
            .unwrap_or_else(|| default_endpoint(&data));
        if endpoint.as_os_str().is_empty() {
            return Err(RuntimeError::InvalidPath("endpoint path is empty"));
        }
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

/// Ensures the shared local daemon is accepting connections and returns its
/// selected endpoint.
///
/// Concurrent callers may both attempt startup. The daemon owner lock admits
/// one winner and every caller converges on the same socket.
///
/// # Errors
/// Returns an error when setup, executable discovery, process startup, or the
/// bounded readiness wait fails.
#[cfg(unix)]
pub fn ensure_locald(paths: &WorkspacePaths) -> Result<PathBuf, RuntimeError> {
    use std::os::unix::net::UnixStream;

    if UnixStream::connect(paths.endpoint()).is_ok() {
        return Ok(paths.endpoint().to_path_buf());
    }
    paths.initialize()?;
    if let Some(parent) = paths.endpoint().parent() {
        fs::create_dir_all(parent).map_err(RuntimeError::Io)?;
    }
    let executable = locald_executable()?;
    let mut child = Command::new(&executable)
        .arg("--endpoint")
        .arg(paths.endpoint())
        .arg("--workspace")
        .arg(paths.data())
        .arg("--authority-secret-file")
        .arg(paths.authority_secret())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|source| RuntimeError::Spawn { executable, source })?;
    let deadline = Instant::now() + START_TIMEOUT;
    let mut child_exit = None;
    loop {
        if UnixStream::connect(paths.endpoint()).is_ok() {
            return Ok(paths.endpoint().to_path_buf());
        }
        if child_exit.is_none()
            && let Some(status) = child.try_wait().map_err(RuntimeError::Io)?
        {
            // A concurrent caller may have won the owner lease while this
            // child was composing. Its listener can appear shortly after the
            // losing child exits, so every contender shares the same bounded
            // readiness deadline instead of failing early.
            child_exit = Some(status.code());
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

/// Reports that automatic local daemon composition is Unix-only.
#[cfg(not(unix))]
pub fn ensure_locald(_paths: &WorkspacePaths) -> Result<PathBuf, RuntimeError> {
    Err(RuntimeError::Unsupported)
}

fn default_endpoint(data: &Path) -> PathBuf {
    let digest = blake3::hash(data.as_os_str().as_encoded_bytes());
    let mut short = String::with_capacity(24);
    for byte in &digest.as_bytes()[..12] {
        use std::fmt::Write as _;
        let _ = write!(short, "{byte:02x}");
    }
    #[cfg(unix)]
    let socket_directory = PathBuf::from("/tmp");
    #[cfg(not(unix))]
    let socket_directory = std::env::temp_dir();
    socket_directory.join(format!("backend-v2-{short}.sock"))
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
    fs::File::open("/dev/urandom")
        .and_then(|mut source| source.read_exact(&mut bytes))
        .map_err(RuntimeError::Io)?;
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
    if metadata.is_file() && metadata.len() == 32 {
        Ok(())
    } else {
        Err(RuntimeError::InvalidCredential(path.to_path_buf()))
    }
}

/// Local composition failure.
#[derive(Debug)]
pub enum RuntimeError {
    /// A filesystem or process operation failed.
    Io(std::io::Error),
    /// One selected path was empty.
    InvalidPath(&'static str),
    /// The authority credential was not one private 32-byte file.
    InvalidCredential(PathBuf),
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
            Self::InvalidCredential(path) => write!(
                formatter,
                "authority credential {} must be one 32-byte file",
                path.display()
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
            Self::Unsupported => formatter.write_str("automatic local runtime requires Unix"),
        }
    }
}

impl std::error::Error for RuntimeError {}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn derived_endpoints_are_short_stable_and_workspace_specific() {
        let first = default_endpoint(Path::new("/a/very/long/project/data/path"));
        let repeated = default_endpoint(Path::new("/a/very/long/project/data/path"));
        let other = default_endpoint(Path::new("/another/project"));
        assert_eq!(first, repeated);
        assert_ne!(first, other);
        assert!(first.as_os_str().len() < 80);
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
