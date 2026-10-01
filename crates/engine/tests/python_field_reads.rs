//! Focused lowering test for Python attribute-read field keys.
//!
//! Non-call attribute reads become `FieldAccess` rows. A same-file field
//! stays local; an imported-module receiver becomes a `pypi` package field
//! key; unresolved attributes stay universe foreign; method calls are never
//! field keys.

#![forbid(unsafe_code)]
#![deny(clippy::expect_used, clippy::panic, clippy::unwrap_used)]

use std::{
    fs,
    process::Command,
    sync::atomic::AtomicBool,
    time::{Duration, Instant, SystemTime, SystemTimeError, UNIX_EPOCH},
};

use backend_engine::driver::{
    CompileControl, CompileFailure, CompileOutput, CompileRequest, CompileScratch, NativeTool,
    ResolvedToolchain, SemanticAuthorityInput, ToolchainSelection, compile,
};
use backend_frontend_python::legacy::{DeclarationKind, OccurrenceKind, Span, extract};
use backend_semantic::ir::{
    EntityId, EntityKind, ForeignOrigin, FragmentView, OccurrenceConfidence, OccurrenceTarget,
    ReferenceKind,
};
use backend_semantic::vocabulary::{LanguageProfile, PythonVersion, Stage};
use thiserror::Error;

static FIXTURE_SEQUENCE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

fn fixture_sequence() -> u64 {
    FIXTURE_SEQUENCE.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
}

const SOURCE: &[u8] = b"\
class Item:
    note: str
    def read_self(self):
        return self.note

def read_item(item: Item):
    return item.note

from workout.service import service

def read_foreign():
    return service.note

def call_foreign():
    return service.set_note()

def read_unresolved(obj):
    return obj.missing
";

#[derive(Debug, Error)]
enum TestError {
    #[error("clock failed")]
    Clock(#[source] SystemTimeError),
    #[error("I/O failed: {0}")]
    Io(&'static str, #[source] std::io::Error),
    #[error("python3 unavailable")]
    MissingPython,
    #[error("python tool failed")]
    Tool(#[source] std::io::Error),
    #[error("toolchain resolution failed")]
    Resolve,
    #[error("compile failed: {0}")]
    Compile(&'static str),
    #[error("validate failed")]
    Validate,
    #[error("extract failed")]
    Extract,
    #[error("lane falsifier: {0}")]
    Falsified(&'static str),
}

fn failure_label(failure: &CompileFailure<'_>) -> &'static str {
    match failure {
        CompileFailure::LoweringUnsupported { .. } => "lowering-unsupported",
        _ => "other",
    }
}

fn entity_ordinal(
    decoded: &FragmentView<'_>,
    atoms: &[&[u8]],
    name: &[u8],
    kind: EntityKind,
) -> Result<EntityId, TestError> {
    decoded
        .entities()
        .find(|entity| {
            atoms.get(entity.name.raw as usize).copied() == Some(name) && entity.kind == kind
        })
        .map(|entity| entity.entity)
        .ok_or(TestError::Falsified("entity ordinal absent"))
}

fn method_ordinal_in_class(
    decoded: &FragmentView<'_>,
    atoms: &[&[u8]],
    module: &backend_frontend_python::legacy::ModuleFacts,
    class_name: &[u8],
    method_name: &[u8],
) -> Result<EntityId, TestError> {
    let class_span = class_span(module, class_name)?;
    let mut class_method_indexes: Vec<usize> = Vec::new();
    for (index, declaration) in module.declarations.iter().enumerate() {
        if declaration.kind == DeclarationKind::Function
            && declaration.name.as_bytes() == method_name
            && declaration.span.start >= class_span.start
            && declaration.span.end <= class_span.end
        {
            class_method_indexes.push(index);
        }
    }
    if class_method_indexes.len() != 1 {
        return Err(TestError::Falsified(
            "method declaration in class not unique",
        ));
    }
    let mut prior_methods = 0_usize;
    for (index, declaration) in module.declarations.iter().enumerate() {
        if index == class_method_indexes[0] {
            break;
        }
        if declaration.kind == DeclarationKind::Function
            && declaration.name.as_bytes() == method_name
        {
            prior_methods += 1;
        }
    }
    let named_methods: Vec<EntityId> = decoded
        .entities()
        .filter(|entity| {
            entity.kind == EntityKind::Function
                && atoms.get(entity.name.raw as usize).copied() == Some(method_name)
        })
        .map(|entity| entity.entity)
        .collect();
    if named_methods.is_empty() {
        return Err(TestError::Falsified("named method entities absent"));
    }
    named_methods
        .get(prior_methods)
        .copied()
        .ok_or(TestError::Falsified("method entity ordinal absent"))
}

fn field_ordinal_in_class(
    decoded: &FragmentView<'_>,
    atoms: &[&[u8]],
    module: &backend_frontend_python::legacy::ModuleFacts,
    class_name: &[u8],
    field_name: &[u8],
) -> Result<EntityId, TestError> {
    let class_span = class_span(module, class_name)?;
    let mut class_field_indexes: Vec<usize> = Vec::new();
    for (index, declaration) in module.declarations.iter().enumerate() {
        if declaration.kind == DeclarationKind::Field
            && declaration.name.as_bytes() == field_name
            && declaration.span.start >= class_span.start
            && declaration.span.end <= class_span.end
        {
            class_field_indexes.push(index);
        }
    }
    if class_field_indexes.len() != 1 {
        return Err(TestError::Falsified(
            "field declaration in class not unique",
        ));
    }
    let mut prior_fields = 0_usize;
    for (index, declaration) in module.declarations.iter().enumerate() {
        if index == class_field_indexes[0] {
            break;
        }
        if declaration.kind == DeclarationKind::Field && declaration.name.as_bytes() == field_name {
            prior_fields += 1;
        }
    }
    let named_fields: Vec<EntityId> = decoded
        .entities()
        .filter(|entity| {
            entity.kind == EntityKind::Field
                && atoms.get(entity.name.raw as usize).copied() == Some(field_name)
        })
        .map(|entity| entity.entity)
        .collect();
    if named_fields.is_empty() {
        return Err(TestError::Falsified("named field entities absent"));
    }
    named_fields
        .get(prior_fields)
        .copied()
        .ok_or(TestError::Falsified("field entity ordinal absent"))
}

fn class_span(
    module: &backend_frontend_python::legacy::ModuleFacts,
    class_name: &[u8],
) -> Result<Span, TestError> {
    let mut matches: Vec<Span> = Vec::new();
    for declaration in &module.declarations {
        if declaration.kind == DeclarationKind::Class && declaration.name.as_bytes() == class_name {
            matches.push(declaration.span);
        }
    }
    if matches.len() != 1 {
        return Err(TestError::Falsified("class span not unique"));
    }
    Ok(matches[0])
}

fn owner_name<'a>(
    decoded: &'a FragmentView<'a>,
    atoms: &'a [&'a [u8]],
    owner: EntityId,
) -> Result<&'a [u8], TestError> {
    decoded
        .entities()
        .find(|entity| entity.entity == owner)
        .and_then(|entity| atoms.get(entity.name.raw as usize).copied())
        .ok_or(TestError::Falsified("owner entity name absent"))
}

fn compile_fixture(
    source: &[u8],
) -> Result<(Vec<u8>, backend_frontend_python::legacy::ModuleFacts), TestError> {
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
    let work = std::env::temp_dir().join(format!(
        "nudox-python-field-reads-{nonce}-{}-{}",
        std::process::id(),
        fixture_sequence()
    ));
    fs::create_dir_all(&work).map_err(|source| TestError::Io("create scratch", source))?;
    let cancelled = AtomicBool::new(false);
    let mut output = vec![0_u8; 8 * 1024 * 1024];
    let mut diagnostic = [0_u8; 4096];
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
    let fragment = {
        let mut bytes = result.fragment.as_ref().to_vec();
        bytes.shrink_to_fit();
        bytes
    };
    let module = extract(source, PythonVersion::Python314).map_err(|_| TestError::Extract)?;
    fs::remove_dir_all(&work).map_err(|source| TestError::Io("remove scratch", source))?;
    Ok((fragment, module))
}

fn occurrences<'a>(
    decoded: &'a FragmentView<'a>,
) -> Result<Vec<backend_semantic::ir::DecodedOccurrence<'a>>, TestError> {
    let mut rows: Vec<backend_semantic::ir::DecodedOccurrence<'_>> = Vec::new();
    if let Some(mut cursor) = decoded.occurrences() {
        for row in cursor.by_ref() {
            rows.push(row.map_err(|_| TestError::Falsified("occurrence decode"))?);
        }
    }
    Ok(rows)
}

/// A resolved binding is right at either evidence tier: `Index` from the
/// syntax lane alone, or `Oracle` when pyrefly is on PATH and confirms the
/// site. These laws are about the binding, so they hold on machines with and
/// without pyrefly (CI's compiler shell provisions it; a bare host may not).
fn index_or_oracle(confidence: OccurrenceConfidence) -> bool {
    matches!(
        confidence,
        OccurrenceConfidence::Index | OccurrenceConfidence::Oracle
    )
}

#[test]
fn python_field_reads_lower_honestly() -> Result<(), TestError> {
    let (fragment, module) = compile_fixture(SOURCE)?;
    let decoded = FragmentView::validate(&fragment).map_err(|_| TestError::Validate)?;
    let atoms: Vec<&[u8]> = decoded.atoms().map(|atom| atom.bytes).collect();
    let rows = occurrences(&decoded)?;

    let item_note = field_ordinal_in_class(&decoded, &atoms, &module, b"Item", b"note")?;
    let read_self = entity_ordinal(&decoded, &atoms, b"read_self", EntityKind::Function)?;
    let read_item = entity_ordinal(&decoded, &atoms, b"read_item", EntityKind::Function)?;
    let read_foreign = entity_ordinal(&decoded, &atoms, b"read_foreign", EntityKind::Function)?;
    let call_foreign = entity_ordinal(&decoded, &atoms, b"call_foreign", EntityKind::Function)?;
    let read_unresolved =
        entity_ordinal(&decoded, &atoms, b"read_unresolved", EntityKind::Function)?;

    let extractor_reads = module
        .occurrences
        .iter()
        .filter(|occurrence| occurrence.kind == OccurrenceKind::AttributeRead)
        .count();
    if extractor_reads != 4 {
        return Err(TestError::Falsified(
            "extractor did not record four attribute reads",
        ));
    }

    let self_reads: Vec<_> = rows
        .iter()
        .filter(|row| {
            row.occurrence.kind == ReferenceKind::FieldAccess
                && row.owner == read_self
                && matches!(
                    row.occurrence.target,
                    OccurrenceTarget::Local(target) if target == item_note
                )
        })
        .collect();
    if self_reads.len() != 1 {
        return Err(TestError::Falsified(
            "self.note is not exactly one local FieldAccess",
        ));
    }

    let item_reads: Vec<_> = rows
        .iter()
        .filter(|row| {
            row.occurrence.kind == ReferenceKind::FieldAccess
                && row.owner == read_item
                && matches!(
                    row.occurrence.target,
                    OccurrenceTarget::Local(target) if target == item_note
                )
        })
        .collect();
    if item_reads.len() != 1 {
        return Err(TestError::Falsified(
            "item.note is not exactly one local FieldAccess",
        ));
    }

    let package_reads: Vec<_> = rows
        .iter()
        .filter(|row| {
            row.occurrence.kind == ReferenceKind::FieldAccess
                && row.owner == read_foreign
                && matches!(
                    &row.occurrence.target,
                    OccurrenceTarget::Foreign(key)
                        if key.path == "workout.service"
                            && key.display == "note"
                            && key.kind == Some(EntityKind::Field)
                            && matches!(
                                key.origin,
                                ForeignOrigin::Package(lineage) if lineage.name == "workout"
                            )
                )
        })
        .collect();
    if package_reads.len() != 1 {
        return Err(TestError::Falsified(
            "service.note is not exactly one package FieldAccess",
        ));
    }
    if owner_name(&decoded, &atoms, package_reads[0].owner)? != b"read_foreign" {
        return Err(TestError::Falsified(
            "service.note FieldAccess owner is not read_foreign",
        ));
    }

    let set_note_calls: Vec<_> = rows
        .iter()
        .filter(|row| {
            row.occurrence.kind == ReferenceKind::MethodCall
                && row.owner == call_foreign
                && matches!(
                    &row.occurrence.target,
                    OccurrenceTarget::Foreign(key)
                        if key.path == "workout.service"
                            && key.display == "set_note"
                            && matches!(
                                key.origin,
                                ForeignOrigin::Package(lineage) if lineage.name == "workout"
                            )
                )
        })
        .collect();
    if set_note_calls.len() != 1 {
        return Err(TestError::Falsified(
            "service.set_note() is not exactly one package MethodCall",
        ));
    }
    for row in rows.iter().filter(|row| {
        row.owner == call_foreign && row.occurrence.kind == ReferenceKind::FieldAccess
    }) {
        if matches!(
            &row.occurrence.target,
            OccurrenceTarget::Foreign(key) if key.display == "set_note"
        ) {
            return Err(TestError::Falsified(
                "service.set_note() produced a FieldAccess keyed as set_note",
            ));
        }
    }

    let unresolved_reads: Vec<_> = rows
        .iter()
        .filter(|row| {
            row.occurrence.kind == ReferenceKind::FieldAccess
                && row.owner == read_unresolved
                && matches!(
                    &row.occurrence.target,
                    OccurrenceTarget::Foreign(key)
                        if key.path == "missing"
                            && key.display == "missing"
                            && key.kind == Some(EntityKind::Field)
                            && matches!(key.origin, ForeignOrigin::Universe { ecosystem: "pypi" })
                )
        })
        .collect();
    if unresolved_reads.len() != 1 {
        return Err(TestError::Falsified(
            "obj.missing is not exactly one universe FieldAccess",
        ));
    }

    Ok(())
}

fn local_field_access<'a>(
    rows: &'a [backend_semantic::ir::DecodedOccurrence<'a>],
    owner: EntityId,
    field: EntityId,
) -> Result<&'a backend_semantic::ir::DecodedOccurrence<'a>, TestError> {
    let matches: Vec<_> = rows
        .iter()
        .filter(|row| {
            row.occurrence.kind == ReferenceKind::FieldAccess
                && row.owner == owner
                && index_or_oracle(row.occurrence.confidence)
                && matches!(
                    row.occurrence.target,
                    OccurrenceTarget::Local(target) if target == field
                )
        })
        .collect();
    if matches.len() != 1 {
        return Err(TestError::Falsified("local FieldAccess not unique"));
    }
    Ok(matches[0])
}

fn local_method_call<'a>(
    rows: &'a [backend_semantic::ir::DecodedOccurrence<'a>],
    owner: EntityId,
    method: EntityId,
) -> Result<&'a backend_semantic::ir::DecodedOccurrence<'a>, TestError> {
    let matches: Vec<_> = rows
        .iter()
        .filter(|row| {
            row.occurrence.kind == ReferenceKind::MethodCall
                && row.owner == owner
                && index_or_oracle(row.occurrence.confidence)
                && matches!(
                    row.occurrence.target,
                    OccurrenceTarget::Local(target) if target == method
                )
        })
        .collect();
    if matches.len() != 1 {
        return Err(TestError::Falsified("local MethodCall not unique"));
    }
    Ok(matches[0])
}

fn universe_field_access<'a>(
    rows: &'a [backend_semantic::ir::DecodedOccurrence<'a>],
    owner: EntityId,
    spelling: &str,
) -> Result<&'a backend_semantic::ir::DecodedOccurrence<'a>, TestError> {
    let matches: Vec<_> = rows
        .iter()
        .filter(|row| {
            row.occurrence.kind == ReferenceKind::FieldAccess
                && row.owner == owner
                && index_or_oracle(row.occurrence.confidence)
                && matches!(
                    &row.occurrence.target,
                    OccurrenceTarget::Foreign(key)
                        if key.path == spelling
                            && key.display == spelling
                            && key.kind == Some(EntityKind::Field)
                            && matches!(key.origin, ForeignOrigin::Universe { ecosystem: "pypi" })
                )
        })
        .collect();
    if matches.len() != 1 {
        return Err(TestError::Falsified("universe FieldAccess not unique"));
    }
    Ok(matches[0])
}

fn function_decl_start(
    module: &backend_frontend_python::legacy::ModuleFacts,
    name: &[u8],
) -> Result<u32, TestError> {
    let mut matches: Vec<u32> = Vec::new();
    for declaration in &module.declarations {
        if declaration.kind == DeclarationKind::Function && declaration.name.as_bytes() == name {
            matches.push(declaration.span.start);
        }
    }
    if matches.len() != 1 {
        return Err(TestError::Falsified(
            "function declaration start not unique",
        ));
    }
    Ok(matches[0])
}

fn recover_site_bytes<'a>(
    source: &'a [u8],
    owner_decl_start: u32,
    occurrence: &backend_semantic::ir::DecodedOccurrence<'a>,
) -> Result<&'a [u8], TestError> {
    let start = owner_decl_start + occurrence.occurrence.span.start;
    let end = owner_decl_start + occurrence.occurrence.span.end;
    let start = usize::try_from(start).map_err(|_| TestError::Falsified("site start"))?;
    let end = usize::try_from(end).map_err(|_| TestError::Falsified("site end"))?;
    source
        .get(start..end)
        .ok_or(TestError::Falsified("site bytes out of range"))
}

fn assert_site_spelling<'a>(
    source: &'a [u8],
    module: &backend_frontend_python::legacy::ModuleFacts,
    owner_name: &[u8],
    occurrence: &backend_semantic::ir::DecodedOccurrence<'a>,
    spelling: &[u8],
) -> Result<(), TestError> {
    let owner_start = function_decl_start(module, owner_name)?;
    let site = recover_site_bytes(source, owner_start, occurrence)?;
    if site != spelling {
        return Err(TestError::Falsified("recovered site bytes mismatch"));
    }
    Ok(())
}

fn assert_no_local_target(
    rows: &[backend_semantic::ir::DecodedOccurrence<'_>],
    owner: EntityId,
    kind: ReferenceKind,
    decoy: EntityId,
) -> Result<(), TestError> {
    for row in rows
        .iter()
        .filter(|row| row.owner == owner && row.occurrence.kind == kind)
    {
        if matches!(
            row.occurrence.target,
            OccurrenceTarget::Local(target) if target == decoy
        ) {
            return Err(TestError::Falsified("reader owned a decoy local target"));
        }
    }
    Ok(())
}

fn pin_local_field_access<'a>(
    source: &'a [u8],
    module: &backend_frontend_python::legacy::ModuleFacts,
    decoded: &FragmentView<'a>,
    atoms: &[&[u8]],
    rows: &'a [backend_semantic::ir::DecodedOccurrence<'a>],
    owner_name: &[u8],
    winner: EntityId,
    decoy: Option<EntityId>,
    spelling: &[u8],
) -> Result<(), TestError> {
    let owner = entity_ordinal(decoded, atoms, owner_name, EntityKind::Function)?;
    let row = local_field_access(rows, owner, winner)?;
    if let Some(decoy) = decoy {
        assert_no_local_target(rows, owner, ReferenceKind::FieldAccess, decoy)?;
    }
    assert_site_spelling(source, module, owner_name, row, spelling)?;
    Ok(())
}

fn pin_local_method_call<'a>(
    source: &'a [u8],
    module: &backend_frontend_python::legacy::ModuleFacts,
    decoded: &FragmentView<'a>,
    atoms: &[&[u8]],
    rows: &'a [backend_semantic::ir::DecodedOccurrence<'a>],
    owner_name: &[u8],
    winner: EntityId,
    decoy: Option<EntityId>,
    spelling: &[u8],
) -> Result<(), TestError> {
    let owner = entity_ordinal(decoded, atoms, owner_name, EntityKind::Function)?;
    let row = local_method_call(rows, owner, winner)?;
    if let Some(decoy) = decoy {
        assert_no_local_target(rows, owner, ReferenceKind::MethodCall, decoy)?;
    }
    assert_site_spelling(source, module, owner_name, row, spelling)?;
    Ok(())
}

fn pin_universe_field_access<'a>(
    source: &'a [u8],
    module: &backend_frontend_python::legacy::ModuleFacts,
    decoded: &FragmentView<'a>,
    atoms: &[&[u8]],
    rows: &'a [backend_semantic::ir::DecodedOccurrence<'a>],
    owner_name: &[u8],
    spelling: &str,
) -> Result<(), TestError> {
    let owner = entity_ordinal(decoded, atoms, owner_name, EntityKind::Function)?;
    let row = universe_field_access(rows, owner, spelling)?;
    assert_site_spelling(source, module, owner_name, row, spelling.as_bytes())?;
    Ok(())
}

#[test]
fn python_inherited_field_read_resolves_to_base() -> Result<(), TestError> {
    let source = b"class Base:\n    note: str\n\nclass Child(Base):\n    def read(self):\n        return self.note\n";
    let (fragment, module) = compile_fixture(source)?;
    let decoded = FragmentView::validate(&fragment).map_err(|_| TestError::Validate)?;
    let atoms: Vec<&[u8]> = decoded.atoms().map(|atom| atom.bytes).collect();
    let rows = occurrences(&decoded)?;
    let base_note = field_ordinal_in_class(&decoded, &atoms, &module, b"Base", b"note")?;
    let read = entity_ordinal(&decoded, &atoms, b"read", EntityKind::Function)?;
    local_field_access(&rows, read, base_note)?;
    Ok(())
}

#[test]
fn python_inherited_field_read_two_level_chain() -> Result<(), TestError> {
    let source = b"class Grand:\n    note: str\n\nclass Mid(Grand):\n    pass\n\nclass Child(Mid):\n    def read(self):\n        return self.note\n";
    let (fragment, module) = compile_fixture(source)?;
    let decoded = FragmentView::validate(&fragment).map_err(|_| TestError::Validate)?;
    let atoms: Vec<&[u8]> = decoded.atoms().map(|atom| atom.bytes).collect();
    let rows = occurrences(&decoded)?;
    let grand_note = field_ordinal_in_class(&decoded, &atoms, &module, b"Grand", b"note")?;
    let read = entity_ordinal(&decoded, &atoms, b"read", EntityKind::Function)?;
    local_field_access(&rows, read, grand_note)?;
    Ok(())
}

#[test]
fn python_inherited_field_read_ambiguous_bases_stays_universe() -> Result<(), TestError> {
    let source = b"class Left:\n    note: str\n\nclass Right:\n    note: str\n\nclass Child(Left, Right):\n    def read(self):\n        return self.note\n";
    let (fragment, module) = compile_fixture(source)?;
    let decoded = FragmentView::validate(&fragment).map_err(|_| TestError::Validate)?;
    let atoms: Vec<&[u8]> = decoded.atoms().map(|atom| atom.bytes).collect();
    let rows = occurrences(&decoded)?;
    let left_note = field_ordinal_in_class(&decoded, &atoms, &module, b"Left", b"note")?;
    let right_note = field_ordinal_in_class(&decoded, &atoms, &module, b"Right", b"note")?;
    if left_note == right_note {
        return Err(TestError::Falsified(
            "Left.note and Right.note are one field",
        ));
    }
    let read = entity_ordinal(&decoded, &atoms, b"read", EntityKind::Function)?;
    universe_field_access(&rows, read, "note")?;
    Ok(())
}

#[test]
fn python_inherited_field_read_shadows_base_field() -> Result<(), TestError> {
    let source = b"class Base:\n    note: str\n\nclass Child(Base):\n    note: str\n    def read(self):\n        return self.note\n";
    let (fragment, module) = compile_fixture(source)?;
    let decoded = FragmentView::validate(&fragment).map_err(|_| TestError::Validate)?;
    let atoms: Vec<&[u8]> = decoded.atoms().map(|atom| atom.bytes).collect();
    let rows = occurrences(&decoded)?;
    let child_note = field_ordinal_in_class(&decoded, &atoms, &module, b"Child", b"note")?;
    let read = entity_ordinal(&decoded, &atoms, b"read", EntityKind::Function)?;
    local_field_access(&rows, read, child_note)?;
    Ok(())
}

#[test]
fn python_inherited_field_read_diamond_resolves_to_base() -> Result<(), TestError> {
    let source = b"class Base:\n    note: str\n\nclass Left(Base):\n    pass\n\nclass Right(Base):\n    pass\n\nclass Child(Left, Right):\n    def read(self):\n        return self.note\n";
    let (fragment, module) = compile_fixture(source)?;
    let decoded = FragmentView::validate(&fragment).map_err(|_| TestError::Validate)?;
    let atoms: Vec<&[u8]> = decoded.atoms().map(|atom| atom.bytes).collect();
    let rows = occurrences(&decoded)?;
    let base_note = field_ordinal_in_class(&decoded, &atoms, &module, b"Base", b"note")?;
    let read = entity_ordinal(&decoded, &atoms, b"read", EntityKind::Function)?;
    local_field_access(&rows, read, base_note)?;
    Ok(())
}

#[test]
fn python_inherited_method_call_resolves_to_base() -> Result<(), TestError> {
    let source = b"class Base:\n    def set_note(self):\n        pass\n\nclass Child(Base):\n    def run(self):\n        self.set_note()\n";
    let (fragment, module) = compile_fixture(source)?;
    let decoded = FragmentView::validate(&fragment).map_err(|_| TestError::Validate)?;
    let atoms: Vec<&[u8]> = decoded.atoms().map(|atom| atom.bytes).collect();
    let rows = occurrences(&decoded)?;
    let base_set_note = method_ordinal_in_class(&decoded, &atoms, &module, b"Base", b"set_note")?;
    let run = entity_ordinal(&decoded, &atoms, b"run", EntityKind::Function)?;
    local_method_call(&rows, run, base_set_note)?;
    Ok(())
}

#[test]
fn python_inherited_method_call_shadows_base_method() -> Result<(), TestError> {
    let source = b"class Base:\n    def set_note(self):\n        pass\n\nclass Child(Base):\n    def set_note(self):\n        pass\n    def run(self):\n        self.set_note()\n";
    let (fragment, module) = compile_fixture(source)?;
    let decoded = FragmentView::validate(&fragment).map_err(|_| TestError::Validate)?;
    let atoms: Vec<&[u8]> = decoded.atoms().map(|atom| atom.bytes).collect();
    let rows = occurrences(&decoded)?;
    let child_set_note = method_ordinal_in_class(&decoded, &atoms, &module, b"Child", b"set_note")?;
    let run = entity_ordinal(&decoded, &atoms, b"run", EntityKind::Function)?;
    local_method_call(&rows, run, child_set_note)?;
    Ok(())
}

#[test]
fn python_inherited_field_read_skips_unresolved_base() -> Result<(), TestError> {
    let source = b"class Base:\n    note: str\n\nclass Child(object, Base):\n    def read(self):\n        return self.note\n";
    let (fragment, module) = compile_fixture(source)?;
    let decoded = FragmentView::validate(&fragment).map_err(|_| TestError::Validate)?;
    let atoms: Vec<&[u8]> = decoded.atoms().map(|atom| atom.bytes).collect();
    let rows = occurrences(&decoded)?;
    let base_note = field_ordinal_in_class(&decoded, &atoms, &module, b"Base", b"note")?;
    let read = entity_ordinal(&decoded, &atoms, b"read", EntityKind::Function)?;
    local_field_access(&rows, read, base_note)?;
    Ok(())
}

#[test]
fn python_inherited_field_read_generic_base() -> Result<(), TestError> {
    use backend_frontend_python::legacy::Annotation;
    let source = b"class Base:\n    note: str\n\nclass Child(Base[int]):\n    def read(self):\n        return self.note\n";
    let (fragment, module) = compile_fixture(source)?;
    let child = module
        .declarations
        .iter()
        .find(|declaration| declaration.name == "Child")
        .ok_or(TestError::Falsified("Child class absent"))?;
    let records_generic_base = child.bases.iter().any(|base| {
        matches!(
            base,
            Annotation::Generic { base: inner, .. }
                if matches!(inner.as_ref(), Annotation::Name { name, .. } if name == "Base")
        )
    });
    if !records_generic_base {
        return Err(TestError::Falsified(
            "extractor does not record Child(Base[int]) as a Generic base whose name is Base",
        ));
    }
    let decoded = FragmentView::validate(&fragment).map_err(|_| TestError::Validate)?;
    let atoms: Vec<&[u8]> = decoded.atoms().map(|atom| atom.bytes).collect();
    let rows = occurrences(&decoded)?;
    let base_note = field_ordinal_in_class(&decoded, &atoms, &module, b"Base", b"note")?;
    let read = entity_ordinal(&decoded, &atoms, b"read", EntityKind::Function)?;
    local_field_access(&rows, read, base_note)?;
    Ok(())
}

#[test]
fn python_inherited_field_read_ambiguous_local_stays_universe() -> Result<(), TestError> {
    let source = b"class Base:\n    other: str\n\nclass Child(Base):\n    note: str\n    note: int\n    def read(self):\n        return self.note\n";
    let (fragment, _module) = compile_fixture(source)?;
    let decoded = FragmentView::validate(&fragment).map_err(|_| TestError::Validate)?;
    let atoms: Vec<&[u8]> = decoded.atoms().map(|atom| atom.bytes).collect();
    let rows = occurrences(&decoded)?;
    let read = entity_ordinal(&decoded, &atoms, b"read", EntityKind::Function)?;
    universe_field_access(&rows, read, "note")?;
    Ok(())
}

#[test]
fn python_enclosing_class_method_value_same_class() -> Result<(), TestError> {
    let source = b"class Item:\n    def note(self):\n        pass\n    def read(self):\n        return self.note\n";
    let (fragment, module) = compile_fixture(source)?;
    let decoded = FragmentView::validate(&fragment).map_err(|_| TestError::Validate)?;
    let atoms: Vec<&[u8]> = decoded.atoms().map(|atom| atom.bytes).collect();
    let rows = occurrences(&decoded)?;
    let item_note = method_ordinal_in_class(&decoded, &atoms, &module, b"Item", b"note")?;
    let read = entity_ordinal(&decoded, &atoms, b"read", EntityKind::Function)?;
    local_field_access(&rows, read, item_note)?;
    Ok(())
}

#[test]
fn python_enclosing_class_method_value_inherited() -> Result<(), TestError> {
    let source = b"class Base:\n    def note(self):\n        pass\n\nclass Child(Base):\n    def read(self):\n        return self.note\n";
    let (fragment, module) = compile_fixture(source)?;
    let decoded = FragmentView::validate(&fragment).map_err(|_| TestError::Validate)?;
    let atoms: Vec<&[u8]> = decoded.atoms().map(|atom| atom.bytes).collect();
    let rows = occurrences(&decoded)?;
    let base_note = method_ordinal_in_class(&decoded, &atoms, &module, b"Base", b"note")?;
    let read = entity_ordinal(&decoded, &atoms, b"read", EntityKind::Function)?;
    local_field_access(&rows, read, base_note)?;
    Ok(())
}

#[test]
fn python_enclosing_class_method_value_two_level_chain() -> Result<(), TestError> {
    let source = b"class Grand:\n    def note(self):\n        pass\n\nclass Base(Grand):\n    pass\n\nclass Child(Base):\n    def read(self):\n        return self.note\n";
    let (fragment, module) = compile_fixture(source)?;
    let decoded = FragmentView::validate(&fragment).map_err(|_| TestError::Validate)?;
    let atoms: Vec<&[u8]> = decoded.atoms().map(|atom| atom.bytes).collect();
    let rows = occurrences(&decoded)?;
    let grand_note = method_ordinal_in_class(&decoded, &atoms, &module, b"Grand", b"note")?;
    let read = entity_ordinal(&decoded, &atoms, b"read", EntityKind::Function)?;
    local_field_access(&rows, read, grand_note)?;
    Ok(())
}

#[test]
fn python_enclosing_class_method_value_child_shadows_base() -> Result<(), TestError> {
    let source = b"class Base:\n    def note(self):\n        pass\n\nclass Child(Base):\n    def note(self):\n        pass\n    def read(self):\n        return self.note\n";
    let (fragment, module) = compile_fixture(source)?;
    let decoded = FragmentView::validate(&fragment).map_err(|_| TestError::Validate)?;
    let atoms: Vec<&[u8]> = decoded.atoms().map(|atom| atom.bytes).collect();
    let rows = occurrences(&decoded)?;
    let base_note = method_ordinal_in_class(&decoded, &atoms, &module, b"Base", b"note")?;
    let child_note = method_ordinal_in_class(&decoded, &atoms, &module, b"Child", b"note")?;
    if base_note == child_note {
        return Err(TestError::Falsified(
            "Base.note and Child.note are one method",
        ));
    }
    let read = entity_ordinal(&decoded, &atoms, b"read", EntityKind::Function)?;
    local_field_access(&rows, read, child_note)?;
    Ok(())
}

#[test]
fn python_enclosing_class_field_wins_over_method_on_class() -> Result<(), TestError> {
    let source = b"class Child:\n    note: str\n    def note(self):\n        pass\n    def read(self):\n        return self.note\n";
    let (fragment, module) = compile_fixture(source)?;
    let decoded = FragmentView::validate(&fragment).map_err(|_| TestError::Validate)?;
    let atoms: Vec<&[u8]> = decoded.atoms().map(|atom| atom.bytes).collect();
    let rows = occurrences(&decoded)?;
    let child_field = field_ordinal_in_class(&decoded, &atoms, &module, b"Child", b"note")?;
    let child_method = method_ordinal_in_class(&decoded, &atoms, &module, b"Child", b"note")?;
    if child_field == child_method {
        return Err(TestError::Falsified(
            "Child field and method share one ordinal",
        ));
    }
    let read = entity_ordinal(&decoded, &atoms, b"read", EntityKind::Function)?;
    local_field_access(&rows, read, child_field)?;
    Ok(())
}

#[test]
fn python_enclosing_class_inherited_field_wins_over_inherited_method() -> Result<(), TestError> {
    let source = b"class Base:\n    note: str\n    def note(self):\n        pass\n\nclass Child(Base):\n    def read(self):\n        return self.note\n";
    let (fragment, module) = compile_fixture(source)?;
    let decoded = FragmentView::validate(&fragment).map_err(|_| TestError::Validate)?;
    let atoms: Vec<&[u8]> = decoded.atoms().map(|atom| atom.bytes).collect();
    let rows = occurrences(&decoded)?;
    let base_field = field_ordinal_in_class(&decoded, &atoms, &module, b"Base", b"note")?;
    let base_method = method_ordinal_in_class(&decoded, &atoms, &module, b"Base", b"note")?;
    if base_field == base_method {
        return Err(TestError::Falsified(
            "Base field and method share one ordinal",
        ));
    }
    let read = entity_ordinal(&decoded, &atoms, b"read", EntityKind::Function)?;
    local_field_access(&rows, read, base_field)?;
    Ok(())
}

#[test]
fn python_enclosing_class_ambiguous_local_methods_stays_universe() -> Result<(), TestError> {
    let source = b"class Child:\n    class Helper:\n        def note(self):\n            pass\n    def note(self):\n        pass\n    def read(self):\n        return self.note\n";
    let (fragment, module) = compile_fixture(source)?;
    let decoded = FragmentView::validate(&fragment).map_err(|_| TestError::Validate)?;
    let atoms: Vec<&[u8]> = decoded.atoms().map(|atom| atom.bytes).collect();
    let rows = occurrences(&decoded)?;
    let class_span = class_span(&module, b"Child")?;
    let mut method_indexes: Vec<usize> = Vec::new();
    for (index, declaration) in module.declarations.iter().enumerate() {
        if declaration.kind == DeclarationKind::Function
            && declaration.name.as_bytes() == b"note"
            && declaration.span.start >= class_span.start
            && declaration.span.end <= class_span.end
        {
            method_indexes.push(index);
        }
    }
    if method_indexes.len() != 2 {
        return Err(TestError::Falsified(
            "Child does not declare two note methods",
        ));
    }
    let helper_note = method_ordinal_in_class(&decoded, &atoms, &module, b"Helper", b"note")?;
    let named_methods: Vec<EntityId> = decoded
        .entities()
        .filter(|entity| {
            entity.kind == EntityKind::Function
                && atoms.get(entity.name.raw as usize).copied() == Some(b"note")
        })
        .map(|entity| entity.entity)
        .collect();
    if named_methods.len() < 2 {
        return Err(TestError::Falsified("two note method entities absent"));
    }
    if named_methods.iter().all(|ordinal| *ordinal == helper_note) {
        return Err(TestError::Falsified("only one distinct note method entity"));
    }
    let read = entity_ordinal(&decoded, &atoms, b"read", EntityKind::Function)?;
    universe_field_access(&rows, read, "note")?;
    Ok(())
}

#[test]
fn python_enclosing_class_ambiguous_inherited_methods_stays_universe() -> Result<(), TestError> {
    let source = b"class Left:\n    def note(self):\n        pass\n\nclass Right:\n    def note(self):\n        pass\n\nclass Child(Left, Right):\n    def read(self):\n        return self.note\n";
    let (fragment, module) = compile_fixture(source)?;
    let decoded = FragmentView::validate(&fragment).map_err(|_| TestError::Validate)?;
    let atoms: Vec<&[u8]> = decoded.atoms().map(|atom| atom.bytes).collect();
    let rows = occurrences(&decoded)?;
    let left_note = method_ordinal_in_class(&decoded, &atoms, &module, b"Left", b"note")?;
    let right_note = method_ordinal_in_class(&decoded, &atoms, &module, b"Right", b"note")?;
    if left_note == right_note {
        return Err(TestError::Falsified(
            "Left.note and Right.note are one method",
        ));
    }
    let read = entity_ordinal(&decoded, &atoms, b"read", EntityKind::Function)?;
    universe_field_access(&rows, read, "note")?;
    Ok(())
}

#[test]
fn python_enclosing_class_ambiguous_inherited_fields_do_not_fall_through_to_method()
-> Result<(), TestError> {
    let source = b"class Left:\n    note: str\n\nclass Right:\n    note: str\n    def note(self):\n        pass\n\nclass Child(Left, Right):\n    def read(self):\n        return self.note\n";
    let (fragment, module) = compile_fixture(source)?;
    let decoded = FragmentView::validate(&fragment).map_err(|_| TestError::Validate)?;
    let atoms: Vec<&[u8]> = decoded.atoms().map(|atom| atom.bytes).collect();
    let rows = occurrences(&decoded)?;
    let left_note = field_ordinal_in_class(&decoded, &atoms, &module, b"Left", b"note")?;
    let right_note = field_ordinal_in_class(&decoded, &atoms, &module, b"Right", b"note")?;
    if left_note == right_note {
        return Err(TestError::Falsified(
            "Left.note and Right.note are one field",
        ));
    }
    let right_method = method_ordinal_in_class(&decoded, &atoms, &module, b"Right", b"note")?;
    if right_note == right_method {
        return Err(TestError::Falsified(
            "Right field and method share one ordinal",
        ));
    }
    let read = entity_ordinal(&decoded, &atoms, b"read", EntityKind::Function)?;
    universe_field_access(&rows, read, "note")?;
    Ok(())
}

#[test]
fn python_enclosing_class_imported_only_base_stays_universe() -> Result<(), TestError> {
    let source = b"from workout.service import WorkoutService\n\nclass Child(WorkoutService):\n    def read(self):\n        return self.note\n";
    let (fragment, _module) = compile_fixture(source)?;
    let decoded = FragmentView::validate(&fragment).map_err(|_| TestError::Validate)?;
    let atoms: Vec<&[u8]> = decoded.atoms().map(|atom| atom.bytes).collect();
    let rows = occurrences(&decoded)?;
    let read = entity_ordinal(&decoded, &atoms, b"read", EntityKind::Function)?;
    universe_field_access(&rows, read, "note")?;
    Ok(())
}

#[test]
fn python_super_field_read_resolves_to_base() -> Result<(), TestError> {
    let source = b"class Other:\n    note: str\n\nclass Base:\n    note: str\n\nclass Child(Base):\n    def read(self):\n        return super().note\n";
    let (fragment, module) = compile_fixture(source)?;
    let decoded = FragmentView::validate(&fragment).map_err(|_| TestError::Validate)?;
    let atoms: Vec<&[u8]> = decoded.atoms().map(|atom| atom.bytes).collect();
    let rows = occurrences(&decoded)?;
    let base_note = field_ordinal_in_class(&decoded, &atoms, &module, b"Base", b"note")?;
    let other_note = field_ordinal_in_class(&decoded, &atoms, &module, b"Other", b"note")?;
    if base_note == other_note {
        return Err(TestError::Falsified(
            "Base.note and Other.note are one field",
        ));
    }
    pin_local_field_access(
        source,
        &module,
        &decoded,
        &atoms,
        &rows,
        b"read",
        base_note,
        Some(other_note),
        b"note",
    )?;
    Ok(())
}

#[test]
fn python_super_field_read_two_level_chain() -> Result<(), TestError> {
    let source = b"class Other:\n    note: str\n\nclass Grand:\n    note: str\n\nclass Mid(Grand):\n    pass\n\nclass Child(Mid):\n    def read(self):\n        return super().note\n";
    let (fragment, module) = compile_fixture(source)?;
    let decoded = FragmentView::validate(&fragment).map_err(|_| TestError::Validate)?;
    let atoms: Vec<&[u8]> = decoded.atoms().map(|atom| atom.bytes).collect();
    let rows = occurrences(&decoded)?;
    let grand_note = field_ordinal_in_class(&decoded, &atoms, &module, b"Grand", b"note")?;
    let other_note = field_ordinal_in_class(&decoded, &atoms, &module, b"Other", b"note")?;
    if grand_note == other_note {
        return Err(TestError::Falsified(
            "Grand.note and Other.note are one field",
        ));
    }
    pin_local_field_access(
        source,
        &module,
        &decoded,
        &atoms,
        &rows,
        b"read",
        grand_note,
        Some(other_note),
        b"note",
    )?;
    Ok(())
}

#[test]
fn python_super_method_call_skips_child_override() -> Result<(), TestError> {
    let source = b"class Other:\n    def set_note(self):\n        pass\n\nclass Base:\n    def set_note(self):\n        pass\n\nclass Child(Base):\n    def set_note(self):\n        pass\n    def read(self):\n        super().set_note()\n";
    let (fragment, module) = compile_fixture(source)?;
    let decoded = FragmentView::validate(&fragment).map_err(|_| TestError::Validate)?;
    let atoms: Vec<&[u8]> = decoded.atoms().map(|atom| atom.bytes).collect();
    let rows = occurrences(&decoded)?;
    let base_set_note = method_ordinal_in_class(&decoded, &atoms, &module, b"Base", b"set_note")?;
    let child_set_note = method_ordinal_in_class(&decoded, &atoms, &module, b"Child", b"set_note")?;
    let other_set_note = method_ordinal_in_class(&decoded, &atoms, &module, b"Other", b"set_note")?;
    if base_set_note == child_set_note || base_set_note == other_set_note {
        return Err(TestError::Falsified("set_note ordinals are not distinct"));
    }
    pin_local_method_call(
        source,
        &module,
        &decoded,
        &atoms,
        &rows,
        b"read",
        base_set_note,
        Some(child_set_note),
        b"set_note",
    )?;
    let read = entity_ordinal(&decoded, &atoms, b"read", EntityKind::Function)?;
    assert_no_local_target(&rows, read, ReferenceKind::MethodCall, other_set_note)?;
    Ok(())
}

#[test]
fn python_super_method_value_resolves_to_base() -> Result<(), TestError> {
    let source = b"class Other:\n    def note(self):\n        pass\n\nclass Base:\n    def note(self):\n        pass\n\nclass Child(Base):\n    def note(self):\n        pass\n    def read(self):\n        return super().note\n";
    let (fragment, module) = compile_fixture(source)?;
    let decoded = FragmentView::validate(&fragment).map_err(|_| TestError::Validate)?;
    let atoms: Vec<&[u8]> = decoded.atoms().map(|atom| atom.bytes).collect();
    let rows = occurrences(&decoded)?;
    let base_note = method_ordinal_in_class(&decoded, &atoms, &module, b"Base", b"note")?;
    let child_note = method_ordinal_in_class(&decoded, &atoms, &module, b"Child", b"note")?;
    if base_note == child_note {
        return Err(TestError::Falsified(
            "Base.note and Child.note are one method",
        ));
    }
    pin_local_field_access(
        source,
        &module,
        &decoded,
        &atoms,
        &rows,
        b"read",
        base_note,
        Some(child_note),
        b"note",
    )?;
    let read = entity_ordinal(&decoded, &atoms, b"read", EntityKind::Function)?;
    for row in rows
        .iter()
        .filter(|row| row.owner == read && row.occurrence.kind == ReferenceKind::MethodCall)
    {
        let spelling_note = matches!(
            &row.occurrence.target,
            OccurrenceTarget::Foreign(key) if key.display == "note"
        ) || matches!(
            row.occurrence.target,
            OccurrenceTarget::Local(target) if target == base_note || target == child_note
        );
        if spelling_note {
            return Err(TestError::Falsified(
                "super().note produced a MethodCall owned by read",
            ));
        }
    }
    Ok(())
}

#[test]
fn python_super_field_wins_over_child_method() -> Result<(), TestError> {
    let source = b"class Other:\n    note: str\n\nclass Base:\n    note: str\n\nclass Child(Base):\n    def note(self):\n        pass\n    def read(self):\n        return super().note\n";
    let (fragment, module) = compile_fixture(source)?;
    let decoded = FragmentView::validate(&fragment).map_err(|_| TestError::Validate)?;
    let atoms: Vec<&[u8]> = decoded.atoms().map(|atom| atom.bytes).collect();
    let rows = occurrences(&decoded)?;
    let base_field = field_ordinal_in_class(&decoded, &atoms, &module, b"Base", b"note")?;
    let child_method = method_ordinal_in_class(&decoded, &atoms, &module, b"Child", b"note")?;
    if base_field == child_method {
        return Err(TestError::Falsified(
            "Base field and Child method share one ordinal",
        ));
    }
    pin_local_field_access(
        source,
        &module,
        &decoded,
        &atoms,
        &rows,
        b"read",
        base_field,
        Some(child_method),
        b"note",
    )?;
    Ok(())
}

#[test]
fn python_super_child_field_does_not_hide_base_method() -> Result<(), TestError> {
    let source = b"class Other:\n    def note(self):\n        pass\n\nclass Base:\n    def note(self):\n        pass\n\nclass Child(Base):\n    note: str\n    def read(self):\n        return super().note\n";
    let (fragment, module) = compile_fixture(source)?;
    let decoded = FragmentView::validate(&fragment).map_err(|_| TestError::Validate)?;
    let atoms: Vec<&[u8]> = decoded.atoms().map(|atom| atom.bytes).collect();
    let rows = occurrences(&decoded)?;
    let base_method = method_ordinal_in_class(&decoded, &atoms, &module, b"Base", b"note")?;
    let child_field = field_ordinal_in_class(&decoded, &atoms, &module, b"Child", b"note")?;
    if base_method == child_field {
        return Err(TestError::Falsified(
            "Base method and Child field share one ordinal",
        ));
    }
    pin_local_field_access(
        source,
        &module,
        &decoded,
        &atoms,
        &rows,
        b"read",
        base_method,
        Some(child_field),
        b"note",
    )?;
    Ok(())
}

#[test]
// Two same-file bases both declare `note`. Zero-argument `super()` follows the
// MRO to the first base that declares it, so the read binds `Left.note` and
// never `Right.note` (9a1917cbe replaced the older "ambiguous bases stay
// universe" rule; py_super_two_bases_targets_left_note_not_right is the
// method-call twin).
fn python_super_two_bases_field_read_targets_left() -> Result<(), TestError> {
    let source = b"class Left:\n    note: str\n\nclass Right:\n    note: str\n\nclass Child(Left, Right):\n    def read(self):\n        return super().note\n";
    let (fragment, module) = compile_fixture(source)?;
    let decoded = FragmentView::validate(&fragment).map_err(|_| TestError::Validate)?;
    let atoms: Vec<&[u8]> = decoded.atoms().map(|atom| atom.bytes).collect();
    let rows = occurrences(&decoded)?;
    let left_note = field_ordinal_in_class(&decoded, &atoms, &module, b"Left", b"note")?;
    let right_note = field_ordinal_in_class(&decoded, &atoms, &module, b"Right", b"note")?;
    if left_note == right_note {
        return Err(TestError::Falsified(
            "Left.note and Right.note are one field",
        ));
    }
    let read = entity_ordinal(&decoded, &atoms, b"read", EntityKind::Function)?;
    local_field_access(&rows, read, left_note)?;
    assert_no_local_target(&rows, read, ReferenceKind::FieldAccess, right_note)?;
    Ok(())
}

#[test]
fn python_super_imported_only_base_stays_universe() -> Result<(), TestError> {
    let source = b"from workout.service import WorkoutService\n\nclass Child(WorkoutService):\n    def read(self):\n        return super().note\n";
    let (fragment, module) = compile_fixture(source)?;
    let decoded = FragmentView::validate(&fragment).map_err(|_| TestError::Validate)?;
    let atoms: Vec<&[u8]> = decoded.atoms().map(|atom| atom.bytes).collect();
    let rows = occurrences(&decoded)?;
    pin_universe_field_access(source, &module, &decoded, &atoms, &rows, b"read", "note")?;
    Ok(())
}

#[test]
fn python_super_explicit_form_stays_universe() -> Result<(), TestError> {
    let source = b"class Other:\n    note: str\n\nclass Base:\n    note: str\n\nclass Child(Base):\n    def read(self):\n        return super(Base, self).note\n";
    let (fragment, module) = compile_fixture(source)?;
    let decoded = FragmentView::validate(&fragment).map_err(|_| TestError::Validate)?;
    let atoms: Vec<&[u8]> = decoded.atoms().map(|atom| atom.bytes).collect();
    let rows = occurrences(&decoded)?;
    let base_note = field_ordinal_in_class(&decoded, &atoms, &module, b"Base", b"note")?;
    pin_universe_field_access(source, &module, &decoded, &atoms, &rows, b"read", "note")?;
    let read = entity_ordinal(&decoded, &atoms, b"read", EntityKind::Function)?;
    assert_no_local_target(&rows, read, ReferenceKind::FieldAccess, base_note)?;
    Ok(())
}

#[test]
fn python_obj_super_call_stays_universe() -> Result<(), TestError> {
    let source = b"class Other:\n    note: str\n\nclass Base:\n    note: str\n\nclass Child(Base):\n    def read(self, obj):\n        return obj.super().note\n";
    let (fragment, module) = compile_fixture(source)?;
    let decoded = FragmentView::validate(&fragment).map_err(|_| TestError::Validate)?;
    let atoms: Vec<&[u8]> = decoded.atoms().map(|atom| atom.bytes).collect();
    let rows = occurrences(&decoded)?;
    let base_note = field_ordinal_in_class(&decoded, &atoms, &module, b"Base", b"note")?;
    pin_universe_field_access(source, &module, &decoded, &atoms, &rows, b"read", "note")?;
    let read = entity_ordinal(&decoded, &atoms, b"read", EntityKind::Function)?;
    assert_no_local_target(&rows, read, ReferenceKind::FieldAccess, base_note)?;
    Ok(())
}

#[test]
fn python_super_generic_base_resolves_to_base() -> Result<(), TestError> {
    let source = b"class Other:\n    note: str\n\nclass Base:\n    note: str\n\nclass Child(Base[int]):\n    def read(self):\n        return super().note\n";
    let (fragment, module) = compile_fixture(source)?;
    let decoded = FragmentView::validate(&fragment).map_err(|_| TestError::Validate)?;
    let atoms: Vec<&[u8]> = decoded.atoms().map(|atom| atom.bytes).collect();
    let rows = occurrences(&decoded)?;
    let base_note = field_ordinal_in_class(&decoded, &atoms, &module, b"Base", b"note")?;
    let other_note = field_ordinal_in_class(&decoded, &atoms, &module, b"Other", b"note")?;
    if base_note == other_note {
        return Err(TestError::Falsified(
            "Base.note and Other.note are one field",
        ));
    }
    pin_local_field_access(
        source,
        &module,
        &decoded,
        &atoms,
        &rows,
        b"read",
        base_note,
        Some(other_note),
        b"note",
    )?;
    Ok(())
}

#[test]
fn python_self_field_read_still_binds_child_override() -> Result<(), TestError> {
    let source = b"class Base:\n    note: str\n\nclass Child(Base):\n    note: str\n    def read(self):\n        return self.note\n";
    let (fragment, module) = compile_fixture(source)?;
    let decoded = FragmentView::validate(&fragment).map_err(|_| TestError::Validate)?;
    let atoms: Vec<&[u8]> = decoded.atoms().map(|atom| atom.bytes).collect();
    let rows = occurrences(&decoded)?;
    let child_note = field_ordinal_in_class(&decoded, &atoms, &module, b"Child", b"note")?;
    let base_note = field_ordinal_in_class(&decoded, &atoms, &module, b"Base", b"note")?;
    if child_note == base_note {
        return Err(TestError::Falsified(
            "Child.note and Base.note are one field",
        ));
    }
    pin_local_field_access(
        source,
        &module,
        &decoded,
        &atoms,
        &rows,
        b"read",
        child_note,
        Some(base_note),
        b"note",
    )?;
    Ok(())
}

#[test]
fn python_class_field_read_binds_named_class() -> Result<(), TestError> {
    let source = b"class Other:\n    note: str\n\nclass Item:\n    note: str\n\ndef read():\n    return Item.note\n";
    let (fragment, module) = compile_fixture(source)?;
    let decoded = FragmentView::validate(&fragment).map_err(|_| TestError::Validate)?;
    let atoms: Vec<&[u8]> = decoded.atoms().map(|atom| atom.bytes).collect();
    let rows = occurrences(&decoded)?;
    let item_note = field_ordinal_in_class(&decoded, &atoms, &module, b"Item", b"note")?;
    let other_note = field_ordinal_in_class(&decoded, &atoms, &module, b"Other", b"note")?;
    if item_note == other_note {
        return Err(TestError::Falsified(
            "Item.note and Other.note are one field",
        ));
    }
    pin_local_field_access(
        source,
        &module,
        &decoded,
        &atoms,
        &rows,
        b"read",
        item_note,
        Some(other_note),
        b"note",
    )?;
    Ok(())
}

#[test]
fn python_class_method_call_binds_named_class() -> Result<(), TestError> {
    let source = b"class Other:\n    def set_note(self):\n        pass\n\nclass Item:\n    def set_note(self):\n        pass\n\ndef run():\n    Item.set_note()\n";
    let (fragment, module) = compile_fixture(source)?;
    let decoded = FragmentView::validate(&fragment).map_err(|_| TestError::Validate)?;
    let atoms: Vec<&[u8]> = decoded.atoms().map(|atom| atom.bytes).collect();
    let rows = occurrences(&decoded)?;
    let item_set_note = method_ordinal_in_class(&decoded, &atoms, &module, b"Item", b"set_note")?;
    let other_set_note = method_ordinal_in_class(&decoded, &atoms, &module, b"Other", b"set_note")?;
    if item_set_note == other_set_note {
        return Err(TestError::Falsified(
            "Item.set_note and Other.set_note are one method",
        ));
    }
    pin_local_method_call(
        source,
        &module,
        &decoded,
        &atoms,
        &rows,
        b"run",
        item_set_note,
        Some(other_set_note),
        b"set_note",
    )?;
    Ok(())
}

#[test]
fn python_class_inherited_field_read() -> Result<(), TestError> {
    let source = b"class Other:\n    note: str\n\nclass Base:\n    note: str\n\nclass Item(Base):\n    pass\n\ndef read():\n    return Item.note\n";
    let (fragment, module) = compile_fixture(source)?;
    let decoded = FragmentView::validate(&fragment).map_err(|_| TestError::Validate)?;
    let atoms: Vec<&[u8]> = decoded.atoms().map(|atom| atom.bytes).collect();
    let rows = occurrences(&decoded)?;
    let base_note = field_ordinal_in_class(&decoded, &atoms, &module, b"Base", b"note")?;
    let other_note = field_ordinal_in_class(&decoded, &atoms, &module, b"Other", b"note")?;
    if base_note == other_note {
        return Err(TestError::Falsified(
            "Base.note and Other.note are one field",
        ));
    }
    pin_local_field_access(
        source,
        &module,
        &decoded,
        &atoms,
        &rows,
        b"read",
        base_note,
        Some(other_note),
        b"note",
    )?;
    Ok(())
}

#[test]
fn python_class_inherited_method_call() -> Result<(), TestError> {
    let source = b"class Other:\n    def set_note(self):\n        pass\n\nclass Base:\n    def set_note(self):\n        pass\n\nclass Item(Base):\n    pass\n\ndef run():\n    Item.set_note()\n";
    let (fragment, module) = compile_fixture(source)?;
    let decoded = FragmentView::validate(&fragment).map_err(|_| TestError::Validate)?;
    let atoms: Vec<&[u8]> = decoded.atoms().map(|atom| atom.bytes).collect();
    let rows = occurrences(&decoded)?;
    let base_set_note = method_ordinal_in_class(&decoded, &atoms, &module, b"Base", b"set_note")?;
    let other_set_note = method_ordinal_in_class(&decoded, &atoms, &module, b"Other", b"set_note")?;
    if base_set_note == other_set_note {
        return Err(TestError::Falsified(
            "Base.set_note and Other.set_note are one method",
        ));
    }
    pin_local_method_call(
        source,
        &module,
        &decoded,
        &atoms,
        &rows,
        b"run",
        base_set_note,
        Some(other_set_note),
        b"set_note",
    )?;
    Ok(())
}

#[test]
fn python_class_field_read_shadows_base() -> Result<(), TestError> {
    let source = b"class Base:\n    note: str\n\nclass Item(Base):\n    note: str\n\ndef read():\n    return Item.note\n";
    let (fragment, module) = compile_fixture(source)?;
    let decoded = FragmentView::validate(&fragment).map_err(|_| TestError::Validate)?;
    let atoms: Vec<&[u8]> = decoded.atoms().map(|atom| atom.bytes).collect();
    let rows = occurrences(&decoded)?;
    let item_note = field_ordinal_in_class(&decoded, &atoms, &module, b"Item", b"note")?;
    let base_note = field_ordinal_in_class(&decoded, &atoms, &module, b"Base", b"note")?;
    if item_note == base_note {
        return Err(TestError::Falsified(
            "Item.note and Base.note are one field",
        ));
    }
    pin_local_field_access(
        source,
        &module,
        &decoded,
        &atoms,
        &rows,
        b"read",
        item_note,
        Some(base_note),
        b"note",
    )?;
    Ok(())
}

#[test]
fn python_class_method_value_binds_named_class() -> Result<(), TestError> {
    let source = b"class Other:\n    def note(self):\n        pass\n\nclass Item:\n    def note(self):\n        pass\n\ndef read():\n    return Item.note\n";
    let (fragment, module) = compile_fixture(source)?;
    let decoded = FragmentView::validate(&fragment).map_err(|_| TestError::Validate)?;
    let atoms: Vec<&[u8]> = decoded.atoms().map(|atom| atom.bytes).collect();
    let rows = occurrences(&decoded)?;
    let item_note = method_ordinal_in_class(&decoded, &atoms, &module, b"Item", b"note")?;
    let other_note = method_ordinal_in_class(&decoded, &atoms, &module, b"Other", b"note")?;
    if item_note == other_note {
        return Err(TestError::Falsified(
            "Item.note and Other.note are one method",
        ));
    }
    pin_local_field_access(
        source,
        &module,
        &decoded,
        &atoms,
        &rows,
        b"read",
        item_note,
        Some(other_note),
        b"note",
    )?;
    let read = entity_ordinal(&decoded, &atoms, b"read", EntityKind::Function)?;
    for row in rows
        .iter()
        .filter(|row| row.owner == read && row.occurrence.kind == ReferenceKind::MethodCall)
    {
        let spelling_note = matches!(
            &row.occurrence.target,
            OccurrenceTarget::Foreign(key) if key.display == "note"
        ) || matches!(
            row.occurrence.target,
            OccurrenceTarget::Local(target) if target == item_note || target == other_note
        );
        if spelling_note {
            return Err(TestError::Falsified(
                "Item.note produced a MethodCall owned by read",
            ));
        }
    }
    Ok(())
}

#[test]
fn python_class_ambiguous_inherited_fields_stays_universe() -> Result<(), TestError> {
    let source = b"class Left:\n    note: str\n\nclass Right:\n    note: str\n\nclass Item(Left, Right):\n    pass\n\ndef read():\n    return Item.note\n";
    let (fragment, module) = compile_fixture(source)?;
    let decoded = FragmentView::validate(&fragment).map_err(|_| TestError::Validate)?;
    let atoms: Vec<&[u8]> = decoded.atoms().map(|atom| atom.bytes).collect();
    let rows = occurrences(&decoded)?;
    let left_note = field_ordinal_in_class(&decoded, &atoms, &module, b"Left", b"note")?;
    let right_note = field_ordinal_in_class(&decoded, &atoms, &module, b"Right", b"note")?;
    if left_note == right_note {
        return Err(TestError::Falsified(
            "Left.note and Right.note are one field",
        ));
    }
    pin_universe_field_access(source, &module, &decoded, &atoms, &rows, b"read", "note")?;
    Ok(())
}

#[test]
fn python_class_generic_base_field_read() -> Result<(), TestError> {
    let source = b"class Other:\n    note: str\n\nclass Base:\n    note: str\n\nclass Item(Base[int]):\n    pass\n\ndef read():\n    return Item.note\n";
    let (fragment, module) = compile_fixture(source)?;
    let decoded = FragmentView::validate(&fragment).map_err(|_| TestError::Validate)?;
    let atoms: Vec<&[u8]> = decoded.atoms().map(|atom| atom.bytes).collect();
    let rows = occurrences(&decoded)?;
    let base_note = field_ordinal_in_class(&decoded, &atoms, &module, b"Base", b"note")?;
    let other_note = field_ordinal_in_class(&decoded, &atoms, &module, b"Other", b"note")?;
    if base_note == other_note {
        return Err(TestError::Falsified(
            "Base.note and Other.note are one field",
        ));
    }
    pin_local_field_access(
        source,
        &module,
        &decoded,
        &atoms,
        &rows,
        b"read",
        base_note,
        Some(other_note),
        b"note",
    )?;
    Ok(())
}
