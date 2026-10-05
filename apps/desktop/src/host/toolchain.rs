//! The compiler paths the desktop supplies to the owner it embeds.
//!
//! The owner finds its compilers only through explicit absolute paths
//! (`NUDOX_*`, never `PATH`): "an operator can opt into a toolchain by
//! supplying its absolute typed path". The desktop that embeds an owner is that
//! operator. Since the index merge the owner needs, for Rust, a Cargo and a
//! Cargo home as well as `NUDOX_RUSTC` (a lone `NUDOX_RUSTC` used to be enough
//! and the development shell still exports only that), and for Go a module
//! cache. Without them the owner's Rust and Go adapters are absent and every
//! root of those languages is refused as `Unavailable { language, stage:
//! LowerIr }`.
//!
//! What is supplied is derived from what the process was given, never guessed:
//! Cargo is the one beside the selected `rustc`, Cargo's home is where Cargo
//! keeps it (`CARGO_HOME`, else `~/.cargo`), Go's module cache where Go keeps
//! it (`GOMODCACHE`, else `GOPATH/pkg/mod`, else `~/go/pkg/mod`). A path is
//! supplied only when the process has not set that variable itself. Derived
//! tools and caches must exist as the owner requires, except that the default
//! Cargo home can be safely created under the existing home. Configured paths
//! stay exact, including invalid ones the owner must honestly refuse.
//!
//! A person who opens the app from the Finder gives it no `NUDOX_*` variable
//! and a `PATH` of `/usr/bin:/bin:/usr/sbin:/sbin`. For them the desktop looks
//! for the Rust they installed ([`find_rust`]): the process's `PATH`, then
//! rustup's `~/.cargo/bin`, Homebrew, and Nix profiles, and takes the first
//! directory that holds both `rustc` and `cargo` (a pair from one install,
//! never a `rustc` from one and a `cargo` from another). An explicit
//! `NUDOX_RUSTC` still wins. What it found, or where it looked in vain, is
//! [`report`]ed, so the window can say it in words instead of every Rust
//! package being refused as unavailable.

use backend_local_service::{ClosedLocalHostEnvironmentSnapshot, LocalHostVariable};
use std::collections::BTreeMap;
use std::ffi::OsString;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{PoisonError, RwLock};

/// Where a person's Rust was found.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Place {
    /// The process named it (`NUDOX_RUSTC`: the development shell, an operator).
    Named,
    /// A directory on the process's `PATH`.
    Path,
    /// rustup's proxies (`$CARGO_HOME/bin`, else `~/.cargo/bin`).
    Rustup,
    /// Homebrew (`/opt/homebrew/bin`, `/usr/local/bin`).
    Homebrew,
    /// A Nix profile.
    Nix,
}

impl Place {
    /// How a person names it.
    pub(crate) const fn words(self) -> &'static str {
        match self {
            Self::Named => "as configured",
            Self::Path => "on your PATH",
            Self::Rustup => "from rustup",
            Self::Homebrew => "from Homebrew",
            Self::Nix => "from Nix",
        }
    }
}

/// The Rust the owner compiles with, as this launch found it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum Rust {
    /// A `rustc` with its `cargo` beside it.
    Found {
        rustc: PathBuf,
        place: Place,
        /// `rustc --version`'s first line, when it answered.
        version: Option<String>,
    },
    /// No directory it looked in held both; each looked-in directory, in order.
    Missing { looked: Vec<PathBuf> },
}

impl Rust {
    /// What the window says about it.
    pub(crate) fn words(&self) -> String {
        match self {
            Self::Found { rustc, place, version } => {
                let name = version.as_deref().and_then(|version| version.split_whitespace().nth(1)).map_or_else(|| "Rust".to_owned(), |number| format!("Rust {number}"));
                format!("{name} at {} ({})", rustc.display(), place.words())
            }
            Self::Missing { .. } => {
                "No Rust toolchain was found. Install one with rustup (rustup.rs) or Homebrew (brew install rust), then quit and reopen Nudox.".to_owned()
            }
        }
    }
}

/// Directories people install Rust into outside their home, in the order they
/// are tried after the process's `PATH` and rustup's.
const SYSTEM_BINS: [(&str, Place); 5] = [
    ("/opt/homebrew/bin", Place::Homebrew),
    ("/usr/local/bin", Place::Homebrew),
    ("/run/current-system/sw/bin", Place::Nix),
    ("/nix/var/nix/profiles/default/bin", Place::Nix),
    ("/etc/profiles/per-user", Place::Nix),
];

/// The file name a `tool` executable has on this platform (`rustc.exe` on
/// Windows, `rustc` elsewhere).
fn executable(tool: &str) -> String {
    format!("{tool}{}", std::env::consts::EXE_SUFFIX)
}

/// The person's home directory variable: `HOME`, and on Windows, which does
/// not set it, `USERPROFILE` (where rustup puts `.cargo`).
fn home_variable(value: &dyn Fn(&str) -> Option<OsString>) -> Option<OsString> {
    value("HOME").or_else(|| if cfg!(windows) { value("USERPROFILE") } else { None })
}

/// The first directory that holds both `rustc` and `cargo`: an explicit
/// `NUDOX_RUSTC`, else the process's `PATH`, rustup's proxies, Homebrew, Nix.
/// `system` is [`SYSTEM_BINS`] in production (a test gives its own).
pub(crate) fn find_rust(variable: &dyn Fn(&str) -> Option<OsString>, system: &[(PathBuf, Place)]) -> Rust {
    let value = |name: &str| variable(name).filter(|value| !value.is_empty());
    if let Some(rustc) = value("NUDOX_RUSTC") {
        return Rust::Found { rustc: PathBuf::from(rustc), place: Place::Named, version: None };
    }
    let home = home_variable(&value).map(PathBuf::from).filter(|home| home.is_absolute());
    let mut places = Vec::new();
    if let Some(path) = value("PATH") {
        places.extend(std::env::split_paths(&path).filter(|dir| dir.is_absolute()).map(|dir| (dir, Place::Path)));
    }
    let cargo_home = value("CARGO_HOME").map(PathBuf::from).filter(|home| home.is_absolute()).or_else(|| home.as_ref().map(|home| home.join(".cargo")));
    if let Some(cargo_home) = cargo_home {
        places.push((cargo_home.join("bin"), Place::Rustup));
    }
    for (dir, place) in system {
        // `/etc/profiles/per-user` is per user: NixOS and nix-darwin's.
        let dir = if dir.ends_with("per-user") {
            match home.as_ref().and_then(|home| home.file_name()) {
                Some(user) => dir.join(user).join("bin"),
                None => continue,
            }
        } else {
            dir.clone()
        };
        places.push((dir, *place));
    }
    if let Some(home) = &home {
        places.push((home.join(".nix-profile/bin"), Place::Nix));
    }
    let mut looked = Vec::new();
    for (dir, place) in places {
        if looked.contains(&dir) {
            continue;
        }
        let (rustc, cargo) = (dir.join(executable("rustc")), dir.join(executable("cargo")));
        if rustc.is_file() && cargo.is_file() {
            return Rust::Found { rustc, place, version: None };
        }
        looked.push(dir);
    }
    Rust::Missing { looked }
}

/// What this launch found, once the owner was started.
static REPORT: RwLock<Option<Rust>> = RwLock::new(None);

/// The Rust this launch found for the owner, once it started.
pub(crate) fn report() -> Option<Rust> {
    REPORT.read().unwrap_or_else(PoisonError::into_inner).clone()
}

/// `rustc --version`'s first line, within two seconds (a rustup proxy may
/// first resolve its toolchain).
fn version_of(rustc: &Path) -> Option<String> {
    let mut child = std::process::Command::new(rustc)
        .arg("--version")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .ok()?;
    let started = std::time::Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(status)) if status.success() => break,
            Ok(Some(_)) | Err(_) => return None,
            Ok(None) if started.elapsed() > std::time::Duration::from_secs(2) => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
            Ok(None) => std::thread::sleep(std::time::Duration::from_millis(20)),
        }
    }
    let mut out = String::new();
    std::io::Read::read_to_string(&mut child.stdout.take()?, &mut out).ok()?;
    out.lines().next().map(str::trim).filter(|line| !line.is_empty()).map(str::to_owned)
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
    if !system.is_empty()
        && !set("NUDOX_TSC")
        && let Some(typescript) = find_typescript(variable, system)
    {
        supplied.push((LocalHostVariable::NudoxTypeScriptCompiler, typescript.tsc));
        if !set("NUDOX_TYPESCRIPT_NODE") {
            supplied.push((LocalHostVariable::NudoxTypeScriptNode, typescript.node));
        }
        if !set("NUDOX_TYPESCRIPT_MODULE_ROOT") {
            supplied.push((LocalHostVariable::NudoxTypeScriptModuleRoot, typescript.module_root));
        }
    }
    supplied
}

/// A TypeScript checker found as one install: its `tsc`, a `node` to run the
/// checker driver with, and the `node_modules` directory holding `typescript`.
struct TypeScript {
    tsc: PathBuf,
    node: PathBuf,
    module_root: PathBuf,
}

/// The first `tsc` on the process's `PATH`, in `~/.local/bin` (npm's
/// `--prefix ~/.local`), Homebrew, Nix, or `/usr/bin`, with a `node` beside it
/// or in the same places, and the module root its install resolves to. All
/// three are found together or none is supplied, so the owner never sees a
/// half-configured checker.
fn find_typescript(variable: &dyn Fn(&str) -> Option<OsString>, system: &[(PathBuf, Place)]) -> Option<TypeScript> {
    let value = |name: &str| variable(name).filter(|value| !value.is_empty());
    let home = home_variable(&value).map(PathBuf::from).filter(|home| home.is_absolute());
    let mut places: Vec<PathBuf> = Vec::new();
    if let Some(path) = value("PATH") {
        places.extend(std::env::split_paths(&path).filter(|dir| dir.is_absolute()));
    }
    if let Some(home) = &home {
        places.push(home.join(".local/bin"));
    }
    for (dir, _) in system {
        if !dir.ends_with("per-user") {
            places.push(dir.clone());
        }
    }
    if let Some(home) = &home {
        places.push(home.join(".nix-profile/bin"));
    }
    places.push(PathBuf::from("/usr/bin"));
    places.dedup();
    let in_places = |tool: &str| places.iter().map(|dir| dir.join(executable(tool))).find(|candidate| candidate.is_file());
    let tsc = in_places("tsc")?;
    let node = tsc.with_file_name(executable("node")).is_file()
        .then(|| tsc.with_file_name(executable("node")))
        .or_else(|| in_places("node"))?;
    let module_root = typescript_module_root(&tsc)?;
    Some(TypeScript { tsc, node, module_root })
}

/// The `node_modules` directory that holds the `typescript` package a `tsc`
/// belongs to: npm links `bin/tsc` to `node_modules/typescript/bin/tsc`, and a
/// prefix install keeps it under `lib/node_modules` beside `bin`.
fn typescript_module_root(tsc: &Path) -> Option<PathBuf> {
    let resolved = std::fs::canonicalize(tsc).ok()?;
    let package = resolved.ancestors().find(|dir| dir.file_name().is_some_and(|name| name == "typescript")
        && dir.parent().is_some_and(|parent| parent.file_name().is_some_and(|name| name == "node_modules")));
    if let Some(package) = package {
        return package.parent().map(Path::to_path_buf);
    }
    let prefix_root = tsc.parent()?.parent()?.join("lib/node_modules");
    prefix_root.join("typescript").is_dir().then_some(prefix_root)
}

/// Derivation does not create directories. Explicit paths are kept verbatim so
/// the owner, including its normal invalid-path refusal, remains the authority.
struct CompilerSelection {
    paths: Vec<(LocalHostVariable, PathBuf)>,
    inferred_cargo_home: Option<PathBuf>,
}

impl CompilerSelection {
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
    let captured = process_variables();
    let variable = |name: &str| captured.get(name).cloned();
    let system = SYSTEM_BINS.iter().map(|(dir, place)| (PathBuf::from(dir), *place)).collect::<Vec<_>>();
    let found = match find_rust(&variable, &system) {
        Rust::Found { rustc, place, .. } => {
            let version = version_of(&rustc);
            Rust::Found { rustc, place, version }
        }
        missing @ Rust::Missing { .. } => missing,
    };
    *REPORT.write().unwrap_or_else(PoisonError::into_inner) = Some(found);
    let selected = CompilerSelection::derive(&variable, &system);
    let snapshot = closed_snapshot(selected.paths.clone())?;
    selected.realize()?;
    Ok(snapshot)
}

/// Attaching does not discover tools or create caches. The local registry
/// source may use this client's existing Cargo cache, without claiming that
/// these are the attached owner's compiler or policy settings.
pub(crate) fn captured_by_the_process() -> io::Result<ClosedLocalHostEnvironmentSnapshot> {
    if let Some(encoded) = std::env::var_os(backend_local_service::COMPILER_ENVIRONMENT_ENV) {
        let encoded = encoded.to_str().ok_or_else(|| io::Error::new(
            io::ErrorKind::InvalidInput, "closed compiler environment must be UTF-8",
        ))?;
        return ClosedLocalHostEnvironmentSnapshot::parse(encoded)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error));
    }
    let captured = process_variables();
    closed_snapshot(closed_paths(&|name| captured.get(name).cloned()))
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
