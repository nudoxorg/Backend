use std::{
    ffi::OsString,
    fs,
    io::{self, Read, Write},
    path::Path,
    process::{Child, ChildStderr, ChildStdin, Command, ExitStatus, Stdio},
    sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        mpsc::{Receiver, TryRecvError, sync_channel},
    },
    thread,
    time::Duration,
};

use nudox_compile_vocab::NativeTool;

use crate::types::{
    CompileControl, CompileFailure, CompileRecipeFact, CompileScratch, NativeDiagnostic,
    NativeRecipe, NativeWorkError, NativeWorkPhase, NativeWorkPrimary, ResolvedToolchain,
    SourceIdentity,
};

const RUST_METADATA_FILE: &str = "nudox-probe.rmeta";

trait NativeFrontend {
    fn command(toolchain: ResolvedToolchain<'_>, native_work: &Path) -> Command;
}

struct RustFrontend;
struct ClangFrontend;
struct PythonFrontend;

impl NativeFrontend for RustFrontend {
    fn command(toolchain: ResolvedToolchain<'_>, native_work: &Path) -> Command {
        let mut command = Command::new(toolchain.executable());
        let mut metadata = OsString::from("--emit=metadata=");
        metadata.push(native_work.join(RUST_METADATA_FILE));
        command.args([
            "--crate-type=lib",
            "--edition=2024",
            "--crate-name=nudox_probe",
        ]);
        command.arg(metadata).arg("-").current_dir(native_work);
        command
    }
}

impl NativeFrontend for ClangFrontend {
    fn command(toolchain: ResolvedToolchain<'_>, native_work: &Path) -> Command {
        let mut command = Command::new(toolchain.executable());
        command
            .args(["-x", "c", "-fsyntax-only", "-w", "-"])
            .current_dir(native_work);
        command
    }
}

impl NativeFrontend for PythonFrontend {
    fn command(toolchain: ResolvedToolchain<'_>, native_work: &Path) -> Command {
        let mut command = Command::new(toolchain.executable());
        command.args([
            "-c",
            "import sys; compile(sys.stdin.read(), '<nudox>', 'exec')",
        ]);
        command.current_dir(native_work);
        command
    }
}

pub(crate) fn parse_with_native_tool<'source, 'toolchain, 'cancel, 'diagnostic, 'work>(
    recipe: NativeRecipe<'source, 'toolchain>,
    source: SourceIdentity,
    recipe_fact: CompileRecipeFact,
    scratch: CompileScratch<'diagnostic, 'work>,
    control: CompileControl<'cancel>,
) -> Result<(), CompileFailure<'diagnostic>> {
    match recipe.toolchain.tool {
        NativeTool::Rustc => drive::<RustFrontend>(recipe, source, recipe_fact, scratch, control),
        NativeTool::Clang => drive::<ClangFrontend>(recipe, source, recipe_fact, scratch, control),
        NativeTool::Python => {
            drive::<PythonFrontend>(recipe, source, recipe_fact, scratch, control)
        }
        NativeTool::TypeScriptCompiler
        | NativeTool::GoCompiler
        | NativeTool::JavaCompiler
        | NativeTool::CSharpCompiler => Err(CompileFailure::ToolingUnavailable {
            source_identity: source,
            language: recipe.language,
            stage: recipe.stage,
            tool: recipe.toolchain.tool,
        }),
    }
}

fn drive<'source, 'toolchain, 'cancel, 'diagnostic, 'work, ConcreteFrontend: NativeFrontend>(
    recipe: NativeRecipe<'source, 'toolchain>,
    source: SourceIdentity,
    recipe_fact: CompileRecipeFact,
    scratch: CompileScratch<'diagnostic, 'work>,
    control: CompileControl<'cancel>,
) -> Result<(), CompileFailure<'diagnostic>> {
    prepare_native_work(scratch.native_work).map_err(|cause| CompileFailure::NativeWork {
        source_identity: source,
        recipe: recipe_fact,
        phase: NativeWorkPhase::Prepare,
        cause,
    })?;
    let CompileScratch {
        diagnostic_output,
        native_work,
    } = scratch;
    let result = drive_child::<ConcreteFrontend>(
        recipe,
        source,
        recipe_fact,
        diagnostic_output,
        native_work,
        control,
    );
    match cleanup_native_work(native_work) {
        Ok(()) => result,
        Err(cleanup) => match result {
            Ok(()) => Err(CompileFailure::NativeWork {
                source_identity: source,
                recipe: recipe_fact,
                phase: NativeWorkPhase::Cleanup,
                cause: cleanup,
            }),
            Err(primary) => Err(compound_native_work_cleanup(
                primary,
                source,
                recipe_fact,
                cleanup,
            )),
        },
    }
}

fn drive_child<'source, 'toolchain, 'cancel, 'diagnostic, ConcreteFrontend: NativeFrontend>(
    recipe: NativeRecipe<'source, 'toolchain>,
    source: SourceIdentity,
    recipe_fact: CompileRecipeFact,
    diagnostic_output: &'diagnostic mut [u8],
    native_work: &Path,
    control: CompileControl<'cancel>,
) -> Result<(), CompileFailure<'diagnostic>> {
    let diagnostic_limit_bytes = diagnostic_output.len();
    if control.cancelled.load(Ordering::Acquire) {
        return Err(CompileFailure::Cancelled {
            source_identity: source,
            recipe: recipe_fact,
            diagnostic: NativeDiagnostic {
                bytes: &diagnostic_output[..0],
                truncated: false,
            },
        });
    }
    if std::time::Instant::now() >= control.deadline {
        return Err(CompileFailure::DeadlineExceeded {
            source_identity: source,
            recipe: recipe_fact,
            diagnostic: NativeDiagnostic {
                bytes: &diagnostic_output[..0],
                truncated: false,
            },
        });
    }
    let mut command = ConcreteFrontend::command(recipe.toolchain, native_work);
    command
        .stdin(Stdio::piped())
        // Native adapter stdout is non-semantic and discarded by the operating system rather
        // than buffered in this process. Stderr is the separately bounded diagnostic stream.
        .stdout(Stdio::null())
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
    let stdin = match child.stdin.take() {
        Some(stdin) => stdin,
        None => return missing_input(&mut child, source, recipe_fact),
    };
    let diagnostic_limit = AtomicBool::new(false);
    let observed_diagnostic = AtomicUsize::new(0);
    thread::scope(|scope| {
        let reader = scope.spawn(|| {
            read_diagnostic(
                stderr,
                diagnostic_output,
                &diagnostic_limit,
                &observed_diagnostic,
            )
        });
        let (writer_done, writer_status) = sync_channel(1);
        let writer = scope.spawn(move || {
            let _sent = writer_done.send(write_source(stdin, recipe.source));
        });
        let terminal = wait_for_terminal(
            &mut child,
            &control,
            &diagnostic_limit,
            &writer_status,
            source,
            recipe_fact,
        );
        // Terminal observation is attempted before either scoped worker is joined. Ordinary
        // terminals and successful interruption are reaped; an exact interruption I/O error can
        // instead mean reaping was not proved. Scoped ownership still requires both joins, and a
        // worker panic resumes its original payload rather than becoming a fabricated compile
        // terminal.
        match writer.join() {
            Ok(()) => {}
            Err(payload) => std::panic::resume_unwind(payload),
        }
        let diagnostic = match reader.join() {
            Ok(diagnostic) => diagnostic,
            Err(payload) => std::panic::resume_unwind(payload),
        };
        let diagnostic = diagnostic.map_err(|cause| CompileFailure::ToolDiagnosticRead {
            source_identity: source,
            recipe: recipe_fact,
            cause,
        })?;
        let observed = observed_diagnostic.load(Ordering::Acquire);
        if diagnostic_limit.load(Ordering::Acquire) {
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

fn prepare_native_work(native_work: &Path) -> Result<(), NativeWorkError> {
    if !native_work.is_absolute() {
        return Err(NativeWorkError::RelativeDirectory);
    }
    let mut entries = fs::read_dir(native_work).map_err(NativeWorkError::Inspect)?;
    match entries.next() {
        Some(Ok(_entry)) => Err(NativeWorkError::NotEmpty),
        Some(Err(cause)) => Err(NativeWorkError::Inspect(cause)),
        None => Ok(()),
    }
}

fn cleanup_native_work(native_work: &Path) -> Result<(), NativeWorkError> {
    let metadata = native_work.join(RUST_METADATA_FILE);
    match fs::remove_file(metadata) {
        Ok(()) => {}
        Err(cause) if cause.kind() == io::ErrorKind::NotFound => {}
        Err(cause) => return Err(NativeWorkError::RemoveMetadata(cause)),
    }
    prepare_native_work(native_work)
}

fn compound_native_work_cleanup<'diagnostic>(
    primary: CompileFailure<'diagnostic>,
    source: SourceIdentity,
    recipe: CompileRecipeFact,
    cleanup: NativeWorkError,
) -> CompileFailure<'diagnostic> {
    let primary = match primary {
        CompileFailure::ToolStart { cause, .. } => NativeWorkPrimary::ToolStart { cause },
        CompileFailure::MissingToolInput { .. } => NativeWorkPrimary::MissingToolInput,
        CompileFailure::MissingToolInputCleanup { cleanup, .. } => {
            NativeWorkPrimary::MissingToolInputCleanup { cleanup }
        }
        CompileFailure::MissingToolDiagnostic { .. } => NativeWorkPrimary::MissingToolDiagnostic,
        CompileFailure::MissingToolDiagnosticCleanup { cleanup, .. } => {
            NativeWorkPrimary::MissingToolDiagnosticCleanup { cleanup }
        }
        CompileFailure::ToolInput { cause, .. } => NativeWorkPrimary::ToolInput { cause },
        CompileFailure::ToolInputCleanup { cause, cleanup, .. } => {
            NativeWorkPrimary::ToolInputCleanup { cause, cleanup }
        }
        CompileFailure::ToolTerminate { cause, .. } => NativeWorkPrimary::ToolTerminate { cause },
        CompileFailure::ToolWait { cause, .. } => NativeWorkPrimary::ToolWait { cause },
        CompileFailure::ToolWaitCleanup { cause, cleanup, .. } => {
            NativeWorkPrimary::ToolWaitCleanup { cause, cleanup }
        }
        CompileFailure::ToolDiagnosticRead { cause, .. } => {
            NativeWorkPrimary::ToolDiagnosticRead { cause }
        }
        CompileFailure::Cancelled { diagnostic, .. } => NativeWorkPrimary::Cancelled { diagnostic },
        CompileFailure::DeadlineExceeded { diagnostic, .. } => {
            NativeWorkPrimary::DeadlineExceeded { diagnostic }
        }
        CompileFailure::DiagnosticLimit {
            limit,
            observed,
            diagnostic,
            ..
        } => NativeWorkPrimary::DiagnosticLimit {
            limit,
            observed,
            diagnostic,
        },
        CompileFailure::NativeRejected {
            status, diagnostic, ..
        } => NativeWorkPrimary::NativeRejected { status, diagnostic },
        CompileFailure::SourceLength { .. }
        | CompileFailure::UnsupportedStage { .. }
        | CompileFailure::ToolchainSelectionMismatch { .. }
        | CompileFailure::ToolchainMismatch { .. }
        | CompileFailure::NativeWork { .. }
        | CompileFailure::NativeWorkCleanup { .. }
        | CompileFailure::ToolingUnavailable { .. }
        | CompileFailure::LoweringUnsupported { .. }
        | CompileFailure::Prepare { .. }
        | CompileFailure::Write { .. }
        | CompileFailure::Validate { .. } => return primary,
    };
    CompileFailure::NativeWorkCleanup {
        source_identity: source,
        recipe,
        primary,
        cleanup,
    }
}

enum ChildTerminal {
    Success,
    Rejected(ExitStatus),
    InputFailed(io::Error),
    Cancelled,
    DeadlineExceeded,
    DiagnosticLimit,
}

fn missing_diagnostic<'diagnostic>(
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

fn missing_input<'diagnostic>(
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

fn wait_for_terminal<'diagnostic>(
    child: &mut Child,
    control: &CompileControl<'_>,
    diagnostic_limit: &AtomicBool,
    writer_status: &Receiver<Result<(), io::Error>>,
    source: SourceIdentity,
    recipe: CompileRecipeFact,
) -> Result<ChildTerminal, CompileFailure<'diagnostic>> {
    let mut source_delivered = false;
    loop {
        if !source_delivered {
            match writer_status.try_recv() {
                Ok(Ok(())) => source_delivered = true,
                Ok(Err(cause)) => return input_failed(child, source, recipe, cause),
                Err(TryRecvError::Empty) => {}
                // The only sender lives in the scoped writer. If it disconnected through a
                // panic, the owner still attempts terminal observation before joining; the join
                // then resumes the exact panic payload rather than fabricating a compiler error.
                Err(TryRecvError::Disconnected) => {}
            }
        }
        if diagnostic_limit.load(Ordering::Acquire) {
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

fn input_failed<'diagnostic>(
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

fn terminate(child: &mut Child) -> Result<(), io::Error> {
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

fn write_source(mut stdin: ChildStdin, source: &[u8]) -> Result<(), io::Error> {
    stdin.write_all(source)
}

fn read_diagnostic<'diagnostic>(
    mut stderr: ChildStderr,
    output: &'diagnostic mut [u8],
    diagnostic_limit: &AtomicBool,
    observed: &AtomicUsize,
) -> Result<NativeDiagnostic<'diagnostic>, io::Error> {
    let mut retained = 0;
    let mut truncated = false;
    let mut chunk = [0; 512];
    loop {
        let read = stderr.read(&mut chunk)?;
        if read == 0 {
            return Ok(NativeDiagnostic {
                bytes: &output[..retained],
                truncated,
            });
        }
        let observed_total = observed.fetch_add(read, Ordering::AcqRel) + read;
        let room = output.len().saturating_sub(retained);
        let copied = room.min(read);
        output[retained..retained + copied].copy_from_slice(&chunk[..copied]);
        retained += copied;
        truncated |= copied != read;
        if observed_total > output.len() {
            diagnostic_limit.store(true, Ordering::Release);
        }
    }
}
