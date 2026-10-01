//! Python import-nominal use-case flow: local alias nominal plus external return.

#![forbid(unsafe_code)]
#![deny(clippy::expect_used, clippy::panic, clippy::unwrap_used)]

#[path = "use_case_support/mod.rs"]
mod use_case_support;

use std::{
    io,
    path::Path,
    process::Command,
    sync::atomic::AtomicBool,
    time::{Duration, Instant},
};

use backend_engine::driver::{
    CompileControl, CompileFailure, CompileOutput, CompileRequest, CompileScratch, NativeTool,
    ResolvedToolchain, SemanticAuthorityInput, ToolchainSelection, compile_semantic,
};
use backend_semantic::ir::{
    DecodedTypeFact, EntityKind, FragmentView, NominalRef, SemanticTypeTag, TypeReason,
};
use backend_semantic::vocabulary::{LanguageProfile, PythonVersion, Stage};
use thiserror::Error;

const FLOW_SOURCE: &[u8] = b"class Plain:\n    pass\n\ntype Box = Plain\n\nfrom pathlib import Path\n\ndef f(x: Box) -> Path:\n    return Path(\".\")\n";

#[derive(Debug, Error)]
enum TestError {
    #[error("python3 is unavailable")]
    MissingPython,
    #[error("python tool failed: {0}")]
    Tool(#[source] std::io::Error),
    #[error("toolchain resolution failed")]
    Resolve,
    #[error("compile failed: {0}")]
    Compile(&'static str),
    #[error("fragment validation failed")]
    Validate,
    #[error("lane falsifier: {0}")]
    Falsified(&'static str),
    #[error("use-case report failed: {0}")]
    Report(#[source] io::Error),
}

fn failure_label(failure: &CompileFailure<'_>) -> &'static str {
    match failure {
        CompileFailure::LoweringUnsupported { .. } => "lowering-unsupported",
        CompileFailure::Authority { .. } => "authority",
        _ => "compile",
    }
}

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
    let absolute = executable.canonicalize().map_err(TestError::Tool)?;
    let absolute: &'static Path = Box::leak(absolute.into_boxed_path());
    ResolvedToolchain::from_version(NativeTool::Python, absolute, version_bytes)
        .map_err(|_| TestError::Resolve)
}

struct Lane<'fragment> {
    entities: Vec<(&'fragment [u8], EntityKind)>,
    types: Vec<DecodedTypeFact<'fragment>>,
}

fn lane_of(bytes: &[u8]) -> Result<Lane<'_>, TestError> {
    let view = FragmentView::validate(bytes).map_err(|_| TestError::Validate)?;
    let mut entities = Vec::new();
    for entity in view.entities() {
        let ordinal = usize::try_from(entity.name.raw).map_err(|_| TestError::Falsified("name"))?;
        let name = view
            .atoms()
            .nth(ordinal)
            .map(|atom| atom.bytes)
            .ok_or(TestError::Falsified("entity atom"))?;
        entities.push((name, entity.kind));
    }
    let mut types = Vec::new();
    if let Some(cursor) = view.type_facts() {
        for row in cursor {
            types.push(row.map_err(|_| TestError::Falsified("type fact"))?);
        }
    }
    Ok(Lane { entities, types })
}

fn entity_ordinal(lane: &Lane<'_>, name: &[u8], kind: EntityKind) -> Result<usize, TestError> {
    lane.entities
        .iter()
        .position(|(known, known_kind)| *known == name && *known_kind == kind)
        .ok_or(TestError::Falsified("entity absent"))
}

fn owned_row(lane: &Lane<'_>, owner: usize) -> Result<usize, TestError> {
    lane.types
        .iter()
        .position(|fact| fact.owner.raw as usize == owner)
        .ok_or(TestError::Falsified("entity type row absent"))
}

fn parameter_type<'a>(lane: &'a Lane<'a>, name: &[u8]) -> Result<&'a DecodedTypeFact<'a>, TestError> {
    let owner = entity_ordinal(lane, name, EntityKind::Parameter)?;
    let row = owned_row(lane, owner)?;
    Ok(&lane.types[row])
}

fn nominal_target(fact: &DecodedTypeFact<'_>) -> Result<Option<u32>, TestError> {
    if fact.record.tag != SemanticTypeTag::Nominal {
        return Ok(None);
    }
    match fact.record.nominal {
        Some(NominalRef::Local(id)) => Ok(Some(id.raw)),
        _ => Ok(None),
    }
}

fn unknown_reason(fact: &DecodedTypeFact<'_>) -> Option<TypeReason> {
    if fact.record.tag == SemanticTypeTag::Unknown {
        TypeReason::try_from(fact.record.payload0).ok()
    } else {
        None
    }
}

/// A PEP 695 alias of a declared class and an external import in one module:
/// `Box` on the parameter resolves to `Plain`, and `Path` on the return stays
/// `UnresolvedExternal`.
#[test]
fn import_nominal_flow() -> Result<(), TestError> {
    let toolchain = match python_toolchain() {
        Err(TestError::MissingPython) => {
            use_case_support::skip("python", "import-nominal-flow", "python3")
                .map_err(TestError::Report)?;
            return Ok(());
        }
        Ok(toolchain) => toolchain,
        Err(error) => return Err(error),
    };

    let timer = use_case_support::CompileTimer::start();
    let cancelled = AtomicBool::new(false);
    let mut diagnostic = [0_u8; 4096];
    let mut output = vec![0_u8; 8 * 1024 * 1024];
    let compiled = compile_semantic(
        CompileRequest {
            profile: LanguageProfile::Python(PythonVersion::Python314),
            stage: Stage::LowerIr,
            source: FLOW_SOURCE,
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
            native_work: Path::new("/tmp"),
        },
        CompileOutput {
            fragment_output: &mut output,
        },
    )
    .map_err(|failure| TestError::Compile(failure_label(&failure)))?;

    let lane = lane_of(compiled.artifact.fragment.as_ref())?;

    let plain = entity_ordinal(&lane, b"Plain", EntityKind::Record)?;
    let param = parameter_type(&lane, b"x")?;
    let target = nominal_target(param)?.ok_or(TestError::Falsified("Box is not nominal"))?;
    if target as usize != plain {
        return Err(TestError::Falsified(
            "Box must refer to Plain's nominal row",
        ));
    }

    let result = parameter_type(&lane, b"f")?;
    let reason = unknown_reason(result).ok_or(TestError::Falsified("Path is not unknown"))?;
    if reason != TypeReason::UnresolvedExternal {
        return Err(TestError::Falsified(
            "pathlib.Path must stay UnresolvedExternal",
        ));
    }

    use_case_support::finish(
        "python",
        "import-nominal-flow",
        timer.elapsed(),
        &compiled.ir,
    )
    .map_err(TestError::Report)?;
    Ok(())
}
