//! Regression: lifetime generic arguments must lower as identity-changing
//! annotations. `Ref<'static>` and `Ref<'b>` are different type applications.

#![forbid(unsafe_code)]
#![deny(clippy::expect_used, clippy::panic, clippy::unwrap_used)]

use std::{
    fs,
    path::PathBuf,
    sync::atomic::{AtomicBool, AtomicU64, Ordering},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use backend_engine::driver::{
    CompileControl, CompileFailure, CompileOutput, CompileRequest, CompileScratch,
    ResolvedToolchain, SemanticAuthorityInput, ToolchainSelection, compile,
};
use backend_frontend_rust::legacy::{
    RustAuthorityError, RustFeatureControl, RustProject, RustToolchain, SourceByteLimit,
};
use backend_semantic::ir::{EntityKind, FragmentView, SemanticTypeTag};
use backend_semantic::vocabulary::{LanguageProfile, RustEdition, Stage};
use thiserror::Error;

static FIXTURE_SEQUENCE: AtomicU64 = AtomicU64::new(0);

const LIFETIME_BINDING_SOURCE: &str = r#"pub struct Ref<'a>(&'a u8);
pub fn early(_: Ref<'static>) {}
pub fn late<'b>(_: Ref<'b>) {}
"#;

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
    #[error("fragment validation failed")]
    Validate(#[from] backend_semantic::ir::FragmentError),
    #[error("lane falsifier: {0}")]
    Falsified(&'static str),
    #[error("coordinate conversion failed")]
    Coordinate,
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

fn fixture_root(body: &str) -> Result<PathBuf, TestError> {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(TestError::Clock)?
        .as_nanos();
    let sequence = FIXTURE_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let root = std::env::temp_dir().join(format!(
        "nudox-rust-binding-identity-{nonce}-{}-{sequence}",
        std::process::id()
    ));
    fs::create_dir_all(root.join("src")).map_err(|source| TestError::Io {
        operation: "create fixture",
        source,
    })?;
    fs::write(
        root.join("Cargo.toml"),
        "[package]\nname = \"binding_identity_fixture\"\nversion = \"0.1.0\"\nedition = \"2024\"\n",
    )
    .map_err(|source| TestError::Io {
        operation: "write manifest",
        source,
    })?;
    fs::write(root.join("src/lib.rs"), body).map_err(|source| TestError::Io {
        operation: "write crate root",
        source,
    })?;
    Ok(root)
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

fn compile_fixture(body: &str) -> Result<Vec<u8>, TestError> {
    let root = fixture_root(body)?;
    let outcome = compile_body(&root, body);
    fs::remove_dir_all(&root).map_err(|source| TestError::Io {
        operation: "remove fixture",
        source,
    })?;
    outcome
}

fn compile_body(root: &PathBuf, body: &str) -> Result<Vec<u8>, TestError> {
    let source_path = root.join("src/lib.rs");
    let Some(tool) = resolve_tool() else {
        return Err(TestError::MissingRustc);
    };
    let toolchain = RustToolchain::discover(&tool).map_err(|_| TestError::MissingRustc)?;
    let project =
        RustProject::open_with_source(root, &source_path, &toolchain, RustEdition::Rust2024)?;
    let resolved = ResolvedToolchain::from_version(
        backend_engine::driver::NativeTool::Rustc,
        &tool,
        b"compiler-driver-rust-binding-identity-fixture",
    )
    .map_err(|_| TestError::MissingRustc)?;
    let cancelled = AtomicBool::new(false);
    let mut diagnostic = [0; 0];
    let mut fragment_output = vec![0_u8; 262_144];
    let request = CompileRequest {
        profile: LanguageProfile::Rust(RustEdition::Rust2024),
        stage: Stage::LowerIr,
        source: body.as_bytes(),
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
    };
    match compile(
        request,
        CompileScratch {
            diagnostic_output: &mut diagnostic,
            native_work: root,
        },
        CompileOutput {
            fragment_output: &mut fragment_output,
        },
    ) {
        Ok(_) => {}
        Err(failure) => return Err(TestError::Compile(failure_label(&failure))),
    }
    let declared = fragment_output
        .get(8..12)
        .and_then(|bytes| <[u8; 4]>::try_from(bytes).ok())
        .map(u32::from_le_bytes)
        .ok_or(TestError::Falsified("fragment header truncated"))?;
    let length = usize::try_from(declared).map_err(|_| TestError::Coordinate)?;
    let bytes = fragment_output.get(..length).ok_or(TestError::Falsified(
        "declared length exceeds the output buffer",
    ))?;
    Ok(bytes.to_vec())
}

struct Lane<'fragment> {
    entities: Vec<(&'fragment [u8], EntityKind)>,
    types: Vec<backend_semantic::ir::DecodedTypeFact<'fragment>>,
}

fn lane_of(bytes: &[u8]) -> Result<Lane<'_>, TestError> {
    let view = FragmentView::validate(bytes)?;
    let mut entities = Vec::new();
    for entity in view.entities() {
        let atom = usize::try_from(entity.name.raw).map_err(|_| TestError::Coordinate)?;
        let name = view
            .atoms()
            .nth(atom)
            .map(|atom| atom.bytes)
            .ok_or(TestError::Falsified("entity atom out of range"))?;
        entities.push((name, entity.kind));
    }
    let mut types = Vec::new();
    if let Some(mut cursor) = view.type_facts() {
        for row in cursor.by_ref() {
            types.push(row.map_err(|_| TestError::Falsified("type fact decode"))?);
        }
    }
    Ok(Lane { entities, types })
}

fn parameter_apply_row(lane: &Lane<'_>, parameter: usize) -> Result<usize, TestError> {
    lane.types
        .iter()
        .position(|fact| {
            fact.owner.raw as usize == parameter && fact.record.tag == SemanticTypeTag::Apply
        })
        .ok_or(TestError::Falsified("parameter Apply row absent"))
}

fn parameter_lifetime_spelling<'a>(
    lane: &'a Lane<'a>,
    parameter: usize,
    apply_row: usize,
) -> Result<&'a [u8], TestError> {
    let apply = &lane.types[apply_row];
    if apply.record.tag != SemanticTypeTag::Apply {
        return Err(TestError::Falsified("parameter type is not Apply"));
    }
    if apply.record.children.length != 2 {
        return Err(TestError::Falsified(
            "Ref apply lacks base and lifetime argument",
        ));
    }
    let mut lifetimes = lane.types.iter().filter(|fact| {
        fact.owner.raw as usize == parameter && fact.record.tag == SemanticTypeTag::Inferred
    });
    let lifetime = lifetimes
        .next()
        .ok_or(TestError::Falsified("lifetime argument row absent"))?;
    if lifetimes.next().is_some() {
        return Err(TestError::Falsified(
            "parameter carries more than one lifetime argument row",
        ));
    }
    lifetime
        .record
        .text
        .ok_or(TestError::Falsified("lifetime spelling absent"))
}

fn wildcard_ref_apply_row(lane: &Lane<'_>, lifetime: &[u8]) -> Result<usize, TestError> {
    for (ordinal, (name, kind)) in lane.entities.iter().enumerate() {
        if *name != b"_" || *kind != EntityKind::Parameter {
            continue;
        }
        let apply_row = parameter_apply_row(lane, ordinal)?;
        if parameter_lifetime_spelling(lane, ordinal, apply_row)? == lifetime {
            return Ok(apply_row);
        }
    }
    Err(TestError::Falsified("wildcard Ref apply absent"))
}

/// `Ref<'static>` and `Ref<'b>` must lower as distinct Apply nodes whose
/// lifetime arguments keep their exact written spellings.
#[test]
fn lifetime_generic_argument_preserves_written_spelling() -> Result<(), TestError> {
    let bytes = compile_fixture(LIFETIME_BINDING_SOURCE)?;
    let lane = lane_of(&bytes)?;

    let early_row = wildcard_ref_apply_row(&lane, b"'static")?;
    let late_row = wildcard_ref_apply_row(&lane, b"'b")?;

    if lane.types[early_row].record.tag != SemanticTypeTag::Apply {
        return Err(TestError::Falsified("early parameter is not Apply"));
    }
    if lane.types[late_row].record.tag != SemanticTypeTag::Apply {
        return Err(TestError::Falsified("late parameter is not Apply"));
    }

    let early_parameter = lane.types[early_row].owner.raw as usize;
    let late_parameter = lane.types[late_row].owner.raw as usize;
    let early_lifetime =
        parameter_lifetime_spelling(&lane, early_parameter, early_row)?;
    let late_lifetime = parameter_lifetime_spelling(&lane, late_parameter, late_row)?;

    if early_lifetime == late_lifetime {
        return Err(TestError::Falsified(
            "Ref<'static> and Ref<'b> must not share one lifetime argument",
        ));
    }
    if early_lifetime != b"'static" {
        return Err(TestError::Falsified(
            "early parameter must keep the written 'static lifetime",
        ));
    }
    if late_lifetime != b"'b" {
        return Err(TestError::Falsified(
            "late parameter must keep the written 'b lifetime",
        ));
    }
    Ok(())
}
