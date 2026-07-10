//! Sealed command / input construction.
//!
//! [`Sealer`] is the only intentional place that may touch ambient host state
//! (PATH, toolchains, scratch roots) when projecting into a budget. Callers
//! that already hold a fully resolved budget can build [`SealedCommand`]
//! directly via [`SealedCommand::new`].

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use heart::JobKey;

use crate::budget::{CapabilityBudget, FsGrant, NetGrant};
use crate::limits::{Limits, Network};
use crate::spec::{Env, Mounts, Spec};

/// Hash-pinned inputs already on disk, with a fully resolved budget.
///
/// The [`key`](Self::key) is the content-addressed job identity computed from
/// `ToolchainSet::digest()` + source / lock hashes; `run_producer` uses it as
/// the CAS key.
#[derive(Debug, Clone)]
pub struct SealedInput {
	/// Content-addressed job identity (CAS key).
	pub key: JobKey,
	/// Package / tree root (already local, hash-pinned by the caller).
	pub root: PathBuf,
	/// Resolved capability budget — no re-resolution downstream.
	pub budget: CapabilityBudget,
}

impl SealedInput {
	/// Construct a sealed input with an explicit job key.
	pub fn new(key: JobKey, root: impl Into<PathBuf>, budget: CapabilityBudget) -> Self {
		Self {
			key,
			root: root.into(),
			budget,
		}
	}

	/// Construct with a placeholder key derived from the root path (dev / tests
	/// that do not go through the CAS). Prefer [`Self::new`] with a real key.
	pub fn unkeyed(root: impl Into<PathBuf>, budget: CapabilityBudget) -> Self {
		let root = root.into();
		let key = JobKey::derive(
			b"unkeyed",
			b"",
			root.as_os_str().as_encoded_bytes(),
			b"",
		);
		Self { key, root, budget }
	}
}

/// Fully specified command under a capability budget.
///
/// Replaces ad-hoc [`Spec`] + external isolated builders as the cage-facing
/// unit of work.
#[derive(Debug, Clone)]
pub struct SealedCommand {
	/// Program to exec.
	pub command: PathBuf,
	/// Arguments (not including argv0).
	pub args: Vec<OsString>,
	/// Working directory inside the guest (must be visible via the grant).
	pub cwd: Option<PathBuf>,
	/// Fully resolved capability budget.
	pub budget: CapabilityBudget,
}

impl SealedCommand {
	/// Build from resolved parts (no ambient discovery).
	pub fn new(
		command: impl Into<PathBuf>,
		args: impl IntoIterator<Item = impl Into<OsString>>,
		budget: CapabilityBudget,
	) -> Self {
		Self {
			command: command.into(),
			args: args.into_iter().map(Into::into).collect(),
			cwd: None,
			budget,
		}
	}

	/// Set guest cwd.
	pub fn cwd(mut self, cwd: impl Into<PathBuf>) -> Self {
		self.cwd = Some(cwd.into());
		self
	}

	/// Project into the legacy [`Spec`] shape for Backend shims.
	pub fn into_spec(self) -> Spec {
		let mut spec = Spec::new(self.command, self.budget.resources)
			.args(self.args)
			.env(self.budget.env)
			.mounts(self.budget.fs.to_mounts())
			.network(Network::from(self.budget.net));
		if let Some(cwd) = self.cwd {
			spec = spec.cwd(cwd);
		}
		spec
	}

	/// Lift a legacy [`Spec`] into a sealed command (shim for existing producers).
	///
	/// Uses the first writable mount as scratch when present; otherwise a
	/// placeholder under the system temp dir (caller should prefer real scratch).
	pub fn from_spec(spec: Spec) -> Self {
		let scratch = spec
			.mounts
			.writable
			.first()
			.cloned()
			.unwrap_or_else(std::env::temp_dir);
		let fs = FsGrant {
			read_only: spec.mounts.read_only,
			scratch,
			// Remaining RW binds stay as extra writables (primary already chosen).
			writable: spec.mounts.writable.into_iter().skip(1).collect(),
		};
		Self {
			command: spec.command,
			args: spec.args,
			cwd: spec.cwd,
			budget: CapabilityBudget {
				fs,
				net: NetGrant::from(spec.network),
				env: spec.env,
				resources: spec.limits,
			},
		}
	}

	/// Command path for diagnostics.
	pub fn command_path(&self) -> &Path {
		&self.command
	}

	/// Display label for error messages.
	pub fn command_display(&self) -> String {
		self.command.display().to_string()
	}
}

/// Projects ambient host facts into sealed budgets / commands.
///
/// Phase 2 keeps this thin: producers still assemble mounts/env themselves and
/// call [`SealedCommand::new`] / [`from_spec`](SealedCommand::from_spec). The
/// type marks the ambient-access boundary for later ForgeRuntime work.
#[derive(Debug, Default, Clone)]
pub struct Sealer {
	_private: (),
}

impl Sealer {
	/// Construct a sealer.
	pub fn new() -> Self {
		Self { _private: () }
	}

	/// Seal a command under an already-resolved budget (no ambient reads).
	pub fn seal_command(
		&self,
		command: impl Into<PathBuf>,
		args: impl IntoIterator<Item = impl Into<OsString>>,
		budget: CapabilityBudget,
	) -> SealedCommand {
		SealedCommand::new(command, args, budget)
	}

	/// Build a budget from explicit pieces (no ambient reads).
	pub fn budget(
		&self,
		fs: FsGrant,
		net: NetGrant,
		env: Env,
		resources: Limits,
	) -> CapabilityBudget {
		CapabilityBudget::new(fs, net, env, resources)
	}

	/// Lift mounts + env + limits into a budget (scratch required).
	pub fn budget_from_mounts(
		&self,
		mounts: Mounts,
		scratch: impl Into<PathBuf>,
		net: NetGrant,
		env: Env,
		resources: Limits,
	) -> CapabilityBudget {
		CapabilityBudget::new(FsGrant::from_mounts(mounts, scratch), net, env, resources)
	}
}
