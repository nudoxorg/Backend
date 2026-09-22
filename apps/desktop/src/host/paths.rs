//! Workspace path discovery, with one canonical spelling per data directory.
//! The local service's endpoint is a hash of the data directory's text, so two
//! launch contexts that reach the same folder differently must agree on it.
//!
//! This matters in practice: a shell launch resolves `/tmp/demo` while a
//! `LaunchServices` launch resolves `/private/tmp/demo`, and the two hash to two
//! different sockets even though they are one folder holding one owner lock.
//! Canonicalising the data directory before the endpoint is derived is what
//! makes "attach to the live owner" reliable rather than lucky.

use backend_runtime::{
    RuntimeError, WorkspacePaths, AUTHORITY_SECRET_ENV, DATA_ENV, ENDPOINT_ENV, PROJECT_ENV,
};
use std::path::{Path, PathBuf};

/// Discovers the project session and pins it to a canonical data directory.
///
/// # Errors
/// Returns an error when the current directory cannot be read, a configured
/// path is empty, or the private state directory cannot be prepared.
pub(crate) fn discover() -> Result<WorkspacePaths, RuntimeError> {
    // A launched desktop app has no reliable current directory. Finder,
    // Explorer, and a Linux desktop commonly start us in the user's home or
    // in `/`; treating that directory as the project is how the old GUI
    // opened a blank, apparently broken window unless BACKEND_PROJECT had
    // been exported first. Keep the shared runtime discovery for explicit
    // surface configuration and for useful shell launches from a repository,
    // but give a genuinely ambient GUI launch a durable user workspace plus a
    // harmless empty starter project. The first-run surface then asks for the
    // real folder and submits it through the same live index request as every
    // later add.
    let discovered = if ambient_gui_launch()
        && !looks_like_project(&std::env::current_dir().map_err(RuntimeError::Io)?)
    {
        ambient_paths(&application_data_root()?)?
    } else {
        WorkspacePaths::discover(None, None, None)?
    };
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

/// Builds the durable empty launch session used before the first folder is
/// chosen. Keeping this pure over its root lets the cold-restart test prove
/// that two ambient launches derive one workspace identity without mutating
/// process environment variables.
fn ambient_paths(user_root: &Path) -> Result<WorkspacePaths, RuntimeError> {
    let data = user_root.join("workspace");
    let starter = user_root.join("starter");
    std::fs::create_dir_all(&starter).map_err(RuntimeError::Io)?;
    WorkspacePaths::discover(Some(starter), Some(data), None)
}

/// Returns whether no surface explicitly selected a project/session.
fn ambient_gui_launch() -> bool {
    [PROJECT_ENV, DATA_ENV, ENDPOINT_ENV, AUTHORITY_SECRET_ENV]
        .into_iter()
        .all(|name| std::env::var_os(name).is_none())
}

/// Returns whether a directory is a plausible project boundary.
pub(crate) fn looks_like_project(path: &Path) -> bool {
    path.join(".git").exists()
        || [
            "Cargo.toml",
            "package.json",
            "go.mod",
            "pyproject.toml",
            "pom.xml",
            "build.gradle",
            "build.gradle.kts",
            "CMakeLists.txt",
        ]
        .into_iter()
        .any(|marker| path.join(marker).is_file())
}

/// Returns the per-user data root without adding a runtime dependency just to
/// answer one platform path question. The directory is created by
/// `WorkspacePaths::initialize` after discovery has selected it. A missing
/// platform data variable is an actionable launch error; falling back to a
/// shared temporary directory would make cold restart and multi-project shelf
/// identity unstable.
fn application_data_root() -> Result<PathBuf, RuntimeError> {
    #[cfg(target_os = "macos")]
    {
        if let Some(home) = absolute_env_path("HOME") {
            return Ok(home
                .join("Library")
                .join("Application Support")
                .join("Nudox"));
        }
    }
    #[cfg(target_os = "windows")]
    {
        if let Some(app_data) = absolute_env_path("APPDATA") {
            return Ok(app_data.join("Nudox"));
        }
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        if let Some(data_home) = absolute_env_path("XDG_DATA_HOME") {
            return Ok(data_home.join("nudox"));
        }
        if let Some(home) = absolute_env_path("HOME") {
            return Ok(home.join(".local").join("share").join("nudox"));
        }
    }
    Err(RuntimeError::InvalidPath(
        "cannot determine a per-user application data directory; set HOME, APPDATA, or XDG_DATA_HOME to an absolute path",
    ))
}

/// Reads a platform data variable only when it names an absolute, non-empty
/// directory. Relative values would put durable shelf state under whichever
/// directory launched the app, which is not a stable per-user location.
fn absolute_env_path(name: &str) -> Option<PathBuf> {
    let path = PathBuf::from(std::env::var_os(name)?);
    (!path.as_os_str().is_empty() && path.is_absolute()).then_some(path)
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use super::{ambient_paths, looks_like_project};
    use std::fs;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn only_admits_direct_project_markers() {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock after epoch")
            .as_nanos();
        let root = std::env::temp_dir().join(format!("nudox-paths-{nonce}"));
        fs::create_dir_all(root.join("nested")).expect("temporary project root");
        assert!(!looks_like_project(&root));
        fs::write(root.join("nested").join("Cargo.toml"), b"[package]").expect("nested marker");
        assert!(!looks_like_project(&root));
        fs::write(root.join("package.json"), b"{}").expect("project marker");
        assert!(looks_like_project(&root));
        fs::remove_dir_all(root).expect("temporary project cleanup");
    }

    #[test]
    fn ambient_launch_paths_are_stable_across_cold_restart() {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock after epoch")
            .as_nanos();
        let root = std::env::temp_dir().join(format!("nudox-cold-restart-{nonce}"));
        let first = ambient_paths(&root).expect("first ambient launch");
        first.initialize().expect("initialize first launch");
        let second = ambient_paths(&root).expect("cold restart launch");
        second.initialize().expect("initialize restart");
        assert_eq!(first.project(), second.project());
        assert_eq!(first.data(), second.data());
        assert_eq!(first.endpoint(), second.endpoint());
        fs::remove_dir_all(root).expect("temporary restart cleanup");
    }
}
