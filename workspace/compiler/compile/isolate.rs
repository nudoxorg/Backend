//! Seam F: run producer toolchains inside the sandbox.
//!
//! Producers keep their language-specific logic; this module is the only place
//! that builds a [`sandbox::Spec`] for external toolchains (Rust/Java/Go) and
//! for the in-process worker binary (Nix/TS/Python).

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Output as StdOutput;

use sandbox::{
	run, Env, KillReason, Mounts, Network, ProducerProfile, SandboxError, Spec,
};

// `which` is re-exported transitively via sandbox; use std discovery if needed.
mod which {
	use std::path::{Path, PathBuf};
	pub fn which(bin: &Path) -> Result<PathBuf, ()> {
		if bin.is_absolute() && bin.exists() {
			return Ok(bin.to_path_buf());
		}
		let name = bin.as_os_str();
		let path = std::env::var_os("PATH").ok_or(())?;
		for dir in std::env::split_paths(&path) {
			let candidate = dir.join(name);
			if candidate.is_file() {
				return Ok(candidate);
			}
		}
		Err(())
	}
}

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
}

/// Run an isolated command; on success return a std-like [`StdOutput`].
pub fn run_isolated(cmd: IsolatedCommand) -> Result<StdOutput, IsolatedFailure> {
	let command_label = format!(
		"{} {}",
		cmd.program.display(),
		cmd.args
			.iter()
			.map(|a| a.to_string_lossy())
			.collect::<Vec<_>>()
			.join(" ")
	);

	let mut env = Env::empty().set(
		"PATH",
		std::env::var_os("PATH").unwrap_or_else(|| "/usr/bin:/bin".into()),
	);
	// Preserve a minimal locale so toolchains don't panic.
	if let Ok(v) = std::env::var("LANG") {
		env = env.set("LANG", v);
	}
	if let Ok(v) = std::env::var("LC_ALL") {
		env = env.set("LC_ALL", v);
	}
	// Nix store / rustup when present (RO via ambient path visibility on
	// passthrough/seatbelt; bwrap binds /nix).
	if let Ok(v) = std::env::var("RUSTUP_HOME") {
		env = env.set("RUSTUP_HOME", v);
	}
	if let Ok(v) = std::env::var("CARGO_HOME") {
		env = env.set("CARGO_HOME", v);
	}
	if let Ok(v) = std::env::var("JAVA_HOME") {
		env = env.set("JAVA_HOME", v);
	}
	if let Ok(v) = std::env::var("GOROOT") {
		env = env.set("GOROOT", v);
	}
	if let Ok(v) = std::env::var("GOPATH") {
		env = env.set("GOPATH", v);
	}
	for (k, v) in cmd.env {
		env = env.set(k, v);
	}

	let mut mounts = Mounts::new();
	for p in &cmd.read_only {
		mounts = mounts.ro(p);
	}
	for p in &cmd.writable {
		mounts = mounts.rw(p);
	}
	// Nix store RO for absolute PT_INTERP / RPATH toolchains (bwrap also
	// binds /nix/store globally; Landlock/seatbelt need the path listed).
	if Path::new("/nix/store").is_dir() {
		mounts = mounts.ro("/nix/store");
	}
	// Always allow temp + home for toolchains that need caches; writable.
	if let Ok(tmp) = std::env::temp_dir().canonicalize() {
		mounts = mounts.rw(tmp);
	}
	if let Some(home) = std::env::var_os("HOME") {
		mounts = mounts.rw(PathBuf::from(home));
	}
	if let Ok(cwd) = std::env::current_dir() {
		mounts = mounts.ro(&cwd);
	}
	if let Some(ref cwd) = cmd.cwd {
		mounts = mounts.rw(cwd);
	}

	let mut spec = Spec::new(cmd.program, cmd.profile.limits())
		.args(cmd.args)
		.env(env)
		.mounts(mounts)
		.network(cmd.network);
	if let Some(cwd) = cmd.cwd {
		spec = spec.cwd(cwd);
	}

	match run(spec) {
		Ok(out) => {
			if out.success() {
				Ok(StdOutput {
					status: out.status,
					stdout: out.stdout,
					stderr: out.stderr,
				})
			} else {
				Err(IsolatedFailure {
					command: command_label,
					kind: IsolatedFailureKind::NonZero {
						status: out.status.to_string(),
					},
					stdout: nonempty_utf8(&out.stdout),
					stderr: nonempty_utf8(&out.stderr),
				})
			}
		}
		Err(SandboxError::Killed { reason, .. }) => Err(IsolatedFailure {
			command: command_label,
			kind: IsolatedFailureKind::Resource(reason),
			stdout: None,
			stderr: None,
		}),
		Err(SandboxError::ToolchainMissing { program }) => Err(IsolatedFailure {
			command: command_label,
			kind: IsolatedFailureKind::ToolchainMissing(program),
			stdout: None,
			stderr: None,
		}),
		Err(e) => Err(IsolatedFailure {
			command: command_label,
			kind: IsolatedFailureKind::Sandbox(e.to_string()),
			stdout: None,
			stderr: None,
		}),
	}
}

fn nonempty_utf8(bytes: &[u8]) -> Option<String> {
	let s = String::from_utf8_lossy(bytes).trim().to_string();
	if s.is_empty() {
		None
	} else {
		Some(s)
	}
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

/// Resolve the producer-worker binary, if configured.
///
/// Set `NUDOX_PRODUCER_WORKER` to an absolute path (or name on PATH) to force
/// in-process languages (nix/ts/python) through a crash-isolated subprocess.
pub fn producer_worker_bin() -> Option<PathBuf> {
	let raw = std::env::var_os("NUDOX_PRODUCER_WORKER")?;
	let path = PathBuf::from(raw);
	if path.exists() {
		return Some(path);
	}
	which::which(&path).ok()
}

/// Lower via the producer-worker when configured; otherwise `None` so callers
/// fall back to in-process (dev). Returns the IR JSON body on success.
pub fn try_worker_lower(lang: &str, root: &Path) -> Option<Result<String, IsolatedFailure>> {
	let bin = producer_worker_bin()?;
	let profile = match lang {
		"nix" => ProducerProfile::Nix,
		_ => ProducerProfile::StaticParser,
	};
	let cmd = IsolatedCommand::new(bin, profile)
		.arg("lower")
		.arg(lang)
		.arg(root)
		.ro(root)
		.rw(std::env::temp_dir());
	Some(run_isolated(cmd).map(|o| String::from_utf8_lossy(&o.stdout).into_owned()))
}
