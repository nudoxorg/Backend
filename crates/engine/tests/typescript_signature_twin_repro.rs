//! Focused repro for the `npm:es-toolkit@1.52.0` audit row.
//!
//! The selected file (`dist/function/partialRight.d.ts`) spells one overload
//! signature twice, word for word — lines 35 and 70 both declare
//! `partialRight<T1, R>(func: (arg1: T1) => R, arg1: T1): () => R`. The
//! checker reports every spelled declaration, so the lane must mint two
//! entities; the flattened identity build previously raised its
//! `DuplicateDeclarationIdentity` terminal because the twins share name,
//! owner, record, and every child shape. The count of already-committed
//! structurally identical siblings is authority-proven source order, so the
//! twin count now enters as the identity discriminator exactly when nonzero.

use std::{
    path::{Path, PathBuf},
    sync::atomic::{AtomicBool, AtomicUsize, Ordering},
    time::{Duration, Instant},
};

use backend_engine::driver::{
    CompileControl, CompileRequest, CompileScratch, NativeTool, ResolvedToolchain,
    SemanticAuthorityInput, ToolchainSelection, compile_semantic,
};
use backend_frontend_typescript::legacy::{Checker, Report};
use backend_semantic::vocabulary::{LanguageProfile, Stage, TypeScriptSource};

static CANCELLED: AtomicBool = AtomicBool::new(false);
static LEAK_SEQUENCE: AtomicUsize = AtomicUsize::new(0);

/// The twin shape: two word-for-word identical overload declarations plus one
/// structurally distinct overload of the same name.
const TWIN_SHAPE: &[u8] = b"
export declare function pick<T>(value: T): T;
export declare function pick<T>(value: T): T;
export declare function pick(value: string, other: number): string;
";

fn toolchain() -> ResolvedToolchain<'static> {
    ResolvedToolchain::from_version(
        NativeTool::TypeScriptCompiler,
        Path::new("/bin/true"),
        b"typescript-signature-twin-repro",
    )
    .expect("toolchain resolution is deterministic for the pinned tool identity")
}

fn leak(source: &[u8]) -> &'static [u8] {
    Box::leak(Vec::from(source).into_boxed_slice())
}

fn diagnostic() -> &'static mut [u8] {
    let _ = LEAK_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    Box::leak(Box::new([0_u8; 8192]))
}

/// Drives the full `LowerIr` semantic compile exactly as the audit does.
fn lower_semantic(source: &'static [u8], authority: &Report) -> Result<String, String> {
    let cancelled = AtomicBool::new(false);
    let mut fragment_output = vec![0_u8; 16 * 1024 * 1024];
    match backend_engine::driver::compile_semantic(
        CompileRequest {
            profile: LanguageProfile::TypeScript(TypeScriptSource::TypeScript),
            stage: Stage::LowerIr,
            source,
            declaration_scope: backend_engine::driver::DeclarationScope::fixture(),
            toolchain: ToolchainSelection::ResolvedNative(toolchain()),
            authority: SemanticAuthorityInput::TypeScript { report: authority },
            control: CompileControl {
                deadline: Instant::now() + Duration::from_secs(240),
                cancelled: &cancelled,
            },
        },
        CompileScratch {
            diagnostic_output: diagnostic(),
            native_work: Path::new("/tmp"),
        },
        backend_engine::driver::CompileOutput {
            fragment_output: &mut fragment_output,
        },
    ) {
        Ok(compiled) => {
            let picks = compiled
                .ir
                .items()
                .filter(|item| item.kind() == backend_semantic::ir::ItemKind::Function)
                .count();
            Ok(format!("OK ({picks} function rows)"))
        }
        Err(failure) => Ok(format!("{failure:?}")),
    }
}

/// An ambient witness report: the lowering drives from the written syntax,
/// the checker report only proves the bytes are a declaration file.
fn ambient_report(source: &[u8]) -> Report {
    let digest = backend_frontend_typescript::legacy::source_digest(source)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    Report {
        schema_version: 1,
        source_digest: digest,
        declaration_file: true,
        diagnostics: Box::new([]),
        declarations: Box::new([]),
        references: Box::new([]),
        narrowings: Box::new([]),
    }
}

#[test]
fn typescript_signature_twins_stay_distinct_declarations() {
    let source: &'static [u8] = leak(TWIN_SHAPE);
    let authority = ambient_report(TWIN_SHAPE);
    let outcome = lower_semantic(source, &authority).expect("compile drives");
    eprintln!("TWIN-SHAPE => {outcome}");
    assert!(
        outcome.starts_with("OK"),
        "word-for-word repeated overloads must lower, got {outcome}"
    );
    assert!(
        outcome.contains("3 function rows"),
        "both twin declarations and the distinct overload must mint rows, got {outcome}"
    );
}

/// The corpus-guarded half: the exact audited file, through the real checker,
/// must produce output instead of the duplicate terminal.
#[test]
fn typescript_es_toolkit_partial_right_lowers() {
    let Some(root) = std::env::var_os("NUDOX_TYPESCRIPT_CORPUS_DIR").map(PathBuf::from) else {
        eprintln!("NUDOX_TYPESCRIPT_CORPUS_DIR unset; skipping the corpus-guarded row");
        return;
    };
    let package = root.join("es-toolkit-1.52.0");
    let entry = package.join("dist/function/partialRight.d.ts");
    let Ok(bytes) = std::fs::read(&entry) else {
        panic!("corpus pinned entry must exist at {}", entry.display());
    };
    let twin_lines = bytes
        .split(|byte| *byte == b'\n')
        .filter(|line| {
            line.ends_with(b"(func: (arg1: T1) => R, arg1: T1): () => R;")
                && line.starts_with(b"declare function partialRight<T1, R>")
        })
        .count();
    assert_eq!(
        twin_lines, 2,
        "the pinned corpus entry must still carry the word-for-word twin overloads"
    );
    let report = Checker::default()
        .run_in_package(TypeScriptSource::TypeScript, &bytes, &package)
        .expect("the corpus entry checks");
    let outcome = lower_semantic(leak(&bytes), &report).expect("compile drives");
    eprintln!("ES-TOOLKIT partialRight.d.ts => {outcome}");
    assert!(
        outcome.starts_with("OK"),
        "the audited es-toolkit entry must lower, got {outcome}"
    );
}
