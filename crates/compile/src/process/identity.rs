//! Content identities, bounded digest caching, and executable leases.

use super::SupervisedCommand;
use crate::{CancellationObserver, ProcessError, ToolchainId, typed_of};
use backend_version::Schema;
#[cfg(unix)]
#[path = "identity_cache.rs"]
mod identity_cache;
#[cfg(unix)]
use identity_cache::{executable_cache_insert, executable_cache_lookup, sample_bytes, sample_file};
use std::{
    fs::{File, Metadata, OpenOptions, remove_dir, remove_file},
    io::{Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
};

use std::sync::atomic::{AtomicU64, Ordering};

static EXECUTABLE_TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Maximum executable bytes retained while deriving a toolchain identity or
/// constructing an immutable execution lease.
pub const MAX_EXECUTABLE_BYTES: u64 = 512 * 1024 * 1024;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct FileStamp {
    length: u64,
    modified_nanos: Option<u128>,
    #[cfg(unix)]
    device: u64,
    #[cfg(unix)]
    inode: u64,
    #[cfg(unix)]
    changed_nanos: i128,
}

impl FileStamp {
    fn from_metadata(metadata: &Metadata) -> Self {
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            Self {
                length: metadata.len(),
                modified_nanos: metadata
                    .modified()
                    .ok()
                    .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
                    .map(|duration| duration.as_nanos()),
                device: metadata.dev(),
                inode: metadata.ino(),
                changed_nanos: i128::from(metadata.ctime())
                    .saturating_mul(1_000_000_000)
                    .saturating_add(i128::from(metadata.ctime_nsec())),
            }
        }
        #[cfg(not(unix))]
        {
            Self {
                length: metadata.len(),
                modified_nanos: metadata
                    .modified()
                    .ok()
                    .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
                    .map(|duration| duration.as_nanos()),
            }
        }
    }
}

/// Opens a candidate executable without allowing a FIFO path to block the caller before its
/// descriptor can be classified. On Unix, `O_NONBLOCK` is harmless for regular files and makes
/// opening a FIFO return immediately; every caller still rejects non-regular descriptors after
/// `fstat`.
fn open_executable_file(path: &Path) -> Result<File, ProcessError> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;

        let mut options = OpenOptions::new();
        options.read(true).custom_flags(
            (rustix::fs::OFlags::NONBLOCK | rustix::fs::OFlags::CLOEXEC).bits() as i32,
        );
        options
            .open(path)
            .map_err(|_| ProcessError::ExecutableUnavailable)
    }
    #[cfg(not(unix))]
    {
        File::open(path).map_err(|_| ProcessError::ExecutableUnavailable)
    }
}

/// Content identity captured for one executable file.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ExecutableIdentity {
    digest: ToolchainId,
    stamp: FileStamp,
}

impl ExecutableIdentity {
    /// Reads an executable file and derives an identity from its exact bytes.
    ///
    /// # Errors
    ///
    /// Returns [`ProcessError::ExecutableUnavailable`] when the path cannot
    /// be opened as a regular file, [`ProcessError::ExecutableDrift`] when it
    /// changes during the read, or [`ProcessError::Io`] for another read
    /// failure.
    pub fn from_path(path: &Path) -> Result<Self, ProcessError> {
        let mut file = open_executable_file(path)?;
        let before = file
            .metadata()
            .map_err(|_| ProcessError::ExecutableUnavailable)?;
        if !before.is_file() {
            return Err(ProcessError::ExecutableUnavailable);
        }
        if before.len() > MAX_EXECUTABLE_BYTES {
            return Err(ProcessError::ExecutableLimit);
        }
        let before_stamp = FileStamp::from_metadata(&before);
        #[cfg(unix)]
        {
            if let Some(cached) = executable_cache_lookup(path, before_stamp) {
                let sample_matches = sample_file(&mut file, before.len())
                    .is_ok_and(|sample| sample == cached.sample);
                let after = file
                    .metadata()
                    .map_err(|_| ProcessError::ExecutableUnavailable)?;
                if before_stamp != FileStamp::from_metadata(&after) {
                    return Err(ProcessError::ExecutableDrift);
                }
                if sample_matches {
                    return Ok(cached.identity);
                }
            }
        }

        file.seek(SeekFrom::Start(0))
            .map_err(|_| ProcessError::Io)?;
        let bytes = read_bounded_executable(&mut file, before.len())?;
        let after = file
            .metadata()
            .map_err(|_| ProcessError::ExecutableUnavailable)?;
        let after_stamp = FileStamp::from_metadata(&after);
        if before_stamp != after_stamp {
            return Err(ProcessError::ExecutableDrift);
        }
        let digest = typed_of::<crate::ToolchainSchema>(&bytes);
        let identity = Self {
            digest,
            stamp: after_stamp,
        };
        #[cfg(unix)]
        executable_cache_insert(path, identity.clone(), sample_bytes(&bytes));
        Ok(identity)
    }

    /// Returns the content-derived executable identity.
    #[must_use]
    pub const fn digest(&self) -> ToolchainId {
        self.digest
    }

    /// Verifies that `path` still names the exact admitted executable bytes.
    ///
    /// # Errors
    ///
    /// Returns [`ProcessError::ExecutableDrift`] when the file content no
    /// longer matches this identity, or [`ProcessError::ExecutableUnavailable`]
    /// when it cannot be read.
    pub fn verify_path(&self, path: &Path) -> Result<(), ProcessError> {
        let current = Self::from_path(path)?;
        if current == *self {
            Ok(())
        } else {
            Err(ProcessError::ExecutableDrift)
        }
    }

    pub(crate) fn verify_path_until<C: CancellationObserver + ?Sized>(
        &self,
        path: &Path,
        cancellation: &C,
        deadline: std::time::Instant,
    ) -> Result<(), ProcessError> {
        process_checkpoint(cancellation, deadline)?;
        let mut file = open_executable_file(path)?;
        let before = file
            .metadata()
            .map_err(|_| ProcessError::ExecutableUnavailable)?;
        if !before.is_file() {
            return Err(ProcessError::ExecutableUnavailable);
        }
        if before.len() > MAX_EXECUTABLE_BYTES {
            return Err(ProcessError::ExecutableLimit);
        }
        let before_stamp = FileStamp::from_metadata(&before);
        #[cfg(unix)]
        if let Some(cached) = executable_cache_lookup(path, before_stamp) {
            let sample_matches =
                sample_file(&mut file, before.len()).is_ok_and(|sample| sample == cached.sample);
            process_checkpoint(cancellation, deadline)?;
            let after = file
                .metadata()
                .map_err(|_| ProcessError::ExecutableUnavailable)?;
            if before_stamp != FileStamp::from_metadata(&after) {
                return Err(ProcessError::ExecutableDrift);
            }
            if sample_matches {
                return if cached.identity == *self {
                    Ok(())
                } else {
                    Err(ProcessError::ExecutableDrift)
                };
            }
        }
        file.seek(SeekFrom::Start(0))
            .map_err(|_| ProcessError::Io)?;
        let mut hasher = backend_version::ObjectVersionHasher::new(
            backend_version::SchemaIdentity::new(
                crate::ToolchainSchema::DOMAIN,
                crate::ToolchainSchema::TYPE,
                crate::ToolchainSchema::VERSION,
            ),
            usize::try_from(before.len()).map_err(|_| ProcessError::ExecutableLimit)?,
        )
        .map_err(|_| ProcessError::ExecutableLimit)?;
        let mut scratch = [0_u8; 64 * 1024];
        let mut remaining = before.len();
        while remaining > 0 {
            process_checkpoint(cancellation, deadline)?;
            let limit = usize::try_from(remaining.min(scratch.len() as u64))
                .map_err(|_| ProcessError::ExecutableLimit)?;
            let read = file
                .read(&mut scratch[..limit])
                .map_err(|_| ProcessError::Io)?;
            if read == 0 {
                return Err(ProcessError::ExecutableDrift);
            }
            hasher
                .update(&scratch[..read])
                .map_err(|_| ProcessError::ExecutableDrift)?;
            remaining -= read as u64;
        }
        process_checkpoint(cancellation, deadline)?;
        let after = file
            .metadata()
            .map_err(|_| ProcessError::ExecutableUnavailable)?;
        if before_stamp != FileStamp::from_metadata(&after) {
            return Err(ProcessError::ExecutableDrift);
        }
        let digest = hasher
            .finish_version::<crate::ToolchainSchema>()
            .map_err(|_| ProcessError::ExecutableDrift)?;
        if digest != self.digest {
            return Err(ProcessError::ExecutableDrift);
        }
        Ok(())
    }
}

/// A toolchain executable plus its declared SDK/dependency closure.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ToolchainArtifact {
    executable: ExecutableIdentity,
    dependencies: Vec<ToolchainId>,
    identity: ToolchainId,
}

/// A one-shot capability proving the executable bytes observed for a command.
///
/// The capability is intentionally tied to the command path and cannot be
/// constructed from a digest. Process startup may still perform a final
/// check, but callers can no longer pass a bare success flag across the
/// authority boundary.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VerifiedExecutable {
    path: PathBuf,
    identity: Option<ExecutableIdentity>,
}

impl VerifiedExecutable {
    pub(super) fn new(path: &Path, identity: Option<ExecutableIdentity>) -> Self {
        Self {
            path: path.to_owned(),
            identity,
        }
    }

    /// Returns the path whose bytes were checked.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Returns the content identity observed during admission, when present.
    #[must_use]
    pub fn identity(&self) -> Option<&ExecutableIdentity> {
        self.identity.as_ref()
    }
}

impl ToolchainArtifact {
    /// Builds a canonical artifact identity from an executable and closure.
    #[must_use]
    pub fn new(executable: ExecutableIdentity, mut dependencies: Vec<ToolchainId>) -> Self {
        dependencies.sort_by_key(|dependency| dependency.to_bytes());
        dependencies.dedup();
        let mut bytes = Vec::with_capacity(32 + dependencies.len().saturating_mul(32));
        bytes.extend_from_slice(&executable.digest().to_bytes());
        for dependency in &dependencies {
            bytes.extend_from_slice(&dependency.to_bytes());
        }
        let identity = typed_of::<crate::ToolchainSchema>(&bytes);
        Self {
            executable,
            dependencies,
            identity,
        }
    }

    /// Reads an executable and builds its canonical artifact identity.
    ///
    /// # Errors
    ///
    /// Returns a [`ProcessError`] when the executable cannot be read or drifts
    /// while its identity is being captured.
    pub fn from_path(path: &Path, dependencies: Vec<ToolchainId>) -> Result<Self, ProcessError> {
        Ok(Self::new(
            ExecutableIdentity::from_path(path)?,
            dependencies,
        ))
    }

    /// Returns the executable identity in this artifact.
    #[must_use]
    pub const fn executable(&self) -> &ExecutableIdentity {
        &self.executable
    }

    /// Returns the sorted, deduplicated declared closure identities.
    #[must_use]
    pub fn dependencies(&self) -> &[ToolchainId] {
        &self.dependencies
    }

    /// Returns the identity of the executable plus declared closure.
    #[must_use]
    pub const fn identity(&self) -> ToolchainId {
        self.identity
    }

    /// Verifies the executable portion of this artifact at a path.
    ///
    /// # Errors
    ///
    /// Returns [`ProcessError::ExecutableDrift`] when the executable no longer
    /// matches the artifact, or [`ProcessError::ExecutableUnavailable`] when
    /// it cannot be read.
    pub fn verify_path(&self, path: &Path) -> Result<(), ProcessError> {
        self.executable.verify_path(path)
    }

    pub(crate) fn verify_path_until<C: CancellationObserver + ?Sized>(
        &self,
        path: &Path,
        cancellation: &C,
        deadline: std::time::Instant,
    ) -> Result<(), ProcessError> {
        self.executable
            .verify_path_until(path, cancellation, deadline)
    }
}

fn process_checkpoint<C: CancellationObserver + ?Sized>(
    cancellation: &C,
    deadline: std::time::Instant,
) -> Result<(), ProcessError> {
    cancellation.checkpoint().map_err(ProcessError::from)?;
    if std::time::Instant::now() >= deadline {
        Err(ProcessError::Deadline)
    } else {
        Ok(())
    }
}

/// Owns the exact executable bytes that a child process will run.
///
/// Authority commands execute through a private path bound to the verified
/// executable.  On macOS an authority whose file or containing directory is
/// writable is copied into a private 0700 directory and reopened after its
/// mode is reduced to 0500.  Protected system binaries retain their original
/// signed path only when both the file and its containing directory reject
/// caller writes; this avoids claiming that a shared inode is immutable.  On
/// other targets the verified bytes are copied into a private temporary
/// executable.  Generic commands retain their original path because they have
/// no content authority binding.
pub(crate) struct ExecutableLease {
    path: PathBuf,
    remove_on_drop: bool,
    temporary_directory: Option<PathBuf>,
}

impl ExecutableLease {
    pub(crate) fn prepare(command: &SupervisedCommand) -> Result<Self, ProcessError> {
        #[cfg(target_os = "macos")]
        {
            Self::prepare_macos(command)
        }
        #[cfg(not(target_os = "macos"))]
        Self::prepare_copy(command)
    }

    pub(crate) fn prepare_until<C: CancellationObserver + ?Sized>(
        command: &SupervisedCommand,
        cancellation: &C,
        deadline: std::time::Instant,
    ) -> Result<Self, ProcessError> {
        process_checkpoint(cancellation, deadline)?;
        #[cfg(target_os = "macos")]
        {
            Self::prepare_macos_until(command, cancellation, deadline)
        }
        #[cfg(not(target_os = "macos"))]
        {
            Self::prepare_copy_until(command, cancellation, deadline)
        }
    }

    #[cfg(target_os = "macos")]
    fn prepare_macos_until<C: CancellationObserver + ?Sized>(
        command: &SupervisedCommand,
        cancellation: &C,
        deadline: std::time::Instant,
    ) -> Result<Self, ProcessError> {
        if command.toolchain().is_none() {
            process_checkpoint(cancellation, deadline)?;
            return Ok(Self {
                path: command.program().to_owned(),
                remove_on_drop: false,
                temporary_directory: None,
            });
        }
        let expected = command
            .executable_identity()
            .ok_or(ProcessError::ExecutableUnavailable)?;
        let source_path = std::fs::canonicalize(command.program())
            .map_err(|_| ProcessError::ExecutableUnavailable)?;
        let mut source = open_executable_file(&source_path)?;
        let before = source
            .metadata()
            .map_err(|_| ProcessError::ExecutableUnavailable)?;
        if !before.is_file() {
            return Err(ProcessError::ExecutableUnavailable);
        }
        let before_stamp = FileStamp::from_metadata(&before);
        let bytes =
            read_bounded_executable_until(&mut source, before.len(), cancellation, deadline)?;
        let after = source
            .metadata()
            .map_err(|_| ProcessError::ExecutableUnavailable)?;
        if before_stamp != FileStamp::from_metadata(&after)
            || toolchain_digest_until(&bytes, cancellation, deadline)? != expected.digest()
        {
            return Err(ProcessError::ExecutableDrift);
        }
        process_checkpoint(cancellation, deadline)?;
        if !is_macho(&bytes) {
            return Self::from_bytes_until(&bytes, &before, cancellation, deadline);
        }
        let file_is_writable = file_is_writable_or_changed(&source_path, before_stamp);
        process_checkpoint(cancellation, deadline)?;
        if file_is_writable || directory_is_writable(source_path.as_path()) {
            return Self::from_bytes_until(&bytes, &before, cancellation, deadline);
        }
        process_checkpoint(cancellation, deadline)?;
        Ok(Self {
            path: source_path,
            remove_on_drop: false,
            temporary_directory: None,
        })
    }

    #[cfg(not(target_os = "macos"))]
    fn prepare_copy_until<C: CancellationObserver + ?Sized>(
        command: &SupervisedCommand,
        cancellation: &C,
        deadline: std::time::Instant,
    ) -> Result<Self, ProcessError> {
        if command.toolchain().is_none() {
            process_checkpoint(cancellation, deadline)?;
            return Ok(Self {
                path: command.program().to_owned(),
                remove_on_drop: false,
                temporary_directory: None,
            });
        }
        let expected = command
            .executable_identity()
            .ok_or(ProcessError::ExecutableUnavailable)?;
        let mut source = open_executable_file(command.program())?;
        let before = source
            .metadata()
            .map_err(|_| ProcessError::ExecutableUnavailable)?;
        if !before.is_file() {
            return Err(ProcessError::ExecutableUnavailable);
        }
        let before_stamp = FileStamp::from_metadata(&before);
        let bytes =
            read_bounded_executable_until(&mut source, before.len(), cancellation, deadline)?;
        let after = source
            .metadata()
            .map_err(|_| ProcessError::ExecutableUnavailable)?;
        if before_stamp != FileStamp::from_metadata(&after)
            || toolchain_digest_until(&bytes, cancellation, deadline)? != expected.digest()
        {
            return Err(ProcessError::ExecutableDrift);
        }
        Self::from_bytes_until(&bytes, &before, cancellation, deadline)
    }

    fn from_bytes_until<C: CancellationObserver + ?Sized>(
        bytes: &[u8],
        source: &Metadata,
        cancellation: &C,
        deadline: std::time::Instant,
    ) -> Result<Self, ProcessError> {
        let directory = private_temp_directory("exec")?;
        let path = directory.join("exec.out");
        let result = (|| {
            process_checkpoint(cancellation, deadline)?;
            let mut file = OpenOptions::new()
                .create_new(true)
                .write(true)
                .read(true)
                .open(&path)
                .map_err(|_| ProcessError::TemporaryFile)?;
            for chunk in bytes.chunks(64 * 1024) {
                process_checkpoint(cancellation, deadline)?;
                file.write_all(chunk).map_err(|_| ProcessError::Io)?;
            }
            file.sync_all().map_err(|_| ProcessError::Io)?;
            let mut permissions = source.permissions();
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                permissions.set_mode(0o500);
            }
            #[cfg(not(unix))]
            permissions.set_readonly(true);
            file.set_permissions(permissions)
                .map_err(|_| ProcessError::Io)?;
            file.seek(SeekFrom::Start(0))
                .map_err(|_| ProcessError::Io)?;
            let mut observed = [0_u8; 64 * 1024];
            for chunk in bytes.chunks(64 * 1024) {
                process_checkpoint(cancellation, deadline)?;
                file.read_exact(&mut observed[..chunk.len()])
                    .map_err(|_| ProcessError::Io)?;
                if observed[..chunk.len()] != chunk[..] {
                    return Err(ProcessError::ExecutableDrift);
                }
            }
            process_checkpoint(cancellation, deadline)
        })();
        if let Err(error) = result {
            let _ = remove_file(&path);
            let _ = remove_dir(&directory);
            return Err(error);
        }
        Ok(Self {
            path,
            remove_on_drop: true,
            temporary_directory: Some(directory),
        })
    }

    #[cfg(target_os = "macos")]
    fn prepare_macos(command: &SupervisedCommand) -> Result<Self, ProcessError> {
        if command.toolchain().is_none() {
            return Ok(Self {
                path: command.program().to_owned(),
                remove_on_drop: false,
                temporary_directory: None,
            });
        }
        let expected = command
            .executable_identity()
            .ok_or(ProcessError::ExecutableUnavailable)?;
        let source_path = std::fs::canonicalize(command.program())
            .map_err(|_| ProcessError::ExecutableUnavailable)?;
        let mut source = open_executable_file(&source_path)?;
        let before = source
            .metadata()
            .map_err(|_| ProcessError::ExecutableUnavailable)?;
        if !before.is_file() {
            return Err(ProcessError::ExecutableUnavailable);
        }
        let before_stamp = FileStamp::from_metadata(&before);
        let bytes = read_bounded_executable(&mut source, before.len())?;
        let after = source
            .metadata()
            .map_err(|_| ProcessError::ExecutableUnavailable)?;
        if before_stamp != FileStamp::from_metadata(&after)
            || typed_of::<crate::ToolchainSchema>(&bytes) != expected.digest()
        {
            return Err(ProcessError::ExecutableDrift);
        }

        // A script or unsigned executable can be copied without losing a
        // platform signature. Copying isolates the lease from an in-place
        // write to the caller's original inode.
        if !is_macho(&bytes) {
            return Self::from_bytes(&bytes, &before);
        }
        // A signed Mach-O must retain its embedded code-signing envelope. A
        // caller-writable file or source directory permits a private
        // byte-for-byte copy, which also prevents a later chmod or in-place
        // write from changing the admitted image.
        let file_is_writable = file_is_writable_or_changed(&source_path, before_stamp);
        if file_is_writable || directory_is_writable(source_path.as_path()) {
            return Self::from_bytes(&bytes, &before);
        }
        // SIP-protected system binaries can reject private execution copies.
        // Their file and parent directory are both outside caller control, so
        // retaining the original signed path does not expose a caller-level
        // rename or in-place mutation race.
        Ok(Self {
            path: source_path,
            remove_on_drop: false,
            temporary_directory: None,
        })
    }

    #[cfg(not(target_os = "macos"))]
    fn prepare_copy(command: &SupervisedCommand) -> Result<Self, ProcessError> {
        if command.toolchain().is_none() {
            return Ok(Self {
                path: command.program().to_owned(),
                remove_on_drop: false,
                temporary_directory: None,
            });
        }
        let expected = command
            .executable_identity()
            .ok_or(ProcessError::ExecutableUnavailable)?;
        let mut source = open_executable_file(command.program())?;
        let before = source
            .metadata()
            .map_err(|_| ProcessError::ExecutableUnavailable)?;
        if !before.is_file() {
            return Err(ProcessError::ExecutableUnavailable);
        }
        let before_stamp = FileStamp::from_metadata(&before);
        let bytes = read_bounded_executable(&mut source, before.len())?;
        let after = source
            .metadata()
            .map_err(|_| ProcessError::ExecutableUnavailable)?;
        if before_stamp != FileStamp::from_metadata(&after) {
            return Err(ProcessError::ExecutableDrift);
        }
        if typed_of::<crate::ToolchainSchema>(&bytes) != expected.digest() {
            return Err(ProcessError::ExecutableDrift);
        }
        Self::from_bytes(&bytes, &before)
    }

    fn from_bytes(bytes: &[u8], source: &Metadata) -> Result<Self, ProcessError> {
        let directory = private_temp_directory("exec")?;
        let path = directory.join("exec.out");
        let result = (|| {
            let mut file = OpenOptions::new()
                .create_new(true)
                .write(true)
                .read(true)
                .open(&path)
                .map_err(|_| ProcessError::TemporaryFile)?;
            file.write_all(bytes).map_err(|_| ProcessError::Io)?;
            file.sync_all().map_err(|_| ProcessError::Io)?;
            let mut permissions = source.permissions();
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                permissions.set_mode(0o500);
            }
            #[cfg(not(unix))]
            permissions.set_readonly(true);
            file.set_permissions(permissions)
                .map_err(|_| ProcessError::Io)?;
            file.seek(SeekFrom::Start(0))
                .map_err(|_| ProcessError::Io)?;
            let mut copied = Vec::new();
            file.read_to_end(&mut copied)
                .map_err(|_| ProcessError::Io)?;
            if typed_of::<crate::ToolchainSchema>(&copied)
                != typed_of::<crate::ToolchainSchema>(bytes)
            {
                return Err(ProcessError::ExecutableDrift);
            }
            Ok(())
        })();
        if let Err(error) = result {
            let _ = remove_file(&path);
            let _ = remove_dir(&directory);
            return Err(error);
        }
        Ok(Self {
            path,
            remove_on_drop: true,
            temporary_directory: Some(directory),
        })
    }

    pub(crate) fn path(&self) -> &Path {
        &self.path
    }
}

fn read_bounded_executable(file: &mut File, declared: u64) -> Result<Vec<u8>, ProcessError> {
    if declared > MAX_EXECUTABLE_BYTES {
        return Err(ProcessError::ExecutableLimit);
    }
    file.seek(SeekFrom::Start(0))
        .map_err(|_| ProcessError::Io)?;
    let capacity = usize::try_from(declared).map_err(|_| ProcessError::ExecutableLimit)?;
    let mut bytes = Vec::new();
    bytes
        .try_reserve_exact(capacity)
        .map_err(|_| ProcessError::ExecutableLimit)?;
    file.take(MAX_EXECUTABLE_BYTES.saturating_add(1))
        .read_to_end(&mut bytes)
        .map_err(|_| ProcessError::Io)?;
    if bytes.len() as u64 > MAX_EXECUTABLE_BYTES {
        return Err(ProcessError::ExecutableLimit);
    }
    Ok(bytes)
}

fn read_bounded_executable_until<C: CancellationObserver + ?Sized>(
    file: &mut File,
    declared: u64,
    cancellation: &C,
    deadline: std::time::Instant,
) -> Result<Vec<u8>, ProcessError> {
    if declared > MAX_EXECUTABLE_BYTES {
        return Err(ProcessError::ExecutableLimit);
    }
    file.seek(SeekFrom::Start(0))
        .map_err(|_| ProcessError::Io)?;
    let capacity = usize::try_from(declared).map_err(|_| ProcessError::ExecutableLimit)?;
    let mut bytes = Vec::new();
    bytes
        .try_reserve_exact(capacity)
        .map_err(|_| ProcessError::ExecutableLimit)?;
    let mut remaining = declared;
    let mut scratch = [0_u8; 64 * 1024];
    while remaining > 0 {
        process_checkpoint(cancellation, deadline)?;
        let limit = usize::try_from(remaining.min(scratch.len() as u64))
            .map_err(|_| ProcessError::ExecutableLimit)?;
        let read = file
            .read(&mut scratch[..limit])
            .map_err(|_| ProcessError::Io)?;
        if read == 0 {
            return Err(ProcessError::ExecutableDrift);
        }
        bytes.extend_from_slice(&scratch[..read]);
        remaining -= read as u64;
    }
    process_checkpoint(cancellation, deadline)?;
    let mut extra = [0_u8; 1];
    if file.read(&mut extra).map_err(|_| ProcessError::Io)? != 0 {
        return Err(ProcessError::ExecutableDrift);
    }
    Ok(bytes)
}

fn toolchain_digest_until<C: CancellationObserver + ?Sized>(
    bytes: &[u8],
    cancellation: &C,
    deadline: std::time::Instant,
) -> Result<ToolchainId, ProcessError> {
    let mut hasher = backend_version::ObjectVersionHasher::new(
        backend_version::SchemaIdentity::new(
            crate::ToolchainSchema::DOMAIN,
            crate::ToolchainSchema::TYPE,
            crate::ToolchainSchema::VERSION,
        ),
        bytes.len(),
    )
    .map_err(|_| ProcessError::ExecutableLimit)?;
    for chunk in bytes.chunks(64 * 1024) {
        process_checkpoint(cancellation, deadline)?;
        hasher
            .update(chunk)
            .map_err(|_| ProcessError::ExecutableDrift)?;
    }
    process_checkpoint(cancellation, deadline)?;
    hasher
        .finish_version::<crate::ToolchainSchema>()
        .map_err(|_| ProcessError::ExecutableDrift)
}

fn private_temp_directory(label: &str) -> Result<PathBuf, ProcessError> {
    let base = std::env::temp_dir();
    for _ in 0..16 {
        let nonce = EXECUTABLE_TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);
        let directory = base.join(format!(
            ".backend-compile-{}-{nonce}-{label}",
            std::process::id()
        ));
        match std::fs::create_dir(&directory) {
            Ok(()) => {
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    let Ok(metadata) = std::fs::metadata(&directory) else {
                        let _ = remove_dir(&directory);
                        return Err(ProcessError::TemporaryFile);
                    };
                    let mut permissions = metadata.permissions();
                    permissions.set_mode(0o700);
                    if std::fs::set_permissions(&directory, permissions).is_err() {
                        let _ = remove_dir(&directory);
                        return Err(ProcessError::TemporaryFile);
                    }
                }
                return Ok(directory);
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(_) => return Err(ProcessError::TemporaryFile),
        }
    }
    Err(ProcessError::TemporaryFile)
}

#[cfg(target_os = "macos")]
fn is_macho(bytes: &[u8]) -> bool {
    matches!(
        bytes.get(..4),
        Some(
            [0xfe, 0xed, 0xfa, 0xce | 0xcf]
                | [0xce | 0xcf, 0xfa, 0xed, 0xfe]
                | [0xca, 0xfe, 0xba, 0xbe | 0xbf]
                | [0xbe | 0xbf, 0xba, 0xfe, 0xca],
        )
    )
}

#[cfg(target_os = "macos")]
fn directory_is_writable(file: &Path) -> bool {
    let Some(directory) = file.parent() else {
        return false;
    };
    let nonce = EXECUTABLE_TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);
    let probe = directory.join(format!(
        ".backend-compile-write-probe-{}-{nonce}",
        std::process::id()
    ));
    match OpenOptions::new().create_new(true).write(true).open(&probe) {
        Ok(_) => {
            let _ = remove_file(probe);
            true
        }
        Err(_) => false,
    }
}

#[cfg(target_os = "macos")]
fn file_is_writable_or_changed(path: &Path, expected: FileStamp) -> bool {
    use std::os::unix::fs::OpenOptionsExt;

    let mut options = OpenOptions::new();
    options
        .write(true)
        .custom_flags((rustix::fs::OFlags::NONBLOCK | rustix::fs::OFlags::CLOEXEC).bits() as i32);
    match options.open(path) {
        Ok(_) => true,
        Err(_) => std::fs::metadata(path)
            .map(|metadata| !metadata.is_file() || FileStamp::from_metadata(&metadata) != expected)
            .unwrap_or(true),
    }
}

impl Drop for ExecutableLease {
    fn drop(&mut self) {
        if self.remove_on_drop {
            let _ = remove_file(&self.path);
            if let Some(directory) = &self.temporary_directory {
                let _ = remove_dir(directory);
            }
        }
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::{
        fs,
        os::unix::fs::PermissionsExt,
        sync::mpsc,
        time::{Duration, SystemTime, UNIX_EPOCH},
    };

    fn scratch_directory(label: &str) -> Result<PathBuf, Box<dyn std::error::Error>> {
        let stamp = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos();
        let directory = std::env::temp_dir().join(format!(
            "backend-executable-identity-{}-{label}-{stamp}",
            std::process::id()
        ));
        fs::create_dir(&directory)?;
        fs::set_permissions(&directory, fs::Permissions::from_mode(0o700))?;
        Ok(directory)
    }

    #[test]
    fn executable_identity_refuses_a_fifo_without_waiting_for_a_writer()
    -> Result<(), Box<dyn std::error::Error>> {
        use std::os::unix::fs::OpenOptionsExt;

        let directory = scratch_directory("fifo")?;
        let fifo = directory.join("candidate");
        let status = std::process::Command::new("mkfifo").arg(&fifo).status()?;
        assert!(status.success(), "mkfifo fixture creation failed");
        let candidate = fifo.clone();
        let (finished, completion) = mpsc::sync_channel(1);
        let worker = std::thread::spawn(move || {
            let result = ExecutableIdentity::from_path(&candidate);
            let _ = finished.send(());
            result
        });

        let completed_without_writer = completion.recv_timeout(Duration::from_secs(2)).is_ok();
        let release = if completed_without_writer {
            None
        } else {
            let mut options = OpenOptions::new();
            options.read(true).write(true).custom_flags(
                (rustix::fs::OFlags::NONBLOCK | rustix::fs::OFlags::CLOEXEC).bits() as i32,
            );
            // An RDWR FIFO handle releases an implementation that incorrectly blocks in
            // `File::open`, while also ensuring a delayed test thread cannot strand the suite.
            Some(options.open(&fifo)?)
        };
        let result = worker
            .join()
            .map_err(|_| std::io::Error::other("identity worker panicked"))?;
        drop(release);
        fs::remove_dir_all(&directory)?;

        assert!(
            completed_without_writer,
            "identity capture waited for a FIFO peer instead of refusing the special file"
        );
        assert!(matches!(result, Err(ProcessError::ExecutableUnavailable)));
        Ok(())
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn protected_macho_lease_uses_the_canonical_target_after_alias_replacement()
    -> Result<(), Box<dyn std::error::Error>> {
        use crate::{ProcessEnvironment, ProcessLimits, ProcessStdin, ProtocolDescriptor};
        use std::{os::unix::fs::symlink, process::Command};

        let original = Path::new("/usr/bin/true");
        let replacement = Path::new("/usr/bin/false");
        let canonical = fs::canonicalize(original)?;
        let bytes = fs::read(&canonical)?;
        if !is_macho(&bytes) {
            return Ok(());
        }
        let directory = scratch_directory("protected-macho")?;
        let alias = directory.join("tool");
        symlink(original, &alias)?;
        let artifact = ToolchainArtifact::from_path(&alias, Vec::new())?;
        let command = SupervisedCommand::for_authority_with_artifact(
            alias.clone(),
            Vec::new(),
            ProcessEnvironment::new(Vec::new())?,
            directory.clone(),
            ProcessStdin::null(),
            artifact,
            None,
            ProtocolDescriptor::cold(),
            ProcessLimits::new(64, 64, Duration::from_secs(2), 128)?,
        )?;
        let lease = ExecutableLease::prepare_until(
            &command,
            &std::sync::atomic::AtomicBool::new(false),
            std::time::Instant::now() + Duration::from_secs(5),
        )?;
        if lease.remove_on_drop {
            // Test installations where the system binary is writable exercise the private-copy
            // path instead of the protected-system-image path.
            drop(lease);
            fs::remove_dir_all(&directory)?;
            return Ok(());
        }
        assert_eq!(lease.path(), canonical);

        fs::remove_file(&alias)?;
        symlink(replacement, &alias)?;
        let status = Command::new(lease.path()).status()?;
        assert!(
            status.success(),
            "the admitted canonical image must still run"
        );

        drop(lease);
        fs::remove_dir_all(&directory)?;
        Ok(())
    }
}
