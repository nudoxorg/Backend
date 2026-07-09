//! No isolation — dev/unsupported only.
//!
//! Refuses to run when `NUDOX_SANDBOX_REQUIRE=1` or `NUDOX_ENV=prod`.

use std::process::Stdio;

use crate::backend::supervisor::{self, apply_rlimits};
use crate::backend::{Backend, Capabilities};
use crate::cgroup::Cgroup;
use crate::error::SandboxError;
use crate::spec::{Output, Spec};

/// Passthrough backend: runs the command with rlimits only.
#[derive(Debug, Default)]
pub struct Passthrough {
	/// When true, always deny (tests / explicit strict mode).
	force_deny: bool,
}

impl Passthrough {
	/// Construct (respects `NUDOX_SANDBOX_REQUIRE` / `NUDOX_ENV` at run time).
	pub fn new() -> Self {
		Self { force_deny: false }
	}

	/// Construct a passthrough that always refuses (unit tests, fail-closed hooks).
	pub fn always_deny() -> Self {
		Self { force_deny: true }
	}

	fn production_forbidden(&self) -> bool {
		self.force_deny || crate::probe::IsolationPolicy::env_requires_production()
	}
}

impl Backend for Passthrough {
	fn name(&self) -> &'static str {
		"passthrough"
	}

	fn capabilities(&self) -> Capabilities {
		Capabilities {
			isolation: false,
			network_off: false,
			fs_scope: false,
			seccomp: false,
			cgroups: false,
			production_grade: false,
		}
	}

	fn run(&self, spec: Spec) -> Result<Output, SandboxError> {
		if self.production_forbidden() {
			return Err(SandboxError::Denied {
				reason: "passthrough backend forbidden when NUDOX_SANDBOX_REQUIRE=1 or NUDOX_ENV=prod"
					.into(),
			});
		}

		if which::which(&spec.command).is_err() && !spec.command.exists() {
			return Err(SandboxError::ToolchainMissing {
				program: spec.command_display(),
			});
		}

		let limits = spec.limits;
		let cgroup = Cgroup::try_create(&limits)?;

		let mut cmd = supervisor::base_command(&spec.command);
		cmd.args(&spec.args);
		// Re-apply only the allowlist after env_clear in base_command.
		for (k, v) in spec.env.iter() {
			cmd.env(k, v);
		}
		if let Some(cwd) = &spec.cwd {
			cmd.current_dir(cwd);
		}
		// Parent still has host env cleared only for the child via env_clear + setenv.
		// Ensure stdin null already set.
		cmd.stdin(Stdio::null());

		#[cfg(unix)]
		{
			use std::os::unix::process::CommandExt;
			let limits = limits;
			unsafe {
				cmd.pre_exec(move || {
					apply_rlimits(&limits).map_err(crate::error::to_io_error)?;
					Ok(())
				});
			}
		}

		let child = cmd.spawn().map_err(SandboxError::Spawn)?;
		supervisor::supervise(child, &limits, cgroup)
	}
}
