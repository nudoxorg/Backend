//! Fixtures shared by test modules.

use std::io;
use std::path::Path;

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
