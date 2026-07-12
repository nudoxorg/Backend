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
//! Adaptive multi-step toolchains (Rust multi-crate, Java javac+javadoc) override
//! [`Producer::produce`] and return [`ProducerError::Plan`] / [`ProducerError::Decode`]
//! from `plan`/`decode` so introspection never sees a hollow empty command list.

mod resource;
pub mod runtime;
mod scratch;
mod wire;

pub use resource::buck_resource;
pub use runtime::{
	ForgeContext, LocalForgeContext, cache_get_or_build, execute_plan, run_producer,
};
pub use sandbox::SandboxKey;
pub use scratch::Scratch;
pub use wire::{
	FsPathParent, PathParent, apply_members, apply_members_to_index, members_by_parent,
	wire_index_members, wire_members,
};

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::ExitStatus;
use std::time::Duration;

use heart::{JobKey, Language};
use ir::entry::Index;
use sandbox::{
	CancelToken, Captured, Env, Mounts, ProcessEnd, ProducerProfile, SealedCommand,
	SealedInput, ToolchainSet, WorkerLang,
};
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::compile::isolate::{
	IsolatedFailure, IsolatedFailureKind, package_tree_binds,
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

// `ThreatTier` now lives in `sandbox::budget` (below the compiler in the dep
// DAG) so it can drive the seal-time budget clamp directly. Re-exported here
// (`sandbox::ThreatTier`) for the producer impls that name it via this module.
pub use sandbox::ThreatTier;

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
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct AuxOutputs {
	/// Absolute path → source text, when a producer captures it (Rust).
	pub source_map: Option<HashMap<String, String>>,
}

/// IR plus optional aux data from one producer run.
#[derive(Debug, Clone, Serialize, Deserialize)]
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

	/// Plan construction failed (including adaptive multi-step producers).
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

	/// Adaptive multi-step: use [`Producer::produce`], not plan→execute.
	pub fn adaptive(step: &str) -> Self {
		Self::Plan(format!(
			"{step}: adaptive multi-step — call produce() (Phase 4 stages sealed commands)"
		))
	}
}

// ─── Trait ───────────────────────────────────────────────────────────────────

/// Language producer: plan sealed work, decode captures into IR.
pub trait Producer: Send + Sync {
	/// Versioned identity (`"rustdoc/3"`, `"snix/1"`, …).
	const ID: ProducerId;

	/// Ecosystem this producer lowers.
	fn language(&self) -> Language;

	/// Threat tier (drives budget *policy*; ceilings via [`Self::profile`]).
	fn tier(&self) -> ThreatTier;

	/// Resource profile for sealing / cage limits (language-specific, not tier-only).
	fn profile(&self) -> ProducerProfile {
		match self.language() {
			Language::Rust => ProducerProfile::Rust,
			Language::Go => ProducerProfile::Go,
			Language::Java => ProducerProfile::Java,
			Language::Nix => ProducerProfile::Nix,
			Language::Python | Language::Typescript => ProducerProfile::StaticParser,
		}
	}

	/// Convenience for monomorphic call sites (`GoProducer::ID` also works).
	fn id(&self) -> ProducerId {
		Self::ID
	}

	/// Build the execution plan for a sealed package input.
	///
	/// May materialize oracles / write scratch under `input.budget.fs.scratch`
	/// (seal-time host prep). Must not re-read ambient secrets into the guest.
	///
	/// Adaptive multi-step producers return [`ProducerError::Plan`] here and
	/// own orchestration in [`Self::produce`].
	///
	/// `ctx` is available for sealing external-command plans against the injected
	/// toolchains / overrides.
	fn plan<C: ForgeContext>(
		&self,
		ctx: &C,
		input: &SealedInput,
	) -> Result<ExecPlan, ProducerError>;

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
	/// Production policy never reaches here (`require_worker` → error). Command
	/// producers (Go/Java one-shot path) run their external toolchain through
	/// `ctx.cage()`.
	fn lower_in_process<C: ForgeContext>(
		&self,
		_ctx: &C,
		_root: &Path,
	) -> Result<ProducerOutput, ProducerError> {
		Err(ProducerError::WorkerRequired(
			"no in-process fallback for this producer".into(),
		))
	}

	/// Full produce: plan → execute → decode, under the injected context.
	///
	/// Override when the plan is adaptive (Rust multi-crate metadata → N rustdocs).
	fn produce<C: ForgeContext>(
		&self,
		ctx: &C,
		input: &SealedInput,
	) -> Result<ProducerOutput, ProducerError> {
		execute(ctx, self, input)
	}
}

// ─── Seal + RAII scratch ─────────────────────────────────────────────────────

/// Sealed package root + owned scratch (cleaned on drop).
///
/// Hold this for the duration of [`Producer::produce`] so the budget's RW
/// scratch remains valid and is removed afterward.
pub struct SealedPackage {
	input: SealedInput,
	_scratch: Scratch,
}

impl SealedPackage {
	/// Borrow the sealed input.
	pub fn input(&self) -> &SealedInput {
		&self.input
	}

	/// Package root.
	pub fn root(&self) -> &Path {
		&self.input.root
	}
}

/// Producer version domain-separator for the job key (bump on IR-shape change).
pub const PRODUCER_VERSION: &str = "nudox-producer/2";

/// Seal a package root under a language profile + injected toolchains/overrides.
///
/// Scratch is a unique temp dir owned by the returned [`SealedPackage`] (RAII).
/// The job key is `H(producer_version ‖ toolchain.digest ‖ source ‖ lock)`, so
/// re-indexing an unchanged package is a CAS hit under `run_producer`.
///
/// This is the sealer boundary: it reads no policy env — `PATH`/locale/toolchain
/// discovery are the injected [`ToolchainSet`]'s job (done once at assemble).
pub fn seal_package(
	toolchains: &ToolchainSet,
	overrides: &sandbox::OverrideTable,
	root: &Path,
	profile: ProducerProfile,
	tier: ThreatTier,
	package: Option<&SandboxKey>,
	source_hash: heart::ContentHash,
	dep_lock_hash: heart::ContentHash,
) -> Result<SealedPackage, ProducerError> {
	let scratch = Scratch::temp("producer")?;
	let scratch_root = scratch.path().to_path_buf();

	// A minimal, hermetic env: a fixed PATH (the cage sets its own default too)
	// plus the injected toolchain bindings. No ambient host env is read here.
	let mut env = Env::empty().set("PATH", "/usr/bin:/bin:/nix/var/nix/profiles/default/bin");
	env = toolchains.apply_env(env);
	// Defense-in-depth: strip secret-shaped keys if any bulk-copy ever lands.
	env = project_hermetic_env(env);

	let mut mounts = Mounts::new();
	for p in package_tree_binds(root) {
		mounts = mounts.ro(p);
	}
	for p in toolchains.ro_binds() {
		mounts = mounts.ro(p);
	}
	if Path::new("/nix/store").is_dir() {
		mounts = mounts.ro("/nix/store");
	}
	mounts = mounts.rw(&scratch_root);

	// Per-package / per-profile overlay first (Phase 4 override table); the
	// threat-tier ceiling then clamps it down inside `Job::seal`. A Hostile
	// interpreter can never be granted more than its tier permits, however loose
	// the profile or a per-package override is.
	let resolved = overrides.resolve(profile, package);

	let key = JobKey::derive(
		PRODUCER_VERSION.as_bytes(),
		toolchains.digest().as_bytes(),
		source_hash.as_bytes(),
		dep_lock_hash.as_bytes(),
	);

	// Route through the acquire→seal typestate (DAEMON-PLAN §2.1) so the net-off
	// guarantee is *type-enforced* on the live path, not merely a runtime value:
	// `Job<Acquiring>::seal(tier)` is the sole constructor of a `Job<Sealed>`, it
	// forces `net = tier.net_default()` (always `NetGrant::Off`) and clamps the
	// resolved limits to `tier` — so `into_input()` yields a `SealedInput` whose
	// `CapabilityBudget` *cannot* carry the network on. Behavior is preserved
	// exactly: the projected budget is
	// `CapabilityBudget::new(FsGrant::from_mounts(mounts, scratch), Off,
	//  env, tier.clamp(resolved))`, byte-for-byte what `budget_from_mounts` +
	// `SealedInput::new` produced before, with the same `key` and `root`.
	//
	// The acquiring `net` posture is irrelevant here — this is already the seal
	// boundary (acquisition is complete), and `seal()` discards it regardless.
	let fs = sandbox::FsGrant::from_mounts(mounts, scratch_root);
	let input = sandbox::Job::acquiring(
		key,
		root,
		fs,
		env,
		resolved,
		sandbox::NetGrant::Off,
	)
	.seal(tier)
	.into_input();

	Ok(SealedPackage {
		input,
		_scratch: scratch,
	})
}

/// Strip secret-shaped keys from a sealed env allowlist.
///
/// `Env` is intentionally not ambient, but bulk `from_pairs` can still inject
/// credentials. This is the choke point that drops them before the budget is
/// sealed. Replaces process-global `remove_var` scrubbing.
pub fn project_hermetic_env(env: Env) -> Env {
	Env::from_pairs(
		env.into_pairs()
			.into_iter()
			.filter(|(k, _)| !is_secret_env_key(&k.to_string_lossy())),
	)
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

/// Plan → run → decode for any producer, under the injected context.
pub fn execute<C: ForgeContext, P: Producer + ?Sized>(
	ctx: &C,
	p: &P,
	input: &SealedInput,
) -> Result<ProducerOutput, ProducerError> {
	match p.plan(ctx, input)? {
		ExecPlan::Library(lang) => execute_library(ctx, p, input, lang),
		ExecPlan::Commands(cmds) => execute_commands(ctx, p, input, cmds),
	}
}

fn execute_library<C: ForgeContext, P: Producer + ?Sized>(
	ctx: &C,
	p: &P,
	input: &SealedInput,
	lang: WorkerLang,
) -> Result<ProducerOutput, ProducerError> {
	match ctx.worker_pool(lang) {
		Some(pool) => {
			let body = pool.lower(lang, &input.root).map_err(|e| {
				ProducerError::Isolated(IsolatedFailure {
					command: format!("producer-worker {}", lang.as_str()),
					kind: IsolatedFailureKind::Sandbox(e.to_string()),
					stdout: None,
					stderr: None,
				})
			})?;
			let captured = captured_from_body(body);
			p.decode(input, captured)
		}
		None if ctx.require_worker() => Err(ProducerError::WorkerRequired(format!(
			"producer-worker {} unavailable and worker required",
			lang.as_str()
		))),
		None => {
			// Dev fallback only — production `require_worker` returns above.
			p.lower_in_process(ctx, &input.root)
		}
	}
}

fn execute_commands<C: ForgeContext, P: Producer + ?Sized>(
	ctx: &C,
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
		last = run_sealed_command(ctx, cmd, &label)?;
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

/// Run one sealed command through the injected cage, honoring the never token.
fn run_sealed_command<C: ForgeContext>(
	ctx: &C,
	cmd: SealedCommand,
	label: &str,
) -> Result<Captured, ProducerError> {
	match ctx.cage().run(cmd, &CancelToken::never()) {
		Ok(out) => Ok(out),
		Err(e) => Err(ProducerError::Isolated(cage_error_to_isolated(e, label))),
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

#[cfg(test)]
mod hermetic_env_tests {
	use super::*;

	#[test]
	fn project_strips_secret_shaped_keys() {
		let env = Env::empty()
			.set("PATH", "/usr/bin")
			.set("AWS_SECRET_ACCESS_KEY", "leak")
			.set("GITHUB_TOKEN", "ghp")
			.set("HARMLESS", "ok");
		let projected = project_hermetic_env(env);
		let keys: Vec<_> = projected
			.iter()
			.map(|(k, _)| k.to_string_lossy().into_owned())
			.collect();
		assert!(keys.iter().any(|k| k == "PATH"));
		assert!(keys.iter().any(|k| k == "HARMLESS"));
		assert!(!keys.iter().any(|k| k.contains("AWS")));
		assert!(!keys.iter().any(|k| k.contains("GITHUB")));
	}
}
