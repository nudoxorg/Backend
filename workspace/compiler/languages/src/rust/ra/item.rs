//! `ModuleDef` → `Lowering<RaId>` declarations (one pass, order-independent).
//!
//! # Design
//!
//! The old `item.rs` built intermediate `Vec<Entry>` + deferred lists and
//! returned them.  The new version emits directly into `Lowering<RaId>` using
//! `declare`/`refer`.  Because `refer` resolves forward references at `finish`
//! time, we never need to topologically sort declarations.
//!
//! Every item emitted here gets a `parent` ID in the Lowering graph.  The
//! parent for module-level items is the module's `RaId`.  For trait assoc items
//! the parent is the trait's `RaId`.  For impl methods the parent is the impl's
//! `RaId`.
//!
//! # ID scheme
//!
//! See `ctx.rs` for the authoritative description.  In brief:
//!
//! - Nested items (impl/trait members, fields, variants, params) always derive
//!   their id from their parent via `LowerCtx::child_id(parent, name)`.  The
//!   parent impl id encodes the self type and trait, so even items with the
//!   same name in different impls are distinct.
//!
//! - Module-level type-namespace items (struct, enum, union, trait, mod, type
//!   alias) use bare names: `child_id(module_id, name)` = `"crate::name"`.
//!
//! - Module-level value-namespace items (fn, const, static) append `!v`:
//!   `child_id(module_id, "name!v")`.  This prevents collision with a
//!   same-named module or type (e.g. `mod serve` vs `fn serve` in axum).
//!
//! - Module-level macro items append `!m`: `child_id(module_id, "name!m")`.
//!
//! The `!` delimiter cannot appear in any Rust identifier or path, ruling out
//! false collisions.
//!
//! # Param IDs
//!
//! Parameters are separate entries (kind = `Param`).  Their `RaId` is
//! `"{fn_id}::param::{name}"` for inputs and `"{fn_id}::param::return"` for
//! the output.  These are synthetic but unique per function, because `fn_id`
//! itself is now globally unique.
//!
//! # IR gaps
//!
//! - `Macro` items → emitted as `Module` (the new IR has no Macro kind).
//! - `BuiltinType` → emitted as `Alias` with `target = None`.
//! - Union fields → emitted as `Field` entries with `FieldKey::Named`.
//! - Enum variant discriminants → stored as `Variant.discr`.
//! - Intra-doc links → `Symbol.doc_links`.
//! - Re-exports (pub use) → `Reexport` entries.

use ra_ap_hir::{
    Adt, AsAssocItem, AssocItem, AssocItemContainer, FieldSource, Function as HirFunction,
    HasSource, HasVisibility, HirDisplay, Impl, LangItem, Module, ModuleDef, PathResolution,
    ScopeDef, Struct, StructKind, Trait, attach_db,
};
use ra_ap_syntax::{
    AstNode, SyntaxKind,
    ast::{self as syn_ast, HasGenericParams, HasTypeBounds},
};

use nudox_ir::{
    entry::{SourceLocation, Unlocated},
    index::Ref,
    kinds::{
        Alias, AutoFact, Const, Enum, Field, FieldAttribute, FieldKey, Function, Impl as IrImpl,
        ImplFlags, Module as IrModule, Param, Record, RecordForm, Reexport, Static,
        Trait as IrTrait, TraitFlags, TriState, Variant, VariantForm,
    },
    lower::Lowering,
    vocab::{ReferenceKind, RelSpan},
};

use super::{
    ctx::{LowerCtx, PendingOcc, PendingTarget},
    docs, function, generics, source, ty,
};
use crate::rust::RaId;
use crate::rust::error::Error;

// ── Namespace tag helper ──────────────────────────────────────────────────────

/// Return the namespace suffix for `def` when it lives at module level.
///
/// Rust has three namespaces — type, value, and macro — and a module can hold
/// items with the same name in different namespaces simultaneously (e.g. `mod
/// serve` in the type namespace and `fn serve` in the value namespace).  To
/// prevent id collisions we append a short, `!`-delimited tag to value- and
/// macro-namespace items.  Type-namespace items (modules, structs, enums,
/// traits, type aliases) remain bare.
///
/// The same rule must be applied **consistently** everywhere an id is derived
/// from a `ModuleDef` at module level:
/// - `lower()` (declaration ids)
/// - `lower_module()` re-export ids    ← previously missing, now uses this fn
///
/// Using this single helper ensures the two sites can never drift.
fn ns_tag(def: ModuleDef) -> &'static str {
    match def {
        // Value namespace.
        ModuleDef::Function(_) | ModuleDef::Const(_) | ModuleDef::Static(_) => "!v",
        // Macro namespace.
        ModuleDef::Macro(_) => "!m",
        // Type namespace (module, struct, enum, union, trait, type alias,
        // builtin type, enum variant).  Bare — no tag.
        _ => "",
    }
}

// ── Single id authority ───────────────────────────────────────────────────────

/// The single source of truth for "what is this def's `RaId`".
///
/// Container-aware and namespace-tagged, so a *reference* key and a
/// *declaration* key for the same item are equal by construction.
///
/// | Item kind                       | Returned id                          |
/// |---------------------------------|--------------------------------------|
/// | Assoc item in a trait           | `{trait_id}::{name}`                 |
/// | Assoc item in an impl           | `{impl_id}::{name}` (no ns tag)      |
/// | Module-level fn / const / static| `{module_canonical}{ns_tag!v}`       |
/// | Module-level macro              | `{module_canonical}{ns_tag!m}`       |
/// | All other module-level items    | `{module_canonical}` (bare)          |
///
/// This is the **authoritative** id constructor.  It must be called at both
/// declaration sites and reference sites so the two can never diverge.
///
/// The function is `pub(super)` so that `ty.rs` and `generics.rs` can import it
/// via `super::item::id_of`.
pub(super) fn id_of(ctx: &mut LowerCtx<'_>, def: ModuleDef) -> Option<RaId> {
    // ── Assoc items: container-aware id ──────────────────────────────────────
    let assoc: Option<AssocItem> = match def {
        ModuleDef::Function(f) => f.as_assoc_item(ctx.db),
        ModuleDef::TypeAlias(ta) => ta.as_assoc_item(ctx.db),
        ModuleDef::Const(c) => c.as_assoc_item(ctx.db),
        _ => None,
    };

    if let Some(ai) = assoc {
        let name = def.name(ctx.db)?.as_str().to_owned();
        return match ai.container(ctx.db) {
            AssocItemContainer::Trait(t) => {
                let trait_id = ctx.ra_id(ModuleDef::Trait(t))?;
                Some(ctx.child_id(&trait_id, &name))
            }
            AssocItemContainer::Impl(imp) => {
                let iid = impl_id(ctx, imp);
                Some(ctx.child_id(&iid, &name))
            }
        };
    }

    // ── Module-level items: canonical path + ns_tag ───────────────────────────
    // `ctx.canonical(def)` returns the untagged module path (e.g. `axum::serve`
    // for both the module and the function).  We append the namespace tag so the
    // reference key matches the declaration key.
    let canon = ctx.canonical(def)?;
    let tag = ns_tag(def);
    if tag.is_empty() {
        Some(canon)
    } else {
        Some(RaId::from(format!("{canon}{tag}").as_str()))
    }
}

// ── RefFor helper closure ─────────────────────────────────────────────────────

/// Build a `ref_for` closure that maps a canonical path key to a
/// `Ref<Param>`-compatible `RawRef`.
///
/// **Local keys** (those belonging to `ctx.krate`) are resolved via
/// `Lowering::refer` — a forward-reference that `finish` resolves once all
/// declarations are in.
///
/// **Foreign keys** (belonging to a different crate) are resolved via
/// `Lowering::refer_import`, producing a cross-package `RawRef`.  The
/// `PackageId` is synthesised from the first path segment of the key (the
/// crate name), using `PackageId::path`.  `PackageId::path` is the only
/// constructor available; we pass the crate name as a synthetic "path" string.
/// This is deterministic (crate names are unique in a Cargo lockfile) and
/// stable across runs.
///
/// `Ref::into_raw` is confirmed public in `nudox_ir::index` (line 268 of
/// index.rs: `pub fn into_raw(self) -> RawRef`).  The cast is sound.
macro_rules! make_ref_for {
    ($ctx:ident, $lowering:ident) => {{
        // Compute the local crate name and reborrow the lowering sink so that
        // a `move` closure can own both without consuming the original `$lowering`
        // variable in the caller's scope.
        //
        // Reborrow pattern: `let r = &mut *$lowering` creates a shorter-lived
        // &mut that is distinct from `$lowering`.  When the `move` closure captures
        // `r`, it borrows `$lowering` for the closure's lifetime.  Once the closure
        // is dropped (at end of each call site's scope), `$lowering` is usable again
        // — e.g. for `out.declare(...)` calls that follow.
        let local_crate: String = super::ctx::crate_name($ctx.db, $ctx.krate);
        // Snapshot the foreign-lineage map: the closure cannot borrow `$ctx`
        // (the caller keeps using it) but needs the real Cargo.toml names, not
        // the underscored rustc ones a path string carries.
        let foreign_lineages: rustc_hash::FxHashMap<String, nudox_ir::change::PackageLineageId> =
            $ctx.foreign_lineages.clone();
        let _lowering_reborrow = &mut *$lowering;
        move |key: &crate::rust::ra::ctx::PathKey| -> Option<nudox_ir::index::RawRef> {
            // Decide whether `key` belongs to the local crate.
            // A key is local iff it starts with `{local_crate}` followed by
            // either end-of-string (the crate root) or `::`.
            let n = local_crate.len();
            let is_local = key.starts_with(local_crate.as_str())
                && (key.len() == n || key.as_bytes().get(n) == Some(&b':'));

            if is_local {
                let typed: Ref<IrModule> = _lowering_reborrow.refer(key.clone());
                Some(typed.into_raw())
            } else {
                // Foreign package: a self-describing cross-package reference.
                // It carries the target's real lineage (when the crate graph
                // knows it), its canonical path, and the leaf name to render —
                // so `impl ? for Memchr` becomes `impl Clone for Memchr` with no
                // corpus, and the reference can be linked later.
                let typed: Ref<IrModule> = _lowering_reborrow.refer_import(
                    crate::rust::ra::ctx::foreign_key_from(&foreign_lineages, key.as_str()),
                );
                Some(typed.into_raw())
            }
        }
    }};
}

// ── Module entry ──────────────────────────────────────────────────────────────

/// Declare `module` as an `IrModule` entry in `out`.
///
/// Child module / member paths are not listed here — the tree is derived from
/// parent pointers by `Lowering::finish`.
///
/// After declaring the module itself, this function walks `module.scope` and
/// emits `Reexport` entries for every `pub use` whose scope spelling differs
/// from the defining item's canonical path.  A re-export entry uses
/// `Lowering::declare_ref`, pointing at the canonical entry via `refer`.
pub(crate) fn lower_module(
    ctx: &mut LowerCtx<'_>,
    module: Module,
    parent: Option<RaId>,
    out: &mut Lowering<RaId>,
) -> Result<(), Error> {
    let def = ModuleDef::Module(module);
    let Some(mod_id) = ctx.ra_id(def) else {
        return Ok(());
    };
    let parts = ctx
        .symbol_parts(def)
        .unwrap_or_else(|| super::ctx::SymbolParts {
            name: "<anon>".to_owned(),
            visibility: nudox_ir::entry::Visibility::Private,
            documentation: String::new(),
            aliases: Vec::new(),
            deprecation: None,
            doc_links: Vec::new(),
            cfg: None,
            attrs: Box::new([]),
        });
    let location = source::def_location(ctx, def);
    let sym = parts.into_symbol(&location);
    ctx.check_unique(&mod_id);
    out.declare_at(mod_id.clone(), parent, sym, IrModule, location);

    // ── Re-exports (pub use) ──────────────────────────────────────────────────
    // Walk this module's scope.  Items whose *scope path* (module + name)
    // differs from their *canonical path* are re-exports.  Each such item gets
    // a `Reexport` entry (backed by `declare_ref`) pointing at the original.
    //
    // A re-export's RaId is `<module_id>::<scope_name><ns_tag>` where the
    // namespace tag mirrors exactly what `lower()` applies when declaring the
    // item: value-namespace items (fn / const / static) get `!v`, macro items
    // get `!m`, type-namespace items remain bare.  Previously the tag was
    // absent here, causing collisions such as:
    //
    //   `axum::serve`    — both the *module* (type ns, bare) and the
    //                      re-exported *function* (value ns) landed on the
    //                      same id `axum::serve`.
    //
    //   `axum::routing::method_filter::Debug` — the *trait* `Debug` (type ns)
    //                      and the *derive macro* `Debug` (macro ns) both
    //                      landed on the same id.
    //
    // The `ns_tag()` helper (defined at the top of this module) is shared with
    // `lower()` so the two sites cannot drift.
    //
    // NOTE: The `is_reexport` check still compares *untagged* scope paths
    // against the canonical key (which is also untagged — `ctx.canonical()`
    // never includes `!v`/`!m`).  This is intentional: tagging the scope path
    // would produce false positives (every function in its own module would
    // look like a re-export because its tagged scope path ≠ untagged canon key).
    let module_segs: Option<Vec<String>> = ctx.path_segments(ModuleDef::Module(module));

    // ── Which names in scope are actually re-exported ─────────────────────────
    //
    // `Module::scope` returns every name *bindable inside* the module, which
    // includes the module's own private `use` statements. Those are not
    // re-exports — they are the module body's implementation detail, no more
    // part of the package's API than a local variable is.
    //
    // The filter that used to live here asked the wrong object. It took the
    // visibility of the *imported item* (`child.visibility`), and an item
    // imported from another crate is essentially always `pub` there, so every
    // private import of a public foreign item was recorded as a re-export of
    // this package. That is not a rounding error: `memchr/src/vector.rs:294`
    // holds `use core::arch::aarch64::*;` inside a private module inside a
    // private module, and it alone produced 10 014 `Reexport` entries naming
    // NEON/SVE intrinsics — 88% of memchr's entire lowered IR was `core`'s API
    // wearing memchr's name.
    //
    // The right question is about the *binding*, not the item: is this name
    // visible outside the module that declares it? `Module::scope`'s
    // `visible_from` parameter answers precisely that, because it filters on the
    // scope entry's visibility, which for an import is the visibility of the
    // `use` itself.
    //
    //   * nested module     → anchor is its parent. `pub use` and `pub(crate)
    //                         use` survive; a plain `use` does not.
    //   * crate root module → "outside this module" *is* "outside the crate",
    //                         so the anchor must be foreign
    //                         (`LowerCtx::foreign_anchor`, which is an exact
    //                         `Visibility::Public` test — see its doc comment).
    //                         This is what stops a bare `use std::…;` in
    //                         `lib.rs` from becoming a re-export.
    //
    // `document_private` keeps its documented meaning — whether *non-public*
    // re-exports are emitted — by choosing between those two anchors rather
    // than by disabling the check. With it off, the foreign anchor is used
    // everywhere, so only genuinely public re-exports are recorded.
    let anchor = if ctx.document_private {
        module.parent(ctx.db).or(ctx.foreign_anchor)
    } else {
        ctx.foreign_anchor
    };

    // Collect (scope_name, child_def) pairs for re-exports, to avoid holding
    // an immutable borrow on `ctx` while mutating `out`.
    let scope_items: Vec<(smol_str::SmolStr, ModuleDef)> = module
        .scope(ctx.db, anchor)
        .into_iter()
        .filter_map(|(name, scope_def)| {
            let ScopeDef::ModuleDef(child) = scope_def else {
                return None;
            };
            Some((smol_str::SmolStr::from(name.as_str()), child))
        })
        .collect();

    for (scope_name, child) in scope_items {
        // canonical path of the defining item (always untagged)
        let Some(canon_key) = ctx.canonical(child) else {
            continue;
        };

        // scope path of this entry = module_segs + scope_name (untagged)
        // Used only for the "is this actually a re-export?" test.
        let is_reexport = if let Some(ref segs) = module_segs {
            let mut scope_segs = segs.clone();
            scope_segs.push(scope_name.to_string());
            let scope_path: smol_str::SmolStr = scope_segs.join("::").into();
            scope_path != canon_key
        } else {
            false
        };

        if !is_reexport {
            continue;
        }

        // RaId for the re-export: "<module_id>::<scope_name><ns_tag>"
        // The namespace tag disambiguates items from different namespaces that
        // share a scope name (e.g. the trait `Debug` vs the derive macro `Debug`).
        let tag = ns_tag(child);
        let reexport_id = RaId::from(format!("{mod_id}::{scope_name}{tag}").as_str());

        // The `target_ref` must point to the *declaration* id of the canonical
        // item, which includes the namespace tag.  `canon_key` (from
        // `ctx.canonical()`) is always untagged — it reflects the defining-path
        // hierarchy without any namespace suffix.  The actual declaration id for
        // value-namespace items (fn / const / static) uses `!v`, and for macros
        // `!m` (see `lower()`).  We therefore construct the tagged key here.
        //
        // For type-namespace items `tag = ""`, so `tagged_key == canon_key`.
        let tagged_key: smol_str::SmolStr =
            smol_str::SmolStr::from(format!("{canon_key}{tag}").as_str());

        // Obtain a Ref pointing to the canonical entry.  For foreign-crate items
        // that are re-exported, we must use `refer_import` (not `refer`) to avoid
        // creating an undeclared local slot.  A key is local iff it starts with the
        // current crate's name followed by end-of-string or `::`.
        let local_crate_name = super::ctx::crate_name(ctx.db, ctx.krate);
        let target_ref: Ref<Reexport> = {
            let n = local_crate_name.len();
            let is_local_key = tagged_key.starts_with(local_crate_name.as_str())
                && (tagged_key.len() == n || tagged_key.as_bytes().get(n) == Some(&b':'));

            let raw: nudox_ir::index::RawRef = if is_local_key {
                out.refer::<IrModule>(tagged_key).into_raw()
            } else {
                out.refer_import::<IrModule>(ctx.foreign_key(tagged_key.as_str()))
                    .into_raw()
            };
            // Exhaustive on purpose. This used to be `match raw.as_local() { …,
            // None => continue }`, which was load-bearing only because
            // `refer_import` returned a `Ref::Local` into the import arena. The
            // moment a foreign reference stops being `Local`, that `continue`
            // silently drops **every** `pub use core::fmt::Debug;`-shaped
            // re-export from the IR — no error, no warning, just fewer entries,
            // which is precisely the failure class this whole change exists to
            // kill.
            match raw {
                nudox_ir::index::Ref::Local(untyped_idx) => {
                    nudox_ir::index::Ref::Local(untyped_idx.typed())
                }
                nudox_ir::index::Ref::Foreign { key, target } => {
                    nudox_ir::index::Ref::Foreign { key, target }
                }
                // `refer`/`refer_import` never mint an `Intro`; sealing does.
                nudox_ir::index::Ref::Intro(id) => nudox_ir::index::Ref::Intro(id),
            }
        };

        // This scan reaches re-exports through module scope rather than the
        // `use` syntax node. The canonical item's location is not the
        // re-export's location, and pointing there would make navigation land
        // on a different declaration. The alias entry itself is synthesized
        // by this lowering pass, so its absence is irreducible and explicit.
        let reexport_location = scope_only_reexport_location();
        let (reexport_source, reexport_span) = reexport_location.legacy_pair();
        let reexport_sym = nudox_ir::entry::Symbol {
            name: scope_name.to_string(),
            visibility: nudox_ir::entry::Visibility::Public,
            documentation: String::new(),
            source: reexport_source,
            span: reexport_span,
            aliases: Box::new([]),
            deprecation: None,
            doc_links: Box::new([]),
            attrs: Box::new([]),
            cfg: None,
        };

        ctx.check_unique(&reexport_id);
        out.declare_ref_at::<Reexport>(
            reexport_id,
            Some(mod_id.clone()),
            reexport_sym,
            target_ref,
            reexport_location,
        );
    }

    Ok(())
}

/// A scope-only alias has no syntax node in the lowering input. Its target's
/// location is intentionally not borrowed because that would navigate to a
/// different declaration.
fn scope_only_reexport_location() -> SourceLocation {
    SourceLocation::Unlocated(Unlocated::Synthesized)
}

// ── Primary item lowering ─────────────────────────────────────────────────────

/// Lower one module-level `ModuleDef` into `out`.
///
/// Trait assoc items and impl methods are emitted by `lower_trait` and
/// `lower_impl` respectively.
///
/// # Namespace disambiguation
///
/// Rust has three namespaces: type, value, and macro.  A module can contain
/// both `mod serve` (type ns) and `fn serve` (value ns) — they are distinct
/// items but would produce the same id without disambiguation.  We tag
/// value-namespace items with `!v` and macro-namespace items with `!m`:
/// `module_id::serve!v` and `module_id::serve!m`.  Type-namespace items remain
/// bare.  The `!` delimiter cannot appear in a Rust identifier or path.
///
/// This tag is applied **only at module level**.  Items nested inside impl/trait
/// blocks do not need a namespace tag because the parent impl id already provides
/// full disambiguation.
pub(crate) fn lower(
    ctx: &mut LowerCtx<'_>,
    def: ModuleDef,
    parent: Option<RaId>,
    out: &mut Lowering<RaId>,
) -> Result<(), Error> {
    if matches!(def, ModuleDef::Module(_) | ModuleDef::EnumVariant(_)) {
        return Ok(());
    }

    match def {
        ModuleDef::Function(f) => {
            // Value namespace: tag with `!v` so `fn serve` does not collide
            // with `mod serve` or `struct Serve`.
            //
            // Id comes from `id_of` — the single authority also used by every
            // reference site (`ty.rs`, `generics.rs`) — rather than from the
            // walk-supplied `parent`. For ordinary items the two agree (the
            // module a function is declared in is always the module RA's
            // `declarations()` found it under), but see the `ModuleDef::Macro`
            // arm below for the case where they do not.
            let fn_name = f.name(ctx.db).as_str().to_owned();
            let fn_id = id_of(ctx, def).unwrap_or_else(|| RaId::from(fn_name.as_str()));
            lower_free_function_with_id(ctx, f, fn_id, parent, out)?;
        }
        ModuleDef::Adt(Adt::Struct(s)) => {
            lower_struct(ctx, s, parent, out)?;
        }
        ModuleDef::Adt(Adt::Enum(e)) => {
            lower_enum(ctx, e, parent, out)?;
        }
        ModuleDef::Adt(Adt::Union(u)) => {
            lower_union(ctx, u, parent, out)?;
        }
        ModuleDef::Trait(t) => {
            lower_trait(ctx, t, parent, out)?;
        }
        ModuleDef::TypeAlias(ta) => {
            lower_type_alias(ctx, ModuleDef::TypeAlias(ta), parent, out)?;
        }
        ModuleDef::Const(c) => {
            // `const _: () = …` has no name and therefore no identity.
            //
            // It is unnameable and unreferenceable by construction — nothing
            // can import it, no signature can mention it, and no doc tool
            // shows it. It exists for its side effects at compile time
            // (assertions, trait-impl hygiene), and macros emit it freely:
            // `pin_project_lite` and derive expansions put several in one
            // module, which is how six of them collided on `{module}::_!v`.
            //
            // Synthesising an identity for something that has none would mean
            // inventing a positional or counter-based id — exactly the
            // order-dependent construct the rest of this module exists to
            // avoid. Skipping is not a loss of fidelity: there is nothing here
            // for a consumer to refer to.
            let Some(name) = c.name(ctx.db) else {
                return Ok(());
            };
            // Value namespace: tag with `!v`.  Id via `id_of` — see the
            // `ModuleDef::Function` arm above for why.
            let const_name = name.as_str().to_owned();
            let const_id = id_of(ctx, def).unwrap_or_else(|| RaId::from(const_name.as_str()));
            lower_const_with_id(ctx, c, const_id, parent, out)?;
        }
        ModuleDef::Static(s) => {
            // Value namespace: tag with `!v`.  Id via `id_of` — see the
            // `ModuleDef::Function` arm above for why.
            let static_name = s.name(ctx.db).as_str().to_owned();
            let static_id = id_of(ctx, def).unwrap_or_else(|| RaId::from(static_name.as_str()));
            lower_static_with_id(ctx, s, static_id, parent, out)?;
        }
        ModuleDef::Macro(m) => {
            // Macro namespace: tag with `!m`.  No Macro kind in new IR — emit as Module.
            //
            // This is the case `id_of` exists for. A `#[macro_export]`
            // `macro_rules!` item is *declared* (per `Module::declarations`,
            // which drives the walk in `walk.rs`) at the **crate root**,
            // unconditionally, regardless of which module it is textually
            // written in — that is what `#[macro_export]` means. But its
            // *canonical path* (`def.module(db)`, which `id_of`/`canonical`
            // read) is still its **defining** module.
            //
            // The walk-supplied `parent` here is therefore the crate root,
            // not the defining module. Keying the declaration off `parent`
            // (as every other arm above used to, and as this arm used to)
            // computes `{root}::{name}!m` — but `lower_module`'s re-export
            // scan (which walks `Module::scope`, where the macro is also
            // visible at the root through the very same `#[macro_export]`
            // visibility) independently computes a *reference* to this same
            // item as `{defining_module}::{name}!m` via `ctx.canonical`. Two
            // formulas, two answers for one item: the declare call collides
            // with the reexport's own id at the root path, while the
            // reexport's target reference points at a defining-module path
            // that nothing ever declares.
            //
            // Routing the declaration through `id_of` (the defining-module
            // path) instead makes both sides agree by construction: the real
            // declaration lands where every reference expects it, and
            // `lower_module`'s scan legitimately emits a `Reexport` at the
            // root path pointing to it (that reexport *is* the correct model
            // of `#[macro_export]`'s crate-root visibility — the macro is
            // simultaneously "declared in its home module" and "re-exported
            // at the crate root", exactly like a `pub use` would be).
            let macro_name = m.name(ctx.db).as_str().to_owned();
            let macro_id =
                id_of(ctx, ModuleDef::Macro(m)).unwrap_or_else(|| RaId::from(macro_name.as_str()));
            let parts = ctx
                .symbol_parts(ModuleDef::Macro(m))
                .unwrap_or_else(default_parts);
            let location = source::def_location(ctx, ModuleDef::Macro(m));
            ctx.check_unique(&macro_id);
            out.declare_at(
                macro_id,
                parent,
                parts.into_symbol(&location),
                IrModule,
                location,
            );
        }
        ModuleDef::BuiltinType(b) => {
            // Emit as Alias with no target.
            let id = RaId::from(b.name().as_str());
            // `u32` and friends are declared by the language, not by any file
            // in any package. `Synthesized` is the accurate answer, not a gap.
            let location = SourceLocation::Unlocated(Unlocated::Synthesized);
            let (source, span) = location.legacy_pair();
            let sym = nudox_ir::entry::Symbol {
                name: b.name().as_str().to_owned(),
                visibility: nudox_ir::entry::Visibility::Public,
                documentation: String::new(),
                source,
                span,
                aliases: Box::new([]),
                deprecation: None,
                doc_links: Box::new([]),
                attrs: Box::new([]),
                cfg: None,
            };
            ctx.check_unique(&id);
            out.declare_at(
                id,
                parent,
                sym,
                Alias::builder().maybe_target(None).build(),
                location,
            );
        }
        ModuleDef::Module(_) | ModuleDef::EnumVariant(_) => {}
    }
    Ok(())
}

// ── Free function ─────────────────────────────────────────────────────────────

/// Lower a free or impl/trait-assoc function with an already-computed `fn_id`.
///
/// The caller is responsible for computing a globally unique `fn_id`:
/// - module-level: `child_id(module_id, "{name}!v")` (value-ns tag)
/// - impl/trait member: `child_id(impl_or_trait_id, name)` (no tag needed;
///   the parent id already encodes the self type and trait)
fn lower_free_function_with_id(
    ctx: &mut LowerCtx<'_>,
    f: HirFunction,
    fn_id: RaId,
    parent: Option<RaId>,
    out: &mut Lowering<RaId>,
) -> Result<(), Error> {
    let def = ModuleDef::Function(f);
    let mut ref_for = make_ref_for!(ctx, out);
    let Some(fd) = function::lower_function(ctx, f, &mut ref_for) else {
        return Ok(());
    };

    // Declare Param entries under the *function's own* id, not its enclosing
    // module/impl — see `declare_params` and this closure's inline comment for
    // why: the seal pass (`workspace/ir/model/src/package/seal.rs`) derives a
    // param's collision identity purely from its declared-parent chain, and a
    // param parented on the module cannot be told apart from the same-named
    // param of any sibling function in that module.
    let input_refs = declare_params(ctx, &fn_id, &fd.input_params, out);
    let output_refs = fd.output_param.as_ref().map(|pd| {
        let param_id = RaId::from(format!("{fn_id}::param::return").as_str());
        let param_sym = plain_sym(&pd.name);
        let param_body = Param::builder()
            .maybe_ty(pd.ty.clone())
            .attributes(pd.attributes.iter().copied())
            .build();
        ctx.check_unique(&param_id);
        // Parent on `fn_id`, not `parent` (the module/impl): a `return` Param's
        // ancestor-path must include the owning function's name so that two
        // sibling functions' return params seal to distinct IntroIds without
        // relying on the ordinal-escalation backstop.
        out.declare_at(
            param_id.clone(),
            Some(fn_id.clone()),
            param_sym,
            param_body,
            SourceLocation::Unlocated(Unlocated::Synthesized),
        );
        let r: Ref<Param> = out.refer(param_id);
        r
    });

    let fn_body = Function::builder()
        .maybe_receiver(fd.body.receiver)
        .input_params(input_refs)
        .output_params(output_refs)
        .modifiers(fd.body.modifiers.iter().copied())
        .generics(fd.body.generics.iter().cloned())
        .wheres(fd.body.wheres.iter().cloned())
        .maybe_abi(fd.body.abi.clone())
        .is_defaulted(fd.body.is_defaulted)
        .build();

    let location = source::fn_location(ctx, f);
    let parts = ctx.symbol_parts(def).unwrap_or_else(default_parts);
    let sym = parts.into_symbol(&location);

    ctx.check_unique(&fn_id);
    out.declare_at(fn_id.clone(), parent, sym, fn_body, location);

    // Record occurrences from the function body (post-declare so fn_id is valid).
    // Individual sema calls inside are already panic-guarded; a body with no
    // AST source (e.g. `BuiltinDeriveImplMethod`) returns early itself.
    record_body_occurrences(ctx, f, &fn_id);

    Ok(())
}

// ── Body occurrence recording ─────────────────────────────────────────────────

/// Walk a function's AST body and record pre-seal occurrence facts into
/// `ctx.occurrence_buf`.
///
/// # What is recorded
///
/// * `MethodCall`: every `recv.method(args)` expression whose target function
///   can be resolved via `Semantics::resolve_method_call`.
/// * `FunctionCall`: every `path(args)` expression whose callee resolves to a
///   function via `Semantics::resolve_path` on the call's path prefix.
/// * `TypeReference`: every `PathType` in the body whose target is a struct /
///   enum / union / trait resolved via `Semantics::resolve_path`.
///
/// # Cost bound
///
/// The syntax tree is walked once and each resolved source occurrence emits
/// one compact fact. There is no arbitrary per-function cap: truncating a
/// generated function at the 500th call made the usage index depend on source
/// order while still claiming completeness. Posting-list construction later
/// deduplicates owners for reverse-usage queries; body facts retain every span.
///
/// # Confidence
///
/// All occurrences emitted here carry `Confidence::Oracle` — every fact is
/// backed by RA's full type-inference pass, not heuristics.
///
/// # Cross-crate targets
///
/// Local targets (same crate) are stored as `PendingTarget::Local(ra_id)`.
/// Foreign targets are stored as `PendingTarget::Foreign(canonical_path)`.
/// Both are resolved to `IntroId` / `StableRef` post-seal.
fn record_body_occurrences(ctx: &mut LowerCtx<'_>, f: HirFunction, fn_id: &RaId) {
    // Get the function's AST source. BuiltinDeriveImplMethod falls back to the
    // trait method AST, which has no body → we bail early on `None`.
    let Some(src) = ctx.sema.source(f).or_else(|| f.source(ctx.db)) else {
        return;
    };
    // `ast::Fn::body()` returns the body block; `None` for declarations
    // (e.g. trait method stubs) and for BuiltinDeriveImplMethod fallbacks.
    let Some(body) = src.value.body() else {
        return;
    };

    // The function's span start in the file — used to compute relative spans.
    let fn_span_start = u32::from(src.value.syntax().text_range().start());

    let local_crate = super::ctx::crate_name(ctx.db, ctx.krate);

    // Walk every descendant node in the body once.
    for node in body.syntax().descendants() {
        match node.kind() {
            // ── Method call: `receiver.method(args)` ─────────────────────────
            SyntaxKind::METHOD_CALL_EXPR => {
                let Some(call) = syn_ast::MethodCallExpr::cast(node.clone()) else {
                    continue;
                };
                let Some(target_fn) =
                    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        ctx.sema.resolve_method_call(&call)
                    }))
                    .ok()
                    .flatten()
                else {
                    continue;
                };

                let Some(target_id) = method_call_target_id(ctx, target_fn, &local_crate) else {
                    continue;
                };

                let Some(name_ref) = call.name_ref() else {
                    continue;
                };
                let span = rel_span(fn_span_start, name_ref.syntax().text_range());

                ctx.occurrence_buf.push(PendingOcc {
                    owner: fn_id.clone(),
                    target: target_id,
                    kind: ReferenceKind::MethodCall,
                    span,
                });
            }

            // ── Path call: `func(args)` or `Type::method(args)` ──────────────
            SyntaxKind::CALL_EXPR => {
                let Some(call) = syn_ast::CallExpr::cast(node.clone()) else {
                    continue;
                };
                let Some(callee) = call.expr() else {
                    continue;
                };
                // Extract the path from the callee expression.
                let path_opt = match &callee {
                    syn_ast::Expr::PathExpr(pe) => pe.path(),
                    _ => continue,
                };
                let Some(path) = path_opt else {
                    continue;
                };
                let Some(resolution) =
                    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        ctx.sema.resolve_path(&path)
                    }))
                    .ok()
                    .flatten()
                else {
                    continue;
                };

                let Some(target_id) = path_resolution_to_target(ctx, resolution, &local_crate)
                else {
                    continue;
                };

                let span = rel_span(fn_span_start, path.syntax().text_range());

                ctx.occurrence_buf.push(PendingOcc {
                    owner: fn_id.clone(),
                    target: target_id,
                    kind: ReferenceKind::FunctionCall,
                    span,
                });
            }

            // ── Type references: `Foo`, `Foo<T>`, `<T as Trait>::Assoc` ──────
            SyntaxKind::PATH_TYPE => {
                let Some(pt) = syn_ast::PathType::cast(node.clone()) else {
                    continue;
                };
                let Some(path) = pt.path() else {
                    continue;
                };
                let Some(resolution) =
                    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        ctx.sema.resolve_path(&path)
                    }))
                    .ok()
                    .flatten()
                else {
                    continue;
                };

                // Only record type-namespace targets.
                let Some(target_id) = type_ref_target(ctx, resolution, &local_crate) else {
                    continue;
                };

                let span = rel_span(fn_span_start, path.syntax().text_range());

                ctx.occurrence_buf.push(PendingOcc {
                    owner: fn_id.clone(),
                    target: target_id,
                    kind: ReferenceKind::TypeReference,
                    span,
                });
            }

            _ => {}
        }
    }
}

/// Compute a `RelSpan` from a salsa `TextRange` and an optional owner span start.
#[inline]
fn rel_span(fn_span_start: u32, r: ra_ap_syntax::TextRange) -> RelSpan {
    let start = u32::from(r.start())
        .checked_sub(fn_span_start)
        .expect("a body descendant cannot precede its owning function");
    let end = u32::from(r.end())
        .checked_sub(fn_span_start)
        .expect("a body descendant cannot precede its owning function");
    RelSpan::new(start, end)
}

/// Resolve a method call's target `Function` to a `PendingTarget`.
///
/// Returns `None` when the method has no canonical path (anonymous impl on a
/// block-local type) or when it belongs to a crate with no display name.
fn method_call_target_id(
    ctx: &mut LowerCtx<'_>,
    target_fn: HirFunction,
    local_crate: &str,
) -> Option<PendingTarget> {
    let ra_def = ModuleDef::Function(target_fn);
    let canon = ctx.canonical(ra_def)?;
    if canon.starts_with(local_crate)
        && (canon.len() == local_crate.len()
            || canon.as_bytes().get(local_crate.len()) == Some(&b':'))
    {
        // Local — but we need the id_of form (with assoc container + ns tag).
        let full_id = id_of(ctx, ra_def)?;
        Some(PendingTarget::Local(full_id))
    } else {
        Some(PendingTarget::Foreign(ctx.foreign_key(canon.as_str())))
    }
}

/// Convert a `PathResolution` to a `PendingTarget` (function-call context).
fn path_resolution_to_target(
    ctx: &mut LowerCtx<'_>,
    res: PathResolution,
    local_crate: &str,
) -> Option<PendingTarget> {
    let def = match res {
        PathResolution::Def(d) => d,
        _ => return None,
    };
    // Only function-like targets.
    if !matches!(def, ModuleDef::Function(_)) {
        return None;
    }
    let canon = ctx.canonical(def)?;
    if canon.starts_with(local_crate)
        && (canon.len() == local_crate.len()
            || canon.as_bytes().get(local_crate.len()) == Some(&b':'))
    {
        let full_id = id_of(ctx, def)?;
        Some(PendingTarget::Local(full_id))
    } else {
        Some(PendingTarget::Foreign(ctx.foreign_key(canon.as_str())))
    }
}

/// Convert a `PathResolution` to a `PendingTarget` (type-reference context).
///
/// Returns `None` for non-type-namespace targets (functions, consts, modules).
fn type_ref_target(
    ctx: &mut LowerCtx<'_>,
    res: PathResolution,
    local_crate: &str,
) -> Option<PendingTarget> {
    let def = match res {
        PathResolution::Def(d) => d,
        _ => return None,
    };
    // Type namespace: struct, enum, union, trait, type alias.
    match def {
        ModuleDef::Adt(_) | ModuleDef::Trait(_) | ModuleDef::TypeAlias(_) => {}
        _ => return None,
    }
    let canon = ctx.canonical(def)?;
    if canon.starts_with(local_crate)
        && (canon.len() == local_crate.len()
            || canon.as_bytes().get(local_crate.len()) == Some(&b':'))
    {
        let full_id = id_of(ctx, def)?;
        Some(PendingTarget::Local(full_id))
    } else {
        Some(PendingTarget::Foreign(ctx.foreign_key(canon.as_str())))
    }
}

/// Lower an impl/trait assoc function where the id is derived from the parent
/// impl/trait id via `child_id`.
///
/// Called from `lower_impl` and `lower_trait_assoc_items`.  The `parent` here
/// is the impl's or trait's `RaId`, which already encodes the self type and
/// optional trait, so the child name alone is sufficient disambiguation.
fn lower_assoc_function(
    ctx: &mut LowerCtx<'_>,
    f: HirFunction,
    parent_id: &RaId,
    parent: Option<RaId>,
    out: &mut Lowering<RaId>,
) -> Result<(), Error> {
    let fn_name = f.name(ctx.db).as_str().to_owned();
    let fn_id = ctx.child_id(parent_id, &fn_name);
    lower_free_function_with_id(ctx, f, fn_id, parent, out)
}

// ── Struct ────────────────────────────────────────────────────────────────────

fn lower_struct(
    ctx: &mut LowerCtx<'_>,
    s: Struct,
    parent: Option<RaId>,
    out: &mut Lowering<RaId>,
) -> Result<(), Error> {
    let def = ModuleDef::Adt(Adt::Struct(s));
    let Some(struct_id) = ctx.ra_id(def) else {
        return Ok(());
    };
    let mut ref_for = make_ref_for!(ctx, out);

    let form = match s.kind(ctx.db) {
        StructKind::Unit => RecordForm::Unit,
        StructKind::Tuple => RecordForm::Tuple,
        StructKind::Record => RecordForm::Struct,
    };

    let (generics, wheres) = {
        let src = ctx.sema.source(s).or_else(|| s.source(ctx.db));
        src.map(|src| {
            generics::lower_generics(
                ctx,
                src.value.generic_param_list(),
                src.value.where_clause(),
                &mut ref_for,
            )
        })
        .unwrap_or_default()
    };

    // Auto-trait facts.
    // `LangItem::Sync` and `LangItem::Unpin` are both resolvable via
    // `Trait::lang`.  `Send`, `UnwindSafe`, and `RefUnwindSafe` are NOT lang
    // items in ra_ap_hir 0.0.341 (`AttrFlags` in ra_ap_hir_def only records
    // `Sync` / `Unpin`); those three would require walking the `std` crate's
    // `marker` module by hand — not attempted here, so they are omitted.
    let hir_self = attach_db(ctx.db, || ra_ap_hir::Adt::Struct(s).ty(ctx.db));
    let auto: Vec<AutoFact> = probe_auto_traits_partial(ctx, &hir_self);

    let field_refs = match s.kind(ctx.db) {
        StructKind::Unit => Vec::new(),
        StructKind::Tuple | StructKind::Record => {
            declare_hir_fields(ctx, &struct_id, s.fields(ctx.db), form, parent.clone(), out)
        }
    };

    let record_body = Record::builder()
        .form(form)
        .fields(field_refs)
        .generics(generics)
        .wheres(wheres)
        .auto(auto)
        .build();

    let location = source::def_location(ctx, def);
    let parts = ctx.symbol_parts(def).unwrap_or_else(default_parts);
    let sym = parts.into_symbol(&location);

    ctx.check_unique(&struct_id);
    out.declare_at(struct_id, parent, sym, record_body, location);
    Ok(())
}

// ── Enum ──────────────────────────────────────────────────────────────────────

fn lower_enum(
    ctx: &mut LowerCtx<'_>,
    e: ra_ap_hir::Enum,
    parent: Option<RaId>,
    out: &mut Lowering<RaId>,
) -> Result<(), Error> {
    let def = ModuleDef::Adt(Adt::Enum(e));
    let Some(enum_id) = ctx.ra_id(def) else {
        return Ok(());
    };
    let mut ref_for = make_ref_for!(ctx, out);

    let (generics, wheres) = {
        let src = ctx.sema.source(e).or_else(|| e.source(ctx.db));
        src.map(|src| {
            generics::lower_generics(
                ctx,
                src.value.generic_param_list(),
                src.value.where_clause(),
                &mut ref_for,
            )
        })
        .unwrap_or_default()
    };

    // Auto-trait facts (same partial probing as for structs — see struct note).
    let hir_self = attach_db(ctx.db, || ra_ap_hir::Adt::Enum(e).ty(ctx.db));
    let auto: Vec<AutoFact> = probe_auto_traits_partial(ctx, &hir_self);

    // (declare_hir_fields creates its own ref_for internally).

    // Declare variant entries.
    let variant_refs: Vec<Ref<Variant>> = e
        .variants(ctx.db)
        .into_iter()
        .map(|v| {
            let variant_name = v.name(ctx.db).as_str().to_owned();
            let variant_id = RaId::from(format!("{enum_id}::{variant_name}").as_str());

            let form = match v.kind(ctx.db) {
                StructKind::Unit => VariantForm::Unit,
                StructKind::Tuple => VariantForm::Tuple,
                StructKind::Record => VariantForm::Struct,
            };

            // Declare field entries inside the variant.
            let field_refs = match v.kind(ctx.db) {
                StructKind::Unit => Vec::new(),
                _ => declare_hir_fields(
                    ctx,
                    &variant_id,
                    v.fields(ctx.db),
                    match form {
                        VariantForm::Tuple => RecordForm::Tuple,
                        _ => RecordForm::Struct,
                    },
                    Some(variant_id.clone()),
                    out,
                ),
            };

            // Discriminant (only for unit variants with an explicit value).
            // ast::Variant has .const_arg() not .expr(); the discriminant
            // expression lives inside the ConstArg.
            let discr: Option<String> = v
                .source(ctx.db)
                .and_then(|src| src.value.const_arg())
                .and_then(|ca| ca.expr())
                .map(|e_node| e_node.syntax().text().to_string());

            let variant_body = Variant::builder()
                .form(form)
                .fields(field_refs)
                .maybe_discr(discr)
                .build();

            let doc = docs::documentation(ctx, v);
            let doc_links = docs::doc_links(ctx, v, doc.as_deref()).unwrap_or_default();
            let attrs = v.source(ctx.db).map(|src| docs::ast_attrs(src.value));
            let variant_location = source::variant_location(ctx, v);
            let (variant_source, variant_span) = variant_location.legacy_pair();
            let variant_sym = nudox_ir::entry::Symbol {
                name: variant_name,
                visibility: nudox_ir::entry::Visibility::Public,
                documentation: doc.unwrap_or_default(),
                source: variant_source,
                span: variant_span,
                aliases: Box::new([]),
                deprecation: None,
                doc_links: doc_links.into(),
                attrs: attrs.unwrap_or_default(),
                cfg: docs::cfg_expr(ctx, v),
            };

            ctx.check_unique(&variant_id);
            out.declare_at(
                variant_id.clone(),
                Some(enum_id.clone()),
                variant_sym,
                variant_body,
                variant_location,
            );
            out.refer::<Variant>(variant_id)
        })
        .collect();

    let enum_body = Enum::builder()
        .variants(variant_refs)
        .generics(generics)
        .wheres(wheres)
        .auto(auto)
        .build();

    let location = source::def_location(ctx, def);
    let parts = ctx.symbol_parts(def).unwrap_or_else(default_parts);
    let sym = parts.into_symbol(&location);

    ctx.check_unique(&enum_id);
    out.declare_at(enum_id, parent, sym, enum_body, location);
    Ok(())
}

// ── Union ─────────────────────────────────────────────────────────────────────

fn lower_union(
    ctx: &mut LowerCtx<'_>,
    u: ra_ap_hir::Union,
    parent: Option<RaId>,
    out: &mut Lowering<RaId>,
) -> Result<(), Error> {
    let def = ModuleDef::Adt(Adt::Union(u));
    let Some(union_id) = ctx.ra_id(def) else {
        return Ok(());
    };
    // Declare fields (declare_hir_fields creates its own ref_for internally).
    // For union fields the key is `Named` — they share storage rather than
    // being co-resident, but each field is still accessed by name.
    let field_refs = declare_hir_fields(
        ctx,
        &union_id,
        u.fields(ctx.db),
        // Pass Struct so declare_hir_fields uses FieldKey::Named for every
        // field; positional keys only make sense for tuple records.
        RecordForm::Struct,
        parent.clone(),
        out,
    );

    // Unions lower as `Record` with `RecordForm::Union`.  The fields are
    // *alternatives* (only one is live at a time) rather than co-resident
    // members, which is a semantic distinction that affects both layout and
    // safety analysis.  Collapsing this to `RecordForm::Struct` silently
    // misreports the type.
    let record_body = Record::builder()
        .form(RecordForm::Union)
        .fields(field_refs)
        .build();

    let location = source::def_location(ctx, def);
    let parts = ctx.symbol_parts(def).unwrap_or_else(default_parts);
    let sym = parts.into_symbol(&location);

    ctx.check_unique(&union_id);
    out.declare_at(union_id, parent, sym, record_body, location);
    Ok(())
}

// ── Trait ─────────────────────────────────────────────────────────────────────

fn lower_trait(
    ctx: &mut LowerCtx<'_>,
    t: Trait,
    parent: Option<RaId>,
    out: &mut Lowering<RaId>,
) -> Result<(), Error> {
    let def = ModuleDef::Trait(t);
    let Some(trait_id) = ctx.ra_id(def) else {
        return Ok(());
    };

    // ── Guard: skip the whole trait if its id is already declared ────────────
    //
    // Mirrors the guard in `lower_impl`.  If the same trait is encountered a
    // second time (e.g. because a crate-level re-export and a module-level
    // declaration both appear in the walk), the first call wins.  Emitting
    // members with a trait id that is *already* declared as a different kind
    // (e.g. a Module) would silently re-parent them under that entry.
    if out.is_declared(&trait_id) {
        tracing::warn!(
            trait_id = %trait_id,
            "skipping duplicate trait: trait_id already declared; \
             members will not be emitted to avoid orphaned parent links"
        );
        return Ok(());
    }

    let mut ref_for = make_ref_for!(ctx, out);

    let trait_ast = ctx
        .sema
        .source(t)
        .or_else(|| t.source(ctx.db))
        .map(|src| src.value);

    let (generics, wheres) = trait_ast
        .as_ref()
        .map(|ast| {
            generics::lower_generics(
                ctx,
                ast.generic_param_list(),
                ast.where_clause(),
                &mut ref_for,
            )
        })
        .unwrap_or_default();

    // Supertrait bounds → `Trait.supers: List<Type>`.
    //
    // `Trait::direct_supertraits` strips generic args — it returns bare `Trait`
    // values with no information about the written `Bar<u32>` arguments.  To
    // preserve those we walk the AST `type_bound_list` instead.  Each path
    // bound resolves to a `ModuleDef::Trait` via `Semantics::resolve_path`;
    // the associated generic args are recovered from the last path segment via
    // `last_segment_type_args`.  The result is:
    //   • `trait Foo: Bar<u32>` → `Type::Apply { base: Nominal(Bar), args: [u32] }`
    //   • `trait Foo: Bar`      → `Type::Nominal(Bar)`
    let supers: Vec<nudox_ir::kinds::Type> = {
        let ast_bounds = trait_ast.as_ref().and_then(|ast| ast.type_bound_list());

        if let Some(bounds) = ast_bounds {
            bounds
                .bounds()
                .filter_map(|b| {
                    // Only PathType bounds (not lifetime bounds).
                    let path_ty = match b.kind() {
                        Some(ra_ap_syntax::ast::TypeBoundKind::PathType(_, path_ty)) => path_ty,
                        _ => return None,
                    };
                    let path = path_ty.path()?;
                    // Resolve the path to a Trait.
                    let res = ty::resolve_path_opt(ctx, &path)?;
                    let def = match res {
                        ra_ap_hir::PathResolution::Def(d) => d,
                        _ => return None,
                    };
                    let key = id_of(ctx, def)?;
                    let raw_ref = ref_for(&key)?;
                    let base = nudox_ir::kinds::Type::Nominal(raw_ref);
                    // Recover written generic args from the last path segment.
                    let type_args = ty::last_segment_type_args_pub(ctx, &path, &mut ref_for);
                    if type_args.is_empty() {
                        Some(base)
                    } else {
                        Some(nudox_ir::kinds::Type::Apply {
                            base: Box::new(base),
                            args: type_args.into_boxed_slice(),
                        })
                    }
                })
                .collect()
        } else {
            // No AST source (e.g. macro-generated trait); fall back to HIR,
            // which loses generic args but is better than nothing.
            t.direct_supertraits(ctx.db)
                .into_iter()
                .filter_map(|st| {
                    let st_def = ModuleDef::Trait(st);
                    let key = id_of(ctx, st_def)?;
                    let raw_ref = ref_for(&key)?;
                    Some(nudox_ir::kinds::Type::Nominal(raw_ref))
                })
                .collect()
        }
    };

    let flags = TraitFlags {
        is_unsafe: t.is_unsafe(ctx.db),
        is_auto: t.is_auto(ctx.db),
        dyn_compat: match t.dyn_compatibility(ctx.db) {
            None => TriState::Yes,
            Some(_) => TriState::No,
        },
        sealed: detect_sealed(ctx, t),
    };

    let trait_body = IrTrait::builder()
        .flags(flags)
        .supers(supers)
        .generics(generics)
        .wheres(wheres)
        .build();

    let location = source::def_location(ctx, def);
    let parts = ctx.symbol_parts(def).unwrap_or_else(default_parts);
    let sym = parts.into_symbol(&location);

    // ── Declare the trait entry FIRST, then its members ──────────────────────
    //
    // Mirrors the fix in `lower_impl`: if we declared assoc items *before* the
    // trait entry and then a panic or early-return prevented the trait's own
    // `out.declare` call, those members would have `parent = Some(trait_id)`
    // pointing at a slot that is never filled — or, worse, at a *different*
    // entry that happens to carry the same id string (e.g. the enclosing module
    // when the trait_id was already interned via `intern(parent=trait_id)` in
    // `declare_params`, and the module was declared with that same string).
    //
    // Declaring the trait first ensures that if the items loop panics, the trait
    // entry itself still exists in the tree and the already-declared members
    // keep a valid parent.
    ctx.check_unique(&trait_id);
    out.declare_at(trait_id.clone(), parent, sym, trait_body, location);

    // ── Declare assoc items as children ──────────────────────────────────────
    lower_trait_assoc_items(ctx, t, &trait_id, out)?;

    Ok(())
}

fn lower_trait_assoc_items(
    ctx: &mut LowerCtx<'_>,
    t: Trait,
    trait_id: &RaId,
    out: &mut Lowering<RaId>,
) -> Result<(), Error> {
    for item in t.items(ctx.db) {
        match item {
            AssocItem::Function(f) => {
                // Use child_id(trait_id, name): the trait id already encodes
                // the full path, so two traits with the same assoc fn name
                // cannot collide.
                lower_assoc_function(ctx, f, trait_id, Some(trait_id.clone()), out)?;
            }
            AssocItem::TypeAlias(ta) => {
                // Assoc type alias: id = child_id(trait_id, name).
                lower_assoc_type_alias(ctx, ta, trait_id, Some(trait_id.clone()), out)?;
            }
            AssocItem::Const(c) => {
                // Assoc const: id = child_id(trait_id, name).
                lower_assoc_const(ctx, c, trait_id, Some(trait_id.clone()), out)?;
            }
        }
    }
    Ok(())
}

fn detect_sealed(ctx: &LowerCtx<'_>, t: Trait) -> nudox_ir::kinds::Sealed {
    use nudox_ir::kinds::Sealed;
    use ra_ap_hir::Visibility as HirVis;

    let is_sealed = t.direct_supertraits(ctx.db).into_iter().any(|st| {
        !matches!(st.visibility(ctx.db), HirVis::Public)
            || !st
                .module(ctx.db)
                .path_to_root(ctx.db)
                .into_iter()
                .filter(|m| !m.is_crate_root(ctx.db))
                .all(|m| matches!(m.visibility(ctx.db), HirVis::Public))
    });
    if is_sealed {
        Sealed::PubApi
    } else {
        Sealed::None
    }
}

// ── Impl ──────────────────────────────────────────────────────────────────────

/// Lower a trait impl or inherent impl block into `out`.
///
/// Inherent impls are emitted as `Impl { of: None, … }`.  Trait impls carry
/// `of: Some(Type::Nominal(trait_ref))`.  Child methods are declared under the
/// impl's `RaId`.
///
/// # Member-before-impl ordering hazard (fixed)
///
/// The old ordering was:
///   1. emit members (with `parent = Some(impl_id)`)
///   2. `check_unique(&impl_id)` → panics on duplicate
///   3. `out.declare(impl_id, ...)`
///
/// When step 2 panicked, the panic was caught by `catch_non_cancelled` in
/// `walk.rs`, silently discarding the call.  The impl entry was never declared,
/// so `slots[impl_id]` stayed `None` (referred but undeclared).  In `finish()`,
/// a referred-but-undeclared id means the parent pointer resolves to `None`
/// (root module), which is a Module-kind entry — exactly the bug where `fmt`
/// entries appeared as children of a Module rather than an Impl.
///
/// **Fix**: declare the impl entry *first*, then emit members.  If the impl_id
/// is a duplicate (already declared), we skip the whole impl including its
/// members — an orphaned member is always worse than an absent one.
pub(crate) fn lower_impl(
    ctx: &mut LowerCtx<'_>,
    imp: Impl,
    parent: Option<RaId>,
    out: &mut Lowering<RaId>,
) -> Result<(), Error> {
    if imp.is_negative(ctx.db) {
        // Negative impls (`impl !Trait for T`) are emitted with `ImplFlags::negative`.
        // We still emit them so they are visible to analysis.
    }

    let impl_id = impl_id(ctx, imp);

    // ── Guard: skip the whole impl if its id is already declared ─────────────
    //
    // `check_unique` is a debug-only assert; `is_declared` is the release-safe
    // equivalent.  Both are checked here so that:
    //   • debug builds panic loudly at the duplicate site (not inside finish()),
    //   • release builds skip cleanly without pushing to `duplicates`.
    //
    // Emitting members when the impl entry cannot be declared produces orphaned
    // entries whose parent id is interned but never filled — `finish()` would
    // then report the impl_id as undeclared (even though members reference it),
    // or in the best case the members resolve their parent to root (Module),
    // silently poisoning the child list of whichever entry has the same id.
    if out.is_declared(&impl_id) {
        tracing::warn!(
            impl_id = %impl_id,
            "skipping duplicate impl: impl_id already declared; \
             members will not be emitted to avoid orphaned parent links"
        );
        return Ok(());
    }

    let mut ref_for = make_ref_for!(ctx, out);

    let self_ty = {
        let hir_self = attach_db(ctx.db, || imp.self_ty(ctx.db));
        ty::lower_hir_type_fallback(ctx, &hir_self, &mut ref_for)
    };

    let of_type = imp.trait_(ctx.db).and_then(|tr| {
        let tr_def = ModuleDef::Trait(tr);
        let key = id_of(ctx, tr_def)?;
        let raw_ref = ref_for(&key)?;
        Some(nudox_ir::kinds::Type::Nominal(raw_ref))
    });

    let impl_ast = ctx
        .sema
        .source(imp)
        .or_else(|| imp.source(ctx.db))
        .map(|s| s.value);

    let (generics, wheres) = impl_ast
        .as_ref()
        .map(|ast| {
            generics::lower_generics(
                ctx,
                ast.generic_param_list(),
                ast.where_clause(),
                &mut ref_for,
            )
        })
        .unwrap_or_default();

    let flags = ImplFlags {
        negative: imp.is_negative(ctx.db),
        blanket: attach_db(ctx.db, || {
            imp.self_ty(ctx.db).as_type_param(ctx.db).is_some()
        }),
    };

    let impl_body = IrImpl::builder()
        .flags(flags)
        .maybe_of(of_type)
        .self_ty(self_ty)
        .generics(generics)
        .wheres(wheres)
        .build();

    // The `impl` block's own header range. `wire::ImplRow` had no source field
    // at all (docs/LIMITATIONS.md L42.3); this is where the value it now carries is
    // produced.
    let impl_location = source::impl_location(ctx, imp);
    let (impl_source, impl_span) = impl_location.legacy_pair();
    let impl_attrs = impl_ast.as_ref().map(|ast| docs::ast_attrs(ast.clone()));
    let sym = nudox_ir::entry::Symbol {
        name: impl_display_name(ctx, imp),
        visibility: nudox_ir::entry::Visibility::Public,
        documentation: docs::documentation(ctx, imp).unwrap_or_default(),
        source: impl_source,
        span: impl_span,
        aliases: Box::new([]),
        deprecation: None,
        doc_links: Box::new([]),
        attrs: impl_attrs.unwrap_or_default(),
        cfg: docs::cfg_expr(ctx, imp),
    };

    // ── Declare the impl entry FIRST ─────────────────────────────────────────
    //
    // Members reference `impl_id` as their parent.  If this declaration comes
    // *after* the members (the old order), a panic here (caught by
    // `catch_non_cancelled`) leaves the members with an undeclared parent —
    // which `finish()` treats as "referred but never declared" and resolves to
    // the root module.  Declaring first ensures the parent is always live before
    // any member references it.
    ctx.check_unique(&impl_id);
    out.declare_at(impl_id.clone(), parent, sym, impl_body, impl_location);

    // ── Declare impl methods as children ─────────────────────────────────────
    //
    // Use lower_assoc_* so the id is child_id(impl_id, name) rather than
    // ra_id(def), which would collide across different impls providing the same
    // method name (e.g. Display::fmt in many types in one module).
    //
    // ALL assoc items are declared, regardless of visibility.  A private method
    // that appears in a public signature (e.g. as a `Fn`-bound type argument)
    // would leave a dangling `refer` slot if we skipped it here (Group C fix).
    // Visibility is stored in `Symbol.visibility`; consumers filter on that.
    for item in imp.items(ctx.db) {
        if let AssocItem::Function(f) = item {
            lower_assoc_function(ctx, f, &impl_id, Some(impl_id.clone()), out)?;
        }
        // TypeAlias and Const assoc items inside impl blocks.
        if let AssocItem::TypeAlias(ta) = item {
            lower_assoc_type_alias(ctx, ta, &impl_id, Some(impl_id.clone()), out)?;
        }
        if let AssocItem::Const(c) = item {
            lower_assoc_const(ctx, c, &impl_id, Some(impl_id.clone()), out)?;
        }
    }

    Ok(())
}

/// Generate a unique, semantic `RaId` for an impl block.
///
/// Impls have no stable name in Rust.  We synthesise one from the module,
/// the fully-rendered self type, the trait (if any), and the where clause:
///
/// - Inherent impl: `{module}::<{self_ty}>{where}`
/// - Trait impl:    `{module}::<{self_ty} as {trait_canonical}{trait_args}>{where}`
///
/// ## Trait path (Cause 2 fix)
///
/// `trait_ref.display(db, display_target)` renders the trait name relative to
/// the target crate, so two different traits named `Error` from different
/// crates both render as plain `"Error"` — a collision (e.g. `serde_core::de::Error`
/// vs `std::error::Error` for `PathDeserializationError`).
///
/// We fix this by splitting the trait key into two parts:
/// 1. **Canonical path** — `ctx.canonical(ModuleDef::Trait(t))`, which always
///    produces the fully-qualified defining path (`core::fmt::Debug`,
///    `serde_core::de::Error`, etc.) regardless of the display target.
/// 2. **Generic argument list** — extracted from `trait_ref.display()` as the
///    substring starting at the first `<`.  For `From<axum::extract::InvalidUtf8>`
///    this yields `<axum::extract::InvalidUtf8>`; for bare `Error` it yields `""`.
///
/// Combining them: `canonical_path + arg_suffix` gives a globally unique,
/// fully-qualified key for any trait instantiation.
///
/// ## Where clause (Cause 3 fix)
///
/// Two inherent `impl<L, M, S, F> WithGracefulShutdown<L, M, S, F>` blocks in
/// the same module differ only in their `where` clauses.  Including the where
/// clause text (whitespace-normalised) in the key makes them distinct:
///
///   `axum::serve::<WithGracefulShutdown<L, M, S, F>> where L: Listener`
///   `axum::serve::<WithGracefulShutdown<L, M, S, F>> where L: Listener, ...`
///
/// The generic param list is included too for the rarer case of two impls
/// whose parameter declarations differ but whose where clauses are the same
/// (e.g. `#[cfg]`-gated impls with different bounds spelled as params).
///
/// ## Last resort (not implemented)
///
/// If the above still collides (e.g. two `#[cfg]`-split impls that are
/// syntactically identical), we would fall back to `source_path:line`.  That
/// case has not been observed in axum and is deferred rather than implemented
/// pessimistically.
///
/// A human-readable name for an impl block: `impl Debug for Foo<T>`.
///
/// # Why this is not the `RaId`
///
/// `impl_id` is an *identity*: it carries the module, the fully-qualified
/// trait path, the rendered generics and where-clause, and the sorted member
/// list, because all of that is needed to keep two impls apart. Using it as
/// the display name put strings like
/// `axum::extract::rejection::<FailedToDeserializeForm as core::fmt::Debug>[fmt]`
/// into the `name` field — which is what the search index tokenises and what
/// every UI shows. 791 of axum's entries were affected.
///
/// Identity and presentation are different jobs. This renders the impl the way
/// rustdoc writes it, and nothing else depends on its exact shape.
fn impl_display_name(ctx: &mut LowerCtx<'_>, imp: Impl) -> String {
    let self_ty = attach_db(ctx.db, || {
        imp.self_ty(ctx.db).display(ctx.db, ctx.display).to_string()
    });
    let name = match attach_db(ctx.db, || imp.trait_ref(ctx.db)) {
        Some(trait_ref) => {
            // Use the bare trait name (e.g. "Debug") to avoid
            // fully-qualified paths like "core::fmt::Debug" in names.
            let short_name = attach_db(ctx.db, || {
                trait_ref.trait_().name(ctx.db).as_str().to_owned()
            });
            // Preserve generic arguments from the full display string.
            let full_display = attach_db(ctx.db, || {
                trait_ref.display(ctx.db, ctx.display).to_string()
            });
            let args = full_display
                .find('<')
                .map(|lt| full_display[lt..].to_owned())
                .unwrap_or_default();
            format!("impl {short_name}{args} for {self_ty}")
        }
        None => format!("impl {self_ty}"),
    };
    // `short_name` above only strips qualification from the *trait path*
    // itself. It does nothing about a qualified associated-type path
    // (`<T as IntoParallelIterator>::Item`) sitting inside a generic
    // argument of the trait's `args` or of `self_ty` — and that is not rare:
    // any combinator over a generic parameter's associated type writes
    // exactly this (rayon's `impl Folder<T> for FlattenFolder<C, <C as
    // Consumer<<T as IntoParallelIterator>::Item>>::Result>` is where this
    // was found). That syntax is legal Rust and rust-analyzer's `Display`
    // renders it faithfully, but it embeds a `::` inside a `<...>` — the
    // exact shape the doc comment above already fixed once for the trait
    // path, recurring through a route that fix didn't cover. `Symbol.name`
    // must never contain `::` (see `crates/nudox-store/tests/real_package.rs`:
    // the name index treats it as a path separator), so collapse every such
    // qualifier to its bare trailing segment before returning.
    strip_qualified_path_qualifiers(name)
}

/// Collapse every `<Type as Trait>::Assoc` qualified-path segment in `s` down
/// to its bare trailing identifier (`Assoc`).
///
/// This exists for [`impl_display_name`]: a *display* string built from
/// rust-analyzer's `Display` impl is correct Rust syntax, but correct Rust
/// syntax is not an identifier, and `Symbol.name` is contractually required
/// to be one. Qualified-path syntax is the one construct that can smuggle a
/// `::` into an otherwise plain `Name<Args>` string. Nesting
/// (`<C as Consumer<<T as X>::Item>>::Result`) is handled by repeating to a
/// fixpoint: each pass collapses the innermost qualifier, which can expose
/// an outer one that was only a qualifier once its own inner `<...>` shrank.
fn strip_qualified_path_qualifiers(mut s: String) -> String {
    while let Some(next) = strip_one_qualifier(&s) {
        s = next;
    }
    s
}

/// Collapse the single innermost `<... as ...>::Ident` qualifier in `s`, or
/// return `None` if `s` contains none.
///
/// "Innermost" is found with a bracket-depth stack rather than a regex
/// because the qualifier's own generic arguments can themselves contain
/// unrelated `<...>` pairs (`Consumer<Item>` has one with no `as` in it) —
/// only a bracket-matched scan tells a qualifying `<X as Y>` apart from a
/// plain generic-argument list that merely contains one deeper in the tree.
///
/// A `>` with an empty stack is not a bracket close at all — `Fn(i32) ->
/// bool`-shaped `Fn`-trait sugar puts a bare `->` right in the same display
/// string, with no preceding unmatched `<`. That is skipped, not treated as
/// malformed input: an early `return None` there would abort the scan and
/// leave a real qualifier later in the string unstripped.
fn strip_one_qualifier(s: &str) -> Option<String> {
    let mut open_stack: Vec<usize> = Vec::new();
    for (i, ch) in s.char_indices() {
        match ch {
            '<' => open_stack.push(i),
            '>' => {
                let Some(open) = open_stack.pop() else {
                    continue;
                };
                let inner = &s[open + 1..i];
                if inner.contains(" as ") {
                    // `s[..open]` + whatever follows `<...>`, with a
                    // qualifier's leading `::` also dropped so the result
                    // reads as a plain trailing identifier rather than
                    // `Consumer::Result`.
                    let after = &s[i + 1..];
                    let tail = after.strip_prefix("::").unwrap_or(after);
                    return Some(format!("{}{}", &s[..open], tail));
                }
            }
            _ => {}
        }
    }
    None
}

/// **No counter** — the key is purely structural and deterministic.
fn impl_id(ctx: &mut LowerCtx<'_>, imp: Impl) -> RaId {
    let module = imp.module(ctx.db);
    let mod_key = ctx
        .canonical(ModuleDef::Module(module))
        .unwrap_or_else(|| smol_str::SmolStr::from("<anon>"));

    // Render the full self type including generic args.
    let self_ty_str = attach_db(ctx.db, || {
        imp.self_ty(ctx.db).display(ctx.db, ctx.display).to_string()
    });

    // Retrieve the AST for generic params + where clause (used for uniqueness
    // between impls that share a self type and trait but differ in bounds).
    let impl_ast = ctx
        .sema
        .source(imp)
        .or_else(|| imp.source(ctx.db))
        .map(|s| s.value);

    // Normalise-and-join generic params + where clause into a single suffix.
    // Whitespace is collapsed so minor formatting differences do not cause
    // false collisions.
    let bounds_suffix: String = {
        let generic_params = impl_ast
            .as_ref()
            .and_then(|ast| ast.generic_param_list())
            .map(|gp| gp.syntax().text().to_string())
            .unwrap_or_default();
        let where_clause = impl_ast
            .as_ref()
            .and_then(|ast| ast.where_clause())
            .map(|wc| wc.syntax().text().to_string())
            .unwrap_or_default();
        let raw = format!("{generic_params}{where_clause}");
        // Collapse whitespace/newlines to single spaces for a stable key.
        raw.split_whitespace().collect::<Vec<_>>().join(" ")
    };

    // Tertiary tiebreaker: sorted assoc item names.
    //
    // When two inherent impls in the same module share identical generic params
    // and where clauses (e.g. two `impl<H, T, S> HandlerService<H, T, S>` blocks
    // with no where clause, or two `impl EventDataWriter` blocks), there is no
    // semantic key derived from the impl *signature* that can distinguish them.
    // In that case we sort the names of the impl's assoc items and append them.
    //
    // Rust forbids two items with the same name in the same type-namespace impl
    // block, so the item sets of two impls of the same type are always disjoint
    // (method named `foo` can appear in at most one inherent impl of `T`).
    // Therefore: if the item sets differ, the sorted name list differs → unique.
    //
    // This key is purely semantic: it changes only when items are added, renamed,
    // or removed, all of which are genuine changes to the impl block.  It does
    // NOT churn when lines are added above the block, unlike a position-based
    // fallback.
    //
    // Degenerate edge case: two `#[cfg]`-gated impls that are syntactically
    // identical (same generics, same where clause, same item names).  If that
    // occurs, `check_unique` will fire and the position-based fallback described
    // in the design doc should be added at that time.
    let items_names: String = {
        let mut names: Vec<String> = imp
            .items(ctx.db)
            .into_iter()
            .filter_map(|item| match item {
                AssocItem::Function(f) => Some(f.name(ctx.db).as_str().to_owned()),
                AssocItem::TypeAlias(ta) => Some(ta.name(ctx.db).as_str().to_owned()),
                AssocItem::Const(c) => c.name(ctx.db).map(|n| n.as_str().to_owned()),
            })
            .collect();
        names.sort();
        names.join(",")
    };

    // Build the where/bounds/items suffix: bounds take priority; items are
    // appended only when bounds alone may not be sufficient.
    let where_part = if bounds_suffix.is_empty() {
        if items_names.is_empty() {
            String::new()
        } else {
            format!("[{items_names}]")
        }
    } else {
        format!(" {bounds_suffix}[{items_names}]")
    };

    // Build the impl id based on whether this is a trait impl or inherent.
    let id_str = if let Some(trait_) = imp.trait_(ctx.db) {
        // Trait impl: `{module}::<{self_ty} as {canonical_trait}{args}>{where}`
        //
        // Use the canonical path of the trait (always fully qualified) rather
        // than trait_ref.display() (which shortens to the local name and
        // causes collisions between same-named traits from different crates).
        let canonical_trait = ctx
            .canonical(ModuleDef::Trait(trait_))
            .map(|s| s.to_string())
            .unwrap_or_else(|| {
                // Fallback: render via trait_ref.display() if canonical fails.
                // This should be rare (e.g. traits from macro-generated code).
                attach_db(ctx.db, || {
                    imp.trait_ref(ctx.db)
                        .map(|tr| tr.display(ctx.db, ctx.display).to_string())
                        .unwrap_or_else(|| "<unknown>".to_owned())
                })
            });

        // Extract generic args from trait_ref.display() — the `<…>` portion.
        // For `From<axum::extract::InvalidUtf8>` → `<axum::extract::InvalidUtf8>`.
        // For bare `Error` (no generics) → `""`.
        let trait_args: String = attach_db(ctx.db, || {
            imp.trait_ref(ctx.db)
                .map(|tr| {
                    let display = tr.display(ctx.db, ctx.display).to_string();
                    // Find the first `<` and take everything from there.
                    display
                        .find('<')
                        .map(|lt| display[lt..].to_owned())
                        .unwrap_or_default()
                })
                .unwrap_or_default()
        });

        format!("{mod_key}::<{self_ty_str} as {canonical_trait}{trait_args}>{where_part}")
    } else {
        // Inherent impl: `{module}::<{self_ty}>{where}`
        format!("{mod_key}::<{self_ty_str}>{where_part}")
    };

    RaId::from(id_str.as_str())
}

// ── Type alias ────────────────────────────────────────────────────────────────

/// Shared logic for lowering a type alias given an already-computed id.
fn lower_type_alias_with_id(
    ctx: &mut LowerCtx<'_>,
    ta: ra_ap_hir::TypeAlias,
    ta_id: RaId,
    parent: Option<RaId>,
    out: &mut Lowering<RaId>,
) -> Result<(), Error> {
    let def = ModuleDef::TypeAlias(ta);
    let mut ref_for = make_ref_for!(ctx, out);

    let src = ctx.sema.source(ta).or_else(|| ta.source(ctx.db));

    let (generics, wheres) = src
        .as_ref()
        .map(|s| {
            generics::lower_generics(
                ctx,
                s.value.generic_param_list(),
                s.value.where_clause(),
                &mut ref_for,
            )
        })
        .unwrap_or_default();

    let target = if let Some(src) = &src
        && let Some(ty_node) = src.value.ty()
    {
        Some(ty::lower_ast_type(ctx, &ty_node, &mut ref_for))
    } else {
        let hir_ty = attach_db(ctx.db, || ta.ty(ctx.db));
        let lowered = ty::lower_hir_type_fallback(ctx, &hir_ty, &mut ref_for);
        Some(lowered)
    };

    // Bounds on the assoc type (`type Item: Display;`).
    let bounds: Vec<nudox_ir::kinds::Type> = src
        .as_ref()
        .and_then(|s| s.value.type_bound_list())
        .map(|list| {
            list.bounds()
                .filter_map(|b| match b.kind() {
                    Some(ra_ap_syntax::ast::TypeBoundKind::PathType(_, path_ty)) => {
                        let path = path_ty.path()?;
                        let res = ty::resolve_path_opt(ctx, &path)?;
                        if let ra_ap_hir::PathResolution::Def(def) = res {
                            let key = id_of(ctx, def)?;
                            let raw = ref_for(&key)?;
                            Some(nudox_ir::kinds::Type::Nominal(raw))
                        } else {
                            None
                        }
                    }
                    _ => None,
                })
                .collect()
        })
        .unwrap_or_default();

    let alias_body = Alias::builder()
        // Was `!matches!(t, Type::Any)`, which discarded the alias target for
        // *every* foreign or unresolvable RHS — `type R = io::Result<()>` kept
        // nothing at all. Post-CC-2 those carry their spelling
        // (`UnresolvedExternal`), so only the genuinely contentless gap is
        // dropped: `Unknown(OracleGap)` says no more than `None` does.
        .maybe_target(target.filter(|t| {
            !matches!(
                t,
                nudox_ir::kinds::Type::Unknown(nudox_ir::kinds::UnknownType::OracleGap)
            )
        }))
        .generics(generics)
        .wheres(wheres)
        .bounds(bounds)
        .build();

    let location = source::def_location(ctx, def);
    let parts = ctx.symbol_parts(def).unwrap_or_else(default_parts);
    let sym = parts.into_symbol(&location);

    ctx.check_unique(&ta_id);
    out.declare_at(ta_id, parent, sym, alias_body, location);
    Ok(())
}

/// Lower a module-level type alias (type namespace, no `!v` / `!m` tag).
fn lower_type_alias(
    ctx: &mut LowerCtx<'_>,
    def: ModuleDef,
    parent: Option<RaId>,
    out: &mut Lowering<RaId>,
) -> Result<(), Error> {
    let ModuleDef::TypeAlias(ta) = def else {
        return Ok(());
    };
    let Some(ta_id) = ctx.ra_id(def) else {
        return Ok(());
    };
    lower_type_alias_with_id(ctx, ta, ta_id, parent, out)
}

/// Lower an assoc type alias inside an impl/trait block.
///
/// The id is `child_id(parent_impl_or_trait_id, name)`, which is unique because
/// the parent id encodes the self type and optional trait.
fn lower_assoc_type_alias(
    ctx: &mut LowerCtx<'_>,
    ta: ra_ap_hir::TypeAlias,
    parent_id: &RaId,
    parent: Option<RaId>,
    out: &mut Lowering<RaId>,
) -> Result<(), Error> {
    let ta_name = ta.name(ctx.db).as_str().to_owned();
    let ta_id = ctx.child_id(parent_id, &ta_name);
    lower_type_alias_with_id(ctx, ta, ta_id, parent, out)
}

// ── Const ─────────────────────────────────────────────────────────────────────

/// Shared logic for lowering a const given an already-computed id.
fn lower_const_with_id(
    ctx: &mut LowerCtx<'_>,
    c: ra_ap_hir::Const,
    const_id: RaId,
    parent: Option<RaId>,
    out: &mut Lowering<RaId>,
) -> Result<(), Error> {
    let def = ModuleDef::Const(c);
    let mut ref_for = make_ref_for!(ctx, out);

    let src = ctx.sema.source(c).or_else(|| c.source(ctx.db));
    let const_ty = if let Some(src) = &src
        && let Some(ty_node) = src.value.ty()
    {
        ty::lower_ast_type(ctx, &ty_node, &mut ref_for)
    } else {
        let hir_ty = attach_db(ctx.db, || c.ty(ctx.db));
        ty::lower_hir_type_fallback(ctx, &hir_ty, &mut ref_for)
    };

    // Constant value: prefer the evaluated representation when the RA trait
    // solver can produce one; otherwise fall back to the source text.
    //
    // `Const::eval` → `EvaluatedConst::render` gives the evaluated numeric
    // string (e.g. `42 (0x2A)` for integer consts).  The IR field `value` is a
    // `String` so we store this directly.
    //
    // Note on IR-side ConstExpr: a structured `ConstExpr` (Int/Float/BinOp/…)
    // would require changing `nudox_ir::kinds::Const::value` from `Option<String>`
    // to a sum type, plus visitor/serde derivations, plus a builder update.  The
    // necessary IR-side signature would be:
    //   ```
    //   pub enum ConstExpr {
    //       Int(i128),
    //       Uint(u128),
    //       Float(f64),
    //       Bool(bool),
    //       Str(String),
    //       BinOp { op: BinOp, lhs: Box<ConstExpr>, rhs: Box<ConstExpr> },
    //       UnaryOp { op: UnaryOp, operand: Box<ConstExpr> },
    //       Path(String),       // const reference by canonical path
    //       Unknown(String),    // source text fallback
    //   }
    //   ```
    let value: Option<String> = {
        // Try evaluated value first (panic-safe).
        let evaluated = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            c.eval(ctx.db).ok().map(|ev| ev.render(ctx.db, ctx.display))
        }));
        match evaluated {
            Ok(Some(rendered)) => Some(rendered),
            _ => {
                // Fall back to source text.
                src.as_ref()
                    .and_then(|s| s.value.body())
                    .map(|e| e.syntax().text().to_string())
            }
        }
    };

    let const_body = Const::builder()
        .ty(const_ty.clone())
        .maybe_value(value.map(|source| {
            nudox_ir::build::ConstExpr::builder()
                .ty(const_ty)
                .source(source)
                .build()
        }))
        .build();

    let location = source::def_location(ctx, def);
    let parts = ctx.symbol_parts(def).unwrap_or_else(default_parts);
    let sym = parts.into_symbol(&location);

    ctx.check_unique(&const_id);
    out.declare_at(const_id, parent, sym, const_body, location);
    Ok(())
}

/// Lower an assoc const inside an impl/trait block.
///
/// The id is `child_id(parent_impl_or_trait_id, name)`.
fn lower_assoc_const(
    ctx: &mut LowerCtx<'_>,
    c: ra_ap_hir::Const,
    parent_id: &RaId,
    parent: Option<RaId>,
    out: &mut Lowering<RaId>,
) -> Result<(), Error> {
    // Const::name returns Option<Name> (anonymous consts in patterns use `_`).
    let const_name = c
        .name(ctx.db)
        .map(|n| n.as_str().to_owned())
        .unwrap_or_else(|| "_".to_owned());
    let const_id = ctx.child_id(parent_id, &const_name);
    lower_const_with_id(ctx, c, const_id, parent, out)
}

// ── Static ────────────────────────────────────────────────────────────────────

/// Shared logic for lowering a static item given an already-computed id.
fn lower_static_with_id(
    ctx: &mut LowerCtx<'_>,
    s: ra_ap_hir::Static,
    static_id: RaId,
    parent: Option<RaId>,
    out: &mut Lowering<RaId>,
) -> Result<(), Error> {
    let def = ModuleDef::Static(s);
    let mut ref_for = make_ref_for!(ctx, out);

    let src = ctx.sema.source(s).or_else(|| s.source(ctx.db));
    let static_ty = if let Some(src) = &src
        && let Some(ty_node) = src.value.ty()
    {
        ty::lower_ast_type(ctx, &ty_node, &mut ref_for)
    } else {
        let hir_ty = attach_db(ctx.db, || s.ty(ctx.db));
        ty::lower_hir_type_fallback(ctx, &hir_ty, &mut ref_for)
    };

    let mutable = s.is_mut(ctx.db);

    let static_body = Static::builder().ty(static_ty).mutable(mutable).build();

    let location = source::def_location(ctx, def);
    let parts = ctx.symbol_parts(def).unwrap_or_else(default_parts);
    let sym = parts.into_symbol(&location);

    ctx.check_unique(&static_id);
    out.declare_at(static_id, parent, sym, static_body, location);
    Ok(())
}

// ── Field helpers ─────────────────────────────────────────────────────────────

/// Intermediate data collected for one field before it is declared into
/// `Lowering`.  Separating the collection pass from the declaration pass
/// avoids the borrow conflict between `ref_for` (which captures `out`) and the
/// `out` parameter itself.
struct FieldData {
    field_id: RaId,
    /// Bare field name (e.g. `"value"`, not `"axum::extract::path::de::EnumDeserializer::value"`).
    name: String,
    key: FieldKey,
    ty: nudox_ir::kinds::Type,
    visibility: nudox_ir::entry::Visibility,
    doc: String,
    doc_links: Vec<nudox_ir::entry::DocLink>,
    cfg: Option<nudox_ir::entry::CfgExpr>,
    attrs: Box<[nudox_ir::entry::AttrTok]>,
    /// Where the field is written.
    ///
    /// Computed in a pass *before* `ref_for` exists, because `ref_for` borrows
    /// `out` and the line-index cache lives on `ctx` — the two cannot be
    /// borrowed mutably at once, and threading the location through the data
    /// struct is cheaper than restructuring the borrow.
    location: SourceLocation,
}

/// Declare a vec of HIR fields as `Field` entries and return their `Ref<Field>`.
///
/// This function creates its own `ref_for` closure internally so the caller
/// does not need to pass one — which would create a double-borrow conflict
/// since `ref_for` itself borrows `out` via the `make_ref_for!` macro.
fn declare_hir_fields(
    ctx: &mut LowerCtx<'_>,
    owner_id: &RaId,
    fields: Vec<ra_ap_hir::Field>,
    form: RecordForm,
    parent: Option<RaId>,
    out: &mut Lowering<RaId>,
) -> Vec<Ref<Field>> {
    // Pass 1: compute all type / metadata while holding `ref_for` (which
    // borrows `out` via the closure).
    // Pass 0: locations, before `ref_for` takes its borrow of `out`.
    let locations: Vec<SourceLocation> = fields
        .iter()
        .map(|f| source::field_location(ctx, *f))
        .collect();

    let data: Vec<FieldData> = {
        let mut ref_for = make_ref_for!(ctx, out);
        fields
            .iter()
            .zip(locations)
            .enumerate()
            .map(|(idx, (f, location))| {
                let field_name = f.name(ctx.db).as_str().to_owned();
                let field_id = RaId::from(format!("{owner_id}::{field_name}").as_str());

                let key = match form {
                    RecordForm::Tuple => FieldKey::Positional(idx),
                    _ => FieldKey::Named,
                };

                let ast_ty = ctx
                    .sema
                    .source(*f)
                    .or_else(|| f.source(ctx.db))
                    .and_then(|src| match src.value {
                        FieldSource::Named(rf) => rf.ty(),
                        FieldSource::Pos(tf) => tf.ty(),
                    });
                let hir_ty = attach_db(ctx.db, || f.ty(ctx.db));
                let field_ty =
                    ty::lower_type_prefer_ast(ctx, ast_ty.as_ref(), &hir_ty, &mut ref_for);

                let visibility = ctx.visibility(*f);
                let doc = docs::documentation(ctx, *f).unwrap_or_default();
                let doc_links = docs::doc_links(ctx, *f, Some(&doc)).unwrap_or_default();
                let cfg = docs::cfg_expr(ctx, *f);
                let attrs = ctx
                    .sema
                    .source(*f)
                    .or_else(|| f.source(ctx.db))
                    .map(|src| match src.value {
                        FieldSource::Named(field) => docs::ast_attrs(field),
                        FieldSource::Pos(field) => docs::ast_attrs(field),
                    })
                    .unwrap_or_default();

                FieldData {
                    field_id,
                    name: field_name,
                    key,
                    ty: field_ty,
                    visibility,
                    doc,
                    doc_links,
                    cfg,
                    attrs,
                    location,
                }
            })
            .collect()
    };

    // Pass 2: declare all fields into `out` (ref_for is gone; out is free).
    data.into_iter()
        .map(|fd| {
            let attrs: Vec<FieldAttribute> = Vec::new();
            let field_body = Field::builder()
                .key(fd.key)
                .ty(fd.ty)
                .attributes(attrs)
                .build();

            let (field_source, field_span) = fd.location.legacy_pair();
            let field_sym = nudox_ir::entry::Symbol {
                name: fd.name,
                visibility: fd.visibility,
                documentation: fd.doc,
                source: field_source,
                span: field_span,
                aliases: Box::new([]),
                deprecation: None,
                doc_links: fd.doc_links.into_boxed_slice(),
                attrs: fd.attrs,
                cfg: fd.cfg,
            };

            ctx.check_unique(&fd.field_id);
            out.declare_at(
                fd.field_id.clone(),
                parent.clone(),
                field_sym,
                field_body,
                fd.location,
            );
            out.refer::<Field>(fd.field_id)
        })
        .collect()
}

// ── Param declaration helper ──────────────────────────────────────────────────

/// Declare all input params and return their `Ref<Param>` list.
///
/// Parented on `fn_id` — the owning function — rather than the function's
/// enclosing module/impl. The seal pass
/// (`workspace/ir/model/src/package/seal.rs`) computes a param's collision
/// key from `(kind, ancestor-path, leaf-name)`, walking declared-parent
/// pointers; a param parented on the module produces an ancestor-path with no
/// trace of which function declared it, so same-named params of sibling
/// functions in one module are indistinguishable at that key and fall through
/// to `Disambiguator::Span` — which is *also* useless here because every
/// producer-synthesized param (`plain_sym`) is
/// `SourceLocation::Unlocated(Synthesized)` and therefore has no span at all.
/// Parenting on
/// `fn_id` puts the function's name in the ancestor-path itself, so siblings'
/// params are distinct by construction and never need the escalation
/// backstop (`Disambiguator::Ordinal`) to be told apart.
pub(crate) fn declare_params(
    ctx: &mut LowerCtx<'_>,
    fn_id: &RaId,
    params: &[function::ParamData],
    out: &mut Lowering<RaId>,
) -> Vec<Ref<Param>> {
    params
        .iter()
        .map(|pd| {
            let param_id = RaId::from(format!("{fn_id}::param::{}", pd.name).as_str());
            let param_body = Param::builder()
                .maybe_ty(pd.ty.clone())
                .attributes(pd.attributes.iter().copied())
                .build();
            let param_sym = plain_sym(&pd.name);
            ctx.check_unique(&param_id);
            out.declare_at(
                param_id.clone(),
                Some(fn_id.clone()),
                param_sym,
                param_body,
                SourceLocation::Unlocated(Unlocated::Synthesized),
            );
            out.refer::<Param>(param_id)
        })
        .collect()
}

// ── Auto-trait probing ────────────────────────────────────────────────────────

/// Probe the two auto-traits that are resolvable via lang items in ra_ap 0.0.341.
///
/// ## What IS reachable
///
/// Only `LangItem::Sync` and `LangItem::Unpin` are registered as lang items in
/// `ra_ap_hir_def` at this version.  Both are accessible via
/// `Trait::lang(db, krate, LangItem::Sync / Unpin)` and then probed with
/// `hir_ty.impls_trait(db, trait_, &[])`.
///
/// ## What is NOT reachable
///
/// `Send`, `UnwindSafe`, and `RefUnwindSafe` are **not** lang items in
/// `ra_ap_hir_def-0.0.341`.  Tried: `LangItem::Send` — does not exist on the
/// `LangItemEnum` type; checked `hir_def/src/lang_item.rs` directly (the macro
/// table lists `Sync` at line 454 and `Unpin` at line 536; `Send` is absent).
/// `AttrsWithOwner` has no `is_send()` / `is_sync()` predicates either.
/// To query those three traits would require walking the `std::marker` module
/// by name from a std/core `Crate` dependency, which is fragile and not
/// attempted here.
fn probe_auto_traits_partial<'db>(
    ctx: &LowerCtx<'_>,
    hir_ty: &ra_ap_hir::Type<'db>,
) -> Vec<AutoFact> {
    use nudox_ir::kinds::facts::{AutoState, AutoTrait};

    let krate = ctx.krate;
    let mut facts = Vec::new();

    // Helper: map impls_trait result → AutoState.
    // `impls_trait` returns `true` when the trait is unconditionally implemented.
    // We cannot distinguish "conditional" from "not implemented" here — a `false`
    // result means "ra does not confirm unconditional impl", which could be either
    // `No` or `Cond`.  We conservatively emit `No`; a fuller solver pass could
    // distinguish these using `has_any_impl`.
    let probe = |trait_opt: Option<Trait>| -> Option<AutoState> {
        let t = trait_opt?;
        if hir_ty.impls_trait(ctx.db, t, &[]) {
            Some(AutoState::Yes)
        } else {
            // Cannot tell No from Cond at this API level — emit No.
            Some(AutoState::No)
        }
    };

    if let Some(state) = probe(Trait::lang(ctx.db, krate, LangItem::Sync)) {
        facts.push(AutoFact {
            trait_: AutoTrait::Sync,
            state,
        });
    }
    if let Some(state) = probe(Trait::lang(ctx.db, krate, LangItem::Unpin)) {
        facts.push(AutoFact {
            trait_: AutoTrait::Unpin,
            state,
        });
    }

    facts
}

// ── Convenience helpers ───────────────────────────────────────────────────────

/// A symbol for an entry this producer invents: parameters, `$return`
/// pseudo-params, desugared receivers.
///
/// Its location is [`Unlocated::Synthesized`] — not a gap to be closed later,
/// but the correct answer: no file contains a declaration of `$return`.
fn plain_sym(name: &str) -> nudox_ir::entry::Symbol {
    let (source, span) = SourceLocation::Unlocated(Unlocated::Synthesized).legacy_pair();
    nudox_ir::entry::Symbol {
        name: name.to_owned(),
        visibility: nudox_ir::entry::Visibility::Private,
        documentation: String::new(),
        source,
        span,
        aliases: Box::new([]),
        deprecation: None,
        doc_links: Box::new([]),
        attrs: Box::new([]),
        cfg: None,
    }
}

fn default_parts() -> super::ctx::SymbolParts {
    super::ctx::SymbolParts {
        name: "<unknown>".to_owned(),
        visibility: nudox_ir::entry::Visibility::Private,
        documentation: String::new(),
        aliases: Vec::new(),
        deprecation: None,
        doc_links: Vec::new(),
        cfg: None,
        attrs: Box::new([]),
    }
}

// ── Unit tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::{scope_only_reexport_location, strip_qualified_path_qualifiers};
    use nudox_ir::{
        change::{EcosystemId, PackageLineageId, PackageName},
        entry::{EntryInner, SourceLocation, Unlocated},
        kinds::{Field, FieldKey, Module as IrModule, Record, RecordForm, Reexport, Type},
        lower::Lowering,
        package::PackageId,
    };

    #[test]
    fn scope_only_reexport_location_is_explicitly_synthesized() {
        assert_eq!(
            scope_only_reexport_location(),
            SourceLocation::Unlocated(Unlocated::Synthesized)
        );
    }

    fn sym(name: &str) -> nudox_ir::entry::Symbol {
        nudox_ir::entry::Symbol {
            name: name.to_owned(),
            visibility: nudox_ir::entry::Visibility::Public,
            documentation: String::new(),
            source: std::path::PathBuf::new(),
            span: 0..0,
            aliases: Box::new([]),
            deprecation: None,
            doc_links: Box::new([]),
            attrs: Box::new([]),
            cfg: None,
        }
    }

    fn lineage() -> PackageLineageId {
        PackageLineageId::new(EcosystemId::new("cargo"), PackageName::new("test-producer"))
    }

    // ── Fix 1: Union lowers to RecordForm::Union ──────────────────────────────

    /// A `Record` built with `RecordForm::Union` must be preserved end-to-end
    /// through `Lowering` + seal.  This verifies the IR slot accepts
    /// `Union` and that the lowering path produces the correct form.
    #[test]
    fn union_lowers_to_record_form_union() {
        let mut low: Lowering<usize> = Lowering::new(PackageId::path("pkg"), sym("root"));

        // Declare a field.
        let f: nudox_ir::index::Ref<Field> = low.declare(
            2,
            Some(1),
            sym("data"),
            Field::builder().key(FieldKey::Named).ty(Type::I32).build(),
        );

        // Declare a union record using RecordForm::Union.
        low.declare(
            1,
            None,
            sym("MyUnion"),
            Record::builder()
                .form(RecordForm::Union)
                .fields([f])
                .build(),
        );

        let pkg = low.finish().expect("finish must succeed");
        let sealed = pkg.seal(&lineage(), &nudox_ir::foreign::Unlinked).table;

        // Find MyUnion entry.
        let found = sealed.iter().find(|(_, e)| e.sym().name == "MyUnion");
        let (_, union_entry) = found.expect("MyUnion must exist after sealing");

        // Verify the kind is Record with Union form via downcast.
        let typed = union_entry
            .downcast::<Record>()
            .expect("MyUnion must be an owned Record entry");
        assert_eq!(
            typed.body().form,
            RecordForm::Union,
            "union must have RecordForm::Union, not Struct"
        );
    }

    // ── Fix 2: pub use → Reexport entry ──────────────────────────────────────

    /// A `declare_ref` entry must appear as a `Reference` in the package tree.
    /// This mirrors exactly what `lower_module` emits for a `pub use` re-export.
    #[test]
    fn reexport_produces_reference_entry() {
        let mut low: Lowering<usize> = Lowering::new(PackageId::path("pkg"), sym("root"));

        // Original module entry.
        let orig: nudox_ir::index::Ref<IrModule> =
            low.declare(1, None, sym("inner_module"), IrModule);

        // Re-export under a different name (mirrors what lower_module emits).
        let reexport_ref: nudox_ir::index::Ref<Reexport> = {
            let raw = orig.into_raw();
            match raw.as_local() {
                Some(idx) => nudox_ir::index::Ref::Local(idx.typed()),
                None => panic!("must be Local during build"),
            }
        };
        low.declare_ref::<Reexport>(2, None, sym("pub_alias"), reexport_ref);

        let pkg = low.finish().expect("finish must succeed");
        let sealed = pkg.seal(&lineage(), &nudox_ir::foreign::Unlinked).table;

        let alias = sealed
            .iter()
            .find(|(_, e)| e.sym().name == "pub_alias")
            .expect("pub_alias must exist");

        assert!(
            matches!(alias.1.kind(), EntryInner::Reference(_)),
            "pub_alias must be a Reference entry (re-export)"
        );
    }

    // ── Fix 3: supertrait generic args → Type::Apply ──────────────────────────

    /// `Type::Apply { base, args }` is the shape for a parameterised bound like
    /// `Bar<u32>`.  This test verifies the IR can hold such a type and that
    /// serde round-trip preserves the argument.
    #[test]
    fn supertrait_apply_type_roundtrips() {
        // Build `Bar<u32>` manually — what the AST path produces for
        // `trait Foo: Bar<u32>` when generic args are recovered.
        let mut low: Lowering<usize> = Lowering::new(PackageId::path("pkg"), sym("root"));

        // Declare Bar so the Ref is valid.
        let bar_ref: nudox_ir::index::Ref<IrModule> = low.declare(10, None, sym("Bar"), IrModule);

        let bar_nominal = Type::Nominal(bar_ref.into_raw());
        let bar_applied = Type::Apply {
            base: Box::new(bar_nominal),
            args: Box::new([Type::U32]),
        };

        // Verify serde round-trip preserves the Apply shape.
        let json = serde_json::to_string(&bar_applied).expect("serialize");
        let rt: Type = serde_json::from_str(&json).expect("deserialize");

        match rt {
            Type::Apply { args, .. } => {
                assert_eq!(args.len(), 1, "must have one type arg");
                assert_eq!(args[0], Type::U32, "arg must be u32");
            }
            other => panic!("expected Apply, got {other:?}"),
        }
    }

    // ── Fix 4: qualified-path syntax never survives into an impl's name ──────
    //
    // Found lowering `rayon-1.9.0` for real: `impl_display_name` produced
    // `impl Folder<T> for FlattenFolder<C, <C as
    // Consumer<<T as IntoParallelIterator>::Item>>::Result>`, which fails the
    // corpus harness's "names are identifiers" invariant
    // (`crates/nudox-store/tests/real_package.rs`).

    /// A single, non-nested qualified path collapses to its trailing segment.
    #[test]
    fn strip_qualified_path_qualifiers_collapses_a_single_qualifier() {
        assert_eq!(
            strip_qualified_path_qualifiers(
                "FlattenFolder<T, <T as IntoParallelIterator>::Item>".to_owned()
            ),
            "FlattenFolder<T, Item>",
        );
    }

    /// The exact nested shape rayon 1.9.0 produced must fully collapse — both
    /// qualifiers, not just the innermost one.
    #[test]
    fn strip_qualified_path_qualifiers_handles_nesting() {
        assert_eq!(
            strip_qualified_path_qualifiers(
                "impl Folder<T> for FlattenFolder<C, \
                 <C as Consumer<<T as IntoParallelIterator>::Item>>::Result>"
                    .to_owned()
            ),
            "impl Folder<T> for FlattenFolder<C, Result>",
        );
    }

    /// A plain generic type with no qualified path is returned unchanged
    /// (this is the overwhelming common case and must stay a no-op).
    #[test]
    fn strip_qualified_path_qualifiers_is_a_no_op_without_a_qualifier() {
        let plain = "HashMap<K, V>".to_owned();
        assert_eq!(strip_qualified_path_qualifiers(plain.clone()), plain);
    }

    /// `Fn`-trait arrow sugar (`Fn(i32) -> bool`) puts a bare, unmatched `>`
    /// in the string with no preceding `<`. That must not abort the scan
    /// before it reaches a real qualifier later in the same string.
    #[test]
    fn strip_qualified_path_qualifiers_tolerates_fn_trait_arrows() {
        assert_eq!(
            strip_qualified_path_qualifiers(
                "impl Foo<dyn Fn(i32) -> bool> for <T as Bar>::Baz".to_owned()
            ),
            "impl Foo<dyn Fn(i32) -> bool> for Baz",
        );
    }
}
