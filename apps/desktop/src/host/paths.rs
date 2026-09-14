//! Workspace path discovery, with one canonical spelling per data directory.
//! The local service's endpoint is a hash of the data directory's text, so two
//! launch contexts that reach the same folder differently must agree on it.
//!
//! This matters in practice: a shell launch resolves `/tmp/demo` while a
//! `LaunchServices` launch resolves `/private/tmp/demo`, and the two hash to two
//! different sockets even though they are one folder holding one owner lock.
//! Canonicalising the data directory before the endpoint is derived is what
//! makes "attach to the live owner" reliable rather than lucky.

use backend_runtime::{RuntimeError, WorkspacePaths};

/// Discovers the project session and pins it to a canonical data directory.
///
/// # Errors
/// Returns an error when the current directory cannot be read, a configured
/// path is empty, or the private state directory cannot be prepared.
pub(crate) fn discover() -> Result<WorkspacePaths, RuntimeError> {
    let discovered = WorkspacePaths::discover(None, None, None)?;
    discovered.initialize()?;
    let Ok(canonical) = discovered.data().canonicalize() else {
        return Ok(discovered);
    };
    if canonical == discovered.data() {
        return Ok(discovered);
    }
    let repinned = WorkspacePaths::discover(
        Some(discovered.project().to_path_buf()),
        Some(canonical),
        None,
    )?;
    repinned.initialize()?;
    Ok(repinned)
}
