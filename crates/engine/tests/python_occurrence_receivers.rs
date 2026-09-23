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
