//! Focused lowering test for Python attribute-read occurrences.
//!
//! Non-call attribute reads become `FieldAccess` rows; callee attributes on
//! calls stay `MethodCall` only. Local `self`/`cls` reads resolve to the
//! enclosing class field; every other receiver stays an honest foreign field.

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
    DeclarationKind, OccurrenceKind, OccurrenceReceiver, Span, extract,
};
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
class Point:
    score: int
    def read(self, other):
        seen = self.score
        return other.score

class Other:
    score: int
    def read(self):
        return self.score

class Client:
    def send(self):
        return self.send()

def loose(obj):
    return obj.missing

class Chain:
    score: int
    def widen(self):
        return self.score.bit_length()
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

fn field_entities_named(
    decoded: &FragmentView<'_>,
    atoms: &[&[u8]],
    name: &[u8],
) -> Result<Vec<EntityId>, TestError> {
    let fields: Vec<EntityId> = decoded
        .entities()
        .filter(|entity| {
            entity.kind == EntityKind::Field
                && atoms.get(entity.name.raw as usize).copied() == Some(name)
        })
        .map(|entity| entity.entity)
        .collect();
    if fields.is_empty() {
        return Err(TestError::Falsified("named field entities absent"));
    }
    Ok(fields)
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

fn function_span_in_class(
    module: &backend_frontend_python::legacy::ModuleFacts,
    class_name: &[u8],
    function_name: &[u8],
) -> Result<Span, TestError> {
    let class_span = class_span(module, class_name)?;
    let mut matches: Vec<Span> = Vec::new();
    for declaration in &module.declarations {
        if declaration.kind != DeclarationKind::Function {
            continue;
        }
        if declaration.name.as_bytes() != function_name {
            continue;
        }
        if declaration.span.start >= class_span.start && declaration.span.end <= class_span.end {
            matches.push(declaration.span);
        }
    }
    if matches.len() != 1 {
        return Err(TestError::Falsified("function span in class not unique"));
    }
    Ok(matches[0])
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
    let named_fields = field_entities_named(decoded, atoms, field_name)?;
    named_fields
        .get(prior_fields)
        .copied()
        .ok_or(TestError::Falsified("field entity ordinal absent"))
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
        "nudox-python-attribute-reads-{nonce}-{}-{}",
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

#[test]
fn python_attribute_reads_lower_honestly() -> Result<(), TestError> {
    let (fragment, module) = compile_fixture(SOURCE)?;
    let decoded = FragmentView::validate(&fragment).map_err(|_| TestError::Validate)?;
    let atoms: Vec<&[u8]> = decoded.atoms().map(|atom| atom.bytes).collect();
    let rows = occurrences(&decoded)?;

    let point_score = field_ordinal_in_class(&decoded, &atoms, &module, b"Point", b"score")?;
    let other_score = field_ordinal_in_class(&decoded, &atoms, &module, b"Other", b"score")?;
    let chain_score = field_ordinal_in_class(&decoded, &atoms, &module, b"Chain", b"score")?;
    let other_read = rows
        .iter()
        .filter(|row| {
            row.occurrence.kind == ReferenceKind::FieldAccess
                && matches!(
                    row.occurrence.target,
                    OccurrenceTarget::Local(target) if target == other_score
                )
        })
        .map(|row| row.owner)
        .next()
        .ok_or(TestError::Falsified("other read owner absent"))?;
    if owner_name(&decoded, &atoms, other_read)? != b"read" {
        return Err(TestError::Falsified("other score owner is not read"));
    }
    let chain_widen = entity_ordinal(&decoded, &atoms, b"widen", EntityKind::Function)?;
    let send_method = entity_ordinal(&decoded, &atoms, b"send", EntityKind::Function)?;

    let point_local_reads: Vec<_> = rows
        .iter()
        .filter(|row| {
            row.occurrence.kind == ReferenceKind::FieldAccess
                && matches!(
                    row.occurrence.target,
                    OccurrenceTarget::Local(target) if target == point_score
                )
        })
        .collect();
    if point_local_reads.len() != 1 {
        return Err(TestError::Falsified(
            "Point.score does not have exactly one local FieldAccess",
        ));
    }
    let point_row = point_local_reads[0];
    if owner_name(&decoded, &atoms, point_row.owner)? != b"read" {
        return Err(TestError::Falsified(
            "Point.score FieldAccess owner is not named read",
        ));
    }
    if point_row.occurrence.confidence != OccurrenceConfidence::Index {
        return Err(TestError::Falsified(
            "Point.score FieldAccess confidence is not Index",
        ));
    }
    let point_class = class_span(&module, b"Point")?;
    let point_read_span = function_span_in_class(&module, b"Point", b"read")?;
    if point_read_span.start < point_class.start {
        return Err(TestError::Falsified("Point.read starts outside Point"));
    }
    if point_read_span.end > point_class.end {
        return Err(TestError::Falsified("Point.read ends outside Point"));
    }
    let other_local_reads: Vec<_> = rows
        .iter()
        .filter(|row| {
            row.occurrence.kind == ReferenceKind::FieldAccess
                && matches!(
                    row.occurrence.target,
                    OccurrenceTarget::Local(target) if target == other_score
                )
        })
        .collect();
    if other_local_reads.len() != 1 {
        return Err(TestError::Falsified(
            "Other.score does not have exactly one local FieldAccess",
        ));
    }
    if other_local_reads[0].owner != other_read {
        return Err(TestError::Falsified(
            "Other.score FieldAccess owner is not Other.read",
        ));
    }
    if other_local_reads[0].occurrence.confidence != OccurrenceConfidence::Index {
        return Err(TestError::Falsified(
            "Other.score FieldAccess confidence is not Index",
        ));
    }

    let chain_local_reads: Vec<_> = rows
        .iter()
        .filter(|row| {
            row.occurrence.kind == ReferenceKind::FieldAccess
                && matches!(
                    row.occurrence.target,
                    OccurrenceTarget::Local(target) if target == chain_score
                )
        })
        .collect();
    if chain_local_reads.len() != 1 {
        return Err(TestError::Falsified(
            "Chain.score does not have exactly one local FieldAccess",
        ));
    }
    if chain_local_reads[0].owner != chain_widen {
        return Err(TestError::Falsified(
            "Chain.score FieldAccess owner is not Chain.widen",
        ));
    }
    if chain_local_reads[0].occurrence.confidence != OccurrenceConfidence::Index {
        return Err(TestError::Falsified(
            "Chain.score FieldAccess confidence is not Index",
        ));
    }

    let foreign_other_score: Vec<_> = rows
        .iter()
        .filter(|row| {
            row.occurrence.kind == ReferenceKind::FieldAccess
                && matches!(
                    &row.occurrence.target,
                    OccurrenceTarget::Foreign(key)
                        if key.path == "score"
                            && key.display == "score"
                            && key.kind == Some(EntityKind::Field)
                            && matches!(key.origin, ForeignOrigin::Universe { ecosystem: "pypi" })
                )
        })
        .collect();
    if foreign_other_score.len() != 1 {
        return Err(TestError::Falsified(
            "other.score is not exactly one foreign FieldAccess",
        ));
    }
    if matches!(
        foreign_other_score[0].occurrence.target,
        OccurrenceTarget::Local(_)
    ) {
        return Err(TestError::Falsified("other.score is Local"));
    }

    let foreign_missing: Vec<_> = rows
        .iter()
        .filter(|row| {
            row.occurrence.kind == ReferenceKind::FieldAccess
                && matches!(
                    &row.occurrence.target,
                    OccurrenceTarget::Foreign(key)
                        if key.path == "missing"
                            && key.kind == Some(EntityKind::Field)
                            && matches!(key.origin, ForeignOrigin::Universe { ecosystem: "pypi" })
                )
        })
        .collect();
    if foreign_missing.len() != 1 {
        return Err(TestError::Falsified(
            "obj.missing is not exactly one foreign FieldAccess",
        ));
    }
    if matches!(foreign_missing[0].occurrence.target, OccurrenceTarget::Local(_)) {
        return Err(TestError::Falsified("obj.missing is Local"));
    }
    if matches!(
        foreign_missing[0].occurrence.target,
        OccurrenceTarget::Foreign(key) if key.kind == Some(EntityKind::Function)
    ) {
        return Err(TestError::Falsified("obj.missing is EntityKind::Function"));
    }

    let send_calls: Vec<_> = rows
        .iter()
        .filter(|row| {
            row.occurrence.kind == ReferenceKind::MethodCall
                && matches!(
                    row.occurrence.target,
                    OccurrenceTarget::Local(target) if target == send_method
                )
        })
        .collect();
    if send_calls.len() != 1 {
        return Err(TestError::Falsified(
            "self.send() is not exactly one local MethodCall",
        ));
    }
    for row in rows.iter().filter(|row| row.occurrence.kind == ReferenceKind::FieldAccess) {
        if row.occurrence.target == OccurrenceTarget::Local(send_method) {
            return Err(TestError::Falsified(
                "self.send() produced a local FieldAccess of send",
            ));
        }
        if matches!(
            &row.occurrence.target,
            OccurrenceTarget::Foreign(key) if key.path == "send"
        ) {
            return Err(TestError::Falsified(
                "self.send() produced a foreign FieldAccess keyed as send",
            ));
        }
        if matches!(
            &row.occurrence.target,
            OccurrenceTarget::Foreign(key) if key.display == "send"
        ) {
            return Err(TestError::Falsified(
                "self.send() produced a foreign FieldAccess displayed as send",
            ));
        }
    }

    let bit_length_calls: Vec<_> = rows
        .iter()
        .filter(|row| {
            row.occurrence.kind == ReferenceKind::MethodCall
                && matches!(
                    &row.occurrence.target,
                    OccurrenceTarget::Foreign(key)
                        if key.path == "bit_length"
                            && key.kind == Some(EntityKind::Function)
                            && matches!(key.origin, ForeignOrigin::Universe { ecosystem: "pypi" })
                )
        })
        .collect();
    if bit_length_calls.len() != 1 {
        return Err(TestError::Falsified(
            "self.score.bit_length() is not exactly one foreign MethodCall",
        ));
    }

    let point_attr = module
        .occurrences
        .iter()
        .find(|occurrence| {
            occurrence.kind == OccurrenceKind::AttributeRead
                && occurrence.target == "score"
                && occurrence.receiver == OccurrenceReceiver::EnclosingClass {
                    class: "Point".to_owned(),
                }
        })
        .ok_or(TestError::Falsified(
            "Point.score AttributeRead occurrence absent in extractor",
        ))?;
    let point_attr_bytes = SOURCE
        .get(point_attr.span.start as usize..point_attr.span.end as usize)
        .ok_or(TestError::Falsified("Point.score AttributeRead span out of range"))?;
    if point_attr_bytes != b"score" {
        return Err(TestError::Falsified(
            "Point.score AttributeRead span does not slice score",
        ));
    }

    let lowered_start = point_read_span.start + point_row.occurrence.span.start;
    let lowered_end = point_read_span.start + point_row.occurrence.span.end;
    if lowered_start != point_attr.span.start {
        return Err(TestError::Falsified(
            "Point.score lowered span start does not match extractor span",
        ));
    }
    if lowered_end != point_attr.span.end {
        return Err(TestError::Falsified(
            "Point.score lowered span end does not match extractor span",
        ));
    }
    let lowered_bytes = SOURCE
        .get(lowered_start as usize..lowered_end as usize)
        .ok_or(TestError::Falsified("Point.score lowered span out of range"))?;
    if lowered_bytes != b"score" {
        return Err(TestError::Falsified(
            "Point.score lowered span does not slice score",
        ));
    }

    let chain_attr = module
        .occurrences
        .iter()
        .find(|occurrence| {
            occurrence.kind == OccurrenceKind::AttributeRead
                && occurrence.target == "score"
                && occurrence.receiver == OccurrenceReceiver::EnclosingClass {
                    class: "Chain".to_owned(),
                }
        })
        .ok_or(TestError::Falsified(
            "Chain.score AttributeRead occurrence absent in extractor",
        ))?;
    let chain_attr_bytes = SOURCE
        .get(chain_attr.span.start as usize..chain_attr.span.end as usize)
        .ok_or(TestError::Falsified("Chain.score AttributeRead span out of range"))?;
    if chain_attr_bytes != b"score" {
        return Err(TestError::Falsified(
            "Chain.score AttributeRead span does not slice score",
        ));
    }
    let chain_read_span = function_span_in_class(&module, b"Chain", b"widen")?;
    let chain_lowered_start = chain_read_span.start + chain_local_reads[0].occurrence.span.start;
    let chain_lowered_end = chain_read_span.start + chain_local_reads[0].occurrence.span.end;
    if chain_lowered_start != chain_attr.span.start {
        return Err(TestError::Falsified(
            "Chain.score lowered span start does not match extractor span",
        ));
    }
    if chain_lowered_end != chain_attr.span.end {
        return Err(TestError::Falsified(
            "Chain.score lowered span end does not match extractor span",
        ));
    }
    let chain_lowered_bytes = SOURCE
        .get(chain_lowered_start as usize..chain_lowered_end as usize)
        .ok_or(TestError::Falsified("Chain.score lowered span out of range"))?;
    if chain_lowered_bytes != b"score" {
        return Err(TestError::Falsified(
            "Chain.score lowered span does not slice score",
        ));
    }

    Ok(())
}

const AMBIGUOUS_SOURCE: &[u8] = b"\
class Ambiguous:
    score: int
    class Inner:
        score: str
    def read(self):
        return self.score
";

#[test]
fn ambiguous_class_score_read_stays_foreign() -> Result<(), TestError> {
    let (fragment, _module) = compile_fixture(AMBIGUOUS_SOURCE)?;
    let decoded = FragmentView::validate(&fragment).map_err(|_| TestError::Validate)?;
    let atoms: Vec<&[u8]> = decoded.atoms().map(|atom| atom.bytes).collect();
    let rows = occurrences(&decoded)?;
    let read_owner = entity_ordinal(&decoded, &atoms, b"read", EntityKind::Function)?;
    let ambiguous_reads: Vec<_> = rows
        .iter()
        .filter(|row| {
            row.owner == read_owner && row.occurrence.kind == ReferenceKind::FieldAccess
        })
        .collect();
    if ambiguous_reads.len() != 1 {
        return Err(TestError::Falsified(
            "Ambiguous.read does not emit exactly one FieldAccess",
        ));
    }
    let row = ambiguous_reads[0];
    if matches!(row.occurrence.target, OccurrenceTarget::Local(_)) {
        return Err(TestError::Falsified("Ambiguous self.score is Local"));
    }
    if !matches!(
        &row.occurrence.target,
        OccurrenceTarget::Foreign(key)
            if key.path == "score"
                && key.kind == Some(EntityKind::Field)
                && matches!(key.origin, ForeignOrigin::Universe { ecosystem: "pypi" })
    ) {
        return Err(TestError::Falsified(
            "Ambiguous self.score is not a foreign Field key score",
        ));
    }
    Ok(())
}

const GATED_SOURCE: &[u8] = b"\
def score():
    return 0

class Gated:
    score: int
    def read(self):
        return self.score.score()
";

#[test]
fn gated_score_score_emits_inner_field_and_method_call() -> Result<(), TestError> {
    let (fragment, module) = compile_fixture(GATED_SOURCE)?;
    let decoded = FragmentView::validate(&fragment).map_err(|_| TestError::Validate)?;
    let atoms: Vec<&[u8]> = decoded.atoms().map(|atom| atom.bytes).collect();
    let rows = occurrences(&decoded)?;
    let gated_score = field_ordinal_in_class(&decoded, &atoms, &module, b"Gated", b"score")?;
    let module_score = entity_ordinal(&decoded, &atoms, b"score", EntityKind::Function)?;
    let gated_read_span = function_span_in_class(&module, b"Gated", b"read")?;
    let read_owner = entity_ordinal(&decoded, &atoms, b"read", EntityKind::Function)?;

    let attribute_reads: Vec<_> = module
        .occurrences
        .iter()
        .filter(|occurrence| {
            occurrence.kind == OccurrenceKind::AttributeRead
                && occurrence.receiver == OccurrenceReceiver::EnclosingClass {
                    class: "Gated".to_owned(),
                }
        })
        .collect();
    if attribute_reads.len() != 1 {
        return Err(TestError::Falsified(
            "Gated.read does not record exactly one AttributeRead",
        ));
    }
    let inner_attr = attribute_reads[0];
    let inner_bytes = GATED_SOURCE
        .get(inner_attr.span.start as usize..inner_attr.span.end as usize)
        .ok_or(TestError::Falsified("inner score span out of range"))?;
    if inner_bytes != b"score" {
        return Err(TestError::Falsified("inner score span is not score bytes"));
    }

    let method_calls: Vec<_> = module
        .occurrences
        .iter()
        .filter(|occurrence| occurrence.kind == OccurrenceKind::MethodCall)
        .filter(|occurrence| occurrence.target == "score")
        .collect();
    if method_calls.len() != 1 {
        return Err(TestError::Falsified(
            "Gated.read does not record exactly one MethodCall of score",
        ));
    }
    let callee_call = method_calls[0];
    let callee_bytes = GATED_SOURCE
        .get(callee_call.span.start as usize..callee_call.span.end as usize)
        .ok_or(TestError::Falsified("callee span out of range"))?;
    if callee_bytes == b"score" {
        return Err(TestError::Falsified(
            "gated MethodCall callee span is only the score token",
        ));
    }
    let callee_token_start = callee_call
        .span
        .end
        .checked_sub(inner_attr.span.end - inner_attr.span.start)
        .ok_or(TestError::Falsified("callee token start overflow"))?;
    let callee_token = Span {
        start: callee_token_start,
        end: callee_call.span.end,
    };
    if callee_token.start == inner_attr.span.start {
        return Err(TestError::Falsified(
            "gated callee token is the inner score token",
        ));
    }

    let inner_reads: Vec<_> = rows
        .iter()
        .filter(|row| {
            row.occurrence.kind == ReferenceKind::FieldAccess
                && row.owner == read_owner
                && matches!(
                    row.occurrence.target,
                    OccurrenceTarget::Local(target) if target == gated_score
                )
        })
        .collect();
    if inner_reads.len() != 1 {
        return Err(TestError::Falsified(
            "inner score does not have exactly one local FieldAccess",
        ));
    }
    let inner_lowered_start = gated_read_span.start + inner_reads[0].occurrence.span.start;
    let inner_lowered_end = gated_read_span.start + inner_reads[0].occurrence.span.end;
    if inner_lowered_start != inner_attr.span.start {
        return Err(TestError::Falsified(
            "inner score lowered span start does not match extractor",
        ));
    }
    if inner_lowered_end != inner_attr.span.end {
        return Err(TestError::Falsified(
            "inner score lowered span end does not match extractor",
        ));
    }

    let score_calls: Vec<_> = rows
        .iter()
        .filter(|row| {
            row.occurrence.kind == ReferenceKind::MethodCall
                && row.owner == read_owner
                && matches!(
                    row.occurrence.target,
                    OccurrenceTarget::Local(target) if target == module_score
                )
        })
        .collect();
    if score_calls.len() != 1 {
        return Err(TestError::Falsified(
            "self.score.score() does not emit exactly one MethodCall of score",
        ));
    }

    for row in rows.iter().filter(|row| row.occurrence.kind == ReferenceKind::FieldAccess) {
        if row.owner != read_owner {
            continue;
        }
        let abs_start = gated_read_span.start + row.occurrence.span.start;
        let abs_end = gated_read_span.start + row.occurrence.span.end;
        if abs_start == callee_token.start {
            if abs_end == callee_token.end {
                return Err(TestError::Falsified(
                    "gated call produced a FieldAccess on the callee token",
                ));
            }
        }
    }

    Ok(())
}
