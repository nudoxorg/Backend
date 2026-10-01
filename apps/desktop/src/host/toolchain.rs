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
//! supplied only when the process has not set that variable itself and the path
//! exists as the owner requires (an absolute file or directory): the owner
//! refuses to start on a configured path it cannot use, and a derived one must
//! never turn a working configuration into a refusal.
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

use backend_local_service::LocalHostVariable;
use std::ffi::OsString;
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

/// The first directory that holds both `rustc` and `cargo`: an explicit
/// `NUDOX_RUSTC`, else the process's `PATH`, rustup's proxies, Homebrew, Nix.
/// `system` is [`SYSTEM_BINS`] in production (a test gives its own).
pub(crate) fn find_rust(variable: &dyn Fn(&str) -> Option<OsString>, system: &[(PathBuf, Place)]) -> Rust {
    let value = |name: &str| variable(name).filter(|value| !value.is_empty());
    if let Some(rustc) = value("NUDOX_RUSTC") {
        return Rust::Found { rustc: PathBuf::from(rustc), place: Place::Named, version: None };
    }
    let home = value("HOME").map(PathBuf::from).filter(|home| home.is_absolute());
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
        let (rustc, cargo) = (dir.join("rustc"), dir.join("cargo"));
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
    let home = || path("HOME");
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
            && let Some(cargo) = file(Path::new(&rustc).with_file_name("cargo"))
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

/// The paths to supply given this process's own environment, a person's
/// Rust looked for where people install it; what was found is [`report`]ed.
pub(crate) fn supplied_by_the_process() -> Vec<(LocalHostVariable, PathBuf)> {
    let variable = |name: &str| std::env::var_os(name);
    let system = SYSTEM_BINS.iter().map(|(dir, place)| (PathBuf::from(dir), *place)).collect::<Vec<_>>();
    let found = match find_rust(&variable, &system) {
        Rust::Found { rustc, place, .. } => {
            let version = version_of(&rustc);
            Rust::Found { rustc, place, version }
        }
        missing @ Rust::Missing { .. } => missing,
    };
    *REPORT.write().unwrap_or_else(PoisonError::into_inner) = Some(found);
    supplied_among(&variable, &system)
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
        let root = PathBuf::from("/tmp").join(format!("nx-toolchain-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let bin = root.join("toolchain/bin");
        let cargo_home = root.join("home/.cargo");
        let go_cache = root.join("home/go/pkg/mod");
        for directory in [&bin, &cargo_home, &go_cache] {
            std::fs::create_dir_all(directory).expect("scratch directory");
        }
        for tool in ["rustc", "cargo"] {
            std::fs::write(bin.join(tool), b"#!/bin/sh\n").expect("tool");
        }
        Machine {
            rustc: bin.join("rustc"),
            cargo: bin.join("cargo"),
            cargo_home,
            go_cache,
            root,
        }
    }

    fn env(pairs: &[(&str, &Path)]) -> impl Fn(&str) -> Option<OsString> {
        let pairs = pairs
            .iter()
            .map(|(name, value)| ((*name).to_owned(), value.as_os_str().to_owned()))
            .collect::<BTreeMap<_, _>>();
        move |name| pairs.get(name).cloned()
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
            std::fs::write(dir.join(tool), b"#!/bin/sh\n").expect("tool");
        }
        dir.to_path_buf()
    }

    #[test]
    fn a_finder_launch_finds_the_rust_a_person_installed_and_supplies_it_whole() {
        let root = PathBuf::from("/tmp").join(format!("nx-toolchain-finder-{}", std::process::id()));
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
                (LocalHostVariable::NudoxRustc, rustup.join("rustc")),
                (LocalHostVariable::NudoxCargo, rustup.join("cargo")),
                (LocalHostVariable::NudoxCargoHome, home.join(".cargo")),
            ],
            "rustc, the cargo beside it, and cargo's home"
        );
        assert_eq!(find_rust(&finder, &system), Rust::Found { rustc: rustup.join("rustc"), place: Place::Rustup, version: None });
        // Without rustup: Homebrew's.
        std::fs::remove_dir_all(&rustup).expect("uninstall rustup");
        assert_eq!(find_rust(&finder, &system), Rust::Found { rustc: homebrew.join("rustc"), place: Place::Homebrew, version: None });
        // A terminal launch's PATH comes first; a directory with a rustc and
        // no cargo beside it is not an install.
        let half = bin(&root.join("half/bin"), &["rustc"]);
        let whole = bin(&root.join("whole/bin"), &["rustc", "cargo"]);
        let path = std::env::join_paths([&half, &whole]).expect("PATH");
        let terminal_vars = [("HOME", home.as_path()), ("PATH", Path::new(&path))];
        let terminal = env(&terminal_vars);
        assert_eq!(find_rust(&terminal, &system), Rust::Found { rustc: whole.join("rustc"), place: Place::Path, version: None });
        // An explicit NUDOX_RUSTC wins over anything found.
        let named_rustc = homebrew.join("rustc");
        let named_vars = [("HOME", home.as_path()), ("PATH", Path::new(&path)), ("NUDOX_RUSTC", named_rustc.as_path())];
        let named = env(&named_vars);
        assert_eq!(find_rust(&named, &system), Rust::Found { rustc: homebrew.join("rustc"), place: Place::Named, version: None });
        assert!(
            !supplied_among(&named, &system).iter().any(|(variable, _)| *variable == LocalHostVariable::NudoxRustc),
            "a named rustc is the process's own variable, never supplied over it"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_machine_with_no_rust_says_so_in_words_and_supplies_nothing() {
        let root = PathBuf::from("/tmp").join(format!("nx-toolchain-none-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let home = root.join("home");
        std::fs::create_dir_all(&home).expect("home");
        let empty = bin(&root.join("system/bin"), &[]);
        let finder_vars = [("HOME", home.as_path()), ("PATH", Path::new("/usr/bin:/bin"))];
        let finder = env(&finder_vars);
        let system = [(empty.clone(), Place::Homebrew)];
        let found = find_rust(&finder, &system);
        let Rust::Missing { looked } = &found else { panic!("no rustc anywhere: {found:?}") };
        assert_eq!(
            looked,
            &[PathBuf::from("/usr/bin"), PathBuf::from("/bin"), home.join(".cargo/bin"), empty, home.join(".nix-profile/bin")],
            "every place it looked, in order"
        );
        assert_eq!(
            found.words(),
            "No Rust toolchain was found. Install one with rustup (rustup.rs) or Homebrew (brew install rust), then quit and reopen Nudox."
        );
        assert!(supplied_among(&finder, &system).is_empty());
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
