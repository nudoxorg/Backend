//! Golden rendering over the Python semantic lane.

use std::{
    fs,
    process::Command,
    sync::atomic::AtomicBool,
    time::{Duration, Instant, SystemTime, SystemTimeError, UNIX_EPOCH},
};

use backend_engine::driver::{
    CompileControl, CompileFailure, CompileOutput, CompileRequest, CompileScratch, NativeTool,
    ResolvedToolchain, SemanticAuthorityInput, ToolchainSelection, compile, compile_ir,
};
use backend_semantic::ir::{
    DecodedDocFact, DecodedOccurrence, DecodedTypeFact, EntityId, EntityKind, FragmentView, Ir,
    ItemKind, OccurrenceTarget, PrimitiveShape, SemanticTypeTag,
};
use backend_semantic::vocabulary::{LanguageProfile, PythonVersion, Stage};
use thiserror::Error;

const SOURCE: &[u8] = br#""""A documented Python module."""
import typing
from pathlib import Path as LocalPath

class Plain:
    """Plain documentation."""
    field: list[int]

    @staticmethod
    def method(value: dict[str, int]) -> tuple[int, str]:
        return (1, "ok")

class Mapping(typing.TypedDict):
    name: str
    count: typing.NotRequired[int]

class Reader(typing.Protocol):
    def read(self, size: int) -> bytes: ...

type Alias = typing.Callable[[int], str]

@typing.overload
def overloaded(value: int) -> str: ...
@typing.overload
def overloaded(value: str) -> int: ...
def overloaded(value: int | str, /, *args: tuple[int, str], flag: bool = True, **kwargs: Optional[int]) -> Literal["ok"]: ...

def calls(value: int, /, *, enabled: bool = False) -> str:
    return overloaded(value)

answer: Optional[int] = None
items: list[int] = []
lookup: dict[str, int] = {}
callback: typing.Callable[[int], str]
maybe: int | None
choice: Literal["ok"]
quoted: "Plain"
"""#;

const FORWARD_REFERENCE_SOURCE: &[u8] = br#"import typing
class Mapping(typing.TypedDict):
    name: str
    count: typing.NotRequired[int]
class Reader(typing.Protocol):
    def read(self, size: int) -> bytes: ...
"#;

#[derive(Debug, Error)]
enum TestError {
    #[error("fixture I/O failed: {0}")]
    Io(#[source] std::io::Error),
    #[error("clock failed: {0}")]
    Clock(#[source] SystemTimeError),
    #[error("python3 is unavailable")]
    MissingPython,
    #[error("python tool failed: {0}")]
    Tool(#[source] std::io::Error),
    #[error("toolchain resolution failed")]
    Resolve,
    #[error("compile failed: {0}")]
    Compile(&'static str),
    #[error("entity {name:?} was not found")]
    MissingEntity { name: &'static str },
    #[error("renderer mismatch for {name}: expected {expected:?}, actual {actual:?}")]
    Mismatch {
        name: &'static str,
        expected: String,
        actual: String,
    },
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

fn compile_source(source: &'static [u8]) -> Result<Ir, TestError> {
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
    let toolchain = ResolvedToolchain::from_version(NativeTool::Python, &executable, version_bytes)
        .map_err(|_| TestError::Resolve)?;
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(TestError::Clock)?
        .as_nanos();
    let work = std::env::temp_dir().join(format!("nudox-python-render-{nonce}"));
    fs::create_dir_all(&work).map_err(TestError::Io)?;
    let cancelled = AtomicBool::new(false);
    let mut diagnostic = [0_u8; 4096];
    let result = compile_ir(
        CompileRequest {
            profile: LanguageProfile::Python(PythonVersion::Python314),
            stage: Stage::LowerIr,
            source,
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
            native_work: &work,
        },
    )
    .map(|compiled| compiled.ir)
    .map_err(|failure| TestError::Compile(failure_label(&failure)));
    let cleanup = fs::remove_dir_all(&work).map_err(TestError::Io);
    cleanup?;
    result
}

fn compile_fragment(source: &'static [u8]) -> Result<Vec<u8>, TestError> {
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
    let toolchain = ResolvedToolchain::from_version(NativeTool::Python, &executable, version_bytes)
        .map_err(|_| TestError::Resolve)?;
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(TestError::Clock)?
        .as_nanos();
    let work = std::env::temp_dir().join(format!("nudox-python-forward-{nonce}"));
    fs::create_dir_all(&work).map_err(TestError::Io)?;
    let cancelled = AtomicBool::new(false);
    let mut diagnostic = [0_u8; 4096];
    let mut output = vec![0_u8; 8 * 1024 * 1024];
    let result = compile(
        CompileRequest {
            profile: LanguageProfile::Python(PythonVersion::Python314),
            stage: Stage::LowerIr,
            source,
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
            native_work: &work,
        },
        CompileOutput {
            fragment_output: &mut output,
        },
    )
    .map_err(|failure| TestError::Compile(failure_label(&failure)))?;
    let bytes = result.fragment.as_ref().to_vec();
    fs::remove_dir_all(&work).map_err(TestError::Io)?;
    Ok(bytes)
}

fn compile_fixture() -> Result<Ir, TestError> {
    compile_source(SOURCE)
}

fn entity(ir: &Ir, name: &'static str, kind: ItemKind) -> Result<EntityId, TestError> {
    ir.items()
        .find(|item| item.kind() == kind && item.name() == name.as_bytes())
        .map(|item| item.id())
        .ok_or(TestError::MissingEntity { name })
}

fn exact_signature(
    ir: &Ir,
    name: &'static str,
    kind: ItemKind,
    expected: &str,
) -> Result<(), TestError> {
    let id = entity(ir, name, kind)?;
    let actual = ir
        .signature(id)
        .ok_or(TestError::Falsified("signature unavailable"))?
        .to_string();
    if actual == expected {
        Ok(())
    } else {
        Err(TestError::Mismatch {
            name,
            expected: expected.to_owned(),
            actual,
        })
    }
}

fn exact_type(ir: &Ir, name: &'static str, expected: &str) -> Result<(), TestError> {
    let id = entity(ir, name, ItemKind::Static)?;
    let ty = ir
        .item(id)
        .and_then(|item| item.semantic_type())
        .ok_or(TestError::MissingEntity { name })?;
    let actual = ir
        .display_type(ty)
        .ok_or(TestError::Falsified("type display unavailable"))?
        .to_string();
    if actual == expected {
        Ok(())
    } else {
        Err(TestError::Mismatch {
            name,
            expected: expected.to_owned(),
            actual,
        })
    }
}

#[test]
fn python_lane_renders_exact_declarations_and_docs() -> Result<(), TestError> {
    let ir = compile_fixture()?;
    exact_signature(&ir, "Plain", ItemKind::Record, "struct Plain")?;
    exact_signature(&ir, "Mapping", ItemKind::Record, "struct Mapping")?;
    exact_signature(&ir, "Reader", ItemKind::Record, "struct Reader")?;
    exact_signature(
        &ir,
        "overloaded",
        ItemKind::Function,
        "fn overloaded(value: int) -> str",
    )?;
    exact_signature(
        &ir,
        "calls",
        ItemKind::Function,
        "fn calls(value: int, enabled: bool) -> str",
    )?;
    exact_signature(&ir, "answer", ItemKind::Static, "static answer: int | None")?;
    exact_signature(&ir, "items", ItemKind::Static, "static items: list<int>")?;
    exact_signature(
        &ir,
        "lookup",
        ItemKind::Static,
        "static lookup: dict<str, int>",
    )?;
    exact_signature(
        &ir,
        "callback",
        ItemKind::Static,
        "static callback: fn(int) -> str",
    )?;
    exact_signature(&ir, "maybe", ItemKind::Static, "static maybe: int | None")?;
    exact_signature(&ir, "choice", ItemKind::Static, "static choice: str")?;
    let plain = entity(&ir, "Plain", ItemKind::Record)?;
    let docs = ir
        .display_docs(plain)
        .ok_or(TestError::Falsified("docs unavailable"))?
        .to_string();
    if docs != "Plain documentation." {
        return Err(TestError::Mismatch {
            name: "Plain docs",
            expected: "Plain documentation.".to_owned(),
            actual: docs,
        });
    }
    let embedding = ir
        .embedding_text(plain, backend_semantic::ir::semantic_render::EmbeddingProfile::DOCUMENTED)
        .ok_or(TestError::Falsified("embedding unavailable"))?
        .to_string();
    if embedding != "struct Plain\n\nPlain documentation." {
        return Err(TestError::Mismatch {
            name: "Plain embedding",
            expected: "struct Plain\n\nPlain documentation.".to_owned(),
            actual: embedding,
        });
    }
    Ok(())
}

#[test]
fn python_lane_renders_compound_types_and_is_deterministic() -> Result<(), TestError> {
    let first = compile_fixture()?;
    let second = compile_fixture()?;
    let mut first_text = Vec::new();
    let mut second_text = Vec::new();
    for item in first.items() {
        if let Some(ty) = item.semantic_type() {
            if let Some(display) = first.display_type(ty) {
                first_text.push((item.name().to_vec(), display.to_string()));
            }
        }
    }
    for item in second.items() {
        if let Some(ty) = item.semantic_type() {
            if let Some(display) = second.display_type(ty) {
                second_text.push((item.name().to_vec(), display.to_string()));
            }
        }
    }
    if first_text != second_text {
        return Err(TestError::Falsified("independent renders differ"));
    }
    exact_type(&first, "items", "list<int>")?;
    exact_type(&first, "lookup", "dict<str, int>")?;
    exact_type(&first, "callback", "fn(int) -> str")?;
    exact_type(&first, "answer", "int | None")?;
    exact_type(&first, "maybe", "int | None")?;
    let alternate = compile_source(b"left: int\nright: str\n")?;
    entity(&alternate, "left", ItemKind::Static)?;
    exact_type(&alternate, "left", "int")?;
    exact_type(&alternate, "right", "str")?;
    exact_type(&first, "choice", "str")?;
    Ok(())
}

#[test]
fn python_fragment_planes_carry_what_the_ir_tree_omits() -> Result<(), TestError> {
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
    let toolchain = ResolvedToolchain::from_version(NativeTool::Python, &executable, version_bytes)
        .map_err(|_| TestError::Resolve)?;
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(TestError::Clock)?
        .as_nanos();
    let work = std::env::temp_dir().join(format!("nudox-python-fragment-{nonce}"));
    fs::create_dir_all(&work).map_err(TestError::Io)?;
    let cancelled = AtomicBool::new(false);
    let mut diagnostic = [0_u8; 4096];
    let mut output = vec![0_u8; 8 * 1024 * 1024];
    let result = compile(
        CompileRequest {
            profile: LanguageProfile::Python(PythonVersion::Python314),
            stage: Stage::LowerIr,
            source: SOURCE,
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
            native_work: &work,
        },
        CompileOutput {
            fragment_output: &mut output,
        },
    )
    .map_err(|failure| TestError::Compile(failure_label(&failure)))?;
    let bytes = result.fragment.as_ref();
    let decoded = FragmentView::validate(bytes).map_err(|_| TestError::Compile("validate"))?;

    let atoms: Vec<&[u8]> = decoded.atoms().map(|atom| atom.bytes).collect();
    let entities: Vec<(&[u8], EntityKind)> = decoded
        .entities()
        .map(|row| {
            let name = usize::try_from(row.name.raw)
                .ok()
                .and_then(|ordinal| atoms.get(ordinal).copied())
                .unwrap_or(&[]);
            (name, row.kind)
        })
        .collect();
    let overloaded = entities
        .iter()
        .position(|(name, kind)| *name == b"overloaded" && *kind == EntityKind::Function)
        .ok_or(TestError::MissingEntity { name: "overloaded" })?;

    let mut docs: Vec<DecodedDocFact<'_>> = Vec::new();
    if let Some(mut cursor) = decoded.docs() {
        for row in cursor.by_ref() {
            docs.push(row.map_err(|_| TestError::Falsified("doc decode"))?);
        }
    }
    if !docs.iter().any(|fact| {
        matches!(fact.fragment, backend_semantic::ir::DocFragmentInput::Text(text) if text == b"Plain documentation.")
    }) {
        return Err(TestError::Falsified("docstring absent from fragment docs lane"));
    }

    let mut types: Vec<DecodedTypeFact<'_>> = Vec::new();
    if let Some(mut cursor) = decoded.type_facts() {
        for row in cursor.by_ref() {
            types.push(row.map_err(|_| TestError::Falsified("type fact decode"))?);
        }
    }
    for name in ["items", "lookup"] {
        let owner = entities
            .iter()
            .position(|(known, kind)| *known == name.as_bytes() && *kind == EntityKind::Static)
            .ok_or(TestError::MissingEntity { name })?;
        if !types.iter().any(|fact| {
            fact.owner.raw as usize == owner && fact.record.tag == SemanticTypeTag::Apply
        }) {
            return Err(TestError::Falsified(
                "compound type fact absent from fragment lane",
            ));
        }
    }
    let callback_owner = entities
        .iter()
        .position(|(known, kind)| *known == b"callback" && *kind == EntityKind::Static)
        .ok_or(TestError::MissingEntity { name: "callback" })?;
    if !types.iter().any(|fact| {
        fact.owner.raw as usize == callback_owner
            && fact.record.tag == SemanticTypeTag::FunctionPointer
    }) {
        return Err(TestError::Falsified(
            "callback function-pointer fact absent from fragment lane",
        ));
    }

    let mut occurrences: Vec<DecodedOccurrence<'_>> = Vec::new();
    if let Some(mut cursor) = decoded.occurrences() {
        for row in cursor.by_ref() {
            occurrences.push(row.map_err(|_| TestError::Falsified("occurrence decode"))?);
        }
    }
    // The fragment path is the deterministic syntax-only lane: import
    // bindings resolve to foreign pypi package keys at Index tier, and the
    // checker's Import/Oracle upgrades live on the checker-provisioned
    // paths (see the live-Ir tests and the real-package matrix).
    if !occurrences.iter().any(|fact| {
        fact.occurrence.confidence == backend_semantic::ir::OccurrenceConfidence::Index
            && matches!(fact.occurrence.target, OccurrenceTarget::Local(target) if target.raw as usize == overloaded)
    }) {
        return Err(TestError::Falsified(
            "overloaded call occurrence absent at the index tier",
        ));
    }
    fs::remove_dir_all(&work).map_err(TestError::Io)?;
    Ok(())
}

/// One little-endian u32 word of a validated section payload.
fn wire_word(payload: &[u8], at: usize) -> Result<u32, TestError> {
    payload
        .get(at..at.checked_add(4).ok_or(TestError::Coordinate)?)
        .and_then(|bytes| <[u8; 4]>::try_from(bytes).ok())
        .map(u32::from_le_bytes)
        .ok_or(TestError::Falsified("payload word out of range"))
}

/// Advances over one variable-size presence cell in the type-fact grammar.
fn skip_cell(payload: &[u8], cursor: &mut usize) -> Result<(), TestError> {
    const CELL_HEADER_BYTES: usize = 1 + 4;

    match payload.get(*cursor).copied() {
        Some(0) => *cursor = cursor.checked_add(1).ok_or(TestError::Coordinate)?,
        Some(1) => {
            let length = usize::try_from(wire_word(
                payload,
                cursor.checked_add(1).ok_or(TestError::Coordinate)?,
            )?)
            .map_err(|_| TestError::Coordinate)?;
            *cursor = cursor
                .checked_add(CELL_HEADER_BYTES)
                .and_then(|at| at.checked_add(length))
                .ok_or(TestError::Coordinate)?;
        }
        _ => return Err(TestError::Falsified("truncated presence cell")),
    }
    Ok(())
}

/// Advances over the variable-size cells of one type-fact record.
fn skip_record_cells(payload: &[u8], cursor: &mut usize) -> Result<(), TestError> {
    const NOMINAL_NONE_BYTES: usize = 1;
    const NOMINAL_LOCAL_BYTES: usize = 1 + 4;
    const NOMINAL_EXTERNAL_BYTES: usize = 1 + 32 + 4;
    const CHILD_SPAN_BYTES: usize = 8;

    skip_cell(payload, cursor)?;
    skip_cell(payload, cursor)?;
    match payload.get(*cursor).copied() {
        Some(0) => {
            *cursor = cursor
                .checked_add(NOMINAL_NONE_BYTES)
                .ok_or(TestError::Coordinate)?
        }
        Some(1) => {
            *cursor = cursor
                .checked_add(NOMINAL_LOCAL_BYTES)
                .ok_or(TestError::Coordinate)?
        }
        Some(2) => {
            *cursor = cursor
                .checked_add(NOMINAL_EXTERNAL_BYTES)
                .ok_or(TestError::Coordinate)?
        }
        _ => return Err(TestError::Falsified("truncated nominal cell")),
    }
    *cursor = cursor
        .checked_add(CHILD_SPAN_BYTES)
        .ok_or(TestError::Coordinate)?;
    Ok(())
}

/// Decodes one pooled child and returns only a local row target.
fn child_local_target(payload: &[u8], cursor: &mut usize) -> Result<Option<u32>, TestError> {
    const LOCAL_TAG: u8 = 0;
    const EXTERNAL_TAG: u8 = 1;
    const TEXT_TAG: u8 = 2;
    const LOCAL_CELL_BYTES: usize = 1 + 4;
    const EXTERNAL_CELL_BYTES: usize = 1 + 32 + 4;

    let target = match payload.get(*cursor).copied() {
        Some(LOCAL_TAG) => {
            let target = wire_word(payload, cursor.checked_add(1).ok_or(TestError::Coordinate)?)?;
            *cursor = cursor
                .checked_add(LOCAL_CELL_BYTES)
                .ok_or(TestError::Coordinate)?;
            Some(target)
        }
        Some(EXTERNAL_TAG) => {
            *cursor = cursor
                .checked_add(EXTERNAL_CELL_BYTES)
                .ok_or(TestError::Coordinate)?;
            None
        }
        Some(TEXT_TAG) => {
            *cursor = cursor.checked_add(1).ok_or(TestError::Coordinate)?;
            None
        }
        _ => return Err(TestError::Falsified("truncated child target")),
    };
    skip_cell(payload, cursor)?;
    *cursor = cursor.checked_add(1).ok_or(TestError::Coordinate)?;
    Ok(target)
}

/// Decodes one Apply record's base child local row target from the raw
/// payload: skip every record of both segments, then walk the pooled child
/// lane to the record's span start.
fn apply_base_child(payload: &[u8], fact: &DecodedTypeFact<'_>) -> Result<u32, TestError> {
    const RECORD_FIXED_CELLS: usize = 4 + 1 + 4 + 4;

    let declared = usize::try_from(wire_word(payload, 0)?).map_err(|_| TestError::Coordinate)?;
    let computed = usize::try_from(wire_word(payload, 4)?).map_err(|_| TestError::Coordinate)?;
    let mut cursor = 8usize;
    for _ in 0..(declared + computed) {
        cursor = cursor
            .checked_add(RECORD_FIXED_CELLS)
            .ok_or(TestError::Coordinate)?;
        skip_record_cells(payload, &mut cursor)?;
    }
    let start = usize::try_from(fact.record.children.start).map_err(|_| TestError::Coordinate)?;
    let length = usize::try_from(fact.record.children.length).map_err(|_| TestError::Coordinate)?;
    let end = start.checked_add(length).ok_or(TestError::Coordinate)?;
    let child_count =
        usize::try_from(wire_word(payload, cursor)?).map_err(|_| TestError::Coordinate)?;
    cursor = cursor.checked_add(4).ok_or(TestError::Coordinate)?;
    if end > child_count {
        return Err(TestError::Falsified("child span outside the pooled lane"));
    }
    for position in 0..end {
        let target = child_local_target(payload, &mut cursor)?;
        if position == start {
            return target.ok_or(TestError::Falsified("base child is not a local row"));
        }
    }
    Err(TestError::Falsified("base child outside the child span"))
}

/// The fragment lane itself carries the lifted compound root: the `items`
/// Apply record owns exactly a base child plus its one argument, and that
/// base child decodes to the Primitive/Builtin `list` row.
#[test]
fn python_fragment_apply_base_decodes_to_the_list_builtin_row() -> Result<(), TestError> {
    let bytes = compile_fragment(SOURCE)?;
    let decoded = FragmentView::validate(&bytes).map_err(|_| TestError::Compile("validate"))?;
    let atoms: Vec<&[u8]> = decoded.atoms().map(|atom| atom.bytes).collect();
    let entities: Vec<(&[u8], EntityKind)> = decoded
        .entities()
        .map(|row| {
            let name = usize::try_from(row.name.raw)
                .ok()
                .and_then(|ordinal| atoms.get(ordinal).copied())
                .unwrap_or(&[]);
            (name, row.kind)
        })
        .collect();
    let owner = entities
        .iter()
        .position(|(name, kind)| *name == b"items" && *kind == EntityKind::Static)
        .ok_or(TestError::MissingEntity { name: "items" })?;
    let mut types: Vec<DecodedTypeFact<'_>> = Vec::new();
    if let Some(mut cursor) = decoded.type_facts() {
        for row in cursor.by_ref() {
            types.push(row.map_err(|_| TestError::Falsified("type fact decode"))?);
        }
    }
    let fact = types
        .iter()
        .find(|fact| fact.owner.raw as usize == owner && fact.record.tag == SemanticTypeTag::Apply)
        .ok_or(TestError::Falsified(
            "items compound fact absent from fragment lane",
        ))?;
    if fact.record.children.length != 2 {
        return Err(TestError::Falsified(
            "items Apply record does not own exactly a base and one argument",
        ));
    }
    let base = apply_base_child(
        decoded
            .type_fact_payload()
            .ok_or(TestError::Falsified("no type fact section"))?,
        fact,
    )?;
    let row = types
        .get(usize::try_from(base).map_err(|_| TestError::Coordinate)?)
        .ok_or(TestError::Falsified("base child row outside the type lane"))?;
    if row.record.tag != SemanticTypeTag::Primitive
        || row.record.payload0 != u32::from(PrimitiveShape::Builtin)
        || row.record.text != Some(b"list")
    {
        return Err(TestError::Falsified(
            "base child row is not the Primitive/Builtin list row",
        ));
    }
    Ok(())
}

#[test]
fn checker_inference_is_rendered_when_pyrefly_is_available() -> Result<(), TestError> {
    let checker = backend_frontend_python::legacy::Pyrefly::from_env();
    if !checker.is_available() {
        return Ok(());
    }
    let ir = compile_source(b"inferred = 1\n")?;
    let id = entity(&ir, "inferred", ItemKind::Static)?;
    let ty = ir
        .item(id)
        .and_then(|item| item.semantic_type())
        .ok_or(TestError::Falsified("inferred type unavailable"))?;
    let actual = ir
        .display_type(ty)
        .ok_or(TestError::Falsified("inferred display unavailable"))?
        .to_string();
    if actual == "i32" {
        Ok(())
    } else {
        Err(TestError::Mismatch {
            name: "inferred",
            expected: "i32".to_owned(),
            actual,
        })
    }
}

#[test]
fn python_fragment_forward_reference_structural_rows_validate() -> Result<(), TestError> {
    let bytes = compile_fragment(FORWARD_REFERENCE_SOURCE)?;
    let decoded = FragmentView::validate(&bytes).map_err(|_| TestError::Compile("validate"))?;
    let atoms: Vec<&[u8]> = decoded.atoms().map(|atom| atom.bytes).collect();
    let entities: Vec<(&[u8], EntityKind)> = decoded
        .entities()
        .map(|row| {
            let name = usize::try_from(row.name.raw)
                .ok()
                .and_then(|ordinal| atoms.get(ordinal).copied())
                .unwrap_or(&[]);
            (name, row.kind)
        })
        .collect();
    let mapping = entities
        .iter()
        .position(|(name, kind)| *name == b"Mapping" && *kind == EntityKind::Record)
        .ok_or(TestError::MissingEntity { name: "Mapping" })?;
    let reader = entities
        .iter()
        .position(|(name, kind)| *name == b"Reader" && *kind == EntityKind::Record)
        .ok_or(TestError::MissingEntity { name: "Reader" })?;
    let read = entities
        .iter()
        .position(|(name, kind)| *name == b"read" && *kind == EntityKind::Function)
        .ok_or(TestError::MissingEntity { name: "read" })?;
    let mut cursor = decoded
        .type_facts()
        .ok_or(TestError::Compile("type facts"))?;
    let rows: Vec<DecodedTypeFact<'_>> = cursor
        .by_ref()
        .map(|row| row.map_err(|_| TestError::Compile("type fact decode")))
        .collect::<Result<_, _>>()?;
    let records: Vec<&DecodedTypeFact<'_>> = rows
        .iter()
        .filter(|row| row.record.tag == SemanticTypeTag::AnonymousRecord)
        .collect();
    let pointers: Vec<&DecodedTypeFact<'_>> = rows
        .iter()
        .filter(|row| row.record.tag == SemanticTypeTag::FunctionPointer)
        .collect();
    // Both structural classes host their members: Mapping (first
    // declaration) reserves its own ordinal, Reader follows. Each record
    // names its member count; the method FnPtr exists twice — once as
    // Reader's pooled member row, once as the `read` function's own typed
    // fact.
    if records.len() != 2
        || pointers.len() != 2
        || !records
            .iter()
            .any(|row| row.owner.raw as usize == mapping && row.record.children.length == 2)
        || !records
            .iter()
            .any(|row| row.owner.raw as usize == reader && row.record.children.length == 1)
        || !pointers
            .iter()
            .any(|row| row.owner.raw as usize == reader && row.record.children.length == 3)
        || !pointers
            .iter()
            .any(|row| row.owner.raw as usize == read && row.record.children.length == 3)
    {
        return Err(TestError::Falsified("structural member rows absent"));
    }
    Ok(())
}
