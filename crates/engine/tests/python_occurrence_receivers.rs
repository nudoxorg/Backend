//! Focused widening test for Python attribute-call occurrences.
//!
//! A `self.method()` site resolves to the local method of the enclosing
//! class; an `obj.unknown()` site stays an honest typed foreign method key;
//! a module-gated call (`json.loads` where `loads` is an imported binding)
//! keeps its original import-foreign keying; and a plain imported-module
//! receiver (`json.dumps`) resolves through that module's package key. A
//! gated call whose attribute spelling is only a *nested* import binding —
//! in `names`, so gated, but with no pushed module row — keys by the
//! attribute token alone, never by the receiver-qualified callee spelling.

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
use backend_frontend_python::legacy::{DeclarationKind, Span, extract};
use backend_semantic::ir::{
    EntityId, EntityKind, ForeignOrigin, FragmentView, OccurrenceConfidence, OccurrenceTarget,
    ReferenceKind,
};
use backend_semantic::vocabulary::{LanguageProfile, PythonVersion, Stage};
use thiserror::Error;

/// Distinguishes fixture directories created by parallel test threads within
/// one process, where the clock and pid alone can repeat.
static FIXTURE_SEQUENCE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

fn fixture_sequence() -> u64 {
    FIXTURE_SEQUENCE.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
}

const SOURCE: &[u8] = b"\
import json
from base64 import b64decode

class Client:
    def execute(self, command):
        return self.send(command)
    def send(self, command):
        return command

def outer():
    from textwrap import dedent

def gated(data):
    return json.loads(data) + len(b64decode(data))

def nested_gate(data):
    return json.dedent(data)

def loose(obj):
    return obj.unknown()
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

#[test]
fn python_receiver_occurrences_resolve_honestly() -> Result<(), TestError> {
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
        "nudox-python-receiver-{nonce}-{}-{}",
        std::process::id(),
        fixture_sequence()
    ));
    fs::create_dir_all(&work).map_err(|source| TestError::Io("create scratch", source))?;
    let cancelled = AtomicBool::new(false);
    // Two-run byte stability: the widened occurrence rows must not make the
    // fragment digest input-order or memory dependent.
    let mut runs: Vec<Vec<u8>> = Vec::new();
    for _ in 0..2 {
        let mut output = vec![0_u8; 8 * 1024 * 1024];
        let mut diagnostic = [0_u8; 4096];
        let result = compile(
            CompileRequest {
                profile: LanguageProfile::Python(PythonVersion::Python314),
                stage: Stage::LowerIr,
                source: SOURCE,
                declaration_scope: backend_engine::driver::DeclarationScope::fixture(),
                toolchain: ToolchainSelection::ResolvedNative(toolchain.clone()),
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
        let trimmed = {
            let mut fragment = result.fragment.as_ref().to_vec();
            fragment.shrink_to_fit();
            fragment
        };
        runs.push(trimmed);
    }
    assert_eq!(runs[0], runs[1], "python fragments must be byte-stable");
    let decoded = FragmentView::validate(&runs[0]).map_err(|_| TestError::Validate)?;

    let atoms: Vec<&[u8]> = decoded.atoms().map(|atom| atom.bytes).collect();
    let mut occurrences: Vec<backend_semantic::ir::DecodedOccurrence<'_>> = Vec::new();
    if let Some(mut cursor) = decoded.occurrences() {
        for row in cursor.by_ref() {
            occurrences.push(row.map_err(|_| TestError::Falsified("occurrence decode"))?);
        }
    }
    let send_ordinal = decoded
        .entities()
        .find(|entity| {
            atoms.get(entity.name.raw as usize).copied() == Some(b"send".as_slice())
                && entity.kind == EntityKind::Function
        })
        .map(|entity| entity.entity.raw)
        .ok_or(TestError::Falsified("send method entity absent"))?;

    // 1. `self.send(command)` resolves to the local method of the
    //    enclosing class, as a MethodCall.
    if !occurrences.iter().any(|row| {
        row.occurrence.kind == ReferenceKind::MethodCall
            && matches!(
                row.occurrence.target,
                OccurrenceTarget::Local(target) if target.raw == send_ordinal
            )
    }) {
        return Err(TestError::Falsified(
            "self.send does not resolve to the local method",
        ));
    }

    // 2. `obj.unknown()` stays an honest typed foreign method key.
    if !occurrences.iter().any(|row| {
        matches!(
            &row.occurrence.target,
            OccurrenceTarget::Foreign(key)
                if key.path == "unknown"
                    && key.kind == Some(EntityKind::Function)
                    && matches!(key.origin, ForeignOrigin::Universe { ecosystem: "pypi" })
        )
    }) {
        return Err(TestError::Falsified(
            "obj.unknown is not an honest typed foreign method key",
        ));
    }

    // 3. The module-gated `json.loads` call keeps its original import
    //    keying: a foreign pypi package key naming the imported module,
    //    with the attribute as the display binding.
    if !occurrences.iter().any(|row| {
        matches!(
            &row.occurrence.target,
            OccurrenceTarget::Foreign(key)
                if key.path == "json"
                    && key.display == "loads"
                    && matches!(key.origin, ForeignOrigin::Package(lineage) if lineage.name == "json")
        )
    }) {
        return Err(TestError::Falsified(
            "module-gated json.loads lost its import-foreign key",
        ));
    }

    // 4. `b64decode(data)` (bare name over an import binding) keeps the
    //    original import-foreign keying, byte for byte.
    if !occurrences.iter().any(|row| {
        matches!(
            &row.occurrence.target,
            OccurrenceTarget::Foreign(key)
                if key.path == "b64decode"
                    && key.display == "b64decode"
        )
    }) {
        return Err(TestError::Falsified(
            "b64decode lost its import-foreign key",
        ));
    }

    // 5. A gated call whose attribute spelling is a declared module name
    //    only through a *nested* import binding (so the gate fires but no
    //    pushed row carries the name) keys by the attribute token alone:
    //    `json.dedent` is a universe foreign key named `dedent`, never the
    //    receiver-qualified `json.dedent` — exactly the key the matched
    //    import path displays for the same target.
    if !occurrences.iter().any(|row| {
        matches!(
            &row.occurrence.target,
            OccurrenceTarget::Foreign(key)
                if key.path == "dedent"
                    && key.display == "dedent"
                    && matches!(key.origin, ForeignOrigin::Universe { ecosystem: "pypi" })
        )
    }) {
        return Err(TestError::Falsified(
            "gated-but-unpushed json.dedent does not key the attribute token",
        ));
    }

    fs::remove_dir_all(&work).map_err(|source| TestError::Io("remove scratch", source))?;
    Ok(())
}

fn compile_python_fragment(
    source: &[u8],
    work: &std::path::Path,
    toolchain: &ResolvedToolchain,
    cancelled: &AtomicBool,
) -> Result<Vec<u8>, TestError> {
    let mut output = vec![0_u8; 8 * 1024 * 1024];
    let mut diagnostic = [0_u8; 4096];
    let result = compile(
        CompileRequest {
            profile: LanguageProfile::Python(PythonVersion::Python314),
            stage: Stage::LowerIr,
            source,
            declaration_scope: backend_engine::driver::DeclarationScope::fixture(),
            toolchain: ToolchainSelection::ResolvedNative(toolchain.clone()),
            authority: SemanticAuthorityInput::None,
            control: CompileControl {
                deadline: Instant::now() + Duration::from_secs(30),
                cancelled,
            },
        },
        CompileScratch {
            diagnostic_output: &mut diagnostic,
            native_work: work,
        },
        CompileOutput {
            fragment_output: &mut output,
        },
    )
    .map_err(|failure| TestError::Compile(failure_label(&failure)))?;
    Ok(result.fragment.as_ref().to_vec())
}

fn entity_ordinal_by_name(
    decoded: &FragmentView<'_>,
    atoms: &[&[u8]],
    name: &[u8],
    kind: EntityKind,
) -> Result<u32, TestError> {
    decoded
        .entities()
        .find(|entity| {
            atoms.get(entity.name.raw as usize).copied() == Some(name) && entity.kind == kind
        })
        .map(|entity| entity.entity.raw)
        .ok_or(TestError::Falsified("entity absent"))
}

fn set_note_in_owner<'a>(
    occurrences: &'a [backend_semantic::ir::DecodedOccurrence<'a>],
    owner: u32,
) -> Option<&'a backend_semantic::ir::DecodedOccurrence<'a>> {
    occurrences.iter().find(|row| {
        row.owner.raw == owner
            && row.occurrence.kind == ReferenceKind::MethodCall
    })
}

fn field_access_in_owner<'a>(
    occurrences: &'a [backend_semantic::ir::DecodedOccurrence<'a>],
    owner: u32,
) -> Result<&'a backend_semantic::ir::DecodedOccurrence<'a>, TestError> {
    let matches: Vec<_> = occurrences
        .iter()
        .filter(|row| row.owner.raw == owner && row.occurrence.kind == ReferenceKind::FieldAccess)
        .collect();
    if matches.len() != 1 {
        return Err(TestError::Falsified("expected exactly one FieldAccess in owner"));
    }
    Ok(matches[0])
}

fn decode_occurrences(
    fragment: &[u8],
) -> Result<(FragmentView<'_>, Vec<&[u8]>, Vec<backend_semantic::ir::DecodedOccurrence<'_>>), TestError>
{
    let decoded = FragmentView::validate(fragment).map_err(|_| TestError::Validate)?;
    let atoms: Vec<&[u8]> = decoded.atoms().map(|atom| atom.bytes).collect();
    let mut occurrences: Vec<backend_semantic::ir::DecodedOccurrence<'_>> = Vec::new();
    if let Some(mut cursor) = decoded.occurrences() {
        for row in cursor.by_ref() {
            occurrences.push(row.map_err(|_| TestError::Falsified("occurrence decode"))?);
        }
    }
    Ok((decoded, atoms, occurrences))
}

fn assert_foreign_package_field(
    target: &OccurrenceTarget<'_>,
    path: &str,
    display: &str,
    lineage: &str,
) -> Result<(), TestError> {
    match target {
        OccurrenceTarget::Foreign(key) => {
            if key.path != path {
                return Err(TestError::Falsified("foreign package path mismatch"));
            }
            if key.display != display {
                return Err(TestError::Falsified("foreign package display mismatch"));
            }
            if key.kind != Some(EntityKind::Field) {
                return Err(TestError::Falsified("foreign package kind is not Field"));
            }
            if !matches!(
                key.origin,
                ForeignOrigin::Package(package)
                    if package.ecosystem == "pypi" && package.name == lineage
            ) {
                return Err(TestError::Falsified("foreign package lineage mismatch"));
            }
            Ok(())
        }
        OccurrenceTarget::Local(_) => Err(TestError::Falsified("expected foreign package target")),
        OccurrenceTarget::Stable(_) => Err(TestError::Falsified("expected foreign package target")),
    }
}

fn assert_universe_field(
    target: &OccurrenceTarget<'_>,
    path: &str,
    display: &str,
) -> Result<(), TestError> {
    match target {
        OccurrenceTarget::Foreign(key) => {
            if key.path != path {
                return Err(TestError::Falsified("universe field path mismatch"));
            }
            if key.display != display {
                return Err(TestError::Falsified("universe field display mismatch"));
            }
            if key.kind != Some(EntityKind::Field) {
                return Err(TestError::Falsified("universe field kind is not Field"));
            }
            if !matches!(key.origin, ForeignOrigin::Universe { ecosystem: "pypi" }) {
                return Err(TestError::Falsified("universe field ecosystem is not pypi"));
            }
            Ok(())
        }
        OccurrenceTarget::Local(_) => Err(TestError::Falsified("expected universe field target")),
        OccurrenceTarget::Stable(_) => Err(TestError::Falsified("expected universe field target")),
    }
}

#[test]
fn annotated_receiver_call_uses_the_imported_or_local_class() -> Result<(), TestError> {
    const ANNOTATED_SOURCE: &[u8] = b"\
from workout.service import WorkoutService

class LocalService:
    def set_note(self):
        return 1

def sync(service: WorkoutService):
    return service.set_note()

def local(service: LocalService):
    return service.set_note()

def plain(service):
    return service.set_note()
";

    const AMBIGUOUS_SOURCE: &[u8] = b"\
from workout.service import Service
from other.place import Service

def sync(service: Service):
    return service.set_note()
";

    const PAIRED_SOURCE: &[u8] = b"\
class Pair:
    def set_note(self):
        return 1
    class Inner:
        def set_note(self):
            return 2

def paired(service: Pair):
    return service.set_note()
";

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
        "nudox-python-annotated-receiver-{nonce}-{}-{}",
        std::process::id(),
        fixture_sequence()
    ));
    fs::create_dir_all(&work).map_err(|source| TestError::Io("create scratch", source))?;
    let cancelled = AtomicBool::new(false);

    let annotated_fragment =
        compile_python_fragment(ANNOTATED_SOURCE, &work, &toolchain, &cancelled)?;
    let decoded = FragmentView::validate(&annotated_fragment).map_err(|_| TestError::Validate)?;
    let atoms: Vec<&[u8]> = decoded.atoms().map(|atom| atom.bytes).collect();
    let mut occurrences: Vec<backend_semantic::ir::DecodedOccurrence<'_>> = Vec::new();
    if let Some(mut cursor) = decoded.occurrences() {
        for row in cursor.by_ref() {
            occurrences.push(row.map_err(|_| TestError::Falsified("occurrence decode"))?);
        }
    }

    let sync_owner = entity_ordinal_by_name(&decoded, &atoms, b"sync", EntityKind::Function)?;
    let local_owner = entity_ordinal_by_name(&decoded, &atoms, b"local", EntityKind::Function)?;
    let plain_owner = entity_ordinal_by_name(&decoded, &atoms, b"plain", EntityKind::Function)?;
    let set_note_method =
        entity_ordinal_by_name(&decoded, &atoms, b"set_note", EntityKind::Function)?;

    let sync_call = set_note_in_owner(&occurrences, sync_owner)
        .ok_or(TestError::Falsified("sync set_note occurrence absent"))?;
    let OccurrenceTarget::Foreign(sync_foreign) = &sync_call.occurrence.target else {
        return Err(TestError::Falsified(
            "sync set_note is not a foreign package key",
        ));
    };
    if sync_foreign.path != "workout.service" || sync_foreign.display != "set_note" {
        return Err(TestError::Falsified(
            "sync set_note is not a workout.service package key",
        ));
    }
    if !matches!(
        sync_foreign.origin,
        ForeignOrigin::Package(lineage) if lineage.name == "workout"
    ) {
        return Err(TestError::Falsified(
            "sync set_note package lineage is not workout",
        ));
    }

    let local_call = set_note_in_owner(&occurrences, local_owner)
        .ok_or(TestError::Falsified("local set_note occurrence absent"))?;
    if !matches!(
        &local_call.occurrence.target,
        OccurrenceTarget::Local(target) if target.raw == set_note_method
    ) {
        return Err(TestError::Falsified(
            "local set_note does not resolve to LocalService.set_note",
        ));
    }

    let plain_call = set_note_in_owner(&occurrences, plain_owner)
        .ok_or(TestError::Falsified("plain set_note occurrence absent"))?;
    if !matches!(
        &plain_call.occurrence.target,
        OccurrenceTarget::Foreign(key)
            if key.path == "set_note"
                && key.display == "set_note"
                && matches!(key.origin, ForeignOrigin::Universe { ecosystem: "pypi" })
    ) {
        return Err(TestError::Falsified(
            "plain set_note is not an honest universe foreign method key",
        ));
    }

    let ambiguous_fragment =
        compile_python_fragment(AMBIGUOUS_SOURCE, &work, &toolchain, &cancelled)?;
    let ambiguous =
        FragmentView::validate(&ambiguous_fragment).map_err(|_| TestError::Validate)?;
    let mut ambiguous_occurrences: Vec<backend_semantic::ir::DecodedOccurrence<'_>> = Vec::new();
    if let Some(mut cursor) = ambiguous.occurrences() {
        for row in cursor.by_ref() {
            ambiguous_occurrences
                .push(row.map_err(|_| TestError::Falsified("ambiguous occurrence decode"))?);
        }
    }
    let ambiguous_atoms: Vec<&[u8]> = ambiguous.atoms().map(|atom| atom.bytes).collect();
    let ambiguous_sync =
        entity_ordinal_by_name(&ambiguous, &ambiguous_atoms, b"sync", EntityKind::Function)?;
    let ambiguous_call = set_note_in_owner(&ambiguous_occurrences, ambiguous_sync)
        .ok_or(TestError::Falsified("ambiguous sync set_note absent"))?;
    // Identical import aliases shadow with later-wins, so only `other.place`
    // stays live and the annotated receiver resolves through that module.
    // The extractor keeps the first module-level `Service` import; the
    // second binding is dropped before lowering, so the annotated receiver
    // resolves through `workout.service`, not `other.place`.
    if !matches!(
        &ambiguous_call.occurrence.target,
        OccurrenceTarget::Foreign(key)
            if key.path == "workout.service"
                && key.display == "set_note"
                && matches!(
                    key.origin,
                    ForeignOrigin::Package(lineage) if lineage.name == "workout"
                )
    ) {
        return Err(TestError::Falsified(
            "ambiguous Service import did not resolve through the surviving alias",
        ));
    }

    let paired_fragment =
        compile_python_fragment(PAIRED_SOURCE, &work, &toolchain, &cancelled)?;
    let paired = FragmentView::validate(&paired_fragment).map_err(|_| TestError::Validate)?;
    let mut paired_occurrences: Vec<backend_semantic::ir::DecodedOccurrence<'_>> = Vec::new();
    if let Some(mut cursor) = paired.occurrences() {
        for row in cursor.by_ref() {
            paired_occurrences
                .push(row.map_err(|_| TestError::Falsified("paired occurrence decode"))?);
        }
    }
    let paired_atoms: Vec<&[u8]> = paired.atoms().map(|atom| atom.bytes).collect();
    let paired_owner =
        entity_ordinal_by_name(&paired, &paired_atoms, b"paired", EntityKind::Function)?;
    let paired_call = set_note_in_owner(&paired_occurrences, paired_owner)
        .ok_or(TestError::Falsified("paired set_note occurrence absent"))?;
    if paired_call.occurrence.kind != ReferenceKind::MethodCall {
        return Err(TestError::Falsified("paired set_note is not a MethodCall"));
    }
    if matches!(&paired_call.occurrence.target, OccurrenceTarget::Local(_)) {
        return Err(TestError::Falsified(
            "paired set_note must not resolve to a nested local method",
        ));
    }
    if !matches!(
        &paired_call.occurrence.target,
        OccurrenceTarget::Foreign(key)
            if key.path == "set_note"
                && key.display == "set_note"
                && key.kind == Some(EntityKind::Function)
                && matches!(key.origin, ForeignOrigin::Universe { ecosystem: "pypi" })
    ) {
        return Err(TestError::Falsified(
            "paired set_note is not an honest universe foreign method key",
        ));
    }

    fs::remove_dir_all(&work).map_err(|source| TestError::Io("remove scratch", source))?;
    Ok(())
}

#[test]
fn annotated_receiver_attribute_read_uses_the_imported_or_local_class() -> Result<(), TestError> {
    const ANNOTATED_SOURCE: &[u8] = b"\
from workout.service import WorkoutService

class LocalService:
    def set_note(self):
        return 1

def sync(service: WorkoutService):
    bound = service.set_note

def local(service: LocalService):
    bound = service.set_note

def plain(service):
    seen = service.set_note
";

    const FIELD_SOURCE: &[u8] = b"\
class LocalService:
    note = 1

def read(service: LocalService):
    seen = service.note
";

    const AMBIGUOUS_SOURCE: &[u8] = b"\
from workout.service import Service
from other.place import Service

def sync(service: Service):
    seen = service.set_note
";

    const PAIRED_SOURCE: &[u8] = b"\
class Pair:
    def set_note(self):
        return 1
    class Inner:
        def set_note(self):
            return 2

def paired(service: Pair):
    seen = service.set_note
";

    const MIXED_SOURCE: &[u8] = b"\
class Both:
    note = 1
    def note(self):
        return 2

def mixed(service: Both):
    seen = service.note
";

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
        "nudox-python-annotated-attr-read-{nonce}-{}-{}",
        std::process::id(),
        fixture_sequence()
    ));
    fs::create_dir_all(&work).map_err(|source| TestError::Io("create scratch", source))?;
    let cancelled = AtomicBool::new(false);

    let annotated_fragment =
        compile_python_fragment(ANNOTATED_SOURCE, &work, &toolchain, &cancelled)?;
    let (decoded, atoms, occurrences) = decode_occurrences(&annotated_fragment)?;

    let sync_owner = entity_ordinal_by_name(&decoded, &atoms, b"sync", EntityKind::Function)?;
    let local_owner = entity_ordinal_by_name(&decoded, &atoms, b"local", EntityKind::Function)?;
    let plain_owner = entity_ordinal_by_name(&decoded, &atoms, b"plain", EntityKind::Function)?;
    let set_note_method =
        entity_ordinal_by_name(&decoded, &atoms, b"set_note", EntityKind::Function)?;

    let sync_read = field_access_in_owner(&occurrences, sync_owner)?;
    assert_foreign_package_field(&sync_read.occurrence.target, "workout.service", "set_note", "workout")?;

    let local_read = field_access_in_owner(&occurrences, local_owner)?;
    match &local_read.occurrence.target {
        OccurrenceTarget::Local(target) if target.raw == set_note_method => {}
        _ => return Err(TestError::Falsified("local attribute read target mismatch")),
    }

    let plain_read = field_access_in_owner(&occurrences, plain_owner)?;
    assert_universe_field(&plain_read.occurrence.target, "set_note", "set_note")?;

    let field_fragment = compile_python_fragment(FIELD_SOURCE, &work, &toolchain, &cancelled)?;
    let (field_decoded, field_atoms, field_occurrences) = decode_occurrences(&field_fragment)?;
    let read_owner =
        entity_ordinal_by_name(&field_decoded, &field_atoms, b"read", EntityKind::Function)?;
    let note_field = entity_ordinal_by_name(&field_decoded, &field_atoms, b"note", EntityKind::Field)?;
    let field_read = field_access_in_owner(&field_occurrences, read_owner)?;
    match &field_read.occurrence.target {
        OccurrenceTarget::Local(target) if target.raw == note_field => {}
        _ => return Err(TestError::Falsified("field attribute read target mismatch")),
    }

    let ambiguous_fragment =
        compile_python_fragment(AMBIGUOUS_SOURCE, &work, &toolchain, &cancelled)?;
    let (ambiguous_decoded, ambiguous_atoms, ambiguous_occurrences) =
        decode_occurrences(&ambiguous_fragment)?;
    let ambiguous_sync =
        entity_ordinal_by_name(&ambiguous_decoded, &ambiguous_atoms, b"sync", EntityKind::Function)?;
    let ambiguous_read = field_access_in_owner(&ambiguous_occurrences, ambiguous_sync)?;
    assert_foreign_package_field(
        &ambiguous_read.occurrence.target,
        "workout.service",
        "set_note",
        "workout",
    )?;
    match &ambiguous_read.occurrence.target {
        OccurrenceTarget::Foreign(key) if key.path == "other.place" => {
            return Err(TestError::Falsified("ambiguous import resolved through other.place"));
        }
        _ => {}
    }

    let paired_fragment =
        compile_python_fragment(PAIRED_SOURCE, &work, &toolchain, &cancelled)?;
    let (paired_decoded, paired_atoms, paired_occurrences) = decode_occurrences(&paired_fragment)?;
    let paired_owner =
        entity_ordinal_by_name(&paired_decoded, &paired_atoms, b"paired", EntityKind::Function)?;
    let paired_read = field_access_in_owner(&paired_occurrences, paired_owner)?;
    if matches!(&paired_read.occurrence.target, OccurrenceTarget::Local(_)) {
        return Err(TestError::Falsified("paired attribute read must not be local"));
    }
    assert_universe_field(&paired_read.occurrence.target, "set_note", "set_note")?;

    let mixed_fragment = compile_python_fragment(MIXED_SOURCE, &work, &toolchain, &cancelled)?;
    let (mixed_decoded, mixed_atoms, mixed_occurrences) = decode_occurrences(&mixed_fragment)?;
    let mixed_owner =
        entity_ordinal_by_name(&mixed_decoded, &mixed_atoms, b"mixed", EntityKind::Function)?;
    let mixed_read = field_access_in_owner(&mixed_occurrences, mixed_owner)?;
    if matches!(&mixed_read.occurrence.target, OccurrenceTarget::Local(_)) {
        return Err(TestError::Falsified("mixed attribute read must not be local"));
    }
    assert_universe_field(&mixed_read.occurrence.target, "note", "note")?;

    fs::remove_dir_all(&work).map_err(|source| TestError::Io("remove scratch", source))?;
    Ok(())
}

fn compile_with_module(
    source: &[u8],
    work: &std::path::Path,
    toolchain: &ResolvedToolchain,
    cancelled: &AtomicBool,
) -> Result<(Vec<u8>, backend_frontend_python::legacy::ModuleFacts), TestError> {
    let fragment = compile_python_fragment(source, work, toolchain, cancelled)?;
    let module = extract(source, PythonVersion::Python314).map_err(|_| TestError::Extract)?;
    Ok((fragment, module))
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
        return Err(TestError::Falsified("method declaration in class not unique"));
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

fn assert_local_field_access(
    read: &backend_semantic::ir::DecodedOccurrence<'_>,
    expected: EntityId,
) -> Result<(), TestError> {
    if read.occurrence.kind != ReferenceKind::FieldAccess {
        return Err(TestError::Falsified("attribute read is not FieldAccess"));
    }
    if read.occurrence.confidence != OccurrenceConfidence::Index {
        return Err(TestError::Falsified("local FieldAccess confidence is not Index"));
    }
    if !matches!(
        &read.occurrence.target,
        OccurrenceTarget::Local(target) if target.raw == expected.raw
    ) {
        return Err(TestError::Falsified(
            "local FieldAccess target does not match the class-scoped entity",
        ));
    }
    Ok(())
}

fn assert_universe_field_access(
    read: &backend_semantic::ir::DecodedOccurrence<'_>,
    spelling: &str,
) -> Result<(), TestError> {
    if read.occurrence.kind != ReferenceKind::FieldAccess {
        return Err(TestError::Falsified("attribute read is not FieldAccess"));
    }
    if read.occurrence.confidence != OccurrenceConfidence::Index {
        return Err(TestError::Falsified("universe FieldAccess confidence is not Index"));
    }
    assert_universe_field(&read.occurrence.target, spelling, spelling)?;
    Ok(())
}

struct PythonFixture {
    executable: std::path::PathBuf,
    version_bytes: Vec<u8>,
    work: std::path::PathBuf,
    cancelled: AtomicBool,
}

fn python_fixture() -> Result<PythonFixture, TestError> {
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
        version.stderr
    } else {
        version.stdout
    };
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(TestError::Clock)?
        .as_nanos();
    let work = std::env::temp_dir().join(format!(
        "nudox-python-annotated-inherited-read-{nonce}-{}-{}",
        std::process::id(),
        fixture_sequence()
    ));
    fs::create_dir_all(&work).map_err(|source| TestError::Io("create scratch", source))?;
    Ok(PythonFixture {
        executable,
        version_bytes,
        work,
        cancelled: AtomicBool::new(false),
    })
}

fn resolved_toolchain(fixture: &PythonFixture) -> Result<ResolvedToolchain<'_>, TestError> {
    ResolvedToolchain::from_version(
        NativeTool::Python,
        &fixture.executable,
        &fixture.version_bytes,
    )
    .map_err(|_| TestError::Resolve)
}

fn run_inherited_read_fixture(
    source: &[u8],
) -> Result<(Vec<u8>, backend_frontend_python::legacy::ModuleFacts), TestError> {
    let fixture = python_fixture()?;
    let toolchain = resolved_toolchain(&fixture)?;
    let (fragment_bytes, module) =
        compile_with_module(source, &fixture.work, &toolchain, &fixture.cancelled)?;
    fs::remove_dir_all(&fixture.work).map_err(|source| TestError::Io("remove scratch", source))?;
    Ok((fragment_bytes, module))
}

#[test]
fn annotated_inherited_attribute_read_resolves_unique_base_field() -> Result<(), TestError> {
    const SOURCE: &[u8] = b"\
class Other:
    note = 9
class Base:
    note = 1
class Child(Base):
    pass
def read(item: Child):
    return item.note
";
    let (fragment_bytes, module) = run_inherited_read_fixture(SOURCE)?;
    let (decoded, atoms, occurrences) = decode_occurrences(&fragment_bytes)?;
    let read_owner = entity_ordinal_by_name(&decoded, &atoms, b"read", EntityKind::Function)?;
    let other_note = field_ordinal_in_class(&decoded, &atoms, &module, b"Other", b"note")?;
    let base_note = field_ordinal_in_class(&decoded, &atoms, &module, b"Base", b"note")?;
    if other_note == base_note {
        return Err(TestError::Falsified("Other.note and Base.note must differ"));
    }
    let read = field_access_in_owner(&occurrences, read_owner)?;
    assert_local_field_access(read, base_note)?;
    Ok(())
}

#[test]
fn annotated_inherited_attribute_read_resolves_unique_base_method() -> Result<(), TestError> {
    const SOURCE: &[u8] = b"\
class Other:
    def note(self):
        return 9
class Base:
    def note(self):
        return 1
class Child(Base):
    pass
def read(item: Child):
    return item.note
";
    let (fragment_bytes, module) = run_inherited_read_fixture(SOURCE)?;
    let (decoded, atoms, occurrences) = decode_occurrences(&fragment_bytes)?;
    let read_owner = entity_ordinal_by_name(&decoded, &atoms, b"read", EntityKind::Function)?;
    let other_note = method_ordinal_in_class(&decoded, &atoms, &module, b"Other", b"note")?;
    let base_note = method_ordinal_in_class(&decoded, &atoms, &module, b"Base", b"note")?;
    if other_note == base_note {
        return Err(TestError::Falsified("Other.note and Base.note must differ"));
    }
    let read = field_access_in_owner(&occurrences, read_owner)?;
    if read.occurrence.kind == ReferenceKind::MethodCall {
        return Err(TestError::Falsified("attribute read must not be MethodCall"));
    }
    assert_local_field_access(read, base_note)?;
    Ok(())
}

#[test]
fn annotated_inherited_attribute_read_resolves_two_level_base_method() -> Result<(), TestError> {
    const SOURCE: &[u8] = b"\
class Other:
    def note(self):
        return 9
class Grand:
    def note(self):
        return 1
class Base(Grand):
    pass
class Child(Base):
    pass
def read(item: Child):
    return item.note
";
    let (fragment_bytes, module) = run_inherited_read_fixture(SOURCE)?;
    let (decoded, atoms, occurrences) = decode_occurrences(&fragment_bytes)?;
    let read_owner = entity_ordinal_by_name(&decoded, &atoms, b"read", EntityKind::Function)?;
    let grand_note = method_ordinal_in_class(&decoded, &atoms, &module, b"Grand", b"note")?;
    let read = field_access_in_owner(&occurrences, read_owner)?;
    assert_local_field_access(read, grand_note)?;
    Ok(())
}

#[test]
fn annotated_inherited_attribute_read_prefers_child_field_over_base_method() -> Result<(), TestError> {
    const SOURCE: &[u8] = b"\
class Other:
    note = 9
class Base:
    def note(self):
        return 1
class Child(Base):
    note = 1
def read(item: Child):
    return item.note
";
    let (fragment_bytes, module) = run_inherited_read_fixture(SOURCE)?;
    let (decoded, atoms, occurrences) = decode_occurrences(&fragment_bytes)?;
    let read_owner = entity_ordinal_by_name(&decoded, &atoms, b"read", EntityKind::Function)?;
    let child_note = field_ordinal_in_class(&decoded, &atoms, &module, b"Child", b"note")?;
    let base_note = method_ordinal_in_class(&decoded, &atoms, &module, b"Base", b"note")?;
    if child_note == base_note {
        return Err(TestError::Falsified("Child field and Base method must differ"));
    }
    let read = field_access_in_owner(&occurrences, read_owner)?;
    assert_local_field_access(read, child_note)?;
    Ok(())
}

#[test]
fn annotated_inherited_attribute_read_stays_universe_for_ambiguous_local_members() -> Result<(), TestError> {
    const SOURCE: &[u8] = b"\
class Other:
    note = 9
class Base:
    note = 1
class Child(Base):
    note = 1
    def note(self):
        return 2
def read(item: Child):
    return item.note
";
    let (fragment_bytes, module) = run_inherited_read_fixture(SOURCE)?;
    let (decoded, atoms, occurrences) = decode_occurrences(&fragment_bytes)?;
    let read_owner = entity_ordinal_by_name(&decoded, &atoms, b"read", EntityKind::Function)?;
    let child_field = field_ordinal_in_class(&decoded, &atoms, &module, b"Child", b"note")?;
    let child_method = method_ordinal_in_class(&decoded, &atoms, &module, b"Child", b"note")?;
    let base_field = field_ordinal_in_class(&decoded, &atoms, &module, b"Base", b"note")?;
    if child_field == child_method || child_field == base_field || child_method == base_field {
        return Err(TestError::Falsified(
            "child field, child method, and base field must be three distinct ordinals",
        ));
    }
    let read = field_access_in_owner(&occurrences, read_owner)?;
    assert_universe_field_access(read, "note")?;
    Ok(())
}

#[test]
fn annotated_inherited_attribute_read_stays_universe_for_ambiguous_bases() -> Result<(), TestError> {
    const SOURCE: &[u8] = b"\
class Other:
    note = 9
class Left:
    note = 1
class Right:
    note = 2
class Child(Left, Right):
    pass
def read(item: Child):
    return item.note
";
    let (fragment_bytes, module) = run_inherited_read_fixture(SOURCE)?;
    let (decoded, atoms, occurrences) = decode_occurrences(&fragment_bytes)?;
    let read_owner = entity_ordinal_by_name(&decoded, &atoms, b"read", EntityKind::Function)?;
    let left_note = field_ordinal_in_class(&decoded, &atoms, &module, b"Left", b"note")?;
    let right_note = field_ordinal_in_class(&decoded, &atoms, &module, b"Right", b"note")?;
    if left_note == right_note {
        return Err(TestError::Falsified("Left.note and Right.note must differ"));
    }
    let read = field_access_in_owner(&occurrences, read_owner)?;
    assert_universe_field_access(read, "note")?;
    Ok(())
}

#[test]
fn annotated_inherited_attribute_read_stays_universe_for_field_method_across_bases() -> Result<(), TestError> {
    const SOURCE: &[u8] = b"\
class Other:
    note = 9
class Left:
    note = 1
class Right:
    def note(self):
        return 2
class Child(Left, Right):
    pass
def read(item: Child):
    return item.note
";
    let (fragment_bytes, module) = run_inherited_read_fixture(SOURCE)?;
    let (decoded, atoms, occurrences) = decode_occurrences(&fragment_bytes)?;
    let read_owner = entity_ordinal_by_name(&decoded, &atoms, b"read", EntityKind::Function)?;
    let left_note = field_ordinal_in_class(&decoded, &atoms, &module, b"Left", b"note")?;
    let right_note = method_ordinal_in_class(&decoded, &atoms, &module, b"Right", b"note")?;
    if left_note == right_note {
        return Err(TestError::Falsified("Left field and Right method must differ"));
    }
    let read = field_access_in_owner(&occurrences, read_owner)?;
    assert_universe_field_access(read, "note")?;
    Ok(())
}

#[test]
fn annotated_inherited_attribute_read_stays_universe_when_base_has_field_and_method(
) -> Result<(), TestError> {
    const SOURCE: &[u8] = b"\
class Base:
    note = 1
    def note(self):
        return 2
class Child(Base):
    pass
def read(item: Child):
    return item.note
";
    let (fragment_bytes, module) = run_inherited_read_fixture(SOURCE)?;
    let (decoded, atoms, occurrences) = decode_occurrences(&fragment_bytes)?;
    let read_owner = entity_ordinal_by_name(&decoded, &atoms, b"read", EntityKind::Function)?;
    let base_field = field_ordinal_in_class(&decoded, &atoms, &module, b"Base", b"note")?;
    let base_method = method_ordinal_in_class(&decoded, &atoms, &module, b"Base", b"note")?;
    if base_field == base_method {
        return Err(TestError::Falsified("Base field and Base method must differ"));
    }
    let read = field_access_in_owner(&occurrences, read_owner)?;
    assert_universe_field_access(read, "note")?;
    Ok(())
}

#[test]
fn annotated_inherited_attribute_read_keeps_unique_module_field_when_bases_declare_none(
) -> Result<(), TestError> {
    const SOURCE: &[u8] = b"\
class Other:
    note = 1
class Child:
    pass
def read(item: Child):
    return item.note
";
    let (fragment_bytes, module) = run_inherited_read_fixture(SOURCE)?;
    let (decoded, atoms, occurrences) = decode_occurrences(&fragment_bytes)?;
    let read_owner = entity_ordinal_by_name(&decoded, &atoms, b"read", EntityKind::Function)?;
    let other_note = field_ordinal_in_class(&decoded, &atoms, &module, b"Other", b"note")?;
    let read = field_access_in_owner(&occurrences, read_owner)?;
    assert_local_field_access(read, other_note)?;
    Ok(())
}

#[test]
fn annotated_inherited_attribute_read_resolves_diamond_base_field() -> Result<(), TestError> {
    const SOURCE: &[u8] = b"\
class Other:
    note = 9
class Base:
    note = 1
class Left(Base):
    pass
class Right(Base):
    pass
class Child(Left, Right):
    pass
def read(item: Child):
    return item.note
";
    let (fragment_bytes, module) = run_inherited_read_fixture(SOURCE)?;
    let (decoded, atoms, occurrences) = decode_occurrences(&fragment_bytes)?;
    let read_owner = entity_ordinal_by_name(&decoded, &atoms, b"read", EntityKind::Function)?;
    let base_note = field_ordinal_in_class(&decoded, &atoms, &module, b"Base", b"note")?;
    let read = field_access_in_owner(&occurrences, read_owner)?;
    assert_local_field_access(read, base_note)?;
    Ok(())
}

#[test]
fn annotated_inherited_attribute_read_skips_unresolved_base() -> Result<(), TestError> {
    const SOURCE: &[u8] = b"\
class Other:
    def note(self):
        return 9
class Base:
    def note(self):
        return 1
class Child(object, Base):
    pass
def read(item: Child):
    return item.note
";
    let (fragment_bytes, module) = run_inherited_read_fixture(SOURCE)?;
    let (decoded, atoms, occurrences) = decode_occurrences(&fragment_bytes)?;
    let read_owner = entity_ordinal_by_name(&decoded, &atoms, b"read", EntityKind::Function)?;
    let base_note = method_ordinal_in_class(&decoded, &atoms, &module, b"Base", b"note")?;
    let read = field_access_in_owner(&occurrences, read_owner)?;
    assert_local_field_access(read, base_note)?;
    Ok(())
}

#[test]
fn annotated_inherited_attribute_read_resolves_generic_base_field() -> Result<(), TestError> {
    const SOURCE: &[u8] = b"\
class Other:
    note = 9
class Base:
    note = 1
class Child(Base[int]):
    pass
def read(item: Child):
    return item.note
";
    let (fragment_bytes, module) = run_inherited_read_fixture(SOURCE)?;
    let (decoded, atoms, occurrences) = decode_occurrences(&fragment_bytes)?;
    let read_owner = entity_ordinal_by_name(&decoded, &atoms, b"read", EntityKind::Function)?;
    let base_note = field_ordinal_in_class(&decoded, &atoms, &module, b"Base", b"note")?;
    let read = field_access_in_owner(&occurrences, read_owner)?;
    assert_local_field_access(read, base_note)?;
    Ok(())
}

#[test]
fn annotated_inherited_attribute_read_stays_universe_for_imported_only_base() -> Result<(), TestError> {
    const SOURCE: &[u8] = b"\
from workout.service import WorkoutService
class Child(WorkoutService):
    pass
def read(item: Child):
    return item.note
";
    let (fragment_bytes, _) = run_inherited_read_fixture(SOURCE)?;
    let (decoded, atoms, occurrences) = decode_occurrences(&fragment_bytes)?;
    let read_owner = entity_ordinal_by_name(&decoded, &atoms, b"read", EntityKind::Function)?;
    let read = field_access_in_owner(&occurrences, read_owner)?;
    assert_universe_field_access(read, "note")?;
    Ok(())
}
