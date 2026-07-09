//! The unit of isolation: what to run, where it may look, and what it may cost.

use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::process::ExitStatus;
use std::time::Duration;

use crate::error::KillReason;
use crate::limits::{Limits, Network};

/// Explicit environment — never inherits ambient host secrets.
///
/// Construction is only via [`Env::empty`] + [`Env::set`] (or [`Env::from_pairs`]).
/// There is no `from_os_environ()` on purpose.
#[derive(Debug, Clone, Default)]
pub struct Env {
	pairs: Vec<(OsString, OsString)>,
}

impl Env {
	/// Empty allowlist.
	pub fn empty() -> Self {
		Self { pairs: Vec::new() }
	}

	/// Build from an iterator of pairs (still an allowlist, not ambient).
	pub fn from_pairs<I, K, V>(pairs: I) -> Self
	where
		I: IntoIterator<Item = (K, V)>,
		K: Into<OsString>,
		V: Into<OsString>,
	{
		Self {
			pairs: pairs.into_iter().map(|(k, v)| (k.into(), v.into())).collect(),
		}
	}

	/// Insert or replace one binding.
	pub fn set(mut self, key: impl Into<OsString>, val: impl Into<OsString>) -> Self {
		let key = key.into();
		self.pairs.retain(|(k, _)| *k != key);
		self.pairs.push((key, val.into()));
		self
	}

	/// Borrowed view of the allowlist.
	pub fn iter(&self) -> impl Iterator<Item = (&OsStr, &OsStr)> {
		self.pairs.iter().map(|(k, v)| (k.as_os_str(), v.as_os_str()))
	}

	/// Owned pairs for backends that consume them.
	pub fn into_pairs(self) -> Vec<(OsString, OsString)> {
		self.pairs
	}
}

/// Filesystem capability surface for the guest.
#[derive(Debug, Clone, Default)]
pub struct Mounts {
	/// Paths bind-mounted read-only (toolchain store paths, package source).
	pub read_only: Vec<PathBuf>,
	/// Paths bind-mounted read-write (scratch, target dirs, json out).
	pub writable: Vec<PathBuf>,
}

impl Mounts {
	/// Start empty.
	pub fn new() -> Self {
		Self::default()
	}

	/// Add a read-only bind.
	pub fn ro(mut self, path: impl Into<PathBuf>) -> Self {
		self.read_only.push(path.into());
		self
	}

	/// Add a writable bind.
	pub fn rw(mut self, path: impl Into<PathBuf>) -> Self {
		self.writable.push(path.into());
		self
	}
}

/// Fully-specified sandboxed command.
#[derive(Debug, Clone)]
pub struct Spec {
	/// Program to exec (looked up on host for passthrough; bound into guest for bwrap).
	pub command: PathBuf,
	/// Arguments (not including argv0).
	pub args: Vec<OsString>,
	/// Explicit env allowlist.
	pub env: Env,
	/// Working directory inside the guest (must be visible via mounts).
	pub cwd: Option<PathBuf>,
	/// FS capability surface.
	pub mounts: Mounts,
	/// Resource ceilings.
	pub limits: Limits,
	/// Network policy (default off).
	pub network: Network,
}

impl Spec {
	/// Builder entry: command + required limits.
	pub fn new(command: impl Into<PathBuf>, limits: Limits) -> Self {
		Self {
			command: command.into(),
			args: Vec::new(),
			env: Env::empty(),
			cwd: None,
			mounts: Mounts::new(),
			limits,
			network: Network::Off,
		}
	}

	/// Append one argument.
	pub fn arg(mut self, arg: impl Into<OsString>) -> Self {
		self.args.push(arg.into());
		self
	}

	/// Append many arguments.
	pub fn args<I, S>(mut self, args: I) -> Self
	where
		I: IntoIterator<Item = S>,
		S: Into<OsString>,
	{
		self.args.extend(args.into_iter().map(Into::into));
		self
	}

	/// Replace the env allowlist.
	pub fn env(mut self, env: Env) -> Self {
		self.env = env;
		self
	}

	/// Set cwd.
	pub fn cwd(mut self, cwd: impl Into<PathBuf>) -> Self {
		self.cwd = Some(cwd.into());
		self
	}

	/// Replace mounts.
	pub fn mounts(mut self, mounts: Mounts) -> Self {
		self.mounts = mounts;
		self
	}

	/// Set network policy.
	pub fn network(mut self, network: Network) -> Self {
		self.network = network;
		self
	}

	/// Convenience: command as a display string for error messages.
	pub fn command_display(&self) -> String {
		self.command.display().to_string()
	}

	/// Host path that must exist for the command binary (for ToolchainMissing).
	pub fn command_path(&self) -> &Path {
		&self.command
	}
}

/// How a sandboxed process ended.
///
/// Mutually exclusive: a process either exited with a status or was killed by a
/// resource ceiling. The impossible `(status, killed: Some(_))` pair is gone.
#[derive(Debug, Clone)]
pub enum ProcessEnd {
	/// The process exited on its own (zero or non-zero).
	Exited(ExitStatus),
	/// The supervisor or OS killed the process for a resource ceiling.
	///
	/// Usually promoted to [`crate::SandboxError::Killed`] before returning to
	/// callers; retained here so the type system cannot represent both an exit
	/// status and a kill reason at once.
	Killed(KillReason),
}

/// Captured result of a successful (or non-zero-exit) sandboxed run.
///
/// Resource kills usually surface as [`crate::SandboxError::Killed`], not as
/// [`ProcessEnd::Killed`] inside this type.
#[derive(Debug, Clone)]
pub struct Output {
	/// Child stdout (already capped by the supervisor).
	pub stdout: Vec<u8>,
	/// Child stderr (already capped).
	pub stderr: Vec<u8>,
	/// How the process ended.
	pub end: ProcessEnd,
	/// Wall time observed by the supervisor.
	pub wall: Duration,
	/// Peak memory from cgroup `memory.peak`, when available.
	pub peak_mem: Option<u64>,
}

/// Alias matching the sealed-compute vocabulary (`Captured` / `ProcessEnd`).
///
/// Prefer this name at new call sites; [`Output`] remains for existing code.
pub type Captured = Output;

impl Output {
	/// Whether the child exited with status 0 (not killed).
	pub fn success(&self) -> bool {
		matches!(&self.end, ProcessEnd::Exited(s) if s.success())
	}

	/// Exit status when the process exited; `None` if it was killed.
	pub fn status(&self) -> Option<ExitStatus> {
		match self.end {
			ProcessEnd::Exited(s) => Some(s),
			ProcessEnd::Killed(_) => None,
		}
	}
}
