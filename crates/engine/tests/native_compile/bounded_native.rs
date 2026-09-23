//! Exercises the `engine driver` tests native-compile bounded-native contract through its observable boundary.
//! The cases target malformed, partial, reordered, and resource-constrained behavior.
//! Assertions retain exact typed causes so regressions cannot pass through lossy errors.
//!
//! The public `compile` entry no longer spawns a native syntax pass: authority
//! is admitted first and every `LowerIr` profile lowers through its direct
//! authority (`0f8f0120f`, `8ded4315e`). The hostile-helper child proofs
//! (deadline kill, closed stdin, pre-cancel, cleanup compounding, diagnostic
//! lease) therefore live beside the private sidecar they exercise, in
//! `src/driver/native/bounded_tests.rs`. The pre-spawn stage terminal stays
//! here because it is still observable at the public boundary.
use std::{
    env,
    fs::{self, OpenOptions},
    io::Write,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    sync::atomic::{AtomicBool, AtomicUsize, Ordering},
    time::{Duration, Instant},
};

use backend_engine::driver::{
    CompileControl, CompileFailure, CompileOutput, CompileRequest, CompileScratch, NativeTool,
    ResolvedToolchain, ToolchainSelection, compile,
};
use backend_semantic::vocabulary::{FrontendError, Language, LanguageProfile, RustEdition, Stage};
use thiserror::Error;

use super::support::{CompileTerminal, TemporaryWork, TestFailure, compile_terminal};

static SCRIPT_SEQUENCE: AtomicUsize = AtomicUsize::new(0);

#[derive(Debug, Error)]
enum ScriptFailure {
    #[error("could not create the bounded-native test executable")]
    Create(#[source] std::io::Error),
    #[error("could not write the bounded-native test executable")]
    Write(#[source] std::io::Error),
    #[error("could not make the bounded-native test executable runnable")]
    Permissions(#[source] std::io::Error),
    #[error(transparent)]
    Resolve(#[from] backend_engine::driver::ToolchainResolutionError),
    #[error(transparent)]
    Work(#[from] TestFailure),
    #[error(
        "the hostile native helper was expected to end at {expected:?}, but ended at {observed:?}"
    )]
    CompileTerminal {
        expected: ScriptExpectedTerminal,
        observed: CompileTerminal,
    },
    #[error("the hostile native helper violated the {invariant:?} invariant")]
    Invariant { invariant: ScriptInvariant },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ScriptExpectedTerminal {
    UnsupportedStage,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ScriptInvariant {
    OutputUnchanged,
}

struct TemporaryExecutable {
    path: PathBuf,
}

impl Drop for TemporaryExecutable {
    fn drop(&mut self) {
        let _removed = fs::remove_file(&self.path);
    }
}

fn script(body: &[u8]) -> Result<TemporaryExecutable, ScriptFailure> {
    let sequence = SCRIPT_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let path = env::temp_dir().join(format!(
        "compiler-driver-{}-{sequence}.sh",
        std::process::id()
    ));
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&path)
        .map_err(ScriptFailure::Create)?;
    file.write_all(body).map_err(ScriptFailure::Write)?;
    file.set_permissions(fs::Permissions::from_mode(0o700))
        .map_err(ScriptFailure::Permissions)?;
    Ok(TemporaryExecutable { path })
}

#[test]
fn parse_stage_is_a_pre_spawn_typed_terminal_and_never_lends_ir() -> Result<(), ScriptFailure> {
    let executable = script(b"#!/bin/sh\nprintf x > started\nwhile :; do :; done\n")?;
    let toolchain = ResolvedToolchain::from_version(
        NativeTool::Rustc,
        Path::new(&executable.path),
        b"parse-stage-fixture",
    )?;
    let native_work = TemporaryWork::create()?;
    let cancelled = AtomicBool::new(false);
    let mut diagnostic = [0; 128];
    let mut output = [0xa5; 512];
    match compile(
        CompileRequest {
            profile: LanguageProfile::Rust(RustEdition::Rust2024),
            stage: Stage::Parse,
            source: b"pub const alpha: bool = true;",
            declaration_scope: backend_engine::driver::DeclarationScope::fixture(),
            toolchain: ToolchainSelection::ResolvedNative(toolchain),
            authority: backend_engine::driver::SemanticAuthorityInput::None,
            control: CompileControl {
                deadline: Instant::now() + Duration::from_secs(1),
                cancelled: &cancelled,
            },
        },
        CompileScratch {
            diagnostic_output: &mut diagnostic,
            native_work: native_work.path(),
        },
        CompileOutput {
            fragment_output: &mut output,
        },
    ) {
        Err(CompileFailure::UnsupportedStage {
            language: Language::Rust,
            stage: Stage::Parse,
            cause:
                FrontendError::UnsupportedStage {
                    language: Language::Rust,
                    stage: Stage::Parse,
                },
            ..
        }) => {}
        Err(failure) => {
            return Err(ScriptFailure::CompileTerminal {
                expected: ScriptExpectedTerminal::UnsupportedStage,
                observed: compile_terminal(&failure),
            });
        }
        Ok(_compiled) => {
            return Err(ScriptFailure::CompileTerminal {
                expected: ScriptExpectedTerminal::UnsupportedStage,
                observed: CompileTerminal::Compiled,
            });
        }
    }
    native_work.assert_empty()?;
    if !output.iter().all(|byte| *byte == 0xa5) {
        return Err(ScriptFailure::Invariant {
            invariant: ScriptInvariant::OutputUnchanged,
        });
    }
    Ok(())
}
