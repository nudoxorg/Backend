#![forbid(unsafe_code)]

use std::{
    fs,
    path::PathBuf,
    sync::atomic::AtomicBool,
    time::{Duration, Instant},
};

use compiler_driver::{
    CompileControl, CompileFailure, CompileOutput, CompileRequest, CompileScratch, NativeTool,
    ResolvedToolchain, SemanticAuthorityInput, ToolchainSelection, compile,
};
use compiler_languages_rust::{RustFeatureControl, RustPackageUrl, RustToolchain, SourceByteLimit};
use compiler_vocabulary::{LanguageProfile, Stage};

fn rustc_path() -> Result<PathBuf, Box<dyn std::error::Error>> {
    let path = std::env::var_os("RUSTC")
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .or_else(|| {
            std::env::var_os("PATH").and_then(|value| {
                std::env::split_paths(&value)
                    .map(|directory| directory.join("rustc"))
                    .find(|path| path.is_file())
            })
        })
        .ok_or("no absolute rustc was available")?;
    Ok(path.canonicalize()?)
}

/// The pre-change unbounded walk on this fixture was observed at 195–252 seconds
/// under load. Enforcement at the first emitter phase boundary after the
/// 10-second deadline was observed at 33 seconds uncontended and 48 seconds
/// loaded. The 150-second floor therefore proves early termination without being
/// load-fragile.
#[test]
fn registry_log_walk_returns_the_exact_deadline_terminal() -> Result<(), Box<dyn std::error::Error>>
{
    let tool = rustc_path()?;
    let toolchain = RustToolchain::discover(&tool)?;
    let workspace = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()?;
    let registry = std::env::var_os("CARGO_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".cargo")))
        .ok_or("no Cargo home was available")?
        .join("registry/src");
    let cancelled = AtomicBool::new(false);
    let located = RustPackageUrl::parse("cargo:log@0.4.34")?.locate(
        &workspace,
        &toolchain,
        Some(&registry),
        &cancelled,
    )?;
    let source = fs::read(&located.project().source_path)?;
    let resolved = ResolvedToolchain::from_version(
        NativeTool::Rustc,
        &tool,
        b"compiler-driver-rust-deadline-falsifier",
    )?;
    let mut diagnostic = [];
    let mut output = vec![0_u8; 16 * 1024 * 1024];
    let request = CompileRequest {
        profile: LanguageProfile::Rust(located.project().edition),
        stage: Stage::LowerIr,
        source: &source,
            declaration_scope: compiler_driver::DeclarationScope::fixture(),
        toolchain: ToolchainSelection::ResolvedNative(resolved),
        authority: SemanticAuthorityInput::Rust {
            project: located.project(),
            maximum_source_bytes: SourceByteLimit::from(4 * 1024 * 1024),
            features: RustFeatureControl::default(),
        },
        control: CompileControl {
            deadline: Instant::now() + Duration::from_secs(10),
            cancelled: &cancelled,
        },
    };
    let started = Instant::now();
    let result = compile(
        request,
        CompileScratch {
            diagnostic_output: &mut diagnostic,
            native_work: &workspace,
        },
        CompileOutput {
            fragment_output: &mut output,
        },
    );
    let elapsed = started.elapsed();
    assert!(
        elapsed < Duration::from_secs(150),
        "deadline walk took {elapsed:?}"
    );
    assert!(matches!(
        result,
        Err(CompileFailure::DeadlineExceeded { .. })
    ));
    Ok(())
}
