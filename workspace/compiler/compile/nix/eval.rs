//! The dynamic layer: hermetically evaluate a flake's outputs with the
//! vendored `snix-eval`, then hand the resulting `Value` tree to the `walker`
//! for traversal + fusion against the static table.
//!
//! # Hermeticity stack (design §7.4)
//!
//! 1. **Pure builtins only** — never `enable_impure` / `impure_builtins()`.
//!    That omits `builtins.getEnv` and `builtins.currentTime` entirely
//!    (`currentTime` is *not* on `EvalIO`; it is injected as a wall-clock
//!    constant by `impure_builtins()`). Sealing by omission is stronger than
//!    scrubbing.
//! 2. [`DocsIO`] — whitelist FS for `import` / path coercion; `get_env` is
//!    sealed closed so a future impure add cannot leak host env.
//! 3. [`scrub_host_secrets`] — defense-in-depth against anything that reads
//!    `std::env` outside EvalIO.
//! 4. Optional worker subprocess (`sandbox::worker`) for crash/OOM isolation.
//!
//! `import` stays enabled (flakes need it); the IO boundary is the cage.
//! Do **not** wire snix-glue / `TvixStoreIO` (reqwest fetchers).

use std::io;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use snix_eval::{EvalMode, Evaluation, EvalIO, FileType};

use super::error::{NixError, Result};
use super::package::FlakeMeta;
use super::syntax::StaticTable;
use super::{context, walker};

/// Wall-clock budget for a single package's evaluation. The worker cgroup is
/// the hard memory ceiling; this remains the cooperative wall budget.
const EVAL_BUDGET: Duration = Duration::from_secs(120);

/// Host env keys that must never be visible to untrusted Nix (design §7.4).
const SECRET_ENV_PREFIXES: &[&str] = &[
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

/// Build a hermetic snix [`Evaluation`] over `io`.
///
/// Invariants encoded here (not at call sites):
/// - pure builtins only (no `getEnv`, no live `currentTime`)
/// - `import` on for flake structure
/// - `NIX_PATH` never inherited (`nix_path(None)` is the builder default)
/// - lazy mode so nixpkgs-scale flakes are not forced wholesale
pub fn hermetic_evaluation(io: Rc<dyn EvalIO>) -> Evaluation<'_, '_, '_, Rc<dyn EvalIO>> {
	Evaluation::builder(io)
		.mode(EvalMode::Lazy)
		.enable_import()
		.build()
}

/// Evaluate the flake at `root` and lower its output tree, or return `None`
/// when there is nothing evaluable (no `flake.nix`, or evaluation degrades to
/// empty). Errors are reserved for hard failures the caller logs and recovers
/// from — per-node eval failures degrade to documented gaps inside `walker`.
pub fn produce(
	root: &Path,
	table: &StaticTable,
	meta: &FlakeMeta,
) -> Result<Option<context::Surface>> {
	let flake_nix = root.join("flake.nix");
	if !flake_nix.exists() {
		return Ok(None);
	}

	// Defense-in-depth: scrub host secrets before any snix code runs. DocsIO
	// already seals get_env; pure builtins omit getEnv/currentTime. Scrub
	// closes the residual process-env channel.
	let _secret_guard = scrub_host_secrets();

	// The whitelist filesystem: the flake tree plus a sibling `inputs/` dir the
	// traversal layer materialized (both read-only, no absolute-path escape).
	let inputs_dir = root.parent().map(|p| p.join("inputs"));
	let io: Rc<dyn EvalIO> = Rc::new(DocsIO::new(root.to_path_buf(), inputs_dir));

	let deadline = Instant::now() + EVAL_BUDGET;
	let code = flake_outputs_shim(root, &discover_input_names(root));

	let evaluation = hermetic_evaluation(io);
	let source_map = evaluation.source_map();
	let result = evaluation.evaluate(&code, Some(flake_nix.clone()));

	for warning in &result.warnings {
		tracing::debug!(?warning, "nix eval warning");
	}
	if !result.errors.is_empty() {
		// A top-level failure (e.g. the shim itself broke) — degrade to
		// static-only rather than aborting the package.
		tracing::warn!(errors = result.errors.len(), "nix eval produced errors; static-only");
		return Ok(None);
	}

	let Some(value) = result.value else {
		return Ok(None);
	};

	if Instant::now() > deadline {
		return Err(NixError::EvalTimeout {
			seconds: EVAL_BUDGET.as_secs(),
		});
	}

	let surface = walker::walk(&value, &source_map, table, meta, deadline);
	Ok(Some(surface))
}

/// Remove secret-bearing env keys for the duration of eval.
///
/// Returns a guard that restores the previous values on drop so concurrent
/// host code outside this thread is less surprised (best-effort; env is
/// process-global).
fn scrub_host_secrets() -> SecretEnvGuard {
	let mut removed = Vec::new();
	let keys: Vec<String> = std::env::vars()
		.map(|(k, _)| k)
		.filter(|k| {
			SECRET_ENV_PREFIXES.iter().any(|p| {
				if p.ends_with('_') {
					k.starts_with(p)
				} else {
					k == *p || k.starts_with(&format!("{p}_"))
				}
			})
		})
		.collect();
	for key in keys {
		if let Ok(val) = std::env::var(&key) {
			removed.push((key.clone(), val));
			// SAFETY: eval is scheduled on a dedicated per-package thread /
			// worker; concurrent getenv during scrub is a microseconds race,
			// not the security boundary (DocsIO + pure builtins seal getEnv).
			unsafe {
				std::env::remove_var(&key);
			}
		}
	}
	SecretEnvGuard { removed }
}

struct SecretEnvGuard {
	removed: Vec<(String, String)>,
}

impl Drop for SecretEnvGuard {
	fn drop(&mut self) {
		for (k, v) in self.removed.drain(..) {
			unsafe {
				std::env::set_var(k, v);
			}
		}
	}
}

// ───────────────────────────────────────────────────────────────────────────
// Flake-outputs shim
// ───────────────────────────────────────────────────────────────────────────

/// A flake-compat-style fixed point: import the flake, synthesize each input
/// as `{ outPath, rev, narHash, lastModified, … } // outputs`, and call
/// `outputs (inputs // { inherit self; })`.
fn flake_outputs_shim(root: &Path, inputs: &[String]) -> String {
	let root_str = root.display();
	let mut input_bindings = String::new();
	for name in inputs {
		input_bindings.push_str(&format!(
			r#"    {name} = let
      src = /{root}/../inputs/{name};
      flakePath = src + "/flake.nix";
      base = {{ outPath = src; rev = "0000000000000000000000000000000000000000"; narHash = ""; lastModified = 0; }};
      outs = if builtins.pathExists flakePath
             then (import flakePath).outputs (allInputs // {{ self = base; }})
             else {{}};
    in base // outs;
"#,
			name = name,
			root = root_str
		));
	}

	format!(
		r#"let
  flake = import /{root}/flake.nix;
  allInputs = rec {{
{inputs}  }};
  self = flake.outputs (allInputs // {{ inherit self; outPath = /{root}; }});
in self
"#,
		root = root_str,
		inputs = input_bindings,
	)
}

/// Read the locked input names from `flake.lock` (best-effort); an absent or
/// unreadable lock yields an empty set (the shim then supplies no inputs).
fn discover_input_names(root: &Path) -> Vec<String> {
	let lock = root.join("flake.lock");
	let Ok(text) = std::fs::read_to_string(&lock) else {
		return Vec::new();
	};
	let Ok(json) = serde_json::from_str::<serde_json::Value>(&text) else {
		return Vec::new();
	};
	let root_key = json
		.get("root")
		.and_then(|r| r.as_str())
		.unwrap_or("root");
	json.get("nodes")
		.and_then(|n| n.get(root_key))
		.and_then(|r| r.get("inputs"))
		.and_then(|i| i.as_object())
		.map(|m| m.keys().cloned().collect())
		.unwrap_or_default()
}

// ───────────────────────────────────────────────────────────────────────────
// Hermetic whitelist filesystem
// ───────────────────────────────────────────────────────────────────────────

/// A read-only [`EvalIO`] that serves exactly the flake tree and the
/// materialized `inputs/` directory. Everything else is denied. `import_path`
/// is identity (no store); hermeticity is enforced by path containment.
pub struct DocsIO {
	root: PathBuf,
	inputs: Option<PathBuf>,
	/// Tripped if an escape attempt is seen (surfaced as a warning, not fatal).
	escaped: AtomicBool,
}

impl DocsIO {
	/// Whitelist `root` and optional sibling `inputs` directory.
	pub fn new(root: PathBuf, inputs: Option<PathBuf>) -> Self {
		Self {
			root,
			inputs,
			escaped: AtomicBool::new(false),
		}
	}

	/// Whether `path` is contained within the flake root or inputs dir.
	fn is_allowed(&self, path: &Path) -> bool {
		let canonical = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
		let under = |base: &Path| {
			base.canonicalize()
				.map(|b| canonical.starts_with(&b))
				.unwrap_or(false)
		};
		if under(&self.root) {
			return true;
		}
		if let Some(inputs) = &self.inputs {
			if under(inputs) {
				return true;
			}
		}
		self.escaped.store(true, Ordering::Relaxed);
		false
	}

	/// Whether any escape attempt was observed during evaluation.
	pub fn escaped(&self) -> bool {
		self.escaped.load(Ordering::Relaxed)
	}

	fn guard(&self, path: &Path) -> io::Result<()> {
		if self.is_allowed(path) {
			Ok(())
		} else {
			Err(io::Error::new(
				io::ErrorKind::PermissionDenied,
				format!("path {} escapes the hermetic flake root", path.display()),
			))
		}
	}
}

impl EvalIO for DocsIO {
	fn path_exists(&self, path: &Path) -> io::Result<bool> {
		if !self.is_allowed(path) {
			return Ok(false);
		}
		Ok(path.exists())
	}

	fn open(&self, path: &Path) -> io::Result<Box<dyn io::Read>> {
		self.guard(path)?;
		let file = std::fs::File::open(path)?;
		Ok(Box::new(file))
	}

	fn file_type(&self, path: &Path) -> io::Result<FileType> {
		self.guard(path)?;
		let meta = std::fs::symlink_metadata(path)?;
		Ok(if meta.is_dir() {
			FileType::Directory
		} else if meta.file_type().is_symlink() {
			FileType::Symlink
		} else {
			FileType::Regular
		})
	}

	fn read_dir(&self, path: &Path) -> io::Result<Vec<(bytes::Bytes, FileType)>> {
		self.guard(path)?;
		let mut entries = Vec::new();
		for entry in std::fs::read_dir(path)? {
			let entry = entry?;
			let name = bytes::Bytes::from(entry.file_name().to_string_lossy().into_owned());
			let ft = entry.file_type()?;
			let kind = if ft.is_dir() {
				FileType::Directory
			} else if ft.is_symlink() {
				FileType::Symlink
			} else {
				FileType::Regular
			};
			entries.push((name, kind));
		}
		Ok(entries)
	}

	fn import_path(&self, path: &Path) -> io::Result<PathBuf> {
		// No store: importing a path is identity within the sandbox.
		self.guard(path)?;
		Ok(path.to_path_buf())
	}

	fn get_env(&self, _key: &std::ffi::OsStr) -> Option<std::ffi::OsString> {
		// Sealed: even if impure builtins are added later, EvalIO must not
		// expose host credentials. Combined with pure-builtins omission this
		// is the §7.4 env discipline.
		None
	}
}

#[cfg(test)]
mod env_seal_tests {
	use super::*;
	use snix_eval::EvalIO;

	#[test]
	fn docs_io_get_env_always_none() {
		let io = DocsIO::new(std::env::temp_dir(), None);
		unsafe {
			std::env::set_var("AWS_SECRET_ACCESS_KEY", "test-leak");
			std::env::set_var("GITHUB_TOKEN", "ghp_test");
		}
		assert!(io
			.get_env(std::ffi::OsStr::new("AWS_SECRET_ACCESS_KEY"))
			.is_none());
		assert!(io.get_env(std::ffi::OsStr::new("GITHUB_TOKEN")).is_none());
		assert!(io.get_env(std::ffi::OsStr::new("PATH")).is_none());
		unsafe {
			std::env::remove_var("AWS_SECRET_ACCESS_KEY");
			std::env::remove_var("GITHUB_TOKEN");
		}
	}

	#[test]
	fn scrub_removes_secret_prefixes() {
		unsafe {
			std::env::set_var("AWS_ACCESS_KEY_ID", "AKIA");
			std::env::set_var("CACHIX_AUTH_TOKEN", "xyz");
			std::env::set_var("HARMLESS_VAR", "keep");
		}
		{
			let _g = scrub_host_secrets();
			assert!(std::env::var("AWS_ACCESS_KEY_ID").is_err());
			assert!(std::env::var("CACHIX_AUTH_TOKEN").is_err());
			assert_eq!(std::env::var("HARMLESS_VAR").ok().as_deref(), Some("keep"));
		}
		assert_eq!(
			std::env::var("AWS_ACCESS_KEY_ID").ok().as_deref(),
			Some("AKIA")
		);
		unsafe {
			std::env::remove_var("AWS_ACCESS_KEY_ID");
			std::env::remove_var("CACHIX_AUTH_TOKEN");
			std::env::remove_var("HARMLESS_VAR");
		}
	}

	/// Pure hermetic eval: `builtins.getEnv` is absent (not just empty).
	#[test]
	fn hermetic_eval_omits_get_env_builtin() {
		unsafe {
			std::env::set_var("AWS_SECRET_ACCESS_KEY", "must-not-appear");
		}
		let io: Rc<dyn EvalIO> = Rc::new(DocsIO::new(std::env::temp_dir(), None));
		let evaluation = hermetic_evaluation(io);
		let result = evaluation.evaluate(r#"builtins.getEnv "AWS_SECRET_ACCESS_KEY""#, None);
		// Either errors (attribute missing) or yields empty — never the secret.
		if let Some(v) = &result.value {
			let s = format!("{v:?}");
			assert!(
				!s.contains("must-not-appear"),
				"getEnv leaked host secret: {s}"
			);
		}
		// Body of errors/warnings must not include the planted value either.
		let dump = format!("{:?}", result.errors);
		assert!(!dump.contains("must-not-appear"));
		unsafe {
			std::env::remove_var("AWS_SECRET_ACCESS_KEY");
		}
	}

	/// Pure hermetic eval: `builtins.currentTime` is absent (not wall clock).
	#[test]
	fn hermetic_eval_omits_current_time_builtin() {
		let io: Rc<dyn EvalIO> = Rc::new(DocsIO::new(std::env::temp_dir(), None));
		let evaluation = hermetic_evaluation(io);
		let result = evaluation.evaluate("builtins.currentTime", None);
		// Pure builtins do not register currentTime. A successful integer
		// would mean impure injection slipped in — reject any non-error path
		// that looks like a live timestamp.
		if let Some(snix_eval::Value::Integer(n)) = result.value {
			panic!("currentTime must not be a live clock; got {n}");
		}
	}

	#[test]
	fn docs_io_denies_path_escape() {
		let root = std::env::temp_dir().join(format!("nudox-docsio-{}", std::process::id()));
		std::fs::create_dir_all(&root).unwrap();
		let io = DocsIO::new(root.clone(), None);
		assert!(io.open(Path::new("/etc/passwd")).is_err());
		assert!(io.escaped());
		let _ = std::fs::remove_dir_all(&root);
	}
}
