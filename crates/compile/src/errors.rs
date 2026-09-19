//! Typed failures shared by protocol, pooling, cancellation, and processes.

use std::fmt;

/// Failure while decoding a versioned authority frame.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FrameError {
    /// The frame did not contain a complete protocol header.
    Truncated,
    /// The frame magic did not identify the authority protocol.
    InvalidMagic,
    /// The frame protocol version is not supported by this crate.
    UnsupportedVersion(u16),
    /// The frame kind is unknown to this protocol version.
    UnknownKind(u8),
    /// The frame length does not exactly match its kind.
    InvalidLength {
        /// Exact encoded length required by the frame kind.
        expected: usize,
        /// Encoded length supplied by the peer.
        actual: usize,
    },
    /// A fixed-width identity could not be admitted at the protocol boundary.
    InvalidIdentity,
}

impl fmt::Display for FrameError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Truncated => f.write_str("authority frame is truncated"),
            Self::InvalidMagic => f.write_str("authority frame has invalid magic"),
            Self::UnsupportedVersion(version) => {
                write!(f, "authority frame version {version} is unsupported")
            }
            Self::UnknownKind(kind) => write!(f, "authority frame kind {kind} is unknown"),
            Self::InvalidLength { expected, actual } => {
                write!(
                    f,
                    "authority frame length {actual} does not equal {expected}"
                )
            }
            Self::InvalidIdentity => f.write_str("authority frame identity was not admitted"),
        }
    }
}

impl std::error::Error for FrameError {}

/// Failure while acquiring or returning bounded mutable scratch storage.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PoolError {
    /// The requested number of live buffers exceeds the pool capacity.
    Exhausted,
    /// The requested or appended bytes exceed the configured buffer bound.
    BufferLimit,
    /// The pool's live-buffer counter would overflow.
    CounterOverflow,
    /// The allocator could not reserve the requested capacity.
    Allocation,
}

impl fmt::Display for PoolError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Exhausted => f.write_str("buffer pool capacity is exhausted"),
            Self::BufferLimit => f.write_str("buffer exceeds its configured limit"),
            Self::CounterOverflow => f.write_str("buffer pool counter overflowed"),
            Self::Allocation => f.write_str("buffer allocation failed"),
        }
    }
}

impl std::error::Error for PoolError {}

/// Failure while supervising a bounded authority process.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProcessError {
    /// Program or working directory was not absolute.
    RelativePath,
    /// The program path did not name a usable executable path.
    InvalidProgram,
    /// The configured executable could not be read as a stable artifact.
    ExecutableUnavailable,
    /// The configured executable exceeded the identity byte budget.
    ExecutableLimit,
    /// The configured executable changed while it was being admitted.
    ExecutableDrift,
    /// A declared toolchain artifact did not match its command identity.
    IdentityMismatch,
    /// A process limit was zero.
    ZeroLimit,
    /// A requested hard limit is unavailable on the current supervisor.
    UnsupportedLimit(UnsupportedLimit),
    /// An environment key appeared more than once.
    DuplicateEnvironment,
    /// An environment key or value contained a forbidden NUL byte.
    InvalidEnvironment,
    /// Arguments or explicit environment exceeded the process configuration budget.
    ConfigurationLimit,
    /// The temporary output file could not be created.
    TemporaryFile,
    /// A process could not be started.
    SpawnFailure,
    /// A process or output stream could not be queried or read.
    Io,
    /// A child could not be killed and reaped after cancellation or a limit.
    NotReaped,
    /// Output exceeded stdout, stderr, or aggregate output limits.
    OutputLimit,
    /// Files grew beyond the configured workspace growth limit.
    WorkspaceLimit,
    /// Standard input exceeded the command's bounded request budget.
    InputLimit,
    /// The process exceeded its wall-time limit.
    Deadline,
    /// The caller requested cancellation before completion.
    Cancelled,
    /// A process/session protocol invariant was violated.
    Protocol,
    /// A request sequence cannot be advanced without wrapping.
    SequenceExhausted,
    /// Temporary output could not be removed after the child was reaped.
    Cleanup,
}

impl fmt::Display for ProcessError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let message = match self {
            Self::RelativePath => "process paths must be absolute",
            Self::InvalidProgram => "process program path is invalid",
            Self::ExecutableUnavailable => "process executable is unavailable",
            Self::ExecutableLimit => "process executable exceeded its identity byte budget",
            Self::ExecutableDrift => "process executable changed during admission",
            Self::IdentityMismatch => "process toolchain identity does not match executable",
            Self::ZeroLimit => "process limits must be non-zero",
            Self::UnsupportedLimit(limit) => {
                return write!(f, "unsupported process limit: {limit}");
            }
            Self::DuplicateEnvironment => "process environment contains a duplicate key",
            Self::InvalidEnvironment => "process environment contains an invalid byte",
            Self::ConfigurationLimit => "process configuration exceeded its bounded budget",
            Self::TemporaryFile => "process output file could not be created",
            Self::SpawnFailure => "authority process could not be started",
            Self::Io => "authority process I/O failed",
            Self::NotReaped => "authority process was not reaped",
            Self::OutputLimit => "authority process output exceeded its limit",
            Self::WorkspaceLimit => "authority workspace grew beyond its limit",
            Self::InputLimit => "authority process input exceeded its limit",
            Self::Deadline => "authority process exceeded its deadline",
            Self::Cancelled => "authority process was cancelled",
            Self::Protocol => "authority process protocol failed",
            Self::SequenceExhausted => "authority session sequence is exhausted",
            Self::Cleanup => "authority process temporary output could not be removed",
        };
        f.write_str(message)
    }
}

impl std::error::Error for ProcessError {}

/// A hard process bound that this crate cannot enforce on the current
/// supervisor/platform.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum UnsupportedLimit {
    /// Requested process-count bound for an authority.
    ProcessCount,
    /// Maximum resident memory for the authority's process tree.
    MemoryBytes,
    /// Maximum CPU time for the authority's process tree.
    CpuTime,
    /// Process-group teardown needed to guarantee descendant cancellation.
    ProcessGroup,
}

impl fmt::Display for UnsupportedLimit {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let message = match self {
            Self::ProcessCount => "process-count bound",
            Self::MemoryBytes => "memory bound",
            Self::CpuTime => "CPU-time bound",
            Self::ProcessGroup => "process-group teardown",
        };
        f.write_str(message)
    }
}

impl std::error::Error for UnsupportedLimit {}
