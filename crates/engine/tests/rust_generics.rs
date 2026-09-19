//! Falsifies lossless declaration-site generic emission against a real Cargo
//! fixture project. Every test drives the complete direct-HIR compile through
//! rust-analyzer and reopens the validated extension pools: generic parameter
//! names, kinds (`Type`/`Lifetime`/`ConstValue`), ordered inline bounds, and
//! the exact terminal for forms the pass cannot yet preserve.

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
use backend_semantic::ir::{
    DecodedTypeParameter, DecodedTypeParameterBound, DecodedTypeParameterKind,
    DecodedTypeParameterSemantics, EntityId, EntityKind, FragmentError, FragmentView,
    LanguageExtensionWireFact as _, NominalRef, ReopenedTypeParameterList, SemanticTypeTag,
    TypeReason,
};
use backend_semantic::vocabulary::{LanguageProfile, RustEdition, Stage};
use thiserror::Error;

/// Separates concurrently executing fixtures created during one process lifetime.
static FIXTURE_SEQUENCE: AtomicU64 = AtomicU64::new(0);

/// Builds one real Cargo fixture package in a fresh temporary directory.
fn fixture_root(body: &str) -> Result<PathBuf, TestError> {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(TestError::Clock)?
        .as_nanos();
    let sequence = FIXTURE_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let root = std::env::temp_dir().join(format!("nudox-rust-generics-{nonce}-{sequence}"));
    fs::create_dir_all(root.join("src")).map_err(|source| TestError::Io {
        operation: "create fixture",
        source,
    })?;
    fs::write(
        root.join("Cargo.toml"),
        "[package]\nname = \"generics_fixture\"\nversion = \"0.1.0\"\nedition = \"2024\"\n",
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
        backend_engine::driver::NativeTool::Rustc,
        &tool,
        b"compiler-driver-rust-generics-fixture",
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

/// Labels one compile failure class for typed test reports.
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
    Validate(#[from] FragmentError),
    /// One pooled type-parameter cell failed its reopen law.
    #[error("extension pool cell failed to reopen")]
    PoolFault(#[from] backend_semantic::ir::ExtensionPoolFault),
    /// A decoded plane disagreed with the emitted lane.
    #[error("lane falsifier failed: {0}")]
    Falsified(&'static str),
    /// A platform coordinate could not be converted.
    #[error("coordinate conversion failed")]
    Coordinate,
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
fn rust_extension(
    view: &FragmentView<'_>,
    ordinal: usize,
) -> Result<backend_semantic::ir::RustFacts, TestError> {
    let payload = view
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
        offset + rows * 4 + usize::try_from(fact_ordinal).map_err(|_| TestError::Coordinate)? * 24;
    backend_semantic::ir::RustFacts::decode(payload, at)
        .ok_or(TestError::Falsified("rust row decode"))
}

/// Reopens the exact ordered generic parameters owned by one entity row.
fn reopened_parameters<'fragment>(
    view: &'fragment FragmentView<'fragment>,
    ordinal: usize,
) -> Result<
    (
        backend_semantic::ir::ReopenedExtensionPools<'fragment>,
        Vec<DecodedTypeParameter<'fragment>>,
    ),
    TestError,
> {
    let extension = rust_extension(view, ordinal)?;
    let pools = view
        .discover()
        .extension_pools()
        .map_err(|_| TestError::Falsified("extension pools failed to reopen"))?
        .ok_or(TestError::Falsified("no extension pools"))?;
    let list = pools.type_parameter_list(extension.where_clauses)?;
    let ReopenedTypeParameterList::Exact(list) = list else {
        return Err(TestError::Falsified(
            "legacy start-only type parameter list",
        ));
    };
    let mut parameters = Vec::new();
    for parameter in list.cursor()? {
        parameters.push(parameter?);
    }
    Ok((pools, parameters))
}

/// Decodes every type fact in durable ordinal order.
fn type_facts<'fragment>(
    view: &'fragment FragmentView<'fragment>,
) -> Result<Vec<backend_semantic::ir::DecodedTypeFact<'fragment>>, TestError> {
    let mut facts = Vec::new();
    if let Some(mut cursor) = view.type_facts() {
        for row in cursor.by_ref() {
            facts.push(row.map_err(|_| TestError::Falsified("type fact decode"))?);
        }
    }
    Ok(facts)
}

/// Decodes one parameter's ordered bound spellings: a foreign trait bound is
/// the exact spelling of its unresolved external row, and a lifetime bound
/// keeps its leading quote.
fn bound_spellings<'fragment>(
    view: &'fragment FragmentView<'fragment>,
    pools: &'fragment backend_semantic::ir::ReopenedExtensionPools<'fragment>,
    parameter: DecodedTypeParameter<'fragment>,
) -> Result<Vec<&'fragment [u8]>, TestError> {
    let bounds = pools
        .type_parameter_bounds(parameter)?
        .ok_or(TestError::Falsified("type parameter bounds absent"))?;
    let facts = type_facts(view)?;
    let mut spellings = Vec::new();
    for index in 0..bounds.length {
        match bounds.get(index)? {
            DecodedTypeParameterBound::Type(row) => {
                let fact = facts
                    .get(usize::try_from(row).map_err(|_| TestError::Coordinate)?)
                    .ok_or(TestError::Falsified("bound type row absent"))?;
                spellings.push(
                    fact.record
                        .text
                        .ok_or(TestError::Falsified("bound spelling absent"))?,
                );
            }
            DecodedTypeParameterBound::Lifetime(lifetime) => spellings.push(lifetime),
        }
    }
    Ok(spellings)
}

/// Finds the ordinal of one entity by exact name and kind.
fn entity_ordinal(
    view: &FragmentView<'_>,
    name: &[u8],
    kind: EntityKind,
) -> Result<usize, TestError> {
    for (ordinal, entity) in view.entities().enumerate() {
        let atom = usize::try_from(entity.name.raw).map_err(|_| TestError::Coordinate)?;
        let spelling = view.atoms().nth(atom).map(|atom| atom.bytes);
        if spelling == Some(name) && entity.kind == kind {
            return Ok(ordinal);
        }
    }
    Err(TestError::Falsified("entity absent"))
}

/// Asserts one decoded unknown record carries the exact reason and spelling.
fn assert_unknown(
    fact: &backend_semantic::ir::DecodedTypeFact<'_>,
    reason: TypeReason,
    spelling: &[u8],
) -> Result<(), TestError> {
    if fact.record.tag != SemanticTypeTag::Unknown
        || fact.record.payload0 != u32::from(reason)
        || fact.record.text != Some(spelling)
    {
        return Err(TestError::Falsified(
            "bound record is not the expected unknown",
        ));
    }
    Ok(())
}

/// A declaration-site type parameter emits with its foreign `Clone` bound as
/// an unresolved external row, never a fabricated nominal or an `OracleGap`.
#[test]
fn declaration_site_type_bound_emits_foreign_unresolved() -> Result<(), TestError> {
    let bytes = compile_fixture("pub struct S<T: Clone> { pub v: T }\n")?;
    let view = FragmentView::validate(&bytes)?;
    let s = entity_ordinal(&view, b"S", EntityKind::Record)?;
    let (pools, parameters) = reopened_parameters(&view, s)?;
    if parameters.len() != 1 {
        return Err(TestError::Falsified("expected one generic parameter"));
    }
    let parameter = parameters[0];
    if parameter.name != b"T" {
        return Err(TestError::Falsified("type parameter name lost"));
    }
    let DecodedTypeParameterSemantics::Exact {
        kind: DecodedTypeParameterKind::Type { .. },
        ..
    } = parameter.semantics
    else {
        return Err(TestError::Falsified("type parameter kind lost"));
    };
    let bounds = pools
        .type_parameter_bounds(parameter)?
        .ok_or(TestError::Falsified("type parameter bounds absent"))?;
    if bounds.length != 1 {
        return Err(TestError::Falsified("expected one bound"));
    }
    let DecodedTypeParameterBound::Type(row) = bounds.get(0)? else {
        return Err(TestError::Falsified("Clone bound is not a type"));
    };
    let facts = type_facts(&view)?;
    let fact = facts
        .get(usize::try_from(row).map_err(|_| TestError::Coordinate)?)
        .ok_or(TestError::Falsified("bound type row absent"))?;
    assert_unknown(fact, TypeReason::UnresolvedExternal, b"Clone")
}

/// A lifetime parameter emits with `Lifetime` kind and its written spelling.
#[test]
fn declaration_site_lifetime_parameter_emits() -> Result<(), TestError> {
    let bytes = compile_fixture("pub struct S<'a> { pub v: &'a u8 }\n")?;
    let view = FragmentView::validate(&bytes)?;
    let s = entity_ordinal(&view, b"S", EntityKind::Record)?;
    let (_pools, parameters) = reopened_parameters(&view, s)?;
    if parameters.len() != 1 {
        return Err(TestError::Falsified("expected one lifetime parameter"));
    }
    let parameter = parameters[0];
    if parameter.name != b"'a" {
        return Err(TestError::Falsified("lifetime parameter name lost"));
    }
    if !matches!(
        parameter.semantics,
        DecodedTypeParameterSemantics::Exact {
            kind: DecodedTypeParameterKind::Lifetime,
            ..
        }
    ) {
        return Err(TestError::Falsified("lifetime parameter kind lost"));
    }
    Ok(())
}

/// A const-generic declaration emits a `ConstValue` parameter whose declared
/// value type is preserved, even though the value type stays an honest
/// `NoIrRepresentation` unknown for now.
#[test]
fn declaration_site_const_parameter_emits() -> Result<(), TestError> {
    let bytes = compile_fixture("pub struct S<const N: usize> { pub v: u8 }\n")?;
    let view = FragmentView::validate(&bytes)?;
    let s = entity_ordinal(&view, b"S", EntityKind::Record)?;
    let (_pools, parameters) = reopened_parameters(&view, s)?;
    if parameters.len() != 1 {
        return Err(TestError::Falsified("expected one const parameter"));
    }
    let parameter = parameters[0];
    if parameter.name != b"N" {
        return Err(TestError::Falsified("const parameter name lost"));
    }
    let DecodedTypeParameterSemantics::Exact {
        kind: DecodedTypeParameterKind::ConstValue { value_type },
        ..
    } = parameter.semantics
    else {
        return Err(TestError::Falsified("const parameter kind lost"));
    };
    let facts = type_facts(&view)?;
    let fact = facts
        .get(usize::try_from(value_type).map_err(|_| TestError::Coordinate)?)
        .ok_or(TestError::Falsified("const value type row absent"))?;
    assert_unknown(fact, TypeReason::NoIrRepresentation, b"usize")
}

/// A const value default is preserved as its exact written expression
/// spelling on the const-defaults suffix list, never silently dropped.
#[test]
fn const_parameter_default_keeps_written_spelling() -> Result<(), TestError> {
    let bytes =
        compile_fixture("pub struct S<const N: usize = 4096> { pub v: u8 }\n")?;
    let view = FragmentView::validate(&bytes)?;
    let s = entity_ordinal(&view, b"S", EntityKind::Record)?;
    let extension = rust_extension(&view, s)?;
    let pools = view
        .discover()
        .extension_pools()
        .map_err(|_| TestError::Falsified("extension pools failed to reopen"))?
        .ok_or(TestError::Falsified("no extension pools"))?;
    let list = pools
        .atom_list(extension.const_defaults.raw)
        .map_err(|_| TestError::Falsified("const defaults absent"))?;
    if list.len() != 1 {
        return Err(TestError::Falsified(
            "const defaults misaligned with parameter run",
        ));
    }
    if !view.atoms().any(|atom| atom.bytes == b"4096") {
        return Err(TestError::Falsified("const default spelling lost"));
    }
    Ok(())
}

/// Rust makes every declared generic default trailing (E0128), so a const
/// parameter without a default can never follow one that has a default. The
/// defaults lane is therefore a suffix: a non-defaulted const parameter
/// contributes no entry and never an invalid empty atom, and the emitted
/// fragment carries no empty atom at all.
#[test]
fn const_defaults_form_a_suffix_without_empty_atoms() -> Result<(), TestError> {
    let bytes = compile_fixture(
        "pub struct S<const N: usize, const M: usize = 8> { pub v: [u8; M] }\n",
    )?;
    let view = FragmentView::validate(&bytes)?;
    if view.atoms().any(|atom| atom.bytes.is_empty()) {
        return Err(TestError::Falsified(
            "fragment carries an invalid empty atom",
        ));
    }
    let s = entity_ordinal(&view, b"S", EntityKind::Record)?;
    let extension = rust_extension(&view, s)?;
    let pools = view
        .discover()
        .extension_pools()
        .map_err(|_| TestError::Falsified("extension pools failed to reopen"))?
        .ok_or(TestError::Falsified("no extension pools"))?;
    let list = pools
        .atom_list(extension.const_defaults.raw)
        .map_err(|_| TestError::Falsified("const defaults absent"))?;
    if list.len() != 1 {
        return Err(TestError::Falsified(
            "the non-defaulted const prefix leaked into the defaults suffix",
        ));
    }
    let only = list.get(0).ok_or(TestError::Falsified("default absent"))?;
    let spelling = view
        .atoms()
        .nth(usize::try_from(only).map_err(|_| TestError::Coordinate)?)
        .map(|atom| atom.bytes);
    if spelling != Some(b"8".as_slice()) {
        return Err(TestError::Falsified("const default suffix spelling lost"));
    }
    Ok(())
}

/// A bound that names a local trait declared later in the file resolves to
/// that trait's reserved lane ordinal, never an unresolved external row or a
/// fabricated nominal.
#[test]
fn forward_local_trait_bound_resolves_to_declared_ordinal() -> Result<(), TestError> {
    let bytes = compile_fixture("pub struct S<T: Later> { pub v: T }\npub trait Later {}\n")?;
    let view = FragmentView::validate(&bytes)?;
    let s = entity_ordinal(&view, b"S", EntityKind::Record)?;
    let later = entity_ordinal(&view, b"Later", EntityKind::Trait)?;
    let (pools, parameters) = reopened_parameters(&view, s)?;
    if parameters.len() != 1 {
        return Err(TestError::Falsified("expected one generic parameter"));
    }
    let bounds = pools
        .type_parameter_bounds(parameters[0])?
        .ok_or(TestError::Falsified("type parameter bounds absent"))?;
    if bounds.length != 1 {
        return Err(TestError::Falsified("expected one forward bound"));
    }
    let DecodedTypeParameterBound::Type(row) = bounds.get(0)? else {
        return Err(TestError::Falsified("forward bound is not a trait bound"));
    };
    if usize::try_from(row).map_err(|_| TestError::Coordinate)? != later {
        return Err(TestError::Falsified(
            "forward bound did not use the local trait ordinal",
        ));
    }
    let facts = type_facts(&view)?;
    let later_id = EntityId::new(u32::try_from(later).map_err(|_| TestError::Coordinate)?);
    let fact = facts
        .iter()
        .find(|fact| fact.owner == later_id)
        .ok_or(TestError::Falsified("forward trait type fact absent"))?;
    if fact.record.tag != SemanticTypeTag::Nominal
        || fact.record.nominal != Some(NominalRef::Local(later_id))
    {
        return Err(TestError::Falsified(
            "forward trait ordinal is not the emitted nominal",
        ));
    }
    Ok(())
}

/// A declaration-site type default binds its parameter to the declared
/// ordinal: the default is a scoped relationship, never an erased `None`.
#[test]
fn declaration_site_type_default_resolves_to_declared_ordinal() -> Result<(), TestError> {
    let bytes =
        compile_fixture("pub struct Inner;\npub struct S<T = Inner> { pub v: Option<T> }\n")?;
    let view = FragmentView::validate(&bytes)?;
    let s = entity_ordinal(&view, b"S", EntityKind::Record)?;
    let inner = entity_ordinal(&view, b"Inner", EntityKind::Record)?;
    let (_pools, parameters) = reopened_parameters(&view, s)?;
    if parameters.len() != 1 {
        return Err(TestError::Falsified("expected one generic parameter"));
    }
    let default = parameters[0]
        .default
        .ok_or(TestError::Falsified("type default lost"))?;
    let facts = type_facts(&view)?;
    let fact = facts
        .get(usize::try_from(default).map_err(|_| TestError::Coordinate)?)
        .ok_or(TestError::Falsified("default type row absent"))?;
    let inner_id = EntityId::new(u32::try_from(inner).map_err(|_| TestError::Coordinate)?);
    if fact.record.tag != SemanticTypeTag::Nominal
        || fact.record.nominal != Some(NominalRef::Local(inner_id))
    {
        return Err(TestError::Falsified(
            "type default is not the local nominal",
        ));
    }
    Ok(())
}

/// A `where` clause merges into each declared parameter's single ordered run:
/// inline bounds first, then matching predicates in source order, with the
/// parameter count unchanged and no bound duplicated.
#[test]
fn where_clause_merges_into_declared_parameters() -> Result<(), TestError> {
    let bytes = compile_fixture(
        "pub struct S<T: Eq, U> where T: Display + Debug, U: Clone { pub t: T, pub u: U }\n",
    )?;
    let view = FragmentView::validate(&bytes)?;
    let s = entity_ordinal(&view, b"S", EntityKind::Record)?;
    let (pools, parameters) = reopened_parameters(&view, s)?;
    if parameters.len() != 2 {
        return Err(TestError::Falsified("where clause changed parameter count"));
    }
    let t = bound_spellings(&view, &pools, parameters[0])?;
    if t.as_slice() != [b"Eq".as_slice(), b"Display".as_slice(), b"Debug".as_slice()] {
        return Err(TestError::Falsified("T merged bounds are out of order"));
    }
    let u = bound_spellings(&view, &pools, parameters[1])?;
    if u.as_slice() != [b"Clone".as_slice()] {
        return Err(TestError::Falsified("U merged bounds are wrong"));
    }
    Ok(())
}

/// A `where` predicate whose subject is not a declared parameter emits into
/// the free-predicate lane: the subject keeps its exact written spelling and
/// its ordered bound run, instead of the exact unsupported terminal.
#[test]
fn where_free_predicate_emits_subject_and_bounds() -> Result<(), TestError> {
    let bytes = compile_fixture("pub fn f<T>() where Vec<T>: Clone {}\n")?;
    let view = FragmentView::validate(&bytes)?;
    let f = entity_ordinal(&view, b"f", EntityKind::Function)?;
    let extension = rust_extension(&view, f)?;
    let pools = view
        .discover()
        .extension_pools()
        .map_err(|_| TestError::Falsified("extension pools failed to reopen"))?
        .ok_or(TestError::Falsified("no extension pools"))?;
    let range = pools
        .free_predicate_list(extension.free_predicates)
        .map_err(|_| TestError::Falsified("free predicate list absent"))?;
    if range.length != 1 {
        return Err(TestError::Falsified("expected one free predicate"));
    }
    let predicate = pools
        .free_predicate(range.start)
        .map_err(|_| TestError::Falsified("free predicate absent"))?;
    let facts = type_facts(&view)?;
    let subject = facts
        .get(usize::try_from(predicate.subject).map_err(|_| TestError::Coordinate)?)
        .ok_or(TestError::Falsified("free subject row absent"))?;
    assert_unknown(
        subject,
        TypeReason::NoIrRepresentation,
        b"Vec<T>",
    )?;
    let bounds = pools
        .free_predicate_bounds(predicate)
        .map_err(|_| TestError::Falsified("free bounds absent"))?;
    if bounds.length != 1 {
        return Err(TestError::Falsified("expected one free bound"));
    }
    let DecodedTypeParameterBound::Type(row) = bounds
        .get(0)
        .map_err(|_| TestError::Falsified("free bound decode"))?
    else {
        return Err(TestError::Falsified("free Clone bound is not a type"));
    };
    let bound = facts
        .get(usize::try_from(row).map_err(|_| TestError::Coordinate)?)
        .ok_or(TestError::Falsified("free bound row absent"))?;
    assert_unknown(bound, TypeReason::UnresolvedExternal, b"Clone")
}

/// An argument-position `impl Trait` keeps its exact written bound text as an
/// unresolved external row, never a textless `OracleGap` carrier.
#[test]
fn impl_trait_argument_keeps_written_spelling() -> Result<(), TestError> {
    let bytes = compile_fixture("pub fn f(x: impl Clone) {}\n")?;
    let view = FragmentView::validate(&bytes)?;
    let facts = type_facts(&view)?;
    let fact = facts
        .iter()
        .find(|fact| fact.record.text == Some(b"impl Clone".as_slice()))
        .ok_or(TestError::Falsified("impl Trait spelling lost"))?;
    assert_unknown(fact, TypeReason::UnresolvedExternal, b"impl Clone")
}

/// A mixed const/type generic list emits in written order, not the HIR
/// internal ordering, with each parameter's exact kind preserved.
#[test]
fn mixed_const_and_type_parameters_emit_in_written_order() -> Result<(), TestError> {
    let bytes = compile_fixture("pub struct S<const N: usize, T> { pub v: T }\n")?;
    let view = FragmentView::validate(&bytes)?;
    let s = entity_ordinal(&view, b"S", EntityKind::Record)?;
    let (_pools, parameters) = reopened_parameters(&view, s)?;
    if parameters.len() != 2 {
        return Err(TestError::Falsified("mixed generic list changed count"));
    }
    if parameters[0].name != b"N" || parameters[1].name != b"T" {
        return Err(TestError::Falsified("mixed generic list lost written order"));
    }
    let DecodedTypeParameterSemantics::Exact {
        kind: DecodedTypeParameterKind::ConstValue { .. },
        ..
    } = parameters[0].semantics
    else {
        return Err(TestError::Falsified("const parameter kind lost"));
    };
    let DecodedTypeParameterSemantics::Exact {
        kind: DecodedTypeParameterKind::Type { .. },
        ..
    } = parameters[1].semantics
    else {
        return Err(TestError::Falsified("type parameter kind lost"));
    };
    Ok(())
}

/// A `where 'a: 'b` predicate lands as a lifetime bound on the `'a` lifetime
/// parameter and never as a type bound or a duplicate parameter row.
#[test]
fn where_lifetime_bound_lands_on_lifetime_parameter() -> Result<(), TestError> {
    let bytes = compile_fixture("pub struct S<'a, 'b> where 'a: 'b { pub v: &'a u8 }\n")?;
    let view = FragmentView::validate(&bytes)?;
    let s = entity_ordinal(&view, b"S", EntityKind::Record)?;
    let (pools, parameters) = reopened_parameters(&view, s)?;
    if parameters.len() != 2 {
        return Err(TestError::Falsified("lifetime where clause changed count"));
    }
    if parameters[0].name != b"'a" || parameters[1].name != b"'b" {
        return Err(TestError::Falsified("lifetime parameter order lost"));
    }
    let a = bound_spellings(&view, &pools, parameters[0])?;
    if a.as_slice() != [b"'b".as_slice()] {
        return Err(TestError::Falsified("'a lifetime bound lost"));
    }
    let b = bound_spellings(&view, &pools, parameters[1])?;
    if !b.is_empty() {
        return Err(TestError::Falsified("'b gained a bound"));
    }
    Ok(())
}
