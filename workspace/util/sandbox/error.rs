//! Typed sandbox failure modes.
//!
//! Maps cleanly onto producer `ProcessFailure` / `EvalTimeout` / OOM outcomes
//! without stringly-typed "backend said no".

use std::io;
use std::time::Duration;

use thiserror::Error;

/// Why a sandboxed process was terminated by the supervisor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum KillReason {
	/// Wall-clock budget exhausted.
	Wall,
	/// cgroup / rlimit memory ceiling hit.
	Oom,
	/// CPU-time rlimit exhausted.
	CpuTime,
	/// stdout or stderr byte cap hit.
	OutputCap,
	/// pid / process budget exhausted.
	Pids,
}

impl std::fmt::Display for KillReason {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		f.write_str(match self {
			Self::Wall => "wall-time limit",
			Self::Oom => "memory limit",
			Self::CpuTime => "cpu-time limit",
			Self::OutputCap => "output size limit",
			Self::Pids => "process count limit",
		})
	}
}

/// Failures from constructing or running a sandbox.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum SandboxError {
	/// The selected backend refused the request (policy / production gate).
	#[error("sandbox denied: {reason}")]
	Denied {
		/// Human-readable denial reason.
		reason: String,
	},

	/// Required toolchain binary is missing from the host (or from binds).
	#[error("toolchain missing: {program}")]
	ToolchainMissing {
		/// Program name or path that was missing.
		program: String,
	},

	/// bubblewrap / sandbox-exec binary not found when required.
	#[error("sandbox helper missing: {program}")]
	HelperMissing {
		/// Helper binary that was required.
		program: String,
	},

	/// Spawning the child failed before isolation applied.
	#[error("failed to spawn sandboxed process")]
	Spawn(#[source] io::Error),

	/// Child was killed by a resource ceiling.
	#[error("sandboxed process killed: {reason}")]
	Killed {
		/// Which ceiling fired.
		reason: KillReason,
		/// Observed wall time before kill.
		wall: Duration,
	},

	/// Backend-specific failure (bwrap exit, seatbelt profile, cgroup setup).
	#[error("sandbox backend error: {0}")]
	Backend(String),

	/// I/O while reading child pipes or scratch.
	#[error("sandbox I/O error")]
	Io(#[from] io::Error),
}
