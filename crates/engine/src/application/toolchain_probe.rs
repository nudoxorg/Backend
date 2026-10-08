//! Bounded admission probe for explicit native toolchain executables.
//!
//! Version bytes are recipe authority, so startup must obtain them without an ambient executable
//! search and without allowing a child to own unbounded output or time. This module owns that one
//! process invariant; language adapters receive only the resulting absolute executable and typed
//! identity.

use std::{
    collections::TryReserveError,
    ffi::OsString,
    fmt,
    io::{self, Read},
    num::NonZeroUsize,
    ops::Deref,
    path::{Path, PathBuf},
    process::{Child, Command, ExitStatus, Stdio},
    sync::mpsc::{Sender, TryRecvError, channel},
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

use crate::driver::ToolchainResolutionError;
use backend_semantic::vocabulary::{NativeTool, NativeWorker, NativeWorkerPanic};
use thiserror::Error;

#[cfg(test)]
std::thread_local! {
    static EXECUTABLE_CONTENT_HASH_BYTES: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

#[cfg(test)]
pub(crate) fn executable_content_hash_bytes_for_test() -> u64 {
    EXECUTABLE_CONTENT_HASH_BYTES.with(std::cell::Cell::get)
}

/// Stable identity for the direct compiler and version-probe environment contract.
///
/// Native lower-IR compiler children start with no inherited variables. The
/// .NET tool receives two fixed CLI controls for both probing and compilation;
/// an actual build also gets its CLI/cache directories from the request's
/// scratch lease.
/// Keep this byte string stable unless that contract changes; recipe identity
/// includes it so a policy change cannot reuse an older compiler result.
pub(crate) const NATIVE_COMPILER_ENVIRONMENT_POLICY_ID: &[u8] =
    b"native-compiler-environment/empty-v1\0";

/// The shared closed-environment policy for direct native compiler children.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) struct NativeCompilerEnvironment;

impl NativeCompilerEnvironment {
    /// Removes every inherited process variable before any admitted adapter
    /// configuration is applied, then adds the tool family's exact controls.
    pub(crate) fn apply(command: &mut Command, tool: NativeTool) {
        command.env_clear();
        if tool == NativeTool::CSharpCompiler {
            command
                .env("DOTNET_CLI_TELEMETRY_OPTOUT", "1")
                .env("DOTNET_SKIP_FIRST_TIME_EXPERIENCE", "1");
        }
    }
}

const READ_CHUNK_BYTES: usize = 4096;
const PROBE_POLL_INTERVAL: Duration = Duration::from_millis(2);
pub(crate) const MAX_INVOCATION_EXECUTABLE_BYTES: u64 = 512 * 1024 * 1024;
const MAX_TYPESCRIPT_MODULE_FILES: usize = 256;
const MAX_TYPESCRIPT_MODULE_ENTRIES: usize = 512;
const MAX_TYPESCRIPT_MODULE_BYTES: u64 = 96 * 1024 * 1024;
const MAX_TYPESCRIPT_MODULE_DEPTH: usize = 32;

/// Hashes one already-selected executable without allocating its full image.
///
/// Callers admit only canonical absolute paths. The digest is over the bytes read from one open
/// regular-file handle. Before returning, this checks that the handle's identity and length stayed
/// stable and that the selected pathname still resolves to that same file object. This detects
/// ordinary pathname replacement and concurrent writes; it cannot eliminate the race between the
/// final pathname check and a later process opening the path.
pub(crate) fn executable_content_digest(path: &Path) -> io::Result<[u8; 32]> {
    if !path.is_absolute() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "selected executable path is not absolute",
        ));
    }
    let canonical = std::fs::canonicalize(path)?;
    if canonical != path {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "selected executable path is not canonical",
        ));
    }
    let mut file = std::fs::File::open(path)?;
    let before = file.metadata()?;
    if !before.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "selected executable is not a regular file",
        ));
    }
    if before.len() > MAX_INVOCATION_EXECUTABLE_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "selected executable exceeds the admitted snapshot bound",
        ));
    }
    let before_identity = invocation_file_identity(&before, path, Some(&file))?;
    let before_modified = before.modified().ok();
    let mut hasher = blake3::Hasher::new();
    let mut total = 0_u64;
    let mut buffer = [0_u8; READ_CHUNK_BYTES];
    loop {
        let count = file.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        total = total.checked_add(count as u64).ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidData, "executable length overflow")
        })?;
        if total > MAX_INVOCATION_EXECUTABLE_BYTES {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "selected executable exceeds the admitted snapshot bound",
            ));
        }
        hasher.update(&buffer[..count]);
        #[cfg(test)]
        EXECUTABLE_CONTENT_HASH_BYTES.with(|bytes| {
            bytes.set(bytes.get().saturating_add(count as u64));
        });
    }
    let after = file.metadata()?;
    let path_metadata = std::fs::metadata(path)?;
    if total != before.len()
        || after.len() != before.len()
        || invocation_file_identity(&after, path, Some(&file))? != before_identity
        || invocation_file_identity(&path_metadata, path, None)? != before_identity
        || before_modified != after.modified().ok()
        || std::fs::canonicalize(path)? != path
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "selected executable changed while it was being witnessed",
        ));
    }
    Ok(*hasher.finalize().as_bytes())
}

/// Digests the complete selected `typescript` package under a canonical `node_modules` root.
///
/// The inventory is bounded by file count, total bytes, and directory depth. Symlinks and special
/// files are refused, so the package tree cannot redirect module loads outside this selected root.
pub(crate) fn typescript_module_closure_digest(module_root: &Path) -> io::Result<[u8; 32]> {
    if !module_root.is_absolute() || std::fs::canonicalize(module_root)? != module_root {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "selected TypeScript module root is not canonical and absolute",
        ));
    }
    let package_root = module_root.join("typescript");
    let canonical_package = std::fs::canonicalize(&package_root)?;
    if canonical_package != package_root || !canonical_package.starts_with(module_root) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "selected TypeScript package escapes its admitted module root",
        ));
    }
    let mut entries = Vec::new();
    let mut file_count = 0_usize;
    let mut total_bytes = 0_u64;
    collect_typescript_module_entries(
        &package_root,
        &package_root,
        0,
        &mut entries,
        &mut file_count,
        &mut total_bytes,
    )?;
    entries.retain(|entry| entry.kind == b'f');
    entries.sort_unstable_by(|left, right| left.relative_path.cmp(&right.relative_path));

    typescript_module_files_digest(entries.iter().map(|entry| {
        (
            entry.relative_path.as_path(),
            entry.length,
            entry.digest.unwrap_or([0; 32]),
        )
    }))
}

/// Hashes ordered file-content facts for one captured TypeScript package.
pub(crate) fn typescript_module_files_digest<'path>(
    files: impl IntoIterator<Item = (&'path Path, u64, [u8; 32])>,
) -> io::Result<[u8; 32]> {
    let mut files = files
        .into_iter()
        .map(|(path, length, digest)| {
            if path.is_absolute()
                || path
                    .components()
                    .any(|component| !matches!(component, std::path::Component::Normal(_)))
            {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "TypeScript package file path is not normalized and relative",
                ));
            }
            Ok((path.to_path_buf(), length, digest))
        })
        .collect::<io::Result<Vec<_>>>()?;
    files.sort_unstable_by(|left, right| left.0.cmp(&right.0));
    if files.len() > MAX_TYPESCRIPT_MODULE_FILES
        || files.windows(2).any(|pair| pair[0].0 == pair[1].0)
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "selected TypeScript package has duplicate or excessive file inputs",
        ));
    }
    let mut total_bytes = 0_u64;
    let mut digest = blake3::Hasher::new();
    digest.update(b"backend.typescript.module-closure.v1\0");
    digest.update(&(files.len() as u64).to_be_bytes());
    for (relative, length, content_digest) in files {
        total_bytes = total_bytes.checked_add(length).ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidData, "module package size overflow")
        })?;
        if total_bytes > MAX_TYPESCRIPT_MODULE_BYTES {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "selected TypeScript package exceeds the admitted byte bound",
            ));
        }
        let path = relative.as_os_str().as_encoded_bytes();
        digest.update(&(path.len() as u64).to_be_bytes());
        digest.update(path);
        digest.update(&length.to_be_bytes());
        digest.update(&content_digest);
    }
    Ok(*digest.finalize().as_bytes())
}

struct TypeScriptModuleEntry {
    relative_path: PathBuf,
    kind: u8,
    length: u64,
    digest: Option<[u8; 32]>,
}

fn collect_typescript_module_entries(
    package_root: &Path,
    directory: &Path,
    depth: usize,
    entries: &mut Vec<TypeScriptModuleEntry>,
    file_count: &mut usize,
    total_bytes: &mut u64,
) -> io::Result<()> {
    if depth > MAX_TYPESCRIPT_MODULE_DEPTH {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "selected TypeScript package exceeds the admitted directory depth",
        ));
    }
    let before = std::fs::symlink_metadata(directory)?;
    if !before.is_dir() || before.file_type().is_symlink() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "selected TypeScript package contains a non-regular directory",
        ));
    }
    let before_identity = invocation_file_identity(&before, directory, None)?;
    let mut children = std::fs::read_dir(directory)?
        .map(|entry| entry.map(|entry| entry.path()))
        .collect::<io::Result<Vec<_>>>()?;
    children.sort_unstable();
    for path in children {
        let metadata = std::fs::symlink_metadata(&path)?;
        if metadata.file_type().is_symlink() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "selected TypeScript package contains a symlink",
            ));
        }
        if metadata.is_dir() {
            entries.push(TypeScriptModuleEntry {
                relative_path: path
                    .strip_prefix(package_root)
                    .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "module path escape"))?
                    .to_path_buf(),
                kind: b'd',
                length: 0,
                digest: None,
            });
            if entries.len() > MAX_TYPESCRIPT_MODULE_ENTRIES {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "selected TypeScript package exceeds the admitted entry count",
                ));
            }
            collect_typescript_module_entries(
                package_root,
                &path,
                depth + 1,
                entries,
                file_count,
                total_bytes,
            )?;
        } else if metadata.is_file() {
            *file_count = file_count.checked_add(1).ok_or_else(|| {
                io::Error::new(io::ErrorKind::InvalidData, "module file count overflow")
            })?;
            if *file_count > MAX_TYPESCRIPT_MODULE_FILES {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "selected TypeScript package exceeds the admitted file count",
                ));
            }
            *total_bytes = total_bytes.checked_add(metadata.len()).ok_or_else(|| {
                io::Error::new(io::ErrorKind::InvalidData, "module package size overflow")
            })?;
            if *total_bytes > MAX_TYPESCRIPT_MODULE_BYTES {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "selected TypeScript package exceeds the admitted byte bound",
                ));
            }
            let file_digest = executable_content_digest(&path)?;
            entries.push(TypeScriptModuleEntry {
                relative_path: path
                    .strip_prefix(package_root)
                    .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "module path escape"))?
                    .to_path_buf(),
                kind: b'f',
                length: metadata.len(),
                digest: Some(file_digest),
            });
            if entries.len() > MAX_TYPESCRIPT_MODULE_ENTRIES {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "selected TypeScript package exceeds the admitted entry count",
                ));
            }
        } else {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "selected TypeScript package contains a special file",
            ));
        }
    }
    let after = std::fs::symlink_metadata(directory)?;
    if invocation_file_identity(&after, directory, None)? != before_identity
        || std::fs::canonicalize(directory)? != directory
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "selected TypeScript package directory changed while witnessed",
        ));
    }
    Ok(())
}

#[cfg(windows)]
pub(crate) type FileObjectId = u128;
#[cfg(not(windows))]
pub(crate) type FileObjectId = u64;

#[cfg(unix)]
fn invocation_file_identity(
    metadata: &std::fs::Metadata,
    _path: &Path,
    _file: Option<&std::fs::File>,
) -> io::Result<(u64, FileObjectId)> {
    use std::os::unix::fs::MetadataExt;
    Ok((metadata.dev(), metadata.ino()))
}

/// Returns the identity of one already-selected regular file without rereading its contents.
/// Callers use this only inside a package lease whose full content witness is revalidated at
/// package boundaries.
pub(crate) fn executable_object_identity(path: &Path) -> io::Result<(u64, FileObjectId)> {
    if !path.is_absolute() || std::fs::canonicalize(path)? != path {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "selected executable path is no longer canonical and absolute",
        ));
    }
    let metadata = std::fs::metadata(path)?;
    if !metadata.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "selected executable is no longer a regular file",
        ));
    }
    invocation_file_identity(&metadata, path, None)
}

/// Returns the identity of one already-selected compiler package directory.
pub(crate) fn compiler_directory_object_identity(path: &Path) -> io::Result<(u64, FileObjectId)> {
    if !path.is_absolute() || std::fs::canonicalize(path)? != path {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "selected compiler directory is no longer canonical and absolute",
        ));
    }
    let metadata = std::fs::metadata(path)?;
    if !metadata.is_dir() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "selected compiler package is no longer a directory",
        ));
    }
    invocation_file_identity(&metadata, path, None)
}

#[cfg(windows)]
fn invocation_file_identity(
    _metadata: &std::fs::Metadata,
    path: &Path,
    file: Option<&std::fs::File>,
) -> io::Result<(u64, FileObjectId)> {
    let identity = match file {
        Some(file) => backend_platform::FileIdentity::of_file(file)?,
        None => backend_platform::FileIdentity::of_path_nofollow(path)?,
    };
    Ok(identity.parts())
}

#[cfg(not(any(unix, windows)))]
fn invocation_file_identity(
    _metadata: &std::fs::Metadata,
    _path: &Path,
    _file: Option<&std::fs::File>,
) -> io::Result<(u64, FileObjectId)> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "stable file identity is unavailable on this platform",
    ))
}

/// Immutable validated bounds for one native version probe.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ToolchainProbeLimitsView {
    /// Maximum wall-clock duration owned by the child transaction.
    pub timeout: Duration,
    /// Maximum independently retained bytes for stdout and stderr.
    pub maximum_stream_bytes: NonZeroUsize,
}

/// Validated probe bounds exposed through an immutable view.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ToolchainProbeLimits {
    view: ToolchainProbeLimitsView,
}

impl ToolchainProbeLimits {
    /// Admits nonzero time and stream bounds.
    pub const fn new(
        timeout: Duration,
        maximum_stream_bytes: NonZeroUsize,
    ) -> Result<Self, ToolchainProbeLimitError> {
        if timeout.is_zero() {
            return Err(ToolchainProbeLimitError::ZeroTimeout);
        }
        Ok(Self {
            view: ToolchainProbeLimitsView {
                timeout,
                maximum_stream_bytes,
            },
        })
    }
}

impl Deref for ToolchainProbeLimits {
    type Target = ToolchainProbeLimitsView;

    fn deref(&self) -> &Self::Target {
        &self.view
    }
}

/// Rejection while validating version-probe resource bounds.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum ToolchainProbeLimitError {
    /// An immediate deadline cannot establish executable authority.
    #[error("toolchain version probe timeout must be nonzero")]
    ZeroTimeout,
}

/// Primary process outcome that may also be retained by a cleanup or stream fault.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ToolchainProbePrimary {
    /// A named child stream crossed the admitted byte cap.
    OutputLimit {
        /// Stream which first crossed the bound.
        worker: NativeWorker,
        /// Bytes observed through the crossing read.
        observed: usize,
        /// Admitted maximum for each stream.
        maximum: usize,
    },
    /// The child remained live past its admitted interval.
    Deadline {
        /// Exact configured interval.
        timeout: Duration,
    },
    /// The caller's cancellation flag stopped the child transaction.
    Cancelled,
}

/// Closed cleanup operation that failed after a probe had already reached a terminal.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ToolchainProbeCleanupAction {
    /// Terminating the still-live process failed.
    Terminate,
    /// Reaping the terminated or already-exited process failed.
    Reap,
}

/// Bytes of each stream quoted by a failed probe's message.
const FAILURE_EXCERPT_BYTES: usize = 512;

/// Quotes the beginning of a failed probe's two bounded streams.
///
/// Only the display is shortened; the error itself retains the complete bounded streams.
struct FailureStreams<'streams> {
    stdout: &'streams [u8],
    stderr: &'streams [u8],
}

impl<'streams> FailureStreams<'streams> {
    const fn new(stdout: &'streams [u8], stderr: &'streams [u8]) -> Self {
        Self { stdout, stderr }
    }
}

impl fmt::Display for FailureStreams<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (name, bytes) in [("stderr", self.stderr), ("stdout", self.stdout)] {
            if bytes.is_empty() {
                continue;
            }
            let shown = &bytes[..bytes.len().min(FAILURE_EXCERPT_BYTES)];
            // `Debug` keeps a multi-line quotation on one line and escapes control bytes.
            write!(
                formatter,
                "; {name}: {:?}",
                String::from_utf8_lossy(shown).trim_end()
            )?;
            if shown.len() < bytes.len() {
                write!(
                    formatter,
                    " (first {} of {} bytes)",
                    shown.len(),
                    bytes.len()
                )?;
            }
        }
        Ok(())
    }
}

/// Exact bounded-reader failure for one named child stream.
#[derive(Debug, Error)]
pub enum ToolchainProbeStreamError {
    /// Exact retained-output allocation failed before reading.
    #[error("{worker:?} version-probe output allocation failed")]
    Allocation {
        /// Named child stream.
        worker: NativeWorker,
        /// Exact standard allocation cause.
        #[source]
        source: TryReserveError,
    },
    /// The operating system rejected a stream read.
    #[error("{worker:?} version-probe read failed")]
    Read {
        /// Named child stream.
        worker: NativeWorker,
        /// Exact operating-system cause.
        #[source]
        source: io::Error,
    },
    /// The bounded stream worker panicked with its original supported payload facts.
    #[error("version-probe stream worker panicked: {cause}")]
    WorkerPanic {
        /// Named worker and bounded original payload.
        #[source]
        cause: NativeWorkerPanic,
    },
}

/// Exact terminal while establishing one explicit native-tool version identity.
#[derive(Debug, Error)]
pub enum ToolchainProbeError {
    /// A global TypeScript script was selected without an admitted Node runtime.
    #[error("TypeScript compiler script {compiler:?} has no admitted Node interpreter")]
    TypeScriptInterpreterUnavailable {
        /// Exact TypeScript script selected by the host.
        compiler: PathBuf,
    },
    /// The selected TypeScript script has no admitted TypeScript module package root.
    #[error("TypeScript compiler script {compiler:?} has no admitted module package root")]
    TypeScriptModuleRootUnavailable {
        /// Exact TypeScript script selected by the host.
        compiler: PathBuf,
    },
    /// The selected compiler entry does not belong to the selected TypeScript module root.
    #[error("TypeScript compiler script {compiler:?} does not match module root {module_root:?}")]
    TypeScriptModuleEntryMismatch {
        /// Exact TypeScript script selected by the host.
        compiler: PathBuf,
        /// Exact module root selected by the host.
        module_root: PathBuf,
    },
    /// Relative execution would consult ambient process state.
    #[error("{tool:?} version probe executable is relative: {executable:?}")]
    RelativeExecutable {
        /// Closed tool family.
        tool: NativeTool,
        /// Rejected caller path.
        executable: PathBuf,
    },
    /// An invocation file could not be captured under the executable snapshot bound.
    #[error("could not witness selected TypeScript {role:?} at {path:?}")]
    ExecutableWitness {
        /// Which exact invocation input could not be witnessed.
        role: ToolchainProbeFileRole,
        /// Exact absolute invocation path.
        path: PathBuf,
        /// Original filesystem or bound failure.
        #[source]
        source: io::Error,
    },
    /// An invocation file changed between version admission and snapshot completion.
    #[error("selected TypeScript {role:?} changed during admission at {path:?}")]
    ExecutableChanged {
        /// Which exact invocation input changed.
        role: ToolchainProbeFileRole,
        /// Exact absolute invocation path.
        path: PathBuf,
    },
    /// The absolute executable could not be started.
    #[error("could not start {tool:?} version probe at {executable:?}")]
    Spawn {
        /// Closed tool family.
        tool: NativeTool,
        /// Exact selected absolute path.
        executable: PathBuf,
        /// Original operating-system cause.
        #[source]
        source: io::Error,
    },
    /// A compiler-owner probe worker could not be created before a process was started.
    #[error("could not start the bounded probe worker for {tool:?}")]
    ProbeWorkerSpawn {
        /// Closed tool family.
        tool: NativeTool,
        /// Original operating-system cause.
        #[source]
        source: io::Error,
    },
    /// A named reader thread could not be created.
    #[error("could not start {worker:?} reader for {tool:?} version probe")]
    ReaderSpawn {
        /// Closed tool family.
        tool: NativeTool,
        /// Named reader.
        worker: NativeWorker,
        /// Original operating-system cause.
        #[source]
        source: io::Error,
    },
    /// A piped stream promised to the child transaction was absent.
    #[error("{tool:?} version probe did not expose {worker:?}")]
    MissingStream {
        /// Closed tool family.
        tool: NativeTool,
        /// Missing named stream.
        worker: NativeWorker,
    },
    /// Polling the child process failed.
    #[error("could not observe {tool:?} version probe")]
    Wait {
        /// Closed tool family.
        tool: NativeTool,
        /// Original operating-system cause.
        #[source]
        source: io::Error,
    },
    /// Process cleanup failed while retaining the exact terminal that triggered cleanup.
    #[error("could not {action:?} {tool:?} version probe after {preceding}")]
    Cleanup {
        /// Closed tool family.
        tool: NativeTool,
        /// Closed cleanup operation.
        action: ToolchainProbeCleanupAction,
        /// Complete earlier probe terminal.
        preceding: Box<ToolchainProbeError>,
        /// Original operating-system cause.
        #[source]
        source: io::Error,
    },
    /// A bounded stream failed, retaining any process terminal that caused cancellation.
    #[error("{tool:?} version probe stream failed: {source}")]
    Stream {
        /// Closed tool family.
        tool: NativeTool,
        /// Complete earlier process or cleanup terminal, when one existed.
        preceding: Option<Box<ToolchainProbeError>>,
        /// Complete original stream cause.
        #[source]
        source: ToolchainProbeStreamError,
    },
    /// Both independent bounded streams failed and both exact causes are retained.
    #[error("{tool:?} version probe stdout and stderr streams both failed")]
    Streams {
        /// Closed tool family.
        tool: NativeTool,
        /// Complete earlier process or cleanup terminal, when one existed.
        preceding: Option<Box<ToolchainProbeError>>,
        /// Complete standard-output reader cause.
        stdout: ToolchainProbeStreamError,
        /// Complete standard-error reader cause.
        stderr: ToolchainProbeStreamError,
    },
    /// The probe crossed an admitted process resource bound and was reaped.
    #[error("{tool:?} version probe rejected by {primary:?}")]
    Bounded {
        /// Closed tool family.
        tool: NativeTool,
        /// Exact first deadline or output-cap terminal.
        primary: ToolchainProbePrimary,
    },
    /// Internal child observation completed without a process status or another terminal.
    #[error("{tool:?} version probe completed without an exit status")]
    MissingStatus {
        /// Closed tool family.
        tool: NativeTool,
    },
    /// The executable exited unsuccessfully with both complete bounded streams retained.
    ///
    /// The message quotes the start of each stream so the cause of a refusal is visible without a
    /// debugger: a tool that rejects its argument says so on stderr.
    #[error(
        "{tool:?} version probe exited with {status}{}",
        FailureStreams::new(.stdout, .stderr)
    )]
    Exit {
        /// Closed tool family.
        tool: NativeTool,
        /// Exact process status.
        status: ExitStatus,
        /// Complete bounded stdout bytes.
        stdout: Box<[u8]>,
        /// Complete bounded stderr bytes.
        stderr: Box<[u8]>,
    },
    /// A successful executable provided no identity bytes on either stream.
    #[error("{tool:?} version probe returned empty stdout and stderr")]
    Empty {
        /// Closed tool family.
        tool: NativeTool,
    },
    /// The admitted absolute path could not be rebound to its identity.
    #[error("{tool:?} version identity could not be bound to its executable")]
    Resolution {
        /// Closed tool family.
        tool: NativeTool,
        /// Exact toolchain admission cause.
        #[source]
        source: ToolchainResolutionError,
    },
}

/// Exact files and module closure that authorize an interpreted TypeScript launch.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ToolchainProbeFileRole {
    /// TypeScript compiler JavaScript source.
    Script,
    /// Node executable used to interpret that source.
    Interpreter,
    /// Compiler package's bounded module and declaration tree.
    CompilerModule,
}

pub(crate) fn probe_version(
    tool: NativeTool,
    executable: &Path,
    limits: ToolchainProbeLimits,
) -> Result<Box<[u8]>, ToolchainProbeError> {
    probe_command(tool, executable, &[version_argument(tool)], limits)
}

pub(crate) fn probe_command(
    tool: NativeTool,
    executable: &Path,
    arguments: &[&str],
    limits: ToolchainProbeLimits,
) -> Result<Box<[u8]>, ToolchainProbeError> {
    if !executable.is_absolute() {
        return Err(ToolchainProbeError::RelativeExecutable {
            tool,
            executable: executable.to_path_buf(),
        });
    }
    let mut command = Command::new(executable);
    NativeCompilerEnvironment::apply(&mut command, tool);
    command.args(arguments);
    probe_prepared_command(tool, executable, command, limits, None)
}

/// Probes the package-owned TypeScript JavaScript entry through one exact Node executable.
///
/// `tsc`'s package-manager link and its `#!/usr/bin/env node` shebang are never executed by the
/// operating system. Node receives the canonical package script as an argument. The only child
/// search path is Node's own directory, which keeps any nested `env node` invocation bound to the
/// same admitted runtime while preserving the closed environment contract.
pub(crate) fn probe_typescript_script_with_node(
    compiler_script: &Path,
    node: &Path,
    limits: ToolchainProbeLimits,
) -> Result<Box<[u8]>, ToolchainProbeError> {
    let tool = NativeTool::TypeScriptCompiler;
    for executable in [compiler_script, node] {
        if !executable.is_absolute() {
            return Err(ToolchainProbeError::RelativeExecutable {
                tool,
                executable: executable.to_path_buf(),
            });
        }
    }
    let mut command = Command::new(node);
    NativeCompilerEnvironment::apply(&mut command, tool);
    if let Some(node_directory) = node.parent() {
        command.env("PATH", node_directory);
    }
    command.arg(compiler_script).arg("--version");
    probe_prepared_command(tool, node, command, limits, None)
}

/// Runs the admitted TypeScript compiler API bridge through the exact Node
/// executable under bounded output, deadline, and caller cancellation.
pub(crate) fn run_typescript_program_bridge(
    node: &Path,
    script: &str,
    arguments: &[OsString],
    working_directory: &Path,
    limits: ToolchainProbeLimits,
    cancelled: &std::sync::atomic::AtomicBool,
) -> Result<Box<[u8]>, ToolchainProbeError> {
    let tool = NativeTool::TypeScriptCompiler;
    if !node.is_absolute() {
        return Err(ToolchainProbeError::RelativeExecutable {
            tool,
            executable: node.to_path_buf(),
        });
    }
    let mut command = Command::new(node);
    NativeCompilerEnvironment::apply(&mut command, tool);
    if let Some(node_directory) = node.parent() {
        command.env("PATH", node_directory);
    }
    command
        .arg("-e")
        .arg(script)
        .args(arguments)
        .current_dir(working_directory);
    probe_prepared_command(tool, node, command, limits, Some(cancelled))
}

/// Admits a TypeScript JavaScript entry, the exact Node interpreter, and the bounded package tree
/// that supplies the compiler modules.
///
/// The selected files and complete package closure are snapshotted before probing and rechecked
/// after both version commands finish. The returned identities remain distinct: callers can report
/// the compiler's version while binding the interpreter and module package into the invocation
/// recipe.
pub(crate) fn admit_typescript_script_invocation(
    compiler_script: &Path,
    node: &Path,
    module_root: &Path,
    limits: ToolchainProbeLimits,
) -> Result<(Box<[u8]>, Box<[u8]>, [u8; 32], [u8; 32], [u8; 32]), ToolchainProbeError> {
    let tool = NativeTool::TypeScriptCompiler;
    if compiler_script != module_root.join("typescript/bin/tsc") {
        return Err(ToolchainProbeError::TypeScriptModuleEntryMismatch {
            compiler: compiler_script.to_path_buf(),
            module_root: module_root.to_path_buf(),
        });
    }
    let module_before = typescript_module_closure_digest(module_root).map_err(|source| {
        ToolchainProbeError::ExecutableWitness {
            role: ToolchainProbeFileRole::CompilerModule,
            path: module_root.join("typescript"),
            source,
        }
    })?;
    let script_before = executable_content_digest(compiler_script).map_err(|source| {
        ToolchainProbeError::ExecutableWitness {
            role: ToolchainProbeFileRole::Script,
            path: compiler_script.to_path_buf(),
            source,
        }
    })?;
    let interpreter_before = executable_content_digest(node).map_err(|source| {
        ToolchainProbeError::ExecutableWitness {
            role: ToolchainProbeFileRole::Interpreter,
            path: node.to_path_buf(),
            source,
        }
    })?;
    let interpreter_version = probe_version(tool, node, limits)?;
    let script_version = probe_typescript_script_with_node(compiler_script, node, limits)?;
    let script_after = executable_content_digest(compiler_script).map_err(|source| {
        ToolchainProbeError::ExecutableWitness {
            role: ToolchainProbeFileRole::Script,
            path: compiler_script.to_path_buf(),
            source,
        }
    })?;
    let interpreter_after = executable_content_digest(node).map_err(|source| {
        ToolchainProbeError::ExecutableWitness {
            role: ToolchainProbeFileRole::Interpreter,
            path: node.to_path_buf(),
            source,
        }
    })?;
    let module_after = typescript_module_closure_digest(module_root).map_err(|source| {
        ToolchainProbeError::ExecutableWitness {
            role: ToolchainProbeFileRole::CompilerModule,
            path: module_root.join("typescript"),
            source,
        }
    })?;
    if script_before != script_after {
        return Err(ToolchainProbeError::ExecutableChanged {
            role: ToolchainProbeFileRole::Script,
            path: compiler_script.to_path_buf(),
        });
    }
    if interpreter_before != interpreter_after {
        return Err(ToolchainProbeError::ExecutableChanged {
            role: ToolchainProbeFileRole::Interpreter,
            path: node.to_path_buf(),
        });
    }
    if module_before != module_after {
        return Err(ToolchainProbeError::ExecutableChanged {
            role: ToolchainProbeFileRole::CompilerModule,
            path: module_root.join("typescript"),
        });
    }
    Ok((
        script_version,
        interpreter_version,
        script_after,
        interpreter_after,
        module_after,
    ))
}

fn probe_prepared_command(
    tool: NativeTool,
    executable: &Path,
    mut command: Command,
    limits: ToolchainProbeLimits,
    cancelled: Option<&std::sync::atomic::AtomicBool>,
) -> Result<Box<[u8]>, ToolchainProbeError> {
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    let mut child = command
        .spawn()
        .map_err(|source| ToolchainProbeError::Spawn {
            tool,
            executable: executable.to_path_buf(),
            source,
        })?;
    let stdout = match child.stdout.take() {
        Some(stdout) => stdout,
        None => {
            let terminal = ToolchainProbeError::MissingStream {
                tool,
                worker: NativeWorker::StandardOutputReader,
            };
            return Err(terminate_and_reap(tool, &mut child, terminal));
        }
    };
    let stderr = match child.stderr.take() {
        Some(stderr) => stderr,
        None => {
            let terminal = ToolchainProbeError::MissingStream {
                tool,
                worker: NativeWorker::StandardErrorReader,
            };
            return Err(terminate_and_reap(tool, &mut child, terminal));
        }
    };
    let (limit_sender, limit_receiver) = channel();
    let stdout_sender = limit_sender.clone();
    let stdout_worker = match spawn_reader(
        tool,
        NativeWorker::StandardOutputReader,
        stdout,
        limits.maximum_stream_bytes,
        stdout_sender,
    ) {
        Ok(worker) => worker,
        Err(terminal) => return Err(terminate_and_reap(tool, &mut child, terminal)),
    };
    let stderr_worker = match spawn_reader(
        tool,
        NativeWorker::StandardErrorReader,
        stderr,
        limits.maximum_stream_bytes,
        limit_sender,
    ) {
        Ok(worker) => worker,
        Err(terminal) => {
            let preceding = terminate_and_reap(tool, &mut child, terminal);
            return Err(
                match join_reader(stdout_worker, NativeWorker::StandardOutputReader) {
                    Ok(_stdout) => preceding,
                    Err(source) => ToolchainProbeError::Stream {
                        tool,
                        preceding: Some(Box::new(preceding)),
                        source,
                    },
                },
            );
        }
    };

    let started = Instant::now();
    let mut preceding = None;
    let mut status = None;
    loop {
        match limit_receiver.try_recv() {
            Ok(primary) => {
                preceding = Some(terminate_and_reap(
                    tool,
                    &mut child,
                    ToolchainProbeError::Bounded { tool, primary },
                ));
                break;
            }
            Err(TryRecvError::Empty) => {}
            Err(TryRecvError::Disconnected) => {}
        }
        match child.try_wait() {
            Ok(Some(observed)) => {
                status = Some(observed);
                break;
            }
            Ok(None) => {}
            Err(source) => {
                preceding = Some(terminate_and_reap(
                    tool,
                    &mut child,
                    ToolchainProbeError::Wait { tool, source },
                ));
                break;
            }
        }
        if cancelled.is_some_and(|flag| flag.load(std::sync::atomic::Ordering::Acquire)) {
            preceding = Some(terminate_and_reap(
                tool,
                &mut child,
                ToolchainProbeError::Bounded {
                    tool,
                    primary: ToolchainProbePrimary::Cancelled,
                },
            ));
            break;
        }
        if started.elapsed() >= limits.timeout {
            let primary = ToolchainProbePrimary::Deadline {
                timeout: limits.timeout,
            };
            preceding = Some(terminate_and_reap(
                tool,
                &mut child,
                ToolchainProbeError::Bounded { tool, primary },
            ));
            break;
        }
        thread::sleep(PROBE_POLL_INTERVAL);
    }

    let stdout_result = join_reader(stdout_worker, NativeWorker::StandardOutputReader);
    let stderr_result = join_reader(stderr_worker, NativeWorker::StandardErrorReader);
    let (stdout, stderr) = match (stdout_result, stderr_result) {
        (Ok(stdout), Ok(stderr)) => (stdout, stderr),
        (Err(stdout), Err(stderr)) => {
            return Err(ToolchainProbeError::Streams {
                tool,
                preceding: preceding.map(Box::new),
                stdout,
                stderr,
            });
        }
        (Err(source), Ok(_stderr)) | (Ok(_stderr), Err(source)) => {
            return Err(ToolchainProbeError::Stream {
                tool,
                preceding: preceding.map(Box::new),
                source,
            });
        }
    };
    if let Some(terminal) = preceding {
        return Err(terminal);
    }
    // A short-lived child can exit between the coordinator's last channel
    // poll and the reader's cap-crossing send. Joining both readers above is
    // the completion barrier for those sends, so inspect the channel once
    // more before treating the observed exit status and retained prefix as a
    // valid version identity.
    if let Ok(primary) = limit_receiver.try_recv() {
        return Err(ToolchainProbeError::Bounded { tool, primary });
    }
    let status = status.ok_or(ToolchainProbeError::MissingStatus { tool })?;
    if !status.success() {
        return Err(ToolchainProbeError::Exit {
            tool,
            status,
            stdout,
            stderr,
        });
    }
    if !stdout.is_empty() {
        return Ok(stdout);
    }
    if !stderr.is_empty() {
        return Ok(stderr);
    }
    Err(ToolchainProbeError::Empty { tool })
}

fn version_argument(tool: NativeTool) -> &'static str {
    match tool {
        NativeTool::GoCompiler => "version",
        NativeTool::Rustc
        | NativeTool::Clang
        | NativeTool::Python
        | NativeTool::TypeScriptCompiler
        | NativeTool::JavaCompiler
        | NativeTool::CSharpCompiler => "--version",
    }
}

fn spawn_reader<Reader: Read + Send + 'static>(
    tool: NativeTool,
    worker: NativeWorker,
    reader: Reader,
    maximum: NonZeroUsize,
    limit_sender: Sender<ToolchainProbePrimary>,
) -> Result<JoinHandle<Result<Box<[u8]>, ToolchainProbeStreamError>>, ToolchainProbeError> {
    thread::Builder::new()
        .name(
            match worker {
                NativeWorker::SourceWriter => "nudox-toolchain-version-source",
                NativeWorker::StandardOutputReader => "nudox-toolchain-version-stdout",
                NativeWorker::StandardErrorReader => "nudox-toolchain-version-stderr",
            }
            .to_owned(),
        )
        .spawn(move || read_bounded(reader, worker, maximum, &limit_sender))
        .map_err(|source| ToolchainProbeError::ReaderSpawn {
            tool,
            worker,
            source,
        })
}

fn read_bounded(
    mut reader: impl Read,
    worker: NativeWorker,
    maximum: NonZeroUsize,
    limit_sender: &Sender<ToolchainProbePrimary>,
) -> Result<Box<[u8]>, ToolchainProbeStreamError> {
    let mut bytes = Vec::new();
    bytes
        .try_reserve_exact(maximum.get())
        .map_err(|source| ToolchainProbeStreamError::Allocation { worker, source })?;
    let mut buffer = [0_u8; READ_CHUNK_BYTES];
    let mut observed = 0_usize;
    loop {
        let read = reader
            .read(&mut buffer)
            .map_err(|source| ToolchainProbeStreamError::Read { worker, source })?;
        if read == 0 {
            return Ok(bytes.into_boxed_slice());
        }
        observed = observed.checked_add(read).unwrap_or(usize::MAX);
        let remaining = maximum.get().saturating_sub(bytes.len());
        bytes.extend_from_slice(&buffer[..read.min(remaining)]);
        if observed > maximum.get() {
            let _sent = limit_sender.send(ToolchainProbePrimary::OutputLimit {
                worker,
                observed,
                maximum: maximum.get(),
            });
            return Ok(bytes.into_boxed_slice());
        }
    }
}

fn join_reader(
    worker: JoinHandle<Result<Box<[u8]>, ToolchainProbeStreamError>>,
    identity: NativeWorker,
) -> Result<Box<[u8]>, ToolchainProbeStreamError> {
    worker
        .join()
        .map_err(|payload| ToolchainProbeStreamError::WorkerPanic {
            cause: NativeWorkerPanic::capture(identity, payload.as_ref()),
        })?
}

fn terminate_and_reap(
    tool: NativeTool,
    child: &mut Child,
    preceding: ToolchainProbeError,
) -> ToolchainProbeError {
    if let Err(source) = terminate_process_group(child)
        && source.kind() != io::ErrorKind::InvalidInput
    {
        return ToolchainProbeError::Cleanup {
            tool,
            action: ToolchainProbeCleanupAction::Terminate,
            preceding: Box::new(preceding),
            source,
        };
    }
    match child.wait() {
        Ok(_status) => preceding,
        Err(source) => ToolchainProbeError::Cleanup {
            tool,
            action: ToolchainProbeCleanupAction::Reap,
            preceding: Box::new(preceding),
            source,
        },
    }
}

fn terminate_process_group(child: &mut Child) -> io::Result<()> {
    #[cfg(unix)]
    {
        let process_group = format!("-{}", child.id());
        let status = Command::new("kill")
            .args(["-KILL", "--", process_group.as_str()])
            .status()?;
        if status.success() {
            return Ok(());
        }
    }
    child.kill()
}

#[cfg(all(test, unix))]
mod tests {
    use std::{num::NonZeroUsize, process::Command, time::Duration};

    use backend_semantic::vocabulary::NativeTool;

    use super::{
        NATIVE_COMPILER_ENVIRONMENT_POLICY_ID, NativeCompilerEnvironment, ToolchainProbeError,
        ToolchainProbeLimits, probe_command,
    };

    #[test]
    fn compiler_environment_contract_is_versioned_and_removes_ambient_settings() {
        assert_eq!(
            NATIVE_COMPILER_ENVIRONMENT_POLICY_ID,
            b"native-compiler-environment/empty-v1\0"
        );

        // Seed representative Rust/C/Python controls on the Command, then apply
        // the same policy used by version probes and lower-IR compiler children.
        let mut command = Command::new("/bin/sh");
        command
            .env("RUSTFLAGS", "--cfg ambient")
            .env("CPATH", "/ambient/include")
            .env("PYTHONPATH", "/ambient/python");
        NativeCompilerEnvironment::apply(&mut command, NativeTool::Rustc);
        let status = command
            .args([
                "-c",
                "test -z \"${RUSTFLAGS+x}\" && test -z \"${CPATH+x}\" && test -z \"${PYTHONPATH+x}\"",
            ])
            .status()
            .expect("start environment-policy witness");
        assert!(
            status.success(),
            "an ambient compiler variable reached the child"
        );
    }

    #[test]
    fn dotnet_probe_and_compile_share_only_the_fixed_cli_controls() {
        let mut command = Command::new("/bin/sh");
        command
            .env("DOTNET_CLI_TELEMETRY_OPTOUT", "0")
            .env("DOTNET_SKIP_FIRST_TIME_EXPERIENCE", "0")
            .env("CPATH", "/ambient/include");
        NativeCompilerEnvironment::apply(&mut command, NativeTool::CSharpCompiler);
        let status = command
            .args([
                "-c",
                "test \"$DOTNET_CLI_TELEMETRY_OPTOUT\" = 1 && test \"$DOTNET_SKIP_FIRST_TIME_EXPERIENCE\" = 1 && test -z \"${CPATH+x}\"",
            ])
            .status()
            .expect("start .NET environment-policy witness");
        assert!(
            status.success(),
            ".NET did not receive its exact controlled environment"
        );
    }

    #[test]
    fn a_missing_admitted_executable_has_a_typed_spawn_failure() {
        let executable = std::env::temp_dir()
            .join(format!("nudox-missing-compiler-{}", std::process::id()))
            .join("rustc");
        let limits = ToolchainProbeLimits::new(
            Duration::from_secs(1),
            NonZeroUsize::new(128).expect("nonzero output bound"),
        )
        .expect("valid probe bounds");
        let error = probe_command(NativeTool::Rustc, &executable, &["--version"], limits)
            .expect_err("missing compiler path must not fall back to PATH");
        assert!(matches!(
            error,
            ToolchainProbeError::Spawn { executable: observed, .. }
                if observed == executable
        ));
    }
}

#[cfg(test)]
mod failure_tests {
    use std::{
        num::NonZeroUsize,
        path::{Path, PathBuf},
        time::Duration,
    };

    use backend_semantic::vocabulary::NativeTool;

    use super::{
        FAILURE_EXCERPT_BYTES, FailureStreams, ToolchainProbeError, ToolchainProbeLimits,
        probe_command,
    };

    /// Name of the fixture test this module re-executes as a failing tool.
    const FIXTURE: &str = "application::toolchain_probe::failure_tests::failing_tool_fixture";
    /// Argument that switches the fixture from a no-op test to a failing tool.
    const MODE: &str = "nudox-probe-fixture-fail";

    fn limits() -> ToolchainProbeLimits {
        ToolchainProbeLimits::new(
            Duration::from_secs(30),
            NonZeroUsize::new(64 * 1024).expect("nonzero output bound"),
        )
        .expect("valid probe bounds")
    }

    /// Re-executed by the probe tests below. Run as an ordinary test it does
    /// nothing; run with [`MODE`] among its arguments it behaves like a tool
    /// that rejects its arguments: a diagnostic on stderr and exit status 3.
    #[test]
    fn failing_tool_fixture() {
        if std::env::args().any(|argument| argument == MODE) {
            eprintln!("fixture tool: unexpected argument '--print'");
            println!("fixture tool: usage text");
            std::process::exit(3);
        }
    }

    #[test]
    fn a_failed_probe_keeps_and_shows_what_the_tool_said() {
        let executable = std::env::current_exe().expect("test executable");
        let error = probe_command(
            NativeTool::Rustc,
            &executable,
            &["--exact", FIXTURE, MODE, "--nocapture"],
            limits(),
        )
        .expect_err("the fixture tool exits unsuccessfully");

        let message = error.to_string();
        let ToolchainProbeError::Exit {
            status,
            stdout,
            stderr,
            ..
        } = &error
        else {
            panic!("expected a typed exit, observed {error:?}");
        };
        assert_eq!(status.code(), Some(3));
        assert!(
            String::from_utf8_lossy(stderr).contains("unexpected argument '--print'"),
            "the typed error retains stderr: {stderr:?}"
        );
        assert!(String::from_utf8_lossy(stdout).contains("usage text"));
        assert!(
            message.contains("unexpected argument '--print'") && message.contains("stderr"),
            "the message quotes stderr: {message}"
        );
    }

    #[test]
    fn the_quotation_is_bounded_escaped_and_names_each_nonempty_stream() {
        assert_eq!(FailureStreams::new(b"", b"").to_string(), "");
        assert_eq!(
            FailureStreams::new(b"", b"error: bad\nline two\n").to_string(),
            "; stderr: \"error: bad\\nline two\""
        );
        assert_eq!(
            FailureStreams::new(b"out", b"err").to_string(),
            "; stderr: \"err\"; stdout: \"out\""
        );
        let long = vec![b'x'; FAILURE_EXCERPT_BYTES + 7];
        let shown = FailureStreams::new(b"", &long).to_string();
        assert_eq!(
            shown,
            format!(
                "; stderr: \"{}\" (first {FAILURE_EXCERPT_BYTES} of {} bytes)",
                "x".repeat(FAILURE_EXCERPT_BYTES),
                FAILURE_EXCERPT_BYTES + 7
            )
        );
        // Bytes that are not UTF-8 are quoted lossily instead of failing the message.
        assert_eq!(
            FailureStreams::new(b"", &[0xff, b'a']).to_string(),
            "; stderr: \"\u{fffd}a\""
        );
    }

    /// The compiler the host would select when `NUDOX_RUSTC` names the first `rustc` on `PATH`.
    fn host_rustc() -> PathBuf {
        let suffix = std::env::consts::EXE_SUFFIX;
        let from_path = std::env::var_os("PATH").and_then(|path| {
            std::env::split_paths(&path)
                .map(|directory| directory.join(format!("rustc{suffix}")))
                .find(|candidate| candidate.is_file())
        });
        let selected = std::env::var_os("RUSTC")
            .map(PathBuf::from)
            .filter(|path| path.is_absolute() && path.is_file())
            .or(from_path)
            .expect("a cargo test environment provides rustc through RUSTC or PATH");
        backend_frontend_rust::legacy::canonical_executable(&selected)
            .expect("resolve the selected rustc as the host does")
    }

    /// A rustup proxy is a link to the `rustup` binary. The probe must reach the compiler, not
    /// the toolchain manager that rejected `--print sysroot` and answered `--version` with its own
    /// version.
    #[test]
    fn the_selected_rustc_answers_as_a_compiler_not_as_the_toolchain_manager() {
        let rustc = host_rustc();
        let version = probe_command(NativeTool::Rustc, &rustc, &["-vV"], limits())
            .unwrap_or_else(|error| panic!("rustc -vV through {rustc:?}: {error}"));
        let text = String::from_utf8_lossy(&version);
        assert!(
            text.starts_with("rustc "),
            "unexpected version text: {text}"
        );
        assert!(text.contains("release:"), "unexpected version text: {text}");

        let sysroot = probe_command(NativeTool::Rustc, &rustc, &["--print", "sysroot"], limits())
            .unwrap_or_else(|error| panic!("rustc --print sysroot through {rustc:?}: {error}"));
        let sysroot = PathBuf::from(String::from_utf8_lossy(&sysroot).trim());
        assert!(
            Path::new(&sysroot).is_absolute() && sysroot.is_dir(),
            "the sysroot is an existing absolute directory: {sysroot:?}"
        );
    }
}
