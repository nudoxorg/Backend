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

use std::path::PathBuf;

use ra_ap_hir::{
    Adt, AsAssocItem, AssocItem, AssocItemContainer, FieldSource, HasSource, HasVisibility,
    HirDisplay, Impl, LangItem, Module, ModuleDef, ScopeDef, Struct, StructKind, Trait, attach_db,
};
use ra_ap_syntax::ast::HasTypeBounds;
use ra_ap_syntax::{AstNode, ast::HasGenericParams};

use nudox_ir::{
    id::UniqueId,
    index::Ref,
    kinds::{
        Alias, AutoFact, Const, Enum, Field, FieldAttribute, FieldKey,
        Function, Impl as IrImpl, ImplFlags, Module as IrModule, Param, Record, RecordForm,
        Reexport, Static, Trait as IrTrait, TraitFlags, TriState, Variant, VariantForm,
    },
    lower::Lowering,
    package::PackageId,
};

use crate::RaId;
use crate::error::RustProducerError;
use super::{
    ctx::LowerCtx,
    docs, function, generics, source, ty,
};

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
        let _lowering_reborrow = &mut *$lowering;
        move |key: &crate::ra::ctx::PathKey| -> Option<nudox_ir::index::RawRef> {
            // Decide whether `key` belongs to the local crate.
            // A key is local iff it starts with `{local_crate}` followed by
            // either end-of-string (the crate root) or `::`.
            let n = local_crate.len();
            let is_local = key.starts_with(local_crate.as_str())
                && (key.len() == n
                    || key.as_bytes().get(n) == Some(&b':'));

            if is_local {
                let typed: Ref<IrModule> = _lowering_reborrow.refer(key.clone());
                Some(typed.into_raw())
            } else {
                // Foreign package: use the first `::` segment as the crate
                // name and synthesise a `PackageId` from it.
                let foreign_crate = key.split("::").next().unwrap_or("_");
                let pkg_id = PackageId::path(foreign_crate);
                let uid = UniqueId::new(pkg_id, key.clone());
                let typed: Ref<IrModule> = _lowering_reborrow.refer_import(uid);
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
) -> Result<(), RustProducerError> {
    let def = ModuleDef::Module(module);
    let Some(mod_id) = ctx.ra_id(def) else {
        return Ok(());
    };
    let parts = ctx.symbol_parts(def).unwrap_or_else(|| super::ctx::SymbolParts {
        name: "<anon>".to_owned(),
        visibility: nudox_ir::entry::Visibility::Private,
        documentation: String::new(),
        aliases: Vec::new(),
        deprecation: None,
        doc_links: Vec::new(),
        cfg: None,
        attrs: Box::new([]),
    });
    let sym = parts.into_symbol(PathBuf::new(), 0..0);
    ctx.check_unique(&mod_id);
    out.declare(mod_id.clone(), parent, sym, IrModule);

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

    // Collect (scope_name, child_def) pairs for re-exports, to avoid holding
    // an immutable borrow on `ctx` while mutating `out`.
    let scope_items: Vec<(smol_str::SmolStr, ModuleDef)> = module
        .scope(ctx.db, None)
        .into_iter()
        .filter_map(|(name, scope_def)| {
            let ScopeDef::ModuleDef(child) = scope_def else {
                return None;
            };
            // Only public re-exports.
            if !ctx.document_private
                && !matches!(child.visibility(ctx.db), ra_ap_hir::Visibility::Public)
            {
                return None;
            }
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
                let foreign_crate = tagged_key.split("::").next().unwrap_or("_");
                let pkg_id = PackageId::path(foreign_crate);
                let uid = UniqueId::new(pkg_id, tagged_key.clone());
                out.refer_import::<IrModule>(uid).into_raw()
            };
            match raw.as_local() {
                Some(untyped_idx) => nudox_ir::index::Ref::Local(untyped_idx.typed()),
                // Import refs also come back as Local in the import index.
                None => continue,
            }
        };

        let reexport_sym = nudox_ir::entry::Symbol {
            name: scope_name.to_string(),
            visibility: nudox_ir::entry::Visibility::Public,
            documentation: String::new(),
            source: PathBuf::new(),
            span: 0..0,
            aliases: Box::new([]),
            deprecation: None,
            doc_links: Box::new([]),
            attrs: Box::new([]),
            cfg: None,
        };

        ctx.check_unique(&reexport_id);
        out.declare_ref::<Reexport>(
            reexport_id,
            Some(mod_id.clone()),
            reexport_sym,
            target_ref,
        );
    }

    Ok(())
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
) -> Result<(), RustProducerError> {
    if matches!(def, ModuleDef::Module(_) | ModuleDef::EnumVariant(_)) {
        return Ok(());
    }

    match def {
        ModuleDef::Function(f) => {
            // Value namespace: tag with `!v` so `fn serve` does not collide
            // with `mod serve` or `struct Serve`.
            let fn_name = f.name(ctx.db).as_str().to_owned();
            let fn_id = parent
                .as_ref()
                .map(|p| ctx.child_id(p, &format!("{fn_name}!v")))
                .unwrap_or_else(|| ctx.ra_id(def).unwrap_or_else(|| RaId::from(fn_name.as_str())));
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
            // Value namespace: tag with `!v`.
            let const_name = name.as_str().to_owned();
            let const_id = parent
                .as_ref()
                .map(|p| ctx.child_id(p, &format!("{const_name}!v")))
                .unwrap_or_else(|| ctx.ra_id(def).unwrap_or_else(|| RaId::from(const_name.as_str())));
            lower_const_with_id(ctx, c, const_id, parent, out)?;
        }
        ModuleDef::Static(s) => {
            // Value namespace: tag with `!v`.
            let static_name = s.name(ctx.db).as_str().to_owned();
            let static_id = parent
                .as_ref()
                .map(|p| ctx.child_id(p, &format!("{static_name}!v")))
                .unwrap_or_else(|| ctx.ra_id(def).unwrap_or_else(|| RaId::from(static_name.as_str())));
            lower_static_with_id(ctx, s, static_id, parent, out)?;
        }
        ModuleDef::Macro(m) => {
            // Macro namespace: tag with `!m`.  No Macro kind in new IR — emit as Module.
            let macro_name = m.name(ctx.db).as_str().to_owned();
            let macro_id = parent
                .as_ref()
                .map(|p| ctx.child_id(p, &format!("{macro_name}!m")))
                .unwrap_or_else(|| ctx.ra_id(ModuleDef::Macro(m)).unwrap_or_else(|| RaId::from(macro_name.as_str())));
            let parts = ctx.symbol_parts(ModuleDef::Macro(m)).unwrap_or_else(default_parts);
            ctx.check_unique(&macro_id);
            out.declare(macro_id, parent, parts.into_symbol(PathBuf::new(), 0..0), IrModule);
        }
        ModuleDef::BuiltinType(b) => {
            // Emit as Alias with no target.
            let id = RaId::from(b.name().as_str());
            let sym = nudox_ir::entry::Symbol {
                name: b.name().as_str().to_owned(),
                visibility: nudox_ir::entry::Visibility::Public,
                documentation: String::new(),
                source: PathBuf::new(),
                span: 0..0,
                aliases: Box::new([]),
                deprecation: None,
                doc_links: Box::new([]),
                attrs: Box::new([]),
                cfg: None,
            };
            ctx.check_unique(&id);
            out.declare(id, parent, sym, Alias::builder().maybe_target(None).build());
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
    f: ra_ap_hir::Function,
    fn_id: RaId,
    parent: Option<RaId>,
    out: &mut Lowering<RaId>,
) -> Result<(), RustProducerError> {
    let def = ModuleDef::Function(f);
    let mut ref_for = make_ref_for!(ctx, out);
    let Some(fd) = function::lower_function(ctx, f, &mut ref_for) else {
        return Ok(());
    };

    // Declare Param entries first, then build Refs for the Function body.
    let input_refs = declare_params(ctx, &fn_id, &fd.input_params, parent.clone(), out);
    let output_refs = fd
        .output_param
        .as_ref()
        .map(|pd| {
            let param_id = RaId::from(format!("{fn_id}::param::return").as_str());
            let param_sym = plain_sym(&pd.name);
            let param_body = Param::builder()
                .maybe_ty(pd.ty.clone())
                .attributes(pd.attributes.iter().copied())
                .build();
            ctx.check_unique(&param_id);
            out.declare(param_id.clone(), parent.clone(), param_sym, param_body);
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

    let (src_path, span) = source::fn_source_range(ctx, f)
        .unwrap_or_else(|| (PathBuf::new(), 0..0));
    let parts = ctx.symbol_parts(def).unwrap_or_else(default_parts);
    let sym = parts.into_symbol(src_path, span);

    ctx.check_unique(&fn_id);
    out.declare(fn_id, parent, sym, fn_body);
    Ok(())
}

/// Lower an impl/trait assoc function where the id is derived from the parent
/// impl/trait id via `child_id`.
///
/// Called from `lower_impl` and `lower_trait_assoc_items`.  The `parent` here
/// is the impl's or trait's `RaId`, which already encodes the self type and
/// optional trait, so the child name alone is sufficient disambiguation.
fn lower_assoc_function(
    ctx: &mut LowerCtx<'_>,
    f: ra_ap_hir::Function,
    parent_id: &RaId,
    parent: Option<RaId>,
    out: &mut Lowering<RaId>,
) -> Result<(), RustProducerError> {
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
) -> Result<(), RustProducerError> {
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

    let (src_path, span) = source::def_source_range(ctx, def);
    let parts = ctx.symbol_parts(def).unwrap_or_else(default_parts);
    let sym = parts.into_symbol(src_path, span);

    ctx.check_unique(&struct_id);
    out.declare(struct_id, parent, sym, record_body);
    Ok(())
}

// ── Enum ──────────────────────────────────────────────────────────────────────

fn lower_enum(
    ctx: &mut LowerCtx<'_>,
    e: ra_ap_hir::Enum,
    parent: Option<RaId>,
    out: &mut Lowering<RaId>,
) -> Result<(), RustProducerError> {
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
                    match form { VariantForm::Tuple => RecordForm::Tuple, _ => RecordForm::Struct },
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
            let variant_sym = nudox_ir::entry::Symbol {
                name: variant_name,
                visibility: nudox_ir::entry::Visibility::Public,
                documentation: doc.unwrap_or_default(),
                source: PathBuf::new(),
                span: 0..0,
                aliases: Box::new([]),
                deprecation: None,
                doc_links: Box::new([]),
                attrs: Box::new([]),
                cfg: None,
            };

            ctx.check_unique(&variant_id);
            out.declare(variant_id.clone(), Some(enum_id.clone()), variant_sym, variant_body);
            out.refer::<Variant>(variant_id)
        })
        .collect();

    let enum_body = Enum::builder()
        .variants(variant_refs)
        .generics(generics)
        .wheres(wheres)
        .auto(auto)
        .build();

    let (src_path, span) = source::def_source_range(ctx, def);
    let parts = ctx.symbol_parts(def).unwrap_or_else(default_parts);
    let sym = parts.into_symbol(src_path, span);

    ctx.check_unique(&enum_id);
    out.declare(enum_id, parent, sym, enum_body);
    Ok(())
}

// ── Union ─────────────────────────────────────────────────────────────────────

fn lower_union(
    ctx: &mut LowerCtx<'_>,
    u: ra_ap_hir::Union,
    parent: Option<RaId>,
    out: &mut Lowering<RaId>,
) -> Result<(), RustProducerError> {
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

    let (src_path, span) = source::def_source_range(ctx, def);
    let parts = ctx.symbol_parts(def).unwrap_or_else(default_parts);
    let sym = parts.into_symbol(src_path, span);

    ctx.check_unique(&union_id);
    out.declare(union_id, parent, sym, record_body);
    Ok(())
}

// ── Trait ─────────────────────────────────────────────────────────────────────

fn lower_trait(
    ctx: &mut LowerCtx<'_>,
    t: Trait,
    parent: Option<RaId>,
    out: &mut Lowering<RaId>,
) -> Result<(), RustProducerError> {
    let def = ModuleDef::Trait(t);
    let Some(trait_id) = ctx.ra_id(def) else {
        return Ok(());
    };
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
        let ast_bounds = trait_ast
            .as_ref()
            .and_then(|ast| ast.type_bound_list());

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

    // Declare assoc items as children.
    lower_trait_assoc_items(ctx, t, &trait_id, out)?;

    let trait_body = IrTrait::builder()
        .flags(flags)
        .supers(supers)
        .generics(generics)
        .wheres(wheres)
        .build();

    let (src_path, span) = source::def_source_range(ctx, def);
    let parts = ctx.symbol_parts(def).unwrap_or_else(default_parts);
    let sym = parts.into_symbol(src_path, span);

    ctx.check_unique(&trait_id);
    out.declare(trait_id, parent, sym, trait_body);
    Ok(())
}

fn lower_trait_assoc_items(
    ctx: &mut LowerCtx<'_>,
    t: Trait,
    trait_id: &RaId,
    out: &mut Lowering<RaId>,
) -> Result<(), RustProducerError> {
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
                lower_assoc_type_alias(
                    ctx,
                    ta,
                    trait_id,
                    Some(trait_id.clone()),
                    out,
                )?;
            }
            AssocItem::Const(c) => {
                // Assoc const: id = child_id(trait_id, name).
                lower_assoc_const(ctx, c, trait_id, Some(trait_id.clone()), out)?;
            }
        }
    }
    Ok(())
}

fn detect_sealed(
    ctx: &LowerCtx<'_>,
    t: Trait,
) -> nudox_ir::kinds::Sealed {
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
    if is_sealed { Sealed::PubApi } else { Sealed::None }
}

// ── Impl ──────────────────────────────────────────────────────────────────────

/// Lower a trait impl or inherent impl block into `out`.
///
/// Inherent impls are emitted as `Impl { of: None, … }`.  Trait impls carry
/// `of: Some(Type::Nominal(trait_ref))`.  Child methods are declared under the
/// impl's `RaId`.
pub(crate) fn lower_impl(
    ctx: &mut LowerCtx<'_>,
    imp: Impl,
    parent: Option<RaId>,
    out: &mut Lowering<RaId>,
) -> Result<(), RustProducerError> {
    if imp.is_negative(ctx.db) {
        // Negative impls (`impl !Trait for T`) are emitted with `ImplFlags::negative`.
        // We still emit them so they are visible to analysis.
    }

    let impl_id = impl_id(ctx, imp);

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
        blanket: attach_db(ctx.db, || imp.self_ty(ctx.db).as_type_param(ctx.db).is_some()),
    };

    // Declare impl methods as children.
    // Key: use lower_assoc_* so the id is child_id(impl_id, name) rather than
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

    let impl_body = IrImpl::builder()
        .flags(flags)
        .maybe_of(of_type)
        .self_ty(self_ty)
        .generics(generics)
        .wheres(wheres)
        .build();

    let sym = nudox_ir::entry::Symbol {
        name: impl_id.to_string(),
        visibility: nudox_ir::entry::Visibility::Public,
        documentation: docs::documentation(ctx, imp).unwrap_or_default(),
        source: PathBuf::new(),
        span: 0..0,
        aliases: Box::new([]),
        deprecation: None,
        doc_links: Box::new([]),
        attrs: Box::new([]),
        cfg: None,
    };

    ctx.check_unique(&impl_id);
    out.declare(impl_id, parent, sym, impl_body);
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
    let impl_ast = ctx.sema.source(imp).or_else(|| imp.source(ctx.db)).map(|s| s.value);

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
) -> Result<(), RustProducerError> {
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
        .maybe_target(target.filter(|t| !matches!(t, nudox_ir::kinds::Type::Any)))
        .generics(generics)
        .wheres(wheres)
        .bounds(bounds)
        .build();

    let (src_path, span) = source::def_source_range(ctx, def);
    let parts = ctx.symbol_parts(def).unwrap_or_else(default_parts);
    let sym = parts.into_symbol(src_path, span);

    ctx.check_unique(&ta_id);
    out.declare(ta_id, parent, sym, alias_body);
    Ok(())
}

/// Lower a module-level type alias (type namespace, no `!v` / `!m` tag).
fn lower_type_alias(
    ctx: &mut LowerCtx<'_>,
    def: ModuleDef,
    parent: Option<RaId>,
    out: &mut Lowering<RaId>,
) -> Result<(), RustProducerError> {
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
) -> Result<(), RustProducerError> {
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
) -> Result<(), RustProducerError> {
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
    //   `nudox_ir::kinds::Const::value` becomes `Option<ConstExpr>`.
    //   Until then we use the best string we can obtain.
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

    let const_body = Const::builder().ty(const_ty).maybe_value(value).build();

    let (src_path, span) = source::def_source_range(ctx, def);
    let parts = ctx.symbol_parts(def).unwrap_or_else(default_parts);
    let sym = parts.into_symbol(src_path, span);

    ctx.check_unique(&const_id);
    out.declare(const_id, parent, sym, const_body);
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
) -> Result<(), RustProducerError> {
    // Const::name returns Option<Name> (anonymous consts in patterns use `_`).
    let const_name = c.name(ctx.db)
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
) -> Result<(), RustProducerError> {
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

    let (src_path, span) = source::def_source_range(ctx, def);
    let parts = ctx.symbol_parts(def).unwrap_or_else(default_parts);
    let sym = parts.into_symbol(src_path, span);

    ctx.check_unique(&static_id);
    out.declare(static_id, parent, sym, static_body);
    Ok(())
}

// ── Field helpers ─────────────────────────────────────────────────────────────

/// Intermediate data collected for one field before it is declared into
/// `Lowering`.  Separating the collection pass from the declaration pass
/// avoids the borrow conflict between `ref_for` (which captures `out`) and the
/// `out` parameter itself.
struct FieldData {
    field_id: RaId,
    key: FieldKey,
    ty: nudox_ir::kinds::Type,
    visibility: nudox_ir::entry::Visibility,
    doc: String,
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
    let data: Vec<FieldData> = {
        let mut ref_for = make_ref_for!(ctx, out);
        fields
            .iter()
            .enumerate()
            .map(|(idx, f)| {
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
                let field_ty = ty::lower_type_prefer_ast(ctx, ast_ty.as_ref(), &hir_ty, &mut ref_for);

                let visibility = ctx.visibility(*f);
                let doc = docs::documentation(ctx, *f).unwrap_or_default();

                FieldData { field_id, key, ty: field_ty, visibility, doc }
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

            let field_sym = nudox_ir::entry::Symbol {
                name: fd.field_id.to_string(),
                visibility: fd.visibility,
                documentation: fd.doc,
                source: PathBuf::new(),
                span: 0..0,
                aliases: Box::new([]),
                deprecation: None,
                doc_links: Box::new([]),
                attrs: Box::new([]),
                cfg: None,
            };

            ctx.check_unique(&fd.field_id);
            out.declare(fd.field_id.clone(), parent.clone(), field_sym, field_body);
            out.refer::<Field>(fd.field_id)
        })
        .collect()
}

// ── Param declaration helper ──────────────────────────────────────────────────

/// Declare all input params and return their `Ref<Param>` list.
pub(crate) fn declare_params(
    ctx: &mut LowerCtx<'_>,
    fn_id: &RaId,
    params: &[function::ParamData],
    parent: Option<RaId>,
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
            out.declare(param_id.clone(), parent.clone(), param_sym, param_body);
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
        facts.push(AutoFact { trait_: AutoTrait::Sync, state });
    }
    if let Some(state) = probe(Trait::lang(ctx.db, krate, LangItem::Unpin)) {
        facts.push(AutoFact { trait_: AutoTrait::Unpin, state });
    }

    facts
}

// ── Convenience helpers ───────────────────────────────────────────────────────

fn plain_sym(name: &str) -> nudox_ir::entry::Symbol {
    nudox_ir::entry::Symbol {
        name: name.to_owned(),
        visibility: nudox_ir::entry::Visibility::Private,
        documentation: String::new(),
        source: PathBuf::new(),
        span: 0..0,
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
    use nudox_ir::{
        change::{EcosystemId, PackageLineageId, PackageName},
        entry::EntryInner,
        kinds::{Field, FieldKey, Module as IrModule, Record, RecordForm, Reexport, Type},
        lower::Lowering,
        package::PackageId,
    };

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
        let sealed = pkg.seal(&lineage());

        // Find MyUnion entry.
        let found = sealed.iter().find(|(_, e)| e.sym().name == "MyUnion");
        let (_, union_entry) = found.expect("MyUnion must exist after sealing");

        // Verify the kind is Record with Union form via downcast.
        let typed = union_entry.downcast::<Record>()
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
        let sealed = pkg.seal(&lineage());

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
        let bar_ref: nudox_ir::index::Ref<IrModule> =
            low.declare(10, None, sym("Bar"), IrModule);

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
}
