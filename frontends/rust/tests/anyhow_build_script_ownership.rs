//! Regresses Cargo ownership of the exact pinned `anyhow` package frontier.

use std::{
    error::Error,
    fs, io,
    path::{Path, PathBuf},
    sync::atomic::AtomicBool,
    time::{Duration, Instant},
};

use backend_frontend_rust::legacy::{
    RustAnalysisControl, RustCargoMetadataPolicy, RustFeatureControl, RustToolchain,
    RustWorkspaceFile, RustWorkspaceSessionKey, RustWorkspaceSessionLane, SourceByteLimit,
};
use backend_semantic::vocabulary::{RustEdition, Stage};

// This is the .rs member list of the checksum-pinned anyhow-1.0.104 crate archive.
// Capturing all three Cargo source-target families (library, build script, tests)
// matches the complete package frontier used when the engine begins a session.
const RUST_SOURCE_PATHS: &[&str] = &[
    "build.rs",
    "src/backtrace.rs",
    "src/chain.rs",
    "src/context.rs",
    "src/ensure.rs",
    "src/error.rs",
    "src/fmt.rs",
    "src/kind.rs",
    "src/lib.rs",
    "src/macros.rs",
    "src/nightly.rs",
    "src/ptr.rs",
    "src/wrapper.rs",
    "tests/common/mod.rs",
    "tests/compiletest.rs",
    "tests/drop/mod.rs",
    "tests/test_autotrait.rs",
    "tests/test_backtrace.rs",
    "tests/test_boxed.rs",
    "tests/test_chain.rs",
    "tests/test_context.rs",
    "tests/test_convert.rs",
    "tests/test_downcast.rs",
    "tests/test_ensure.rs",
    "tests/test_ffi.rs",
    "tests/test_fmt.rs",
    "tests/test_macros.rs",
    "tests/test_repr.rs",
    "tests/test_source.rs",
    "tests/ui/chained-comparison.rs",
    "tests/ui/empty-ensure.rs",
    "tests/ui/ensure-nonbool.rs",
    "tests/ui/must-use.rs",
    "tests/ui/no-impl.rs",
    "tests/ui/temporary-value.rs",
    "tests/ui/wrong-interpolation.rs",
];

#[test]
fn pinned_anyhow_build_script_has_one_active_cargo_owner() -> Result<(), Box<dyn Error>> {
    const CARGO_LOCK: &str = include_str!("../../../Cargo.lock");
    assert!(CARGO_LOCK.split("[[package]]").any(|package| {
        package
            .lines()
            .any(|line| line.trim() == "name = \"anyhow\"")
            && package
                .lines()
                .any(|line| line.trim() == "version = \"1.0.104\"")
            && package.lines().any(|line| {
                line.trim() == "source = \"registry+https://github.com/rust-lang/crates.io-index\""
            }) && package.lines().any(|line| {
            line.trim()
                == "checksum = \"330a5ed07fa54e4702c9d6c4174f74427fc0ef6e214bbd677ae50a5099946470\""
        })
    }));
    let root = cached_anyhow_1_0_104()?;
    let manifest = fs::read_to_string(root.join("Cargo.toml"))?;
    assert!(
        manifest
            .lines()
            .any(|line| line.trim() == "name = \"anyhow\"")
    );
    assert!(
        manifest
            .lines()
            .any(|line| line.trim() == "version = \"1.0.104\"")
    );
    assert!(
        manifest
            .lines()
            .any(|line| line.trim() == "edition = \"2021\"")
    );
    assert!(
        manifest
            .lines()
            .any(|line| line.trim() == "build = \"build.rs\"")
    );

    let mut sources = RUST_SOURCE_PATHS
        .iter()
        .map(|relative| {
            let relative_path = PathBuf::from(relative);
            let source = fs::read_to_string(root.join(&relative_path))?;
            Ok((relative_path, source))
        })
        .collect::<Result<Vec<_>, io::Error>>()?;
    sources.sort_unstable_by(|left, right| left.0.cmp(&right.0));
    let source_paths = sources
        .iter()
        .map(|(path, _)| path.clone())
        .collect::<Vec<_>>();
    let files = sources
        .iter()
        .map(|(relative_path, source)| RustWorkspaceFile {
            relative_path,
            source,
        })
        .collect::<Vec<_>>();

    let rustc = std::env::var_os("RUSTC").map_or_else(|| PathBuf::from("rustc"), PathBuf::from);
    let toolchain = RustToolchain::discover(rustc)?;
    let key = RustWorkspaceSessionKey::new(
        &root,
        &toolchain,
        RustEdition::Rust2021,
        Stage::LowerIr,
        RustFeatureControl::default(),
        RustCargoMetadataPolicy::Offline,
        None,
        None,
        None,
        [0x4a; 32],
        &source_paths,
    )?;
    let cancelled = AtomicBool::new(false);
    let control = RustAnalysisControl {
        cancelled: &cancelled,
        maximum_source_bytes: SourceByteLimit::from(64 * 1024 * 1024),
        deadline: Instant::now() + Duration::from_secs(300),
    };
    let mut lane = RustWorkspaceSessionLane::default();
    // `begin` performs the same full captured-source package preload as the
    // engine. A single-source RustProject open does not exercise that path.
    let lease = lane
        .begin(key, &files, control)
        .unwrap_or_else(|error| panic!("full package preload failed: {error:?}"));
    let build_source = sources
        .iter()
        .find(|(path, _)| path == Path::new("build.rs"))
        .expect("pinned crate source list includes build.rs");
    let outcome = lease.workspace().analyze_source(
        root.join(&build_source.0),
        &build_source.1,
        control,
        |authority| {
            assert_eq!(authority.source, build_source.1.as_bytes());
            Ok(())
        },
    );
    assert!(
        outcome.is_ok(),
        "exact crates.io anyhow 1.0.104 full source frontier must resolve build.rs to its Cargo target owner; got {outcome:?}"
    );
    lease.commit();
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
