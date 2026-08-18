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

use std::{collections::VecDeque, time::Instant};

use ra_ap_hir::{Impl, Module, ModuleDef};

use nudox_ir::lower::Lowering;

use super::{ctx::LowerCtx, item};
use crate::rust::RaId;

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
                .or_else(|| p.downcast_ref::<&str>().map(|s| s.contains("Cancelled")))
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
pub(crate) fn lower_crate(ctx: &mut LowerCtx<'_>, out: &mut Lowering<RaId>) {
    // ── Same-display-name guard ───────────────────────────────────────────────
    //
    // Cargo lets a `[[bench]]`, `[[test]]`, or `[[example]]` target share its
    // package's crate name — `bytes-1.11.0` names its `benches/bytes.rs`
    // target `"bytes"`, identical to its `[lib]` target. rust-analyzer's crate
    // graph therefore contains two distinct `Crate`s that both answer `"bytes"`
    // to `display_name`. The caller (`ra::mod::lower_all_packages_into`)
    // selects crates to walk by matching that display name against the
    // requested package, so it walks *both* — and both declare a root module
    // at the same id (`"bytes"`), and both attempt a `Reexport` for `Bytes` at
    // `"bytes::Bytes"` (the library via its genuine `pub use crate::bytes::Bytes`,
    // the bench crate because its plain `use bytes::Bytes;` resolves to a
    // public item and gets swept up by `lower_module`'s scope scan). Every
    // `out.declare`/`declare_ref` call in `item.rs` *is* already routed through
    // `id_of`/`check_unique`, so this is not a missing call to that authority —
    // it is a second, entirely distinct `Crate` being fed through the walk at
    // all, upstream of any single id computation.
    //
    // The general, order-independent signal for "this crate is not the
    // canonical crate for its own name" is structural rather than name-based:
    // Rust forbids a crate from depending on itself, so a crate that has a
    // *dependency* sharing its own display name cannot be the library that
    // name denotes — it can only be a bench/test/example target that Cargo
    // auto-injected `extern crate <lib>` into. Skip walking such a crate
    // entirely (rather than letting individual items collide downstream): a
    // bench/test file contributes no API surface of its own, it only calls
    // into the library, so there is nothing here to lose.
    if let Some(own_name) = ctx.krate.display_name(ctx.db).map(|n| n.to_string())
        && ctx.krate.dependencies(ctx.db).iter().any(|dep| {
            dep.krate
                .display_name(ctx.db)
                .map(|n| n.to_string())
                .as_deref()
                == Some(own_name.as_str())
        })
    {
        return;
    }

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

    let modules_started = Instant::now();
    for module in &modules {
        let mod_parent: Option<RaId> = module
            .parent(ctx.db)
            .and_then(|p| ctx.ra_id(ModuleDef::Module(p)));

        // Declare the module itself.
        catch_non_cancelled(
            // SAFETY: all captures are 'static or live past this call.
            std::panic::AssertUnwindSafe(|| {
                item::lower_module(ctx, *module, mod_parent.clone(), out);
            }),
            (),
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
            catch_non_cancelled(
                std::panic::AssertUnwindSafe(|| {
                    item::lower(ctx, child_def, parent_id.clone(), out);
                }),
                (),
            );
        }
    }
    super::phase_complete(
        "lower_modules",
        modules_started.elapsed(),
        format!("modules={}", modules.len()),
    );

    // Second pass: lower all impl blocks (including macro-generated ones inside
    // `const _: () = { … }` blocks that `Module::impl_defs` misses).
    // `Impl::all_in_crate` recurses into unnamed-const block def-maps so that
    // impls generated by macros like `pin_project!` or `impl_service!` are
    // included.  For each impl we walk up to the nearest file-level (non-block)
    // module to obtain the correct parent id.
    let all_impls = Impl::all_in_crate(ctx.db, ctx.krate);
    let impls: Vec<(Impl, Option<RaId>)> = all_impls
        .into_iter()
        .filter(|imp| {
            // Skip impls that live *inside* an unnamed `const _: () = { … }`
            // block. Those are macro hygiene, not API surface.
            //
            // `pin_project_lite` emits a hygiene block per struct containing
            // `Projection`, `ProjectionRef`, `__Origin` and `MustNotImplDrop`,
            // plus impls over them. Including the impls without the types they
            // name produced ~36 dangling references and, because a block module
            // has no canonical path, every such impl keyed as `<anon>::…` and
            // collided with its siblings — 17 of 19 real-crate tests failed.
            //
            // Declaring the block-local types instead would "fix" the dangle by
            // publishing `__Origin` and `MustNotImplDrop` as though they were
            // axum's API. They are not; rustdoc does not show them either. An
            // impl that cannot be named cannot be navigated to, so the correct
            // move is to omit it.
            //
            // Macro-generated impls written at *module* level — the ones this
            // pass exists to recover — are unaffected: their enclosing module
            // is a real one, so they compare equal here.
            let module = imp.module(ctx.db);
            module == module.nearest_non_block_module(ctx.db)
        })
        .map(|imp| {
            let file_module = imp.module(ctx.db).nearest_non_block_module(ctx.db);
            let parent_id = ctx.ra_id(ModuleDef::Module(file_module));
            (imp, parent_id)
        })
        .collect();

    let impls_count = impls.len();
    let impls_started = Instant::now();
    for (imp, parent_id) in impls {
        catch_non_cancelled(
            std::panic::AssertUnwindSafe(|| {
                item::lower_impl(ctx, imp, parent_id, out);
            }),
            (),
        );
    }
    super::phase_complete(
        "lower_impls",
        impls_started.elapsed(),
        format!("impls={impls_count}"),
    );

}
