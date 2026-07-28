//! Crate traversal: BFS over `Module` tree → `item::lower` for each `ModuleDef`.
//!
//! # Design
//!
//! One pass over the RA module tree in declaration order.  For each crate root
//! module we declare an `IrModule` entry, then recurse into child modules.
//! `Impl` blocks are collected during the walk and lowered at the end so that
//! the types they refer to are already declared (though `Lowering::finish`
//! handles forward refs, this ordering is still cleaner).
//!
//! # Panic safety
//!
//! RA salsa queries can panic with `Cancelled`.  We catch all panics on a
//! per-item basis; `Cancelled` panics are re-raised so the top-level driver can
//! retry the salsa database.  Other panics are logged and the item is skipped.

use std::collections::VecDeque;

use ra_ap_hir::{Impl, Module, ModuleDef};

use nudox_ir::lower::Lowering;

use crate::{RaId, error::RustProducerError};
use super::{ctx::LowerCtx, item};

// ── Panic helpers ─────────────────────────────────────────────────────────────

/// Try to call `f`; if it panics with a non-`Cancelled` reason, log and return
/// the fallback.  If it panics with `Cancelled`, re-raise.
fn catch_non_cancelled<F, T>(f: F, fallback: T) -> T
where
    F: FnOnce() -> T + std::panic::UnwindSafe,
{
    match std::panic::catch_unwind(f) {
        Ok(v) => v,
        Err(p) => {
            // If the payload is the well-known RA cancellation string, re-raise.
            let is_cancelled = p
                .downcast_ref::<String>()
                .map(|s| s.contains("Cancelled"))
                .or_else(|| {
                    p.downcast_ref::<&str>()
                        .map(|s| s.contains("Cancelled"))
                })
                .unwrap_or(false);
            if is_cancelled {
                std::panic::resume_unwind(p);
            }
            tracing::warn!("item lowering panic (non-fatal, skipping item)");
            fallback
        }
    }
}

// ── Entry point ───────────────────────────────────────────────────────────────

/// Lower every documented item in `ctx.krate` into `out`.
pub(crate) fn lower_crate(
    ctx: &mut LowerCtx<'_>,
    out: &mut Lowering<RaId>,
) -> Result<(), RustProducerError> {
    // Collect all modules in BFS order.
    let root = ctx.krate.root_module(ctx.db);
    let mut modules: Vec<Module> = Vec::new();
    let mut queue: VecDeque<Module> = VecDeque::new();
    queue.push_back(root);
    while let Some(m) = queue.pop_front() {
        modules.push(m);
        for child in m.children(ctx.db) {
            queue.push_back(child);
        }
    }

    // Impls collected during the walk; lowered in a second pass.
    let mut impls: Vec<(Impl, Option<RaId>)> = Vec::new();

    for module in &modules {
        let _mod_def = ModuleDef::Module(*module);
        let mod_parent: Option<RaId> = module
            .parent(ctx.db)
            .and_then(|p| ctx.ra_id(ModuleDef::Module(p)));

        // Declare the module itself.
        let _ = catch_non_cancelled(
            // SAFETY: all captures are 'static or live past this call.
            std::panic::AssertUnwindSafe(|| {
                item::lower_module(ctx, *module, mod_parent.clone(), out)
            }),
            Ok(()),
        );

        // Lower each item defined in this module.
        for child_def in module.declarations(ctx.db) {
            // Skip enum variants at module level (lowered inside enum).
            if matches!(child_def, ModuleDef::EnumVariant(_)) {
                continue;
            }

            // Always declare every item — including private ones — so that
            // public signatures that reference private types do not produce
            // dangling `refer` slots.  Visibility is recorded in `Symbol.visibility`
            // so downstream consumers can apply their own filtering policy.
            // Skipping private *declarations* while allowing public *references*
            // is precisely the bug class we are fixing (Group C).
            //
            // The `document_private` flag now only governs whether the re-export
            // walk in `lower_module` emits non-public re-exports.  Declaration
            // of items defined in this module is unconditional.

            let parent_id = ctx.ra_id(ModuleDef::Module(*module));
            let _ = catch_non_cancelled(
                std::panic::AssertUnwindSafe(|| {
                    item::lower(ctx, child_def, parent_id.clone(), out)
                }),
                Ok(()),
            );
        }

        // Collect impl blocks defined in this module.
        for imp in module.impl_defs(ctx.db) {
            let parent_id = ctx.ra_id(ModuleDef::Module(*module));
            impls.push((imp, parent_id));
        }
    }

    // Second pass: lower all impl blocks.
    // By this point all struct/enum/trait entries are declared.
    for (imp, parent_id) in impls {
        let _ = catch_non_cancelled(
            std::panic::AssertUnwindSafe(|| {
                item::lower_impl(ctx, imp, parent_id, out)
            }),
            Ok(()),
        );
    }

    Ok(())
}
