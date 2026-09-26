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
use backend_frontend_python::legacy::{
    DeclarationKind, OccurrenceKind, Span, extract,
};
use backend_semantic::ir::{
    EntityId, EntityKind, ForeignOrigin, FragmentView, OccurrenceTarget, ReferenceKind,
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
        return Err(TestError::Falsified("field declaration in class not unique"));
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
    let read_unresolved = entity_ordinal(&decoded, &atoms, b"read_unresolved", EntityKind::Function)?;

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
    for row in rows
        .iter()
        .filter(|row| row.owner == call_foreign && row.occurrence.kind == ReferenceKind::FieldAccess)
    {
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
