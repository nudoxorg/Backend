//! Focused repro for the `golang:github.com/stretchr/testify@v1.9.0` audit row.
//!
//! The audit selects the root package's only non-test file, `doc.go` (759
//! bytes): a package clause plus comments, with no declarations at all. That
//! is legal Go, and the Go authority image for it succeeds while proving zero
//! declarations. The engine used to reject the collected-empty fact set with
//! the lane-wide `NoSupportedDeclaration` terminal, contradicting an
//! authority that succeeded; a Go package the authority proves empty now
//! lowers to the same zero-declaration product.

use std::{
    fs,
    path::{Path, PathBuf},
    sync::atomic::{AtomicBool, AtomicU64, Ordering},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use backend_engine::driver::{
    CompileControl, CompileRequest, CompileScratch, NativeTool, ResolvedToolchain,
    SemanticAuthorityInput, ToolchainSelection, compile_ir,
};
use backend_frontend_go::legacy::GoOracle;
use backend_semantic::vocabulary::{GoVersion, LanguageProfile, Stage};

static FIXTURE_SEQUENCE: AtomicU64 = AtomicU64::new(0);

/// The audited shape: a package clause plus a doc comment, nothing else.
const DOC_ONLY_SOURCE: &[u8] = b"// Package doconly exists only to carry its package clause.\npackage doconly\n";

fn toolchain() -> ResolvedToolchain<'static> {
    ResolvedToolchain::from_version(
        NativeTool::GoCompiler,
        Path::new("/bin/true"),
        b"go-compiler-doc-only-repro",
    )
    .expect("toolchain resolution is deterministic for the pinned tool identity")
}

/// Stages one doc-only Go module and returns its root and source path.
fn stage_doc_only_module() -> Result<(PathBuf, PathBuf), String> {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0);
    let sequence = FIXTURE_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let root = std::env::temp_dir().join(format!(
        "nudox-go-doc-only-{nonce}-{}-{sequence}",
        std::process::id()
    ));
    fs::create_dir_all(&root).map_err(|error| error.to_string())?;
    fs::write(root.join("go.mod"), b"module doconly.example/fixture\n\ngo 1.24\n")
        .map_err(|error| error.to_string())?;
    let source_path = root.join("doc.go");
    fs::write(&source_path, DOC_ONLY_SOURCE).map_err(|error| error.to_string())?;
    Ok((root, source_path))
}

#[test]
fn go_doc_only_package_lowers_to_the_authority_empty_product() {
    let (module_root, source_path) = stage_doc_only_module().expect("the fixture stages");
    let source = fs::read(&source_path).expect("the staged source is readable");

    let oracle = GoOracle {
        output_limit: 32 * 1024 * 1024,
        timeout: Duration::from_secs(300),
    };
    let image_bytes = oracle
        .authority_image_for_package(&source_path, &module_root)
        .expect("the authority accepts a declaration-free package");
    let image = backend_frontend_go::legacy::GoImage::open(&image_bytes)
        .expect("the authority image opens");
    assert_eq!(
        image.declaration_count(),
        0,
        "the doc-only package proves zero declarations"
    );

    let cancelled = AtomicBool::new(false);
    let mut diagnostic = vec![0; 64 * 1024];
    let request = CompileRequest {
        profile: LanguageProfile::Go(GoVersion::Go125),
        stage: Stage::LowerIr,
        source: &source,
        declaration_scope: backend_engine::driver::DeclarationScope::fixture(),
        toolchain: ToolchainSelection::ResolvedNative(toolchain()),
        authority: SemanticAuthorityInput::Go {
            image: &image_bytes,
        },
        control: CompileControl {
            deadline: Instant::now() + Duration::from_secs(300),
            cancelled: &cancelled,
        },
    };
    let compiled = compile_ir(
        request,
        CompileScratch {
            diagnostic_output: &mut diagnostic,
            native_work: &module_root,
        },
    );
    let _ = fs::remove_dir_all(&module_root);
    let ir = compiled.expect("a declaration-free Go package lowers like its authority");
    assert_eq!(
        ir.ir.entity_count(),
        0,
        "the lowered product carries exactly the authority's zero declarations"
    );
}
