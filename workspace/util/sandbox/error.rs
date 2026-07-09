//! Typed sandbox / cage failure modes.
//!
//! [`SandboxError`] remains the Backend-facing surface. New cage paths prefer
//! [`CageError`] with enumerated variants; the two convert losslessly enough
//! for shims.

use std::io;
use std::path::PathBuf;
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

/// Map a displayable error into an `io::Error` for `pre_exec` and similar
/// callbacks that only accept `io::Error`.
pub fn to_io_error(err: impl std::fmt::Display) -> io::Error {
	io::Error::other(err.to_string())
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

	/// Caller cancelled via [`crate::CancelToken`].
	#[error("sandbox run cancelled")]
	Cancelled,

	/// I/O while reading child pipes or scratch.
	#[error("sandbox I/O error")]
	Io(#[from] io::Error),
}

/// Enumerated cage failure modes (Phase 2+).
///
/// Prefer these over stringly [`SandboxError::Backend`] on new paths.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum CageError {
	/// Policy / production gate refused the request.
	#[error("cage denied: {reason}")]
	Denied {
		/// Human-readable denial reason.
		reason: String,
	},

	/// Required toolchain binary missing.
	#[error("toolchain missing: {program}")]
	ToolchainMissing {
		/// Program name or path.
		program: String,
	},

	/// bubblewrap / sandbox-exec / helper missing.
	#[error("sandbox helper missing: {program}")]
	HelperMissing {
		/// Helper binary name.
		program: String,
	},

	/// Spawn failed before isolation applied.
	#[error("failed to spawn sandboxed process")]
	Spawn(#[source] io::Error),

	/// Resource ceiling kill.
	#[error("sandboxed process killed: {reason}")]
	Killed {
		/// Which ceiling fired.
		reason: KillReason,
		/// Observed wall time before kill.
		wall: Duration,
	},

	/// cgroup sysfs write failed.
	#[error("cgroup write {path}: {message}")]
	CgroupWrite {
		/// Path that failed.
		path: PathBuf,
		/// OS / context message.
		message: String,
	},

	/// Seccomp BPF compile or install failed.
	#[error("seccomp compile failed: {0}")]
	SeccompCompile(String),

	/// A path required by the FS grant is missing on the host.
	#[error("required mount missing: {path}")]
	MountMissing {
		/// Missing path.
		path: PathBuf,
	},

	/// Worker line-protocol failure.
	#[error("worker protocol: {detail}")]
	WorkerProtocol {
		/// Detail message.
		detail: String,
	},

	/// Caller cancelled via [`crate::CancelToken`].
	#[error("cage run cancelled")]
	Cancelled,

	/// I/O while reading pipes or scratch.
	#[error("cage I/O error")]
	Io(#[from] io::Error),

	/// Catch-all for legacy Backend string errors during the shim window.
	#[error("cage backend: {0}")]
	Backend(String),
}

impl From<SandboxError> for CageError {
	fn from(e: SandboxError) -> Self {
		match e {
			SandboxError::Denied { reason } => Self::Denied { reason },
			SandboxError::ToolchainMissing { program } => Self::ToolchainMissing { program },
			SandboxError::HelperMissing { program } => Self::HelperMissing { program },
			SandboxError::Spawn(err) => Self::Spawn(err),
			SandboxError::Killed { reason, wall } => Self::Killed { reason, wall },
			// Recover enumerated cgroup writes encoded via SandboxError::from(CageError).
			SandboxError::Backend(s) if s.starts_with("cgroup write ") => {
				// "cgroup write {path}: {message}"
				let rest = s.trim_start_matches("cgroup write ");
				if let Some((path, message)) = rest.split_once(": ") {
					Self::CgroupWrite {
						path: PathBuf::from(path),
						message: message.to_string(),
					}
				} else {
					Self::Backend(s)
				}
			}
			SandboxError::Backend(s) => Self::Backend(s),
			SandboxError::Cancelled => Self::Cancelled,
			SandboxError::Io(err) => Self::Io(err),
		}
	}
}

impl From<CageError> for SandboxError {
	fn from(e: CageError) -> Self {
		match e {
			CageError::Denied { reason } => Self::Denied { reason },
			CageError::ToolchainMissing { program } => Self::ToolchainMissing { program },
			CageError::HelperMissing { program } => Self::HelperMissing { program },
			CageError::Spawn(err) => Self::Spawn(err),
			CageError::Killed { reason, wall } => Self::Killed { reason, wall },
			CageError::CgroupWrite { path, message } => {
				Self::Backend(format!("cgroup write {}: {message}", path.display()))
			}
			CageError::SeccompCompile(s) => Self::Backend(format!("seccomp: {s}")),
			CageError::MountMissing { path } => {
				Self::Backend(format!("mount missing: {}", path.display()))
			}
			CageError::WorkerProtocol { detail } => Self::Backend(format!("worker: {detail}")),
			CageError::Cancelled => Self::Cancelled,
			CageError::Io(err) => Self::Io(err),
			CageError::Backend(s) => Self::Backend(s),
		}
	}
}
