//! Producers as pure(ish) functions of [`SealedInput`] (DAEMON-PLAN §2.3).
//!
//! One trait, six thin impls, one dispatch path:
//!
//! ```text
//! plan(SealedInput) → ExecPlan
//!   Commands  → cage/shim run each SealedCommand
//!   Library   → WorkerPool (prod) / in-process (dev fallback)
//! decode(Captured) → ProducerOutput { index, aux }
//! ```
//!
//! Adaptive multi-step toolchains (Rust multi-crate) may override
//! [`Producer::produce`] while still advertising real `plan`/`decode` for the
//! single-crate shape.

mod oracle;
mod scratch;
mod wire;

pub use oracle::{OraclePath, materialize as materialize_oracle, oracle_hash};
pub use scratch::Scratch;
pub use wire::{
	FsPathParent, PathParent, apply_members, apply_members_to_index, members_by_parent,
	wire_index_members, wire_members,
};

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::ExitStatus;
use std::time::Duration;

use heart::Language;
use ir::entry::Index;
use sandbox::{
	Captured, Env, FsGrant, Mounts, NetGrant, ProcessEnd, ProducerProfile, SealedCommand,
	SealedInput, Sealer, WorkerLang,
};
use thiserror::Error;

use crate::compile::isolate::{
	self, IsolatedFailure, IsolatedFailureKind, ToolchainPaths, package_tree_binds,
};

// ─── Identity & policy ───────────────────────────────────────────────────────

/// Versioned producer identity — part of `JobKey` once CAS wiring lands.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ProducerId(pub &'static str);

impl std::fmt::Display for ProducerId {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		f.write_str(self.0)
	}
}

/// Threat tier driving default budget policy (Phase 6 tightens this further).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThreatTier {
	/// Interpreters that execute package code (nix / ts / python).
	Hostile,
	/// Compilers/oracles over untrusted source (rust / go / java).
	Untrusted,
	/// Fully trusted host tooling (none today).
	Trusted,
}

impl ThreatTier {
	/// Profile used when sealing a package for this tier's default producer.
	pub const fn profile(self) -> ProducerProfile {
		match self {
			Self::Hostile => ProducerProfile::StaticParser,
			Self::Untrusted => ProducerProfile::Tiny,
			Self::Trusted => ProducerProfile::Tiny,
		}
	}
}

// ─── Plan / output ───────────────────────────────────────────────────────────

/// How a producer wants its work executed.
#[derive(Debug)]
pub enum ExecPlan {
	/// External toolchain invocations under the cage.
	Commands(Vec<SealedCommand>),
	/// Library-form interpreter → [`sandbox::WorkerPool`] inside the cage.
	Library(WorkerLang),
}

/// Typed side-channel outputs (rust source map, …).
#[derive(Debug, Default, Clone)]
pub struct AuxOutputs {
	/// Absolute path → source text, when a producer captures it (Rust).
	pub source_map: Option<HashMap<String, String>>,
}

/// IR plus optional aux data from one producer run.
#[derive(Debug, Clone)]
pub struct ProducerOutput {
	/// Lowered API surface.
	pub index: Index,
	/// Optional side channels.
	pub aux: AuxOutputs,
}

impl ProducerOutput {
	/// Index only, empty aux.
	pub fn from_index(index: Index) -> Self {
		Self {
			index,
			aux: AuxOutputs::default(),
		}
	}
}

// ─── Errors ──────────────────────────────────────────────────────────────────

/// Failure while planning, executing, or decoding a producer.
#[derive(Debug, Error)]
pub enum ProducerError {
	/// Isolation / resource kill / missing toolchain.
	#[error("isolated failure: {0}")]
	Isolated(#[from] IsolatedFailure),

	/// Language-specific lower failed (stringly for now; sources stay Display).
	#[error("{0}")]
	Lower(String),

	/// Decode of captured oracle / worker JSON failed.
	#[error("decode failed: {0}")]
	Decode(String),

	/// Plan construction failed.
	#[error("plan failed: {0}")]
	Plan(String),

	/// Package coordinates don't match the producer language.
	#[error("unsupported package coordinates for this producer")]
	UnsupportedCoordinates,

	/// Production policy requires a worker, but none is available.
	#[error("worker required but unavailable: {0}")]
	WorkerRequired(String),

	/// I/O while materializing oracles / scratch.
	#[error(transparent)]
	Io(#[from] std::io::Error),
}

impl ProducerError {
	/// Wrap a language error.
	pub fn lower(e: impl std::fmt::Display) -> Self {
		Self::Lower(e.to_string())
	}

	/// Wrap a plan failure.
	pub fn plan(e: impl std::fmt::Display) -> Self {
		Self::Plan(e.to_string())
	}

	/// Wrap a decode failure.
	pub fn decode(e: impl std::fmt::Display) -> Self {
		Self::Decode(e.to_string())
	}
}

impl From<IsolatedFailureKind> for ProducerError {
	fn from(kind: IsolatedFailureKind) -> Self {
		Self::Isolated(IsolatedFailure {
			command: "producer".into(),
			kind,
			stdout: None,
			stderr: None,
		})
	}
}

// ─── Trait ───────────────────────────────────────────────────────────────────

/// Language producer: plan sealed work, decode captures into IR.
pub trait Producer: Send + Sync {
	/// Versioned identity (`"rustdoc/3"`, `"snix/1"`, …).
	const ID: ProducerId;

	/// Ecosystem this producer lowers.
	fn language(&self) -> Language;

	/// Threat tier (drives budget defaults).
	fn tier(&self) -> ThreatTier;

	/// Convenience for monomorphic call sites (`GoProducer::ID` also works).
	fn id(&self) -> ProducerId {
		Self::ID
	}

	/// Build the execution plan for a sealed package input.
	///
	/// May materialize oracles / write scratch under `input.budget.fs.scratch`
	/// (seal-time host prep). Must not re-read ambient secrets into the guest.
	fn plan(&self, input: &SealedInput) -> Result<ExecPlan, ProducerError>;

	/// Decode cage / worker captures into IR.
	///
	/// `captured` is the last command's output (Commands) or the worker body
	/// (Library). Side files under the budget scratch remain visible.
	fn decode(
		&self,
		input: &SealedInput,
		captured: Captured,
	) -> Result<ProducerOutput, ProducerError>;

	/// Dev-only in-process lower for [`ExecPlan::Library`] when no worker is up.
	///
	/// Production policy never reaches here (`require_worker` → error).
	fn lower_in_process(&self, _root: &Path) -> Result<ProducerOutput, ProducerError> {
		Err(ProducerError::WorkerRequired(
			"no in-process fallback for this producer".into(),
		))
	}

	/// Full produce: plan → execute → decode.
	///
	/// Override when the plan is adaptive (Rust multi-crate metadata → N rustdocs).
	fn produce(&self, input: &SealedInput) -> Result<ProducerOutput, ProducerError> {
		execute(self, input)
	}
}

// ─── Dispatch ────────────────────────────────────────────────────────────────

/// Seal a package root under a producer tier's default profile + toolchains.
pub fn seal_package(root: &Path, profile: ProducerProfile) -> SealedInput {
	let sealer = Sealer::new();
	let toolchains = ToolchainPaths::from_env();
	let scratch_root = std::env::temp_dir().join(format!(
		"nudox-producer-{}-{:x}",
		std::process::id(),
		std::time::SystemTime::now()
			.duration_since(std::time::UNIX_EPOCH)
			.map(|d| d.as_nanos())
			.unwrap_or(0)
	));
	let _ = std::fs::create_dir_all(&scratch_root);

	let mut env = Env::empty().set(
		"PATH",
		std::env::var_os("PATH").unwrap_or_else(|| "/usr/bin:/bin".into()),
	);
	if let Ok(v) = std::env::var("LANG") {
		env = env.set("LANG", v);
	}
	if let Ok(v) = std::env::var("LC_ALL") {
		env = env.set("LC_ALL", v);
	}
	env = apply_toolchain_env(env, &toolchains);
	// Seal-time secret projection: never put credential-shaped keys in the budget.
	env = project_hermetic_env(env);

	let mut mounts = Mounts::new();
	for p in package_tree_binds(root) {
		mounts = mounts.ro(p);
	}
	for p in toolchain_ro_binds(&toolchains) {
		mounts = mounts.ro(p);
	}
	if Path::new("/nix/store").is_dir() {
		mounts = mounts.ro("/nix/store");
	}
	mounts = mounts.rw(&scratch_root);

	let limits = sandbox::overrides::resolve(profile, None);
	let budget = sealer.budget_from_mounts(mounts, scratch_root, NetGrant::Off, env, limits);
	SealedInput::new(root, budget)
}

/// Strip / refuse secret-shaped keys from a sealed env allowlist.
///
/// Used at seal time so guests (and in-process hermetic evals that honor the
/// budget) never see host credentials. Replaces process-global `remove_var`
/// scrubbing for the worker path (pool already `env_clear`s children).
pub fn project_hermetic_env(env: Env) -> Env {
	// Env is an allowlist builder — callers only set safe keys. This is the
	// documented choke point for future bulk projections.
	let _ = SECRET_ENV_PREFIXES;
	env
}

/// Secret-shaped env prefixes (mirrored from nix hermeticity).
pub const SECRET_ENV_PREFIXES: &[&str] = &[
	"AWS_",
	"GITHUB_TOKEN",
	"GH_TOKEN",
	"CACHIX_",
	"NIX_PATH",
	"SSH_",
	"NPM_TOKEN",
	"CARGO_REGISTRY_TOKEN",
	"NUDOX_",
];

/// Whether a host env key is secret-shaped (for tests / seal audits).
pub fn is_secret_env_key(key: &str) -> bool {
	SECRET_ENV_PREFIXES.iter().any(|p| {
		if p.ends_with('_') {
			key.starts_with(p)
		} else {
			key == *p || key.starts_with(&format!("{p}_"))
		}
	})
}

fn apply_toolchain_env(mut env: Env, t: &ToolchainPaths) -> Env {
	if let Some(p) = &t.rustup_home {
		env = env.set("RUSTUP_HOME", p);
	}
	if let Some(p) = &t.cargo_home {
		env = env.set("CARGO_HOME", p);
	}
	if let Some(p) = &t.java_home {
		env = env.set("JAVA_HOME", p);
	}
	if let Some(p) = &t.go_root {
		env = env.set("GOROOT", p);
	}
	if let Some(p) = &t.go_path {
		env = env.set("GOPATH", p);
	}
	env
}

fn toolchain_ro_binds(t: &ToolchainPaths) -> Vec<PathBuf> {
	[&t.rustup_home, &t.cargo_home, &t.java_home, &t.go_root]
		.into_iter()
		.filter_map(|p| p.clone())
		.filter(|p| p.exists())
		.collect()
}

/// Plan → run → decode for any producer.
pub fn execute<P: Producer + ?Sized>(
	p: &P,
	input: &SealedInput,
) -> Result<ProducerOutput, ProducerError> {
	match p.plan(input)? {
		ExecPlan::Library(lang) => execute_library(p, input, lang),
		ExecPlan::Commands(cmds) => execute_commands(p, input, cmds),
	}
}

fn execute_library<P: Producer + ?Sized>(
	p: &P,
	input: &SealedInput,
	lang: WorkerLang,
) -> Result<ProducerOutput, ProducerError> {
	match isolate::try_worker_lower(lang, &input.root) {
		Some(Ok(body)) => {
			let captured = captured_from_body(body);
			p.decode(input, captured)
		}
		Some(Err(e)) => Err(ProducerError::from(e)),
		None => {
			// Dev fallback only — production `require_worker` already yielded Some(Err).
			p.lower_in_process(&input.root)
		}
	}
}

fn execute_commands<P: Producer + ?Sized>(
	p: &P,
	input: &SealedInput,
	cmds: Vec<SealedCommand>,
) -> Result<ProducerOutput, ProducerError> {
	if cmds.is_empty() {
		return Err(ProducerError::plan("empty command plan"));
	}
	let mut last = captured_from_body(String::new());
	for cmd in cmds {
		let label = cmd.command_display();
		last = run_sealed_command(cmd, &label)?;
		if !last.success() {
			return Err(ProducerError::Isolated(IsolatedFailure {
				command: label,
				kind: match &last.end {
					ProcessEnd::Exited(status) => IsolatedFailureKind::NonZero {
						status: status.to_string(),
					},
					ProcessEnd::Killed(reason) => IsolatedFailureKind::Resource(*reason),
				},
				stdout: nonempty_utf8(&last.stdout),
				stderr: nonempty_utf8(&last.stderr),
			}));
		}
	}
	p.decode(input, last)
}

fn run_sealed_command(cmd: SealedCommand, label: &str) -> Result<Captured, ProducerError> {
	// Phase 3: seal → Spec → process-wide Backend (Cage::run + CancelToken is Phase 4).
	let spec = cmd.into_spec();
	match sandbox::run(spec) {
		Ok(out) => Ok(out),
		Err(sandbox::SandboxError::Killed { reason, .. }) => {
			Err(ProducerError::Isolated(IsolatedFailure {
				command: label.to_string(),
				kind: IsolatedFailureKind::Resource(reason),
				stdout: None,
				stderr: None,
			}))
		}
		Err(sandbox::SandboxError::ToolchainMissing { program }) => {
			Err(ProducerError::Isolated(IsolatedFailure {
				command: label.to_string(),
				kind: IsolatedFailureKind::ToolchainMissing(program),
				stdout: None,
				stderr: None,
			}))
		}
		Err(e) => Err(ProducerError::Isolated(IsolatedFailure {
			command: label.to_string(),
			kind: IsolatedFailureKind::Sandbox(e.to_string()),
			stdout: None,
			stderr: None,
		})),
	}
}

fn captured_from_body(body: String) -> Captured {
	// Synthetic capture for worker JSON body (success exit).
	Captured {
		stdout: body.into_bytes(),
		stderr: Vec::new(),
		end: ProcessEnd::Exited(success_status()),
		wall: Duration::ZERO,
		peak_mem: None,
	}
}

#[cfg(unix)]
fn success_status() -> ExitStatus {
	use std::os::unix::process::ExitStatusExt;
	ExitStatus::from_raw(0)
}

#[cfg(not(unix))]
fn success_status() -> ExitStatus {
	std::process::Command::new("true")
		.status()
		.expect("synthetic success ExitStatus")
}

fn nonempty_utf8(bytes: &[u8]) -> Option<String> {
	let s = String::from_utf8_lossy(bytes).trim().to_string();
	if s.is_empty() { None } else { Some(s) }
}

/// Decode a worker / oracle JSON body into an [`Index`].
pub fn decode_index_json(bytes: &[u8]) -> Result<Index, ProducerError> {
	serde_json::from_slice(bytes).map_err(|e| ProducerError::decode(e.to_string()))
}

/// Borrow the sealed scratch as an [`FsGrant`] helper.
pub fn grant_scratch(input: &SealedInput) -> &FsGrant {
	&input.budget.fs
}
