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
use backend_semantic::ir::{
    EntityKind, ForeignOrigin, FragmentView, OccurrenceTarget, ReferenceKind,
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
        row.owner.raw == owner && row.occurrence.kind == ReferenceKind::MethodCall
    })
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
