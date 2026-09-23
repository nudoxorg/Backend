//! Real-corpus render snapshots: one honest spot check per language.
//!
//! For each of the seven language lanes this harness takes 4-6 REAL corpus
//! packages (`NUDOX_*_CORPUS_DIR`), selects the audit's largest source file,
//! compiles it through the real authority (`backend_engine::driver::compile_ir`
//! with the same `SemanticAuthorityInput` arm the flow audit uses), and
//! renders the first 25 entities through the neutral renderer
//! (`Ir::signature`, the exact surface the grouped lane checks with
//! `RenderVerdict::Rendered`) plus the two prepared renderers in
//! `ir::semantic_render` (canonical type and semantic document).
//!
//! The per-language snapshots below pin the CURRENT honest state, gaps
//! included. Each known gap is named inline so a future fix moves its
//! snapshot visibly instead of silently. Rendering is exercised twice per
//! package and both passes must be byte-identical (the engine lane separately
//! proves recompilation byte-stability, e.g. `clang_lane`'s
//! `c_render_is_visibility_free_qualified_and_byte_stable`).
//!
//! Every lane reports a typed skip (test returns) when its corpus root or
//! native tool is not provisioned; absence never masquerades as a render
//! result.
//!
//! Set `NUDOX_RENDER_SNAPSHOT_PRINT=1` to print the freshly observed report
//! on mismatch (the test still fails, so a snapshot can never refresh
//! silently).

#![forbid(unsafe_code)]
// The pinned report grammar is fixed-format text (push_str of a formatted
// block is the clearest form), and the authority budgets are pinned
// audit-style whole-second constants copied from the flow audit.
#![allow(clippy::format_push_string, clippy::duration_suboptimal_units)]

use std::{
    fs::{self, DirEntry},
    io::Write as _,
    path::{Path, PathBuf},
    sync::atomic::{AtomicBool, AtomicU64, Ordering},
    time::{Duration, Instant},
};

use backend_engine::driver::{
    CompileControl, CompileRequest, CompileScratch, DeclarationScope, NativeTool,
    ResolvedToolchain, SemanticAuthorityInput, ToolchainSelection, compile_ir,
};
use backend_semantic::ir::{
    CanonicalTypeRenderError, CanonicalTypeRenderLimits, ItemKind, LanguageProfile,
    SemanticDocumentError, prepare_canonical_type, prepare_semantic_document,
};
use backend_semantic::vocabulary::{
    CSharpVersion, CxxStandard, GoVersion, JavaRelease, PythonVersion, RustEdition,
    TypeScriptSource,
};

/// Entities rendered per package (the audit-scale spot-check window).
const ENTITIES_PER_PACKAGE: usize = 25;

/// Canonical/document traversal bound; deep real types (TS object literals)
/// otherwise trip the limit before rendering anything useful.
/// One-thousand nested semantic type nodes; a compile-time-legal bound.
const RENDER_DEPTH: usize = 1024;

fn render_limits() -> CanonicalTypeRenderLimits {
    match core::num::NonZeroUsize::new(RENDER_DEPTH) {
        Some(depth) => CanonicalTypeRenderLimits::new(depth),
        None => unreachable!("RENDER_DEPTH is nonzero"),
    }
}

/// The single `panic!` is the test-failure signal for a lane-harness
/// fault (spawn/join), which has no meaningful report value.
#[allow(clippy::panic)]
/// Deep real sources can exceed libtest's default stack during native
/// authority drives (see the flow audit's row-worker stack budget); every
/// lane body runs on a dedicated thread with a documented budget instead.
fn on_big_stack<T: Send + 'static>(body: impl FnOnce() -> T + Send + 'static) -> T {
    const STACK_BYTES: usize = 256 * 1024 * 1024;
    match std::thread::Builder::new()
        .name("render-snapshot-corpus".to_owned())
        .stack_size(STACK_BYTES)
        .spawn(body)
        .map_err(|error| error.to_string())
        .and_then(|handle| handle.join().map_err(|_| "lane worker panicked".to_owned()))
    {
        Ok(value) => value,
        Err(message) => {
            // The lane harness itself failed; surface it as a failed report.
            panic!("render-snapshot lane worker failed: {message}");
        }
    }
}

/// Resolves `$ENV` to an existing directory, or `None` for a typed skip.
fn corpus_root(variable: &str) -> Option<PathBuf> {
    let root = PathBuf::from(std::env::var_os(variable)?);
    root.is_dir().then_some(root)
}

/// Resolves an absolute native tool from `$ENV` or `PATH`, or `None`.
fn native_tool(variable: &str, name: &str) -> Option<PathBuf> {
    if let Some(tool) = std::env::var_os(variable) {
        let tool = PathBuf::from(tool);
        if tool.is_absolute() && tool.is_file() {
            return tool.canonicalize().ok();
        }
    }
    let paths = std::env::var_os("PATH")?;
    std::env::split_paths(&paths)
        .map(|directory| directory.join(name))
        .find(|candidate| candidate.is_file())
        .and_then(|candidate| candidate.canonicalize().ok())
}

/// Deterministic temp fixture root for one staged package.
fn scratch_dir(label: &str) -> PathBuf {
    static SEQUENCE: AtomicU64 = AtomicU64::new(0);
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |duration| duration.as_nanos());
    let root = std::env::temp_dir().join(format!(
        "nudox-render-snapshot-{label}-{}-{}-{nonce}",
        std::process::id(),
        SEQUENCE.fetch_add(1, Ordering::Relaxed),
    ));
    let _ = fs::remove_dir_all(&root);
    root
}

/// The audit's source selection: the largest candidate file under `root`,
/// ties broken by the lexicographically smaller path.
fn largest_file(root: &Path, accepts: impl Fn(&Path) -> bool) -> Option<PathBuf> {
    let mut best: Option<(PathBuf, u64)> = None;
    let mut stack = vec![root.to_path_buf()];
    while let Some(directory) = stack.pop() {
        let entries = fs::read_dir(&directory).ok()?;
        let mut children: Vec<_> = entries.filter_map(Result::ok).collect();
        children.sort_by_key(DirEntry::path);
        for entry in children {
            let path = entry.path();
            let Ok(file_type) = entry.file_type() else {
                continue;
            };
            if file_type.is_dir() {
                stack.push(path);
            } else if file_type.is_file() && accepts(&path) {
                let len = entry.metadata().map_or(0, |meta| meta.len());
                let replace = match &best {
                    None => true,
                    Some((known, known_len)) => {
                        len > *known_len || (len == *known_len && path < *known)
                    }
                };
                if replace {
                    best = Some((path, len));
                }
            }
        }
    }
    best.map(|(path, _)| path)
}

fn has_extension(path: &Path, extensions: &[&str]) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| extensions.contains(&extension))
}

/// One classified neutral-render observation for a compiled package.
struct PackageReport {
    package: String,
    file: String,
    bytes: usize,
    lines: Vec<String>,
    entities: usize,
    signature_rendered: usize,
    signature_unavailable: usize,
    placeholder: usize,
    malformed: usize,
    canonical_ok: usize,
    canonical_err: usize,
    document_ok: usize,
    document_err: usize,
    compile_terminal: Option<String>,
}

/// Placeholder markers the legacy neutral formatter emits for reference rows
/// it cannot resolve. `?` followed by a lowercase letter is the closed
/// marker grammar (`?unresolved`, `?dangling`, `?dynamic`, `?oracle-gap`, ...);
/// a bare `?` (TS wildcard/optional) never matches.
fn placeholder_marker(text: &str) -> Option<String> {
    let bytes = text.as_bytes();
    for (index, byte) in bytes.iter().enumerate() {
        if *byte == b'?' && bytes.get(index + 1).is_some_and(u8::is_ascii_lowercase) {
            let end = bytes[index + 1..]
                .iter()
                .position(|byte| !(byte.is_ascii_alphanumeric() || *byte == b'-'))
                .map_or(bytes.len(), |offset| index + 1 + offset);
            return Some(text[index..end].to_owned());
        }
    }
    None
}

/// Renders one compiled package twice and proves both passes byte-identical.
fn render_package(
    package: &str,
    selected: &Path,
    corpus: &Path,
    ir: &backend_semantic::ir::Ir,
    profile: LanguageProfile,
) -> Result<PackageReport, String> {
    let first = render_pass(ir, profile);
    let second = render_pass(ir, profile);
    if first != second {
        return Err(format!(
            "{package}: neutral rendering is not byte-stable across two passes"
        ));
    }
    let mut report = PackageReport {
        package: package.to_owned(),
        file: selected
            .strip_prefix(corpus)
            .unwrap_or(selected)
            .to_string_lossy()
            .into_owned(),
        bytes: fs::metadata(selected)
            .map_or(0, |meta| usize::try_from(meta.len()).unwrap_or(usize::MAX)),
        lines: Vec::new(),
        entities: 0,
        signature_rendered: 0,
        signature_unavailable: 0,
        placeholder: 0,
        malformed: 0,
        canonical_ok: 0,
        canonical_err: 0,
        document_ok: 0,
        document_err: 0,
        compile_terminal: None,
    };
    report.lines = first.entity_lines;
    report.entities = first.counts.entities;
    report.signature_rendered = first.counts.signature_rendered;
    report.signature_unavailable = first.counts.signature_unavailable;
    report.placeholder = first.counts.placeholder;
    report.malformed = first.counts.malformed;
    report.canonical_ok = first.counts.canonical_ok;
    report.canonical_err = first.counts.canonical_err;
    report.document_ok = first.counts.document_ok;
    report.document_err = first.counts.document_err;
    Ok(report)
}

struct RenderPass {
    entity_lines: Vec<String>,
    counts: PassCounts,
}

impl PartialEq for RenderPass {
    fn eq(&self, other: &Self) -> bool {
        self.entity_lines == other.entity_lines && self.counts == other.counts
    }
}

#[derive(Clone, Copy, Default, PartialEq, Eq)]
struct PassCounts {
    entities: usize,
    signature_rendered: usize,
    signature_unavailable: usize,
    placeholder: usize,
    malformed: usize,
    canonical_ok: usize,
    canonical_err: usize,
    document_ok: usize,
    document_err: usize,
}

/// One full neutral render sweep over a compiled image.
#[allow(clippy::too_many_lines)]
fn render_pass(ir: &backend_semantic::ir::Ir, profile: LanguageProfile) -> RenderPass {
    let limits = render_limits();
    let mut counts = PassCounts::default();
    let mut entity_lines = Vec::new();
    for item in ir.items().take(ENTITIES_PER_PACKAGE) {
        counts.entities += 1;
        let kind = format!("{:?}", item.kind());
        let name = String::from_utf8_lossy(item.name()).into_owned();
        let signature = ir.signature(item.id()).map(|display| display.to_string());
        let mut markers = Vec::new();
        if let Some(ref text) = signature {
            counts.signature_rendered += 1;
            if let Some(marker) = placeholder_marker(text) {
                counts.placeholder += 1;
                markers.push(format!("placeholder:{marker}"));
            }
            if name.is_empty() {
                counts.malformed += 1;
                markers.push("malformed:empty-name".to_owned());
            }
            if item.kind() == ItemKind::Function && !text.contains('(') {
                counts.malformed += 1;
                markers.push("malformed:no-parameter-list".to_owned());
            }
        } else {
            counts.signature_unavailable += 1;
            markers.push("unavailable:signature".to_owned());
        }
        let canonical =
            item.semantic_type()
                .map(|ty| match prepare_canonical_type(ir, ty, limits) {
                    Ok(prepared) => {
                        let mut text = String::new();
                        match prepared.write_to(&mut text) {
                            Ok(()) => {
                                counts.canonical_ok += 1;
                                None
                            }
                            Err(error) => {
                                counts.canonical_err += 1;
                                Some(canonical_error_name(&error))
                            }
                        }
                    }
                    Err(error) => {
                        counts.canonical_err += 1;
                        Some(canonical_error_name(&error))
                    }
                });
        if let Some(Some(label)) = canonical {
            markers.push(format!("canonical:{label}"));
        }
        let mut document_text = String::new();
        let document = match prepare_semantic_document(profile, ir, item.id(), limits) {
            Ok(prepared) => match prepared.write_to(&mut document_text) {
                Ok(()) => {
                    counts.document_ok += 1;
                    None
                }
                Err(error) => Some(document_error_name(&error)),
            },
            Err(error) => Some(document_error_name(&error)),
        };
        if let Some(label) = document {
            markers.push(format!("document:{label}"));
        }
        let signature_text = signature.map_or_else(
            || "<unavailable>".to_owned(),
            |text| text.replace('\n', "\\n"),
        );
        let marker_text = if markers.is_empty() {
            String::new()
        } else {
            format!(" [{}]", markers.join(","))
        };
        entity_lines.push(format!("\t{kind} {name} :: {signature_text}{marker_text}"));
    }
    RenderPass {
        entity_lines,
        counts,
    }
}

fn canonical_error_name(error: &CanonicalTypeRenderError) -> &'static str {
    match error {
        CanonicalTypeRenderError::MissingRoot { .. } => "missing-root",
        CanonicalTypeRenderError::MissingReference { .. } => "missing-reference",
        CanonicalTypeRenderError::TraversalLimit { .. } => "traversal-limit",
        CanonicalTypeRenderError::OutputLengthOverflow { .. } => "length-overflow",
        CanonicalTypeRenderError::OutputTooSmall { .. } => "output-too-small",
        CanonicalTypeRenderError::PreparedLengthMismatch { .. } => "prepared-length-mismatch",
        CanonicalTypeRenderError::OutputWrite { .. } => "output-write",
        CanonicalTypeRenderError::OutputEncoding { .. } => "output-encoding",
        _ => "other",
    }
}

fn document_error_name(error: &SemanticDocumentError) -> &'static str {
    match error {
        SemanticDocumentError::MissingEntity { .. } => "missing-entity",
        SemanticDocumentError::ProfileUnavailable { .. } => "profile-unavailable",
        SemanticDocumentError::ProfileMismatch { .. } => "profile-mismatch",
        SemanticDocumentError::UnsupportedFact { .. } => "unsupported-fact",
        SemanticDocumentError::AuthorityMismatch { .. } => "authority-mismatch",
        SemanticDocumentError::MissingReference { .. } => "missing-reference",
        SemanticDocumentError::CanonicalType { .. } => "canonical-type",
        SemanticDocumentError::OutputTooSmall { .. } => "output-too-small",
        SemanticDocumentError::OutputLengthOverflow { .. } => "length-overflow",
        SemanticDocumentError::PreparedLengthMismatch { .. } => "prepared-length-mismatch",
        SemanticDocumentError::OutputWrite { .. } => "output-write",
        SemanticDocumentError::OutputEncoding { .. } => "output-encoding",
    }
}

/// Renders the pinned snapshot text for one language and fails (printing the
/// freshly observed text) on any drift.
fn check_snapshot(language: &str, actual: &str, expected: &str) -> Result<(), String> {
    if actual == expected {
        return Ok(());
    }
    if std::env::var_os("NUDOX_RENDER_SNAPSHOT_PRINT").is_some() {
        std::io::stderr()
            .write_all(
                format!("---- observed {language} snapshot ----\n{actual}---- end ----\n")
                    .as_bytes(),
            )
            .map_err(|error| error.to_string())?;
    }
    let first_difference = actual
        .lines()
        .zip(expected.lines())
        .position(|(actual, expected)| actual != expected)
        .unwrap_or_else(|| actual.lines().count().min(expected.lines().count()));
    Err(format!(
        "{language} render snapshot drifted at line {}:\n  expected: {:?}\n  observed: {:?}\n(set NUDOX_RENDER_SNAPSHOT_PRINT=1 to print the full observed snapshot)",
        first_difference + 1,
        expected.lines().nth(first_difference).unwrap_or(""),
        actual.lines().nth(first_difference).unwrap_or(""),
    ))
}

fn package_block(report: &PackageReport) -> String {
    if let Some(terminal) = &report.compile_terminal {
        return format!(
            "package {} file={} bytes={} terminal({terminal})\n",
            report.package, report.file, report.bytes
        );
    }
    let mut text = format!(
        "package {} file={} bytes={}\n",
        report.package, report.file, report.bytes
    );
    for line in &report.lines {
        text.push_str(line);
        text.push('\n');
    }
    text.push_str(&format!(
        "summary entities={} signature rendered={} unavailable={} placeholder={} malformed={} canonical ok={} err={} document ok={} err={}\n",
        report.entities,
        report.signature_rendered,
        report.signature_unavailable,
        report.placeholder,
        report.malformed,
        report.canonical_ok,
        report.canonical_err,
        report.document_ok,
        report.document_err,
    ));
    text
}

/// Shared compile plumbing: `Stage::LowerIr` through the real authority.
struct LaneCompile<'a> {
    source: &'a [u8],
    profile: LanguageProfile,
    authority: SemanticAuthorityInput<'a>,
    tool: &'a Path,
    tool_kind: NativeTool,
    work: &'a Path,
}

fn compile_lane(lane: &LaneCompile<'_>) -> Result<backend_semantic::ir::Ir, String> {
    let resolved =
        ResolvedToolchain::from_version(lane.tool_kind, lane.tool, b"render-snapshot-corpus")
            .map_err(|error| format!("toolchain: {error}"))?;
    let cancelled = AtomicBool::new(false);
    let mut diagnostic = vec![0_u8; 1 << 20];
    let outcome = compile_ir(
        CompileRequest {
            profile: lane.profile,
            stage: backend_semantic::vocabulary::Stage::LowerIr,
            source: lane.source,
            declaration_scope: DeclarationScope::fixture(),
            toolchain: ToolchainSelection::ResolvedNative(resolved),
            authority: lane.authority,
            control: CompileControl {
                deadline: Instant::now() + Duration::from_secs(5 * 60),
                cancelled: &cancelled,
            },
        },
        CompileScratch {
            diagnostic_output: &mut diagnostic,
            native_work: lane.work,
        },
    )
    .map(|compiled| compiled.ir)
    .map_err(|failure| format!("compile-terminal: {}", failure_label(&failure)))?;
    Ok(outcome)
}

fn failure_label(failure: &backend_engine::driver::CompileFailure<'_>) -> &'static str {
    use backend_engine::driver::CompileFailure;
    match failure {
        CompileFailure::SourceLength { .. } => "source-length",
        CompileFailure::UnsupportedStage { .. } => "unsupported-stage",
        CompileFailure::ToolchainSelectionMismatch { .. } => "toolchain-selection-mismatch",
        CompileFailure::ToolchainMismatch { .. } => "toolchain-mismatch",
        CompileFailure::NativeWork { .. } => "native-work",
        CompileFailure::NativeWorkCleanup { .. } => "native-work-cleanup",
        CompileFailure::ToolingUnavailable { .. } => "tooling-unavailable",
        CompileFailure::ToolStart { .. } => "tool-start",
        CompileFailure::MissingToolInput { .. } => "missing-tool-input",
        CompileFailure::MissingToolInputCleanup { .. } => "missing-tool-input-cleanup",
        CompileFailure::MissingToolDiagnostic { .. } => "missing-tool-diagnostic",
        CompileFailure::MissingToolDiagnosticCleanup { .. } => "missing-tool-diagnostic-cleanup",
        CompileFailure::ToolInput { .. } => "tool-input",
        CompileFailure::ToolInputCleanup { .. } => "tool-input-cleanup",
        CompileFailure::ToolTerminate { .. } => "tool-terminate",
        CompileFailure::ToolWait { .. } => "tool-wait",
        CompileFailure::ToolWaitCleanup { .. } => "tool-wait-cleanup",
        CompileFailure::ToolDiagnosticRead { .. } => "tool-diagnostic-read",
        CompileFailure::ToolDiagnosticReadCleanup { .. } => "tool-diagnostic-read-cleanup",
        CompileFailure::NativeWorkerPanic { .. } => "native-worker-panic",
        CompileFailure::Cancelled { .. } => "cancelled",
        CompileFailure::DeadlineExceeded { .. } => "deadline-exceeded",
        CompileFailure::DiagnosticLimit { .. } => "diagnostic-limit",
        CompileFailure::NativeRejected { .. } => "native-rejected",
        CompileFailure::Authority { .. } => "authority",
        CompileFailure::AuthorityInputRequired { .. } => "authority-input-required",
        CompileFailure::AuthorityInputProfileMismatch { .. } => "authority-input-profile-mismatch",
        CompileFailure::LoweringUnsupported { .. } => "lowering-unsupported",
        CompileFailure::ExtensionAtomUnbound { .. } => "extension-atom-unbound",
        CompileFailure::ExtensionTypeParametersUnbound { .. } => {
            "extension-type-parameters-unbound"
        }
        CompileFailure::ClangProjection { .. } => "clang-projection",
        CompileFailure::Build { .. } => "build",
        CompileFailure::Prepare { .. } => "prepare",
        CompileFailure::Write { .. } => "write",
        CompileFailure::Validate { .. } => "validate",
    }
}

// ---------------------------------------------------------------------------
// Rust lane: synthetic single-file Cargo crate through rust-analyzer HIR,
// mirroring `crates/engine/tests/rust_real_corpus_repro.rs`.
// ---------------------------------------------------------------------------

fn rust_lane(root: &Path) -> Result<String, String> {
    let mut text = String::new();
    for package in RUST_PACKAGES {
        let directory = root.join(package);
        let Some(selected) = largest_file(&directory, |path| has_extension(path, &["rs"])) else {
            text.push_str(&format!("package {package} missing\n"));
            continue;
        };
        let source = fs::read(&selected).map_err(|error| format!("{package}: {error}"))?;
        let report = compile_rust_package(package, root, &selected, &source)?;
        text.push_str(&package_block(&report));
    }
    Ok(format!("# rust render snapshot\n{text}"))
}

fn compile_rust_package(
    package: &str,
    corpus: &Path,
    selected: &Path,
    source: &[u8],
) -> Result<PackageReport, String> {
    let tool = native_tool("NUDOX_RUSTC", "rustc").ok_or("rustc unavailable")?;
    let toolchain = backend_frontend_rust::legacy::RustToolchain::discover(&tool)
        .map_err(|error| format!("rust toolchain: {error:?}"))?;
    let fixture = scratch_dir("rust");
    fs::create_dir_all(fixture.join("src")).map_err(|error| error.to_string())?;
    fs::write(
        fixture.join("Cargo.toml"),
        b"[package]\nname = \"fixture\"\nversion = \"0.1.0\"\nedition = \"2024\"\n",
    )
    .map_err(|error| error.to_string())?;
    let source_path = fixture.join("src/lib.rs");
    fs::write(&source_path, source).map_err(|error| error.to_string())?;
    let outcome = (|| {
        let project = backend_frontend_rust::legacy::RustProject::open_with_source(
            &fixture,
            &source_path,
            &toolchain,
            RustEdition::Rust2024,
        )
        .map_err(|error| format!("rust-project: {error:?}"))?;
        let ir = compile_lane(&LaneCompile {
            source,
            profile: LanguageProfile::Rust(RustEdition::Rust2024),
            authority: SemanticAuthorityInput::Rust {
                project: &project,
                maximum_source_bytes: backend_frontend_rust::legacy::SourceByteLimit(
                    u32::try_from(source.len()).unwrap_or(u32::MAX),
                ),
                features: backend_frontend_rust::legacy::RustFeatureControl::default(),
            },
            tool: &tool,
            tool_kind: NativeTool::Rustc,
            work: &fixture,
        })?;
        render_package(
            package,
            selected,
            corpus,
            &ir,
            LanguageProfile::Rust(RustEdition::Rust2024),
        )
    })();
    let _ = fs::remove_dir_all(&fixture);
    outcome
}

// ---------------------------------------------------------------------------
// Go lane: staged real module through the go oracle, mirroring
// `crates/engine/tests/zz_go_repro.rs` (which proves these modules lower
// without corpus requirement completion).
// ---------------------------------------------------------------------------

fn go_lane(root: &Path) -> Result<String, String> {
    let mut text = String::new();
    for package in GO_PACKAGES {
        let directory = root.join(package);
        let Some(selected) = largest_file(&directory, |path| {
            has_extension(path, &["go"])
                && !path
                    .file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| name.ends_with("_test.go"))
        }) else {
            text.push_str(&format!("package {package} missing\n"));
            continue;
        };
        let source = fs::read(&selected).map_err(|error| format!("{package}: {error}"))?;
        let report = compile_go_package(package, root, &selected, &source)?;
        text.push_str(&package_block(&report));
    }
    Ok(format!("# go render snapshot\n{text}"))
}

fn compile_go_package(
    package: &str,
    corpus: &Path,
    selected: &Path,
    source: &[u8],
) -> Result<PackageReport, String> {
    let tool = native_tool("NUDOX_GO", "go").ok_or("go unavailable")?;
    let fixture = scratch_dir("go");
    fs::create_dir_all(&fixture).map_err(|error| error.to_string())?;
    let outcome = (|| {
        let staged = backend_frontend_go::legacy::stage_module(&fixture, selected, source)
            .map_err(|error| format!("stage: {error:?}"))?;
        let oracle = backend_frontend_go::legacy::GoOracle {
            output_limit: 32 * 1024 * 1024,
            timeout: Duration::from_secs(120),
        };
        let image = oracle
            .authority_image_for_package(&staged.source, &staged.root)
            .map_err(|error| format!("go-oracle: {error}"))?;
        let work = fixture.join("work");
        fs::create_dir_all(&work).map_err(|error| error.to_string())?;
        let ir = compile_lane(&LaneCompile {
            source,
            profile: LanguageProfile::Go(GoVersion::Go125),
            authority: SemanticAuthorityInput::Go { image: &image },
            tool: &tool,
            tool_kind: NativeTool::GoCompiler,
            work: &work,
        })?;
        render_package(
            package,
            selected,
            corpus,
            &ir,
            LanguageProfile::Go(GoVersion::Go125),
        )
    })();
    let _ = fs::remove_dir_all(&fixture);
    outcome
}

// ---------------------------------------------------------------------------
// TypeScript lane: package-checked report through the vendored TSZ checker,
// mirroring `crates/engine/tests/zz_ts_repro.rs` / the real audit's budgets.
// ---------------------------------------------------------------------------

fn typescript_lane(root: &Path) -> Result<String, String> {
    let mut text = String::new();
    for package in TYPESCRIPT_PACKAGES {
        let directory = root.join(package);
        let Some(selected) = largest_file(&directory, |path| has_extension(path, &["ts", "tsx"]))
        else {
            text.push_str(&format!("package {package} missing\n"));
            continue;
        };
        let source = fs::read(&selected).map_err(|error| format!("{package}: {error}"))?;
        let report = compile_typescript_package(package, root, &selected, &source)?;
        text.push_str(&package_block(&report));
    }
    Ok(format!("# typescript render snapshot\n{text}"))
}

fn compile_typescript_package(
    package: &str,
    corpus: &Path,
    selected: &Path,
    source: &[u8],
) -> Result<PackageReport, String> {
    let tool = native_tool("NUDOX_TSC", "tsc").ok_or("tsc unavailable")?;
    let profile = TypeScriptSource::TypeScript;
    let report = backend_frontend_typescript::legacy::Checker {
        output_limit: 64 * 1024 * 1024,
        timeout: Duration::from_secs(120),
    }
    .run_in_package(
        profile,
        source,
        match selected.parent() {
            Some(parent) => parent,
            None => return Err("selected file has no parent directory".to_owned()),
        },
    )
    .map_err(|error| format!("typescript-checker: {error:?}"))?;
    let work = scratch_dir("typescript");
    fs::create_dir_all(&work).map_err(|error| error.to_string())?;
    let outcome = (|| {
        let ir = compile_lane(&LaneCompile {
            source,
            profile: LanguageProfile::TypeScript(profile),
            authority: SemanticAuthorityInput::TypeScript { report: &report },
            tool: &tool,
            tool_kind: NativeTool::TypeScriptCompiler,
            work: &work,
        })?;
        render_package(
            package,
            selected,
            corpus,
            &ir,
            LanguageProfile::TypeScript(profile),
        )
    })();
    let _ = fs::remove_dir_all(&work);
    outcome
}

// ---------------------------------------------------------------------------
// Python lane: pyrefly-checked report, mirroring the real audit's Python arm.
// ---------------------------------------------------------------------------

fn python_lane(root: &Path) -> Result<String, String> {
    let checker = backend_frontend_python::legacy::Pyrefly::from_env();
    if !checker.is_available() {
        return Err("pyrefly unavailable".to_owned());
    }
    let mut text = String::new();
    for package in PYTHON_PACKAGES {
        let directory = root.join(package);
        let Some(selected) = largest_file(&directory, |path| has_extension(path, &["py"])) else {
            text.push_str(&format!("package {package} missing\n"));
            continue;
        };
        let source = fs::read(&selected).map_err(|error| format!("{package}: {error}"))?;
        let report = compile_python_package(package, root, &selected, &source, &checker)?;
        text.push_str(&package_block(&report));
    }
    Ok(format!("# python render snapshot\n{text}"))
}

fn compile_python_package(
    package: &str,
    corpus: &Path,
    selected: &Path,
    source: &[u8],
    checker: &backend_frontend_python::legacy::Pyrefly,
) -> Result<PackageReport, String> {
    let tool = native_tool("NUDOX_PYTHON", "python3").ok_or("python unavailable")?;
    let profile = PythonVersion::Python314;
    let facts = backend_frontend_python::legacy::extract(source, profile)
        .map_err(|error| format!("python-extract: {error:?}"))?;
    let report_authority = checker
        .analyze(source, profile, &facts)
        .map_err(|error| format!("pyrefly: {error:?}"))?;
    let work = scratch_dir("python");
    fs::create_dir_all(&work).map_err(|error| error.to_string())?;
    let outcome = (|| {
        let ir = compile_lane(&LaneCompile {
            source,
            profile: LanguageProfile::Python(profile),
            authority: SemanticAuthorityInput::Python {
                report: &report_authority,
            },
            tool: &tool,
            tool_kind: NativeTool::Python,
            work: &work,
        })?;
        render_package(
            package,
            selected,
            corpus,
            &ir,
            LanguageProfile::Python(profile),
        )
    })();
    let _ = fs::remove_dir_all(&work);
    outcome
}

// ---------------------------------------------------------------------------
// Java lane: doclet harness image over the whole package with the sibling
// sourcepath, mirroring `crates/engine/tests/java_real_package_repro.rs`.
// ---------------------------------------------------------------------------

fn java_package_dir(root: &Path, coordinate: &str) -> Option<PathBuf> {
    let raw = coordinate.strip_prefix("maven:")?;
    let (name, version) = raw.rsplit_once('@')?;
    let mut path = root.to_path_buf();
    for component in name.split(':') {
        for part in component.split('.') {
            path.push(part);
        }
    }
    path.push(version);
    path.is_dir().then_some(path)
}

fn java_sourcepath_roots(package_root: &Path) -> Vec<PathBuf> {
    let mut entries = vec![package_root.to_path_buf()];
    let Some(group_dir) = package_root.parent().and_then(Path::parent) else {
        return entries;
    };
    let Ok(artifacts) = fs::read_dir(group_dir) else {
        return entries;
    };
    for artifact in artifacts.flatten() {
        let artifact_path = artifact.path();
        if !artifact_path.is_dir() {
            continue;
        }
        let Ok(versions) = fs::read_dir(&artifact_path) else {
            continue;
        };
        for version in versions.flatten() {
            let candidate = version.path();
            if candidate == package_root || !candidate.is_dir() {
                continue;
            }
            if candidate.join("module-info.java").is_file() {
                continue;
            }
            entries.push(candidate);
        }
    }
    entries.sort();
    entries.dedup();
    entries
}

fn java_lane(root: &Path) -> Result<String, String> {
    let jdk = backend_frontend_java::legacy::harness::JdkToolchain::from_env()
        .map_err(|error| format!("jdk: {error}"))?;
    let mut harness = backend_frontend_java::legacy::harness::Harness::new()
        .map_err(|error| format!("harness: {error}"))?;
    harness
        .prepare(&jdk)
        .map_err(|error| format!("harness-prepare: {error}"))?;
    let mut text = String::new();
    for coordinate in JAVA_PACKAGES {
        let Some(directory) = java_package_dir(root, coordinate) else {
            text.push_str(&format!("package {coordinate} missing\n"));
            continue;
        };
        let mut files = Vec::new();
        collect_java_sources(&directory, &mut files).map_err(|error| error.to_string())?;
        files.sort();
        let Some(selected) = files
            .iter()
            .max_by_key(|path| fs::metadata(path).map_or(0, |meta| meta.len()))
            .cloned()
        else {
            text.push_str(&format!("package {coordinate} missing\n"));
            continue;
        };
        let source = fs::read(&selected).map_err(|error| format!("{coordinate}: {error}"))?;
        let report = compile_java_package(
            coordinate, root, &directory, &selected, &source, &jdk, &harness,
        )?;
        text.push_str(&package_block(&report));
    }
    Ok(format!("# java render snapshot\n{text}"))
}

fn collect_java_sources(dir: &Path, out: &mut Vec<PathBuf>) -> std::io::Result<()> {
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        if entry.file_type()?.is_dir() {
            collect_java_sources(&path, out)?;
        } else if path.extension().and_then(|ext| ext.to_str()) == Some("java") {
            out.push(path);
        }
    }
    Ok(())
}

#[allow(clippy::too_many_lines)]
fn compile_java_package(
    coordinate: &str,
    corpus: &Path,
    package_root: &Path,
    selected: &Path,
    source: &[u8],
    jdk: &backend_frontend_java::legacy::harness::JdkToolchain<'_>,
    harness: &backend_frontend_java::legacy::harness::Harness,
) -> Result<PackageReport, String> {
    let mut names: Vec<PathBuf> = Vec::new();
    let mut buffers: Vec<Vec<u8>> = Vec::new();
    let selected_relative = selected
        .strip_prefix(package_root)
        .map_err(|error| error.to_string())?;
    names.push(selected_relative.to_path_buf());
    buffers.push(source.to_vec());
    let mut files = Vec::new();
    collect_java_sources(package_root, &mut files).map_err(|error| error.to_string())?;
    files.sort();
    for file in &files {
        if file == selected {
            continue;
        }
        let relative = file
            .strip_prefix(package_root)
            .map_err(|error| error.to_string())?;
        names.push(relative.to_path_buf());
        buffers.push(fs::read(file).map_err(|error| error.to_string())?);
    }
    let sources: Vec<backend_frontend_java::legacy::harness::JavaSource<'_>> = names
        .iter()
        .zip(buffers.iter())
        .map(
            |(name, bytes)| backend_frontend_java::legacy::harness::JavaSource {
                name: name.as_path(),
                bytes,
            },
        )
        .collect();
    let request = backend_frontend_java::legacy::harness::HarnessRequest {
        sources: &sources,
        classpath: &[],
        release: backend_frontend_java::legacy::JavaRelease::Java21,
    };
    let roots = java_sourcepath_roots(package_root);
    let roots: Vec<&Path> = roots.iter().map(PathBuf::as_path).collect();
    let mut image = Vec::with_capacity(64 * 1024);
    harness
        .image_with_sourcepath(jdk, request, &roots, &mut image)
        .map_err(|error| format!("java-harness: {error}"))?;
    let work = scratch_dir("java");
    fs::create_dir_all(&work).map_err(|error| error.to_string())?;
    let javac = native_tool("NUDOX_JAVAC", "javac");
    let tool: PathBuf = match javac {
        Some(path) => path,
        None => PathBuf::from("/usr/bin/true"),
    };
    let outcome = (|| {
        let ir = compile_lane(&LaneCompile {
            source,
            profile: LanguageProfile::Java(JavaRelease::Java21),
            authority: SemanticAuthorityInput::Java { image: &image },
            tool: &tool,
            tool_kind: NativeTool::JavaCompiler,
            work: &work,
        })?;
        render_package(
            coordinate,
            selected,
            corpus,
            &ir,
            LanguageProfile::Java(JavaRelease::Java21),
        )
    })();
    let _ = fs::remove_dir_all(&work);
    outcome
}

// ---------------------------------------------------------------------------
// C# lane: the vendored helper oracle published once per process and driven
// in source mode, mirroring `crates/engine/tests/csharp_identity_regressions.rs`.
// ---------------------------------------------------------------------------

fn dotnet_tool() -> Option<PathBuf> {
    native_tool("NUDOX_DOTNET", "dotnet")
}

/// Publishes the vendored helper once per process; `None` is an environment
/// fact (the publish failed) the caller reports as a skip.
///
/// The build is the flow harness's locked-restore flow
/// (`compiler_corpus/authority.rs` `CSharpAuthorityProvider::new`): a
/// locked restore pins the Roslyn closure from `packages.lock.json`, and
/// the no-restore publish reuses exactly those assets. The committed `obj/`
/// build assets are not part of the source tree, so publishing without a
/// prior restore has no assets to consume.
fn published_oracle(dotnet: &Path) -> Option<PathBuf> {
    static ORACLE: std::sync::OnceLock<Option<PathBuf>> = std::sync::OnceLock::new();
    ORACLE
        .get_or_init(|| {
            let helper = Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../frontends/csharp/src/legacy/helper");
            let publish = scratch_dir("csharp-publish").join("publish");
            fs::create_dir_all(&publish).ok()?;
            let restored = std::process::Command::new(dotnet)
                .args(["restore", "oracle.csproj", "--locked-mode", "--nologo"])
                .current_dir(&helper)
                .status()
                .ok()?;
            if !restored.success() {
                return None;
            }
            let status = std::process::Command::new(dotnet)
                .args([
                    "publish",
                    "oracle.csproj",
                    "-c",
                    "Release",
                    "--nologo",
                    "--no-restore",
                    "-o",
                ])
                .arg(&publish)
                .current_dir(&helper)
                .status()
                .ok()?;
            (status.success() && publish.join("oracle.dll").is_file())
                .then_some(publish.join("oracle.dll"))
        })
        .clone()
}

fn csharp_lane(root: &Path) -> Result<String, String> {
    let tool = dotnet_tool().ok_or("dotnet unavailable")?;
    let oracle = published_oracle(&tool).ok_or("csharp helper publish unavailable")?;
    let mut text = String::new();
    for package in CSHARP_PACKAGES {
        let directory = root.join(package);
        let Some(selected) = largest_file(&directory, |path| has_extension(path, &["cs"])) else {
            text.push_str(&format!("package {package} missing\n"));
            continue;
        };
        let source = fs::read(&selected).map_err(|error| format!("{package}: {error}"))?;
        let report = compile_csharp_package(package, root, &selected, &source, &tool, &oracle)?;
        text.push_str(&package_block(&report));
    }
    Ok(format!("# csharp render snapshot\n{text}"))
}

fn compile_csharp_package(
    package: &str,
    corpus: &Path,
    selected: &Path,
    source: &[u8],
    dotnet: &Path,
    oracle: &Path,
) -> Result<PackageReport, String> {
    let fixture = scratch_dir("csharp");
    fs::create_dir_all(fixture.join("root")).map_err(|error| error.to_string())?;
    let outcome = (|| {
        let binding = fixture.join("root").join("Package.cs");
        fs::write(&binding, source).map_err(|error| error.to_string())?;
        let image_path = fixture.join("authority.image");
        let status = std::process::Command::new(dotnet)
            .arg("exec")
            .arg(oracle)
            .arg("--mode")
            .arg("source")
            .arg("--assembly-name")
            .arg("IdentityProbe")
            .arg("--authority-image")
            .arg("--source-binding")
            .arg(&binding)
            .arg("--out")
            .arg(&image_path)
            .arg("--root")
            .arg(fixture.join("root"))
            .status()
            .map_err(|error| error.to_string())?;
        if !status.success() {
            return Err("csharp-oracle exit".to_owned());
        }
        let image = fs::read(&image_path).map_err(|error| error.to_string())?;
        let ir = compile_lane(&LaneCompile {
            source,
            profile: LanguageProfile::CSharp(CSharpVersion::CSharp14),
            authority: SemanticAuthorityInput::CSharp { image: &image },
            tool: dotnet,
            tool_kind: NativeTool::CSharpCompiler,
            work: &fixture,
        })?;
        render_package(
            package,
            selected,
            corpus,
            &ir,
            LanguageProfile::CSharp(CSharpVersion::CSharp14),
        )
    })();
    let _ = fs::remove_dir_all(&fixture);
    outcome
}

// ---------------------------------------------------------------------------
// Clang lane: whole-project `ClangProject` authority at the audit's profile
// (Cxx23 for every clang row), mirroring the real audit's clang arm.
// ---------------------------------------------------------------------------

fn clang_lane(root: &Path) -> Result<String, String> {
    let tool = native_tool("NUDOX_CLANG", "clang").ok_or("clang unavailable")?;
    let mut text = String::new();
    for package in CLANG_PACKAGES {
        let directory = root.join(package);
        let Some(selected) = largest_file(&directory, |path| {
            has_extension(path, &["cpp", "cc", "cxx", "c"])
        }) else {
            text.push_str(&format!("package {package} missing\n"));
            continue;
        };
        let source = fs::read(&selected).map_err(|error| format!("{package}: {error}"))?;
        let report = compile_clang_package(package, root, &directory, &selected, &source, &tool)?;
        text.push_str(&package_block(&report));
    }
    Ok(format!("# clang render snapshot\n{text}"))
}

fn compile_clang_package(
    package: &str,
    corpus: &Path,
    package_root: &Path,
    selected: &Path,
    source: &[u8],
    tool: &Path,
) -> Result<PackageReport, String> {
    let project = backend_frontend_clang::ClangProject::open(package_root, selected)
        .map_err(|error| format!("clang-project: {error}"))?;
    let work = scratch_dir("clang");
    fs::create_dir_all(&work).map_err(|error| error.to_string())?;
    let outcome = (|| {
        let ir = compile_lane(&LaneCompile {
            source,
            profile: LanguageProfile::Cxx(CxxStandard::Cxx23),
            authority: SemanticAuthorityInput::Clang { project: &project },
            tool,
            tool_kind: NativeTool::Clang,
            work: &work,
        })?;
        render_package(
            package,
            selected,
            corpus,
            &ir,
            LanguageProfile::Cxx(CxxStandard::Cxx23),
        )
    })();
    let _ = fs::remove_dir_all(&work);
    outcome
}

// ---------------------------------------------------------------------------
// Package selections (all verified present in the pinned corpus roots).
// ---------------------------------------------------------------------------

const RUST_PACKAGES: &[&str] = &[
    "ahash-0.8.11",
    "bitflags-2.6.0",
    "itoa-1.0.14",
    "log-0.4.22",
    "memchr-2.7.4",
];

const GO_PACKAGES: &[&str] = &[
    "github.com/google/uuid@v1.6.0",
    "github.com/go-chi/chi/v5@v5.0.12",
    "github.com/google/go-cmp@v0.6.0",
    "gopkg.in/yaml.v3@v3.0.1",
    "golang.org/x/mod@v0.17.0",
];

const TYPESCRIPT_PACKAGES: &[&str] = &[
    "axios-1.7.9",
    "chalk-5.3.0",
    "commander-13.1.0",
    "detect-libc-2.1.2",
    "tslib-2.8.1",
];

const PYTHON_PACKAGES: &[&str] = &[
    "PyYAML-6.0.2",
    "anyio-4.14.2",
    "appdirs-1.4.4",
    "attrs-25.3.0",
    "packaging-25.0",
];

const JAVA_PACKAGES: &[&str] = &[
    "maven:commons-io:commons-io@2.15.1",
    "maven:com.google.code.gson:gson@2.10.1",
    "maven:commons-codec:commons-codec@1.16.0",
    "maven:org.opentest4j:opentest4j@1.3.0",
    "maven:org.ow2.asm:asm@9.6",
];

const CSHARP_PACKAGES: &[&str] = &[
    "morelinq.source.moreenumerable.batch/1.0.1",
    "morelinq.source.moreenumerable.assertcount/1.0.2",
    "morelinq.source.moreenumerable.groupadjacent/1.0.1",
    "morelinq.source.moreenumerable.todelimitedstring/1.1.2",
    "morelinq.source.moreenumerable.consume/1.0.1",
];

const CLANG_PACKAGES: &[&str] = &["cJSON", "Unity", "STC", "brotli", "pugixml"];

// ---------------------------------------------------------------------------
// Snapshot entry points.
// ---------------------------------------------------------------------------

macro_rules! language_snapshot {
    (
        $test:ident,
        $language:literal,
        $variable:literal,
        $lane:ident,
        $expected:expr $(,)?
    ) => {
        #[test]
        fn $test() {
            let Some(root) = corpus_root($variable) else {
                eprintln!(
                    "{} unset; typed skip: no {} render snapshot on this host",
                    $variable, $language
                );
                return;
            };
            let observed = on_big_stack(move || ($lane)(&root));
            match observed {
                Ok(report) => check_snapshot($language, &report, $expected)
                    .unwrap_or_else(|message| panic!("{message}")),
                Err(message) => panic!("{} render lane failed: {message}", $language),
            }
        }
    };
}

language_snapshot!(
    render_snapshot_rust,
    "rust",
    "NUDOX_RUST_CORPUS_DIR",
    rust_lane,
    EXPECTED_RUST,
);

language_snapshot!(
    render_snapshot_go,
    "go",
    "NUDOX_GO_CORPUS_DIR",
    go_lane,
    EXPECTED_GO,
);

language_snapshot!(
    render_snapshot_typescript,
    "typescript",
    "NUDOX_TYPESCRIPT_CORPUS_DIR",
    typescript_lane,
    EXPECTED_TYPESCRIPT,
);

language_snapshot!(
    render_snapshot_python,
    "python",
    "NUDOX_PYTHON_CORPUS_DIR",
    python_lane,
    EXPECTED_PYTHON,
);

language_snapshot!(
    render_snapshot_java,
    "java",
    "NUDOX_JAVA_CORPUS_DIR",
    java_lane,
    EXPECTED_JAVA,
);

language_snapshot!(
    render_snapshot_csharp,
    "csharp",
    "NUDOX_CSHARP_CORPUS_DIR",
    csharp_lane,
    EXPECTED_CSHARP,
);

language_snapshot!(
    render_snapshot_clang,
    "clang",
    "NUDOX_CLANG_CORPUS_DIR",
    clang_lane,
    EXPECTED_CLANG,
);

// # KNOWN GAPS (rust, pinned 2026-09):
// # - Record/Enum/Trait signatures render the head only: the legacy
// #   neutral formatter's record arm (crates/semantic/src/ir/render.rs:148-154)
// #   renders no member body even though the IR carries members().
// # - Foreign ADTs rust-analyzer resolves render by their written
// #   spelling (`Option<One>`, `AtomicUsize`); a foreign position with no
// #   written spelling stays `?oracle-gap`, and macro rows `?unannotated`
// #   (upstream type-row availability, not the renderer's choice).
#[rustfmt::skip]
const EXPECTED_RUST: &str = r"# rust render snapshot
package ahash-0.8.11 file=ahash-0.8.11/src/random_state.rs bytes=18534
	Trait RandomSource :: pub trait RandomSource
	Record DefaultRandomSource :: struct DefaultRandomSource
	Record RandomState :: pub struct RandomState
	Constant PI :: pub(crate) const PI: [u64; 4]
	Constant PI2 :: pub(crate) const PI2: [u64; 4]
	Parameter self :: self: &Self
	Parameter gen_hasher_seed :: gen_hasher_seed: native-uint
	Function gen_hasher_seed :: fn gen_hasher_seed(self: &Self) -> native-uint
	Field counter :: counter: AtomicUsize
	Implementation DefaultRandomSource :: impl DefaultRandomSource = DefaultRandomSource
	Parameter new :: new: DefaultRandomSource
	Function new :: fn new() -> DefaultRandomSource
	Field k0 :: pub(crate) k0: u64
	Field k1 :: pub(crate) k1: u64
	Field k2 :: pub(crate) k2: u64
	Field k3 :: pub(crate) k3: u64
	Implementation RandomState :: impl RandomState = RandomState
	Parameter self :: self: &RandomState
	Parameter f :: f: &mut Formatter
	Parameter fmt :: fmt: Result
	Function fmt :: fn fmt(self: &RandomState, f: &mut Formatter) -> Result
	Parameter new :: pub new: RandomState
	Function new :: pub fn new() -> RandomState
	Parameter k0 :: k0: u64
	Parameter k1 :: k1: u64
summary entities=25 signature rendered=25 unavailable=0 placeholder=0 malformed=0 canonical ok=25 err=0 document ok=25 err=0
package bitflags-2.6.0 file=bitflags-2.6.0/src/lib.rs bytes=27206
	Module iter :: pub mod iter
	Module parser :: pub mod parser
	Module traits :: mod traits
	Module __private :: pub mod __private
	Module public :: mod public
	Module internal :: mod internal
	Module external :: mod external
	Macro bitflags :: macro bitflags: ?unannotated [placeholder:?unannotated]
	Macro __impl_bitflags :: macro __impl_bitflags: ?unannotated [placeholder:?unannotated]
	Macro __bitflags_expr_safe_attrs :: macro __bitflags_expr_safe_attrs: ?unannotated [placeholder:?unannotated]
	Macro __bitflags_flag :: macro __bitflags_flag: ?unannotated [placeholder:?unannotated]
summary entities=11 signature rendered=11 unavailable=0 placeholder=4 malformed=0 canonical ok=11 err=0 document ok=11 err=0
package itoa-1.0.14 file=itoa-1.0.14/src/lib.rs bytes=11578
	Module udiv128 :: mod udiv128
	Record Buffer :: pub struct Buffer
	Trait Integer :: pub trait Integer
	Module private :: mod private
	Trait Sealed :: pub trait Sealed
	Parameter MaybeUninit<u8> :: MaybeUninit<u8>: MaybeUninit<u8>
	Field bytes :: bytes: [MaybeUninit<u8>; i128::MAX_STR_LEN]
	Implementation Buffer :: impl Buffer = Buffer
	Parameter default :: default: Buffer
	Function default :: fn default() -> Buffer
	Parameter self :: self: &Buffer
	Parameter clone :: clone: Buffer
	Function clone :: fn clone(self: &Buffer) -> Buffer
	Parameter new :: pub new: Buffer
	Function new :: pub fn new() -> Buffer
	Parameter self :: self: &mut Buffer
	Parameter i :: i: I
	Parameter format :: pub format: &str
	Function format :: pub fn format(self: &mut Buffer, i: I) -> &str
	Constant MAX_STR_LEN :: const MAX_STR_LEN: native-uint
	Alias Buffer :: type Buffer = ?oracle-gap [placeholder:?oracle-gap]
	Parameter self :: self: Self
	Parameter buf :: buf: &mut ?oracle-gap [placeholder:?oracle-gap]
	Parameter write :: write: &str
	Function write :: fn write(self: Self, buf: &mut ?oracle-gap) -> &str [placeholder:?oracle-gap]
summary entities=25 signature rendered=25 unavailable=0 placeholder=3 malformed=0 canonical ok=25 err=0 document ok=25 err=0
package log-0.4.22 file=log-0.4.22/src/lib.rs bytes=60684
	Module macros :: mod macros
	Module serde :: mod serde
	Enum Level :: pub enum Level
	Enum LevelFilter :: pub enum LevelFilter
	Enum MaybeStaticStr :: enum MaybeStaticStr
	Record Record :: pub struct Record
	Record RecordBuilder :: pub struct RecordBuilder
	Record Metadata :: pub struct Metadata
	Record MetadataBuilder :: pub struct MetadataBuilder
	Trait Log :: pub trait Log
	Record NopLogger :: struct NopLogger
	Record SetLoggerError :: pub struct SetLoggerError
	Record ParseLevelError :: pub struct ParseLevelError
	Module __private_api :: pub mod __private_api
	Parameter dyn Log :: dyn Log: dyn Log
	Static LOGGER :: static LOGGER: &dyn Log
	Static STATE :: static STATE: AtomicUsize
	Constant UNINITIALIZED :: const UNINITIALIZED: native-uint
	Constant INITIALIZING :: const INITIALIZING: native-uint
	Constant INITIALIZED :: const INITIALIZED: native-uint
	Static MAX_LOG_LEVEL_FILTER :: static MAX_LOG_LEVEL_FILTER: AtomicUsize
	Parameter &str :: &str: &str
	Static LOG_LEVEL_NAMES :: static LOG_LEVEL_NAMES: [&str; 6]
	Static SET_LOGGER_ERROR :: static SET_LOGGER_ERROR: &str
	Static LEVEL_PARSE_ERROR :: static LEVEL_PARSE_ERROR: &str
summary entities=25 signature rendered=25 unavailable=0 placeholder=0 malformed=0 canonical ok=25 err=0 document ok=25 err=0
package memchr-2.7.4 file=memchr-2.7.4/src/arch/x86_64/avx2/memchr.rs bytes=50366
	Record One :: pub struct One
	Record OneIter :: pub struct OneIter
	Record Two :: pub struct Two
	Record TwoIter :: pub struct TwoIter
	Record Three :: pub struct Three
	Record ThreeIter :: pub struct ThreeIter
	Field sse2 :: sse2: ?oracle-gap [placeholder:?oracle-gap]
	Field avx2 :: avx2: ?oracle-gap [placeholder:?oracle-gap]
	Implementation One :: impl One = One
	Parameter needle :: needle: u8
	Parameter new :: pub new: Option<One>
	Function new :: pub fn new(needle: u8) -> Option<One>
	Parameter needle :: needle: u8
	Parameter new_unchecked :: pub new_unchecked: One
	Function new_unchecked :: pub fn new_unchecked(needle: u8) -> One
	Parameter is_available :: pub is_available: bool
	Function is_available :: pub fn is_available() -> bool
	Parameter self :: self: &One
	Parameter [u8] :: [u8]: [u8]
	Parameter haystack :: haystack: &[u8]
	Parameter find :: pub find: Option<native-uint>
	Function find :: pub fn find(self: &One, haystack: &[u8]) -> Option<native-uint>
	Parameter self :: self: &One
	Parameter haystack :: haystack: &[u8]
	Parameter rfind :: pub rfind: Option<native-uint>
summary entities=25 signature rendered=25 unavailable=0 placeholder=2 malformed=0 canonical ok=25 err=0 document ok=25 err=0
";

// # KNOWN GAPS (go, pinned 2026-09):
// # - struct signatures render the head only (render.rs record arm).
// # - Cross-package types (net/http Handler, ...) render
// #   `?unresolved-external`; universe builtins without a lattice row
// #   (`error`) and oracle-unprojected types render
// #   `?no-ir-representation` (upstream authority/type rows).
#[rustfmt::skip]
const EXPECTED_GO: &str = r"# go render snapshot
package github.com/google/uuid@v1.6.0 file=github.com/google/uuid@v1.6.0/uuid.go bytes=9633
	Record Domain :: struct Domain
	Record NullUUID :: struct NullUUID
	Record Time :: struct Time
	Record UUID :: struct UUID
	Record UUIDs :: struct UUIDs
	Record Variant :: struct Variant
	Record Version :: struct Version
	Record invalidLengthError :: struct invalidLengthError
	Parameter _ :: _: native-int
	Function ClockSequence :: fn ClockSequence() -> native-int
	Function DisableRandPool :: fn DisableRandPool()
	Parameter _ :: _: str
	Function String :: fn String() -> str
	Function EnableRandPool :: fn EnableRandPool()
	Parameter b :: b: [u8]
	Parameter uuid :: uuid: UUID
	Parameter err :: err: ?no-ir-representation(error) [placeholder:?no-ir-representation]
	Function FromBytes :: fn FromBytes(b: [u8]) -> (uuid: UUID, err: ?no-ir-representation(error)) [placeholder:?no-ir-representation]
	Constant Future :: const Future: Variant
	Parameter _ :: _: Time
	Parameter _1 :: _1: u16
	Parameter _2 :: _2: ?no-ir-representation(error) [placeholder:?no-ir-representation]
	Function GetTime :: fn GetTime() -> (Time, u16, ?no-ir-representation(error)) [placeholder:?no-ir-representation]
	Constant Group :: const Group: Domain
	Constant Invalid :: const Invalid: Variant
summary entities=25 signature rendered=25 unavailable=0 placeholder=4 malformed=0 canonical ok=25 err=0 document ok=25 err=0
package github.com/go-chi/chi/v5@v5.0.12 file=github.com/go-chi/chi/v5@v5.0.12/tree.go bytes=20744
	Record ChainHandler :: struct ChainHandler
	Record Context :: struct Context
	Record Middlewares :: struct Middlewares
	Record Mux :: struct Mux
	Record Route :: struct Route
	Record RouteParams :: struct RouteParams
	Trait Router :: trait Router
	Trait Routes :: trait Routes
	Record WalkFunc :: struct WalkFunc
	Record contextKey :: struct contextKey
	Record endpoint :: struct endpoint
	Record endpoints :: struct endpoints
	Record methodTyp :: struct methodTyp
	Record node :: struct node
	Record nodeTyp :: struct nodeTyp
	Record nodes :: struct nodes
	Parameter middlewares :: middlewares: [fn(?no-ir-representation(Handler)) -> ?no-ir-representation(Handler)] [placeholder:?no-ir-representation]
	Parameter _1 :: _1: Middlewares
	Function Chain :: fn Chain(...middlewares: [fn(?no-ir-representation(Handler)) -> ?no-ir-representation(Handler)]) -> Middlewares [placeholder:?no-ir-representation]
	Field Endpoint :: Endpoint: ?unresolved-external(Handler) [placeholder:?unresolved-external]
	Field chain :: chain: ?unresolved-external(Handler) [placeholder:?unresolved-external]
	Field Middlewares :: Middlewares: Middlewares
	Parameter w :: w: ?unresolved-external(ResponseWriter) [placeholder:?unresolved-external]
	Parameter r :: r: *mut ?no-ir-representation(Request) [placeholder:?no-ir-representation]
	Function ServeHTTP :: fn ServeHTTP(w: ?unresolved-external(ResponseWriter), r: *mut ?no-ir-representation(Request)) [placeholder:?unresolved-external]
summary entities=25 signature rendered=25 unavailable=0 placeholder=7 malformed=0 canonical ok=25 err=0 document ok=25 err=0
package github.com/google/go-cmp@v0.6.0 file=github.com/google/go-cmp@v0.6.0/cmp/compare.go bytes=22889
	Record Indirect :: struct Indirect
	Record MapIndex :: struct MapIndex
	Trait Option :: trait Option
	Record Options :: struct Options
	Record Path :: struct Path
	Trait PathStep :: trait PathStep
	Record Result :: struct Result
	Record SliceIndex :: struct SliceIndex
	Record StructField :: struct StructField
	Record Transform :: struct Transform
	Record TypeAssertion :: struct TypeAssertion
	Trait applicableOption :: trait applicableOption
	Record commentString :: struct commentString
	Record comparer :: struct comparer
	Record core :: struct core
	Trait coreOption :: trait coreOption
	Record defaultReporter :: struct defaultReporter
	Record diffMode :: struct diffMode
	Record diffStats :: struct diffStats
	Record dynChecker :: struct dynChecker
	Record exporter :: struct exporter
	Record formatOptions :: struct formatOptions
	Record formatValueOptions :: struct formatValueOptions
	Record ignore :: struct ignore
	Record indentMode :: struct indentMode
summary entities=25 signature rendered=25 unavailable=0 placeholder=0 malformed=0 canonical ok=25 err=0 document ok=25 err=0
package gopkg.in/yaml.v3@v3.0.1 file=gopkg.in/yaml.v3@v3.0.1/scannerc.go bytes=87960
	Record Decoder :: struct Decoder
	Record Encoder :: struct Encoder
	Trait IsZeroer :: trait IsZeroer
	Record Kind :: struct Kind
	Trait Marshaler :: trait Marshaler
	Record Node :: struct Node
	Record Style :: struct Style
	Record TypeError :: struct TypeError
	Trait Unmarshaler :: trait Unmarshaler
	Record decoder :: struct decoder
	Record encoder :: struct encoder
	Record fieldInfo :: struct fieldInfo
	Record keyList :: struct keyList
	Trait obsoleteUnmarshaler :: trait obsoleteUnmarshaler
	Record parser :: struct parser
	Record resolveMapItem :: struct resolveMapItem
	Record structInfo :: struct structInfo
	Record yamlError :: struct yamlError
	Record yaml_alias_data_t :: struct yaml_alias_data_t
	Record yaml_break_t :: struct yaml_break_t
	Record yaml_comment_t :: struct yaml_comment_t
	Record yaml_document_t :: struct yaml_document_t
	Record yaml_emitter_state_t :: struct yaml_emitter_state_t
	Record yaml_emitter_t :: struct yaml_emitter_t
	Record yaml_encoding_t :: struct yaml_encoding_t
summary entities=25 signature rendered=25 unavailable=0 placeholder=0 malformed=0 canonical ok=25 err=0 document ok=25 err=0
package golang.org/x/mod@v0.17.0 file=golang.org/x/mod@v0.17.0/modfile/rule.go bytes=45441
	Record Comment :: struct Comment
	Record CommentBlock :: struct CommentBlock
	Record Comments :: struct Comments
	Record Error :: struct Error
	Record ErrorList :: struct ErrorList
	Record Exclude :: struct Exclude
	Trait Expr :: trait Expr
	Record File :: struct File
	Record FileSyntax :: struct FileSyntax
	Record Go :: struct Go
	Record LParen :: struct LParen
	Record Line :: struct Line
	Record LineBlock :: struct LineBlock
	Record Module :: struct Module
	Record Position :: struct Position
	Record RParen :: struct RParen
	Record Replace :: struct Replace
	Record Require :: struct Require
	Record Retract :: struct Retract
	Record Toolchain :: struct Toolchain
	Record Use :: struct Use
	Record VersionFixer :: struct VersionFixer
	Record VersionInterval :: struct VersionInterval
	Record WorkFile :: struct WorkFile
	Record input :: struct input
summary entities=25 signature rendered=25 unavailable=0 placeholder=0 malformed=0 canonical ok=25 err=0 document ok=25 err=0
";

// # KNOWN GAPS (typescript, pinned 2026-09):
// # - Namespace overload declarations lower a Function whose NAME is the
// #   whole source signature text (chalk: `(...text: unknown[]): string;`),
// #   so the neutral render double-prints it (upstream lowering naming).
// # - Re-exports render `?unresolved-external`; dynamic aliases
// #   `type any = ?dynamic` (honest upstream rows).
#[rustfmt::skip]
const EXPECTED_TYPESCRIPT: &str = r#"# typescript render snapshot
package axios-1.7.9 file=axios-1.7.9/index.d.ts bytes=18440
	Trait RawAxiosHeaders :: trait RawAxiosHeaders
	Record AxiosHeaders :: struct AxiosHeaders
	Trait AxiosRequestTransformer :: trait AxiosRequestTransformer
	Trait AxiosResponseTransformer :: trait AxiosResponseTransformer
	Trait AxiosAdapter :: trait AxiosAdapter
	Trait AxiosBasicCredentials :: trait AxiosBasicCredentials
	Trait AxiosProxyConfig :: trait AxiosProxyConfig
	Enum HttpStatusCode :: enum HttpStatusCode
	Trait TransitionalOptions :: trait TransitionalOptions
	Trait GenericAbortSignal :: trait GenericAbortSignal
	Trait FormDataVisitorHelpers :: trait FormDataVisitorHelpers
	Trait SerializerVisitor :: trait SerializerVisitor
	Trait SerializerOptions :: trait SerializerOptions
	Trait FormSerializerOptions :: trait FormSerializerOptions
	Trait ParamEncoder :: trait ParamEncoder
	Trait CustomParamsSerializer :: trait CustomParamsSerializer
	Trait ParamsSerializerOptions :: trait ParamsSerializerOptions
	Trait AxiosProgressEvent :: trait AxiosProgressEvent
	Trait LookupAddressEntry :: trait LookupAddressEntry
	Parameter D :: D: D
	Alias any :: type any = ?dynamic [placeholder:?dynamic]
	Trait AxiosRequestConfig :: trait AxiosRequestConfig
	Parameter D :: D: D
	Trait InternalAxiosRequestConfig :: trait InternalAxiosRequestConfig
	Trait HeadersDefaults :: trait HeadersDefaults
summary entities=25 signature rendered=25 unavailable=0 placeholder=1 malformed=0 canonical ok=25 err=0 document ok=25 err=0
package chalk-5.3.0 file=chalk-5.3.0/source/index.d.ts bytes=6911
	Reexport ModifierName :: use ModifierName: ?unresolved-external(ModifierName) [placeholder:?unresolved-external]
	Reexport ForegroundColorName :: use ForegroundColorName: ?unresolved-external(ForegroundColorName) [placeholder:?unresolved-external]
	Reexport BackgroundColorName :: use BackgroundColorName: ?unresolved-external(BackgroundColorName) [placeholder:?unresolved-external]
	Reexport ColorName :: use ColorName: ?unresolved-external(ColorName) [placeholder:?unresolved-external]
	Reexport ColorInfo :: use ColorInfo: ?unresolved-external(ColorInfo) [placeholder:?unresolved-external]
	Reexport ColorSupportLevel :: use ColorSupportLevel: ?unresolved-external(ColorSupportLevel) [placeholder:?unresolved-external]
	Trait Options :: trait Options
	Trait ChalkInstance :: trait ChalkInstance
	Field level :: level: ColorSupportLevel
	Constant Chalk :: const Chalk: ?no-ir-representation(new (options?: Options) => ChalkInstance) [placeholder:?no-ir-representation]
	Alias unknown :: type unknown = any
	Parameter ...text :: ...text: []any
	Alias string :: type string = str
	Function (...text: unknown[]): string; :: fn (...text: unknown[]): string;(......text: []any) -> str
	Field level :: level: ColorSupportLevel
	Alias number :: type number = f64
	Alias this :: type this = this
	Field rgb :: rgb: fn(red: f64, green: f64, blue: f64) -> this
	Field hex :: hex: fn(color: str) -> this
	Field ansi256 :: ansi256: fn(index: f64) -> this
	Field bgRgb :: bgRgb: fn(red: f64, green: f64, blue: f64) -> this
	Field bgHex :: bgHex: fn(color: str) -> this
	Field bgAnsi256 :: bgAnsi256: fn(index: f64) -> this
	Field reset :: reset: this
	Field bold :: bold: this
summary entities=25 signature rendered=25 unavailable=0 placeholder=7 malformed=0 canonical ok=25 err=0 document ok=25 err=0
package commander-13.1.0 file=commander-13.1.0/typings/index.d.ts bytes=32757
	Record CommanderError :: struct CommanderError
	Record InvalidArgumentError :: struct InvalidArgumentError
	Trait ErrorOptions :: trait ErrorOptions
	Record Argument :: struct Argument
	Record Option :: struct Option
	Record Help :: struct Help
	Trait ParseOptions :: trait ParseOptions
	Trait HelpContext :: trait HelpContext
	Trait AddHelpTextContext :: trait AddHelpTextContext
	Trait OutputConfiguration :: trait OutputConfiguration
	Record Command :: struct Command
	Trait CommandOptions :: trait CommandOptions
	Trait ExecutableCommandOptions :: trait ExecutableCommandOptions
	Trait ParseOptionsResult :: trait ParseOptionsResult
	Parameter LiteralType :: LiteralType: LiteralType
	Parameter BaseType :: BaseType: BaseType
	Alias string :: type string = str
	Alias number :: type number = f64
	Alias string | number :: type string | number = str | f64
	Alias Record :: type Record = Record
	Alias never :: type never = !
	Alias Record<never, never> :: type Record<never, never> = Record<!, !>
	Alias (BaseType & Record<never, never>) :: type (BaseType & Record<never, never>) = BaseType & Record<!, !>
	Alias LiteralUnion :: type LiteralUnion = LiteralType | BaseType & Record<!, !>
	Field code :: code: str
summary entities=25 signature rendered=25 unavailable=0 placeholder=0 malformed=0 canonical ok=25 err=0 document ok=25 err=0
package detect-libc-2.1.2 file=detect-libc-2.1.2/index.d.ts bytes=436
	Constant GLIBC :: const GLIBC: "glibc"
	Constant MUSL :: const MUSL: "musl"
	Alias Promise :: type Promise = Promise
	Alias string :: type string = str
	Alias null :: type null = null
	Alias string | null :: type string | null = str | null
	Alias Promise<string | null> :: type Promise<string | null> = Promise<str | null>
	Function family :: fn family() -> Promise<str | null>
	Function familySync :: fn familySync() -> str | null
	Alias boolean :: type boolean = bool
	Alias Promise<boolean> :: type Promise<boolean> = Promise<bool>
	Function isNonGlibcLinux :: fn isNonGlibcLinux() -> Promise<bool>
	Function isNonGlibcLinuxSync :: fn isNonGlibcLinuxSync() -> bool
	Function version :: fn version() -> Promise<str | null>
	Function versionSync :: fn versionSync() -> str | null
summary entities=15 signature rendered=15 unavailable=0 placeholder=0 malformed=0 canonical ok=15 err=0 document ok=15 err=0
package tslib-2.8.1 file=tslib-2.8.1/tslib.d.ts bytes=18317
	Parameter d :: d: Function
	Parameter b :: b: Function
	Alias void :: type void = void
	Function __extends :: fn __extends(d: Function, b: Function) -> void
	Parameter t :: t: ?dynamic [placeholder:?dynamic]
	Alias any :: type any = ?dynamic [placeholder:?dynamic]
	Parameter ...sources :: ...sources: []?dynamic [placeholder:?dynamic]
	Function __assign :: fn __assign(t: ?dynamic, ......sources: []?dynamic) -> ?dynamic [placeholder:?dynamic]
	Parameter t :: t: ?dynamic [placeholder:?dynamic]
	Alias string :: type string = str
	Alias symbol :: type symbol = ?no-ir-representation(symbol) [placeholder:?no-ir-representation]
	Alias (string | symbol) :: type (string | symbol) = str | ?no-ir-representation(symbol) [placeholder:?no-ir-representation]
	Parameter propertyNames :: propertyNames: []str | ?no-ir-representation(symbol) [placeholder:?no-ir-representation]
	Function __rest :: fn __rest(t: ?dynamic, propertyNames: []str | ?no-ir-representation(symbol)) -> ?dynamic [placeholder:?dynamic]
	Alias Function :: type Function = Function
	Parameter decorators :: decorators: []Function
	Parameter target :: target: ?dynamic [placeholder:?dynamic]
	Parameter key :: key: str | ?no-ir-representation(symbol) [placeholder:?no-ir-representation]
	Parameter desc :: desc: ?dynamic [placeholder:?dynamic]
	Function __decorate :: fn __decorate(decorators: []Function, target: ?dynamic, key?: str | ?no-ir-representation(symbol), desc?: ?dynamic) -> ?dynamic [placeholder:?dynamic]
	Parameter paramIndex :: paramIndex: f64
	Parameter decorator :: decorator: Function
	Function __param :: fn __param(paramIndex: f64, decorator: Function) -> Function
	Alias null :: type null = null
	Parameter ctor :: ctor: Function | null
summary entities=25 signature rendered=25 unavailable=0 placeholder=13 malformed=0 canonical ok=25 err=0 document ok=25 err=0
"#;

// # KNOWN GAPS (python, pinned 2026-09):
// # - Pinned under the Nix shell, where pyrefly (NUDOX_PYREFLY_BIN) always
// #   runs: unannotated positions take the checker's inference, stay
// #   `?unannotated` when it proved nothing, and `?oracle-gap` when it
// #   answered a shape the lane cannot host (honest, checker-bounded).
// # - Star-imports lower an Alias literally named `*` (upstream naming).
#[rustfmt::skip]
const EXPECTED_PYTHON: &str = r"# python render snapshot
package PyYAML-6.0.2 file=PyYAML-6.0.2/lib/yaml/scanner.py bytes=51279
	Record ScannerError :: struct ScannerError
	Record SimpleKey :: struct SimpleKey
	Record Scanner :: struct Scanner
	Static __all__ :: static __all__: list<str>
	Alias MarkedYAMLError :: type MarkedYAMLError = ?unannotated [placeholder:?unannotated]
	Alias * :: type * = ?unannotated [placeholder:?unannotated]
	Parameter self :: self: SimpleKey
	Parameter token_number :: token_number: ?unannotated [placeholder:?unannotated]
	Parameter required :: required: ?unannotated [placeholder:?unannotated]
	Parameter index :: index: ?unannotated [placeholder:?unannotated]
	Parameter line :: line: ?unannotated [placeholder:?unannotated]
	Parameter column :: column: ?unannotated [placeholder:?unannotated]
	Parameter mark :: mark: ?unannotated [placeholder:?unannotated]
	Function __init__ :: fn __init__(self: SimpleKey, token_number: ?unannotated, required: ?unannotated, index: ?unannotated, line: ?unannotated, column: ?unannotated, mark: ?unannotated) [placeholder:?unannotated]
	Parameter self :: self: Scanner
	Function __init__ :: fn __init__(self: Scanner)
	Parameter self :: self: Scanner
	Parameter choices :: choices: ?oracle-gap [placeholder:?oracle-gap]
	Function check_token :: fn check_token(self: Scanner, choices: ?oracle-gap) [placeholder:?oracle-gap]
	Parameter self :: self: Scanner
	Function peek_token :: fn peek_token(self: Scanner)
	Parameter self :: self: Scanner
	Function get_token :: fn get_token(self: Scanner)
	Parameter self :: self: Scanner
	Function need_more_tokens :: fn need_more_tokens(self: Scanner)
summary entities=25 signature rendered=25 unavailable=0 placeholder=11 malformed=0 canonical ok=25 err=0 document ok=25 err=0
package anyio-4.14.2 file=anyio-4.14.2/src/anyio/_backends/_asyncio.py bytes=104400
	Record _State :: struct _State
	Record Runner :: struct Runner
	Record CancelScope :: struct CancelScope
	Record TaskState :: struct TaskState
	Record _AsyncioTaskStatus :: struct _AsyncioTaskStatus
	Record TaskGroup :: struct TaskGroup
	Record WorkerThread :: struct WorkerThread
	Record StreamReaderWrapper :: struct StreamReaderWrapper
	Record StreamWriterWrapper :: struct StreamWriterWrapper
	Record Process :: struct Process
	Record StreamProtocol :: struct StreamProtocol
	Record DatagramProtocol :: struct DatagramProtocol
	Record SocketStream :: struct SocketStream
	Record _RawSocketMixin :: struct _RawSocketMixin
	Record UNIXSocketStream :: struct UNIXSocketStream
	Record TCPSocketListener :: struct TCPSocketListener
	Record UNIXSocketListener :: struct UNIXSocketListener
	Record UDPSocket :: struct UDPSocket
	Record ConnectedUDPSocket :: struct ConnectedUDPSocket
	Record UNIXDatagramSocket :: struct UNIXDatagramSocket
	Record ConnectedUNIXDatagramSocket :: struct ConnectedUNIXDatagramSocket
	Record Event :: struct Event
	Record Lock :: struct Lock
	Record Semaphore :: struct Semaphore
	Record CapacityLimiter :: struct CapacityLimiter
summary entities=25 signature rendered=25 unavailable=0 placeholder=0 malformed=0 canonical ok=25 err=0 document ok=25 err=0
package appdirs-1.4.4 file=appdirs-1.4.4/appdirs.py bytes=24720
	Record AppDirs :: struct AppDirs
	Static __version__ :: static __version__: str
	Static __version_info__ :: static __version_info__: ?oracle-gap [placeholder:?oracle-gap]
	Alias sys :: type sys = ?unannotated [placeholder:?unannotated]
	Alias os :: type os = ?unannotated [placeholder:?unannotated]
	Static PY3 :: static PY3: bool
	Static unicode :: static unicode: ?oracle-gap [placeholder:?oracle-gap]
	Alias platform :: type platform = ?unannotated [placeholder:?unannotated]
	Static os_name :: static os_name: ?unannotated [placeholder:?unannotated]
	Static system :: static system: str
	Parameter appname :: appname: None
	Parameter appauthor :: appauthor: None
	Parameter version :: version: None
	Parameter roaming :: roaming: bool
	Function user_data_dir :: fn user_data_dir(appname: None, appauthor: None, version: None, roaming: bool)
	Parameter appname :: appname: None
	Parameter appauthor :: appauthor: None
	Parameter version :: version: None
	Parameter multipath :: multipath: bool
	Function site_data_dir :: fn site_data_dir(appname: None, appauthor: None, version: None, multipath: bool)
	Parameter appname :: appname: None
	Parameter appauthor :: appauthor: None
	Parameter version :: version: None
	Parameter roaming :: roaming: bool
	Function user_config_dir :: fn user_config_dir(appname: None, appauthor: None, version: None, roaming: bool)
summary entities=25 signature rendered=25 unavailable=0 placeholder=6 malformed=0 canonical ok=25 err=0 document ok=25 err=0
package attrs-25.3.0 file=attrs-25.3.0/src/attr/_make.py bytes=96664
	Record _Nothing :: struct _Nothing
	Record _CacheHashWrapper :: struct _CacheHashWrapper
	Record _Attributes :: struct _Attributes
	Record _ClassBuilder :: struct _ClassBuilder
	Record Attribute :: struct Attribute
	Record _CountingAttr :: struct _CountingAttr
	Record Factory :: struct Factory
	Record Converter :: struct Converter
	Record _AndValidator :: struct _AndValidator
	Alias annotations :: type annotations = ?unannotated [placeholder:?unannotated]
	Alias abc :: type abc = ?unannotated [placeholder:?unannotated]
	Alias contextlib :: type contextlib = ?unannotated [placeholder:?unannotated]
	Alias copy :: type copy = ?unannotated [placeholder:?unannotated]
	Alias enum :: type enum = ?unannotated [placeholder:?unannotated]
	Alias inspect :: type inspect = ?unannotated [placeholder:?unannotated]
	Alias itertools :: type itertools = ?unannotated [placeholder:?unannotated]
	Alias linecache :: type linecache = ?unannotated [placeholder:?unannotated]
	Alias sys :: type sys = ?unannotated [placeholder:?unannotated]
	Alias types :: type types = ?unannotated [placeholder:?unannotated]
	Alias unicodedata :: type unicodedata = ?unannotated [placeholder:?unannotated]
	Alias Callable :: type Callable = ?unannotated [placeholder:?unannotated]
	Alias Mapping :: type Mapping = ?unannotated [placeholder:?unannotated]
	Alias cached_property :: type cached_property = ?unannotated [placeholder:?unannotated]
	Alias Any :: type Any = ?unannotated [placeholder:?unannotated]
	Alias NamedTuple :: type NamedTuple = ?unannotated [placeholder:?unannotated]
summary entities=25 signature rendered=25 unavailable=0 placeholder=16 malformed=0 canonical ok=25 err=0 document ok=25 err=0
package packaging-25.0 file=packaging-25.0/tests/test_tags.py bytes=62275
	Record TestTag :: struct TestTag
	Record TestParseTag :: struct TestParseTag
	Record TestInterpreterName :: struct TestInterpreterName
	Record MockImplementation :: struct MockImplementation
	Record TestInterpreterVersion :: struct TestInterpreterVersion
	Record MockConfigVar :: struct MockConfigVar
	Record TestMacOSPlatforms :: struct TestMacOSPlatforms
	Record TestIOSPlatforms :: struct TestIOSPlatforms
	Record TestAndroidPlatforms :: struct TestAndroidPlatforms
	Record TestManylinuxPlatform :: struct TestManylinuxPlatform
	Record TestCPythonABI :: struct TestCPythonABI
	Record TestCPythonTags :: struct TestCPythonTags
	Record TestGenericTags :: struct TestGenericTags
	Record TestCompatibleTags :: struct TestCompatibleTags
	Record TestSysTags :: struct TestSysTags
	Record TestBitness :: struct TestBitness
	Alias abc :: type abc = ?unannotated [placeholder:?unannotated]
	Alias subprocess :: type subprocess = ?unannotated [placeholder:?unannotated]
	Alias ctypes :: type ctypes = ?unannotated [placeholder:?unannotated]
	Static ctypes :: static ctypes: ?oracle-gap [placeholder:?oracle-gap]
	Alias importlib :: type importlib = ?unannotated [placeholder:?unannotated]
	Alias os :: type os = ?unannotated [placeholder:?unannotated]
	Alias pathlib :: type pathlib = ?unannotated [placeholder:?unannotated]
	Alias platform :: type platform = ?unannotated [placeholder:?unannotated]
	Alias struct :: type struct = ?unannotated [placeholder:?unannotated]
summary entities=25 signature rendered=25 unavailable=0 placeholder=9 malformed=0 canonical ok=25 err=0 document ok=25 err=0
";

// # KNOWN GAPS (java, pinned 2026-09):
// # - Cleanest lane: 0 placeholders. Package scopes emit BOTH a Namespace
// #   and a Module row for one package (upstream duplication, gson).
// # - Names are fully qualified (org.apache.commons.io.FileUtils), a
// #   lowering naming choice, not a renderer defect.
#[rustfmt::skip]
const EXPECTED_JAVA: &str = r"# java render snapshot
package maven:commons-io:commons-io@2.15.1 file=commons-io/commons-io/2.15.1/org/apache/commons/io/FileUtils.java bytes=166590
	Namespace org.apache.commons.io :: namespace org.apache.commons.io
	Record org.apache.commons.io.FileUtils :: struct org.apache.commons.io.FileUtils
	Record org.apache.commons.io.ByteOrderMark :: struct org.apache.commons.io.ByteOrderMark
	Record org.apache.commons.io.ByteOrderParser :: struct org.apache.commons.io.ByteOrderParser
	Record org.apache.commons.io.Charsets :: struct org.apache.commons.io.Charsets
	Record org.apache.commons.io.CloseableURLConnection :: struct org.apache.commons.io.CloseableURLConnection
	Record org.apache.commons.io.CopyUtils :: struct org.apache.commons.io.CopyUtils
	Record org.apache.commons.io.DirectoryWalker :: struct org.apache.commons.io.DirectoryWalker
	Record org.apache.commons.io.DirectoryWalker.CancelException :: struct org.apache.commons.io.DirectoryWalker.CancelException
	Record org.apache.commons.io.EndianUtils :: struct org.apache.commons.io.EndianUtils
	Record org.apache.commons.io.FileCleaner :: struct org.apache.commons.io.FileCleaner
	Record org.apache.commons.io.FileCleaningTracker :: struct org.apache.commons.io.FileCleaningTracker
	Record org.apache.commons.io.FileCleaningTracker.Reaper :: struct org.apache.commons.io.FileCleaningTracker.Reaper
	Record org.apache.commons.io.FileCleaningTracker.Tracker :: struct org.apache.commons.io.FileCleaningTracker.Tracker
	Record org.apache.commons.io.FileDeleteStrategy :: struct org.apache.commons.io.FileDeleteStrategy
	Record org.apache.commons.io.FileDeleteStrategy.ForceFileDeleteStrategy :: struct org.apache.commons.io.FileDeleteStrategy.ForceFileDeleteStrategy
	Record org.apache.commons.io.FileExistsException :: struct org.apache.commons.io.FileExistsException
	Enum org.apache.commons.io.FileSystem :: enum org.apache.commons.io.FileSystem
	Record org.apache.commons.io.FileSystemUtils :: struct org.apache.commons.io.FileSystemUtils
	Record org.apache.commons.io.FilenameUtils :: struct org.apache.commons.io.FilenameUtils
	Record org.apache.commons.io.HexDump :: struct org.apache.commons.io.HexDump
	Record org.apache.commons.io.IO :: struct org.apache.commons.io.IO
	Enum org.apache.commons.io.IOCase :: enum org.apache.commons.io.IOCase
	Record org.apache.commons.io.IOExceptionList :: struct org.apache.commons.io.IOExceptionList
	Record org.apache.commons.io.IOExceptionWithCause :: struct org.apache.commons.io.IOExceptionWithCause
summary entities=25 signature rendered=25 unavailable=0 placeholder=0 malformed=0 canonical ok=25 err=0 document ok=25 err=0
package maven:com.google.code.gson:gson@2.10.1 file=com/google/code/gson/gson/2.10.1/com/google/gson/Gson.java bytes=62313
	Namespace com.google.gson :: namespace com.google.gson
	Module com.google.gson :: mod com.google.gson
	Record com.google.gson.Gson :: struct com.google.gson.Gson
	Record com.google.gson.Gson.FutureTypeAdapter :: struct com.google.gson.Gson.FutureTypeAdapter
	Trait com.google.gson.ExclusionStrategy :: trait com.google.gson.ExclusionStrategy
	Record com.google.gson.FieldAttributes :: struct com.google.gson.FieldAttributes
	Enum com.google.gson.FieldNamingPolicy :: enum com.google.gson.FieldNamingPolicy
	Trait com.google.gson.FieldNamingStrategy :: trait com.google.gson.FieldNamingStrategy
	Record com.google.gson.GsonBuilder :: struct com.google.gson.GsonBuilder
	Trait com.google.gson.InstanceCreator :: trait com.google.gson.InstanceCreator
	Record com.google.gson.JsonArray :: struct com.google.gson.JsonArray
	Trait com.google.gson.JsonDeserializationContext :: trait com.google.gson.JsonDeserializationContext
	Trait com.google.gson.JsonDeserializer :: trait com.google.gson.JsonDeserializer
	Record com.google.gson.JsonElement :: struct com.google.gson.JsonElement
	Record com.google.gson.JsonIOException :: struct com.google.gson.JsonIOException
	Record com.google.gson.JsonNull :: struct com.google.gson.JsonNull
	Record com.google.gson.JsonObject :: struct com.google.gson.JsonObject
	Record com.google.gson.JsonParseException :: struct com.google.gson.JsonParseException
	Record com.google.gson.JsonParser :: struct com.google.gson.JsonParser
	Record com.google.gson.JsonPrimitive :: struct com.google.gson.JsonPrimitive
	Trait com.google.gson.JsonSerializationContext :: trait com.google.gson.JsonSerializationContext
	Trait com.google.gson.JsonSerializer :: trait com.google.gson.JsonSerializer
	Record com.google.gson.JsonStreamParser :: struct com.google.gson.JsonStreamParser
	Record com.google.gson.JsonSyntaxException :: struct com.google.gson.JsonSyntaxException
	Enum com.google.gson.LongSerializationPolicy :: enum com.google.gson.LongSerializationPolicy
summary entities=25 signature rendered=25 unavailable=0 placeholder=0 malformed=0 canonical ok=25 err=0 document ok=25 err=0
package maven:commons-codec:commons-codec@1.16.0 file=commons-codec/commons-codec/1.16.0/org/apache/commons/codec/digest/DigestUtils.java bytes=54025
	Namespace org.apache.commons.codec.digest :: namespace org.apache.commons.codec.digest
	Record org.apache.commons.codec.digest.DigestUtils :: struct org.apache.commons.codec.digest.DigestUtils
	Namespace org.apache.commons.codec :: namespace org.apache.commons.codec
	Trait org.apache.commons.codec.BinaryDecoder :: trait org.apache.commons.codec.BinaryDecoder
	Trait org.apache.commons.codec.BinaryEncoder :: trait org.apache.commons.codec.BinaryEncoder
	Record org.apache.commons.codec.CharEncoding :: struct org.apache.commons.codec.CharEncoding
	Record org.apache.commons.codec.Charsets :: struct org.apache.commons.codec.Charsets
	Enum org.apache.commons.codec.CodecPolicy :: enum org.apache.commons.codec.CodecPolicy
	Trait org.apache.commons.codec.Decoder :: trait org.apache.commons.codec.Decoder
	Record org.apache.commons.codec.DecoderException :: struct org.apache.commons.codec.DecoderException
	Trait org.apache.commons.codec.Encoder :: trait org.apache.commons.codec.Encoder
	Record org.apache.commons.codec.EncoderException :: struct org.apache.commons.codec.EncoderException
	Record org.apache.commons.codec.Resources :: struct org.apache.commons.codec.Resources
	Trait org.apache.commons.codec.StringDecoder :: trait org.apache.commons.codec.StringDecoder
	Trait org.apache.commons.codec.StringEncoder :: trait org.apache.commons.codec.StringEncoder
	Record org.apache.commons.codec.StringEncoderComparator :: struct org.apache.commons.codec.StringEncoderComparator
	Namespace org.apache.commons.codec.binary :: namespace org.apache.commons.codec.binary
	Record org.apache.commons.codec.binary.Base16 :: struct org.apache.commons.codec.binary.Base16
	Record org.apache.commons.codec.binary.Base16InputStream :: struct org.apache.commons.codec.binary.Base16InputStream
	Record org.apache.commons.codec.binary.Base16OutputStream :: struct org.apache.commons.codec.binary.Base16OutputStream
	Record org.apache.commons.codec.binary.Base32 :: struct org.apache.commons.codec.binary.Base32
	Record org.apache.commons.codec.binary.Base32InputStream :: struct org.apache.commons.codec.binary.Base32InputStream
	Record org.apache.commons.codec.binary.Base32OutputStream :: struct org.apache.commons.codec.binary.Base32OutputStream
	Record org.apache.commons.codec.binary.Base64 :: struct org.apache.commons.codec.binary.Base64
	Record org.apache.commons.codec.binary.Base64InputStream :: struct org.apache.commons.codec.binary.Base64InputStream
summary entities=25 signature rendered=25 unavailable=0 placeholder=0 malformed=0 canonical ok=25 err=0 document ok=25 err=0
package maven:org.opentest4j:opentest4j@1.3.0 file=org/opentest4j/opentest4j/1.3.0/org/opentest4j/ValueWrapper.java bytes=6497
	Namespace org.opentest4j :: namespace org.opentest4j
	Record org.opentest4j.ValueWrapper :: struct org.opentest4j.ValueWrapper
	Record org.opentest4j.AssertionFailedError :: struct org.opentest4j.AssertionFailedError
	Record org.opentest4j.FileInfo :: struct org.opentest4j.FileInfo
	Record org.opentest4j.IncompleteExecutionException :: struct org.opentest4j.IncompleteExecutionException
	Record org.opentest4j.MultipleFailuresError :: struct org.opentest4j.MultipleFailuresError
	Record org.opentest4j.TestAbortedException :: struct org.opentest4j.TestAbortedException
	Record org.opentest4j.TestSkippedException :: struct org.opentest4j.TestSkippedException
	Field serialVersionUID :: serialVersionUID: i64
	Field nullValueWrapper :: nullValueWrapper: org.opentest4j.ValueWrapper
	Parameter value :: value: ?no-ir-representation(java.lang.Object) [placeholder:?no-ir-representation]
	Parameter org.opentest4j.ValueWrapper :: org.opentest4j.ValueWrapper: org.opentest4j.ValueWrapper
	Function create :: fn create(value: ?no-ir-representation(java.lang.Object)) -> org.opentest4j.ValueWrapper [placeholder:?no-ir-representation]
	Parameter value :: value: ?no-ir-representation(java.lang.Object) [placeholder:?no-ir-representation]
	Parameter stringRepresentation :: stringRepresentation: ?no-ir-representation(java.lang.String) [placeholder:?no-ir-representation]
	Parameter org.opentest4j.ValueWrapper :: org.opentest4j.ValueWrapper: org.opentest4j.ValueWrapper
	Function create :: fn create(value: ?no-ir-representation(java.lang.Object), stringRepresentation: ?no-ir-representation(java.lang.String)) -> org.opentest4j.ValueWrapper [placeholder:?no-ir-representation]
	Field value :: value: ?no-ir-representation(java.io.Serializable) [placeholder:?no-ir-representation]
	Field type :: type: ?no-ir-representation(java.lang.Class) [placeholder:?no-ir-representation]
	Field stringRepresentation :: stringRepresentation: ?no-ir-representation(java.lang.String) [placeholder:?no-ir-representation]
	Field identityHashCode :: identityHashCode: i32
	Field ephemeralValue :: ephemeralValue: ?no-ir-representation(java.lang.Object) [placeholder:?no-ir-representation]
	Parameter value :: value: ?no-ir-representation(java.lang.Object) [placeholder:?no-ir-representation]
	Parameter stringRepresentation :: stringRepresentation: ?no-ir-representation(java.lang.String) [placeholder:?no-ir-representation]
	Function <init> :: fn <init>(value: ?no-ir-representation(java.lang.Object), stringRepresentation: ?no-ir-representation(java.lang.String)) [placeholder:?no-ir-representation]
summary entities=25 signature rendered=25 unavailable=0 placeholder=12 malformed=0 canonical ok=25 err=0 document ok=25 err=0
package maven:org.ow2.asm:asm@9.6 file=org/ow2/asm/asm/9.6/org/objectweb/asm/ClassReader.java bytes=172115
	Namespace org.objectweb.asm :: namespace org.objectweb.asm
	Record org.objectweb.asm.ClassReader :: struct org.objectweb.asm.ClassReader
	Record org.objectweb.asm.AnnotationVisitor :: struct org.objectweb.asm.AnnotationVisitor
	Record org.objectweb.asm.AnnotationWriter :: struct org.objectweb.asm.AnnotationWriter
	Record org.objectweb.asm.Attribute :: struct org.objectweb.asm.Attribute
	Record org.objectweb.asm.Attribute.Set :: struct org.objectweb.asm.Attribute.Set
	Record org.objectweb.asm.ByteVector :: struct org.objectweb.asm.ByteVector
	Record org.objectweb.asm.ClassTooLargeException :: struct org.objectweb.asm.ClassTooLargeException
	Record org.objectweb.asm.ClassVisitor :: struct org.objectweb.asm.ClassVisitor
	Record org.objectweb.asm.ClassWriter :: struct org.objectweb.asm.ClassWriter
	Record org.objectweb.asm.ConstantDynamic :: struct org.objectweb.asm.ConstantDynamic
	Record org.objectweb.asm.Constants :: struct org.objectweb.asm.Constants
	Record org.objectweb.asm.Context :: struct org.objectweb.asm.Context
	Record org.objectweb.asm.CurrentFrame :: struct org.objectweb.asm.CurrentFrame
	Record org.objectweb.asm.Edge :: struct org.objectweb.asm.Edge
	Record org.objectweb.asm.FieldVisitor :: struct org.objectweb.asm.FieldVisitor
	Record org.objectweb.asm.FieldWriter :: struct org.objectweb.asm.FieldWriter
	Record org.objectweb.asm.Frame :: struct org.objectweb.asm.Frame
	Record org.objectweb.asm.Handle :: struct org.objectweb.asm.Handle
	Record org.objectweb.asm.Handler :: struct org.objectweb.asm.Handler
	Record org.objectweb.asm.Label :: struct org.objectweb.asm.Label
	Record org.objectweb.asm.MethodTooLargeException :: struct org.objectweb.asm.MethodTooLargeException
	Record org.objectweb.asm.MethodVisitor :: struct org.objectweb.asm.MethodVisitor
	Record org.objectweb.asm.MethodWriter :: struct org.objectweb.asm.MethodWriter
	Record org.objectweb.asm.ModuleVisitor :: struct org.objectweb.asm.ModuleVisitor
summary entities=25 signature rendered=25 unavailable=0 placeholder=0 malformed=0 canonical ok=25 err=0 document ok=25 err=0
";

// # KNOWN GAPS (csharp, pinned 2026-09):
// # - Framework types render `?unresolved-external(...)` because the
// #   helper's source-mode authority has no BCL assembly scope (upstream).
// # - Public methods carry no visibility prefix (the image does not mark
// #   them Public), so the neutral `pub ` head is absent (upstream).
#[rustfmt::skip]
const EXPECTED_CSHARP: &str = r"# csharp render snapshot
package morelinq.source.moreenumerable.batch/1.0.1 file=morelinq.source.moreenumerable.batch/1.0.1/content/net20/MoreLinq/MoreEnumerable.Batch.cs bytes=4270
	Namespace MoreLinq :: namespace MoreLinq
	Record MoreEnumerable :: struct MoreEnumerable
	Parameter source :: source: ?unresolved-external(System.Collections.Generic.IEnumerable<TSource>) [placeholder:?unresolved-external]
	Parameter size :: size: i32
	Parameter System.Collections.Generic.IEnumerable<System.Collections.Generic.IEnumerable<TSource>> :: System.Collections.Generic.IEnumerable<System.Collections.Generic.IEnumerable<TSource>>: ?unresolved-external(System.Collections.Generic.IEnumerable<System.Collections.Generic.IEnumerable<TSource>>) [placeholder:?unresolved-external]
	Function Batch :: fn Batch(source: ?unresolved-external(System.Collections.Generic.IEnumerable<TSource>), size: i32) -> ?unresolved-external(System.Collections.Generic.IEnumerable<System.Collections.Generic.IEnumerable<TSource>>) [placeholder:?unresolved-external]
	Parameter source :: source: ?unresolved-external(System.Collections.Generic.IEnumerable<TSource>) [placeholder:?unresolved-external]
	Parameter size :: size: i32
	Parameter resultSelector :: resultSelector: ?unresolved-external(System.Func<System.Collections.Generic.IEnumerable<TSource>, TResult>) [placeholder:?unresolved-external]
	Parameter System.Collections.Generic.IEnumerable<TResult> :: System.Collections.Generic.IEnumerable<TResult>: ?unresolved-external(System.Collections.Generic.IEnumerable<TResult>) [placeholder:?unresolved-external]
	Function Batch :: fn Batch(source: ?unresolved-external(System.Collections.Generic.IEnumerable<TSource>), size: i32, resultSelector: ?unresolved-external(System.Func<System.Collections.Generic.IEnumerable<TSource>, TResult>)) -> ?unresolved-external(System.Collections.Generic.IEnumerable<TResult>) [placeholder:?unresolved-external]
	Parameter source :: source: ?unresolved-external(System.Collections.Generic.IEnumerable<TSource>) [placeholder:?unresolved-external]
	Parameter size :: size: i32
	Parameter resultSelector :: resultSelector: ?unresolved-external(System.Func<System.Collections.Generic.IEnumerable<TSource>, TResult>) [placeholder:?unresolved-external]
	Parameter System.Collections.Generic.IEnumerable<TResult> :: System.Collections.Generic.IEnumerable<TResult>: ?unresolved-external(System.Collections.Generic.IEnumerable<TResult>) [placeholder:?unresolved-external]
	Function BatchImpl :: fn BatchImpl(source: ?unresolved-external(System.Collections.Generic.IEnumerable<TSource>), size: i32, resultSelector: ?unresolved-external(System.Func<System.Collections.Generic.IEnumerable<TSource>, TResult>)) -> ?unresolved-external(System.Collections.Generic.IEnumerable<TResult>) [placeholder:?unresolved-external]
summary entities=16 signature rendered=16 unavailable=0 placeholder=11 malformed=0 canonical ok=16 err=0 document ok=16 err=0
package morelinq.source.moreenumerable.assertcount/1.0.2 file=morelinq.source.moreenumerable.assertcount/1.0.2/content/net20/MoreLinq/MoreEnumerable.AssertCount.cs bytes=5239
	Namespace MoreLinq :: namespace MoreLinq
	Record MoreEnumerable :: struct MoreEnumerable
	Parameter source :: source: ?unresolved-external(System.Collections.Generic.IEnumerable<TSource>) [placeholder:?unresolved-external]
	Parameter count :: count: i32
	Parameter errorSelector :: errorSelector: ?unresolved-external(System.Func<int, int, System.Exception>) [placeholder:?unresolved-external]
	Parameter System.Collections.Generic.IEnumerable<TSource> :: System.Collections.Generic.IEnumerable<TSource>: ?unresolved-external(System.Collections.Generic.IEnumerable<TSource>) [placeholder:?unresolved-external]
	Function AssertCountImpl :: fn AssertCountImpl(source: ?unresolved-external(System.Collections.Generic.IEnumerable<TSource>), count: i32, errorSelector: ?unresolved-external(System.Func<int, int, System.Exception>)) -> ?unresolved-external(System.Collections.Generic.IEnumerable<TSource>) [placeholder:?unresolved-external]
	Parameter source :: source: ?unresolved-external(System.Collections.Generic.IEnumerable<TSource>) [placeholder:?unresolved-external]
	Parameter count :: count: i32
	Parameter errorSelector :: errorSelector: ?unresolved-external(System.Func<int, int, System.Exception>) [placeholder:?unresolved-external]
	Parameter System.Collections.Generic.IEnumerable<TSource> :: System.Collections.Generic.IEnumerable<TSource>: ?unresolved-external(System.Collections.Generic.IEnumerable<TSource>) [placeholder:?unresolved-external]
	Function ExpectingCountYieldingImpl :: fn ExpectingCountYieldingImpl(source: ?unresolved-external(System.Collections.Generic.IEnumerable<TSource>), count: i32, errorSelector: ?unresolved-external(System.Func<int, int, System.Exception>)) -> ?unresolved-external(System.Collections.Generic.IEnumerable<TSource>) [placeholder:?unresolved-external]
summary entities=12 signature rendered=12 unavailable=0 placeholder=8 malformed=0 canonical ok=12 err=0 document ok=12 err=0
package morelinq.source.moreenumerable.groupadjacent/1.0.1 file=morelinq.source.moreenumerable.groupadjacent/1.0.1/content/net20/MoreLinq/MoreEnumerable.GroupAdjacent.cs bytes=11660
	Namespace MoreLinq :: namespace MoreLinq
	Record MoreEnumerable :: struct MoreEnumerable
	Record Grouping :: struct Grouping
	Record Grouping :: struct Grouping
	Parameter source :: source: ?unresolved-external(System.Collections.Generic.IEnumerable<TSource>) [placeholder:?unresolved-external]
	Parameter keySelector :: keySelector: ?unresolved-external(System.Func<TSource, TKey>) [placeholder:?unresolved-external]
	Parameter System.Collections.Generic.IEnumerable<System.Linq.IGrouping<TKey, TSource>> :: System.Collections.Generic.IEnumerable<System.Linq.IGrouping<TKey, TSource>>: ?unresolved-external(System.Collections.Generic.IEnumerable<System.Linq.IGrouping<TKey, TSource>>) [placeholder:?unresolved-external]
	Function GroupAdjacent :: fn GroupAdjacent(source: ?unresolved-external(System.Collections.Generic.IEnumerable<TSource>), keySelector: ?unresolved-external(System.Func<TSource, TKey>)) -> ?unresolved-external(System.Collections.Generic.IEnumerable<System.Linq.IGrouping<TKey, TSource>>) [placeholder:?unresolved-external]
	Parameter source :: source: ?unresolved-external(System.Collections.Generic.IEnumerable<TSource>) [placeholder:?unresolved-external]
	Parameter keySelector :: keySelector: ?unresolved-external(System.Func<TSource, TKey>) [placeholder:?unresolved-external]
	Parameter comparer :: comparer: ?unresolved-external(System.Collections.Generic.IEqualityComparer<TKey>) [placeholder:?unresolved-external]
	Parameter System.Collections.Generic.IEnumerable<System.Linq.IGrouping<TKey, TSource>> :: System.Collections.Generic.IEnumerable<System.Linq.IGrouping<TKey, TSource>>: ?unresolved-external(System.Collections.Generic.IEnumerable<System.Linq.IGrouping<TKey, TSource>>) [placeholder:?unresolved-external]
	Function GroupAdjacent :: fn GroupAdjacent(source: ?unresolved-external(System.Collections.Generic.IEnumerable<TSource>), keySelector: ?unresolved-external(System.Func<TSource, TKey>), comparer: ?unresolved-external(System.Collections.Generic.IEqualityComparer<TKey>)) -> ?unresolved-external(System.Collections.Generic.IEnumerable<System.Linq.IGrouping<TKey, TSource>>) [placeholder:?unresolved-external]
	Parameter source :: source: ?unresolved-external(System.Collections.Generic.IEnumerable<TSource>) [placeholder:?unresolved-external]
	Parameter keySelector :: keySelector: ?unresolved-external(System.Func<TSource, TKey>) [placeholder:?unresolved-external]
	Parameter elementSelector :: elementSelector: ?unresolved-external(System.Func<TSource, TElement>) [placeholder:?unresolved-external]
	Parameter System.Collections.Generic.IEnumerable<System.Linq.IGrouping<TKey, TElement>> :: System.Collections.Generic.IEnumerable<System.Linq.IGrouping<TKey, TElement>>: ?unresolved-external(System.Collections.Generic.IEnumerable<System.Linq.IGrouping<TKey, TElement>>) [placeholder:?unresolved-external]
	Function GroupAdjacent :: fn GroupAdjacent(source: ?unresolved-external(System.Collections.Generic.IEnumerable<TSource>), keySelector: ?unresolved-external(System.Func<TSource, TKey>), elementSelector: ?unresolved-external(System.Func<TSource, TElement>)) -> ?unresolved-external(System.Collections.Generic.IEnumerable<System.Linq.IGrouping<TKey, TElement>>) [placeholder:?unresolved-external]
	Parameter source :: source: ?unresolved-external(System.Collections.Generic.IEnumerable<TSource>) [placeholder:?unresolved-external]
	Parameter keySelector :: keySelector: ?unresolved-external(System.Func<TSource, TKey>) [placeholder:?unresolved-external]
	Parameter elementSelector :: elementSelector: ?unresolved-external(System.Func<TSource, TElement>) [placeholder:?unresolved-external]
	Parameter comparer :: comparer: ?unresolved-external(System.Collections.Generic.IEqualityComparer<TKey>) [placeholder:?unresolved-external]
	Parameter System.Collections.Generic.IEnumerable<System.Linq.IGrouping<TKey, TElement>> :: System.Collections.Generic.IEnumerable<System.Linq.IGrouping<TKey, TElement>>: ?unresolved-external(System.Collections.Generic.IEnumerable<System.Linq.IGrouping<TKey, TElement>>) [placeholder:?unresolved-external]
	Function GroupAdjacent :: fn GroupAdjacent(source: ?unresolved-external(System.Collections.Generic.IEnumerable<TSource>), keySelector: ?unresolved-external(System.Func<TSource, TKey>), elementSelector: ?unresolved-external(System.Func<TSource, TElement>), comparer: ?unresolved-external(System.Collections.Generic.IEqualityComparer<TKey>)) -> ?unresolved-external(System.Collections.Generic.IEnumerable<System.Linq.IGrouping<TKey, TElement>>) [placeholder:?unresolved-external]
	Parameter source :: source: ?unresolved-external(System.Collections.Generic.IEnumerable<TSource>) [placeholder:?unresolved-external]
summary entities=25 signature rendered=25 unavailable=0 placeholder=21 malformed=0 canonical ok=25 err=0 document ok=25 err=0
package morelinq.source.moreenumerable.todelimitedstring/1.1.2 file=morelinq.source.moreenumerable.todelimitedstring/1.1.2/content/net20/MoreLinq/MoreEnumerable.ToDelimitedString.g.cs bytes=34127
	Namespace MoreLinq :: namespace MoreLinq
	Record MoreEnumerable :: struct MoreEnumerable
	Record StringBuilderAppenders :: struct StringBuilderAppenders
	Parameter source :: source: ?unresolved-external(System.Collections.Generic.IEnumerable<bool>) [placeholder:?unresolved-external]
	Parameter string :: string: str
	Function ToDelimitedString :: fn ToDelimitedString(source: ?unresolved-external(System.Collections.Generic.IEnumerable<bool>)) -> str [placeholder:?unresolved-external]
	Field Boolean :: Boolean: ?unresolved-external(System.Func<System.Text.StringBuilder, bool, System.Text.StringBuilder>) [placeholder:?unresolved-external]
	Parameter source :: source: ?unresolved-external(System.Collections.Generic.IEnumerable<bool>) [placeholder:?unresolved-external]
	Parameter delimiter :: delimiter: str
	Parameter string :: string: str
	Function ToDelimitedString :: fn ToDelimitedString(source: ?unresolved-external(System.Collections.Generic.IEnumerable<bool>), delimiter: str) -> str [placeholder:?unresolved-external]
	Parameter source :: source: ?unresolved-external(System.Collections.Generic.IEnumerable<byte>) [placeholder:?unresolved-external]
	Parameter string :: string: str
	Function ToDelimitedString :: fn ToDelimitedString(source: ?unresolved-external(System.Collections.Generic.IEnumerable<byte>)) -> str [placeholder:?unresolved-external]
	Field Byte :: Byte: ?unresolved-external(System.Func<System.Text.StringBuilder, byte, System.Text.StringBuilder>) [placeholder:?unresolved-external]
	Parameter source :: source: ?unresolved-external(System.Collections.Generic.IEnumerable<byte>) [placeholder:?unresolved-external]
	Parameter delimiter :: delimiter: str
	Parameter string :: string: str
	Function ToDelimitedString :: fn ToDelimitedString(source: ?unresolved-external(System.Collections.Generic.IEnumerable<byte>), delimiter: str) -> str [placeholder:?unresolved-external]
	Parameter source :: source: ?unresolved-external(System.Collections.Generic.IEnumerable<char>) [placeholder:?unresolved-external]
	Parameter string :: string: str
	Function ToDelimitedString :: fn ToDelimitedString(source: ?unresolved-external(System.Collections.Generic.IEnumerable<char>)) -> str [placeholder:?unresolved-external]
	Field Char :: Char: ?unresolved-external(System.Func<System.Text.StringBuilder, char, System.Text.StringBuilder>) [placeholder:?unresolved-external]
	Parameter source :: source: ?unresolved-external(System.Collections.Generic.IEnumerable<char>) [placeholder:?unresolved-external]
	Parameter delimiter :: delimiter: str
summary entities=25 signature rendered=25 unavailable=0 placeholder=14 malformed=0 canonical ok=25 err=0 document ok=25 err=0
package morelinq.source.moreenumerable.consume/1.0.1 file=morelinq.source.moreenumerable.consume/1.0.1/content/net20/MoreLinq/MoreEnumerable.Consume.cs bytes=1430
	Namespace MoreLinq :: namespace MoreLinq
	Record MoreEnumerable :: struct MoreEnumerable
	Parameter source :: source: ?unresolved-external(System.Collections.Generic.IEnumerable<T>) [placeholder:?unresolved-external]
	Function Consume :: fn Consume(source: ?unresolved-external(System.Collections.Generic.IEnumerable<T>)) [placeholder:?unresolved-external]
summary entities=4 signature rendered=4 unavailable=0 placeholder=2 malformed=0 canonical ok=4 err=0 document ok=4 err=0
";

// # KNOWN GAPS (clang, pinned 2026-09):
// # - The engine bar (crates/engine/tests/clang_lane.rs:587
// #   c_render_is_visibility_free_qualified_and_byte_stable) expects a
// #   struct member body; the neutral formatter renders the head only
// #   (render.rs record arm), so that engine test is red today. This
// #   snapshot pins the current head-only state.
// # - Macros render `?unannotated`, extern globals `?external`, and
// #   template/macro-dependent pointees `?oracle-gap` (upstream rows).
// # - Shell-sensitive: pinned under `.#development`, where brotli's
// #   `uint8_t` does not resolve and clang recovers it as `int`
// #   (`const i32[]`). Under `.#complete` the typedef resolves and the same
// #   rows render `[const unsigned-char[8]; 122784]`.
#[rustfmt::skip]
const EXPECTED_CLANG: &str = r"# clang render snapshot
package cJSON file=cJSON/tests/unity/test/tests/testunity.c bytes=123592
	Macro EXPECT_ABORT_BEGIN :: macro EXPECT_ABORT_BEGIN: ?unannotated [placeholder:?unannotated]
	Macro VERIFY_FAILS_END :: macro VERIFY_FAILS_END: ?unannotated [placeholder:?unannotated]
	Macro VERIFY_IGNORES_END :: macro VERIFY_IGNORES_END: ?unannotated [placeholder:?unannotated]
	Macro USING_SPY_AS :: macro USING_SPY_AS: ?unannotated [placeholder:?unannotated]
	Macro ASSIGN_VALUE :: macro ASSIGN_VALUE: ?unannotated [placeholder:?unannotated]
	Macro VAL_putcharSpy :: macro VAL_putcharSpy: ?unannotated [placeholder:?unannotated]
	Macro EXPAND_AND_USE_2ND :: macro EXPAND_AND_USE_2ND: ?unannotated [placeholder:?unannotated]
	Macro SECOND_PARAM :: macro SECOND_PARAM: ?unannotated [placeholder:?unannotated]
	Macro TEST_ASSERT_EQUAL_PRINT_NUMBERS :: macro TEST_ASSERT_EQUAL_PRINT_NUMBERS: ?unannotated [placeholder:?unannotated]
	Macro TEST_ASSERT_EQUAL_PRINT_UNSIGNED_NUMBERS :: macro TEST_ASSERT_EQUAL_PRINT_UNSIGNED_NUMBERS: ?unannotated [placeholder:?unannotated]
	Macro TEST_ASSERT_EQUAL_PRINT_FLOATING :: macro TEST_ASSERT_EQUAL_PRINT_FLOATING: ?unannotated [placeholder:?unannotated]
	Static f_zero :: static f_zero: const i32
	Static d_zero :: static d_zero: const i32
	Function startPutcharSpy :: fn startPutcharSpy()
	Function endPutcharSpy :: fn endPutcharSpy()
	Parameter getBufferPutcharSpy :: getBufferPutcharSpy: c-char-signed[8]*
	Function getBufferPutcharSpy :: fn getBufferPutcharSpy() -> c-char-signed[8]*
	Static SetToOneToFailInTearDown :: static SetToOneToFailInTearDown: i32
	Static SetToOneMeanWeAlreadyCheckedThisGuy :: static SetToOneMeanWeAlreadyCheckedThisGuy: i32
	Function setUp :: fn setUp()
	Function tearDown :: fn tearDown()
	Function testUnitySizeInitializationReminder :: fn testUnitySizeInitializationReminder()
	Field TestFile :: TestFile: const c-char-signed[8]*
	Field CurrentTestName :: CurrentTestName: const c-char-signed[8]*
	Field CurrentDetails1 :: CurrentDetails1: const c-char-signed[8]*
summary entities=25 signature rendered=25 unavailable=0 placeholder=11 malformed=0 canonical ok=25 err=0 document ok=25 err=0
package Unity file=Unity/src/unity.c bytes=80340
	Macro UNITY_PROGMEM :: macro UNITY_PROGMEM: ?unannotated [placeholder:?unannotated]
	Macro UNITY_FAIL_AND_BAIL :: macro UNITY_FAIL_AND_BAIL: ?unannotated [placeholder:?unannotated]
	Macro UNITY_IGNORE_AND_BAIL :: macro UNITY_IGNORE_AND_BAIL: ?unannotated [placeholder:?unannotated]
	Macro RETURN_IF_FAIL_OR_IGNORE :: macro RETURN_IF_FAIL_OR_IGNORE: ?unannotated [placeholder:?unannotated]
	Macro UnityPrintPointlessAndBail :: macro UnityPrintPointlessAndBail: ?unannotated [placeholder:?unannotated]
	Macro UNITY_FLOAT_OR_DOUBLE_WITHIN :: macro UNITY_FLOAT_OR_DOUBLE_WITHIN: ?unannotated [placeholder:?unannotated]
	Macro UNITY_NAN_CHECK :: macro UNITY_NAN_CHECK: ?unannotated [placeholder:?unannotated]
	Macro UNITY_PRINT_EXPECTED_AND_ACTUAL_FLOAT :: macro UNITY_PRINT_EXPECTED_AND_ACTUAL_FLOAT: ?unannotated [placeholder:?unannotated]
	Static Unity :: static Unity: ?external [placeholder:?external]
	Static UnityStrOk :: static UnityStrOk: [const c-char-signed[8]; 3]
	Static UnityStrPass :: static UnityStrPass: [const c-char-signed[8]; 5]
	Static UnityStrFail :: static UnityStrFail: [const c-char-signed[8]; 5]
	Static UnityStrIgnore :: static UnityStrIgnore: [const c-char-signed[8]; 7]
	Static UnityStrNull :: static UnityStrNull: [const c-char-signed[8]; 5]
	Static UnityStrSpacer :: static UnityStrSpacer: [const c-char-signed[8]; 3]
	Static UnityStrExpected :: static UnityStrExpected: [const c-char-signed[8]; 11]
	Static UnityStrWas :: static UnityStrWas: [const c-char-signed[8]; 6]
	Static UnityStrGt :: static UnityStrGt: [const c-char-signed[8]; 21]
	Static UnityStrLt :: static UnityStrLt: [const c-char-signed[8]; 18]
	Static UnityStrOrEqual :: static UnityStrOrEqual: [const c-char-signed[8]; 13]
	Static UnityStrNotEqual :: static UnityStrNotEqual: [const c-char-signed[8]; 21]
	Static UnityStrElement :: static UnityStrElement: [const c-char-signed[8]; 10]
	Static UnityStrByte :: static UnityStrByte: [const c-char-signed[8]; 7]
	Static UnityStrMemory :: static UnityStrMemory: [const c-char-signed[8]; 18]
	Static UnityStrDelta :: static UnityStrDelta: [const c-char-signed[8]; 26]
summary entities=25 signature rendered=25 unavailable=0 placeholder=9 malformed=0 canonical ok=25 err=0 document ok=25 err=0
package STC file=STC/include/stc/priv/cregex_prv.c bytes=41971
	Alias _Rune :: type _Rune = _Rune
	Alias _Token :: type _Token = _Token
	Alias _Reclass :: type _Reclass = _Reclass
	Record _Reinst :: struct _Reinst
	Alias _Reinst :: type _Reinst = _Reinst
	Alias _Reflags :: type _Reflags = _Reflags
	Record _Reprog :: struct _Reprog
	Alias _Reprog :: type _Reprog = _Reprog
	Alias _Resub :: type _Resub = _Resub
	Record _Resublist :: struct _Resublist
	Alias _Resublist :: type _Resublist = _Resublist
	Record _Relist :: struct _Relist
	Alias _Relist :: type _Relist = _Relist
	Record _Reljunk :: struct _Reljunk
	Alias _Reljunk :: type _Reljunk = _Reljunk
	Record _Node :: struct _Node
	Alias _Node :: type _Node = _Node
	Record _Parser :: struct _Parser
	Alias _Parser :: type _Parser = _Parser
	Macro STC_CREGEX_PRV_C_INCLUDED :: macro STC_CREGEX_PRV_C_INCLUDED: ?unannotated [placeholder:?unannotated]
	Macro _NCLASS :: macro _NCLASS: ?unannotated [placeholder:?unannotated]
	Macro _NSUBEXP :: macro _NSUBEXP: ?unannotated [placeholder:?unannotated]
	Macro _NCCRUNE :: macro _NCCRUNE: ?unannotated [placeholder:?unannotated]
	Macro _LISTSIZE :: macro _LISTSIZE: ?unannotated [placeholder:?unannotated]
	Macro _BIGLISTSIZE :: macro _BIGLISTSIZE: ?unannotated [placeholder:?unannotated]
summary entities=25 signature rendered=25 unavailable=0 placeholder=6 malformed=0 canonical ok=25 err=0 document ok=25 err=0
package brotli file=brotli/c/common/dictionary.c bytes=472009
	Static kBrotliDictionaryData :: static kBrotliDictionaryData: const i32[]
	Static kBrotliDictionary :: static kBrotliDictionary: const ?external [placeholder:?external]
	Parameter BrotliGetDictionary :: BrotliGetDictionary: const ?external* [placeholder:?external]
	Function BrotliGetDictionary :: fn BrotliGetDictionary() -> const ?external* [placeholder:?external]
	Parameter data :: data: const i32*
	Function BrotliSetDictionaryData :: fn BrotliSetDictionaryData(data: const i32*)
	Module dictionary.h :: mod dictionary.h
	Module platform.h :: mod platform.h
summary entities=8 signature rendered=8 unavailable=0 placeholder=3 malformed=0 canonical ok=8 err=0 document ok=8 err=0
package pugixml file=pugixml/src/pugixml.cpp bytes=346827
	Namespace PUGI_IMPL_NS_BEGIN :: namespace PUGI_IMPL_NS_BEGIN
	Namespace PUGI_IMPL_NS_BEGIN :: namespace PUGI_IMPL_NS_BEGIN
	Record xml_memory_management_function_storage :: struct xml_memory_management_function_storage
	Alias xml_memory :: type xml_memory = xml_memory
	Record auto_deleter :: struct auto_deleter
	Alias D :: type D = D
	Record xml_allocator :: struct xml_allocator
	Record xml_memory_page :: struct xml_memory_page
	Record xml_memory_string_header :: struct xml_memory_string_header
	Record xml_attribute_struct :: struct xml_attribute_struct
	Record xml_node_struct :: struct xml_node_struct
	Record xml_extra_buffer :: struct xml_extra_buffer
	Record xml_document_struct :: struct xml_document_struct
	Function get_allocator :: fn get_allocator(const ?oracle-gap*) -> xml_allocator& [placeholder:?oracle-gap]
	Function get_document :: fn get_document(const ?oracle-gap*) -> xml_document_struct& [placeholder:?oracle-gap]
	Record opt_false :: struct opt_false
	Record opt_true :: struct opt_true
	Record utf8_counter :: struct utf8_counter
	Alias value_type :: type value_type = value_type
	Record utf8_writer :: struct utf8_writer
	Alias value_type :: type value_type = value_type
	Record utf16_counter :: struct utf16_counter
	Alias value_type :: type value_type = value_type
	Record utf16_writer :: struct utf16_writer
	Alias value_type :: type value_type = value_type
summary entities=25 signature rendered=25 unavailable=0 placeholder=2 malformed=0 canonical ok=25 err=0 document ok=25 err=0
";
