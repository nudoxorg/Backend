//! Regression tests for Python nominal resolution through unambiguous bindings.

#![forbid(unsafe_code)]
#![deny(clippy::expect_used, clippy::panic, clippy::unwrap_used)]

use std::{
    path::Path,
    process::Command,
    sync::atomic::AtomicBool,
    time::{Duration, Instant},
};

use backend_engine::driver::{
    CompileControl, CompileFailure, CompileOutput, CompileRequest, CompileScratch, NativeTool,
    ResolvedToolchain, SemanticAuthorityInput, ToolchainSelection, compile,
};
use backend_semantic::ir::{
    DecodedTypeFact, EntityKind, FragmentView, NominalRef, SemanticTypeTag, TypeReason,
};
use backend_semantic::vocabulary::{LanguageProfile, PythonVersion, Stage};
use thiserror::Error;

const TYPE_ALIAS_SOURCE: &[u8] = b"class Plain:\n    pass\n\ntype Box = Plain\n\ndef f(x: Box) -> Box:\n    pass\n";
const AMBIGUOUS_IMPORT_SOURCE: &[u8] =
    b"from a import Foo\nfrom b import Foo\n\ndef f(x: Foo) -> Foo:\n    pass\n";
const EXTERNAL_IMPORT_SOURCE: &[u8] = b"from pathlib import Path\n\ndef f(x: Path) -> Path:\n    pass\n";

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

fn compile_fragment(source: &[u8]) -> Result<Vec<u8>, TestError> {
    let toolchain = python_toolchain()?;
    let cancelled = AtomicBool::new(false);
    let mut diagnostic = [0_u8; 4096];
    let mut output = vec![0_u8; 8 * 1024 * 1024];
    let compiled = compile(
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
            native_work: Path::new("/tmp"),
        },
        CompileOutput {
            fragment_output: &mut output,
        },
    )
    .map_err(|failure| TestError::Compile(failure_label(&failure)))?;
    Ok(compiled.fragment.as_ref().to_vec())
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

/// A PEP 695 `type` alias of a declared class must lower to that class's
/// nominal, not `UnresolvedExternal`.
#[test]
fn type_alias_of_declared_class_is_nominal() -> Result<(), TestError> {
    let bytes = compile_fragment(TYPE_ALIAS_SOURCE)?;
    let lane = lane_of(&bytes)?;
    let plain = entity_ordinal(&lane, b"Plain", EntityKind::Record)?;
    let param = parameter_type(&lane, b"x")?;
    let target = nominal_target(param)?.ok_or(TestError::Falsified("Box is not nominal"))?;
    if target as usize != plain {
        return Err(TestError::Falsified(
            "Box must refer to Plain's nominal row",
        ));
    }
    Ok(())
}

/// Two imports of the same spelling from different modules stay unresolved.
#[test]
fn ambiguous_import_stays_unresolved() -> Result<(), TestError> {
    let bytes = compile_fragment(AMBIGUOUS_IMPORT_SOURCE)?;
    let lane = lane_of(&bytes)?;
    let param = parameter_type(&lane, b"x")?;
    if nominal_target(param)?.is_some() {
        return Err(TestError::Falsified("ambiguous Foo must not become nominal"));
    }
    let reason = unknown_reason(param).ok_or(TestError::Falsified("Foo is not unknown"))?;
    if reason == TypeReason::UnresolvedExternal {
        return Err(TestError::Falsified(
            "ambiguous Foo must not be reported external",
        ));
    }
    Ok(())
}

/// An import of a name this module does not declare stays external.
#[test]
fn external_import_stays_external() -> Result<(), TestError> {
    let bytes = compile_fragment(EXTERNAL_IMPORT_SOURCE)?;
    let lane = lane_of(&bytes)?;
    let param = parameter_type(&lane, b"x")?;
    let reason = unknown_reason(param).ok_or(TestError::Falsified("Path is not unknown"))?;
    if reason != TypeReason::UnresolvedExternal {
        return Err(TestError::Falsified(
            "pathlib.Path must stay UnresolvedExternal",
        ));
    }
    Ok(())
}
