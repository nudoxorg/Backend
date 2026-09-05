//! Bounded admission probe for explicit native toolchain executables.
//!
//! Version bytes are recipe authority, so startup must obtain them without an ambient executable
//! search and without allowing a child to own unbounded output or time. This module owns that one
//! process invariant; language adapters receive only the resulting absolute executable and typed
//! identity.

use std::{
    collections::TryReserveError,
    io::{self, Read},
    num::NonZeroUsize,
    ops::Deref,
    path::{Path, PathBuf},
    process::{Child, Command, ExitStatus, Stdio},
    sync::mpsc::{Sender, TryRecvError, channel},
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

use compiler_driver::ToolchainResolutionError;
use compiler_vocabulary::{NativeTool, NativeWorker, NativeWorkerPanic};
use thiserror::Error;

const READ_CHUNK_BYTES: usize = 4096;
const PROBE_POLL_INTERVAL: Duration = Duration::from_millis(2);

/// Immutable validated bounds for one native version probe.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ToolchainProbeLimitsView {
    /// Maximum wall-clock duration owned by the child transaction.
    pub timeout: Duration,
    /// Maximum independently retained bytes for stdout and stderr.
    pub maximum_stream_bytes: NonZeroUsize,
}

/// Validated probe bounds exposed through an immutable view.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ToolchainProbeLimits {
    view: ToolchainProbeLimitsView,
}

impl ToolchainProbeLimits {
    /// Admits nonzero time and stream bounds.
    pub const fn new(
        timeout: Duration,
        maximum_stream_bytes: NonZeroUsize,
    ) -> Result<Self, ToolchainProbeLimitError> {
        if timeout.is_zero() {
            return Err(ToolchainProbeLimitError::ZeroTimeout);
        }
        Ok(Self {
            view: ToolchainProbeLimitsView {
                timeout,
                maximum_stream_bytes,
            },
        })
    }
}

impl Deref for ToolchainProbeLimits {
    type Target = ToolchainProbeLimitsView;

    fn deref(&self) -> &Self::Target {
        &self.view
    }
}

/// Rejection while validating version-probe resource bounds.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum ToolchainProbeLimitError {
    /// An immediate deadline cannot establish executable authority.
    #[error("toolchain version probe timeout must be nonzero")]
    ZeroTimeout,
}

/// Primary process outcome that may also be retained by a cleanup or stream fault.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ToolchainProbePrimary {
    /// A named child stream crossed the admitted byte cap.
    OutputLimit {
        /// Stream which first crossed the bound.
        worker: NativeWorker,
        /// Bytes observed through the crossing read.
        observed: usize,
        /// Admitted maximum for each stream.
        maximum: usize,
    },
    /// The child remained live past its admitted interval.
    Deadline {
        /// Exact configured interval.
        timeout: Duration,
    },
}

/// Closed cleanup operation that failed after a probe had already reached a terminal.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ToolchainProbeCleanupAction {
    /// Terminating the still-live process failed.
    Terminate,
    /// Reaping the terminated or already-exited process failed.
    Reap,
}

/// Exact bounded-reader failure for one named child stream.
#[derive(Debug, Error)]
pub enum ToolchainProbeStreamError {
    /// Exact retained-output allocation failed before reading.
    #[error("{worker:?} version-probe output allocation failed")]
    Allocation {
        /// Named child stream.
        worker: NativeWorker,
        /// Exact standard allocation cause.
        #[source]
        source: TryReserveError,
    },
    /// The operating system rejected a stream read.
    #[error("{worker:?} version-probe read failed")]
    Read {
        /// Named child stream.
        worker: NativeWorker,
        /// Exact operating-system cause.
        #[source]
        source: io::Error,
    },
    /// The bounded stream worker panicked with its original supported payload facts.
    #[error("version-probe stream worker panicked: {cause}")]
    WorkerPanic {
        /// Named worker and bounded original payload.
        #[source]
        cause: NativeWorkerPanic,
    },
}

/// Exact terminal while establishing one explicit native-tool version identity.
#[derive(Debug, Error)]
pub enum ToolchainProbeError {
    /// Relative execution would consult ambient process state.
    #[error("{tool:?} version probe executable is relative: {executable:?}")]
    RelativeExecutable {
        /// Closed tool family.
        tool: NativeTool,
        /// Rejected caller path.
        executable: PathBuf,
    },
    /// The absolute executable could not be started.
    #[error("could not start {tool:?} version probe at {executable:?}")]
    Spawn {
        /// Closed tool family.
        tool: NativeTool,
        /// Exact selected absolute path.
        executable: PathBuf,
        /// Original operating-system cause.
        #[source]
        source: io::Error,
    },
    /// A named reader thread could not be created.
    #[error("could not start {worker:?} reader for {tool:?} version probe")]
    ReaderSpawn {
        /// Closed tool family.
        tool: NativeTool,
        /// Named reader.
        worker: NativeWorker,
        /// Original operating-system cause.
        #[source]
        source: io::Error,
    },
    /// A piped stream promised to the child transaction was absent.
    #[error("{tool:?} version probe did not expose {worker:?}")]
    MissingStream {
        /// Closed tool family.
        tool: NativeTool,
        /// Missing named stream.
        worker: NativeWorker,
    },
    /// Polling the child process failed.
    #[error("could not observe {tool:?} version probe")]
    Wait {
        /// Closed tool family.
        tool: NativeTool,
        /// Original operating-system cause.
        #[source]
        source: io::Error,
    },
    /// Process cleanup failed while retaining the exact terminal that triggered cleanup.
    #[error("could not {action:?} {tool:?} version probe after {preceding}")]
    Cleanup {
        /// Closed tool family.
        tool: NativeTool,
        /// Closed cleanup operation.
        action: ToolchainProbeCleanupAction,
        /// Complete earlier probe terminal.
        preceding: Box<ToolchainProbeError>,
        /// Original operating-system cause.
        #[source]
        source: io::Error,
    },
    /// A bounded stream failed, retaining any process terminal that caused cancellation.
    #[error("{tool:?} version probe stream failed: {source}")]
    Stream {
        /// Closed tool family.
        tool: NativeTool,
        /// Complete earlier process or cleanup terminal, when one existed.
        preceding: Option<Box<ToolchainProbeError>>,
        /// Complete original stream cause.
        #[source]
        source: ToolchainProbeStreamError,
    },
    /// Both independent bounded streams failed and both exact causes are retained.
    #[error("{tool:?} version probe stdout and stderr streams both failed")]
    Streams {
        /// Closed tool family.
        tool: NativeTool,
        /// Complete earlier process or cleanup terminal, when one existed.
        preceding: Option<Box<ToolchainProbeError>>,
        /// Complete standard-output reader cause.
        stdout: ToolchainProbeStreamError,
        /// Complete standard-error reader cause.
        stderr: ToolchainProbeStreamError,
    },
    /// The probe crossed an admitted process resource bound and was reaped.
    #[error("{tool:?} version probe rejected by {primary:?}")]
    Bounded {
        /// Closed tool family.
        tool: NativeTool,
        /// Exact first deadline or output-cap terminal.
        primary: ToolchainProbePrimary,
    },
    /// Internal child observation completed without a process status or another terminal.
    #[error("{tool:?} version probe completed without an exit status")]
    MissingStatus {
        /// Closed tool family.
        tool: NativeTool,
    },
    /// The executable exited unsuccessfully with both complete bounded streams retained.
    #[error("{tool:?} version probe exited with {status}")]
    Exit {
        /// Closed tool family.
        tool: NativeTool,
        /// Exact process status.
        status: ExitStatus,
        /// Complete bounded stdout bytes.
        stdout: Box<[u8]>,
        /// Complete bounded stderr bytes.
        stderr: Box<[u8]>,
    },
    /// A successful executable provided no identity bytes on either stream.
    #[error("{tool:?} version probe returned empty stdout and stderr")]
    Empty {
        /// Closed tool family.
        tool: NativeTool,
    },
    /// The admitted absolute path could not be rebound to its identity.
    #[error("{tool:?} version identity could not be bound to its executable")]
    Resolution {
        /// Closed tool family.
        tool: NativeTool,
        /// Exact toolchain admission cause.
        #[source]
        source: ToolchainResolutionError,
    },
}

pub(crate) fn probe_version(
    tool: NativeTool,
    executable: &Path,
    limits: ToolchainProbeLimits,
) -> Result<Box<[u8]>, ToolchainProbeError> {
    if !executable.is_absolute() {
        return Err(ToolchainProbeError::RelativeExecutable {
            tool,
            executable: executable.to_path_buf(),
        });
    }
    let mut command = Command::new(executable);
    command
        .arg(version_argument(tool))
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = command
        .spawn()
        .map_err(|source| ToolchainProbeError::Spawn {
            tool,
            executable: executable.to_path_buf(),
            source,
        })?;
    let stdout = match child.stdout.take() {
        Some(stdout) => stdout,
        None => {
            let terminal = ToolchainProbeError::MissingStream {
                tool,
                worker: NativeWorker::StandardOutputReader,
            };
            return Err(terminate_and_reap(tool, &mut child, terminal));
        }
    };
    let stderr = match child.stderr.take() {
        Some(stderr) => stderr,
        None => {
            let terminal = ToolchainProbeError::MissingStream {
                tool,
                worker: NativeWorker::StandardErrorReader,
            };
            return Err(terminate_and_reap(tool, &mut child, terminal));
        }
    };
    let (limit_sender, limit_receiver) = channel();
    let stdout_sender = limit_sender.clone();
    let stdout_worker = match spawn_reader(
        tool,
        NativeWorker::StandardOutputReader,
        stdout,
        limits.maximum_stream_bytes,
        stdout_sender,
    ) {
        Ok(worker) => worker,
        Err(terminal) => return Err(terminate_and_reap(tool, &mut child, terminal)),
    };
    let stderr_worker = match spawn_reader(
        tool,
        NativeWorker::StandardErrorReader,
        stderr,
        limits.maximum_stream_bytes,
        limit_sender,
    ) {
        Ok(worker) => worker,
        Err(terminal) => {
            let preceding = terminate_and_reap(tool, &mut child, terminal);
            return Err(
                match join_reader(stdout_worker, NativeWorker::StandardOutputReader) {
                    Ok(_stdout) => preceding,
                    Err(source) => ToolchainProbeError::Stream {
                        tool,
                        preceding: Some(Box::new(preceding)),
                        source,
                    },
                },
            );
        }
    };

    let started = Instant::now();
    let mut preceding = None;
    let mut status = None;
    loop {
        match limit_receiver.try_recv() {
            Ok(primary) => {
                preceding = Some(terminate_and_reap(
                    tool,
                    &mut child,
                    ToolchainProbeError::Bounded { tool, primary },
                ));
                break;
            }
            Err(TryRecvError::Empty) => {}
            Err(TryRecvError::Disconnected) => {}
        }
        match child.try_wait() {
            Ok(Some(observed)) => {
                status = Some(observed);
                break;
            }
            Ok(None) => {}
            Err(source) => {
                preceding = Some(terminate_and_reap(
                    tool,
                    &mut child,
                    ToolchainProbeError::Wait { tool, source },
                ));
                break;
            }
        }
        if started.elapsed() >= limits.timeout {
            let primary = ToolchainProbePrimary::Deadline {
                timeout: limits.timeout,
            };
            preceding = Some(terminate_and_reap(
                tool,
                &mut child,
                ToolchainProbeError::Bounded { tool, primary },
            ));
            break;
        }
        thread::sleep(PROBE_POLL_INTERVAL);
    }

    let stdout_result = join_reader(stdout_worker, NativeWorker::StandardOutputReader);
    let stderr_result = join_reader(stderr_worker, NativeWorker::StandardErrorReader);
    let (stdout, stderr) = match (stdout_result, stderr_result) {
        (Ok(stdout), Ok(stderr)) => (stdout, stderr),
        (Err(stdout), Err(stderr)) => {
            return Err(ToolchainProbeError::Streams {
                tool,
                preceding: preceding.map(Box::new),
                stdout,
                stderr,
            });
        }
        (Err(source), Ok(_stderr)) | (Ok(_stderr), Err(source)) => {
            return Err(ToolchainProbeError::Stream {
                tool,
                preceding: preceding.map(Box::new),
                source,
            });
        }
    };
    if let Some(terminal) = preceding {
        return Err(terminal);
    }
    let status = status.ok_or(ToolchainProbeError::MissingStatus { tool })?;
    if !status.success() {
        return Err(ToolchainProbeError::Exit {
            tool,
            status,
            stdout,
            stderr,
        });
    }
    if !stdout.is_empty() {
        return Ok(stdout);
    }
    if !stderr.is_empty() {
        return Ok(stderr);
    }
    Err(ToolchainProbeError::Empty { tool })
}

fn version_argument(tool: NativeTool) -> &'static str {
    match tool {
        NativeTool::GoCompiler => "version",
        NativeTool::Rustc
        | NativeTool::Clang
        | NativeTool::Python
        | NativeTool::TypeScriptCompiler
        | NativeTool::JavaCompiler
        | NativeTool::CSharpCompiler => "--version",
    }
}

fn spawn_reader<Reader: Read + Send + 'static>(
    tool: NativeTool,
    worker: NativeWorker,
    reader: Reader,
    maximum: NonZeroUsize,
    limit_sender: Sender<ToolchainProbePrimary>,
) -> Result<JoinHandle<Result<Box<[u8]>, ToolchainProbeStreamError>>, ToolchainProbeError> {
    thread::Builder::new()
        .name(
            match worker {
                NativeWorker::SourceWriter => "nudox-toolchain-version-source",
                NativeWorker::StandardOutputReader => "nudox-toolchain-version-stdout",
                NativeWorker::StandardErrorReader => "nudox-toolchain-version-stderr",
            }
            .to_owned(),
        )
        .spawn(move || read_bounded(reader, worker, maximum, &limit_sender))
        .map_err(|source| ToolchainProbeError::ReaderSpawn {
            tool,
            worker,
            source,
        })
}

fn read_bounded(
    mut reader: impl Read,
    worker: NativeWorker,
    maximum: NonZeroUsize,
    limit_sender: &Sender<ToolchainProbePrimary>,
) -> Result<Box<[u8]>, ToolchainProbeStreamError> {
    let mut bytes = Vec::new();
    bytes
        .try_reserve_exact(maximum.get())
        .map_err(|source| ToolchainProbeStreamError::Allocation { worker, source })?;
    let mut buffer = [0_u8; READ_CHUNK_BYTES];
    let mut observed = 0_usize;
    loop {
        let read = reader
            .read(&mut buffer)
            .map_err(|source| ToolchainProbeStreamError::Read { worker, source })?;
        if read == 0 {
            return Ok(bytes.into_boxed_slice());
        }
        observed = observed.checked_add(read).unwrap_or(usize::MAX);
        let remaining = maximum.get().saturating_sub(bytes.len());
        bytes.extend_from_slice(&buffer[..read.min(remaining)]);
        if observed > maximum.get() {
            let _sent = limit_sender.send(ToolchainProbePrimary::OutputLimit {
                worker,
                observed,
                maximum: maximum.get(),
            });
            return Ok(bytes.into_boxed_slice());
        }
    }
}

fn join_reader(
    worker: JoinHandle<Result<Box<[u8]>, ToolchainProbeStreamError>>,
    identity: NativeWorker,
) -> Result<Box<[u8]>, ToolchainProbeStreamError> {
    worker
        .join()
        .map_err(|payload| ToolchainProbeStreamError::WorkerPanic {
            cause: NativeWorkerPanic::capture(identity, payload.as_ref()),
        })?
}

fn terminate_and_reap(
    tool: NativeTool,
    child: &mut Child,
    preceding: ToolchainProbeError,
) -> ToolchainProbeError {
    if let Err(source) = child.kill()
        && source.kind() != io::ErrorKind::InvalidInput
    {
        return ToolchainProbeError::Cleanup {
            tool,
            action: ToolchainProbeCleanupAction::Terminate,
            preceding: Box::new(preceding),
            source,
        };
    }
    match child.wait() {
        Ok(_status) => preceding,
        Err(source) => ToolchainProbeError::Cleanup {
            tool,
            action: ToolchainProbeCleanupAction::Reap,
            preceding: Box::new(preceding),
            source,
        },
    }
}
