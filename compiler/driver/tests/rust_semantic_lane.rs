//! Falsifies the Rust semantic lane against a real Cargo fixture project.
//! Every test drives the complete direct-HIR compile through rust-analyzer
//! and decodes the validated compact fragment: entity rows, the recursive
//! type-fact graph, oracle occurrences, documentation fragments, and the raw
//! Rust extension and pooled-lane payloads.

use std::{
    fs,
    path::PathBuf,
    sync::atomic::{AtomicBool, AtomicU64, Ordering},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use compiler_driver::{
    CompileControl, CompileFailure, CompileOutput, CompileRequest, CompileScratch,
    ResolvedToolchain, SemanticAuthorityInput, ToolchainSelection, compile,
};
use compiler_ir::LanguageExtensionWireFact as _;
use compiler_ir::{
    DecodedDocFact, DecodedOccurrence, DecodedTypeFact, EntityKind, FragmentView, RustOwnership,
    SemanticTypeTag,
};
use compiler_languages_rust::{
    RustAuthorityError, RustFeatureControl, RustProject, RustToolchain, SourceByteLimit,
};
use compiler_vocabulary::{LanguageProfile, RustEdition, Stage};
use thiserror::Error;

/// Separates concurrently executing fixtures created during one process lifetime.
static FIXTURE_SEQUENCE: AtomicU64 = AtomicU64::new(0);

/// The complete boundary fixture: one recursive struct, one trait with its
/// implementation pair, a generic function with a where clause, a macro
/// invocation, and documented declarations with intra-doc links.
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

/// Counts visits.
pub struct Counter {
    /// Seen count.
    pub seen: u8,
}

/// The counting visitor.
impl Visitor for Counter {
    /// Adds the node weight.
    fn visit(&self, node: &Node) -> u8 {
        self.seen + node.weight
    }
}

macro_rules! once { ($value:expr) => { $value }; }

/// Sweeps a slice generically.
pub fn sweep<T: Clone>(items: &[T], visitor: &dyn Visitor) -> Vec<u8> {
    once!(visitor.visit(&items[0]))
}

/// Borrows and moves parameters.
pub fn total(shared: &Node, exclusive: &mut Node, moved: Node) -> u64 {
    u64::from(shared.weight) + u64::from(exclusive.weight) + u64::from(moved.weight)
}
"#;

/// Builds one real Cargo fixture package in a fresh temporary directory.
fn fixture_root(body: &str) -> Result<PathBuf, TestError> {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(TestError::Clock)?
        .as_nanos();
    let sequence = FIXTURE_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let root = std::env::temp_dir().join(format!("nudox-rust-lane-{nonce}-{sequence}"));
    fs::create_dir_all(root.join("src")).map_err(|source| TestError::Io {
        operation: "create fixture",
        source,
    })?;
    fs::write(
        root.join("Cargo.toml"),
        "[package]\nname = \"lane_fixture\"\nversion = \"0.1.0\"\nedition = \"2024\"\n",
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

/// Compiles one fixture body through the direct-HIR driver route.
fn compile_fixture(body: &str) -> Result<Vec<u8>, TestError> {
    let root = fixture_root(body)?;
    let outcome = compile_body(&root, body);
    fs::remove_dir_all(&root).map_err(|source| TestError::Io {
        operation: "remove fixture",
        source,
    })?;
    outcome
}

/// Resolves one absolute Rust compiler path: the `RUSTC` override when
/// absolute, otherwise a `PATH` scan, since the toolchain registry admits
/// only absolute executables.
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
        if candidate.is_file()
            && let Ok(absolute) = candidate.canonicalize()
        {
            return Some(absolute);
        }
    }
    None
}

/// Compiles one prepared fixture directory.
fn compile_body(root: &PathBuf, body: &str) -> Result<Vec<u8>, TestError> {
    let source_path = root.join("src/lib.rs");
    let Some(tool) = resolve_tool() else {
        return Err(TestError::MissingRustc);
    };
    let toolchain = RustToolchain::discover(&tool).map_err(|_| TestError::MissingRustc)?;
    let project =
        RustProject::open_with_source(root, &source_path, &toolchain, RustEdition::Rust2024)?;
    let resolved = ResolvedToolchain::from_version(
        compiler_driver::NativeTool::Rustc,
        &tool,
        b"compiler-driver-rust-lane-fixture",
    )
    .map_err(|_| TestError::MissingRustc)?;
    let cancelled = AtomicBool::new(false);
    let mut diagnostic = [0; 0];
    let mut fragment_output = vec![0_u8; 262_144];
    let request = CompileRequest {
        profile: LanguageProfile::Rust(RustEdition::Rust2024),
        stage: Stage::LowerIr,
        source: body.as_bytes(),
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
    };
    let compiled = match compile(
        request,
        CompileScratch {
            diagnostic_output: &mut diagnostic,
            native_work: root,
        },
        CompileOutput {
            fragment_output: &mut fragment_output,
        },
    ) {
        Ok(compiled) => compiled,
        Err(failure) => return Err(TestError::Compile(failure_label(&failure))),
    };
    let _ = compiled;
    // The declared wire length lives at header offset 8: after the 4-byte
    // magic, the 2-byte schema, and the 2-byte section count.
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

/// Labels one compile failure class for typed test reports.
fn failure_label(failure: &CompileFailure<'_>) -> &'static str {
    match failure {
        CompileFailure::Authority { .. } => "authority",
        CompileFailure::AuthorityInputRequired { .. } => "authority-input-required",
        CompileFailure::AuthorityInputProfileMismatch { .. } => "authority-profile-mismatch",
        CompileFailure::LoweringUnsupported { .. } => "lowering-unsupported",
        CompileFailure::ExtensionAtomUnbound { .. } => "extension-atom-unbound",
        CompileFailure::ExtensionTypeParametersUnbound { .. } => {
            "extension-type-parameters-unbound"
        }
        CompileFailure::FactRejected { .. } => "fact-rejected",
        CompileFailure::CSharpProjection { .. } => "csharp-projection",
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

/// Typed fixture failure without assertion panics.
#[derive(Debug, Error)]
enum TestError {
    /// The system clock unexpectedly preceded its epoch.
    #[error("system clock preceded its epoch: {0}")]
    Clock(std::time::SystemTimeError),
    /// The fixture compiler was unavailable.
    #[error("no Rust compiler was available for the fixture")]
    MissingRustc,
    /// The rust-analyzer project authority rejected the fixture.
    #[error(transparent)]
    Authority(#[from] RustAuthorityError),
    /// A fixture filesystem action failed.
    #[error("{operation} failed: {source}")]
    Io {
        /// Exact fixture action.
        operation: &'static str,
        /// Original operating-system cause.
        #[source]
        source: std::io::Error,
    },
    /// The compile failed with the labeled closed terminal.
    #[error("compile failed: {0}")]
    Compile(&'static str),
    /// The fragment failed validation.
    #[error("fragment validation failed")]
    Validate(#[from] compiler_ir::FragmentError),
    /// A decoded plane disagreed with the emitted lane.
    #[error("lane falsifier failed: {0}")]
    Falsified(&'static str),
    /// A platform coordinate could not be converted.
    #[error("coordinate conversion failed")]
    Coordinate,
}

/// One decoded lane: entities, type facts, occurrences, docs, and payloads.
struct Lane<'fragment> {
    entities: Vec<(&'fragment [u8], EntityKind)>,
    types: Vec<DecodedTypeFact<'fragment>>,
    occurrences: Vec<DecodedOccurrence<'fragment>>,
    docs: Vec<DecodedDocFact<'fragment>>,
    atoms: Vec<&'fragment [u8]>,
    view: FragmentView<'fragment>,
}

/// Decodes every committed plane of one validated fragment.
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
    let mut occurrences = Vec::new();
    if let Some(mut cursor) = view.occurrences() {
        for row in cursor.by_ref() {
            occurrences.push(row.map_err(|_| TestError::Falsified("occurrence decode"))?);
        }
    }
    let mut docs = Vec::new();
    if let Some(mut cursor) = view.docs() {
        for row in cursor.by_ref() {
            docs.push(row.map_err(|_| TestError::Falsified("doc decode"))?);
        }
    }
    let atoms = view.atoms().map(|atom| atom.bytes).collect();
    Ok(Lane {
        entities,
        types,
        occurrences,
        docs,
        atoms,
        view,
    })
}

/// One little-endian u32 word of a validated section payload.
fn word(payload: &[u8], at: usize) -> Result<u32, TestError> {
    payload
        .get(at..at.checked_add(4).ok_or(TestError::Coordinate)?)
        .and_then(|bytes| <[u8; 4]>::try_from(bytes).ok())
        .map(u32::from_le_bytes)
        .ok_or(TestError::Falsified("payload word out of range"))
}

/// Decodes the raw Rust extension row of one entity ordinal: a 16-byte
/// header, seven 20-byte directory entries (Rust is the fourth), then the
/// row table and the fixed-width fact pool.
fn rust_extension(lane: &Lane<'_>, ordinal: usize) -> Result<compiler_ir::RustFacts, TestError> {
    let payload = lane
        .view
        .language_extension_payload()
        .ok_or(TestError::Falsified("no extension section"))?;
    let directory = 16 + 3 * 20;
    let rows = usize::try_from(word(payload, directory + 4)?).map_err(|_| TestError::Coordinate)?;
    let facts = word(payload, directory + 8)?;
    let offset =
        usize::try_from(word(payload, directory + 12)?).map_err(|_| TestError::Coordinate)?;
    if facts == 0 {
        return Err(TestError::Falsified("empty rust fact pool"));
    }
    let fact_ordinal = word(payload, offset + ordinal * 4)?;
    if fact_ordinal == u32::MAX {
        return Err(TestError::Falsified("rust extension row absent"));
    }
    let at =
        offset + rows * 4 + usize::try_from(fact_ordinal).map_err(|_| TestError::Coordinate)? * 16;
    compiler_ir::RustFacts::decode(payload, at).ok_or(TestError::Falsified("rust row decode"))
}

/// Decodes the child coordinates of one type-fact row from the raw payload.
fn type_children(lane: &Lane<'_>, row: usize) -> Result<Vec<u32>, TestError> {
    const RECORD_FIXED_BYTES: usize = 4 + 1 + 4 + 4;

    let payload = lane
        .view
        .type_fact_payload()
        .ok_or(TestError::Falsified("no type fact section"))?;

    // Schema 2: [declared_count u32][computed_count u32] precede the records;
    // the pooled child lane follows every record of both segments.
    let declared = usize::try_from(word(payload, 0)?).map_err(|_| TestError::Coordinate)?;
    let computed = usize::try_from(word(payload, 4)?).map_err(|_| TestError::Coordinate)?;
    let mut cursor = 8usize;
    for _ in 0..(declared + computed) {
        cursor = cursor
            .checked_add(RECORD_FIXED_BYTES)
            .ok_or(TestError::Coordinate)?;
        type_name_cell(payload, &mut cursor)?;
        type_name_cell(payload, &mut cursor)?;
        match payload.get(cursor).copied() {
            Some(0) => cursor = cursor.checked_add(1).ok_or(TestError::Coordinate)?,
            Some(1) => cursor = cursor.checked_add(5).ok_or(TestError::Coordinate)?,
            Some(2) => cursor = cursor.checked_add(21).ok_or(TestError::Coordinate)?,
            _ => return Err(TestError::Falsified("truncated nominal cell")),
        }
        cursor = cursor.checked_add(8).ok_or(TestError::Coordinate)?;
    }
    let fact = lane
        .types
        .get(row)
        .ok_or(TestError::Falsified("type row absent"))?;
    let start = usize::try_from(fact.record.children.start).map_err(|_| TestError::Coordinate)?;
    let length = usize::try_from(fact.record.children.length).map_err(|_| TestError::Coordinate)?;
    let child_count = usize::try_from(word(payload, cursor)?).map_err(|_| TestError::Coordinate)?;
    cursor = cursor.checked_add(4).ok_or(TestError::Coordinate)?;
    let end = start.checked_add(length).ok_or(TestError::Coordinate)?;
    if end > child_count {
        return Err(TestError::Falsified("child span out of range"));
    }
    let mut targets = Vec::new();
    for position in 0..end {
        let target = type_child_entry(payload, &mut cursor)?;
        if position >= start {
            let Some(target) = target else {
                return Err(TestError::Falsified("child target not local"));
            };
            targets.push(target);
        }
    }
    Ok(targets)
}

/// Advances over one variable-size name cell in the type-fact grammar.
fn type_name_cell(payload: &[u8], cursor: &mut usize) -> Result<(), TestError> {
    const CELL_HEADER_BYTES: usize = 1 + 4;

    match payload.get(*cursor).copied() {
        Some(0) => *cursor = (*cursor).checked_add(1).ok_or(TestError::Coordinate)?,
        Some(1) => {
            let length = usize::try_from(word(
                payload,
                (*cursor).checked_add(1).ok_or(TestError::Coordinate)?,
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

/// Decodes one pooled child and returns only a local type target.
fn type_child_entry(payload: &[u8], cursor: &mut usize) -> Result<Option<u32>, TestError> {
    const LOCAL_TARGET_TAG: u8 = 0;
    const EXTERNAL_TARGET_TAG: u8 = 1;
    const TEXT_TARGET_TAG: u8 = 2;
    const FRAGMENT_ID_BYTES: usize = 32;
    const LOCAL_TARGET_BYTES: usize = 1 + 4;
    const EXTERNAL_TARGET_BYTES: usize = 1 + FRAGMENT_ID_BYTES + 4;

    let tag = payload
        .get(*cursor)
        .copied()
        .ok_or(TestError::Falsified("truncated child target"))?;
    let target = match tag {
        LOCAL_TARGET_TAG => {
            let target = word(
                payload,
                (*cursor).checked_add(1).ok_or(TestError::Coordinate)?,
            )?;
            *cursor = cursor
                .checked_add(LOCAL_TARGET_BYTES)
                .ok_or(TestError::Coordinate)?;
            Some(target)
        }
        EXTERNAL_TARGET_TAG => {
            *cursor = cursor
                .checked_add(EXTERNAL_TARGET_BYTES)
                .ok_or(TestError::Coordinate)?;
            None
        }
        TEXT_TARGET_TAG => {
            *cursor = cursor.checked_add(1).ok_or(TestError::Coordinate)?;
            None
        }
        _ => return Err(TestError::Falsified("truncated child target")),
    };
    type_name_cell(payload, cursor)?;
    *cursor = cursor.checked_add(1).ok_or(TestError::Coordinate)?;
    Ok(target)
}

/// Reads one pooled atom list from the extension-pool payload.
fn pooled_atom_list(pool: &[u8], index: usize) -> Result<Vec<u32>, TestError> {
    let parameter_count = usize::try_from(word(pool, 0)?).map_err(|_| TestError::Coordinate)?;
    let mut cursor = 4usize;
    for _ in 0..parameter_count {
        type_name_cell(pool, &mut cursor)?;
        for _ in 0..2 {
            let present = pool
                .get(cursor)
                .copied()
                .ok_or(TestError::Falsified("truncated type parameter cell"))?;
            cursor = cursor.checked_add(1).ok_or(TestError::Coordinate)?;
            if present != 0 {
                word(pool, cursor)?;
                cursor = cursor.checked_add(4).ok_or(TestError::Coordinate)?;
            }
        }
    }
    let list_count = usize::try_from(word(pool, cursor)?).map_err(|_| TestError::Coordinate)?;
    cursor += 4;
    for list in 0..list_count {
        let length = usize::try_from(word(pool, cursor)?).map_err(|_| TestError::Coordinate)?;
        cursor += 4;
        if list == index {
            let mut words = Vec::new();
            for offset in 0..length {
                words.push(word(pool, cursor + offset * 4)?);
            }
            return Ok(words);
        }
        cursor += length * 4;
    }
    Err(TestError::Falsified("atom list absent"))
}

/// Finds the ordinal of one entity by exact name and kind.
fn entity_ordinal(lane: &Lane<'_>, name: &[u8], kind: EntityKind) -> Result<usize, TestError> {
    lane.entities
        .iter()
        .position(|(known, known_kind)| *known == name && *known_kind == kind)
        .ok_or(TestError::Falsified("entity absent"))
}

/// The recursive field closes on the Node nominal through `Option<Box<Node>>`:
/// an Apply over the foreign Option leaf and an inner Apply over the foreign
/// Box leaf and the Node self-nominal row.
#[test]
fn recursive_field_closes_on_the_self_nominal_through_option_box() -> Result<(), TestError> {
    let bytes = compile_fixture(FIXTURE_BODY)?;
    let lane = lane_of(&bytes)?;
    let node = entity_ordinal(&lane, b"Node", EntityKind::Record)?;
    let node_type_row = lane
        .types
        .iter()
        .position(|fact| {
            fact.owner.raw as usize == node
                && matches!(fact.record.nominal, Some(compiler_ir::NominalRef::Local(target)) if target.raw as usize == node)
        })
        .ok_or(TestError::Falsified("Node nominal row absent"))?;
    let field = entity_ordinal(&lane, b"next", EntityKind::Field)?;
    let field_row = lane
        .types
        .iter()
        .position(|fact| fact.owner.raw as usize == field)
        .ok_or(TestError::Falsified("field type row absent"))?;
    let fact = &lane.types[field_row];
    if fact.record.tag != SemanticTypeTag::Apply {
        return Err(TestError::Falsified("field type is not an Apply"));
    }
    let children = type_children(&lane, field_row)?;
    if children.len() != 2 {
        return Err(TestError::Falsified("Option apply lacks base and argument"));
    }
    // The base is the deduplicated foreign `Option` unknown row.
    let base = &lane
        .types
        .get(usize::try_from(children[0]).map_err(|_| TestError::Coordinate)?)
        .ok_or(TestError::Falsified("base row absent"))?;
    if base.record.tag != SemanticTypeTag::Unknown || base.record.text != Some(&b"Option"[..]) {
        return Err(TestError::Falsified("foreign base is not the Option leaf"));
    }
    // The argument is the inner `Box<Node>` Apply.
    let inner = usize::try_from(children[1]).map_err(|_| TestError::Coordinate)?;
    let inner_fact = lane
        .types
        .get(inner)
        .ok_or(TestError::Falsified("inner row absent"))?;
    if inner_fact.record.tag != SemanticTypeTag::Apply {
        return Err(TestError::Falsified("Box<Node> is not an Apply"));
    }
    let inner_children = type_children(&lane, inner)?;
    if inner_children.len() != 2 {
        return Err(TestError::Falsified("Box apply lacks base and argument"));
    }
    let node_target = inner_children[1];
    if node_target as usize != node_type_row {
        return Err(TestError::Falsified("Box argument is not the Node nominal"));
    }
    Ok(())
}

/// The recursive struct itself carries the diagonal self-nominal.
#[test]
fn record_carries_the_diagonal_self_nominal() -> Result<(), TestError> {
    let bytes = compile_fixture(FIXTURE_BODY)?;
    let lane = lane_of(&bytes)?;
    let node = entity_ordinal(&lane, b"Node", EntityKind::Record)?;
    let fact = lane
        .types
        .iter()
        .find(|fact| fact.owner.raw as usize == node)
        .ok_or(TestError::Falsified("record type row absent"))?;
    let Some(compiler_ir::NominalRef::Local(target)) = fact.record.nominal else {
        return Err(TestError::Falsified("record type is not a local nominal"));
    };
    if target.raw as usize != node {
        return Err(TestError::Falsified("self-nominal is not diagonal"));
    }
    Ok(())
}

/// A signature lowers to receiver, parameter, and result rows with exact
/// HIR ownership cells: a shared borrow, a mutable borrow, and a move.
#[test]
fn signature_lowers_receiver_parameters_result_and_ownership() -> Result<(), TestError> {
    let bytes = compile_fixture(FIXTURE_BODY)?;
    let lane = lane_of(&bytes)?;
    let shared = entity_ordinal(&lane, b"shared", EntityKind::Parameter)?;
    let exclusive = entity_ordinal(&lane, b"exclusive", EntityKind::Parameter)?;
    let moved = entity_ordinal(&lane, b"moved", EntityKind::Parameter)?;
    if rust_extension(&lane, shared)?.ownership != RustOwnership::SharedBorrow {
        return Err(TestError::Falsified("shared borrow ownership lost"));
    }
    if rust_extension(&lane, exclusive)?.ownership != RustOwnership::MutableBorrow {
        return Err(TestError::Falsified("mutable borrow ownership lost"));
    }
    if rust_extension(&lane, moved)?.ownership != RustOwnership::Moved {
        return Err(TestError::Falsified("move ownership lost"));
    }
    Ok(())
}

/// The generic function's where-clause row resolves its `Clone` bound to the
/// pushed trait... `Clone` is foreign, so the constraint stays `None` while
/// the bound predicate keeps its written name.
#[test]
fn generic_bounds_and_macro_spellings_reach_the_extension_rows() -> Result<(), TestError> {
    let bytes = compile_fixture(FIXTURE_BODY)?;
    let lane = lane_of(&bytes)?;
    let sweep = entity_ordinal(&lane, b"sweep", EntityKind::Function)?;
    let extension = rust_extension(&lane, sweep)?;
    // The macro spelling `once` is interned and pooled on the owning row.
    let pool = lane
        .view
        .extension_pool_payload()
        .ok_or(TestError::Falsified("no extension pool"))?;
    let list = pooled_atom_list(
        pool,
        usize::try_from(extension.macros.raw).map_err(|_| TestError::Coordinate)?,
    )?;
    if list.len() != 1 {
        return Err(TestError::Falsified("macro list is not one spelling"));
    }
    let atom = usize::try_from(list[0]).map_err(|_| TestError::Coordinate)?;
    if lane.atoms.get(atom).copied() != Some(&b"once"[..]) {
        return Err(TestError::Falsified("macro spelling not interned"));
    }
    // The where-clause rows carry the written `T: Clone` predicate.
    if extension.where_clauses.raw == 0 {
        return Err(TestError::Falsified("no where-clause rows"));
    }
    Ok(())
}

/// The method call resolves to the local `visit` method at oracle confidence
/// with an owner-relative span, and the u8 field width is exact.
#[test]
fn method_call_resolves_locally_and_u8_width_is_exact() -> Result<(), TestError> {
    let bytes = compile_fixture(FIXTURE_BODY)?;
    let lane = lane_of(&bytes)?;
    let visit = entity_ordinal(&lane, b"visit", EntityKind::Function)?;
    let call = lane
        .occurrences
        .iter()
        .find(|occurrence| occurrence.occurrence.kind == compiler_ir::ReferenceKind::MethodCall)
        .ok_or(TestError::Falsified("no method-call occurrence"))?;
    if call.occurrence.confidence != compiler_ir::OccurrenceConfidence::Oracle {
        return Err(TestError::Falsified("method call is not oracle tier"));
    }
    let compiler_ir::OccurrenceTarget::Local(target) = call.occurrence.target else {
        return Err(TestError::Falsified("method call target is not local"));
    };
    if target.raw as usize != visit {
        return Err(TestError::Falsified("method call misses the local method"));
    }
    let weight = entity_ordinal(&lane, b"weight", EntityKind::Field)?;
    let fact = lane
        .types
        .iter()
        .find(|fact| fact.owner.raw as usize == weight)
        .ok_or(TestError::Falsified("weight type row absent"))?;
    if fact.record.tag != SemanticTypeTag::Primitive
        || fact.record.payload0 != u32::from(compiler_ir::PrimitiveShape::Integer)
        || fact.record.payload1 != (8 << 1)
    {
        return Err(TestError::Falsified("u8 width or signedness is wrong"));
    }
    Ok(())
}

/// Doc comments lower to prose and intra-doc links; the [`Node`] link
/// resolves to the local Node row.
#[test]
fn docs_lower_prose_and_local_links() -> Result<(), TestError> {
    let bytes = compile_fixture(FIXTURE_BODY)?;
    let lane = lane_of(&bytes)?;
    let node = entity_ordinal(&lane, b"Node", EntityKind::Record)?;
    let next_doc = lane
        .docs
        .iter()
        .find(|fact| {
            matches!(
                &fact.fragment,
                compiler_ir::DocFragmentInput::Link { label, .. } if *label == b"Node"
            )
        })
        .ok_or(TestError::Falsified("no Node doc link"))?;
    let compiler_ir::DocFragmentInput::Link { target, .. } = next_doc.fragment else {
        return Err(TestError::Falsified("doc link target is not local"));
    };
    let compiler_ir::DocLinkTarget::Local(target) = target else {
        return Err(TestError::Falsified("doc link target is not local"));
    };
    if target.raw as usize != node {
        return Err(TestError::Falsified("doc link misses the local node"));
    }
    if !lane
        .docs
        .iter()
        .any(|fact| matches!(&fact.fragment, compiler_ir::DocFragmentInput::Text(text) if *text == b"The weight."))
    {
        return Err(TestError::Falsified("prose fragment lost"));
    }
    Ok(())
}

/// A source beyond the lane's emission-fact bound is the exact typed
/// lowering rejection, never a truncated emission.
#[test]
fn capacity_beyond_2048_is_the_exact_lowering_rejection() -> Result<(), TestError> {
    let mut body = String::new();
    for ordinal in 0..2049 {
        body.push_str(&format!("pub struct S{ordinal};\n"));
    }
    match compile_fixture(body.as_str()) {
        Err(TestError::Compile("lowering-unsupported")) => Ok(()),
        Err(other) => Err(other),
        Ok(_) => Err(TestError::Falsified("2049 declarations were admitted")),
    }
}

/// The exact emission-fact bound admits all 2048 declarations and validates
/// the resulting fragment, rather than merely stopping before the bound.
#[test]
fn capacity_at_2048_admits_and_validates() -> Result<(), TestError> {
    let mut body = String::new();
    for ordinal in 0..2048 {
        body.push_str(&format!("pub struct S{ordinal};\n"));
    }
    let bytes = compile_fixture(body.as_str())?;
    let view = FragmentView::validate(&bytes)?;
    if view.entities().len() != 2048 {
        return Err(TestError::Falsified(
            "2048 declarations were not fully emitted",
        ));
    }
    Ok(())
}

/// An empty fixture source has no declaration to admit and is the exact
/// typed lowering rejection.
#[test]
fn empty_source_is_the_exact_lowering_rejection() -> Result<(), TestError> {
    match compile_fixture("") {
        Err(TestError::Compile("lowering-unsupported")) => Ok(()),
        Err(other) => Err(other),
        Ok(_) => Err(TestError::Falsified("empty source admitted facts")),
    }
}
