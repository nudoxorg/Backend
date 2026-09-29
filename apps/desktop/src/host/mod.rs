//! Process lifecycle: workspace paths, the owner lease, and the first frame.
//! One rule governs this module: a live owner always wins over a new one.
//! Nothing here renders; it only decides which service this window talks to.

pub(crate) mod editor;
pub(crate) mod launch;
pub(crate) mod lease;
pub(crate) mod owner;
pub(crate) mod paths;
#[cfg(test)]
mod window_first_tests;

/// Creates `path` and every missing parent owner-only (0700). The owner
/// refuses a state directory any other user could enter, and a test that
/// creates one with the process's umask (022) gets 0755.
///
/// # Errors
/// The operating system's, from creating a directory.
#[cfg(test)]
pub(crate) fn private_dir(path: &std::path::Path) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt as _;
        std::fs::DirBuilder::new().recursive(true).mode(0o700).create(path)
    }
    #[cfg(not(unix))]
    {
        std::fs::create_dir_all(path)
    }
}
