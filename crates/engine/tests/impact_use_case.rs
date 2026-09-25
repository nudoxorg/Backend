//! Multi-declaration impact and semantic-plane where-is flows for Rust and Python.
//!
//! Each language lowers a small caller/callee package, proves the call occurrence
//! resolves to the local callee, derives an impact set from incoming call links,
//! answers a where-is question by scanning stored names/docs/types (no LLM), and
//! records a token digest under `target/use-case-bench/`.

#![forbid(unsafe_code)]

#[path = "use_case_support/mod.rs"]
mod use_case_support;

use std::{
    fs,
    io,
    path::{Path, PathBuf},
    process::Command,
    sync::atomic::{AtomicBool, AtomicU64, Ordering},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use backend_engine::driver::{
    CompileControl, CompileFailure, CompileOutput, CompileRequest, CompileScratch, NativeTool,
    ResolvedToolchain, SemanticAuthorityInput, ToolchainSelection, compile_ir, compile_semantic,
};
use backend_frontend_rust::legacy::{
    RustAuthorityError, RustFeatureControl, RustProject, RustToolchain, SourceByteLimit,
};
use backend_semantic::ir::{DocFragment, EntityKind, Ir, LinkKind, LinkTarget};
use backend_semantic::vocabulary::{LanguageProfile, PythonVersion, RustEdition, Stage};
use serde::Deserialize;
use thiserror::Error;
use use_case_support::{CompileTimer, UseCaseReport, estimate_tokens, finish, name_digest, skip};

static FIXTURE_SEQUENCE: AtomicU64 = AtomicU64::new(0);

const RUST_SOURCE: &str = r#"use std::io;

/// Handles division errors from Result/Err paths.
fn handle_division_error(err: io::Error) -> u8 {
    0
}

fn divide(a: u8, b: u8) -> Result<u8, io::Error> {
    if b == 0 {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "zero"));
    }
    Ok(a / b)
}

fn caller() -> Result<u8, io::Error> {
    divide(1, 2)
}

fn unrelated() -> u8 {
    42
}
"#;

const PYTHON_SOURCE: &[u8] = b"class Result:
    def __init__(self, value: int) -> None:
        self.value = value

class Err(Exception):
    pass

def handle_division_error(err: Err) -> int:
    \"\"\"Explicit error-handling for Result/Err paths.\"\"\"
    return 0

def divide(a: int, b: int) -> Result:
    if b == 0:
        raise Err(\"division by zero\")
    return Result(a // b)

def callee(x: int) -> int:
    return x + 1

def caller() -> int:
    return callee(41)

def unrelated() -> int:
    return 0
";

const WHERE_IS_NEEDLES: &[&[u8]] = &[b"Result", b"Err"];
const USE_CASE: &str = "impact-flow";

#[derive(Debug, Error)]
enum TestError {
    #[error("system clock preceded its epoch: {0}")]
    Clock(std::time::SystemTimeError),
    #[error("no Rust compiler was available for the fixture")]
    MissingRustc,
    #[error("python3 is unavailable")]
    MissingPython,
    #[error("python tool failed: {0}")]
    Tool(#[source] std::io::Error),
    #[error("toolchain resolution failed")]
    Resolve,
    #[error(transparent)]
    Authority(#[from] RustAuthorityError),
    #[error("{operation} failed: {source}")]
    Io {
        operation: &'static str,
        source: std::io::Error,
    },
    #[error("compile failed: {0}")]
    Compile(&'static str),
    #[error("lane falsifier: {0}")]
    Falsified(&'static str),
    #[error("use-case report failed: {0}")]
    Report(#[source] io::Error),
}

fn failure_label(failure: &CompileFailure<'_>) -> &'static str {
    match failure {
        CompileFailure::Authority { .. } => "authority",
        CompileFailure::LoweringUnsupported { .. } => "lowering-unsupported",
        _ => "compile",
    }
}

fn contains_bytes(haystack: &[u8], needle: &[u8]) -> bool {
    if needle.is_empty() {
        return false;
    }
    haystack.windows(needle.len()).any(|window| window == needle)
}

fn entity_id(ir: &Ir, name: &[u8], kind: EntityKind) -> Result<backend_semantic::ir::EntityId, TestError> {
    ir.items()
        .find(|item| item.name() == name && item.kind() == kind)
        .map(|item| item.id())
        .ok_or_else(|| TestError::Falsified("expected entity absent"))
}

fn caller_links_to_local_callee(
    ir: &Ir,
    caller: backend_semantic::ir::EntityId,
    callee: backend_semantic::ir::EntityId,
) -> Result<(), TestError> {
    for (_, occurrence) in ir.link_occurrences_from(caller) {
        let Some(link) = ir.link(occurrence.link) else {
            continue;
        };
        if !matches!(link.kind, LinkKind::Calls | LinkKind::MethodCall) {
            continue;
        }
        match link.target {
            LinkTarget::Local(target) if target == callee => return Ok(()),
            LinkTarget::Local(_) => {}
            LinkTarget::External(_) => {
                return Err(TestError::Falsified(
                    "caller occurrence must not resolve to a foreign gap",
                ));
            }
        }
    }
    Err(TestError::Falsified(
        "caller occurrence must resolve to the local callee (edge not dropped)",
    ))
}

fn impact_owner_names(
    ir: &Ir,
    callee: backend_semantic::ir::EntityId,
) -> Vec<&[u8]> {
    let mut owners = Vec::new();
    for (_, link) in ir.links_to(callee) {
        if !matches!(link.kind, LinkKind::Calls | LinkKind::MethodCall) {
            continue;
        }
        if !matches!(link.target, LinkTarget::Local(target) if target == callee) {
            continue;
        }
        if let Some(owner) = ir.item(link.from) {
            owners.push(owner.name());
        }
    }
    owners
}

fn semantic_plane_where_is(ir: &Ir) -> Vec<backend_semantic::ir::EntityId> {
    let mut hits = Vec::new();
    for item in ir.items() {
        if item.kind() != EntityKind::Function {
            continue;
        }
        if declaration_matches_where_is(ir, item.id(), item.name()) {
            hits.push(item.id());
        }
    }
    hits
}

fn declaration_matches_where_is(
    ir: &Ir,
    entity: backend_semantic::ir::EntityId,
    name: &[u8],
) -> bool {
    if WHERE_IS_NEEDLES.iter().any(|needle| contains_bytes(name, needle)) {
        return true;
    }
    if let Some(docs) = ir.display_docs(entity) {
        let rendered = docs.to_string();
        if WHERE_IS_NEEDLES
            .iter()
            .any(|needle| contains_bytes(rendered.as_bytes(), needle))
        {
            return true;
        }
    }
    for fragment in ir.item(entity).map(|item| item.docs()).unwrap_or(&[]) {
        let bytes = match fragment {
            DocFragment::Text(text) | DocFragment::Code(text) => ir.text(*text),
            DocFragment::Link { label, .. } => ir.text(*label),
            DocFragment::SoftBreak | DocFragment::HardBreak => None,
        };
        if let Some(text) = bytes {
            if WHERE_IS_NEEDLES.iter().any(|needle| contains_bytes(text.as_bytes(), needle)) {
                return true;
            }
        }
    }
    if let Some(ty) = ir.item(entity).and_then(|item| item.semantic_type()) {
        if let Some(display) = ir.display_type(ty) {
            let rendered = display.to_string();
            if WHERE_IS_NEEDLES
                .iter()
                .any(|needle| contains_bytes(rendered.as_bytes(), needle))
            {
                return true;
            }
        }
    }
    if let Some(signature) = ir.signature(entity) {
        let rendered = signature.to_string();
        if WHERE_IS_NEEDLES
            .iter()
            .any(|needle| contains_bytes(rendered.as_bytes(), needle))
        {
            return true;
        }
    }
    false
}

fn assert_bench_report(
    language: &'static str,
    report: &UseCaseReport,
    ir: &Ir,
    callee_name: &[u8],
) -> Result<(), TestError> {
    let path = bench_report_path(language);
    if !path.is_file() {
        return Err(TestError::Falsified("bench report file was not written"));
    }
    let body = fs::read_to_string(&path).map_err(|source| TestError::Io {
        operation: "read bench report",
        source,
    })?;
    let stored: StoredBenchReport = serde_json::from_str(&body).map_err(|error| {
        TestError::Io {
            operation: "decode bench report",
            source: io::Error::new(io::ErrorKind::InvalidData, error),
        }
    })?;
    if stored.use_case != USE_CASE {
        return Err(TestError::Falsified("bench report use_case mismatch"));
    }
    if stored.estimated_tokens != estimate_tokens(stored.digest_bytes) {
        return Err(TestError::Falsified(
            "bench report token estimate must ceil(bytes/4)",
        ));
    }
    if stored.estimated_tokens != report.estimated_tokens {
        return Err(TestError::Falsified("bench report token estimate drift"));
    }
    let total_occurrences = ir.link_occurrences().len();
    if stored.occurrences != total_occurrences {
        return Err(TestError::Falsified(
            "bench report occurrence count must match lowered IR",
        ));
    }
    if report.occurrences != total_occurrences {
        return Err(TestError::Falsified(
            "bench report occurrence count must match finish()",
        ));
    }
    let digest = name_digest(ir);
    if digest.len() != stored.digest_bytes {
        return Err(TestError::Falsified("bench digest byte length mismatch"));
    }
    let callee_line = format!(
        "{:?}\t{}",
        EntityKind::Function,
        String::from_utf8_lossy(callee_name)
    );
    if !digest.lines().any(|line| line == callee_line) {
        return Err(TestError::Falsified(
            "bench digest must name the callee entity",
        ));
    }
    Ok(())
}

fn bench_report_path(language: &'static str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../target/use-case-bench")
        .join(format!("{language}-{USE_CASE}.json"))
}

#[derive(Debug, Deserialize)]
struct StoredBenchReport {
    use_case: String,
    digest_bytes: usize,
    estimated_tokens: usize,
    occurrences: usize,
}

fn resolve_rust_tool() -> Option<PathBuf> {
    if let Some(tool) = std::env::var_os("RUSTC") {
        let tool = PathBuf::from(tool);
        if tool.is_absolute() {
            return Some(tool);
        }
    }
    let path = std::env::var_os("PATH")?;
    for directory in std::env::split_paths(&path) {
        let candidate = directory.join("rustc");
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

fn rust_fixture_root() -> Result<PathBuf, TestError> {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(TestError::Clock)?
        .as_nanos();
    let sequence = FIXTURE_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let root = std::env::temp_dir().join(format!(
        "nudox-rust-impact-{nonce}-{}-{sequence}",
        std::process::id()
    ));
    fs::create_dir_all(root.join("src")).map_err(|source| TestError::Io {
        operation: "create fixture",
        source,
    })?;
    fs::write(
        root.join("Cargo.toml"),
        "[package]\nname = \"impact_fixture\"\nversion = \"0.1.0\"\nedition = \"2024\"\n",
    )
    .map_err(|source| TestError::Io {
        operation: "write manifest",
        source,
    })?;
    fs::write(root.join("src/lib.rs"), RUST_SOURCE.as_bytes()).map_err(|source| TestError::Io {
        operation: "write crate root",
        source,
    })?;
    Ok(root)
}

fn compile_rust_fixture(root: &PathBuf) -> Result<Ir, TestError> {
    let source_path = root.join("src/lib.rs");
    let source = fs::read(&source_path).map_err(|source| TestError::Io {
        operation: "read crate root",
        source,
    })?;
    let tool = resolve_rust_tool().ok_or(TestError::MissingRustc)?;
    let toolchain = RustToolchain::discover(&tool).map_err(|_| TestError::MissingRustc)?;
    let project =
        RustProject::open_with_source(root, &source_path, &toolchain, RustEdition::Rust2024)?;
    let resolved = ResolvedToolchain::from_version(
        NativeTool::Rustc,
        &tool,
        b"compiler-driver-rust-impact-flow",
    )
    .map_err(|_| TestError::MissingRustc)?;
    let cancelled = AtomicBool::new(false);
    let mut diagnostic = [0; 4096];
    compile_ir(
        CompileRequest {
            profile: LanguageProfile::Rust(RustEdition::Rust2024),
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
                deadline: Instant::now() + Duration::from_secs(120),
                cancelled: &cancelled,
            },
        },
        CompileScratch {
            diagnostic_output: &mut diagnostic,
            native_work: root,
        },
    )
    .map_err(|failure| TestError::Compile(failure_label(&failure)))
    .map(|output| output.ir)
}

fn python_toolchain() -> Result<ResolvedToolchain<'static>, TestError> {
    let executable = std::env::var_os("PATH")
        .and_then(|path| {
            std::env::split_paths(&path)
                .map(|directory| directory.join("python3"))
                .find(|candidate| candidate.is_file())
        })
        .ok_or(TestError::MissingPython)?;
    let version = Command::new(&executable)
        .arg("--version")
        .output()
        .map_err(TestError::Tool)?;
    let version_bytes = if version.stdout.is_empty() {
        version.stderr.as_slice()
    } else {
        version.stdout.as_slice()
    };
    let absolute = executable.canonicalize().map_err(TestError::Tool)?;
    let absolute: &'static Path = Box::leak(absolute.into_boxed_path());
    ResolvedToolchain::from_version(NativeTool::Python, absolute, version_bytes)
        .map_err(|_| TestError::Resolve)
}

fn compile_python_fixture(toolchain: ResolvedToolchain<'static>) -> Result<Ir, TestError> {
    let cancelled = AtomicBool::new(false);
    let mut diagnostic = [0_u8; 4096];
    let mut output = vec![0_u8; 8 * 1024 * 1024];
    let compiled = compile_semantic(
        CompileRequest {
            profile: LanguageProfile::Python(PythonVersion::Python314),
            stage: Stage::LowerIr,
            source: PYTHON_SOURCE,
            declaration_scope: backend_engine::driver::DeclarationScope::fixture(),
            toolchain: ToolchainSelection::ResolvedNative(toolchain),
            authority: SemanticAuthorityInput::None,
            control: CompileControl {
                deadline: Instant::now() + Duration::from_secs(30),
                cancelled: &cancelled,
            },
        },
        CompileScratch {
            diagnostic_output: &mut diagnostic,
            native_work: Path::new("/tmp"),
        },
        CompileOutput {
            fragment_output: &mut output,
        },
    )
    .map_err(|failure| TestError::Compile(failure_label(&failure)))?;
    Ok(compiled.ir)
}

fn assert_impact_flow(
    language: &'static str,
    ir: &Ir,
    callee_name: &[u8],
    caller_name: &[u8],
    unrelated_name: &[u8],
    error_handler_name: &[u8],
    compile: Duration,
) -> Result<(), TestError> {
    let callee = entity_id(ir, callee_name, EntityKind::Function)?;
    let caller = entity_id(ir, caller_name, EntityKind::Function)?;
    entity_id(ir, unrelated_name, EntityKind::Function)?;
    let error_handler = entity_id(ir, error_handler_name, EntityKind::Function)?;

    caller_links_to_local_callee(ir, caller, callee)?;

    let impact = impact_owner_names(ir, callee);
    if !impact.iter().any(|name| *name == caller_name) {
        return Err(TestError::Falsified(
            "impact set must include the caller declaration name",
        ));
    }
    if impact.iter().any(|name| *name == unrelated_name) {
        return Err(TestError::Falsified(
            "impact set must not include the unrelated declaration",
        ));
    }

    let where_is_hits = semantic_plane_where_is(ir);
    if !where_is_hits.iter().any(|entity| *entity == error_handler) {
        return Err(TestError::Falsified(
            "where-is scan must find the explicit error-handling declaration",
        ));
    }

    let report = finish(language, USE_CASE, compile, ir).map_err(TestError::Report)?;
    assert_bench_report(language, &report, ir, callee_name)?;
    Ok(())
}

#[test]
fn rust_impact_flow() -> Result<(), TestError> {
    if resolve_rust_tool().is_none() {
        skip("rust", USE_CASE, "rustc-missing").map_err(TestError::Report)?;
        return Ok(());
    }

    let root = rust_fixture_root()?;
    let timer = CompileTimer::start();
    let ir = compile_rust_fixture(&root)?;
    let elapsed = timer.elapsed();
    let _ = fs::remove_dir_all(&root);

    assert_impact_flow(
        "rust",
        &ir,
        b"divide",
        b"caller",
        b"unrelated",
        b"handle_division_error",
        elapsed,
    )
}

#[test]
fn python_impact_flow() -> Result<(), TestError> {
    let toolchain = match python_toolchain() {
        Err(TestError::MissingPython) => {
            skip("python", USE_CASE, "python3").map_err(TestError::Report)?;
            return Ok(());
        }
        Ok(toolchain) => toolchain,
        Err(error) => return Err(error),
    };

    let timer = CompileTimer::start();
    let ir = compile_python_fixture(toolchain)?;
    let elapsed = timer.elapsed();

    assert_impact_flow(
        "python",
        &ir,
        b"callee",
        b"caller",
        b"unrelated",
        b"handle_division_error",
        elapsed,
    )
}
