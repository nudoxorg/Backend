//! Seam F: run producer toolchains inside the sandbox.
//!
//! Producers keep their language-specific logic; this module is the only place
//! that builds a [`sandbox::Spec`] for external toolchains (Rust/Java/Go) and
//! for the in-process worker binary (Nix/TS/Python).

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Output as StdOutput;
use std::sync::OnceLock;

use sandbox::{
	run, Env, KillReason, LimitOverride, Mounts, Network, ProducerProfile, SandboxError, Spec,
	WorkerLang, WorkerPool, WorkerPoolConfig,
};

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

/// Pinned toolchain paths for hermetic P-parse (design §5.1).
///
/// Loaded once from env; ambient HOME is never used as a writable mount.
#[derive(Debug, Clone, Default)]
pub struct ToolchainPaths {
	/// fenix/rustup root (RO).
	pub rustup_home: Option<PathBuf>,
	/// cargo home (RO preferred; RW only when explicitly set for registries).
	pub cargo_home: Option<PathBuf>,
	/// JAVA_HOME.
	pub java_home: Option<PathBuf>,
	/// GOROOT.
	pub go_root: Option<PathBuf>,
	/// GOPATH (scratch-like; optional).
	pub go_path: Option<PathBuf>,
}

impl ToolchainPaths {
	/// From `NUDOX_TOOLCHAIN_*` then standard env vars (still allowlisted, not ambient dump).
	pub fn from_env() -> Self {
		fn first(keys: &[&str]) -> Option<PathBuf> {
			keys.iter()
				.find_map(|k| std::env::var_os(k).map(PathBuf::from))
				.filter(|p| p.as_os_str().len() > 0)
		}
		Self {
			rustup_home: first(&["NUDOX_TOOLCHAIN_RUSTUP_HOME", "RUSTUP_HOME"]),
			cargo_home: first(&["NUDOX_TOOLCHAIN_CARGO_HOME", "CARGO_HOME"]),
			java_home: first(&["NUDOX_TOOLCHAIN_JAVA_HOME", "JAVA_HOME"]),
			go_root: first(&["NUDOX_TOOLCHAIN_GOROOT", "GOROOT"]),
			go_path: first(&["NUDOX_TOOLCHAIN_GOPATH", "GOPATH"]),
		}
	}

	fn apply_env(&self, mut env: Env) -> Env {
		if let Some(p) = &self.rustup_home {
			env = env.set("RUSTUP_HOME", p);
		}
		if let Some(p) = &self.cargo_home {
			env = env.set("CARGO_HOME", p);
		}
		if let Some(p) = &self.java_home {
			env = env.set("JAVA_HOME", p);
		}
		if let Some(p) = &self.go_root {
			env = env.set("GOROOT", p);
		}
		if let Some(p) = &self.go_path {
			env = env.set("GOPATH", p);
		}
		env
	}

	fn ro_binds(&self) -> Vec<PathBuf> {
		[
			&self.rustup_home,
			&self.cargo_home,
			&self.java_home,
			&self.go_root,
		]
		.into_iter()
		.filter_map(|p| p.clone())
		.filter(|p| p.exists())
		.collect()
	}
}

fn global_toolchains() -> &'static ToolchainPaths {
	static T: OnceLock<ToolchainPaths> = OnceLock::new();
	T.get_or_init(ToolchainPaths::from_env)
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

	let toolchains = global_toolchains();
	let mut env = Env::empty().set(
		"PATH",
		std::env::var_os("PATH").unwrap_or_else(|| "/usr/bin:/bin".into()),
	);
	// Minimal locale so toolchains don't panic — not a secret channel.
	if let Ok(v) = std::env::var("LANG") {
		env = env.set("LANG", v);
	}
	if let Ok(v) = std::env::var("LC_ALL") {
		env = env.set("LC_ALL", v);
	}
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
	// Narrow scratch — never ambient $HOME.
	let scratch = std::env::temp_dir();
	mounts = mounts.rw(&scratch);
	if let Some(ref cwd) = cmd.cwd {
		mounts = mounts.rw(cwd);
	}

	// Profile → process-wide config overlays → per-command overlay.
	let mut limits = sandbox::overrides::resolve(cmd.profile, None);
	limits = cmd.override_.apply(limits);
	let mut spec = Spec::new(cmd.program, limits)
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
				let status = out.status().expect("success implies exited");
				Ok(StdOutput {
					status,
					stdout: out.stdout,
					stderr: out.stderr,
				})
			} else {
				let status = match out.end {
					sandbox::ProcessEnd::Exited(s) => s.to_string(),
					sandbox::ProcessEnd::Killed(r) => format!("killed ({r})"),
				};
				Err(IsolatedFailure {
					command: command_label,
					kind: IsolatedFailureKind::NonZero { status },
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

/// Resolve the producer-worker binary.
///
/// Order: `NUDOX_PRODUCER_WORKER` → same-dir `producer-worker` next to current
/// exe → `producer-worker` on PATH.
pub fn producer_worker_bin() -> Option<PathBuf> {
	if let Some(raw) = std::env::var_os("NUDOX_PRODUCER_WORKER") {
		let path = PathBuf::from(raw);
		if path.exists() {
			return Some(path);
		}
		if let Ok(w) = which_bin(&path) {
			return Some(w);
		}
	}
	if let Ok(exe) = std::env::current_exe() {
		if let Some(dir) = exe.parent() {
			let candidate = dir.join("producer-worker");
			if candidate.exists() {
				return Some(candidate);
			}
		}
	}
	which_bin(Path::new("producer-worker")).ok()
}

fn which_bin(bin: &Path) -> Result<PathBuf, ()> {
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

/// Whether the worker path is mandatory (no in-process interpreters).
///
/// Delegates to the single prod-gate resolution in
/// [`sandbox::IsolationPolicy::require_worker`].
pub fn require_worker() -> bool {
	sandbox::IsolationPolicy::require_worker()
}

fn nix_pool() -> Option<&'static WorkerPool> {
	static POOL: OnceLock<Option<WorkerPool>> = OnceLock::new();
	POOL.get_or_init(|| {
		let bin = producer_worker_bin()?;
		WorkerPool::new(WorkerPoolConfig::nix(bin)).ok()
	})
	.as_ref()
}

fn parser_pool() -> Option<&'static WorkerPool> {
	static POOL: OnceLock<Option<WorkerPool>> = OnceLock::new();
	POOL.get_or_init(|| {
		let bin = producer_worker_bin()?;
		WorkerPool::new(WorkerPoolConfig::static_parser(bin)).ok()
	})
	.as_ref()
}

/// Lower via pooled worker when available.
///
/// - Dev: returns `None` if no worker binary → caller may fall back in-process.
/// - Prod (`require_worker`): returns `Some(Err)` if the worker is missing.
pub fn try_worker_lower(lang: WorkerLang, root: &Path) -> Option<Result<String, IsolatedFailure>> {
	let pool = match lang {
		WorkerLang::Nix => nix_pool(),
		WorkerLang::Typescript | WorkerLang::Python => parser_pool(),
	};

	match pool {
		Some(pool) => Some(pool.lower(lang, root).map_err(|e| IsolatedFailure {
			command: format!("producer-worker {}", lang.as_str()),
			kind: IsolatedFailureKind::Sandbox(e.to_string()),
			stdout: None,
			stderr: None,
		})),
		None if require_worker() => Some(Err(IsolatedFailure {
			command: format!("producer-worker {}", lang.as_str()),
			kind: IsolatedFailureKind::ToolchainMissing(
				"producer-worker (set NUDOX_PRODUCER_WORKER)".into(),
			),
			stdout: None,
			stderr: None,
		})),
		None => None,
	}
}

/// Warm worker pools at process start (optional; first lower also initializes).
pub fn warm_workers() {
	let _ = nix_pool();
	let _ = parser_pool();
}
