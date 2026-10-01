//! The Rust semantic lowering keeps the declaration facts a reader weighs:
//! every `#[deprecated]` spelling is staged, exactly as written, on the
//! generic item attribute plane (with that plane marked captured, so "no
//! such attribute" is a statement), and documentation keeps its line
//! boundaries so a `# Errors` heading still starts its own line.

use std::{
    fs,
    path::PathBuf,
    sync::atomic::{AtomicBool, AtomicU64, Ordering},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use backend_engine::driver::{
    CompileControl, CompileFailure, CompileRequest, CompileScratch, NativeTool, ResolvedToolchain,
    SemanticAuthorityInput, compile_ir,
};
use backend_frontend_rust::legacy::{
    RustFeatureControl, RustProject, RustToolchain, SourceByteLimit,
};
use backend_semantic::ir::{
    DocFragment, EntityId, FactAvailability, Ir, ItemKind, SemanticReader,
};
use backend_semantic::vocabulary::{LanguageProfile, RustEdition, Stage};

const FIXTURE_BODY: &str = r#"//! Fixture crate docs.

/// Makes one.
///
/// # Errors
/// Fails when the input is empty.
#[deprecated(since = "1.2.0", note = "use `fresh`")]
#[inline]
pub fn stale() -> u8 {
    1
}

pub struct Plain {
    #[deprecated = "read `name` instead"]
    pub label: u8,
}

pub fn current() -> u8 {
    2
}
"#;

static FIXTURE_SEQUENCE: AtomicU64 = AtomicU64::new(0);

type Failure = Box<dyn std::error::Error>;

fn failure_label(failure: &CompileFailure<'_>) -> &'static str {
    match failure {
        CompileFailure::Authority { .. } => "authority",
        CompileFailure::AuthorityInputRequired { .. } => "authority-input-required",
        CompileFailure::AuthorityInputProfileMismatch { .. } => "authority-profile-mismatch",
        CompileFailure::LoweringUnsupported { cause, .. } => match cause {
            backend_semantic::vocabulary::LoweringUnsupported::FactRejected { .. } => "fact-rejected",
            backend_semantic::vocabulary::LoweringUnsupported::CSharpProjection { .. } => {
                "csharp-projection"
            }
            _ => "lowering-unsupported",
        },
        CompileFailure::ExtensionAtomUnbound { .. } => "extension-atom-unbound",
        CompileFailure::ExtensionTypeParametersUnbound { .. } => {
            "extension-type-parameters-unbound"
        }
        CompileFailure::Build { .. } => "build",
        CompileFailure::Prepare { .. } => "prepare",
        CompileFailure::Write { .. } => "write",
        CompileFailure::Validate { .. } => "validate",
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
        CompileFailure::ClangProjection { .. } => "clang-projection",
    }
}

fn compile_fixture() -> Result<Ir, Failure> {
    let nonce = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos();
    let sequence = FIXTURE_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let root = std::env::temp_dir().join(format!(
        "nudox-rust-facts-{nonce}-{}-{sequence}",
        std::process::id()
    ));
    fs::create_dir_all(root.join("src"))?;
    fs::write(
        root.join("Cargo.toml"),
        "[package]\nname = \"facts_fixture\"\nversion = \"0.1.0\"\nedition = \"2024\"\n",
    )?;
    let source_path = root.join("src/lib.rs");
    fs::write(&source_path, FIXTURE_BODY)?;
    let tool = std::env::var_os("RUSTC")
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .or_else(|| {
            std::env::split_paths(&std::env::var_os("PATH")?)
                .map(|directory| directory.join("rustc"))
                .find(|path| path.is_file())
                .and_then(|path| path.canonicalize().ok())
        })
        .ok_or("no absolute rustc was available")?;
    let frontend = RustToolchain::discover(&tool)?;
    let project =
        RustProject::open_with_source(&root, &source_path, &frontend, RustEdition::Rust2024)?;
    let resolved =
        ResolvedToolchain::from_version(NativeTool::Rustc, &tool, b"rust-declaration-facts")?;
    let cancelled = AtomicBool::new(false);
    let mut diagnostic = [0; 0];
    let result = compile_ir(
        CompileRequest {
            profile: LanguageProfile::Rust(RustEdition::Rust2024),
            stage: Stage::LowerIr,
            source: FIXTURE_BODY.as_bytes(),
            declaration_scope: backend_engine::driver::DeclarationScope::fixture(),
            toolchain: backend_engine::driver::ToolchainSelection::ResolvedNative(resolved),
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
            native_work: &root,
        },
    )
    .map(|compiled| compiled.ir)
    .map_err(|failure| format!("compile failed: {}", failure_label(&failure)));
    fs::remove_dir_all(&root)?;
    Ok(result?)
}

fn item(ir: &Ir, name: &[u8], kind: ItemKind) -> Result<EntityId, Failure> {
    ir.items()
        .find(|item| item.name() == name && item.kind() == kind)
        .map(|item| item.id())
        .ok_or_else(|| format!("no {kind:?} named {}", String::from_utf8_lossy(name)).into())
}

/// The attributes staged on one entity, as UTF-8 text, and whether the
/// attribute plane was captured for it.
fn attributes(ir: &Ir, id: EntityId) -> Result<(Vec<String>, FactAvailability), Failure> {
    let entity = SemanticReader::entity(ir, id).ok_or("entity row absent")?;
    let item = ir.item(id).ok_or("item absent")?;
    let texts = item
        .attributes()
        .iter()
        .map(|atom| {
            ir.atom(*atom)
                .map(|bytes| String::from_utf8_lossy(bytes).into_owned())
                .ok_or("attribute atom absent")
        })
        .collect::<Result<Vec<_>, _>>()?;
    Ok((texts, entity.authority.attributes))
}

/// Renders documentation with one `\n` per break, text and code verbatim.
fn docs(ir: &Ir, id: EntityId) -> Result<String, Failure> {
    let item = ir.item(id).ok_or("item absent")?;
    let mut out = String::new();
    for fragment in item.docs() {
        match fragment {
            DocFragment::Text(text) | DocFragment::Code(text) => {
                out.push_str(ir.text(*text).ok_or("doc text absent")?);
            }
            DocFragment::Link { label, .. } => {
                out.push_str(ir.text(*label).ok_or("doc label absent")?);
            }
            DocFragment::SoftBreak | DocFragment::HardBreak => out.push('\n'),
        }
    }
    Ok(out)
}

#[test]
fn every_deprecated_spelling_is_staged_exactly_as_written() -> Result<(), Failure> {
    let ir = compile_fixture()?;
    let stale = item(&ir, b"stale", ItemKind::Function)?;
    assert_eq!(
        attributes(&ir, stale)?,
        (
            vec![r#"deprecated(since = "1.2.0", note = "use `fresh`")"#.to_owned()],
            FactAvailability::Captured
        ),
        "`#[inline]` is not a fact a reader weighs, and is not staged"
    );
    let label = item(&ir, b"label", ItemKind::Field)?;
    assert_eq!(
        attributes(&ir, label)?,
        (
            vec![r#"deprecated = "read `name` instead""#.to_owned()],
            FactAvailability::Captured
        )
    );
    let current = item(&ir, b"current", ItemKind::Function)?;
    assert_eq!(
        attributes(&ir, current)?,
        (Vec::new(), FactAvailability::Captured),
        "a declaration written without the attribute states that it has none"
    );
    Ok(())
}

#[test]
fn documentation_keeps_its_line_boundaries() -> Result<(), Failure> {
    let ir = compile_fixture()?;
    let stale = item(&ir, b"stale", ItemKind::Function)?;
    assert_eq!(
        docs(&ir, stale)?,
        "Makes one.\n\n# Errors\nFails when the input is empty."
    );
    Ok(())
}
