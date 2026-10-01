//! Python shadowing collapse: later bindings win over byte-identical twins.
//!
//! Python legally redefines a name in one scope; the identity model is
//! coordinate-free, so twins share one family and one variant. These
//! falsifiers prove the lowerer keeps the live (later) binding per
//! `(owner, kind, name)` signature group while preserving distinct overloads
//! and different-scope same-name declarations.

#![forbid(unsafe_code)]
#![deny(clippy::expect_used, clippy::panic, clippy::unwrap_used)]

use std::{
    fs,
    path::PathBuf,
    process::Command,
    sync::atomic::{AtomicBool, AtomicU64, Ordering},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use backend_engine::driver::{
    CompileControl, CompileFailure, CompileRequest, CompileScratch, NativeTool, ResolvedToolchain,
    SemanticAuthorityInput, ToolchainSelection, compile_ir,
};
use backend_semantic::ir::ItemKind;
use backend_semantic::vocabulary::{LanguageProfile, PythonVersion, Stage};
use thiserror::Error;

static SCRATCH_SEQUENCE: AtomicU64 = AtomicU64::new(0);
const WORKER_STACK_BYTES: usize = 16 * 1024 * 1024;

/// Two zero-parameter twins in one function scope (the pyparsing `a_method`
/// shape): the later definition is live, the earlier is shadowed.
const TWIN_METHODS: &[u8] = b"def outer():\n    def a_method():\n        return 1\n    a_method()\n    def a_method():\n        return 2\n    return a_method()\n";
/// Conditional twins at module root (the six `get_unbound_function` shape).
const CONDITIONAL_TWINS: &[u8] = b"if True:\n    def get_unbound_function(unbound):\n        return unbound\nelse:\n    def get_unbound_function(unbound):\n        return unbound\n";
/// Three identical nested `convert` definitions (the click shape): each twin
/// carries its own result slot, so the dead subtrees must go with them.
const TRIPLE_CONVERT: &[u8] = b"def type_cast_value(self, value):\n    if True:\n        def convert(value):\n            return value\n    elif False:\n        def convert(value):\n            return value\n    else:\n        def convert(value):\n            return value\n    return convert(value)\n";
/// Distinct overloads must all survive: different signatures never merge.
const DISTINCT_OVERLOADS: &[u8] = b"import typing\n@typing.overload\ndef overloaded(value: int) -> str: ...\n@typing.overload\ndef overloaded(value: str) -> int: ...\ndef overloaded(value):\n    return value\n";
/// Same method name in different classes must never merge.
const DIFFERENT_SCOPES: &[u8] = b"class Alpha:\n    def foo(self):\n        return 1\nclass Beta:\n    def foo(self):\n        return 2\n";
/// Shadowed module variable: later binding wins.
const TWIN_VARIABLES: &[u8] = b"x: int = 1\nx: int = 2\n";
/// The lane widens `Literal[True]` to `bool`, so a literal twin and a bare
/// `bool` twin share one variant (the click `lookup_default` shape): later wins.
const LITERAL_BOOL_TWINS: &[u8] = b"import typing\ndef check(flag: typing.Literal[True]):\n    return flag\ndef check(flag: bool):\n    return flag\n";
/// The lane lowers `typing.Any` exactly like bare `Any`, so those twins
/// share one variant: later wins.
const TYPING_ALIAS_TWINS: &[u8] = b"import typing\ndef f(value: typing.Any):\n    return value\ndef f(value: Any):\n    return value\n";

#[derive(Debug, Error)]
enum TestError {
    #[error("clock failed")]
    Clock(#[source] std::time::SystemTimeError),
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
    #[error("lane falsifier: {0}")]
    Falsified(&'static str),
    #[error("worker failed")]
    Worker,
}

fn with_deep_stack<Decoded>(
    work: impl FnOnce() -> Result<Decoded, TestError> + Send,
) -> Result<Decoded, TestError>
where
    Decoded: Send,
{
    std::thread::scope(|scope| {
        let worker = std::thread::Builder::new()
            .stack_size(WORKER_STACK_BYTES)
            .spawn_scoped(scope, work)
            .map_err(|_| TestError::Worker)?;
        worker.join().map_err(|_| TestError::Worker)?
    })
}

fn failure_label(failure: &CompileFailure<'_>) -> &'static str {
    match failure {
        CompileFailure::Build { .. } => "build",
        CompileFailure::LoweringUnsupported { .. } => "lowering-unsupported",
        _ => "other",
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
    let absolute = executable
        .canonicalize()
        .map_err(|source| TestError::Io("canonicalize python3", source))?;
    let absolute: &'static std::path::Path = Box::leak(absolute.into_boxed_path());
    ResolvedToolchain::from_version(NativeTool::Python, absolute, version_bytes)
        .map_err(|_| TestError::Resolve)
}

fn scratch_dir(label: &'static str) -> Result<PathBuf, TestError> {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(TestError::Clock)?
        .as_nanos();
    let sequence = SCRATCH_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let work = std::env::temp_dir().join(format!(
        "nudox-python-shadow-{label}-{nonce}-{}-{sequence}",
        std::process::id()
    ));
    fs::create_dir_all(&work).map_err(|source| TestError::Io("create scratch", source))?;
    Ok(work)
}

fn function_count(source: &'static [u8], label: &'static str, name: &[u8]) -> Result<usize, TestError> {
    let toolchain = python_toolchain()?;
    let work = scratch_dir(label)?;
    let cancelled = AtomicBool::new(false);
    let outcome = with_deep_stack(|| {
        match compile_ir(
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
                diagnostic_output: &mut [0_u8; 4096],
                native_work: &work,
            },
        ) {
            Ok(compiled) => {
                let count = compiled
                    .ir
                    .items()
                    .filter(|item| item.kind() == ItemKind::Function && item.name() == name)
                    .count();
                Ok(count)
            }
            Err(failure) => Err(TestError::Compile(failure_label(&failure))),
        }
    })?;
    fs::remove_dir_all(&work).map_err(|source| TestError::Io("remove scratch", source))?;
    Ok(outcome)
}

fn compiles(source: &'static [u8], label: &'static str) -> Result<(), TestError> {
    let toolchain = python_toolchain()?;
    let work = scratch_dir(label)?;
    let cancelled = AtomicBool::new(false);
    with_deep_stack(|| {
        match compile_ir(
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
                diagnostic_output: &mut [0_u8; 4096],
                native_work: &work,
            },
        ) {
            Ok(_) => Ok(()),
            Err(failure) => Err(TestError::Compile(failure_label(&failure))),
        }
    })?;
    fs::remove_dir_all(&work).map_err(|source| TestError::Io("remove scratch", source))?;
    Ok(())
}

fn static_count(source: &'static [u8], label: &'static str, name: &[u8]) -> Result<usize, TestError> {
    let toolchain = python_toolchain()?;
    let work = scratch_dir(label)?;
    let cancelled = AtomicBool::new(false);
    let outcome = with_deep_stack(|| {
        match compile_ir(
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
                diagnostic_output: &mut [0_u8; 4096],
                native_work: &work,
            },
        ) {
            Ok(compiled) => {
                let count = compiled
                    .ir
                    .items()
                    .filter(|item| item.kind() == ItemKind::Static && item.name() == name)
                    .count();
                Ok(count)
            }
            Err(failure) => Err(TestError::Compile(failure_label(&failure))),
        }
    })?;
    fs::remove_dir_all(&work).map_err(|source| TestError::Io("remove scratch", source))?;
    Ok(outcome)
}

#[test]
fn twin_methods_in_one_scope_keep_only_the_live_binding() -> Result<(), TestError> {
    let count = function_count(TWIN_METHODS, "twin", b"a_method")?;
    if count != 1 {
        return Err(TestError::Falsified(
            "shadowed twin did not collapse to exactly one live function",
        ));
    }
    Ok(())
}

#[test]
fn conditional_twins_at_module_root_keep_only_the_live_binding() -> Result<(), TestError> {
    let count = function_count(CONDITIONAL_TWINS, "cond", b"get_unbound_function")?;
    if count != 1 {
        return Err(TestError::Falsified(
            "conditional twins did not collapse to exactly one live function",
        ));
    }
    Ok(())
}

#[test]
fn triple_nested_convert_twins_compile_with_their_result_slots() -> Result<(), TestError> {
    compiles(TRIPLE_CONVERT, "convert")?;
    let count = function_count(TRIPLE_CONVERT, "convert-count", b"convert")?;
    if count != 1 {
        return Err(TestError::Falsified(
            "triple convert twins did not collapse to exactly one live function",
        ));
    }
    Ok(())
}

#[test]
fn distinct_overloads_all_survive() -> Result<(), TestError> {
    let count = function_count(DISTINCT_OVERLOADS, "overloads", b"overloaded")?;
    if count != 3 {
        return Err(TestError::Falsified(
            "distinct overloads were wrongly merged; expected all three to survive",
        ));
    }
    Ok(())
}

#[test]
fn same_name_in_different_scopes_never_merge() -> Result<(), TestError> {
    let count = function_count(DIFFERENT_SCOPES, "scopes", b"foo")?;
    if count != 2 {
        return Err(TestError::Falsified(
            "same-name methods in different classes were wrongly merged",
        ));
    }
    Ok(())
}

#[test]
fn twin_module_variables_keep_only_the_live_binding() -> Result<(), TestError> {
    let count = static_count(TWIN_VARIABLES, "vars", b"x")?;
    if count != 1 {
        return Err(TestError::Falsified(
            "shadowed variable did not collapse to exactly one live static",
        ));
    }
    Ok(())
}

#[test]
fn literal_widened_twins_share_one_live_binding() -> Result<(), TestError> {
    // `typing.Literal[True]` widens to `bool` in the lane, exactly like the
    // bare annotation, so these twins share one variant and only the later
    // (live) binding survives.
    let count = function_count(LITERAL_BOOL_TWINS, "literal", b"check")?;
    if count != 1 {
        return Err(TestError::Falsified(
            "literal-widened twins did not collapse to exactly one live function",
        ));
    }
    Ok(())
}

#[test]
fn typing_aliased_twins_share_one_live_binding() -> Result<(), TestError> {
    // `typing.Any` lowers exactly like bare `Any`, so these twins share one
    // variant and only the later (live) binding survives.
    let count = function_count(TYPING_ALIAS_TWINS, "typing-alias", b"f")?;
    if count != 1 {
        return Err(TestError::Falsified(
            "typing-aliased twins did not collapse to exactly one live function",
        ));
    }
    Ok(())
}
