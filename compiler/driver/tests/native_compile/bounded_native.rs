//! Exercises the `compiler-driver` tests native-compile bounded-native contract through its observable boundary.
//! The cases target malformed, partial, reordered, and resource-constrained behavior.
//! Assertions retain exact typed causes so regressions cannot pass through lossy errors.
use std::{
    env,
    fs::{self, OpenOptions},
    io::Write,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    sync::atomic::{AtomicBool, AtomicUsize, Ordering},
    time::{Duration, Instant},
};

use compiler_driver::{
    CompileControl, CompileFailure, CompileOutput, CompileRequest, CompileScratch, NativeTool,
    NativeWorkError, NativeWorkPrimary, ResolvedToolchain, ToolchainSelection, compile,
};
use compiler_vocabulary::{FrontendError, Language, Stage};
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
    Resolve(#[from] compiler_driver::ToolchainResolutionError),
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
    #[error("could not remove the exact hostile helper artifact after its cleanup test")]
    RemoveHostileArtifact(#[source] std::io::Error),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ScriptExpectedTerminal {
    DeadlineExceeded,
    ToolInput,
    Cancelled,
    UnsupportedStage,
    NativeRejectedCleanup,
    DiagnosticLimit,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ScriptInvariant {
    OutputUnchanged,
    DiagnosticLimitValue,
    DiagnosticObservedLimit,
    RetainedDiagnosticLength,
    DiagnosticTruncated,
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

fn request<'source, 'path, 'cancel>(
    source: &'source [u8],
    toolchain: ToolchainSelection<'path>,
    cancelled: &'cancel AtomicBool,
    deadline: Instant,
) -> CompileRequest<'source, 'path, 'cancel> {
    CompileRequest {
        language: Language::Rust,
        stage: Stage::LowerIr,
        source,
        toolchain,
        control: CompileControl {
            deadline,
            cancelled,
        },
    }
}

#[test]
fn nonreading_never_exit_tool_is_killed_without_blocking_the_deadline_owner()
-> Result<(), ScriptFailure> {
    let executable = script(b"#!/bin/sh\nwhile :; do :; done\n")?;
    let toolchain = ResolvedToolchain::from_version(
        NativeTool::Rustc,
        Path::new(&executable.path),
        b"nonreading-fixture",
    )?;
    let source = [b'x'; 131_072];
    let native_work = TemporaryWork::create()?;
    let cancelled = AtomicBool::new(false);
    let mut diagnostic = [0; 128];
    let mut output = [0; 512];
    match compile(
        request(
            &source,
            ToolchainSelection::ResolvedNative(toolchain),
            &cancelled,
            Instant::now() + Duration::from_millis(300),
        ),
        CompileScratch {
            diagnostic_output: &mut diagnostic,
            native_work: native_work.path(),
        },
        CompileOutput {
            fragment_output: &mut output,
        },
    ) {
        Err(CompileFailure::DeadlineExceeded { .. }) => {}
        Err(failure) => {
            return Err(ScriptFailure::CompileTerminal {
                expected: ScriptExpectedTerminal::DeadlineExceeded,
                observed: compile_terminal(&failure),
            });
        }
        Ok(_compiled) => {
            return Err(ScriptFailure::CompileTerminal {
                expected: ScriptExpectedTerminal::DeadlineExceeded,
                observed: CompileTerminal::Compiled,
            });
        }
    }
    native_work.assert_empty()?;
    Ok(())
}

#[test]
fn stdin_closed_then_never_exit_retains_the_input_cause_and_reaps_the_child()
-> Result<(), ScriptFailure> {
    let executable = script(b"#!/bin/sh\nexec 0<&-\nwhile :; do :; done\n")?;
    let toolchain = ResolvedToolchain::from_version(
        NativeTool::Rustc,
        Path::new(&executable.path),
        b"stdin-closed-fixture",
    )?;
    let native_work = TemporaryWork::create()?;
    let cancelled = AtomicBool::new(false);
    let source = [b'x'; 131_072];
    let mut diagnostic = [0; 128];
    let mut output = [0xa5; 512];
    match compile(
        request(
            &source,
            ToolchainSelection::ResolvedNative(toolchain),
            &cancelled,
            Instant::now() + Duration::from_secs(1),
        ),
        CompileScratch {
            diagnostic_output: &mut diagnostic,
            native_work: native_work.path(),
        },
        CompileOutput {
            fragment_output: &mut output,
        },
    ) {
        Err(CompileFailure::ToolInput { .. }) => {}
        Err(failure) => {
            return Err(ScriptFailure::CompileTerminal {
                expected: ScriptExpectedTerminal::ToolInput,
                observed: compile_terminal(&failure),
            });
        }
        Ok(_compiled) => {
            return Err(ScriptFailure::CompileTerminal {
                expected: ScriptExpectedTerminal::ToolInput,
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

#[test]
fn pre_cancelled_request_never_starts_the_marker_tool() -> Result<(), ScriptFailure> {
    let executable = script(b"#!/bin/sh\nprintf x > started\nwhile :; do :; done\n")?;
    let toolchain = ResolvedToolchain::from_version(
        NativeTool::Rustc,
        Path::new(&executable.path),
        b"pre-cancelled-fixture",
    )?;
    let native_work = TemporaryWork::create()?;
    let cancelled = AtomicBool::new(true);
    let mut diagnostic = [0; 128];
    let mut output = [0xa5; 512];
    match compile(
        request(
            b"pub const alpha: bool = true;",
            ToolchainSelection::ResolvedNative(toolchain),
            &cancelled,
            Instant::now() + Duration::from_secs(1),
        ),
        CompileScratch {
            diagnostic_output: &mut diagnostic,
            native_work: native_work.path(),
        },
        CompileOutput {
            fragment_output: &mut output,
        },
    ) {
        Err(CompileFailure::Cancelled { diagnostic, .. }) if diagnostic.bytes.is_empty() => {}
        Err(failure) => {
            return Err(ScriptFailure::CompileTerminal {
                expected: ScriptExpectedTerminal::Cancelled,
                observed: compile_terminal(&failure),
            });
        }
        Ok(_compiled) => {
            return Err(ScriptFailure::CompileTerminal {
                expected: ScriptExpectedTerminal::Cancelled,
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
            language: Language::Rust,
            stage: Stage::Parse,
            source: b"pub const alpha: bool = true;",
            toolchain: ToolchainSelection::ResolvedNative(toolchain),
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

#[test]
fn cleanup_failure_retains_the_exact_native_rejection_terminal() -> Result<(), ScriptFailure> {
    let executable = script(
        b"#!/bin/sh\nIFS= read -r ignored\nprintf x > foreign\nprintf rejected >&2\nexit 1\n",
    )?;
    let toolchain = ResolvedToolchain::from_version(
        NativeTool::Rustc,
        Path::new(&executable.path),
        b"cleanup-failure-fixture",
    )?;
    let native_work = TemporaryWork::create()?;
    let cancelled = AtomicBool::new(false);
    let mut diagnostic = [0; 128];
    let mut output = [0xa5; 512];
    match compile(
        request(
            b"fixture\n",
            ToolchainSelection::ResolvedNative(toolchain),
            &cancelled,
            Instant::now() + Duration::from_secs(1),
        ),
        CompileScratch {
            diagnostic_output: &mut diagnostic,
            native_work: native_work.path(),
        },
        CompileOutput {
            fragment_output: &mut output,
        },
    ) {
        Err(CompileFailure::NativeWorkCleanup {
            primary: NativeWorkPrimary::NativeRejected { diagnostic, .. },
            cleanup: NativeWorkError::NotEmpty,
            ..
        }) if diagnostic.bytes == b"rejected" => {}
        Err(failure) => {
            return Err(ScriptFailure::CompileTerminal {
                expected: ScriptExpectedTerminal::NativeRejectedCleanup,
                observed: compile_terminal(&failure),
            });
        }
        Ok(_compiled) => {
            return Err(ScriptFailure::CompileTerminal {
                expected: ScriptExpectedTerminal::NativeRejectedCleanup,
                observed: CompileTerminal::Compiled,
            });
        }
    }
    fs::remove_file(native_work.path().join("foreign"))
        .map_err(ScriptFailure::RemoveHostileArtifact)?;
    native_work.assert_empty()?;
    if !output.iter().all(|byte| *byte == 0xa5) {
        return Err(ScriptFailure::Invariant {
            invariant: ScriptInvariant::OutputUnchanged,
        });
    }
    Ok(())
}

#[test]
fn noisy_tool_exceeds_the_bounded_diagnostic_lease_before_a_compile_terminal()
-> Result<(), ScriptFailure> {
    let executable =
        script(b"#!/bin/sh\nwhile :; do printf 'aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa' >&2; done\n")?;
    let toolchain = ResolvedToolchain::from_version(
        NativeTool::Rustc,
        Path::new(&executable.path),
        b"noisy-fixture",
    )?;
    let cancelled = AtomicBool::new(false);
    let native_work = TemporaryWork::create()?;
    let mut diagnostic = [0; 32];
    let mut output = [0; 512];
    match compile(
        request(
            b"pub const alpha: bool = true;",
            ToolchainSelection::ResolvedNative(toolchain),
            &cancelled,
            Instant::now() + Duration::from_secs(1),
        ),
        CompileScratch {
            diagnostic_output: &mut diagnostic,
            native_work: native_work.path(),
        },
        CompileOutput {
            fragment_output: &mut output,
        },
    ) {
        Err(CompileFailure::DiagnosticLimit {
            limit,
            observed,
            diagnostic,
            ..
        }) => {
            if limit != 32 {
                return Err(ScriptFailure::Invariant {
                    invariant: ScriptInvariant::DiagnosticLimitValue,
                });
            }
            if observed <= limit {
                return Err(ScriptFailure::Invariant {
                    invariant: ScriptInvariant::DiagnosticObservedLimit,
                });
            }
            if diagnostic.bytes.len() != limit {
                return Err(ScriptFailure::Invariant {
                    invariant: ScriptInvariant::RetainedDiagnosticLength,
                });
            }
            if !diagnostic.truncated {
                return Err(ScriptFailure::Invariant {
                    invariant: ScriptInvariant::DiagnosticTruncated,
                });
            }
        }
        Err(failure) => {
            return Err(ScriptFailure::CompileTerminal {
                expected: ScriptExpectedTerminal::DiagnosticLimit,
                observed: compile_terminal(&failure),
            });
        }
        Ok(_compiled) => {
            return Err(ScriptFailure::CompileTerminal {
                expected: ScriptExpectedTerminal::DiagnosticLimit,
                observed: CompileTerminal::Compiled,
            });
        }
    }
    native_work.assert_empty()?;
    Ok(())
}
