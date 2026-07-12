//! Tier C · in-process `tsz` TypeScript checker oracle.
//!
//! For a source-only package we drive the git-vendored `tsz` compiler
//! (pure Rust) in-process: build a multi-file program, run binding + checking,
//! and emit inference-accurate `.d.ts` per module via the declaration emitter.
//! Those declarations carry checker-INFERRED return types, evaluated object
//! shapes, `Promise<T>` unwrapping, and resolved re-exports that the purely
//! syntactic OXC pass cannot recover. We then re-run the OXC Tier-A extractor
//! over the emitted `.d.ts` (the API-Extractor pattern).
//!
//! Producer honesty: this NEVER breaks the producer. Any failure (`tsz` is
//! pre-release — unsupported syntax, empty emit, panic-free error paths)
//! returns `Err(reason)` and the caller falls back to the syntactic OXC pass.
//! It is opt-in at runtime via [`ORACLE_ENV`]; unset (the default) means
//! behavior is byte-identical to the Tier-A/B pipeline.
//!
//! `tsz` is PURE RUST — it runs directly in-process, NO subprocess/sandbox.
//!
//! # Pipeline (confirmed against vendored tsz main @ `dff7690`)
//!
//! 1. [`tsz_core::parallel::ensure_rayon_global_pool`] — installs a
//!    large-stack process-global Rayon pool (the checker recurses deeply;
//!    default thread stacks overflow). `parse_and_libs.rs:356`.
//! 2. [`tsz_core::parallel::compile_files`] — parse + bind + merge into a
//!    [`MergedProgram`] (`checking.rs:26`). This does NOT run the checker.
//! 3. Per file: build a per-file binder
//!    ([`tsz_core::parallel::create_binder_from_bound_file`], `checking.rs:1042`),
//!    a shared [`QueryCache`] over `program.type_interner`, a
//!    [`CheckerState`] (`state.rs:230`), run `check_source_file`
//!    (`check.rs:1527`) and pull the populated [`TypeCache`] out with
//!    `extract_cache` (`state.rs:873`).
//! 4. Per file: build a [`TypeCacheView`] from the `TypeCache` (public
//!    struct-literal fields; `tsz-cli/.../emit.rs:35`) and drive
//!    [`DeclarationEmitter::with_type_info`] (`setup.rs:137`) →
//!    `emit(source_file)` (`emit_declarations.rs:12`) for the `.d.ts` text.

use std::path::{Path, PathBuf};

use ir::entry::Index;

use tsz_core::checker::{CheckerState, TypeCache};
use tsz_core::declaration_emitter::DeclarationEmitter;
use tsz_core::parallel::{
	self, BoundFile, MergedProgram, create_binder_from_bound_file, ensure_rayon_global_pool,
};
use tsz_solver::construction::QueryCache;

/// Runtime opt-in switch. The oracle runs only when this is set to `1`/`true`.
pub const ORACLE_ENV: &str = "NUDOX_TYPESCRIPT_ORACLE";

/// Is the tsz oracle enabled for this run?
pub fn enabled() -> bool {
	std::env::var(ORACLE_ENV)
		.map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
		.unwrap_or(false)
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

/// Is this a TypeScript source file we should feed to the checker?
///
/// Includes `.ts`/`.tsx`/`.mts`/`.cts` and existing `.d.ts` (as type context),
/// excludes plain JS and non-source dirs.
fn is_ts_source(path: &Path) -> bool {
	let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
		return false;
	};
	let lower = name.to_ascii_lowercase();
	lower.ends_with(".ts")
		|| lower.ends_with(".tsx")
		|| lower.ends_with(".mts")
		|| lower.ends_with(".cts")
}

/// Directories we never descend into when discovering source files.
fn is_skipped_dir(name: &str) -> bool {
	matches!(
		name,
		"node_modules" | ".git" | ".hg" | "dist" | "build" | "out" | "coverage" | "target"
	)
}

/// Walk `root` and collect all TypeScript source files (absolute paths).
fn discover_ts_files(root: &Path) -> Result<Vec<PathBuf>, String> {
	let mut out = Vec::new();
	let mut stack = vec![root.to_path_buf()];
	while let Some(dir) = stack.pop() {
		let entries =
			std::fs::read_dir(&dir).map_err(|e| format!("read_dir {}: {e}", dir.display()))?;
		for entry in entries.flatten() {
			let path = entry.path();
			let file_type = match entry.file_type() {
				Ok(ft) => ft,
				Err(_) => continue,
			};
			if file_type.is_dir() {
				let skip = path
					.file_name()
					.and_then(|n| n.to_str())
					.map(is_skipped_dir)
					.unwrap_or(false);
				if !skip {
					stack.push(path);
				}
			} else if file_type.is_file() && is_ts_source(&path) {
				out.push(path);
			}
		}
	}
	out.sort();
	Ok(out)
}

/// Build the emitter-local [`TypeCacheView`] from a checker-produced
/// [`TypeCache`]. Mirrors `tsz-cli/src/driver/emit.rs::type_cache_view`
/// verbatim (the `TypeCacheView` struct has no constructor — it is built by
/// its public fields).
fn type_cache_view(cache: &TypeCache) -> tsz_emitter::type_cache_view::TypeCacheView {
	tsz_emitter::type_cache_view::TypeCacheView {
		node_types: cache.node_types.to_hash_map(),
		symbol_types: cache.symbol_types.to_hash_map(),
		def_to_symbol: cache.def_to_symbol.clone(),
		def_types: cache.def_types.clone(),
		def_type_params: cache.def_type_params.clone(),
		boxed_types: cache.boxed_types.clone(),
		boxed_def_ids: cache.boxed_def_ids.clone(),
		well_known_symbol_names: cache.well_known_symbol_names.clone(),
		def_to_name: cache.def_to_name.clone(),
	}
}

/// Type-check every file in the merged program and return the per-file
/// [`TypeCache`] keyed by file index (parallel to `program.files`).
///
/// A [`QueryCache`] is shared across all files (memoized subtype/evaluate
/// queries over the one program-wide `TypeInterner`); each file gets its own
/// per-file binder + [`CheckerState`]. This matches the CLI driver's cold
/// build (`tsz-cli/src/driver/check.rs:517,1527`).
fn check_program(program: &MergedProgram) -> Vec<TypeCache> {
	// One QueryCache over the program's interner + shared definition store, so
	// cross-file `Lazy(DefId)` references resolve coherently (check.rs:517).
	let query_cache =
		QueryCache::new(&program.type_interner).with_definition_store(&program.definition_store);

	let mut caches = Vec::with_capacity(program.files.len());
	for (file_idx, file) in program.files.iter().enumerate() {
		let binder = create_binder_from_bound_file(file, program, file_idx);
		let mut checker = CheckerState::new(
			&file.arena,
			&binder,
			&query_cache,
			file.file_name.clone(),
			// CheckerOptions: Default (matches the checker's own defaults;
			// checker.rs:226). Package-tuned strictness is not needed for
			// declaration emit — we only want inferred public-surface types.
			Default::default(),
		);
		checker.check_source_file(file.source_file);
		caches.push(checker.extract_cache());
	}
	caches
}

/// Emit an inference-accurate `.d.ts` for one bound file, or `None` if the
/// declaration emitter produced no output (empty or emit-blocked).
fn emit_declaration(program: &MergedProgram, file: &BoundFile, file_idx: usize, cache: &TypeCache) -> Option<String> {
	// Per-file binder + shared TypeCacheView feed the emitter's inference-aware
	// path (emit.rs:611). Both must outlive `emit()`.
	let binder = create_binder_from_bound_file(file, program, file_idx);
	let view = type_cache_view(cache);

	let mut emitter =
		DeclarationEmitter::with_type_info(&file.arena, view, &program.type_interner, &binder);
	// Ground the emitter in the current file (foreign-symbol/import resolution).
	emitter.set_current_arena(std::sync::Arc::clone(&file.arena), file.file_name.clone());

	let contents = emitter.emit(file.source_file);

	// #972-analogue: a declaration emit blocked by an isolated-declarations /
	// portability error carries an Error-category diagnostic; treat as no emit.
	let blocked = emitter.take_diagnostics().iter().any(|d| {
		d.category == tsz_common::diagnostics::DiagnosticCategory::Error
	});
	if blocked || contents.trim().is_empty() {
		None
	} else {
		Some(contents)
	}
}

/// The emitted `.d.ts` path for a source file, mirroring the source tree layout
/// under `out_dir`: `root/a/b/foo.ts` → `out_dir/a/b/foo.d.ts`.
fn dts_output_path(root: &Path, out_dir: &Path, src: &Path) -> PathBuf {
	let rel = src.strip_prefix(root).unwrap_or(src);
	let mut out = out_dir.join(rel);
	// Replace the final `.ts`/`.tsx`/`.mts`/`.cts` (or `.d.ts`) with `.d.ts`.
	let stem = out
		.file_name()
		.and_then(|n| n.to_str())
		.map(|n| {
			let base = n
				.strip_suffix(".d.ts")
				.or_else(|| n.strip_suffix(".tsx"))
				.or_else(|| n.strip_suffix(".mts"))
				.or_else(|| n.strip_suffix(".cts"))
				.or_else(|| n.strip_suffix(".ts"))
				.unwrap_or(n);
			format!("{base}.d.ts")
		})
		.unwrap_or_else(|| "index.d.ts".to_string());
	out.set_file_name(stem);
	out
}

/// Run the in-process tsz check + declaration-emit oracle over the package at
/// `root`, then re-extract IR from the emitted `.d.ts`.
///
/// On success returns the checker-normalized [`Index`]. On ANY failure returns
/// `Err(reason)` — the caller must fall back to the syntactic pass.
pub fn normalize(root: &Path, name: &str) -> Result<Index, String> {
	if already_has_declarations(root) {
		return Err("package already ships .d.ts; syntactic pass suffices".to_string());
	}

	// The checker recurses deeply — install the large-stack Rayon pool FIRST
	// (parse_and_libs.rs:356). Idempotent; safe to call every run.
	ensure_rayon_global_pool();

	let target = root
		.canonicalize()
		.map_err(|e| format!("canonicalize package root: {e}"))?;

	let src_files = discover_ts_files(&target)?;
	if src_files.is_empty() {
		return Err("no TypeScript source files found".to_string());
	}

	// (file_name, source_text) pairs for the multi-file program.
	let mut inputs: Vec<(String, String)> = Vec::with_capacity(src_files.len());
	for path in &src_files {
		let text = std::fs::read_to_string(path)
			.map_err(|e| format!("read {}: {e}", path.display()))?;
		inputs.push((path.to_string_lossy().into_owned(), text));
	}

	// Parse + bind + merge (checking.rs:26). tsz drives its own libs.
	let program: MergedProgram = parallel::compile_files(inputs);

	// Check every file → per-file TypeCache (parallel to program.files).
	let caches = check_program(&program);

	// Per-run tmp dir mirroring the source tree for emitted declarations.
	let out_dir = std::env::temp_dir().join(format!(
		"nudox-tsz-{}-{}",
		std::process::id(),
		name.replace(['/', '\\', '@'], "_")
	));
	let _ = std::fs::remove_dir_all(&out_dir);
	std::fs::create_dir_all(&out_dir).map_err(|e| format!("create tsz outDir: {e}"))?;

	let result = (|| {
		let mut emitted_any = false;
		for (file_idx, file) in program.files.iter().enumerate() {
			// Only emit for the package's own source files (skip any lib/ambient
			// files tsz injected, which are not under `target`).
			let file_path = PathBuf::from(&file.file_name);
			if !file_path.starts_with(&target) {
				continue;
			}
			let Some(cache) = caches.get(file_idx) else {
				continue;
			};
			let Some(contents) = emit_declaration(&program, file, file_idx, cache) else {
				continue;
			};

			let dts_path = dts_output_path(&target, &out_dir, &file_path);
			if let Some(parent) = dts_path.parent() {
				std::fs::create_dir_all(parent)
					.map_err(|e| format!("create dts dir {}: {e}", parent.display()))?;
			}
			std::fs::write(&dts_path, contents)
				.map_err(|e| format!("write {}: {e}", dts_path.display()))?;
			emitted_any = true;
		}

		if !emitted_any {
			return Err("tsz emitted no .d.ts (unsupported syntax or empty surface)".to_string());
		}

		// A package.json (if present) helps entry discovery pick the same root.
		let pkg = target.join("package.json");
		if pkg.is_file() {
			let _ = std::fs::copy(&pkg, out_dir.join("package.json"));
		}

		// Re-extract via the OXC Tier-A pipeline over the emitted declarations.
		super::super::oxc::generate_ir(&out_dir, name)
			.map_err(|e| format!("re-extract emitted .d.ts: {e}"))
	})();

	let _ = std::fs::remove_dir_all(&out_dir);
	result
}
