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
//! # Param IDs
//!
//! Parameters are separate entries (kind = `Param`).  Their `RaId` is
//! `"{fn_id}::param::{name}"` for inputs and `"{fn_id}::param::return"` for
//! the output.  These are synthetic but unique per function.
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
    Adt, AssocItem, FieldSource, HasSource, HasVisibility, Impl, Module, ModuleDef, Struct,
    StructKind, Trait, attach_db,
};
use ra_ap_syntax::ast::HasTypeBounds;
use ra_ap_syntax::{AstNode, ast::HasGenericParams};

use nudox_ir::{
    index::{RawRef, Ref},
    kinds::{
        Alias, AutoFact, Const, Enum, Field, FieldAttribute, FieldKey,
        Function, Impl as IrImpl, ImplFlags, Module as IrModule, Param, Record, RecordForm,
        Static, Trait as IrTrait, TraitFlags, TriState, Variant, VariantForm,
    },
    lower::Lowering,
};

use crate::RaId;
use crate::error::RustProducerError;
use super::{
    ctx::LowerCtx,
    docs, function, generics, source, ty,
};

// ── RefFor helper closure ─────────────────────────────────────────────────────

/// Build a `ref_for` closure that maps a canonical path key to a
/// `Ref<Param>`-compatible `RawRef` by forwarding to `Lowering::refer`.
///
/// The closure is generic over `K: nudox_ir::kind::EntryKind` but for
/// `Type::Nominal` we need a `RawRef`.  We use `Lowering::refer::<nudox_ir::kinds::Module>`
/// as an untyped vehicle (the kind does not matter for `RawRef` purposes; we
/// always erase to `RawRef` immediately).  The produced `RawRef` is valid as
/// long as the `Lowering` session is alive.
///
/// `Ref::into_raw` is confirmed public in `nudox_ir::index` (line 268 of
/// index.rs: `pub fn into_raw(self) -> RawRef`).  The cast is sound.
macro_rules! make_ref_for {
    ($lowering:ident) => {{
        |key: &crate::ra::ctx::PathKey| -> Option<nudox_ir::index::RawRef> {
            // We produce a Ref<IrModule> as the typed vehicle and immediately
            // erase it.  The type parameter does not affect the interned index
            // — it is purely a phantom marker.
            let typed: Ref<IrModule> = $lowering.refer(key.clone());
            Some(typed.into_raw())
        }
    }};
}

// ── Module entry ──────────────────────────────────────────────────────────────

/// Declare `module` as an `IrModule` entry in `out`.
///
/// Child module / member paths are not listed here — the tree is derived from
/// parent pointers by `Lowering::finish`.
pub(crate) fn lower_module(
    ctx: &mut LowerCtx<'_>,
    module: Module,
    parent: Option<RaId>,
    out: &mut Lowering<RaId>,
) -> Result<(), RustProducerError> {
    let def = ModuleDef::Module(module);
    let Some(id) = ctx.ra_id(def) else {
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
    });
    let sym = parts.into_symbol(PathBuf::new(), 0..0);
    out.declare(id, parent, sym, IrModule);
    Ok(())
}

// ── Primary item lowering ─────────────────────────────────────────────────────

/// Lower one module-level `ModuleDef` into `out`.
///
/// Trait assoc items and impl methods are emitted by `lower_trait` and
/// `lower_impl` respectively.
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
            lower_free_function(ctx, f, parent, out)?;
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
            lower_const(ctx, c, parent, out)?;
        }
        ModuleDef::Static(s) => {
            lower_static(ctx, s, parent, out)?;
        }
        ModuleDef::Macro(m) => {
            // No Macro kind in new IR — emit as Module.
            let Some(id) = ctx.ra_id(ModuleDef::Macro(m)) else {
                return Ok(());
            };
            let parts = ctx.symbol_parts(ModuleDef::Macro(m)).unwrap_or_else(default_parts);
            out.declare(id, parent, parts.into_symbol(PathBuf::new(), 0..0), IrModule);
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
            out.declare(id, parent, sym, Alias::builder().maybe_target(None).build());
        }
        ModuleDef::Module(_) | ModuleDef::EnumVariant(_) => {}
    }
    Ok(())
}

// ── Free function ─────────────────────────────────────────────────────────────

fn lower_free_function(
    ctx: &mut LowerCtx<'_>,
    f: ra_ap_hir::Function,
    parent: Option<RaId>,
    out: &mut Lowering<RaId>,
) -> Result<(), RustProducerError> {
    let def = ModuleDef::Function(f);
    let Some(fn_id) = ctx.ra_id(def) else {
        return Ok(());
    };
    let mut ref_for = make_ref_for!(out);
    let Some(fd) = function::lower_function(ctx, f, &mut ref_for) else {
        return Ok(());
    };
    drop(ref_for);

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

    out.declare(fn_id, parent, sym, fn_body);
    Ok(())
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
    let mut ref_for = make_ref_for!(out);

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

    // Auto-trait facts: derive Send/Sync/Unpin from HIR when available.
    // UNCERTAINTY: `hir::Type::is_send`, `is_sync` — these may not exist in
    // ra_ap 0.0.341's public API.  The auto-trait probe tier in the old code
    // went through `ProbeTier::Std` which required extra synthesis.  For now
    // we leave `auto` empty and document it as a known gap.
    let auto: Vec<AutoFact> = Vec::new();

    drop(ref_for);

    // Declare field entries after releasing ref_for so out is free.
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
    let mut ref_for = make_ref_for!(out);

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

    let auto: Vec<AutoFact> = Vec::new();

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

            out.declare(variant_id.clone(), Some(enum_id.clone()), variant_sym, variant_body);
            out.refer::<Variant>(variant_id)
        })
        .collect();
    drop(ref_for);

    let enum_body = Enum::builder()
        .variants(variant_refs)
        .generics(generics)
        .wheres(wheres)
        .auto(auto)
        .build();

    let (src_path, span) = source::def_source_range(ctx, def);
    let parts = ctx.symbol_parts(def).unwrap_or_else(default_parts);
    let sym = parts.into_symbol(src_path, span);

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
    let mut ref_for = make_ref_for!(out);

    // Declare fields.
    let field_refs = declare_hir_fields(
        ctx,
        &union_id,
        u.fields(ctx.db),
        RecordForm::Struct,
        &mut ref_for,
        parent.clone(),
        out,
    );
    drop(ref_for);

    // Unions are represented as `Record` with `RecordForm::Struct` for now.
    // The new IR has no separate Union kind.
    // UNCERTAINTY: The IR `kind.rs` shows `Kind::Record` but no `Kind::Union`.
    // Unions therefore lower as `Record`; this loses the union semantics tag.
    let record_body = Record::builder()
        .form(RecordForm::Struct)
        .fields(field_refs)
        .build();

    let (src_path, span) = source::def_source_range(ctx, def);
    let parts = ctx.symbol_parts(def).unwrap_or_else(default_parts);
    let sym = parts.into_symbol(src_path, span);

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
    let mut ref_for = make_ref_for!(out);

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
    let supers: Vec<nudox_ir::kinds::Type> = t
        .direct_supertraits(ctx.db)
        .into_iter()
        .filter_map(|st| {
            let st_def = ModuleDef::Trait(st);
            let key = ctx.canonical(st_def)?;
            let raw_ref = ref_for(&key)?;
            Some(nudox_ir::kinds::Type::Nominal(raw_ref))
        })
        .collect();

    let flags = TraitFlags {
        is_unsafe: t.is_unsafe(ctx.db),
        is_auto: t.is_auto(ctx.db),
        dyn_compat: match t.dyn_compatibility(ctx.db) {
            None => TriState::Yes,
            Some(_) => TriState::No,
        },
        sealed: detect_sealed(ctx, t),
    };
    drop(ref_for);

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
                lower_free_function(ctx, f, Some(trait_id.clone()), out)?;
            }
            AssocItem::TypeAlias(ta) => {
                lower_type_alias(
                    ctx,
                    ModuleDef::TypeAlias(ta),
                    Some(trait_id.clone()),
                    out,
                )?;
            }
            AssocItem::Const(c) => {
                lower_const(ctx, c, Some(trait_id.clone()), out)?;
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

    let mut ref_for = make_ref_for!(out);

    let self_ty = {
        let hir_self = attach_db(ctx.db, || imp.self_ty(ctx.db));
        ty::lower_hir_type_fallback(ctx, &hir_self, &mut ref_for)
    };

    let of_type = imp.trait_(ctx.db).and_then(|tr| {
        let tr_def = ModuleDef::Trait(tr);
        let key = ctx.canonical(tr_def)?;
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
    drop(ref_for);

    // Declare impl methods as children.
    for item in imp.items(ctx.db) {
        if let AssocItem::Function(f) = item {
            // Inherit visibility gate from the impl (trait methods always visible).
            if imp.trait_(ctx.db).is_none()
                && !ctx.document_private
                && !matches!(f.visibility(ctx.db), ra_ap_hir::Visibility::Public)
            {
                continue;
            }
            lower_free_function(ctx, f, Some(impl_id.clone()), out)?;
        }
        // TypeAlias and Const assoc items inside impl blocks.
        if let AssocItem::TypeAlias(ta) = item {
            lower_type_alias(ctx, ModuleDef::TypeAlias(ta), Some(impl_id.clone()), out)?;
        }
        if let AssocItem::Const(c) = item {
            lower_const(ctx, c, Some(impl_id.clone()), out)?;
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

    out.declare(impl_id, parent, sym, impl_body);
    Ok(())
}

/// Generate a unique `RaId` for an impl block.
///
/// Impls have no stable name in Rust.  We synthesise one from the crate
/// module, the trait (if any), and the self-type's short name.  When two
/// distinct impls produce the same label (which can happen) we append `#N`.
fn impl_id(ctx: &mut LowerCtx<'_>, imp: Impl) -> RaId {
    let module = imp.module(ctx.db);
    let mod_key = ctx
        .canonical(ModuleDef::Module(module))
        .unwrap_or_else(|| smol_str::SmolStr::from("<anon>"));

    let trait_part = imp
        .trait_(ctx.db)
        .and_then(|tr| ctx.canonical(ModuleDef::Trait(tr)))
        .map(|k| format!("::{k}"))
        .unwrap_or_default();

    let self_part = {
        let self_ty = attach_db(ctx.db, || imp.self_ty(ctx.db));
        if let Some(adt) = attach_db(ctx.db, || self_ty.as_adt()) {
            ctx.canonical(ModuleDef::Adt(adt))
                .map(|k| format!("::{k}"))
                .unwrap_or_default()
        } else {
            String::new()
        }
    };

    let stem = smol_str::SmolStr::from(
        format!("{mod_key}<impl{trait_part}for{self_part}>").as_str()
    );
    ctx.method_id(&mod_key, &stem)
}

// ── Type alias ────────────────────────────────────────────────────────────────

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
    let mut ref_for = make_ref_for!(out);

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
                            let key = ctx.canonical(def)?;
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
    drop(ref_for);

    let alias_body = Alias::builder()
        .maybe_target(target.filter(|t| !matches!(t, nudox_ir::kinds::Type::Any)))
        .generics(generics)
        .wheres(wheres)
        .bounds(bounds)
        .build();

    let (src_path, span) = source::def_source_range(ctx, def);
    let parts = ctx.symbol_parts(def).unwrap_or_else(default_parts);
    let sym = parts.into_symbol(src_path, span);

    out.declare(ta_id, parent, sym, alias_body);
    Ok(())
}

// ── Const ─────────────────────────────────────────────────────────────────────

fn lower_const(
    ctx: &mut LowerCtx<'_>,
    c: ra_ap_hir::Const,
    parent: Option<RaId>,
    out: &mut Lowering<RaId>,
) -> Result<(), RustProducerError> {
    let def = ModuleDef::Const(c);
    let Some(const_id) = ctx.ra_id(def) else {
        return Ok(());
    };
    let mut ref_for = make_ref_for!(out);

    let src = ctx.sema.source(c).or_else(|| c.source(ctx.db));
    let const_ty = if let Some(src) = &src
        && let Some(ty_node) = src.value.ty()
    {
        ty::lower_ast_type(ctx, &ty_node, &mut ref_for)
    } else {
        let hir_ty = attach_db(ctx.db, || c.ty(ctx.db));
        ty::lower_hir_type_fallback(ctx, &hir_ty, &mut ref_for)
    };

    let value: Option<String> = src
        .as_ref()
        .and_then(|s| s.value.body())
        .map(|e| e.syntax().text().to_string());
    drop(ref_for);

    let const_body = Const::builder().ty(const_ty).maybe_value(value).build();

    let (src_path, span) = source::def_source_range(ctx, def);
    let parts = ctx.symbol_parts(def).unwrap_or_else(default_parts);
    let sym = parts.into_symbol(src_path, span);

    out.declare(const_id, parent, sym, const_body);
    Ok(())
}

// ── Static ────────────────────────────────────────────────────────────────────

fn lower_static(
    ctx: &mut LowerCtx<'_>,
    s: ra_ap_hir::Static,
    parent: Option<RaId>,
    out: &mut Lowering<RaId>,
) -> Result<(), RustProducerError> {
    let def = ModuleDef::Static(s);
    let Some(static_id) = ctx.ra_id(def) else {
        return Ok(());
    };
    let mut ref_for = make_ref_for!(out);

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
    drop(ref_for);

    let static_body = Static::builder().ty(static_ty).mutable(mutable).build();

    let (src_path, span) = source::def_source_range(ctx, def);
    let parts = ctx.symbol_parts(def).unwrap_or_else(default_parts);
    let sym = parts.into_symbol(src_path, span);

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
        let mut ref_for = make_ref_for!(out);
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
        // ref_for dropped here, releasing the borrow on out
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

            out.declare(fd.field_id.clone(), parent.clone(), field_sym, field_body);
            out.refer::<Field>(fd.field_id)
        })
        .collect()
}

// ── Param declaration helper ──────────────────────────────────────────────────

/// Declare all input params and return their `Ref<Param>` list.
pub(crate) fn declare_params(
    ctx: &LowerCtx<'_>,
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
            out.declare(param_id.clone(), parent.clone(), param_sym, param_body);
            out.refer::<Param>(param_id)
        })
        .collect()
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
    }
}
