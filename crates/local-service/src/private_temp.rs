//! Private temporary directories shared by product code and its fixtures.

use std::{io, path::Path, path::PathBuf};

/// Creates an owner-only temporary directory and keeps its cleanup tied to the
/// returned `TempDir`'s lifetime.
pub(crate) fn create(label: &str) -> io::Result<tempfile::TempDir> {
    create_inner(label, false)
}

/// Creates an owner-only temporary directory with room for a Unix socket
/// endpoint. Socket-backed fixtures use this variant; file-only stores do not
/// inherit the unrelated socket path limit.
pub(crate) fn create_for_socket(label: &str) -> io::Result<tempfile::TempDir> {
    create_inner(label, true)
}

fn create_inner(label: &str, require_endpoint_room: bool) -> io::Result<tempfile::TempDir> {
    let label = label
        .chars()
        .filter(|character| character.is_ascii_alphanumeric())
        .take(6)
        .collect::<String>();
    let prefix = if label.is_empty() {
        "nx-private-".to_owned()
    } else {
        format!("nx-{label}-")
    };

    let mut roots = Vec::new();
    #[cfg(unix)]
    {
        roots.push(PathBuf::from("/tmp"));
        roots.push(PathBuf::from("/var/tmp"));
    }
    roots.push(std::env::temp_dir());
    roots.dedup();

    let mut last_error = None;
    for root in roots {
        #[cfg(unix)]
        let root = match root.canonicalize() {
            Ok(root) => root,
            Err(error) => {
                last_error = Some(error);
                continue;
            }
        };
        let directory = match tempfile::Builder::new().prefix(&prefix).tempdir_in(&root) {
            Ok(directory) => directory,
            Err(error) => {
                last_error = Some(error);
                continue;
            }
        };
        if let Err(error) = verify_private(directory.path()) {
            last_error = Some(error);
            continue;
        }
        #[cfg(unix)]
        if require_endpoint_room {
            if let Err(error) = verify_endpoint_room(directory.path()) {
                last_error = Some(error);
                continue;
            }
        }
        #[cfg(not(unix))]
        let _ = require_endpoint_room;
        return Ok(directory);
    }

    Err(last_error.unwrap_or_else(|| {
        io::Error::new(
            io::ErrorKind::NotFound,
            "no suitable private temporary directory is available",
        )
    }))
}

fn verify_private(path: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        let capability = backend_platform::DirectoryCapability::open(path)?;
        capability.restrict_private()?;
        capability.validate_private()?;
        capability.verify_path(path)
    }
    #[cfg(windows)]
    {
        backend_platform::win32::security::restrict_to_current_user(path)?;
        let capability = backend_platform::DirectoryCapability::open(path)?;
        capability.validate_private()?;
        capability.verify_path(path)
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = path;
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "private temporary directories are unsupported on this platform",
        ))
    }
}

#[cfg(unix)]
fn verify_endpoint_room(path: &Path) -> io::Result<()> {
    const ENDPOINT: &str = "backend-test-local-owner-0123456789abcdef0123456789abcdef.sock";
    let canonical = path.canonicalize()?;
    if backend_engine::UnixEndpointRef::new(&path.join(ENDPOINT)).is_err()
        || backend_engine::UnixEndpointRef::new(&canonical.join(ENDPOINT)).is_err()
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "temporary directory leaves insufficient room for a Unix endpoint",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::create;
    #[cfg(unix)]
    use super::create_for_socket;

    #[cfg(any(unix, windows))]
    #[allow(clippy::expect_used)]
    #[test]
    fn private_temp_directory_is_admitted_and_removed_with_its_owner() {
        let directory = create("shared product fixture").expect("private temp directory");
        let path = directory.path().to_path_buf();
        assert!(path.is_dir());
        let capability = backend_platform::DirectoryCapability::open(&path)
            .expect("open private directory capability");
        capability
            .validate_private()
            .expect("directory is owner-only");
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt as _;
            let metadata = std::fs::symlink_metadata(&path).expect("directory metadata");
            assert!(metadata.is_dir());
            assert!(!metadata.file_type().is_symlink());
            assert_eq!(metadata.uid(), rustix::process::geteuid().as_raw());
            assert_eq!(metadata.mode() & 0o777, 0o700);
        }
        drop(capability);
        drop(directory);
        assert!(!path.exists(), "TempDir owns cleanup for its full lifetime");
    }

    #[cfg(unix)]
    #[allow(clippy::expect_used)]
    #[test]
    fn socket_temp_directory_leaves_endpoint_room() {
        let directory = create_for_socket("socket fixture").expect("socket temp directory");
        assert!(super::verify_endpoint_room(directory.path()).is_ok());
    }
}
