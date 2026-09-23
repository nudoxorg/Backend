//! Golden rendering over the Go semantic authority lane.

use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
    sync::atomic::AtomicBool,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use backend_engine::driver::{
    CompileControl, CompileRequest, CompileScratch, NativeTool, ResolvedToolchain,
    SemanticAuthorityInput, ToolchainSelection, compile_ir,
};
use backend_semantic::ir::{EntityId, Ir, ItemKind};
use backend_frontend_go::legacy::GoOracle;
use backend_semantic::vocabulary::{GoVersion, LanguageProfile, Stage};
use thiserror::Error;

/// Distinguishes fixture directories created by parallel test threads within
/// one process, where the clock and pid alone can repeat.
static FIXTURE_SEQUENCE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

fn fixture_sequence() -> u64 {
    FIXTURE_SEQUENCE.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
}

const SOURCE: &[u8] = br#"// Package demo documents the semantic lane.
package demo

// Embedded supplies the identity.
type Embedded struct {
	ID int
}

// Widget is a documented record.
type Widget struct {
	Name string `json:"name"`
	Embedded
}

// Base is the embedded interface.
type Base interface {
	Base() bool
}

// Reader combines an explicit method and an embedded interface.
type Reader interface {
	Read(size int) string
	Base
}

// Box retains one value.
type Box[T any] struct {
	Value T
}

// Combine returns its first argument.
func Combine[T any, U interface{ ~int | ~string }](left T, right U) T {
	return left
}

// Split names both of its results.
func Split(value string) (head string, count int) { return value, len(value) }

// Pair leaves both of its results unnamed.
func Pair() (string, int) { return "", 0 }

// Value is a value-receiver method.
func (widget Widget) Value() string { return widget.Name }

// Pointer is a pointer-receiver method.
func (widget *Widget) Pointer() int { return widget.ID }

const (
	// First is the first constant.
	First = iota
	Second
)
"#;

#[derive(Debug, Error)]
enum TestError {
    #[error("fixture I/O failed: {0}")]
    Io(#[source] std::io::Error),
    #[error("Go toolchain command failed at {path}: {source}")]
    Tool {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("Go toolchain is unavailable at {path}")]
    MissingTool { path: PathBuf },
    #[error("Go toolchain resolution failed")]
    Resolve,
    #[error("Go authority image failed: {0}")]
    Oracle(#[source] backend_frontend_go::legacy::OracleError),
    #[error("compile_ir failed: {0}")]
    Compile(String),
    #[error("entity {name:?} was not found")]
    MissingEntity { name: &'static str },
    #[error("render mismatch for {name}: expected {expected:?}, actual {actual:?}")]
    Mismatch {
        name: &'static str,
        expected: String,
        actual: String,
    },
    #[error("rendering falsifier did not change {0}")]
    Unchanged(&'static str),
}

fn compile_source(source: &[u8]) -> Result<Ir, TestError> {
    let compiler = Path::new("/Users/mileswirht/nudox-tools/go/bin/go");
    if !compiler.is_file() {
        return Err(TestError::MissingTool {
            path: compiler.to_owned(),
        });
    }
    let version = Command::new(compiler)
        .arg("version")
        .output()
        .map_err(|source| TestError::Tool {
            path: compiler.to_owned(),
            source,
        })?;
    let version_bytes = if version.stdout.is_empty() {
        version.stderr.as_slice()
    } else {
        version.stdout.as_slice()
    };
    let toolchain =
        ResolvedToolchain::from_version(NativeTool::GoCompiler, compiler, version_bytes)
            .map_err(|_| TestError::Resolve)?;
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| TestError::Resolve)?
        .as_nanos();
    let root =
        std::env::temp_dir().join(format!(
        "nudox-go-render-{nonce}-{}-{}",
        std::process::id(),
        fixture_sequence()
    ));
    fs::create_dir_all(&root).map_err(TestError::Io)?;
    fs::write(
        root.join("go.mod"),
        "module example.com/render\n\ngo 1.23\n",
    )
    .map_err(TestError::Io)?;
    let source_path = root.join("main.go");
    fs::write(&source_path, source).map_err(TestError::Io)?;
    let image = GoOracle::default()
        .authority_image(&source_path, &root)
        .map_err(TestError::Oracle)?;
    let cancelled = AtomicBool::new(false);
    let mut diagnostic = [0_u8; 4096];
    let result = compile_ir(
        CompileRequest {
            profile: LanguageProfile::Go(GoVersion::Go125),
            stage: Stage::LowerIr,
            source,
            declaration_scope: backend_engine::driver::DeclarationScope::fixture(),
            toolchain: ToolchainSelection::ResolvedNative(toolchain),
            authority: SemanticAuthorityInput::Go { image: &image },
            control: CompileControl {
                deadline: Instant::now() + Duration::from_secs(60),
                cancelled: &cancelled,
            },
        },
        CompileScratch {
            diagnostic_output: &mut diagnostic,
            native_work: &root,
        },
    )
    .map(|compiled| compiled.ir)
    .map_err(|failure| TestError::Compile(format!("{failure:?}")));
    let cleanup = fs::remove_dir_all(&root).map_err(TestError::Io);
    cleanup?;
    result
}

fn entity(ir: &Ir, name: &'static str, kind: ItemKind) -> Result<EntityId, TestError> {
    ir.items()
        .find(|item| item.kind() == kind && item.name() == name.as_bytes())
        .map(|item| item.id())
        .ok_or(TestError::MissingEntity { name })
}

fn render_signature(ir: &Ir, name: &'static str, kind: ItemKind) -> Result<String, TestError> {
    Ok(ir
        .signature(entity(ir, name, kind)?)
        .ok_or(TestError::MissingEntity { name })?
        .to_string())
}

fn render_docs(ir: &Ir, name: &'static str, kind: ItemKind) -> Result<String, TestError> {
    Ok(ir
        .display_docs(entity(ir, name, kind)?)
        .ok_or(TestError::MissingEntity { name })?
        .to_string())
}

#[test]
fn go_lane_renders_exact_declarations_and_docs() -> Result<(), TestError> {
    let ir = compile_source(SOURCE)?;
    let goldens = [
        (
            "Widget signature",
            render_signature(&ir, "Widget", ItemKind::Record)?,
            "struct Widget",
        ),
        (
            "Widget type",
            {
                let item = ir
                    .item(entity(&ir, "Widget", ItemKind::Record)?)
                    .ok_or(TestError::MissingEntity { name: "Widget" })?;
                let ty = item
                    .semantic_type()
                    .ok_or(TestError::MissingEntity { name: "Widget" })?;
                ir.display_type(ty)
                    .ok_or(TestError::MissingEntity { name: "Widget" })?
                    .to_string()
            },
            "Widget",
        ),
        (
            "Name field type",
            {
                let item = ir
                    .item(entity(&ir, "Name", ItemKind::Field)?)
                    .ok_or(TestError::MissingEntity { name: "Name" })?;
                let ty = item
                    .semantic_type()
                    .ok_or(TestError::MissingEntity { name: "Name" })?;
                ir.display_type(ty)
                    .ok_or(TestError::MissingEntity { name: "Name" })?
                    .to_string()
            },
            "str",
        ),
        (
            "Reader method-set member",
            render_signature(&ir, "Read", ItemKind::Function)?,
            "fn Read(size: native-int) -> str",
        ),
        (
            "generic function",
            render_signature(&ir, "Combine", ItemKind::Function)?,
            "fn Combine(left: T, right: U) -> T",
        ),
        (
            "pointer method",
            render_signature(&ir, "Pointer", ItemKind::Function)?,
            "fn Pointer() -> native-int",
        ),
        (
            "named results",
            render_signature(&ir, "Split", ItemKind::Function)?,
            "fn Split(value: str) -> (head: str, count: native-int)",
        ),
        (
            "unnamed results",
            render_signature(&ir, "Pair", ItemKind::Function)?,
            "fn Pair() -> (str, native-int)",
        ),
        (
            "function docs",
            render_docs(&ir, "Combine", ItemKind::Function)?,
            "Combine returns its first argument.",
        ),
        (
            "constant",
            render_signature(&ir, "First", ItemKind::Constant)?,
            // An `iota` constant is an untyped integer constant: exact and
            // arbitrary precision, so it renders as the `integer` builtin.
            "const First: integer",
        ),
    ];
    for (name, actual, expected) in goldens {
        if actual != expected {
            return Err(TestError::Mismatch {
                name,
                expected: expected.to_owned(),
                actual,
            });
        }
    }

    let mut field_mutation = SOURCE.to_vec();
    // The mutation must keep the module type-correct: the W13 oracle
    // refusal rejects images whose packages no longer type-check, so
    // changing `Name`'s type coherently changes `Value`'s return type.
    let old_field = b"Name string `json:\"name\"`";
    let new_field = b"Name int `json:\"name\"`";
    let old_method = b"func (widget Widget) Value() string { return widget.Name }";
    let new_method = b"func (widget Widget) Value() int { return widget.Name }";
    let at = field_mutation
        .windows(old_field.len())
        .position(|window| window == old_field)
        .ok_or(TestError::Unchanged("field type"))?;
    field_mutation.splice(at..at + old_field.len(), new_field.iter().copied());
    let method_at = field_mutation
        .windows(old_method.len())
        .position(|window| window == old_method)
        .ok_or(TestError::Unchanged("value method"))?;
    field_mutation.splice(
        method_at..method_at + old_method.len(),
        new_method.iter().copied(),
    );
    let mutated = compile_source(&field_mutation)?;
    let original_field = {
        let item = ir
            .item(entity(&ir, "Name", ItemKind::Field)?)
            .ok_or(TestError::MissingEntity { name: "Name" })?;
        let ty = item
            .semantic_type()
            .ok_or(TestError::MissingEntity { name: "Name" })?;
        ir.display_type(ty)
            .ok_or(TestError::MissingEntity { name: "Name" })?
            .to_string()
    };
    let mutated_field = {
        let item = mutated
            .item(entity(&mutated, "Name", ItemKind::Field)?)
            .ok_or(TestError::MissingEntity { name: "Name" })?;
        let ty = item
            .semantic_type()
            .ok_or(TestError::MissingEntity { name: "Name" })?;
        mutated
            .display_type(ty)
            .ok_or(TestError::MissingEntity { name: "Name" })?
            .to_string()
    };
    if mutated_field != "native-int" || original_field == mutated_field {
        return Err(TestError::Mismatch {
            name: "mutated field type",
            expected: "native-int".to_owned(),
            actual: mutated_field,
        });
    }

    let mut doc_mutation = SOURCE.to_vec();
    let old_doc = b"// Widget is a documented record.";
    let new_doc = b"// Widget documentation changed.";
    let at = doc_mutation
        .windows(old_doc.len())
        .position(|window| window == old_doc)
        .ok_or(TestError::Unchanged("doc comment"))?;
    doc_mutation.splice(at..at + old_doc.len(), new_doc.iter().copied());
    let mutated = compile_source(&doc_mutation)?;
    let original_doc = render_docs(&ir, "Widget", ItemKind::Record)?;
    let mutated_doc = render_docs(&mutated, "Widget", ItemKind::Record)?;
    if mutated_doc != "Widget documentation changed." || original_doc == mutated_doc {
        return Err(TestError::Mismatch {
            name: "mutated doc comment",
            expected: "Widget documentation changed.".to_owned(),
            actual: mutated_doc,
        });
    }
    Ok(())
}
