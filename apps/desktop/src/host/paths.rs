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
    AUTHORITY_SECRET_ENV, DATA_ENV, ENDPOINT_ENV, PROJECT_ENV, RuntimeError, WorkspacePaths,
};
use std::ffi::OsString;
use std::fs;
use std::io;
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

/// Initializes paths for the durable empty launch session used before the
/// first folder is chosen. This takes its root directly so cold-restart tests
/// can exercise the production layout without changing process environment.
pub(crate) fn ambient_paths(application_root: &Path) -> Result<WorkspacePaths, RuntimeError> {
    ensure_private_application_root(application_root).map_err(RuntimeError::Io)?;
    let data = application_root.join("workspace");
    let starter = application_root.join("starter");
    backend_platform::durable::ensure_private_directory(&starter).map_err(RuntimeError::Io)?;
    WorkspacePaths::discover(Some(starter), Some(data), None)
}

/// Creates the application-owned root without changing any existing parent.
///
/// OS application-data parents are commonly readable by other users (for
/// example, `~/Library/Application Support` under umask 022). The first
/// application directory beneath that location must therefore be created as
/// an owner-only child. If the platform data hierarchy itself is missing, each
/// newly created level becomes private before it is used as the next parent.
/// Existing application roots are admitted only when already private.
///
/// Every existing path component is inspected with `symlink_metadata`; a link
/// or non-directory is rejected rather than followed or repaired.
fn ensure_private_application_root(path: &Path) -> io::Result<()> {
    if !path.is_absolute() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "application data root must be absolute",
        ));
    }

    let mut existing = PathBuf::new();
    let mut first_missing_parent = None;
    let mut missing = Vec::<OsString>::new();
    for component in path.components() {
        match component {
            std::path::Component::Prefix(prefix) => existing.push(prefix.as_os_str()),
            std::path::Component::RootDir => existing.push(component.as_os_str()),
            std::path::Component::CurDir => continue,
            std::path::Component::ParentDir => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "application data root cannot contain parent traversal",
                ));
            }
            std::path::Component::Normal(name) => {
                if first_missing_parent.is_some() {
                    missing.push(name.to_os_string());
                    continue;
                }
                existing.push(name);
                match fs::symlink_metadata(&existing) {
                    Ok(metadata) if metadata.file_type().is_symlink() => {
                        return Err(io::Error::new(
                            io::ErrorKind::InvalidInput,
                            "application data path contains a symbolic link",
                        ));
                    }
                    Ok(metadata) if !metadata.is_dir() => {
                        return Err(io::Error::new(
                            io::ErrorKind::NotADirectory,
                            "application data path component is not a directory",
                        ));
                    }
                    Ok(_) => {}
                    Err(error) if error.kind() == io::ErrorKind::NotFound => {
                        existing.pop();
                        first_missing_parent = Some(existing.clone());
                        missing.push(name.to_os_string());
                    }
                    Err(error) => return Err(error),
                }
            }
        }
    }

    if missing.is_empty() {
        return backend_platform::durable::ensure_private_child_directory(path);
    }

    // The path walk above has found a continuous missing suffix. Anchor its
    // first creation under the existing OS-owned parent; later levels are
    // nested under directories this loop just created privately.
    let mut parent = first_missing_parent.ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "application data root has no existing parent",
        )
    })?;
    for (index, name) in missing.into_iter().enumerate() {
        let child = parent.join(name);
        if index == 0 {
            backend_platform::durable::ensure_private_child_directory(&child)?;
        } else {
            backend_platform::durable::ensure_private_directory(&child)?;
        }
        parent = child;
    }
    debug_assert_eq!(parent, path);
    Ok(())
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
/// answer one platform path question. The application root is admitted by
/// `ambient_paths` and the workspace directory is created by
/// `WorkspacePaths::initialize`. A missing platform data variable is an
/// actionable launch error; falling back to a shared temporary directory
/// would make cold restart and multi-project shelf identity unstable.
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
    use super::{
        AUTHORITY_SECRET_ENV, DATA_ENV, ENDPOINT_ENV, PROJECT_ENV, ambient_paths,
        ensure_private_application_root, looks_like_project,
    };
    use std::fs;
    use std::path::PathBuf;
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
        let fixture = std::env::temp_dir().join(format!("nudox-cold-restart-{nonce}"));
        crate::host::private_dir(&fixture).expect("a private fixture parent");
        let fixture = fixture.canonicalize().expect("canonical fixture parent");
        let root = fixture.join("Nudox");
        let first = ambient_paths(&root).expect("first ambient launch");
        first.initialize().expect("initialize first launch");
        let second = ambient_paths(&root).expect("cold restart launch");
        second.initialize().expect("initialize restart");
        assert_eq!(first.project(), second.project());
        assert_eq!(first.data(), second.data());
        assert_eq!(first.endpoint(), second.endpoint());
        fs::remove_dir_all(fixture).expect("temporary restart cleanup");
    }

    #[cfg(unix)]
    #[test]
    fn existing_public_app_root_is_refused_without_chmod_or_symlink_following() {
        use std::os::unix::fs::{PermissionsExt as _, symlink};
        use std::time::{SystemTime, UNIX_EPOCH};

        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock after epoch")
            .as_nanos();
        let fixture = std::env::temp_dir().join(format!("nudox-app-root-admission-{nonce}"));
        crate::host::private_dir(&fixture).expect("private test fixture");
        let fixture = fixture.canonicalize().expect("canonical test fixture");

        let public = fixture.join("public-app-root");
        fs::create_dir(&public).expect("public preexisting app root");
        fs::set_permissions(&public, fs::Permissions::from_mode(0o755))
            .expect("make existing app root intentionally nonprivate");
        assert!(ensure_private_application_root(&public).is_err());
        assert_eq!(
            fs::metadata(&public)
                .expect("existing app root remains in place")
                .permissions()
                .mode()
                & 0o777,
            0o755,
            "startup must not repair an existing public directory by chmod",
        );

        let private_target = fixture.join("private-target");
        fs::create_dir(&private_target).expect("private symlink target");
        fs::set_permissions(&private_target, fs::Permissions::from_mode(0o700))
            .expect("private target permissions");
        let link = fixture.join("linked-app-root");
        symlink(&private_target, &link).expect("create app-root symlink fixture");
        assert!(ensure_private_application_root(&link).is_err());

        fs::remove_dir_all(fixture).expect("remove admission fixture");
    }

    #[cfg(unix)]
    #[test]
    fn missing_platform_data_hierarchy_is_created_private_level_by_level() {
        use std::os::unix::fs::PermissionsExt as _;
        use std::time::{SystemTime, UNIX_EPOCH};

        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock after epoch")
            .as_nanos();
        let fixture = std::env::temp_dir().join(format!("nudox-new-data-tree-{nonce}"));
        crate::host::private_dir(&fixture).expect("private test fixture");
        let fixture = fixture.canonicalize().expect("canonical test fixture");
        let home = fixture.join("home");
        fs::create_dir(&home).expect("existing user home");
        fs::set_permissions(&home, fs::Permissions::from_mode(0o755)).expect("home permissions");
        let app_root = home.join(".local").join("share").join("nudox");

        ensure_private_application_root(&app_root).expect("create absent app-data hierarchy");

        for directory in [
            home.join(".local"),
            home.join(".local").join("share"),
            app_root.clone(),
        ] {
            assert_eq!(
                fs::metadata(directory)
                    .expect("new private data directory")
                    .permissions()
                    .mode()
                    & 0o777,
                0o700,
            );
        }
        assert_eq!(
            fs::metadata(&home)
                .expect("preexisting user home")
                .permissions()
                .mode()
                & 0o777,
            0o755,
            "the helper leaves a preexisting readable parent untouched",
        );

        fs::remove_dir_all(fixture).expect("remove data tree fixture");
    }

    #[cfg(unix)]
    #[test]
    fn finder_umasks_create_private_state_and_cold_restart_keeps_settings() {
        use std::os::unix::fs::PermissionsExt as _;
        use std::process::Command;
        use std::time::{SystemTime, UNIX_EPOCH};

        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock after epoch")
            .as_nanos();
        let fixture = std::env::temp_dir().join(format!("nudox-finder-launch-{nonce}"));
        crate::host::private_dir(&fixture).expect("private test fixture");
        let fixture = fixture.canonicalize().expect("canonical fixture root");
        let home = fixture.join("home");
        fs::create_dir_all(&home).expect("fixture home");
        #[cfg(target_os = "macos")]
        let ordinary_parent = home.join("Library").join("Application Support");
        #[cfg(all(unix, not(target_os = "macos")))]
        let ordinary_parent = home.join(".local").join("share");
        fs::create_dir_all(&ordinary_parent).expect("ordinary OS application-data parent");
        fs::set_permissions(&ordinary_parent, fs::Permissions::from_mode(0o755))
            .expect("leave existing application-data parent unchanged and public-readable");
        let launch_directory = fixture.join("launch-cwd");
        fs::create_dir(&launch_directory).expect("empty launch working directory");
        let current_exe = std::env::current_exe().expect("test executable");
        let child_root = fixture.join("Nudox-launch-child-root");
        crate::host::private_dir(&child_root).expect("private child-test marker root");

        let run = |umask: &str, operation: &str| {
            let output = Command::new("/bin/sh")
                .arg("-c")
                .arg("umask \"$1\"; shift; exec \"$@\"")
                .arg("finder-umask-child")
                .arg(umask)
                .arg(&current_exe)
                .arg("--exact")
                .arg("host::paths::tests::finder_launch_child_entry")
                .arg("--nocapture")
                .current_dir(&launch_directory)
                .env("HOME", &home)
                .env("NUDOX_NATIVE_LAUNCH_TEST_ROOT", &child_root)
                .env("NUDOX_NATIVE_LAUNCH_TEST_OPERATION", operation)
                .env_remove(PROJECT_ENV)
                .env_remove(DATA_ENV)
                .env_remove(ENDPOINT_ENV)
                .env_remove(AUTHORITY_SECRET_ENV)
                .env_remove("XDG_DATA_HOME")
                .env_remove("XDG_STATE_HOME")
                .output()
                .expect("launch child with an isolated umask");
            assert!(
                output.status.success(),
                "native discovery child failed under umask {umask} ({operation}):\n{}\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr),
            );
        };

        // First launch creates the production app-data tree with Finder's
        // ordinary 022 mask. Both restart processes use the same HOME and
        // empty current directory, with no project/state override variables.
        run("022", "save");
        run("077", "read");
        run("022", "read");

        fs::remove_dir_all(fixture).expect("remove isolated child-launch fixture");
    }

    /// Runs inside the subprocess above. An unset root means the normal unit
    /// test pass, where this helper intentionally does nothing.
    #[cfg(unix)]
    #[test]
    fn finder_launch_child_entry() {
        use std::os::unix::fs::PermissionsExt as _;

        let Some(test_root) = std::env::var_os("NUDOX_NATIVE_LAUNCH_TEST_ROOT") else {
            return;
        };
        let test_root = PathBuf::from(test_root);
        let app_root = super::application_data_root().expect("OS application-data root");
        let paths = super::discover().expect("normal ambient GUI discovery");
        assert_eq!(paths.project(), app_root.join("starter"));
        assert_eq!(paths.data(), app_root.join("workspace"));
        assert_eq!(
            paths.endpoint(),
            backend_runtime::derive_endpoint(paths.data()).as_path(),
            "cold launch must reconnect to the same derived owner endpoint",
        );
        let starter = app_root.join("starter");
        for directory in [app_root.as_path(), starter.as_path(), paths.data()] {
            assert_eq!(
                fs::metadata(directory)
                    .expect("application state directory")
                    .permissions()
                    .mode()
                    & 0o777,
                0o700,
                "new application state directories are private under either umask",
            );
        }
        assert_eq!(
            fs::metadata(paths.authority_secret())
                .expect("owner credential")
                .permissions()
                .mode()
                & 0o777,
            0o600,
            "the authority credential is private under either umask",
        );

        let store = crate::model::PersistentState::at(paths.data().join("desktop-state.json"));
        match std::env::var("NUDOX_NATIVE_LAUNCH_TEST_OPERATION").as_deref() {
            Ok("save") => {
                let mut state = crate::model::PersistedDesktopState::default();
                state.shelf_open = false;
                state.settings_page = Some("appearance".to_owned());
                store.save(&state).expect("save cold-restart settings");
                fs::write(
                    test_root.join("expected-project"),
                    paths.project().as_os_str().as_encoded_bytes(),
                )
                .expect("record path identity for second process");
            }
            Ok("read") => {
                assert_eq!(
                    fs::read(test_root.join("expected-project")).expect("first launch identity"),
                    paths.project().as_os_str().as_encoded_bytes(),
                    "restarted native discovery must select the same starter project",
                );
                let state = store.load_recovering().expect("restore cold settings");
                assert!(!state.state.shelf_open);
                assert_eq!(state.state.settings_page.as_deref(), Some("appearance"));
            }
            operation => panic!("unexpected child operation: {operation:?}"),
        }
    }
}
