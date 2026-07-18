//! The one cage abstraction (SMOLVM-PLAN §3).
//!
//! Isolation is a trait, not a peer-of-backends enum. Production work runs
//! inside a hardware-virtualized microVM —
//! [`SmolvmCage`](crate::smolvm::SmolvmCage) — on every platform; dev work
//! runs under [`DevPassthrough`] (constructible only with
//! [`Policy::Development`]). [`WorkerPool`](crate::WorkerPool) is
//! cage-internal for library-form producers and runs *inside the guest* in
//! the VM model, not as a host-side peer.
//!
//! The layered OS sandbox (bwrap namespaces + seccomp + landlock) is gone:
//! the VM boundary carries the trust weight instead (SMOLVM-PLAN §3.3).

use crate::backend::supervisor::{self, apply_rlimits};
use crate::cancel::CancelToken;
use crate::cgroup::Cgroup;
use crate::error::CageError;
use crate::seal::SealedCommand;
use crate::smolvm::SmolvmCage;
use crate::smolvm_backend::{RootfsStore, SmolvmRuntime};
use crate::spec::Output;

/// Stable identity of a cage instance (logs / metrics).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct CageId(pub &'static str);

impl CageId {
	/// Borrow the id string.
	pub fn as_str(&self) -> &'static str {
		self.0
	}
}

impl std::fmt::Display for CageId {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		f.write_str(self.0)
	}
}

/// What a cage can enforce.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CageCaps {
	/// A real isolation boundary exists (hardware virtualization).
	pub isolation: bool,
	/// Network can be made *absent* (no NIC), not merely filtered.
	pub network_off: bool,
	/// Filesystem visibility is scoped to the granted mounts.
	pub fs_scope: bool,
	/// Resource ceilings (memory / cpu / pids) are enforced.
	pub resource_limits: bool,
	/// Suitable as production security boundary of record.
	pub production_grade: bool,
}

/// Runtime policy resolved once at assemble.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Policy {
	/// Production: only production-grade cages.
	Production,
	/// Development: passthrough allowed.
	Development,
}

impl Policy {
	/// From the existing isolation env gate.
	pub fn from_env() -> Self {
		if crate::probe::IsolationPolicy::env_requires_production() {
			Self::Production
		} else {
			Self::Development
		}
	}
}

/// Capability-budgeted process isolation.
pub trait Cage: Send + Sync {
	/// Stable cage identity.
	fn id(&self) -> CageId;

	/// What this cage can enforce on this host.
	fn capabilities(&self) -> CageCaps;

	/// Run a sealed command under the budget; honor `cancel` when possible.
	fn run(&self, cmd: SealedCommand, cancel: &CancelToken) -> Result<Output, CageError>;
}

// ─── DevPassthrough ─────────────────────────────────────────────────────────

/// Dev-only cage: rlimits (+ best-effort cgroup for scheduling fairness),
/// no isolation boundary of any kind.
///
/// Constructible only from [`Policy::Development`] — the type cannot be built
/// under a production policy, replacing thrice-read env gates at the boundary.
#[derive(Debug)]
pub struct DevPassthrough {
	_policy: DevOnly,
}

/// Zero-sized token proving construction went through [`Policy::Development`].
#[derive(Debug, Clone, Copy)]
struct DevOnly;

impl DevPassthrough {
	/// Construct when `policy` is [`Policy::Development`].
	pub fn try_new(policy: Policy) -> Result<Self, CageError> {
		match policy {
			Policy::Development => Ok(Self { _policy: DevOnly }),
			Policy::Production => Err(CageError::Denied {
				reason: "DevPassthrough is not constructible under Policy::Production".into(),
			}),
		}
	}

	/// Convenience: build from the process env policy (dev only).
	pub fn from_env() -> Result<Self, CageError> {
		Self::try_new(Policy::from_env())
	}
}

impl Cage for DevPassthrough {
	fn id(&self) -> CageId {
		CageId("dev-passthrough")
	}

	fn capabilities(&self) -> CageCaps {
		CageCaps {
			isolation: false,
			network_off: false,
			fs_scope: false,
			resource_limits: false,
			production_grade: false,
		}
	}

	fn run(&self, cmd: SealedCommand, cancel: &CancelToken) -> Result<Output, CageError> {
		if cancel.is_cancelled() {
			return Err(CageError::Cancelled);
		}

		if which::which(&cmd.command).is_err() && !cmd.command.exists() {
			return Err(CageError::ToolchainMissing {
				program: cmd.command_display(),
			});
		}

		let limits = cmd.budget.resources;
		let cgroup = Cgroup::try_create(&limits).map_err(CageError::from)?;

		use std::process::Stdio;
		let mut proc = supervisor::base_command(&cmd.command);
		proc.args(&cmd.args);
		for (k, v) in cmd.budget.env.iter() {
			proc.env(k, v);
		}
		if let Some(cwd) = &cmd.cwd {
			proc.current_dir(cwd);
		}
		proc.stdin(Stdio::null());

		#[cfg(unix)]
		{
			use std::os::unix::process::CommandExt;
			#[allow(clippy::redundant_locals)]
			let limits = limits;
			unsafe {
				proc.pre_exec(move || {
					apply_rlimits(&limits).map_err(crate::error::to_io_error)?;
					Ok(())
				});
			}
		}

		if cancel.is_cancelled() {
			return Err(CageError::Cancelled);
		}

		let child = proc.spawn().map_err(CageError::Spawn)?;
		supervisor::supervise(child, &limits, cgroup, cancel).map_err(CageError::from)
	}
}

/// Run a sealed command on the production smolvm microVM cage (SMOLVM-PLAN §3).
///
/// Constructs a [`SmolvmCage`] backed by the real [`SmolvmRuntime`], with
/// the rootfs store bootstrapped from `NUDOX_GUEST_ROOTFS`. Every path through
/// this function either boots a hardware-virtualized VM or fails with a typed
/// [`CageError`]. There is no fallback to [`DevPassthrough`] — that path is
/// only reachable by explicitly constructing a [`DevPassthrough`] under
/// [`Policy::Development`].
///
/// # Errors
///
/// - [`CageError::Cancelled`] — cancel token fired before launch.
/// - [`CageError::Vm`] with [`crate::vm::VmError::InvalidConfig`] — missing or
///   invalid `NUDOX_GUEST_ROOTFS` env var.
/// - [`CageError::Vm`] with [`crate::vm::VmError::Launch`] — hypervisor refused
///   to boot (libkrun unavailable, smolvm binary missing, etc.).
pub fn run_sealed(cmd: SealedCommand, cancel: &CancelToken) -> Result<Output, CageError> {
	if cancel.is_cancelled() {
		return Err(CageError::Cancelled);
	}

	let store = RootfsStore::from_env().map_err(CageError::from)?;
	let runtime = SmolvmRuntime::new(store);
	let cage = SmolvmCage::with_runtime(runtime);
	Cage::run(&cage, cmd, cancel)
}
