//! Defines native child behavior for the `backend-engine` driver, whose purpose is to run bounded native toolchains and lower their output into canonical IR.
//! This module owns the native child invariants and typed state transitions.
//! Its narrow surface prevents representation and policy details from leaking outward.
use std::{
    path::Path,
    process::Stdio,
    sync::{atomic::Ordering, mpsc::sync_channel},
    thread,
};

use crate::driver::types::{
    CompileControl, CompileFailure, CompileRecipeFact, NativeDiagnostic, NativeRecipe,
    NativeWorker, NativeWorkerPanic, SourceIdentity,
};

use super::{
    diagnostic::{DiagnosticCollector, read_diagnostic},
    frontend::NativeFrontend,
    terminal::{ChildTerminal, missing_diagnostic, missing_input, wait_for_terminal, write_source},
};

pub(super) fn drive_child<
    'source,
    'toolchain,
    'cancel,
    'diagnostic,
    ConcreteFrontend: NativeFrontend,
>(
    profile: ConcreteFrontend::Profile,
    recipe: NativeRecipe<'source, 'toolchain>,
    source: SourceIdentity,
    recipe_fact: CompileRecipeFact,
    diagnostic_output: &'diagnostic mut [u8],
    native_work: &Path,
    control: CompileControl<'cancel>,
) -> Result<(), CompileFailure<'diagnostic>> {
    let diagnostic_limit_bytes = diagnostic_output.len();
    let empty_diagnostic: &'diagnostic [u8] = &[];
    if control.cancelled.load(Ordering::Acquire) {
        return Err(CompileFailure::Cancelled {
            source_identity: source,
            recipe: recipe_fact,
            diagnostic: NativeDiagnostic {
                bytes: empty_diagnostic,
                observed: 0,
                truncated: false,
            },
        });
    }
    if std::time::Instant::now() >= control.deadline {
        return Err(CompileFailure::DeadlineExceeded {
            source_identity: source,
            recipe: recipe_fact,
            diagnostic: NativeDiagnostic {
                bytes: empty_diagnostic,
                observed: 0,
                truncated: false,
            },
        });
    }
    let mut command = ConcreteFrontend::command(profile, recipe.toolchain, native_work);
    command
        .stdin(Stdio::piped())
        // Some native CLIs report compiler diagnostics on stdout while others use stderr. Both
        // streams are drained into one caller-owned bounded diagnostic lease.
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = command.spawn().map_err(|cause| CompileFailure::ToolStart {
        source_identity: source,
        recipe: recipe_fact,
        cause,
    })?;
    let stderr = match child.stderr.take() {
        Some(stderr) => stderr,
        None => return missing_diagnostic(&mut child, source, recipe_fact),
    };
    let stdout = match child.stdout.take() {
        Some(stdout) => stdout,
        None => return missing_diagnostic(&mut child, source, recipe_fact),
    };
    let stdin = match child.stdin.take() {
        Some(stdin) => stdin,
        None => return missing_input(&mut child, source, recipe_fact),
    };
    let stdin_source = if ConcreteFrontend::source_via_stdin() {
        recipe.source
    } else {
        b""
    };
    thread::scope(|scope| {
        let (diagnostic_done, diagnostic_status) = sync_channel(4);
        let stderr_done = diagnostic_done.clone();
        let stderr_reader = scope.spawn(move || read_diagnostic(stderr, stderr_done));
        let stdout_reader = scope.spawn(move || read_diagnostic(stdout, diagnostic_done));
        let (writer_done, writer_status) = sync_channel(1);
        let writer =
            scope.spawn(
                move || match writer_done.send(write_source(stdin, stdin_source)) {
                    Ok(()) => Ok(()),
                    Err(std::sync::mpsc::SendError(cause)) => cause,
                },
            );
        let mut diagnostic = DiagnosticCollector::new(diagnostic_output, 2);
        let terminal = wait_for_terminal(
            &mut child,
            &control,
            &writer_status,
            &diagnostic_status,
            &mut diagnostic,
            source,
            recipe_fact,
        );
        // Terminal observation is attempted before scoped workers are joined. Ordinary terminals
        // and successful interruption are reaped; an exact interruption I/O error can instead
        // mean reaping was not proved. Every worker is then joined before an error is returned so
        // no scoped panic can resume into the compiler caller.
        // Drain before joining the readers: a reader can be blocked on the bounded channel after
        // a native child exits, and joining it first would deadlock the completed compiler call.
        let diagnostic_result = diagnostic.finish(&diagnostic_status);
        let mut worker_panic_cause = None;
        let mut input_cause = None;
        match writer.join() {
            Ok(Ok(())) => {}
            Ok(Err(cause)) => input_cause = Some(cause),
            Err(payload) => {
                worker_panic_cause = Some(NativeWorkerPanic::capture(
                    NativeWorker::SourceWriter,
                    payload.as_ref(),
                ));
            }
        }
        for (worker, reader) in [
            (NativeWorker::StandardErrorReader, stderr_reader),
            (NativeWorker::StandardOutputReader, stdout_reader),
        ] {
            if let Err(payload) = reader.join() {
                worker_panic_cause
                    .get_or_insert_with(|| NativeWorkerPanic::capture(worker, payload.as_ref()));
            }
        }
        if let Some(cause) = worker_panic_cause {
            return Err(CompileFailure::NativeWorkerPanic {
                source_identity: source,
                recipe: recipe_fact,
                cause,
            });
        }
        if let Some(cause) = input_cause {
            return Err(CompileFailure::ToolInput {
                source_identity: source,
                recipe: recipe_fact,
                cause,
            });
        }
        diagnostic_result.map_err(|cause| CompileFailure::ToolDiagnosticRead {
            source_identity: source,
            recipe: recipe_fact,
            cause,
        })?;
        let observed = diagnostic.observed;
        let limit_exceeded = diagnostic.limit_exceeded;
        let diagnostic = diagnostic.into_view();
        if limit_exceeded {
            return Err(CompileFailure::DiagnosticLimit {
                source_identity: source,
                recipe: recipe_fact,
                limit: diagnostic_limit_bytes,
                observed,
                diagnostic,
            });
        }
        match terminal? {
            ChildTerminal::InputFailed(cause) => Err(CompileFailure::ToolInput {
                source_identity: source,
                recipe: recipe_fact,
                cause,
            }),
            ChildTerminal::Cancelled => Err(CompileFailure::Cancelled {
                source_identity: source,
                recipe: recipe_fact,
                diagnostic,
            }),
            ChildTerminal::DeadlineExceeded => Err(CompileFailure::DeadlineExceeded {
                source_identity: source,
                recipe: recipe_fact,
                diagnostic,
            }),
            ChildTerminal::DiagnosticLimit => Err(CompileFailure::DiagnosticLimit {
                source_identity: source,
                recipe: recipe_fact,
                limit: diagnostic_limit_bytes,
                observed,
                diagnostic,
            }),
            ChildTerminal::Success => Ok(()),
            ChildTerminal::Rejected(status) => Err(CompileFailure::NativeRejected {
                source_identity: source,
                recipe: recipe_fact,
                status,
                diagnostic,
            }),
        }
    })
}
