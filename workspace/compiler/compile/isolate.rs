//! Seam F: build isolation inputs for external producer toolchains.
//!
//! Producers keep their language-specific logic; this module projects an
//! [`IsolatedCommand`] builder into a [`sandbox::SealedCommand`] under the
//! injected [`ForgeContext`]'s toolchains, and runs it through `ctx.cage()`.
//!
//! There are no process globals here anymore: toolchains, the cage, and the
//! override table all arrive through the context.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Output as StdOutput;

use sandbox::{
	CancelToken, Env, KillReason, LimitOverride, Mounts, Network, ProducerProfile, SealedCommand,
	Sealer,
};

use crate::compile::producer::ForgeContext;

/// Map a sandbox kill / denial into a displayable process-style failure.
#[derive(Debug)]
pub struct IsolatedFailure {
	/// Command label for error messages.
	pub command: String,
	/// Structured reason.
	pub kind: IsolatedFailureKind,
	/// Captured stdout (capped).
	pub stdout: Option<String>,
	/// Captured stderr (capped).
	pub stderr: Option<String>,
}

/// Why isolation failed or the guest was killed.
#[derive(Debug)]
pub enum IsolatedFailureKind {
	/// Wall / CPU / OOM / output / pids ceiling.
	Resource(KillReason),
	/// Non-zero exit.
	NonZero {
		/// Exit status display.
		status: String,
	},
	/// Sandbox refused or could not spawn.
	Sandbox(String),
	/// Toolchain binary missing.
	ToolchainMissing(String),
}

impl std::fmt::Display for IsolatedFailure {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		write!(f, "isolated `{}` failed: {}", self.command, self.kind)
	}
}

impl std::error::Error for IsolatedFailure {}

impl std::fmt::Display for IsolatedFailureKind {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		match self {
			Self::Resource(r) => write!(f, "resource kill ({r})"),
			Self::NonZero { status } => write!(f, "exit {status}"),
			Self::Sandbox(s) => write!(f, "sandbox: {s}"),
			Self::ToolchainMissing(p) => write!(f, "missing toolchain {p}"),
		}
	}
}

/// Inputs for an isolated toolchain invocation.
///
/// Thin builder over [`SealedCommand`]: familiar to producers; [`run_isolated`]
/// / [`seal`] project it into a sealed command under a [`ForgeContext`].
pub struct IsolatedCommand {
	/// Program (name on PATH or absolute).
	pub program: PathBuf,
	/// argv[1..].
	pub args: Vec<OsString>,
	/// Explicit env allowlist (merged with a minimal PATH).
	pub env: Vec<(OsString, OsString)>,
	/// Working directory.
	pub cwd: Option<PathBuf>,
	/// Paths the guest may read.
	pub read_only: Vec<PathBuf>,
	/// Paths the guest may write.
	pub writable: Vec<PathBuf>,
	/// Limit profile.
	pub profile: ProducerProfile,
	/// Optional per-package overlay.
	pub override_: LimitOverride,
	/// Network (default off).
	pub network: Network,
}

impl IsolatedCommand {
	/// Start a builder for `program` with a profile.
	pub fn new(program: impl Into<PathBuf>, profile: ProducerProfile) -> Self {
		Self {
			program: program.into(),
			args: Vec::new(),
			env: Vec::new(),
			cwd: None,
			read_only: Vec::new(),
			writable: Vec::new(),
			profile,
			override_: LimitOverride::none(),
			network: Network::Off,
		}
	}

	/// Append an argument.
	pub fn arg(mut self, a: impl Into<OsString>) -> Self {
		self.args.push(a.into());
		self
	}

	/// Append arguments.
	pub fn args<I, S>(mut self, args: I) -> Self
	where
		I: IntoIterator<Item = S>,
		S: Into<OsString>,
	{
		self.args.extend(args.into_iter().map(Into::into));
		self
	}

	/// Set one env binding.
	pub fn env(mut self, k: impl Into<OsString>, v: impl Into<OsString>) -> Self {
		self.env.push((k.into(), v.into()));
		self
	}

	/// Set cwd.
	pub fn cwd(mut self, p: impl Into<PathBuf>) -> Self {
		self.cwd = Some(p.into());
		self
	}

	/// RO bind.
	pub fn ro(mut self, p: impl Into<PathBuf>) -> Self {
		self.read_only.push(p.into());
		self
	}

	/// RW bind.
	pub fn rw(mut self, p: impl Into<PathBuf>) -> Self {
		self.writable.push(p.into());
		self
	}

	/// Network policy.
	pub fn network(mut self, n: Network) -> Self {
		self.network = n;
		self
	}

	/// Per-package limit overlay.
	pub fn limit_override(mut self, o: LimitOverride) -> Self {
		self.override_ = o;
		self
	}
}

/// Run an isolated command through the context cage; on success return an
/// [`StdOutput`]-shaped capture.
pub fn run_isolated<C: ForgeContext>(
	ctx: &C,
	cmd: IsolatedCommand,
) -> Result<StdOutput, IsolatedFailure> {
	let command_label = format!(
		"{} {}",
		cmd.program.display(),
		cmd.args
			.iter()
			.map(|a| a.to_string_lossy())
			.collect::<Vec<_>>()
			.join(" ")
	);

	let sealed = seal(ctx, cmd);
	match ctx.cage().run(sealed, &CancelToken::never()) {
		Ok(out) => {
			if out.success() {
				let status = out.status().expect("success implies exited");
				Ok(StdOutput {
					status,
					stdout: out.stdout,
					stderr: out.stderr,
				})
			} else {
				match out.end {
					sandbox::ProcessEnd::Exited(status) => Err(IsolatedFailure {
						command: command_label,
						kind: IsolatedFailureKind::NonZero {
							status: status.to_string(),
						},
						stdout: nonempty_utf8(&out.stdout),
						stderr: nonempty_utf8(&out.stderr),
					}),
					sandbox::ProcessEnd::Killed(reason) => Err(IsolatedFailure {
						command: command_label,
						kind: IsolatedFailureKind::Resource(reason),
						stdout: nonempty_utf8(&out.stdout),
						stderr: nonempty_utf8(&out.stderr),
					}),
				}
			}
		}
		Err(e) => Err(cage_error_to_isolated(e, &command_label)),
	}
}

/// Map a [`sandbox::CageError`] into the displayable isolated failure.
fn cage_error_to_isolated(e: sandbox::CageError, label: &str) -> IsolatedFailure {
	use sandbox::CageError;
	let kind = match e {
		CageError::Killed { reason, .. } => IsolatedFailureKind::Resource(reason),
		CageError::ToolchainMissing { program } | CageError::HelperMissing { program } => {
			IsolatedFailureKind::ToolchainMissing(program)
		}
		other => IsolatedFailureKind::Sandbox(other.to_string()),
	};
	IsolatedFailure {
		command: label.to_string(),
		kind,
		stdout: None,
		stderr: None,
	}
}

fn nonempty_utf8(bytes: &[u8]) -> Option<String> {
	let s = String::from_utf8_lossy(bytes).trim().to_string();
	if s.is_empty() { None } else { Some(s) }
}

/// Project an [`IsolatedCommand`] into a [`SealedCommand`] under `ctx`.
///
/// The sealer boundary: reads no policy env. `PATH` is a fixed hermetic set;
/// toolchain bindings come from `ctx.toolchains()`; limits from
/// `ctx.overrides()`.
pub fn seal<C: ForgeContext>(ctx: &C, cmd: IsolatedCommand) -> SealedCommand {
	let sealer = Sealer::new();
	let toolchains = ctx.toolchains();

	// Fixed hermetic PATH, optionally prefixed with dev-only toolchain dirs
	// (`NUDOX_TOOLCHAIN_PATH`, resolved in `ToolchainSet::from_env`) so hosts
	// where `go`/`javadoc` live outside the hermetic set (e.g. a Nix devshell)
	// can still find them. Empty by default → hermetic PATH unchanged.
	const HERMETIC_PATH: &str = "/usr/bin:/bin:/nix/var/nix/profiles/default/bin";
	let path = match toolchains.path_prefix() {
		Some(prefix) => format!("{prefix}:{HERMETIC_PATH}"),
		None => HERMETIC_PATH.to_string(),
	};
	let mut env = Env::empty().set("PATH", path);
	env = toolchains.apply_env(env);
	for (k, v) in cmd.env {
		env = env.set(k, v);
	}

	let mut mounts = Mounts::new();
	for p in &cmd.read_only {
		mounts = mounts.ro(p);
	}
	for p in toolchains.ro_binds() {
		mounts = mounts.ro(p);
	}
	for p in &cmd.writable {
		mounts = mounts.rw(p);
	}
	// Nix store RO for absolute PT_INTERP / RPATH toolchains.
	if Path::new("/nix/store").is_dir() {
		mounts = mounts.ro("/nix/store");
	}
	// Narrow scratch — never ambient $HOME. temp_dir is a cwd-style read, not policy.
	let scratch = std::env::temp_dir();
	mounts = mounts.rw(&scratch);
	if let Some(ref cwd) = cmd.cwd {
		mounts = mounts.rw(cwd);
	}

	let mut limits = ctx.overrides().resolve(cmd.profile, None);
	limits = cmd.override_.apply(limits);

	let budget = sealer.budget_from_mounts(
		mounts,
		scratch,
		sandbox::NetGrant::from(cmd.network),
		env,
		limits,
	);
	let mut sealed = sealer.seal_command(cmd.program, cmd.args, budget);
	if let Some(cwd) = cmd.cwd {
		sealed = sealed.cwd(cwd);
	}
	sealed
}

/// Collect ancestor directory binds so a package tree is visible.
pub fn package_tree_binds(root: &Path) -> Vec<PathBuf> {
	let mut out = Vec::new();
	if let Ok(c) = root.canonicalize() {
		out.push(c);
	} else {
		out.push(root.to_path_buf());
	}
	out
}
