//! Crate / module traversal → `Entry` stream (§3.4).

use std::panic::{self, AssertUnwindSafe};

use ir::kind::Entry;
use ra_ap_base_db::salsa::Cancelled;
use ra_ap_hir::{Module, ModuleDef};
use rustc_hash::FxHashMap as HashMap;
use tracing::debug;

use super::super::error::Package;
use super::{ctx::LowerCtx, item, source};

/// Walk every module of `ctx.krate`, lowering declarations and impls.
///
/// Per-item: non-`Cancelled` panics drop the item (lenient, matches rustdoc).
/// `Cancelled` aborts the whole crate as retryable.
///
/// After the DFS, [`item::attach_record_impls`] fills `Record.methods` /
/// `implemented_protocols` from the impl index built via
/// [`LowerCtx::build_impl_index`] / `item::lower_impl`.
pub(crate) fn lower_crate(
	ctx: &mut LowerCtx<'_>,
) -> Result<(Vec<Entry>, HashMap<String, String>), Package> {
	let mut out = Vec::new();
	let mut source_map = HashMap::default();
	let mut stack: Vec<Module> = vec![ctx.krate.root_module(ctx.db)];

	while let Some(module) = stack.pop() {
		stack.extend(module.children(ctx.db));

		if let Some(entry) = catch_opt(|| item::module_entry(ctx, module))? {
			out.push(entry);
		}

		for def in module.declarations(ctx.db) {
			if !ctx.include(def) {
				continue;
			}

			// Source map for free functions (best-effort; `source::` may be stubbed).
			if let ModuleDef::Function(f) = def
				&& let Some((key, text)) = catch_opt(|| source::fn_source(ctx, f))?
			{
				source_map.insert(key.to_string(), text);
			}

			if let Some(entry) = catch_opt(|| item::lower(ctx, def))? {
				out.push(entry);
			}
		}

		for imp in module.impl_defs(ctx.db) {
			// TraitImpl entries + provisional impl buckets for the second pass.
			catch_unit(|| item::lower_impl(ctx, &mut out, imp))?;
		}
	}

	// Rebuild the impl index once (dedupes any provisional buckets from
	// `lower_impl`, and picks up all_in_crate coverage including derives).
	catch_unit(|| ctx.build_impl_index())?;

	// Second pass: attach methods + protocols onto RecordType entries.
	catch_unit(|| item::attach_record_impls(ctx, &mut out))?;

	Ok((out, source_map))
}

/// Catch panics from an `Option`-returning lowerer. Cancelled aborts; other
/// panics → `Ok(None)` (lenient). Inner `None` also yields `Ok(None)`.
fn catch_opt<T>(f: impl FnOnce() -> Option<T>) -> Result<Option<T>, Package> {
	match panic::catch_unwind(AssertUnwindSafe(f)) {
		Ok(v) => Ok(v),
		Err(payload) => {
			if payload.downcast_ref::<Cancelled>().is_some() {
				return Err(Package::Cancelled);
			}
			debug!("item lowering panicked; dropping item (lenient)");
			Ok(None)
		}
	}
}

/// Catch panics from a unit lowerer (impls, second-pass attach).
fn catch_unit(f: impl FnOnce()) -> Result<(), Package> {
	match panic::catch_unwind(AssertUnwindSafe(f)) {
		Ok(()) => Ok(()),
		Err(payload) => {
			if payload.downcast_ref::<Cancelled>().is_some() {
				return Err(Package::Cancelled);
			}
			debug!("item lowering panicked; dropping item (lenient)");
			Ok(())
		}
	}
}
