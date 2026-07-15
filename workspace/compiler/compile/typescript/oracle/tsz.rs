//! Tier C · in-process `tsz` TypeScript checker oracle.
//!
//! For a source-only package we drive the git-vendored `tsz` compiler (pure
//! Rust) in-process: build a multi-file program, run binding + checking, then
//! query the CHECKER directly (node/symbol → `TypeId` → structured `TypeData`)
//! for the inference-accurate types the purely syntactic OXC pass cannot
//! recover — checker-inferred return types, evaluated object shapes,
//! `Promise<T>` unwrapping, and cross-module type resolution.
//!
//! Unlike the earlier design, this NO LONGER emits `.d.ts` text and re-parses
//! it. The OXC Tier-A pass gives structure + syntactic types; we then walk the
//! resulting [`Index`] and ENRICH the entries whose types are missing/opaque by
//! splicing in the checker's answer. Mode 1 (inferred types) and Mode 2
//! (cross-module resolution) collapse into this one enrichment pass.
//!
//! Producer honesty: this NEVER breaks the producer. Any failure (`tsz` is
//! pre-release — unsupported syntax, empty check, panic-guarded error paths)
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
//!    default thread stacks overflow).
//! 2. [`tsz_core::parallel::compile_files`] — parse + bind + merge into a
//!    [`MergedProgram`]. This does NOT run the checker.
//! 3. Per file: build a per-file binder
//!    ([`tsz_core::parallel::create_binder_from_bound_file`]), a shared
//!    [`QueryCache`] over `program.type_interner`, a [`CheckerState`], run
//!    `check_source_file`, then query top-level symbols' types via
//!    `get_type_of_node` / `get_type_of_symbol` and map [`TypeData`] →
//!    [`ir::ty::Type`] (see [`super::tsz_types`]).
//! 4. Splice the recovered types into the OXC-produced [`Index`].

use std::path::{Path, PathBuf};

use ir::entry::Index;
use ir::kind::Entry;
use ir::parameter::Parameter;
use ir::pipeline::output_parameters_from_type;
use ir::ty::Type;

use tsz_binder::state::BinderState;
use tsz_core::checker::CheckerState;
use tsz_core::parallel::{
	self, MergedProgram, create_binder_from_bound_file, ensure_rayon_global_pool,
};
use tsz_solver::construction::QueryCache;

use super::tsz_types;

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

/// Strip a TypeScript-like extension off a filename to get its module stem.
/// `foo.ts` → `foo`, `bar.d.ts` → `bar`, `mod.tsx` → `mod`.
fn file_stem(name: &str) -> String {
	for suf in [
		".d.ts", ".d.mts", ".d.cts", ".tsx", ".mts", ".cts", ".ts",
	] {
		if let Some(base) = name.strip_suffix(suf) {
			return base.to_string();
		}
	}
	name.to_string()
}

/// The inferred type recovered from the checker for one top-level symbol.
struct RecoveredType {
	/// The symbol's own inferred type (e.g. the object shape of a `const`,
	/// the arrow type of a function, the value type of a variable).
	own: Type,
	/// For functions/callables, the inferred RETURN type (`R` in `() => R`);
	/// `None` for non-callable symbols.
	ret: Option<Type>,
}

/// Type-check every file and collect, per top-level exported/local name, the
/// checker-recovered types. Keyed both by `(file_stem, name)` (precise) and by
/// bare `name` (used only when globally unique) for tolerant correlation with
/// the OXC-produced [`Index`] paths, whose module segment is derived from the
/// file basename.
///
/// A [`QueryCache`] is shared across all files so cross-file `Lazy(DefId)`
/// references resolve coherently (Mode 2).
fn recover_types(
	program: &MergedProgram,
) -> (
	rustc_hash::FxHashMap<(String, String), RecoveredType>,
	rustc_hash::FxHashMap<String, usize>,
) {
	use rustc_hash::FxHashMap;

	let query_cache =
		QueryCache::new(&program.type_interner).with_definition_store(&program.definition_store);

	let interner: &dyn tsz_solver::construction::TypeDatabase = &program.type_interner;

	let mut by_key: FxHashMap<(String, String), RecoveredType> = FxHashMap::default();
	// Count of distinct owners per bare name, for uniqueness gating.
	let mut name_counts: FxHashMap<String, usize> = FxHashMap::default();

	for (file_idx, file) in program.files.iter().enumerate() {
		// Per-file binder (borrowed by the CheckerState for its whole lifetime).
		let binder: BinderState = create_binder_from_bound_file(file, program, file_idx);

		let base = file
			.file_name
			.rsplit(['/', '\\'])
			.next()
			.unwrap_or(file.file_name.as_str());
		let stem = file_stem(base);

		let mut checker = CheckerState::new(
			&file.arena,
			&binder,
			&query_cache,
			file.file_name.clone(),
			Default::default(),
		);
		checker.check_source_file(file.source_file);

		// Snapshot the top-level names first so the immutable borrow of
		// `binder.file_locals` doesn't overlap the `&mut checker` queries.
		let names: Vec<(String, tsz_binder::SymbolId)> = binder
			.file_locals
			.iter()
			.map(|(name, id)| (name.clone(), *id))
			.collect();

		for (name, sym_id) in names {
			// Prefer symbol-based queries; for variable-like bindings the LSP
			// hover pattern queries the declaration node instead. We keep it
			// simple and robust: query the symbol type, which resolves to the
			// value type for variables and the arrow type for functions.
			let type_id = checker.get_type_of_symbol(sym_id);

			// Map the symbol's own type + (for callables) its return type.
			// `format_type` (an `&self` method) renders the printed spelling for
			// the re-parse fallback inside the mapper.
			let own = tsz_types::type_id_to_ir(interner, type_id, &|id| checker.format_type(id));
			let ret = tsz_types::return_type_of(interner, type_id)
				.map(|rid| tsz_types::type_id_to_ir(interner, rid, &|id| checker.format_type(id)));

			by_key.insert((stem.clone(), name.clone()), RecoveredType { own, ret });
			*name_counts.entry(name).or_insert(0) += 1;
		}
	}

	(by_key, name_counts)
}

/// Look up a recovered type for an entry by its leaf name, using the module
/// segment of its path as the file stem when available, falling back to a
/// globally-unique bare-name match.
fn recovered_for<'a>(
	by_key: &'a rustc_hash::FxHashMap<(String, String), RecoveredType>,
	name_counts: &rustc_hash::FxHashMap<String, usize>,
	stem_hint: Option<&str>,
	leaf: &str,
) -> Option<&'a RecoveredType> {
	if let Some(stem) = stem_hint {
		if let Some(r) = by_key.get(&(stem.to_string(), leaf.to_string())) {
			return Some(r);
		}
	}
	// Bare-name fallback: only if exactly one owner has this name (avoids
	// cross-module collisions silently mislabeling a type).
	if name_counts.get(leaf).copied() == Some(1) {
		return by_key
			.iter()
			.find(|((_, n), _)| n == leaf)
			.map(|(_, r)| r);
	}
	None
}

/// Derive the module-stem hint for an entry from its `NudoxPath`. The OXC
/// linker names modules `stem::leaf` (dotted `::`-joined), so the segment just
/// before the leaf is (in the common flat case) the file stem.
fn stem_hint_for(entry: &Entry) -> Option<String> {
	use ir::entry::NudoxPath;
	let path = match entry.path() {
		NudoxPath::Local(p) => p.to_string_lossy().into_owned(),
		NudoxPath::External { path, .. } => path.to_string_lossy().into_owned(),
	};
	let segs: Vec<&str> = path.split("::").filter(|s| !s.is_empty()).collect();
	if segs.len() >= 2 {
		Some(segs[segs.len() - 2].to_string())
	} else {
		None
	}
}

/// Does an `output_parameters` slot lack a concrete inferred type — i.e. it is
/// absent, or its single output is `Any`/unit — such that the checker's answer
/// would enrich it?
fn outputs_need_enrichment(outputs: &Option<Vec<Parameter>>) -> bool {
	match outputs {
		None => true,
		Some(params) => params.iter().all(|p| match p {
			Parameter::Literal(l) => matches!(
				l.r#type,
				None | Some(Type::Any) | Some(Type::Infer)
			),
			_ => false,
		}),
	}
}

/// Walk the OXC-produced [`Index`] and splice in checker-recovered types where
/// the syntactic pass left a gap.
fn enrich_index(
	index: &mut Index,
	by_key: &rustc_hash::FxHashMap<(String, String), RecoveredType>,
	name_counts: &rustc_hash::FxHashMap<String, usize>,
) {
	for entry in index.entries_by_path.values_mut() {
		let stem = stem_hint_for(entry);
		let leaf = entry.name().to_string();
		let Some(recovered) = recovered_for(by_key, name_counts, stem.as_deref(), &leaf) else {
			continue;
		};

		match entry {
			// Functions: fill an absent/opaque return type from the checker's
			// inferred return (Mode 1). Only touch it when the syntactic pass
			// had nothing concrete — never clobber a real source annotation.
			Entry::Function(sym) => {
				if outputs_need_enrichment(&sym.inner.output_parameters) {
					if let Some(ret) = &recovered.ret {
						sym.inner.output_parameters =
							output_parameters_from_type(ret.clone());
					}
				}
			}

			// Type aliases: replace an opaque/`Any` alias body with the
			// checker's resolved type (Mode 2 — cross-module resolution).
			// Generics on TypeAliasBody are preserved (only `target` is swapped).
			Entry::TypeAlias(sym) => {
				if matches!(sym.inner.target, Type::Any | Type::Infer) {
					sym.inner.target = recovered.own.clone();
				}
			}

			// Constants / Variables: fill TypedBinding.ty when the syntactic
			// pass left it empty/opaque and the checker recovered a type.
			Entry::Constant(sym) | Entry::Variable(sym) => {
				let needs = match &sym.inner.ty {
					None => true,
					Some(Type::Any) | Some(Type::Infer) => true,
					_ => false,
				};
				if needs {
					sym.inner.ty = Some(recovered.own.clone());
				}
			}

			_ => {}
		}
	}
}

/// Run the in-process tsz check oracle over the package at `root`: extract
/// structure via the OXC pipeline, then enrich it with checker-recovered types.
///
/// On success returns the enriched [`Index`]. On ANY failure returns
/// `Err(reason)` — the caller must fall back to the syntactic pass.
pub fn normalize(root: &Path, name: &str) -> Result<Index, String> {
	if already_has_declarations(root) {
		return Err("package already ships .d.ts; syntactic pass suffices".to_string());
	}

	// The checker recurses deeply — install the large-stack Rayon pool FIRST.
	// Idempotent; safe to call every run.
	ensure_rayon_global_pool();

	let target = root
		.canonicalize()
		.map_err(|e| format!("canonicalize package root: {e}"))?;

	// (a) Structure + syntactic types from the OXC Tier-A pipeline.
	let mut index = super::super::oxc::generate_ir(&target, name)
		.map_err(|e| format!("oxc structure pass: {e}"))?;

	// (b) Discover + read the same source files for the checker.
	let src_files = discover_ts_files(&target)?;
	if src_files.is_empty() {
		return Err("no TypeScript source files found".to_string());
	}
	let mut inputs: Vec<(String, String)> = Vec::with_capacity(src_files.len());
	for path in &src_files {
		let text = std::fs::read_to_string(path)
			.map_err(|e| format!("read {}: {e}", path.display()))?;
		inputs.push((path.to_string_lossy().into_owned(), text));
	}

	// (c) Parse + bind + merge, then check every file and recover types.
	//     Guarded so any checker panic degrades to Tier-A rather than aborting.
	let recovered = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
		let program: MergedProgram = parallel::compile_files(inputs);
		recover_types(&program)
	}))
	.map_err(|_| "tsz checker panicked; falling back to syntactic".to_string())?;

	let (by_key, name_counts) = recovered;
	if by_key.is_empty() {
		return Err("tsz recovered no types (unsupported syntax or empty surface)".to_string());
	}

	// (d) Splice recovered types into the syntactic IR.
	enrich_index(&mut index, &by_key, &name_counts);

	Ok(index)
}
