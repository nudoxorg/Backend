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
        Alias, Const, Enum, Field, FieldAttribute, FieldKey, FnModifier, Function, GenericParam,
        Module, Param, ParamAttribute, Record, RecordForm, Static, Trait, Variant, VariantForm,
        ty::{Primitive, TupleElement, Type},
    },
};

use nudox_ir::lower::Lowering;
use oxc_resolver::Resolver;

use crate::typescript::{
    entry::{is_ts_module_path, make_resolver},
    extract::{
        Accessibility, ClassBody, ConstBody, DeclBody, DeclFact, DeprecationOwned, EnumBody,
        FunctionBody, GenericParamOwned, InterfaceBody, LiteralOwned, MemberKind, MemberModifiers,
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
    let export_index: HashMap<&Path, &crate::typescript::extract::ExportTable> = modules
        .iter()
        .map(|m| (m.path.as_path(), &m.exports))
        .collect();

    // The same `.d.ts`-first, `extension_alias`-aware resolver `graph.rs` used
    // to walk import edges (and therefore to decide what actually got
    // `declare()`d). Re-export resolution must use the identical resolver, not
    // a second, independent one — see `resolve_module_path`'s doc comment for
    // the real-package failure this fixes.
    let resolver = make_resolver();

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
                // The whole file, not `0..0`. `module.module_name` keeps
                // only the file's last path segment
                // (`specifier_to_module_name`), so two files sharing a
                // basename in different directories (real case: rxjs's
                // `internal/observable/combineLatest.d.ts` and
                // `internal/operators/combineLatest.d.ts`) collide on the
                // seal's base identity key `(Module, [], name)` — every
                // `Module` entry is declared with `parent: None`, so the
                // ancestor path is always empty regardless of directory.
                // A degenerate `0..0` span made every such collision
                // identical on the `Span` tier too, forcing escalation to
                // order-dependent `Ordinal`; a real whole-file span breaks
                // the tie unless two colliding files happen to be exactly
                // the same byte length.
                span: 0..module.source_len,
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
            emit_decl(
                decl,
                Some(module_id.clone()),
                out,
                &module_index,
                &export_index,
                &resolver,
            );
        }

        // Emit re-exports from the export table.
        emit_reexports(module, Some(module_id.clone()), out, &export_index, &resolver);
    }
}

// ── Module root TsId ──────────────────────────────────────────────────────────

/// Sentinel name marking a `TsId` as a module root (`module_ts_id`), never a
/// real declaration. `decl_ts_id` compares against this to tell "my parent is
/// the module itself" (no qualification needed) apart from "my parent is a
/// real enclosing namespace" (qualify).
const MODULE_ROOT_NAME: &str = "$module";

fn module_ts_id(module: &ModuleFacts) -> TsId {
    TsId::new(module.path.clone(), MODULE_ROOT_NAME, 0)
}

/// Build the `TsId` for one declaration, qualified by its enclosing
/// *namespace* chain (never by its enclosing module).
///
/// TypeScript scopes a name to its immediate namespace, not to the whole
/// file: two sibling namespaces (or namespaces nested at any depth) may each
/// declare a member with the same local name without colliding. Two real
/// examples hit by the npm corpus sweep:
/// - `zod`'s `lib/helpers/util.d.ts` declares both `namespace objectUtil {
///   type identity = ... }` and `namespace util { type identity = ... }` —
///   distinct types that happen to share the name `identity`.
/// - `@types/node`'s `fs.d.ts` declares `namespace readFile { function
///   __promisify__(...) }`, `namespace writeFile { function __promisify__(...) }`,
///   … one `__promisify__` per overloaded top-level function, by Node's own
///   `util.promisify` typing convention (dozens of these per file).
///
/// Before this function existed, `emit_decl` built every id as
/// `TsId::new(decl.module, decl.name, decl.decl_index)` regardless of
/// nesting, so every one of those same-named siblings collapsed onto the
/// identical `(module, name, discriminant)` triple and `Lowering::finish`
/// rejected the whole package as "declared more than once" — the single
/// largest cause of real `.d.ts`-heavy npm packages failing to lower at all.
///
/// Top-level declarations are deliberately unaffected: their `parent` is the
/// module root (`module_ts_id`, sentinel name `MODULE_ROOT_NAME`), so
/// `id.name` stays exactly `decl.name` for them. That has to hold, because
/// re-export resolution (`emit_reexports`/`emit_inline_reexport`)
/// independently constructs `TsId::new(module, export_name, 0)` for a
/// top-level export and needs it to match exactly what got declared here —
/// qualifying top-level ids too would silently break every re-export lookup
/// in the 17 fixtures that already lower cleanly.
fn decl_ts_id(decl: &DeclFact, parent: Option<&TsId>) -> TsId {
    match parent {
        Some(p) if p.name != MODULE_ROOT_NAME => TsId::new(
            decl.module.clone(),
            format!("{}::{}", p.name, decl.name),
            decl.decl_index,
        ),
        _ => TsId::new(decl.module.clone(), decl.name.clone(), decl.decl_index),
    }
}

// ── Per-declaration emission ──────────────────────────────────────────────────

fn emit_decl(
    decl: &DeclFact,
    parent: Option<TsId>,
    out: &mut Lowering<TsId>,
    module_index: &HashMap<&Path, &str>,
    export_index: &HashMap<&Path, &crate::typescript::extract::ExportTable>,
    resolver: &Resolver,
) {
    let id = decl_ts_id(decl, parent.as_ref());
    let sym = make_sym(decl);

    match &decl.body {
        DeclBody::Interface(body) => emit_interface(id, parent, sym, body, out),
        DeclBody::Class(body) => emit_class(id, parent, sym, body, out),
        DeclBody::TypeAlias(body) => emit_type_alias(id, parent, sym, body, out),
        DeclBody::Enum(body) => emit_enum(id, parent, sym, body, out),
        DeclBody::Namespace(body) => {
            emit_namespace(id, parent, sym, body, out, module_index, export_index, resolver);
        }
        DeclBody::Function(body) => emit_function(id, parent, sym, body, out),
        DeclBody::Const(body) => emit_const(id, parent, sym, body, out),
        DeclBody::Static(body) => emit_static(id, parent, sym, body, out),
        DeclBody::Reexport {
            module_request,
            import_name,
        } => {
            // Re-export inline: handled here and in emit_reexports for table-driven ones.
            emit_inline_reexport(
                id,
                parent,
                sym,
                module_request,
                import_name,
                out,
                module_index,
                resolver,
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
        Trait::builder().generics(generics).supers(supers).build(),
    );

    // Emit methods as child Function entries.
    //
    // `idx` (not a hardcoded 0) is the discriminant: an interface signature
    // list can declare the same method name more than once — TypeScript
    // overload signatures (e.g. `refine(check): this; refine(check, msg):
    // this;`) each become their own `MethodFact` with an identical `name`.
    // A shared discriminant made every overload's `method_id` compare equal,
    // which `Lowering::finish` then rejected wholesale as "declared more
    // than once" — this was the majority cause of real `.d.ts`-heavy npm
    // packages (zod, commander, class-validator, reflect-metadata, dayjs)
    // failing to lower at all. `emit_class`'s sibling loop already avoids
    // this (it multiplies member index by overload index); interfaces need
    // the same treatment.
    for (idx, method) in body.methods.iter().enumerate() {
        let method_id = TsId::new(
            id.module.clone(),
            format!("{}::{}", id.name, method.name),
            idx as u32,
        );
        let method_sym = Symbol {
            name: method.name.clone(),
            visibility: accessibility_to_visibility(method.modifiers.accessibility),
            documentation: method.doc.doc.clone().unwrap_or_default(),
            source: id.module.clone(),
            span: (method.sig.span_start as usize)..(method.sig.span_end as usize),
            aliases: Box::new([]),
            deprecation: method.doc.deprecation.clone().map(DeprecationOwned::into_ir),
            doc_links: Box::new([]),
            attrs: Box::new([]),
            cfg: None,
        };
        emit_function(method_id, Some(id.clone()), method_sym, &method.sig, out);
    }

    // Emit properties as child Field entries.
    for prop in &body.properties {
        let prop_id = TsId::new(id.module.clone(), format!("{}::{}", id.name, prop.name), 0);
        let prop_sym = Symbol {
            name: prop.name.clone(),
            visibility: accessibility_to_visibility(prop.modifiers.accessibility),
            documentation: prop.doc.doc.clone().unwrap_or_default(),
            source: id.module.clone(),
            span: (prop.span_start as usize)..(prop.span_end as usize),
            aliases: Box::new([]),
            deprecation: prop.doc.deprecation.clone().map(DeprecationOwned::into_ir),
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
            name: if idx_num == 0 {
                "__index".to_string()
            } else {
                format!("__index_{idx_num}")
            },
            visibility: Visibility::Public,
            documentation: String::new(),
            source: id.module.clone(),
            span: (idx_sig.span_start as usize)..(idx_sig.span_end as usize),
            aliases: Box::new([]),
            deprecation: None,
            doc_links: Box::new([]),
            attrs: Box::new([]),
            cfg: None,
        };
        // Emit as a Function with the key as param and value as return type.
        // Both the synthetic param and the synthetic function reuse the real
        // `TSIndexSignature` span (`idx_sig.span_start/end`) — there is no
        // narrower real span for a param or return type this producer
        // extracts (`TypeOwned` carries no span of its own; see
        // `extract/mod.rs`'s `TypeOwned` doc comment), but the whole
        // `[k: string]: T` node's real bytes are honest and available.
        let key_param_body = crate::typescript::extract::ParamFact {
            name: idx_sig.key_name.clone(),
            ty: Some(idx_sig.key_ty.clone()),
            is_optional: false,
            is_rest: false,
            is_readonly: false,
            span_start: idx_sig.span_start,
            span_end: idx_sig.span_end,
        };
        let index_fn_body = FunctionBody {
            generics: Vec::new(),
            params: vec![key_param_body],
            return_type: Some(idx_sig.value_ty.clone()),
            is_async: false,
            is_generator: false,
            has_body: false,
            receiver: crate::typescript::extract::ReceiverKind::SharedRef,
            span_start: idx_sig.span_start,
            span_end: idx_sig.span_end,
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
            name: if cs_num == 0 {
                "new".to_string()
            } else {
                format!("new_{cs_num}")
            },
            visibility: Visibility::Public,
            documentation: String::new(),
            source: id.module.clone(),
            span: (cs.span_start as usize)..(cs.span_end as usize),
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
                    format!("{}::{}", id.name, member.name),
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
                    span: (member.span_start as usize)..(member.span_end as usize),
                    aliases: Box::new([]),
                    deprecation: member.doc.deprecation.clone().map(DeprecationOwned::into_ir),
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
                        format!("{}::{}", id.name, member.name),
                        (idx * 1000 + overload_idx) as u32,
                    );
                    let method_sym = Symbol {
                        name: member.name.clone(),
                        visibility: accessibility_to_visibility(member.modifiers.accessibility),
                        documentation: member.doc.doc.clone().unwrap_or_default(),
                        source: id.module.clone(),
                        // `member.span_start/end`, not `sig`'s: each
                        // `MethodDefinition` AST node produces exactly one
                        // `MemberFact` with a singleton `Vec<FunctionBody>`
                        // (class-method overloads are separate top-level
                        // `MethodDefinition`s, each its own `MemberFact`), so
                        // the member's own span is the whole method
                        // declaration including its name — `sig`'s span
                        // (the `Function` value node) omits the name.
                        span: (member.span_start as usize)..(member.span_end as usize),
                        aliases: Box::new([]),
                        deprecation: member.doc.deprecation.clone().map(DeprecationOwned::into_ir),
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
                    format!("{}::{}", id.name, member.name),
                    (idx * 1000) as u32,
                );
                let method_sym = Symbol {
                    name: member.name.clone(),
                    visibility: accessibility_to_visibility(member.modifiers.accessibility),
                    documentation: member.doc.doc.clone().unwrap_or_default(),
                    source: id.module.clone(),
                    span: (member.span_start as usize)..(member.span_end as usize),
                    aliases: Box::new([]),
                    deprecation: member.doc.deprecation.clone().map(DeprecationOwned::into_ir),
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
                    format!("{}::{}", id.name, block_name),
                    idx as u32,
                );
                let sb_sym = Symbol {
                    name: block_name.clone(),
                    visibility: Visibility::Private,
                    documentation: String::new(),
                    source: id.module.clone(),
                    span: (member.span_start as usize)..(member.span_end as usize),
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
                    span_start: member.span_start,
                    span_end: member.span_end,
                };
                emit_function(sb_id, Some(id.clone()), sb_sym, &static_fn_body, out);
            }
            // Fields / Accessors were already emitted above.
            MemberKind::Property { .. } | MemberKind::Accessor { .. } => {}
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
        Alias::builder().target(target).generics(generics).build(),
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
        let variant_id = TsId::new(
            id.module.clone(),
            format!("{}::{}", id.name, v.name),
            idx as u32,
        );
        let vref: Ref<Variant> = out.refer(variant_id.clone());
        variant_refs.push(vref);

        let variant_sym = Symbol {
            name: v.name.clone(),
            visibility: Visibility::Public,
            documentation: String::new(),
            source: id.module.clone(),
            span: (v.span_start as usize)..(v.span_end as usize),
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
    export_index: &HashMap<&Path, &crate::typescript::extract::ExportTable>,
    resolver: &Resolver,
) {
    let _: Ref<Module> = out.declare(id.clone(), parent, sym, Module);

    for child in &body.children {
        emit_decl(child, Some(id.clone()), out, module_index, export_index, resolver);
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
    // Carries each param's real byte span (`ParamFact::span_start/end`)
    // alongside its `Param` payload, so the symbol declared below can use it
    // instead of a degenerate `0..0`.
    let mut param_ids: Vec<(TsId, Param, u32, u32)> = Vec::with_capacity(body.params.len());

    for (idx, p) in body.params.iter().enumerate() {
        // The discriminant must fold in `id.discriminant`, not just the
        // local param index: `id.discriminant` is how the caller
        // distinguishes sibling overloads that share `id.name` (e.g.
        // `emit_class`'s `idx*1000+overload_idx`, or this function's own
        // caller when `id` came from a module-level overload group via
        // `decl.decl_index`). Two overloads whose first parameter happens
        // to share a name would otherwise both mint
        // `TsId(module, "<name>::param::<param>", idx)` and collide in
        // `Lowering::finish` as "declared more than once" even though the
        // functions themselves were correctly disambiguated.
        let param_id = TsId::new(
            id.module.clone(),
            format!("{}::param::{}", id.name, p.name),
            id.discriminant.saturating_mul(1000).saturating_add(idx as u32),
        );
        let pref: Ref<Param> = out.refer(param_id.clone());
        param_refs.push(pref);

        let mut attrs: Vec<ParamAttribute> = Vec::new();
        if p.is_optional {
            attrs.push(ParamAttribute::Optional);
        }
        if p.is_rest {
            attrs.push(ParamAttribute::Variadic);
        }
        param_ids.push((
            param_id,
            Param::builder()
                .maybe_ty(p.ty.as_ref().map(lower_type))
                .attributes(attrs)
                .build(),
            p.span_start,
            p.span_end,
        ));
    }

    // Output parameter: return type, if any.
    let mut output_refs: Vec<Ref<Param>> = Vec::new();
    if let Some(ret) = &body.return_type {
        // Fold `id.discriminant` in rather than hardcoding 0, for the same
        // reason as the param loop above: two overloads sharing `id.name`
        // both have a return type, so a shared discriminant made every
        // overload's `<name>::return` collide as "declared more than once"
        // (observed on real fixtures: `axios`'s `AxiosHeaders::set`,
        // `commander`'s `Command::version`, and every overloaded interface
        // method once the `emit_interface` fix above stopped masking it).
        let ret_id = TsId::new(
            id.module.clone(),
            format!("{}::return", id.name),
            id.discriminant,
        );
        let rref: Ref<Param> = out.refer(ret_id.clone());
        output_refs.push(rref);
        let param_sym = Symbol {
            name: "return".to_string(),
            visibility: Visibility::Public,
            documentation: String::new(),
            source: id.module.clone(),
            // Synthetic entry: `TypeOwned` carries no span of its own (see
            // `extract/mod.rs`), so there is no independent byte range for
            // "just the return type". The enclosing declaration's span is
            // real, honest bytes that do contain the return type, not a
            // precise sub-span of it — a superset, not a wrong range.
            span: (body.span_start as usize)..(body.span_end as usize),
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
    for (param_id, param, span_start, span_end) in param_ids {
        let param_sym = Symbol {
            name: param_name_from_id(&param_id.name),
            visibility: Visibility::Public,
            documentation: String::new(),
            source: id.module.clone(),
            span: (span_start as usize)..(span_end as usize),
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
    // The declaration exists; the type annotation does not. `const x = 1` is
    // not the same claim as `const x: any = 1`.
    let ty = body.ty.as_ref().map_or(Type::UNANNOTATED, lower_type);
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
    // The declaration exists; the type annotation does not. `const x = 1` is
    // not the same claim as `const x: any = 1`.
    let ty = body.ty.as_ref().map_or(Type::UNANNOTATED, lower_type);
    let _: Ref<Static> = out.declare(
        id,
        parent,
        sym,
        Static::builder().ty(ty).mutable(body.is_mutable).build(),
    );
}

// ── Re-exports ─────────────────────────────────────────────────────────────────

/// Resolve `name`, as exported *without* a `from` clause by the module at
/// `table_path` (whose export surface is `table`), to the `TsId`(s) a
/// `refer()` elsewhere should target — **plural**, because `name` can name
/// an *overloaded* top-level declaration in the target module. Each
/// overload is its own IR entry (`TsId`'s `discriminant`; see `id.rs`), and
/// a re-export must be able to reach all of them, not just the first.
///
/// This used to return a single `TsId::new(table_path, name, 0)` — a
/// hardcoded `0` — which is where `LocalExport::Named`'s `overload_count`
/// used to be lost: `resolve_export_target` had no way to know whether
/// `local_name` was declared once or many times, so it could only ever
/// point at the first declaration in source order. Confirmed cost on a real
/// package: rxjs's public root (`dist/types/index.d.ts`) does `export {
/// combineLatest } from './internal/observable/combineLatest';`, and
/// `combineLatest` is declared 13 times (real TS overloads) in that file —
/// every consumer reaching `combineLatest` through the package root used to
/// see only the first signature.
///
/// Both re-export paths below (`export { x } from "m"` and `export * from
/// "m"`) used to build `TsId::new(table_path, name, 0)` directly — correct
/// only when the target module's externally-visible name and its own
/// `declare()`d name coincide. Real packages break that assumption two
/// ways; see [`crate::typescript::extract::LocalExport`]'s doc comment. This is the one
/// place both paths resolve through, so a package that reaches the same
/// renamed or namespace-import export via either form resolves identically
/// instead of one path working and the other dangling.
///
/// Returns an empty `Vec` when nothing should be referred at all: either
/// `name` resolves to a value with no identifier (`LocalExport::Unresolvable`
/// — e.g. `export default "literal";`), or the target module's own
/// re-export chain (`resolve_module_path`) doesn't resolve. When `name` is
/// absent from `table.locals` entirely, it is a genuine indirect re-export
/// (`export { x } from "m2"` inside the target module) — using `name`
/// unchanged is *correct* there, not a fallback guess, because the target
/// module's own `emit_reexports` call declares it under that exact alias
/// via `declare_ref`. That chained case is always single-valued: a
/// downstream `emit_reexports` call always declares its own reexport id
/// under discriminant `0` (see the call sites below), regardless of the
/// original declaration's discriminant, so there is nothing to fan out here
/// — the fan-out already happened, if at all, at the module that owns the
/// real declaration.
fn resolve_export_target(
    resolver: &Resolver,
    table_path: &Path,
    table: &crate::typescript::extract::ExportTable,
    name: &str,
) -> Vec<TsId> {
    use crate::typescript::extract::LocalExport;

    match table.locals.get(name) {
        Some(LocalExport::Named { local_name, overload_count }) => (0..*overload_count)
            .map(|discriminant| TsId::new(table_path.to_path_buf(), local_name.clone(), discriminant))
            .collect(),
        Some(LocalExport::NamespaceOf(module_request)) => {
            resolve_module_path(resolver, table_path, module_request)
                .map(|ns_path| vec![TsId::new(ns_path, MODULE_ROOT_NAME, 0)])
                .unwrap_or_default()
        }
        Some(LocalExport::Unresolvable) => Vec::new(),
        None => vec![TsId::new(table_path.to_path_buf(), name.to_string(), 0)],
    }
}

fn emit_reexports(
    module: &ModuleFacts,
    parent: Option<TsId>,
    out: &mut Lowering<TsId>,
    export_index: &HashMap<&Path, &crate::typescript::extract::ExportTable>,
    resolver: &Resolver,
) {
    // One module can only ever declare one `TsId::new(module.path, name, 0)`
    // for its own re-export surface — both loops below write into that same
    // namespace — so a name already fanned out (by either loop) must not be
    // declared a second time. Real packages hit this: `date-fns`'s
    // `format.d.mts` AND `parse.d.mts` each independently `import {
    // longFormatters } from "./_lib/format/longFormatters.js"; export {
    // longFormatters };`, and `index.d.mts` star-exports both `format.js`
    // *and* `parse.js` — so `longFormatters` is a genuinely reachable name via
    // two convergent barrels pointing at the exact same underlying
    // declaration. Without dedup, the second `declare_ref` for the identical
    // `(module, "longFormatters", 0)` id is rejected by `Lowering::finish` as
    // "declared more than once", failing the whole package over a re-export
    // that is not actually ambiguous — both paths name the same target.
    let mut reexported: std::collections::HashSet<String> = std::collections::HashSet::new();

    // Named re-exports: `export { Foo } from "m"`.
    for indirect in &module.exports.indirect {
        let target_path = resolve_module_path(resolver, &module.path, &indirect.module_request);
        let target_name = &indirect.import_name;
        let export_name = &indirect.export_name;

        if !reexported.insert(export_name.clone()) {
            continue;
        }

        // The target ID(s) in the source module — resolved against *its own*
        // export surface (`resolve_export_target`), since `target_name` is
        // the name `m` exports this under, not necessarily what `m` itself
        // `declare()`d it as (a bare rename or namespace-import passthrough
        // inside `m`; see that function's doc comment). `uuid`'s
        // `dist/esm-node/index.js` hits this directly: `export { default as
        // v1 } from "./v1.js"` needs `v1.js`'s "default" resolved to the
        // `v1` function it actually declares. Plural because `target_name`
        // can be an overloaded declaration (`resolve_export_target`'s doc
        // comment) — every overload gets its own reexport entry below.
        let target_ids: Vec<TsId> = target_path
            .as_ref()
            .map(|p| {
                export_index.get(p.as_path()).map_or_else(
                    || vec![TsId::new(p.clone(), target_name, 0)],
                    |target_table| resolve_export_target(resolver, p, target_table, target_name),
                )
            })
            .unwrap_or_default();

        let sym = Symbol {
            name: export_name.clone(),
            visibility: Visibility::Public,
            documentation: String::new(),
            source: module.path.clone(),
            // Real span of the whole `export { .. } from "m";` statement
            // (`ExportEntry::statement_span`, threaded through as
            // `IndirectExport::span_start/end`), not `0..0`.
            span: (indirect.span_start as usize)..(indirect.span_end as usize),
            aliases: Box::new([]),
            deprecation: None,
            doc_links: Box::new([]),
            attrs: Box::new([]),
            cfg: None,
        };

        // The reexport ID in the current module: one per resolved overload,
        // discriminant `0..target_ids.len()` — mirroring how `emit_class`'s
        // method loop and `emit_interface`'s method loop each give sibling
        // overloads distinct discriminants under a shared name, so this is
        // consistent with how overloads are represented everywhere else in
        // this producer, not a special case invented for re-exports.
        for (discriminant, tid) in target_ids.into_iter().enumerate() {
            let reexport_id = TsId::new(module.path.clone(), export_name, discriminant as u32);
            let target_ref: Ref<Module> = out.refer(tid);
            let _: Ref<Module> =
                out.declare_ref(reexport_id, parent.clone(), sym.clone(), target_ref);
        }
    }

    // Star re-exports: `export * from "m"`.
    for star in &module.exports.star {
        let target_path = resolve_module_path(resolver, &module.path, &star.module_request);
        let Some(tp) = target_path else { continue };

        // Fan out to all names the target module exports. Each name is
        // resolved against the target's own export surface
        // (`resolve_export_target`) rather than assumed to be its own
        // `declare()`d name — zod's `types.d.ts` fans through
        // `export { anyType as any, ..., voidType as void }` and date-fns's
        // `format.d.ts` fans through `export { format as formatDate }`,
        // both bare local renames the target module itself declares under
        // the *pre*-rename name.
        if let Some(target_table) = export_index.get(tp.as_path()) {
            for export_name in &target_table.exported_names {
                if !reexported.insert(export_name.clone()) {
                    continue;
                }
                let target_ids =
                    resolve_export_target(resolver, tp.as_path(), target_table, export_name);
                if target_ids.is_empty() {
                    continue;
                }
                let sym = Symbol {
                    name: export_name.clone(),
                    visibility: Visibility::Public,
                    documentation: String::new(),
                    source: module.path.clone(),
                    // Real span of the `export * from "m";` statement
                    // (`StarExport::span_start/end`). Every name fanned out
                    // from this one statement shares it — see
                    // `StarExport`'s doc comment for why that cannot
                    // introduce a collision (the identity key includes the
                    // name, which differs per fanned-out entry).
                    span: (star.span_start as usize)..(star.span_end as usize),
                    aliases: Box::new([]),
                    deprecation: None,
                    doc_links: Box::new([]),
                    attrs: Box::new([]),
                    cfg: None,
                };
                // One reexport entry per resolved overload — see the named
                // re-export loop above for why this mirrors
                // `emit_class`/`emit_interface`'s own overload-discriminant
                // convention rather than inventing a new one.
                for (discriminant, target_id) in target_ids.into_iter().enumerate() {
                    let reexport_id =
                        TsId::new(module.path.clone(), export_name, discriminant as u32);
                    let target_ref: Ref<Module> = out.refer(target_id);
                    let _: Ref<Module> =
                        out.declare_ref(reexport_id, parent.clone(), sym.clone(), target_ref);
                }
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
    resolver: &Resolver,
) {
    let target_path = resolve_module_path(resolver, &id.module, module_request);
    if let Some(tp) = target_path {
        let target_id = TsId::new(tp, import_name, 0);
        let target_ref: Ref<Module> = out.refer(target_id);
        let _: Ref<Module> = out.declare_ref(id, parent, sym, target_ref);
    }
}

/// Given the current module's absolute path and a re-export specifier,
/// return the absolute path of the target module — via the *same* resolver
/// `graph.rs` uses to walk import edges, not an independent one.
///
/// # Why this used to be a hand-rolled extension guesser, and why that broke
///
/// This function used to probe the filesystem directly with a fixed
/// extension-priority list (`.ts`, `.tsx`, `.d.ts`, `.js`, …), *except* when
/// the specifier already contained a literal extension (e.g.
/// `'./vendor/ansi-styles/index.js'`) — in that case it returned the literal
/// path unchanged the moment `path.is_file()` was true, without ever
/// considering a co-located `.d.ts` twin.
///
/// That is backwards from both TypeScript's own resolution and from what
/// `graph.rs`'s resolver (`entry::make_resolver`, `extension_alias: ".js" ->
/// [".d.ts", ".ts", ".js"]`) actually does when it walks the same edge as an
/// `import`. Two real npm packages hit the mismatch: `chalk`'s
/// `source/index.d.ts` re-exports `ModifierName` etc. `from
/// './vendor/ansi-styles/index.js'`, and `date-fns`'s internal modules
/// re-export `formatters` etc. `from './formatters.js'` — both packages ship
/// a twin `index.d.ts`/`formatters.d.ts` right next to the `.js` file, and
/// *that* is where the real declaration lives; `graph.rs` correctly walked
/// there and declared it, while the naive resolver here kept literally
/// returning the `.js` path. Every `refer()` built from its result therefore
/// pointed at a `TsId` nothing had ever `declare()`d, and `Lowering::finish`
/// rejected the whole package as "referred but never declared". Reusing the
/// one resolver both call sites need makes the mismatch structurally
/// impossible instead of a fact both files had to independently get right.
fn resolve_module_path(resolver: &Resolver, current: &Path, specifier: &str) -> Option<PathBuf> {
    if !specifier.starts_with('.') {
        return None;
    }
    let parent_dir = current.parent()?;
    let resolved = resolver.resolve(parent_dir, specifier).ok()?.into_path_buf();
    if resolved.is_file() && is_ts_module_path(&resolved) {
        Some(resolved)
    } else {
        None
    }
}

// ── Type lowering: TypeOwned → IR Type ────────────────────────────────────────

pub(crate) fn lower_type(ty: &TypeOwned) -> Type {
    match ty {
        // TypeScript is the language that makes the `Any` / `Unknown` split
        // unarguable, because it ships both and they are *not* interchangeable:
        //
        // - `unknown` is the genuine top type. Every value is assignable to it
        //   and no member access or narrowing is permitted until you refine it.
        // - `any` is the escape hatch. It is assignable in both directions and
        //   disables checking entirely — `--noImplicitAny` exists precisely
        //   because it is the thing you want to find and remove.
        //
        // Lowering both to `Type::Any` made a hardened `unknown` signature and
        // an unchecked `any` signature byte-identical in the IR.
        TypeOwned::Unknown => Type::Any,
        TypeOwned::Any => Type::DYNAMIC,
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
        TypeOwned::Primitive(s) | TypeOwned::Unsupported(s) => {
            Type::Primitive(Primitive::Builtin(s.clone()))
        }
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
        TypeOwned::Union(arms) => Type::Union(
            arms.iter()
                .map(lower_type)
                .collect::<Vec<_>>()
                .into_boxed_slice(),
        ),
        TypeOwned::Intersection(arms) => Type::Intersection(
            arms.iter()
                .map(lower_type)
                .collect::<Vec<_>>()
                .into_boxed_slice(),
        ),
        TypeOwned::Tuple(members) => {
            let elems: Vec<TupleElement> = members
                .iter()
                .map(|m| match m {
                    TypeOwned::NamedTupleElem { label, ty } => TupleElement::Named {
                        label: label.clone(),
                        ty: lower_type(ty),
                    },
                    other => TupleElement::Positional(lower_type(other)),
                })
                .collect();
            Type::Tuple(elems.into_boxed_slice())
        }
        TypeOwned::Array(inner) => {
            // Model `T[]` as a Slice.
            Type::Slice(Box::new(lower_type(inner)))
        }
        // ── Named tuple element (item 10) ─────────────────────────────────
        // When NamedTupleElem appears outside a Tuple context (the
        // TSType::TSNamedTupleMember top-level path in types.rs), wrap it in a
        // single-element named tuple so the label is not lost.  The normal path
        // is through TypeOwned::Tuple where each element is matched above.
        TypeOwned::NamedTupleElem { label, ty } => Type::Tuple(
            [TupleElement::Named {
                label: label.clone(),
                ty: lower_type(ty),
            }]
            .into(),
        ),
        // ── Function / constructor types (item: FunctionPointer) ──────────
        // TypeOwned::Function was previously lowered to
        // Primitive::Builtin("Function"), losing all signature information.
        // Type::FunctionPointer now exists in nudox-ir; use it.
        TypeOwned::Function(body) => {
            let params: Vec<Type> = body
                .params
                .iter()
                // A parameter of a function *type* with no annotation. TS
                // treats it as implicit-any, but the source did not write
                // `any` — under `--noImplicitAny` this is an error, and the
                // IR must be able to tell the two apart.
                .map(|p| p.ty.as_ref().map_or(Type::UNANNOTATED, lower_type))
                .collect();
            let ret = body.return_type.as_ref().map(|r| Box::new(lower_type(r)));
            // TypeScript functions are always managed; no ABI.
            Type::FunctionPointer {
                params: params.into_boxed_slice(),
                ret,
                abi: None,
            }
        }
        TypeOwned::Literal(lit) => lower_literal(lit),
        TypeOwned::Conditional {
            check,
            extends_ty,
            then_ty,
            else_ty,
        } => Type::Conditional {
            check: Box::new(lower_type(check)),
            extends_ty: Box::new(lower_type(extends_ty)),
            then_ty: Box::new(lower_type(then_ty)),
            else_ty: Box::new(lower_type(else_ty)),
        },
        TypeOwned::Mapped {
            key_var,
            source,
            value,
            readonly,
            optional,
        } => Type::Mapped {
            key_var: key_var.clone(),
            source: Box::new(lower_type(source)),
            value: Box::new(lower_type(value)),
            readonly: *readonly,
            optional: *optional,
        },
        TypeOwned::TemplateLiteral(parts) => {
            use nudox_ir::kinds::ty::TemplatePart as IrPart;
            let ir_parts: Vec<IrPart> = parts
                .iter()
                .map(|p| match p {
                    crate::typescript::extract::TemplatePart::Literal(s) => IrPart::Literal(s.clone()),
                    crate::typescript::extract::TemplatePart::Interpolated(ty) => {
                        IrPart::Interpolated(Box::new(lower_type(ty)))
                    }
                })
                .collect();
            Type::TemplateLiteral(ir_parts.into_boxed_slice())
        }
        TypeOwned::ObjectLiteral(members) => {
            use nudox_ir::kinds::ty::{AnonField, AnonRecordForm};
            let ir_members: Vec<AnonField> = members
                .iter()
                .map(|m| AnonField {
                    name: m.name.clone(),
                    ty: lower_type(&m.ty),
                    optional: m.optional,
                    readonly: m.readonly,
                })
                .collect();
            Type::AnonymousRecord {
                form: AnonRecordForm::Struct,
                members: ir_members.into_boxed_slice(),
            }
        }
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
            bounds: p
                .bounds
                .iter()
                .map(lower_type)
                .collect::<Vec<_>>()
                .into_boxed_slice(),
            default: p.default.as_ref().map(lower_type),
            // Wire TS 4.7+ `in`/`out` declaration-site variance annotations.
            // `None` when no modifier was present (the common case).
            variance: p.variance,
        })
        .collect()
}

// ── Symbol construction ────────────────────────────────────────────────────────

fn make_sym(decl: &DeclFact) -> Symbol {
    Symbol {
        name: decl.name.clone(),
        visibility: decl.visibility,
        documentation: decl.doc.doc.clone().unwrap_or_default(),
        source: decl.module.clone(),
        span: (decl.span_start as usize)..(decl.span_end as usize),
        aliases: Box::new([]),
        // DeprecationOwned is Clone; convert to IR's Deprecation which is not.
        deprecation: decl.doc.deprecation.clone().map(DeprecationOwned::into_ir),
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

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod cc2_tests {
    use super::*;
    use nudox_ir::kinds::UnknownType;

    /// TypeScript ships both spellings, and they are not interchangeable.
    ///
    /// `unknown` is the genuine top type — assignable *to* from everything,
    /// narrowable only by refinement. `any` is the escape hatch — assignable
    /// in both directions and check-suppressing. Lowering both to `Type::Any`
    /// made a hardened `unknown` signature and an unchecked `any` signature
    /// byte-identical in the IR, which is exactly the thing `--noImplicitAny`
    /// exists to let you find.
    #[test]
    fn unknown_is_the_top_type_and_any_is_the_escape_hatch() {
        let unknown = lower_type(&TypeOwned::Unknown);
        let any = lower_type(&TypeOwned::Any);
        assert_eq!(unknown, Type::Any, "`unknown` is TypeScript's top type");
        assert_eq!(
            any,
            Type::Unknown(UnknownType::DynamicallyTyped),
            "`any` is the gradual-typing escape hatch, not a top type"
        );
        assert_ne!(unknown, any, "`unknown` and `any` must not share an encoding");
    }

    /// An *implicit* any — a parameter with no annotation at all — is a third
    /// thing again, and the one `--noImplicitAny` reports.
    #[test]
    fn implicit_any_is_unannotated_not_written_any() {
        let unannotated = Type::UNANNOTATED;
        assert_ne!(
            unannotated,
            lower_type(&TypeOwned::Any),
            "`(x) => …` and `(x: any) => …` are different source"
        );
        assert_ne!(unannotated, lower_type(&TypeOwned::Unknown));
        assert_eq!(unannotated.to_string(), "?unannotated");
    }
}
