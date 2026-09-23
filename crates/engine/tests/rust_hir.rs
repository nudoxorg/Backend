//! Exercises Rust driver admission through a caller-selected rust-analyzer Cargo graph.
//! Proves HIR declarations enter the compact canonical fragment without a native scanner route.
//! Keeps setup failures typed and validates source-backed declaration identity after admission.

use std::{
    fs,
    path::PathBuf,
    sync::atomic::AtomicBool,
    time::{Duration, Instant},
};

use backend_engine::driver::{
    CompileControl, CompileFailure, CompileOutput, CompileRequest, CompileScratch, NativeTool,
    ResolvedToolchain, SemanticAuthorityInput, ToolchainResolutionError, ToolchainSelection,
    compile,
};
use backend_semantic::ir::EntityKind;
use backend_frontend_rust::legacy::{
    RustAuthorityError, RustFeatureControl, RustProject, RustToolchain, SourceByteLimit,
};
use backend_semantic::vocabulary::{LanguageProfile, RustEdition, Stage};
use thiserror::Error;

/// Typed direct-HIR fixture failure without assertion panics or string terminals.
#[derive(Debug, Error)]
enum TestError {
    /// Cargo did not expose the compiler executable that launched this Rust test.
    #[error("Cargo did not supply an absolute Rust compiler path")]
    MissingRustCompiler,
    /// The direct authority project source could not be read.
    #[error("could not read the direct Rust authority fixture source: {0}")]
    Source(#[source] std::io::Error),
    /// The exact rust-analyzer project authority rejected the fixture.
    #[error(transparent)]
    Authority(#[from] RustAuthorityError),
    /// Driver toolchain provenance could not bind the caller-selected Rust compiler.
    #[error(transparent)]
    Toolchain(#[from] ToolchainResolutionError),
    /// Direct HIR admission did not produce a compact fragment.
    #[error("direct Rust HIR admission did not produce a compact fragment")]
    Compile,
    /// A validated entity atom could not be addressed on this platform.
    #[error("validated entity atom coordinate could not fit this platform")]
    AtomCoordinate(#[source] std::num::TryFromIntError),
    /// A validated entity named an atom absent from its own compact fragment.
    #[error("validated entity referenced a missing compact atom")]
    MissingAtom,
    /// The source-backed `Boxed` declaration was not admitted from HIR.
    #[error("rust-analyzer declaration identity did not survive compact admission")]
    RustToolchain,
}

/// Proves one real Cargo package reaches the direct HIR fact lane without a scanner.
#[test]
fn real_rust_analyzer_project_admits_source_backed_declarations() -> Result<(), TestError> {
    // Pre-cutover this dogfooded `compiler/languages/rust/lib.rs`, addressed
    // relative to the (also pre-cutover) `compiler/engine` manifest dir, and
    // that single file declared `RustToolchain` directly. Commit 4298579e0
    // ("cutover(w7): fold compiler-language-* into frontends/* as legacy
    // modules") folded that crate into `frontends/rust`, whose crate root
    // (`frontends/rust/lib.rs`) now only re-exports `RustToolchain` — the
    // struct itself moved into a submodule file
    // (`frontends/rust/src/legacy/authority.rs`) reached only through a
    // `#[path]`-redirected `mod`. `RustProject::analyze_with_features` opens
    // rust-analyzer's VFS by exact path and only a crate's own registered
    // target root (its `[lib]`/`[[bin]]`/`[[test]]` path — never an
    // arbitrary file reached transitively via `mod`) is guaranteed to be
    // loaded there, so pointing at the submodule file directly fails with a
    // typed `SourceNotLoaded`, not a lowering result. Every other caller in
    // this codebase (the corpus tests, `rust_semantic_lane`, `rust_shortcuts`)
    // already keeps to this rule by construction: they always hand it a
    // package's own crate root. `frontends/rust/fixtures/rich_project` is an
    // existing small standalone Cargo package built for exactly this direct-
    // HIR purpose (already used as a fixture by
    // `crates/engine/src/driver/lower/tests.rs`) whose crate root declares
    // `pub struct Boxed` directly, so this proof uses that instead.
    let project_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../frontends/rust/fixtures/rich_project");
    let source_path = project_root.join("src/lib.rs");
    let source = fs::read(&source_path).map_err(TestError::Source)?;
    let tool = std::env::var_os("RUSTC").map_or_else(
        || PathBuf::from("/etc/profiles/per-user/mileswirht/bin/rustc"),
        PathBuf::from,
    );
    if !tool.is_absolute() {
        return Err(TestError::MissingRustCompiler);
    }
    let frontend_toolchain = RustToolchain::discover(&tool).map_err(RustAuthorityError::from)?;
    // `rich_project`'s own manifest declares `edition = "2021"`.
    let project = RustProject::open_with_source(
        &project_root,
        &source_path,
        &frontend_toolchain,
        RustEdition::Rust2021,
    )?;
    let resolved = ResolvedToolchain::from_version(
        NativeTool::Rustc,
        &tool,
        b"compiler-driver-direct-rust-analyzer",
    )?;
    let cancelled = AtomicBool::new(false);
    let mut diagnostic = [0; 0];
    let mut fragment_output = [0; 65_536];
    let compiled = match compile(
        CompileRequest {
            profile: LanguageProfile::Rust(RustEdition::Rust2021),
            stage: Stage::LowerIr,
            source: &source,
            declaration_scope: backend_engine::driver::DeclarationScope::fixture(),
            toolchain: ToolchainSelection::ResolvedNative(resolved),
            authority: SemanticAuthorityInput::Rust {
                project: &project,
                maximum_source_bytes: SourceByteLimit::from(65_536),
                features: RustFeatureControl::default(),
            },
            control: CompileControl {
                // This bound proves enforcement shape, not performance; fixture analysis can exceed 30s under load.
                deadline: Instant::now() + Duration::from_secs(300),
                cancelled: &cancelled,
            },
        },
        CompileScratch {
            diagnostic_output: &mut diagnostic,
            native_work: &project_root,
        },
        CompileOutput {
            fragment_output: &mut fragment_output,
        },
    ) {
        Ok(compiled) => compiled,
        Err(CompileFailure::Authority { .. })
        | Err(CompileFailure::AuthorityInputRequired { .. })
        | Err(CompileFailure::AuthorityInputProfileMismatch { .. })
        | Err(CompileFailure::LoweringUnsupported { .. })
        | Err(CompileFailure::ExtensionAtomUnbound { .. })
        | Err(CompileFailure::ExtensionTypeParametersUnbound { .. })
        | Err(CompileFailure::ClangProjection { .. })
        | Err(CompileFailure::Build { .. })
        | Err(CompileFailure::Prepare { .. })
        | Err(CompileFailure::Write { .. })
        | Err(CompileFailure::Validate { .. })
        | Err(CompileFailure::SourceLength { .. })
        | Err(CompileFailure::UnsupportedStage { .. })
        | Err(CompileFailure::ToolchainSelectionMismatch { .. })
        | Err(CompileFailure::ToolchainMismatch { .. })
        | Err(CompileFailure::NativeWork { .. })
        | Err(CompileFailure::NativeWorkCleanup { .. })
        | Err(CompileFailure::ToolingUnavailable { .. })
        | Err(CompileFailure::ToolStart { .. })
        | Err(CompileFailure::MissingToolInput { .. })
        | Err(CompileFailure::MissingToolInputCleanup { .. })
        | Err(CompileFailure::MissingToolDiagnostic { .. })
        | Err(CompileFailure::MissingToolDiagnosticCleanup { .. })
        | Err(CompileFailure::ToolInput { .. })
        | Err(CompileFailure::ToolInputCleanup { .. })
        | Err(CompileFailure::ToolTerminate { .. })
        | Err(CompileFailure::ToolWait { .. })
        | Err(CompileFailure::ToolWaitCleanup { .. })
        | Err(CompileFailure::ToolDiagnosticRead { .. })
        | Err(CompileFailure::ToolDiagnosticReadCleanup { .. })
        | Err(CompileFailure::NativeWorkerPanic { .. })
        | Err(CompileFailure::Cancelled { .. })
        | Err(CompileFailure::DeadlineExceeded { .. })
        | Err(CompileFailure::DiagnosticLimit { .. })
        | Err(CompileFailure::NativeRejected { .. }) => return Err(TestError::Compile),
    };
    let mut found = false;
    for entity in compiled.fragment.entities() {
        if entity.kind != EntityKind::Record {
            continue;
        }
        let atom = usize::try_from(entity.name.raw).map_err(TestError::AtomCoordinate)?;
        let Some(name) = compiled.fragment.atoms().nth(atom) else {
            return Err(TestError::MissingAtom);
        };
        if name.bytes == b"Boxed" {
            found = true;
            break;
        }
    }
    if found {
        Ok(())
    } else {
        Err(TestError::RustToolchain)
    }
}
