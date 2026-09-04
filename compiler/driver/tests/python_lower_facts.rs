//! Falsifies the Python result-slot fact geometry over the decoded fragment
//! lanes: every return annotation carries its ordered type children, a wide
//! parameter list survives the honest per-fact child bound, and the bound
//! edge stays a typed rejection terminal.

#![forbid(unsafe_code)]
#![deny(clippy::expect_used, clippy::panic, clippy::unwrap_used)]

use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
    sync::atomic::{AtomicBool, AtomicU64, Ordering},
    time::{Duration, Instant, SystemTime, SystemTimeError, UNIX_EPOCH},
};

use compiler_driver::{
    CompileControl, CompileFailure, CompileOutput, CompileRequest, CompileScratch, FactFault,
    FactRejection, NativeTool, ResolvedToolchain, SemanticAuthorityInput, ToolchainSelection,
    compile, compile_ir,
};
use compiler_ir::{
    ConcreteType, DecodedTypeFact, EntityKind, FragmentView, Ir, ItemKind, PrimitiveShape,
    SemanticTypeTag, TypeExpr,
};
use compiler_vocabulary::{LanguageProfile, PythonVersion, Stage};
use thiserror::Error;

/// Separates concurrently executing fixtures created during one process lifetime.
static SCRATCH_SEQUENCE: AtomicU64 = AtomicU64::new(0);

const TUPLE_RESULT: &[u8] = b"anchor: int = 0\ndef f() -> tuple[int, str]: ...\n";
const DICT_RESULT: &[u8] = b"anchor: int = 0\ndef g() -> dict[str, int]: ...\n";
const UNION_RESULT: &[u8] = b"anchor: int = 0\ndef u() -> int | str: ...\n";
const TEN_PARAMETERS: &[u8] = b"def h(a, b, c, d, e, f, g, h2, i, j) -> None: ...\n";
const SEVENTEEN_PARAMETERS: &[u8] = b"def k(
    a1, a2, a3, a4, a5, a6, a7, a8, a9, a10, a11, a12, a13, a14, a15, a16, a17,
) -> None: ...\n";

#[derive(Debug, Error)]
enum TestError {
    #[error("system clock preceded its epoch: {0}")]
    Clock(#[source] SystemTimeError),
    #[error("{operation} failed: {source}")]
    Io {
        operation: &'static str,
        #[source]
        source: std::io::Error,
    },
    #[error("python3 is unavailable")]
    MissingPython,
    #[error("python tool failed: {0}")]
    Tool(#[source] std::io::Error),
    #[error("toolchain resolution failed")]
    Resolve,
    #[error("compile failed: {0}")]
    Compile(&'static str),
    #[error("fragment validation failed: {source}")]
    Validate {
        #[source]
        source: compiler_ir::FragmentError,
    },
    #[error("emission lane rejected a fact: {0:?}")]
    Rejected(FactRejection),
    #[error("lane falsifier: {0}")]
    Falsified(&'static str),
    #[error("coordinate conversion failed")]
    Coordinate,
    #[error("deep-stack worker failed")]
    Worker,
}

/// Debug-profile admission materializes megabytes of lane scratch per
/// compile; every compile runs on an explicit deep worker stack so the
/// falsifiers hold on libtest's default thread size.
const WORKER_STACK_BYTES: usize = 16 * 1024 * 1024;

fn with_deep_stack<Decoded>(
    work: impl FnOnce() -> Result<Decoded, TestError> + Send,
) -> Result<Decoded, TestError>
where
    Decoded: Send,
{
    std::thread::scope(|scope| {
        let worker = std::thread::Builder::new()
            .stack_size(WORKER_STACK_BYTES)
            .spawn_scoped(scope, work)
            .map_err(|_| TestError::Worker)?;
        worker.join().map_err(|_| TestError::Worker)?
    })
}

fn failure_label(failure: &CompileFailure<'_>) -> &'static str {
    match failure {
        CompileFailure::Authority { .. } => "authority",
        CompileFailure::AuthorityInputRequired { .. } => "authority-input-required",
        CompileFailure::AuthorityInputProfileMismatch { .. } => "authority-profile-mismatch",
        CompileFailure::LoweringUnsupported { .. } => "lowering-unsupported",
        CompileFailure::ExtensionAtomUnbound { .. } => "extension-atom-unbound",
        CompileFailure::FactRejected { .. } => "fact-rejected",
        CompileFailure::CSharpProjection { .. } => "csharp-projection",
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

/// Resolves one leaked absolute python3 path bound to its probed version.
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
    let absolute = executable.canonicalize().map_err(|source| TestError::Io {
        operation: "canonicalize python3",
        source,
    })?;
    let absolute: &'static Path = Box::leak(absolute.into_boxed_path());
    ResolvedToolchain::from_version(NativeTool::Python, absolute, version_bytes)
        .map_err(|_| TestError::Resolve)
}

/// One fresh scratch directory for the native tool adapter.
fn scratch_dir(label: &'static str) -> Result<PathBuf, TestError> {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(TestError::Clock)?
        .as_nanos();
    let sequence = SCRATCH_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let work = std::env::temp_dir().join(format!("nudox-python-facts-{label}-{nonce}-{sequence}"));
    fs::create_dir_all(&work).map_err(|source| TestError::Io {
        operation: "create scratch",
        source,
    })?;
    Ok(work)
}

/// Compiles one fixture into an owned validated fragment, or returns the
/// exact typed fact rejection the lane raised instead.
fn attempt_fragment(
    source: &'static [u8],
    label: &'static str,
) -> Result<Result<Vec<u8>, FactRejection>, TestError> {
    let toolchain = python_toolchain()?;
    let work = scratch_dir(label)?;
    let cancelled = AtomicBool::new(false);
    let outcome = with_deep_stack(|| {
        let mut diagnostic = [0_u8; 4096];
        let mut output = vec![0_u8; 8 * 1024 * 1024];
        match compile(
            CompileRequest {
                profile: LanguageProfile::Python(PythonVersion::Python314),
                stage: Stage::LowerIr,
                source,
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
        ) {
            Ok(compiled) => Ok(Ok(compiled.fragment.as_ref().to_vec())),
            Err(CompileFailure::FactRejected { rejected, .. }) => Ok(Err(rejected)),
            Err(failure) => Err(TestError::Compile(failure_label(&failure))),
        }
    })?;
    fs::remove_dir_all(&work).map_err(|source| TestError::Io {
        operation: "remove scratch",
        source,
    })?;
    Ok(outcome)
}

/// Compiles one fixture and runs `then` against the semantic image inside
/// the deep-stack worker (`Ir` borrows pooled `NonNull` lanes, so it never
/// crosses the join boundary).
fn with_image<T>(
    source: &'static [u8],
    label: &'static str,
    then: impl FnOnce(&Ir) -> Result<T, TestError> + Send,
) -> Result<T, TestError>
where
    T: Send,
{
    let toolchain = python_toolchain()?;
    let work = scratch_dir(label)?;
    let cancelled = AtomicBool::new(false);
    let outcome = with_deep_stack(|| {
        let mut diagnostic = [0_u8; 4096];
        match compile_ir(
            CompileRequest {
                profile: LanguageProfile::Python(PythonVersion::Python314),
                stage: Stage::LowerIr,
                source,
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
        ) {
            Ok(compiled) => then(&compiled.ir),
            Err(failure) => Err(TestError::Compile(failure_label(&failure))),
        }
    })?;
    fs::remove_dir_all(&work).map_err(|source| TestError::Io {
        operation: "remove scratch",
        source,
    })?;
    Ok(outcome)
}

/// One decoded lane: entity rows, type-fact rows, and the validated view.
struct Lane<'fragment> {
    entities: Vec<(&'fragment [u8], EntityKind)>,
    types: Vec<DecodedTypeFact<'fragment>>,
    view: FragmentView<'fragment>,
}

fn lane_of(bytes: &[u8]) -> Result<Lane<'_>, TestError> {
    let view = FragmentView::validate(bytes).map_err(|source| TestError::Validate { source })?;
    let mut entities = Vec::new();
    for entity in view.entities() {
        let ordinal = usize::try_from(entity.name.raw).map_err(|_| TestError::Coordinate)?;
        let name = view
            .atoms()
            .nth(ordinal)
            .map(|atom| atom.bytes)
            .ok_or(TestError::Falsified("entity atom out of range"))?;
        entities.push((name, entity.kind));
    }
    let mut types = Vec::new();
    if let Some(mut cursor) = view.type_facts() {
        for row in cursor.by_ref() {
            types.push(row.map_err(|_| TestError::Falsified("type fact decode failed"))?);
        }
    }
    Ok(Lane {
        entities,
        types,
        view,
    })
}

/// Finds the ordinal of one entity by exact name and kind.
fn entity_ordinal(lane: &Lane<'_>, name: &[u8], kind: EntityKind) -> Result<usize, TestError> {
    lane.entities
        .iter()
        .position(|(known, known_kind)| *known == name && *known_kind == kind)
        .ok_or(TestError::Falsified("entity absent"))
}

/// Finds the one type-fact row owned by one entity ordinal.
fn owned_row(lane: &Lane<'_>, owner: usize) -> Result<usize, TestError> {
    lane.types
        .iter()
        .position(|fact| fact.owner.raw as usize == owner)
        .ok_or(TestError::Falsified("entity type row absent"))
}

/// Borrows one type-fact row by its wire lane ordinal.
fn row_at<'lane, 'fragment>(
    lane: &'lane Lane<'fragment>,
    target: u32,
) -> Result<&'lane DecodedTypeFact<'fragment>, TestError> {
    lane.types
        .get(usize::try_from(target).map_err(|_| TestError::Coordinate)?)
        .ok_or(TestError::Falsified("child row outside the type lane"))
}

/// Asserts one child row is exactly the expected primitive shape and text.
fn expect_primitive(
    lane: &Lane<'_>,
    target: u32,
    shape: PrimitiveShape,
    text: Option<&[u8]>,
) -> Result<(), TestError> {
    let row = row_at(lane, target)?;
    if row.record.tag != SemanticTypeTag::Primitive
        || row.record.payload0 != u32::from(shape)
        || row.record.text != text
    {
        return Err(TestError::Falsified(
            "child row is not the expected primitive",
        ));
    }
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
fn skip_name_cell(payload: &[u8], cursor: &mut usize) -> Result<(), TestError> {
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
        _ => return Err(TestError::Falsified("truncated name cell")),
    }
    Ok(())
}

/// Advances over the variable-size cells of one type-fact record.
fn skip_record_cells(payload: &[u8], cursor: &mut usize) -> Result<(), TestError> {
    const NOMINAL_NONE_BYTES: usize = 1;
    const NOMINAL_LOCAL_BYTES: usize = 1 + 4;
    const NOMINAL_EXTERNAL_BYTES: usize = 1 + 32 + 4;
    const CHILD_SPAN_BYTES: usize = 8;

    skip_name_cell(payload, cursor)?;
    skip_name_cell(payload, cursor)?;
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
fn child_target(payload: &[u8], cursor: &mut usize) -> Result<Option<u32>, TestError> {
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
    skip_name_cell(payload, cursor)?;
    *cursor = cursor.checked_add(1).ok_or(TestError::Coordinate)?;
    Ok(target)
}

/// Decodes the local child-row targets of one type-fact row from the raw
/// payload: skip every record of both segments, then walk the pooled child
/// lane to the row's span.
fn type_child_targets(lane: &Lane<'_>, row: usize) -> Result<Vec<u32>, TestError> {
    const RECORD_FIXED_CELLS: usize = 4 + 1 + 4 + 4;

    let payload = lane
        .view
        .type_fact_payload()
        .ok_or(TestError::Falsified("no type fact section"))?;
    let declared = usize::try_from(wire_word(payload, 0)?).map_err(|_| TestError::Coordinate)?;
    let computed = usize::try_from(wire_word(payload, 4)?).map_err(|_| TestError::Coordinate)?;
    let mut cursor = 8usize;
    for _ in 0..(declared + computed) {
        cursor = cursor
            .checked_add(RECORD_FIXED_CELLS)
            .ok_or(TestError::Coordinate)?;
        skip_record_cells(payload, &mut cursor)?;
    }
    let fact = lane
        .types
        .get(row)
        .ok_or(TestError::Falsified("type row absent"))?;
    let start = usize::try_from(fact.record.children.start).map_err(|_| TestError::Coordinate)?;
    let length = usize::try_from(fact.record.children.length).map_err(|_| TestError::Coordinate)?;
    let child_count =
        usize::try_from(wire_word(payload, cursor)?).map_err(|_| TestError::Coordinate)?;
    cursor = cursor.checked_add(4).ok_or(TestError::Coordinate)?;
    let end = start.checked_add(length).ok_or(TestError::Coordinate)?;
    if end > child_count {
        return Err(TestError::Falsified("child span outside the pooled lane"));
    }
    let mut targets = Vec::new();
    for position in 0..end {
        let target = child_target(payload, &mut cursor)?;
        if position >= start {
            let Some(target) = target else {
                return Err(TestError::Falsified("child target is not a local row"));
            };
            targets.push(target);
        }
    }
    Ok(targets)
}

/// The result slot of `-> tuple[int, str]` owns a Tuple record whose exactly
/// two children are the int and str rows in written order.
#[test]
fn tuple_result_slot_carries_exactly_two_element_children() -> Result<(), TestError> {
    let bytes = attempt_fragment(TUPLE_RESULT, "tuple")?.map_err(TestError::Rejected)?;
    let lane = lane_of(&bytes)?;
    let slot = entity_ordinal(&lane, b"f", EntityKind::Parameter)?;
    let row = owned_row(&lane, slot)?;
    let fact = &lane.types[row];
    if fact.record.tag != SemanticTypeTag::Tuple {
        return Err(TestError::Falsified("result slot is not a Tuple"));
    }
    if fact.record.children.length != 2 {
        return Err(TestError::Falsified(
            "tuple result slot does not own exactly 2 children",
        ));
    }
    let children = type_child_targets(&lane, row)?;
    expect_primitive(&lane, children[0], PrimitiveShape::Integer, None)?;
    expect_primitive(&lane, children[1], PrimitiveShape::Str, None)?;
    Ok(())
}

/// The result slot of `-> dict[str, int]` compiles and owns an Apply record
/// whose exactly three children are the dict base and the str, int arguments
/// in written order.
#[test]
fn dict_result_slot_applies_base_and_arguments_in_order() -> Result<(), TestError> {
    let bytes = attempt_fragment(DICT_RESULT, "dict")?.map_err(TestError::Rejected)?;
    let lane = lane_of(&bytes)?;
    let slot = entity_ordinal(&lane, b"g", EntityKind::Parameter)?;
    let row = owned_row(&lane, slot)?;
    let fact = &lane.types[row];
    if fact.record.tag != SemanticTypeTag::Apply {
        return Err(TestError::Falsified("result slot is not an Apply"));
    }
    if fact.record.children.length != 3 {
        return Err(TestError::Falsified(
            "dict result slot does not own exactly 3 children",
        ));
    }
    let children = type_child_targets(&lane, row)?;
    expect_primitive(&lane, children[0], PrimitiveShape::Builtin, Some(b"dict"))?;
    expect_primitive(&lane, children[1], PrimitiveShape::Str, None)?;
    expect_primitive(&lane, children[2], PrimitiveShape::Integer, None)?;
    Ok(())
}

/// A ten-parameter function survives the raised per-fact child bound and its
/// function row carries all ten ordered parameter children.
#[test]
fn ten_parameter_function_keeps_every_parameter_child() -> Result<(), TestError> {
    with_image(TEN_PARAMETERS, "ten", |ir| {
        let function = ir
            .items()
            .find(|item| item.kind() == ItemKind::Function && item.name() == b"h")
            .ok_or(TestError::Falsified("function h absent"))?;
        let ty = function
            .semantic_type()
            .ok_or(TestError::Falsified("function h is untyped"))?;
        let TypeExpr::Concrete(ConcreteType::Function { parameters, .. }) = ir
            .ty(ty)
            .ok_or(TestError::Falsified("function type row absent"))?
        else {
            return Err(TestError::Falsified(
                "function type is not a concrete function",
            ));
        };
        let parameters = ir
            .tuple_elements(parameters)
            .ok_or(TestError::Falsified("parameter list absent"))?;
        let expected: [&[u8]; 10] =
            [b"a", b"b", b"c", b"d", b"e", b"f", b"g", b"h2", b"i", b"j"];
        if parameters.len() != expected.len() {
            return Err(TestError::Falsified("function row lost parameter children"));
        }
        for (parameter, name) in parameters.iter().zip(expected) {
            let Some(label) = parameter.label else {
                return Err(TestError::Falsified("parameter label absent"));
            };
            if ir.atom(label) != Some(name) {
                return Err(TestError::Falsified("parameter children are out of order"));
            }
        }
        Ok(())
    })
}

/// The raised bound stays honest: one parameter past it rejects with the
/// exact typed ChildCapacity terminal, never a panic or a silent truncation.
#[test]
fn seventeen_parameter_function_rejects_with_child_capacity() -> Result<(), TestError> {
    match attempt_fragment(SEVENTEEN_PARAMETERS, "seventeen")? {
        Err(rejection) if rejection.cause == FactFault::ChildCapacity => Ok(()),
        Err(rejection) => Err(TestError::Rejected(rejection)),
        Ok(_) => Err(TestError::Falsified(
            "a 17-parameter function was admitted past the bound",
        )),
    }
}

/// The result slot of `-> int | str` owns a Union record whose exactly two
/// children are the int and str rows in written order.
#[test]
fn union_result_slot_carries_exactly_two_member_children() -> Result<(), TestError> {
    let bytes = attempt_fragment(UNION_RESULT, "union")?.map_err(TestError::Rejected)?;
    let lane = lane_of(&bytes)?;
    let slot = entity_ordinal(&lane, b"u", EntityKind::Parameter)?;
    let row = owned_row(&lane, slot)?;
    let fact = &lane.types[row];
    if fact.record.tag != SemanticTypeTag::Union {
        return Err(TestError::Falsified("result slot is not a Union"));
    }
    if fact.record.children.length != 2 {
        return Err(TestError::Falsified(
            "union result slot does not own exactly 2 children",
        ));
    }
    let children = type_child_targets(&lane, row)?;
    expect_primitive(&lane, children[0], PrimitiveShape::Integer, None)?;
    expect_primitive(&lane, children[1], PrimitiveShape::Str, None)?;
    Ok(())
}
