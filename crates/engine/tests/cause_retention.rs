//! Exact-cause retention on the shared compile terminal.
//!
//! Law: a rejected emission fact keeps its ordinal, rejected name length, and
//! full typed cause through the public compile journey; the terminal never
//! folds a bounded-lane rejection onto the coarse `NoSupportedDeclaration`
//! cause.

use std::{
    process::Command,
    sync::atomic::{AtomicBool, Ordering},
    time::{Duration, Instant},
};

use backend_engine::driver::{
    CompileControl, CompileFailure, CompileRequest, CompileScratch, NativeTool, ResolvedToolchain,
    SemanticAuthorityInput, ToolchainSelection, compile_ir,
};
use backend_semantic::vocabulary::{
    LanguageProfile, LoweringUnsupported, ProjectionAdmissionFault, PythonVersion, Stage,
};
use thiserror::Error;

#[derive(Debug, Error)]
enum TestError {
    #[error("no local python3 toolchain on PATH")]
    MissingPython,
    #[error("python3 --version failed")]
    Version,
    #[error("toolchain resolution failed")]
    Resolve,
    #[error("the maximal source was admitted; the bounded lane never rejected")]
    Admitted,
    #[error("the lane rejection lost its exact terminal: {0:?}")]
    WrongTerminal(String),
    #[error("the rejection lost its capacity cause: {0:?}")]
    WrongCause(ProjectionAdmissionFault),
    #[error("the rejected name length {0} does not name the rejected ordinal")]
    WrongName(u64),
    #[error("work directory setup failed")]
    Work,
}

/// Renders one minimal `def f<N>(): ...` declaration.
fn one_function(index: usize) -> String {
    format!("def f{index}():\n    return 0\n\n")
}

fn decimal_digits(mut value: u64) -> u64 {
    let mut count = 1;
    while value >= 10 {
        value /= 10;
        count += 1;
    }
    count
}

/// The bounded emission lane rejects the fact past its frozen capacity with
/// the exact ordinal, rejected name length, and portable capacity cause
/// at the public compile terminal. The assertion is stated against the
/// rejected ordinal itself, so it survives the frozen bound moving.
#[test]
fn overflowing_the_emission_lane_retains_the_exact_rejection_operands() -> Result<(), TestError> {
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
        .map_err(|_| TestError::Version)?;
    let version_bytes = if version.stdout.is_empty() {
        version.stderr.as_slice()
    } else {
        version.stdout.as_slice()
    };
    let toolchain = ResolvedToolchain::from_version(NativeTool::Python, &executable, version_bytes)
        .map_err(|_| TestError::Resolve)?;
    let mut source = String::new();
    // The landed emission geometry is MAX_EMISSION_FACTS = 16384: one
    // fact per zero-arity function, so bound + 1 functions is the exact
    // first overflow. The law is unchanged — the rejection retains the
    // exact rejected ordinal and cause.
    for index in 0..16_385usize {
        source.push_str(&one_function(index));
    }
    let source_bytes = source.into_bytes();
    let cancelled = AtomicBool::new(false);
    let mut diagnostic = [0_u8; 4096];
    let work = std::env::temp_dir().join(format!("nudox-cause-retention-{}", std::process::id()));
    std::fs::create_dir_all(&work).map_err(|_| TestError::Work)?;
    let failure = compile_ir(
        CompileRequest {
            profile: LanguageProfile::Python(PythonVersion::Python314),
            stage: Stage::LowerIr,
            source: &source_bytes,
            declaration_scope: backend_engine::driver::DeclarationScope::fixture(),
            toolchain: ToolchainSelection::ResolvedNative(toolchain),
            authority: SemanticAuthorityInput::None,
            control: CompileControl {
                deadline: Instant::now() + Duration::from_secs(120),
                cancelled: &cancelled,
            },
        },
        CompileScratch {
            diagnostic_output: &mut diagnostic,
            native_work: &work,
        },
    )
    .err()
    .ok_or(TestError::Admitted);
    cancelled.store(true, Ordering::Relaxed);
    let _ = std::fs::remove_dir_all(&work);
    let failure = match failure {
        Ok(failure) => failure,
        Err(admitted) => return Err(admitted),
    };
    let CompileFailure::LoweringUnsupported {
        cause:
            LoweringUnsupported::FactRejected {
                fact,
                name_len,
                cause,
            },
        ..
    } = &failure
    else {
        return Err(TestError::WrongTerminal(format!("{failure:?}")));
    };
    if *cause != ProjectionAdmissionFault::Capacity {
        return Err(TestError::WrongCause(*cause));
    }
    if *name_len != 1 + decimal_digits(*fact) {
        return Err(TestError::WrongName(*name_len));
    }
    Ok(())
}
