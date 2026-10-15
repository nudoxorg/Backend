//! Cross-module Rust bindings: lifetime arguments and associated-type bounds
//! lower distinctly when declarations live in a child module.

#![forbid(unsafe_code)]

#[path = "use_case_support/mod.rs"]
mod use_case_support;

use std::{
    fs,
    path::PathBuf,
    sync::atomic::{AtomicBool, AtomicU64, Ordering},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use backend_engine::driver::{
    CompileControl, CompileFailure, CompileRequest, CompileScratch, NativeTool, ResolvedToolchain,
    SemanticAuthorityInput, ToolchainSelection, compile_ir,
};
use backend_frontend_rust::legacy::{
    RustAuthorityError, RustFeatureControl, RustProject, RustToolchain, SourceByteLimit,
};
use backend_semantic::ir::{
    BuiltinType, ConcreteType, EntityKind, ExternalTarget, Ir, SemanticReader, TypeExpr, TypeId,
};
use backend_semantic::vocabulary::{LanguageProfile, RustEdition, Stage};
use thiserror::Error;
use use_case_support::{CompileTimer, finish, skip};

static FIXTURE_SEQUENCE: AtomicU64 = AtomicU64::new(0);

const LIB_ROOT: &str = "pub struct Ref<'a>(&'a u8);\npub trait Iter { type Item; }\n";
const CHILD_RS: &str = "use crate::{Iter, Ref};\npub fn early(_: Ref<'static>) {}\npub fn bound<T: Iter<Item = u8>>() {}\npub fn other<T: Iter<Item = String>>() {}\n";

fn composed_lib_rs() -> String {
    format!("{}mod child {{\n{}}}\n", LIB_ROOT, CHILD_RS)
}

#[derive(Debug, Error)]
enum TestError {
    #[error("system clock preceded its epoch: {0}")]
    Clock(std::time::SystemTimeError),
    #[error("no Rust compiler was available for the fixture")]
    MissingRustc,
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
    #[error("benchmark report failed: {0}")]
    Report(#[from] std::io::Error),
}

fn failure_label(failure: &CompileFailure<'_>) -> &'static str {
    match failure {
        CompileFailure::Authority { .. } => "authority",
        CompileFailure::AuthorityInputRequired { .. } => "authority-input-required",
        CompileFailure::AuthorityInputProfileMismatch { .. } => "authority-profile-mismatch",
        CompileFailure::LoweringUnsupported { cause, .. } => match cause {
            backend_semantic::vocabulary::LoweringUnsupported::FactRejected { .. } => {
                "fact-rejected"
            }
            backend_semantic::vocabulary::LoweringUnsupported::CSharpProjection { .. } => {
                "csharp-projection"
            }
            _ => "lowering-unsupported",
        },
        CompileFailure::ExtensionAtomUnbound { .. } => "extension-atom-unbound",
        CompileFailure::ExtensionTypeParametersUnbound { .. } => {
            "extension-type-parameters-unbound"
        }
        CompileFailure::ClangProjection { .. } => "clang-projection",
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
    }
}

fn resolve_tool() -> Option<PathBuf> {
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

fn fixture_root() -> Result<PathBuf, TestError> {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(TestError::Clock)?
        .as_nanos();
    let sequence = FIXTURE_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let root = std::env::temp_dir().join(format!(
        "nudox-rust-cross-module-{nonce}-{}-{sequence}",
        std::process::id()
    ));
    fs::create_dir_all(root.join("src")).map_err(|source| TestError::Io {
        operation: "create fixture",
        source,
    })?;
    fs::write(
        root.join("Cargo.toml"),
        "[package]\nname = \"cross_module_fixture\"\nversion = \"0.1.0\"\nedition = \"2024\"\n",
    )
    .map_err(|source| TestError::Io {
        operation: "write manifest",
        source,
    })?;
    let lib_rs = composed_lib_rs();
    fs::write(root.join("src/lib.rs"), lib_rs.as_bytes()).map_err(|source| TestError::Io {
        operation: "write crate root",
        source,
    })?;
    fs::write(root.join("src/child.rs"), CHILD_RS).map_err(|source| TestError::Io {
        operation: "write child module",
        source,
    })?;
    Ok(root)
}

fn compile_fixture(root: &PathBuf) -> Result<Ir, TestError> {
    let source_path = root.join("src/lib.rs");
    let source = fs::read(&source_path).map_err(|source| TestError::Io {
        operation: "read crate root",
        source,
    })?;
    let tool = resolve_tool().ok_or(TestError::MissingRustc)?;
    let toolchain = RustToolchain::discover(&tool).map_err(|_| TestError::MissingRustc)?;
    let project =
        RustProject::open_with_source(root, &source_path, &toolchain, RustEdition::Rust2024)?;
    let resolved = ResolvedToolchain::from_version(
        NativeTool::Rustc,
        &tool,
        b"compiler-driver-rust-cross-module-bindings",
    )
    .map_err(|_| TestError::MissingRustc)?;
    let cancelled = AtomicBool::new(false);
    let mut diagnostic = [0; 4096];
    let timer = CompileTimer::start();
    let ir = compile_ir(
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
    .map_err(|failure| TestError::Compile(failure_label(&failure)))?
    .ir;
    finish("rust", "cross-module-bindings", timer.elapsed(), &ir)?;
    Ok(ir)
}

fn entity_in_parent(
    ir: &Ir,
    parent: backend_semantic::ir::EntityId,
    name: &[u8],
    kind: EntityKind,
) -> Result<backend_semantic::ir::EntityId, TestError> {
    ir.items()
        .find(|item| {
            item.name() == name && item.kind() == kind && item.parent() == Some(parent)
        })
        .map(|item| item.id())
        .ok_or(TestError::Falsified("entity absent in expected parent module"))
}

fn parameter_of(
    ir: &Ir,
    function: backend_semantic::ir::EntityId,
) -> Result<backend_semantic::ir::EntityId, TestError> {
    ir.items()
        .find(|item| item.kind() == EntityKind::Parameter && item.parent() == Some(function))
        .map(|item| item.id())
        .ok_or(TestError::Falsified("function parameter absent"))
}

fn applied_lifetime_spelling(ir: &Ir, ty: TypeId) -> Result<&[u8], TestError> {
    let TypeExpr::Concrete(ConcreteType::Applied { arguments, .. }) =
        ir.ty(ty).ok_or(TestError::Falsified("parameter type row absent"))?
    else {
        return Err(TestError::Falsified("parameter type is not Apply"));
    };
    let arguments = ir
        .types(arguments)
        .ok_or(TestError::Falsified("Apply arguments absent"))?;
    let lifetime = arguments
        .iter()
        .find_map(|argument| match ir.ty(*argument) {
            Some(TypeExpr::Concrete(ConcreteType::Inferred(spelling))) => spelling
                .and_then(|atom| ir.atom(atom))
                .or(Some(b"_".as_slice())),
            _ => None,
        })
        .ok_or(TestError::Falsified("lifetime argument row absent"))?;
    Ok(lifetime)
}

fn item_binding_targets(ir: &Ir) -> Result<Vec<TypeId>, TestError> {
    let mut targets = Vec::new();
    for (_id, ty) in ir.canonical_types() {
        let TypeExpr::Concrete(ConcreteType::QualifiedPath {
            spelling,
            self_type,
            ..
        }) = ty
        else {
            continue;
        };
        if ir.atom(spelling) != Some(b"Item") {
            continue;
        }
        targets.push(self_type);
    }
    Ok(targets)
}

fn is_u8(ir: &Ir, ty: TypeId) -> bool {
    matches!(
        ir.ty(ty),
        Some(TypeExpr::Concrete(ConcreteType::Builtin(BuiltinType::U8)))
    )
}

fn atom_is(ir: &Ir, atom: backend_semantic::ir::AtomId, spelling: &[u8]) -> bool {
    ir.atom(atom) == Some(spelling)
}

fn is_string(ir: &Ir, ty: TypeId) -> bool {
    match ir.ty(ty) {
        Some(TypeExpr::Concrete(ConcreteType::Nominal(entity))) => ir
            .items()
            .find(|item| item.id() == entity)
            .is_some_and(|item| item.name() == b"String"),
        Some(TypeExpr::Concrete(ConcreteType::External(external))) => match ir.external(external) {
            Some(ExternalTarget::Foreign(target)) => {
                atom_is(ir, target.display, b"String") || atom_is(ir, target.path, b"String")
            }
            Some(ExternalTarget::FragmentEntity { display, .. }) => {
                atom_is(ir, *display, b"String")
            }
            Some(ExternalTarget::Stable { .. }) | None => false,
        },
        _ => false,
    }
}

/// Child-module `Ref<'static>`, distinct `Iter<Item = …>` bounds, and the
/// `child` module entity survive a real two-file Cargo compile.
#[test]
fn cross_module_bindings_lower_through_the_rust_authority() -> Result<(), TestError> {
    if resolve_tool().is_none() {
        skip("rust", "cross-module-bindings", "rustc-missing")?;
        return Ok(());
    }

    let root = fixture_root()?;
    let ir = compile_fixture(&root)?;
    let _ = fs::remove_dir_all(&root);

    if use_case_support::count_named(&ir, EntityKind::Module, b"child") != 1 {
        return Err(TestError::Falsified("child module entity absent"));
    }
    let child_module = ir
        .items()
        .find(|item| item.kind() == EntityKind::Module && item.name() == b"child")
        .map(|item| item.id())
        .ok_or(TestError::Falsified("child module entity absent"))?;

    let early = entity_in_parent(&ir, child_module, b"early", EntityKind::Function)?;
    let parameter = parameter_of(&ir, early)?;
    let parameter_type = ir
        .items()
        .find(|item| item.id() == parameter)
        .and_then(|item| item.semantic_type())
        .ok_or(TestError::Falsified("early parameter is untyped"))?;
    let lifetime = applied_lifetime_spelling(&ir, parameter_type)?;
    if lifetime != b"'static" {
        return Err(TestError::Falsified(
            "early parameter must keep the written 'static lifetime",
        ));
    }

    let bindings = item_binding_targets(&ir)?;
    if bindings.len() < 2 {
        return Err(TestError::Falsified("two associated Item bindings absent"));
    }
    let mut u8_binding = None;
    let mut string_binding = None;
    for binding in &bindings {
        if is_u8(&ir, *binding) {
            u8_binding = Some(*binding);
        }
        if is_string(&ir, *binding) {
            string_binding = Some(*binding);
        }
    }
    let u8_binding = u8_binding.ok_or(TestError::Falsified("Iter<Item = u8> binding absent"))?;
    let string_binding =
        string_binding.ok_or(TestError::Falsified("Iter<Item = String> binding absent"))?;
    if u8_binding == string_binding {
        return Err(TestError::Falsified(
            "Iterator<Item = u8> and Iterator<Item = String> must differ",
        ));
    }

    entity_in_parent(&ir, child_module, b"bound", EntityKind::Function)?;
    entity_in_parent(&ir, child_module, b"other", EntityKind::Function)?;

    Ok(())
}
