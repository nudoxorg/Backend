//! Regression: repeated Go blank struct fields must not share one declaration
//! identity.
//!
//! Go allows several `_` fields in one struct. The source spelling stays `_`,
//! but each unbound field needs a positional identity discriminator or
//! `finish` returns `DuplicateDeclarationIdentity`.

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
use backend_semantic::ir::{EntityKind, Ir};
use backend_semantic::vocabulary::{GoVersion, LanguageProfile, Stage};

static FIXTURE_SEQUENCE: AtomicU64 = AtomicU64::new(0);

const BLANK_STRUCT_SOURCE: &[u8] = br#"package blank

type Pad struct {
	_ int
	Keep int
	_ int
}
"#;

fn toolchain() -> ResolvedToolchain<'static> {
    ResolvedToolchain::from_version(
        NativeTool::GoCompiler,
        Path::new("/bin/true"),
        b"go-compiler-blank-identity-repro",
    )
    .expect("toolchain resolution is deterministic for the pinned tool identity")
}

fn stage_blank_module() -> Result<(PathBuf, PathBuf), String> {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0);
    let sequence = FIXTURE_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let root = std::env::temp_dir().join(format!(
        "nudox-go-blank-identity-{nonce}-{}-{sequence}",
        std::process::id()
    ));
    fs::create_dir_all(&root).map_err(|error| error.to_string())?;
    fs::write(root.join("go.mod"), b"module blank.example/fixture\n\ngo 1.22\n")
        .map_err(|error| error.to_string())?;
    let source_path = root.join("blank.go");
    fs::write(&source_path, BLANK_STRUCT_SOURCE).map_err(|error| error.to_string())?;
    Ok((root, source_path))
}

#[test]
fn repeated_blank_struct_fields_lower_distinctly_through_the_oracle() {
    let (module_root, source_path) = stage_blank_module().expect("the fixture stages");
    let source = fs::read(&source_path).expect("the staged source is readable");

    let oracle = GoOracle {
        output_limit: 32 * 1024 * 1024,
        timeout: Duration::from_secs(300),
    };
    let image_bytes = oracle
        .authority_image_for_package(&source_path, &module_root)
        .expect("the authority accepts repeated blank struct fields");

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

    let ir = match compiled {
        Ok(output) => output.ir,
        Err(error) => panic!("repeated blank struct fields must lower, not duplicate: {error:?}"),
    };

    let blanks: Vec<_> = ir
        .items()
        .filter(|item| item.kind() == EntityKind::Field && item.name() == b"_")
        .map(|item| item.version().identity())
        .collect();
    assert_eq!(blanks.len(), 2, "both same-typed blank fields must lower");
    assert_ne!(
        blanks[0], blanks[1],
        "same-typed blank fields must not share one declaration identity"
    );
    assert_eq!(
        ir.items()
            .filter(|item| item.kind() == EntityKind::Field && item.name() == b"Keep")
            .count(),
        1,
        "the named field stays one declaration"
    );
}
