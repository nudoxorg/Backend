//! Tier C · Mode 1 — the tsgo (TypeScript 7) batch checker oracle.
//!
//! For a source-only package, run `tsgo --declaration --emitDeclarationOnly
//! --outDir <tmp>` in the sandbox, then re-run the OXC Tier-A/B extractor over
//! the checker-emitted `.d.ts` — those declarations carry inferred return
//! types, evaluated public-surface types, and resolved re-exports that the
//! purely-syntactic pass cannot recover (the API-Extractor pattern).
//!
//! Producer honesty: this NEVER breaks the producer. Any failure (binary
//! missing, non-zero exit, empty emit — GA gaps microsoft/typescript-go#972,
//! #1952) returns `Err(reason)` and the caller falls back to the syntactic
//! OXC pass. It is also opt-in at runtime via [`ORACLE_ENV`]; unset (the
//! default) means behavior is byte-identical to the Tier-A/B pipeline.

use std::path::{Path, PathBuf};

use ir::entry::Index;
use sandbox::ProducerProfile;

use crate::compile::isolate::{self, IsolatedCommand};
use crate::compile::producer::{self, ForgeContext};

/// Runtime opt-in switch. The oracle runs only when this is set to `1`/`true`.
pub const ORACLE_ENV: &str = "NUDOX_TYPESCRIPT_ORACLE";

/// Is the tsgo oracle enabled for this run?
pub fn enabled() -> bool {
	std::env::var(ORACLE_ENV)
		.map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
		.unwrap_or(false)
}

/// Locate the vendored tsgo binary (Buck2 `resources` entry on the compiler
/// crate). `Err` (e.g. the binary was not vendored for this platform) → the
/// caller falls back to the syntactic pass.
pub fn tsgo_binary() -> std::io::Result<PathBuf> {
	producer::buck_resource("tsgo")
}

/// Shallow heuristic: does `root` already ship `.d.ts` declarations? If so the
/// syntactic pass already has checker-authored types and the oracle adds
/// nothing — skip it. Checks the package root and a `src/` subdir, one level.
fn already_has_declarations(root: &Path) -> bool {
	fn dir_has_dts(dir: &Path) -> bool {
		let Ok(entries) = std::fs::read_dir(dir) else {
			return false;
		};
		entries.flatten().any(|e| {
			e.file_name()
				.to_str()
				.map(|n| n.ends_with(".d.ts") || n.ends_with(".d.mts") || n.ends_with(".d.cts"))
				.unwrap_or(false)
		})
	}
	dir_has_dts(root) || dir_has_dts(&root.join("src"))
}

/// Does `dir` (recursively) contain any emitted `.d.ts`? Used to detect the
/// #972 "no emit on type error" gap.
fn emitted_any_dts(dir: &Path) -> bool {
	let Ok(entries) = std::fs::read_dir(dir) else {
		return false;
	};
	for entry in entries.flatten() {
		let path = entry.path();
		if path.is_dir() {
			if emitted_any_dts(&path) {
				return true;
			}
		} else if path
			.file_name()
			.and_then(|n| n.to_str())
			.map(|n| n.ends_with(".d.ts"))
			.unwrap_or(false)
		{
			return true;
		}
	}
	false
}

/// Run the tsgo batch-emit oracle over the package at `root`.
///
/// On success returns the checker-normalized [`Index`]. On ANY failure returns
/// `Err(reason)` — the caller must fall back to the syntactic pass.
pub fn normalize<C: ForgeContext>(ctx: &C, root: &Path, name: &str) -> Result<Index, String> {
	if already_has_declarations(root) {
		return Err("package already ships .d.ts; syntactic pass suffices".to_string());
	}

	let bin = tsgo_binary().map_err(|e| format!("tsgo binary unavailable: {e}"))?;
	let target = root
		.canonicalize()
		.map_err(|e| format!("canonicalize package root: {e}"))?;

	// Entry files for tsgo to type-check + emit from (it follows imports).
	let entries = super::super::oxc::entry::discover_entry_points(&target)
		.map_err(|e| format!("entry discovery for tsgo: {e}"))?;
	if entries.is_empty() {
		return Err("no TypeScript entry points found".to_string());
	}

	// A per-run tmpfs outDir for the emitted declarations.
	let out_dir = std::env::temp_dir().join(format!(
		"nudox-tsgo-{}-{}",
		std::process::id(),
		name.replace(['/', '\\', '@'], "_")
	));
	let _ = std::fs::remove_dir_all(&out_dir);
	std::fs::create_dir_all(&out_dir).map_err(|e| format!("create tsgo outDir: {e}"))?;

	let mut cmd = IsolatedCommand::new(&bin, ProducerProfile::Typescript)
		.arg("--declaration")
		.arg("--emitDeclarationOnly")
		.arg("--skipLibCheck")
		.arg("--outDir")
		.arg(&out_dir)
		.arg("--rootDir")
		.arg(&target)
		.cwd(&target)
		.ro(&bin)
		.ro(&target)
		.rw(&out_dir);
	for entry in &entries {
		cmd = cmd.arg(entry.as_os_str());
	}

	let run = isolate::run_isolated(ctx, cmd);
	let result = (|| {
		let output = run.map_err(|e| format!("tsgo run failed: {e}"))?;
		if !output.status.success() {
			return Err(format!(
				"tsgo exit {}: {}",
				output.status,
				String::from_utf8_lossy(&output.stderr).trim()
			));
		}
		// #972: tsgo emits nothing when type errors exist.
		if !emitted_any_dts(&out_dir) {
			return Err("tsgo emitted no .d.ts (type errors — GA #972)".to_string());
		}
		// Re-extract via the existing OXC pipeline over the emitted declarations.
		// A package.json (if present) helps entry discovery pick the same root.
		let pkg = target.join("package.json");
		if pkg.is_file() {
			let _ = std::fs::copy(&pkg, out_dir.join("package.json"));
		}
		super::super::oxc::generate_ir(&out_dir, name)
			.map_err(|e| format!("re-extract emitted .d.ts: {e}"))
	})();

	let _ = std::fs::remove_dir_all(&out_dir);
	result
}
