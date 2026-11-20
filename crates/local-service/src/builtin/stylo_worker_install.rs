//! Admission of the fixed installed Stylo companion against the owner image.
//!
//! The release build embeds a typed digest of the final installed worker bytes.
//! A missing anchor, missing worker, or mismatch leaves this optional lane
//! unavailable. Runtime environment and project paths cannot supply the anchor.

use super::stylo_worker::StyloWorkerPermit;
use backend_compile::{ExecutableIdentity, ProcessError, ToolchainArtifact};
use std::{
    path::Path,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Instant,
};

/// Fixed execution budget for observing the optional installed companion.
/// This is adapter policy; elapsed time is not part of analysis identity.
pub(super) const INSTALLED_WORKER_ADMISSION_BUDGET: std::time::Duration =
    std::time::Duration::from_secs(30);

const fn valid_compiled_anchor(value: &str) -> bool {
    let bytes = value.as_bytes();
    if bytes.len() != 64 {
        return false;
    }
    let mut i = 0;
    let mut nonzero = false;
    while i < bytes.len() {
        let b = bytes[i];
        if !((b >= b'0' && b <= b'9') || (b >= b'a' && b <= b'f')) {
            return false;
        }
        nonzero |= b != b'0';
        i += 1;
    }
    nonzero
}

const _: () = if let Some(value) = option_env!("NUDOX_BUILD_STYLO_WORKER_ID") {
    assert!(
        valid_compiled_anchor(value),
        "invalid compiled Stylo worker identity"
    );
};

/// Selects only the sibling of this executable under the scan's deadline.
/// Cancellation and deadline expiry stay distinct from a missing capability.
pub(super) fn selected_installed_stylo_worker_until(
    cancellation: &AtomicBool,
    deadline: Instant,
) -> Result<Option<Arc<StyloWorkerPermit>>, ProcessError> {
    checkpoint(cancellation, deadline)?;
    let Some(expected) = option_env!("NUDOX_BUILD_STYLO_WORKER_ID").and_then(decode_anchor) else {
        return Ok(None);
    };
    let Ok(owner) = std::env::current_exe() else {
        checkpoint(cancellation, deadline)?;
        return Ok(None);
    };
    admit_sibling_until(&owner, expected, cancellation, deadline)
}

fn checkpoint(cancellation: &AtomicBool, deadline: Instant) -> Result<(), ProcessError> {
    if cancellation.load(Ordering::Acquire) {
        Err(ProcessError::Cancelled)
    } else if Instant::now() >= deadline {
        Err(ProcessError::Deadline)
    } else {
        Ok(())
    }
}

fn decode_anchor(value: &str) -> Option<[u8; 32]> {
    if !valid_compiled_anchor(value) {
        return None;
    }
    let mut out = [0; 32];
    for (slot, pair) in out.iter_mut().zip(value.as_bytes().chunks_exact(2)) {
        let digit = |b| if b <= b'9' { b - b'0' } else { b - b'a' + 10 };
        *slot = digit(pair[0]) * 16 + digit(pair[1]);
    }
    (out != [0; 32]).then_some(out)
}

#[cfg(target_os = "linux")]
fn installed_paths(owner: &Path) -> Option<(std::path::PathBuf, std::path::PathBuf)> {
    let owner = owner.canonicalize().ok()?;
    let bin = owner.parent()?;
    if bin.file_name()?.to_str()? != "bin" {
        return None;
    }
    let library_directory = bin.parent()?.join("lib");
    if !std::fs::symlink_metadata(&library_directory)
        .ok()?
        .file_type()
        .is_dir()
    {
        return None;
    }
    let library = library_directory.join("libgcc_s.so.1");
    let library_metadata = std::fs::symlink_metadata(&library).ok()?;
    if !library_metadata.file_type().is_file() || library_metadata.len() > 16 * 1024 * 1024 {
        return None;
    }
    let candidate = owner.with_file_name(format!(
        "backend-stylo-worker{}",
        std::env::consts::EXE_SUFFIX
    ));
    let metadata = std::fs::symlink_metadata(&candidate).ok()?;
    if !metadata.file_type().is_file() {
        return None;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        if metadata.permissions().mode() & 0o111 == 0 {
            return None;
        }
    }
    Some((candidate, library))
}

#[cfg(target_os = "linux")]
fn admit_sibling_until(
    owner: &Path,
    expected: [u8; 32],
    cancellation: &AtomicBool,
    deadline: Instant,
) -> Result<Option<Arc<StyloWorkerPermit>>, ProcessError> {
    checkpoint(cancellation, deadline)?;
    let paths = installed_paths(owner);
    checkpoint(cancellation, deadline)?;
    let Some((candidate, library)) = paths else {
        return Ok(None);
    };
    let capture =
        |path: &Path, maximum_bytes: u64| match ExecutableIdentity::from_path_until_with_limit(
            path,
            cancellation,
            deadline,
            maximum_bytes,
        ) {
            Ok(identity) => Ok(Some(identity)),
            Err(error @ (ProcessError::Cancelled | ProcessError::Deadline)) => Err(error),
            Err(_) => {
                checkpoint(cancellation, deadline)?;
                Ok(None)
            }
        };
    let Some(library_identity) = capture(&library, 16 * 1024 * 1024)? else {
        return Ok(None);
    };
    let Some(worker_identity) = capture(&candidate, backend_compile::MAX_EXECUTABLE_BYTES)? else {
        return Ok(None);
    };
    let artifact = ToolchainArtifact::new(worker_identity, vec![library_identity.digest()]);
    checkpoint(cancellation, deadline)?;
    if artifact.identity().to_bytes() != expected {
        return Ok(None);
    }
    let permit = StyloWorkerPermit::from_captured_linux_library(
        candidate,
        artifact,
        backend_frontend_stylo::support_fingerprint(),
        library,
        library_identity,
    )
    .ok()
    .map(Arc::new);
    checkpoint(cancellation, deadline)?;
    Ok(permit)
}

#[cfg(target_os = "macos")]
fn admit_sibling_until(
    owner: &Path,
    expected: [u8; 32],
    cancellation: &AtomicBool,
    deadline: Instant,
) -> Result<Option<Arc<StyloWorkerPermit>>, ProcessError> {
    use std::os::unix::fs::PermissionsExt as _;
    checkpoint(cancellation, deadline)?;
    let candidate = (|| {
        let owner = owner.canonicalize().ok()?;
        let macos = owner.parent()?;
        if macos.file_name()?.to_str()? != "MacOS"
            || macos.parent()?.file_name()?.to_str()? != "Contents"
        {
            return None;
        }
        let candidate = macos.join("backend-stylo-worker");
        let metadata = std::fs::symlink_metadata(&candidate).ok()?;
        if !metadata.file_type().is_file() || metadata.permissions().mode() & 0o111 == 0 {
            return None;
        }
        Some(candidate)
    })();
    checkpoint(cancellation, deadline)?;
    let Some(candidate) = candidate else {
        return Ok(None);
    };
    let identity = match ExecutableIdentity::from_path_until(&candidate, cancellation, deadline) {
        Ok(identity) => identity,
        Err(error @ (ProcessError::Cancelled | ProcessError::Deadline)) => return Err(error),
        Err(_) => {
            checkpoint(cancellation, deadline)?;
            return Ok(None);
        }
    };
    let artifact = ToolchainArtifact::new(identity, Vec::new());
    checkpoint(cancellation, deadline)?;
    if artifact.identity().to_bytes() != expected {
        return Ok(None);
    }
    let permit = StyloWorkerPermit::from_captured_macos_worker(
        candidate,
        artifact,
        backend_frontend_stylo::support_fingerprint(),
    )
    .ok()
    .map(Arc::new);
    checkpoint(cancellation, deadline)?;
    Ok(permit)
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn admit_sibling_until(
    _owner: &Path,
    _expected: [u8; 32],
    cancellation: &AtomicBool,
    deadline: Instant,
) -> Result<Option<Arc<StyloWorkerPermit>>, ProcessError> {
    checkpoint(cancellation, deadline)?;
    Ok(None)
}

#[cfg(all(test, target_os = "macos"))]
mod macos_tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt as _;

    #[test]
    fn macos_selection_requires_exact_bundle_sibling_and_anchor() {
        let dir = tempfile::tempdir().expect("private fixture");
        let macos = dir.path().join("Nudox.app/Contents/MacOS");
        std::fs::create_dir_all(&macos).expect("bundle fixture");
        let owner = macos.join("backend-locald");
        let worker = macos.join("backend-stylo-worker");
        std::fs::write(&owner, b"owner").expect("owner fixture");
        std::fs::write(&worker, b"signed worker fixture").expect("worker fixture");
        std::fs::set_permissions(&worker, std::fs::Permissions::from_mode(0o700))
            .expect("worker mode");
        let expected = ToolchainArtifact::from_path(&worker, Vec::new())
            .expect("identity")
            .identity()
            .to_bytes();
        let cancelled = AtomicBool::new(false);
        let future = || Instant::now() + std::time::Duration::from_secs(5);
        assert!(
            admit_sibling_until(&owner, expected, &cancelled, future())
                .expect("select")
                .is_some()
        );
        assert!(
            admit_sibling_until(&owner, [2; 32], &cancelled, future())
                .expect("wrong anchor")
                .is_none()
        );
        std::fs::write(&worker, b"tampered").expect("tamper");
        assert!(
            admit_sibling_until(&owner, expected, &cancelled, future())
                .expect("tamper")
                .is_none()
        );
        std::fs::remove_file(&worker).expect("remove worker");
        std::os::unix::fs::symlink(&owner, &worker).expect("symlink");
        assert!(
            admit_sibling_until(&owner, expected, &cancelled, future())
                .expect("symlink")
                .is_none()
        );
        assert!(matches!(
            admit_sibling_until(&owner, expected, &AtomicBool::new(true), future()),
            Err(ProcessError::Cancelled)
        ));
        assert!(matches!(
            admit_sibling_until(&owner, expected, &cancelled, Instant::now()),
            Err(ProcessError::Deadline)
        ));
    }
}

#[cfg(all(test, target_os = "linux"))]
fn admit_sibling(owner: &Path, expected: [u8; 32]) -> Option<Arc<StyloWorkerPermit>> {
    admit_sibling_until(
        owner,
        expected,
        &AtomicBool::new(false),
        Instant::now() + std::time::Duration::from_secs(5),
    )
    .expect("bounded fixture")
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt as _;

    #[test]
    fn selection_reports_cancelled_and_expired_before_missing_capability() {
        let owner = Path::new("/missing/bin/backend-locald");
        assert!(matches!(
            admit_sibling_until(
                owner,
                [1; 32],
                &AtomicBool::new(true),
                Instant::now() + std::time::Duration::from_secs(5)
            ),
            Err(ProcessError::Cancelled)
        ));
        assert!(matches!(
            admit_sibling_until(owner, [1; 32], &AtomicBool::new(false), Instant::now()),
            Err(ProcessError::Deadline)
        ));
        assert!(matches!(
            admit_sibling_until(
                owner,
                [1; 32],
                &AtomicBool::new(false),
                Instant::now() + std::time::Duration::from_secs(5)
            ),
            Ok(None)
        ));
    }

    #[test]
    fn selected_worker_requires_exact_compiled_anchor_and_regular_sibling() {
        let directory = tempfile::tempdir().expect("private test directory");
        let bin = directory.path().join("bin");
        let lib = directory.path().join("lib");
        std::fs::create_dir(&bin).expect("bin fixture");
        std::fs::create_dir(&lib).expect("lib fixture");
        let owner = bin.join("backend-locald");
        std::fs::write(&owner, b"owner").expect("owner fixture");
        let worker = bin.join("backend-stylo-worker");
        let library = lib.join("libgcc_s.so.1");
        assert!(admit_sibling(&owner, [1; 32]).is_none());
        std::fs::write(&worker, b"admitted worker").expect("worker fixture");
        std::fs::set_permissions(&worker, std::fs::Permissions::from_mode(0o700))
            .expect("executable");
        assert!(admit_sibling(&owner, [1; 32]).is_none());
        std::fs::write(&library, b"admitted library").expect("library fixture");
        let library_identity = ExecutableIdentity::from_path(&library).expect("library identity");
        let expected = ToolchainArtifact::from_path(&worker, vec![library_identity.digest()])
            .expect("identity")
            .identity()
            .to_bytes();
        assert!(admit_sibling(&owner, expected).is_some());
        assert!(admit_sibling(&owner, [2; 32]).is_none());
        std::fs::write(&library, b"tampered library").expect("replace library");
        assert!(admit_sibling(&owner, expected).is_none());
        std::fs::write(&library, b"admitted library").expect("restore library");
        std::fs::remove_file(&library).expect("remove library");
        std::os::unix::fs::symlink(&worker, &library).expect("library symlink");
        assert!(admit_sibling(&owner, expected).is_none());
        std::fs::remove_file(&library).expect("remove library symlink");
        std::fs::write(&library, b"admitted library").expect("restore library");
        std::fs::write(&worker, b"tampered worker").expect("replace worker");
        assert!(admit_sibling(&owner, expected).is_none());
        std::fs::remove_file(&worker).expect("remove worker");
        std::os::unix::fs::symlink(&owner, &worker).expect("symlink");
        assert!(admit_sibling(&owner, expected).is_none());
        assert!(decode_anchor("").is_none());
        assert!(decode_anchor(&"00".repeat(32)).is_none());
        assert_eq!(decode_anchor(&"ab".repeat(32)), Some([0xab; 32]));
    }
}
