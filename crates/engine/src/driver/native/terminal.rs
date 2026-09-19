//! Defines native terminal behavior for the `backend-engine` driver, whose purpose is to run bounded native toolchains and lower their output into canonical IR.
//! This module owns the native terminal invariants and typed state transitions.
//! Its narrow surface prevents representation and policy details from leaking outward.
use std::{
    io::{self, Write},
    process::{Child, ChildStdin, ExitStatus},
    sync::{
        atomic::Ordering,
        mpsc::{Receiver, TryRecvError},
    },
    thread,
    time::Duration,
};

use crate::driver::types::{CompileControl, CompileFailure, CompileRecipeFact, SourceIdentity};

use super::diagnostic::{DiagnosticCollector, DiagnosticMessage};

pub(super) enum ChildTerminal {
    Success,
    Rejected(ExitStatus),
    InputFailed(io::Error),
    Cancelled,
    DeadlineExceeded,
    DiagnosticLimit,
}

pub(super) fn missing_diagnostic<'diagnostic>(
    child: &mut Child,
    source: SourceIdentity,
    recipe: CompileRecipeFact,
) -> Result<(), CompileFailure<'diagnostic>> {
    match terminate(child) {
        Ok(()) => Err(CompileFailure::MissingToolDiagnostic {
            source_identity: source,
            recipe,
        }),
        Err(cleanup) => Err(CompileFailure::MissingToolDiagnosticCleanup {
            source_identity: source,
            recipe,
            cleanup,
        }),
    }
}

pub(super) fn missing_input<'diagnostic>(
    child: &mut Child,
    source: SourceIdentity,
    recipe: CompileRecipeFact,
) -> Result<(), CompileFailure<'diagnostic>> {
    match terminate(child) {
        Ok(()) => Err(CompileFailure::MissingToolInput {
            source_identity: source,
            recipe,
        }),
        Err(cleanup) => Err(CompileFailure::MissingToolInputCleanup {
            source_identity: source,
            recipe,
            cleanup,
        }),
    }
}

pub(super) fn wait_for_terminal<'diagnostic>(
    child: &mut Child,
    control: &CompileControl<'_>,
    writer_status: &Receiver<Result<(), io::Error>>,
    diagnostic_status: &Receiver<DiagnosticMessage>,
    diagnostic: &mut DiagnosticCollector<'_>,
    source: SourceIdentity,
    recipe: CompileRecipeFact,
) -> Result<ChildTerminal, CompileFailure<'diagnostic>> {
    let mut source_delivered = false;
    loop {
        if let Err(cause) = diagnostic.drain(diagnostic_status) {
            return match terminate(child) {
                Ok(()) => Err(CompileFailure::ToolDiagnosticRead {
                    source_identity: source,
                    recipe,
                    cause,
                }),
                Err(cleanup) => Err(CompileFailure::ToolDiagnosticReadCleanup {
                    source_identity: source,
                    recipe,
                    cause,
                    cleanup,
                }),
            };
        }
        if !source_delivered {
            match writer_status.try_recv() {
                Ok(Ok(())) => source_delivered = true,
                Ok(Err(cause)) => return input_failed(child, source, recipe, cause),
                Err(TryRecvError::Empty) => {}
                // The only sender lives in the scoped writer. If it disconnected through a
                // panic, the owner still attempts terminal observation before joining; the join
                // then translates it to the closed native worker-panic terminal.
                Err(TryRecvError::Disconnected) => {}
            }
        }
        if diagnostic.limit_exceeded {
            terminate(child).map_err(|cause| CompileFailure::ToolTerminate {
                source_identity: source,
                recipe,
                cause,
            })?;
            return Ok(ChildTerminal::DiagnosticLimit);
        }
        if control.cancelled.load(Ordering::Acquire) {
            terminate(child).map_err(|cause| CompileFailure::ToolTerminate {
                source_identity: source,
                recipe,
                cause,
            })?;
            return Ok(ChildTerminal::Cancelled);
        }
        if std::time::Instant::now() >= control.deadline {
            terminate(child).map_err(|cause| CompileFailure::ToolTerminate {
                source_identity: source,
                recipe,
                cause,
            })?;
            return Ok(ChildTerminal::DeadlineExceeded);
        }
        match child.try_wait() {
            Ok(Some(status)) if status.success() => return Ok(ChildTerminal::Success),
            Ok(Some(status)) => return Ok(ChildTerminal::Rejected(status)),
            Ok(None) => thread::sleep(Duration::from_millis(1)),
            Err(cause) => {
                return match terminate(child) {
                    Ok(()) => Err(CompileFailure::ToolWait {
                        source_identity: source,
                        recipe,
                        cause,
                    }),
                    Err(cleanup) => Err(CompileFailure::ToolWaitCleanup {
                        source_identity: source,
                        recipe,
                        cause,
                        cleanup,
                    }),
                };
            }
        }
    }
}

pub(super) fn input_failed<'diagnostic>(
    child: &mut Child,
    source: SourceIdentity,
    recipe: CompileRecipeFact,
    cause: io::Error,
) -> Result<ChildTerminal, CompileFailure<'diagnostic>> {
    match terminate(child) {
        Ok(()) => Ok(ChildTerminal::InputFailed(cause)),
        Err(cleanup) => Err(CompileFailure::ToolInputCleanup {
            source_identity: source,
            recipe,
            cause,
            cleanup,
        }),
    }
}

pub(super) fn terminate(child: &mut Child) -> Result<(), io::Error> {
    match child.kill() {
        Ok(()) => {
            let _status = child.wait()?;
            Ok(())
        }
        Err(kill) => match child.try_wait() {
            Ok(Some(_status)) => Ok(()),
            Ok(None) => Err(kill),
            Err(wait) => Err(wait),
        },
    }
}

pub(super) fn write_source(mut stdin: ChildStdin, source: &[u8]) -> Result<(), io::Error> {
    stdin.write_all(source)
}
