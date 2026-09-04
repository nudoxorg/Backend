//! Golden docs.rs-style rendering over the Rust semantic admission lane.

use std::{
    fs,
    path::PathBuf,
    sync::atomic::{AtomicBool, AtomicU64, Ordering},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use compiler_driver::{
    CompileControl, CompileFailure, CompileRequest, CompileScratch, NativeTool, ResolvedToolchain,
    SemanticAuthorityInput, ToolchainResolutionError, ToolchainSelection, compile_ir,
};
use compiler_ir::{EntityId, ItemKind};
use compiler_languages_rust::{
    RustAuthorityError, RustFeatureControl, RustProject, RustToolchain, SourceByteLimit,
};
use compiler_vocabulary::{LanguageProfile, RustEdition, Stage};
use thiserror::Error;

const FIXTURE_BODY: &str = r#"//! Fixture crate docs.

/// A recursive node storing [`Node`] links.
pub struct Node {
    /// The next node through `Option<Box<Node>>`.
    pub next: Option<Box<Node>>,
    /// The weight.
    pub weight: u8,
}

/// Visits nodes.
pub trait Visitor {
    /// Visits one node.
    fn visit(&self, node: &Node) -> u8;
}

pub struct Counter {
    pub seen: u8,
}

pub fn sweep<T: Clone>(items: &[T], visitor: &dyn Visitor) -> Vec<u8> {
    visitor.visit(&items[0])
}

/// Borrows and moves parameters.
pub fn total(shared: &Node, exclusive: &mut Node, moved: Node) -> u64 {
    u64::from(shared.weight) + u64::from(exclusive.weight) + u64::from(moved.weight)
}

pub fn apply(callback: fn(u8) -> u8, seed: u8) -> u8 {
    callback(seed)
}

fn private_total(shared: &Node, exclusive: &mut Node, moved: Node) -> u64 {
    total(shared, exclusive, moved)
}
"#;

static FIXTURE_SEQUENCE: AtomicU64 = AtomicU64::new(0);

const NODE_SIGNATURE: &str = "pub struct Node";
const VISIT_SIGNATURE: &str = "fn visit(self: &Self, node: &Node) -> u8";
const TOTAL_SIGNATURE: &str =
    "pub fn total(shared: &Node, exclusive: &mut Node, moved: Node) -> u64";
const NODE_DOCS: &str = "A recursive node storing [Node](struct.Node.html) links.";
const U8_TYPE: &str = "u8";
const NEXT_TYPE: &str = "Option<Box<Node>>";
const APPLY_SIGNATURE: &str = "pub fn apply(callback: fn(u8) -> u8, seed: u8) -> u8";
const PRIVATE_TOTAL_SIGNATURE: &str =
    "fn private_total(shared: &Node, exclusive: &mut Node, moved: Node) -> u64";

#[derive(Debug, Error)]
enum TestError {
    #[error("fixture I/O failed during {operation}: {source}")]
    Io {
        operation: &'static str,
        #[source]
        source: std::io::Error,
    },
    #[error("system clock failed: {0}")]
    Clock(#[source] std::time::SystemTimeError),
    #[error("no absolute rustc was available")]
    MissingRustc,
    #[error(transparent)]
    Authority(#[from] RustAuthorityError),
    #[error(transparent)]
    Load(#[from] compiler_languages_rust::LoadError),
    #[error(transparent)]
    Toolchain(#[from] ToolchainResolutionError),
    #[error("compile failed: {0}")]
    Compile(&'static str),
    #[error("entity {name:?} of kind {kind:?} was not found")]
    MissingEntity { name: &'static [u8], kind: ItemKind },
    #[error("{name} differed at line {line}: expected {expected:?}, actual {actual:?}")]
    Mismatch {
        name: &'static str,
        line: usize,
        expected: String,
        actual: String,
    },
}

fn fixture_root() -> Result<PathBuf, TestError> {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(TestError::Clock)?
        .as_nanos();
    let sequence = FIXTURE_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let root = std::env::temp_dir().join(format!("nudox-rust-render-{nonce}-{sequence}"));
    fs::create_dir_all(root.join("src")).map_err(|source| TestError::Io {
        operation: "create fixture",
        source,
    })?;
    fs::write(
        root.join("Cargo.toml"),
        "[package]\nname = \"render_fixture\"\nversion = \"0.1.0\"\nedition = \"2024\"\n",
    )
    .map_err(|source| TestError::Io {
        operation: "write manifest",
        source,
    })?;
    fs::write(root.join("src/lib.rs"), FIXTURE_BODY).map_err(|source| TestError::Io {
        operation: "write source",
        source,
    })?;
    Ok(root)
}

fn failure_label(failure: &CompileFailure<'_>) -> &'static str {
    match failure {
        CompileFailure::Authority { .. } => "authority",
        CompileFailure::AuthorityInputRequired { .. } => "authority-input-required",
        CompileFailure::AuthorityInputProfileMismatch { .. } => "authority-profile-mismatch",
        CompileFailure::LoweringUnsupported { .. } => "lowering-unsupported",
        CompileFailure::ExtensionAtomUnbound { .. } => "extension-atom-unbound",
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
        CompileFailure::FactRejected { .. } => "fact-rejected",
        CompileFailure::CSharpProjection { .. } => "csharp-projection",
    }
}

fn compile_fixture() -> Result<compiler_ir::Ir, TestError> {
    let root = fixture_root()?;
    let source_path = root.join("src/lib.rs");
    let tool = std::env::var_os("RUSTC")
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .or_else(|| {
            std::env::split_paths(&std::env::var_os("PATH")?)
                .map(|directory| directory.join("rustc"))
                .find(|path| path.is_file())
                .and_then(|path| path.canonicalize().ok())
        })
        .ok_or(TestError::MissingRustc)?;
    let frontend = RustToolchain::discover(&tool)?;
    let project =
        RustProject::open_with_source(&root, &source_path, &frontend, RustEdition::Rust2024)?;
    let resolved =
        ResolvedToolchain::from_version(NativeTool::Rustc, &tool, b"rust-render-golden")?;
    let cancelled = AtomicBool::new(false);
    let mut diagnostic = [0; 0];
    let result = compile_ir(
        CompileRequest {
            profile: LanguageProfile::Rust(RustEdition::Rust2024),
            stage: Stage::LowerIr,
            source: FIXTURE_BODY.as_bytes(),
            declaration_scope: compiler_driver::DeclarationScope::fixture(),
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
            native_work: &root,
        },
    )
    .map(|compiled| compiled.ir)
    .map_err(|failure| TestError::Compile(failure_label(&failure)));
    fs::remove_dir_all(&root).map_err(|source| TestError::Io {
        operation: "remove fixture",
        source,
    })?;
    result
}

fn item(ir: &compiler_ir::Ir, name: &'static [u8], kind: ItemKind) -> Result<EntityId, TestError> {
    ir.items()
        .find(|item| item.name() == name && item.kind() == kind)
        .map(|item| item.id())
        .ok_or(TestError::MissingEntity { name, kind })
}

fn assert_golden(name: &'static str, actual: String, expected: &str) -> Result<(), TestError> {
    let line = actual
        .lines()
        .zip(expected.lines())
        .position(|(actual, expected)| actual != expected)
        .unwrap_or_else(|| actual.lines().count().min(expected.lines().count()));
    if actual != expected {
        return Err(TestError::Mismatch {
            name,
            line: line + 1,
            expected: expected.to_owned(),
            actual,
        });
    }
    Ok(())
}

fn signature(ir: &compiler_ir::Ir, id: EntityId, name: &'static str) -> Result<String, TestError> {
    ir.signature(id)
        .map(|display| display.to_string())
        .ok_or(TestError::MissingEntity {
            name: name.as_bytes(),
            kind: ItemKind::Function,
        })
}

#[test]
fn node_record_signature_is_frozen() -> Result<(), TestError> {
    let ir = compile_fixture()?;
    let node = item(&ir, b"Node", ItemKind::Record)?;
    assert_golden(
        "Node signature",
        signature(&ir, node, "Node")?,
        NODE_SIGNATURE,
    )
}

#[test]
fn visitor_method_signature_is_frozen() -> Result<(), TestError> {
    let ir = compile_fixture()?;
    let visit = item(&ir, b"visit", ItemKind::Function)?;
    assert_golden(
        "visit signature",
        signature(&ir, visit, "visit")?,
        VISIT_SIGNATURE,
    )
}

#[test]
fn total_signature_is_frozen() -> Result<(), TestError> {
    let ir = compile_fixture()?;
    let total = item(&ir, b"total", ItemKind::Function)?;
    assert_golden(
        "total signature",
        signature(&ir, total, "total")?,
        TOTAL_SIGNATURE,
    )
}

#[test]
fn node_docs_are_frozen() -> Result<(), TestError> {
    let ir = compile_fixture()?;
    let node = item(&ir, b"Node", ItemKind::Record)?;
    assert_golden(
        "Node docs",
        ir.display_docs(node)
            .map(|display| display.to_string())
            .ok_or(TestError::MissingEntity {
                name: b"Node docs",
                kind: ItemKind::Record,
            })?,
        NODE_DOCS,
    )
}

#[test]
fn u8_type_render_is_frozen() -> Result<(), TestError> {
    let ir = compile_fixture()?;
    let weight = item(&ir, b"weight", ItemKind::Field)?;
    let ty = ir
        .item(weight)
        .and_then(|item| item.semantic_type())
        .ok_or(TestError::MissingEntity {
            name: b"weight type",
            kind: ItemKind::Field,
        })?;
    assert_golden(
        "weight type",
        ir.display_type(ty)
            .map(|display| display.to_string())
            .ok_or(TestError::MissingEntity {
                name: b"weight type",
                kind: ItemKind::Field,
            })?,
        U8_TYPE,
    )
}

#[test]
fn next_type_preserves_foreign_apply_spellings() -> Result<(), TestError> {
    let ir = compile_fixture()?;
    let next = item(&ir, b"next", ItemKind::Field)?;
    let ty =
        ir.item(next)
            .and_then(|item| item.semantic_type())
            .ok_or(TestError::MissingEntity {
                name: b"next type",
                kind: ItemKind::Field,
            })?;
    assert_golden(
        "next type",
        ir.display_type(ty)
            .map(|display| display.to_string())
            .ok_or(TestError::MissingEntity {
                name: b"next type",
                kind: ItemKind::Field,
            })?,
        NEXT_TYPE,
    )
}

#[test]
fn function_pointer_parameter_signature_is_frozen() -> Result<(), TestError> {
    let ir = compile_fixture()?;
    let apply = item(&ir, b"apply", ItemKind::Function)?;
    assert_golden(
        "apply signature",
        signature(&ir, apply, "apply")?,
        APPLY_SIGNATURE,
    )
}

#[test]
fn rust_visibility_is_preserved_for_same_shaped_declarations() -> Result<(), TestError> {
    let ir = compile_fixture()?;
    let public = item(&ir, b"total", ItemKind::Function)?;
    let private = item(&ir, b"private_total", ItemKind::Function)?;
    assert_eq!(
        ir.item(public)
            .ok_or(TestError::MissingEntity {
                name: b"public total",
                kind: ItemKind::Function,
            })?
            .visibility(),
        compiler_ir::Visibility::Public
    );
    assert_eq!(
        ir.item(private)
            .ok_or(TestError::MissingEntity {
                name: b"private total",
                kind: ItemKind::Function,
            })?
            .visibility(),
        compiler_ir::Visibility::Private
    );
    assert_eq!(
        signature(&ir, private, "private_total")?,
        PRIVATE_TOTAL_SIGNATURE
    );
    Ok(())
}
