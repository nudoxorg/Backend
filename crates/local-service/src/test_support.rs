//! Fixtures shared by test modules.

use std::io;
use std::path::{Path, PathBuf};

/// Makes `path` private to the current user the way the platform's durable
/// state requires: mode 0700 on Unix, a protected owner-only DACL on Windows.
///
/// Durable workspace objects refuse a root that anyone else could enter, so a
/// test directory created with the process's default permissions is not a
/// valid root on either platform until this has run.
pub(crate) fn make_private(path: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))
    }
    #[cfg(windows)]
    {
        backend_platform::win32::security::restrict_to_current_user(path)
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = path;
        Ok(())
    }
}

/// A bindable local-socket path for a test listener, unique to this call.
///
/// The per-session macOS temporary directory does not leave room for a
/// bindable `sun_path`, so this falls back to `/tmp` when the preferred
/// location does not fit.
pub(crate) fn socket_path(label: &str) -> PathBuf {
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |duration| duration.as_nanos());
    let leaf = format!("backend-locald-{label}-{nonce}.sock");
    let preferred = std::env::temp_dir().join(&leaf);
    if backend_engine::UnixEndpointRef::new(&preferred).is_ok() {
        preferred
    } else {
        Path::new("/tmp").join(leaf)
    }
}
