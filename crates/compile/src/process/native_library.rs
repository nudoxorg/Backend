//! A bounded private lease for the one admitted Linux libgcc dependency.
use super::{
    ExecutableIdentity, ProcessEnvironment, ProcessReceipt, ToolchainArtifact,
    identity::private_temp_directory,
};
use crate::{CancellationObserver, ProcessError, ToolchainId, ToolchainSchema, typed_of};
use std::{
    io::{Read, Write},
    path::{Path, PathBuf},
    time::Instant,
};

const MAX_LIBRARY_BYTES: usize = 16 * 1024 * 1024;
const LIBRARY_NAME: &str = "libgcc_s.so.1";

fn checkpoint<C: CancellationObserver + ?Sized>(
    cancellation: &C,
    deadline: Instant,
) -> Result<(), ProcessError> {
    if cancellation.is_cancelled() {
        Err(ProcessError::Cancelled)
    } else if Instant::now() >= deadline {
        Err(ProcessError::Deadline)
    } else {
        Ok(())
    }
}

fn read_library<C: CancellationObserver + ?Sized>(
    path: &Path,
    cancellation: &C,
    deadline: Instant,
) -> Result<Vec<u8>, ProcessError> {
    use rustix::fs::{Mode, OFlags, open};
    checkpoint(cancellation, deadline)?;
    let fd = open(
        path,
        OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .map_err(|_| ProcessError::ExecutableUnavailable)?;
    let mut file = std::fs::File::from(fd);
    let metadata = file
        .metadata()
        .map_err(|_| ProcessError::ExecutableUnavailable)?;
    if !metadata.is_file() {
        return Err(ProcessError::ExecutableUnavailable);
    }
    if metadata.len() > MAX_LIBRARY_BYTES as u64 {
        return Err(ProcessError::ExecutableLimit);
    }
    let mut bytes = Vec::new();
    let mut chunk = [0u8; 64 * 1024];
    loop {
        checkpoint(cancellation, deadline)?;
        let n = file.read(&mut chunk).map_err(|_| ProcessError::Io)?;
        if n == 0 {
            break;
        }
        if bytes.len().saturating_add(n) > MAX_LIBRARY_BYTES {
            return Err(ProcessError::ExecutableLimit);
        }
        bytes.extend_from_slice(&chunk[..n]);
    }
    checkpoint(cancellation, deadline)?;
    Ok(bytes)
}

/// Exact, private, no-follow copy of one already-admitted `libgcc_s.so.1`.
///
/// This type is a loader-custody mechanism, not authority to admit an image.
/// The caller must bind the executable, library and source-produced basis to
/// trusted installed build evidence before constructing a command.
#[derive(Debug)]
pub struct NativeLibraryLease {
    directory: Option<PathBuf>,
    digest: ToolchainId,
}

impl NativeLibraryLease {
    /// Copy and verify the fixed dependency under one cancellation/deadline.
    ///
    /// # Errors
    /// Rejects wrong dependency identity, unsafe path, bounds or interrupted I/O.
    pub fn prepare<C: CancellationObserver + ?Sized>(
        path: &Path,
        identity: &ExecutableIdentity,
        artifact: &ToolchainArtifact,
        cancellation: &C,
        deadline: Instant,
    ) -> Result<Self, ProcessError> {
        if !path.is_absolute()
            || path.file_name().and_then(|n| n.to_str()) != Some(LIBRARY_NAME)
            || artifact.dependencies() != [identity.digest()]
        {
            return Err(ProcessError::IdentityMismatch);
        }
        let bytes = read_library(path, cancellation, deadline)?;
        let digest = typed_of::<ToolchainSchema>(&bytes);
        checkpoint(cancellation, deadline)?;
        if digest != identity.digest() {
            return Err(ProcessError::ExecutableDrift);
        }
        let directory = private_temp_directory("native-lib")?;
        let private_path = directory.join(LIBRARY_NAME);
        let result = (|| {
            let mut output = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&private_path)
                .map_err(|_| ProcessError::TemporaryFile)?;
            for chunk in bytes.chunks(64 * 1024) {
                checkpoint(cancellation, deadline)?;
                output.write_all(chunk).map_err(|_| ProcessError::Io)?;
            }
            output.sync_all().map_err(|_| ProcessError::Io)?;
            let mut permissions = output
                .metadata()
                .map_err(|_| ProcessError::Io)?
                .permissions();
            use std::os::unix::fs::PermissionsExt as _;
            permissions.set_mode(0o400);
            output
                .set_permissions(permissions)
                .map_err(|_| ProcessError::Io)?;
            drop(output);
            let lease = Self {
                directory: Some(directory.clone()),
                digest,
            };
            lease.verify(cancellation, deadline)?;
            Ok(lease)
        })();
        if result.is_err() {
            let _ = std::fs::remove_file(&private_path);
            let _ = std::fs::remove_dir(&directory);
        }
        result
    }

    /// Return the exact admitted dependency identity.
    #[must_use]
    pub const fn digest(&self) -> ToolchainId {
        self.digest
    }

    /// Return the only permitted loader environment for this private lease.
    ///
    /// # Errors
    /// Rejects an unavailable or malformed private directory path.
    pub fn environment(&self) -> Result<ProcessEnvironment, ProcessError> {
        let path = self
            .directory
            .as_deref()
            .and_then(Path::to_str)
            .ok_or(ProcessError::InvalidEnvironment)?;
        if path.is_empty() || path.contains(':') {
            return Err(ProcessError::InvalidEnvironment);
        }
        ProcessEnvironment::new(vec![("LD_LIBRARY_PATH".into(), path.into())])
    }

    /// Recheck the private library bytes against the admitted digest.
    ///
    /// # Errors
    /// Returns drift, bounds, cancellation or deadline failures.
    pub fn verify<C: CancellationObserver + ?Sized>(
        &self,
        cancellation: &C,
        deadline: Instant,
    ) -> Result<(), ProcessError> {
        let directory = self
            .directory
            .as_ref()
            .ok_or(ProcessError::ExecutableUnavailable)?;
        let bytes = read_library(&directory.join(LIBRARY_NAME), cancellation, deadline)?;
        let digest = typed_of::<ToolchainSchema>(&bytes);
        checkpoint(cancellation, deadline)?;
        if digest != self.digest {
            return Err(ProcessError::ExecutableDrift);
        }
        Ok(())
    }

    /// Remove private files only after the supervised child is known reaped.
    ///
    /// # Errors
    /// Returns an error if the receipt is not reaped or cleanup fails.
    pub fn close_after_reaped(&mut self, receipt: &ProcessReceipt) -> Result<(), ProcessError> {
        if !receipt.reaped() {
            return Err(ProcessError::NotReaped);
        }
        self.cleanup()
    }

    fn cleanup(&mut self) -> Result<(), ProcessError> {
        let Some(directory) = self.directory.take() else {
            return Ok(());
        };
        std::fs::remove_file(directory.join(LIBRARY_NAME)).map_err(|_| ProcessError::Io)?;
        std::fs::remove_dir(directory).map_err(|_| ProcessError::Io)
    }
}

impl Drop for NativeLibraryLease {
    fn drop(&mut self) {
        let _ = self.cleanup();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        os::unix::fs::{PermissionsExt as _, symlink},
        sync::atomic::AtomicBool,
        time::Duration,
    };

    #[test]
    fn bounded_private_library_checks_dependencies_tamper_and_cancellation() {
        let root = private_temp_directory("library-test").unwrap();
        let source = root.join(LIBRARY_NAME);
        std::fs::write(&source, b"library bytes").unwrap();
        let cancel = AtomicBool::new(false);
        let deadline = || Instant::now() + Duration::from_secs(10);
        let library = ExecutableIdentity::from_path_until(&source, &cancel, deadline()).unwrap();
        let executable =
            ExecutableIdentity::from_path_until(Path::new("/bin/sh"), &cancel, deadline()).unwrap();
        let invalid = ToolchainArtifact::new(executable.clone(), vec![]);
        assert!(matches!(
            NativeLibraryLease::prepare(&source, &library, &invalid, &cancel, deadline()),
            Err(ProcessError::IdentityMismatch)
        ));
        let artifact = ToolchainArtifact::new(executable, vec![library.digest()]);
        let lease =
            NativeLibraryLease::prepare(&source, &library, &artifact, &cancel, deadline()).unwrap();
        let private = lease.directory.as_ref().unwrap().clone();
        assert_eq!(
            lease.environment().unwrap().variables()[0].0,
            "LD_LIBRARY_PATH"
        );
        assert!(lease.verify(&cancel, deadline()).is_ok());
        let path = private.join(LIBRARY_NAME);
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        std::fs::write(&path, b"tampered").unwrap();
        assert!(matches!(
            lease.verify(&cancel, deadline()),
            Err(ProcessError::ExecutableDrift)
        ));
        drop(lease);
        assert!(!private.exists());
        cancel.store(true, std::sync::atomic::Ordering::Release);
        assert!(matches!(
            NativeLibraryLease::prepare(&source, &library, &artifact, &cancel, deadline()),
            Err(ProcessError::Cancelled)
        ));
        cancel.store(false, std::sync::atomic::Ordering::Release);
        assert!(matches!(
            NativeLibraryLease::prepare(&source, &library, &artifact, &cancel, Instant::now()),
            Err(ProcessError::Deadline)
        ));
        std::fs::remove_file(&source).unwrap();
        symlink("/bin/sh", &source).unwrap();
        assert!(matches!(
            NativeLibraryLease::prepare(&source, &library, &artifact, &cancel, deadline()),
            Err(ProcessError::ExecutableUnavailable)
        ));
        std::fs::remove_file(&source).unwrap();
        std::fs::remove_dir(root).unwrap();
    }
}
