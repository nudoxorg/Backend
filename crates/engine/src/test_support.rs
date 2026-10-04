//! Fixtures shared by the engine's unit tests.
//!
//! Persistent engine state lives in directories that are private to the current
//! user. On Windows that means a protected DACL, which a plain `create_dir_all`
//! under the system temporary directory never produces, and absolute paths need
//! a drive letter, which a POSIX-style literal such as `/opt/tool` lacks. The
//! helpers here give a test the same directories and paths the product would
//! be handed on each platform.

use std::{
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

static SEQUENCE: AtomicU64 = AtomicU64::new(0);

/// Creates a new directory that is private to the current user and returns its
/// resolved path.
///
/// The directory name carries the process id and a process-wide sequence, so
/// concurrent tests never collide.
///
/// # Panics
///
/// Panics when the directory cannot be created; a test fixture has no better
/// recovery.
#[must_use]
#[allow(
    clippy::expect_used,
    reason = "a fixture that cannot be created cannot be recovered by the test"
)]
pub(crate) fn private_directory(label: &str) -> PathBuf {
    let path = std::env::temp_dir().join(format!(
        "nudox-engine-{label}-{}-{}",
        std::process::id(),
        SEQUENCE.fetch_add(1, Ordering::Relaxed)
    ));
    create_private(&path).expect("create a private fixture directory");
    std::fs::canonicalize(&path).expect("resolve the private fixture directory")
}

#[cfg(windows)]
fn create_private(path: &Path) -> std::io::Result<()> {
    backend_platform::win32::workspace_fs::WorkspaceRoot::ensure_private_child_directory(path)
}

#[cfg(unix)]
fn create_private(path: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::{DirBuilderExt as _, PermissionsExt as _};
    std::fs::DirBuilder::new().mode(0o700).create(path)?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))
}

#[cfg(not(any(unix, windows)))]
fn create_private(path: &Path) -> std::io::Result<()> {
    std::fs::create_dir(path)
}

/// [`host_absolute`] for call sites that borrow a `&'static Path`, such as
/// toolchain fixtures. The path is leaked: a unit-test process is short-lived and
/// the fixture set is a handful of literals.
#[must_use]
pub(crate) fn host_path(posix: &str) -> &'static Path {
    Box::leak(host_absolute(posix).into_boxed_path())
}

/// Maps a POSIX-style absolute literal to an absolute path on this host.
///
/// `/host-a/bin/tool` is rooted but not absolute on Windows, where it needs a
/// drive. The mapping keeps the literal's components and adds the drive of the
/// temporary directory, so two literals that differ stay different paths.
#[must_use]
pub(crate) fn host_absolute(posix: &str) -> PathBuf {
    #[cfg(windows)]
    {
        let drive = std::env::temp_dir()
            .components()
            .next()
            .map(|component| component.as_os_str().to_owned())
            .unwrap_or_else(|| "C:".into());
        let mut path = PathBuf::from(drive);
        path.push("\\");
        path.extend(posix.trim_start_matches('/').split('/'));
        path
    }
    #[cfg(not(windows))]
    {
        PathBuf::from(posix)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_absolute_paths_are_absolute_and_keep_their_components() {
        let first = host_absolute("/host-a/bin/pyrefly");
        let second = host_absolute("/host-b/bin/pyrefly");
        assert!(first.is_absolute(), "{first:?}");
        assert_ne!(first, second);
        assert_eq!(first.file_name(), second.file_name());
        assert!(first.ends_with(Path::new("host-a").join("bin").join("pyrefly")));
    }

    #[test]
    fn private_directories_are_distinct_and_usable_by_the_platform_capability() {
        let first = private_directory("support");
        let second = private_directory("support");
        assert_ne!(first, second);
        assert!(
            backend_platform::DirectoryCapability::open(&first).is_ok(),
            "the platform opens a directory this helper created"
        );
        let _ = std::fs::remove_dir_all(first);
        let _ = std::fs::remove_dir_all(second);
    }
}
