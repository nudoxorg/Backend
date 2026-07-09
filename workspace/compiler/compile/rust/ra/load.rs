//! Load a Cargo workspace once via `ra_ap_load_cargo` into a
//! [`LoadedWorkspace`] held for the duration of lowering.

use std::{
	path::{Path, PathBuf},
	thread,
};

use ra_ap_ide_db::{RootDatabase, prime_caches};
use ra_ap_load_cargo::{LoadCargoConfig, ProcMacroServerChoice, load_workspace};
use ra_ap_paths::AbsPathBuf;
use ra_ap_proc_macro_api::ProcMacroClient;
use ra_ap_project_model::{CargoConfig, ProjectManifest, ProjectWorkspace, RustLibSource};
use ra_ap_vfs::Vfs;
use tracing::{debug, warn};

use super::super::error::Package;

/// Producer knobs for a single workspace load.
#[derive(Debug, Clone)]
pub struct ExtractConfig {
	pub document_private: bool,
	/// → `CARGO_NET_OFFLINE` + cargo `--offline`.
	pub offline: bool,
	/// Run build scripts (`load_out_dirs_from_check`); default true.
	pub run_build_scripts: bool,
	pub proc_macros: ProcMacroPolicy,
	/// Blanket / auto-trait probe depth (§6.2).
	pub probe: ProbeTier,
	/// `parallel_prime_caches` worker count; default `min(8, cores)`.
	pub num_threads: usize,
}

impl ExtractConfig {
	/// Defaults matched to the rustdoc path (offline, build scripts on, std probes).
	pub fn for_extract(document_private: bool) -> Self {
		Self {
			document_private,
			offline: true,
			run_build_scripts: true,
			proc_macros: ProcMacroPolicy::Sysroot,
			probe: ProbeTier::Std,
			num_threads: thread::available_parallelism()
				.map(|n| n.get().min(8))
				.unwrap_or(1),
		}
	}
}

/// How the proc-macro server is located.
#[derive(Debug, Clone)]
pub enum ProcMacroPolicy {
	/// Use the sysroot's `rust-analyzer-proc-macro-srv`.
	Sysroot,
	/// Explicit path to a server binary built with the same toolchain.
	Explicit(PathBuf),
	/// Skip expansion entirely.
	Disabled,
}

/// How deep auto/blanket-impl synthesis goes (§6.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ProbeTier {
	/// Local crate impls only.
	Off,
	/// Local + std/core/alloc blankets (default; rustdoc parity).
	#[default]
	Std,
	/// Whole dependency graph (opt-in; expensive).
	Graph,
}

/// One loaded analysis session. Dropping `_proc_macro` kills expansion.
pub struct LoadedWorkspace {
	pub db: RootDatabase,
	pub vfs: Vfs,
	/// Kept for package/target metadata (`documented_local_packages`).
	pub ws: ProjectWorkspace,
	_proc_macro: Option<ProcMacroClient>,
}

/// Load `root` (a crate or workspace directory / `Cargo.toml`) into HIR.
///
/// Mirrors `load_workspace_at` but retains [`ProjectWorkspace`] for package
/// enumeration. Primes type caches in parallel before returning.
pub fn load(root: &Path, cfg: &ExtractConfig) -> Result<LoadedWorkspace, Package> {
	let cargo_config = cargo_config(cfg);
	let load_config = load_config(cfg);

	let abs = abs_path(root)?;
	let manifest = ProjectManifest::discover_single(abs.as_ref())
		.map_err(|e| Package::LoadFailed(e.to_string()))?;

	let mut ws = ProjectWorkspace::load(manifest, &cargo_config, &|msg| {
		debug!(target: "ra_load", "{msg}");
	})
	.map_err(|e| Package::LoadFailed(e.to_string()))?;

	if cfg.run_build_scripts {
		match ws.run_build_scripts(&cargo_config, &|msg| {
			debug!(target: "ra_load", "{msg}");
		}) {
			Ok(scripts) => {
				if let Some(err) = scripts.error() {
					warn!(error = %err, "build scripts reported errors; OUT_DIR items may be missing");
				}
				ws.set_build_scripts(scripts);
			}
			Err(e) => {
				warn!(error = %e, "build scripts failed; continuing without OUT_DIR");
			}
		}
	}

	// Clone so we keep `ws` for package metadata after load consumes a copy.
	let (db, vfs, proc_macro) =
		load_workspace(ws.clone(), &cargo_config.extra_env, &load_config)
			.map_err(|e| Package::LoadFailed(e.to_string()))?;

	if proc_macro.is_none() && !matches!(cfg.proc_macros, ProcMacroPolicy::Disabled) {
		// Soft degradation: continue without expansions; surface as a warning.
		// Callers may upgrade this to `Package::ProcMacroDegraded` once Index
		// producer diagnostics exist.
		warn!("proc-macro server unavailable; macro-generated items will be missing");
	}

	// Explicit prime (prefill_caches is false so we can map Cancelled cleanly).
	ra_ap_base_db::salsa::Cancelled::catch(|| {
		prime_caches::parallel_prime_caches(&db, cfg.num_threads, &|_| {});
	})
	.map_err(|_| Package::Cancelled)?;

	Ok(LoadedWorkspace { db, vfs, ws, _proc_macro: proc_macro })
}

fn cargo_config(cfg: &ExtractConfig) -> CargoConfig {
	// Start from Default so `extra_env` uses RA's rustc-hash 2.x hasher
	// (our crate's `rustc_hash` alias is still 1.x and is not type-compatible).
	let mut cargo = CargoConfig {
		sysroot: Some(RustLibSource::Discover),
		all_targets: false,
		// Exclude `#[cfg(test)]` — matches today's rustdoc pass.
		set_test: false,
		// Deps required for cross-crate resolution.
		no_deps: false,
		..CargoConfig::default()
	};
	if cfg.offline {
		cargo.extra_env.insert("CARGO_NET_OFFLINE".into(), Some("true".into()));
		cargo.extra_args.push("--offline".into());
	}
	cargo
}

fn load_config(cfg: &ExtractConfig) -> LoadCargoConfig {
	LoadCargoConfig {
		load_out_dirs_from_check: cfg.run_build_scripts,
		with_proc_macro_server: proc_macro_choice(&cfg.proc_macros),
		// We prime explicitly after load so Cancelled → Package::Cancelled.
		prefill_caches: false,
		num_worker_threads: cfg.num_threads.max(1),
		proc_macro_processes: 1,
	}
}

fn proc_macro_choice(policy: &ProcMacroPolicy) -> ProcMacroServerChoice {
	match policy {
		ProcMacroPolicy::Sysroot => ProcMacroServerChoice::Sysroot,
		ProcMacroPolicy::Explicit(path) => {
			ProcMacroServerChoice::Explicit(AbsPathBuf::assert_utf8(path.clone()))
		}
		ProcMacroPolicy::Disabled => ProcMacroServerChoice::None,
	}
}

fn abs_path(root: &Path) -> Result<AbsPathBuf, Package> {
	let path = if root.is_absolute() {
		root.to_path_buf()
	} else {
		std::env::current_dir().map_err(Package::Io)?.join(root)
	};
	Ok(AbsPathBuf::assert_utf8(path))
}
