//! Proves that borrowed Cargo feature controls change the public Rust semantic terminal.

use std::{
    fs,
    path::PathBuf,
    sync::atomic::{AtomicBool, AtomicU64, Ordering},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use backend_engine::driver::{
    CompileControl, CompileOutput, CompileRequest, CompileScratch, ResolvedToolchain,
    SemanticAuthorityInput, ToolchainSelection, compile,
};
use backend_semantic::ir::{EntityKind, FragmentView};
use backend_frontend_rust::legacy::{RustFeatureControl, RustProject, RustToolchain, SourceByteLimit};
use backend_semantic::vocabulary::{LanguageProfile, RustEdition, Stage};
use thiserror::Error;

const FIXTURE: &str = r#"pub fn anchor() {}

#[cfg(feature = "base")]
pub fn base() {}

#[cfg(feature = "extra")]
pub fn extra() {}
"#;

#[derive(Debug, Error)]
enum TestError {
    #[error("I/O during {operation}: {source}")]
    Io {
        operation: &'static str,
        #[source]
        source: std::io::Error,
    },
    #[error("fixture clock failed: {0}")]
    Clock(#[source] std::time::SystemTimeError),
    #[error("rustc was not found")]
    MissingRustc,
    #[error("Rust authority failed: {0}")]
    Authority(#[source] backend_frontend_rust::legacy::RustAuthorityError),
    #[error("compile failed: {0}")]
    Compile(String),
    #[error("fragment validation failed: {0}")]
    Validate(#[from] backend_semantic::ir::FragmentError),
}

/// Separates concurrently executing fixtures created during one process lifetime.
static FIXTURE_SEQUENCE: AtomicU64 = AtomicU64::new(0);

fn fixture_root() -> Result<PathBuf, TestError> {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(TestError::Clock)?
        .as_nanos();
    let sequence = FIXTURE_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let root = std::env::temp_dir().join(format!("nudox-rust-features-{nonce}-{sequence}"));
    fs::create_dir_all(root.join("src")).map_err(|source| TestError::Io {
        operation: "create fixture",
        source,
    })?;
    fs::write(
        root.join("Cargo.toml"),
        "[package]\nname = \"feature_fixture\"\nversion = \"0.1.0\"\nedition = \"2024\"\n\n[features]\ndefault = [\"base\"]\nbase = []\nextra = []\n",
    )
    .map_err(|source| TestError::Io {
        operation: "write manifest",
        source,
    })?;
    fs::write(root.join("src/lib.rs"), FIXTURE).map_err(|source| TestError::Io {
        operation: "write crate root",
        source,
    })?;
    Ok(root)
}

fn rustc_path() -> Result<PathBuf, TestError> {
    let path = std::env::var_os("PATH").ok_or(TestError::MissingRustc)?;
    std::env::split_paths(&path)
        .map(|directory| directory.join("rustc"))
        .find_map(|candidate| {
            candidate
                .is_file()
                .then(|| candidate.canonicalize().ok())
                .flatten()
        })
        .ok_or(TestError::MissingRustc)
}

fn compile_fixture(features: RustFeatureControl<'_>) -> Result<Vec<u8>, TestError> {
    let root = fixture_root()?;
    let result = (|| {
        let source_path = root.join("src/lib.rs");
        let tool = rustc_path()?;
        let toolchain = RustToolchain::discover(&tool).map_err(|_| TestError::MissingRustc)?;
        let project =
            RustProject::open_with_source(&root, &source_path, &toolchain, RustEdition::Rust2024)
                .map_err(TestError::Authority)?;
        let resolved = ResolvedToolchain::from_version(
            backend_engine::driver::NativeTool::Rustc,
            &tool,
            b"compiler-driver-rust-features",
        )
        .map_err(|_| TestError::MissingRustc)?;
        let cancelled = AtomicBool::new(false);
        let mut diagnostic = [];
        let mut output = vec![0_u8; 65_536];
        let compiled = compile(
            CompileRequest {
                profile: LanguageProfile::Rust(RustEdition::Rust2024),
                stage: Stage::LowerIr,
                source: FIXTURE.as_bytes(),
                declaration_scope: backend_engine::driver::DeclarationScope::fixture(),
                toolchain: ToolchainSelection::ResolvedNative(resolved),
                authority: SemanticAuthorityInput::Rust {
                    project: &project,
                    maximum_source_bytes: SourceByteLimit::from(65_536),
                    features,
                },
                control: CompileControl {
                    deadline: Instant::now() + Duration::from_secs(120),
                    cancelled: &cancelled,
                },
            },
            CompileScratch {
                diagnostic_output: &mut diagnostic,
                native_work: &root,
            },
            CompileOutput {
                fragment_output: &mut output,
            },
        )
        .map_err(|failure| TestError::Compile(format!("{failure:?}")))?;
        let _ = compiled;
        let length = output
            .get(8..12)
            .and_then(|bytes| <[u8; 4]>::try_from(bytes).ok())
            .map(u32::from_le_bytes)
            .and_then(|length| usize::try_from(length).ok())
            .ok_or(TestError::Compile("fragment header truncated".to_owned()))?;
        Ok(output[..length].to_vec())
    })();
    fs::remove_dir_all(&root).map_err(|source| TestError::Io {
        operation: "remove fixture",
        source,
    })?;
    result
}

fn has_entity(bytes: &[u8], name: &[u8]) -> Result<bool, TestError> {
    let view = FragmentView::validate(bytes)?;
    Ok(view.entities().any(|entity| {
        entity.kind == EntityKind::Function
            && usize::try_from(entity.name.raw)
                .ok()
                .and_then(|atom| view.atoms().nth(atom))
                .is_some_and(|atom| atom.bytes == name)
    }))
}

#[test]
fn named_feature_controls_cfg_declaration() -> Result<(), TestError> {
    let defaults = compile_fixture(RustFeatureControl::default())?;
    let enabled = compile_fixture(RustFeatureControl {
        features: &["extra"],
        ..RustFeatureControl::default()
    })?;
    assert!(!has_entity(&defaults, b"extra")?);
    assert!(has_entity(&enabled, b"extra")?);
    Ok(())
}

#[test]
fn no_default_features_suppresses_default_cfg_declaration() -> Result<(), TestError> {
    let disabled = compile_fixture(RustFeatureControl {
        no_default_features: true,
        ..RustFeatureControl::default()
    })?;
    assert!(has_entity(&disabled, b"anchor")?);
    assert!(!has_entity(&disabled, b"base")?);
    assert!(!has_entity(&disabled, b"extra")?);
    Ok(())
}

#[test]
fn all_features_admits_every_cfg_declaration() -> Result<(), TestError> {
    let enabled = compile_fixture(RustFeatureControl {
        all_features: true,
        ..RustFeatureControl::default()
    })?;
    assert!(has_entity(&enabled, b"base")?);
    assert!(has_entity(&enabled, b"extra")?);
    Ok(())
}

#[test]
fn default_features_admit_only_the_default_cfg_declaration() -> Result<(), TestError> {
    let defaults = compile_fixture(RustFeatureControl::default())?;
    assert!(has_entity(&defaults, b"base")?);
    assert!(!has_entity(&defaults, b"extra")?);
    Ok(())
}
