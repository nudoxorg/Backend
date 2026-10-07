//! Frozen compiler launch inputs for the desktop's embedded owner.
//!
//! Desktop and CLI/MCP use the engine's shared installed-tool policy, including
//! Rust pair/cache selection and canonical TypeScript SDK/Node discovery. Only
//! selected typed paths enter the closed owner snapshot. Explicit values retain
//! their normal typed failures; an incoming closed snapshot is never expanded.

use backend_local_service::{
    ClosedLocalHostEnvironmentSnapshot, LocalCompilerHost, LocalHostDiscovery,
    LocalHostEnvironment, LocalHostVariable,
};
use std::collections::BTreeMap;
use std::ffi::OsString;
use std::io;
use std::path::PathBuf;
#[cfg(test)]
use std::path::Path;
use std::sync::{PoisonError, RwLock};

// Installed Rust pair discovery is shared with CLI/MCP host capture.
use backend_local_service::{
    InstalledRustInputs, InstalledRustToolchain as Rust, InstalledToolPlace as Place,
};

#[cfg(test)]
fn executable(tool: &str) -> String { format!("{tool}{}", std::env::consts::EXE_SUFFIX) }
fn home_variable(value: &dyn Fn(&str) -> Option<OsString>) -> Option<OsString> {
    value("HOME").or_else(|| if cfg!(windows) { value("USERPROFILE") } else { None })
}

fn find_rust(variable: &dyn Fn(&str) -> Option<OsString>, system: &[(PathBuf, Place)]) -> Rust {
    backend_local_service::find_installed_rust(&InstalledRustInputs {
        configured_rustc: variable("NUDOX_RUSTC"),
        home: home_variable(variable),
        cargo_home: variable("CARGO_HOME"),
        search_path: variable("PATH"),
    }, system)
}

/// What this launch found, once the owner was started.
static REPORT: RwLock<Option<Rust>> = RwLock::new(None);

/// The Rust this launch found for the owner, once it started.
pub(crate) fn report() -> Option<Rust> {
    REPORT.read().unwrap_or_else(PoisonError::into_inner).clone()
}

/// The paths to supply, given the process's variables.
#[cfg(test)]
pub(crate) fn supplied(
    variable: &dyn Fn(&str) -> Option<OsString>,
) -> Vec<(LocalHostVariable, PathBuf)> {
    supplied_among(variable, &[])
}

/// [`supplied`], with a person's Rust looked for in `system` too when the
/// process named none ([`find_rust`]).
#[cfg(test)]
pub(crate) fn supplied_among(
    variable: &dyn Fn(&str) -> Option<OsString>,
    system: &[(PathBuf, Place)],
) -> Vec<(LocalHostVariable, PathBuf)> {
    let set = |name: &str| variable(name).is_some_and(|value| !value.is_empty());
    let path = |name: &str| {
        variable(name)
            .filter(|value| !value.is_empty())
            .map(PathBuf::from)
    };
    let home = || home_variable(&|name: &str| path(name).map(PathBuf::into_os_string)).map(PathBuf::from);
    let file =
        |candidate: PathBuf| (candidate.is_absolute() && candidate.is_file()).then_some(candidate);
    let directory =
        |candidate: PathBuf| (candidate.is_absolute() && candidate.is_dir()).then_some(candidate);
    let mut supplied = Vec::new();

    let rust = if system.is_empty() && !set("NUDOX_RUSTC") {
        None
    } else {
        match find_rust(variable, system) {
            Rust::Found { rustc, place, .. } => Some((rustc, place)),
            Rust::Missing { .. } => None,
        }
    };
    if let Some((rustc, place)) = rust {
        if place != Place::Named {
            supplied.push((LocalHostVariable::NudoxRustc, rustc.clone()));
        }
        if !set("NUDOX_CARGO")
            && let Some(cargo) = file(Path::new(&rustc).with_file_name(executable("cargo")))
        {
            supplied.push((LocalHostVariable::NudoxCargo, cargo));
        }
        if !set("NUDOX_CARGO_HOME")
            && let Some(cargo_home) = path("CARGO_HOME")
                .or_else(|| home().map(|home| home.join(".cargo")))
                .and_then(directory)
        {
            supplied.push((LocalHostVariable::NudoxCargoHome, cargo_home));
        }
    }
    if set("NUDOX_GO") && !set("NUDOX_GO_ROOT") {
        // Go's own rule: `GOMODCACHE`, else the first `GOPATH` entry's `pkg/mod`.
        let cache = path("GOMODCACHE")
            .or_else(|| {
                path("GOPATH")
                    .and_then(|gopath| std::env::split_paths(&gopath).next())
                    .map(|first| first.join("pkg/mod"))
            })
            .or_else(|| home().map(|home| home.join("go/pkg/mod")));
        if let Some(cache) = cache.and_then(directory) {
            supplied.push((LocalHostVariable::NudoxGoRoot, cache));
        }
    }
    supplied
}

/// Derivation does not create directories. Explicit paths are kept verbatim so
/// the owner, including its normal invalid-path refusal, remains the authority.
struct CompilerSelection {
    paths: Vec<(LocalHostVariable, PathBuf)>,
    #[cfg(test)]
    inferred_cargo_home: Option<PathBuf>,
}

impl CompilerSelection {
    #[cfg(test)]
    fn derive(variable: &dyn Fn(&str) -> Option<OsString>, system: &[(PathBuf, Place)]) -> Self {
        let mut paths = supplied_among(variable, system);
        let rust = find_rust(variable, system);
        let mut inferred_cargo_home = None;
        if let Rust::Found { rustc, .. } = rust {
            // Cargo's own explicit home is also operator configuration. Never
            // silently replace or create it, even if it is empty or invalid.
            if variable("NUDOX_CARGO_HOME").is_none() {
                let configured = variable("CARGO_HOME");
                let cargo_home = configured.clone().map(PathBuf::from).or_else(|| {
                    home_variable(variable).map(PathBuf::from)
                        .filter(|home| home.is_absolute()).map(|home| home.join(".cargo"))
                });
                if let Some(cargo_home) = cargo_home {
                    paths.retain(|(key, _)| *key != LocalHostVariable::NudoxCargoHome);
                    paths.push((LocalHostVariable::NudoxCargoHome, cargo_home.clone()));
                    let cargo = variable("NUDOX_CARGO").map(PathBuf::from)
                        .unwrap_or_else(|| rustc.with_file_name(executable("cargo")));
                    let selected_rustc = variable("NUDOX_RUSTC").map(PathBuf::from)
                        .unwrap_or(rustc);
                    if configured.is_none() && selected_rustc.is_absolute() && selected_rustc.is_file()
                        && cargo.is_absolute() && cargo.is_file()
                    {
                        inferred_cargo_home = Some(cargo_home);
                    }
                }
            }
        }
        // Supplied and process paths now form one frozen selection. In
        // particular, present empty NUDOX_* values cannot be overwritten by a
        // discovered default and thereby hide a configuration error.
        for (key, path) in closed_paths(variable) {
            paths.retain(|(known, _)| *known != key);
            paths.push((key, path));
        }
        Self { paths, inferred_cargo_home }
    }

    /// Only a default cache below the existing home may be created. Existing
    /// user caches, configured paths, ancestors and permissions stay untouched.
    #[cfg(test)]
    fn realize(&self) -> io::Result<()> {
        let Some(path) = &self.inferred_cargo_home else { return Ok(()); };
        if path.is_dir() { return Ok(()); }
        backend_platform::durable::ensure_private_child_directory(path)
            .map_err(|error| io::Error::new(error.kind(), format!("prepare Cargo home at {}: {error}", path.display())))
    }
}

/// Freeze operator variables once, realize only inferred state, and hand the
/// owner the same typed paths that a later MCP client configuration exports.
pub(crate) fn prepared_by_the_process() -> io::Result<ClosedLocalHostEnvironmentSnapshot> {
    if let Some(snapshot) = incoming_closed_snapshot()? {
        return Ok(snapshot);
    }
    let captured = process_variables();
    let variable = |name: &str| captured.get(name).cloned();
    let system = backend_local_service::installed_rust_system_locations();
    let found = find_rust(&variable, &system);
    *REPORT.write().unwrap_or_else(PoisonError::into_inner) = Some(found);
    let selected = CompilerSelection {
        paths: closed_paths(&variable),
        #[cfg(test)]
        inferred_cargo_home: None,
    };
    let snapshot = installed_snapshot(&captured, &selected)?;
    Ok(snapshot)
}

/// Attaching does not discover tools or create caches. The local registry
/// source may use this client's existing Cargo cache, without claiming that
/// these are the attached owner's compiler or policy settings.
pub(crate) fn captured_by_the_process() -> io::Result<ClosedLocalHostEnvironmentSnapshot> {
    if let Some(snapshot) = incoming_closed_snapshot()? {
        return Ok(snapshot);
    }
    let captured = process_variables();
    closed_snapshot(closed_paths(&|name| captured.get(name).cloned()))
}

fn incoming_closed_snapshot() -> io::Result<Option<ClosedLocalHostEnvironmentSnapshot>> {
    if let Some(encoded) = std::env::var_os(backend_local_service::COMPILER_ENVIRONMENT_ENV) {
        let encoded = encoded.to_str().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "closed compiler environment must be UTF-8",
            )
        })?;
        return ClosedLocalHostEnvironmentSnapshot::parse(encoded)
            .map(Some)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error));
    }
    Ok(None)
}

/// The desktop supplies its Rust pair, then delegates installed authority selection to the
/// same engine policy as CLI/MCP locald composition. This reader is already frozen and never
/// falls back to the live process, including for absent and explicitly empty variables.
struct DesktopLaunchEnvironment<'a> {
    captured: &'a BTreeMap<&'static str, OsString>,
    selected: &'a CompilerSelection,
}

impl LocalHostEnvironment for DesktopLaunchEnvironment<'_> {
    fn value(&self, variable: LocalHostVariable) -> Option<OsString> {
        self.selected
            .paths
            .iter()
            .find(|(key, _)| *key == variable)
            .map(|(_, path)| path.as_os_str().to_owned())
    }

    fn search_path(&self) -> Option<OsString> {
        self.captured.get("PATH").cloned()
    }
    fn go_module_cache(&self) -> Option<OsString> {
        self.captured.get("GOMODCACHE").cloned()
    }
    fn go_path(&self) -> Option<OsString> {
        self.captured.get("GOPATH").cloned()
    }
    fn cargo_home(&self) -> Option<OsString> { self.captured.get("CARGO_HOME").cloned() }
    fn user_profile(&self) -> Option<OsString> { self.captured.get("USERPROFILE").cloned() }
}

fn installed_snapshot(
    captured: &BTreeMap<&'static str, OsString>,
    selected: &CompilerSelection,
) -> io::Result<ClosedLocalHostEnvironmentSnapshot> {
    LocalCompilerHost::new(
        DesktopLaunchEnvironment { captured, selected },
        LocalHostDiscovery::InstalledTools,
    )
    .capture_installed_selection()
    .map(|selection| selection.snapshot().clone())
    .map_err(|error| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("capture installed compiler selection: {error}"),
        )
    })
}

fn process_variables() -> BTreeMap<&'static str, OsString> {
    LocalHostVariable::closed_environment_snapshot_roles()
        .map(LocalHostVariable::environment_name)
        .chain(["USERPROFILE", "PATH", "CARGO_HOME", "GOMODCACHE", "GOPATH"])
        .filter_map(|name| std::env::var_os(name).map(|value| (name, value)))
        .collect()
}

fn closed_paths(variable: &dyn Fn(&str) -> Option<OsString>) -> Vec<(LocalHostVariable, PathBuf)> {
    let mut paths = LocalHostVariable::closed_environment_snapshot_roles()
        .filter_map(|key| variable(key.environment_name()).map(|value| (key, PathBuf::from(value))))
        .collect::<Vec<_>>();
    if variable("NUDOX_CARGO_HOME").is_none()
        && let Some(value) = variable("CARGO_HOME")
    {
        paths.push((LocalHostVariable::NudoxCargoHome, PathBuf::from(value)));
    }
    if variable("HOME").is_none()
        && let Some(home) = home_variable(variable)
    {
        paths.push((LocalHostVariable::Home, PathBuf::from(home)));
    }
    paths
}

fn closed_snapshot(paths: Vec<(LocalHostVariable, PathBuf)>) -> io::Result<ClosedLocalHostEnvironmentSnapshot> {
    ClosedLocalHostEnvironmentSnapshot::from_paths(paths)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error))
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    /// Executes the real desktop environment composition against the standalone owner policy.
    /// The supervisor supplies a private HOME/PATH with an actual installed npm SDK symlink;
    /// it must not set NUDOX_TSC/Node/module-root overrides for the default-environment gate.
    #[test]
    #[ignore = "requires a pinned actual installed Linux/npm or macOS application environment"]
    fn actual_default_launch_matches_standalone_installed_tool_selection() {
        for variable in ["NUDOX_TSC", "NUDOX_TYPESCRIPT_NODE", "NUDOX_TYPESCRIPT_MODULE_ROOT",
            backend_local_service::COMPILER_ENVIRONMENT_ENV] {
            assert!(std::env::var_os(variable).is_none(), "default gate cannot configure {variable}");
        }
        let expected_compiler = std::fs::canonicalize(std::env::var_os("NUDOX_SETUP_EXPECTED_GLOBAL_TSC")
            .expect("pinned global npm tsc entrypoint")).expect("canonical global compiler");
        let expected_node = std::fs::canonicalize(std::env::var_os("NUDOX_SETUP_EXPECTED_NODE")
            .expect("pinned installed Node runtime")).expect("canonical Node runtime");
        let desktop = prepared_by_the_process().expect("actual desktop composition");
        let locald = LocalCompilerHost::new(backend_local_service::ProcessHostEnvironment,
            LocalHostDiscovery::InstalledTools).capture_installed_selection().expect("standalone owner composition");
        assert_eq!(&desktop, locald.snapshot(), "GUI and CLI/MCP select the same exact host paths");
        assert_eq!(desktop.path(LocalHostVariable::NudoxTypeScriptDefaultCompiler), Some(expected_compiler.as_path()));
        assert_eq!(desktop.path(LocalHostVariable::NudoxTypeScriptCompiler), None);
        assert_eq!(desktop.path(LocalHostVariable::NudoxTypeScriptDefaultNode), Some(expected_node.as_path()));
        assert!(desktop.path(LocalHostVariable::NudoxTypeScriptModuleRoot).is_some());
        let restarted = prepared_by_the_process().expect("repeat actual desktop composition");
        assert_eq!(desktop, restarted, "unchanged launch inputs yield the same closed owner paths");
        println!("actual-default-host-snapshot={}", desktop.encode().expect("closed snapshot receipt"));
    }

    #[cfg(unix)]
    #[test]
    fn desktop_and_locald_capture_the_same_global_npm_symlink_and_node() {
        use std::os::unix::{fs::PermissionsExt as _, fs::symlink};
        let machine = machine("typescript-shared");
        let home = machine.root.join("home");
        let module_root = home.join(".local/lib/node_modules");
        let package = module_root.join("typescript");
        let compiler = package.join("bin/tsc");
        std::fs::create_dir_all(compiler.parent().expect("compiler parent")).expect("package");
        std::fs::write(&compiler, "#!/usr/bin/env node\n").expect("compiler");
        std::fs::set_permissions(&compiler, std::fs::Permissions::from_mode(0o700))
            .expect("compiler permissions");
        let bin = home.join(".local/bin");
        std::fs::create_dir_all(&bin).expect("user bin");
        symlink(&compiler, bin.join("tsc")).expect("npm tsc symlink");
        let node = bin.join("node");
        std::fs::write(&node, "#!/bin/sh\nexit 0\n").expect("node");
        std::fs::set_permissions(&node, std::fs::Permissions::from_mode(0o700))
            .expect("node permissions");
        let mut captured = BTreeMap::from([
            ("HOME", home.into_os_string()),
            ("PATH", bin.into_os_string()),
        ]);
        let selected = CompilerSelection { paths: closed_paths(&|name| captured.get(name).cloned()), inferred_cargo_home: None };
        let desktop = installed_snapshot(&captured, &selected).expect("desktop capture");
        let locald = LocalCompilerHost::new(
            DesktopLaunchEnvironment {
                captured: &captured,
                selected: &selected,
            },
            LocalHostDiscovery::InstalledTools,
        )
        .capture_installed_selection()
        .expect("locald capture");
        assert_eq!(&desktop, locald.snapshot());
        assert_eq!(
            desktop.path(LocalHostVariable::NudoxTypeScriptDefaultCompiler),
            Some(compiler.as_path())
        );
        assert_eq!(
            desktop.path(LocalHostVariable::NudoxTypeScriptCompiler),
            None,
            "discovered host fallback cannot override project-local SDK precedence"
        );
        assert_eq!(
            desktop.path(LocalHostVariable::NudoxTypeScriptModuleRoot),
            Some(module_root.as_path())
        );
        assert_eq!(
            desktop.path(LocalHostVariable::NudoxTypeScriptDefaultNode),
            Some(node.as_path())
        );

        // Explicit invalid settings must not be repaired by the working PATH pair.
        for value in [
            OsString::new(),
            OsString::from("relative/tsc"),
            machine.root.join("missing/tsc").into_os_string(),
        ] {
            captured.insert("NUDOX_TSC", value);
            let selected = CompilerSelection::derive(&|name| captured.get(name).cloned(), &[]);
            assert!(installed_snapshot(&captured, &selected).is_err());
        }
        std::fs::remove_dir_all(machine.root).expect("owned fixture cleanup");
    }

    /// A toolchain directory and a home, on disk, under a scratch root.
    struct Machine {
        root: PathBuf,
        rustc: PathBuf,
        cargo: PathBuf,
        cargo_home: PathBuf,
        go_cache: PathBuf,
    }

    fn machine(tag: &str) -> Machine {
        let root = crate::host::scratch_base().join(format!("nx-toolchain-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let bin = root.join("toolchain/bin");
        let cargo_home = root.join("home/.cargo");
        let go_cache = root.join("home/go/pkg/mod");
        for directory in [&bin, &cargo_home, &go_cache] {
            std::fs::create_dir_all(directory).expect("scratch directory");
        }
        for tool in ["rustc", "cargo"] {
            std::fs::write(bin.join(executable(tool)), b"#!/bin/sh\n").expect("tool");
        }
        Machine {
            rustc: bin.join(executable("rustc")),
            cargo: bin.join(executable("cargo")),
            cargo_home,
            go_cache,
            root,
        }
    }

    fn env(pairs: &[(&str, &Path)]) -> impl Fn(&str) -> Option<OsString> + use<> {
        let pairs = pairs
            .iter()
            .map(|(name, value)| ((*name).to_owned(), value.as_os_str().to_owned()))
            .collect::<BTreeMap<_, _>>();
        move |name| pairs.get(name).cloned()
    }

    #[test]
    fn fresh_homebrew_selection_realizes_only_its_inferred_cargo_home() {
        let machine = machine("fresh-homebrew");
        std::fs::remove_dir_all(&machine.cargo_home).expect("fresh Cargo user");
        let home = machine.root.join("home");
        let environment = env(&[("HOME", &home)]);
        let system = [(machine.rustc.parent().expect("bin").to_path_buf(), Place::Homebrew)];
        let selected = CompilerSelection::derive(&environment, &system);
        assert!(selected.paths.contains(&(LocalHostVariable::NudoxCargoHome, machine.cargo_home.clone())));
        assert!(!machine.cargo_home.exists(), "derivation cannot create the cache");
        selected.realize().expect("realize inferred cache");
        assert!(machine.cargo_home.is_dir());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            assert_eq!(std::fs::metadata(&machine.cargo_home).expect("created cache").permissions().mode() & 0o777, 0o700);
            std::fs::set_permissions(&machine.cargo_home, std::fs::Permissions::from_mode(0o755)).expect("existing user cache permissions");
        }
        let marker = machine.cargo_home.join("user-cache");
        std::fs::write(&marker, "retain user cache").expect("existing cache content");
        selected.realize().expect("repeat realization");
        assert_eq!(std::fs::read_to_string(marker).expect("retained marker"), "retain user cache");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            assert_eq!(std::fs::metadata(&machine.cargo_home).expect("existing cache").permissions().mode() & 0o777, 0o755, "existing cache permissions stay unchanged");
        }
        std::fs::remove_dir_all(machine.root).expect("owned fixture cleanup");
    }

    #[test]
    fn explicit_invalid_cache_and_compiler_paths_are_never_realized_or_replaced() {
        let machine = machine("explicit-refusal");
        std::fs::remove_dir_all(&machine.cargo_home).expect("fresh Cargo user");
        let home = machine.root.join("home");
        let system = [(machine.rustc.parent().expect("bin").to_path_buf(), Place::Homebrew)];
        let missing = machine.root.join("configured-but-missing");
        for (name, path) in [("CARGO_HOME", missing.as_path()), ("NUDOX_CARGO_HOME", Path::new("")), ("NUDOX_CARGO_HOME", Path::new("relative/cache"))] {
            let selected = CompilerSelection::derive(&env(&[("HOME", &home), (name, path)]), &system);
            assert!(selected.paths.contains(&(LocalHostVariable::NudoxCargoHome, path.to_path_buf())), "owner sees the exact invalid {name}");
            if !path.is_absolute() {
                assert!(closed_snapshot(selected.paths.clone()).is_err(), "invalid {name} is refused before closed handoff");
            }
            selected.realize().expect("configured paths are not created");
            assert!(!machine.cargo_home.exists());
            assert!(!missing.exists());
        }
        let selected = CompilerSelection::derive(&env(&[("HOME", &home), ("NUDOX_RUSTC", Path::new(""))]), &system);
        assert!(selected.paths.contains(&(LocalHostVariable::NudoxRustc, PathBuf::new())));
        selected.realize().expect("invalid explicit compiler cannot create a fallback cache");
        assert!(!machine.cargo_home.exists());
        std::fs::remove_dir_all(machine.root).expect("owned fixture cleanup");
    }

    #[test]
    fn inferred_cache_file_blocker_is_retained_and_refused() {
        let machine = machine("cache-blocker");
        std::fs::remove_dir_all(&machine.cargo_home).expect("fresh Cargo user");
        std::fs::write(&machine.cargo_home, "owned blocker").expect("cache path is a file");
        let home = machine.root.join("home");
        let system = [(machine.rustc.parent().expect("bin").to_path_buf(), Place::Homebrew)];
        let selected = CompilerSelection::derive(&env(&[("HOME", &home)]), &system);
        let error = selected.realize().expect_err("never replace a file with a directory");
        assert!(error.to_string().contains("prepare Cargo home at"));
        assert_eq!(std::fs::read_to_string(&machine.cargo_home).expect("retained blocker"), "owned blocker");
        std::fs::remove_dir_all(machine.root).expect("owned fixture cleanup");
    }

    #[test]
    fn frozen_selection_keeps_typed_helpers_and_excludes_unrelated_environment() {
        let oracle = std::env::temp_dir().join("app/Contents/Resources/go-oracle");
        let roslyn = std::env::temp_dir().join("app/Contents/Resources/roslyn/helper.dll");
        let secret = Path::new("/never-export-authority-secret");
        let selected = CompilerSelection::derive(&env(&[("NUDOX_GO_ORACLE", oracle.as_path()), ("NUDOX_ROSLYN_HELPER", roslyn.as_path()), ("BACKEND_LOCALD_AUTHORITY_SECRET_FILE", secret)]), &[]);
        assert!(selected.paths.contains(&(LocalHostVariable::NudoxGoOracle, oracle.to_path_buf())));
        assert!(selected.paths.contains(&(LocalHostVariable::NudoxRoslynHelper, roslyn.to_path_buf())));
        assert!(!selected.paths.iter().any(|(_, path)| path == secret));
        let closed = closed_snapshot(selected.paths).expect("closed helper selection");
        assert_eq!(closed.path(LocalHostVariable::Home), None);
        assert_eq!(closed.path(LocalHostVariable::NudoxPython), None);
        assert!(!LocalHostVariable::closed_environment_snapshot_roles()
            .any(|key| key == LocalHostVariable::NudoxDataRoot));
    }

    #[test]
    fn the_lone_rustc_the_development_shell_exports_is_completed_with_its_cargo_and_cargo_home() {
        let machine = machine("completed");
        let home = machine.root.join("home");
        let supplied = supplied(&env(&[("NUDOX_RUSTC", &machine.rustc), ("HOME", &home)]));
        assert_eq!(
            supplied,
            vec![
                (LocalHostVariable::NudoxCargo, machine.cargo.clone()),
                (
                    LocalHostVariable::NudoxCargoHome,
                    machine.cargo_home.clone()
                ),
            ],
            "the cargo beside the rustc, and cargo's home under HOME"
        );
    }

    #[test]
    fn what_the_process_set_itself_is_never_replaced() {
        let machine = machine("explicit");
        let home = machine.root.join("home");
        let elsewhere = machine.root.join("elsewhere");
        let supplied = supplied(&env(&[
            ("NUDOX_RUSTC", &machine.rustc),
            ("NUDOX_CARGO", &elsewhere),
            ("NUDOX_CARGO_HOME", &elsewhere),
            ("HOME", &home),
        ]));
        assert!(
            supplied.is_empty(),
            "explicit variables win, even ones the owner will refuse: {supplied:?}"
        );
    }

    #[test]
    fn cargo_home_follows_cargo_home_before_the_home_directory() {
        let machine = machine("cargo-home");
        let home = machine.root.join("home");
        let chosen = machine.root.join("chosen");
        std::fs::create_dir_all(&chosen).expect("chosen");
        let supplied = supplied(&env(&[
            ("NUDOX_RUSTC", &machine.rustc),
            ("HOME", &home),
            ("CARGO_HOME", &chosen),
        ]));
        assert!(
            supplied.contains(&(LocalHostVariable::NudoxCargoHome, chosen)),
            "{supplied:?}"
        );
    }

    #[test]
    fn nothing_is_supplied_that_the_owner_would_refuse_to_start_on() {
        let machine = machine("refusable");
        let missing = machine.root.join("missing");
        let relative = Path::new("relative/.cargo");
        // No rustc: no Rust variables at all.
        assert!(supplied(&env(&[("HOME", &machine.root.join("home"))])).is_empty());
        // A rustc with no cargo beside it, a CARGO_HOME that is not a
        // directory or not absolute, and a home with no `.cargo`.
        std::fs::remove_file(&machine.cargo).expect("remove cargo");
        assert!(
            supplied(&env(&[
                ("NUDOX_RUSTC", &machine.rustc),
                ("HOME", &missing),
                ("CARGO_HOME", relative)
            ]))
            .is_empty()
        );
        assert!(
            supplied(&env(&[
                ("NUDOX_RUSTC", &machine.rustc),
                ("HOME", &missing),
                ("CARGO_HOME", &machine.rustc)
            ]))
            .is_empty()
        );
    }

    /// A directory holding `tools`, each an empty executable file.
    fn bin(dir: &Path, tools: &[&str]) -> PathBuf {
        std::fs::create_dir_all(dir).expect("bin");
        for tool in tools {
            std::fs::write(dir.join(executable(tool)), b"#!/bin/sh\n").expect("tool");
        }
        dir.to_path_buf()
    }

    #[test]
    fn a_finder_launch_finds_the_rust_a_person_installed_and_supplies_it_whole() {
        let root = crate::host::scratch_base().join(format!("nx-toolchain-finder-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let home = root.join("home");
        let rustup = bin(&home.join(".cargo/bin"), &["rustc", "cargo"]);
        let homebrew = bin(&root.join("homebrew/bin"), &["rustc", "cargo"]);
        let system = [(homebrew.clone(), Place::Homebrew)];
        // The Finder's PATH holds no Rust: rustup's proxies are found, and
        // everything the owner needs comes from that one install.
        let finder_vars = [("HOME", home.as_path()), ("PATH", Path::new("/usr/bin:/bin:/usr/sbin:/sbin"))];
        let finder = env(&finder_vars);
        assert_eq!(
            supplied_among(&finder, &system),
            vec![
                (LocalHostVariable::NudoxRustc, rustup.join(executable("rustc"))),
                (LocalHostVariable::NudoxCargo, rustup.join(executable("cargo"))),
                (LocalHostVariable::NudoxCargoHome, home.join(".cargo")),
            ],
            "rustc, the cargo beside it, and cargo's home"
        );
        assert_eq!(find_rust(&finder, &system), Rust::Found { rustc: rustup.join(executable("rustc")), place: Place::Rustup, version: None });
        // Without rustup: Homebrew's.
        std::fs::remove_dir_all(&rustup).expect("uninstall rustup");
        assert_eq!(find_rust(&finder, &system), Rust::Found { rustc: homebrew.join(executable("rustc")), place: Place::Homebrew, version: None });
        // A terminal launch's PATH comes first; a directory with a rustc and
        // no cargo beside it is not an install.
        let half = bin(&root.join("half/bin"), &["rustc"]);
        let whole = bin(&root.join("whole/bin"), &["rustc", "cargo"]);
        let path = std::env::join_paths([&half, &whole]).expect("PATH");
        let terminal_vars = [("HOME", home.as_path()), ("PATH", Path::new(&path))];
        let terminal = env(&terminal_vars);
        assert_eq!(find_rust(&terminal, &system), Rust::Found { rustc: whole.join(executable("rustc")), place: Place::Path, version: None });
        // An explicit NUDOX_RUSTC wins over anything found.
        let named_rustc = homebrew.join(executable("rustc"));
        let named_vars = [("HOME", home.as_path()), ("PATH", Path::new(&path)), ("NUDOX_RUSTC", named_rustc.as_path())];
        let named = env(&named_vars);
        assert_eq!(find_rust(&named, &system), Rust::Found { rustc: homebrew.join(executable("rustc")), place: Place::Named, version: None });
        assert!(
            !supplied_among(&named, &system).iter().any(|(variable, _)| *variable == LocalHostVariable::NudoxRustc),
            "a named rustc is the process's own variable, never supplied over it"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_machine_with_no_rust_says_so_in_words_and_supplies_nothing() {
        let root = crate::host::scratch_base().join(format!("nx-toolchain-none-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let home = root.join("home");
        std::fs::create_dir_all(&home).expect("home");
        let empty = bin(&root.join("system/bin"), &[]);
        // Two PATH directories that hold no Rust: the Finder's on Unix; on
        // Windows, which has neither, two empty absolute ones.
        #[cfg(unix)]
        let path_dirs = [PathBuf::from("/usr/bin"), PathBuf::from("/bin")];
        #[cfg(not(unix))]
        let path_dirs = [bin(&root.join("usr/bin"), &[]), bin(&root.join("bin"), &[])];
        let path = std::env::join_paths(&path_dirs).expect("PATH");
        let finder_vars = [("HOME", home.as_path()), ("PATH", Path::new(&path))];
        let finder = env(&finder_vars);
        let system = [(empty.clone(), Place::Homebrew)];
        let found = find_rust(&finder, &system);
        let Rust::Missing { looked } = &found else { panic!("no rustc anywhere: {found:?}") };
        let [first, second] = path_dirs;
        assert_eq!(
            looked,
            &[first, second, home.join(".cargo/bin"), empty, home.join(".nix-profile/bin")],
            "every place it looked, in order"
        );
        assert_eq!(
            found.words(),
            "No Rust toolchain was found. Install one with rustup (rustup.rs) or Homebrew (brew install rust), then quit and reopen Nudox."
        );
        assert!(supplied_among(&finder, &system).is_empty());
        let _ = std::fs::remove_dir_all(&root);
    }

    /// Windows sets no `HOME`; rustup installs under `%USERPROFILE%.cargo`, and
    /// its proxies are `.exe` files.
    #[cfg(windows)]
    #[test]
    fn a_windows_launch_finds_rustup_under_the_user_profile() {
        let root = crate::host::scratch_base().join(format!("nx-toolchain-profile-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let profile = root.join("profile");
        let rustup = bin(&profile.join(".cargo/bin"), &["rustc", "cargo"]);
        assert!(rustup.join("rustc.exe").is_file(), "the proxies carry the platform suffix");
        let launch_vars = [("USERPROFILE", profile.as_path())];
        let launch = env(&launch_vars);
        assert_eq!(find_rust(&launch, &[]), Rust::Found { rustc: rustup.join("rustc.exe"), place: Place::Rustup, version: None });
        assert_eq!(
            supplied_among(&launch, &[(root.join("absent"), Place::Homebrew)]),
            vec![
                (LocalHostVariable::NudoxRustc, rustup.join("rustc.exe")),
                (LocalHostVariable::NudoxCargo, rustup.join("cargo.exe")),
                (LocalHostVariable::NudoxCargoHome, profile.join(".cargo")),
            ],
            "rustc, the cargo.exe beside it, and cargo's home under the profile"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_found_rust_is_named_with_its_version_and_where_it_came_from() {
        let found = Rust::Found {
            rustc: PathBuf::from("/opt/homebrew/bin/rustc"),
            place: Place::Homebrew,
            version: Some("rustc 1.98.1 (48a229cea 2026-09-01) (Homebrew)".to_owned()),
        };
        assert_eq!(found.words(), "Rust 1.98.1 at /opt/homebrew/bin/rustc (from Homebrew)");
    }

    #[test]
    fn go_gets_its_module_cache_by_gos_own_rule() {
        let machine = machine("go");
        let home = machine.root.join("home");
        let go = machine.root.join("toolchain/bin/go");
        let by_home = supplied(&env(&[("NUDOX_GO", &go), ("HOME", &home)]));
        assert_eq!(
            by_home,
            vec![(LocalHostVariable::NudoxGoRoot, machine.go_cache.clone())]
        );

        let modcache = machine.root.join("modcache");
        std::fs::create_dir_all(&modcache).expect("modcache");
        let by_modcache = supplied(&env(&[
            ("NUDOX_GO", &go),
            ("HOME", &home),
            ("GOMODCACHE", &modcache),
        ]));
        assert_eq!(
            by_modcache,
            vec![(LocalHostVariable::NudoxGoRoot, modcache)],
            "GOMODCACHE first"
        );

        assert!(
            supplied(&env(&[("HOME", &home)])).is_empty(),
            "no Go selected: no Go root"
        );
    }
}
