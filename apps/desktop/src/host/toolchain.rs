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

use backend_local_service::LocalHostVariable;
use std::ffi::OsString;
use std::path::{Path, PathBuf};

/// The paths to supply, given the process's variables.
pub(crate) fn supplied(
    variable: &dyn Fn(&str) -> Option<OsString>,
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

    if let Some(rustc) = path("NUDOX_RUSTC") {
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

/// The paths to supply given this process's own environment.
pub(crate) fn supplied_by_the_process() -> Vec<(LocalHostVariable, PathBuf)> {
    supplied(&|name| std::env::var_os(name))
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
