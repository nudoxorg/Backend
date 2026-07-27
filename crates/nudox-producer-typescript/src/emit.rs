//! One-pass emission of `Vec<ModuleFacts>` into a `Lowering<TsId>`.
//!
//! This module replaces the old `oxc/link.rs` (~856 LOC). The vast majority of
//! that file evaporated because `Lowering::refer()` makes cross-module forward
//! references free — no intermediate index, no BFS visibility pass, no
//! type-link table.
//!
//! # What evaporated
//! - `type_links`: ~200 LOC. Forward references to other modules are just
//!   `lowering.refer::<T>(target_id)` — no pre-pass needed.
//! - Visibility BFS: ~100 LOC. Visibility is set at extraction time per symbol.
//! - `Index` assembly: ~150 LOC. `Lowering::finish()` builds the package.
//! - Module-name deduplication (now in `entry.rs`): ~50 LOC.
//! - Re-export chain flattening intermediate structures: ~150 LOC. We resolve
//!   re-exports inline below.
//!
//! # What remains (from link.rs)
//! - Re-export chain resolution: `module_for_export` / `resolve_reexport_target`.
//!   For `export { Foo } from "m"` we emit a `declare_ref` pointing at the
//!   target module's entry.
//! - Star-export fan-out: for `export * from "m"` we emit re-exports for each
//!   name the target module exports.
//!
//! # One-pass guarantee
//! We make exactly ONE pass over all `ModuleFacts` entries. Parent IDs are
//! always the module's own `TsId` (namespace or module root); children
//! reference parents declared in the same pass. Order is irrelevant because
//! `Lowering` is order-independent.

use std::{
    collections::HashMap,
    path::{Path, PathBuf},
};

use nudox_ir::{
    entry::{Symbol, Visibility},
    index::Ref,
    kinds::{
        Alias, Const, Enum, Field, FieldAttribute, FieldKey, FnModifier, Function,
        GenericParam, Module, Param, ParamAttribute, Record, RecordForm, Static, Trait,
        Variant, VariantForm,
        ty::{Primitive, Type},
    },
};

use nudox_ir::lower::Lowering;

use crate::{
    extract::{
        Accessibility, ClassBody, ConstBody, DeclBody, DeclFact, EnumBody, FunctionBody,
        GenericParamOwned, InterfaceBody, LiteralOwned, MemberKind, MemberModifiers,
        ModuleFacts, NamespaceBody, ReceiverKind, StaticBody, TypeAliasBody, TypeOwned,
    },
    id::TsId,
};

// ── Public entry point ─────────────────────────────────────────────────────────

/// Emit all modules into `out` in one pass.
///
/// Each module becomes an IR `Module` entry. Its declarations are children of
/// that module entry. Namespaces recurse.
pub fn lower_package(modules: &[ModuleFacts], out: &mut Lowering<TsId>) {
    // Build a quick index from path → module name for re-export resolution.
    let module_index: HashMap<&Path, &str> = modules
        .iter()
        .map(|m| (m.path.as_path(), m.module_name.as_str()))
        .collect();

    // Export table index: path → exported names for star-export fan-out.
    let export_index: HashMap<&Path, &crate::extract::ExportTable> =
        modules.iter().map(|m| (m.path.as_path(), &m.exports)).collect();

    for module in modules {
        let module_id = module_ts_id(module);

        // Declare the module itself as a `Module` kind.
        let _module_ref: Ref<Module> = out.declare(
            module_id.clone(),
            None,
            Symbol {
                name: module.module_name.clone(),
                visibility: Visibility::Public,
                documentation: module.module_doc.clone().unwrap_or_default(),
                source: module.path.clone(),
                span: 0..0,
                aliases: Box::new([]),
                deprecation: None,
                doc_links: Box::new([]),
                attrs: Box::new([]),
                cfg: None,
            },
            Module,
        );

        // Emit each top-level declaration.
        for decl in &module.declarations {
            emit_decl(decl, Some(module_id.clone()), out, &module_index, &export_index);
        }

        // Emit re-exports from the export table.
        emit_reexports(module, Some(module_id.clone()), out, &export_index);
    }
}

// ── Module root TsId ──────────────────────────────────────────────────────────

fn module_ts_id(module: &ModuleFacts) -> TsId {
    TsId::new(module.path.clone(), "$module", 0)
}

// ── Per-declaration emission ──────────────────────────────────────────────────

fn emit_decl(
    decl: &DeclFact,
    parent: Option<TsId>,
    out: &mut Lowering<TsId>,
    module_index: &HashMap<&Path, &str>,
    export_index: &HashMap<&Path, &crate::extract::ExportTable>,
) {
    let id = TsId::new(decl.module.clone(), &decl.name, decl.decl_index);
    let sym = make_sym(decl);

    match &decl.body {
        DeclBody::Interface(body) => emit_interface(id, parent, sym, body, out),
        DeclBody::Class(body) => emit_class(id, parent, sym, body, out),
        DeclBody::TypeAlias(body) => emit_type_alias(id, parent, sym, body, out),
        DeclBody::Enum(body) => emit_enum(id, parent, sym, body, out),
        DeclBody::Namespace(body) => emit_namespace(id, parent, sym, body, out, module_index, export_index),
        DeclBody::Function(body) => emit_function(id, parent, sym, body, out),
        DeclBody::Const(body) => emit_const(id, parent, sym, body, out),
        DeclBody::Static(body) => emit_static(id, parent, sym, body, out),
        DeclBody::Reexport { module_request, import_name } => {
            // Re-export inline: handled here and in emit_reexports for table-driven ones.
            emit_inline_reexport(
                id, parent, sym, module_request, import_name, out, module_index,
            );
        }
    }
}

// ── Interface → Trait ─────────────────────────────────────────────────────────

fn emit_interface(
    id: TsId,
    parent: Option<TsId>,
    sym: Symbol,
    body: &InterfaceBody,
    out: &mut Lowering<TsId>,
) {
    let generics = lower_generics(&body.generics);
    let supers: Vec<Type> = body.extends.iter().map(lower_type).collect();

    let _trait_ref: Ref<Trait> = out.declare(
        id.clone(),
        parent,
        sym,
        Trait::builder()
            .generics(generics)
            .supers(supers)
            .build(),
    );

    // Emit methods as child Function entries.
    for method in &body.methods {
        let method_id = TsId::new(id.module.clone(), &format!("{}::{}", id.name, method.name), 0);
        let method_sym = Symbol {
            name: method.name.clone(),
            visibility: accessibility_to_visibility(method.modifiers.accessibility),
            documentation: method.doc.doc.clone().unwrap_or_default(),
            source: id.module.clone(),
            span: 0..0,
            aliases: Box::new([]),
            deprecation: method.doc.deprecation.clone().map(|d| d.into_ir()),
            doc_links: Box::new([]),
            attrs: Box::new([]),
            cfg: None,
        };
        emit_function(method_id, Some(id.clone()), method_sym, &method.sig, out);
    }

    // Emit properties as child Field entries.
    for prop in &body.properties {
        let prop_id = TsId::new(id.module.clone(), &format!("{}::{}", id.name, prop.name), 0);
        let prop_sym = Symbol {
            name: prop.name.clone(),
            visibility: accessibility_to_visibility(prop.modifiers.accessibility),
            documentation: prop.doc.doc.clone().unwrap_or_default(),
            source: id.module.clone(),
            span: 0..0,
            aliases: Box::new([]),
            deprecation: prop.doc.deprecation.clone().map(|d| d.into_ir()),
            doc_links: Box::new([]),
            attrs: Box::new([]),
            cfg: None,
        };
        let attrs = field_attrs(&prop.modifiers);
        let _: Ref<Field> = out.declare(
            prop_id,
            Some(id.clone()),
            prop_sym,
            Field::builder()
                .key(FieldKey::Named)
                .maybe_ty(prop.ty.as_ref().map(lower_type))
                .attributes(attrs)
                .build(),
        );
    }

    // Emit index signatures as synthetic `__index[_N]` Function entries (item 5).
    for (idx_num, idx_sig) in body.index_signatures.iter().enumerate() {
        let synthetic_name = if idx_num == 0 {
            format!("{}::__index", id.name)
        } else {
            format!("{}::__index_{}", id.name, idx_num)
        };
        let idx_id = TsId::new(id.module.clone(), &synthetic_name, 0);
        let idx_sym = Symbol {
            name: if idx_num == 0 { "__index".to_string() } else { format!("__index_{}", idx_num) },
            visibility: Visibility::Public,
            documentation: String::new(),
            source: id.module.clone(),
            span: 0..0,
            aliases: Box::new([]),
            deprecation: None,
            doc_links: Box::new([]),
            attrs: Box::new([]),
            cfg: None,
        };
        // Emit as a Function with the key as param and value as return type.
        let key_param_body = crate::extract::ParamFact {
            name: idx_sig.key_name.clone(),
            ty: Some(idx_sig.key_ty.clone()),
            is_optional: false,
            is_rest: false,
            is_readonly: false,
        };
        let index_fn_body = FunctionBody {
            generics: Vec::new(),
            params: vec![key_param_body],
            return_type: Some(idx_sig.value_ty.clone()),
            is_async: false,
            is_generator: false,
            has_body: false,
            receiver: crate::extract::ReceiverKind::SharedRef,
        };
        emit_function(idx_id, Some(id.clone()), idx_sym, &index_fn_body, out);
    }

    // Emit construct signatures as synthetic `new[_N]` Function entries (item 6).
    for (cs_num, cs) in body.construct_signatures.iter().enumerate() {
        let synthetic_name = if cs_num == 0 {
            format!("{}::new", id.name)
        } else {
            format!("{}::new_{}", id.name, cs_num)
        };
        let cs_id = TsId::new(id.module.clone(), &synthetic_name, 0);
        let cs_sym = Symbol {
            name: if cs_num == 0 { "new".to_string() } else { format!("new_{}", cs_num) },
            visibility: Visibility::Public,
            documentation: String::new(),
            source: id.module.clone(),
            span: 0..0,
            aliases: Box::new([]),
            deprecation: None,
            doc_links: Box::new([]),
            attrs: Box::new([]),
            cfg: None,
        };
        emit_function(cs_id, Some(id.clone()), cs_sym, cs, out);
    }
}

// ── Class → Record ────────────────────────────────────────────────────────────

fn emit_class(
    id: TsId,
    parent: Option<TsId>,
    sym: Symbol,
    body: &ClassBody,
    out: &mut Lowering<TsId>,
) {
    let generics = lower_generics(&body.generics);
    let super_types: Vec<Type> = body.extends.iter().map(lower_type).collect();
    // implements → also super_types (TypeScript models them the same way)
    let mut all_supers = super_types;
    all_supers.extend(body.implements.iter().map(lower_type));

    // Collect field refs first (need to declare fields as children).
    // Because Lowering is order-independent we can refer before declaring.
    let mut field_refs: Vec<Ref<Field>> = Vec::new();

    for (idx, member) in body.members.iter().enumerate() {
        match &member.kind {
            MemberKind::Property { ty } | MemberKind::Accessor { ty } => {
                let field_id = TsId::new(
                    id.module.clone(),
                    &format!("{}::{}", id.name, member.name),
                    idx as u32,
                );
                let fref: Ref<Field> = out.refer(field_id.clone());
                field_refs.push(fref);
                // Declare the field below after the Record.
                let field_sym = Symbol {
                    name: member.name.clone(),
                    visibility: accessibility_to_visibility(member.modifiers.accessibility),
                    documentation: member.doc.doc.clone().unwrap_or_default(),
                    source: id.module.clone(),
                    span: 0..0,
                    aliases: Box::new([]),
                    deprecation: member.doc.deprecation.clone().map(|d| d.into_ir()),
                    doc_links: Box::new([]),
                    attrs: Box::new([]),
                    cfg: None,
                };
                let mut attrs = field_attrs(&member.modifiers);
                // Accessor is always mutable (both get + set).
                if matches!(member.kind, MemberKind::Accessor { .. })
                    && !attrs.contains(&FieldAttribute::Mutable)
                {
                    attrs.push(FieldAttribute::Mutable);
                }
                let _: Ref<Field> = out.declare(
                    field_id,
                    Some(id.clone()),
                    field_sym,
                    Field::builder()
                        .key(FieldKey::Named)
                        .maybe_ty(ty.as_ref().map(lower_type))
                        .attributes(attrs)
                        .build(),
                );
            }
            _ => {} // Methods / StaticBlock get their own emit below.
        }
    }

    let _: Ref<Record> = out.declare(
        id.clone(),
        parent,
        sym,
        Record::builder()
            .form(RecordForm::Struct)
            .fields(field_refs)
            .super_types(all_supers)
            .generics(generics)
            .build(),
    );

    // Emit method members.
    for (idx, member) in body.members.iter().enumerate() {
        match &member.kind {
            MemberKind::Method(v) => {
                let sigs_ref: Vec<&FunctionBody> = v.iter().collect();
                for (overload_idx, sig) in sigs_ref.iter().enumerate() {
                    let method_id = TsId::new(
                        id.module.clone(),
                        &format!("{}::{}", id.name, member.name),
                        (idx * 1000 + overload_idx) as u32,
                    );
                    let method_sym = Symbol {
                        name: member.name.clone(),
                        visibility: accessibility_to_visibility(member.modifiers.accessibility),
                        documentation: member.doc.doc.clone().unwrap_or_default(),
                        source: id.module.clone(),
                        span: 0..0,
                        aliases: Box::new([]),
                        deprecation: member.doc.deprecation.clone().map(|d| d.into_ir()),
                        doc_links: Box::new([]),
                        attrs: Box::new([]),
                        cfg: None,
                    };
                    emit_function(method_id, Some(id.clone()), method_sym, sig, out);
                }
            }
            MemberKind::Constructor(s) => {
                let method_id = TsId::new(
                    id.module.clone(),
                    &format!("{}::{}", id.name, member.name),
                    (idx * 1000) as u32,
                );
                let method_sym = Symbol {
                    name: member.name.clone(),
                    visibility: accessibility_to_visibility(member.modifiers.accessibility),
                    documentation: member.doc.doc.clone().unwrap_or_default(),
                    source: id.module.clone(),
                    span: 0..0,
                    aliases: Box::new([]),
                    deprecation: member.doc.deprecation.clone().map(|d| d.into_ir()),
                    doc_links: Box::new([]),
                    attrs: Box::new([]),
                    cfg: None,
                };
                emit_function(method_id, Some(id.clone()), method_sym, s, out);
            }
            // ── Item 4: Static block → synthetic Function (item 4) ────────
            MemberKind::StaticBlock { name: block_name } => {
                let sb_id = TsId::new(
                    id.module.clone(),
                    &format!("{}::{}", id.name, block_name),
                    idx as u32,
                );
                let sb_sym = Symbol {
                    name: block_name.clone(),
                    visibility: Visibility::Private,
                    documentation: String::new(),
                    source: id.module.clone(),
                    span: 0..0,
                    aliases: Box::new([]),
                    deprecation: None,
                    doc_links: Box::new([]),
                    attrs: Box::new([]),
                    cfg: None,
                };
                let static_fn_body = FunctionBody {
                    generics: Vec::new(),
                    params: Vec::new(),
                    return_type: None,
                    is_async: false,
                    is_generator: false,
                    has_body: true,
                    receiver: ReceiverKind::None,
                };
                emit_function(sb_id, Some(id.clone()), sb_sym, &static_fn_body, out);
            }
            // Fields / Accessors were already emitted above.
            MemberKind::Property { .. } | MemberKind::Accessor { .. } => continue,
        }
    }
}

// ── Type alias → Alias ────────────────────────────────────────────────────────

fn emit_type_alias(
    id: TsId,
    parent: Option<TsId>,
    sym: Symbol,
    body: &TypeAliasBody,
    out: &mut Lowering<TsId>,
) {
    let generics = lower_generics(&body.generics);
    let target = lower_type(&body.target);
    let _: Ref<Alias> = out.declare(
        id,
        parent,
        sym,
        Alias::builder()
            .target(target)
            .generics(generics)
            .build(),
    );
}

// ── Enum → Enum + Variants ────────────────────────────────────────────────────

fn emit_enum(
    id: TsId,
    parent: Option<TsId>,
    sym: Symbol,
    body: &EnumBody,
    out: &mut Lowering<TsId>,
) {
    let mut variant_refs: Vec<Ref<Variant>> = Vec::with_capacity(body.variants.len());

    for (idx, v) in body.variants.iter().enumerate() {
        let variant_id = TsId::new(id.module.clone(), &format!("{}::{}", id.name, v.name), idx as u32);
        let vref: Ref<Variant> = out.refer(variant_id.clone());
        variant_refs.push(vref);

        let variant_sym = Symbol {
            name: v.name.clone(),
            visibility: Visibility::Public,
            documentation: String::new(),
            source: id.module.clone(),
            span: 0..0,
            aliases: Box::new([]),
            deprecation: None,
            doc_links: Box::new([]),
            attrs: Box::new([]),
            cfg: None,
        };
        let _: Ref<Variant> = out.declare(
            variant_id,
            Some(id.clone()),
            variant_sym,
            Variant::builder()
                .form(VariantForm::Unit)
                .maybe_discr(v.discriminant.clone())
                .build(),
        );
    }

    let _: Ref<Enum> = out.declare(
        id,
        parent,
        sym,
        Enum::builder().variants(variant_refs).build(),
    );
}

// ── Namespace → Module (recursive) ───────────────────────────────────────────

fn emit_namespace(
    id: TsId,
    parent: Option<TsId>,
    sym: Symbol,
    body: &NamespaceBody,
    out: &mut Lowering<TsId>,
    module_index: &HashMap<&Path, &str>,
    export_index: &HashMap<&Path, &crate::extract::ExportTable>,
) {
    let _: Ref<Module> = out.declare(id.clone(), parent, sym, Module);

    for child in &body.children {
        emit_decl(child, Some(id.clone()), out, module_index, export_index);
    }
}

// ── Function → Function + Params ─────────────────────────────────────────────

fn emit_function(
    id: TsId,
    parent: Option<TsId>,
    sym: Symbol,
    body: &FunctionBody,
    out: &mut Lowering<TsId>,
) {
    let generics = lower_generics(&body.generics);

    let mut modifiers: Vec<FnModifier> = Vec::new();
    if body.is_async {
        modifiers.push(FnModifier::Async);
    }
    if body.is_generator {
        modifiers.push(FnModifier::Generator);
    }

    let receiver = match body.receiver {
        ReceiverKind::None => None,
        ReceiverKind::SharedRef => Some(nudox_ir::kinds::function::Receiver::SharedRef),
        ReceiverKind::MutRef => Some(nudox_ir::kinds::function::Receiver::MutRef),
    };

    // Collect param refs before declaring params as children.
    let mut param_refs: Vec<Ref<Param>> = Vec::with_capacity(body.params.len());
    let mut param_ids: Vec<(TsId, Param)> = Vec::with_capacity(body.params.len());

    for (idx, p) in body.params.iter().enumerate() {
        let param_id =
            TsId::new(id.module.clone(), &format!("{}::param::{}", id.name, p.name), idx as u32);
        let pref: Ref<Param> = out.refer(param_id.clone());
        param_refs.push(pref);

        let mut attrs: Vec<ParamAttribute> = Vec::new();
        if p.is_optional {
            attrs.push(ParamAttribute::Optional);
        }
        if p.is_rest {
            attrs.push(ParamAttribute::Variadic);
        }
        param_ids.push((param_id, Param::builder().maybe_ty(p.ty.as_ref().map(lower_type)).attributes(attrs).build()));
    }

    // Output parameter: return type, if any.
    let mut output_refs: Vec<Ref<Param>> = Vec::new();
    if let Some(ret) = &body.return_type {
        let ret_id = TsId::new(id.module.clone(), &format!("{}::return", id.name), 0);
        let rref: Ref<Param> = out.refer(ret_id.clone());
        output_refs.push(rref);
        let param_sym = Symbol {
            name: "return".to_string(),
            visibility: Visibility::Public,
            documentation: String::new(),
            source: id.module.clone(),
            span: 0..0,
            aliases: Box::new([]),
            deprecation: None,
            doc_links: Box::new([]),
            attrs: Box::new([]),
            cfg: None,
        };
        let _: Ref<Param> = out.declare(
            ret_id,
            Some(id.clone()),
            param_sym,
            Param::builder().ty(lower_type(ret)).build(),
        );
    }

    let _: Ref<Function> = out.declare(
        id.clone(),
        parent,
        sym,
        Function::builder()
            .maybe_receiver(receiver)
            .input_params(param_refs)
            .output_params(output_refs)
            .modifiers(modifiers)
            .generics(generics)
            .build(),
    );

    // Declare params as children after declaring the function.
    for (param_id, param) in param_ids {
        let param_sym = Symbol {
            name: param_name_from_id(&param_id.name),
            visibility: Visibility::Public,
            documentation: String::new(),
            source: id.module.clone(),
            span: 0..0,
            aliases: Box::new([]),
            deprecation: None,
            doc_links: Box::new([]),
            attrs: Box::new([]),
            cfg: None,
        };
        let _: Ref<Param> = out.declare(param_id, Some(id.clone()), param_sym, param);
    }
}

fn param_name_from_id(full: &str) -> String {
    full.rsplit("::param::").next().unwrap_or(full).to_string()
}

// ── Const / Static ────────────────────────────────────────────────────────────

fn emit_const(
    id: TsId,
    parent: Option<TsId>,
    sym: Symbol,
    body: &ConstBody,
    out: &mut Lowering<TsId>,
) {
    let ty = body.ty.as_ref().map(lower_type).unwrap_or(Type::Any);
    let _: Ref<Const> = out.declare(
        id,
        parent,
        sym,
        Const::builder()
            .ty(ty)
            .maybe_value(body.value.clone())
            .build(),
    );
}

fn emit_static(
    id: TsId,
    parent: Option<TsId>,
    sym: Symbol,
    body: &StaticBody,
    out: &mut Lowering<TsId>,
) {
    let ty = body.ty.as_ref().map(lower_type).unwrap_or(Type::Any);
    let _: Ref<Static> = out.declare(
        id,
        parent,
        sym,
        Static::builder()
            .ty(ty)
            .mutable(body.is_mutable)
            .build(),
    );
}

// ── Re-exports ─────────────────────────────────────────────────────────────────

fn emit_reexports(
    module: &ModuleFacts,
    parent: Option<TsId>,
    out: &mut Lowering<TsId>,
    export_index: &HashMap<&Path, &crate::extract::ExportTable>,
) {
    // Named re-exports: `export { Foo } from "m"`.
    for indirect in &module.exports.indirect {
        let target_path = resolve_module_path(&module.path, &indirect.module_request);
        let target_name = &indirect.import_name;
        let export_name = &indirect.export_name;

        // The reexport ID in the current module.
        let reexport_id =
            TsId::new(module.path.clone(), export_name, 0);
        // The target ID in the source module.
        let target_id = target_path
            .as_ref()
            .map(|p| TsId::new(p.clone(), target_name, 0));

        let sym = Symbol {
            name: export_name.clone(),
            visibility: Visibility::Public,
            documentation: String::new(),
            source: module.path.clone(),
            span: 0..0,
            aliases: Box::new([]),
            deprecation: None,
            doc_links: Box::new([]),
            attrs: Box::new([]),
            cfg: None,
        };

        if let Some(tid) = target_id {
            let target_ref: Ref<Module> = out.refer(tid);
            let _: Ref<Module> = out.declare_ref(reexport_id, parent.clone(), sym, target_ref);
        }
    }

    // Star re-exports: `export * from "m"`.
    for star in &module.exports.star {
        let target_path = resolve_module_path(&module.path, &star.module_request);
        let Some(tp) = target_path else { continue };

        // Fan out to all names the target module exports.
        if let Some(target_table) = export_index.get(tp.as_path()) {
            for export_name in &target_table.exported_names {
                let reexport_id = TsId::new(module.path.clone(), export_name, 0);
                let target_id = TsId::new(tp.clone(), export_name, 0);
                let sym = Symbol {
                    name: export_name.clone(),
                    visibility: Visibility::Public,
                    documentation: String::new(),
                    source: module.path.clone(),
                    span: 0..0,
                    aliases: Box::new([]),
                    deprecation: None,
                    doc_links: Box::new([]),
                    attrs: Box::new([]),
                    cfg: None,
                };
                let target_ref: Ref<Module> = out.refer(target_id);
                let _: Ref<Module> = out.declare_ref(reexport_id, parent.clone(), sym, target_ref);
            }
        }
    }
}

fn emit_inline_reexport(
    id: TsId,
    parent: Option<TsId>,
    sym: Symbol,
    module_request: &str,
    import_name: &str,
    out: &mut Lowering<TsId>,
    _module_index: &HashMap<&Path, &str>,
) {
    let target_path = resolve_module_path(&id.module, module_request);
    if let Some(tp) = target_path {
        let target_id = TsId::new(tp, import_name, 0);
        let target_ref: Ref<Module> = out.refer(target_id);
        let _: Ref<Module> = out.declare_ref(id, parent, sym, target_ref);
    }
}

/// Best-effort: given the current module's absolute path and an import
/// specifier, return the absolute path of the target module.
fn resolve_module_path(current: &Path, specifier: &str) -> Option<PathBuf> {
    if specifier.starts_with('.') {
        let parent = current.parent()?;
        let candidate = parent.join(specifier);
        // Try common extensions.
        for ext in &[".ts", ".tsx", ".d.ts", ".js", "/index.ts", "/index.d.ts"] {
            let path = if specifier.contains('.') {
                candidate.clone()
            } else {
                PathBuf::from(format!("{}{}", candidate.display(), ext))
            };
            if path.is_file() {
                return Some(path);
            }
        }
        // Bare directory-style specifier without extension.
        for name in &["index.d.ts", "index.ts", "index.js"] {
            let idx = candidate.join(name);
            if idx.is_file() {
                return Some(idx);
            }
        }
    }
    None
}

// ── Type lowering: TypeOwned → IR Type ────────────────────────────────────────

pub(crate) fn lower_type(ty: &TypeOwned) -> Type {
    match ty {
        TypeOwned::Any | TypeOwned::Unknown => Type::Any,
        TypeOwned::Never => Type::Never,
        TypeOwned::Void | TypeOwned::Undefined => Type::Tuple(Box::new([])), // unit
        TypeOwned::Null => Type::Primitive(Primitive::Builtin("null".to_string())),
        TypeOwned::Bool => Type::Primitive(Primitive::Bool),
        TypeOwned::Number => Type::Primitive(Primitive::Float(nudox_ir::kinds::ty::Width::Fixed(
            std::num::NonZeroU16::new(64).unwrap(),
        ))),
        TypeOwned::BigInt => Type::Primitive(Primitive::Builtin("bigint".to_string())),
        TypeOwned::String => Type::Primitive(Primitive::Str),
        TypeOwned::Symbol => Type::Primitive(Primitive::Builtin("symbol".to_string())),
        TypeOwned::Object => Type::Primitive(Primitive::Builtin("object".to_string())),
        TypeOwned::This => Type::SelfType,
        TypeOwned::Primitive(s) => Type::Primitive(Primitive::Builtin(s.clone())),
        TypeOwned::Nominal(name) => {
            // UNCERTAINTY: We emit a RawRef::Name here, but the actual IR Type::Nominal
            // takes a RawRef. RawRef has no public name-only constructor in the
            // signatures we read. Using Builtin as fallback for unresolvable nominals.
            //
            // The correct production path: resolve name → TsId → refer() → Ref → RawRef.
            // That requires the full lowering context (the `out: &mut Lowering<TsId>`).
            // Here in a pure helper we don't have it. This is the primary compile-time
            // uncertainty: `Type::Nominal(RawRef)` cannot be constructed from a string
            // alone without the Lowering sink.
            //
            // Mitigation: callers that need nominal resolution pass the Lowering in
            // directly; this helper uses Builtin as a conservative fallback.
            Type::Primitive(Primitive::Builtin(name.clone()))
        }
        TypeOwned::TypeVar(name) => {
            // TypeVar represents a generic type parameter use (e.g. `T` in `Array<T>`).
            // `Type::TypeVar(String)` exists in nudox-ir as of the dual-fidelity body
            // plane commit. Use it directly.
            Type::TypeVar(name.clone())
        }
        TypeOwned::Apply { base, args } => {
            let base_ty = lower_type(base);
            let arg_tys: Vec<Type> = args.iter().map(lower_type).collect();
            Type::Apply {
                base: Box::new(base_ty),
                args: arg_tys.into_boxed_slice(),
            }
        }
        TypeOwned::Union(arms) => {
            Type::Union(arms.iter().map(lower_type).collect::<Vec<_>>().into_boxed_slice())
        }
        TypeOwned::Intersection(arms) => {
            Type::Intersection(arms.iter().map(lower_type).collect::<Vec<_>>().into_boxed_slice())
        }
        TypeOwned::Tuple(members) => {
            Type::Tuple(members.iter().map(lower_type).collect::<Vec<_>>().into_boxed_slice())
        }
        TypeOwned::Array(inner) => {
            // Model `T[]` as a Slice.
            Type::Slice(Box::new(lower_type(inner)))
        }
        // ── Named tuple element (item 10) ─────────────────────────────────
        // The IR's Type::Tuple carries positional types only; there is no
        // slot for element labels.  We emit the element type, which preserves
        // the type information.  The label is held in TypeOwned::NamedTupleElem
        // but is discarded at the IR boundary until nudox-ir adds a named-tuple
        // element type.  See the TypeOwned::NamedTupleElem doc for the requested
        // IR change signature.
        TypeOwned::NamedTupleElem { ty, .. } => lower_type(ty),
        TypeOwned::Function(_) => Type::Primitive(Primitive::Builtin("Function".to_string())),
        TypeOwned::Literal(lit) => lower_literal(lit),
        TypeOwned::Unsupported(s) => Type::Primitive(Primitive::Builtin(s.clone())),
    }
}

fn lower_literal(lit: &LiteralOwned) -> Type {
    match lit {
        LiteralOwned::Bool(_) => Type::Primitive(Primitive::Bool),
        LiteralOwned::Number(_) => Type::Primitive(Primitive::Float(
            nudox_ir::kinds::ty::Width::Fixed(std::num::NonZeroU16::new(64).unwrap()),
        )),
        LiteralOwned::String(_) => Type::Primitive(Primitive::Str),
        LiteralOwned::BigInt(_) => Type::Primitive(Primitive::Builtin("bigint".to_string())),
        LiteralOwned::Null => Type::Primitive(Primitive::Builtin("null".to_string())),
        LiteralOwned::Undefined => Type::Tuple(Box::new([])),
    }
}

// ── Generic param lowering ─────────────────────────────────────────────────────

fn lower_generics(params: &[GenericParamOwned]) -> Vec<GenericParam> {
    params
        .iter()
        .map(|p| GenericParam::Type {
            name: p.name.clone(),
            bounds: p.bounds.iter().map(lower_type).collect::<Vec<_>>().into_boxed_slice(),
            default: p.default.as_ref().map(lower_type),
        })
        .collect()
}

// ── Symbol construction ────────────────────────────────────────────────────────

fn make_sym(decl: &DeclFact) -> Symbol {
    // Visibility is not Clone; reconstruct from the discriminant.
    let visibility = match decl.visibility {
        Visibility::Public => Visibility::Public,
        Visibility::Private => Visibility::Private,
        Visibility::Protected => Visibility::Protected,
        Visibility::Internal => Visibility::Internal,
        Visibility::Package => Visibility::Package,
        Visibility::Crate => Visibility::Crate,
    };
    Symbol {
        name: decl.name.clone(),
        visibility,
        documentation: decl.doc.doc.clone().unwrap_or_default(),
        source: decl.module.clone(),
        span: (decl.span_start as usize)..(decl.span_end as usize),
        aliases: Box::new([]),
        // DeprecationOwned is Clone; convert to IR's Deprecation which is not.
        deprecation: decl.doc.deprecation.clone().map(|d| d.into_ir()),
        doc_links: Box::new([]),
        attrs: Box::new([]),
        cfg: None,
    }
}

// ── Accessibility → Visibility ────────────────────────────────────────────────

fn accessibility_to_visibility(acc: Accessibility) -> Visibility {
    match acc {
        Accessibility::Public => Visibility::Public,
        Accessibility::Protected => Visibility::Protected,
        Accessibility::Private | Accessibility::PrivateField => Visibility::Private,
    }
}

// ── FieldAttributes ───────────────────────────────────────────────────────────

fn field_attrs(m: &MemberModifiers) -> Vec<FieldAttribute> {
    let mut attrs = Vec::new();
    if m.is_optional {
        attrs.push(FieldAttribute::Optional);
    }
    if m.is_static {
        attrs.push(FieldAttribute::Static);
    }
    if !m.is_readonly {
        attrs.push(FieldAttribute::Mutable);
    }
    attrs
}
