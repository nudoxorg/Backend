//! Content identities, bounded digest caching, and executable leases.

use super::SupervisedCommand;
use crate::{ProcessError, ToolchainId, typed_of};
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
        let mut file = File::open(path).map_err(|_| ProcessError::ExecutableUnavailable)?;
        let before = file
            .metadata()
            .map_err(|_| ProcessError::ExecutableUnavailable)?;
        if !before.is_file() {
            return Err(ProcessError::ExecutableUnavailable);
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
        let mut bytes = Vec::new();
        file.read_to_end(&mut bytes).map_err(|_| ProcessError::Io)?;
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
        let mut source =
            File::open(command.program()).map_err(|_| ProcessError::ExecutableUnavailable)?;
        let before = source
            .metadata()
            .map_err(|_| ProcessError::ExecutableUnavailable)?;
        if !before.is_file() {
            return Err(ProcessError::ExecutableUnavailable);
        }
        let before_stamp = FileStamp::from_metadata(&before);
        let mut bytes = Vec::new();
        source
            .read_to_end(&mut bytes)
            .map_err(|_| ProcessError::Io)?;
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
        let source_path = std::fs::canonicalize(command.program())
            .map_err(|_| ProcessError::ExecutableUnavailable)?;

        // A signed Mach-O must retain its embedded code-signing envelope. A
        // caller-writable file or source directory permits a private
        // byte-for-byte copy, which also prevents a later chmod or in-place
        // write from changing the admitted image.
        let file_is_writable = OpenOptions::new().write(true).open(&source_path).is_ok();
        if file_is_writable || directory_is_writable(source_path.as_path()) {
            return Self::from_bytes(&bytes, &before);
        }
        // SIP-protected system binaries can reject private execution copies.
        // Their file and parent directory are both outside caller control, so
        // retaining the original signed path does not expose a caller-level
        // rename or in-place mutation race.
        Ok(Self {
            path: command.program().to_owned(),
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
        let mut source =
            File::open(command.program()).map_err(|_| ProcessError::ExecutableUnavailable)?;
        let before = source
            .metadata()
            .map_err(|_| ProcessError::ExecutableUnavailable)?;
        if !before.is_file() {
            return Err(ProcessError::ExecutableUnavailable);
        }
        let before_stamp = FileStamp::from_metadata(&before);
        let mut bytes = Vec::new();
        source
            .read_to_end(&mut bytes)
            .map_err(|_| ProcessError::Io)?;
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
