//! Process lifecycle: workspace paths, the owner lease, and the first frame.
//! One rule governs this module: a live owner always wins over a new one.
//! Nothing here renders; it only decides which service this window talks to.

pub(crate) mod aside;
pub(crate) mod bootstrap;
pub(crate) mod close;
pub(crate) mod editor;
#[cfg(test)]
mod embedded_owner_tests;
pub(crate) mod launch;
pub(crate) mod lease;
#[cfg(test)]
mod lifecycle_tests;
pub(crate) mod menus;
pub(crate) mod owner;
mod observation;
pub(crate) mod paths;
pub(crate) mod registry;
pub(crate) mod toolchain;
#[cfg(test)]
mod window_first_tests;
pub(crate) mod window_size;

/// Creates `path` and every missing parent owner-only. The owner refuses a
/// state directory any other user could enter: on Unix a test that creates
/// one with the process's umask (022) gets 0755, so each level is 0700; on
/// Windows an inherited DACL is refused, so each missing level is created
/// with the platform's protected owner-only DACL (the production
/// application-root walk), and an existing level must already be private.
///
/// # Errors
/// The operating system's, from creating a directory, or a refusal of an
/// existing non-private level, link, or relative path (Windows).
#[cfg(test)]
pub(crate) fn private_dir(path: &std::path::Path) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt as _;
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(path)
    }
    #[cfg(windows)]
    {
        paths::ensure_private_tree(path)
    }
}

/// Where test scratch roots live: `/tmp` on Unix (under nix `temp_dir()`
/// makes an owner's socket path longer than `sockaddr_un` allows), and the
/// per-user temporary directory on Windows, which has no `/tmp` (a rooted
/// `/tmp` there is relative to the current drive and not private).
#[cfg(test)]
pub(crate) fn scratch_base() -> std::path::PathBuf {
    #[cfg(unix)]
    {
        std::path::PathBuf::from("/tmp")
    }
    #[cfg(windows)]
    {
        std::env::temp_dir()
    }
}
