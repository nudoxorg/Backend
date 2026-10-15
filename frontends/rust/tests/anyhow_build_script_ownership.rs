//! Regresses Cargo ownership of the exact pinned `anyhow` build-script root.

use std::{
    error::Error,
    fs, io,
    path::{Path, PathBuf},
    sync::atomic::AtomicBool,
    time::{Duration, Instant},
};

use backend_frontend_rust::legacy::{
    RustAnalysisControl, RustProject, RustSourceScope, RustToolchain, SourceByteLimit,
};
use backend_semantic::vocabulary::RustEdition;

#[test]
fn pinned_anyhow_build_script_has_one_active_cargo_owner() -> Result<(), Box<dyn Error>> {
    const CARGO_LOCK: &str = include_str!("../../../Cargo.lock");
    assert!(CARGO_LOCK.split("[[package]]").any(|package| {
        package.lines().any(|line| line.trim() == "name = \"anyhow\"")
            && package
                .lines()
                .any(|line| line.trim() == "version = \"1.0.104\"")
            && package.lines().any(|line| {
                line.trim() == "source = \"registry+https://github.com/rust-lang/crates.io-index\""
            })
            && package.lines().any(|line| {
                line.trim() == "checksum = \"330a5ed07fa54e4702c9d6c4174f74427fc0ef6e214bbd677ae50a5099946470\""
            })
    }));
    let root = cached_anyhow_1_0_104()?;
    let manifest = fs::read_to_string(root.join("Cargo.toml"))?;
    assert!(manifest
        .lines()
        .any(|line| line.trim() == "name = \"anyhow\""));
    assert!(manifest
        .lines()
        .any(|line| line.trim() == "version = \"1.0.104\""));
    assert!(manifest
        .lines()
        .any(|line| line.trim() == "edition = \"2021\""));
    assert!(manifest
        .lines()
        .any(|line| line.trim() == "build = \"build.rs\""));

    let source_path = root.join("build.rs");
    let source = fs::read(&source_path)?;
    let rustc = std::env::var_os("RUSTC").map_or_else(|| PathBuf::from("rustc"), PathBuf::from);
    let toolchain = RustToolchain::discover(rustc)?;
    let project =
        RustProject::open_with_source(&root, &source_path, &toolchain, RustEdition::Rust2021)?;
    let cancelled = AtomicBool::new(false);
    let outcome = project.analyze(
        RustAnalysisControl {
            cancelled: &cancelled,
            maximum_source_bytes: SourceByteLimit::from(64 * 1024),
            deadline: Instant::now() + Duration::from_secs(180),
        },
        |authority| {
            assert_eq!(authority.source_scope, RustSourceScope::CargoTargetRoot);
            assert_eq!(authority.source, source.as_slice());
            Ok(())
        },
    );
    assert!(
        outcome.is_ok(),
        "exact crates.io anyhow 1.0.104 build.rs must resolve to its Cargo target owner; got {outcome:?}"
    );
    Ok(())
}

/// Locates only the exact crates.io registry extraction; the test never downloads sources.
fn cached_anyhow_1_0_104() -> Result<PathBuf, Box<dyn Error>> {
    let mut matches = Vec::new();
    let mut cargo_homes = Vec::new();
    if let Some(cargo_home) = std::env::var_os("CARGO_HOME") {
        cargo_homes.push(PathBuf::from(cargo_home));
    }
    if let Some(home) = std::env::var_os("HOME") {
        cargo_homes.push(PathBuf::from(home).join(".cargo"));
    }
    cargo_homes.sort();
    cargo_homes.dedup();
    for cargo_home in &cargo_homes {
        let source_roots = cargo_home.join("registry/src");
        let registries = match fs::read_dir(&source_roots) {
            Ok(registries) => registries,
            Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error.into()),
        };
        for registry in registries {
            let registry = registry?.path();
            if !registry
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with("index.crates.io-"))
            {
                continue;
            }
            let candidate = registry.join("anyhow-1.0.104");
            if candidate.join("Cargo.toml").is_file() && candidate.join("build.rs").is_file() {
                matches.push(candidate);
            }
        }
    }
    matches.sort();
    matches.dedup();
    if matches.len() != 1 {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!(
                "expected one cached crates.io anyhow-1.0.104 source with build.rs, found {} under {:?}",
                matches.len(),
                cargo_homes
            ),
        )
        .into());
    }
    let root = matches
        .pop()
        .expect("one exact registry source was checked");
    if !Path::new(&root).join("src/lib.rs").is_file() {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            "pinned anyhow-1.0.104 source is missing its declared library root",
        )
        .into());
    }
    Ok(root)
}
