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
    EntityKind, ForeignOrigin, FragmentView, OccurrenceConfidence, OccurrenceTarget, ReferenceKind,
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
        row.owner.raw == owner
            && row.occurrence.kind == ReferenceKind::MethodCall
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

struct InheritedFixtureEnv {
    executable: std::path::PathBuf,
    work: std::path::PathBuf,
}

impl InheritedFixtureEnv {
    fn toolchain(&self) -> Result<ResolvedToolchain<'_>, TestError> {
        let version = Command::new(&self.executable)
            .arg("--version")
            .output()
            .map_err(TestError::Tool)?;
        let version_bytes = if version.stdout.is_empty() {
            version.stderr.as_slice()
        } else {
            version.stdout.as_slice()
        };
        ResolvedToolchain::from_version(NativeTool::Python, &self.executable, version_bytes)
            .map_err(|_| TestError::Resolve)
    }
}

fn inherited_fixture_env() -> Result<InheritedFixtureEnv, TestError> {
    let executable = std::env::var_os("PATH")
        .and_then(|path| {
            std::env::split_paths(&path)
                .map(|directory| directory.join("python3"))
                .find(|candidate| candidate.is_file())
        })
        .ok_or(TestError::MissingPython)?;
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(TestError::Clock)?
        .as_nanos();
    let work = std::env::temp_dir().join(format!(
        "nudox-python-inherited-{nonce}-{}-{}",
        std::process::id(),
        fixture_sequence()
    ));
    fs::create_dir_all(&work).map_err(|source| TestError::Io("create scratch", source))?;
    Ok(InheritedFixtureEnv { executable, work })
}

struct InheritedFragment {
    bytes: Vec<u8>,
}

struct InheritedFragmentView<'a> {
    decoded: FragmentView<'a>,
    atoms: Vec<&'a [u8]>,
    occurrences: Vec<backend_semantic::ir::DecodedOccurrence<'a>>,
}

impl InheritedFragment {
    fn view(&self) -> Result<InheritedFragmentView<'_>, TestError> {
        let decoded = FragmentView::validate(&self.bytes).map_err(|_| TestError::Validate)?;
        let atoms: Vec<&[u8]> = decoded.atoms().map(|atom| atom.bytes).collect();
        let mut occurrences: Vec<backend_semantic::ir::DecodedOccurrence<'_>> = Vec::new();
        if let Some(mut cursor) = decoded.occurrences() {
            for row in cursor.by_ref() {
                occurrences.push(row.map_err(|_| TestError::Falsified("occurrence decode"))?);
            }
        }
        Ok(InheritedFragmentView {
            decoded,
            atoms,
            occurrences,
        })
    }
}

fn compile_inherited_fragment(
    source: &[u8],
    work: &std::path::Path,
    toolchain: &ResolvedToolchain<'_>,
    cancelled: &AtomicBool,
) -> Result<InheritedFragment, TestError> {
    Ok(InheritedFragment {
        bytes: compile_python_fragment(source, work, toolchain, cancelled)?,
    })
}

fn entity_ordinal_at_index(
    fragment: &InheritedFragmentView<'_>,
    name: &[u8],
    kind: EntityKind,
    index: usize,
) -> Result<u32, TestError> {
    let mut matches = Vec::new();
    for entity in fragment.decoded.entities() {
        if fragment.atoms.get(entity.name.raw as usize).copied() == Some(name)
            && entity.kind == kind
        {
            matches.push(entity.entity.raw);
        }
    }
    if matches.len() <= index {
        return Err(TestError::Falsified("entity absent"));
    }
    Ok(matches[index])
}

fn read_method_calls<'a>(
    fragment: &'a InheritedFragmentView<'a>,
    read_owner: u32,
) -> Result<Vec<&'a backend_semantic::ir::DecodedOccurrence<'a>>, TestError> {
    let calls: Vec<_> = fragment
        .occurrences
        .iter()
        .filter(|row| {
            row.owner.raw == read_owner && row.occurrence.kind == ReferenceKind::MethodCall
        })
        .collect();
    Ok(calls)
}

fn read_field_accesses<'a>(
    fragment: &'a InheritedFragmentView<'a>,
    read_owner: u32,
) -> Result<Vec<&'a backend_semantic::ir::DecodedOccurrence<'a>>, TestError> {
    let reads: Vec<_> = fragment
        .occurrences
        .iter()
        .filter(|row| {
            row.owner.raw == read_owner && row.occurrence.kind == ReferenceKind::FieldAccess
        })
        .collect();
    Ok(reads)
}

fn field_accesses_with_path<'a>(
    fragment: &'a InheritedFragmentView<'a>,
    read_owner: u32,
    path: &[u8],
) -> Result<Vec<&'a backend_semantic::ir::DecodedOccurrence<'a>>, TestError> {
    let reads = read_field_accesses(fragment, read_owner)?;
    let mut matched = Vec::new();
    for row in reads {
        let Some(spelling) = occurrence_field_path(fragment, row) else {
            continue;
        };
        if spelling == path {
            matched.push(row);
        }
    }
    Ok(matched)
}

fn method_calls_with_path<'a>(
    fragment: &'a InheritedFragmentView<'a>,
    read_owner: u32,
    path: &[u8],
) -> Result<Vec<&'a backend_semantic::ir::DecodedOccurrence<'a>>, TestError> {
    let calls = read_method_calls(fragment, read_owner)?;
    let mut matched = Vec::new();
    for row in calls {
        let Some(spelling) = occurrence_field_path(fragment, row) else {
            continue;
        };
        if spelling == path {
            matched.push(row);
        }
    }
    Ok(matched)
}

fn occurrence_field_path<'a>(
    fragment: &'a InheritedFragmentView<'a>,
    row: &backend_semantic::ir::DecodedOccurrence<'a>,
) -> Option<&'a [u8]> {
    match &row.occurrence.target {
        OccurrenceTarget::Local(id) => fragment.decoded.entities().find_map(|entity| {
            if entity.entity.raw == id.raw {
                fragment.atoms.get(entity.name.raw as usize).copied()
            } else {
                None
            }
        }),
        OccurrenceTarget::Foreign(key) if key.path.as_bytes() == key.display.as_bytes() => {
            Some(key.path.as_bytes())
        }
        OccurrenceTarget::Foreign(key) => Some(key.display.as_bytes()),
        OccurrenceTarget::Stable(_) => None,
    }
}

fn assert_local_index(
    row: &backend_semantic::ir::DecodedOccurrence<'_>,
    ordinal: u32,
    label: &'static str,
) -> Result<(), TestError> {
    if !matches!(
        &row.occurrence.target,
        OccurrenceTarget::Local(target) if target.raw == ordinal
    ) {
        return Err(TestError::Falsified(label));
    }
    if row.occurrence.confidence != OccurrenceConfidence::Index {
        return Err(TestError::Falsified(label));
    }
    Ok(())
}

fn assert_universe_method(
    row: &backend_semantic::ir::DecodedOccurrence<'_>,
    label: &'static str,
) -> Result<(), TestError> {
    if !matches!(
        &row.occurrence.target,
        OccurrenceTarget::Foreign(key)
            if key.path == "note"
                && key.display == "note"
                && key.kind == Some(EntityKind::Function)
                && matches!(key.origin, ForeignOrigin::Universe { ecosystem: "pypi" })
    ) {
        return Err(TestError::Falsified(label));
    }
    Ok(())
}

fn assert_universe_method_named(
    row: &backend_semantic::ir::DecodedOccurrence<'_>,
    name: &[u8],
    label: &'static str,
) -> Result<(), TestError> {
    let name = core::str::from_utf8(name).map_err(|_| TestError::Falsified(label))?;
    if !matches!(
        &row.occurrence.target,
        OccurrenceTarget::Foreign(key)
            if key.path == name
                && key.display == name
                && key.kind == Some(EntityKind::Function)
                && matches!(key.origin, ForeignOrigin::Universe { ecosystem: "pypi" })
    ) {
        return Err(TestError::Falsified(label));
    }
    if row.occurrence.confidence != OccurrenceConfidence::Index {
        return Err(TestError::Falsified(label));
    }
    Ok(())
}

fn assert_universe_field(
    row: &backend_semantic::ir::DecodedOccurrence<'_>,
    label: &'static str,
) -> Result<(), TestError> {
    if !matches!(
        &row.occurrence.target,
        OccurrenceTarget::Foreign(key)
            if key.path == "note"
                && key.display == "note"
                && key.kind == Some(EntityKind::Field)
                && matches!(key.origin, ForeignOrigin::Universe { ecosystem: "pypi" })
    ) {
        return Err(TestError::Falsified(label));
    }
    Ok(())
}

fn assert_universe_field_named(
    row: &backend_semantic::ir::DecodedOccurrence<'_>,
    name: &[u8],
    label: &'static str,
) -> Result<(), TestError> {
    let name = core::str::from_utf8(name).map_err(|_| TestError::Falsified(label))?;
    if !matches!(
        &row.occurrence.target,
        OccurrenceTarget::Foreign(key)
            if key.path == name
                && key.display == name
                && key.kind == Some(EntityKind::Field)
                && matches!(key.origin, ForeignOrigin::Universe { ecosystem: "pypi" })
    ) {
        return Err(TestError::Falsified(label));
    }
    if row.occurrence.confidence != OccurrenceConfidence::Index {
        return Err(TestError::Falsified(label));
    }
    Ok(())
}

#[test]
fn py_inherited_call_targets_base_note_not_decoy() -> Result<(), TestError> {
    const SOURCE: &[u8] = b"\
class Decoy:
    def note(self):
        return 0

class Base:
    def note(self):
        return 1

class Child(Base):
    def read(self):
        return self.note()
";
    let env = inherited_fixture_env()?;
    let toolchain = env.toolchain()?;
    let cancelled = AtomicBool::new(false);
    let fragment = compile_inherited_fragment(SOURCE, &env.work, &toolchain, &cancelled)?;
    let view = fragment.view()?;
    let read_owner = entity_ordinal_at_index(&view, b"read", EntityKind::Function, 0)?;
    let decoy_note = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 0)?;
    let base_note = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 1)?;
    let calls = read_method_calls(&view, read_owner)?;
    if calls.len() != 1 {
        return Err(TestError::Falsified("read owns one MethodCall"));
    }
    assert_local_index(calls[0], base_note, "self.note resolves to Base.note")?;
    if matches!(
        &calls[0].occurrence.target,
        OccurrenceTarget::Local(target) if target.raw == decoy_note
    ) {
        return Err(TestError::Falsified("self.note must not resolve to Decoy.note"));
    }
    fs::remove_dir_all(&env.work).map_err(|source| TestError::Io("remove scratch", source))?;
    Ok(())
}

#[test]
fn py_inherited_own_method_stays_on_child() -> Result<(), TestError> {
    const SOURCE: &[u8] = b"\
class Base:
    def note(self):
        return 1

class Child(Base):
    def note(self):
        return 2

    def read(self):
        return self.note()
";
    let env = inherited_fixture_env()?;
    let toolchain = env.toolchain()?;
    let cancelled = AtomicBool::new(false);
    let fragment = compile_inherited_fragment(SOURCE, &env.work, &toolchain, &cancelled)?;
    let view = fragment.view()?;
    let read_owner = entity_ordinal_at_index(&view, b"read", EntityKind::Function, 0)?;
    let child_note = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 1)?;
    let calls = read_method_calls(&view, read_owner)?;
    if calls.len() != 1 {
        return Err(TestError::Falsified("read owns one MethodCall"));
    }
    assert_local_index(calls[0], child_note, "self.note resolves to Child.note")?;
    fs::remove_dir_all(&env.work).map_err(|source| TestError::Io("remove scratch", source))?;
    Ok(())
}

#[test]
fn py_inherited_annotated_call_targets_base_note() -> Result<(), TestError> {
    const SOURCE: &[u8] = b"\
class Base:
    def note(self):
        return 1

class Child(Base):
    pass

def read(service: Child):
    return service.note()
";
    let env = inherited_fixture_env()?;
    let toolchain = env.toolchain()?;
    let cancelled = AtomicBool::new(false);
    let fragment = compile_inherited_fragment(SOURCE, &env.work, &toolchain, &cancelled)?;
    let view = fragment.view()?;
    let read_owner = entity_ordinal_at_index(&view, b"read", EntityKind::Function, 0)?;
    let base_note = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 0)?;
    let calls = read_method_calls(&view, read_owner)?;
    if calls.len() != 1 {
        return Err(TestError::Falsified("read owns one MethodCall"));
    }
    assert_local_index(calls[0], base_note, "service.note resolves to Base.note")?;
    fs::remove_dir_all(&env.work).map_err(|source| TestError::Io("remove scratch", source))?;
    Ok(())
}

#[test]
fn py_inherited_field_targets_base_score_not_decoy() -> Result<(), TestError> {
    const SOURCE: &[u8] = b"\
class Decoy:
    score = 9

class Base:
    score = 1

class Child(Base):
    def read(self):
        return self.score
";
    let env = inherited_fixture_env()?;
    let toolchain = env.toolchain()?;
    let cancelled = AtomicBool::new(false);
    let fragment = compile_inherited_fragment(SOURCE, &env.work, &toolchain, &cancelled)?;
    let view = fragment.view()?;
    let read_owner = entity_ordinal_at_index(&view, b"read", EntityKind::Function, 0)?;
    let base_score = entity_ordinal_at_index(&view, b"score", EntityKind::Field, 1)?;
    let calls = read_method_calls(&view, read_owner)?;
    if !calls.is_empty() {
        return Err(TestError::Falsified("read owns zero MethodCalls"));
    }
    let reads = read_field_accesses(&view, read_owner)?;
    if reads.len() != 1 {
        return Err(TestError::Falsified("read owns one FieldAccess"));
    }
    assert_local_index(reads[0], base_score, "self.score resolves to Base.score")?;
    fs::remove_dir_all(&env.work).map_err(|source| TestError::Io("remove scratch", source))?;
    Ok(())
}

#[test]
fn py_inherited_two_bases_stay_universe() -> Result<(), TestError> {
    const SOURCE: &[u8] = b"\
class Left:
    def note(self):
        return 1

class Right:
    def note(self):
        return 2

class Child(Left, Right):
    def read(self):
        return self.note()
";
    let env = inherited_fixture_env()?;
    let toolchain = env.toolchain()?;
    let cancelled = AtomicBool::new(false);
    let fragment = compile_inherited_fragment(SOURCE, &env.work, &toolchain, &cancelled)?;
    let view = fragment.view()?;
    let read_owner = entity_ordinal_at_index(&view, b"read", EntityKind::Function, 0)?;
    let left_note = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 0)?;
    let right_note = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 1)?;
    let calls = read_method_calls(&view, read_owner)?;
    if calls.len() != 1 {
        return Err(TestError::Falsified("read owns one MethodCall"));
    }
    assert_universe_method(calls[0], "self.note stays a pypi universe key")?;
    if matches!(&calls[0].occurrence.target, OccurrenceTarget::Local(target) if target.raw == left_note)
        || matches!(
            &calls[0].occurrence.target,
            OccurrenceTarget::Local(target) if target.raw == right_note
        )
    {
        return Err(TestError::Falsified("self.note must not bind either base note"));
    }
    fs::remove_dir_all(&env.work).map_err(|source| TestError::Io("remove scratch", source))?;
    Ok(())
}

#[test]
fn py_inherited_diamond_targets_root_note() -> Result<(), TestError> {
    const SOURCE: &[u8] = b"\
class Root:
    def note(self):
        return 1

class Left(Root):
    pass

class Right(Root):
    pass

class Child(Left, Right):
    def read(self):
        return self.note()
";
    let env = inherited_fixture_env()?;
    let toolchain = env.toolchain()?;
    let cancelled = AtomicBool::new(false);
    let fragment = compile_inherited_fragment(SOURCE, &env.work, &toolchain, &cancelled)?;
    let view = fragment.view()?;
    let read_owner = entity_ordinal_at_index(&view, b"read", EntityKind::Function, 0)?;
    let root_note = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 0)?;
    let calls = read_method_calls(&view, read_owner)?;
    if calls.len() != 1 {
        return Err(TestError::Falsified("read owns one MethodCall"));
    }
    assert_local_index(calls[0], root_note, "self.note resolves to Root.note")?;
    fs::remove_dir_all(&env.work).map_err(|source| TestError::Io("remove scratch", source))?;
    Ok(())
}

#[test]
fn py_inherited_one_of_two_bases_targets_that_note() -> Result<(), TestError> {
    const SOURCE: &[u8] = b"\
class Left:
    def note(self):
        return 1

class Right:
    pass

class Child(Left, Right):
    def read(self):
        return self.note()
";
    let env = inherited_fixture_env()?;
    let toolchain = env.toolchain()?;
    let cancelled = AtomicBool::new(false);
    let fragment = compile_inherited_fragment(SOURCE, &env.work, &toolchain, &cancelled)?;
    let view = fragment.view()?;
    let read_owner = entity_ordinal_at_index(&view, b"read", EntityKind::Function, 0)?;
    let left_note = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 0)?;
    let calls = read_method_calls(&view, read_owner)?;
    if calls.len() != 1 {
        return Err(TestError::Falsified("read owns one MethodCall"));
    }
    assert_local_index(calls[0], left_note, "self.note resolves to Left.note")?;
    fs::remove_dir_all(&env.work).map_err(|source| TestError::Io("remove scratch", source))?;
    Ok(())
}

#[test]
fn py_inherited_nearest_base_wins() -> Result<(), TestError> {
    const SOURCE: &[u8] = b"\
class Base:
    def note(self):
        return 1

class Mid(Base):
    def note(self):
        return 2

class Child(Mid):
    def read(self):
        return self.note()
";
    let env = inherited_fixture_env()?;
    let toolchain = env.toolchain()?;
    let cancelled = AtomicBool::new(false);
    let fragment = compile_inherited_fragment(SOURCE, &env.work, &toolchain, &cancelled)?;
    let view = fragment.view()?;
    let read_owner = entity_ordinal_at_index(&view, b"read", EntityKind::Function, 0)?;
    let mid_note = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 1)?;
    let calls = read_method_calls(&view, read_owner)?;
    if calls.len() != 1 {
        return Err(TestError::Falsified("read owns one MethodCall"));
    }
    assert_local_index(calls[0], mid_note, "self.note resolves to Mid.note")?;
    fs::remove_dir_all(&env.work).map_err(|source| TestError::Io("remove scratch", source))?;
    Ok(())
}

#[test]
fn py_inherited_grandchild_targets_base_note() -> Result<(), TestError> {
    const SOURCE: &[u8] = b"\
class Base:
    def note(self):
        return 1

class Mid(Base):
    pass

class Child(Mid):
    def read(self):
        return self.note()
";
    let env = inherited_fixture_env()?;
    let toolchain = env.toolchain()?;
    let cancelled = AtomicBool::new(false);
    let fragment = compile_inherited_fragment(SOURCE, &env.work, &toolchain, &cancelled)?;
    let view = fragment.view()?;
    let read_owner = entity_ordinal_at_index(&view, b"read", EntityKind::Function, 0)?;
    let base_note = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 0)?;
    let calls = read_method_calls(&view, read_owner)?;
    if calls.len() != 1 {
        return Err(TestError::Falsified("read owns one MethodCall"));
    }
    assert_local_index(calls[0], base_note, "self.note resolves to Base.note")?;
    fs::remove_dir_all(&env.work).map_err(|source| TestError::Io("remove scratch", source))?;
    Ok(())
}

#[test]
fn py_inherited_qualified_base_stays_universe() -> Result<(), TestError> {
    const SOURCE: &[u8] = b"\
class Base:
    def note(self):
        return 1

class Child(mod.Base):
    def read(self):
        return self.note()
";
    let env = inherited_fixture_env()?;
    let toolchain = env.toolchain()?;
    let cancelled = AtomicBool::new(false);
    let fragment = compile_inherited_fragment(SOURCE, &env.work, &toolchain, &cancelled)?;
    let view = fragment.view()?;
    let read_owner = entity_ordinal_at_index(&view, b"read", EntityKind::Function, 0)?;
    let base_note = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 0)?;
    let calls = read_method_calls(&view, read_owner)?;
    if calls.len() != 1 {
        return Err(TestError::Falsified("read owns one MethodCall"));
    }
    assert_universe_method(calls[0], "qualified base keeps a universe key")?;
    if matches!(
        &calls[0].occurrence.target,
        OccurrenceTarget::Local(target) if target.raw == base_note
    ) {
        return Err(TestError::Falsified("qualified base must not bind local Base.note"));
    }
    fs::remove_dir_all(&env.work).map_err(|source| TestError::Io("remove scratch", source))?;
    Ok(())
}

#[test]
fn py_inherited_quoted_base_stays_universe() -> Result<(), TestError> {
    const SOURCE: &[u8] = b"\
class Base:
    def note(self):
        return 1

class Child(\"Base\"):
    def read(self):
        return self.note()
";
    let env = inherited_fixture_env()?;
    let toolchain = env.toolchain()?;
    let cancelled = AtomicBool::new(false);
    let fragment = compile_inherited_fragment(SOURCE, &env.work, &toolchain, &cancelled)?;
    let view = fragment.view()?;
    let read_owner = entity_ordinal_at_index(&view, b"read", EntityKind::Function, 0)?;
    let base_note = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 0)?;
    let calls = read_method_calls(&view, read_owner)?;
    if calls.len() != 1 {
        return Err(TestError::Falsified("read owns one MethodCall"));
    }
    assert_universe_method(calls[0], "quoted base keeps a universe key")?;
    if matches!(
        &calls[0].occurrence.target,
        OccurrenceTarget::Local(target) if target.raw == base_note
    ) {
        return Err(TestError::Falsified("quoted base must not bind local Base.note"));
    }
    fs::remove_dir_all(&env.work).map_err(|source| TestError::Io("remove scratch", source))?;
    Ok(())
}

#[test]
fn py_inherited_generic_base_targets_base_note() -> Result<(), TestError> {
    const SOURCE: &[u8] = b"\
class Base:
    def note(self):
        return 1

class Child(Base[int]):
    def read(self):
        return self.note()
";
    let env = inherited_fixture_env()?;
    let toolchain = env.toolchain()?;
    let cancelled = AtomicBool::new(false);
    let fragment = compile_inherited_fragment(SOURCE, &env.work, &toolchain, &cancelled)?;
    let view = fragment.view()?;
    let read_owner = entity_ordinal_at_index(&view, b"read", EntityKind::Function, 0)?;
    let base_note = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 0)?;
    let calls = read_method_calls(&view, read_owner)?;
    if calls.len() != 1 {
        return Err(TestError::Falsified("read owns one MethodCall"));
    }
    assert_local_index(calls[0], base_note, "generic base resolves to Base.note")?;
    fs::remove_dir_all(&env.work).map_err(|source| TestError::Io("remove scratch", source))?;
    Ok(())
}

#[test]
fn py_inherited_cls_call_targets_base_note() -> Result<(), TestError> {
    const SOURCE: &[u8] = b"\
class Base:
    def note(self):
        return 1

class Child(Base):
    @classmethod
    def read(cls):
        return cls.note()
";
    let env = inherited_fixture_env()?;
    let toolchain = env.toolchain()?;
    let cancelled = AtomicBool::new(false);
    let fragment = compile_inherited_fragment(SOURCE, &env.work, &toolchain, &cancelled)?;
    let view = fragment.view()?;
    let read_owner = entity_ordinal_at_index(&view, b"read", EntityKind::Function, 0)?;
    let base_note = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 0)?;
    let calls = read_method_calls(&view, read_owner)?;
    if calls.len() != 1 {
        return Err(TestError::Falsified("read owns one MethodCall"));
    }
    assert_local_index(calls[0], base_note, "cls.note resolves to Base.note")?;
    fs::remove_dir_all(&env.work).map_err(|source| TestError::Io("remove scratch", source))?;
    Ok(())
}

#[test]
fn py_inherited_eighth_base_targets_note() -> Result<(), TestError> {
    const SOURCE: &[u8] = b"\
class Base:
    def note(self):
        return 1

class A1(Base):
    pass

class A2(A1):
    pass

class A3(A2):
    pass

class A4(A3):
    pass

class A5(A4):
    pass

class A6(A5):
    pass

class A7(A6):
    pass

class Child(A7):
    def read(self):
        return self.note()
";
    let env = inherited_fixture_env()?;
    let toolchain = env.toolchain()?;
    let cancelled = AtomicBool::new(false);
    let fragment = compile_inherited_fragment(SOURCE, &env.work, &toolchain, &cancelled)?;
    let view = fragment.view()?;
    let read_owner = entity_ordinal_at_index(&view, b"read", EntityKind::Function, 0)?;
    let base_note = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 0)?;
    let calls = read_method_calls(&view, read_owner)?;
    if calls.len() != 1 {
        return Err(TestError::Falsified("read owns one MethodCall"));
    }
    assert_local_index(calls[0], base_note, "eighth base link resolves to Base.note")?;
    fs::remove_dir_all(&env.work).map_err(|source| TestError::Io("remove scratch", source))?;
    Ok(())
}

#[test]
fn py_inherited_ninth_base_stays_universe() -> Result<(), TestError> {
    const SOURCE: &[u8] = b"\
class Base:
    def note(self):
        return 1

class A1(Base):
    pass

class A2(A1):
    pass

class A3(A2):
    pass

class A4(A3):
    pass

class A5(A4):
    pass

class A6(A5):
    pass

class A7(A6):
    pass

class A8(A7):
    pass

class Child(A8):
    def read(self):
        return self.note()
";
    let env = inherited_fixture_env()?;
    let toolchain = env.toolchain()?;
    let cancelled = AtomicBool::new(false);
    let fragment = compile_inherited_fragment(SOURCE, &env.work, &toolchain, &cancelled)?;
    let view = fragment.view()?;
    let read_owner = entity_ordinal_at_index(&view, b"read", EntityKind::Function, 0)?;
    let base_note = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 0)?;
    let calls = read_method_calls(&view, read_owner)?;
    if calls.len() != 1 {
        return Err(TestError::Falsified("read owns one MethodCall"));
    }
    assert_universe_method(calls[0], "ninth base link stays a universe key")?;
    if matches!(
        &calls[0].occurrence.target,
        OccurrenceTarget::Local(target) if target.raw == base_note
    ) {
        return Err(TestError::Falsified("ninth base link must not bind Base.note"));
    }
    fs::remove_dir_all(&env.work).map_err(|source| TestError::Io("remove scratch", source))?;
    Ok(())
}

#[test]
fn py_inherited_deep_sibling_does_not_starve_unique_base() -> Result<(), TestError> {
    const SOURCE: &[u8] = b"\
class Tail:
    pass

class A1(Tail):
    pass

class A2(A1):
    pass

class A3(A2):
    pass

class A4(A3):
    pass

class A5(A4):
    pass

class A6(A5):
    pass

class A7(A6):
    pass

class A8(A7):
    pass

class Left(A8):
    pass

class Right:
    def note(self):
        return 1

class Child(Left, Right):
    def read(self):
        return self.note()
";
    let env = inherited_fixture_env()?;
    let toolchain = env.toolchain()?;
    let cancelled = AtomicBool::new(false);
    let fragment = compile_inherited_fragment(SOURCE, &env.work, &toolchain, &cancelled)?;
    let view = fragment.view()?;
    let read_owner = entity_ordinal_at_index(&view, b"read", EntityKind::Function, 0)?;
    let right_note = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 0)?;
    let calls = read_method_calls(&view, read_owner)?;
    if calls.len() != 1 {
        return Err(TestError::Falsified("read owns one MethodCall"));
    }
    assert_local_index(calls[0], right_note, "self.note resolves to Right.note")?;
    fs::remove_dir_all(&env.work).map_err(|source| TestError::Io("remove scratch", source))?;
    Ok(())
}

#[test]
fn py_inherited_plain_receiver_stays_universe() -> Result<(), TestError> {
    const SOURCE: &[u8] = b"\
class Base:
    def note(self):
        return 1

def read(obj):
    return obj.note()
";
    let env = inherited_fixture_env()?;
    let toolchain = env.toolchain()?;
    let cancelled = AtomicBool::new(false);
    let fragment = compile_inherited_fragment(SOURCE, &env.work, &toolchain, &cancelled)?;
    let view = fragment.view()?;
    let read_owner = entity_ordinal_at_index(&view, b"read", EntityKind::Function, 0)?;
    let base_note = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 0)?;
    let calls = read_method_calls(&view, read_owner)?;
    if calls.len() != 1 {
        return Err(TestError::Falsified("read owns one MethodCall"));
    }
    assert_universe_method(calls[0], "plain receiver stays a universe key")?;
    if matches!(
        &calls[0].occurrence.target,
        OccurrenceTarget::Local(target) if target.raw == base_note
    ) {
        return Err(TestError::Falsified("plain receiver must not bind Base.note"));
    }
    fs::remove_dir_all(&env.work).map_err(|source| TestError::Io("remove scratch", source))?;
    Ok(())
}

#[test]
fn py_method_value_targets_child_note_not_decoy() -> Result<(), TestError> {
    const SOURCE: &[u8] = b"\
class Decoy:
    def note(self):
        return 0

class Child:
    def note(self):
        return 1

    def read(self):
        return self.note
";
    let env = inherited_fixture_env()?;
    let toolchain = env.toolchain()?;
    let cancelled = AtomicBool::new(false);
    let fragment = compile_inherited_fragment(SOURCE, &env.work, &toolchain, &cancelled)?;
    let view = fragment.view()?;
    let read_owner = entity_ordinal_at_index(&view, b"read", EntityKind::Function, 0)?;
    let decoy_note = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 0)?;
    let child_note = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 1)?;
    let calls = read_method_calls(&view, read_owner)?;
    if !calls.is_empty() {
        return Err(TestError::Falsified("read owns zero MethodCalls"));
    }
    let reads = read_field_accesses(&view, read_owner)?;
    if reads.len() != 1 {
        return Err(TestError::Falsified("read owns one FieldAccess"));
    }
    assert_local_index(reads[0], child_note, "self.note resolves to Child.note")?;
    if matches!(
        &reads[0].occurrence.target,
        OccurrenceTarget::Local(target) if target.raw == decoy_note
    ) {
        return Err(TestError::Falsified("self.note must not resolve to Decoy.note"));
    }
    fs::remove_dir_all(&env.work).map_err(|source| TestError::Io("remove scratch", source))?;
    Ok(())
}

#[test]
fn py_method_value_inherited_targets_base_note_not_decoy() -> Result<(), TestError> {
    const SOURCE: &[u8] = b"\
class Decoy:
    def note(self):
        return 0

class Base:
    def note(self):
        return 1

class Child(Base):
    def read(self):
        return self.note
";
    let env = inherited_fixture_env()?;
    let toolchain = env.toolchain()?;
    let cancelled = AtomicBool::new(false);
    let fragment = compile_inherited_fragment(SOURCE, &env.work, &toolchain, &cancelled)?;
    let view = fragment.view()?;
    let read_owner = entity_ordinal_at_index(&view, b"read", EntityKind::Function, 0)?;
    let decoy_note = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 0)?;
    let base_note = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 1)?;
    let calls = read_method_calls(&view, read_owner)?;
    if !calls.is_empty() {
        return Err(TestError::Falsified("read owns zero MethodCalls"));
    }
    let reads = read_field_accesses(&view, read_owner)?;
    if reads.len() != 1 {
        return Err(TestError::Falsified("read owns one FieldAccess"));
    }
    assert_local_index(reads[0], base_note, "self.note resolves to Base.note")?;
    if matches!(
        &reads[0].occurrence.target,
        OccurrenceTarget::Local(target) if target.raw == decoy_note
    ) {
        return Err(TestError::Falsified("self.note must not resolve to Decoy.note"));
    }
    fs::remove_dir_all(&env.work).map_err(|source| TestError::Io("remove scratch", source))?;
    Ok(())
}

#[test]
fn py_method_value_own_method_wins_over_base() -> Result<(), TestError> {
    const SOURCE: &[u8] = b"\
class Base:
    def note(self):
        return 1

class Child(Base):
    def note(self):
        return 2

    def read(self):
        return self.note
";
    let env = inherited_fixture_env()?;
    let toolchain = env.toolchain()?;
    let cancelled = AtomicBool::new(false);
    let fragment = compile_inherited_fragment(SOURCE, &env.work, &toolchain, &cancelled)?;
    let view = fragment.view()?;
    let read_owner = entity_ordinal_at_index(&view, b"read", EntityKind::Function, 0)?;
    let base_note = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 0)?;
    let child_note = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 1)?;
    let calls = read_method_calls(&view, read_owner)?;
    if !calls.is_empty() {
        return Err(TestError::Falsified("read owns zero MethodCalls"));
    }
    let reads = read_field_accesses(&view, read_owner)?;
    if reads.len() != 1 {
        return Err(TestError::Falsified("read owns one FieldAccess"));
    }
    assert_local_index(reads[0], child_note, "self.note resolves to Child.note")?;
    if matches!(
        &reads[0].occurrence.target,
        OccurrenceTarget::Local(target) if target.raw == base_note
    ) {
        return Err(TestError::Falsified("self.note must not resolve to Base.note"));
    }
    fs::remove_dir_all(&env.work).map_err(|source| TestError::Io("remove scratch", source))?;
    Ok(())
}

#[test]
fn py_method_value_field_wins_over_method() -> Result<(), TestError> {
    const SOURCE: &[u8] = b"\
class Child:
    note = 1

    def note(self):
        return self.note

    def read(self):
        return self.note
";
    let env = inherited_fixture_env()?;
    let toolchain = env.toolchain()?;
    let cancelled = AtomicBool::new(false);
    let fragment = compile_inherited_fragment(SOURCE, &env.work, &toolchain, &cancelled)?;
    let view = fragment.view()?;
    let read_owner = entity_ordinal_at_index(&view, b"read", EntityKind::Function, 0)?;
    let note_field = entity_ordinal_at_index(&view, b"note", EntityKind::Field, 0)?;
    let note_method = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 0)?;
    let calls = read_method_calls(&view, read_owner)?;
    if !calls.is_empty() {
        return Err(TestError::Falsified("read owns zero MethodCalls"));
    }
    let reads = read_field_accesses(&view, read_owner)?;
    if reads.len() != 1 {
        return Err(TestError::Falsified("read owns one FieldAccess"));
    }
    assert_local_index(reads[0], note_field, "self.note resolves to Child.note field")?;
    if matches!(
        &reads[0].occurrence.target,
        OccurrenceTarget::Local(target) if target.raw == note_method
    ) {
        return Err(TestError::Falsified("self.note must not resolve to Child.note method"));
    }
    fs::remove_dir_all(&env.work).map_err(|source| TestError::Io("remove scratch", source))?;
    Ok(())
}

#[test]
fn py_method_value_two_bases_stay_universe() -> Result<(), TestError> {
    const SOURCE: &[u8] = b"\
class Left:
    def note(self):
        return 1

class Right:
    def note(self):
        return 2

class Child(Left, Right):
    def read(self):
        return self.note
";
    let env = inherited_fixture_env()?;
    let toolchain = env.toolchain()?;
    let cancelled = AtomicBool::new(false);
    let fragment = compile_inherited_fragment(SOURCE, &env.work, &toolchain, &cancelled)?;
    let view = fragment.view()?;
    let read_owner = entity_ordinal_at_index(&view, b"read", EntityKind::Function, 0)?;
    let left_note = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 0)?;
    let right_note = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 1)?;
    let calls = read_method_calls(&view, read_owner)?;
    if !calls.is_empty() {
        return Err(TestError::Falsified("read owns zero MethodCalls"));
    }
    let reads = read_field_accesses(&view, read_owner)?;
    if reads.len() != 1 {
        return Err(TestError::Falsified("read owns one FieldAccess"));
    }
    assert_universe_field(reads[0], "self.note stays a pypi universe field key")?;
    if matches!(
        &reads[0].occurrence.target,
        OccurrenceTarget::Local(target) if target.raw == left_note
    ) {
        return Err(TestError::Falsified("self.note must not bind Left.note"));
    }
    if matches!(
        &reads[0].occurrence.target,
        OccurrenceTarget::Local(target) if target.raw == right_note
    ) {
        return Err(TestError::Falsified("self.note must not bind Right.note"));
    }
    fs::remove_dir_all(&env.work).map_err(|source| TestError::Io("remove scratch", source))?;
    Ok(())
}

#[test]
fn py_method_value_call_stays_method_call() -> Result<(), TestError> {
    const SOURCE: &[u8] = b"\
class Child:
    def note(self):
        return 1

    def read(self):
        return self.note()
";
    let env = inherited_fixture_env()?;
    let toolchain = env.toolchain()?;
    let cancelled = AtomicBool::new(false);
    let fragment = compile_inherited_fragment(SOURCE, &env.work, &toolchain, &cancelled)?;
    let view = fragment.view()?;
    let read_owner = entity_ordinal_at_index(&view, b"read", EntityKind::Function, 0)?;
    let child_note = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 0)?;
    let calls = read_method_calls(&view, read_owner)?;
    if calls.len() != 1 {
        return Err(TestError::Falsified("read owns one MethodCall"));
    }
    assert_local_index(calls[0], child_note, "self.note() resolves to Child.note")?;
    let reads = read_field_accesses(&view, read_owner)?;
    if !reads.is_empty() {
        return Err(TestError::Falsified("read owns zero FieldAccess rows"));
    }
    fs::remove_dir_all(&env.work).map_err(|source| TestError::Io("remove scratch", source))?;
    Ok(())
}

#[test]
fn py_method_value_cls_targets_child_note() -> Result<(), TestError> {
    const SOURCE: &[u8] = b"\
class Child:
    def note(self):
        return 1

    @classmethod
    def read(cls):
        return cls.note
";
    let env = inherited_fixture_env()?;
    let toolchain = env.toolchain()?;
    let cancelled = AtomicBool::new(false);
    let fragment = compile_inherited_fragment(SOURCE, &env.work, &toolchain, &cancelled)?;
    let view = fragment.view()?;
    let read_owner = entity_ordinal_at_index(&view, b"read", EntityKind::Function, 0)?;
    let child_note = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 0)?;
    let calls = read_method_calls(&view, read_owner)?;
    if !calls.is_empty() {
        return Err(TestError::Falsified("read owns zero MethodCalls"));
    }
    let reads = read_field_accesses(&view, read_owner)?;
    if reads.len() != 1 {
        return Err(TestError::Falsified("read owns one FieldAccess"));
    }
    assert_local_index(reads[0], child_note, "cls.note resolves to Child.note")?;
    fs::remove_dir_all(&env.work).map_err(|source| TestError::Io("remove scratch", source))?;
    Ok(())
}

#[test]
fn py_method_value_plain_stays_universe() -> Result<(), TestError> {
    const SOURCE: &[u8] = b"\
class Child:
    def note(self):
        return 1

def read(obj):
    return obj.note
";
    let env = inherited_fixture_env()?;
    let toolchain = env.toolchain()?;
    let cancelled = AtomicBool::new(false);
    let fragment = compile_inherited_fragment(SOURCE, &env.work, &toolchain, &cancelled)?;
    let view = fragment.view()?;
    let read_owner = entity_ordinal_at_index(&view, b"read", EntityKind::Function, 0)?;
    let child_note = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 0)?;
    let calls = read_method_calls(&view, read_owner)?;
    if !calls.is_empty() {
        return Err(TestError::Falsified("read owns zero MethodCalls"));
    }
    let reads = read_field_accesses(&view, read_owner)?;
    if reads.len() != 1 {
        return Err(TestError::Falsified("read owns one FieldAccess"));
    }
    assert_universe_field(reads[0], "plain receiver stays a universe field key")?;
    if matches!(
        &reads[0].occurrence.target,
        OccurrenceTarget::Local(target) if target.raw == child_note
    ) {
        return Err(TestError::Falsified("plain receiver must not bind Child.note"));
    }
    fs::remove_dir_all(&env.work).map_err(|source| TestError::Io("remove scratch", source))?;
    Ok(())
}

#[test]
fn py_method_value_nearest_base_wins() -> Result<(), TestError> {
    const SOURCE: &[u8] = b"\
class Base:
    def note(self):
        return 1

class Mid(Base):
    def note(self):
        return 2

class Child(Mid):
    def read(self):
        return self.note
";
    let env = inherited_fixture_env()?;
    let toolchain = env.toolchain()?;
    let cancelled = AtomicBool::new(false);
    let fragment = compile_inherited_fragment(SOURCE, &env.work, &toolchain, &cancelled)?;
    let view = fragment.view()?;
    let read_owner = entity_ordinal_at_index(&view, b"read", EntityKind::Function, 0)?;
    let base_note = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 0)?;
    let mid_note = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 1)?;
    let calls = read_method_calls(&view, read_owner)?;
    if !calls.is_empty() {
        return Err(TestError::Falsified("read owns zero MethodCalls"));
    }
    let reads = read_field_accesses(&view, read_owner)?;
    if reads.len() != 1 {
        return Err(TestError::Falsified("read owns one FieldAccess"));
    }
    assert_local_index(reads[0], mid_note, "self.note resolves to Mid.note")?;
    if matches!(
        &reads[0].occurrence.target,
        OccurrenceTarget::Local(target) if target.raw == base_note
    ) {
        return Err(TestError::Falsified("self.note must not resolve to Base.note"));
    }
    fs::remove_dir_all(&env.work).map_err(|source| TestError::Io("remove scratch", source))?;
    Ok(())
}

#[test]
fn py_annotated_read_method_targets_child_note_not_decoy() -> Result<(), TestError> {
    const SOURCE: &[u8] = b"\
class Decoy:
    def note(self):
        return 0

class Child:
    def note(self):
        return 1

def read(service: Child):
    return service.note
";
    let env = inherited_fixture_env()?;
    let toolchain = env.toolchain()?;
    let cancelled = AtomicBool::new(false);
    let fragment = compile_inherited_fragment(SOURCE, &env.work, &toolchain, &cancelled)?;
    let view = fragment.view()?;
    let read_owner = entity_ordinal_at_index(&view, b"read", EntityKind::Function, 0)?;
    let decoy_note = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 0)?;
    let child_note = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 1)?;
    let calls = read_method_calls(&view, read_owner)?;
    if !calls.is_empty() {
        return Err(TestError::Falsified("read owns zero MethodCalls"));
    }
    let reads = read_field_accesses(&view, read_owner)?;
    if reads.len() != 1 {
        return Err(TestError::Falsified("read owns one FieldAccess"));
    }
    assert_local_index(reads[0], child_note, "service.note resolves to Child.note")?;
    if matches!(
        &reads[0].occurrence.target,
        OccurrenceTarget::Local(target) if target.raw == decoy_note
    ) {
        return Err(TestError::Falsified("service.note must not resolve to Decoy.note"));
    }
    fs::remove_dir_all(&env.work).map_err(|source| TestError::Io("remove scratch", source))?;
    Ok(())
}

#[test]
fn py_annotated_read_inherited_method_targets_base_note() -> Result<(), TestError> {
    const SOURCE: &[u8] = b"\
class Base:
    def note(self):
        return 1

class Child(Base):
    pass

def read(service: Child):
    return service.note
";
    let env = inherited_fixture_env()?;
    let toolchain = env.toolchain()?;
    let cancelled = AtomicBool::new(false);
    let fragment = compile_inherited_fragment(SOURCE, &env.work, &toolchain, &cancelled)?;
    let view = fragment.view()?;
    let read_owner = entity_ordinal_at_index(&view, b"read", EntityKind::Function, 0)?;
    let base_note = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 0)?;
    let calls = read_method_calls(&view, read_owner)?;
    if !calls.is_empty() {
        return Err(TestError::Falsified("read owns zero MethodCalls"));
    }
    let reads = read_field_accesses(&view, read_owner)?;
    if reads.len() != 1 {
        return Err(TestError::Falsified("read owns one FieldAccess"));
    }
    assert_local_index(reads[0], base_note, "service.note resolves to Base.note")?;
    fs::remove_dir_all(&env.work).map_err(|source| TestError::Io("remove scratch", source))?;
    Ok(())
}

#[test]
fn py_annotated_read_field_targets_child_score_not_decoy() -> Result<(), TestError> {
    const SOURCE: &[u8] = b"\
class Decoy:
    score = 9

class Child:
    score = 1

def read(service: Child):
    return service.score
";
    let env = inherited_fixture_env()?;
    let toolchain = env.toolchain()?;
    let cancelled = AtomicBool::new(false);
    let fragment = compile_inherited_fragment(SOURCE, &env.work, &toolchain, &cancelled)?;
    let view = fragment.view()?;
    let read_owner = entity_ordinal_at_index(&view, b"read", EntityKind::Function, 0)?;
    let decoy_score = entity_ordinal_at_index(&view, b"score", EntityKind::Field, 0)?;
    let child_score = entity_ordinal_at_index(&view, b"score", EntityKind::Field, 1)?;
    let calls = read_method_calls(&view, read_owner)?;
    if !calls.is_empty() {
        return Err(TestError::Falsified("read owns zero MethodCalls"));
    }
    let reads = read_field_accesses(&view, read_owner)?;
    if reads.len() != 1 {
        return Err(TestError::Falsified("read owns one FieldAccess"));
    }
    assert_local_index(reads[0], child_score, "service.score resolves to Child.score")?;
    if matches!(
        &reads[0].occurrence.target,
        OccurrenceTarget::Local(target) if target.raw == decoy_score
    ) {
        return Err(TestError::Falsified("service.score must not resolve to Decoy.score"));
    }
    fs::remove_dir_all(&env.work).map_err(|source| TestError::Io("remove scratch", source))?;
    Ok(())
}

#[test]
fn py_annotated_read_own_method_wins_over_base() -> Result<(), TestError> {
    const SOURCE: &[u8] = b"\
class Base:
    def note(self):
        return 1

class Child(Base):
    def note(self):
        return 2

def read(service: Child):
    return service.note
";
    let env = inherited_fixture_env()?;
    let toolchain = env.toolchain()?;
    let cancelled = AtomicBool::new(false);
    let fragment = compile_inherited_fragment(SOURCE, &env.work, &toolchain, &cancelled)?;
    let view = fragment.view()?;
    let read_owner = entity_ordinal_at_index(&view, b"read", EntityKind::Function, 0)?;
    let base_note = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 0)?;
    let child_note = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 1)?;
    let calls = read_method_calls(&view, read_owner)?;
    if !calls.is_empty() {
        return Err(TestError::Falsified("read owns zero MethodCalls"));
    }
    let reads = read_field_accesses(&view, read_owner)?;
    if reads.len() != 1 {
        return Err(TestError::Falsified("read owns one FieldAccess"));
    }
    assert_local_index(reads[0], child_note, "service.note resolves to Child.note")?;
    if matches!(
        &reads[0].occurrence.target,
        OccurrenceTarget::Local(target) if target.raw == base_note
    ) {
        return Err(TestError::Falsified("service.note must not resolve to Base.note"));
    }
    fs::remove_dir_all(&env.work).map_err(|source| TestError::Io("remove scratch", source))?;
    Ok(())
}

#[test]
fn py_annotated_read_field_wins_over_method() -> Result<(), TestError> {
    const SOURCE: &[u8] = b"\
class Child:
    note = 1

    def note(self):
        return self.note

def read(service: Child):
    return service.note
";
    let env = inherited_fixture_env()?;
    let toolchain = env.toolchain()?;
    let cancelled = AtomicBool::new(false);
    let fragment = compile_inherited_fragment(SOURCE, &env.work, &toolchain, &cancelled)?;
    let view = fragment.view()?;
    let read_owner = entity_ordinal_at_index(&view, b"read", EntityKind::Function, 0)?;
    let note_field = entity_ordinal_at_index(&view, b"note", EntityKind::Field, 0)?;
    let note_method = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 0)?;
    let calls = read_method_calls(&view, read_owner)?;
    if !calls.is_empty() {
        return Err(TestError::Falsified("read owns zero MethodCalls"));
    }
    let reads = read_field_accesses(&view, read_owner)?;
    if reads.len() != 1 {
        return Err(TestError::Falsified("read owns one FieldAccess"));
    }
    assert_local_index(reads[0], note_field, "service.note resolves to Child.note field")?;
    if matches!(
        &reads[0].occurrence.target,
        OccurrenceTarget::Local(target) if target.raw == note_method
    ) {
        return Err(TestError::Falsified("service.note must not resolve to Child.note method"));
    }
    fs::remove_dir_all(&env.work).map_err(|source| TestError::Io("remove scratch", source))?;
    Ok(())
}

#[test]
fn py_annotated_read_two_bases_stay_universe() -> Result<(), TestError> {
    const SOURCE: &[u8] = b"\
class Left:
    def note(self):
        return 1

class Right:
    def note(self):
        return 2

class Child(Left, Right):
    pass

def read(service: Child):
    return service.note
";
    let env = inherited_fixture_env()?;
    let toolchain = env.toolchain()?;
    let cancelled = AtomicBool::new(false);
    let fragment = compile_inherited_fragment(SOURCE, &env.work, &toolchain, &cancelled)?;
    let view = fragment.view()?;
    let read_owner = entity_ordinal_at_index(&view, b"read", EntityKind::Function, 0)?;
    let left_note = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 0)?;
    let right_note = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 1)?;
    let calls = read_method_calls(&view, read_owner)?;
    if !calls.is_empty() {
        return Err(TestError::Falsified("read owns zero MethodCalls"));
    }
    let reads = read_field_accesses(&view, read_owner)?;
    if reads.len() != 1 {
        return Err(TestError::Falsified("read owns one FieldAccess"));
    }
    assert_universe_field(reads[0], "service.note stays a pypi universe field key")?;
    if reads[0].occurrence.confidence != OccurrenceConfidence::Index {
        return Err(TestError::Falsified("service.note universe field confidence is Index"));
    }
    if matches!(
        &reads[0].occurrence.target,
        OccurrenceTarget::Local(target) if target.raw == left_note
    ) {
        return Err(TestError::Falsified("service.note must not bind Left.note"));
    }
    if matches!(
        &reads[0].occurrence.target,
        OccurrenceTarget::Local(target) if target.raw == right_note
    ) {
        return Err(TestError::Falsified("service.note must not bind Right.note"));
    }
    fs::remove_dir_all(&env.work).map_err(|source| TestError::Io("remove scratch", source))?;
    Ok(())
}

#[test]
fn py_annotated_read_plain_stays_universe() -> Result<(), TestError> {
    const SOURCE: &[u8] = b"\
class Child:
    def note(self):
        return 1

def read(obj):
    return obj.note
";
    let env = inherited_fixture_env()?;
    let toolchain = env.toolchain()?;
    let cancelled = AtomicBool::new(false);
    let fragment = compile_inherited_fragment(SOURCE, &env.work, &toolchain, &cancelled)?;
    let view = fragment.view()?;
    let read_owner = entity_ordinal_at_index(&view, b"read", EntityKind::Function, 0)?;
    let child_note = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 0)?;
    let calls = read_method_calls(&view, read_owner)?;
    if !calls.is_empty() {
        return Err(TestError::Falsified("read owns zero MethodCalls"));
    }
    let reads = read_field_accesses(&view, read_owner)?;
    if reads.len() != 1 {
        return Err(TestError::Falsified("read owns one FieldAccess"));
    }
    assert_universe_field(reads[0], "plain receiver stays a universe field key")?;
    if reads[0].occurrence.confidence != OccurrenceConfidence::Index {
        return Err(TestError::Falsified("plain receiver universe field confidence is Index"));
    }
    if matches!(
        &reads[0].occurrence.target,
        OccurrenceTarget::Local(target) if target.raw == child_note
    ) {
        return Err(TestError::Falsified("plain receiver must not bind Child.note"));
    }
    fs::remove_dir_all(&env.work).map_err(|source| TestError::Io("remove scratch", source))?;
    Ok(())
}

#[test]
fn py_annotated_read_call_stays_method_call() -> Result<(), TestError> {
    const SOURCE: &[u8] = b"\
class Child:
    def note(self):
        return 1

def read(service: Child):
    return service.note()
";
    let env = inherited_fixture_env()?;
    let toolchain = env.toolchain()?;
    let cancelled = AtomicBool::new(false);
    let fragment = compile_inherited_fragment(SOURCE, &env.work, &toolchain, &cancelled)?;
    let view = fragment.view()?;
    let read_owner = entity_ordinal_at_index(&view, b"read", EntityKind::Function, 0)?;
    let child_note = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 0)?;
    let calls = read_method_calls(&view, read_owner)?;
    if calls.len() != 1 {
        return Err(TestError::Falsified("read owns one MethodCall"));
    }
    assert_local_index(calls[0], child_note, "service.note() resolves to Child.note")?;
    let reads = read_field_accesses(&view, read_owner)?;
    if !reads.is_empty() {
        return Err(TestError::Falsified("read owns zero FieldAccess rows"));
    }
    fs::remove_dir_all(&env.work).map_err(|source| TestError::Io("remove scratch", source))?;
    Ok(())
}

#[test]
fn py_annotated_read_alias_field_uses_package_key() -> Result<(), TestError> {
    const SOURCE: &[u8] = b"\
from workout.service import WorkoutService

def read(service: WorkoutService):
    return service.score
";
    let env = inherited_fixture_env()?;
    let toolchain = env.toolchain()?;
    let cancelled = AtomicBool::new(false);
    let fragment = compile_inherited_fragment(SOURCE, &env.work, &toolchain, &cancelled)?;
    let view = fragment.view()?;
    let read_owner = entity_ordinal_at_index(&view, b"read", EntityKind::Function, 0)?;
    let calls = read_method_calls(&view, read_owner)?;
    if !calls.is_empty() {
        return Err(TestError::Falsified("read owns zero MethodCalls"));
    }
    let reads = read_field_accesses(&view, read_owner)?;
    if reads.len() != 1 {
        return Err(TestError::Falsified("read owns one FieldAccess"));
    }
    if !matches!(
        &reads[0].occurrence.target,
        OccurrenceTarget::Foreign(key)
            if key.path == "workout.service"
                && key.display == "score"
                && key.kind == Some(EntityKind::Field)
                && matches!(
                    key.origin,
                    ForeignOrigin::Package(lineage) if lineage.name == "workout"
                )
    ) {
        return Err(TestError::Falsified(
            "service.score is not a workout.service package field key",
        ));
    }
    if reads[0].occurrence.confidence != OccurrenceConfidence::Index {
        return Err(TestError::Falsified("service.score package field confidence is Index"));
    }
    if matches!(&reads[0].occurrence.target, OccurrenceTarget::Local(_)) {
        return Err(TestError::Falsified("service.score must not resolve locally"));
    }
    fs::remove_dir_all(&env.work).map_err(|source| TestError::Io("remove scratch", source))?;
    Ok(())
}

#[test]
fn py_super_call_targets_base_note_not_child_or_other() -> Result<(), TestError> {
    const SOURCE: &[u8] = b"\
class Other:
    def note(self):
        return 0

class Base:
    def note(self):
        return 1

class Child(Base):
    def note(self):
        return 2

    def read(self):
        return super().note()
";
    let env = inherited_fixture_env()?;
    let toolchain = env.toolchain()?;
    let cancelled = AtomicBool::new(false);
    let fragment = compile_inherited_fragment(SOURCE, &env.work, &toolchain, &cancelled)?;
    let view = fragment.view()?;
    let read_owner = entity_ordinal_at_index(&view, b"read", EntityKind::Function, 0)?;
    let other_note = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 0)?;
    let base_note = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 1)?;
    let child_note = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 2)?;
    let calls = read_method_calls(&view, read_owner)?;
    if calls.len() != 1 {
        return Err(TestError::Falsified("read owns one MethodCall"));
    }
    assert_local_index(calls[0], base_note, "super().note resolves to Base.note")?;
    if matches!(
        &calls[0].occurrence.target,
        OccurrenceTarget::Local(target) if target.raw == other_note
    ) {
        return Err(TestError::Falsified("super().note must not resolve to Other.note"));
    }
    if matches!(
        &calls[0].occurrence.target,
        OccurrenceTarget::Local(target) if target.raw == child_note
    ) {
        return Err(TestError::Falsified("super().note must not resolve to Child.note"));
    }
    fs::remove_dir_all(&env.work).map_err(|source| TestError::Io("remove scratch", source))?;
    Ok(())
}

#[test]
fn py_super_call_targets_only_base_note() -> Result<(), TestError> {
    const SOURCE: &[u8] = b"\
class Base:
    def note(self):
        return 1

class Child(Base):
    def read(self):
        return super().note()
";
    let env = inherited_fixture_env()?;
    let toolchain = env.toolchain()?;
    let cancelled = AtomicBool::new(false);
    let fragment = compile_inherited_fragment(SOURCE, &env.work, &toolchain, &cancelled)?;
    let view = fragment.view()?;
    let read_owner = entity_ordinal_at_index(&view, b"read", EntityKind::Function, 0)?;
    let base_note = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 0)?;
    let calls = read_method_calls(&view, read_owner)?;
    if calls.len() != 1 {
        return Err(TestError::Falsified("read owns one MethodCall"));
    }
    assert_local_index(calls[0], base_note, "super().note resolves to Base.note")?;
    fs::remove_dir_all(&env.work).map_err(|source| TestError::Io("remove scratch", source))?;
    Ok(())
}

#[test]
fn py_super_field_targets_base_score_not_child() -> Result<(), TestError> {
    const SOURCE: &[u8] = b"\
class Base:
    score = 1

class Child(Base):
    score = 2

    def read(self):
        return super().score
";
    let env = inherited_fixture_env()?;
    let toolchain = env.toolchain()?;
    let cancelled = AtomicBool::new(false);
    let fragment = compile_inherited_fragment(SOURCE, &env.work, &toolchain, &cancelled)?;
    let view = fragment.view()?;
    let read_owner = entity_ordinal_at_index(&view, b"read", EntityKind::Function, 0)?;
    let base_score = entity_ordinal_at_index(&view, b"score", EntityKind::Field, 0)?;
    let child_score = entity_ordinal_at_index(&view, b"score", EntityKind::Field, 1)?;
    let calls = read_method_calls(&view, read_owner)?;
    if !calls.is_empty() {
        return Err(TestError::Falsified("read owns zero MethodCalls"));
    }
    let reads = read_field_accesses(&view, read_owner)?;
    if reads.len() != 1 {
        return Err(TestError::Falsified("read owns one FieldAccess"));
    }
    assert_local_index(reads[0], base_score, "super().score resolves to Base.score")?;
    if matches!(
        &reads[0].occurrence.target,
        OccurrenceTarget::Local(target) if target.raw == child_score
    ) {
        return Err(TestError::Falsified("super().score must not resolve to Child.score"));
    }
    fs::remove_dir_all(&env.work).map_err(|source| TestError::Io("remove scratch", source))?;
    Ok(())
}

#[test]
fn py_super_method_value_targets_base_note_not_child() -> Result<(), TestError> {
    const SOURCE: &[u8] = b"\
class Base:
    def note(self):
        return 1

class Child(Base):
    def note(self):
        return 2

    def read(self):
        return super().note
";
    let env = inherited_fixture_env()?;
    let toolchain = env.toolchain()?;
    let cancelled = AtomicBool::new(false);
    let fragment = compile_inherited_fragment(SOURCE, &env.work, &toolchain, &cancelled)?;
    let view = fragment.view()?;
    let read_owner = entity_ordinal_at_index(&view, b"read", EntityKind::Function, 0)?;
    let base_note = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 0)?;
    let child_note = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 1)?;
    let calls = read_method_calls(&view, read_owner)?;
    if !calls.is_empty() {
        return Err(TestError::Falsified("read owns zero MethodCalls"));
    }
    let reads = read_field_accesses(&view, read_owner)?;
    if reads.len() != 1 {
        return Err(TestError::Falsified("read owns one FieldAccess"));
    }
    assert_local_index(reads[0], base_note, "super().note resolves to Base.note")?;
    if matches!(
        &reads[0].occurrence.target,
        OccurrenceTarget::Local(target) if target.raw == child_note
    ) {
        return Err(TestError::Falsified("super().note must not resolve to Child.note"));
    }
    fs::remove_dir_all(&env.work).map_err(|source| TestError::Io("remove scratch", source))?;
    Ok(())
}

#[test]
fn py_super_two_arg_stays_universe() -> Result<(), TestError> {
    const SOURCE: &[u8] = b"\
class Base:
    def note(self):
        return 1

class Child(Base):
    def note(self):
        return 2

    def read(self):
        return super(Base, self).note()
";
    let env = inherited_fixture_env()?;
    let toolchain = env.toolchain()?;
    let cancelled = AtomicBool::new(false);
    let fragment = compile_inherited_fragment(SOURCE, &env.work, &toolchain, &cancelled)?;
    let view = fragment.view()?;
    let read_owner = entity_ordinal_at_index(&view, b"read", EntityKind::Function, 0)?;
    let base_note = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 0)?;
    let child_note = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 1)?;
    let calls = read_method_calls(&view, read_owner)?;
    if calls.len() != 1 {
        return Err(TestError::Falsified("read owns one MethodCall"));
    }
    assert_universe_method(calls[0], "two-arg super stays a universe key")?;
    if calls[0].occurrence.confidence != OccurrenceConfidence::Index {
        return Err(TestError::Falsified("two-arg super confidence is Index"));
    }
    if matches!(
        &calls[0].occurrence.target,
        OccurrenceTarget::Local(target) if target.raw == base_note
    ) {
        return Err(TestError::Falsified("two-arg super must not bind Base.note"));
    }
    if matches!(
        &calls[0].occurrence.target,
        OccurrenceTarget::Local(target) if target.raw == child_note
    ) {
        return Err(TestError::Falsified("two-arg super must not bind Child.note"));
    }
    fs::remove_dir_all(&env.work).map_err(|source| TestError::Io("remove scratch", source))?;
    Ok(())
}

#[test]
fn py_super_two_bases_targets_left_note_not_right() -> Result<(), TestError> {
    const SOURCE: &[u8] = b"\
class Left:
    def note(self):
        return 1

class Right:
    def note(self):
        return 2

class Child(Left, Right):
    def read(self):
        return super().note()
";
    let env = inherited_fixture_env()?;
    let toolchain = env.toolchain()?;
    let cancelled = AtomicBool::new(false);
    let fragment = compile_inherited_fragment(SOURCE, &env.work, &toolchain, &cancelled)?;
    let view = fragment.view()?;
    let read_owner = entity_ordinal_at_index(&view, b"read", EntityKind::Function, 0)?;
    let left_note = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 0)?;
    let right_note = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 1)?;
    let calls = read_method_calls(&view, read_owner)?;
    if calls.len() != 1 {
        return Err(TestError::Falsified("read owns one MethodCall"));
    }
    assert_local_index(calls[0], left_note, "super().note resolves to Left.note")?;
    if matches!(
        &calls[0].occurrence.target,
        OccurrenceTarget::Local(target) if target.raw == right_note
    ) {
        return Err(TestError::Falsified("super().note must not resolve to Right.note"));
    }
    fs::remove_dir_all(&env.work).map_err(|source| TestError::Io("remove scratch", source))?;
    Ok(())
}

#[test]
fn py_super_c3_targets_next_note_not_root() -> Result<(), TestError> {
    const SOURCE: &[u8] = b"\
class A:
    def note(self):
        return 1

class B(A):
    pass

class C(A):
    def note(self):
        return 2

class D(B, C):
    def read(self):
        return super().note()
";
    let env = inherited_fixture_env()?;
    let toolchain = env.toolchain()?;
    let cancelled = AtomicBool::new(false);
    let fragment = compile_inherited_fragment(SOURCE, &env.work, &toolchain, &cancelled)?;
    let view = fragment.view()?;
    let read_owner = entity_ordinal_at_index(&view, b"read", EntityKind::Function, 0)?;
    let root_note = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 0)?;
    let next_note = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 1)?;
    let calls = read_method_calls(&view, read_owner)?;
    if calls.len() != 1 {
        return Err(TestError::Falsified("read owns one MethodCall"));
    }
    assert_local_index(calls[0], next_note, "super().note resolves to C.note")?;
    if matches!(
        &calls[0].occurrence.target,
        OccurrenceTarget::Local(target) if target.raw == root_note
    ) {
        return Err(TestError::Falsified("super().note must not resolve to A.note"));
    }
    fs::remove_dir_all(&env.work).map_err(|source| TestError::Io("remove scratch", source))?;
    Ok(())
}

#[test]
fn py_super_module_call_stays_universe() -> Result<(), TestError> {
    const SOURCE: &[u8] = b"\
class Base:
    def note(self):
        return 1

def read():
    return super().note()
";
    let env = inherited_fixture_env()?;
    let toolchain = env.toolchain()?;
    let cancelled = AtomicBool::new(false);
    let fragment = compile_inherited_fragment(SOURCE, &env.work, &toolchain, &cancelled)?;
    let view = fragment.view()?;
    let read_owner = entity_ordinal_at_index(&view, b"read", EntityKind::Function, 0)?;
    let base_note = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 0)?;
    let calls = read_method_calls(&view, read_owner)?;
    if calls.len() != 1 {
        return Err(TestError::Falsified("read owns one MethodCall"));
    }
    assert_universe_method(calls[0], "module-level super stays a universe key")?;
    if calls[0].occurrence.confidence != OccurrenceConfidence::Index {
        return Err(TestError::Falsified("module-level super confidence is Index"));
    }
    if matches!(
        &calls[0].occurrence.target,
        OccurrenceTarget::Local(target) if target.raw == base_note
    ) {
        return Err(TestError::Falsified("module-level super must not bind Base.note"));
    }
    fs::remove_dir_all(&env.work).map_err(|source| TestError::Io("remove scratch", source))?;
    Ok(())
}

#[test]
fn py_super_nested_function_stays_universe() -> Result<(), TestError> {
    const SOURCE: &[u8] = b"\
class Base:
    def note(self):
        return 1

class Child(Base):
    def read(self):
        def inner():
            return super().note()
        return inner()
";
    let env = inherited_fixture_env()?;
    let toolchain = env.toolchain()?;
    let cancelled = AtomicBool::new(false);
    let fragment = compile_inherited_fragment(SOURCE, &env.work, &toolchain, &cancelled)?;
    let view = fragment.view()?;
    let read_owner = entity_ordinal_at_index(&view, b"read", EntityKind::Function, 0)?;
    let inner_owner = entity_ordinal_at_index(&view, b"inner", EntityKind::Function, 0)?;
    let base_note = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 0)?;
    let read_calls = read_method_calls(&view, read_owner)?;
    if !read_calls.is_empty() {
        for call in &read_calls {
            if matches!(
                &call.occurrence.target,
                OccurrenceTarget::Local(target) if target.raw == base_note
            ) {
                return Err(TestError::Falsified("read must not bind Base.note"));
            }
        }
    }
    let inner_calls = read_method_calls(&view, inner_owner)?;
    if inner_calls.len() != 1 {
        return Err(TestError::Falsified("inner owns one MethodCall"));
    }
    assert_universe_method(inner_calls[0], "nested super stays a universe key")?;
    if inner_calls[0].occurrence.confidence != OccurrenceConfidence::Index {
        return Err(TestError::Falsified("nested super confidence is Index"));
    }
    if matches!(
        &inner_calls[0].occurrence.target,
        OccurrenceTarget::Local(target) if target.raw == base_note
    ) {
        return Err(TestError::Falsified("nested super must not bind Base.note"));
    }
    fs::remove_dir_all(&env.work).map_err(|source| TestError::Io("remove scratch", source))?;
    Ok(())
}

#[test]
fn py_super_name_stays_universe() -> Result<(), TestError> {
    const SOURCE: &[u8] = b"\
class Base:
    def note(self):
        return 1

class Child(Base):
    def read(self):
        return super.note()
";
    let env = inherited_fixture_env()?;
    let toolchain = env.toolchain()?;
    let cancelled = AtomicBool::new(false);
    let fragment = compile_inherited_fragment(SOURCE, &env.work, &toolchain, &cancelled)?;
    let view = fragment.view()?;
    let read_owner = entity_ordinal_at_index(&view, b"read", EntityKind::Function, 0)?;
    let base_note = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 0)?;
    let calls = read_method_calls(&view, read_owner)?;
    if calls.len() != 1 {
        return Err(TestError::Falsified("read owns one MethodCall"));
    }
    assert_universe_method(calls[0], "super.note stays a universe key")?;
    if calls[0].occurrence.confidence != OccurrenceConfidence::Index {
        return Err(TestError::Falsified("super.note confidence is Index"));
    }
    if matches!(
        &calls[0].occurrence.target,
        OccurrenceTarget::Local(target) if target.raw == base_note
    ) {
        return Err(TestError::Falsified("super.note must not bind Base.note"));
    }
    fs::remove_dir_all(&env.work).map_err(|source| TestError::Io("remove scratch", source))?;
    Ok(())
}

#[test]
fn py_super_quoted_base_stays_universe() -> Result<(), TestError> {
    const SOURCE: &[u8] = b"\
class Base:
    def note(self):
        return 1

class Child(\"Base\"):
    def read(self):
        return super().note()
";
    let env = inherited_fixture_env()?;
    let toolchain = env.toolchain()?;
    let cancelled = AtomicBool::new(false);
    let fragment = compile_inherited_fragment(SOURCE, &env.work, &toolchain, &cancelled)?;
    let view = fragment.view()?;
    let read_owner = entity_ordinal_at_index(&view, b"read", EntityKind::Function, 0)?;
    let base_note = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 0)?;
    let calls = read_method_calls(&view, read_owner)?;
    if calls.len() != 1 {
        return Err(TestError::Falsified("read owns one MethodCall"));
    }
    assert_universe_method(calls[0], "quoted base keeps a universe key")?;
    if calls[0].occurrence.confidence != OccurrenceConfidence::Index {
        return Err(TestError::Falsified("quoted base confidence is Index"));
    }
    if matches!(
        &calls[0].occurrence.target,
        OccurrenceTarget::Local(target) if target.raw == base_note
    ) {
        return Err(TestError::Falsified("quoted base must not bind Base.note"));
    }
    fs::remove_dir_all(&env.work).map_err(|source| TestError::Io("remove scratch", source))?;
    Ok(())
}

#[test]
fn py_super_qualified_base_stays_universe() -> Result<(), TestError> {
    const SOURCE: &[u8] = b"\
class Base:
    def note(self):
        return 1

class Child(mod.Base):
    def read(self):
        return super().note()
";
    let env = inherited_fixture_env()?;
    let toolchain = env.toolchain()?;
    let cancelled = AtomicBool::new(false);
    let fragment = compile_inherited_fragment(SOURCE, &env.work, &toolchain, &cancelled)?;
    let view = fragment.view()?;
    let read_owner = entity_ordinal_at_index(&view, b"read", EntityKind::Function, 0)?;
    let base_note = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 0)?;
    let calls = read_method_calls(&view, read_owner)?;
    if calls.len() != 1 {
        return Err(TestError::Falsified("read owns one MethodCall"));
    }
    assert_universe_method(calls[0], "qualified base keeps a universe key")?;
    if calls[0].occurrence.confidence != OccurrenceConfidence::Index {
        return Err(TestError::Falsified("qualified base confidence is Index"));
    }
    if matches!(
        &calls[0].occurrence.target,
        OccurrenceTarget::Local(target) if target.raw == base_note
    ) {
        return Err(TestError::Falsified("qualified base must not bind Base.note"));
    }
    fs::remove_dir_all(&env.work).map_err(|source| TestError::Io("remove scratch", source))?;
    Ok(())
}

#[test]
fn py_super_generic_base_targets_base_note() -> Result<(), TestError> {
    const SOURCE: &[u8] = b"\
class Base:
    def note(self):
        return 1

class Child(Base[int]):
    def read(self):
        return super().note()
";
    let env = inherited_fixture_env()?;
    let toolchain = env.toolchain()?;
    let cancelled = AtomicBool::new(false);
    let fragment = compile_inherited_fragment(SOURCE, &env.work, &toolchain, &cancelled)?;
    let view = fragment.view()?;
    let read_owner = entity_ordinal_at_index(&view, b"read", EntityKind::Function, 0)?;
    let base_note = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 0)?;
    let calls = read_method_calls(&view, read_owner)?;
    if calls.len() != 1 {
        return Err(TestError::Falsified("read owns one MethodCall"));
    }
    assert_local_index(calls[0], base_note, "generic base resolves to Base.note")?;
    fs::remove_dir_all(&env.work).map_err(|source| TestError::Io("remove scratch", source))?;
    Ok(())
}

#[test]
fn py_super_object_base_targets_base_note() -> Result<(), TestError> {
    const SOURCE: &[u8] = b"\
class Base:
    def note(self):
        return 1

class Child(Base, object):
    def read(self):
        return super().note()
";
    let env = inherited_fixture_env()?;
    let toolchain = env.toolchain()?;
    let cancelled = AtomicBool::new(false);
    let fragment = compile_inherited_fragment(SOURCE, &env.work, &toolchain, &cancelled)?;
    let view = fragment.view()?;
    let read_owner = entity_ordinal_at_index(&view, b"read", EntityKind::Function, 0)?;
    let base_note = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 0)?;
    let calls = read_method_calls(&view, read_owner)?;
    if calls.len() != 1 {
        return Err(TestError::Falsified("read owns one MethodCall"));
    }
    assert_local_index(calls[0], base_note, "object base resolves to Base.note")?;
    fs::remove_dir_all(&env.work).map_err(|source| TestError::Io("remove scratch", source))?;
    Ok(())
}

#[test]
fn py_super_eighth_base_targets_note() -> Result<(), TestError> {
    const SOURCE: &[u8] = b"\
class Base:
    def note(self):
        return 1

class A1(Base):
    pass

class A2(A1):
    pass

class A3(A2):
    pass

class A4(A3):
    pass

class A5(A4):
    pass

class A6(A5):
    pass

class A7(A6):
    pass

class Child(A7):
    def read(self):
        return super().note()
";
    let env = inherited_fixture_env()?;
    let toolchain = env.toolchain()?;
    let cancelled = AtomicBool::new(false);
    let fragment = compile_inherited_fragment(SOURCE, &env.work, &toolchain, &cancelled)?;
    let view = fragment.view()?;
    let read_owner = entity_ordinal_at_index(&view, b"read", EntityKind::Function, 0)?;
    let base_note = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 0)?;
    let calls = read_method_calls(&view, read_owner)?;
    if calls.len() != 1 {
        return Err(TestError::Falsified("read owns one MethodCall"));
    }
    assert_local_index(calls[0], base_note, "eighth base link resolves to Base.note")?;
    fs::remove_dir_all(&env.work).map_err(|source| TestError::Io("remove scratch", source))?;
    Ok(())
}

#[test]
fn py_super_ninth_base_stays_universe() -> Result<(), TestError> {
    const SOURCE: &[u8] = b"\
class Base:
    def note(self):
        return 1

class A1(Base):
    pass

class A2(A1):
    pass

class A3(A2):
    pass

class A4(A3):
    pass

class A5(A4):
    pass

class A6(A5):
    pass

class A7(A6):
    pass

class A8(A7):
    pass

class Child(A8):
    def read(self):
        return super().note()
";
    let env = inherited_fixture_env()?;
    let toolchain = env.toolchain()?;
    let cancelled = AtomicBool::new(false);
    let fragment = compile_inherited_fragment(SOURCE, &env.work, &toolchain, &cancelled)?;
    let view = fragment.view()?;
    let read_owner = entity_ordinal_at_index(&view, b"read", EntityKind::Function, 0)?;
    let base_note = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 0)?;
    let calls = read_method_calls(&view, read_owner)?;
    if calls.len() != 1 {
        return Err(TestError::Falsified("read owns one MethodCall"));
    }
    assert_universe_method(calls[0], "ninth base link stays a universe key")?;
    if calls[0].occurrence.confidence != OccurrenceConfidence::Index {
        return Err(TestError::Falsified("ninth base link confidence is Index"));
    }
    if matches!(
        &calls[0].occurrence.target,
        OccurrenceTarget::Local(target) if target.raw == base_note
    ) {
        return Err(TestError::Falsified("ninth base link must not bind Base.note"));
    }
    fs::remove_dir_all(&env.work).map_err(|source| TestError::Io("remove scratch", source))?;
    Ok(())
}

#[test]
fn py_super_ambiguous_base_methods_stay_universe() -> Result<(), TestError> {
    const SOURCE: &[u8] = b"\
class Base:
    def note(self):
        return 1

    def note(self, extra):
        return 2

class Child(Base):
    def read(self):
        return super().note()
";
    let env = inherited_fixture_env()?;
    let toolchain = env.toolchain()?;
    let cancelled = AtomicBool::new(false);
    let fragment = compile_inherited_fragment(SOURCE, &env.work, &toolchain, &cancelled)?;
    let view = fragment.view()?;
    let read_owner = entity_ordinal_at_index(&view, b"read", EntityKind::Function, 0)?;
    let first_note = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 0)?;
    let second_note = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 1)?;
    let calls = read_method_calls(&view, read_owner)?;
    if calls.len() != 1 {
        return Err(TestError::Falsified("read owns one MethodCall"));
    }
    assert_universe_method(calls[0], "ambiguous base methods stay a universe key")?;
    if calls[0].occurrence.confidence != OccurrenceConfidence::Index {
        return Err(TestError::Falsified("ambiguous base methods confidence is Index"));
    }
    if matches!(
        &calls[0].occurrence.target,
        OccurrenceTarget::Local(target) if target.raw == first_note
    ) {
        return Err(TestError::Falsified("super().note must not bind first Base.note"));
    }
    if matches!(
        &calls[0].occurrence.target,
        OccurrenceTarget::Local(target) if target.raw == second_note
    ) {
        return Err(TestError::Falsified("super().note must not bind second Base.note"));
    }
    fs::remove_dir_all(&env.work).map_err(|source| TestError::Io("remove scratch", source))?;
    Ok(())
}

#[test]
fn py_super_field_wins_over_method() -> Result<(), TestError> {
    const SOURCE: &[u8] = b"\
class Base:
    note = 1

    def note(self):
        return self.note

class Child(Base):
    def read(self):
        return super().note
";
    let env = inherited_fixture_env()?;
    let toolchain = env.toolchain()?;
    let cancelled = AtomicBool::new(false);
    let fragment = compile_inherited_fragment(SOURCE, &env.work, &toolchain, &cancelled)?;
    let view = fragment.view()?;
    let read_owner = entity_ordinal_at_index(&view, b"read", EntityKind::Function, 0)?;
    let note_field = entity_ordinal_at_index(&view, b"note", EntityKind::Field, 0)?;
    let note_method = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 0)?;
    let calls = read_method_calls(&view, read_owner)?;
    if !calls.is_empty() {
        return Err(TestError::Falsified("read owns zero MethodCalls"));
    }
    let reads = read_field_accesses(&view, read_owner)?;
    if reads.len() != 1 {
        return Err(TestError::Falsified("read owns one FieldAccess"));
    }
    assert_local_index(reads[0], note_field, "super().note resolves to Base.note field")?;
    if matches!(
        &reads[0].occurrence.target,
        OccurrenceTarget::Local(target) if target.raw == note_method
    ) {
        return Err(TestError::Falsified("super().note must not resolve to Base.note method"));
    }
    fs::remove_dir_all(&env.work).map_err(|source| TestError::Io("remove scratch", source))?;
    Ok(())
}

#[test]
fn py_super_call_binds_method_not_field() -> Result<(), TestError> {
    const SOURCE: &[u8] = b"\
class Base:
    note = 1

    def note(self):
        return self.note

class Child(Base):
    def read(self):
        return super().note()
";
    let env = inherited_fixture_env()?;
    let toolchain = env.toolchain()?;
    let cancelled = AtomicBool::new(false);
    let fragment = compile_inherited_fragment(SOURCE, &env.work, &toolchain, &cancelled)?;
    let view = fragment.view()?;
    let read_owner = entity_ordinal_at_index(&view, b"read", EntityKind::Function, 0)?;
    let note_field = entity_ordinal_at_index(&view, b"note", EntityKind::Field, 0)?;
    let note_method = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 0)?;
    let calls = read_method_calls(&view, read_owner)?;
    if calls.len() != 1 {
        return Err(TestError::Falsified("read owns one MethodCall"));
    }
    assert_local_index(calls[0], note_method, "super().note() resolves to Base.note method")?;
    if matches!(
        &calls[0].occurrence.target,
        OccurrenceTarget::Local(target) if target.raw == note_field
    ) {
        return Err(TestError::Falsified("super().note() must not resolve to Base.note field"));
    }
    fs::remove_dir_all(&env.work).map_err(|source| TestError::Io("remove scratch", source))?;
    Ok(())
}

#[test]
fn py_super_cycle_stays_universe() -> Result<(), TestError> {
    const SOURCE: &[u8] = b"\
class A(B):
    def note(self):
        return 1

    def read(self):
        return super().note()

class B(A):
    def note(self):
        return 2

    def read(self):
        return super().note()
";
    let env = inherited_fixture_env()?;
    let toolchain = env.toolchain()?;
    let cancelled = AtomicBool::new(false);
    let fragment = compile_inherited_fragment(SOURCE, &env.work, &toolchain, &cancelled)?;
    let view = fragment.view()?;
    let a_read = entity_ordinal_at_index(&view, b"read", EntityKind::Function, 0)?;
    let b_read = entity_ordinal_at_index(&view, b"read", EntityKind::Function, 1)?;
    let a_note = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 0)?;
    let b_note = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 1)?;
    let a_calls = read_method_calls(&view, a_read)?;
    if a_calls.len() != 1 {
        return Err(TestError::Falsified("A.read owns one MethodCall"));
    }
    assert_universe_method(a_calls[0], "cycle A.read stays a universe key")?;
    if a_calls[0].occurrence.confidence != OccurrenceConfidence::Index {
        return Err(TestError::Falsified("cycle A.read confidence is Index"));
    }
    if matches!(
        &a_calls[0].occurrence.target,
        OccurrenceTarget::Local(target) if target.raw == b_note
    ) {
        return Err(TestError::Falsified("A.read must not bind B.note"));
    }
    let b_calls = read_method_calls(&view, b_read)?;
    if b_calls.len() != 1 {
        return Err(TestError::Falsified("B.read owns one MethodCall"));
    }
    assert_universe_method(b_calls[0], "cycle B.read stays a universe key")?;
    if b_calls[0].occurrence.confidence != OccurrenceConfidence::Index {
        return Err(TestError::Falsified("cycle B.read confidence is Index"));
    }
    if matches!(
        &b_calls[0].occurrence.target,
        OccurrenceTarget::Local(target) if target.raw == a_note
    ) {
        return Err(TestError::Falsified("B.read must not bind A.note"));
    }
    fs::remove_dir_all(&env.work).map_err(|source| TestError::Io("remove scratch", source))?;
    Ok(())
}

fn assert_universe_score_field(
    row: &backend_semantic::ir::DecodedOccurrence<'_>,
    label: &'static str,
) -> Result<(), TestError> {
    if !matches!(
        &row.occurrence.target,
        OccurrenceTarget::Foreign(key)
            if key.path == "score"
                && key.display == "score"
                && key.kind == Some(EntityKind::Field)
                && matches!(key.origin, ForeignOrigin::Universe { ecosystem: "pypi" })
    ) {
        return Err(TestError::Falsified(label));
    }
    Ok(())
}

#[test]
fn py_super_explicit_after_child_targets_base_note_not_child() -> Result<(), TestError> {
    const SOURCE: &[u8] = b"\
class Other:
    def note(self):
        return 0

class Base:
    def note(self):
        return 1

class Child(Base):
    def note(self):
        return 2

    def read(self):
        return super(Child, self).note()
";
    let env = inherited_fixture_env()?;
    let toolchain = env.toolchain()?;
    let cancelled = AtomicBool::new(false);
    let fragment = compile_inherited_fragment(SOURCE, &env.work, &toolchain, &cancelled)?;
    let view = fragment.view()?;
    let read_owner = entity_ordinal_at_index(&view, b"read", EntityKind::Function, 0)?;
    let other_note = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 0)?;
    let base_note = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 1)?;
    let child_note = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 2)?;
    let calls = read_method_calls(&view, read_owner)?;
    if calls.len() != 1 {
        return Err(TestError::Falsified("read owns one MethodCall"));
    }
    assert_local_index(
        calls[0],
        base_note,
        "super(Child, self).note resolves to Base.note",
    )?;
    if matches!(
        &calls[0].occurrence.target,
        OccurrenceTarget::Local(target) if target.raw == other_note
    ) {
        return Err(TestError::Falsified(
            "super(Child, self).note must not resolve to Other.note",
        ));
    }
    if matches!(
        &calls[0].occurrence.target,
        OccurrenceTarget::Local(target) if target.raw == child_note
    ) {
        return Err(TestError::Falsified(
            "super(Child, self).note must not resolve to Child.note",
        ));
    }
    fs::remove_dir_all(&env.work).map_err(|source| TestError::Io("remove scratch", source))?;
    Ok(())
}

#[test]
fn py_super_explicit_after_base_stays_universe() -> Result<(), TestError> {
    const SOURCE: &[u8] = b"\
class Other:
    def note(self):
        return 0

class Base:
    def note(self):
        return 1

class Child(Base):
    def note(self):
        return 2

    def read(self):
        return super(Base, self).note()
";
    let env = inherited_fixture_env()?;
    let toolchain = env.toolchain()?;
    let cancelled = AtomicBool::new(false);
    let fragment = compile_inherited_fragment(SOURCE, &env.work, &toolchain, &cancelled)?;
    let view = fragment.view()?;
    let read_owner = entity_ordinal_at_index(&view, b"read", EntityKind::Function, 0)?;
    let base_note = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 1)?;
    let child_note = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 2)?;
    let calls = read_method_calls(&view, read_owner)?;
    if calls.len() != 1 {
        return Err(TestError::Falsified("read owns one MethodCall"));
    }
    assert_universe_method(calls[0], "super(Base, self).note stays a universe key")?;
    if calls[0].occurrence.confidence != OccurrenceConfidence::Index {
        return Err(TestError::Falsified("super(Base, self).note confidence is Index"));
    }
    if matches!(
        &calls[0].occurrence.target,
        OccurrenceTarget::Local(target) if target.raw == base_note
    ) {
        return Err(TestError::Falsified("super(Base, self).note must not bind Base.note"));
    }
    if matches!(
        &calls[0].occurrence.target,
        OccurrenceTarget::Local(target) if target.raw == child_note
    ) {
        return Err(TestError::Falsified("super(Base, self).note must not bind Child.note"));
    }
    fs::remove_dir_all(&env.work).map_err(|source| TestError::Io("remove scratch", source))?;
    Ok(())
}

#[test]
fn py_super_explicit_after_mid_targets_base_note() -> Result<(), TestError> {
    const SOURCE: &[u8] = b"\
class Base:
    def note(self):
        return 1

class Mid(Base):
    pass

class Child(Mid):
    def read(self):
        return super(Mid, self).note()
";
    let env = inherited_fixture_env()?;
    let toolchain = env.toolchain()?;
    let cancelled = AtomicBool::new(false);
    let fragment = compile_inherited_fragment(SOURCE, &env.work, &toolchain, &cancelled)?;
    let view = fragment.view()?;
    let read_owner = entity_ordinal_at_index(&view, b"read", EntityKind::Function, 0)?;
    let base_note = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 0)?;
    let calls = read_method_calls(&view, read_owner)?;
    if calls.len() != 1 {
        return Err(TestError::Falsified("read owns one MethodCall"));
    }
    assert_local_index(
        calls[0],
        base_note,
        "super(Mid, self).note resolves to Base.note",
    )?;
    fs::remove_dir_all(&env.work).map_err(|source| TestError::Io("remove scratch", source))?;
    Ok(())
}

#[test]
fn py_super_explicit_field_after_child_targets_base_score() -> Result<(), TestError> {
    const SOURCE: &[u8] = b"\
class Base:
    score = 1

class Child(Base):
    score = 2

    def read(self):
        return super(Child, self).score
";
    let env = inherited_fixture_env()?;
    let toolchain = env.toolchain()?;
    let cancelled = AtomicBool::new(false);
    let fragment = compile_inherited_fragment(SOURCE, &env.work, &toolchain, &cancelled)?;
    let view = fragment.view()?;
    let read_owner = entity_ordinal_at_index(&view, b"read", EntityKind::Function, 0)?;
    let base_score = entity_ordinal_at_index(&view, b"score", EntityKind::Field, 0)?;
    let child_score = entity_ordinal_at_index(&view, b"score", EntityKind::Field, 1)?;
    let calls = read_method_calls(&view, read_owner)?;
    if !calls.is_empty() {
        return Err(TestError::Falsified("read owns zero MethodCalls"));
    }
    let reads = read_field_accesses(&view, read_owner)?;
    if reads.len() != 1 {
        return Err(TestError::Falsified("read owns one FieldAccess"));
    }
    assert_local_index(
        reads[0],
        base_score,
        "super(Child, self).score resolves to Base.score",
    )?;
    if matches!(
        &reads[0].occurrence.target,
        OccurrenceTarget::Local(target) if target.raw == child_score
    ) {
        return Err(TestError::Falsified(
            "super(Child, self).score must not resolve to Child.score",
        ));
    }
    fs::remove_dir_all(&env.work).map_err(|source| TestError::Io("remove scratch", source))?;
    Ok(())
}

#[test]
fn py_super_explicit_field_after_base_stays_universe() -> Result<(), TestError> {
    const SOURCE: &[u8] = b"\
class Base:
    score = 1

class Child(Base):
    score = 2

    def read(self):
        return super(Base, self).score
";
    let env = inherited_fixture_env()?;
    let toolchain = env.toolchain()?;
    let cancelled = AtomicBool::new(false);
    let fragment = compile_inherited_fragment(SOURCE, &env.work, &toolchain, &cancelled)?;
    let view = fragment.view()?;
    let read_owner = entity_ordinal_at_index(&view, b"read", EntityKind::Function, 0)?;
    let base_score = entity_ordinal_at_index(&view, b"score", EntityKind::Field, 0)?;
    let child_score = entity_ordinal_at_index(&view, b"score", EntityKind::Field, 1)?;
    let calls = read_method_calls(&view, read_owner)?;
    if !calls.is_empty() {
        return Err(TestError::Falsified("read owns zero MethodCalls"));
    }
    let reads = read_field_accesses(&view, read_owner)?;
    if reads.len() != 1 {
        return Err(TestError::Falsified("read owns one FieldAccess"));
    }
    assert_universe_score_field(
        reads[0],
        "super(Base, self).score stays a universe field key",
    )?;
    if reads[0].occurrence.confidence != OccurrenceConfidence::Index {
        return Err(TestError::Falsified("super(Base, self).score confidence is Index"));
    }
    if matches!(
        &reads[0].occurrence.target,
        OccurrenceTarget::Local(target) if target.raw == base_score
    ) {
        return Err(TestError::Falsified(
            "super(Base, self).score must not bind Base.score",
        ));
    }
    if matches!(
        &reads[0].occurrence.target,
        OccurrenceTarget::Local(target) if target.raw == child_score
    ) {
        return Err(TestError::Falsified(
            "super(Base, self).score must not bind Child.score",
        ));
    }
    fs::remove_dir_all(&env.work).map_err(|source| TestError::Io("remove scratch", source))?;
    Ok(())
}

#[test]
fn py_super_explicit_method_value_targets_base_note() -> Result<(), TestError> {
    const SOURCE: &[u8] = b"\
class Base:
    def note(self):
        return 1

class Child(Base):
    def note(self):
        return 2

    def read(self):
        return super(Child, self).note
";
    let env = inherited_fixture_env()?;
    let toolchain = env.toolchain()?;
    let cancelled = AtomicBool::new(false);
    let fragment = compile_inherited_fragment(SOURCE, &env.work, &toolchain, &cancelled)?;
    let view = fragment.view()?;
    let read_owner = entity_ordinal_at_index(&view, b"read", EntityKind::Function, 0)?;
    let base_note = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 0)?;
    let child_note = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 1)?;
    let calls = read_method_calls(&view, read_owner)?;
    if !calls.is_empty() {
        return Err(TestError::Falsified("read owns zero MethodCalls"));
    }
    let reads = read_field_accesses(&view, read_owner)?;
    if reads.len() != 1 {
        return Err(TestError::Falsified("read owns one FieldAccess"));
    }
    assert_local_index(
        reads[0],
        base_note,
        "super(Child, self).note resolves to Base.note",
    )?;
    if matches!(
        &reads[0].occurrence.target,
        OccurrenceTarget::Local(target) if target.raw == child_note
    ) {
        return Err(TestError::Falsified(
            "super(Child, self).note must not resolve to Child.note",
        ));
    }
    fs::remove_dir_all(&env.work).map_err(|source| TestError::Io("remove scratch", source))?;
    Ok(())
}

#[test]
fn py_super_explicit_unknown_start_stays_universe() -> Result<(), TestError> {
    const SOURCE: &[u8] = b"\
class Other:
    def note(self):
        return 0

class Base:
    def note(self):
        return 1

class Child(Base):
    def read(self):
        return super(Other, self).note()
";
    let env = inherited_fixture_env()?;
    let toolchain = env.toolchain()?;
    let cancelled = AtomicBool::new(false);
    let fragment = compile_inherited_fragment(SOURCE, &env.work, &toolchain, &cancelled)?;
    let view = fragment.view()?;
    let read_owner = entity_ordinal_at_index(&view, b"read", EntityKind::Function, 0)?;
    let other_note = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 0)?;
    let base_note = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 1)?;
    let calls = read_method_calls(&view, read_owner)?;
    if calls.len() != 1 {
        return Err(TestError::Falsified("read owns one MethodCall"));
    }
    assert_universe_method(calls[0], "super(Other, self).note stays a universe key")?;
    if calls[0].occurrence.confidence != OccurrenceConfidence::Index {
        return Err(TestError::Falsified("super(Other, self).note confidence is Index"));
    }
    if matches!(
        &calls[0].occurrence.target,
        OccurrenceTarget::Local(target) if target.raw == base_note
    ) {
        return Err(TestError::Falsified(
            "super(Other, self).note must not bind Base.note",
        ));
    }
    if matches!(
        &calls[0].occurrence.target,
        OccurrenceTarget::Local(target) if target.raw == other_note
    ) {
        return Err(TestError::Falsified(
            "super(Other, self).note must not bind Other.note",
        ));
    }
    fs::remove_dir_all(&env.work).map_err(|source| TestError::Io("remove scratch", source))?;
    Ok(())
}

#[test]
fn py_super_explicit_other_instance_stays_universe() -> Result<(), TestError> {
    const SOURCE: &[u8] = b"\
class Base:
    def note(self):
        return 1

class Child(Base):
    def read(self, obj):
        return super(Child, obj).note()
";
    let env = inherited_fixture_env()?;
    let toolchain = env.toolchain()?;
    let cancelled = AtomicBool::new(false);
    let fragment = compile_inherited_fragment(SOURCE, &env.work, &toolchain, &cancelled)?;
    let view = fragment.view()?;
    let read_owner = entity_ordinal_at_index(&view, b"read", EntityKind::Function, 0)?;
    let base_note = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 0)?;
    let calls = read_method_calls(&view, read_owner)?;
    if calls.len() != 1 {
        return Err(TestError::Falsified("read owns one MethodCall"));
    }
    assert_universe_method(calls[0], "super(Child, obj).note stays a universe key")?;
    if calls[0].occurrence.confidence != OccurrenceConfidence::Index {
        return Err(TestError::Falsified("super(Child, obj).note confidence is Index"));
    }
    if matches!(
        &calls[0].occurrence.target,
        OccurrenceTarget::Local(target) if target.raw == base_note
    ) {
        return Err(TestError::Falsified(
            "super(Child, obj).note must not bind Base.note",
        ));
    }
    fs::remove_dir_all(&env.work).map_err(|source| TestError::Io("remove scratch", source))?;
    Ok(())
}

#[test]
fn py_super_explicit_one_arg_stays_universe() -> Result<(), TestError> {
    const SOURCE: &[u8] = b"\
class Base:
    def note(self):
        return 1

class Child(Base):
    def read(self):
        return super(Child).note()
";
    let env = inherited_fixture_env()?;
    let toolchain = env.toolchain()?;
    let cancelled = AtomicBool::new(false);
    let fragment = compile_inherited_fragment(SOURCE, &env.work, &toolchain, &cancelled)?;
    let view = fragment.view()?;
    let read_owner = entity_ordinal_at_index(&view, b"read", EntityKind::Function, 0)?;
    let base_note = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 0)?;
    let calls = read_method_calls(&view, read_owner)?;
    if calls.len() != 1 {
        return Err(TestError::Falsified("read owns one MethodCall"));
    }
    assert_universe_method(calls[0], "super(Child).note stays a universe key")?;
    if calls[0].occurrence.confidence != OccurrenceConfidence::Index {
        return Err(TestError::Falsified("super(Child).note confidence is Index"));
    }
    if matches!(
        &calls[0].occurrence.target,
        OccurrenceTarget::Local(target) if target.raw == base_note
    ) {
        return Err(TestError::Falsified("super(Child).note must not bind Base.note"));
    }
    fs::remove_dir_all(&env.work).map_err(|source| TestError::Io("remove scratch", source))?;
    Ok(())
}

#[test]
fn py_super_explicit_dotted_stays_universe() -> Result<(), TestError> {
    const SOURCE: &[u8] = b"\
import mod

class Base:
    def note(self):
        return 1

class Child(Base):
    def read(self):
        return super(mod.Child, self).note()
";
    let env = inherited_fixture_env()?;
    let toolchain = env.toolchain()?;
    let cancelled = AtomicBool::new(false);
    let fragment = compile_inherited_fragment(SOURCE, &env.work, &toolchain, &cancelled)?;
    let view = fragment.view()?;
    let read_owner = entity_ordinal_at_index(&view, b"read", EntityKind::Function, 0)?;
    let base_note = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 0)?;
    let calls = read_method_calls(&view, read_owner)?;
    if calls.len() != 1 {
        return Err(TestError::Falsified("read owns one MethodCall"));
    }
    assert_universe_method(calls[0], "super(mod.Child, self).note stays a universe key")?;
    if calls[0].occurrence.confidence != OccurrenceConfidence::Index {
        return Err(TestError::Falsified(
            "super(mod.Child, self).note confidence is Index",
        ));
    }
    if matches!(
        &calls[0].occurrence.target,
        OccurrenceTarget::Local(target) if target.raw == base_note
    ) {
        return Err(TestError::Falsified(
            "super(mod.Child, self).note must not bind Base.note",
        ));
    }
    fs::remove_dir_all(&env.work).map_err(|source| TestError::Io("remove scratch", source))?;
    Ok(())
}

#[test]
fn py_super_explicit_nested_stays_universe() -> Result<(), TestError> {
    const SOURCE: &[u8] = b"\
class Base:
    def note(self):
        return 1

class Child(Base):
    def read(self):
        def inner():
            return super(Child, self).note()
        return inner()
";
    let env = inherited_fixture_env()?;
    let toolchain = env.toolchain()?;
    let cancelled = AtomicBool::new(false);
    let fragment = compile_inherited_fragment(SOURCE, &env.work, &toolchain, &cancelled)?;
    let view = fragment.view()?;
    let read_owner = entity_ordinal_at_index(&view, b"read", EntityKind::Function, 0)?;
    let inner_owner = entity_ordinal_at_index(&view, b"inner", EntityKind::Function, 0)?;
    let base_note = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 0)?;
    let read_calls = read_method_calls(&view, read_owner)?;
    if !read_calls.is_empty() {
        for call in &read_calls {
            if matches!(
                &call.occurrence.target,
                OccurrenceTarget::Local(target) if target.raw == base_note
            ) {
                return Err(TestError::Falsified("read must not bind Base.note"));
            }
        }
    }
    let inner_calls = read_method_calls(&view, inner_owner)?;
    if inner_calls.len() != 1 {
        return Err(TestError::Falsified("inner owns one MethodCall"));
    }
    assert_universe_method(inner_calls[0], "nested explicit super stays a universe key")?;
    if inner_calls[0].occurrence.confidence != OccurrenceConfidence::Index {
        return Err(TestError::Falsified("nested explicit super confidence is Index"));
    }
    if matches!(
        &inner_calls[0].occurrence.target,
        OccurrenceTarget::Local(target) if target.raw == base_note
    ) {
        return Err(TestError::Falsified("nested explicit super must not bind Base.note"));
    }
    fs::remove_dir_all(&env.work).map_err(|source| TestError::Io("remove scratch", source))?;
    Ok(())
}

#[test]
fn py_super_explicit_classmethod_targets_base_note() -> Result<(), TestError> {
    const SOURCE: &[u8] = b"\
class Base:
    def note(self):
        return 1

class Child(Base):
    @classmethod
    def read(cls):
        return super(Child, cls).note()
";
    let env = inherited_fixture_env()?;
    let toolchain = env.toolchain()?;
    let cancelled = AtomicBool::new(false);
    let fragment = compile_inherited_fragment(SOURCE, &env.work, &toolchain, &cancelled)?;
    let view = fragment.view()?;
    let read_owner = entity_ordinal_at_index(&view, b"read", EntityKind::Function, 0)?;
    let base_note = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 0)?;
    let calls = read_method_calls(&view, read_owner)?;
    if calls.len() != 1 {
        return Err(TestError::Falsified("read owns one MethodCall"));
    }
    assert_local_index(
        calls[0],
        base_note,
        "super(Child, cls).note resolves to Base.note",
    )?;
    fs::remove_dir_all(&env.work).map_err(|source| TestError::Io("remove scratch", source))?;
    Ok(())
}

#[test]
fn py_super_explicit_after_last_link_stays_universe() -> Result<(), TestError> {
    const SOURCE: &[u8] = b"\
class Base:
    def note(self):
        return 1

class A1(Base):
    pass

class A2(A1):
    pass

class A3(A2):
    pass

class A4(A3):
    pass

class A5(A4):
    pass

class A6(A5):
    pass

class A7(A6):
    pass

class Child(A7):
    def read(self):
        return super(Base, self).note()
";
    let env = inherited_fixture_env()?;
    let toolchain = env.toolchain()?;
    let cancelled = AtomicBool::new(false);
    let fragment = compile_inherited_fragment(SOURCE, &env.work, &toolchain, &cancelled)?;
    let view = fragment.view()?;
    let read_owner = entity_ordinal_at_index(&view, b"read", EntityKind::Function, 0)?;
    let base_note = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 0)?;
    let calls = read_method_calls(&view, read_owner)?;
    if calls.len() != 1 {
        return Err(TestError::Falsified("read owns one MethodCall"));
    }
    assert_universe_method(calls[0], "super(Base, self) after last link stays a universe key")?;
    if calls[0].occurrence.confidence != OccurrenceConfidence::Index {
        return Err(TestError::Falsified(
            "super(Base, self) after last link confidence is Index",
        ));
    }
    if matches!(
        &calls[0].occurrence.target,
        OccurrenceTarget::Local(target) if target.raw == base_note
    ) {
        return Err(TestError::Falsified(
            "super(Base, self) after last link must not bind Base.note",
        ));
    }
    fs::remove_dir_all(&env.work).map_err(|source| TestError::Io("remove scratch", source))?;
    Ok(())
}

#[test]
fn py_generic_annotation_call_targets_child_note_not_decoy() -> Result<(), TestError> {
    const SOURCE: &[u8] = b"\
class Decoy:
    def note(self):
        return 0

class Child:
    def note(self):
        return 1

def read(service: Child[int]):
    return service.note()
";
    let env = inherited_fixture_env()?;
    let toolchain = env.toolchain()?;
    let cancelled = AtomicBool::new(false);
    let fragment = compile_inherited_fragment(SOURCE, &env.work, &toolchain, &cancelled)?;
    let view = fragment.view()?;
    let read_owner = entity_ordinal_at_index(&view, b"read", EntityKind::Function, 0)?;
    let decoy_note = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 0)?;
    let child_note = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 1)?;
    let calls = read_method_calls(&view, read_owner)?;
    if calls.len() != 1 {
        return Err(TestError::Falsified("read owns one MethodCall"));
    }
    assert_local_index(calls[0], child_note, "service.note() resolves to Child.note")?;
    if matches!(
        &calls[0].occurrence.target,
        OccurrenceTarget::Local(target) if target.raw == decoy_note
    ) {
        return Err(TestError::Falsified("service.note() must not resolve to Decoy.note"));
    }
    let reads = read_field_accesses(&view, read_owner)?;
    if !reads.is_empty() {
        return Err(TestError::Falsified("read owns zero FieldAccess rows"));
    }
    fs::remove_dir_all(&env.work).map_err(|source| TestError::Io("remove scratch", source))?;
    Ok(())
}

#[test]
fn py_generic_annotation_value_targets_child_note_not_decoy() -> Result<(), TestError> {
    const SOURCE: &[u8] = b"\
class Decoy:
    def note(self):
        return 0

class Child:
    def note(self):
        return 1

def read(service: Child[int]):
    return service.note
";
    let env = inherited_fixture_env()?;
    let toolchain = env.toolchain()?;
    let cancelled = AtomicBool::new(false);
    let fragment = compile_inherited_fragment(SOURCE, &env.work, &toolchain, &cancelled)?;
    let view = fragment.view()?;
    let read_owner = entity_ordinal_at_index(&view, b"read", EntityKind::Function, 0)?;
    let decoy_note = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 0)?;
    let child_note = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 1)?;
    let calls = read_method_calls(&view, read_owner)?;
    if !calls.is_empty() {
        return Err(TestError::Falsified("read owns zero MethodCalls"));
    }
    let reads = read_field_accesses(&view, read_owner)?;
    if reads.len() != 1 {
        return Err(TestError::Falsified("read owns one FieldAccess"));
    }
    assert_local_index(reads[0], child_note, "service.note resolves to Child.note")?;
    if matches!(
        &reads[0].occurrence.target,
        OccurrenceTarget::Local(target) if target.raw == decoy_note
    ) {
        return Err(TestError::Falsified("service.note must not resolve to Decoy.note"));
    }
    fs::remove_dir_all(&env.work).map_err(|source| TestError::Io("remove scratch", source))?;
    Ok(())
}

#[test]
fn py_generic_annotation_field_targets_child_score_not_decoy() -> Result<(), TestError> {
    const SOURCE: &[u8] = b"\
class Decoy:
    score = 9

class Child:
    score = 1

def read(service: Child[int]):
    return service.score
";
    let env = inherited_fixture_env()?;
    let toolchain = env.toolchain()?;
    let cancelled = AtomicBool::new(false);
    let fragment = compile_inherited_fragment(SOURCE, &env.work, &toolchain, &cancelled)?;
    let view = fragment.view()?;
    let read_owner = entity_ordinal_at_index(&view, b"read", EntityKind::Function, 0)?;
    let decoy_score = entity_ordinal_at_index(&view, b"score", EntityKind::Field, 0)?;
    let child_score = entity_ordinal_at_index(&view, b"score", EntityKind::Field, 1)?;
    let calls = read_method_calls(&view, read_owner)?;
    if !calls.is_empty() {
        return Err(TestError::Falsified("read owns zero MethodCalls"));
    }
    let reads = read_field_accesses(&view, read_owner)?;
    if reads.len() != 1 {
        return Err(TestError::Falsified("read owns one FieldAccess"));
    }
    assert_local_index(reads[0], child_score, "service.score resolves to Child.score")?;
    if matches!(
        &reads[0].occurrence.target,
        OccurrenceTarget::Local(target) if target.raw == decoy_score
    ) {
        return Err(TestError::Falsified("service.score must not resolve to Decoy.score"));
    }
    fs::remove_dir_all(&env.work).map_err(|source| TestError::Io("remove scratch", source))?;
    Ok(())
}

#[test]
fn py_generic_annotation_inherited_call_targets_base_note() -> Result<(), TestError> {
    const SOURCE: &[u8] = b"\
class Base:
    def note(self):
        return 1

class Child(Base):
    pass

def read(service: Child[int]):
    return service.note()
";
    let env = inherited_fixture_env()?;
    let toolchain = env.toolchain()?;
    let cancelled = AtomicBool::new(false);
    let fragment = compile_inherited_fragment(SOURCE, &env.work, &toolchain, &cancelled)?;
    let view = fragment.view()?;
    let read_owner = entity_ordinal_at_index(&view, b"read", EntityKind::Function, 0)?;
    let base_note = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 0)?;
    let calls = read_method_calls(&view, read_owner)?;
    if calls.len() != 1 {
        return Err(TestError::Falsified("read owns one MethodCall"));
    }
    assert_local_index(calls[0], base_note, "service.note() resolves to Base.note")?;
    let reads = read_field_accesses(&view, read_owner)?;
    if !reads.is_empty() {
        return Err(TestError::Falsified("read owns zero FieldAccess rows"));
    }
    fs::remove_dir_all(&env.work).map_err(|source| TestError::Io("remove scratch", source))?;
    Ok(())
}

#[test]
fn py_generic_annotation_list_stays_universe() -> Result<(), TestError> {
    const SOURCE: &[u8] = b"\
class Child:
    def note(self):
        return 1

def read(service: list[Child]):
    return service.note()
";
    let env = inherited_fixture_env()?;
    let toolchain = env.toolchain()?;
    let cancelled = AtomicBool::new(false);
    let fragment = compile_inherited_fragment(SOURCE, &env.work, &toolchain, &cancelled)?;
    let view = fragment.view()?;
    let read_owner = entity_ordinal_at_index(&view, b"read", EntityKind::Function, 0)?;
    let child_note = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 0)?;
    let calls = read_method_calls(&view, read_owner)?;
    if calls.len() != 1 {
        return Err(TestError::Falsified("read owns one MethodCall"));
    }
    assert_universe_method(calls[0], "list[Child] receiver stays a pypi universe key")?;
    if calls[0].occurrence.confidence != OccurrenceConfidence::Index {
        return Err(TestError::Falsified("list[Child] universe method confidence is Index"));
    }
    if matches!(
        &calls[0].occurrence.target,
        OccurrenceTarget::Local(target) if target.raw == child_note
    ) {
        return Err(TestError::Falsified("list[Child] must not bind Child.note"));
    }
    fs::remove_dir_all(&env.work).map_err(|source| TestError::Io("remove scratch", source))?;
    Ok(())
}

#[test]
fn py_generic_annotation_dotted_stays_universe() -> Result<(), TestError> {
    const SOURCE: &[u8] = b"\
class Child:
    def note(self):
        return 1

def read(service: pkg.Child[int]):
    return service.note()
";
    let env = inherited_fixture_env()?;
    let toolchain = env.toolchain()?;
    let cancelled = AtomicBool::new(false);
    let fragment = compile_inherited_fragment(SOURCE, &env.work, &toolchain, &cancelled)?;
    let view = fragment.view()?;
    let read_owner = entity_ordinal_at_index(&view, b"read", EntityKind::Function, 0)?;
    let child_note = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 0)?;
    let calls = read_method_calls(&view, read_owner)?;
    if calls.len() != 1 {
        return Err(TestError::Falsified("read owns one MethodCall"));
    }
    assert_universe_method(calls[0], "pkg.Child[int] receiver stays a pypi universe key")?;
    if calls[0].occurrence.confidence != OccurrenceConfidence::Index {
        return Err(TestError::Falsified("pkg.Child[int] universe method confidence is Index"));
    }
    if matches!(
        &calls[0].occurrence.target,
        OccurrenceTarget::Local(target) if target.raw == child_note
    ) {
        return Err(TestError::Falsified("pkg.Child[int] must not bind Child.note"));
    }
    fs::remove_dir_all(&env.work).map_err(|source| TestError::Io("remove scratch", source))?;
    Ok(())
}

#[test]
fn py_generic_annotation_quoted_call_still_targets_child() -> Result<(), TestError> {
    const SOURCE: &[u8] = b"\
class Child:
    def note(self):
        return 1

def read(service: \"Child\"):
    return service.note()
";
    let env = inherited_fixture_env()?;
    let toolchain = env.toolchain()?;
    let cancelled = AtomicBool::new(false);
    let fragment = compile_inherited_fragment(SOURCE, &env.work, &toolchain, &cancelled)?;
    let view = fragment.view()?;
    let read_owner = entity_ordinal_at_index(&view, b"read", EntityKind::Function, 0)?;
    let child_note = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 0)?;
    let calls = read_method_calls(&view, read_owner)?;
    if calls.len() != 1 {
        return Err(TestError::Falsified("read owns one MethodCall"));
    }
    assert_local_index(calls[0], child_note, "quoted Child annotation resolves to Child.note")?;
    let reads = read_field_accesses(&view, read_owner)?;
    if !reads.is_empty() {
        return Err(TestError::Falsified("read owns zero FieldAccess rows"));
    }
    fs::remove_dir_all(&env.work).map_err(|source| TestError::Io("remove scratch", source))?;
    Ok(())
}

#[test]
fn py_generic_annotation_field_wins_over_method() -> Result<(), TestError> {
    const SOURCE: &[u8] = b"\
class Child:
    note = 1

    def note(self):
        return self.note

def read(service: Child[int]):
    return service.note
";
    let env = inherited_fixture_env()?;
    let toolchain = env.toolchain()?;
    let cancelled = AtomicBool::new(false);
    let fragment = compile_inherited_fragment(SOURCE, &env.work, &toolchain, &cancelled)?;
    let view = fragment.view()?;
    let read_owner = entity_ordinal_at_index(&view, b"read", EntityKind::Function, 0)?;
    let note_field = entity_ordinal_at_index(&view, b"note", EntityKind::Field, 0)?;
    let note_method = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 0)?;
    let calls = read_method_calls(&view, read_owner)?;
    if !calls.is_empty() {
        return Err(TestError::Falsified("read owns zero MethodCalls"));
    }
    let reads = read_field_accesses(&view, read_owner)?;
    if reads.len() != 1 {
        return Err(TestError::Falsified("read owns one FieldAccess"));
    }
    assert_local_index(reads[0], note_field, "service.note resolves to Child.note field")?;
    if matches!(
        &reads[0].occurrence.target,
        OccurrenceTarget::Local(target) if target.raw == note_method
    ) {
        return Err(TestError::Falsified("service.note must not resolve to Child.note method"));
    }
    fs::remove_dir_all(&env.work).map_err(|source| TestError::Io("remove scratch", source))?;
    Ok(())
}

#[test]
fn py_generic_annotation_call_binds_method_not_field() -> Result<(), TestError> {
    const SOURCE: &[u8] = b"\
class Child:
    note = 1

    def note(self):
        return self.note

def read(service: Child[int]):
    return service.note()
";
    let env = inherited_fixture_env()?;
    let toolchain = env.toolchain()?;
    let cancelled = AtomicBool::new(false);
    let fragment = compile_inherited_fragment(SOURCE, &env.work, &toolchain, &cancelled)?;
    let view = fragment.view()?;
    let read_owner = entity_ordinal_at_index(&view, b"read", EntityKind::Function, 0)?;
    let note_field = entity_ordinal_at_index(&view, b"note", EntityKind::Field, 0)?;
    let note_method = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 0)?;
    let calls = read_method_calls(&view, read_owner)?;
    if calls.len() != 1 {
        return Err(TestError::Falsified("read owns one MethodCall"));
    }
    assert_local_index(calls[0], note_method, "service.note() resolves to Child.note")?;
    if matches!(
        &calls[0].occurrence.target,
        OccurrenceTarget::Local(target) if target.raw == note_field
    ) {
        return Err(TestError::Falsified("service.note() must not resolve to Child.note field"));
    }
    let reads = read_field_accesses(&view, read_owner)?;
    if !reads.is_empty() {
        return Err(TestError::Falsified("read owns zero FieldAccess rows"));
    }
    fs::remove_dir_all(&env.work).map_err(|source| TestError::Io("remove scratch", source))?;
    Ok(())
}

#[test]
fn py_generic_annotation_alias_read_uses_package_key() -> Result<(), TestError> {
    const SOURCE: &[u8] = b"\
from workout.service import WorkoutService

def read(service: WorkoutService[int]):
    return service.score
";
    let env = inherited_fixture_env()?;
    let toolchain = env.toolchain()?;
    let cancelled = AtomicBool::new(false);
    let fragment = compile_inherited_fragment(SOURCE, &env.work, &toolchain, &cancelled)?;
    let view = fragment.view()?;
    let read_owner = entity_ordinal_at_index(&view, b"read", EntityKind::Function, 0)?;
    let calls = read_method_calls(&view, read_owner)?;
    if !calls.is_empty() {
        return Err(TestError::Falsified("read owns zero MethodCalls"));
    }
    let reads = read_field_accesses(&view, read_owner)?;
    if reads.len() != 1 {
        return Err(TestError::Falsified("read owns one FieldAccess"));
    }
    if !matches!(
        &reads[0].occurrence.target,
        OccurrenceTarget::Foreign(key)
            if key.path == "workout.service"
                && key.display == "score"
                && key.kind == Some(EntityKind::Field)
                && matches!(
                    key.origin,
                    ForeignOrigin::Package(lineage) if lineage.name == "workout"
                )
    ) {
        return Err(TestError::Falsified(
            "service.score is not a workout.service package field key",
        ));
    }
    if reads[0].occurrence.confidence != OccurrenceConfidence::Index {
        return Err(TestError::Falsified("service.score package field confidence is Index"));
    }
    if matches!(&reads[0].occurrence.target, OccurrenceTarget::Local(_)) {
        return Err(TestError::Falsified("service.score must not resolve locally"));
    }
    fs::remove_dir_all(&env.work).map_err(|source| TestError::Io("remove scratch", source))?;
    Ok(())
}

#[test]
fn py_generic_annotation_alias_call_uses_package_key() -> Result<(), TestError> {
    const SOURCE: &[u8] = b"\
from workout.service import WorkoutService

def read(service: WorkoutService[int]):
    return service.note()
";
    let env = inherited_fixture_env()?;
    let toolchain = env.toolchain()?;
    let cancelled = AtomicBool::new(false);
    let fragment = compile_inherited_fragment(SOURCE, &env.work, &toolchain, &cancelled)?;
    let view = fragment.view()?;
    let read_owner = entity_ordinal_at_index(&view, b"read", EntityKind::Function, 0)?;
    let calls = read_method_calls(&view, read_owner)?;
    if calls.len() != 1 {
        return Err(TestError::Falsified("read owns one MethodCall"));
    }
    if !matches!(
        &calls[0].occurrence.target,
        OccurrenceTarget::Foreign(key)
            if key.path == "workout.service"
                && key.display == "note"
                && key.kind == Some(EntityKind::Function)
                && matches!(
                    key.origin,
                    ForeignOrigin::Package(lineage) if lineage.name == "workout"
                )
    ) {
        return Err(TestError::Falsified(
            "service.note() is not a workout.service package method key",
        ));
    }
    if calls[0].occurrence.confidence != OccurrenceConfidence::Index {
        return Err(TestError::Falsified("service.note() package method confidence is Index"));
    }
    if matches!(&calls[0].occurrence.target, OccurrenceTarget::Local(_)) {
        return Err(TestError::Falsified("service.note() must not resolve locally"));
    }
    let reads = read_field_accesses(&view, read_owner)?;
    if !reads.is_empty() {
        return Err(TestError::Falsified("read owns zero FieldAccess rows"));
    }
    fs::remove_dir_all(&env.work).map_err(|source| TestError::Io("remove scratch", source))?;
    Ok(())
}

#[test]
fn py_union_annotation_call_targets_child_note_not_decoy() -> Result<(), TestError> {
    const SOURCE: &[u8] = b"\
class Decoy:
    def note(self):
        return 0

class Child:
    def note(self):
        return 1

def read(service: Child | None):
    return service.note()
";
    let env = inherited_fixture_env()?;
    let toolchain = env.toolchain()?;
    let cancelled = AtomicBool::new(false);
    let fragment = compile_inherited_fragment(SOURCE, &env.work, &toolchain, &cancelled)?;
    let view = fragment.view()?;
    let read_owner = entity_ordinal_at_index(&view, b"read", EntityKind::Function, 0)?;
    let decoy_note = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 0)?;
    let child_note = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 1)?;
    let calls = read_method_calls(&view, read_owner)?;
    if calls.len() != 1 {
        return Err(TestError::Falsified("read owns one MethodCall"));
    }
    assert_local_index(calls[0], child_note, "service.note() resolves to Child.note")?;
    if matches!(
        &calls[0].occurrence.target,
        OccurrenceTarget::Local(target) if target.raw == decoy_note
    ) {
        return Err(TestError::Falsified("service.note() must not resolve to Decoy.note"));
    }
    let reads = read_field_accesses(&view, read_owner)?;
    if !reads.is_empty() {
        return Err(TestError::Falsified("read owns zero FieldAccess rows"));
    }
    fs::remove_dir_all(&env.work).map_err(|source| TestError::Io("remove scratch", source))?;
    Ok(())
}

#[test]
fn py_union_annotation_value_targets_child_note_not_decoy() -> Result<(), TestError> {
    const SOURCE: &[u8] = b"\
class Decoy:
    def note(self):
        return 0

class Child:
    def note(self):
        return 1

def read(service: Child | None):
    return service.note
";
    let env = inherited_fixture_env()?;
    let toolchain = env.toolchain()?;
    let cancelled = AtomicBool::new(false);
    let fragment = compile_inherited_fragment(SOURCE, &env.work, &toolchain, &cancelled)?;
    let view = fragment.view()?;
    let read_owner = entity_ordinal_at_index(&view, b"read", EntityKind::Function, 0)?;
    let decoy_note = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 0)?;
    let child_note = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 1)?;
    let calls = read_method_calls(&view, read_owner)?;
    if !calls.is_empty() {
        return Err(TestError::Falsified("read owns zero MethodCalls"));
    }
    let reads = read_field_accesses(&view, read_owner)?;
    if reads.len() != 1 {
        return Err(TestError::Falsified("read owns one FieldAccess"));
    }
    assert_local_index(reads[0], child_note, "service.note resolves to Child.note")?;
    if matches!(
        &reads[0].occurrence.target,
        OccurrenceTarget::Local(target) if target.raw == decoy_note
    ) {
        return Err(TestError::Falsified("service.note must not resolve to Decoy.note"));
    }
    fs::remove_dir_all(&env.work).map_err(|source| TestError::Io("remove scratch", source))?;
    Ok(())
}

#[test]
fn py_union_annotation_field_targets_child_score_not_decoy() -> Result<(), TestError> {
    const SOURCE: &[u8] = b"\
class Decoy:
    score = 9

class Child:
    score = 1

def read(service: Child | None):
    return service.score
";
    let env = inherited_fixture_env()?;
    let toolchain = env.toolchain()?;
    let cancelled = AtomicBool::new(false);
    let fragment = compile_inherited_fragment(SOURCE, &env.work, &toolchain, &cancelled)?;
    let view = fragment.view()?;
    let read_owner = entity_ordinal_at_index(&view, b"read", EntityKind::Function, 0)?;
    let decoy_score = entity_ordinal_at_index(&view, b"score", EntityKind::Field, 0)?;
    let child_score = entity_ordinal_at_index(&view, b"score", EntityKind::Field, 1)?;
    let calls = read_method_calls(&view, read_owner)?;
    if !calls.is_empty() {
        return Err(TestError::Falsified("read owns zero MethodCalls"));
    }
    let reads = read_field_accesses(&view, read_owner)?;
    if reads.len() != 1 {
        return Err(TestError::Falsified("read owns one FieldAccess"));
    }
    assert_local_index(reads[0], child_score, "service.score resolves to Child.score")?;
    if matches!(
        &reads[0].occurrence.target,
        OccurrenceTarget::Local(target) if target.raw == decoy_score
    ) {
        return Err(TestError::Falsified("service.score must not resolve to Decoy.score"));
    }
    fs::remove_dir_all(&env.work).map_err(|source| TestError::Io("remove scratch", source))?;
    Ok(())
}

#[test]
fn py_union_annotation_none_first_targets_child_note() -> Result<(), TestError> {
    const SOURCE: &[u8] = b"\
class Child:
    def note(self):
        return 1

def read(service: None | Child):
    return service.note()
";
    let env = inherited_fixture_env()?;
    let toolchain = env.toolchain()?;
    let cancelled = AtomicBool::new(false);
    let fragment = compile_inherited_fragment(SOURCE, &env.work, &toolchain, &cancelled)?;
    let view = fragment.view()?;
    let read_owner = entity_ordinal_at_index(&view, b"read", EntityKind::Function, 0)?;
    let child_note = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 0)?;
    let calls = read_method_calls(&view, read_owner)?;
    if calls.len() != 1 {
        return Err(TestError::Falsified("read owns one MethodCall"));
    }
    assert_local_index(calls[0], child_note, "service.note() resolves to Child.note")?;
    let reads = read_field_accesses(&view, read_owner)?;
    if !reads.is_empty() {
        return Err(TestError::Falsified("read owns zero FieldAccess rows"));
    }
    fs::remove_dir_all(&env.work).map_err(|source| TestError::Io("remove scratch", source))?;
    Ok(())
}

#[test]
fn py_union_annotation_inherited_call_targets_base_note() -> Result<(), TestError> {
    const SOURCE: &[u8] = b"\
class Base:
    def note(self):
        return 1

class Child(Base):
    pass

def read(service: Child | None):
    return service.note()
";
    let env = inherited_fixture_env()?;
    let toolchain = env.toolchain()?;
    let cancelled = AtomicBool::new(false);
    let fragment = compile_inherited_fragment(SOURCE, &env.work, &toolchain, &cancelled)?;
    let view = fragment.view()?;
    let read_owner = entity_ordinal_at_index(&view, b"read", EntityKind::Function, 0)?;
    let base_note = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 0)?;
    let calls = read_method_calls(&view, read_owner)?;
    if calls.len() != 1 {
        return Err(TestError::Falsified("read owns one MethodCall"));
    }
    assert_local_index(calls[0], base_note, "service.note() resolves to Base.note")?;
    let reads = read_field_accesses(&view, read_owner)?;
    if !reads.is_empty() {
        return Err(TestError::Falsified("read owns zero FieldAccess rows"));
    }
    fs::remove_dir_all(&env.work).map_err(|source| TestError::Io("remove scratch", source))?;
    Ok(())
}

#[test]
fn py_union_annotation_two_classes_stay_universe() -> Result<(), TestError> {
    const SOURCE: &[u8] = b"\
class Left:
    def note(self):
        return 0

class Right:
    def note(self):
        return 1

def read(service: Left | Right):
    return service.note()
";
    let env = inherited_fixture_env()?;
    let toolchain = env.toolchain()?;
    let cancelled = AtomicBool::new(false);
    let fragment = compile_inherited_fragment(SOURCE, &env.work, &toolchain, &cancelled)?;
    let view = fragment.view()?;
    let read_owner = entity_ordinal_at_index(&view, b"read", EntityKind::Function, 0)?;
    let left_note = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 0)?;
    let right_note = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 1)?;
    let calls = read_method_calls(&view, read_owner)?;
    if calls.len() != 1 {
        return Err(TestError::Falsified("read owns one MethodCall"));
    }
    assert_universe_method(calls[0], "Left | Right receiver stays a pypi universe key")?;
    if calls[0].occurrence.confidence != OccurrenceConfidence::Index {
        return Err(TestError::Falsified("Left | Right universe method confidence is Index"));
    }
    if matches!(
        &calls[0].occurrence.target,
        OccurrenceTarget::Local(target) if target.raw == left_note
    ) {
        return Err(TestError::Falsified("Left | Right must not bind Left.note"));
    }
    if matches!(
        &calls[0].occurrence.target,
        OccurrenceTarget::Local(target) if target.raw == right_note
    ) {
        return Err(TestError::Falsified("Left | Right must not bind Right.note"));
    }
    fs::remove_dir_all(&env.work).map_err(|source| TestError::Io("remove scratch", source))?;
    Ok(())
}

#[test]
fn py_union_annotation_two_classes_do_not_steal_unique_field() -> Result<(), TestError> {
    const SOURCE: &[u8] = b"\
class Left:
    score = 1

class Right:
    pass

def read(service: Left | Right):
    return service.score
";
    let env = inherited_fixture_env()?;
    let toolchain = env.toolchain()?;
    let cancelled = AtomicBool::new(false);
    let fragment = compile_inherited_fragment(SOURCE, &env.work, &toolchain, &cancelled)?;
    let view = fragment.view()?;
    let read_owner = entity_ordinal_at_index(&view, b"read", EntityKind::Function, 0)?;
    let left_score = entity_ordinal_at_index(&view, b"score", EntityKind::Field, 0)?;
    let calls = read_method_calls(&view, read_owner)?;
    if !calls.is_empty() {
        return Err(TestError::Falsified("read owns zero MethodCalls"));
    }
    let reads = read_field_accesses(&view, read_owner)?;
    if reads.len() != 1 {
        return Err(TestError::Falsified("read owns one FieldAccess"));
    }
    if !matches!(
        &reads[0].occurrence.target,
        OccurrenceTarget::Foreign(key)
            if key.path == "score"
                && key.display == "score"
                && key.kind == Some(EntityKind::Field)
                && matches!(key.origin, ForeignOrigin::Universe { ecosystem: "pypi" })
    ) {
        return Err(TestError::Falsified(
            "Left | Right receiver stays a pypi universe field key",
        ));
    }
    if reads[0].occurrence.confidence != OccurrenceConfidence::Index {
        return Err(TestError::Falsified("Left | Right universe field confidence is Index"));
    }
    if matches!(
        &reads[0].occurrence.target,
        OccurrenceTarget::Local(target) if target.raw == left_score
    ) {
        return Err(TestError::Falsified("Left | Right must not bind Left.score"));
    }
    fs::remove_dir_all(&env.work).map_err(|source| TestError::Io("remove scratch", source))?;
    Ok(())
}

#[test]
fn py_union_annotation_optional_call_targets_child_note() -> Result<(), TestError> {
    const SOURCE: &[u8] = b"\
class Child:
    def note(self):
        return 1

def read(service: Optional[Child]):
    return service.note()
";
    let env = inherited_fixture_env()?;
    let toolchain = env.toolchain()?;
    let cancelled = AtomicBool::new(false);
    let fragment = compile_inherited_fragment(SOURCE, &env.work, &toolchain, &cancelled)?;
    let view = fragment.view()?;
    let read_owner = entity_ordinal_at_index(&view, b"read", EntityKind::Function, 0)?;
    let child_note = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 0)?;
    let calls = read_method_calls(&view, read_owner)?;
    if calls.len() != 1 {
        return Err(TestError::Falsified("read owns one MethodCall"));
    }
    assert_local_index(calls[0], child_note, "service.note() resolves to Child.note")?;
    let reads = read_field_accesses(&view, read_owner)?;
    if !reads.is_empty() {
        return Err(TestError::Falsified("read owns zero FieldAccess rows"));
    }
    fs::remove_dir_all(&env.work).map_err(|source| TestError::Io("remove scratch", source))?;
    Ok(())
}

#[test]
fn py_union_annotation_typing_optional_call_targets_child_note() -> Result<(), TestError> {
    const SOURCE: &[u8] = b"\
class Child:
    def note(self):
        return 1

def read(service: typing.Optional[Child]):
    return service.note()
";
    let env = inherited_fixture_env()?;
    let toolchain = env.toolchain()?;
    let cancelled = AtomicBool::new(false);
    let fragment = compile_inherited_fragment(SOURCE, &env.work, &toolchain, &cancelled)?;
    let view = fragment.view()?;
    let read_owner = entity_ordinal_at_index(&view, b"read", EntityKind::Function, 0)?;
    let child_note = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 0)?;
    let calls = read_method_calls(&view, read_owner)?;
    if calls.len() != 1 {
        return Err(TestError::Falsified("read owns one MethodCall"));
    }
    assert_local_index(calls[0], child_note, "service.note() resolves to Child.note")?;
    let reads = read_field_accesses(&view, read_owner)?;
    if reads.len() != 1 {
        return Err(TestError::Falsified("read owns one FieldAccess"));
    }
    if !matches!(
        &reads[0].occurrence.target,
        OccurrenceTarget::Foreign(key)
            if key.path == "Optional"
                && key.display == "Optional"
                && key.kind == Some(EntityKind::Field)
                && matches!(key.origin, ForeignOrigin::Universe { ecosystem: "pypi" })
    ) {
        return Err(TestError::Falsified(
            "typing.Optional annotation attribute is not a pypi universe field key",
        ));
    }
    if reads[0].occurrence.confidence != OccurrenceConfidence::Index {
        return Err(TestError::Falsified(
            "typing.Optional annotation field confidence is Index",
        ));
    }
    if matches!(
        &reads[0].occurrence.target,
        OccurrenceTarget::Local(target) if target.raw == child_note
    ) {
        return Err(TestError::Falsified(
            "typing.Optional annotation field must not resolve to Child.note",
        ));
    }
    fs::remove_dir_all(&env.work).map_err(|source| TestError::Io("remove scratch", source))?;
    Ok(())
}

#[test]
fn py_union_annotation_typing_union_call_targets_child_note() -> Result<(), TestError> {
    const SOURCE: &[u8] = b"\
class Child:
    def note(self):
        return 1

def read(service: Union[Child, None]):
    return service.note()
";
    let env = inherited_fixture_env()?;
    let toolchain = env.toolchain()?;
    let cancelled = AtomicBool::new(false);
    let fragment = compile_inherited_fragment(SOURCE, &env.work, &toolchain, &cancelled)?;
    let view = fragment.view()?;
    let read_owner = entity_ordinal_at_index(&view, b"read", EntityKind::Function, 0)?;
    let child_note = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 0)?;
    let calls = read_method_calls(&view, read_owner)?;
    if calls.len() != 1 {
        return Err(TestError::Falsified("read owns one MethodCall"));
    }
    assert_local_index(calls[0], child_note, "service.note() resolves to Child.note")?;
    let reads = read_field_accesses(&view, read_owner)?;
    if !reads.is_empty() {
        return Err(TestError::Falsified("read owns zero FieldAccess rows"));
    }
    fs::remove_dir_all(&env.work).map_err(|source| TestError::Io("remove scratch", source))?;
    Ok(())
}

#[test]
fn py_union_annotation_generic_arm_targets_child_note() -> Result<(), TestError> {
    const SOURCE: &[u8] = b"\
class Child:
    def note(self):
        return 1

def read(service: Child[int] | None):
    return service.note()
";
    let env = inherited_fixture_env()?;
    let toolchain = env.toolchain()?;
    let cancelled = AtomicBool::new(false);
    let fragment = compile_inherited_fragment(SOURCE, &env.work, &toolchain, &cancelled)?;
    let view = fragment.view()?;
    let read_owner = entity_ordinal_at_index(&view, b"read", EntityKind::Function, 0)?;
    let child_note = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 0)?;
    let calls = read_method_calls(&view, read_owner)?;
    if calls.len() != 1 {
        return Err(TestError::Falsified("read owns one MethodCall"));
    }
    assert_local_index(calls[0], child_note, "service.note() resolves to Child.note")?;
    let reads = read_field_accesses(&view, read_owner)?;
    if !reads.is_empty() {
        return Err(TestError::Falsified("read owns zero FieldAccess rows"));
    }
    fs::remove_dir_all(&env.work).map_err(|source| TestError::Io("remove scratch", source))?;
    Ok(())
}

#[test]
fn py_union_annotation_other_type_stays_universe() -> Result<(), TestError> {
    const SOURCE: &[u8] = b"\
class Child:
    def note(self):
        return 1

def read(service: Child | int):
    return service.note()
";
    let env = inherited_fixture_env()?;
    let toolchain = env.toolchain()?;
    let cancelled = AtomicBool::new(false);
    let fragment = compile_inherited_fragment(SOURCE, &env.work, &toolchain, &cancelled)?;
    let view = fragment.view()?;
    let read_owner = entity_ordinal_at_index(&view, b"read", EntityKind::Function, 0)?;
    let child_note = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 0)?;
    let calls = read_method_calls(&view, read_owner)?;
    if calls.len() != 1 {
        return Err(TestError::Falsified("read owns one MethodCall"));
    }
    assert_universe_method(calls[0], "Child | int receiver stays a pypi universe key")?;
    if calls[0].occurrence.confidence != OccurrenceConfidence::Index {
        return Err(TestError::Falsified("Child | int universe method confidence is Index"));
    }
    if matches!(
        &calls[0].occurrence.target,
        OccurrenceTarget::Local(target) if target.raw == child_note
    ) {
        return Err(TestError::Falsified("Child | int must not bind Child.note"));
    }
    fs::remove_dir_all(&env.work).map_err(|source| TestError::Io("remove scratch", source))?;
    Ok(())
}

#[test]
fn py_union_annotation_list_stays_universe() -> Result<(), TestError> {
    const SOURCE: &[u8] = b"\
class Child:
    def note(self):
        return 1

def read(service: list[Child]):
    return service.note()
";
    let env = inherited_fixture_env()?;
    let toolchain = env.toolchain()?;
    let cancelled = AtomicBool::new(false);
    let fragment = compile_inherited_fragment(SOURCE, &env.work, &toolchain, &cancelled)?;
    let view = fragment.view()?;
    let read_owner = entity_ordinal_at_index(&view, b"read", EntityKind::Function, 0)?;
    let child_note = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 0)?;
    let calls = read_method_calls(&view, read_owner)?;
    if calls.len() != 1 {
        return Err(TestError::Falsified("read owns one MethodCall"));
    }
    assert_universe_method(calls[0], "list[Child] receiver stays a pypi universe key")?;
    if calls[0].occurrence.confidence != OccurrenceConfidence::Index {
        return Err(TestError::Falsified("list[Child] universe method confidence is Index"));
    }
    if matches!(
        &calls[0].occurrence.target,
        OccurrenceTarget::Local(target) if target.raw == child_note
    ) {
        return Err(TestError::Falsified("list[Child] must not bind Child.note"));
    }
    fs::remove_dir_all(&env.work).map_err(|source| TestError::Io("remove scratch", source))?;
    Ok(())
}

#[test]
fn py_union_annotation_field_wins_over_method() -> Result<(), TestError> {
    const SOURCE: &[u8] = b"\
class Child:
    note = 1

    def note(self):
        return self.note

def read(service: Child | None):
    return service.note
";
    let env = inherited_fixture_env()?;
    let toolchain = env.toolchain()?;
    let cancelled = AtomicBool::new(false);
    let fragment = compile_inherited_fragment(SOURCE, &env.work, &toolchain, &cancelled)?;
    let view = fragment.view()?;
    let read_owner = entity_ordinal_at_index(&view, b"read", EntityKind::Function, 0)?;
    let note_field = entity_ordinal_at_index(&view, b"note", EntityKind::Field, 0)?;
    let note_method = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 0)?;
    let calls = read_method_calls(&view, read_owner)?;
    if !calls.is_empty() {
        return Err(TestError::Falsified("read owns zero MethodCalls"));
    }
    let reads = read_field_accesses(&view, read_owner)?;
    if reads.len() != 1 {
        return Err(TestError::Falsified("read owns one FieldAccess"));
    }
    assert_local_index(reads[0], note_field, "service.note resolves to Child.note field")?;
    if matches!(
        &reads[0].occurrence.target,
        OccurrenceTarget::Local(target) if target.raw == note_method
    ) {
        return Err(TestError::Falsified("service.note must not resolve to Child.note method"));
    }
    fs::remove_dir_all(&env.work).map_err(|source| TestError::Io("remove scratch", source))?;
    Ok(())
}

#[test]
fn py_union_annotation_alias_read_uses_package_key() -> Result<(), TestError> {
    const SOURCE: &[u8] = b"\
from workout.service import WorkoutService

def read(service: WorkoutService | None):
    return service.score
";
    let env = inherited_fixture_env()?;
    let toolchain = env.toolchain()?;
    let cancelled = AtomicBool::new(false);
    let fragment = compile_inherited_fragment(SOURCE, &env.work, &toolchain, &cancelled)?;
    let view = fragment.view()?;
    let read_owner = entity_ordinal_at_index(&view, b"read", EntityKind::Function, 0)?;
    let calls = read_method_calls(&view, read_owner)?;
    if !calls.is_empty() {
        return Err(TestError::Falsified("read owns zero MethodCalls"));
    }
    let reads = read_field_accesses(&view, read_owner)?;
    if reads.len() != 1 {
        return Err(TestError::Falsified("read owns one FieldAccess"));
    }
    if !matches!(
        &reads[0].occurrence.target,
        OccurrenceTarget::Foreign(key)
            if key.path == "workout.service"
                && key.display == "score"
                && key.kind == Some(EntityKind::Field)
                && matches!(
                    key.origin,
                    ForeignOrigin::Package(lineage) if lineage.name == "workout"
                )
    ) {
        return Err(TestError::Falsified(
            "service.score is not a workout.service package field key",
        ));
    }
    if reads[0].occurrence.confidence != OccurrenceConfidence::Index {
        return Err(TestError::Falsified("service.score package field confidence is Index"));
    }
    if matches!(&reads[0].occurrence.target, OccurrenceTarget::Local(_)) {
        return Err(TestError::Falsified("service.score must not resolve locally"));
    }
    fs::remove_dir_all(&env.work).map_err(|source| TestError::Io("remove scratch", source))?;
    Ok(())
}

#[test]
fn py_class_qualified_call_targets_child_note_not_decoy() -> Result<(), TestError> {
    const SOURCE: &[u8] = b"\
class Decoy:
    def note(self):
        return 0

class Child:
    def note(self):
        return 1

def read():
    return Child.note()
";
    let env = inherited_fixture_env()?;
    let toolchain = env.toolchain()?;
    let cancelled = AtomicBool::new(false);
    let fragment = compile_inherited_fragment(SOURCE, &env.work, &toolchain, &cancelled)?;
    let view = fragment.view()?;
    let read_owner = entity_ordinal_at_index(&view, b"read", EntityKind::Function, 0)?;
    let decoy_note = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 0)?;
    let child_note = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 1)?;
    let calls = read_method_calls(&view, read_owner)?;
    if calls.len() != 1 {
        return Err(TestError::Falsified("read owns one MethodCall"));
    }
    assert_local_index(calls[0], child_note, "Child.note() resolves to Child.note")?;
    if matches!(
        &calls[0].occurrence.target,
        OccurrenceTarget::Local(target) if target.raw == decoy_note
    ) {
        return Err(TestError::Falsified("Child.note() must not resolve to Decoy.note"));
    }
    let reads = read_field_accesses(&view, read_owner)?;
    if !reads.is_empty() {
        return Err(TestError::Falsified("read owns zero FieldAccess rows"));
    }
    fs::remove_dir_all(&env.work).map_err(|source| TestError::Io("remove scratch", source))?;
    Ok(())
}

#[test]
fn py_class_qualified_forward_call_targets_child_note_not_decoy() -> Result<(), TestError> {
    const SOURCE: &[u8] = b"\
def read():
    return Child.note()

class Decoy:
    def note(self):
        return 0

class Child:
    def note(self):
        return 1
";
    let env = inherited_fixture_env()?;
    let toolchain = env.toolchain()?;
    let cancelled = AtomicBool::new(false);
    let fragment = compile_inherited_fragment(SOURCE, &env.work, &toolchain, &cancelled)?;
    let view = fragment.view()?;
    let read_owner = entity_ordinal_at_index(&view, b"read", EntityKind::Function, 0)?;
    let decoy_note = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 0)?;
    let child_note = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 1)?;
    let calls = read_method_calls(&view, read_owner)?;
    if calls.len() != 1 {
        return Err(TestError::Falsified("read owns one MethodCall"));
    }
    assert_local_index(calls[0], child_note, "Child.note() resolves to Child.note")?;
    if matches!(
        &calls[0].occurrence.target,
        OccurrenceTarget::Local(target) if target.raw == decoy_note
    ) {
        return Err(TestError::Falsified("Child.note() must not resolve to Decoy.note"));
    }
    let reads = read_field_accesses(&view, read_owner)?;
    if !reads.is_empty() {
        return Err(TestError::Falsified("read owns zero FieldAccess rows"));
    }
    fs::remove_dir_all(&env.work).map_err(|source| TestError::Io("remove scratch", source))?;
    Ok(())
}

#[test]
fn py_class_qualified_value_targets_child_note_not_decoy() -> Result<(), TestError> {
    const SOURCE: &[u8] = b"\
class Decoy:
    def note(self):
        return 0

class Child:
    def note(self):
        return 1

def read():
    return Child.note
";
    let env = inherited_fixture_env()?;
    let toolchain = env.toolchain()?;
    let cancelled = AtomicBool::new(false);
    let fragment = compile_inherited_fragment(SOURCE, &env.work, &toolchain, &cancelled)?;
    let view = fragment.view()?;
    let read_owner = entity_ordinal_at_index(&view, b"read", EntityKind::Function, 0)?;
    let decoy_note = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 0)?;
    let child_note = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 1)?;
    let calls = read_method_calls(&view, read_owner)?;
    if !calls.is_empty() {
        return Err(TestError::Falsified("read owns zero MethodCalls"));
    }
    let reads = read_field_accesses(&view, read_owner)?;
    if reads.len() != 1 {
        return Err(TestError::Falsified("read owns one FieldAccess"));
    }
    assert_local_index(reads[0], child_note, "Child.note resolves to Child.note")?;
    if matches!(
        &reads[0].occurrence.target,
        OccurrenceTarget::Local(target) if target.raw == decoy_note
    ) {
        return Err(TestError::Falsified("Child.note must not resolve to Decoy.note"));
    }
    fs::remove_dir_all(&env.work).map_err(|source| TestError::Io("remove scratch", source))?;
    Ok(())
}

#[test]
fn py_class_qualified_field_targets_child_score_not_decoy() -> Result<(), TestError> {
    const SOURCE: &[u8] = b"\
class Decoy:
    score = 9

class Child:
    score = 1

def read():
    return Child.score
";
    let env = inherited_fixture_env()?;
    let toolchain = env.toolchain()?;
    let cancelled = AtomicBool::new(false);
    let fragment = compile_inherited_fragment(SOURCE, &env.work, &toolchain, &cancelled)?;
    let view = fragment.view()?;
    let read_owner = entity_ordinal_at_index(&view, b"read", EntityKind::Function, 0)?;
    let decoy_score = entity_ordinal_at_index(&view, b"score", EntityKind::Field, 0)?;
    let child_score = entity_ordinal_at_index(&view, b"score", EntityKind::Field, 1)?;
    let calls = read_method_calls(&view, read_owner)?;
    if !calls.is_empty() {
        return Err(TestError::Falsified("read owns zero MethodCalls"));
    }
    let reads = read_field_accesses(&view, read_owner)?;
    if reads.len() != 1 {
        return Err(TestError::Falsified("read owns one FieldAccess"));
    }
    assert_local_index(reads[0], child_score, "Child.score resolves to Child.score")?;
    if matches!(
        &reads[0].occurrence.target,
        OccurrenceTarget::Local(target) if target.raw == decoy_score
    ) {
        return Err(TestError::Falsified("Child.score must not resolve to Decoy.score"));
    }
    fs::remove_dir_all(&env.work).map_err(|source| TestError::Io("remove scratch", source))?;
    Ok(())
}

#[test]
fn py_class_qualified_forward_field_targets_child_score_not_decoy() -> Result<(), TestError> {
    const SOURCE: &[u8] = b"\
def read():
    return Child.score

class Decoy:
    score = 9

class Child:
    score = 1
";
    let env = inherited_fixture_env()?;
    let toolchain = env.toolchain()?;
    let cancelled = AtomicBool::new(false);
    let fragment = compile_inherited_fragment(SOURCE, &env.work, &toolchain, &cancelled)?;
    let view = fragment.view()?;
    let read_owner = entity_ordinal_at_index(&view, b"read", EntityKind::Function, 0)?;
    let decoy_score = entity_ordinal_at_index(&view, b"score", EntityKind::Field, 0)?;
    let child_score = entity_ordinal_at_index(&view, b"score", EntityKind::Field, 1)?;
    let calls = read_method_calls(&view, read_owner)?;
    if !calls.is_empty() {
        return Err(TestError::Falsified("read owns zero MethodCalls"));
    }
    let reads = read_field_accesses(&view, read_owner)?;
    if reads.len() != 1 {
        return Err(TestError::Falsified("read owns one FieldAccess"));
    }
    assert_local_index(reads[0], child_score, "Child.score resolves to Child.score")?;
    if matches!(
        &reads[0].occurrence.target,
        OccurrenceTarget::Local(target) if target.raw == decoy_score
    ) {
        return Err(TestError::Falsified("Child.score must not resolve to Decoy.score"));
    }
    fs::remove_dir_all(&env.work).map_err(|source| TestError::Io("remove scratch", source))?;
    Ok(())
}

#[test]
fn py_class_qualified_inherited_call_targets_base_note_not_decoy() -> Result<(), TestError> {
    const SOURCE: &[u8] = b"\
class Decoy:
    def note(self):
        return 0

class Base:
    def note(self):
        return 1

class Child(Base):
    pass

def read():
    return Child.note()
";
    let env = inherited_fixture_env()?;
    let toolchain = env.toolchain()?;
    let cancelled = AtomicBool::new(false);
    let fragment = compile_inherited_fragment(SOURCE, &env.work, &toolchain, &cancelled)?;
    let view = fragment.view()?;
    let read_owner = entity_ordinal_at_index(&view, b"read", EntityKind::Function, 0)?;
    let decoy_note = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 0)?;
    let base_note = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 1)?;
    let calls = read_method_calls(&view, read_owner)?;
    if calls.len() != 1 {
        return Err(TestError::Falsified("read owns one MethodCall"));
    }
    assert_local_index(calls[0], base_note, "Child.note() resolves to Base.note")?;
    if matches!(
        &calls[0].occurrence.target,
        OccurrenceTarget::Local(target) if target.raw == decoy_note
    ) {
        return Err(TestError::Falsified("Child.note() must not resolve to Decoy.note"));
    }
    let reads = read_field_accesses(&view, read_owner)?;
    if !reads.is_empty() {
        return Err(TestError::Falsified("read owns zero FieldAccess rows"));
    }
    fs::remove_dir_all(&env.work).map_err(|source| TestError::Io("remove scratch", source))?;
    Ok(())
}

#[test]
fn py_class_qualified_inherited_field_targets_base_score_not_decoy() -> Result<(), TestError> {
    const SOURCE: &[u8] = b"\
class Decoy:
    score = 9

class Base:
    score = 1

class Child(Base):
    pass

def read():
    return Child.score
";
    let env = inherited_fixture_env()?;
    let toolchain = env.toolchain()?;
    let cancelled = AtomicBool::new(false);
    let fragment = compile_inherited_fragment(SOURCE, &env.work, &toolchain, &cancelled)?;
    let view = fragment.view()?;
    let read_owner = entity_ordinal_at_index(&view, b"read", EntityKind::Function, 0)?;
    let decoy_score = entity_ordinal_at_index(&view, b"score", EntityKind::Field, 0)?;
    let base_score = entity_ordinal_at_index(&view, b"score", EntityKind::Field, 1)?;
    let calls = read_method_calls(&view, read_owner)?;
    if !calls.is_empty() {
        return Err(TestError::Falsified("read owns zero MethodCalls"));
    }
    let reads = read_field_accesses(&view, read_owner)?;
    if reads.len() != 1 {
        return Err(TestError::Falsified("read owns one FieldAccess"));
    }
    assert_local_index(reads[0], base_score, "Child.score resolves to Base.score")?;
    if matches!(
        &reads[0].occurrence.target,
        OccurrenceTarget::Local(target) if target.raw == decoy_score
    ) {
        return Err(TestError::Falsified("Child.score must not resolve to Decoy.score"));
    }
    fs::remove_dir_all(&env.work).map_err(|source| TestError::Io("remove scratch", source))?;
    Ok(())
}

#[test]
fn py_class_qualified_field_wins_over_method() -> Result<(), TestError> {
    const SOURCE: &[u8] = b"\
class Child:
    note = 1

    def note(self):
        return self.note

def read():
    return Child.note
";
    let env = inherited_fixture_env()?;
    let toolchain = env.toolchain()?;
    let cancelled = AtomicBool::new(false);
    let fragment = compile_inherited_fragment(SOURCE, &env.work, &toolchain, &cancelled)?;
    let view = fragment.view()?;
    let read_owner = entity_ordinal_at_index(&view, b"read", EntityKind::Function, 0)?;
    let note_field = entity_ordinal_at_index(&view, b"note", EntityKind::Field, 0)?;
    let note_method = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 0)?;
    let calls = read_method_calls(&view, read_owner)?;
    if !calls.is_empty() {
        return Err(TestError::Falsified("read owns zero MethodCalls"));
    }
    let reads = read_field_accesses(&view, read_owner)?;
    if reads.len() != 1 {
        return Err(TestError::Falsified("read owns one FieldAccess"));
    }
    assert_local_index(reads[0], note_field, "Child.note resolves to Child.note field")?;
    if matches!(
        &reads[0].occurrence.target,
        OccurrenceTarget::Local(target) if target.raw == note_method
    ) {
        return Err(TestError::Falsified("Child.note must not resolve to Child.note method"));
    }
    fs::remove_dir_all(&env.work).map_err(|source| TestError::Io("remove scratch", source))?;
    Ok(())
}

#[test]
fn py_class_qualified_call_binds_method_not_field() -> Result<(), TestError> {
    const SOURCE: &[u8] = b"\
class Child:
    note = 1

    def note(self):
        return self.note

def read():
    return Child.note()
";
    let env = inherited_fixture_env()?;
    let toolchain = env.toolchain()?;
    let cancelled = AtomicBool::new(false);
    let fragment = compile_inherited_fragment(SOURCE, &env.work, &toolchain, &cancelled)?;
    let view = fragment.view()?;
    let read_owner = entity_ordinal_at_index(&view, b"read", EntityKind::Function, 0)?;
    let note_field = entity_ordinal_at_index(&view, b"note", EntityKind::Field, 0)?;
    let note_method = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 0)?;
    let calls = read_method_calls(&view, read_owner)?;
    if calls.len() != 1 {
        return Err(TestError::Falsified("read owns one MethodCall"));
    }
    assert_local_index(calls[0], note_method, "Child.note() resolves to Child.note method")?;
    if matches!(
        &calls[0].occurrence.target,
        OccurrenceTarget::Local(target) if target.raw == note_field
    ) {
        return Err(TestError::Falsified("Child.note() must not resolve to Child.note field"));
    }
    let reads = read_field_accesses(&view, read_owner)?;
    if !reads.is_empty() {
        return Err(TestError::Falsified("read owns zero FieldAccess rows"));
    }
    fs::remove_dir_all(&env.work).map_err(|source| TestError::Io("remove scratch", source))?;
    Ok(())
}

#[test]
fn py_constructed_call_targets_child_note_not_decoy() -> Result<(), TestError> {
    const SOURCE: &[u8] = b"\
class Decoy:
    def note(self):
        return 0

class Child:
    def note(self):
        return 1

def read():
    return Child().note()
";
    let env = inherited_fixture_env()?;
    let toolchain = env.toolchain()?;
    let cancelled = AtomicBool::new(false);
    let fragment = compile_inherited_fragment(SOURCE, &env.work, &toolchain, &cancelled)?;
    let view = fragment.view()?;
    let read_owner = entity_ordinal_at_index(&view, b"read", EntityKind::Function, 0)?;
    let decoy_note = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 0)?;
    let child_note = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 1)?;
    let calls = read_method_calls(&view, read_owner)?;
    if calls.len() != 1 {
        return Err(TestError::Falsified("read owns one MethodCall"));
    }
    assert_local_index(calls[0], child_note, "Child().note() resolves to Child.note")?;
    if matches!(
        &calls[0].occurrence.target,
        OccurrenceTarget::Local(target) if target.raw == decoy_note
    ) {
        return Err(TestError::Falsified("Child().note() must not resolve to Decoy.note"));
    }
    let reads = read_field_accesses(&view, read_owner)?;
    if !reads.is_empty() {
        return Err(TestError::Falsified("read owns zero FieldAccess rows"));
    }
    fs::remove_dir_all(&env.work).map_err(|source| TestError::Io("remove scratch", source))?;
    Ok(())
}

#[test]
fn py_constructed_forward_call_targets_child_note_not_decoy() -> Result<(), TestError> {
    const SOURCE: &[u8] = b"\
def read():
    return Child().note()

class Decoy:
    def note(self):
        return 0

class Child:
    def note(self):
        return 1
";
    let env = inherited_fixture_env()?;
    let toolchain = env.toolchain()?;
    let cancelled = AtomicBool::new(false);
    let fragment = compile_inherited_fragment(SOURCE, &env.work, &toolchain, &cancelled)?;
    let view = fragment.view()?;
    let read_owner = entity_ordinal_at_index(&view, b"read", EntityKind::Function, 0)?;
    let decoy_note = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 0)?;
    let child_note = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 1)?;
    let calls = read_method_calls(&view, read_owner)?;
    if calls.len() != 1 {
        return Err(TestError::Falsified("read owns one MethodCall"));
    }
    assert_local_index(calls[0], child_note, "Child().note() resolves to Child.note")?;
    if matches!(
        &calls[0].occurrence.target,
        OccurrenceTarget::Local(target) if target.raw == decoy_note
    ) {
        return Err(TestError::Falsified("Child().note() must not resolve to Decoy.note"));
    }
    let reads = read_field_accesses(&view, read_owner)?;
    if !reads.is_empty() {
        return Err(TestError::Falsified("read owns zero FieldAccess rows"));
    }
    fs::remove_dir_all(&env.work).map_err(|source| TestError::Io("remove scratch", source))?;
    Ok(())
}

#[test]
fn py_constructed_value_targets_child_note_not_decoy() -> Result<(), TestError> {
    const SOURCE: &[u8] = b"\
class Decoy:
    def note(self):
        return 0

class Child:
    def note(self):
        return 1

def read():
    return Child().note
";
    let env = inherited_fixture_env()?;
    let toolchain = env.toolchain()?;
    let cancelled = AtomicBool::new(false);
    let fragment = compile_inherited_fragment(SOURCE, &env.work, &toolchain, &cancelled)?;
    let view = fragment.view()?;
    let read_owner = entity_ordinal_at_index(&view, b"read", EntityKind::Function, 0)?;
    let decoy_note = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 0)?;
    let child_note = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 1)?;
    let calls = read_method_calls(&view, read_owner)?;
    if !calls.is_empty() {
        return Err(TestError::Falsified("read owns zero MethodCalls"));
    }
    let reads = read_field_accesses(&view, read_owner)?;
    if reads.len() != 1 {
        return Err(TestError::Falsified("read owns one FieldAccess"));
    }
    assert_local_index(reads[0], child_note, "Child().note resolves to Child.note")?;
    if matches!(
        &reads[0].occurrence.target,
        OccurrenceTarget::Local(target) if target.raw == decoy_note
    ) {
        return Err(TestError::Falsified("Child().note must not resolve to Decoy.note"));
    }
    fs::remove_dir_all(&env.work).map_err(|source| TestError::Io("remove scratch", source))?;
    Ok(())
}

#[test]
fn py_constructed_field_targets_child_score_not_decoy() -> Result<(), TestError> {
    const SOURCE: &[u8] = b"\
class Decoy:
    score = 9

class Child:
    score = 1

def read():
    return Child().score
";
    let env = inherited_fixture_env()?;
    let toolchain = env.toolchain()?;
    let cancelled = AtomicBool::new(false);
    let fragment = compile_inherited_fragment(SOURCE, &env.work, &toolchain, &cancelled)?;
    let view = fragment.view()?;
    let read_owner = entity_ordinal_at_index(&view, b"read", EntityKind::Function, 0)?;
    let decoy_score = entity_ordinal_at_index(&view, b"score", EntityKind::Field, 0)?;
    let child_score = entity_ordinal_at_index(&view, b"score", EntityKind::Field, 1)?;
    let calls = read_method_calls(&view, read_owner)?;
    if !calls.is_empty() {
        return Err(TestError::Falsified("read owns zero MethodCalls"));
    }
    let reads = read_field_accesses(&view, read_owner)?;
    if reads.len() != 1 {
        return Err(TestError::Falsified("read owns one FieldAccess"));
    }
    assert_local_index(reads[0], child_score, "Child().score resolves to Child.score")?;
    if matches!(
        &reads[0].occurrence.target,
        OccurrenceTarget::Local(target) if target.raw == decoy_score
    ) {
        return Err(TestError::Falsified("Child().score must not resolve to Decoy.score"));
    }
    fs::remove_dir_all(&env.work).map_err(|source| TestError::Io("remove scratch", source))?;
    Ok(())
}

#[test]
fn py_constructed_inherited_call_targets_base_note_not_decoy() -> Result<(), TestError> {
    const SOURCE: &[u8] = b"\
class Decoy:
    def note(self):
        return 0

class Base:
    def note(self):
        return 1

class Child(Base):
    pass

def read():
    return Child().note()
";
    let env = inherited_fixture_env()?;
    let toolchain = env.toolchain()?;
    let cancelled = AtomicBool::new(false);
    let fragment = compile_inherited_fragment(SOURCE, &env.work, &toolchain, &cancelled)?;
    let view = fragment.view()?;
    let read_owner = entity_ordinal_at_index(&view, b"read", EntityKind::Function, 0)?;
    let decoy_note = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 0)?;
    let base_note = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 1)?;
    let calls = read_method_calls(&view, read_owner)?;
    if calls.len() != 1 {
        return Err(TestError::Falsified("read owns one MethodCall"));
    }
    assert_local_index(calls[0], base_note, "Child().note() resolves to Base.note")?;
    if matches!(
        &calls[0].occurrence.target,
        OccurrenceTarget::Local(target) if target.raw == decoy_note
    ) {
        return Err(TestError::Falsified("Child().note() must not resolve to Decoy.note"));
    }
    let reads = read_field_accesses(&view, read_owner)?;
    if !reads.is_empty() {
        return Err(TestError::Falsified("read owns zero FieldAccess rows"));
    }
    fs::remove_dir_all(&env.work).map_err(|source| TestError::Io("remove scratch", source))?;
    Ok(())
}

#[test]
fn py_constructed_field_wins_over_method() -> Result<(), TestError> {
    const SOURCE: &[u8] = b"\
class Child:
    note = 1

    def note(self):
        return self.note

def read():
    return Child().note
";
    let env = inherited_fixture_env()?;
    let toolchain = env.toolchain()?;
    let cancelled = AtomicBool::new(false);
    let fragment = compile_inherited_fragment(SOURCE, &env.work, &toolchain, &cancelled)?;
    let view = fragment.view()?;
    let read_owner = entity_ordinal_at_index(&view, b"read", EntityKind::Function, 0)?;
    let note_field = entity_ordinal_at_index(&view, b"note", EntityKind::Field, 0)?;
    let note_method = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 0)?;
    let calls = read_method_calls(&view, read_owner)?;
    if !calls.is_empty() {
        return Err(TestError::Falsified("read owns zero MethodCalls"));
    }
    let reads = read_field_accesses(&view, read_owner)?;
    if reads.len() != 1 {
        return Err(TestError::Falsified("read owns one FieldAccess"));
    }
    assert_local_index(reads[0], note_field, "Child().note resolves to Child.note field")?;
    if matches!(
        &reads[0].occurrence.target,
        OccurrenceTarget::Local(target) if target.raw == note_method
    ) {
        return Err(TestError::Falsified("Child().note must not resolve to Child.note method"));
    }
    fs::remove_dir_all(&env.work).map_err(|source| TestError::Io("remove scratch", source))?;
    Ok(())
}

#[test]
fn py_constructed_call_binds_method_not_field() -> Result<(), TestError> {
    const SOURCE: &[u8] = b"\
class Child:
    note = 1

    def note(self):
        return self.note

def read():
    return Child().note()
";
    let env = inherited_fixture_env()?;
    let toolchain = env.toolchain()?;
    let cancelled = AtomicBool::new(false);
    let fragment = compile_inherited_fragment(SOURCE, &env.work, &toolchain, &cancelled)?;
    let view = fragment.view()?;
    let read_owner = entity_ordinal_at_index(&view, b"read", EntityKind::Function, 0)?;
    let note_field = entity_ordinal_at_index(&view, b"note", EntityKind::Field, 0)?;
    let note_method = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 0)?;
    let calls = read_method_calls(&view, read_owner)?;
    if calls.len() != 1 {
        return Err(TestError::Falsified("read owns one MethodCall"));
    }
    assert_local_index(calls[0], note_method, "Child().note() resolves to Child.note method")?;
    if matches!(
        &calls[0].occurrence.target,
        OccurrenceTarget::Local(target) if target.raw == note_field
    ) {
        return Err(TestError::Falsified("Child().note() must not resolve to Child.note field"));
    }
    let reads = read_field_accesses(&view, read_owner)?;
    if !reads.is_empty() {
        return Err(TestError::Falsified("read owns zero FieldAccess rows"));
    }
    fs::remove_dir_all(&env.work).map_err(|source| TestError::Io("remove scratch", source))?;
    Ok(())
}

#[test]
fn py_constructed_factory_stays_universe() -> Result<(), TestError> {
    const SOURCE: &[u8] = b"\
def factory():
    return 0

class Child:
    def note(self):
        return 1

def read():
    return factory().note()
";
    let env = inherited_fixture_env()?;
    let toolchain = env.toolchain()?;
    let cancelled = AtomicBool::new(false);
    let fragment = compile_inherited_fragment(SOURCE, &env.work, &toolchain, &cancelled)?;
    let view = fragment.view()?;
    let read_owner = entity_ordinal_at_index(&view, b"read", EntityKind::Function, 0)?;
    let child_note = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 0)?;
    let calls = read_method_calls(&view, read_owner)?;
    if calls.len() != 1 {
        return Err(TestError::Falsified("read owns one MethodCall"));
    }
    assert_universe_method(calls[0], "factory().note() stays a pypi universe key")?;
    if calls[0].occurrence.confidence != OccurrenceConfidence::Index {
        return Err(TestError::Falsified("factory().note() universe method confidence is Index"));
    }
    if matches!(
        &calls[0].occurrence.target,
        OccurrenceTarget::Local(target) if target.raw == child_note
    ) {
        return Err(TestError::Falsified("factory().note() must not resolve to Child.note"));
    }
    fs::remove_dir_all(&env.work).map_err(|source| TestError::Io("remove scratch", source))?;
    Ok(())
}

#[test]
fn py_constructed_missing_member_stays_attribute_key() -> Result<(), TestError> {
    const SOURCE: &[u8] = b"\
class Decoy:
    def note(self):
        return 0

class Child:
    pass

def read():
    return Child().note()
";
    let env = inherited_fixture_env()?;
    let toolchain = env.toolchain()?;
    let cancelled = AtomicBool::new(false);
    let fragment = compile_inherited_fragment(SOURCE, &env.work, &toolchain, &cancelled)?;
    let view = fragment.view()?;
    let read_owner = entity_ordinal_at_index(&view, b"read", EntityKind::Function, 0)?;
    let decoy_note = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 0)?;
    let calls = read_method_calls(&view, read_owner)?;
    if calls.len() != 1 {
        return Err(TestError::Falsified("read owns one MethodCall"));
    }
    assert_universe_method(calls[0], "Child().note() stays an attribute universe key")?;
    if calls[0].occurrence.confidence != OccurrenceConfidence::Index {
        return Err(TestError::Falsified("Child().note() universe confidence is Index"));
    }
    if matches!(
        &calls[0].occurrence.target,
        OccurrenceTarget::Local(target) if target.raw == decoy_note
    ) {
        return Err(TestError::Falsified("Child().note() must not resolve to Decoy.note"));
    }
    fs::remove_dir_all(&env.work).map_err(|source| TestError::Io("remove scratch", source))?;
    Ok(())
}

#[test]
fn py_constructed_dotted_stays_universe() -> Result<(), TestError> {
    const SOURCE: &[u8] = b"\
import pkg

class Child:
    def note(self):
        return 1

def read():
    return pkg.Child().note()
";
    let env = inherited_fixture_env()?;
    let toolchain = env.toolchain()?;
    let cancelled = AtomicBool::new(false);
    let fragment = compile_inherited_fragment(SOURCE, &env.work, &toolchain, &cancelled)?;
    let view = fragment.view()?;
    let read_owner = entity_ordinal_at_index(&view, b"read", EntityKind::Function, 0)?;
    let child_note = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 0)?;
    let calls = read_method_calls(&view, read_owner)?;
    if calls.len() != 2 {
        return Err(TestError::Falsified("read owns two MethodCalls"));
    }
    assert_universe_method(calls[0], "pkg.Child().note() stays a pypi universe key")?;
    if calls[0].occurrence.confidence != OccurrenceConfidence::Index {
        return Err(TestError::Falsified("pkg.Child().note() universe method confidence is Index"));
    }
    if matches!(
        &calls[0].occurrence.target,
        OccurrenceTarget::Local(target) if target.raw == child_note
    ) {
        return Err(TestError::Falsified("pkg.Child().note() must not resolve to Child.note"));
    }
    fs::remove_dir_all(&env.work).map_err(|source| TestError::Io("remove scratch", source))?;
    Ok(())
}

#[test]
fn py_class_qualified_missing_member_stays_attribute_key() -> Result<(), TestError> {
    const SOURCE: &[u8] = b"\
class Decoy:
    def note(self):
        return 0

class Child:
    pass

def read():
    return Child.note()
";
    let env = inherited_fixture_env()?;
    let toolchain = env.toolchain()?;
    let cancelled = AtomicBool::new(false);
    let fragment = compile_inherited_fragment(SOURCE, &env.work, &toolchain, &cancelled)?;
    let view = fragment.view()?;
    let read_owner = entity_ordinal_at_index(&view, b"read", EntityKind::Function, 0)?;
    let decoy_note = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 0)?;
    let calls = read_method_calls(&view, read_owner)?;
    if calls.len() != 1 {
        return Err(TestError::Falsified("read owns one MethodCall"));
    }
    assert_universe_method(calls[0], "Child.note() stays an attribute universe key")?;
    if calls[0].occurrence.confidence != OccurrenceConfidence::Index {
        return Err(TestError::Falsified("Child.note() universe confidence is Index"));
    }
    if matches!(
        &calls[0].occurrence.target,
        OccurrenceTarget::Local(target) if target.raw == decoy_note
    ) {
        return Err(TestError::Falsified("Child.note() must not resolve to Decoy.note"));
    }
    fs::remove_dir_all(&env.work).map_err(|source| TestError::Io("remove scratch", source))?;
    Ok(())
}

#[test]
fn py_subscript_constructed_call_targets_child_note_not_decoy() -> Result<(), TestError> {
    const SOURCE: &[u8] = b"\
class Decoy:
    def note(self):
        return 0

class Child:
    def note(self):
        return 1

def read():
    return Child[int]().note()
";
    let env = inherited_fixture_env()?;
    let toolchain = env.toolchain()?;
    let cancelled = AtomicBool::new(false);
    let fragment = compile_inherited_fragment(SOURCE, &env.work, &toolchain, &cancelled)?;
    let view = fragment.view()?;
    let read_owner = entity_ordinal_at_index(&view, b"read", EntityKind::Function, 0)?;
    let decoy_note = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 0)?;
    let child_note = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 1)?;
    let calls = read_method_calls(&view, read_owner)?;
    if calls.len() != 1 {
        return Err(TestError::Falsified("read owns one MethodCall"));
    }
    assert_local_index(calls[0], child_note, "Child[int]().note() resolves to Child.note")?;
    if matches!(
        &calls[0].occurrence.target,
        OccurrenceTarget::Local(target) if target.raw == decoy_note
    ) {
        return Err(TestError::Falsified("Child[int]().note() must not resolve to Decoy.note"));
    }
    let reads = read_field_accesses(&view, read_owner)?;
    if !reads.is_empty() {
        return Err(TestError::Falsified("read owns zero FieldAccess rows"));
    }
    fs::remove_dir_all(&env.work).map_err(|source| TestError::Io("remove scratch", source))?;
    Ok(())
}

#[test]
fn py_subscript_constructed_forward_call_targets_child_note_not_decoy() -> Result<(), TestError> {
    const SOURCE: &[u8] = b"\
def read():
    return Child[int]().note()

class Decoy:
    def note(self):
        return 0

class Child:
    def note(self):
        return 1
";
    let env = inherited_fixture_env()?;
    let toolchain = env.toolchain()?;
    let cancelled = AtomicBool::new(false);
    let fragment = compile_inherited_fragment(SOURCE, &env.work, &toolchain, &cancelled)?;
    let view = fragment.view()?;
    let read_owner = entity_ordinal_at_index(&view, b"read", EntityKind::Function, 0)?;
    let decoy_note = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 0)?;
    let child_note = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 1)?;
    let calls = read_method_calls(&view, read_owner)?;
    if calls.len() != 1 {
        return Err(TestError::Falsified("read owns one MethodCall"));
    }
    assert_local_index(calls[0], child_note, "Child[int]().note() resolves to Child.note")?;
    if matches!(
        &calls[0].occurrence.target,
        OccurrenceTarget::Local(target) if target.raw == decoy_note
    ) {
        return Err(TestError::Falsified("Child[int]().note() must not resolve to Decoy.note"));
    }
    let reads = read_field_accesses(&view, read_owner)?;
    if !reads.is_empty() {
        return Err(TestError::Falsified("read owns zero FieldAccess rows"));
    }
    fs::remove_dir_all(&env.work).map_err(|source| TestError::Io("remove scratch", source))?;
    Ok(())
}

#[test]
fn py_subscript_constructed_value_targets_child_note_not_decoy() -> Result<(), TestError> {
    const SOURCE: &[u8] = b"\
class Decoy:
    def note(self):
        return 0

class Child:
    def note(self):
        return 1

def read():
    return Child[int]().note
";
    let env = inherited_fixture_env()?;
    let toolchain = env.toolchain()?;
    let cancelled = AtomicBool::new(false);
    let fragment = compile_inherited_fragment(SOURCE, &env.work, &toolchain, &cancelled)?;
    let view = fragment.view()?;
    let read_owner = entity_ordinal_at_index(&view, b"read", EntityKind::Function, 0)?;
    let decoy_note = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 0)?;
    let child_note = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 1)?;
    let calls = read_method_calls(&view, read_owner)?;
    if !calls.is_empty() {
        return Err(TestError::Falsified("read owns zero MethodCalls"));
    }
    let reads = read_field_accesses(&view, read_owner)?;
    if reads.len() != 1 {
        return Err(TestError::Falsified("read owns one FieldAccess"));
    }
    assert_local_index(reads[0], child_note, "Child[int]().note resolves to Child.note")?;
    if matches!(
        &reads[0].occurrence.target,
        OccurrenceTarget::Local(target) if target.raw == decoy_note
    ) {
        return Err(TestError::Falsified("Child[int]().note must not resolve to Decoy.note"));
    }
    fs::remove_dir_all(&env.work).map_err(|source| TestError::Io("remove scratch", source))?;
    Ok(())
}

#[test]
fn py_subscript_constructed_field_targets_child_score_not_decoy() -> Result<(), TestError> {
    const SOURCE: &[u8] = b"\
class Decoy:
    score = 9

class Child:
    score = 1

def read():
    return Child[int]().score
";
    let env = inherited_fixture_env()?;
    let toolchain = env.toolchain()?;
    let cancelled = AtomicBool::new(false);
    let fragment = compile_inherited_fragment(SOURCE, &env.work, &toolchain, &cancelled)?;
    let view = fragment.view()?;
    let read_owner = entity_ordinal_at_index(&view, b"read", EntityKind::Function, 0)?;
    let decoy_score = entity_ordinal_at_index(&view, b"score", EntityKind::Field, 0)?;
    let child_score = entity_ordinal_at_index(&view, b"score", EntityKind::Field, 1)?;
    let calls = read_method_calls(&view, read_owner)?;
    if !calls.is_empty() {
        return Err(TestError::Falsified("read owns zero MethodCalls"));
    }
    let reads = read_field_accesses(&view, read_owner)?;
    if reads.len() != 1 {
        return Err(TestError::Falsified("read owns one FieldAccess"));
    }
    assert_local_index(reads[0], child_score, "Child[int]().score resolves to Child.score")?;
    if matches!(
        &reads[0].occurrence.target,
        OccurrenceTarget::Local(target) if target.raw == decoy_score
    ) {
        return Err(TestError::Falsified("Child[int]().score must not resolve to Decoy.score"));
    }
    fs::remove_dir_all(&env.work).map_err(|source| TestError::Io("remove scratch", source))?;
    Ok(())
}

#[test]
fn py_subscript_constructed_inherited_call_targets_base_note_not_decoy() -> Result<(), TestError> {
    const SOURCE: &[u8] = b"\
class Decoy:
    def note(self):
        return 0

class Base:
    def note(self):
        return 1

class Child(Base):
    pass

def read():
    return Child[int]().note()
";
    let env = inherited_fixture_env()?;
    let toolchain = env.toolchain()?;
    let cancelled = AtomicBool::new(false);
    let fragment = compile_inherited_fragment(SOURCE, &env.work, &toolchain, &cancelled)?;
    let view = fragment.view()?;
    let read_owner = entity_ordinal_at_index(&view, b"read", EntityKind::Function, 0)?;
    let decoy_note = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 0)?;
    let base_note = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 1)?;
    let calls = read_method_calls(&view, read_owner)?;
    if calls.len() != 1 {
        return Err(TestError::Falsified("read owns one MethodCall"));
    }
    assert_local_index(calls[0], base_note, "Child[int]().note() resolves to Base.note")?;
    if matches!(
        &calls[0].occurrence.target,
        OccurrenceTarget::Local(target) if target.raw == decoy_note
    ) {
        return Err(TestError::Falsified("Child[int]().note() must not resolve to Decoy.note"));
    }
    let reads = read_field_accesses(&view, read_owner)?;
    if !reads.is_empty() {
        return Err(TestError::Falsified("read owns zero FieldAccess rows"));
    }
    fs::remove_dir_all(&env.work).map_err(|source| TestError::Io("remove scratch", source))?;
    Ok(())
}

#[test]
fn py_subscript_constructed_field_wins_over_method() -> Result<(), TestError> {
    const SOURCE: &[u8] = b"\
class Child:
    score = 1

    def score(self):
        return self.score

def read():
    return Child[int]().score
";
    let env = inherited_fixture_env()?;
    let toolchain = env.toolchain()?;
    let cancelled = AtomicBool::new(false);
    let fragment = compile_inherited_fragment(SOURCE, &env.work, &toolchain, &cancelled)?;
    let view = fragment.view()?;
    let read_owner = entity_ordinal_at_index(&view, b"read", EntityKind::Function, 0)?;
    let score_field = entity_ordinal_at_index(&view, b"score", EntityKind::Field, 0)?;
    let score_method = entity_ordinal_at_index(&view, b"score", EntityKind::Function, 0)?;
    let calls = read_method_calls(&view, read_owner)?;
    if !calls.is_empty() {
        return Err(TestError::Falsified("read owns zero MethodCalls"));
    }
    let reads = read_field_accesses(&view, read_owner)?;
    if reads.len() != 1 {
        return Err(TestError::Falsified("read owns one FieldAccess"));
    }
    assert_local_index(reads[0], score_field, "Child[int]().score resolves to Child.score field")?;
    if matches!(
        &reads[0].occurrence.target,
        OccurrenceTarget::Local(target) if target.raw == score_method
    ) {
        return Err(TestError::Falsified("Child[int]().score must not resolve to Child.score method"));
    }
    fs::remove_dir_all(&env.work).map_err(|source| TestError::Io("remove scratch", source))?;
    Ok(())
}

#[test]
fn py_subscript_constructed_call_binds_method_not_field() -> Result<(), TestError> {
    const SOURCE: &[u8] = b"\
class Child:
    note = 1

    def note(self):
        return self.note

def read():
    return Child[int]().note()
";
    let env = inherited_fixture_env()?;
    let toolchain = env.toolchain()?;
    let cancelled = AtomicBool::new(false);
    let fragment = compile_inherited_fragment(SOURCE, &env.work, &toolchain, &cancelled)?;
    let view = fragment.view()?;
    let read_owner = entity_ordinal_at_index(&view, b"read", EntityKind::Function, 0)?;
    let note_field = entity_ordinal_at_index(&view, b"note", EntityKind::Field, 0)?;
    let note_method = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 0)?;
    let calls = read_method_calls(&view, read_owner)?;
    if calls.len() != 1 {
        return Err(TestError::Falsified("read owns one MethodCall"));
    }
    assert_local_index(calls[0], note_method, "Child[int]().note() resolves to Child.note method")?;
    if matches!(
        &calls[0].occurrence.target,
        OccurrenceTarget::Local(target) if target.raw == note_field
    ) {
        return Err(TestError::Falsified("Child[int]().note() must not resolve to Child.note field"));
    }
    let reads = read_field_accesses(&view, read_owner)?;
    if !reads.is_empty() {
        return Err(TestError::Falsified("read owns zero FieldAccess rows"));
    }
    fs::remove_dir_all(&env.work).map_err(|source| TestError::Io("remove scratch", source))?;
    Ok(())
}

#[test]
fn py_subscript_nested_call_targets_child_note_not_decoy() -> Result<(), TestError> {
    const SOURCE: &[u8] = b"\
class Decoy:
    def note(self):
        return 0

class Child:
    def note(self):
        return 1

def read():
    return Child[int][str]().note()
";
    let env = inherited_fixture_env()?;
    let toolchain = env.toolchain()?;
    let cancelled = AtomicBool::new(false);
    let fragment = compile_inherited_fragment(SOURCE, &env.work, &toolchain, &cancelled)?;
    let view = fragment.view()?;
    let read_owner = entity_ordinal_at_index(&view, b"read", EntityKind::Function, 0)?;
    let decoy_note = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 0)?;
    let child_note = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 1)?;
    let calls = read_method_calls(&view, read_owner)?;
    if calls.len() != 1 {
        return Err(TestError::Falsified("read owns one MethodCall"));
    }
    assert_local_index(calls[0], child_note, "Child[int][str]().note() resolves to Child.note")?;
    if matches!(
        &calls[0].occurrence.target,
        OccurrenceTarget::Local(target) if target.raw == decoy_note
    ) {
        return Err(TestError::Falsified("Child[int][str]().note() must not resolve to Decoy.note"));
    }
    let reads = read_field_accesses(&view, read_owner)?;
    if !reads.is_empty() {
        return Err(TestError::Falsified("read owns zero FieldAccess rows"));
    }
    fs::remove_dir_all(&env.work).map_err(|source| TestError::Io("remove scratch", source))?;
    Ok(())
}

#[test]
fn py_subscript_tuple_slice_call_targets_child_note_not_decoy() -> Result<(), TestError> {
    const SOURCE: &[u8] = b"\
class Decoy:
    def note(self):
        return 0

class Child:
    def note(self):
        return 1

def read():
    return Child[int, str]().note()
";
    let env = inherited_fixture_env()?;
    let toolchain = env.toolchain()?;
    let cancelled = AtomicBool::new(false);
    let fragment = compile_inherited_fragment(SOURCE, &env.work, &toolchain, &cancelled)?;
    let view = fragment.view()?;
    let read_owner = entity_ordinal_at_index(&view, b"read", EntityKind::Function, 0)?;
    let decoy_note = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 0)?;
    let child_note = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 1)?;
    let calls = read_method_calls(&view, read_owner)?;
    if calls.len() != 1 {
        return Err(TestError::Falsified("read owns one MethodCall"));
    }
    assert_local_index(calls[0], child_note, "Child[int, str]().note() resolves to Child.note")?;
    if matches!(
        &calls[0].occurrence.target,
        OccurrenceTarget::Local(target) if target.raw == decoy_note
    ) {
        return Err(TestError::Falsified("Child[int, str]().note() must not resolve to Decoy.note"));
    }
    let reads = read_field_accesses(&view, read_owner)?;
    if !reads.is_empty() {
        return Err(TestError::Falsified("read owns zero FieldAccess rows"));
    }
    fs::remove_dir_all(&env.work).map_err(|source| TestError::Io("remove scratch", source))?;
    Ok(())
}

#[test]
fn py_subscript_pep695_call_targets_child_note() -> Result<(), TestError> {
    const SOURCE: &[u8] = b"\
class Decoy:
    def note(self):
        return 0

class Child[T]:
    def note(self):
        return 1

def read():
    return Child[int]().note()
";
    let env = inherited_fixture_env()?;
    let toolchain = env.toolchain()?;
    let cancelled = AtomicBool::new(false);
    let fragment = compile_inherited_fragment(SOURCE, &env.work, &toolchain, &cancelled)?;
    let view = fragment.view()?;
    let read_owner = entity_ordinal_at_index(&view, b"read", EntityKind::Function, 0)?;
    let decoy_note = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 0)?;
    let child_note = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 1)?;
    let calls = read_method_calls(&view, read_owner)?;
    if calls.len() != 1 {
        return Err(TestError::Falsified("read owns one MethodCall"));
    }
    assert_local_index(calls[0], child_note, "Child[int]().note() resolves to Child.note")?;
    if matches!(
        &calls[0].occurrence.target,
        OccurrenceTarget::Local(target) if target.raw == decoy_note
    ) {
        return Err(TestError::Falsified("Child[int]().note() must not resolve to Decoy.note"));
    }
    let reads = read_field_accesses(&view, read_owner)?;
    if !reads.is_empty() {
        return Err(TestError::Falsified("read owns zero FieldAccess rows"));
    }
    fs::remove_dir_all(&env.work).map_err(|source| TestError::Io("remove scratch", source))?;
    Ok(())
}

#[test]
fn py_subscript_qualified_call_targets_child_note_not_decoy() -> Result<(), TestError> {
    const SOURCE: &[u8] = b"\
class Decoy:
    def note(self):
        return 0

class Child:
    def note(self):
        return 1

def read():
    return Child[int].note()
";
    let env = inherited_fixture_env()?;
    let toolchain = env.toolchain()?;
    let cancelled = AtomicBool::new(false);
    let fragment = compile_inherited_fragment(SOURCE, &env.work, &toolchain, &cancelled)?;
    let view = fragment.view()?;
    let read_owner = entity_ordinal_at_index(&view, b"read", EntityKind::Function, 0)?;
    let decoy_note = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 0)?;
    let child_note = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 1)?;
    let calls = read_method_calls(&view, read_owner)?;
    if calls.len() != 1 {
        return Err(TestError::Falsified("read owns one MethodCall"));
    }
    assert_local_index(calls[0], child_note, "Child[int].note() resolves to Child.note")?;
    if matches!(
        &calls[0].occurrence.target,
        OccurrenceTarget::Local(target) if target.raw == decoy_note
    ) {
        return Err(TestError::Falsified("Child[int].note() must not resolve to Decoy.note"));
    }
    let reads = read_field_accesses(&view, read_owner)?;
    if !reads.is_empty() {
        return Err(TestError::Falsified("read owns zero FieldAccess rows"));
    }
    fs::remove_dir_all(&env.work).map_err(|source| TestError::Io("remove scratch", source))?;
    Ok(())
}

#[test]
fn py_subscript_qualified_field_targets_child_score_not_decoy() -> Result<(), TestError> {
    const SOURCE: &[u8] = b"\
class Decoy:
    score = 9

class Child:
    score = 1

def read():
    return Child[int].score
";
    let env = inherited_fixture_env()?;
    let toolchain = env.toolchain()?;
    let cancelled = AtomicBool::new(false);
    let fragment = compile_inherited_fragment(SOURCE, &env.work, &toolchain, &cancelled)?;
    let view = fragment.view()?;
    let read_owner = entity_ordinal_at_index(&view, b"read", EntityKind::Function, 0)?;
    let decoy_score = entity_ordinal_at_index(&view, b"score", EntityKind::Field, 0)?;
    let child_score = entity_ordinal_at_index(&view, b"score", EntityKind::Field, 1)?;
    let calls = read_method_calls(&view, read_owner)?;
    if !calls.is_empty() {
        return Err(TestError::Falsified("read owns zero MethodCalls"));
    }
    let reads = read_field_accesses(&view, read_owner)?;
    if reads.len() != 1 {
        return Err(TestError::Falsified("read owns one FieldAccess"));
    }
    assert_local_index(reads[0], child_score, "Child[int].score resolves to Child.score")?;
    if matches!(
        &reads[0].occurrence.target,
        OccurrenceTarget::Local(target) if target.raw == decoy_score
    ) {
        return Err(TestError::Falsified("Child[int].score must not resolve to Decoy.score"));
    }
    fs::remove_dir_all(&env.work).map_err(|source| TestError::Io("remove scratch", source))?;
    Ok(())
}

#[test]
fn py_subscript_qualified_value_targets_child_note_not_decoy() -> Result<(), TestError> {
    const SOURCE: &[u8] = b"\
class Decoy:
    def note(self):
        return 0

class Child:
    def note(self):
        return 1

def read():
    return Child[int].note
";
    let env = inherited_fixture_env()?;
    let toolchain = env.toolchain()?;
    let cancelled = AtomicBool::new(false);
    let fragment = compile_inherited_fragment(SOURCE, &env.work, &toolchain, &cancelled)?;
    let view = fragment.view()?;
    let read_owner = entity_ordinal_at_index(&view, b"read", EntityKind::Function, 0)?;
    let decoy_note = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 0)?;
    let child_note = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 1)?;
    let calls = read_method_calls(&view, read_owner)?;
    if !calls.is_empty() {
        return Err(TestError::Falsified("read owns zero MethodCalls"));
    }
    let reads = read_field_accesses(&view, read_owner)?;
    if reads.len() != 1 {
        return Err(TestError::Falsified("read owns one FieldAccess"));
    }
    assert_local_index(reads[0], child_note, "Child[int].note resolves to Child.note")?;
    if matches!(
        &reads[0].occurrence.target,
        OccurrenceTarget::Local(target) if target.raw == decoy_note
    ) {
        return Err(TestError::Falsified("Child[int].note must not resolve to Decoy.note"));
    }
    fs::remove_dir_all(&env.work).map_err(|source| TestError::Io("remove scratch", source))?;
    Ok(())
}

#[test]
fn py_subscript_list_call_stays_universe() -> Result<(), TestError> {
    const SOURCE: &[u8] = b"\
class Child:
    def note(self):
        return 1

def read():
    return list[Child]().note()
";
    let env = inherited_fixture_env()?;
    let toolchain = env.toolchain()?;
    let cancelled = AtomicBool::new(false);
    let fragment = compile_inherited_fragment(SOURCE, &env.work, &toolchain, &cancelled)?;
    let view = fragment.view()?;
    let read_owner = entity_ordinal_at_index(&view, b"read", EntityKind::Function, 0)?;
    let child_note = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 0)?;
    let calls = read_method_calls(&view, read_owner)?;
    if calls.len() != 1 {
        return Err(TestError::Falsified("read owns one MethodCall"));
    }
    assert_universe_method(calls[0], "list[Child]().note() stays a pypi universe key")?;
    if calls[0].occurrence.confidence != OccurrenceConfidence::Index {
        return Err(TestError::Falsified("list[Child]().note() universe method confidence is Index"));
    }
    if matches!(
        &calls[0].occurrence.target,
        OccurrenceTarget::Local(target) if target.raw == child_note
    ) {
        return Err(TestError::Falsified("list[Child]().note() must not resolve to Child.note"));
    }
    fs::remove_dir_all(&env.work).map_err(|source| TestError::Io("remove scratch", source))?;
    Ok(())
}

#[test]
fn py_subscript_dotted_call_stays_universe() -> Result<(), TestError> {
    const SOURCE: &[u8] = b"\
import pkg

class Child:
    def note(self):
        return 1

def read():
    return pkg.Child[int]().note()
";
    let env = inherited_fixture_env()?;
    let toolchain = env.toolchain()?;
    let cancelled = AtomicBool::new(false);
    let fragment = compile_inherited_fragment(SOURCE, &env.work, &toolchain, &cancelled)?;
    let view = fragment.view()?;
    let read_owner = entity_ordinal_at_index(&view, b"read", EntityKind::Function, 0)?;
    let child_note = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 0)?;
    let calls = read_method_calls(&view, read_owner)?;
    if calls.len() != 1 {
        return Err(TestError::Falsified("read owns one MethodCall"));
    }
    assert_universe_method(calls[0], "pkg.Child[int]().note() stays a pypi universe key")?;
    if calls[0].occurrence.confidence != OccurrenceConfidence::Index {
        return Err(TestError::Falsified("pkg.Child[int]().note() universe method confidence is Index"));
    }
    if matches!(
        &calls[0].occurrence.target,
        OccurrenceTarget::Local(target) if target.raw == child_note
    ) {
        return Err(TestError::Falsified("pkg.Child[int]().note() must not resolve to Child.note"));
    }
    let reads = read_field_accesses(&view, read_owner)?;
    if reads.len() != 1 {
        return Err(TestError::Falsified("read owns one FieldAccess"));
    }
    if !matches!(
        &reads[0].occurrence.target,
        OccurrenceTarget::Foreign(key)
            if key.path == "pkg"
                && key.display == "Child"
                && key.kind == Some(EntityKind::Field)
                && matches!(
                    key.origin,
                    ForeignOrigin::Package(lineage)
                        if lineage.ecosystem == "pypi" && lineage.name == "pkg"
                )
    ) {
        return Err(TestError::Falsified(
            "pkg.Child[int]() records pkg.Child as a package field key",
        ));
    }
    if reads[0].occurrence.confidence != OccurrenceConfidence::Index {
        return Err(TestError::Falsified("pkg.Child package field confidence is Index"));
    }
    fs::remove_dir_all(&env.work).map_err(|source| TestError::Io("remove scratch", source))?;
    Ok(())
}

#[test]
fn py_subscript_factory_stays_universe() -> Result<(), TestError> {
    const SOURCE: &[u8] = b"\
def factory():
    return 0

class Child:
    def note(self):
        return 1

def read():
    return factory[int]().note()
";
    let env = inherited_fixture_env()?;
    let toolchain = env.toolchain()?;
    let cancelled = AtomicBool::new(false);
    let fragment = compile_inherited_fragment(SOURCE, &env.work, &toolchain, &cancelled)?;
    let view = fragment.view()?;
    let read_owner = entity_ordinal_at_index(&view, b"read", EntityKind::Function, 0)?;
    let child_note = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 0)?;
    let calls = read_method_calls(&view, read_owner)?;
    if calls.len() != 1 {
        return Err(TestError::Falsified("read owns one MethodCall"));
    }
    assert_universe_method(calls[0], "factory[int]().note() stays a pypi universe key")?;
    if calls[0].occurrence.confidence != OccurrenceConfidence::Index {
        return Err(TestError::Falsified("factory[int]().note() universe method confidence is Index"));
    }
    if matches!(
        &calls[0].occurrence.target,
        OccurrenceTarget::Local(target) if target.raw == child_note
    ) {
        return Err(TestError::Falsified("factory[int]().note() must not resolve to Child.note"));
    }
    fs::remove_dir_all(&env.work).map_err(|source| TestError::Io("remove scratch", source))?;
    Ok(())
}

#[test]
fn py_subscript_missing_member_stays_attribute_key() -> Result<(), TestError> {
    const SOURCE: &[u8] = b"\
class Decoy:
    def note(self):
        return 0

class Child:
    pass

def read():
    return Child[int]().note()
";
    let env = inherited_fixture_env()?;
    let toolchain = env.toolchain()?;
    let cancelled = AtomicBool::new(false);
    let fragment = compile_inherited_fragment(SOURCE, &env.work, &toolchain, &cancelled)?;
    let view = fragment.view()?;
    let read_owner = entity_ordinal_at_index(&view, b"read", EntityKind::Function, 0)?;
    let decoy_note = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 0)?;
    let calls = read_method_calls(&view, read_owner)?;
    if calls.len() != 1 {
        return Err(TestError::Falsified("read owns one MethodCall"));
    }
    assert_universe_method(calls[0], "Child[int]().note() stays an attribute universe key")?;
    if calls[0].occurrence.confidence != OccurrenceConfidence::Index {
        return Err(TestError::Falsified("Child[int]().note() universe confidence is Index"));
    }
    if matches!(
        &calls[0].occurrence.target,
        OccurrenceTarget::Local(target) if target.raw == decoy_note
    ) {
        return Err(TestError::Falsified("Child[int]().note() must not resolve to Decoy.note"));
    }
    fs::remove_dir_all(&env.work).map_err(|source| TestError::Io("remove scratch", source))?;
    Ok(())
}

#[test]
fn py_subscript_qualified_missing_stays_attribute_key() -> Result<(), TestError> {
    const SOURCE: &[u8] = b"\
class Decoy:
    def note(self):
        return 0

class Child:
    pass

def read():
    return Child[int].note()
";
    let env = inherited_fixture_env()?;
    let toolchain = env.toolchain()?;
    let cancelled = AtomicBool::new(false);
    let fragment = compile_inherited_fragment(SOURCE, &env.work, &toolchain, &cancelled)?;
    let view = fragment.view()?;
    let read_owner = entity_ordinal_at_index(&view, b"read", EntityKind::Function, 0)?;
    let decoy_note = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 0)?;
    let calls = read_method_calls(&view, read_owner)?;
    if calls.len() != 1 {
        return Err(TestError::Falsified("read owns one MethodCall"));
    }
    assert_universe_method(calls[0], "Child[int].note() stays an attribute universe key")?;
    if calls[0].occurrence.confidence != OccurrenceConfidence::Index {
        return Err(TestError::Falsified("Child[int].note() universe confidence is Index"));
    }
    if matches!(
        &calls[0].occurrence.target,
        OccurrenceTarget::Local(target) if target.raw == decoy_note
    ) {
        return Err(TestError::Falsified("Child[int].note() must not resolve to Decoy.note"));
    }
    fs::remove_dir_all(&env.work).map_err(|source| TestError::Io("remove scratch", source))?;
    Ok(())
}

#[test]
fn py_subscript_two_scopes_stay_universe() -> Result<(), TestError> {
    const SOURCE: &[u8] = b"\
class Child:
    def note(self):
        return 0

def nest():
    class Child:
        def note(self):
            return 1

def read():
    return Child[int]().note()
";
    let env = inherited_fixture_env()?;
    let toolchain = env.toolchain()?;
    let cancelled = AtomicBool::new(false);
    let fragment = compile_inherited_fragment(SOURCE, &env.work, &toolchain, &cancelled)?;
    let view = fragment.view()?;
    let read_owner = entity_ordinal_at_index(&view, b"read", EntityKind::Function, 0)?;
    let module_note = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 0)?;
    let nested_note = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 1)?;
    let calls = read_method_calls(&view, read_owner)?;
    if calls.len() != 1 {
        return Err(TestError::Falsified("read owns one MethodCall"));
    }
    assert_universe_method(calls[0], "Child[int]().note() stays a pypi universe key")?;
    if calls[0].occurrence.confidence != OccurrenceConfidence::Index {
        return Err(TestError::Falsified("Child[int]().note() universe method confidence is Index"));
    }
    if matches!(
        &calls[0].occurrence.target,
        OccurrenceTarget::Local(target) if target.raw == module_note
    ) {
        return Err(TestError::Falsified("Child[int]().note() must not resolve to module Child.note"));
    }
    if matches!(
        &calls[0].occurrence.target,
        OccurrenceTarget::Local(target) if target.raw == nested_note
    ) {
        return Err(TestError::Falsified("Child[int]().note() must not resolve to nested Child.note"));
    }
    fs::remove_dir_all(&env.work).map_err(|source| TestError::Io("remove scratch", source))?;
    Ok(())
}

#[test]
fn py_local_annotation_call_targets_child_note_not_decoy() -> Result<(), TestError> {
    const SOURCE: &[u8] = b"\
class Decoy:
    def note(self):
        return 0

class Child:
    def note(self):
        return 1

def read():
    obj: Child = Child()
    return obj.note()
";
    let env = inherited_fixture_env()?;
    let toolchain = env.toolchain()?;
    let cancelled = AtomicBool::new(false);
    let fragment = compile_inherited_fragment(SOURCE, &env.work, &toolchain, &cancelled)?;
    let view = fragment.view()?;
    let read_owner = entity_ordinal_at_index(&view, b"read", EntityKind::Function, 0)?;
    let decoy_note = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 0)?;
    let child_note = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 1)?;
    let calls = read_method_calls(&view, read_owner)?;
    if calls.len() != 1 {
        return Err(TestError::Falsified("read owns one MethodCall"));
    }
    assert_local_index(calls[0], child_note, "obj.note() resolves to Child.note")?;
    if matches!(
        &calls[0].occurrence.target,
        OccurrenceTarget::Local(target) if target.raw == decoy_note
    ) {
        return Err(TestError::Falsified("obj.note() must not resolve to Decoy.note"));
    }
    fs::remove_dir_all(&env.work).map_err(|source| TestError::Io("remove scratch", source))?;
    Ok(())
}

#[test]
fn py_local_annotation_forward_call_targets_child_note_not_decoy() -> Result<(), TestError> {
    const SOURCE: &[u8] = b"\
def read():
    obj: Child = Child()
    return obj.note()

class Decoy:
    def note(self):
        return 0

class Child:
    def note(self):
        return 1
";
    let env = inherited_fixture_env()?;
    let toolchain = env.toolchain()?;
    let cancelled = AtomicBool::new(false);
    let fragment = compile_inherited_fragment(SOURCE, &env.work, &toolchain, &cancelled)?;
    let view = fragment.view()?;
    let read_owner = entity_ordinal_at_index(&view, b"read", EntityKind::Function, 0)?;
    let decoy_note = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 0)?;
    let child_note = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 1)?;
    let calls = read_method_calls(&view, read_owner)?;
    if calls.len() != 1 {
        return Err(TestError::Falsified("read owns one MethodCall"));
    }
    assert_local_index(calls[0], child_note, "obj.note() resolves to Child.note")?;
    if matches!(
        &calls[0].occurrence.target,
        OccurrenceTarget::Local(target) if target.raw == decoy_note
    ) {
        return Err(TestError::Falsified("obj.note() must not resolve to Decoy.note"));
    }
    fs::remove_dir_all(&env.work).map_err(|source| TestError::Io("remove scratch", source))?;
    Ok(())
}

#[test]
fn py_local_annotation_value_targets_child_note_not_decoy() -> Result<(), TestError> {
    const SOURCE: &[u8] = b"\
class Decoy:
    def note(self):
        return 0

class Child:
    def note(self):
        return 1

def read():
    obj: Child = Child()
    return obj.note
";
    let env = inherited_fixture_env()?;
    let toolchain = env.toolchain()?;
    let cancelled = AtomicBool::new(false);
    let fragment = compile_inherited_fragment(SOURCE, &env.work, &toolchain, &cancelled)?;
    let view = fragment.view()?;
    let read_owner = entity_ordinal_at_index(&view, b"read", EntityKind::Function, 0)?;
    let decoy_note = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 0)?;
    let child_note = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 1)?;
    let calls = read_method_calls(&view, read_owner)?;
    if !calls.is_empty() {
        return Err(TestError::Falsified("read owns zero MethodCalls"));
    }
    let reads = read_field_accesses(&view, read_owner)?;
    if reads.len() != 1 {
        return Err(TestError::Falsified("read owns one FieldAccess"));
    }
    assert_local_index(reads[0], child_note, "obj.note resolves to Child.note")?;
    if matches!(
        &reads[0].occurrence.target,
        OccurrenceTarget::Local(target) if target.raw == decoy_note
    ) {
        return Err(TestError::Falsified("obj.note must not resolve to Decoy.note"));
    }
    fs::remove_dir_all(&env.work).map_err(|source| TestError::Io("remove scratch", source))?;
    Ok(())
}

#[test]
fn py_local_annotation_field_targets_child_score_not_decoy() -> Result<(), TestError> {
    const SOURCE: &[u8] = b"\
class Decoy:
    score = 9

class Child:
    score = 1

def read():
    obj: Child
    return obj.score
";
    let env = inherited_fixture_env()?;
    let toolchain = env.toolchain()?;
    let cancelled = AtomicBool::new(false);
    let fragment = compile_inherited_fragment(SOURCE, &env.work, &toolchain, &cancelled)?;
    let view = fragment.view()?;
    let read_owner = entity_ordinal_at_index(&view, b"read", EntityKind::Function, 0)?;
    let decoy_score = entity_ordinal_at_index(&view, b"score", EntityKind::Field, 0)?;
    let child_score = entity_ordinal_at_index(&view, b"score", EntityKind::Field, 1)?;
    let calls = read_method_calls(&view, read_owner)?;
    if !calls.is_empty() {
        return Err(TestError::Falsified("read owns zero MethodCalls"));
    }
    let reads = read_field_accesses(&view, read_owner)?;
    if reads.len() != 1 {
        return Err(TestError::Falsified("read owns one FieldAccess"));
    }
    assert_local_index(reads[0], child_score, "obj.score resolves to Child.score")?;
    if matches!(
        &reads[0].occurrence.target,
        OccurrenceTarget::Local(target) if target.raw == decoy_score
    ) {
        return Err(TestError::Falsified("obj.score must not resolve to Decoy.score"));
    }
    fs::remove_dir_all(&env.work).map_err(|source| TestError::Io("remove scratch", source))?;
    Ok(())
}

#[test]
fn py_local_annotation_inherited_call_targets_base_note_not_decoy() -> Result<(), TestError> {
    const SOURCE: &[u8] = b"\
class Decoy:
    def note(self):
        return 0

class Base:
    def note(self):
        return 1

class Child(Base):
    pass

def read():
    obj: Child
    return obj.note()
";
    let env = inherited_fixture_env()?;
    let toolchain = env.toolchain()?;
    let cancelled = AtomicBool::new(false);
    let fragment = compile_inherited_fragment(SOURCE, &env.work, &toolchain, &cancelled)?;
    let view = fragment.view()?;
    let read_owner = entity_ordinal_at_index(&view, b"read", EntityKind::Function, 0)?;
    let decoy_note = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 0)?;
    let base_note = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 1)?;
    let calls = read_method_calls(&view, read_owner)?;
    if calls.len() != 1 {
        return Err(TestError::Falsified("read owns one MethodCall"));
    }
    assert_local_index(calls[0], base_note, "obj.note() resolves to Base.note")?;
    if matches!(
        &calls[0].occurrence.target,
        OccurrenceTarget::Local(target) if target.raw == decoy_note
    ) {
        return Err(TestError::Falsified("obj.note() must not resolve to Decoy.note"));
    }
    fs::remove_dir_all(&env.work).map_err(|source| TestError::Io("remove scratch", source))?;
    Ok(())
}

#[test]
fn py_local_annotation_field_wins_over_method() -> Result<(), TestError> {
    const SOURCE: &[u8] = b"\
class Child:
    score = 1

    def score(self):
        return self.score

def read():
    obj: Child
    return obj.score
";
    let env = inherited_fixture_env()?;
    let toolchain = env.toolchain()?;
    let cancelled = AtomicBool::new(false);
    let fragment = compile_inherited_fragment(SOURCE, &env.work, &toolchain, &cancelled)?;
    let view = fragment.view()?;
    let read_owner = entity_ordinal_at_index(&view, b"read", EntityKind::Function, 0)?;
    let score_field = entity_ordinal_at_index(&view, b"score", EntityKind::Field, 0)?;
    let score_method = entity_ordinal_at_index(&view, b"score", EntityKind::Function, 0)?;
    let calls = read_method_calls(&view, read_owner)?;
    if !calls.is_empty() {
        return Err(TestError::Falsified("read owns zero MethodCalls"));
    }
    let reads = read_field_accesses(&view, read_owner)?;
    if reads.len() != 1 {
        return Err(TestError::Falsified("read owns one FieldAccess"));
    }
    assert_local_index(reads[0], score_field, "obj.score resolves to Child.score field")?;
    if matches!(
        &reads[0].occurrence.target,
        OccurrenceTarget::Local(target) if target.raw == score_method
    ) {
        return Err(TestError::Falsified("obj.score must not resolve to Child.score method"));
    }
    fs::remove_dir_all(&env.work).map_err(|source| TestError::Io("remove scratch", source))?;
    Ok(())
}

#[test]
fn py_local_annotation_call_binds_method_not_field() -> Result<(), TestError> {
    const SOURCE: &[u8] = b"\
class Child:
    note = 1

    def note(self):
        return 0

def read():
    obj: Child
    return obj.note()
";
    let env = inherited_fixture_env()?;
    let toolchain = env.toolchain()?;
    let cancelled = AtomicBool::new(false);
    let fragment = compile_inherited_fragment(SOURCE, &env.work, &toolchain, &cancelled)?;
    let view = fragment.view()?;
    let read_owner = entity_ordinal_at_index(&view, b"read", EntityKind::Function, 0)?;
    let note_field = entity_ordinal_at_index(&view, b"note", EntityKind::Field, 0)?;
    let note_method = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 0)?;
    let calls = read_method_calls(&view, read_owner)?;
    if calls.len() != 1 {
        return Err(TestError::Falsified("read owns one MethodCall"));
    }
    assert_local_index(calls[0], note_method, "obj.note() resolves to Child.note method")?;
    if matches!(
        &calls[0].occurrence.target,
        OccurrenceTarget::Local(target) if target.raw == note_field
    ) {
        return Err(TestError::Falsified("obj.note() must not resolve to Child.note field"));
    }
    fs::remove_dir_all(&env.work).map_err(|source| TestError::Io("remove scratch", source))?;
    Ok(())
}

#[test]
fn py_local_annotation_generic_call_targets_child_note_not_decoy() -> Result<(), TestError> {
    const SOURCE: &[u8] = b"\
class Decoy:
    def note(self):
        return 0

class Child:
    def note(self):
        return 1

def read():
    obj: Child[int] = Child()
    return obj.note()
";
    let env = inherited_fixture_env()?;
    let toolchain = env.toolchain()?;
    let cancelled = AtomicBool::new(false);
    let fragment = compile_inherited_fragment(SOURCE, &env.work, &toolchain, &cancelled)?;
    let view = fragment.view()?;
    let read_owner = entity_ordinal_at_index(&view, b"read", EntityKind::Function, 0)?;
    let decoy_note = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 0)?;
    let child_note = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 1)?;
    let calls = read_method_calls(&view, read_owner)?;
    if calls.len() != 1 {
        return Err(TestError::Falsified("read owns one MethodCall"));
    }
    assert_local_index(calls[0], child_note, "obj.note() resolves to Child.note")?;
    if matches!(
        &calls[0].occurrence.target,
        OccurrenceTarget::Local(target) if target.raw == decoy_note
    ) {
        return Err(TestError::Falsified("obj.note() must not resolve to Decoy.note"));
    }
    fs::remove_dir_all(&env.work).map_err(|source| TestError::Io("remove scratch", source))?;
    Ok(())
}

#[test]
fn py_local_annotation_union_call_targets_child_note_not_decoy() -> Result<(), TestError> {
    const SOURCE: &[u8] = b"\
class Decoy:
    def note(self):
        return 0

class Child:
    def note(self):
        return 1

def read():
    obj: Child | None = Child()
    return obj.note()
";
    let env = inherited_fixture_env()?;
    let toolchain = env.toolchain()?;
    let cancelled = AtomicBool::new(false);
    let fragment = compile_inherited_fragment(SOURCE, &env.work, &toolchain, &cancelled)?;
    let view = fragment.view()?;
    let read_owner = entity_ordinal_at_index(&view, b"read", EntityKind::Function, 0)?;
    let decoy_note = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 0)?;
    let child_note = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 1)?;
    let calls = read_method_calls(&view, read_owner)?;
    if calls.len() != 1 {
        return Err(TestError::Falsified("read owns one MethodCall"));
    }
    assert_local_index(calls[0], child_note, "obj.note() resolves to Child.note")?;
    if matches!(
        &calls[0].occurrence.target,
        OccurrenceTarget::Local(target) if target.raw == decoy_note
    ) {
        return Err(TestError::Falsified("obj.note() must not resolve to Decoy.note"));
    }
    fs::remove_dir_all(&env.work).map_err(|source| TestError::Io("remove scratch", source))?;
    Ok(())
}

#[test]
fn py_local_annotation_optional_call_targets_child_note_not_decoy() -> Result<(), TestError> {
    const SOURCE: &[u8] = b"\
class Decoy:
    def note(self):
        return 0

class Child:
    def note(self):
        return 1

def read():
    obj: Optional[Child] = Child()
    return obj.note()
";
    let env = inherited_fixture_env()?;
    let toolchain = env.toolchain()?;
    let cancelled = AtomicBool::new(false);
    let fragment = compile_inherited_fragment(SOURCE, &env.work, &toolchain, &cancelled)?;
    let view = fragment.view()?;
    let read_owner = entity_ordinal_at_index(&view, b"read", EntityKind::Function, 0)?;
    let decoy_note = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 0)?;
    let child_note = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 1)?;
    let calls = read_method_calls(&view, read_owner)?;
    if calls.len() != 1 {
        return Err(TestError::Falsified("read owns one MethodCall"));
    }
    assert_local_index(calls[0], child_note, "obj.note() resolves to Child.note")?;
    if matches!(
        &calls[0].occurrence.target,
        OccurrenceTarget::Local(target) if target.raw == decoy_note
    ) {
        return Err(TestError::Falsified("obj.note() must not resolve to Decoy.note"));
    }
    fs::remove_dir_all(&env.work).map_err(|source| TestError::Io("remove scratch", source))?;
    Ok(())
}

#[test]
fn py_local_annotation_quoted_call_targets_child_note_not_decoy() -> Result<(), TestError> {
    const SOURCE: &[u8] = b"\
class Decoy:
    def note(self):
        return 0

class Child:
    def note(self):
        return 1

def read():
    obj: \"Child\" = Child()
    return obj.note()
";
    let env = inherited_fixture_env()?;
    let toolchain = env.toolchain()?;
    let cancelled = AtomicBool::new(false);
    let fragment = compile_inherited_fragment(SOURCE, &env.work, &toolchain, &cancelled)?;
    let view = fragment.view()?;
    let read_owner = entity_ordinal_at_index(&view, b"read", EntityKind::Function, 0)?;
    let decoy_note = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 0)?;
    let child_note = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 1)?;
    let calls = read_method_calls(&view, read_owner)?;
    if calls.len() != 1 {
        return Err(TestError::Falsified("read owns one MethodCall"));
    }
    assert_local_index(calls[0], child_note, "obj.note() resolves to Child.note")?;
    if matches!(
        &calls[0].occurrence.target,
        OccurrenceTarget::Local(target) if target.raw == decoy_note
    ) {
        return Err(TestError::Falsified("obj.note() must not resolve to Decoy.note"));
    }
    fs::remove_dir_all(&env.work).map_err(|source| TestError::Io("remove scratch", source))?;
    Ok(())
}

#[test]
fn py_local_annotation_list_call_stays_universe() -> Result<(), TestError> {
    const SOURCE: &[u8] = b"\
class Child:
    def note(self):
        return 1

def read():
    obj: list[Child] = []
    return obj.note()
";
    let env = inherited_fixture_env()?;
    let toolchain = env.toolchain()?;
    let cancelled = AtomicBool::new(false);
    let fragment = compile_inherited_fragment(SOURCE, &env.work, &toolchain, &cancelled)?;
    let view = fragment.view()?;
    let read_owner = entity_ordinal_at_index(&view, b"read", EntityKind::Function, 0)?;
    let child_note = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 0)?;
    let calls = read_method_calls(&view, read_owner)?;
    if calls.len() != 1 {
        return Err(TestError::Falsified("read owns one MethodCall"));
    }
    assert_universe_method(calls[0], "list[Child] local annotation stays a pypi universe key")?;
    if calls[0].occurrence.confidence != OccurrenceConfidence::Index {
        return Err(TestError::Falsified("list[Child] universe method confidence is Index"));
    }
    if matches!(
        &calls[0].occurrence.target,
        OccurrenceTarget::Local(target) if target.raw == child_note
    ) {
        return Err(TestError::Falsified("list[Child] must not bind Child.note"));
    }
    fs::remove_dir_all(&env.work).map_err(|source| TestError::Io("remove scratch", source))?;
    Ok(())
}

#[test]
fn py_local_annotation_dotted_read_stays_universe() -> Result<(), TestError> {
    const SOURCE: &[u8] = b"\
import pkg

score = 1

class Child:
    score = 2

def read():
    obj: pkg.Child
    return obj.score
";
    let env = inherited_fixture_env()?;
    let toolchain = env.toolchain()?;
    let cancelled = AtomicBool::new(false);
    let fragment = compile_inherited_fragment(SOURCE, &env.work, &toolchain, &cancelled)?;
    let view = fragment.view()?;
    let read_owner = entity_ordinal_at_index(&view, b"read", EntityKind::Function, 0)?;
    let module_score = entity_ordinal_at_index(&view, b"score", EntityKind::Static, 0)?;
    let child_score = entity_ordinal_at_index(&view, b"score", EntityKind::Field, 0)?;
    let calls = read_method_calls(&view, read_owner)?;
    if !calls.is_empty() {
        return Err(TestError::Falsified("read owns zero MethodCalls"));
    }
    let reads = read_field_accesses(&view, read_owner)?;
    if reads.len() != 2 {
        return Err(TestError::Falsified("read owns two FieldAccess rows"));
    }
    if !matches!(
        &reads[0].occurrence.target,
        OccurrenceTarget::Foreign(key)
            if key.path == "pkg"
                && key.display == "Child"
                && key.kind == Some(EntityKind::Field)
                && matches!(
                    key.origin,
                    ForeignOrigin::Package(lineage)
                        if lineage.ecosystem == "pypi" && lineage.name == "pkg"
                )
    ) {
        return Err(TestError::Falsified("pkg.Child stays a pypi package field key"));
    }
    if reads[0].occurrence.confidence != OccurrenceConfidence::Index {
        return Err(TestError::Falsified("pkg.Child package field confidence is Index"));
    }
    if !matches!(
        &reads[1].occurrence.target,
        OccurrenceTarget::Foreign(key)
            if key.path == "score"
                && key.display == "score"
                && key.kind == Some(EntityKind::Field)
                && matches!(key.origin, ForeignOrigin::Universe { ecosystem: "pypi" })
    ) {
        return Err(TestError::Falsified("obj.score stays a pypi universe field key"));
    }
    if reads[1].occurrence.confidence != OccurrenceConfidence::Index {
        return Err(TestError::Falsified("obj.score universe field confidence is Index"));
    }
    if matches!(
        &reads[1].occurrence.target,
        OccurrenceTarget::Local(target) if target.raw == module_score
    ) {
        return Err(TestError::Falsified("obj.score must not resolve to module score"));
    }
    if matches!(
        &reads[1].occurrence.target,
        OccurrenceTarget::Local(target) if target.raw == child_score
    ) {
        return Err(TestError::Falsified("obj.score must not resolve to Child.score"));
    }
    fs::remove_dir_all(&env.work).map_err(|source| TestError::Io("remove scratch", source))?;
    Ok(())
}

#[test]
fn py_local_annotation_two_annotations_stay_universe() -> Result<(), TestError> {
    const SOURCE: &[u8] = b"\
class Child:
    def note(self):
        return 1

class Other:
    def note(self):
        return 2

def read():
    obj: Child = Child()
    obj: Other = Other()
    return obj.note()
";
    let env = inherited_fixture_env()?;
    let toolchain = env.toolchain()?;
    let cancelled = AtomicBool::new(false);
    let fragment = compile_inherited_fragment(SOURCE, &env.work, &toolchain, &cancelled)?;
    let view = fragment.view()?;
    let read_owner = entity_ordinal_at_index(&view, b"read", EntityKind::Function, 0)?;
    let child_note = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 0)?;
    let other_note = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 1)?;
    let calls = read_method_calls(&view, read_owner)?;
    if calls.len() != 1 {
        return Err(TestError::Falsified("read owns one MethodCall"));
    }
    assert_universe_method(calls[0], "two local annotations stay a pypi universe key")?;
    if calls[0].occurrence.confidence != OccurrenceConfidence::Index {
        return Err(TestError::Falsified("two local annotations universe method confidence is Index"));
    }
    if matches!(
        &calls[0].occurrence.target,
        OccurrenceTarget::Local(target) if target.raw == child_note
    ) {
        return Err(TestError::Falsified("obj.note() must not bind Child.note"));
    }
    if matches!(
        &calls[0].occurrence.target,
        OccurrenceTarget::Local(target) if target.raw == other_note
    ) {
        return Err(TestError::Falsified("obj.note() must not bind Other.note"));
    }
    fs::remove_dir_all(&env.work).map_err(|source| TestError::Io("remove scratch", source))?;
    Ok(())
}

#[test]
fn py_local_annotation_shadows_parameter() -> Result<(), TestError> {
    const SOURCE: &[u8] = b"\
class Decoy:
    def note(self):
        return 0

class Child:
    def note(self):
        return 1

def read(obj: Decoy):
    obj: Child = Child()
    return obj.note()
";
    let env = inherited_fixture_env()?;
    let toolchain = env.toolchain()?;
    let cancelled = AtomicBool::new(false);
    let fragment = compile_inherited_fragment(SOURCE, &env.work, &toolchain, &cancelled)?;
    let view = fragment.view()?;
    let read_owner = entity_ordinal_at_index(&view, b"read", EntityKind::Function, 0)?;
    let decoy_note = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 0)?;
    let child_note = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 1)?;
    let calls = read_method_calls(&view, read_owner)?;
    if calls.len() != 1 {
        return Err(TestError::Falsified("read owns one MethodCall"));
    }
    assert_local_index(calls[0], child_note, "obj.note() resolves to Child.note")?;
    if matches!(
        &calls[0].occurrence.target,
        OccurrenceTarget::Local(target) if target.raw == decoy_note
    ) {
        return Err(TestError::Falsified("obj.note() must not resolve to Decoy.note"));
    }
    fs::remove_dir_all(&env.work).map_err(|source| TestError::Io("remove scratch", source))?;
    Ok(())
}

#[test]
fn py_closure_annotation_call_targets_child_note_not_decoy() -> Result<(), TestError> {
    const SOURCE: &[u8] = b"\
class Decoy:
    def note(self):
        return 0

class Child:
    def note(self):
        return 1

def outer():
    obj: Child = Child()

    def inner():
        return obj.note()
";
    let env = inherited_fixture_env()?;
    let toolchain = env.toolchain()?;
    let cancelled = AtomicBool::new(false);
    let fragment = compile_inherited_fragment(SOURCE, &env.work, &toolchain, &cancelled)?;
    let view = fragment.view()?;
    let inner_owner = entity_ordinal_at_index(&view, b"inner", EntityKind::Function, 0)?;
    let decoy_note = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 0)?;
    let child_note = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 1)?;
    let calls = read_method_calls(&view, inner_owner)?;
    if calls.len() != 1 {
        return Err(TestError::Falsified("inner owns one MethodCall"));
    }
    assert_local_index(calls[0], child_note, "inner obj.note() resolves to Child.note")?;
    if matches!(
        &calls[0].occurrence.target,
        OccurrenceTarget::Local(target) if target.raw == decoy_note
    ) {
        return Err(TestError::Falsified("inner obj.note() must not resolve to Decoy.note"));
    }
    fs::remove_dir_all(&env.work).map_err(|source| TestError::Io("remove scratch", source))?;
    Ok(())
}

#[test]
fn py_closure_annotation_field_targets_child_score_not_decoy() -> Result<(), TestError> {
    const SOURCE: &[u8] = b"\
class Decoy:
    score = 9

class Child:
    score = 1

def outer():
    obj: Child = Child()

    def inner():
        return obj.score
";
    let env = inherited_fixture_env()?;
    let toolchain = env.toolchain()?;
    let cancelled = AtomicBool::new(false);
    let fragment = compile_inherited_fragment(SOURCE, &env.work, &toolchain, &cancelled)?;
    let view = fragment.view()?;
    let inner_owner = entity_ordinal_at_index(&view, b"inner", EntityKind::Function, 0)?;
    let decoy_score = entity_ordinal_at_index(&view, b"score", EntityKind::Field, 0)?;
    let child_score = entity_ordinal_at_index(&view, b"score", EntityKind::Field, 1)?;
    let calls = read_method_calls(&view, inner_owner)?;
    if !calls.is_empty() {
        return Err(TestError::Falsified("inner owns zero MethodCalls"));
    }
    let reads = read_field_accesses(&view, inner_owner)?;
    if reads.len() != 1 {
        return Err(TestError::Falsified("inner owns one FieldAccess"));
    }
    assert_local_index(reads[0], child_score, "inner obj.score resolves to Child.score")?;
    if matches!(
        &reads[0].occurrence.target,
        OccurrenceTarget::Local(target) if target.raw == decoy_score
    ) {
        return Err(TestError::Falsified("inner obj.score must not resolve to Decoy.score"));
    }
    fs::remove_dir_all(&env.work).map_err(|source| TestError::Io("remove scratch", source))?;
    Ok(())
}

#[test]
fn py_closure_annotation_value_targets_child_note_not_decoy() -> Result<(), TestError> {
    const SOURCE: &[u8] = b"\
class Decoy:
    def note(self):
        return 0

class Child:
    def note(self):
        return 1

def outer():
    obj: Child = Child()

    def inner():
        return obj.note
";
    let env = inherited_fixture_env()?;
    let toolchain = env.toolchain()?;
    let cancelled = AtomicBool::new(false);
    let fragment = compile_inherited_fragment(SOURCE, &env.work, &toolchain, &cancelled)?;
    let view = fragment.view()?;
    let inner_owner = entity_ordinal_at_index(&view, b"inner", EntityKind::Function, 0)?;
    let decoy_note = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 0)?;
    let child_note = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 1)?;
    let calls = read_method_calls(&view, inner_owner)?;
    if !calls.is_empty() {
        return Err(TestError::Falsified("inner owns zero MethodCalls"));
    }
    let reads = read_field_accesses(&view, inner_owner)?;
    if reads.len() != 1 {
        return Err(TestError::Falsified("inner owns one FieldAccess"));
    }
    assert_local_index(reads[0], child_note, "inner obj.note resolves to Child.note")?;
    if matches!(
        &reads[0].occurrence.target,
        OccurrenceTarget::Local(target) if target.raw == decoy_note
    ) {
        return Err(TestError::Falsified("inner obj.note must not resolve to Decoy.note"));
    }
    fs::remove_dir_all(&env.work).map_err(|source| TestError::Io("remove scratch", source))?;
    Ok(())
}

#[test]
fn py_closure_annotation_inherited_call_targets_base_note_not_decoy() -> Result<(), TestError> {
    const SOURCE: &[u8] = b"\
class Decoy:
    def note(self):
        return 0

class Base:
    def note(self):
        return 1

class Child(Base):
    pass

def outer():
    obj: Child = Child()

    def inner():
        return obj.note()
";
    let env = inherited_fixture_env()?;
    let toolchain = env.toolchain()?;
    let cancelled = AtomicBool::new(false);
    let fragment = compile_inherited_fragment(SOURCE, &env.work, &toolchain, &cancelled)?;
    let view = fragment.view()?;
    let inner_owner = entity_ordinal_at_index(&view, b"inner", EntityKind::Function, 0)?;
    let decoy_note = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 0)?;
    let base_note = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 1)?;
    let calls = read_method_calls(&view, inner_owner)?;
    if calls.len() != 1 {
        return Err(TestError::Falsified("inner owns one MethodCall"));
    }
    assert_local_index(calls[0], base_note, "inner obj.note() resolves to Base.note")?;
    if matches!(
        &calls[0].occurrence.target,
        OccurrenceTarget::Local(target) if target.raw == decoy_note
    ) {
        return Err(TestError::Falsified("inner obj.note() must not resolve to Decoy.note"));
    }
    fs::remove_dir_all(&env.work).map_err(|source| TestError::Io("remove scratch", source))?;
    Ok(())
}

#[test]
fn py_closure_annotation_generic_call_targets_child_note_not_decoy() -> Result<(), TestError> {
    const SOURCE: &[u8] = b"\
class Decoy:
    def note(self):
        return 0

class Child:
    def note(self):
        return 1

def outer():
    obj: Child[int] = Child()

    def inner():
        return obj.note()
";
    let env = inherited_fixture_env()?;
    let toolchain = env.toolchain()?;
    let cancelled = AtomicBool::new(false);
    let fragment = compile_inherited_fragment(SOURCE, &env.work, &toolchain, &cancelled)?;
    let view = fragment.view()?;
    let inner_owner = entity_ordinal_at_index(&view, b"inner", EntityKind::Function, 0)?;
    let decoy_note = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 0)?;
    let child_note = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 1)?;
    let calls = read_method_calls(&view, inner_owner)?;
    if calls.len() != 1 {
        return Err(TestError::Falsified("inner owns one MethodCall"));
    }
    assert_local_index(calls[0], child_note, "inner obj.note() resolves to Child.note")?;
    if matches!(
        &calls[0].occurrence.target,
        OccurrenceTarget::Local(target) if target.raw == decoy_note
    ) {
        return Err(TestError::Falsified("inner obj.note() must not resolve to Decoy.note"));
    }
    fs::remove_dir_all(&env.work).map_err(|source| TestError::Io("remove scratch", source))?;
    Ok(())
}

#[test]
fn py_closure_annotation_union_call_targets_child_note_not_decoy() -> Result<(), TestError> {
    const SOURCE: &[u8] = b"\
class Decoy:
    def note(self):
        return 0

class Child:
    def note(self):
        return 1

def outer():
    obj: Child | None = Child()

    def inner():
        return obj.note()
";
    let env = inherited_fixture_env()?;
    let toolchain = env.toolchain()?;
    let cancelled = AtomicBool::new(false);
    let fragment = compile_inherited_fragment(SOURCE, &env.work, &toolchain, &cancelled)?;
    let view = fragment.view()?;
    let inner_owner = entity_ordinal_at_index(&view, b"inner", EntityKind::Function, 0)?;
    let decoy_note = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 0)?;
    let child_note = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 1)?;
    let calls = read_method_calls(&view, inner_owner)?;
    if calls.len() != 1 {
        return Err(TestError::Falsified("inner owns one MethodCall"));
    }
    assert_local_index(calls[0], child_note, "inner obj.note() resolves to Child.note")?;
    if matches!(
        &calls[0].occurrence.target,
        OccurrenceTarget::Local(target) if target.raw == decoy_note
    ) {
        return Err(TestError::Falsified("inner obj.note() must not resolve to Decoy.note"));
    }
    fs::remove_dir_all(&env.work).map_err(|source| TestError::Io("remove scratch", source))?;
    Ok(())
}

#[test]
fn py_closure_annotation_quoted_call_targets_child_note_not_decoy() -> Result<(), TestError> {
    const SOURCE: &[u8] = b"\
class Decoy:
    def note(self):
        return 0

class Child:
    def note(self):
        return 1

def outer():
    obj: \"Child\" = Child()

    def inner():
        return obj.note()
";
    let env = inherited_fixture_env()?;
    let toolchain = env.toolchain()?;
    let cancelled = AtomicBool::new(false);
    let fragment = compile_inherited_fragment(SOURCE, &env.work, &toolchain, &cancelled)?;
    let view = fragment.view()?;
    let inner_owner = entity_ordinal_at_index(&view, b"inner", EntityKind::Function, 0)?;
    let decoy_note = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 0)?;
    let child_note = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 1)?;
    let calls = read_method_calls(&view, inner_owner)?;
    if calls.len() != 1 {
        return Err(TestError::Falsified("inner owns one MethodCall"));
    }
    assert_local_index(calls[0], child_note, "inner obj.note() resolves to Child.note")?;
    if matches!(
        &calls[0].occurrence.target,
        OccurrenceTarget::Local(target) if target.raw == decoy_note
    ) {
        return Err(TestError::Falsified("inner obj.note() must not resolve to Decoy.note"));
    }
    fs::remove_dir_all(&env.work).map_err(|source| TestError::Io("remove scratch", source))?;
    Ok(())
}

#[test]
fn py_closure_annotation_deep_call_targets_child_note_not_decoy() -> Result<(), TestError> {
    const SOURCE: &[u8] = b"\
class Decoy:
    def note(self):
        return 0

class Child:
    def note(self):
        return 1

def outer():
    obj: Child = Child()

    def mid():
        def inner():
            return obj.note()
";
    let env = inherited_fixture_env()?;
    let toolchain = env.toolchain()?;
    let cancelled = AtomicBool::new(false);
    let fragment = compile_inherited_fragment(SOURCE, &env.work, &toolchain, &cancelled)?;
    let view = fragment.view()?;
    let inner_owner = entity_ordinal_at_index(&view, b"inner", EntityKind::Function, 0)?;
    let decoy_note = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 0)?;
    let child_note = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 1)?;
    let calls = read_method_calls(&view, inner_owner)?;
    if calls.len() != 1 {
        return Err(TestError::Falsified("inner owns one MethodCall"));
    }
    assert_local_index(calls[0], child_note, "inner obj.note() resolves to Child.note")?;
    if matches!(
        &calls[0].occurrence.target,
        OccurrenceTarget::Local(target) if target.raw == decoy_note
    ) {
        return Err(TestError::Falsified("inner obj.note() must not resolve to Decoy.note"));
    }
    fs::remove_dir_all(&env.work).map_err(|source| TestError::Io("remove scratch", source))?;
    Ok(())
}

#[test]
fn py_closure_annotation_intervening_assignment_stays_universe() -> Result<(), TestError> {
    const SOURCE: &[u8] = b"\
class Child:
    def note(self):
        return 1

def outer():
    obj: Child = Child()

    def mid():
        obj = 1

        def inner():
            return obj.note()
";
    let env = inherited_fixture_env()?;
    let toolchain = env.toolchain()?;
    let cancelled = AtomicBool::new(false);
    let fragment = compile_inherited_fragment(SOURCE, &env.work, &toolchain, &cancelled)?;
    let view = fragment.view()?;
    let inner_owner = entity_ordinal_at_index(&view, b"inner", EntityKind::Function, 0)?;
    let child_note = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 0)?;
    let calls = read_method_calls(&view, inner_owner)?;
    if calls.len() != 1 {
        return Err(TestError::Falsified("inner owns one MethodCall"));
    }
    assert_universe_method(calls[0], "intervening assignment stays a pypi universe key")?;
    if calls[0].occurrence.confidence != OccurrenceConfidence::Index {
        return Err(TestError::Falsified("intervening assignment universe method confidence is Index"));
    }
    if matches!(
        &calls[0].occurrence.target,
        OccurrenceTarget::Local(target) if target.raw == child_note
    ) {
        return Err(TestError::Falsified("inner obj.note() must not bind Child.note"));
    }
    fs::remove_dir_all(&env.work).map_err(|source| TestError::Io("remove scratch", source))?;
    Ok(())
}

#[test]
fn py_closure_annotation_closer_annotation_wins() -> Result<(), TestError> {
    const SOURCE: &[u8] = b"\
class Decoy:
    def note(self):
        return 0

class Child:
    def note(self):
        return 1

def outer():
    obj: Decoy = Decoy()

    def mid():
        obj: Child = Child()

        def inner():
            return obj.note()
";
    let env = inherited_fixture_env()?;
    let toolchain = env.toolchain()?;
    let cancelled = AtomicBool::new(false);
    let fragment = compile_inherited_fragment(SOURCE, &env.work, &toolchain, &cancelled)?;
    let view = fragment.view()?;
    let inner_owner = entity_ordinal_at_index(&view, b"inner", EntityKind::Function, 0)?;
    let decoy_note = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 0)?;
    let child_note = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 1)?;
    let calls = read_method_calls(&view, inner_owner)?;
    if calls.len() != 1 {
        return Err(TestError::Falsified("inner owns one MethodCall"));
    }
    assert_local_index(calls[0], child_note, "inner obj.note() resolves to Child.note")?;
    if matches!(
        &calls[0].occurrence.target,
        OccurrenceTarget::Local(target) if target.raw == decoy_note
    ) {
        return Err(TestError::Falsified("inner obj.note() must not resolve to Decoy.note"));
    }
    fs::remove_dir_all(&env.work).map_err(|source| TestError::Io("remove scratch", source))?;
    Ok(())
}

#[test]
fn py_closure_annotation_after_nested_def_stays_universe() -> Result<(), TestError> {
    const SOURCE: &[u8] = b"\
class Child:
    def note(self):
        return 1

def outer():
    def inner():
        return obj.note()

    obj: Child = Child()
";
    let env = inherited_fixture_env()?;
    let toolchain = env.toolchain()?;
    let cancelled = AtomicBool::new(false);
    let fragment = compile_inherited_fragment(SOURCE, &env.work, &toolchain, &cancelled)?;
    let view = fragment.view()?;
    let inner_owner = entity_ordinal_at_index(&view, b"inner", EntityKind::Function, 0)?;
    let child_note = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 0)?;
    let calls = read_method_calls(&view, inner_owner)?;
    if calls.len() != 1 {
        return Err(TestError::Falsified("inner owns one MethodCall"));
    }
    assert_universe_method(calls[0], "annotation after nested def stays a pypi universe key")?;
    if calls[0].occurrence.confidence != OccurrenceConfidence::Index {
        return Err(TestError::Falsified("annotation after nested def universe method confidence is Index"));
    }
    if matches!(
        &calls[0].occurrence.target,
        OccurrenceTarget::Local(target) if target.raw == child_note
    ) {
        return Err(TestError::Falsified("inner obj.note() must not bind Child.note"));
    }
    fs::remove_dir_all(&env.work).map_err(|source| TestError::Io("remove scratch", source))?;
    Ok(())
}

#[test]
fn py_closure_annotation_list_read_does_not_take_module_field() -> Result<(), TestError> {
    const SOURCE: &[u8] = b"\
class Holder:
    note = 1

class Child:
    def note(self):
        return 2

def outer():
    obj: list[Child] = []

    def inner():
        return obj.note
";
    let env = inherited_fixture_env()?;
    let toolchain = env.toolchain()?;
    let cancelled = AtomicBool::new(false);
    let fragment = compile_inherited_fragment(SOURCE, &env.work, &toolchain, &cancelled)?;
    let view = fragment.view()?;
    let inner_owner = entity_ordinal_at_index(&view, b"inner", EntityKind::Function, 0)?;
    let holder_note = entity_ordinal_at_index(&view, b"note", EntityKind::Field, 0)?;
    let child_note = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 0)?;
    let reads = read_field_accesses(&view, inner_owner)?;
    if reads.len() != 1 {
        return Err(TestError::Falsified("inner owns one FieldAccess"));
    }
    assert_universe_field(reads[0], "list[Child] read stays a pypi universe field key")?;
    if reads[0].occurrence.confidence != OccurrenceConfidence::Index {
        return Err(TestError::Falsified("list[Child] read universe field confidence is Index"));
    }
    if matches!(
        &reads[0].occurrence.target,
        OccurrenceTarget::Local(target) if target.raw == holder_note
    ) {
        return Err(TestError::Falsified("list[Child] read must not take Holder.note"));
    }
    if matches!(
        &reads[0].occurrence.target,
        OccurrenceTarget::Local(target) if target.raw == child_note
    ) {
        return Err(TestError::Falsified("list[Child] read must not bind Child.note"));
    }
    fs::remove_dir_all(&env.work).map_err(|source| TestError::Io("remove scratch", source))?;
    Ok(())
}

#[test]
fn py_closure_annotation_dotted_stays_universe() -> Result<(), TestError> {
    const SOURCE: &[u8] = b"\
import pkg

class Child:
    def note(self):
        return 1

def outer():
    obj: pkg.Child

    def inner():
        return obj.note()
";
    let env = inherited_fixture_env()?;
    let toolchain = env.toolchain()?;
    let cancelled = AtomicBool::new(false);
    let fragment = compile_inherited_fragment(SOURCE, &env.work, &toolchain, &cancelled)?;
    let view = fragment.view()?;
    let inner_owner = entity_ordinal_at_index(&view, b"inner", EntityKind::Function, 0)?;
    let child_note = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 0)?;
    let calls = read_method_calls(&view, inner_owner)?;
    if calls.len() != 1 {
        return Err(TestError::Falsified("inner owns one MethodCall"));
    }
    assert_universe_method(calls[0], "dotted annotation stays a pypi universe key")?;
    if calls[0].occurrence.confidence != OccurrenceConfidence::Index {
        return Err(TestError::Falsified("dotted annotation universe method confidence is Index"));
    }
    if matches!(
        &calls[0].occurrence.target,
        OccurrenceTarget::Local(target) if target.raw == child_note
    ) {
        return Err(TestError::Falsified("inner obj.note() must not bind Child.note"));
    }
    fs::remove_dir_all(&env.work).map_err(|source| TestError::Io("remove scratch", source))?;
    Ok(())
}

#[test]
fn py_closure_annotation_missing_member_stays_attribute_key() -> Result<(), TestError> {
    const SOURCE: &[u8] = b"\
class Decoy:
    def note(self):
        return 0

class Child:
    pass

def outer():
    obj: Child = Child()

    def inner():
        return obj.note()
";
    let env = inherited_fixture_env()?;
    let toolchain = env.toolchain()?;
    let cancelled = AtomicBool::new(false);
    let fragment = compile_inherited_fragment(SOURCE, &env.work, &toolchain, &cancelled)?;
    let view = fragment.view()?;
    let inner_owner = entity_ordinal_at_index(&view, b"inner", EntityKind::Function, 0)?;
    let decoy_note = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 0)?;
    let calls = read_method_calls(&view, inner_owner)?;
    if calls.len() != 1 {
        return Err(TestError::Falsified("inner owns one MethodCall"));
    }
    assert_universe_method(calls[0], "missing member stays a pypi universe key")?;
    if calls[0].occurrence.confidence != OccurrenceConfidence::Index {
        return Err(TestError::Falsified("missing member universe method confidence is Index"));
    }
    if matches!(
        &calls[0].occurrence.target,
        OccurrenceTarget::Local(target) if target.raw == decoy_note
    ) {
        return Err(TestError::Falsified("inner obj.note() must not resolve to Decoy.note"));
    }
    fs::remove_dir_all(&env.work).map_err(|source| TestError::Io("remove scratch", source))?;
    Ok(())
}

#[test]
fn py_closure_annotation_field_wins_over_method() -> Result<(), TestError> {
    const SOURCE: &[u8] = b"\
class Child:
    score = 1

    def score(self):
        return self.score

def outer():
    obj: Child = Child()

    def inner():
        return obj.score
";
    let env = inherited_fixture_env()?;
    let toolchain = env.toolchain()?;
    let cancelled = AtomicBool::new(false);
    let fragment = compile_inherited_fragment(SOURCE, &env.work, &toolchain, &cancelled)?;
    let view = fragment.view()?;
    let inner_owner = entity_ordinal_at_index(&view, b"inner", EntityKind::Function, 0)?;
    let score_field = entity_ordinal_at_index(&view, b"score", EntityKind::Field, 0)?;
    let score_method = entity_ordinal_at_index(&view, b"score", EntityKind::Function, 0)?;
    let calls = read_method_calls(&view, inner_owner)?;
    if !calls.is_empty() {
        return Err(TestError::Falsified("inner owns zero MethodCalls"));
    }
    let reads = read_field_accesses(&view, inner_owner)?;
    if reads.len() != 1 {
        return Err(TestError::Falsified("inner owns one FieldAccess"));
    }
    assert_local_index(reads[0], score_field, "inner obj.score resolves to Child.score field")?;
    if matches!(
        &reads[0].occurrence.target,
        OccurrenceTarget::Local(target) if target.raw == score_method
    ) {
        return Err(TestError::Falsified("inner obj.score must not resolve to Child.score method"));
    }
    fs::remove_dir_all(&env.work).map_err(|source| TestError::Io("remove scratch", source))?;
    Ok(())
}

#[test]
fn py_closure_annotation_call_binds_method_not_field() -> Result<(), TestError> {
    const SOURCE: &[u8] = b"\
class Child:
    note = 1

    def note(self):
        return 0

def outer():
    obj: Child = Child()

    def inner():
        return obj.note()
";
    let env = inherited_fixture_env()?;
    let toolchain = env.toolchain()?;
    let cancelled = AtomicBool::new(false);
    let fragment = compile_inherited_fragment(SOURCE, &env.work, &toolchain, &cancelled)?;
    let view = fragment.view()?;
    let inner_owner = entity_ordinal_at_index(&view, b"inner", EntityKind::Function, 0)?;
    let note_field = entity_ordinal_at_index(&view, b"note", EntityKind::Field, 0)?;
    let note_method = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 0)?;
    let calls = read_method_calls(&view, inner_owner)?;
    if calls.len() != 1 {
        return Err(TestError::Falsified("inner owns one MethodCall"));
    }
    assert_local_index(calls[0], note_method, "inner obj.note() resolves to Child.note method")?;
    if matches!(
        &calls[0].occurrence.target,
        OccurrenceTarget::Local(target) if target.raw == note_field
    ) {
        return Err(TestError::Falsified("inner obj.note() must not resolve to Child.note field"));
    }
    fs::remove_dir_all(&env.work).map_err(|source| TestError::Io("remove scratch", source))?;
    Ok(())
}

#[test]
fn py_closure_parameter_call_targets_child_note_not_decoy() -> Result<(), TestError> {
    const SOURCE: &[u8] = b"\
class Decoy:
    def note(self):
        return 0

class Child:
    def note(self):
        return 1

def outer(obj: Child):
    def inner():
        return obj.note()
";
    let env = inherited_fixture_env()?;
    let toolchain = env.toolchain()?;
    let cancelled = AtomicBool::new(false);
    let fragment = compile_inherited_fragment(SOURCE, &env.work, &toolchain, &cancelled)?;
    let view = fragment.view()?;
    let inner_owner = entity_ordinal_at_index(&view, b"inner", EntityKind::Function, 0)?;
    let decoy_note = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 0)?;
    let child_note = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 1)?;
    let calls = read_method_calls(&view, inner_owner)?;
    if calls.len() != 1 {
        return Err(TestError::Falsified("inner owns one MethodCall"));
    }
    assert_local_index(calls[0], child_note, "inner obj.note() resolves to Child.note")?;
    if matches!(
        &calls[0].occurrence.target,
        OccurrenceTarget::Local(target) if target.raw == decoy_note
    ) {
        return Err(TestError::Falsified("inner obj.note() must not resolve to Decoy.note"));
    }
    fs::remove_dir_all(&env.work).map_err(|source| TestError::Io("remove scratch", source))?;
    Ok(())
}

#[test]
fn py_closure_parameter_field_targets_child_score_not_decoy() -> Result<(), TestError> {
    const SOURCE: &[u8] = b"\
class Decoy:
    score = 9

class Child:
    score = 1

def outer(obj: Child):
    def inner():
        return obj.score
";
    let env = inherited_fixture_env()?;
    let toolchain = env.toolchain()?;
    let cancelled = AtomicBool::new(false);
    let fragment = compile_inherited_fragment(SOURCE, &env.work, &toolchain, &cancelled)?;
    let view = fragment.view()?;
    let inner_owner = entity_ordinal_at_index(&view, b"inner", EntityKind::Function, 0)?;
    let decoy_score = entity_ordinal_at_index(&view, b"score", EntityKind::Field, 0)?;
    let child_score = entity_ordinal_at_index(&view, b"score", EntityKind::Field, 1)?;
    let calls = read_method_calls(&view, inner_owner)?;
    if !calls.is_empty() {
        return Err(TestError::Falsified("inner owns zero MethodCalls"));
    }
    let reads = read_field_accesses(&view, inner_owner)?;
    if reads.len() != 1 {
        return Err(TestError::Falsified("inner owns one FieldAccess"));
    }
    assert_local_index(reads[0], child_score, "inner obj.score resolves to Child.score")?;
    if matches!(
        &reads[0].occurrence.target,
        OccurrenceTarget::Local(target) if target.raw == decoy_score
    ) {
        return Err(TestError::Falsified("inner obj.score must not resolve to Decoy.score"));
    }
    fs::remove_dir_all(&env.work).map_err(|source| TestError::Io("remove scratch", source))?;
    Ok(())
}

#[test]
fn py_closure_parameter_value_targets_child_note_not_decoy() -> Result<(), TestError> {
    const SOURCE: &[u8] = b"\
class Decoy:
    def note(self):
        return 0

class Child:
    def note(self):
        return 1

def outer(obj: Child):
    def inner():
        return obj.note
";
    let env = inherited_fixture_env()?;
    let toolchain = env.toolchain()?;
    let cancelled = AtomicBool::new(false);
    let fragment = compile_inherited_fragment(SOURCE, &env.work, &toolchain, &cancelled)?;
    let view = fragment.view()?;
    let inner_owner = entity_ordinal_at_index(&view, b"inner", EntityKind::Function, 0)?;
    let decoy_note = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 0)?;
    let child_note = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 1)?;
    let calls = read_method_calls(&view, inner_owner)?;
    if !calls.is_empty() {
        return Err(TestError::Falsified("inner owns zero MethodCalls"));
    }
    let reads = read_field_accesses(&view, inner_owner)?;
    if reads.len() != 1 {
        return Err(TestError::Falsified("inner owns one FieldAccess"));
    }
    assert_local_index(reads[0], child_note, "inner obj.note resolves to Child.note")?;
    if matches!(
        &reads[0].occurrence.target,
        OccurrenceTarget::Local(target) if target.raw == decoy_note
    ) {
        return Err(TestError::Falsified("inner obj.note must not resolve to Decoy.note"));
    }
    fs::remove_dir_all(&env.work).map_err(|source| TestError::Io("remove scratch", source))?;
    Ok(())
}

#[test]
fn py_closure_parameter_inherited_call_targets_base_note_not_decoy() -> Result<(), TestError> {
    const SOURCE: &[u8] = b"\
class Decoy:
    def note(self):
        return 0

class Base:
    def note(self):
        return 1

class Child(Base):
    pass

def outer(obj: Child):
    def inner():
        return obj.note()
";
    let env = inherited_fixture_env()?;
    let toolchain = env.toolchain()?;
    let cancelled = AtomicBool::new(false);
    let fragment = compile_inherited_fragment(SOURCE, &env.work, &toolchain, &cancelled)?;
    let view = fragment.view()?;
    let inner_owner = entity_ordinal_at_index(&view, b"inner", EntityKind::Function, 0)?;
    let decoy_note = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 0)?;
    let base_note = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 1)?;
    let calls = read_method_calls(&view, inner_owner)?;
    if calls.len() != 1 {
        return Err(TestError::Falsified("inner owns one MethodCall"));
    }
    assert_local_index(calls[0], base_note, "inner obj.note() resolves to Base.note")?;
    if matches!(
        &calls[0].occurrence.target,
        OccurrenceTarget::Local(target) if target.raw == decoy_note
    ) {
        return Err(TestError::Falsified("inner obj.note() must not resolve to Decoy.note"));
    }
    fs::remove_dir_all(&env.work).map_err(|source| TestError::Io("remove scratch", source))?;
    Ok(())
}

#[test]
fn py_closure_parameter_generic_call_targets_child_note_not_decoy() -> Result<(), TestError> {
    const SOURCE: &[u8] = b"\
class Decoy:
    def note(self):
        return 0

class Child:
    def note(self):
        return 1

def outer(obj: Child[int]):
    def inner():
        return obj.note()
";
    let env = inherited_fixture_env()?;
    let toolchain = env.toolchain()?;
    let cancelled = AtomicBool::new(false);
    let fragment = compile_inherited_fragment(SOURCE, &env.work, &toolchain, &cancelled)?;
    let view = fragment.view()?;
    let inner_owner = entity_ordinal_at_index(&view, b"inner", EntityKind::Function, 0)?;
    let decoy_note = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 0)?;
    let child_note = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 1)?;
    let calls = read_method_calls(&view, inner_owner)?;
    if calls.len() != 1 {
        return Err(TestError::Falsified("inner owns one MethodCall"));
    }
    assert_local_index(calls[0], child_note, "inner obj.note() resolves to Child.note")?;
    if matches!(
        &calls[0].occurrence.target,
        OccurrenceTarget::Local(target) if target.raw == decoy_note
    ) {
        return Err(TestError::Falsified("inner obj.note() must not resolve to Decoy.note"));
    }
    fs::remove_dir_all(&env.work).map_err(|source| TestError::Io("remove scratch", source))?;
    Ok(())
}

#[test]
fn py_closure_parameter_union_call_targets_child_note_not_decoy() -> Result<(), TestError> {
    const SOURCE: &[u8] = b"\
class Decoy:
    def note(self):
        return 0

class Child:
    def note(self):
        return 1

def outer(obj: Child | None):
    def inner():
        return obj.note()
";
    let env = inherited_fixture_env()?;
    let toolchain = env.toolchain()?;
    let cancelled = AtomicBool::new(false);
    let fragment = compile_inherited_fragment(SOURCE, &env.work, &toolchain, &cancelled)?;
    let view = fragment.view()?;
    let inner_owner = entity_ordinal_at_index(&view, b"inner", EntityKind::Function, 0)?;
    let decoy_note = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 0)?;
    let child_note = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 1)?;
    let calls = read_method_calls(&view, inner_owner)?;
    if calls.len() != 1 {
        return Err(TestError::Falsified("inner owns one MethodCall"));
    }
    assert_local_index(calls[0], child_note, "inner obj.note() resolves to Child.note")?;
    if matches!(
        &calls[0].occurrence.target,
        OccurrenceTarget::Local(target) if target.raw == decoy_note
    ) {
        return Err(TestError::Falsified("inner obj.note() must not resolve to Decoy.note"));
    }
    fs::remove_dir_all(&env.work).map_err(|source| TestError::Io("remove scratch", source))?;
    Ok(())
}

#[test]
fn py_closure_parameter_quoted_call_targets_child_note_not_decoy() -> Result<(), TestError> {
    const SOURCE: &[u8] = b"\
class Decoy:
    def note(self):
        return 0

class Child:
    def note(self):
        return 1

def outer(obj: \"Child\"):
    def inner():
        return obj.note()
";
    let env = inherited_fixture_env()?;
    let toolchain = env.toolchain()?;
    let cancelled = AtomicBool::new(false);
    let fragment = compile_inherited_fragment(SOURCE, &env.work, &toolchain, &cancelled)?;
    let view = fragment.view()?;
    let inner_owner = entity_ordinal_at_index(&view, b"inner", EntityKind::Function, 0)?;
    let decoy_note = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 0)?;
    let child_note = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 1)?;
    let calls = read_method_calls(&view, inner_owner)?;
    if calls.len() != 1 {
        return Err(TestError::Falsified("inner owns one MethodCall"));
    }
    assert_local_index(calls[0], child_note, "inner obj.note() resolves to Child.note")?;
    if matches!(
        &calls[0].occurrence.target,
        OccurrenceTarget::Local(target) if target.raw == decoy_note
    ) {
        return Err(TestError::Falsified("inner obj.note() must not resolve to Decoy.note"));
    }
    fs::remove_dir_all(&env.work).map_err(|source| TestError::Io("remove scratch", source))?;
    Ok(())
}

#[test]
fn py_closure_parameter_deep_call_targets_child_note_not_decoy() -> Result<(), TestError> {
    const SOURCE: &[u8] = b"\
class Decoy:
    def note(self):
        return 0

class Child:
    def note(self):
        return 1

def outer(obj: Child):
    def mid():
        def inner():
            return obj.note()
";
    let env = inherited_fixture_env()?;
    let toolchain = env.toolchain()?;
    let cancelled = AtomicBool::new(false);
    let fragment = compile_inherited_fragment(SOURCE, &env.work, &toolchain, &cancelled)?;
    let view = fragment.view()?;
    let inner_owner = entity_ordinal_at_index(&view, b"inner", EntityKind::Function, 0)?;
    let decoy_note = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 0)?;
    let child_note = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 1)?;
    let calls = read_method_calls(&view, inner_owner)?;
    if calls.len() != 1 {
        return Err(TestError::Falsified("inner owns one MethodCall"));
    }
    assert_local_index(calls[0], child_note, "inner obj.note() resolves to Child.note")?;
    if matches!(
        &calls[0].occurrence.target,
        OccurrenceTarget::Local(target) if target.raw == decoy_note
    ) {
        return Err(TestError::Falsified("inner obj.note() must not resolve to Decoy.note"));
    }
    fs::remove_dir_all(&env.work).map_err(|source| TestError::Io("remove scratch", source))?;
    Ok(())
}

#[test]
fn py_closure_parameter_intervening_assignment_stays_universe() -> Result<(), TestError> {
    const SOURCE: &[u8] = b"\
class Child:
    def note(self):
        return 1

def outer(obj: Child):
    def mid():
        obj = 1

        def inner():
            return obj.note()
";
    let env = inherited_fixture_env()?;
    let toolchain = env.toolchain()?;
    let cancelled = AtomicBool::new(false);
    let fragment = compile_inherited_fragment(SOURCE, &env.work, &toolchain, &cancelled)?;
    let view = fragment.view()?;
    let inner_owner = entity_ordinal_at_index(&view, b"inner", EntityKind::Function, 0)?;
    let child_note = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 0)?;
    let calls = read_method_calls(&view, inner_owner)?;
    if calls.len() != 1 {
        return Err(TestError::Falsified("inner owns one MethodCall"));
    }
    assert_universe_method(calls[0], "intervening assignment stays a pypi universe key")?;
    if calls[0].occurrence.confidence != OccurrenceConfidence::Index {
        return Err(TestError::Falsified("intervening assignment universe method confidence is Index"));
    }
    if matches!(
        &calls[0].occurrence.target,
        OccurrenceTarget::Local(target) if target.raw == child_note
    ) {
        return Err(TestError::Falsified("inner obj.note() must not bind Child.note"));
    }
    fs::remove_dir_all(&env.work).map_err(|source| TestError::Io("remove scratch", source))?;
    Ok(())
}

#[test]
fn py_closure_parameter_closer_parameter_wins() -> Result<(), TestError> {
    const SOURCE: &[u8] = b"\
class Decoy:
    def note(self):
        return 0

class Child:
    def note(self):
        return 1

def outer(obj: Decoy):
    def mid(obj: Child):
        def inner():
            return obj.note()
";
    let env = inherited_fixture_env()?;
    let toolchain = env.toolchain()?;
    let cancelled = AtomicBool::new(false);
    let fragment = compile_inherited_fragment(SOURCE, &env.work, &toolchain, &cancelled)?;
    let view = fragment.view()?;
    let inner_owner = entity_ordinal_at_index(&view, b"inner", EntityKind::Function, 0)?;
    let decoy_note = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 0)?;
    let child_note = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 1)?;
    let calls = read_method_calls(&view, inner_owner)?;
    if calls.len() != 1 {
        return Err(TestError::Falsified("inner owns one MethodCall"));
    }
    assert_local_index(calls[0], child_note, "inner obj.note() resolves to Child.note")?;
    if matches!(
        &calls[0].occurrence.target,
        OccurrenceTarget::Local(target) if target.raw == decoy_note
    ) {
        return Err(TestError::Falsified("inner obj.note() must not resolve to Decoy.note"));
    }
    fs::remove_dir_all(&env.work).map_err(|source| TestError::Io("remove scratch", source))?;
    Ok(())
}

#[test]
fn py_closure_parameter_closer_annassign_wins() -> Result<(), TestError> {
    const SOURCE: &[u8] = b"\
class Decoy:
    def note(self):
        return 0

class Child:
    def note(self):
        return 1

def outer(obj: Decoy):
    def mid():
        obj: Child = Child()

        def inner():
            return obj.note()
";
    let env = inherited_fixture_env()?;
    let toolchain = env.toolchain()?;
    let cancelled = AtomicBool::new(false);
    let fragment = compile_inherited_fragment(SOURCE, &env.work, &toolchain, &cancelled)?;
    let view = fragment.view()?;
    let inner_owner = entity_ordinal_at_index(&view, b"inner", EntityKind::Function, 0)?;
    let decoy_note = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 0)?;
    let child_note = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 1)?;
    let calls = read_method_calls(&view, inner_owner)?;
    if calls.len() != 1 {
        return Err(TestError::Falsified("inner owns one MethodCall"));
    }
    assert_local_index(calls[0], child_note, "inner obj.note() resolves to Child.note")?;
    if matches!(
        &calls[0].occurrence.target,
        OccurrenceTarget::Local(target) if target.raw == decoy_note
    ) {
        return Err(TestError::Falsified("inner obj.note() must not resolve to Decoy.note"));
    }
    fs::remove_dir_all(&env.work).map_err(|source| TestError::Io("remove scratch", source))?;
    Ok(())
}

#[test]
fn py_closure_parameter_list_read_does_not_take_module_field() -> Result<(), TestError> {
    const SOURCE: &[u8] = b"\
class Holder:
    note = 1

class Child:
    def note(self):
        return 2

def outer(obj: list[Child]):
    def inner():
        return obj.note
";
    let env = inherited_fixture_env()?;
    let toolchain = env.toolchain()?;
    let cancelled = AtomicBool::new(false);
    let fragment = compile_inherited_fragment(SOURCE, &env.work, &toolchain, &cancelled)?;
    let view = fragment.view()?;
    let inner_owner = entity_ordinal_at_index(&view, b"inner", EntityKind::Function, 0)?;
    let holder_note = entity_ordinal_at_index(&view, b"note", EntityKind::Field, 0)?;
    let child_note = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 0)?;
    let reads = read_field_accesses(&view, inner_owner)?;
    if reads.len() != 1 {
        return Err(TestError::Falsified("inner owns one FieldAccess"));
    }
    assert_universe_field(reads[0], "list[Child] read stays a pypi universe field key")?;
    if reads[0].occurrence.confidence != OccurrenceConfidence::Index {
        return Err(TestError::Falsified("list[Child] read universe field confidence is Index"));
    }
    if matches!(
        &reads[0].occurrence.target,
        OccurrenceTarget::Local(target) if target.raw == holder_note
    ) {
        return Err(TestError::Falsified("list[Child] read must not take Holder.note"));
    }
    if matches!(
        &reads[0].occurrence.target,
        OccurrenceTarget::Local(target) if target.raw == child_note
    ) {
        return Err(TestError::Falsified("list[Child] read must not bind Child.note"));
    }
    fs::remove_dir_all(&env.work).map_err(|source| TestError::Io("remove scratch", source))?;
    Ok(())
}

#[test]
fn py_closure_parameter_dotted_stays_universe() -> Result<(), TestError> {
    const SOURCE: &[u8] = b"\
import pkg

class Child:
    def note(self):
        return 1

def outer(obj: pkg.Child):
    def inner():
        return obj.note()
";
    let env = inherited_fixture_env()?;
    let toolchain = env.toolchain()?;
    let cancelled = AtomicBool::new(false);
    let fragment = compile_inherited_fragment(SOURCE, &env.work, &toolchain, &cancelled)?;
    let view = fragment.view()?;
    let inner_owner = entity_ordinal_at_index(&view, b"inner", EntityKind::Function, 0)?;
    let child_note = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 0)?;
    let calls = read_method_calls(&view, inner_owner)?;
    if calls.len() != 1 {
        return Err(TestError::Falsified("inner owns one MethodCall"));
    }
    assert_universe_method(calls[0], "dotted annotation stays a pypi universe key")?;
    if calls[0].occurrence.confidence != OccurrenceConfidence::Index {
        return Err(TestError::Falsified("dotted annotation universe method confidence is Index"));
    }
    if matches!(
        &calls[0].occurrence.target,
        OccurrenceTarget::Local(target) if target.raw == child_note
    ) {
        return Err(TestError::Falsified("inner obj.note() must not bind Child.note"));
    }
    fs::remove_dir_all(&env.work).map_err(|source| TestError::Io("remove scratch", source))?;
    Ok(())
}

#[test]
fn py_closure_parameter_missing_member_stays_attribute_key() -> Result<(), TestError> {
    const SOURCE: &[u8] = b"\
class Decoy:
    def note(self):
        return 0

class Child:
    pass

def outer(obj: Child):
    def inner():
        return obj.note()
";
    let env = inherited_fixture_env()?;
    let toolchain = env.toolchain()?;
    let cancelled = AtomicBool::new(false);
    let fragment = compile_inherited_fragment(SOURCE, &env.work, &toolchain, &cancelled)?;
    let view = fragment.view()?;
    let inner_owner = entity_ordinal_at_index(&view, b"inner", EntityKind::Function, 0)?;
    let decoy_note = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 0)?;
    let calls = read_method_calls(&view, inner_owner)?;
    if calls.len() != 1 {
        return Err(TestError::Falsified("inner owns one MethodCall"));
    }
    assert_universe_method(calls[0], "missing member stays a pypi universe key")?;
    if calls[0].occurrence.confidence != OccurrenceConfidence::Index {
        return Err(TestError::Falsified("missing member universe method confidence is Index"));
    }
    if matches!(
        &calls[0].occurrence.target,
        OccurrenceTarget::Local(target) if target.raw == decoy_note
    ) {
        return Err(TestError::Falsified("inner obj.note() must not resolve to Decoy.note"));
    }
    fs::remove_dir_all(&env.work).map_err(|source| TestError::Io("remove scratch", source))?;
    Ok(())
}

#[test]
fn py_closure_parameter_field_wins_over_method() -> Result<(), TestError> {
    const SOURCE: &[u8] = b"\
class Child:
    score = 1

    def score(self):
        return self.score

def outer(obj: Child):
    def inner():
        return obj.score
";
    let env = inherited_fixture_env()?;
    let toolchain = env.toolchain()?;
    let cancelled = AtomicBool::new(false);
    let fragment = compile_inherited_fragment(SOURCE, &env.work, &toolchain, &cancelled)?;
    let view = fragment.view()?;
    let inner_owner = entity_ordinal_at_index(&view, b"inner", EntityKind::Function, 0)?;
    let score_field = entity_ordinal_at_index(&view, b"score", EntityKind::Field, 0)?;
    let score_method = entity_ordinal_at_index(&view, b"score", EntityKind::Function, 0)?;
    let calls = read_method_calls(&view, inner_owner)?;
    if !calls.is_empty() {
        return Err(TestError::Falsified("inner owns zero MethodCalls"));
    }
    let reads = read_field_accesses(&view, inner_owner)?;
    if reads.len() != 1 {
        return Err(TestError::Falsified("inner owns one FieldAccess"));
    }
    assert_local_index(reads[0], score_field, "inner obj.score resolves to Child.score field")?;
    if matches!(
        &reads[0].occurrence.target,
        OccurrenceTarget::Local(target) if target.raw == score_method
    ) {
        return Err(TestError::Falsified("inner obj.score must not resolve to Child.score method"));
    }
    fs::remove_dir_all(&env.work).map_err(|source| TestError::Io("remove scratch", source))?;
    Ok(())
}

#[test]
fn py_closure_parameter_call_binds_method_not_field() -> Result<(), TestError> {
    const SOURCE: &[u8] = b"\
class Child:
    note = 1

    def note(self):
        return 0

def outer(obj: Child):
    def inner():
        return obj.note()
";
    let env = inherited_fixture_env()?;
    let toolchain = env.toolchain()?;
    let cancelled = AtomicBool::new(false);
    let fragment = compile_inherited_fragment(SOURCE, &env.work, &toolchain, &cancelled)?;
    let view = fragment.view()?;
    let inner_owner = entity_ordinal_at_index(&view, b"inner", EntityKind::Function, 0)?;
    let note_field = entity_ordinal_at_index(&view, b"note", EntityKind::Field, 0)?;
    let note_method = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 0)?;
    let calls = read_method_calls(&view, inner_owner)?;
    if calls.len() != 1 {
        return Err(TestError::Falsified("inner owns one MethodCall"));
    }
    assert_local_index(calls[0], note_method, "inner obj.note() resolves to Child.note method")?;
    if matches!(
        &calls[0].occurrence.target,
        OccurrenceTarget::Local(target) if target.raw == note_field
    ) {
        return Err(TestError::Falsified("inner obj.note() must not resolve to Child.note field"));
    }
    fs::remove_dir_all(&env.work).map_err(|source| TestError::Io("remove scratch", source))?;
    Ok(())
}

#[test]
fn py_closure_parameter_ambiguous_union_stays_universe() -> Result<(), TestError> {
    const SOURCE: &[u8] = b"\
class Decoy:
    def note(self):
        return 0

class Child:
    def note(self):
        return 1

def outer(obj: Child | Decoy):
    def inner():
        return obj.note()
";
    let env = inherited_fixture_env()?;
    let toolchain = env.toolchain()?;
    let cancelled = AtomicBool::new(false);
    let fragment = compile_inherited_fragment(SOURCE, &env.work, &toolchain, &cancelled)?;
    let view = fragment.view()?;
    let inner_owner = entity_ordinal_at_index(&view, b"inner", EntityKind::Function, 0)?;
    let decoy_note = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 0)?;
    let child_note = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 1)?;
    let calls = read_method_calls(&view, inner_owner)?;
    if calls.len() != 1 {
        return Err(TestError::Falsified("inner owns one MethodCall"));
    }
    assert_universe_method(calls[0], "ambiguous union stays a pypi universe key")?;
    if calls[0].occurrence.confidence != OccurrenceConfidence::Index {
        return Err(TestError::Falsified("ambiguous union universe method confidence is Index"));
    }
    if matches!(
        &calls[0].occurrence.target,
        OccurrenceTarget::Local(target) if target.raw == decoy_note
    ) {
        return Err(TestError::Falsified("inner obj.note() must not resolve to Decoy.note"));
    }
    if matches!(
        &calls[0].occurrence.target,
        OccurrenceTarget::Local(target) if target.raw == child_note
    ) {
        return Err(TestError::Falsified("inner obj.note() must not bind Child.note"));
    }
    fs::remove_dir_all(&env.work).map_err(|source| TestError::Io("remove scratch", source))?;
    Ok(())
}

#[test]
fn py_local_annotation_use_before_assignment_stays_universe() -> Result<(), TestError> {
    const SOURCE: &[u8] = b"\
class Child:
    def note(self):
        return 1

def read():
    return obj.note()
    obj: Child = Child()
";
    let env = inherited_fixture_env()?;
    let toolchain = env.toolchain()?;
    let cancelled = AtomicBool::new(false);
    let fragment = compile_inherited_fragment(SOURCE, &env.work, &toolchain, &cancelled)?;
    let view = fragment.view()?;
    let read_owner = entity_ordinal_at_index(&view, b"read", EntityKind::Function, 0)?;
    let child_note = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 0)?;
    let calls = read_method_calls(&view, read_owner)?;
    if calls.len() != 1 {
        return Err(TestError::Falsified("read owns one MethodCall"));
    }
    assert_universe_method(calls[0], "use before assignment stays a pypi universe key")?;
    if calls[0].occurrence.confidence != OccurrenceConfidence::Index {
        return Err(TestError::Falsified("use before assignment universe method confidence is Index"));
    }
    if matches!(
        &calls[0].occurrence.target,
        OccurrenceTarget::Local(target) if target.raw == child_note
    ) {
        return Err(TestError::Falsified("obj.note() before assignment must not bind Child.note"));
    }
    fs::remove_dir_all(&env.work).map_err(|source| TestError::Io("remove scratch", source))?;
    Ok(())
}

#[test]
fn py_local_annotation_missing_member_stays_attribute_key() -> Result<(), TestError> {
    const SOURCE: &[u8] = b"\
class Decoy:
    def note(self):
        return 0

class Child:
    pass

def read():
    obj: Child
    return obj.note()
";
    let env = inherited_fixture_env()?;
    let toolchain = env.toolchain()?;
    let cancelled = AtomicBool::new(false);
    let fragment = compile_inherited_fragment(SOURCE, &env.work, &toolchain, &cancelled)?;
    let view = fragment.view()?;
    let read_owner = entity_ordinal_at_index(&view, b"read", EntityKind::Function, 0)?;
    let decoy_note = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 0)?;
    let calls = read_method_calls(&view, read_owner)?;
    if calls.len() != 1 {
        return Err(TestError::Falsified("read owns one MethodCall"));
    }
    assert_universe_method(calls[0], "missing member stays a pypi universe key")?;
    if calls[0].occurrence.confidence != OccurrenceConfidence::Index {
        return Err(TestError::Falsified("missing member universe method confidence is Index"));
    }
    if matches!(
        &calls[0].occurrence.target,
        OccurrenceTarget::Local(target) if target.raw == decoy_note
    ) {
        return Err(TestError::Falsified("obj.note() must not resolve to Decoy.note"));
    }
    fs::remove_dir_all(&env.work).map_err(|source| TestError::Io("remove scratch", source))?;
    Ok(())
}

#[test]
fn py_local_annotation_method_body_targets_child_note_not_decoy() -> Result<(), TestError> {
    const SOURCE: &[u8] = b"\
class Decoy:
    def note(self):
        return 0

class Child:
    def note(self):
        return 1

class Box:
    def read(self):
        obj: Child = Child()
        return obj.note()
";
    let env = inherited_fixture_env()?;
    let toolchain = env.toolchain()?;
    let cancelled = AtomicBool::new(false);
    let fragment = compile_inherited_fragment(SOURCE, &env.work, &toolchain, &cancelled)?;
    let view = fragment.view()?;
    let read_owner = entity_ordinal_at_index(&view, b"read", EntityKind::Function, 0)?;
    let decoy_note = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 0)?;
    let child_note = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 1)?;
    let calls = read_method_calls(&view, read_owner)?;
    if calls.len() != 1 {
        return Err(TestError::Falsified("read owns one MethodCall"));
    }
    assert_local_index(calls[0], child_note, "obj.note() resolves to Child.note")?;
    if matches!(
        &calls[0].occurrence.target,
        OccurrenceTarget::Local(target) if target.raw == decoy_note
    ) {
        return Err(TestError::Falsified("obj.note() must not resolve to Decoy.note"));
    }
    fs::remove_dir_all(&env.work).map_err(|source| TestError::Io("remove scratch", source))?;
    Ok(())
}

#[test]
fn py_local_annotation_two_classes_stay_universe() -> Result<(), TestError> {
    const SOURCE: &[u8] = b"\
class Child:
    def note(self):
        return 1

def nest():
    class Child:
        def note(self):
            return 2

def read():
    obj: Child = Child()
    return obj.note()
";
    let env = inherited_fixture_env()?;
    let toolchain = env.toolchain()?;
    let cancelled = AtomicBool::new(false);
    let fragment = compile_inherited_fragment(SOURCE, &env.work, &toolchain, &cancelled)?;
    let view = fragment.view()?;
    let read_owner = entity_ordinal_at_index(&view, b"read", EntityKind::Function, 0)?;
    let module_note = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 0)?;
    let nested_note = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 1)?;
    let calls = read_method_calls(&view, read_owner)?;
    if calls.len() != 1 {
        return Err(TestError::Falsified("read owns one MethodCall"));
    }
    assert_universe_method(calls[0], "two Child classes stay a pypi universe key")?;
    if calls[0].occurrence.confidence != OccurrenceConfidence::Index {
        return Err(TestError::Falsified("two Child classes universe method confidence is Index"));
    }
    if matches!(
        &calls[0].occurrence.target,
        OccurrenceTarget::Local(target) if target.raw == module_note
    ) {
        return Err(TestError::Falsified("obj.note() must not bind module Child.note"));
    }
    if matches!(
        &calls[0].occurrence.target,
        OccurrenceTarget::Local(target) if target.raw == nested_note
    ) {
        return Err(TestError::Falsified("obj.note() must not bind nested Child.note"));
    }
    fs::remove_dir_all(&env.work).map_err(|source| TestError::Io("remove scratch", source))?;
    Ok(())
}

#[test]
fn py_local_annotation_list_read_does_not_take_module_field() -> Result<(), TestError> {
    const SOURCE: &[u8] = b"\
class Holder:
    note = 1

class Child:
    def note(self):
        return 2

def read():
    obj: list[Child] = []
    return obj.note
";
    let env = inherited_fixture_env()?;
    let toolchain = env.toolchain()?;
    let cancelled = AtomicBool::new(false);
    let fragment = compile_inherited_fragment(SOURCE, &env.work, &toolchain, &cancelled)?;
    let view = fragment.view()?;
    let read_owner = entity_ordinal_at_index(&view, b"read", EntityKind::Function, 0)?;
    let holder_note = entity_ordinal_at_index(&view, b"note", EntityKind::Field, 0)?;
    let child_note = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 0)?;
    let reads = read_field_accesses(&view, read_owner)?;
    if reads.len() != 1 {
        return Err(TestError::Falsified("read owns one FieldAccess"));
    }
    if !matches!(
        &reads[0].occurrence.target,
        OccurrenceTarget::Foreign(key)
            if key.path == "note"
                && key.display == "note"
                && key.kind == Some(EntityKind::Field)
                && matches!(key.origin, ForeignOrigin::Universe { ecosystem: "pypi" })
    ) {
        return Err(TestError::Falsified("list[Child] read stays a pypi universe field key"));
    }
    if reads[0].occurrence.confidence != OccurrenceConfidence::Index {
        return Err(TestError::Falsified("list[Child] read universe field confidence is Index"));
    }
    if matches!(
        &reads[0].occurrence.target,
        OccurrenceTarget::Local(target) if target.raw == holder_note
    ) {
        return Err(TestError::Falsified("list[Child] read must not take Holder.note"));
    }
    if matches!(
        &reads[0].occurrence.target,
        OccurrenceTarget::Local(target) if target.raw == child_note
    ) {
        return Err(TestError::Falsified("list[Child] read must not bind Child.note"));
    }
    fs::remove_dir_all(&env.work).map_err(|source| TestError::Io("remove scratch", source))?;
    Ok(())
}

#[test]
fn py_local_annotation_nested_class_field_stays_universe() -> Result<(), TestError> {
    const SOURCE: &[u8] = b"\
class Decoy:
    def note(self):
        return 0

class Child:
    def note(self):
        return 1

def read():
    class Holder:
        obj: Child = Child()
    return obj.note()
";
    let env = inherited_fixture_env()?;
    let toolchain = env.toolchain()?;
    let cancelled = AtomicBool::new(false);
    let fragment = compile_inherited_fragment(SOURCE, &env.work, &toolchain, &cancelled)?;
    let view = fragment.view()?;
    let read_owner = entity_ordinal_at_index(&view, b"read", EntityKind::Function, 0)?;
    let decoy_note = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 0)?;
    let child_note = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 1)?;
    let calls = read_method_calls(&view, read_owner)?;
    if calls.len() != 1 {
        return Err(TestError::Falsified("read owns one MethodCall"));
    }
    assert_universe_method(calls[0], "nested class field stays a pypi universe key")?;
    if calls[0].occurrence.confidence != OccurrenceConfidence::Index {
        return Err(TestError::Falsified("nested class field universe method confidence is Index"));
    }
    if matches!(
        &calls[0].occurrence.target,
        OccurrenceTarget::Local(target) if target.raw == decoy_note
    ) {
        return Err(TestError::Falsified("obj.note() must not bind Decoy.note"));
    }
    if matches!(
        &calls[0].occurrence.target,
        OccurrenceTarget::Local(target) if target.raw == child_note
    ) {
        return Err(TestError::Falsified("obj.note() must not bind Child.note"));
    }
    fs::remove_dir_all(&env.work).map_err(|source| TestError::Io("remove scratch", source))?;
    Ok(())
}

#[test]
fn py_module_annotation_call_targets_child_note_not_decoy() -> Result<(), TestError> {
    const SOURCE: &[u8] = b"\
class Decoy:
    def note(self):
        return 0

class Child:
    def note(self):
        return 1

obj: Child = Child()

def read():
    return obj.note()
";
    let env = inherited_fixture_env()?;
    let toolchain = env.toolchain()?;
    let cancelled = AtomicBool::new(false);
    let fragment = compile_inherited_fragment(SOURCE, &env.work, &toolchain, &cancelled)?;
    let view = fragment.view()?;
    let read_owner = entity_ordinal_at_index(&view, b"read", EntityKind::Function, 0)?;
    let decoy_note = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 0)?;
    let child_note = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 1)?;
    let calls = read_method_calls(&view, read_owner)?;
    if calls.len() != 1 {
        return Err(TestError::Falsified("read owns one MethodCall"));
    }
    assert_local_index(calls[0], child_note, "obj.note() resolves to Child.note")?;
    if matches!(
        &calls[0].occurrence.target,
        OccurrenceTarget::Local(target) if target.raw == decoy_note
    ) {
        return Err(TestError::Falsified("obj.note() must not resolve to Decoy.note"));
    }
    fs::remove_dir_all(&env.work).map_err(|source| TestError::Io("remove scratch", source))?;
    Ok(())
}

#[test]
fn py_module_annotation_field_targets_child_score_not_decoy() -> Result<(), TestError> {
    const SOURCE: &[u8] = b"\
class Decoy:
    score = 9

class Child:
    score = 1

obj: Child = Child()

def read():
    return obj.score
";
    let env = inherited_fixture_env()?;
    let toolchain = env.toolchain()?;
    let cancelled = AtomicBool::new(false);
    let fragment = compile_inherited_fragment(SOURCE, &env.work, &toolchain, &cancelled)?;
    let view = fragment.view()?;
    let read_owner = entity_ordinal_at_index(&view, b"read", EntityKind::Function, 0)?;
    let decoy_score = entity_ordinal_at_index(&view, b"score", EntityKind::Field, 0)?;
    let child_score = entity_ordinal_at_index(&view, b"score", EntityKind::Field, 1)?;
    let calls = read_method_calls(&view, read_owner)?;
    if !calls.is_empty() {
        return Err(TestError::Falsified("read owns zero MethodCalls"));
    }
    let reads = read_field_accesses(&view, read_owner)?;
    if reads.len() != 1 {
        return Err(TestError::Falsified("read owns one FieldAccess"));
    }
    assert_local_index(reads[0], child_score, "obj.score resolves to Child.score")?;
    if matches!(
        &reads[0].occurrence.target,
        OccurrenceTarget::Local(target) if target.raw == decoy_score
    ) {
        return Err(TestError::Falsified("obj.score must not resolve to Decoy.score"));
    }
    fs::remove_dir_all(&env.work).map_err(|source| TestError::Io("remove scratch", source))?;
    Ok(())
}

#[test]
fn py_module_annotation_value_targets_child_note_not_decoy() -> Result<(), TestError> {
    const SOURCE: &[u8] = b"\
class Decoy:
    def note(self):
        return 0

class Child:
    def note(self):
        return 1

obj: Child = Child()

def read():
    return obj.note
";
    let env = inherited_fixture_env()?;
    let toolchain = env.toolchain()?;
    let cancelled = AtomicBool::new(false);
    let fragment = compile_inherited_fragment(SOURCE, &env.work, &toolchain, &cancelled)?;
    let view = fragment.view()?;
    let read_owner = entity_ordinal_at_index(&view, b"read", EntityKind::Function, 0)?;
    let decoy_note = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 0)?;
    let child_note = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 1)?;
    let calls = read_method_calls(&view, read_owner)?;
    if !calls.is_empty() {
        return Err(TestError::Falsified("read owns zero MethodCalls"));
    }
    let reads = read_field_accesses(&view, read_owner)?;
    if reads.len() != 1 {
        return Err(TestError::Falsified("read owns one FieldAccess"));
    }
    assert_local_index(reads[0], child_note, "obj.note resolves to Child.note")?;
    if matches!(
        &reads[0].occurrence.target,
        OccurrenceTarget::Local(target) if target.raw == decoy_note
    ) {
        return Err(TestError::Falsified("obj.note must not resolve to Decoy.note"));
    }
    fs::remove_dir_all(&env.work).map_err(|source| TestError::Io("remove scratch", source))?;
    Ok(())
}

#[test]
fn py_module_annotation_forward_call_targets_child_note_not_decoy() -> Result<(), TestError> {
    const SOURCE: &[u8] = b"\
obj: Child = Child()

def read():
    return obj.note()

class Decoy:
    def note(self):
        return 0

class Child:
    def note(self):
        return 1
";
    let env = inherited_fixture_env()?;
    let toolchain = env.toolchain()?;
    let cancelled = AtomicBool::new(false);
    let fragment = compile_inherited_fragment(SOURCE, &env.work, &toolchain, &cancelled)?;
    let view = fragment.view()?;
    let read_owner = entity_ordinal_at_index(&view, b"read", EntityKind::Function, 0)?;
    let decoy_note = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 0)?;
    let child_note = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 1)?;
    let calls = read_method_calls(&view, read_owner)?;
    if calls.len() != 1 {
        return Err(TestError::Falsified("read owns one MethodCall"));
    }
    assert_local_index(calls[0], child_note, "obj.note() resolves to Child.note")?;
    if matches!(
        &calls[0].occurrence.target,
        OccurrenceTarget::Local(target) if target.raw == decoy_note
    ) {
        return Err(TestError::Falsified("obj.note() must not resolve to Decoy.note"));
    }
    fs::remove_dir_all(&env.work).map_err(|source| TestError::Io("remove scratch", source))?;
    Ok(())
}

#[test]
fn py_module_annotation_inherited_call_targets_base_note_not_decoy() -> Result<(), TestError> {
    const SOURCE: &[u8] = b"\
class Decoy:
    def note(self):
        return 0

class Base:
    def note(self):
        return 1

class Child(Base):
    pass

obj: Child = Child()

def read():
    return obj.note()
";
    let env = inherited_fixture_env()?;
    let toolchain = env.toolchain()?;
    let cancelled = AtomicBool::new(false);
    let fragment = compile_inherited_fragment(SOURCE, &env.work, &toolchain, &cancelled)?;
    let view = fragment.view()?;
    let read_owner = entity_ordinal_at_index(&view, b"read", EntityKind::Function, 0)?;
    let decoy_note = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 0)?;
    let base_note = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 1)?;
    let calls = read_method_calls(&view, read_owner)?;
    if calls.len() != 1 {
        return Err(TestError::Falsified("read owns one MethodCall"));
    }
    assert_local_index(calls[0], base_note, "obj.note() resolves to Base.note")?;
    if matches!(
        &calls[0].occurrence.target,
        OccurrenceTarget::Local(target) if target.raw == decoy_note
    ) {
        return Err(TestError::Falsified("obj.note() must not resolve to Decoy.note"));
    }
    fs::remove_dir_all(&env.work).map_err(|source| TestError::Io("remove scratch", source))?;
    Ok(())
}

#[test]
fn py_module_annotation_generic_call_targets_child_note_not_decoy() -> Result<(), TestError> {
    const SOURCE: &[u8] = b"\
class Decoy:
    def note(self):
        return 0

class Child:
    def note(self):
        return 1

obj: Child[int] = Child()

def read():
    return obj.note()
";
    let env = inherited_fixture_env()?;
    let toolchain = env.toolchain()?;
    let cancelled = AtomicBool::new(false);
    let fragment = compile_inherited_fragment(SOURCE, &env.work, &toolchain, &cancelled)?;
    let view = fragment.view()?;
    let read_owner = entity_ordinal_at_index(&view, b"read", EntityKind::Function, 0)?;
    let decoy_note = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 0)?;
    let child_note = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 1)?;
    let calls = read_method_calls(&view, read_owner)?;
    if calls.len() != 1 {
        return Err(TestError::Falsified("read owns one MethodCall"));
    }
    assert_local_index(calls[0], child_note, "obj.note() resolves to Child.note")?;
    if matches!(
        &calls[0].occurrence.target,
        OccurrenceTarget::Local(target) if target.raw == decoy_note
    ) {
        return Err(TestError::Falsified("obj.note() must not resolve to Decoy.note"));
    }
    fs::remove_dir_all(&env.work).map_err(|source| TestError::Io("remove scratch", source))?;
    Ok(())
}

#[test]
fn py_module_annotation_union_call_targets_child_note_not_decoy() -> Result<(), TestError> {
    const SOURCE: &[u8] = b"\
class Decoy:
    def note(self):
        return 0

class Child:
    def note(self):
        return 1

obj: Child | None = Child()

def read():
    return obj.note()
";
    let env = inherited_fixture_env()?;
    let toolchain = env.toolchain()?;
    let cancelled = AtomicBool::new(false);
    let fragment = compile_inherited_fragment(SOURCE, &env.work, &toolchain, &cancelled)?;
    let view = fragment.view()?;
    let read_owner = entity_ordinal_at_index(&view, b"read", EntityKind::Function, 0)?;
    let decoy_note = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 0)?;
    let child_note = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 1)?;
    let calls = read_method_calls(&view, read_owner)?;
    if calls.len() != 1 {
        return Err(TestError::Falsified("read owns one MethodCall"));
    }
    assert_local_index(calls[0], child_note, "obj.note() resolves to Child.note")?;
    if matches!(
        &calls[0].occurrence.target,
        OccurrenceTarget::Local(target) if target.raw == decoy_note
    ) {
        return Err(TestError::Falsified("obj.note() must not resolve to Decoy.note"));
    }
    fs::remove_dir_all(&env.work).map_err(|source| TestError::Io("remove scratch", source))?;
    Ok(())
}

#[test]
fn py_module_annotation_quoted_call_targets_child_note_not_decoy() -> Result<(), TestError> {
    const SOURCE: &[u8] = b"\
class Decoy:
    def note(self):
        return 0

class Child:
    def note(self):
        return 1

obj: \"Child\" = Child()

def read():
    return obj.note()
";
    let env = inherited_fixture_env()?;
    let toolchain = env.toolchain()?;
    let cancelled = AtomicBool::new(false);
    let fragment = compile_inherited_fragment(SOURCE, &env.work, &toolchain, &cancelled)?;
    let view = fragment.view()?;
    let read_owner = entity_ordinal_at_index(&view, b"read", EntityKind::Function, 0)?;
    let decoy_note = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 0)?;
    let child_note = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 1)?;
    let calls = read_method_calls(&view, read_owner)?;
    if calls.len() != 1 {
        return Err(TestError::Falsified("read owns one MethodCall"));
    }
    assert_local_index(calls[0], child_note, "obj.note() resolves to Child.note")?;
    if matches!(
        &calls[0].occurrence.target,
        OccurrenceTarget::Local(target) if target.raw == decoy_note
    ) {
        return Err(TestError::Falsified("obj.note() must not resolve to Decoy.note"));
    }
    fs::remove_dir_all(&env.work).map_err(|source| TestError::Io("remove scratch", source))?;
    Ok(())
}

#[test]
fn py_module_annotation_assignment_stays_universe() -> Result<(), TestError> {
    const SOURCE: &[u8] = b"\
class Child:
    def note(self):
        return 1

obj: Child = Child()

def read():
    obj = 1
    return obj.note()
";
    let env = inherited_fixture_env()?;
    let toolchain = env.toolchain()?;
    let cancelled = AtomicBool::new(false);
    let fragment = compile_inherited_fragment(SOURCE, &env.work, &toolchain, &cancelled)?;
    let view = fragment.view()?;
    let read_owner = entity_ordinal_at_index(&view, b"read", EntityKind::Function, 0)?;
    let child_note = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 0)?;
    let calls = read_method_calls(&view, read_owner)?;
    if calls.len() != 1 {
        return Err(TestError::Falsified("read owns one MethodCall"));
    }
    assert_universe_method(calls[0], "assignment shadows module annotation")?;
    if calls[0].occurrence.confidence != OccurrenceConfidence::Index {
        return Err(TestError::Falsified("assignment universe method confidence is Index"));
    }
    if matches!(
        &calls[0].occurrence.target,
        OccurrenceTarget::Local(target) if target.raw == child_note
    ) {
        return Err(TestError::Falsified("obj.note() must not bind Child.note"));
    }
    fs::remove_dir_all(&env.work).map_err(|source| TestError::Io("remove scratch", source))?;
    Ok(())
}

#[test]
fn py_module_annotation_assignment_read_does_not_take_module_field() -> Result<(), TestError> {
    const SOURCE: &[u8] = b"\
class Holder:
    note = 1

class Child:
    def note(self):
        return 2

obj: Child = Child()

def read():
    obj = 1
    return obj.note
";
    let env = inherited_fixture_env()?;
    let toolchain = env.toolchain()?;
    let cancelled = AtomicBool::new(false);
    let fragment = compile_inherited_fragment(SOURCE, &env.work, &toolchain, &cancelled)?;
    let view = fragment.view()?;
    let read_owner = entity_ordinal_at_index(&view, b"read", EntityKind::Function, 0)?;
    let holder_note = entity_ordinal_at_index(&view, b"note", EntityKind::Field, 0)?;
    let child_note = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 0)?;
    let calls = read_method_calls(&view, read_owner)?;
    if !calls.is_empty() {
        return Err(TestError::Falsified("read owns zero MethodCalls"));
    }
    let reads = read_field_accesses(&view, read_owner)?;
    if reads.len() != 1 {
        return Err(TestError::Falsified("read owns one FieldAccess"));
    }
    assert_universe_field(reads[0], "assignment read stays a pypi universe field key")?;
    if reads[0].occurrence.confidence != OccurrenceConfidence::Index {
        return Err(TestError::Falsified("assignment read universe field confidence is Index"));
    }
    if matches!(
        &reads[0].occurrence.target,
        OccurrenceTarget::Local(target) if target.raw == holder_note
    ) {
        return Err(TestError::Falsified("obj.note must not resolve to Holder.note"));
    }
    if matches!(
        &reads[0].occurrence.target,
        OccurrenceTarget::Local(target) if target.raw == child_note
    ) {
        return Err(TestError::Falsified("obj.note must not bind Child.note"));
    }
    fs::remove_dir_all(&env.work).map_err(|source| TestError::Io("remove scratch", source))?;
    Ok(())
}

#[test]
fn py_module_annotation_for_target_stays_universe() -> Result<(), TestError> {
    const SOURCE: &[u8] = b"\
class Child:
    def note(self):
        return 1

obj: Child = Child()
items = [1, 2, 3]

def read():
    for obj in items:
        return obj.note()
";
    let env = inherited_fixture_env()?;
    let toolchain = env.toolchain()?;
    let cancelled = AtomicBool::new(false);
    let fragment = compile_inherited_fragment(SOURCE, &env.work, &toolchain, &cancelled)?;
    let view = fragment.view()?;
    let read_owner = entity_ordinal_at_index(&view, b"read", EntityKind::Function, 0)?;
    let child_note = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 0)?;
    let calls = read_method_calls(&view, read_owner)?;
    if calls.len() != 1 {
        return Err(TestError::Falsified("read owns one MethodCall"));
    }
    assert_universe_method(calls[0], "for-target obj stays a pypi universe key")?;
    if calls[0].occurrence.confidence != OccurrenceConfidence::Index {
        return Err(TestError::Falsified("for-target universe method confidence is Index"));
    }
    if matches!(
        &calls[0].occurrence.target,
        OccurrenceTarget::Local(target) if target.raw == child_note
    ) {
        return Err(TestError::Falsified("for-target obj.note() must not bind Child.note"));
    }
    fs::remove_dir_all(&env.work).map_err(|source| TestError::Io("remove scratch", source))?;
    Ok(())
}

#[test]
fn py_module_annotation_augassign_stays_universe() -> Result<(), TestError> {
    const SOURCE: &[u8] = b"\
class Child:
    def note(self):
        return 1

obj: Child = Child()

def read():
    obj += 1
    return obj.note()
";
    let env = inherited_fixture_env()?;
    let toolchain = env.toolchain()?;
    let cancelled = AtomicBool::new(false);
    let fragment = compile_inherited_fragment(SOURCE, &env.work, &toolchain, &cancelled)?;
    let view = fragment.view()?;
    let read_owner = entity_ordinal_at_index(&view, b"read", EntityKind::Function, 0)?;
    let child_note = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 0)?;
    let calls = read_method_calls(&view, read_owner)?;
    if calls.len() != 1 {
        return Err(TestError::Falsified("read owns one MethodCall"));
    }
    assert_universe_method(calls[0], "augassign shadows module annotation")?;
    if calls[0].occurrence.confidence != OccurrenceConfidence::Index {
        return Err(TestError::Falsified("augassign universe method confidence is Index"));
    }
    if matches!(
        &calls[0].occurrence.target,
        OccurrenceTarget::Local(target) if target.raw == child_note
    ) {
        return Err(TestError::Falsified("obj.note() must not bind Child.note"));
    }
    fs::remove_dir_all(&env.work).map_err(|source| TestError::Io("remove scratch", source))?;
    Ok(())
}

#[test]
fn py_module_annotation_nested_assignment_stays_universe() -> Result<(), TestError> {
    const SOURCE: &[u8] = b"\
class Child:
    def note(self):
        return 1

obj: Child = Child()

def outer():
    obj = 1

    def inner():
        return obj.note()
";
    let env = inherited_fixture_env()?;
    let toolchain = env.toolchain()?;
    let cancelled = AtomicBool::new(false);
    let fragment = compile_inherited_fragment(SOURCE, &env.work, &toolchain, &cancelled)?;
    let view = fragment.view()?;
    let inner_owner = entity_ordinal_at_index(&view, b"inner", EntityKind::Function, 0)?;
    let child_note = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 0)?;
    let calls = read_method_calls(&view, inner_owner)?;
    if calls.len() != 1 {
        return Err(TestError::Falsified("inner owns one MethodCall"));
    }
    assert_universe_method(calls[0], "outer assignment shadows module annotation")?;
    if calls[0].occurrence.confidence != OccurrenceConfidence::Index {
        return Err(TestError::Falsified("nested assignment universe method confidence is Index"));
    }
    if matches!(
        &calls[0].occurrence.target,
        OccurrenceTarget::Local(target) if target.raw == child_note
    ) {
        return Err(TestError::Falsified("inner obj.note() must not bind Child.note"));
    }
    fs::remove_dir_all(&env.work).map_err(|source| TestError::Io("remove scratch", source))?;
    Ok(())
}

#[test]
fn py_module_annotation_nested_use_targets_child_note_not_decoy() -> Result<(), TestError> {
    const SOURCE: &[u8] = b"\
class Decoy:
    def note(self):
        return 0

class Child:
    def note(self):
        return 1

obj: Child = Child()

def outer():
    def inner():
        return obj.note()
";
    let env = inherited_fixture_env()?;
    let toolchain = env.toolchain()?;
    let cancelled = AtomicBool::new(false);
    let fragment = compile_inherited_fragment(SOURCE, &env.work, &toolchain, &cancelled)?;
    let view = fragment.view()?;
    let inner_owner = entity_ordinal_at_index(&view, b"inner", EntityKind::Function, 0)?;
    let decoy_note = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 0)?;
    let child_note = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 1)?;
    let calls = read_method_calls(&view, inner_owner)?;
    if calls.len() != 1 {
        return Err(TestError::Falsified("inner owns one MethodCall"));
    }
    assert_local_index(calls[0], child_note, "inner obj.note() resolves to Child.note")?;
    if matches!(
        &calls[0].occurrence.target,
        OccurrenceTarget::Local(target) if target.raw == decoy_note
    ) {
        return Err(TestError::Falsified("inner obj.note() must not resolve to Decoy.note"));
    }
    fs::remove_dir_all(&env.work).map_err(|source| TestError::Io("remove scratch", source))?;
    Ok(())
}

#[test]
fn py_module_annotation_comprehension_splits_scopes() -> Result<(), TestError> {
    const SOURCE: &[u8] = b"\
class Decoy:
    def note(self):
        return 0

class Child:
    def note(self):
        return 1

obj: Child = Child()

def read(items):
    rows = [obj.note() for obj in items]
    return obj.note()
";
    let env = inherited_fixture_env()?;
    let toolchain = env.toolchain()?;
    let cancelled = AtomicBool::new(false);
    let fragment = compile_inherited_fragment(SOURCE, &env.work, &toolchain, &cancelled)?;
    let view = fragment.view()?;
    let read_owner = entity_ordinal_at_index(&view, b"read", EntityKind::Function, 0)?;
    let decoy_note = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 0)?;
    let child_note = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 1)?;
    let calls = read_method_calls(&view, read_owner)?;
    if calls.len() != 2 {
        return Err(TestError::Falsified("read owns two MethodCalls"));
    }
    assert_universe_method(calls[0], "comprehension obj.note() stays a pypi universe key")?;
    if calls[0].occurrence.confidence != OccurrenceConfidence::Index {
        return Err(TestError::Falsified("comprehension universe method confidence is Index"));
    }
    if matches!(
        &calls[0].occurrence.target,
        OccurrenceTarget::Local(target) if target.raw == child_note
    ) {
        return Err(TestError::Falsified("comprehension obj.note() must not bind Child.note"));
    }
    assert_local_index(calls[1], child_note, "return obj.note() resolves to Child.note")?;
    if matches!(
        &calls[1].occurrence.target,
        OccurrenceTarget::Local(target) if target.raw == decoy_note
    ) {
        return Err(TestError::Falsified("return obj.note() must not resolve to Decoy.note"));
    }
    fs::remove_dir_all(&env.work).map_err(|source| TestError::Io("remove scratch", source))?;
    Ok(())
}

#[test]
fn py_module_annotation_nested_class_does_not_shadow() -> Result<(), TestError> {
    const SOURCE: &[u8] = b"\
class Decoy:
    def note(self):
        return 0

class Child:
    def note(self):
        return 1

obj: Child = Child()

def read():
    class Holder:
        obj = 1
    return obj.note()
";
    let env = inherited_fixture_env()?;
    let toolchain = env.toolchain()?;
    let cancelled = AtomicBool::new(false);
    let fragment = compile_inherited_fragment(SOURCE, &env.work, &toolchain, &cancelled)?;
    let view = fragment.view()?;
    let read_owner = entity_ordinal_at_index(&view, b"read", EntityKind::Function, 0)?;
    let decoy_note = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 0)?;
    let child_note = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 1)?;
    let calls = read_method_calls(&view, read_owner)?;
    if calls.len() != 1 {
        return Err(TestError::Falsified("read owns one MethodCall"));
    }
    assert_local_index(calls[0], child_note, "obj.note() resolves to Child.note")?;
    if matches!(
        &calls[0].occurrence.target,
        OccurrenceTarget::Local(target) if target.raw == decoy_note
    ) {
        return Err(TestError::Falsified("obj.note() must not resolve to Decoy.note"));
    }
    fs::remove_dir_all(&env.work).map_err(|source| TestError::Io("remove scratch", source))?;
    Ok(())
}

#[test]
fn py_module_annotation_global_statement_targets_child_note_not_decoy() -> Result<(), TestError> {
    const SOURCE: &[u8] = b"\
class Decoy:
    def note(self):
        return 0

class Child:
    def note(self):
        return 1

obj: Child = Child()

def read():
    global obj
    obj = 1
    return obj.note()
";
    let env = inherited_fixture_env()?;
    let toolchain = env.toolchain()?;
    let cancelled = AtomicBool::new(false);
    let fragment = compile_inherited_fragment(SOURCE, &env.work, &toolchain, &cancelled)?;
    let view = fragment.view()?;
    let read_owner = entity_ordinal_at_index(&view, b"read", EntityKind::Function, 0)?;
    let decoy_note = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 0)?;
    let child_note = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 1)?;
    let calls = read_method_calls(&view, read_owner)?;
    if calls.len() != 1 {
        return Err(TestError::Falsified("read owns one MethodCall"));
    }
    assert_local_index(calls[0], child_note, "global obj.note() resolves to Child.note")?;
    if matches!(
        &calls[0].occurrence.target,
        OccurrenceTarget::Local(target) if target.raw == decoy_note
    ) {
        return Err(TestError::Falsified("obj.note() must not resolve to Decoy.note"));
    }
    fs::remove_dir_all(&env.work).map_err(|source| TestError::Io("remove scratch", source))?;
    Ok(())
}

#[test]
fn py_module_annotation_nonlocal_stays_universe() -> Result<(), TestError> {
    const SOURCE: &[u8] = b"\
class Child:
    def note(self):
        return 1

obj: Child = Child()

def outer():
    obj = 1

    def inner():
        nonlocal obj
        return obj.note()
";
    let env = inherited_fixture_env()?;
    let toolchain = env.toolchain()?;
    let cancelled = AtomicBool::new(false);
    let fragment = compile_inherited_fragment(SOURCE, &env.work, &toolchain, &cancelled)?;
    let view = fragment.view()?;
    let inner_owner = entity_ordinal_at_index(&view, b"inner", EntityKind::Function, 0)?;
    let child_note = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 0)?;
    let calls = read_method_calls(&view, inner_owner)?;
    if calls.len() != 1 {
        return Err(TestError::Falsified("inner owns one MethodCall"));
    }
    assert_universe_method(calls[0], "nonlocal shadows module annotation")?;
    if calls[0].occurrence.confidence != OccurrenceConfidence::Index {
        return Err(TestError::Falsified("nonlocal universe method confidence is Index"));
    }
    if matches!(
        &calls[0].occurrence.target,
        OccurrenceTarget::Local(target) if target.raw == child_note
    ) {
        return Err(TestError::Falsified("inner obj.note() must not bind Child.note"));
    }
    fs::remove_dir_all(&env.work).map_err(|source| TestError::Io("remove scratch", source))?;
    Ok(())
}

#[test]
fn py_module_annotation_lambda_parameter_stays_universe() -> Result<(), TestError> {
    const SOURCE: &[u8] = b"\
class Decoy:
    def note(self):
        return 0

class Child:
    def note(self):
        return 1

obj: Child = Child()

def read():
    return (lambda obj: obj.note())(0)
";
    let env = inherited_fixture_env()?;
    let toolchain = env.toolchain()?;
    let cancelled = AtomicBool::new(false);
    let fragment = compile_inherited_fragment(SOURCE, &env.work, &toolchain, &cancelled)?;
    let view = fragment.view()?;
    let read_owner = entity_ordinal_at_index(&view, b"read", EntityKind::Function, 0)?;
    let decoy_note = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 0)?;
    let child_note = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 1)?;
    let calls = read_method_calls(&view, read_owner)?;
    if calls.len() != 1 {
        return Err(TestError::Falsified("read owns one MethodCall"));
    }
    assert_universe_method(calls[0], "lambda parameter shadows module annotation")?;
    if calls[0].occurrence.confidence != OccurrenceConfidence::Index {
        return Err(TestError::Falsified("lambda parameter universe method confidence is Index"));
    }
    if matches!(
        &calls[0].occurrence.target,
        OccurrenceTarget::Local(target) if target.raw == child_note
    ) {
        return Err(TestError::Falsified("lambda obj.note() must not bind Child.note"));
    }
    if matches!(
        &calls[0].occurrence.target,
        OccurrenceTarget::Local(target) if target.raw == decoy_note
    ) {
        return Err(TestError::Falsified("lambda obj.note() must not resolve to Decoy.note"));
    }
    fs::remove_dir_all(&env.work).map_err(|source| TestError::Io("remove scratch", source))?;
    Ok(())
}

#[test]
fn py_module_annotation_lambda_targets_child_note_not_decoy() -> Result<(), TestError> {
    const SOURCE: &[u8] = b"\
class Decoy:
    def note(self):
        return 0

class Child:
    def note(self):
        return 1

obj: Child = Child()

def read():
    return (lambda: obj.note())()
";
    let env = inherited_fixture_env()?;
    let toolchain = env.toolchain()?;
    let cancelled = AtomicBool::new(false);
    let fragment = compile_inherited_fragment(SOURCE, &env.work, &toolchain, &cancelled)?;
    let view = fragment.view()?;
    let read_owner = entity_ordinal_at_index(&view, b"read", EntityKind::Function, 0)?;
    let decoy_note = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 0)?;
    let child_note = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 1)?;
    let calls = read_method_calls(&view, read_owner)?;
    if calls.len() != 1 {
        return Err(TestError::Falsified("read owns one MethodCall"));
    }
    assert_local_index(calls[0], child_note, "lambda obj.note() resolves to Child.note")?;
    if matches!(
        &calls[0].occurrence.target,
        OccurrenceTarget::Local(target) if target.raw == decoy_note
    ) {
        return Err(TestError::Falsified("lambda obj.note() must not resolve to Decoy.note"));
    }
    fs::remove_dir_all(&env.work).map_err(|source| TestError::Io("remove scratch", source))?;
    Ok(())
}

#[test]
fn py_module_annotation_lambda_default_targets_child_note_not_decoy() -> Result<(), TestError> {
    const SOURCE: &[u8] = b"\
class Decoy:
    def note(self):
        return 0

class Child:
    def note(self):
        return 1

obj: Child = Child()

def read():
    return (lambda obj=obj.note(): obj)(0)
";
    let env = inherited_fixture_env()?;
    let toolchain = env.toolchain()?;
    let cancelled = AtomicBool::new(false);
    let fragment = compile_inherited_fragment(SOURCE, &env.work, &toolchain, &cancelled)?;
    let view = fragment.view()?;
    let read_owner = entity_ordinal_at_index(&view, b"read", EntityKind::Function, 0)?;
    let decoy_note = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 0)?;
    let child_note = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 1)?;
    let calls = read_method_calls(&view, read_owner)?;
    if calls.len() != 1 {
        return Err(TestError::Falsified("read owns one MethodCall"));
    }
    assert_local_index(
        calls[0],
        child_note,
        "lambda default obj.note() resolves to Child.note",
    )?;
    if matches!(
        &calls[0].occurrence.target,
        OccurrenceTarget::Local(target) if target.raw == decoy_note
    ) {
        return Err(TestError::Falsified("lambda default obj.note() must not resolve to Decoy.note"));
    }
    fs::remove_dir_all(&env.work).map_err(|source| TestError::Io("remove scratch", source))?;
    Ok(())
}

#[test]
fn py_module_annotation_list_read_does_not_take_module_field() -> Result<(), TestError> {
    const SOURCE: &[u8] = b"\
class Holder:
    note = 1

class Child:
    def note(self):
        return 2

obj: list[Child] = []

def read():
    return obj.note
";
    let env = inherited_fixture_env()?;
    let toolchain = env.toolchain()?;
    let cancelled = AtomicBool::new(false);
    let fragment = compile_inherited_fragment(SOURCE, &env.work, &toolchain, &cancelled)?;
    let view = fragment.view()?;
    let read_owner = entity_ordinal_at_index(&view, b"read", EntityKind::Function, 0)?;
    let holder_note = entity_ordinal_at_index(&view, b"note", EntityKind::Field, 0)?;
    let child_note = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 0)?;
    let reads = read_field_accesses(&view, read_owner)?;
    if reads.len() != 1 {
        return Err(TestError::Falsified("read owns one FieldAccess"));
    }
    if !matches!(
        &reads[0].occurrence.target,
        OccurrenceTarget::Foreign(key)
            if key.path == "note"
                && key.display == "note"
                && key.kind == Some(EntityKind::Field)
                && matches!(key.origin, ForeignOrigin::Universe { ecosystem: "pypi" })
    ) {
        return Err(TestError::Falsified("list[Child] read stays a pypi universe field key"));
    }
    if reads[0].occurrence.confidence != OccurrenceConfidence::Index {
        return Err(TestError::Falsified("list[Child] read universe field confidence is Index"));
    }
    if matches!(
        &reads[0].occurrence.target,
        OccurrenceTarget::Local(target) if target.raw == holder_note
    ) {
        return Err(TestError::Falsified("list[Child] read must not take Holder.note"));
    }
    if matches!(
        &reads[0].occurrence.target,
        OccurrenceTarget::Local(target) if target.raw == child_note
    ) {
        return Err(TestError::Falsified("list[Child] read must not bind Child.note"));
    }
    fs::remove_dir_all(&env.work).map_err(|source| TestError::Io("remove scratch", source))?;
    Ok(())
}

#[test]
fn py_module_annotation_dotted_read_stays_universe() -> Result<(), TestError> {
    const SOURCE: &[u8] = b"\
import pkg

score = 1

class Child:
    score = 2

obj: pkg.Child

def read():
    return obj.score
";
    // The annotation `pkg.Child` is module-owned and has no lane owner.
    // `read` records only `obj.score`.
    let env = inherited_fixture_env()?;
    let toolchain = env.toolchain()?;
    let cancelled = AtomicBool::new(false);
    let fragment = compile_inherited_fragment(SOURCE, &env.work, &toolchain, &cancelled)?;
    let view = fragment.view()?;
    let read_owner = entity_ordinal_at_index(&view, b"read", EntityKind::Function, 0)?;
    let module_score = entity_ordinal_at_index(&view, b"score", EntityKind::Static, 0)?;
    let child_score = entity_ordinal_at_index(&view, b"score", EntityKind::Field, 0)?;
    let calls = read_method_calls(&view, read_owner)?;
    if !calls.is_empty() {
        return Err(TestError::Falsified("read owns zero MethodCalls"));
    }
    let reads = read_field_accesses(&view, read_owner)?;
    if reads.len() != 1 {
        return Err(TestError::Falsified("read owns one FieldAccess"));
    }
    if !matches!(
        &reads[0].occurrence.target,
        OccurrenceTarget::Foreign(key)
            if key.path == "score"
                && key.display == "score"
                && key.kind == Some(EntityKind::Field)
                && matches!(key.origin, ForeignOrigin::Universe { ecosystem: "pypi" })
    ) {
        return Err(TestError::Falsified("obj.score stays a pypi universe field key"));
    }
    if reads[0].occurrence.confidence != OccurrenceConfidence::Index {
        return Err(TestError::Falsified("obj.score universe field confidence is Index"));
    }
    if matches!(
        &reads[0].occurrence.target,
        OccurrenceTarget::Local(target) if target.raw == module_score
    ) {
        return Err(TestError::Falsified("obj.score must not resolve to module score"));
    }
    if matches!(
        &reads[0].occurrence.target,
        OccurrenceTarget::Local(target) if target.raw == child_score
    ) {
        return Err(TestError::Falsified("obj.score must not resolve to Child.score"));
    }
    fs::remove_dir_all(&env.work).map_err(|source| TestError::Io("remove scratch", source))?;
    Ok(())
}

#[test]
fn py_module_annotation_rebind_keeps_first_constant() -> Result<(), TestError> {
    const SOURCE: &[u8] = b"\
class Child:
    def note(self):
        return 1

class Other:
    def note(self):
        return 2

obj: Child = Child()
obj: Other = Other()

def read():
    return obj.note()
";
    let env = inherited_fixture_env()?;
    let toolchain = env.toolchain()?;
    let cancelled = AtomicBool::new(false);
    let fragment = compile_inherited_fragment(SOURCE, &env.work, &toolchain, &cancelled)?;
    let view = fragment.view()?;
    let read_owner = entity_ordinal_at_index(&view, b"read", EntityKind::Function, 0)?;
    let child_note = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 0)?;
    let other_note = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 1)?;
    let calls = read_method_calls(&view, read_owner)?;
    if calls.len() != 1 {
        return Err(TestError::Falsified("read owns one MethodCall"));
    }
    assert_local_index(
        calls[0],
        child_note,
        "the first module constant obj: Child wins over the dropped rebinding",
    )?;
    if matches!(
        &calls[0].occurrence.target,
        OccurrenceTarget::Local(target) if target.raw == other_note
    ) {
        return Err(TestError::Falsified("obj.note() must not bind Other.note"));
    }
    fs::remove_dir_all(&env.work).map_err(|source| TestError::Io("remove scratch", source))?;
    Ok(())
}

#[test]
fn py_module_annotation_missing_member_stays_attribute_key() -> Result<(), TestError> {
    const SOURCE: &[u8] = b"\
class Decoy:
    def note(self):
        return 0

class Child:
    pass

obj: Child = Child()

def read():
    return obj.note()
";
    let env = inherited_fixture_env()?;
    let toolchain = env.toolchain()?;
    let cancelled = AtomicBool::new(false);
    let fragment = compile_inherited_fragment(SOURCE, &env.work, &toolchain, &cancelled)?;
    let view = fragment.view()?;
    let read_owner = entity_ordinal_at_index(&view, b"read", EntityKind::Function, 0)?;
    let decoy_note = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 0)?;
    let calls = read_method_calls(&view, read_owner)?;
    if calls.len() != 1 {
        return Err(TestError::Falsified("read owns one MethodCall"));
    }
    assert_universe_method(calls[0], "missing member stays a pypi universe key")?;
    if calls[0].occurrence.confidence != OccurrenceConfidence::Index {
        return Err(TestError::Falsified("missing member universe method confidence is Index"));
    }
    if matches!(
        &calls[0].occurrence.target,
        OccurrenceTarget::Local(target) if target.raw == decoy_note
    ) {
        return Err(TestError::Falsified("obj.note() must not resolve to Decoy.note"));
    }
    fs::remove_dir_all(&env.work).map_err(|source| TestError::Io("remove scratch", source))?;
    Ok(())
}

#[test]
fn py_module_annotation_method_body_targets_child_note_not_decoy() -> Result<(), TestError> {
    const SOURCE: &[u8] = b"\
class Decoy:
    def note(self):
        return 0

class Child:
    def note(self):
        return 1

obj: Child = Child()

class Box:
    def read(self):
        return obj.note()
";
    let env = inherited_fixture_env()?;
    let toolchain = env.toolchain()?;
    let cancelled = AtomicBool::new(false);
    let fragment = compile_inherited_fragment(SOURCE, &env.work, &toolchain, &cancelled)?;
    let view = fragment.view()?;
    let read_owner = entity_ordinal_at_index(&view, b"read", EntityKind::Function, 0)?;
    let decoy_note = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 0)?;
    let child_note = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 1)?;
    let calls = read_method_calls(&view, read_owner)?;
    if calls.len() != 1 {
        return Err(TestError::Falsified("read owns one MethodCall"));
    }
    assert_local_index(calls[0], child_note, "obj.note() resolves to Child.note")?;
    if matches!(
        &calls[0].occurrence.target,
        OccurrenceTarget::Local(target) if target.raw == decoy_note
    ) {
        return Err(TestError::Falsified("obj.note() must not resolve to Decoy.note"));
    }
    fs::remove_dir_all(&env.work).map_err(|source| TestError::Io("remove scratch", source))?;
    Ok(())
}

#[test]
fn py_module_annotation_parameter_wins() -> Result<(), TestError> {
    const SOURCE: &[u8] = b"\
class Decoy:
    def note(self):
        return 0

class Child:
    def note(self):
        return 1

obj: Child = Child()

def read(obj: Decoy):
    return obj.note()
";
    let env = inherited_fixture_env()?;
    let toolchain = env.toolchain()?;
    let cancelled = AtomicBool::new(false);
    let fragment = compile_inherited_fragment(SOURCE, &env.work, &toolchain, &cancelled)?;
    let view = fragment.view()?;
    let read_owner = entity_ordinal_at_index(&view, b"read", EntityKind::Function, 0)?;
    let decoy_note = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 0)?;
    let child_note = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 1)?;
    let calls = read_method_calls(&view, read_owner)?;
    if calls.len() != 1 {
        return Err(TestError::Falsified("read owns one MethodCall"));
    }
    assert_local_index(calls[0], decoy_note, "obj.note() resolves to Decoy.note")?;
    if matches!(
        &calls[0].occurrence.target,
        OccurrenceTarget::Local(target) if target.raw == child_note
    ) {
        return Err(TestError::Falsified("obj.note() must not resolve to Child.note"));
    }
    fs::remove_dir_all(&env.work).map_err(|source| TestError::Io("remove scratch", source))?;
    Ok(())
}

#[test]
fn py_instance_attribute_call_targets_child_note_not_decoy() -> Result<(), TestError> {
    const SOURCE: &[u8] = b"\
class Decoy:
    def note(self):
        return 0

class Child:
    def note(self):
        return 1

class Holder:
    child: Child
    def run(self):
        return self.child.note()
";
    let env = inherited_fixture_env()?;
    let toolchain = env.toolchain()?;
    let cancelled = AtomicBool::new(false);
    let fragment = compile_inherited_fragment(SOURCE, &env.work, &toolchain, &cancelled)?;
    let view = fragment.view()?;
    let run_owner = entity_ordinal_at_index(&view, b"run", EntityKind::Function, 0)?;
    let decoy_note = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 0)?;
    let child_note = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 1)?;
    let calls = read_method_calls(&view, run_owner)?;
    if calls.len() != 1 {
        return Err(TestError::Falsified("run owns one MethodCall"));
    }
    assert_local_index(calls[0], child_note, "self.child.note() resolves to Child.note")?;
    if matches!(
        &calls[0].occurrence.target,
        OccurrenceTarget::Local(target) if target.raw == decoy_note
    ) {
        return Err(TestError::Falsified("self.child.note() must not resolve to Decoy.note"));
    }
    fs::remove_dir_all(&env.work).map_err(|source| TestError::Io("remove scratch", source))?;
    Ok(())
}

#[test]
fn py_instance_attribute_field_targets_child_score_not_decoy() -> Result<(), TestError> {
    const SOURCE: &[u8] = b"\
class Decoy:
    score = 9

class Child:
    score = 1

class Holder:
    child: Child
    def run(self):
        return self.child.score
";
    let env = inherited_fixture_env()?;
    let toolchain = env.toolchain()?;
    let cancelled = AtomicBool::new(false);
    let fragment = compile_inherited_fragment(SOURCE, &env.work, &toolchain, &cancelled)?;
    let view = fragment.view()?;
    let run_owner = entity_ordinal_at_index(&view, b"run", EntityKind::Function, 0)?;
    let decoy_score = entity_ordinal_at_index(&view, b"score", EntityKind::Field, 0)?;
    let child_score = entity_ordinal_at_index(&view, b"score", EntityKind::Field, 1)?;
    let calls = read_method_calls(&view, run_owner)?;
    if !calls.is_empty() {
        return Err(TestError::Falsified("run owns zero MethodCalls"));
    }
    let reads = field_accesses_with_path(&view, run_owner, b"score")?;
    if reads.len() != 1 {
        return Err(TestError::Falsified("run owns one FieldAccess with path score"));
    }
    assert_local_index(reads[0], child_score, "self.child.score resolves to Child.score")?;
    if matches!(
        &reads[0].occurrence.target,
        OccurrenceTarget::Local(target) if target.raw == decoy_score
    ) {
        return Err(TestError::Falsified("self.child.score must not resolve to Decoy.score"));
    }
    fs::remove_dir_all(&env.work).map_err(|source| TestError::Io("remove scratch", source))?;
    Ok(())
}

#[test]
fn py_instance_attribute_value_targets_child_note_not_decoy() -> Result<(), TestError> {
    const SOURCE: &[u8] = b"\
class Decoy:
    def note(self):
        return 0

class Child:
    def note(self):
        return 1

class Holder:
    child: Child
    def run(self):
        return self.child.note
";
    let env = inherited_fixture_env()?;
    let toolchain = env.toolchain()?;
    let cancelled = AtomicBool::new(false);
    let fragment = compile_inherited_fragment(SOURCE, &env.work, &toolchain, &cancelled)?;
    let view = fragment.view()?;
    let run_owner = entity_ordinal_at_index(&view, b"run", EntityKind::Function, 0)?;
    let decoy_note = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 0)?;
    let child_note = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 1)?;
    let calls = read_method_calls(&view, run_owner)?;
    if !calls.is_empty() {
        return Err(TestError::Falsified("run owns zero MethodCalls"));
    }
    let reads = field_accesses_with_path(&view, run_owner, b"note")?;
    if reads.len() != 1 {
        return Err(TestError::Falsified("run owns one FieldAccess with path note"));
    }
    assert_local_index(reads[0], child_note, "self.child.note resolves to Child.note")?;
    if matches!(
        &reads[0].occurrence.target,
        OccurrenceTarget::Local(target) if target.raw == decoy_note
    ) {
        return Err(TestError::Falsified("self.child.note must not resolve to Decoy.note"));
    }
    fs::remove_dir_all(&env.work).map_err(|source| TestError::Io("remove scratch", source))?;
    Ok(())
}

#[test]
fn py_instance_attribute_inherited_field_targets_base_note_not_decoy() -> Result<(), TestError> {
    const SOURCE: &[u8] = b"\
class Decoy:
    def note(self):
        return 0

class Child:
    def note(self):
        return 1

class Base:
    child: Child

class Holder(Base):
    def run(self):
        return self.child.note()
";
    let env = inherited_fixture_env()?;
    let toolchain = env.toolchain()?;
    let cancelled = AtomicBool::new(false);
    let fragment = compile_inherited_fragment(SOURCE, &env.work, &toolchain, &cancelled)?;
    let view = fragment.view()?;
    let run_owner = entity_ordinal_at_index(&view, b"run", EntityKind::Function, 0)?;
    let decoy_note = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 0)?;
    let child_note = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 1)?;
    let calls = read_method_calls(&view, run_owner)?;
    if calls.len() != 1 {
        return Err(TestError::Falsified("run owns one MethodCall"));
    }
    assert_local_index(calls[0], child_note, "self.child.note() resolves to Child.note")?;
    if matches!(
        &calls[0].occurrence.target,
        OccurrenceTarget::Local(target) if target.raw == decoy_note
    ) {
        return Err(TestError::Falsified("self.child.note() must not resolve to Decoy.note"));
    }
    fs::remove_dir_all(&env.work).map_err(|source| TestError::Io("remove scratch", source))?;
    Ok(())
}

#[test]
fn py_instance_attribute_inherited_member_targets_base_note_not_decoy() -> Result<(), TestError> {
    const SOURCE: &[u8] = b"\
class Decoy:
    def note(self):
        return 0

class Base:
    def note(self):
        return 1

class Child(Base):
    pass

class Holder:
    child: Child
    def run(self):
        return self.child.note()
";
    let env = inherited_fixture_env()?;
    let toolchain = env.toolchain()?;
    let cancelled = AtomicBool::new(false);
    let fragment = compile_inherited_fragment(SOURCE, &env.work, &toolchain, &cancelled)?;
    let view = fragment.view()?;
    let run_owner = entity_ordinal_at_index(&view, b"run", EntityKind::Function, 0)?;
    let decoy_note = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 0)?;
    let base_note = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 1)?;
    let calls = read_method_calls(&view, run_owner)?;
    if calls.len() != 1 {
        return Err(TestError::Falsified("run owns one MethodCall"));
    }
    assert_local_index(calls[0], base_note, "self.child.note() resolves to Base.note")?;
    if matches!(
        &calls[0].occurrence.target,
        OccurrenceTarget::Local(target) if target.raw == decoy_note
    ) {
        return Err(TestError::Falsified("self.child.note() must not resolve to Decoy.note"));
    }
    fs::remove_dir_all(&env.work).map_err(|source| TestError::Io("remove scratch", source))?;
    Ok(())
}

#[test]
fn py_instance_attribute_generic_call_targets_child_note_not_decoy() -> Result<(), TestError> {
    const SOURCE: &[u8] = b"\
class Decoy:
    def note(self):
        return 0

class Child:
    def note(self):
        return 1

class Holder:
    child: Child[int]
    def run(self):
        return self.child.note()
";
    let env = inherited_fixture_env()?;
    let toolchain = env.toolchain()?;
    let cancelled = AtomicBool::new(false);
    let fragment = compile_inherited_fragment(SOURCE, &env.work, &toolchain, &cancelled)?;
    let view = fragment.view()?;
    let run_owner = entity_ordinal_at_index(&view, b"run", EntityKind::Function, 0)?;
    let decoy_note = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 0)?;
    let child_note = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 1)?;
    let calls = read_method_calls(&view, run_owner)?;
    if calls.len() != 1 {
        return Err(TestError::Falsified("run owns one MethodCall"));
    }
    assert_local_index(calls[0], child_note, "self.child.note() resolves to Child.note")?;
    if matches!(
        &calls[0].occurrence.target,
        OccurrenceTarget::Local(target) if target.raw == decoy_note
    ) {
        return Err(TestError::Falsified("self.child.note() must not resolve to Decoy.note"));
    }
    fs::remove_dir_all(&env.work).map_err(|source| TestError::Io("remove scratch", source))?;
    Ok(())
}

#[test]
fn py_instance_attribute_union_call_targets_child_note_not_decoy() -> Result<(), TestError> {
    const SOURCE: &[u8] = b"\
class Decoy:
    def note(self):
        return 0

class Child:
    def note(self):
        return 1

class Holder:
    child: Child | None
    def run(self):
        return self.child.note()
";
    let env = inherited_fixture_env()?;
    let toolchain = env.toolchain()?;
    let cancelled = AtomicBool::new(false);
    let fragment = compile_inherited_fragment(SOURCE, &env.work, &toolchain, &cancelled)?;
    let view = fragment.view()?;
    let run_owner = entity_ordinal_at_index(&view, b"run", EntityKind::Function, 0)?;
    let decoy_note = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 0)?;
    let child_note = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 1)?;
    let calls = read_method_calls(&view, run_owner)?;
    if calls.len() != 1 {
        return Err(TestError::Falsified("run owns one MethodCall"));
    }
    assert_local_index(calls[0], child_note, "self.child.note() resolves to Child.note")?;
    if matches!(
        &calls[0].occurrence.target,
        OccurrenceTarget::Local(target) if target.raw == decoy_note
    ) {
        return Err(TestError::Falsified("self.child.note() must not resolve to Decoy.note"));
    }
    fs::remove_dir_all(&env.work).map_err(|source| TestError::Io("remove scratch", source))?;
    Ok(())
}

#[test]
fn py_instance_attribute_quoted_call_targets_child_note_not_decoy() -> Result<(), TestError> {
    const SOURCE: &[u8] = b"\
class Decoy:
    def note(self):
        return 0

class Child:
    def note(self):
        return 1

class Holder:
    child: \"Child\"
    def run(self):
        return self.child.note()
";
    let env = inherited_fixture_env()?;
    let toolchain = env.toolchain()?;
    let cancelled = AtomicBool::new(false);
    let fragment = compile_inherited_fragment(SOURCE, &env.work, &toolchain, &cancelled)?;
    let view = fragment.view()?;
    let run_owner = entity_ordinal_at_index(&view, b"run", EntityKind::Function, 0)?;
    let decoy_note = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 0)?;
    let child_note = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 1)?;
    let calls = read_method_calls(&view, run_owner)?;
    if calls.len() != 1 {
        return Err(TestError::Falsified("run owns one MethodCall"));
    }
    assert_local_index(calls[0], child_note, "self.child.note() resolves to Child.note")?;
    if matches!(
        &calls[0].occurrence.target,
        OccurrenceTarget::Local(target) if target.raw == decoy_note
    ) {
        return Err(TestError::Falsified("self.child.note() must not resolve to Decoy.note"));
    }
    fs::remove_dir_all(&env.work).map_err(|source| TestError::Io("remove scratch", source))?;
    Ok(())
}

#[test]
fn py_instance_attribute_classmethod_call_targets_child_note_not_decoy() -> Result<(), TestError> {
    const SOURCE: &[u8] = b"\
class Decoy:
    def note(self):
        return 0

class Child:
    def note(self):
        return 1

class Holder:
    child: Child
    @classmethod
    def run(cls):
        return cls.child.note()
";
    let env = inherited_fixture_env()?;
    let toolchain = env.toolchain()?;
    let cancelled = AtomicBool::new(false);
    let fragment = compile_inherited_fragment(SOURCE, &env.work, &toolchain, &cancelled)?;
    let view = fragment.view()?;
    let run_owner = entity_ordinal_at_index(&view, b"run", EntityKind::Function, 0)?;
    let decoy_note = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 0)?;
    let child_note = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 1)?;
    let calls = read_method_calls(&view, run_owner)?;
    if calls.len() != 1 {
        return Err(TestError::Falsified("run owns one MethodCall"));
    }
    assert_local_index(calls[0], child_note, "cls.child.note() resolves to Child.note")?;
    if matches!(
        &calls[0].occurrence.target,
        OccurrenceTarget::Local(target) if target.raw == decoy_note
    ) {
        return Err(TestError::Falsified("cls.child.note() must not resolve to Decoy.note"));
    }
    fs::remove_dir_all(&env.work).map_err(|source| TestError::Io("remove scratch", source))?;
    Ok(())
}

#[test]
fn py_instance_attribute_unannotated_stays_universe() -> Result<(), TestError> {
    const SOURCE: &[u8] = b"\
class Decoy:
    def note(self):
        return 0

class Child:
    def note(self):
        return 1

class Holder:
    child = 1
    def run(self):
        return self.child.note()
";
    let env = inherited_fixture_env()?;
    let toolchain = env.toolchain()?;
    let cancelled = AtomicBool::new(false);
    let fragment = compile_inherited_fragment(SOURCE, &env.work, &toolchain, &cancelled)?;
    let view = fragment.view()?;
    let run_owner = entity_ordinal_at_index(&view, b"run", EntityKind::Function, 0)?;
    let child_note = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 1)?;
    let calls = read_method_calls(&view, run_owner)?;
    if calls.len() != 1 {
        return Err(TestError::Falsified("run owns one MethodCall"));
    }
    assert_universe_method(calls[0], "unannotated child stays a pypi universe key")?;
    if matches!(
        &calls[0].occurrence.target,
        OccurrenceTarget::Local(target) if target.raw == child_note
    ) {
        return Err(TestError::Falsified("self.child.note() must not resolve to Child.note"));
    }
    fs::remove_dir_all(&env.work).map_err(|source| TestError::Io("remove scratch", source))?;
    Ok(())
}

#[test]
fn py_instance_attribute_own_field_shadows_base_stays_universe() -> Result<(), TestError> {
    const SOURCE: &[u8] = b"\
class Decoy:
    def note(self):
        return 0

class Child:
    def note(self):
        return 1

class Base:
    child: Child

class Holder(Base):
    child = 1
    def run(self):
        return self.child.note()
";
    let env = inherited_fixture_env()?;
    let toolchain = env.toolchain()?;
    let cancelled = AtomicBool::new(false);
    let fragment = compile_inherited_fragment(SOURCE, &env.work, &toolchain, &cancelled)?;
    let view = fragment.view()?;
    let run_owner = entity_ordinal_at_index(&view, b"run", EntityKind::Function, 0)?;
    let child_note = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 1)?;
    let calls = read_method_calls(&view, run_owner)?;
    if calls.len() != 1 {
        return Err(TestError::Falsified("run owns one MethodCall"));
    }
    assert_universe_method(calls[0], "own unannotated child shadows base annotation")?;
    if matches!(
        &calls[0].occurrence.target,
        OccurrenceTarget::Local(target) if target.raw == child_note
    ) {
        return Err(TestError::Falsified("self.child.note() must not resolve to Child.note"));
    }
    fs::remove_dir_all(&env.work).map_err(|source| TestError::Io("remove scratch", source))?;
    Ok(())
}

#[test]
fn py_instance_attribute_two_bases_stay_universe() -> Result<(), TestError> {
    const SOURCE: &[u8] = b"\
class Decoy:
    def note(self):
        return 0

class Child:
    def note(self):
        return 1

class Left:
    child: Child

class Right:
    child: Decoy

class Holder(Left, Right):
    def run(self):
        return self.child.note()
";
    let env = inherited_fixture_env()?;
    let toolchain = env.toolchain()?;
    let cancelled = AtomicBool::new(false);
    let fragment = compile_inherited_fragment(SOURCE, &env.work, &toolchain, &cancelled)?;
    let view = fragment.view()?;
    let run_owner = entity_ordinal_at_index(&view, b"run", EntityKind::Function, 0)?;
    let decoy_note = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 0)?;
    let child_note = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 1)?;
    let calls = read_method_calls(&view, run_owner)?;
    if calls.len() != 1 {
        return Err(TestError::Falsified("run owns one MethodCall"));
    }
    assert_universe_method(calls[0], "two annotated bases stay a pypi universe key")?;
    if matches!(
        &calls[0].occurrence.target,
        OccurrenceTarget::Local(target) if target.raw == decoy_note
    ) {
        return Err(TestError::Falsified("self.child.note() must not resolve to Decoy.note"));
    }
    if matches!(
        &calls[0].occurrence.target,
        OccurrenceTarget::Local(target) if target.raw == child_note
    ) {
        return Err(TestError::Falsified("self.child.note() must not resolve to Child.note"));
    }
    fs::remove_dir_all(&env.work).map_err(|source| TestError::Io("remove scratch", source))?;
    Ok(())
}

#[test]
fn py_instance_attribute_list_read_does_not_take_module_field() -> Result<(), TestError> {
    const SOURCE: &[u8] = b"\
class Child:
    def note(self):
        return 1

class Holder:
    note = 1
    child: list[Child]
    def run(self):
        return self.child.note
";
    let env = inherited_fixture_env()?;
    let toolchain = env.toolchain()?;
    let cancelled = AtomicBool::new(false);
    let fragment = compile_inherited_fragment(SOURCE, &env.work, &toolchain, &cancelled)?;
    let view = fragment.view()?;
    let run_owner = entity_ordinal_at_index(&view, b"run", EntityKind::Function, 0)?;
    let holder_note = entity_ordinal_at_index(&view, b"note", EntityKind::Field, 0)?;
    let child_note = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 0)?;
    let calls = read_method_calls(&view, run_owner)?;
    if !calls.is_empty() {
        return Err(TestError::Falsified("run owns zero MethodCalls"));
    }
    let reads = field_accesses_with_path(&view, run_owner, b"note")?;
    if reads.len() != 1 {
        return Err(TestError::Falsified("run owns one FieldAccess with path note"));
    }
    assert_universe_field(reads[0], "list[Child] read stays a pypi universe field key")?;
    if reads[0].occurrence.confidence != OccurrenceConfidence::Index {
        return Err(TestError::Falsified("list[Child] universe field confidence is Index"));
    }
    if matches!(
        &reads[0].occurrence.target,
        OccurrenceTarget::Local(target) if target.raw == holder_note
    ) {
        return Err(TestError::Falsified("self.child.note must not resolve to Holder.note"));
    }
    if matches!(
        &reads[0].occurrence.target,
        OccurrenceTarget::Local(target) if target.raw == child_note
    ) {
        return Err(TestError::Falsified("self.child.note must not resolve to Child.note"));
    }
    fs::remove_dir_all(&env.work).map_err(|source| TestError::Io("remove scratch", source))?;
    Ok(())
}

#[test]
fn py_instance_attribute_dotted_stays_universe() -> Result<(), TestError> {
    const SOURCE: &[u8] = b"\
import pkg

class Decoy:
    def note(self):
        return 0

class Child:
    def note(self):
        return 1

class Holder:
    child: pkg.Child
    def run(self):
        return self.child.note()
";
    let env = inherited_fixture_env()?;
    let toolchain = env.toolchain()?;
    let cancelled = AtomicBool::new(false);
    let fragment = compile_inherited_fragment(SOURCE, &env.work, &toolchain, &cancelled)?;
    let view = fragment.view()?;
    let run_owner = entity_ordinal_at_index(&view, b"run", EntityKind::Function, 0)?;
    let child_note = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 1)?;
    let calls = read_method_calls(&view, run_owner)?;
    if calls.len() != 1 {
        return Err(TestError::Falsified("run owns one MethodCall"));
    }
    assert_universe_method(calls[0], "dotted field annotation stays a pypi universe key")?;
    if matches!(
        &calls[0].occurrence.target,
        OccurrenceTarget::Local(target) if target.raw == child_note
    ) {
        return Err(TestError::Falsified("self.child.note() must not resolve to Child.note"));
    }
    fs::remove_dir_all(&env.work).map_err(|source| TestError::Io("remove scratch", source))?;
    Ok(())
}

#[test]
fn py_instance_attribute_missing_member_stays_attribute_key() -> Result<(), TestError> {
    const SOURCE: &[u8] = b"\
class Decoy:
    def note(self):
        return 0

class Child:
    pass

class Holder:
    child: Child
    def run(self):
        return self.child.note()
";
    let env = inherited_fixture_env()?;
    let toolchain = env.toolchain()?;
    let cancelled = AtomicBool::new(false);
    let fragment = compile_inherited_fragment(SOURCE, &env.work, &toolchain, &cancelled)?;
    let view = fragment.view()?;
    let run_owner = entity_ordinal_at_index(&view, b"run", EntityKind::Function, 0)?;
    let decoy_note = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 0)?;
    let calls = read_method_calls(&view, run_owner)?;
    if calls.len() != 1 {
        return Err(TestError::Falsified("run owns one MethodCall"));
    }
    assert_universe_method(calls[0], "missing member stays a pypi universe key")?;
    if matches!(
        &calls[0].occurrence.target,
        OccurrenceTarget::Local(target) if target.raw == decoy_note
    ) {
        return Err(TestError::Falsified("self.child.note() must not resolve to Decoy.note"));
    }
    fs::remove_dir_all(&env.work).map_err(|source| TestError::Io("remove scratch", source))?;
    Ok(())
}

#[test]
fn py_instance_attribute_field_wins_over_method() -> Result<(), TestError> {
    const SOURCE: &[u8] = b"\
class Child:
    score = 1

    def score(self):
        return self.score

class Holder:
    child: Child
    def run(self):
        return self.child.score
";
    let env = inherited_fixture_env()?;
    let toolchain = env.toolchain()?;
    let cancelled = AtomicBool::new(false);
    let fragment = compile_inherited_fragment(SOURCE, &env.work, &toolchain, &cancelled)?;
    let view = fragment.view()?;
    let run_owner = entity_ordinal_at_index(&view, b"run", EntityKind::Function, 0)?;
    let score_field = entity_ordinal_at_index(&view, b"score", EntityKind::Field, 0)?;
    let score_method = entity_ordinal_at_index(&view, b"score", EntityKind::Function, 0)?;
    let calls = read_method_calls(&view, run_owner)?;
    if !calls.is_empty() {
        return Err(TestError::Falsified("run owns zero MethodCalls"));
    }
    let reads = field_accesses_with_path(&view, run_owner, b"score")?;
    if reads.len() != 1 {
        return Err(TestError::Falsified("run owns one FieldAccess with path score"));
    }
    assert_local_index(reads[0], score_field, "self.child.score resolves to Child.score field")?;
    if matches!(
        &reads[0].occurrence.target,
        OccurrenceTarget::Local(target) if target.raw == score_method
    ) {
        return Err(TestError::Falsified("self.child.score must not resolve to Child.score method"));
    }
    fs::remove_dir_all(&env.work).map_err(|source| TestError::Io("remove scratch", source))?;
    Ok(())
}

#[test]
fn py_instance_attribute_call_binds_method_not_field() -> Result<(), TestError> {
    const SOURCE: &[u8] = b"\
class Child:
    note = 1

    def note(self):
        return self.note

class Holder:
    child: Child
    def run(self):
        return self.child.note()
";
    let env = inherited_fixture_env()?;
    let toolchain = env.toolchain()?;
    let cancelled = AtomicBool::new(false);
    let fragment = compile_inherited_fragment(SOURCE, &env.work, &toolchain, &cancelled)?;
    let view = fragment.view()?;
    let run_owner = entity_ordinal_at_index(&view, b"run", EntityKind::Function, 0)?;
    let note_field = entity_ordinal_at_index(&view, b"note", EntityKind::Field, 0)?;
    let note_method = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 0)?;
    let calls = read_method_calls(&view, run_owner)?;
    if calls.len() != 1 {
        return Err(TestError::Falsified("run owns one MethodCall"));
    }
    assert_local_index(calls[0], note_method, "self.child.note() resolves to Child.note method")?;
    if matches!(
        &calls[0].occurrence.target,
        OccurrenceTarget::Local(target) if target.raw == note_field
    ) {
        return Err(TestError::Falsified("self.child.note() must not resolve to Child.note field"));
    }
    fs::remove_dir_all(&env.work).map_err(|source| TestError::Io("remove scratch", source))?;
    Ok(())
}

#[test]
fn py_instance_attribute_nested_function_stays_universe() -> Result<(), TestError> {
    const SOURCE: &[u8] = b"\
class Decoy:
    def note(self):
        return 0

class Child:
    def note(self):
        return 1

class Holder:
    child: Child
    def run(self):
        def inner():
            return self.child.note()
        return inner()
";
    let env = inherited_fixture_env()?;
    let toolchain = env.toolchain()?;
    let cancelled = AtomicBool::new(false);
    let fragment = compile_inherited_fragment(SOURCE, &env.work, &toolchain, &cancelled)?;
    let view = fragment.view()?;
    let inner_owner = entity_ordinal_at_index(&view, b"inner", EntityKind::Function, 0)?;
    let child_note = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 1)?;
    let calls = read_method_calls(&view, inner_owner)?;
    if calls.len() != 1 {
        return Err(TestError::Falsified("inner owns one MethodCall"));
    }
    assert_universe_method(calls[0], "nested function stays a pypi universe key")?;
    if matches!(
        &calls[0].occurrence.target,
        OccurrenceTarget::Local(target) if target.raw == child_note
    ) {
        return Err(TestError::Falsified("self.child.note() must not resolve to Child.note"));
    }
    fs::remove_dir_all(&env.work).map_err(|source| TestError::Io("remove scratch", source))?;
    Ok(())
}

#[test]
fn py_instance_attribute_deeper_chain_stays_universe() -> Result<(), TestError> {
    const SOURCE: &[u8] = b"\
class Decoy:
    def note(self):
        return 0

class Child:
    def note(self):
        return 1
    other: Child

class Holder:
    child: Child
    def run(self):
        return self.child.other.note()
";
    let env = inherited_fixture_env()?;
    let toolchain = env.toolchain()?;
    let cancelled = AtomicBool::new(false);
    let fragment = compile_inherited_fragment(SOURCE, &env.work, &toolchain, &cancelled)?;
    let view = fragment.view()?;
    let run_owner = entity_ordinal_at_index(&view, b"run", EntityKind::Function, 0)?;
    let decoy_note = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 0)?;
    let child_note = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 1)?;
    let holder_child = entity_ordinal_at_index(&view, b"child", EntityKind::Field, 0)?;
    let child_other = entity_ordinal_at_index(&view, b"other", EntityKind::Field, 0)?;
    let calls = read_method_calls(&view, run_owner)?;
    if calls.len() != 1 {
        return Err(TestError::Falsified("run owns one MethodCall"));
    }
    assert_local_index(calls[0], child_note, "self.child.other.note() resolves to Child.note")?;
    if matches!(
        &calls[0].occurrence.target,
        OccurrenceTarget::Local(target) if target.raw == decoy_note
    ) {
        return Err(TestError::Falsified("self.child.other.note() must not resolve to Decoy.note"));
    }
    let child_reads = field_accesses_with_path(&view, run_owner, b"child")?;
    if child_reads.len() != 1 {
        return Err(TestError::Falsified("run owns one FieldAccess with path child"));
    }
    assert_local_index(
        child_reads[0],
        holder_child,
        "self.child resolves to Holder.child",
    )?;
    let other_reads = field_accesses_with_path(&view, run_owner, b"other")?;
    if other_reads.len() != 1 {
        return Err(TestError::Falsified("run owns one FieldAccess with path other"));
    }
    assert_local_index(
        other_reads[0],
        child_other,
        "self.child.other resolves to Child.other",
    )?;
    fs::remove_dir_all(&env.work).map_err(|source| TestError::Io("remove scratch", source))?;
    Ok(())
}

#[test]
fn py_instance_attribute_prefix_field_still_targets_holder_child() -> Result<(), TestError> {
    const SOURCE: &[u8] = b"\
class Decoy:
    def note(self):
        return 0

class Child:
    def note(self):
        return 1

class Holder:
    child: Child
    def run(self):
        return self.child.note()
";
    let env = inherited_fixture_env()?;
    let toolchain = env.toolchain()?;
    let cancelled = AtomicBool::new(false);
    let fragment = compile_inherited_fragment(SOURCE, &env.work, &toolchain, &cancelled)?;
    let view = fragment.view()?;
    let run_owner = entity_ordinal_at_index(&view, b"run", EntityKind::Function, 0)?;
    let decoy_note = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 0)?;
    let child_note = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 1)?;
    let holder_child = entity_ordinal_at_index(&view, b"child", EntityKind::Field, 0)?;
    let calls = read_method_calls(&view, run_owner)?;
    if calls.len() != 1 {
        return Err(TestError::Falsified("run owns one MethodCall"));
    }
    assert_local_index(calls[0], child_note, "self.child.note() resolves to Child.note")?;
    if matches!(
        &calls[0].occurrence.target,
        OccurrenceTarget::Local(target) if target.raw == decoy_note
    ) {
        return Err(TestError::Falsified("self.child.note() must not resolve to Decoy.note"));
    }
    let child_reads = field_accesses_with_path(&view, run_owner, b"child")?;
    if child_reads.len() != 1 {
        return Err(TestError::Falsified("run owns one FieldAccess with path child"));
    }
    assert_local_index(
        child_reads[0],
        holder_child,
        "self.child resolves to Holder.child",
    )?;
    fs::remove_dir_all(&env.work).map_err(|source| TestError::Io("remove scratch", source))?;
    Ok(())
}

#[test]
fn py_instance_attribute_annotated_name_prefix_still_targets_holder_child() -> Result<(), TestError> {
    const SOURCE: &[u8] = b"\
class Decoy:
    def note(self):
        return 0

class Child:
    def note(self):
        return 1

class Holder:
    child: Child

def run(obj: Holder):
    return obj.child.note()
";
    let env = inherited_fixture_env()?;
    let toolchain = env.toolchain()?;
    let cancelled = AtomicBool::new(false);
    let fragment = compile_inherited_fragment(SOURCE, &env.work, &toolchain, &cancelled)?;
    let view = fragment.view()?;
    let run_owner = entity_ordinal_at_index(&view, b"run", EntityKind::Function, 0)?;
    let decoy_note = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 0)?;
    let child_note = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 1)?;
    let holder_child = entity_ordinal_at_index(&view, b"child", EntityKind::Field, 0)?;
    let child_reads = field_accesses_with_path(&view, run_owner, b"child")?;
    if child_reads.len() != 1 {
        return Err(TestError::Falsified("run owns one FieldAccess with path child"));
    }
    assert_local_index(
        child_reads[0],
        holder_child,
        "obj.child resolves to Holder.child",
    )?;
    let calls = read_method_calls(&view, run_owner)?;
    if calls.len() != 1 {
        return Err(TestError::Falsified("run owns one MethodCall"));
    }
    assert_local_index(calls[0], child_note, "obj.child.note() resolves to Child.note")?;
    if matches!(
        &calls[0].occurrence.target,
        OccurrenceTarget::Local(target) if target.raw == decoy_note
    ) {
        return Err(TestError::Falsified("obj.child.note() must not resolve to Decoy.note"));
    }
    fs::remove_dir_all(&env.work).map_err(|source| TestError::Io("remove scratch", source))?;
    Ok(())
}

#[test]
fn py_named_attribute_parameter_call_targets_child_note_not_decoy() -> Result<(), TestError> {
    const SOURCE: &[u8] = b"\
class Decoy:
    def note(self):
        return 0

class Child:
    def note(self):
        return 1

class Holder:
    child: Child

def run(obj: Holder):
    return obj.child.note()
";
    let env = inherited_fixture_env()?;
    let toolchain = env.toolchain()?;
    let cancelled = AtomicBool::new(false);
    let fragment = compile_inherited_fragment(SOURCE, &env.work, &toolchain, &cancelled)?;
    let view = fragment.view()?;
    let run_owner = entity_ordinal_at_index(&view, b"run", EntityKind::Function, 0)?;
    let decoy_note = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 0)?;
    let child_note = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 1)?;
    let holder_child = entity_ordinal_at_index(&view, b"child", EntityKind::Field, 0)?;
    let calls = read_method_calls(&view, run_owner)?;
    if calls.len() != 1 {
        return Err(TestError::Falsified("run owns one MethodCall"));
    }
    assert_local_index(calls[0], child_note, "obj.child.note() resolves to Child.note")?;
    if matches!(
        &calls[0].occurrence.target,
        OccurrenceTarget::Local(target) if target.raw == decoy_note
    ) {
        return Err(TestError::Falsified("obj.child.note() must not resolve to Decoy.note"));
    }
    let child_reads = field_accesses_with_path(&view, run_owner, b"child")?;
    if child_reads.len() != 1 {
        return Err(TestError::Falsified("run owns one FieldAccess with path child"));
    }
    assert_local_index(
        child_reads[0],
        holder_child,
        "obj.child resolves to Holder.child",
    )?;
    fs::remove_dir_all(&env.work).map_err(|source| TestError::Io("remove scratch", source))?;
    Ok(())
}

#[test]
fn py_named_attribute_local_call_targets_child_note_not_decoy() -> Result<(), TestError> {
    const SOURCE: &[u8] = b"\
class Decoy:
    def note(self):
        return 0

class Child:
    def note(self):
        return 1

class Holder:
    child: Child

def run():
    obj: Holder = Holder()
    return obj.child.note()
";
    let env = inherited_fixture_env()?;
    let toolchain = env.toolchain()?;
    let cancelled = AtomicBool::new(false);
    let fragment = compile_inherited_fragment(SOURCE, &env.work, &toolchain, &cancelled)?;
    let view = fragment.view()?;
    let run_owner = entity_ordinal_at_index(&view, b"run", EntityKind::Function, 0)?;
    let decoy_note = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 0)?;
    let child_note = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 1)?;
    let calls = read_method_calls(&view, run_owner)?;
    if calls.len() != 1 {
        return Err(TestError::Falsified("run owns one MethodCall"));
    }
    assert_local_index(calls[0], child_note, "obj.child.note() resolves to Child.note")?;
    if matches!(
        &calls[0].occurrence.target,
        OccurrenceTarget::Local(target) if target.raw == decoy_note
    ) {
        return Err(TestError::Falsified("obj.child.note() must not resolve to Decoy.note"));
    }
    fs::remove_dir_all(&env.work).map_err(|source| TestError::Io("remove scratch", source))?;
    Ok(())
}

#[test]
fn py_named_attribute_module_call_targets_child_note_not_decoy() -> Result<(), TestError> {
    const SOURCE: &[u8] = b"\
class Decoy:
    def note(self):
        return 0

class Child:
    def note(self):
        return 1

class Holder:
    child: Child

obj: Holder = Holder()

def run():
    return obj.child.note()
";
    let env = inherited_fixture_env()?;
    let toolchain = env.toolchain()?;
    let cancelled = AtomicBool::new(false);
    let fragment = compile_inherited_fragment(SOURCE, &env.work, &toolchain, &cancelled)?;
    let view = fragment.view()?;
    let run_owner = entity_ordinal_at_index(&view, b"run", EntityKind::Function, 0)?;
    let decoy_note = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 0)?;
    let child_note = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 1)?;
    let calls = read_method_calls(&view, run_owner)?;
    if calls.len() != 1 {
        return Err(TestError::Falsified("run owns one MethodCall"));
    }
    assert_local_index(calls[0], child_note, "obj.child.note() resolves to Child.note")?;
    if matches!(
        &calls[0].occurrence.target,
        OccurrenceTarget::Local(target) if target.raw == decoy_note
    ) {
        return Err(TestError::Falsified("obj.child.note() must not resolve to Decoy.note"));
    }
    fs::remove_dir_all(&env.work).map_err(|source| TestError::Io("remove scratch", source))?;
    Ok(())
}

#[test]
fn py_named_attribute_closure_call_targets_child_note_not_decoy() -> Result<(), TestError> {
    const SOURCE: &[u8] = b"\
class Decoy:
    def note(self):
        return 0

class Child:
    def note(self):
        return 1

class Holder:
    child: Child

def outer(obj: Holder):
    def inner():
        return obj.child.note()
";
    let env = inherited_fixture_env()?;
    let toolchain = env.toolchain()?;
    let cancelled = AtomicBool::new(false);
    let fragment = compile_inherited_fragment(SOURCE, &env.work, &toolchain, &cancelled)?;
    let view = fragment.view()?;
    let inner_owner = entity_ordinal_at_index(&view, b"inner", EntityKind::Function, 0)?;
    let decoy_note = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 0)?;
    let child_note = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 1)?;
    let calls = read_method_calls(&view, inner_owner)?;
    if calls.len() != 1 {
        return Err(TestError::Falsified("inner owns one MethodCall"));
    }
    assert_local_index(calls[0], child_note, "obj.child.note() resolves to Child.note")?;
    if matches!(
        &calls[0].occurrence.target,
        OccurrenceTarget::Local(target) if target.raw == decoy_note
    ) {
        return Err(TestError::Falsified("obj.child.note() must not resolve to Decoy.note"));
    }
    fs::remove_dir_all(&env.work).map_err(|source| TestError::Io("remove scratch", source))?;
    Ok(())
}

#[test]
fn py_named_attribute_field_targets_child_score_not_decoy() -> Result<(), TestError> {
    const SOURCE: &[u8] = b"\
class Decoy:
    score = 9

class Child:
    score = 1

class Holder:
    child: Child

def run(obj: Holder):
    return obj.child.score
";
    let env = inherited_fixture_env()?;
    let toolchain = env.toolchain()?;
    let cancelled = AtomicBool::new(false);
    let fragment = compile_inherited_fragment(SOURCE, &env.work, &toolchain, &cancelled)?;
    let view = fragment.view()?;
    let run_owner = entity_ordinal_at_index(&view, b"run", EntityKind::Function, 0)?;
    let decoy_score = entity_ordinal_at_index(&view, b"score", EntityKind::Field, 0)?;
    let child_score = entity_ordinal_at_index(&view, b"score", EntityKind::Field, 1)?;
    let calls = read_method_calls(&view, run_owner)?;
    if !calls.is_empty() {
        return Err(TestError::Falsified("run owns zero MethodCalls"));
    }
    let reads = field_accesses_with_path(&view, run_owner, b"score")?;
    if reads.len() != 1 {
        return Err(TestError::Falsified("run owns one FieldAccess with path score"));
    }
    assert_local_index(reads[0], child_score, "obj.child.score resolves to Child.score")?;
    if matches!(
        &reads[0].occurrence.target,
        OccurrenceTarget::Local(target) if target.raw == decoy_score
    ) {
        return Err(TestError::Falsified("obj.child.score must not resolve to Decoy.score"));
    }
    fs::remove_dir_all(&env.work).map_err(|source| TestError::Io("remove scratch", source))?;
    Ok(())
}

#[test]
fn py_named_attribute_value_targets_child_note_not_decoy() -> Result<(), TestError> {
    const SOURCE: &[u8] = b"\
class Decoy:
    def note(self):
        return 0

class Child:
    def note(self):
        return 1

class Holder:
    child: Child

def run(obj: Holder):
    return obj.child.note
";
    let env = inherited_fixture_env()?;
    let toolchain = env.toolchain()?;
    let cancelled = AtomicBool::new(false);
    let fragment = compile_inherited_fragment(SOURCE, &env.work, &toolchain, &cancelled)?;
    let view = fragment.view()?;
    let run_owner = entity_ordinal_at_index(&view, b"run", EntityKind::Function, 0)?;
    let decoy_note = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 0)?;
    let child_note = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 1)?;
    let calls = read_method_calls(&view, run_owner)?;
    if !calls.is_empty() {
        return Err(TestError::Falsified("run owns zero MethodCalls"));
    }
    let reads = field_accesses_with_path(&view, run_owner, b"note")?;
    if reads.len() != 1 {
        return Err(TestError::Falsified("run owns one FieldAccess with path note"));
    }
    assert_local_index(reads[0], child_note, "obj.child.note resolves to Child.note")?;
    if matches!(
        &reads[0].occurrence.target,
        OccurrenceTarget::Local(target) if target.raw == decoy_note
    ) {
        return Err(TestError::Falsified("obj.child.note must not resolve to Decoy.note"));
    }
    fs::remove_dir_all(&env.work).map_err(|source| TestError::Io("remove scratch", source))?;
    Ok(())
}

#[test]
fn py_named_attribute_inherited_field_targets_child_note_not_decoy() -> Result<(), TestError> {
    const SOURCE: &[u8] = b"\
class Decoy:
    def note(self):
        return 0

class Child:
    def note(self):
        return 1

class Base:
    child: Child

class Holder(Base):
    pass

def run(obj: Holder):
    return obj.child.note()
";
    let env = inherited_fixture_env()?;
    let toolchain = env.toolchain()?;
    let cancelled = AtomicBool::new(false);
    let fragment = compile_inherited_fragment(SOURCE, &env.work, &toolchain, &cancelled)?;
    let view = fragment.view()?;
    let run_owner = entity_ordinal_at_index(&view, b"run", EntityKind::Function, 0)?;
    let decoy_note = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 0)?;
    let child_note = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 1)?;
    let calls = read_method_calls(&view, run_owner)?;
    if calls.len() != 1 {
        return Err(TestError::Falsified("run owns one MethodCall"));
    }
    assert_local_index(calls[0], child_note, "obj.child.note() resolves to Child.note")?;
    if matches!(
        &calls[0].occurrence.target,
        OccurrenceTarget::Local(target) if target.raw == decoy_note
    ) {
        return Err(TestError::Falsified("obj.child.note() must not resolve to Decoy.note"));
    }
    fs::remove_dir_all(&env.work).map_err(|source| TestError::Io("remove scratch", source))?;
    Ok(())
}

#[test]
fn py_named_attribute_inherited_member_targets_base_note_not_decoy() -> Result<(), TestError> {
    const SOURCE: &[u8] = b"\
class Decoy:
    def note(self):
        return 0

class Base:
    def note(self):
        return 1

class Child(Base):
    pass

class Holder:
    child: Child

def run(obj: Holder):
    return obj.child.note()
";
    let env = inherited_fixture_env()?;
    let toolchain = env.toolchain()?;
    let cancelled = AtomicBool::new(false);
    let fragment = compile_inherited_fragment(SOURCE, &env.work, &toolchain, &cancelled)?;
    let view = fragment.view()?;
    let run_owner = entity_ordinal_at_index(&view, b"run", EntityKind::Function, 0)?;
    let decoy_note = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 0)?;
    let base_note = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 1)?;
    let calls = read_method_calls(&view, run_owner)?;
    if calls.len() != 1 {
        return Err(TestError::Falsified("run owns one MethodCall"));
    }
    assert_local_index(calls[0], base_note, "obj.child.note() resolves to Base.note")?;
    if matches!(
        &calls[0].occurrence.target,
        OccurrenceTarget::Local(target) if target.raw == decoy_note
    ) {
        return Err(TestError::Falsified("obj.child.note() must not resolve to Decoy.note"));
    }
    fs::remove_dir_all(&env.work).map_err(|source| TestError::Io("remove scratch", source))?;
    Ok(())
}

#[test]
fn py_named_attribute_class_name_call_targets_child_note_not_decoy() -> Result<(), TestError> {
    const SOURCE: &[u8] = b"\
class Decoy:
    def note(self):
        return 0

class Child:
    def note(self):
        return 1

class Holder:
    child: Child

def run():
    return Holder.child.note()
";
    let env = inherited_fixture_env()?;
    let toolchain = env.toolchain()?;
    let cancelled = AtomicBool::new(false);
    let fragment = compile_inherited_fragment(SOURCE, &env.work, &toolchain, &cancelled)?;
    let view = fragment.view()?;
    let run_owner = entity_ordinal_at_index(&view, b"run", EntityKind::Function, 0)?;
    let decoy_note = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 0)?;
    let child_note = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 1)?;
    let calls = read_method_calls(&view, run_owner)?;
    if calls.len() != 1 {
        return Err(TestError::Falsified("run owns one MethodCall"));
    }
    assert_local_index(calls[0], child_note, "Holder.child.note() resolves to Child.note")?;
    if matches!(
        &calls[0].occurrence.target,
        OccurrenceTarget::Local(target) if target.raw == decoy_note
    ) {
        return Err(TestError::Falsified("Holder.child.note() must not resolve to Decoy.note"));
    }
    fs::remove_dir_all(&env.work).map_err(|source| TestError::Io("remove scratch", source))?;
    Ok(())
}

#[test]
fn py_named_attribute_local_shadow_stays_universe() -> Result<(), TestError> {
    const SOURCE: &[u8] = b"\
class Decoy:
    def note(self):
        return 0

class Child:
    def note(self):
        return 1

class Holder:
    child: Child

def run():
    Holder = 1
    return Holder.child.note()
";
    let env = inherited_fixture_env()?;
    let toolchain = env.toolchain()?;
    let cancelled = AtomicBool::new(false);
    let fragment = compile_inherited_fragment(SOURCE, &env.work, &toolchain, &cancelled)?;
    let view = fragment.view()?;
    let run_owner = entity_ordinal_at_index(&view, b"run", EntityKind::Function, 0)?;
    let child_note = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 1)?;
    let calls = read_method_calls(&view, run_owner)?;
    if calls.len() != 1 {
        return Err(TestError::Falsified("run owns one MethodCall"));
    }
    assert_universe_method(calls[0], "shadowed Holder stays a pypi universe key")?;
    if calls[0].occurrence.confidence != OccurrenceConfidence::Index {
        return Err(TestError::Falsified("shadowed Holder universe method confidence is Index"));
    }
    if matches!(
        &calls[0].occurrence.target,
        OccurrenceTarget::Local(target) if target.raw == child_note
    ) {
        return Err(TestError::Falsified("Holder.child.note() must not bind Child.note"));
    }
    fs::remove_dir_all(&env.work).map_err(|source| TestError::Io("remove scratch", source))?;
    Ok(())
}

#[test]
fn py_named_attribute_unannotated_field_stays_universe() -> Result<(), TestError> {
    const SOURCE: &[u8] = b"\
class Decoy:
    def note(self):
        return 0

class Child:
    def note(self):
        return 1

class Holder:
    child = 1

def run(obj: Holder):
    return obj.child.note()
";
    let env = inherited_fixture_env()?;
    let toolchain = env.toolchain()?;
    let cancelled = AtomicBool::new(false);
    let fragment = compile_inherited_fragment(SOURCE, &env.work, &toolchain, &cancelled)?;
    let view = fragment.view()?;
    let run_owner = entity_ordinal_at_index(&view, b"run", EntityKind::Function, 0)?;
    let child_note = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 1)?;
    let calls = read_method_calls(&view, run_owner)?;
    if calls.len() != 1 {
        return Err(TestError::Falsified("run owns one MethodCall"));
    }
    assert_universe_method(calls[0], "unannotated child stays a pypi universe key")?;
    if calls[0].occurrence.confidence != OccurrenceConfidence::Index {
        return Err(TestError::Falsified("unannotated child universe method confidence is Index"));
    }
    if matches!(
        &calls[0].occurrence.target,
        OccurrenceTarget::Local(target) if target.raw == child_note
    ) {
        return Err(TestError::Falsified("obj.child.note() must not bind Child.note"));
    }
    fs::remove_dir_all(&env.work).map_err(|source| TestError::Io("remove scratch", source))?;
    Ok(())
}

#[test]
fn py_named_attribute_two_bases_same_annotation_stay_universe() -> Result<(), TestError> {
    const SOURCE: &[u8] = b"\
class Decoy:
    def note(self):
        return 0

class Child:
    def note(self):
        return 1

class Left:
    child: Child

class Right:
    child: Child

class Holder(Left, Right):
    pass

def run(obj: Holder):
    return obj.child.note()
";
    let env = inherited_fixture_env()?;
    let toolchain = env.toolchain()?;
    let cancelled = AtomicBool::new(false);
    let fragment = compile_inherited_fragment(SOURCE, &env.work, &toolchain, &cancelled)?;
    let view = fragment.view()?;
    let run_owner = entity_ordinal_at_index(&view, b"run", EntityKind::Function, 0)?;
    let decoy_note = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 0)?;
    let child_note = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 1)?;
    let calls = read_method_calls(&view, run_owner)?;
    if calls.len() != 1 {
        return Err(TestError::Falsified("run owns one MethodCall"));
    }
    assert_universe_method(calls[0], "two bases with same annotation stay a pypi universe key")?;
    if calls[0].occurrence.confidence != OccurrenceConfidence::Index {
        return Err(TestError::Falsified("two bases universe method confidence is Index"));
    }
    if matches!(
        &calls[0].occurrence.target,
        OccurrenceTarget::Local(target) if target.raw == decoy_note
    ) {
        return Err(TestError::Falsified("obj.child.note() must not resolve to Decoy.note"));
    }
    if matches!(
        &calls[0].occurrence.target,
        OccurrenceTarget::Local(target) if target.raw == child_note
    ) {
        return Err(TestError::Falsified("obj.child.note() must not bind Child.note"));
    }
    fs::remove_dir_all(&env.work).map_err(|source| TestError::Io("remove scratch", source))?;
    Ok(())
}

#[test]
fn py_named_attribute_diamond_under_mid_targets_child_note_not_decoy() -> Result<(), TestError> {
    const SOURCE: &[u8] = b"\
class Decoy:
    def note(self):
        return 0

class Child:
    def note(self):
        return 1

class Base:
    child: Child

class Left(Base):
    pass

class Right(Base):
    pass

class Mid(Left, Right):
    pass

class Holder(Mid):
    pass

def run(obj: Holder):
    return obj.child.note()
";
    let env = inherited_fixture_env()?;
    let toolchain = env.toolchain()?;
    let cancelled = AtomicBool::new(false);
    let fragment = compile_inherited_fragment(SOURCE, &env.work, &toolchain, &cancelled)?;
    let view = fragment.view()?;
    let run_owner = entity_ordinal_at_index(&view, b"run", EntityKind::Function, 0)?;
    let decoy_note = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 0)?;
    let child_note = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 1)?;
    let calls = read_method_calls(&view, run_owner)?;
    if calls.len() != 1 {
        return Err(TestError::Falsified("run owns one MethodCall"));
    }
    assert_local_index(calls[0], child_note, "obj.child.note() resolves to Child.note")?;
    if matches!(
        &calls[0].occurrence.target,
        OccurrenceTarget::Local(target) if target.raw == decoy_note
    ) {
        return Err(TestError::Falsified("obj.child.note() must not resolve to Decoy.note"));
    }
    fs::remove_dir_all(&env.work).map_err(|source| TestError::Io("remove scratch", source))?;
    Ok(())
}

#[test]
fn py_named_attribute_list_read_does_not_take_module_field() -> Result<(), TestError> {
    const SOURCE: &[u8] = b"\
class Child:
    def note(self):
        return 1

class Holder:
    note = 1
    child: list[Child]

def run(obj: Holder):
    return obj.child.note
";
    let env = inherited_fixture_env()?;
    let toolchain = env.toolchain()?;
    let cancelled = AtomicBool::new(false);
    let fragment = compile_inherited_fragment(SOURCE, &env.work, &toolchain, &cancelled)?;
    let view = fragment.view()?;
    let run_owner = entity_ordinal_at_index(&view, b"run", EntityKind::Function, 0)?;
    let holder_note = entity_ordinal_at_index(&view, b"note", EntityKind::Field, 0)?;
    let child_note = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 0)?;
    let calls = read_method_calls(&view, run_owner)?;
    if !calls.is_empty() {
        return Err(TestError::Falsified("run owns zero MethodCalls"));
    }
    let reads = field_accesses_with_path(&view, run_owner, b"note")?;
    if reads.len() != 1 {
        return Err(TestError::Falsified("run owns one FieldAccess with path note"));
    }
    assert_universe_field(reads[0], "list[Child] read stays a pypi universe field key")?;
    if reads[0].occurrence.confidence != OccurrenceConfidence::Index {
        return Err(TestError::Falsified("list[Child] universe field confidence is Index"));
    }
    if matches!(
        &reads[0].occurrence.target,
        OccurrenceTarget::Local(target) if target.raw == holder_note
    ) {
        return Err(TestError::Falsified("obj.child.note must not resolve to Holder.note"));
    }
    if matches!(
        &reads[0].occurrence.target,
        OccurrenceTarget::Local(target) if target.raw == child_note
    ) {
        return Err(TestError::Falsified("obj.child.note must not resolve to Child.note"));
    }
    fs::remove_dir_all(&env.work).map_err(|source| TestError::Io("remove scratch", source))?;
    Ok(())
}

#[test]
fn py_named_attribute_dotted_stays_universe() -> Result<(), TestError> {
    const SOURCE: &[u8] = b"\
import pkg

class Decoy:
    def note(self):
        return 0

class Child:
    def note(self):
        return 1

class Holder:
    child: pkg.Child

def run(obj: Holder):
    return obj.child.note()
";
    let env = inherited_fixture_env()?;
    let toolchain = env.toolchain()?;
    let cancelled = AtomicBool::new(false);
    let fragment = compile_inherited_fragment(SOURCE, &env.work, &toolchain, &cancelled)?;
    let view = fragment.view()?;
    let run_owner = entity_ordinal_at_index(&view, b"run", EntityKind::Function, 0)?;
    let child_note = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 1)?;
    let calls = read_method_calls(&view, run_owner)?;
    if calls.len() != 1 {
        return Err(TestError::Falsified("run owns one MethodCall"));
    }
    assert_universe_method(calls[0], "dotted field annotation stays a pypi universe key")?;
    if matches!(
        &calls[0].occurrence.target,
        OccurrenceTarget::Local(target) if target.raw == child_note
    ) {
        return Err(TestError::Falsified("obj.child.note() must not resolve to Child.note"));
    }
    fs::remove_dir_all(&env.work).map_err(|source| TestError::Io("remove scratch", source))?;
    Ok(())
}

#[test]
fn py_named_attribute_missing_member_stays_attribute_key() -> Result<(), TestError> {
    const SOURCE: &[u8] = b"\
class Decoy:
    def note(self):
        return 0

class Child:
    pass

class Holder:
    child: Child

def run(obj: Holder):
    return obj.child.note()
";
    let env = inherited_fixture_env()?;
    let toolchain = env.toolchain()?;
    let cancelled = AtomicBool::new(false);
    let fragment = compile_inherited_fragment(SOURCE, &env.work, &toolchain, &cancelled)?;
    let view = fragment.view()?;
    let run_owner = entity_ordinal_at_index(&view, b"run", EntityKind::Function, 0)?;
    let decoy_note = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 0)?;
    let calls = read_method_calls(&view, run_owner)?;
    if calls.len() != 1 {
        return Err(TestError::Falsified("run owns one MethodCall"));
    }
    assert_universe_method(calls[0], "missing member stays a pypi universe key")?;
    if matches!(
        &calls[0].occurrence.target,
        OccurrenceTarget::Local(target) if target.raw == decoy_note
    ) {
        return Err(TestError::Falsified("obj.child.note() must not resolve to Decoy.note"));
    }
    fs::remove_dir_all(&env.work).map_err(|source| TestError::Io("remove scratch", source))?;
    Ok(())
}

#[test]
fn py_named_attribute_field_wins_over_method() -> Result<(), TestError> {
    const SOURCE: &[u8] = b"\
class Decoy:
    score = 9

class Child:
    score = 1
    def score(self):
        return 2

class Holder:
    child: Child

def run(obj: Holder):
    return obj.child.score
";
    let env = inherited_fixture_env()?;
    let toolchain = env.toolchain()?;
    let cancelled = AtomicBool::new(false);
    let fragment = compile_inherited_fragment(SOURCE, &env.work, &toolchain, &cancelled)?;
    let view = fragment.view()?;
    let run_owner = entity_ordinal_at_index(&view, b"run", EntityKind::Function, 0)?;
    let decoy_score = entity_ordinal_at_index(&view, b"score", EntityKind::Field, 0)?;
    let child_score = entity_ordinal_at_index(&view, b"score", EntityKind::Field, 1)?;
    let child_score_method = entity_ordinal_at_index(&view, b"score", EntityKind::Function, 0)?;
    let calls = read_method_calls(&view, run_owner)?;
    if !calls.is_empty() {
        return Err(TestError::Falsified("run owns zero MethodCalls"));
    }
    let reads = field_accesses_with_path(&view, run_owner, b"score")?;
    if reads.len() != 1 {
        return Err(TestError::Falsified("run owns one FieldAccess with path score"));
    }
    assert_local_index(reads[0], child_score, "obj.child.score resolves to Child.score field")?;
    if matches!(
        &reads[0].occurrence.target,
        OccurrenceTarget::Local(target) if target.raw == decoy_score
    ) {
        return Err(TestError::Falsified("obj.child.score must not resolve to Decoy.score"));
    }
    if matches!(
        &reads[0].occurrence.target,
        OccurrenceTarget::Local(target) if target.raw == child_score_method
    ) {
        return Err(TestError::Falsified("obj.child.score must not resolve to Child.score method"));
    }
    fs::remove_dir_all(&env.work).map_err(|source| TestError::Io("remove scratch", source))?;
    Ok(())
}

#[test]
fn py_named_attribute_call_binds_method_not_field() -> Result<(), TestError> {
    const SOURCE: &[u8] = b"\
class Decoy:
    def note(self):
        return 0

class Child:
    note = 1
    def note(self):
        return 2

class Holder:
    child: Child

def run(obj: Holder):
    return obj.child.note()
";
    let env = inherited_fixture_env()?;
    let toolchain = env.toolchain()?;
    let cancelled = AtomicBool::new(false);
    let fragment = compile_inherited_fragment(SOURCE, &env.work, &toolchain, &cancelled)?;
    let view = fragment.view()?;
    let run_owner = entity_ordinal_at_index(&view, b"run", EntityKind::Function, 0)?;
    let decoy_note = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 0)?;
    let child_note_field = entity_ordinal_at_index(&view, b"note", EntityKind::Field, 0)?;
    let child_note = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 1)?;
    let calls = read_method_calls(&view, run_owner)?;
    if calls.len() != 1 {
        return Err(TestError::Falsified("run owns one MethodCall"));
    }
    assert_local_index(calls[0], child_note, "obj.child.note() resolves to Child.note method")?;
    if matches!(
        &calls[0].occurrence.target,
        OccurrenceTarget::Local(target) if target.raw == decoy_note
    ) {
        return Err(TestError::Falsified("obj.child.note() must not resolve to Decoy.note"));
    }
    if matches!(
        &calls[0].occurrence.target,
        OccurrenceTarget::Local(target) if target.raw == child_note_field
    ) {
        return Err(TestError::Falsified("obj.child.note() must not resolve to Child.note field"));
    }
    fs::remove_dir_all(&env.work).map_err(|source| TestError::Io("remove scratch", source))?;
    Ok(())
}

#[test]
fn py_named_attribute_deeper_chain_stays_universe() -> Result<(), TestError> {
    const SOURCE: &[u8] = b"\
class Decoy:
    def note(self):
        return 0

class Child:
    def note(self):
        return 1
    other: Child

class Holder:
    child: Child

def run(obj: Holder):
    return obj.child.other.note()
";
    let env = inherited_fixture_env()?;
    let toolchain = env.toolchain()?;
    let cancelled = AtomicBool::new(false);
    let fragment = compile_inherited_fragment(SOURCE, &env.work, &toolchain, &cancelled)?;
    let view = fragment.view()?;
    let run_owner = entity_ordinal_at_index(&view, b"run", EntityKind::Function, 0)?;
    let decoy_note = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 0)?;
    let child_note = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 1)?;
    let holder_child = entity_ordinal_at_index(&view, b"child", EntityKind::Field, 0)?;
    let child_other = entity_ordinal_at_index(&view, b"other", EntityKind::Field, 0)?;
    let calls = read_method_calls(&view, run_owner)?;
    if calls.len() != 1 {
        return Err(TestError::Falsified("run owns one MethodCall"));
    }
    assert_local_index(calls[0], child_note, "obj.child.other.note() resolves to Child.note")?;
    if matches!(
        &calls[0].occurrence.target,
        OccurrenceTarget::Local(target) if target.raw == decoy_note
    ) {
        return Err(TestError::Falsified("obj.child.other.note() must not resolve to Decoy.note"));
    }
    let child_reads = field_accesses_with_path(&view, run_owner, b"child")?;
    if child_reads.len() != 1 {
        return Err(TestError::Falsified("run owns one FieldAccess with path child"));
    }
    assert_local_index(
        child_reads[0],
        holder_child,
        "obj.child resolves to Holder.child",
    )?;
    let other_reads = field_accesses_with_path(&view, run_owner, b"other")?;
    if other_reads.len() != 1 {
        return Err(TestError::Falsified("run owns one FieldAccess with path other"));
    }
    assert_local_index(
        other_reads[0],
        child_other,
        "obj.child.other resolves to Child.other",
    )?;
    fs::remove_dir_all(&env.work).map_err(|source| TestError::Io("remove scratch", source))?;
    Ok(())
}

#[test]
fn py_named_attribute_ambiguous_union_stays_universe() -> Result<(), TestError> {
    const SOURCE: &[u8] = b"\
class Decoy:
    def note(self):
        return 0

class Child:
    def note(self):
        return 1

class Holder:
    child: Child

def run(obj: Holder | Decoy):
    return obj.child.note()
";
    let env = inherited_fixture_env()?;
    let toolchain = env.toolchain()?;
    let cancelled = AtomicBool::new(false);
    let fragment = compile_inherited_fragment(SOURCE, &env.work, &toolchain, &cancelled)?;
    let view = fragment.view()?;
    let run_owner = entity_ordinal_at_index(&view, b"run", EntityKind::Function, 0)?;
    let decoy_note = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 0)?;
    let child_note = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 1)?;
    let calls = read_method_calls(&view, run_owner)?;
    if calls.len() != 1 {
        return Err(TestError::Falsified("run owns one MethodCall"));
    }
    assert_universe_method(calls[0], "ambiguous union stays a pypi universe key")?;
    if calls[0].occurrence.confidence != OccurrenceConfidence::Index {
        return Err(TestError::Falsified("ambiguous union universe method confidence is Index"));
    }
    if matches!(
        &calls[0].occurrence.target,
        OccurrenceTarget::Local(target) if target.raw == decoy_note
    ) {
        return Err(TestError::Falsified("obj.child.note() must not resolve to Decoy.note"));
    }
    if matches!(
        &calls[0].occurrence.target,
        OccurrenceTarget::Local(target) if target.raw == child_note
    ) {
        return Err(TestError::Falsified("obj.child.note() must not bind Child.note"));
    }
    fs::remove_dir_all(&env.work).map_err(|source| TestError::Io("remove scratch", source))?;
    Ok(())
}

#[test]
fn py_instance_attribute_two_bases_same_annotation_stay_universe() -> Result<(), TestError> {
    const SOURCE: &[u8] = b"\
class Decoy:
    def note(self):
        return 0

class Child:
    def note(self):
        return 1

class Left:
    child: Child

class Right:
    child: Child

class Holder(Left, Right):
    def run(self):
        return self.child.note()
";
    let env = inherited_fixture_env()?;
    let toolchain = env.toolchain()?;
    let cancelled = AtomicBool::new(false);
    let fragment = compile_inherited_fragment(SOURCE, &env.work, &toolchain, &cancelled)?;
    let view = fragment.view()?;
    let run_owner = entity_ordinal_at_index(&view, b"run", EntityKind::Function, 0)?;
    let decoy_note = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 0)?;
    let child_note = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 1)?;
    let calls = read_method_calls(&view, run_owner)?;
    if calls.len() != 1 {
        return Err(TestError::Falsified("run owns one MethodCall"));
    }
    assert_universe_method(
        calls[0],
        "two sibling fields with the same annotation stay a pypi universe key",
    )?;
    if matches!(
        &calls[0].occurrence.target,
        OccurrenceTarget::Local(target) if target.raw == decoy_note
    ) {
        return Err(TestError::Falsified("self.child.note() must not resolve to Decoy.note"));
    }
    if matches!(
        &calls[0].occurrence.target,
        OccurrenceTarget::Local(target) if target.raw == child_note
    ) {
        return Err(TestError::Falsified("self.child.note() must not resolve to Child.note"));
    }
    fs::remove_dir_all(&env.work).map_err(|source| TestError::Io("remove scratch", source))?;
    Ok(())
}

#[test]
fn py_instance_attribute_diamond_field_targets_child_note_not_decoy() -> Result<(), TestError> {
    const SOURCE: &[u8] = b"\
class Decoy:
    def note(self):
        return 0

class Child:
    def note(self):
        return 1

class Base:
    child: Child

class Left(Base):
    pass

class Right(Base):
    pass

class Holder(Left, Right):
    def run(self):
        return self.child.note()
";
    let env = inherited_fixture_env()?;
    let toolchain = env.toolchain()?;
    let cancelled = AtomicBool::new(false);
    let fragment = compile_inherited_fragment(SOURCE, &env.work, &toolchain, &cancelled)?;
    let view = fragment.view()?;
    let run_owner = entity_ordinal_at_index(&view, b"run", EntityKind::Function, 0)?;
    let decoy_note = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 0)?;
    let child_note = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 1)?;
    let calls = read_method_calls(&view, run_owner)?;
    if calls.len() != 1 {
        return Err(TestError::Falsified("run owns one MethodCall"));
    }
    assert_local_index(calls[0], child_note, "self.child.note() resolves to Child.note")?;
    if matches!(
        &calls[0].occurrence.target,
        OccurrenceTarget::Local(target) if target.raw == decoy_note
    ) {
        return Err(TestError::Falsified("self.child.note() must not resolve to Decoy.note"));
    }
    fs::remove_dir_all(&env.work).map_err(|source| TestError::Io("remove scratch", source))?;
    Ok(())
}

#[test]
fn py_instance_attribute_diamond_under_mid_targets_child_note_not_decoy() -> Result<(), TestError> {
    const SOURCE: &[u8] = b"\
class Decoy:
    def note(self):
        return 0

class Child:
    def note(self):
        return 1

class Base:
    child: Child

class Left(Base):
    pass

class Right(Base):
    pass

class Mid(Left, Right):
    pass

class Holder(Mid):
    def run(self):
        return self.child.note()
";
    let env = inherited_fixture_env()?;
    let toolchain = env.toolchain()?;
    let cancelled = AtomicBool::new(false);
    let fragment = compile_inherited_fragment(SOURCE, &env.work, &toolchain, &cancelled)?;
    let view = fragment.view()?;
    let run_owner = entity_ordinal_at_index(&view, b"run", EntityKind::Function, 0)?;
    let decoy_note = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 0)?;
    let child_note = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 1)?;
    let calls = read_method_calls(&view, run_owner)?;
    if calls.len() != 1 {
        return Err(TestError::Falsified("run owns one MethodCall"));
    }
    assert_local_index(calls[0], child_note, "self.child.note() resolves to Child.note")?;
    if matches!(
        &calls[0].occurrence.target,
        OccurrenceTarget::Local(target) if target.raw == decoy_note
    ) {
        return Err(TestError::Falsified("self.child.note() must not resolve to Decoy.note"));
    }
    fs::remove_dir_all(&env.work).map_err(|source| TestError::Io("remove scratch", source))?;
    Ok(())
}

#[test]
fn py_chained_attribute_self_call_targets_child_note_not_decoy() -> Result<(), TestError> {
    const SOURCE: &[u8] = b"\
class Decoy:
    def note(self):
        return 0

class Child:
    def note(self):
        return 1
    other: Child

class Holder:
    child: Child
    def run(self):
        return self.child.other.note()
";
    let env = inherited_fixture_env()?;
    let toolchain = env.toolchain()?;
    let cancelled = AtomicBool::new(false);
    let fragment = compile_inherited_fragment(SOURCE, &env.work, &toolchain, &cancelled)?;
    let view = fragment.view()?;
    let run_owner = entity_ordinal_at_index(&view, b"run", EntityKind::Function, 0)?;
    let decoy_note = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 0)?;
    let child_note = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 1)?;
    let holder_child = entity_ordinal_at_index(&view, b"child", EntityKind::Field, 0)?;
    let child_other = entity_ordinal_at_index(&view, b"other", EntityKind::Field, 0)?;
    let calls = read_method_calls(&view, run_owner)?;
    if calls.len() != 1 {
        return Err(TestError::Falsified("run owns one MethodCall"));
    }
    assert_local_index(calls[0], child_note, "self.child.other.note() resolves to Child.note")?;
    if matches!(
        &calls[0].occurrence.target,
        OccurrenceTarget::Local(target) if target.raw == decoy_note
    ) {
        return Err(TestError::Falsified("self.child.other.note() must not resolve to Decoy.note"));
    }
    let child_reads = field_accesses_with_path(&view, run_owner, b"child")?;
    if child_reads.len() != 1 {
        return Err(TestError::Falsified("run owns one FieldAccess with path child"));
    }
    assert_local_index(
        child_reads[0],
        holder_child,
        "self.child resolves to Holder.child",
    )?;
    let other_reads = field_accesses_with_path(&view, run_owner, b"other")?;
    if other_reads.len() != 1 {
        return Err(TestError::Falsified("run owns one FieldAccess with path other"));
    }
    assert_local_index(
        other_reads[0],
        child_other,
        "self.child.other resolves to Child.other",
    )?;
    fs::remove_dir_all(&env.work).map_err(|source| TestError::Io("remove scratch", source))?;
    Ok(())
}

#[test]
fn py_chained_attribute_named_call_targets_child_note_not_decoy() -> Result<(), TestError> {
    const SOURCE: &[u8] = b"\
class Decoy:
    def note(self):
        return 0

class Child:
    def note(self):
        return 1
    other: Child

class Holder:
    child: Child

def run(obj: Holder):
    return obj.child.other.note()
";
    let env = inherited_fixture_env()?;
    let toolchain = env.toolchain()?;
    let cancelled = AtomicBool::new(false);
    let fragment = compile_inherited_fragment(SOURCE, &env.work, &toolchain, &cancelled)?;
    let view = fragment.view()?;
    let run_owner = entity_ordinal_at_index(&view, b"run", EntityKind::Function, 0)?;
    let decoy_note = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 0)?;
    let child_note = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 1)?;
    let holder_child = entity_ordinal_at_index(&view, b"child", EntityKind::Field, 0)?;
    let child_other = entity_ordinal_at_index(&view, b"other", EntityKind::Field, 0)?;
    let calls = read_method_calls(&view, run_owner)?;
    if calls.len() != 1 {
        return Err(TestError::Falsified("run owns one MethodCall"));
    }
    assert_local_index(calls[0], child_note, "obj.child.other.note() resolves to Child.note")?;
    if matches!(
        &calls[0].occurrence.target,
        OccurrenceTarget::Local(target) if target.raw == decoy_note
    ) {
        return Err(TestError::Falsified("obj.child.other.note() must not resolve to Decoy.note"));
    }
    let child_reads = field_accesses_with_path(&view, run_owner, b"child")?;
    if child_reads.len() != 1 {
        return Err(TestError::Falsified("run owns one FieldAccess with path child"));
    }
    assert_local_index(
        child_reads[0],
        holder_child,
        "obj.child resolves to Holder.child",
    )?;
    let other_reads = field_accesses_with_path(&view, run_owner, b"other")?;
    if other_reads.len() != 1 {
        return Err(TestError::Falsified("run owns one FieldAccess with path other"));
    }
    assert_local_index(
        other_reads[0],
        child_other,
        "obj.child.other resolves to Child.other",
    )?;
    fs::remove_dir_all(&env.work).map_err(|source| TestError::Io("remove scratch", source))?;
    Ok(())
}

#[test]
fn py_chained_attribute_three_deep_call_targets_child_note_not_decoy() -> Result<(), TestError> {
    const SOURCE: &[u8] = b"\
class Decoy:
    def note(self):
        return 0

class Child:
    def note(self):
        return 1
    other: Child

class Mid:
    child: Child

class Holder:
    item: Mid
    def run(self):
        return self.item.child.other.note()
";
    let env = inherited_fixture_env()?;
    let toolchain = env.toolchain()?;
    let cancelled = AtomicBool::new(false);
    let fragment = compile_inherited_fragment(SOURCE, &env.work, &toolchain, &cancelled)?;
    let view = fragment.view()?;
    let run_owner = entity_ordinal_at_index(&view, b"run", EntityKind::Function, 0)?;
    let decoy_note = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 0)?;
    let child_note = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 1)?;
    let calls = read_method_calls(&view, run_owner)?;
    if calls.len() != 1 {
        return Err(TestError::Falsified("run owns one MethodCall"));
    }
    assert_local_index(
        calls[0],
        child_note,
        "self.item.child.other.note() resolves to Child.note",
    )?;
    if matches!(
        &calls[0].occurrence.target,
        OccurrenceTarget::Local(target) if target.raw == decoy_note
    ) {
        return Err(TestError::Falsified(
            "self.item.child.other.note() must not resolve to Decoy.note",
        ));
    }
    fs::remove_dir_all(&env.work).map_err(|source| TestError::Io("remove scratch", source))?;
    Ok(())
}

#[test]
fn py_chained_attribute_closure_call_targets_child_note_not_decoy() -> Result<(), TestError> {
    const SOURCE: &[u8] = b"\
class Decoy:
    def note(self):
        return 0

class Child:
    def note(self):
        return 1
    other: Child

class Holder:
    child: Child

def outer(obj: Holder):
    def inner():
        return obj.child.other.note()
    return inner()
";
    let env = inherited_fixture_env()?;
    let toolchain = env.toolchain()?;
    let cancelled = AtomicBool::new(false);
    let fragment = compile_inherited_fragment(SOURCE, &env.work, &toolchain, &cancelled)?;
    let view = fragment.view()?;
    let inner_owner = entity_ordinal_at_index(&view, b"inner", EntityKind::Function, 0)?;
    let decoy_note = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 0)?;
    let child_note = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 1)?;
    let calls = read_method_calls(&view, inner_owner)?;
    if calls.len() != 1 {
        return Err(TestError::Falsified("inner owns one MethodCall"));
    }
    assert_local_index(calls[0], child_note, "obj.child.other.note() resolves to Child.note")?;
    if matches!(
        &calls[0].occurrence.target,
        OccurrenceTarget::Local(target) if target.raw == decoy_note
    ) {
        return Err(TestError::Falsified("obj.child.other.note() must not resolve to Decoy.note"));
    }
    fs::remove_dir_all(&env.work).map_err(|source| TestError::Io("remove scratch", source))?;
    Ok(())
}

#[test]
fn py_chained_attribute_nested_self_stays_universe() -> Result<(), TestError> {
    const SOURCE: &[u8] = b"\
class Decoy:
    def note(self):
        return 0

class Child:
    def note(self):
        return 1
    other: Child

class Holder:
    child: Child
    def run(self):
        def inner():
            return self.child.other.note()
        return inner()
";
    let env = inherited_fixture_env()?;
    let toolchain = env.toolchain()?;
    let cancelled = AtomicBool::new(false);
    let fragment = compile_inherited_fragment(SOURCE, &env.work, &toolchain, &cancelled)?;
    let view = fragment.view()?;
    let inner_owner = entity_ordinal_at_index(&view, b"inner", EntityKind::Function, 0)?;
    let child_note = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 1)?;
    let calls = read_method_calls(&view, inner_owner)?;
    if calls.len() != 1 {
        return Err(TestError::Falsified("inner owns one MethodCall"));
    }
    assert_universe_method(calls[0], "nested self stays a pypi universe key")?;
    if calls[0].occurrence.confidence != OccurrenceConfidence::Index {
        return Err(TestError::Falsified("nested self universe method confidence is Index"));
    }
    if matches!(
        &calls[0].occurrence.target,
        OccurrenceTarget::Local(target) if target.raw == child_note
    ) {
        return Err(TestError::Falsified("self.child.other.note() must not resolve to Child.note"));
    }
    fs::remove_dir_all(&env.work).map_err(|source| TestError::Io("remove scratch", source))?;
    Ok(())
}

#[test]
fn py_chained_attribute_middle_unannotated_stays_universe() -> Result<(), TestError> {
    const SOURCE: &[u8] = b"\
class Decoy:
    def note(self):
        return 0

class Child:
    def note(self):
        return 1
    other = 1

class Holder:
    child: Child
    def run(self):
        return self.child.other.note()
";
    let env = inherited_fixture_env()?;
    let toolchain = env.toolchain()?;
    let cancelled = AtomicBool::new(false);
    let fragment = compile_inherited_fragment(SOURCE, &env.work, &toolchain, &cancelled)?;
    let view = fragment.view()?;
    let run_owner = entity_ordinal_at_index(&view, b"run", EntityKind::Function, 0)?;
    let decoy_note = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 0)?;
    let child_note = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 1)?;
    let calls = read_method_calls(&view, run_owner)?;
    if calls.len() != 1 {
        return Err(TestError::Falsified("run owns one MethodCall"));
    }
    assert_universe_method(calls[0], "unannotated middle stays a pypi universe key")?;
    if calls[0].occurrence.confidence != OccurrenceConfidence::Index {
        return Err(TestError::Falsified("unannotated middle universe method confidence is Index"));
    }
    if matches!(
        &calls[0].occurrence.target,
        OccurrenceTarget::Local(target) if target.raw == decoy_note
    ) {
        return Err(TestError::Falsified("self.child.other.note() must not resolve to Decoy.note"));
    }
    if matches!(
        &calls[0].occurrence.target,
        OccurrenceTarget::Local(target) if target.raw == child_note
    ) {
        return Err(TestError::Falsified("self.child.other.note() must not resolve to Child.note"));
    }
    fs::remove_dir_all(&env.work).map_err(|source| TestError::Io("remove scratch", source))?;
    Ok(())
}

#[test]
fn py_chained_attribute_second_hop_two_bases_stay_universe() -> Result<(), TestError> {
    const SOURCE: &[u8] = b"\
class Decoy:
    def note(self):
        return 0

class Child:
    def note(self):
        return 1

class Left:
    other: Child

class Right:
    other: Child

class Mid(Left, Right):
    pass

class Holder:
    child: Mid
    def run(self):
        return self.child.other.note()
";
    let env = inherited_fixture_env()?;
    let toolchain = env.toolchain()?;
    let cancelled = AtomicBool::new(false);
    let fragment = compile_inherited_fragment(SOURCE, &env.work, &toolchain, &cancelled)?;
    let view = fragment.view()?;
    let run_owner = entity_ordinal_at_index(&view, b"run", EntityKind::Function, 0)?;
    let decoy_note = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 0)?;
    let child_note = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 1)?;
    let calls = read_method_calls(&view, run_owner)?;
    if calls.len() != 1 {
        return Err(TestError::Falsified("run owns one MethodCall"));
    }
    assert_universe_method(calls[0], "two-base second hop stays a pypi universe key")?;
    if calls[0].occurrence.confidence != OccurrenceConfidence::Index {
        return Err(TestError::Falsified("two-base second hop universe method confidence is Index"));
    }
    if matches!(
        &calls[0].occurrence.target,
        OccurrenceTarget::Local(target) if target.raw == decoy_note
    ) {
        return Err(TestError::Falsified("self.child.other.note() must not resolve to Decoy.note"));
    }
    if matches!(
        &calls[0].occurrence.target,
        OccurrenceTarget::Local(target) if target.raw == child_note
    ) {
        return Err(TestError::Falsified("self.child.other.note() must not resolve to Child.note"));
    }
    fs::remove_dir_all(&env.work).map_err(|source| TestError::Io("remove scratch", source))?;
    Ok(())
}

#[test]
fn py_chained_attribute_diamond_second_hop_targets_child_note_not_decoy() -> Result<(), TestError> {
    const SOURCE: &[u8] = b"\
class Decoy:
    def note(self):
        return 0

class Child:
    def note(self):
        return 1

class Base:
    item: Child

class Left(Base):
    pass

class Right(Base):
    pass

class Mid(Left, Right):
    pass

class Holder:
    child: Mid
    def run(self):
        return self.child.item.note()
";
    let env = inherited_fixture_env()?;
    let toolchain = env.toolchain()?;
    let cancelled = AtomicBool::new(false);
    let fragment = compile_inherited_fragment(SOURCE, &env.work, &toolchain, &cancelled)?;
    let view = fragment.view()?;
    let run_owner = entity_ordinal_at_index(&view, b"run", EntityKind::Function, 0)?;
    let decoy_note = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 0)?;
    let child_note = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 1)?;
    let calls = read_method_calls(&view, run_owner)?;
    if calls.len() != 1 {
        return Err(TestError::Falsified("run owns one MethodCall"));
    }
    assert_local_index(calls[0], child_note, "self.child.item.note() resolves to Child.note")?;
    if matches!(
        &calls[0].occurrence.target,
        OccurrenceTarget::Local(target) if target.raw == decoy_note
    ) {
        return Err(TestError::Falsified("self.child.item.note() must not resolve to Decoy.note"));
    }
    fs::remove_dir_all(&env.work).map_err(|source| TestError::Io("remove scratch", source))?;
    Ok(())
}

#[test]
fn py_chained_attribute_field_targets_child_score_not_decoy() -> Result<(), TestError> {
    const SOURCE: &[u8] = b"\
class Decoy:
    score = 9

class Child:
    score = 1
    other: Child

class Holder:
    child: Child
    def run(self):
        return self.child.other.score
";
    let env = inherited_fixture_env()?;
    let toolchain = env.toolchain()?;
    let cancelled = AtomicBool::new(false);
    let fragment = compile_inherited_fragment(SOURCE, &env.work, &toolchain, &cancelled)?;
    let view = fragment.view()?;
    let run_owner = entity_ordinal_at_index(&view, b"run", EntityKind::Function, 0)?;
    let decoy_score = entity_ordinal_at_index(&view, b"score", EntityKind::Field, 0)?;
    let child_score = entity_ordinal_at_index(&view, b"score", EntityKind::Field, 1)?;
    let calls = read_method_calls(&view, run_owner)?;
    if !calls.is_empty() {
        return Err(TestError::Falsified("run owns zero MethodCalls"));
    }
    let reads = field_accesses_with_path(&view, run_owner, b"score")?;
    if reads.len() != 1 {
        return Err(TestError::Falsified("run owns one FieldAccess with path score"));
    }
    assert_local_index(reads[0], child_score, "self.child.other.score resolves to Child.score")?;
    if matches!(
        &reads[0].occurrence.target,
        OccurrenceTarget::Local(target) if target.raw == decoy_score
    ) {
        return Err(TestError::Falsified("self.child.other.score must not resolve to Decoy.score"));
    }
    fs::remove_dir_all(&env.work).map_err(|source| TestError::Io("remove scratch", source))?;
    Ok(())
}

#[test]
fn py_chained_attribute_value_targets_child_note_not_decoy() -> Result<(), TestError> {
    const SOURCE: &[u8] = b"\
class Decoy:
    def note(self):
        return 0

class Child:
    def note(self):
        return 1
    other: Child

class Holder:
    child: Child
    def run(self):
        return self.child.other.note
";
    let env = inherited_fixture_env()?;
    let toolchain = env.toolchain()?;
    let cancelled = AtomicBool::new(false);
    let fragment = compile_inherited_fragment(SOURCE, &env.work, &toolchain, &cancelled)?;
    let view = fragment.view()?;
    let run_owner = entity_ordinal_at_index(&view, b"run", EntityKind::Function, 0)?;
    let decoy_note = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 0)?;
    let child_note = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 1)?;
    let calls = read_method_calls(&view, run_owner)?;
    if !calls.is_empty() {
        return Err(TestError::Falsified("run owns zero MethodCalls"));
    }
    let reads = field_accesses_with_path(&view, run_owner, b"note")?;
    if reads.len() != 1 {
        return Err(TestError::Falsified("run owns one FieldAccess with path note"));
    }
    assert_local_index(reads[0], child_note, "self.child.other.note resolves to Child.note")?;
    if matches!(
        &reads[0].occurrence.target,
        OccurrenceTarget::Local(target) if target.raw == decoy_note
    ) {
        return Err(TestError::Falsified("self.child.other.note must not resolve to Decoy.note"));
    }
    fs::remove_dir_all(&env.work).map_err(|source| TestError::Io("remove scratch", source))?;
    Ok(())
}

#[test]
fn py_chained_attribute_list_middle_does_not_take_module_field() -> Result<(), TestError> {
    const SOURCE: &[u8] = b"\
class Child:
    def note(self):
        return 1
    other: list[Child]

class Holder:
    note = 1
    child: Child
    def run(self):
        return self.child.other.note
";
    let env = inherited_fixture_env()?;
    let toolchain = env.toolchain()?;
    let cancelled = AtomicBool::new(false);
    let fragment = compile_inherited_fragment(SOURCE, &env.work, &toolchain, &cancelled)?;
    let view = fragment.view()?;
    let run_owner = entity_ordinal_at_index(&view, b"run", EntityKind::Function, 0)?;
    let holder_note = entity_ordinal_at_index(&view, b"note", EntityKind::Field, 0)?;
    let child_note_fn = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 0)?;
    let calls = read_method_calls(&view, run_owner)?;
    if !calls.is_empty() {
        return Err(TestError::Falsified("run owns zero MethodCalls"));
    }
    let reads = field_accesses_with_path(&view, run_owner, b"note")?;
    if reads.len() != 1 {
        return Err(TestError::Falsified("run owns one FieldAccess with path note"));
    }
    assert_universe_field(reads[0], "list[Child] middle stays a pypi universe field key")?;
    if reads[0].occurrence.confidence != OccurrenceConfidence::Index {
        return Err(TestError::Falsified("list[Child] universe field confidence is Index"));
    }
    if matches!(
        &reads[0].occurrence.target,
        OccurrenceTarget::Local(target) if target.raw == holder_note
    ) {
        return Err(TestError::Falsified("self.child.other.note must not resolve to Holder.note"));
    }
    if matches!(
        &reads[0].occurrence.target,
        OccurrenceTarget::Local(target) if target.raw == child_note_fn
    ) {
        return Err(TestError::Falsified("self.child.other.note must not resolve to Child.note"));
    }
    fs::remove_dir_all(&env.work).map_err(|source| TestError::Io("remove scratch", source))?;
    Ok(())
}

#[test]
fn py_chained_attribute_dotted_middle_stays_universe() -> Result<(), TestError> {
    const SOURCE: &[u8] = b"\
import pkg

class Decoy:
    def note(self):
        return 0

class Child:
    def note(self):
        return 1
    other: pkg.Child

class Holder:
    child: Child
    def run(self):
        return self.child.other.note()
";
    let env = inherited_fixture_env()?;
    let toolchain = env.toolchain()?;
    let cancelled = AtomicBool::new(false);
    let fragment = compile_inherited_fragment(SOURCE, &env.work, &toolchain, &cancelled)?;
    let view = fragment.view()?;
    let run_owner = entity_ordinal_at_index(&view, b"run", EntityKind::Function, 0)?;
    let child_note = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 1)?;
    let calls = read_method_calls(&view, run_owner)?;
    if calls.len() != 1 {
        return Err(TestError::Falsified("run owns one MethodCall"));
    }
    assert_universe_method(calls[0], "dotted middle stays a pypi universe key")?;
    if calls[0].occurrence.confidence != OccurrenceConfidence::Index {
        return Err(TestError::Falsified("dotted middle universe method confidence is Index"));
    }
    if matches!(
        &calls[0].occurrence.target,
        OccurrenceTarget::Local(target) if target.raw == child_note
    ) {
        return Err(TestError::Falsified("self.child.other.note must not resolve to Child.note"));
    }
    fs::remove_dir_all(&env.work).map_err(|source| TestError::Io("remove scratch", source))?;
    Ok(())
}

#[test]
fn py_chained_attribute_missing_member_stays_attribute_key() -> Result<(), TestError> {
    const SOURCE: &[u8] = b"\
class Decoy:
    def note(self):
        return 0

class Leaf:
    pass

class Child:
    other: Leaf

class Holder:
    child: Child
    def run(self):
        return self.child.other.note()
";
    let env = inherited_fixture_env()?;
    let toolchain = env.toolchain()?;
    let cancelled = AtomicBool::new(false);
    let fragment = compile_inherited_fragment(SOURCE, &env.work, &toolchain, &cancelled)?;
    let view = fragment.view()?;
    let run_owner = entity_ordinal_at_index(&view, b"run", EntityKind::Function, 0)?;
    let decoy_note = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 0)?;
    let calls = read_method_calls(&view, run_owner)?;
    if calls.len() != 1 {
        return Err(TestError::Falsified("run owns one MethodCall"));
    }
    assert_universe_method(calls[0], "missing member stays a pypi universe key")?;
    if calls[0].occurrence.confidence != OccurrenceConfidence::Index {
        return Err(TestError::Falsified("missing member universe method confidence is Index"));
    }
    if matches!(
        &calls[0].occurrence.target,
        OccurrenceTarget::Local(target) if target.raw == decoy_note
    ) {
        return Err(TestError::Falsified("self.child.other.note() must not resolve to Decoy.note"));
    }
    fs::remove_dir_all(&env.work).map_err(|source| TestError::Io("remove scratch", source))?;
    Ok(())
}

#[test]
fn py_chained_attribute_field_wins_over_method() -> Result<(), TestError> {
    const SOURCE: &[u8] = b"\
class Child:
    score = 1
    other: Child

    def score(self):
        return self.score

class Holder:
    child: Child
    def run(self):
        return self.child.other.score
";
    let env = inherited_fixture_env()?;
    let toolchain = env.toolchain()?;
    let cancelled = AtomicBool::new(false);
    let fragment = compile_inherited_fragment(SOURCE, &env.work, &toolchain, &cancelled)?;
    let view = fragment.view()?;
    let run_owner = entity_ordinal_at_index(&view, b"run", EntityKind::Function, 0)?;
    let score_field = entity_ordinal_at_index(&view, b"score", EntityKind::Field, 0)?;
    let score_method = entity_ordinal_at_index(&view, b"score", EntityKind::Function, 0)?;
    let calls = read_method_calls(&view, run_owner)?;
    if !calls.is_empty() {
        return Err(TestError::Falsified("run owns zero MethodCalls"));
    }
    let reads = field_accesses_with_path(&view, run_owner, b"score")?;
    if reads.len() != 1 {
        return Err(TestError::Falsified("run owns one FieldAccess with path score"));
    }
    assert_local_index(reads[0], score_field, "self.child.other.score resolves to Child.score field")?;
    if matches!(
        &reads[0].occurrence.target,
        OccurrenceTarget::Local(target) if target.raw == score_method
    ) {
        return Err(TestError::Falsified("self.child.other.score must not resolve to Child.score method"));
    }
    fs::remove_dir_all(&env.work).map_err(|source| TestError::Io("remove scratch", source))?;
    Ok(())
}

#[test]
fn py_chained_attribute_call_binds_method_not_field() -> Result<(), TestError> {
    const SOURCE: &[u8] = b"\
class Child:
    note = 1
    other: Child

    def note(self):
        return self.note

class Holder:
    child: Child
    def run(self):
        return self.child.other.note()
";
    let env = inherited_fixture_env()?;
    let toolchain = env.toolchain()?;
    let cancelled = AtomicBool::new(false);
    let fragment = compile_inherited_fragment(SOURCE, &env.work, &toolchain, &cancelled)?;
    let view = fragment.view()?;
    let run_owner = entity_ordinal_at_index(&view, b"run", EntityKind::Function, 0)?;
    let note_field = entity_ordinal_at_index(&view, b"note", EntityKind::Field, 0)?;
    let note_method = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 0)?;
    let calls = read_method_calls(&view, run_owner)?;
    if calls.len() != 1 {
        return Err(TestError::Falsified("run owns one MethodCall"));
    }
    assert_local_index(calls[0], note_method, "self.child.other.note() resolves to Child.note method")?;
    if matches!(
        &calls[0].occurrence.target,
        OccurrenceTarget::Local(target) if target.raw == note_field
    ) {
        return Err(TestError::Falsified("self.child.other.note() must not resolve to Child.note field"));
    }
    fs::remove_dir_all(&env.work).map_err(|source| TestError::Io("remove scratch", source))?;
    Ok(())
}

#[test]
fn py_chained_attribute_subscript_stays_universe() -> Result<(), TestError> {
    const SOURCE: &[u8] = b"\
class Decoy:
    def note(self):
        return 0

class Child:
    def note(self):
        return 1
    other: Child

class Holder:
    child: Child
    def run(self):
        return self.child.other[0].note()
";
    let env = inherited_fixture_env()?;
    let toolchain = env.toolchain()?;
    let cancelled = AtomicBool::new(false);
    let fragment = compile_inherited_fragment(SOURCE, &env.work, &toolchain, &cancelled)?;
    let view = fragment.view()?;
    let run_owner = entity_ordinal_at_index(&view, b"run", EntityKind::Function, 0)?;
    let child_note = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 1)?;
    let calls = read_method_calls(&view, run_owner)?;
    if calls.len() != 1 {
        return Err(TestError::Falsified("run owns one MethodCall"));
    }
    assert_universe_method(calls[0], "subscript middle stays a pypi universe key")?;
    if calls[0].occurrence.confidence != OccurrenceConfidence::Index {
        return Err(TestError::Falsified("subscript middle universe method confidence is Index"));
    }
    if matches!(
        &calls[0].occurrence.target,
        OccurrenceTarget::Local(target) if target.raw == child_note
    ) {
        return Err(TestError::Falsified("self.child.other[0].note() must not resolve to Child.note"));
    }
    fs::remove_dir_all(&env.work).map_err(|source| TestError::Io("remove scratch", source))?;
    Ok(())
}

#[test]
fn py_init_attribute_call_targets_child_note_not_decoy() -> Result<(), TestError> {
    const SOURCE: &[u8] = b"\
class Decoy:
    def note(self):
        return 0

class Child:
    def note(self):
        return 1

class Holder:
    def __init__(self):
        self.child: Child = Child()
    def run(self):
        return self.child.note()
";
    let env = inherited_fixture_env()?;
    let toolchain = env.toolchain()?;
    let cancelled = AtomicBool::new(false);
    let fragment = compile_inherited_fragment(SOURCE, &env.work, &toolchain, &cancelled)?;
    let view = fragment.view()?;
    let run_owner = entity_ordinal_at_index(&view, b"run", EntityKind::Function, 0)?;
    let decoy_note = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 0)?;
    let child_note = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 1)?;
    let calls = read_method_calls(&view, run_owner)?;
    if calls.len() != 1 {
        return Err(TestError::Falsified("run owns one MethodCall"));
    }
    assert_local_index(calls[0], child_note, "self.child.note() resolves to Child.note")?;
    if matches!(
        &calls[0].occurrence.target,
        OccurrenceTarget::Local(target) if target.raw == decoy_note
    ) {
        return Err(TestError::Falsified("self.child.note() must not resolve to Decoy.note"));
    }
    let child_reads = field_accesses_with_path(&view, run_owner, b"child")?;
    if child_reads.len() != 1 {
        return Err(TestError::Falsified("run owns one FieldAccess with path child"));
    }
    assert_universe_field_named(
        child_reads[0],
        b"child",
        "self.child stays a pypi universe field key",
    )?;
    fs::remove_dir_all(&env.work).map_err(|source| TestError::Io("remove scratch", source))?;
    Ok(())
}

#[test]
fn py_init_attribute_named_call_targets_child_note_not_decoy() -> Result<(), TestError> {
    const SOURCE: &[u8] = b"\
class Decoy:
    def note(self):
        return 0

class Child:
    def note(self):
        return 1

class Holder:
    def __init__(self):
        self.child: Child = Child()
    def run(obj: Holder):
        return obj.child.note()
";
    let env = inherited_fixture_env()?;
    let toolchain = env.toolchain()?;
    let cancelled = AtomicBool::new(false);
    let fragment = compile_inherited_fragment(SOURCE, &env.work, &toolchain, &cancelled)?;
    let view = fragment.view()?;
    let run_owner = entity_ordinal_at_index(&view, b"run", EntityKind::Function, 0)?;
    let decoy_note = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 0)?;
    let child_note = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 1)?;
    let calls = read_method_calls(&view, run_owner)?;
    if calls.len() != 1 {
        return Err(TestError::Falsified("run owns one MethodCall"));
    }
    assert_local_index(calls[0], child_note, "obj.child.note() resolves to Child.note")?;
    if matches!(
        &calls[0].occurrence.target,
        OccurrenceTarget::Local(target) if target.raw == decoy_note
    ) {
        return Err(TestError::Falsified("obj.child.note() must not resolve to Decoy.note"));
    }
    fs::remove_dir_all(&env.work).map_err(|source| TestError::Io("remove scratch", source))?;
    Ok(())
}

#[test]
fn py_init_attribute_chain_targets_child_note_not_decoy() -> Result<(), TestError> {
    const SOURCE: &[u8] = b"\
class Decoy:
    def note(self):
        return 0

class Child:
    def note(self):
        return 1
    other: Child

class Holder:
    def __init__(self):
        self.child: Child = Child()
    def run(self):
        return self.child.other.note()
";
    let env = inherited_fixture_env()?;
    let toolchain = env.toolchain()?;
    let cancelled = AtomicBool::new(false);
    let fragment = compile_inherited_fragment(SOURCE, &env.work, &toolchain, &cancelled)?;
    let view = fragment.view()?;
    let run_owner = entity_ordinal_at_index(&view, b"run", EntityKind::Function, 0)?;
    let decoy_note = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 0)?;
    let child_note = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 1)?;
    let child_other = entity_ordinal_at_index(&view, b"other", EntityKind::Field, 0)?;
    let calls = read_method_calls(&view, run_owner)?;
    if calls.len() != 1 {
        return Err(TestError::Falsified("run owns one MethodCall"));
    }
    assert_local_index(calls[0], child_note, "self.child.other.note() resolves to Child.note")?;
    if matches!(
        &calls[0].occurrence.target,
        OccurrenceTarget::Local(target) if target.raw == decoy_note
    ) {
        return Err(TestError::Falsified("self.child.other.note() must not resolve to Decoy.note"));
    }
    let child_reads = field_accesses_with_path(&view, run_owner, b"child")?;
    if child_reads.len() != 1 {
        return Err(TestError::Falsified("run owns one FieldAccess with path child"));
    }
    assert_universe_field_named(
        child_reads[0],
        b"child",
        "self.child stays a pypi universe field key",
    )?;
    let other_reads = field_accesses_with_path(&view, run_owner, b"other")?;
    if other_reads.len() != 1 {
        return Err(TestError::Falsified("run owns one FieldAccess with path other"));
    }
    assert_local_index(
        other_reads[0],
        child_other,
        "self.child.other resolves to Child.other",
    )?;
    fs::remove_dir_all(&env.work).map_err(|source| TestError::Io("remove scratch", source))?;
    Ok(())
}

#[test]
fn py_init_attribute_same_method_targets_child_note_not_decoy() -> Result<(), TestError> {
    const SOURCE: &[u8] = b"\
class Decoy:
    def note(self):
        return 0

class Child:
    def note(self):
        return 1

class Holder:
    def run(self):
        self.child: Child = Child()
        return self.child.note()
";
    let env = inherited_fixture_env()?;
    let toolchain = env.toolchain()?;
    let cancelled = AtomicBool::new(false);
    let fragment = compile_inherited_fragment(SOURCE, &env.work, &toolchain, &cancelled)?;
    let view = fragment.view()?;
    let run_owner = entity_ordinal_at_index(&view, b"run", EntityKind::Function, 0)?;
    let decoy_note = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 0)?;
    let child_note = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 1)?;
    let calls = read_method_calls(&view, run_owner)?;
    if calls.len() != 1 {
        return Err(TestError::Falsified("run owns one MethodCall"));
    }
    assert_local_index(calls[0], child_note, "self.child.note() resolves to Child.note")?;
    if matches!(
        &calls[0].occurrence.target,
        OccurrenceTarget::Local(target) if target.raw == decoy_note
    ) {
        return Err(TestError::Falsified("self.child.note() must not resolve to Decoy.note"));
    }
    fs::remove_dir_all(&env.work).map_err(|source| TestError::Io("remove scratch", source))?;
    Ok(())
}

#[test]
fn py_init_attribute_two_methods_agree_targets_child_note_not_decoy() -> Result<(), TestError> {
    const SOURCE: &[u8] = b"\
class Decoy:
    def note(self):
        return 0

class Child:
    def note(self):
        return 1

class Holder:
    def __init__(self):
        self.child: Child = Child()
    def prepare(self):
        self.child: Child = Child()
    def run(self):
        return self.child.note()
";
    let env = inherited_fixture_env()?;
    let toolchain = env.toolchain()?;
    let cancelled = AtomicBool::new(false);
    let fragment = compile_inherited_fragment(SOURCE, &env.work, &toolchain, &cancelled)?;
    let view = fragment.view()?;
    let run_owner = entity_ordinal_at_index(&view, b"run", EntityKind::Function, 0)?;
    let decoy_note = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 0)?;
    let child_note = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 1)?;
    let calls = read_method_calls(&view, run_owner)?;
    if calls.len() != 1 {
        return Err(TestError::Falsified("run owns one MethodCall"));
    }
    assert_local_index(calls[0], child_note, "self.child.note() resolves to Child.note")?;
    if matches!(
        &calls[0].occurrence.target,
        OccurrenceTarget::Local(target) if target.raw == decoy_note
    ) {
        return Err(TestError::Falsified("self.child.note() must not resolve to Decoy.note"));
    }
    fs::remove_dir_all(&env.work).map_err(|source| TestError::Io("remove scratch", source))?;
    Ok(())
}

#[test]
fn py_init_attribute_two_methods_disagree_stay_universe() -> Result<(), TestError> {
    const SOURCE: &[u8] = b"\
class Decoy:
    def note(self):
        return 0

class Child:
    def note(self):
        return 1

class Holder:
    def __init__(self):
        self.child: Child = Child()
    def prepare(self):
        self.child: Decoy = Decoy()
    def run(self):
        return self.child.note()
";
    let env = inherited_fixture_env()?;
    let toolchain = env.toolchain()?;
    let cancelled = AtomicBool::new(false);
    let fragment = compile_inherited_fragment(SOURCE, &env.work, &toolchain, &cancelled)?;
    let view = fragment.view()?;
    let run_owner = entity_ordinal_at_index(&view, b"run", EntityKind::Function, 0)?;
    let decoy_note = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 0)?;
    let child_note = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 1)?;
    let calls = read_method_calls(&view, run_owner)?;
    if calls.len() != 1 {
        return Err(TestError::Falsified("run owns one MethodCall"));
    }
    assert_universe_method(calls[0], "conflicting instance annotations stay a pypi universe key")?;
    if calls[0].occurrence.confidence != OccurrenceConfidence::Index {
        return Err(TestError::Falsified("conflicting instance annotation universe method confidence is Index"));
    }
    if matches!(
        &calls[0].occurrence.target,
        OccurrenceTarget::Local(target) if target.raw == child_note
    ) {
        return Err(TestError::Falsified("self.child.note() must not resolve to Child.note"));
    }
    if matches!(
        &calls[0].occurrence.target,
        OccurrenceTarget::Local(target) if target.raw == decoy_note
    ) {
        return Err(TestError::Falsified("self.child.note() must not resolve to Decoy.note"));
    }
    fs::remove_dir_all(&env.work).map_err(|source| TestError::Io("remove scratch", source))?;
    Ok(())
}

#[test]
fn py_init_attribute_class_field_wins_over_method() -> Result<(), TestError> {
    const SOURCE: &[u8] = b"\
class Decoy:
    def note(self):
        return 0

class Child:
    def note(self):
        return 1

class Holder:
    child: Decoy
    def __init__(self):
        self.child: Child = Child()
    def run(self):
        return self.child.note()
";
    let env = inherited_fixture_env()?;
    let toolchain = env.toolchain()?;
    let cancelled = AtomicBool::new(false);
    let fragment = compile_inherited_fragment(SOURCE, &env.work, &toolchain, &cancelled)?;
    let view = fragment.view()?;
    let run_owner = entity_ordinal_at_index(&view, b"run", EntityKind::Function, 0)?;
    let decoy_note = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 0)?;
    let child_note = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 1)?;
    let calls = read_method_calls(&view, run_owner)?;
    if calls.len() != 1 {
        return Err(TestError::Falsified("run owns one MethodCall"));
    }
    assert_local_index(calls[0], decoy_note, "self.child.note() resolves to Decoy.note")?;
    if matches!(
        &calls[0].occurrence.target,
        OccurrenceTarget::Local(target) if target.raw == child_note
    ) {
        return Err(TestError::Falsified("self.child.note() must not resolve to Child.note"));
    }
    fs::remove_dir_all(&env.work).map_err(|source| TestError::Io("remove scratch", source))?;
    Ok(())
}

#[test]
fn py_init_attribute_unannotated_class_field_stays_universe() -> Result<(), TestError> {
    const SOURCE: &[u8] = b"\
class Decoy:
    def note(self):
        return 0

class Child:
    def note(self):
        return 1

class Holder:
    child = 1
    def run(self):
        self.child: Child = Child()
        return self.child.note()
";
    let env = inherited_fixture_env()?;
    let toolchain = env.toolchain()?;
    let cancelled = AtomicBool::new(false);
    let fragment = compile_inherited_fragment(SOURCE, &env.work, &toolchain, &cancelled)?;
    let view = fragment.view()?;
    let run_owner = entity_ordinal_at_index(&view, b"run", EntityKind::Function, 0)?;
    let child_note = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 1)?;
    let calls = read_method_calls(&view, run_owner)?;
    if calls.len() != 1 {
        return Err(TestError::Falsified("run owns one MethodCall"));
    }
    assert_universe_method(calls[0], "unannotated class-body child stays a pypi universe key")?;
    if calls[0].occurrence.confidence != OccurrenceConfidence::Index {
        return Err(TestError::Falsified("unannotated class-body child universe method confidence is Index"));
    }
    if matches!(
        &calls[0].occurrence.target,
        OccurrenceTarget::Local(target) if target.raw == child_note
    ) {
        return Err(TestError::Falsified("self.child.note() must not resolve to Child.note"));
    }
    fs::remove_dir_all(&env.work).map_err(|source| TestError::Io("remove scratch", source))?;
    Ok(())
}

#[test]
fn py_init_attribute_blocks_inherited_decoy() -> Result<(), TestError> {
    const SOURCE: &[u8] = b"\
class Decoy:
    def note(self):
        return 0

class Child:
    def note(self):
        return 1

class Base:
    child: Decoy

class Holder(Base):
    def run(self):
        self.child: Child = Child()
        return self.child.note()
";
    let env = inherited_fixture_env()?;
    let toolchain = env.toolchain()?;
    let cancelled = AtomicBool::new(false);
    let fragment = compile_inherited_fragment(SOURCE, &env.work, &toolchain, &cancelled)?;
    let view = fragment.view()?;
    let run_owner = entity_ordinal_at_index(&view, b"run", EntityKind::Function, 0)?;
    let decoy_note = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 0)?;
    let child_note = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 1)?;
    let calls = read_method_calls(&view, run_owner)?;
    if calls.len() != 1 {
        return Err(TestError::Falsified("run owns one MethodCall"));
    }
    assert_local_index(calls[0], child_note, "self.child.note() resolves to Child.note")?;
    if matches!(
        &calls[0].occurrence.target,
        OccurrenceTarget::Local(target) if target.raw == decoy_note
    ) {
        return Err(TestError::Falsified("self.child.note() must not resolve to Decoy.note"));
    }
    fs::remove_dir_all(&env.work).map_err(|source| TestError::Io("remove scratch", source))?;
    Ok(())
}

#[test]
fn py_init_attribute_inherited_still_binds_without_instance_annotation() -> Result<(), TestError> {
    const SOURCE: &[u8] = b"\
class Decoy:
    def note(self):
        return 0

class Child:
    def note(self):
        return 1

class Base:
    child: Child

class Holder(Base):
    def run(self):
        return self.child.note()
";
    let env = inherited_fixture_env()?;
    let toolchain = env.toolchain()?;
    let cancelled = AtomicBool::new(false);
    let fragment = compile_inherited_fragment(SOURCE, &env.work, &toolchain, &cancelled)?;
    let view = fragment.view()?;
    let run_owner = entity_ordinal_at_index(&view, b"run", EntityKind::Function, 0)?;
    let decoy_note = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 0)?;
    let child_note = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 1)?;
    let calls = read_method_calls(&view, run_owner)?;
    if calls.len() != 1 {
        return Err(TestError::Falsified("run owns one MethodCall"));
    }
    assert_local_index(calls[0], child_note, "self.child.note() resolves to Child.note")?;
    if matches!(
        &calls[0].occurrence.target,
        OccurrenceTarget::Local(target) if target.raw == decoy_note
    ) {
        return Err(TestError::Falsified("self.child.note() must not resolve to Decoy.note"));
    }
    fs::remove_dir_all(&env.work).map_err(|source| TestError::Io("remove scratch", source))?;
    Ok(())
}

#[test]
fn py_init_attribute_inner_class_does_not_donate() -> Result<(), TestError> {
    const SOURCE: &[u8] = b"\
class Decoy:
    def note(self):
        return 0

class Child:
    def note(self):
        return 1

class Holder:
    def run(self):
        return self.child.note()

    class Inner:
        def run(self):
            self.child: Decoy = Decoy()
            return self.child.note()
";
    let env = inherited_fixture_env()?;
    let toolchain = env.toolchain()?;
    let cancelled = AtomicBool::new(false);
    let fragment = compile_inherited_fragment(SOURCE, &env.work, &toolchain, &cancelled)?;
    let view = fragment.view()?;
    let holder_run = entity_ordinal_at_index(&view, b"run", EntityKind::Function, 0)?;
    let inner_run = entity_ordinal_at_index(&view, b"run", EntityKind::Function, 1)?;
    let decoy_note = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 0)?;
    let child_note = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 1)?;
    let holder_calls = read_method_calls(&view, holder_run)?;
    if holder_calls.len() != 1 {
        return Err(TestError::Falsified("Holder.run owns one MethodCall"));
    }
    assert_universe_method(
        holder_calls[0],
        "Holder.run without instance annotation stays a pypi universe key",
    )?;
    if holder_calls[0].occurrence.confidence != OccurrenceConfidence::Index {
        return Err(TestError::Falsified("Holder.run universe method confidence is Index"));
    }
    if matches!(
        &holder_calls[0].occurrence.target,
        OccurrenceTarget::Local(target) if target.raw == decoy_note
    ) {
        return Err(TestError::Falsified("Holder.run must not resolve to Decoy.note"));
    }
    if matches!(
        &holder_calls[0].occurrence.target,
        OccurrenceTarget::Local(target) if target.raw == child_note
    ) {
        return Err(TestError::Falsified("Holder.run must not resolve to Child.note"));
    }
    let inner_calls = read_method_calls(&view, inner_run)?;
    if inner_calls.len() != 1 {
        return Err(TestError::Falsified("Inner.run owns one MethodCall"));
    }
    assert_local_index(inner_calls[0], decoy_note, "Inner.run self.child.note() resolves to Decoy.note")?;
    if matches!(
        &inner_calls[0].occurrence.target,
        OccurrenceTarget::Local(target) if target.raw == child_note
    ) {
        return Err(TestError::Falsified("Inner.run must not resolve to Child.note"));
    }
    fs::remove_dir_all(&env.work).map_err(|source| TestError::Io("remove scratch", source))?;
    Ok(())
}

#[test]
fn py_init_attribute_generic_call_targets_child_note_not_decoy() -> Result<(), TestError> {
    const SOURCE: &[u8] = b"\
class Decoy:
    def note(self):
        return 0

class Child:
    def note(self):
        return 1

class Holder:
    def __init__(self):
        self.child: Child[int] = Child()
    def run(self):
        return self.child.note()
";
    let env = inherited_fixture_env()?;
    let toolchain = env.toolchain()?;
    let cancelled = AtomicBool::new(false);
    let fragment = compile_inherited_fragment(SOURCE, &env.work, &toolchain, &cancelled)?;
    let view = fragment.view()?;
    let run_owner = entity_ordinal_at_index(&view, b"run", EntityKind::Function, 0)?;
    let decoy_note = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 0)?;
    let child_note = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 1)?;
    let calls = read_method_calls(&view, run_owner)?;
    if calls.len() != 1 {
        return Err(TestError::Falsified("run owns one MethodCall"));
    }
    assert_local_index(calls[0], child_note, "self.child.note() resolves to Child.note")?;
    if matches!(
        &calls[0].occurrence.target,
        OccurrenceTarget::Local(target) if target.raw == decoy_note
    ) {
        return Err(TestError::Falsified("self.child.note() must not resolve to Decoy.note"));
    }
    fs::remove_dir_all(&env.work).map_err(|source| TestError::Io("remove scratch", source))?;
    Ok(())
}

#[test]
fn py_init_attribute_union_call_targets_child_note_not_decoy() -> Result<(), TestError> {
    const SOURCE: &[u8] = b"\
class Decoy:
    def note(self):
        return 0

class Child:
    def note(self):
        return 1

class Holder:
    def __init__(self):
        self.child: Child | None = Child()
    def run(self):
        return self.child.note()
";
    let env = inherited_fixture_env()?;
    let toolchain = env.toolchain()?;
    let cancelled = AtomicBool::new(false);
    let fragment = compile_inherited_fragment(SOURCE, &env.work, &toolchain, &cancelled)?;
    let view = fragment.view()?;
    let run_owner = entity_ordinal_at_index(&view, b"run", EntityKind::Function, 0)?;
    let decoy_note = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 0)?;
    let child_note = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 1)?;
    let calls = read_method_calls(&view, run_owner)?;
    if calls.len() != 1 {
        return Err(TestError::Falsified("run owns one MethodCall"));
    }
    assert_local_index(calls[0], child_note, "self.child.note() resolves to Child.note")?;
    if matches!(
        &calls[0].occurrence.target,
        OccurrenceTarget::Local(target) if target.raw == decoy_note
    ) {
        return Err(TestError::Falsified("self.child.note() must not resolve to Decoy.note"));
    }
    fs::remove_dir_all(&env.work).map_err(|source| TestError::Io("remove scratch", source))?;
    Ok(())
}

#[test]
fn py_init_attribute_quoted_call_targets_child_note_not_decoy() -> Result<(), TestError> {
    const SOURCE: &[u8] = b"\
class Decoy:
    def note(self):
        return 0

class Child:
    def note(self):
        return 1

class Holder:
    def __init__(self):
        self.child: \"Child\" = Child()
    def run(self):
        return self.child.note()
";
    let env = inherited_fixture_env()?;
    let toolchain = env.toolchain()?;
    let cancelled = AtomicBool::new(false);
    let fragment = compile_inherited_fragment(SOURCE, &env.work, &toolchain, &cancelled)?;
    let view = fragment.view()?;
    let run_owner = entity_ordinal_at_index(&view, b"run", EntityKind::Function, 0)?;
    let decoy_note = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 0)?;
    let child_note = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 1)?;
    let calls = read_method_calls(&view, run_owner)?;
    if calls.len() != 1 {
        return Err(TestError::Falsified("run owns one MethodCall"));
    }
    assert_local_index(calls[0], child_note, "self.child.note() resolves to Child.note")?;
    if matches!(
        &calls[0].occurrence.target,
        OccurrenceTarget::Local(target) if target.raw == decoy_note
    ) {
        return Err(TestError::Falsified("self.child.note() must not resolve to Decoy.note"));
    }
    fs::remove_dir_all(&env.work).map_err(|source| TestError::Io("remove scratch", source))?;
    Ok(())
}

#[test]
fn py_init_attribute_list_read_does_not_take_module_field() -> Result<(), TestError> {
    const SOURCE: &[u8] = b"\
class Child:
    def note(self):
        return 1

class Holder:
    note = 1
    def __init__(self):
        self.child: list[Child] = []
    def run(self):
        return self.child.note
";
    let env = inherited_fixture_env()?;
    let toolchain = env.toolchain()?;
    let cancelled = AtomicBool::new(false);
    let fragment = compile_inherited_fragment(SOURCE, &env.work, &toolchain, &cancelled)?;
    let view = fragment.view()?;
    let run_owner = entity_ordinal_at_index(&view, b"run", EntityKind::Function, 0)?;
    let holder_note = entity_ordinal_at_index(&view, b"note", EntityKind::Field, 0)?;
    let child_note = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 0)?;
    let calls = read_method_calls(&view, run_owner)?;
    if !calls.is_empty() {
        return Err(TestError::Falsified("run owns zero MethodCalls"));
    }
    let reads = field_accesses_with_path(&view, run_owner, b"note")?;
    if reads.len() != 1 {
        return Err(TestError::Falsified("run owns one FieldAccess with path note"));
    }
    assert_universe_field(reads[0], "list[Child] read stays a pypi universe field key")?;
    if reads[0].occurrence.confidence != OccurrenceConfidence::Index {
        return Err(TestError::Falsified("list[Child] universe field confidence is Index"));
    }
    if matches!(
        &reads[0].occurrence.target,
        OccurrenceTarget::Local(target) if target.raw == holder_note
    ) {
        return Err(TestError::Falsified("self.child.note must not resolve to Holder.note"));
    }
    if matches!(
        &reads[0].occurrence.target,
        OccurrenceTarget::Local(target) if target.raw == child_note
    ) {
        return Err(TestError::Falsified("self.child.note must not resolve to Child.note"));
    }
    fs::remove_dir_all(&env.work).map_err(|source| TestError::Io("remove scratch", source))?;
    Ok(())
}

#[test]
fn py_init_attribute_dotted_stays_universe() -> Result<(), TestError> {
    const SOURCE: &[u8] = b"\
import pkg

class Decoy:
    def note(self):
        return 0

class Child:
    def note(self):
        return 1

class Holder:
    def __init__(self):
        self.child: pkg.Child = Child()
    def run(self):
        return self.child.note
";
    let env = inherited_fixture_env()?;
    let toolchain = env.toolchain()?;
    let cancelled = AtomicBool::new(false);
    let fragment = compile_inherited_fragment(SOURCE, &env.work, &toolchain, &cancelled)?;
    let view = fragment.view()?;
    let run_owner = entity_ordinal_at_index(&view, b"run", EntityKind::Function, 0)?;
    let child_note = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 1)?;
    let calls = read_method_calls(&view, run_owner)?;
    if !calls.is_empty() {
        return Err(TestError::Falsified("run owns zero MethodCalls"));
    }
    let reads = field_accesses_with_path(&view, run_owner, b"note")?;
    if reads.len() != 1 {
        return Err(TestError::Falsified("run owns one FieldAccess with path note"));
    }
    assert_universe_field(reads[0], "pkg.Child read stays a pypi universe field key")?;
    if reads[0].occurrence.confidence != OccurrenceConfidence::Index {
        return Err(TestError::Falsified("pkg.Child universe read confidence is Index"));
    }
    if matches!(
        &reads[0].occurrence.target,
        OccurrenceTarget::Local(target) if target.raw == child_note
    ) {
        return Err(TestError::Falsified("self.child.note must not resolve to Child.note"));
    }
    fs::remove_dir_all(&env.work).map_err(|source| TestError::Io("remove scratch", source))?;
    Ok(())
}

#[test]
fn py_init_attribute_missing_member_stays_attribute_key() -> Result<(), TestError> {
    const SOURCE: &[u8] = b"\
class Decoy:
    def note(self):
        return 0

class Child:
    pass

class Holder:
    def __init__(self):
        self.child: Child = Child()
    def run(self):
        return self.child.note()
";
    let env = inherited_fixture_env()?;
    let toolchain = env.toolchain()?;
    let cancelled = AtomicBool::new(false);
    let fragment = compile_inherited_fragment(SOURCE, &env.work, &toolchain, &cancelled)?;
    let view = fragment.view()?;
    let run_owner = entity_ordinal_at_index(&view, b"run", EntityKind::Function, 0)?;
    let decoy_note = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 0)?;
    let calls = read_method_calls(&view, run_owner)?;
    if calls.len() != 1 {
        return Err(TestError::Falsified("run owns one MethodCall"));
    }
    assert_universe_method(calls[0], "Child without note stays a pypi universe key")?;
    if calls[0].occurrence.confidence != OccurrenceConfidence::Index {
        return Err(TestError::Falsified("missing member universe method confidence is Index"));
    }
    if matches!(
        &calls[0].occurrence.target,
        OccurrenceTarget::Local(target) if target.raw == decoy_note
    ) {
        return Err(TestError::Falsified("self.child.note() must not resolve to Decoy.note"));
    }
    fs::remove_dir_all(&env.work).map_err(|source| TestError::Io("remove scratch", source))?;
    Ok(())
}

#[test]
fn py_init_attribute_field_wins_over_method() -> Result<(), TestError> {
    const SOURCE: &[u8] = b"\
class Child:
    score = 1

    def score(self):
        return self.score

class Holder:
    def __init__(self):
        self.child: Child = Child()
    def run(self):
        return self.child.score
";
    let env = inherited_fixture_env()?;
    let toolchain = env.toolchain()?;
    let cancelled = AtomicBool::new(false);
    let fragment = compile_inherited_fragment(SOURCE, &env.work, &toolchain, &cancelled)?;
    let view = fragment.view()?;
    let run_owner = entity_ordinal_at_index(&view, b"run", EntityKind::Function, 0)?;
    let score_field = entity_ordinal_at_index(&view, b"score", EntityKind::Field, 0)?;
    let score_method = entity_ordinal_at_index(&view, b"score", EntityKind::Function, 0)?;
    let calls = read_method_calls(&view, run_owner)?;
    if !calls.is_empty() {
        return Err(TestError::Falsified("run owns zero MethodCalls"));
    }
    let reads = field_accesses_with_path(&view, run_owner, b"score")?;
    if reads.len() != 1 {
        return Err(TestError::Falsified("run owns one FieldAccess with path score"));
    }
    assert_local_index(reads[0], score_field, "self.child.score resolves to Child.score field")?;
    if matches!(
        &reads[0].occurrence.target,
        OccurrenceTarget::Local(target) if target.raw == score_method
    ) {
        return Err(TestError::Falsified("self.child.score must not resolve to Child.score method"));
    }
    fs::remove_dir_all(&env.work).map_err(|source| TestError::Io("remove scratch", source))?;
    Ok(())
}

#[test]
fn py_init_attribute_call_binds_method_not_field() -> Result<(), TestError> {
    const SOURCE: &[u8] = b"\
class Child:
    note = 1

    def note(self):
        return self.note

class Holder:
    def __init__(self):
        self.child: Child = Child()
    def run(self):
        return self.child.note()
";
    let env = inherited_fixture_env()?;
    let toolchain = env.toolchain()?;
    let cancelled = AtomicBool::new(false);
    let fragment = compile_inherited_fragment(SOURCE, &env.work, &toolchain, &cancelled)?;
    let view = fragment.view()?;
    let run_owner = entity_ordinal_at_index(&view, b"run", EntityKind::Function, 0)?;
    let note_field = entity_ordinal_at_index(&view, b"note", EntityKind::Field, 0)?;
    let note_method = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 0)?;
    let calls = read_method_calls(&view, run_owner)?;
    if calls.len() != 1 {
        return Err(TestError::Falsified("run owns one MethodCall"));
    }
    assert_local_index(calls[0], note_method, "self.child.note() resolves to Child.note method")?;
    if matches!(
        &calls[0].occurrence.target,
        OccurrenceTarget::Local(target) if target.raw == note_field
    ) {
        return Err(TestError::Falsified("self.child.note() must not resolve to Child.note field"));
    }
    fs::remove_dir_all(&env.work).map_err(|source| TestError::Io("remove scratch", source))?;
    Ok(())
}

#[test]
fn py_init_attribute_classmethod_targets_child_note_not_decoy() -> Result<(), TestError> {
    const SOURCE: &[u8] = b"\
class Decoy:
    def note(self):
        return 0

class Child:
    def note(self):
        return 1

class Holder:
    @classmethod
    def build(cls):
        cls.child: Child = Child()
    def run(self):
        return self.child.note()
";
    let env = inherited_fixture_env()?;
    let toolchain = env.toolchain()?;
    let cancelled = AtomicBool::new(false);
    let fragment = compile_inherited_fragment(SOURCE, &env.work, &toolchain, &cancelled)?;
    let view = fragment.view()?;
    let run_owner = entity_ordinal_at_index(&view, b"run", EntityKind::Function, 0)?;
    let decoy_note = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 0)?;
    let child_note = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 1)?;
    let calls = read_method_calls(&view, run_owner)?;
    if calls.len() != 1 {
        return Err(TestError::Falsified("run owns one MethodCall"));
    }
    assert_local_index(calls[0], child_note, "self.child.note() resolves to Child.note")?;
    if matches!(
        &calls[0].occurrence.target,
        OccurrenceTarget::Local(target) if target.raw == decoy_note
    ) {
        return Err(TestError::Falsified("self.child.note() must not resolve to Decoy.note"));
    }
    fs::remove_dir_all(&env.work).map_err(|source| TestError::Io("remove scratch", source))?;
    Ok(())
}

#[test]
fn py_init_attribute_nested_class_field_does_not_shadow() -> Result<(), TestError> {
    const SOURCE: &[u8] = b"\
class Decoy:
    def note(self):
        return 0

class Child:
    def note(self):
        return 1

class Holder:
    def __init__(self):
        self.child: Child = Child()
    def run(self):
        return self.child.note()
    class Inner:
        child: Decoy
";
    let env = inherited_fixture_env()?;
    let toolchain = env.toolchain()?;
    let cancelled = AtomicBool::new(false);
    let fragment = compile_inherited_fragment(SOURCE, &env.work, &toolchain, &cancelled)?;
    let view = fragment.view()?;
    let run_owner = entity_ordinal_at_index(&view, b"run", EntityKind::Function, 0)?;
    let decoy_note = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 0)?;
    let child_note = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 1)?;
    let calls = read_method_calls(&view, run_owner)?;
    if calls.len() != 1 {
        return Err(TestError::Falsified("run owns one MethodCall"));
    }
    assert_local_index(calls[0], child_note, "self.child.note() resolves to Child.note")?;
    if matches!(
        &calls[0].occurrence.target,
        OccurrenceTarget::Local(target) if target.raw == decoy_note
    ) {
        return Err(TestError::Falsified("self.child.note() must not resolve to Decoy.note"));
    }
    fs::remove_dir_all(&env.work).map_err(|source| TestError::Io("remove scratch", source))?;
    Ok(())
}

#[test]
fn py_init_attribute_nested_unannotated_field_does_not_shadow() -> Result<(), TestError> {
    const SOURCE: &[u8] = b"\
class Decoy:
    def note(self):
        return 0

class Child:
    def note(self):
        return 1

class Holder:
    def __init__(self):
        self.child: Child = Child()
    def run(self):
        return self.child.note()
    class Inner:
        child = 1
";
    let env = inherited_fixture_env()?;
    let toolchain = env.toolchain()?;
    let cancelled = AtomicBool::new(false);
    let fragment = compile_inherited_fragment(SOURCE, &env.work, &toolchain, &cancelled)?;
    let view = fragment.view()?;
    let run_owner = entity_ordinal_at_index(&view, b"run", EntityKind::Function, 0)?;
    let decoy_note = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 0)?;
    let child_note = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 1)?;
    let calls = read_method_calls(&view, run_owner)?;
    if calls.len() != 1 {
        return Err(TestError::Falsified("run owns one MethodCall"));
    }
    assert_local_index(calls[0], child_note, "self.child.note() resolves to Child.note")?;
    if matches!(
        &calls[0].occurrence.target,
        OccurrenceTarget::Local(target) if target.raw == decoy_note
    ) {
        return Err(TestError::Falsified("self.child.note() must not resolve to Decoy.note"));
    }
    fs::remove_dir_all(&env.work).map_err(|source| TestError::Io("remove scratch", source))?;
    Ok(())
}

#[test]
fn py_call_return_self_call_targets_child_extra_not_decoy() -> Result<(), TestError> {
    const SOURCE: &[u8] = b"\
class Decoy:
    def extra(self):
        return 0

class Child:
    def extra(self):
        return 1

class Holder:
    def note(self) -> Child:
        return Child()
    def run(self):
        return self.note().extra()
";
    let env = inherited_fixture_env()?;
    let toolchain = env.toolchain()?;
    let cancelled = AtomicBool::new(false);
    let fragment = compile_inherited_fragment(SOURCE, &env.work, &toolchain, &cancelled)?;
    let view = fragment.view()?;
    let run_owner = entity_ordinal_at_index(&view, b"run", EntityKind::Function, 0)?;
    let decoy_extra = entity_ordinal_at_index(&view, b"extra", EntityKind::Function, 0)?;
    let child_extra = entity_ordinal_at_index(&view, b"extra", EntityKind::Function, 1)?;
    let holder_note = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 0)?;
    let extra_calls = method_calls_with_path(&view, run_owner, b"extra")?;
    if extra_calls.len() != 1 {
        return Err(TestError::Falsified("run owns one MethodCall with path extra"));
    }
    assert_local_index(extra_calls[0], child_extra, "self.note().extra() resolves to Child.extra")?;
    if matches!(
        &extra_calls[0].occurrence.target,
        OccurrenceTarget::Local(target) if target.raw == decoy_extra
    ) {
        return Err(TestError::Falsified("self.note().extra() must not resolve to Decoy.extra"));
    }
    let note_calls = method_calls_with_path(&view, run_owner, b"note")?;
    if note_calls.len() != 1 {
        return Err(TestError::Falsified("run owns one MethodCall with path note"));
    }
    assert_local_index(note_calls[0], holder_note, "self.note() resolves to Holder.note")?;
    fs::remove_dir_all(&env.work).map_err(|source| TestError::Io("remove scratch", source))?;
    Ok(())
}

#[test]
fn py_call_return_named_call_targets_child_extra_not_decoy() -> Result<(), TestError> {
    const SOURCE: &[u8] = b"\
class Decoy:
    def extra(self):
        return 0

class Child:
    def extra(self):
        return 1

class Holder:
    def note(self) -> Child:
        return Child()
    def run(obj: Holder):
        return obj.note().extra()
";
    let env = inherited_fixture_env()?;
    let toolchain = env.toolchain()?;
    let cancelled = AtomicBool::new(false);
    let fragment = compile_inherited_fragment(SOURCE, &env.work, &toolchain, &cancelled)?;
    let view = fragment.view()?;
    let run_owner = entity_ordinal_at_index(&view, b"run", EntityKind::Function, 0)?;
    let decoy_extra = entity_ordinal_at_index(&view, b"extra", EntityKind::Function, 0)?;
    let child_extra = entity_ordinal_at_index(&view, b"extra", EntityKind::Function, 1)?;
    let holder_note = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 0)?;
    let extra_calls = method_calls_with_path(&view, run_owner, b"extra")?;
    if extra_calls.len() != 1 {
        return Err(TestError::Falsified("run owns one MethodCall with path extra"));
    }
    assert_local_index(extra_calls[0], child_extra, "obj.note().extra() resolves to Child.extra")?;
    if matches!(
        &extra_calls[0].occurrence.target,
        OccurrenceTarget::Local(target) if target.raw == decoy_extra
    ) {
        return Err(TestError::Falsified("obj.note().extra() must not resolve to Decoy.extra"));
    }
    let note_calls = method_calls_with_path(&view, run_owner, b"note")?;
    if note_calls.len() != 1 {
        return Err(TestError::Falsified("run owns one MethodCall with path note"));
    }
    assert_local_index(note_calls[0], holder_note, "obj.note() resolves to Holder.note")?;
    fs::remove_dir_all(&env.work).map_err(|source| TestError::Io("remove scratch", source))?;
    Ok(())
}

#[test]
fn py_call_return_bare_call_targets_child_extra_not_decoy() -> Result<(), TestError> {
    const SOURCE: &[u8] = b"\
class Decoy:
    def extra(self):
        return 0

class Child:
    def extra(self):
        return 1

def note() -> Child:
    return Child()

def run():
    return note().extra()
";
    let env = inherited_fixture_env()?;
    let toolchain = env.toolchain()?;
    let cancelled = AtomicBool::new(false);
    let fragment = compile_inherited_fragment(SOURCE, &env.work, &toolchain, &cancelled)?;
    let view = fragment.view()?;
    let run_owner = entity_ordinal_at_index(&view, b"run", EntityKind::Function, 0)?;
    let decoy_extra = entity_ordinal_at_index(&view, b"extra", EntityKind::Function, 0)?;
    let child_extra = entity_ordinal_at_index(&view, b"extra", EntityKind::Function, 1)?;
    let extra_calls = method_calls_with_path(&view, run_owner, b"extra")?;
    if extra_calls.len() != 1 {
        return Err(TestError::Falsified("run owns one MethodCall with path extra"));
    }
    assert_local_index(extra_calls[0], child_extra, "note().extra() resolves to Child.extra")?;
    if matches!(
        &extra_calls[0].occurrence.target,
        OccurrenceTarget::Local(target) if target.raw == decoy_extra
    ) {
        return Err(TestError::Falsified("note().extra() must not resolve to Decoy.extra"));
    }
    fs::remove_dir_all(&env.work).map_err(|source| TestError::Io("remove scratch", source))?;
    Ok(())
}

#[test]
fn py_call_return_closure_targets_child_extra_not_decoy() -> Result<(), TestError> {
    const SOURCE: &[u8] = b"\
class Decoy:
    def extra(self):
        return 0

class Child:
    def extra(self):
        return 1

class Holder:
    def note(self) -> Child:
        return Child()

def outer(obj: Holder):
    def inner():
        return obj.note().extra()
";
    let env = inherited_fixture_env()?;
    let toolchain = env.toolchain()?;
    let cancelled = AtomicBool::new(false);
    let fragment = compile_inherited_fragment(SOURCE, &env.work, &toolchain, &cancelled)?;
    let view = fragment.view()?;
    let inner_owner = entity_ordinal_at_index(&view, b"inner", EntityKind::Function, 0)?;
    let decoy_extra = entity_ordinal_at_index(&view, b"extra", EntityKind::Function, 0)?;
    let child_extra = entity_ordinal_at_index(&view, b"extra", EntityKind::Function, 1)?;
    let holder_note = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 0)?;
    let extra_calls = method_calls_with_path(&view, inner_owner, b"extra")?;
    if extra_calls.len() != 1 {
        return Err(TestError::Falsified("inner owns one MethodCall with path extra"));
    }
    assert_local_index(extra_calls[0], child_extra, "obj.note().extra() resolves to Child.extra")?;
    if matches!(
        &extra_calls[0].occurrence.target,
        OccurrenceTarget::Local(target) if target.raw == decoy_extra
    ) {
        return Err(TestError::Falsified("obj.note().extra() must not resolve to Decoy.extra"));
    }
    let note_calls = method_calls_with_path(&view, inner_owner, b"note")?;
    if note_calls.len() != 1 {
        return Err(TestError::Falsified("inner owns one MethodCall with path note"));
    }
    assert_local_index(note_calls[0], holder_note, "obj.note() resolves to Holder.note")?;
    fs::remove_dir_all(&env.work).map_err(|source| TestError::Io("remove scratch", source))?;
    Ok(())
}

#[test]
fn py_call_return_inherited_method_targets_child_extra_not_decoy() -> Result<(), TestError> {
    const SOURCE: &[u8] = b"\
class Decoy:
    def extra(self):
        return 0

class Child:
    def extra(self):
        return 1

class Base:
    def note(self) -> Child:
        return Child()

class Holder(Base):
    def run(self):
        return self.note().extra()
";
    let env = inherited_fixture_env()?;
    let toolchain = env.toolchain()?;
    let cancelled = AtomicBool::new(false);
    let fragment = compile_inherited_fragment(SOURCE, &env.work, &toolchain, &cancelled)?;
    let view = fragment.view()?;
    let run_owner = entity_ordinal_at_index(&view, b"run", EntityKind::Function, 0)?;
    let decoy_extra = entity_ordinal_at_index(&view, b"extra", EntityKind::Function, 0)?;
    let child_extra = entity_ordinal_at_index(&view, b"extra", EntityKind::Function, 1)?;
    let base_note = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 0)?;
    let extra_calls = method_calls_with_path(&view, run_owner, b"extra")?;
    if extra_calls.len() != 1 {
        return Err(TestError::Falsified("run owns one MethodCall with path extra"));
    }
    assert_local_index(extra_calls[0], child_extra, "self.note().extra() resolves to Child.extra")?;
    if matches!(
        &extra_calls[0].occurrence.target,
        OccurrenceTarget::Local(target) if target.raw == decoy_extra
    ) {
        return Err(TestError::Falsified("self.note().extra() must not resolve to Decoy.extra"));
    }
    let note_calls = method_calls_with_path(&view, run_owner, b"note")?;
    if note_calls.len() != 1 {
        return Err(TestError::Falsified("run owns one MethodCall with path note"));
    }
    assert_local_index(note_calls[0], base_note, "self.note() resolves to Base.note")?;
    fs::remove_dir_all(&env.work).map_err(|source| TestError::Io("remove scratch", source))?;
    Ok(())
}

#[test]
fn py_call_return_generic_targets_child_extra_not_decoy() -> Result<(), TestError> {
    const SOURCE: &[u8] = b"\
class Decoy:
    def extra(self):
        return 0

class Child:
    def extra(self):
        return 1

class Holder:
    def note(self) -> Child[int]:
        return Child()
    def run(self):
        return self.note().extra()
";
    let env = inherited_fixture_env()?;
    let toolchain = env.toolchain()?;
    let cancelled = AtomicBool::new(false);
    let fragment = compile_inherited_fragment(SOURCE, &env.work, &toolchain, &cancelled)?;
    let view = fragment.view()?;
    let run_owner = entity_ordinal_at_index(&view, b"run", EntityKind::Function, 0)?;
    let decoy_extra = entity_ordinal_at_index(&view, b"extra", EntityKind::Function, 0)?;
    let child_extra = entity_ordinal_at_index(&view, b"extra", EntityKind::Function, 1)?;
    let extra_calls = method_calls_with_path(&view, run_owner, b"extra")?;
    if extra_calls.len() != 1 {
        return Err(TestError::Falsified("run owns one MethodCall with path extra"));
    }
    assert_local_index(extra_calls[0], child_extra, "self.note().extra() resolves to Child.extra")?;
    if matches!(
        &extra_calls[0].occurrence.target,
        OccurrenceTarget::Local(target) if target.raw == decoy_extra
    ) {
        return Err(TestError::Falsified("self.note().extra() must not resolve to Decoy.extra"));
    }
    fs::remove_dir_all(&env.work).map_err(|source| TestError::Io("remove scratch", source))?;
    Ok(())
}

#[test]
fn py_call_return_union_targets_child_extra_not_decoy() -> Result<(), TestError> {
    const SOURCE: &[u8] = b"\
class Decoy:
    def extra(self):
        return 0

class Child:
    def extra(self):
        return 1

class Holder:
    def note(self) -> Child | None:
        return Child()
    def run(self):
        return self.note().extra()
";
    let env = inherited_fixture_env()?;
    let toolchain = env.toolchain()?;
    let cancelled = AtomicBool::new(false);
    let fragment = compile_inherited_fragment(SOURCE, &env.work, &toolchain, &cancelled)?;
    let view = fragment.view()?;
    let run_owner = entity_ordinal_at_index(&view, b"run", EntityKind::Function, 0)?;
    let decoy_extra = entity_ordinal_at_index(&view, b"extra", EntityKind::Function, 0)?;
    let child_extra = entity_ordinal_at_index(&view, b"extra", EntityKind::Function, 1)?;
    let extra_calls = method_calls_with_path(&view, run_owner, b"extra")?;
    if extra_calls.len() != 1 {
        return Err(TestError::Falsified("run owns one MethodCall with path extra"));
    }
    assert_local_index(extra_calls[0], child_extra, "self.note().extra() resolves to Child.extra")?;
    if matches!(
        &extra_calls[0].occurrence.target,
        OccurrenceTarget::Local(target) if target.raw == decoy_extra
    ) {
        return Err(TestError::Falsified("self.note().extra() must not resolve to Decoy.extra"));
    }
    fs::remove_dir_all(&env.work).map_err(|source| TestError::Io("remove scratch", source))?;
    Ok(())
}

#[test]
fn py_call_return_quoted_targets_child_extra_not_decoy() -> Result<(), TestError> {
    const SOURCE: &[u8] = b"\
class Decoy:
    def extra(self):
        return 0

class Child:
    def extra(self):
        return 1

class Holder:
    def note(self) -> \"Child\":
        return Child()
    def run(self):
        return self.note().extra()
";
    let env = inherited_fixture_env()?;
    let toolchain = env.toolchain()?;
    let cancelled = AtomicBool::new(false);
    let fragment = compile_inherited_fragment(SOURCE, &env.work, &toolchain, &cancelled)?;
    let view = fragment.view()?;
    let run_owner = entity_ordinal_at_index(&view, b"run", EntityKind::Function, 0)?;
    let decoy_extra = entity_ordinal_at_index(&view, b"extra", EntityKind::Function, 0)?;
    let child_extra = entity_ordinal_at_index(&view, b"extra", EntityKind::Function, 1)?;
    let extra_calls = method_calls_with_path(&view, run_owner, b"extra")?;
    if extra_calls.len() != 1 {
        return Err(TestError::Falsified("run owns one MethodCall with path extra"));
    }
    assert_local_index(extra_calls[0], child_extra, "self.note().extra() resolves to Child.extra")?;
    if matches!(
        &extra_calls[0].occurrence.target,
        OccurrenceTarget::Local(target) if target.raw == decoy_extra
    ) {
        return Err(TestError::Falsified("self.note().extra() must not resolve to Decoy.extra"));
    }
    fs::remove_dir_all(&env.work).map_err(|source| TestError::Io("remove scratch", source))?;
    Ok(())
}

#[test]
fn py_call_return_missing_return_stays_universe() -> Result<(), TestError> {
    const SOURCE: &[u8] = b"\
class Decoy:
    def extra(self):
        return 0

class Child:
    def extra(self):
        return 1

class Holder:
    def note(self):
        return Child()
    def run(self):
        return self.note().extra()
";
    let env = inherited_fixture_env()?;
    let toolchain = env.toolchain()?;
    let cancelled = AtomicBool::new(false);
    let fragment = compile_inherited_fragment(SOURCE, &env.work, &toolchain, &cancelled)?;
    let view = fragment.view()?;
    let run_owner = entity_ordinal_at_index(&view, b"run", EntityKind::Function, 0)?;
    let child_extra = entity_ordinal_at_index(&view, b"extra", EntityKind::Function, 1)?;
    let extra_calls = method_calls_with_path(&view, run_owner, b"extra")?;
    if extra_calls.len() != 1 {
        return Err(TestError::Falsified("run owns one MethodCall with path extra"));
    }
    assert_universe_method_named(
        extra_calls[0],
        b"extra",
        "missing return annotation stays a pypi universe key",
    )?;
    if matches!(
        &extra_calls[0].occurrence.target,
        OccurrenceTarget::Local(target) if target.raw == child_extra
    ) {
        return Err(TestError::Falsified("self.note().extra() must not resolve to Child.extra"));
    }
    fs::remove_dir_all(&env.work).map_err(|source| TestError::Io("remove scratch", source))?;
    Ok(())
}

#[test]
fn py_call_return_equality_does_not_shadow() -> Result<(), TestError> {
    const SOURCE: &[u8] = b"\
class Decoy:
    def extra(self):
        return 0

class Child:
    def extra(self):
        return 1

class Holder:
    def note(self) -> Child:
        return Child()
    def run(self, obj: Holder):
        if obj == 1:
            return 0
        return obj.note().extra()
";
    let env = inherited_fixture_env()?;
    let toolchain = env.toolchain()?;
    let cancelled = AtomicBool::new(false);
    let fragment = compile_inherited_fragment(SOURCE, &env.work, &toolchain, &cancelled)?;
    let view = fragment.view()?;
    let run_owner = entity_ordinal_at_index(&view, b"run", EntityKind::Function, 0)?;
    let decoy_extra = entity_ordinal_at_index(&view, b"extra", EntityKind::Function, 0)?;
    let child_extra = entity_ordinal_at_index(&view, b"extra", EntityKind::Function, 1)?;
    let holder_note = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 0)?;
    let extra_calls = method_calls_with_path(&view, run_owner, b"extra")?;
    if extra_calls.len() != 1 {
        return Err(TestError::Falsified("run owns one MethodCall with path extra"));
    }
    assert_local_index(extra_calls[0], child_extra, "obj.note().extra() resolves to Child.extra")?;
    if matches!(
        &extra_calls[0].occurrence.target,
        OccurrenceTarget::Local(target) if target.raw == decoy_extra
    ) {
        return Err(TestError::Falsified("obj.note().extra() must not resolve to Decoy.extra"));
    }
    let note_calls = method_calls_with_path(&view, run_owner, b"note")?;
    if note_calls.len() != 1 {
        return Err(TestError::Falsified("run owns one MethodCall with path note"));
    }
    assert_local_index(note_calls[0], holder_note, "obj.note() resolves to Holder.note")?;
    fs::remove_dir_all(&env.work).map_err(|source| TestError::Io("remove scratch", source))?;
    Ok(())
}

#[test]
fn py_call_return_prefix_assignment_does_not_shadow() -> Result<(), TestError> {
    const SOURCE: &[u8] = b"\
class Decoy:
    def extra(self):
        return 0

class Child:
    def extra(self):
        return 1

class Holder:
    def note(self) -> Child:
        return Child()
    def run(self, obj: Holder):
        myobj = 1
        return obj.note().extra()
";
    let env = inherited_fixture_env()?;
    let toolchain = env.toolchain()?;
    let cancelled = AtomicBool::new(false);
    let fragment = compile_inherited_fragment(SOURCE, &env.work, &toolchain, &cancelled)?;
    let view = fragment.view()?;
    let run_owner = entity_ordinal_at_index(&view, b"run", EntityKind::Function, 0)?;
    let decoy_extra = entity_ordinal_at_index(&view, b"extra", EntityKind::Function, 0)?;
    let child_extra = entity_ordinal_at_index(&view, b"extra", EntityKind::Function, 1)?;
    let holder_note = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 0)?;
    let extra_calls = method_calls_with_path(&view, run_owner, b"extra")?;
    if extra_calls.len() != 1 {
        return Err(TestError::Falsified("run owns one MethodCall with path extra"));
    }
    assert_local_index(extra_calls[0], child_extra, "obj.note().extra() resolves to Child.extra")?;
    if matches!(
        &extra_calls[0].occurrence.target,
        OccurrenceTarget::Local(target) if target.raw == decoy_extra
    ) {
        return Err(TestError::Falsified("obj.note().extra() must not resolve to Decoy.extra"));
    }
    let note_calls = method_calls_with_path(&view, run_owner, b"note")?;
    if note_calls.len() != 1 {
        return Err(TestError::Falsified("run owns one MethodCall with path note"));
    }
    assert_local_index(note_calls[0], holder_note, "obj.note() resolves to Holder.note")?;
    fs::remove_dir_all(&env.work).map_err(|source| TestError::Io("remove scratch", source))?;
    Ok(())
}

#[test]
fn py_call_return_local_shadow_stays_universe() -> Result<(), TestError> {
    const SOURCE: &[u8] = b"\
class Decoy:
    def extra(self):
        return 0

class Child:
    def extra(self):
        return 1

class Holder:
    def note(self) -> Child:
        return Child()
    def run(self, obj: Holder):
        obj = 1
        return obj.note().extra()
";
    let env = inherited_fixture_env()?;
    let toolchain = env.toolchain()?;
    let cancelled = AtomicBool::new(false);
    let fragment = compile_inherited_fragment(SOURCE, &env.work, &toolchain, &cancelled)?;
    let view = fragment.view()?;
    let run_owner = entity_ordinal_at_index(&view, b"run", EntityKind::Function, 0)?;
    let child_extra = entity_ordinal_at_index(&view, b"extra", EntityKind::Function, 1)?;
    let extra_calls = method_calls_with_path(&view, run_owner, b"extra")?;
    if extra_calls.len() != 1 {
        return Err(TestError::Falsified("run owns one MethodCall with path extra"));
    }
    assert_universe_method_named(
        extra_calls[0],
        b"extra",
        "shadowed parameter stays a pypi universe key",
    )?;
    if matches!(
        &extra_calls[0].occurrence.target,
        OccurrenceTarget::Local(target) if target.raw == child_extra
    ) {
        return Err(TestError::Falsified("obj.note().extra() must not resolve to Child.extra"));
    }
    fs::remove_dir_all(&env.work).map_err(|source| TestError::Io("remove scratch", source))?;
    Ok(())
}

#[test]
fn py_call_return_two_bases_same_return_stay_universe() -> Result<(), TestError> {
    const SOURCE: &[u8] = b"\
class Decoy:
    def extra(self):
        return 0

class Child:
    def extra(self):
        return 1

class Left:
    def note(self) -> Child:
        return Child()

class Right:
    def note(self) -> Child:
        return Child()

class Holder(Left, Right):
    def run(self):
        return self.note().extra()
";
    let env = inherited_fixture_env()?;
    let toolchain = env.toolchain()?;
    let cancelled = AtomicBool::new(false);
    let fragment = compile_inherited_fragment(SOURCE, &env.work, &toolchain, &cancelled)?;
    let view = fragment.view()?;
    let run_owner = entity_ordinal_at_index(&view, b"run", EntityKind::Function, 0)?;
    let decoy_extra = entity_ordinal_at_index(&view, b"extra", EntityKind::Function, 0)?;
    let child_extra = entity_ordinal_at_index(&view, b"extra", EntityKind::Function, 1)?;
    let extra_calls = method_calls_with_path(&view, run_owner, b"extra")?;
    if extra_calls.len() != 1 {
        return Err(TestError::Falsified("run owns one MethodCall with path extra"));
    }
    assert_universe_method_named(
        extra_calls[0],
        b"extra",
        "two inherited note methods stay a pypi universe key",
    )?;
    if matches!(
        &extra_calls[0].occurrence.target,
        OccurrenceTarget::Local(target) if target.raw == child_extra
    ) {
        return Err(TestError::Falsified("self.note().extra() must not resolve to Child.extra"));
    }
    if matches!(
        &extra_calls[0].occurrence.target,
        OccurrenceTarget::Local(target) if target.raw == decoy_extra
    ) {
        return Err(TestError::Falsified("self.note().extra() must not resolve to Decoy.extra"));
    }
    fs::remove_dir_all(&env.work).map_err(|source| TestError::Io("remove scratch", source))?;
    Ok(())
}

#[test]
fn py_call_return_diamond_targets_child_extra_not_decoy() -> Result<(), TestError> {
    const SOURCE: &[u8] = b"\
class Decoy:
    def extra(self):
        return 0

class Child:
    def extra(self):
        return 1

class Base:
    def note(self) -> Child:
        return Child()

class Left(Base):
    pass

class Right(Base):
    pass

class Mid(Left, Right):
    pass

class Holder(Mid):
    def run(self):
        return self.note().extra()
";
    let env = inherited_fixture_env()?;
    let toolchain = env.toolchain()?;
    let cancelled = AtomicBool::new(false);
    let fragment = compile_inherited_fragment(SOURCE, &env.work, &toolchain, &cancelled)?;
    let view = fragment.view()?;
    let run_owner = entity_ordinal_at_index(&view, b"run", EntityKind::Function, 0)?;
    let decoy_extra = entity_ordinal_at_index(&view, b"extra", EntityKind::Function, 0)?;
    let child_extra = entity_ordinal_at_index(&view, b"extra", EntityKind::Function, 1)?;
    let base_note = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 0)?;
    let extra_calls = method_calls_with_path(&view, run_owner, b"extra")?;
    if extra_calls.len() != 1 {
        return Err(TestError::Falsified("run owns one MethodCall with path extra"));
    }
    assert_local_index(extra_calls[0], child_extra, "self.note().extra() resolves to Child.extra")?;
    if matches!(
        &extra_calls[0].occurrence.target,
        OccurrenceTarget::Local(target) if target.raw == decoy_extra
    ) {
        return Err(TestError::Falsified("self.note().extra() must not resolve to Decoy.extra"));
    }
    let note_calls = method_calls_with_path(&view, run_owner, b"note")?;
    if note_calls.len() != 1 {
        return Err(TestError::Falsified("run owns one MethodCall with path note"));
    }
    assert_local_index(note_calls[0], base_note, "self.note() resolves to Base.note")?;
    fs::remove_dir_all(&env.work).map_err(|source| TestError::Io("remove scratch", source))?;
    Ok(())
}

#[test]
fn py_call_return_list_read_does_not_take_module_field() -> Result<(), TestError> {
    const SOURCE: &[u8] = b"\
extra = 1

class Decoy:
    def extra(self):
        return 0

class Child:
    def extra(self):
        return 1

class Holder:
    def note(self) -> list[Child]:
        return []
    def run(self):
        return self.note().extra
";
    let env = inherited_fixture_env()?;
    let toolchain = env.toolchain()?;
    let cancelled = AtomicBool::new(false);
    let fragment = compile_inherited_fragment(SOURCE, &env.work, &toolchain, &cancelled)?;
    let view = fragment.view()?;
    let run_owner = entity_ordinal_at_index(&view, b"run", EntityKind::Function, 0)?;
    let child_extra = entity_ordinal_at_index(&view, b"extra", EntityKind::Function, 1)?;
    let extra_calls = method_calls_with_path(&view, run_owner, b"extra")?;
    if !extra_calls.is_empty() {
        return Err(TestError::Falsified("run owns zero MethodCalls with path extra"));
    }
    let reads = field_accesses_with_path(&view, run_owner, b"extra")?;
    if reads.len() != 1 {
        return Err(TestError::Falsified("run owns one FieldAccess with path extra"));
    }
    assert_universe_field_named(
        reads[0],
        b"extra",
        "list[Child] read stays a pypi universe field key",
    )?;
    if matches!(
        &reads[0].occurrence.target,
        OccurrenceTarget::Local(target) if target.raw == child_extra
    ) {
        return Err(TestError::Falsified("self.note().extra must not resolve to Child.extra"));
    }
    fs::remove_dir_all(&env.work).map_err(|source| TestError::Io("remove scratch", source))?;
    Ok(())
}

#[test]
fn py_call_return_dotted_stays_universe() -> Result<(), TestError> {
    const SOURCE: &[u8] = b"\
import pkg

class Decoy:
    def extra(self):
        return 0

class Child:
    def extra(self):
        return 1

class Holder:
    def note(self) -> pkg.Child:
        return Child()
    def run(self):
        return self.note().extra()
";
    let env = inherited_fixture_env()?;
    let toolchain = env.toolchain()?;
    let cancelled = AtomicBool::new(false);
    let fragment = compile_inherited_fragment(SOURCE, &env.work, &toolchain, &cancelled)?;
    let view = fragment.view()?;
    let run_owner = entity_ordinal_at_index(&view, b"run", EntityKind::Function, 0)?;
    let child_extra = entity_ordinal_at_index(&view, b"extra", EntityKind::Function, 1)?;
    let extra_calls = method_calls_with_path(&view, run_owner, b"extra")?;
    if extra_calls.len() != 1 {
        return Err(TestError::Falsified("run owns one MethodCall with path extra"));
    }
    assert_universe_method_named(
        extra_calls[0],
        b"extra",
        "pkg.Child return stays a pypi universe key",
    )?;
    if matches!(
        &extra_calls[0].occurrence.target,
        OccurrenceTarget::Local(target) if target.raw == child_extra
    ) {
        return Err(TestError::Falsified("self.note().extra() must not resolve to Child.extra"));
    }
    fs::remove_dir_all(&env.work).map_err(|source| TestError::Io("remove scratch", source))?;
    Ok(())
}

#[test]
fn py_call_return_missing_member_stays_attribute_key() -> Result<(), TestError> {
    const SOURCE: &[u8] = b"\
class Decoy:
    def extra(self):
        return 0

class Child:
    pass

class Holder:
    def note(self) -> Child:
        return Child()
    def run(self):
        return self.note().extra()
";
    let env = inherited_fixture_env()?;
    let toolchain = env.toolchain()?;
    let cancelled = AtomicBool::new(false);
    let fragment = compile_inherited_fragment(SOURCE, &env.work, &toolchain, &cancelled)?;
    let view = fragment.view()?;
    let run_owner = entity_ordinal_at_index(&view, b"run", EntityKind::Function, 0)?;
    let decoy_extra = entity_ordinal_at_index(&view, b"extra", EntityKind::Function, 0)?;
    let extra_calls = method_calls_with_path(&view, run_owner, b"extra")?;
    if extra_calls.len() != 1 {
        return Err(TestError::Falsified("run owns one MethodCall with path extra"));
    }
    assert_universe_method_named(
        extra_calls[0],
        b"extra",
        "missing Child.extra stays a pypi universe key",
    )?;
    if matches!(
        &extra_calls[0].occurrence.target,
        OccurrenceTarget::Local(target) if target.raw == decoy_extra
    ) {
        return Err(TestError::Falsified("self.note().extra() must not resolve to Decoy.extra"));
    }
    fs::remove_dir_all(&env.work).map_err(|source| TestError::Io("remove scratch", source))?;
    Ok(())
}

#[test]
fn py_call_return_field_wins_over_method() -> Result<(), TestError> {
    const SOURCE: &[u8] = b"\
class Child:
    score = 1
    def score(self):
        return 2

class Holder:
    def note(self) -> Child:
        return Child()
    def run(self):
        return self.note().score
";
    let env = inherited_fixture_env()?;
    let toolchain = env.toolchain()?;
    let cancelled = AtomicBool::new(false);
    let fragment = compile_inherited_fragment(SOURCE, &env.work, &toolchain, &cancelled)?;
    let view = fragment.view()?;
    let run_owner = entity_ordinal_at_index(&view, b"run", EntityKind::Function, 0)?;
    let child_score_field = entity_ordinal_at_index(&view, b"score", EntityKind::Field, 0)?;
    let child_score_method = entity_ordinal_at_index(&view, b"score", EntityKind::Function, 0)?;
    let score_calls = method_calls_with_path(&view, run_owner, b"score")?;
    if !score_calls.is_empty() {
        return Err(TestError::Falsified("run owns zero MethodCalls with path score"));
    }
    let reads = field_accesses_with_path(&view, run_owner, b"score")?;
    if reads.len() != 1 {
        return Err(TestError::Falsified("run owns one FieldAccess with path score"));
    }
    assert_local_index(reads[0], child_score_field, "self.note().score resolves to Child.score field")?;
    if matches!(
        &reads[0].occurrence.target,
        OccurrenceTarget::Local(target) if target.raw == child_score_method
    ) {
        return Err(TestError::Falsified("self.note().score must not resolve to Child.score method"));
    }
    fs::remove_dir_all(&env.work).map_err(|source| TestError::Io("remove scratch", source))?;
    Ok(())
}

#[test]
fn py_call_return_call_binds_method_not_field() -> Result<(), TestError> {
    const SOURCE: &[u8] = b"\
class Child:
    extra = 1
    def extra(self):
        return 2

class Holder:
    def note(self) -> Child:
        return Child()
    def run(self):
        return self.note().extra()
";
    let env = inherited_fixture_env()?;
    let toolchain = env.toolchain()?;
    let cancelled = AtomicBool::new(false);
    let fragment = compile_inherited_fragment(SOURCE, &env.work, &toolchain, &cancelled)?;
    let view = fragment.view()?;
    let run_owner = entity_ordinal_at_index(&view, b"run", EntityKind::Function, 0)?;
    let child_extra_field = entity_ordinal_at_index(&view, b"extra", EntityKind::Field, 0)?;
    let child_extra_method = entity_ordinal_at_index(&view, b"extra", EntityKind::Function, 0)?;
    let extra_calls = method_calls_with_path(&view, run_owner, b"extra")?;
    if extra_calls.len() != 1 {
        return Err(TestError::Falsified("run owns one MethodCall with path extra"));
    }
    assert_local_index(extra_calls[0], child_extra_method, "self.note().extra() resolves to Child.extra method")?;
    if matches!(
        &extra_calls[0].occurrence.target,
        OccurrenceTarget::Local(target) if target.raw == child_extra_field
    ) {
        return Err(TestError::Falsified("self.note().extra() must not resolve to Child.extra field"));
    }
    fs::remove_dir_all(&env.work).map_err(|source| TestError::Io("remove scratch", source))?;
    Ok(())
}

#[test]
fn py_call_return_attribute_receiver_stays_universe() -> Result<(), TestError> {
    const SOURCE: &[u8] = b"\
class Decoy:
    def extra(self):
        return 0

class Child:
    def note(self) -> Child:
        return Child()
    def extra(self):
        return 1

class Holder:
    child: Child
    def run(self):
        return self.child.note().extra()
";
    let env = inherited_fixture_env()?;
    let toolchain = env.toolchain()?;
    let cancelled = AtomicBool::new(false);
    let fragment = compile_inherited_fragment(SOURCE, &env.work, &toolchain, &cancelled)?;
    let view = fragment.view()?;
    let run_owner = entity_ordinal_at_index(&view, b"run", EntityKind::Function, 0)?;
    let decoy_extra = entity_ordinal_at_index(&view, b"extra", EntityKind::Function, 0)?;
    let child_extra = entity_ordinal_at_index(&view, b"extra", EntityKind::Function, 1)?;
    let child_note = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 0)?;
    let extra_calls = method_calls_with_path(&view, run_owner, b"extra")?;
    if extra_calls.len() != 1 {
        return Err(TestError::Falsified("run owns one MethodCall with path extra"));
    }
    assert_universe_method_named(
        extra_calls[0],
        b"extra",
        "attribute receiver chain stays a pypi universe key",
    )?;
    if matches!(
        &extra_calls[0].occurrence.target,
        OccurrenceTarget::Local(target) if target.raw == child_extra
    ) {
        return Err(TestError::Falsified("self.child.note().extra() must not resolve to Child.extra"));
    }
    if matches!(
        &extra_calls[0].occurrence.target,
        OccurrenceTarget::Local(target) if target.raw == decoy_extra
    ) {
        return Err(TestError::Falsified("self.child.note().extra() must not resolve to Decoy.extra"));
    }
    let note_calls = method_calls_with_path(&view, run_owner, b"note")?;
    if note_calls.len() != 1 {
        return Err(TestError::Falsified("run owns one MethodCall with path note"));
    }
    assert_local_index(note_calls[0], child_note, "self.child.note() resolves to Child.note")?;
    fs::remove_dir_all(&env.work).map_err(|source| TestError::Io("remove scratch", source))?;
    Ok(())
}

#[test]
fn py_call_return_constructed_chain_stays_universe() -> Result<(), TestError> {
    const SOURCE: &[u8] = b"\
class Decoy:
    def extra(self):
        return 0

class Child:
    def note(self) -> Child:
        return Child()
    def extra(self):
        return 1

class Holder:
    def run(self):
        return Child().note().extra()
";
    let env = inherited_fixture_env()?;
    let toolchain = env.toolchain()?;
    let cancelled = AtomicBool::new(false);
    let fragment = compile_inherited_fragment(SOURCE, &env.work, &toolchain, &cancelled)?;
    let view = fragment.view()?;
    let run_owner = entity_ordinal_at_index(&view, b"run", EntityKind::Function, 0)?;
    let decoy_extra = entity_ordinal_at_index(&view, b"extra", EntityKind::Function, 0)?;
    let child_extra = entity_ordinal_at_index(&view, b"extra", EntityKind::Function, 1)?;
    let child_note = entity_ordinal_at_index(&view, b"note", EntityKind::Function, 0)?;
    let extra_calls = method_calls_with_path(&view, run_owner, b"extra")?;
    if extra_calls.len() != 1 {
        return Err(TestError::Falsified("run owns one MethodCall with path extra"));
    }
    assert_universe_method_named(
        extra_calls[0],
        b"extra",
        "constructed receiver chain stays a pypi universe key",
    )?;
    if matches!(
        &extra_calls[0].occurrence.target,
        OccurrenceTarget::Local(target) if target.raw == child_extra
    ) {
        return Err(TestError::Falsified("Child().note().extra() must not resolve to Child.extra"));
    }
    if matches!(
        &extra_calls[0].occurrence.target,
        OccurrenceTarget::Local(target) if target.raw == decoy_extra
    ) {
        return Err(TestError::Falsified("Child().note().extra() must not resolve to Decoy.extra"));
    }
    let note_calls = method_calls_with_path(&view, run_owner, b"note")?;
    if note_calls.len() != 1 {
        return Err(TestError::Falsified("run owns one MethodCall with path note"));
    }
    assert_local_index(note_calls[0], child_note, "Child().note() resolves to Child.note")?;
    fs::remove_dir_all(&env.work).map_err(|source| TestError::Io("remove scratch", source))?;
    Ok(())
}
