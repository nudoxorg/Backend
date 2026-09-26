//! Use-case flow: blank struct fields stay distinct when the type and its
//! use site live in separate Go source files within one package.

#[path = "use_case_support/mod.rs"]
mod use_case_support;

use std::{
    fs,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::atomic::{AtomicBool, AtomicU64, Ordering},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use backend_engine::driver::{
    CompileControl, CompileRequest, CompileScratch, NativeTool, ResolvedToolchain,
    SemanticAuthorityInput, ToolchainSelection, compile_ir,
};
use backend_frontend_go::legacy::{GoOracle, OracleError};
use backend_semantic::ir::{EntityKind, Ir};
use backend_semantic::vocabulary::{GoVersion, LanguageProfile, Stage};
use use_case_support::{CompileTimer, count_named};

static FIXTURE_SEQUENCE: AtomicU64 = AtomicU64::new(0);

const BLANK_GO: &[u8] = br#"package flow
type Pad struct { _ int; Keep int; _ int }
"#;

const USE_GO: &[u8] = br#"package flow
func Use(p Pad) Pad { return p }
"#;

fn go_compiler() -> String {
    std::env::var("COMPILER_GO_COMPILER").unwrap_or_else(|_| "go".to_owned())
}

fn go_available() -> bool {
    Command::new(go_compiler())
        .arg("version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
}

fn toolchain() -> ResolvedToolchain<'static> {
    ResolvedToolchain::from_version(
        NativeTool::GoCompiler,
        Path::new("/bin/true"),
        b"go-compiler-blank-fields-across-files",
    )
    .expect("toolchain resolution is deterministic for the pinned tool identity")
}

fn stage_flow_module() -> Result<(PathBuf, PathBuf, PathBuf), String> {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0);
    let sequence = FIXTURE_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let root = std::env::temp_dir().join(format!(
        "nudox-go-blank-fields-across-files-{nonce}-{}-{sequence}",
        std::process::id()
    ));
    fs::create_dir_all(&root).map_err(|error| error.to_string())?;
    fs::write(root.join("go.mod"), b"module flow.example/fixture\n\ngo 1.22\n")
        .map_err(|error| error.to_string())?;
    fs::write(root.join("blank.go"), BLANK_GO).map_err(|error| error.to_string())?;
    let use_path = root.join("use.go");
    fs::write(&use_path, USE_GO).map_err(|error| error.to_string())?;
    Ok((root.clone(), root.join("blank.go"), use_path))
}

fn assert_blank_field_identities(ir: &Ir) {
    let blanks: Vec<_> = ir
        .items()
        .filter(|item| item.kind() == EntityKind::Field && item.name() == b"_")
        .map(|item| item.version().identity())
        .collect();
    assert_eq!(
        blanks.len(),
        2,
        "both same-typed blank fields must lower across files"
    );
    assert_ne!(
        blanks[0], blanks[1],
        "same-typed blank fields must not share one declaration identity"
    );
}

#[test]
fn blank_fields_across_files_have_distinct_identities() {
    if !go_available() {
        use_case_support::skip("go", "blank-fields-across-files", "go-missing")
            .expect("skip report writes");
        return;
    }

    let (module_root, _blank_path, use_path) =
        stage_flow_module().expect("the two-file fixture stages");
    let source = fs::read(&use_path).expect("the use site source is readable");

    let oracle = GoOracle {
        output_limit: 32 * 1024 * 1024,
        timeout: Duration::from_secs(300),
    };
    let image_bytes = match oracle.authority_image_for_package(&use_path, &module_root) {
        Ok(bytes) => bytes,
        Err(OracleError::ToolingUnavailable { .. }) => {
            let _ = fs::remove_dir_all(&module_root);
            use_case_support::skip("go", "blank-fields-across-files", "go-missing")
                .expect("skip report writes");
            return;
        }
        Err(error) => {
            let _ = fs::remove_dir_all(&module_root);
            panic!("the authority accepts blank fields across files: {error:?}");
        }
    };

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
    let timer = CompileTimer::start();
    let compiled = compile_ir(
        request,
        CompileScratch {
            diagnostic_output: &mut diagnostic,
            native_work: &module_root,
        },
    );
    let elapsed = timer.elapsed();
    let _ = fs::remove_dir_all(&module_root);

    let ir = match compiled {
        Ok(output) => output.ir,
        Err(error) => panic!("blank fields across files must lower, not duplicate: {error:?}"),
    };

    assert_blank_field_identities(&ir);
    assert_eq!(
        count_named(&ir, EntityKind::Field, b"Keep"),
        1,
        "the named field stays one declaration"
    );
    assert_eq!(
        count_named(&ir, EntityKind::Function, b"Use"),
        1,
        "the cross-file use site keeps the package function"
    );

    use_case_support::finish("go", "blank-fields-across-files", elapsed, &ir)
        .expect("use-case report writes");
}
