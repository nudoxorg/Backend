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
//! - Re-export chain resolution: `module_for_export` /
//!   `resolve_reexport_target`. For `export { Foo } from "m"` we emit a
//!   `declare_ref` pointing at the target module's entry.
//! - Star-export fan-out: for `export * from "m"` we emit re-exports for each
//!   name the target module exports.
//!
//! # Declare-set
//! One set, built from `ModuleFacts` before any `refer`. It is every id this
//! package will `declare` or `declare_ref`. A re-export target in the set is
//! `refer()`'d (the later `declare_ref` fills the slot). A name this package
//! will not declare is `refer_import`, never `refer` of an undeclared id.
//! Nominal lowering uses the same set: a name in it goes through
//! `Lowering::nominal`; a real generic parameter stays `Type::TypeVar`; an
//! unknown name stays unresolved.

use std::{
    collections::{HashMap, HashSet},
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
    vocab::{Confidence, ReferenceKind, RelSpan},
};

use nudox_ir::{
    change::{EcosystemId, PackageLineageId, PackageName},
    foreign::ForeignKey,
    lower::Lowering,
};
use oxc_resolver::Resolver;

use crate::typescript::{
    entry::{is_ts_module_path, make_resolver},
    extract::{
        Accessibility, AnonFieldOwned, AttrTok, ClassBody, ClassFlags, ConstBody, DeclBody,
        DeclFact, DeprecationOwned, DocFacts, EnumBody, FunctionBody, GenericParamOwned,
        IndexSignatureFact, InterfaceBody, LiteralOwned, MemberFact, MemberKind, MemberModifiers,
        MethodFact, ModuleFacts, NamespaceBody, OccurrenceKind, ParamFact, PropertyFact,
        ReceiverKind, StaticBody, TemplatePart, TypeAliasBody, TypeOwned, VariantFact,
    },
    id::TsId,
};

// ── Public entry point
// ─────────────────────────────────────────────────────────

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
    let twins = TwinPlan::build(modules);
    let declared = DeclareSet::build(modules, &twins, &export_index, &resolver);

    for module in modules {
        let module_id = module_ts_id(module, &twins);
        if twins.is_secondary(&module.path) {
            for decl in &module.declarations {
                if matches!(decl.body, DeclBody::Reexport { .. }) {
                    continue;
                }
                emit_decl(
                    decl,
                    Some(module_id.clone()),
                    out,
                    &module_index,
                    &export_index,
                    &resolver,
                    &declared,
                    &twins,
                );
            }
            continue;
        }

        // Declare the module itself as a `Module` kind.
        let _module_ref: Ref<Module> = out.declare(
            module_id.clone(),
            None,
            Symbol {
                name: module.module_name.clone(),
                visibility: Visibility::Public,
                documentation: module.module_doc.clone().unwrap_or_default(),
                source: module.path.clone(),
                aliases: twins.module_aliases(&module.path),
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
                deprecation: None,
                doc_links: Box::new([]),
                attrs: Box::new([]),
                cfg: None,
            },
            Module,
        );

        for decl in &module.declarations {
            if matches!(decl.body, DeclBody::Reexport { .. }) {
                continue;
            }
            emit_decl(
                decl,
                Some(module_id.clone()),
                out,
                &module_index,
                &export_index,
                &resolver,
                &declared,
                &twins,
            );
        }
    }

    // Pass 2: re-exports. Targets in the declare-set were reserved before
    // any `refer`, so a barrel can point at a later `declare_ref`.
    for module in modules.iter() {
        let module_id = module_ts_id(module, &twins);
        for decl in &module.declarations {
            if matches!(decl.body, DeclBody::Reexport { .. }) {
                emit_decl(
                    decl,
                    Some(module_id.clone()),
                    out,
                    &module_index,
                    &export_index,
                    &resolver,
                    &declared,
                    &twins,
                );
            }
        }
        emit_reexports(
            module,
            Some(module_id.clone()),
            out,
            &export_index,
            &resolver,
            &declared,
            &twins,
        );
        emit_default_alias(module, Some(module_id), out, &declared, &twins);
    }

    // ── Same-module occurrence graph ─────────────────────────────────────────
    // A deliberate second pass over every module, run only after the loop
    // above has `declare()`d every declaration in the package: mirrors
    // `RustProducer::lower` (`rust/mod.rs`), which likewise defers all
    // `record_occurrence` calls until `lower_workspace` has finished
    // declaring, rather than interleaving them with the first pass. Nothing
    // here actually needs a *different* module's declarations — every edge
    // in `module.occurrences` is same-module by construction (see
    // `extract::OccurrenceFact`'s doc comment) — but recomputing `decl_ids`
    // per module here, instead of trying to carry it out of the loop above,
    // keeps that one-pass-per-module loop free of a second piece of
    // bookkeeping it doesn't otherwise need.
    for module in modules {
        let module_id = module_ts_id(module, &twins);
        let decl_ids: Vec<TsId> = module
            .declarations
            .iter()
            .map(|decl| decl_ts_id(decl, Some(&module_id), &twins))
            .collect();

        for occ in &module.occurrences {
            let owner_id = decl_ids[occ.owner].clone();
            let target_id = decl_ids[occ.target].clone();
            let owner_decl = &module.declarations[occ.owner];
            let (kind, confidence) = occurrence_kind_and_confidence(occ.kind);
            // `RelSpan` is relative to the *owner*'s own span start (see its
            // doc comment in `nudox_ir::vocab`), not an absolute file offset
            // — `occ.span_start/end` are absolute (OXC bytes), so they are
            // rebased here rather than in extraction, where the owner's span
            // is not yet in scope. `saturating_sub` rather than plain `-`:
            // the reference site is only ever inside the owner's span by the
            // containment check `record_occurrences` already performed, so
            // this cannot actually underflow, but a doctrine-clean producer
            // does not reach for `unwrap`/`expect` to say so.
            let span = RelSpan::new(
                occ.span_start.saturating_sub(owner_decl.span_start),
                occ.span_end.saturating_sub(owner_decl.span_start),
            );
            out.record_occurrence(owner_id, target_id, kind, confidence, span);
        }
    }
}

/// Map an extraction-level [`OccurrenceKind`] onto the IR's
/// `nudox_ir::vocab::ReferenceKind` and the [`Confidence`] it earns.
///
/// A call and a plain value use are both graded `Confidence::Oracle`: OXC's
/// `Semantic` resolved the identifier to its declaring symbol through real
/// lexical scope analysis — a binder, in compiler terms — the same kind of
/// resolution `RustProducer` earns `Oracle` for via rust-analyzer's HIR. It
/// is exact name resolution, not a heuristic guess.
///
/// A type-position reference is graded one tier lower, `Confidence::Index`:
/// TypeScript lets an interface, a class, and a namespace all merge under one
/// name, and each merged declaration becomes its own `DeclFact`/`TsId` here
/// (see `id.rs`'s doc comment). OXC's binder resolves a type reference to
/// *a* declaring symbol span without knowing which sibling of that merged
/// group the author meant as "the type" — there is no type-checking pass
/// here to disambiguate, only the same name-in-scope resolution a call site
/// gets. A call target is exactly one function; a type target can silently
/// land on the wrong sibling of a merged declaration group, which is real,
/// weaker ground than a call.
fn occurrence_kind_and_confidence(kind: OccurrenceKind) -> (ReferenceKind, Confidence) {
    match kind {
        OccurrenceKind::Call => (ReferenceKind::FunctionCall, Confidence::Oracle),
        OccurrenceKind::Type => (ReferenceKind::TypeReference, Confidence::Index),
        OccurrenceKind::ValueUse => (ReferenceKind::VariableUse, Confidence::Oracle),
    }
}

// ── Module root TsId
// ──────────────────────────────────────────────────────────

/// Sentinel name marking a `TsId` as a module root (`module_ts_id`), never a
/// real declaration. `decl_ts_id` compares against this to tell "my parent is
/// the module itself" (no qualification needed) apart from "my parent is a
/// real enclosing namespace" (qualify).
const MODULE_ROOT_NAME: &str = "$module";

fn module_ts_id(module: &ModuleFacts, twins: &TwinPlan) -> TsId {
    TsId::new(
        twins.canonical(&module.path).to_path_buf(),
        MODULE_ROOT_NAME,
        0,
    )
}

/// Build the `TsId` for one declaration, qualified by its enclosing
/// *namespace* chain (never by its enclosing module).
///
/// TypeScript scopes a name to its immediate namespace, not to the whole
/// file: two sibling namespaces (or namespaces nested at any depth) may each
/// declare a member with the same local name without colliding. Two real
/// examples hit by the npm corpus sweep:
/// - `zod`'s `lib/helpers/util.d.ts` declares both `namespace objectUtil { type
///   identity = ... }` and `namespace util { type identity = ... }` — distinct
///   types that happen to share the name `identity`.
/// - `@types/node`'s `fs.d.ts` declares `namespace readFile { function
///   __promisify__(...) }`, `namespace writeFile { function __promisify__(...)
///   }`, … one `__promisify__` per overloaded top-level function, by Node's own
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
fn qualified_decl_name(decl: &DeclFact, parent: Option<&TsId>) -> String {
    match parent {
        Some(p) if p.name != MODULE_ROOT_NAME => format!("{}::{}", p.name, decl.name),
        _ => decl.name.clone(),
    }
}

fn decl_ts_id(decl: &DeclFact, parent: Option<&TsId>, twins: &TwinPlan) -> TsId {
    let name = qualified_decl_name(decl, parent);
    let disc = twins.discriminant(decl, &name);
    TsId::new(twins.canonical(&decl.module).to_path_buf(), name, disc)
}

// ── Per-declaration emission
// ──────────────────────────────────────────────────

fn emit_decl(
    decl: &DeclFact,
    parent: Option<TsId>,
    out: &mut Lowering<TsId>,
    module_index: &HashMap<&Path, &str>,
    export_index: &HashMap<&Path, &crate::typescript::extract::ExportTable>,
    resolver: &Resolver,
    declared: &DeclareSet,
    twins: &TwinPlan,
) {
    if twins.is_skipped(decl, parent.as_ref()) {
        // The keeper already owns this id. Children of a merged namespace
        // are still admitted one by one — a same-file pair never takes this
        // branch, and a differing child keeps its own entry.
        if let DeclBody::Namespace(body) = &decl.body {
            let id = decl_ts_id(decl, parent.as_ref(), twins);
            for child in &body.children {
                emit_decl(
                    child,
                    Some(id.clone()),
                    out,
                    module_index,
                    export_index,
                    resolver,
                    declared,
                    twins,
                );
            }
        }
        return;
    }

    let id = decl_ts_id(decl, parent.as_ref(), twins);
    let mut sym = make_sym(decl);
    if let Some(extra) = twins.extra_paths.get(&id) {
        sym.aliases = extra.clone().into_boxed_slice();
    }

    match &decl.body {
        DeclBody::Interface(body) => emit_interface(id, parent, sym, body, out, declared),
        DeclBody::Class(body) => emit_class(id, parent, sym, body, out, declared),
        DeclBody::TypeAlias(body) => emit_type_alias(id, parent, sym, body, out, declared),
        DeclBody::Enum(body) => emit_enum(id, parent, sym, body, out),
        DeclBody::Namespace(body) => {
            emit_namespace(
                id,
                parent,
                sym,
                body,
                out,
                module_index,
                export_index,
                resolver,
                declared,
                twins,
            );
        }
        DeclBody::Function(body) => emit_function(id, parent, sym, body, out, declared),
        DeclBody::Const(body) => emit_const(id, parent, sym, body, out, declared),
        DeclBody::Static(body) => emit_static(id, parent, sym, body, out, declared),
        DeclBody::Reexport {
            module_request,
            import_name,
        } => {
            emit_inline_reexport(
                id,
                parent,
                sym,
                module_request,
                import_name,
                out,
                resolver,
                declared,
                twins,
            );
        }
    }
}

// ── Interface → Trait
// ─────────────────────────────────────────────────────────

fn emit_interface(
    id: TsId,
    parent: Option<TsId>,
    sym: Symbol,
    body: &InterfaceBody,
    out: &mut Lowering<TsId>,
    names: &DeclareSet,
) {
    let generics = lower_generics(&body.generics, out, names, &id.module);
    let supers: Vec<Type> = body
        .extends
        .iter()
        .map(|t| lower_type(t, out, names, &id.module))
        .collect();

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
        let method_id = child_id(&id, &method.name, (idx * 1000) as u32);
        let method_sym = Symbol {
            name: method.name.clone(),
            visibility: accessibility_to_visibility(method.modifiers.accessibility),
            documentation: method.doc.doc.clone().unwrap_or_default(),
            source: id.module.clone(),
            span: (method.sig.span_start as usize)..(method.sig.span_end as usize),
            aliases: Box::new([]),
            deprecation: method
                .doc
                .deprecation
                .clone()
                .map(DeprecationOwned::into_ir),
            doc_links: Box::new([]),
            attrs: Box::new([]),
            cfg: None,
        };
        emit_function(
            method_id,
            Some(id.clone()),
            method_sym,
            &method.sig,
            out,
            names,
        );
    }

    // Emit properties as child Field entries.
    for (idx, prop) in body.properties.iter().enumerate() {
        let prop_id = child_id(&id, &prop.name, interface_property_disc(body, idx));
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
        let field_ty = prop
            .ty
            .as_ref()
            .map(|t| lower_type(t, out, names, &id.module));
        let _: Ref<Field> = out.declare(
            prop_id,
            Some(id.clone()),
            prop_sym,
            Field::builder()
                .key(FieldKey::Named)
                .maybe_ty(field_ty)
                .attributes(attrs)
                .build(),
        );
    }

    // Emit call signatures as synthetic `__call` functions. A method may
    // already be named `__call` at discriminant 0; that method keeps its id.
    for (call_num, call) in body.call_signatures.iter().enumerate() {
        let call_member = if call_num == 0 {
            "__call".to_string()
        } else {
            format!("__call_{call_num}")
        };
        let taken = body.methods.iter().enumerate().any(|(method_idx, method)| {
            method.name == call_member && (method_idx * 1000) as u32 == call_num as u32
        });
        let call_disc = if taken {
            call_num as u32 + 1
        } else {
            call_num as u32
        };
        let call_id = child_id(&id, &call_member, call_disc);
        let call_sym = Symbol {
            name: call_member,
            visibility: Visibility::Public,
            documentation: String::new(),
            source: id.module.clone(),
            span: (call.span_start as usize)..(call.span_end as usize),
            aliases: Box::new([]),
            deprecation: None,
            doc_links: Box::new([]),
            attrs: Box::new([]),
            cfg: None,
        };
        emit_function(call_id, Some(id.clone()), call_sym, call, out, names);
    }

    // Emit index signatures as synthetic `__index[_N]` Function entries (item 5).
    for (idx_num, idx_sig) in body.index_signatures.iter().enumerate() {
        let index_member = if idx_num == 0 {
            "__index".to_string()
        } else {
            format!("__index_{idx_num}")
        };
        // A real method may already be named `__index`. Its id is
        // `method_index * 1000`, which is 0 for the first method — the same
        // disc this signature would use. Keep the method's id and move the
        // synthetic one.
        let taken = body.methods.iter().enumerate().any(|(method_idx, method)| {
            method.name == index_member && (method_idx * 1000) as u32 == idx_num as u32
        });
        let index_disc = if taken {
            // Method discs are multiples of 1000. `idx_num` is one of those
            // when it collides, so the next integer is not.
            idx_num as u32 + 1
        } else {
            idx_num as u32
        };
        let idx_id = child_id(&id, &index_member, index_disc);
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
            initializer: None,
            decorators: Vec::new(),
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
            this_ty: None,
            abstract_construct: false,
            body_text: None,
            leading_doc: None,
            span_start: idx_sig.span_start,
            span_end: idx_sig.span_end,
        };
        emit_function(
            idx_id,
            Some(id.clone()),
            idx_sym,
            &index_fn_body,
            out,
            names,
        );
    }

    // Emit construct signatures as synthetic `new[_N]` Function entries (item 6).
    for (cs_num, cs) in body.construct_signatures.iter().enumerate() {
        let construct_member = if cs_num == 0 {
            "new".to_string()
        } else {
            format!("new_{cs_num}")
        };
        let cs_id = child_id(&id, &construct_member, cs_num as u32);
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
        emit_function(cs_id, Some(id.clone()), cs_sym, cs, out, names);
    }
}

// ── Class → Record
// ────────────────────────────────────────────────────────────

fn emit_class(
    id: TsId,
    parent: Option<TsId>,
    sym: Symbol,
    body: &ClassBody,
    out: &mut Lowering<TsId>,
    names: &DeclareSet,
) {
    let generics = lower_generics(&body.generics, out, names, &id.module);
    let super_types: Vec<Type> = body
        .extends
        .iter()
        .map(|t| lower_type(t, out, names, &id.module))
        .collect();
    // implements → also super_types (TypeScript models them the same way)
    let mut all_supers = super_types;
    all_supers.extend(
        body.implements
            .iter()
            .map(|t| lower_type(t, out, names, &id.module)),
    );

    // Collect field refs first (need to declare fields as children).
    // Because Lowering is order-independent we can refer before declaring.
    let mut field_refs: Vec<Ref<Field>> = Vec::new();

    for (idx, member) in body.members.iter().enumerate() {
        match &member.kind {
            MemberKind::Property { ty } | MemberKind::Accessor { ty } => {
                let field_id = child_id(&id, &member.name, class_field_disc(body, idx));
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
                    deprecation: member
                        .doc
                        .deprecation
                        .clone()
                        .map(DeprecationOwned::into_ir),
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
                let field_ty = ty.as_ref().map(|t| lower_type(t, out, names, &id.module));
                let _: Ref<Field> = out.declare(
                    field_id,
                    Some(id.clone()),
                    field_sym,
                    Field::builder()
                        .key(FieldKey::Named)
                        .maybe_ty(field_ty)
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
                    let method_id = child_id(&id, &member.name, (idx * 1000 + overload_idx) as u32);
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
                        deprecation: member
                            .doc
                            .deprecation
                            .clone()
                            .map(DeprecationOwned::into_ir),
                        doc_links: Box::new([]),
                        attrs: Box::new([]),
                        cfg: None,
                    };
                    emit_function(method_id, Some(id.clone()), method_sym, sig, out, names);
                }
            }
            MemberKind::Constructor(s) => {
                let method_id = child_id(&id, &member.name, (idx * 1000) as u32);
                let method_sym = Symbol {
                    name: member.name.clone(),
                    visibility: accessibility_to_visibility(member.modifiers.accessibility),
                    documentation: member.doc.doc.clone().unwrap_or_default(),
                    source: id.module.clone(),
                    span: (member.span_start as usize)..(member.span_end as usize),
                    aliases: Box::new([]),
                    deprecation: member
                        .doc
                        .deprecation
                        .clone()
                        .map(DeprecationOwned::into_ir),
                    doc_links: Box::new([]),
                    attrs: Box::new([]),
                    cfg: None,
                };
                emit_function(method_id, Some(id.clone()), method_sym, s, out, names);
            }
            // ── Item 4: Static block → synthetic Function (item 4) ────────
            MemberKind::StaticBlock { name: block_name } => {
                let sb_id = child_id(&id, block_name, idx as u32);
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
                    this_ty: None,
            abstract_construct: false,
            body_text: None,
            leading_doc: None,
                    span_start: member.span_start,
                    span_end: member.span_end,
                };
                emit_function(sb_id, Some(id.clone()), sb_sym, &static_fn_body, out, names);
            }
            // Fields / Accessors were already emitted above.
            MemberKind::Property { .. } | MemberKind::Accessor { .. } => {}
        }
    }
    for (idx_num, idx_sig) in body.index_signatures.iter().enumerate() {
        let index_member = if idx_num == 0 {
            "__index".to_string()
        } else {
            format!("__index_{idx_num}")
        };
        let idx_id = child_id(&id, &index_member, 3_000_000 + idx_num as u32);
        let idx_sym = Symbol {
            name: index_member,
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
        let index_fn_body = FunctionBody {
            generics: Vec::new(),
            params: vec![crate::typescript::extract::ParamFact {
                name: idx_sig.key_name.clone(),
                ty: Some(idx_sig.key_ty.clone()),
                is_optional: false,
                is_rest: false,
                is_readonly: false,
                initializer: None,
                decorators: Vec::new(),
                span_start: idx_sig.span_start,
                span_end: idx_sig.span_end,
            }],
            return_type: Some(idx_sig.value_ty.clone()),
            is_async: false,
            is_generator: false,
            has_body: false,
            receiver: ReceiverKind::None,
            this_ty: None,
            abstract_construct: false,
            body_text: None,
            leading_doc: None,
            span_start: idx_sig.span_start,
            span_end: idx_sig.span_end,
        };
        emit_function(idx_id, Some(id.clone()), idx_sym, &index_fn_body, out, names);
    }
}

// ── Type alias → Alias
// ────────────────────────────────────────────────────────

fn emit_type_alias(
    id: TsId,
    parent: Option<TsId>,
    sym: Symbol,
    body: &TypeAliasBody,
    out: &mut Lowering<TsId>,
    names: &DeclareSet,
) {
    let generics = lower_generics(&body.generics, out, names, &id.module);
    let target = lower_type(&body.target, out, names, &id.module);
    let _: Ref<Alias> = out.declare(
        id,
        parent,
        sym,
        Alias::builder().target(target).generics(generics).build(),
    );
}

// ── Enum → Enum + Variants
// ────────────────────────────────────────────────────

fn emit_enum(
    id: TsId,
    parent: Option<TsId>,
    sym: Symbol,
    body: &EnumBody,
    out: &mut Lowering<TsId>,
) {
    let mut variant_refs: Vec<Ref<Variant>> = Vec::with_capacity(body.variants.len());

    for (idx, v) in body.variants.iter().enumerate() {
        let variant_id = child_id(&id, &v.name, idx as u32);
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
    declared: &DeclareSet,
    twins: &TwinPlan,
) {
    let _: Ref<Module> = out.declare(id.clone(), parent, sym, Module);

    for child in &body.children {
        emit_decl(
            child,
            Some(id.clone()),
            out,
            module_index,
            export_index,
            resolver,
            declared,
            twins,
        );
    }
}

// ── Function → Function + Params ─────────────────────────────────────────────

fn emit_function(
    id: TsId,
    parent: Option<TsId>,
    sym: Symbol,
    body: &FunctionBody,
    out: &mut Lowering<TsId>,
    names: &DeclareSet,
) {
    let generics = lower_generics(&body.generics, out, names, &id.module);

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

    if let Some(this_ty) = &body.this_ty {
        // `this` is a real parameter. Extraction records its type and then
        // drops the binding, so the declaration never reached `finish`.
        let param_id = param_id_for(&id, "this", 0);
        let pref: Ref<Param> = out.refer(param_id.clone());
        param_refs.push(pref);
        param_ids.push((
            param_id,
            Param::builder()
                .ty(lower_type(this_ty, out, names, &id.module))
                .build(),
            body.span_start,
            body.span_end,
        ));
    }

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
        let param_id = param_id_for(&id, &p.name, idx as u32);
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
                .maybe_ty(p.ty.as_ref().map(|t| lower_type(t, out, names, &id.module)))
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
        let ret_ty = lower_type(ret, out, names, &id.module);
        let _: Ref<Param> = out.declare(
            ret_id,
            Some(id.clone()),
            param_sym,
            Param::builder().ty(ret_ty).build(),
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

// ── Const / Static
// ────────────────────────────────────────────────────────────

fn emit_const(
    id: TsId,
    parent: Option<TsId>,
    sym: Symbol,
    body: &ConstBody,
    out: &mut Lowering<TsId>,
    names: &DeclareSet,
) {
    // The declaration exists; the type annotation does not. `const x = 1` is
    // not the same claim as `const x: any = 1`.
    let ty = body
        .ty
        .as_ref()
        .map_or(Type::UNANNOTATED, |t| lower_type(t, out, names, &id.module));
    let _: Ref<Const> = out.declare(
        id,
        parent,
        sym,
        Const::builder()
            .ty(ty.clone())
            .maybe_value(body.value.clone().map(|source| {
                nudox_ir::build::ConstExpr::builder()
                    .ty(ty)
                    .source(source)
                    .build()
            }))
            .build(),
    );
}

fn emit_static(
    id: TsId,
    parent: Option<TsId>,
    sym: Symbol,
    body: &StaticBody,
    out: &mut Lowering<TsId>,
    names: &DeclareSet,
) {
    // The declaration exists; the type annotation does not. `const x = 1` is
    // not the same claim as `const x: any = 1`.
    let ty = body
        .ty
        .as_ref()
        .map_or(Type::UNANNOTATED, |t| lower_type(t, out, names, &id.module));
    let _: Ref<Static> = out.declare(
        id,
        parent,
        sym,
        Static::builder().ty(ty).mutable(body.is_mutable).build(),
    );
}

// ── Re-exports
// ─────────────────────────────────────────────────────────────────

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
/// ways; see [`crate::typescript::extract::LocalExport`]'s doc comment. This is
/// the one place both paths resolve through, so a package that reaches the same
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
    twins: &TwinPlan,
) -> Vec<TsId> {
    use crate::typescript::extract::LocalExport;

    let module = twins.canonical(table_path).to_path_buf();

    // `export *` and `ExportImportName::All` store the import name as `"*"`.
    // Nothing declares that id. The star names the module root, not a symbol.
    if name == "*" {
        return vec![TsId::new(module, MODULE_ROOT_NAME, 0)];
    }

    match table.locals.get(name) {
        Some(LocalExport::Named {
            local_name,
            overload_count,
        }) => (0..*overload_count)
            .map(|discriminant| TsId::new(module.clone(), local_name.clone(), discriminant))
            .collect(),
        Some(LocalExport::NamespaceOf(module_request)) => {
            resolve_module_path(resolver, table_path, module_request)
                .map(|ns_path| {
                    vec![TsId::new(
                        twins.canonical(&ns_path).to_path_buf(),
                        MODULE_ROOT_NAME,
                        0,
                    )]
                })
                .unwrap_or_default()
        }
        Some(LocalExport::Unresolvable) => Vec::new(),
        None => vec![TsId::new(module, name.to_string(), 0)],
    }
}

/// Re-export ids this package will `declare_ref`.
///
/// A barrel is emitted before the file it re-exports. The target id is often
/// that later file's own `declare_ref`. Putting the id in the declare-set
/// lets `refer` fill a slot the later `declare_ref` completes.
fn reexport_ids(
    modules: &[ModuleFacts],
    export_index: &HashMap<&Path, &crate::typescript::extract::ExportTable>,
    resolver: &Resolver,
    twins: &TwinPlan,
) -> HashSet<TsId> {
    let mut ids = HashSet::new();
    for module in modules {
        let here = twins.canonical(&module.path).to_path_buf();
        let mut reexported: HashSet<String> = HashSet::new();
        for indirect in &module.exports.indirect {
            let export_name = &indirect.export_name;
            if export_name == "*" || !reexported.insert(export_name.clone()) {
                continue;
            }
            let target_path = resolve_module_path(resolver, &module.path, &indirect.module_request);
            if indirect.import_name == "*" {
                if target_path.is_some() {
                    ids.insert(TsId::new(here.clone(), export_name.as_str(), 0));
                }
                continue;
            }
            let n = target_path.as_ref().map_or(0, |p| {
                export_index.get(p.as_path()).map_or(1, |table| {
                    resolve_export_target(resolver, p, table, &indirect.import_name, twins).len()
                })
            });
            for discriminant in 0..n {
                ids.insert(TsId::new(
                    here.clone(),
                    export_name.as_str(),
                    discriminant as u32,
                ));
            }
        }
        for star in &module.exports.star {
            let target_path = resolve_module_path(resolver, &module.path, &star.module_request);
            let mut expanded = false;
            if let Some(tp) = target_path.as_ref()
                && let Some(target_table) = export_index.get(tp.as_path())
            {
                for export_name in &target_table.exported_names {
                    if export_name == "*" || !reexported.insert(export_name.clone()) {
                        continue;
                    }
                    let n =
                        resolve_export_target(resolver, tp, target_table, export_name, twins).len();
                    if n == 0 {
                        continue;
                    }
                    expanded = true;
                    for discriminant in 0..n {
                        ids.insert(TsId::new(
                            here.clone(),
                            export_name.as_str(),
                            discriminant as u32,
                        ));
                    }
                }
            }
            if !expanded {
                let stem = specifier_stem(&star.module_request);
                if stem != "*" && reexported.insert(stem.clone()) {
                    ids.insert(TsId::new(here.clone(), stem.as_str(), 0));
                }
            }
        }
        let module_id = module_ts_id(module, twins);
        for decl in &module.declarations {
            if let DeclBody::Reexport { module_request, .. } = &decl.body {
                let resolves =
                    resolve_module_path(resolver, &decl.module, module_request).is_some();
                if resolves || is_package_specifier(module_request) {
                    ids.insert(decl_ts_id(decl, Some(&module_id), twins));
                }
            }
        }
    }
    ids
}

fn emit_reexports(
    module: &ModuleFacts,
    parent: Option<TsId>,
    out: &mut Lowering<TsId>,
    export_index: &HashMap<&Path, &crate::typescript::extract::ExportTable>,
    resolver: &Resolver,
    declared: &DeclareSet,
    twins: &TwinPlan,
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

        if export_name == "*" || !reexported.insert(export_name.clone()) {
            continue;
        }

        // `export * as ns from "m"` arrives here with import name `"*"`.
        // The target is the module root, never a symbol named `"*"`.
        if target_name == "*" {
            if let Some(p) = target_path.as_ref() {
                let root = TsId::new(twins.canonical(p).to_path_buf(), MODULE_ROOT_NAME, 0);
                let target_ref = refer_declared(out, declared, root);
                let sym =
                    reexport_symbol(module, export_name, indirect.span_start, indirect.span_end);
                let reexport_id = TsId::new(
                    twins.canonical(&module.path).to_path_buf(),
                    export_name.as_str(),
                    0,
                );
                if out.is_declared(&reexport_id) {
                    continue;
                }
                let _: Ref<Module> = out.declare_ref(reexport_id, parent.clone(), sym, target_ref);
            }
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
                    || vec![TsId::new(twins.canonical(p).to_path_buf(), target_name, 0)],
                    |target_table| {
                        resolve_export_target(resolver, p, target_table, target_name, twins)
                    },
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
            let reexport_id = TsId::new(
                twins.canonical(&module.path).to_path_buf(),
                export_name,
                discriminant as u32,
            );
            if out.is_declared(&reexport_id) {
                continue;
            }
            let target_ref: Ref<Module> = refer_declared(out, declared, tid);
            let _: Ref<Module> =
                out.declare_ref(reexport_id, parent.clone(), sym.clone(), target_ref);
        }
    }

    // Star re-exports: `export * from "m"`.
    for star in &module.exports.star {
        let target_path = resolve_module_path(resolver, &module.path, &star.module_request);
        let Some(tp) = target_path.as_ref() else {
            emit_unexpanded_star(
                module,
                parent.clone(),
                out,
                star,
                None,
                &mut reexported,
                declared,
                twins,
            );
            continue;
        };

        // Fan out to all names the target module exports. Each name is
        // resolved against the target's own export surface
        // (`resolve_export_target`) rather than assumed to be its own
        // `declare()`d name — zod's `types.d.ts` fans through
        // `export { anyType as any, ..., voidType as void }` and date-fns's
        // `format.d.ts` fans through `export { format as formatDate }`,
        // both bare local renames the target module itself declares under
        // the *pre*-rename name.
        let mut expanded = false;
        if let Some(target_table) = export_index.get(tp.as_path()) {
            for export_name in &target_table.exported_names {
                if export_name == "*" || !reexported.insert(export_name.clone()) {
                    continue;
                }
                let target_ids =
                    resolve_export_target(resolver, tp.as_path(), target_table, export_name, twins);
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
                    if target_id.name == "*" {
                        continue;
                    }
                    expanded = true;
                    let reexport_id = TsId::new(
                        twins.canonical(&module.path).to_path_buf(),
                        export_name,
                        discriminant as u32,
                    );
                    if out.is_declared(&reexport_id) {
                        continue;
                    }
                    let target_ref: Ref<Module> = refer_declared(out, declared, target_id);
                    let _: Ref<Module> =
                        out.declare_ref(reexport_id, parent.clone(), sym.clone(), target_ref);
                }
            }
        }
        // A star that named nothing is one reference to the module root.
        // The symbol is the file stem, never `"*"`.
        if !expanded {
            emit_unexpanded_star(
                module,
                parent.clone(),
                out,
                star,
                Some(tp.as_path()),
                &mut reexported,
                declared,
                twins,
            );
        }
    }
}

fn reexport_symbol(module: &ModuleFacts, name: &str, start: u32, end: u32) -> Symbol {
    Symbol {
        name: name.to_string(),
        visibility: Visibility::Public,
        documentation: String::new(),
        source: module.path.clone(),
        span: (start as usize)..(end as usize),
        aliases: Box::new([]),
        deprecation: None,
        doc_links: Box::new([]),
        attrs: Box::new([]),
        cfg: None,
    }
}

/// `refer` when this package will declare `id`. Otherwise `refer_import`.
///
/// `refer` on an id that is not in the declare-set inserts an empty slot,
/// and `finish` then rejects the package. A missing file, a dependency, or
/// any other name this package will not declare is a foreign key.
fn refer_declared(out: &mut Lowering<TsId>, declared: &DeclareSet, id: TsId) -> Ref<Module> {
    if declared.contains(&id) {
        return out.refer(id);
    }
    let display: Box<str> = id.name.as_str().into();
    let path: Box<str> = format!("{}::{}#{}", id.module.display(), id.name, id.discriminant).into();
    out.refer_import(ForeignKey::in_namespace(
        EcosystemId::new("npm"),
        id.module.display().to_string(),
        path,
        display,
    ))
}

fn emit_unexpanded_star(
    module: &ModuleFacts,
    parent: Option<TsId>,
    out: &mut Lowering<TsId>,
    star: &crate::typescript::extract::StarExport,
    target: Option<&Path>,
    reexported: &mut HashSet<String>,
    declared: &DeclareSet,
    twins: &TwinPlan,
) {
    let stem = specifier_stem(&star.module_request);
    if stem == "*" || !reexported.insert(stem.clone()) {
        return;
    }
    let target_ref = if let Some(tp) = target {
        let root = TsId::new(twins.canonical(tp).to_path_buf(), MODULE_ROOT_NAME, 0);
        refer_declared(out, declared, root)
    } else {
        out.refer_import(ForeignKey::in_namespace(
            EcosystemId::new("npm"),
            star.module_request.clone(),
            Box::<str>::from(format!("{}::$module", star.module_request)),
            Box::<str>::from("$module"),
        ))
    };
    let reexport_id = TsId::new(
        twins.canonical(&module.path).to_path_buf(),
        stem.as_str(),
        0,
    );
    if out.is_declared(&reexport_id) {
        return;
    }
    let sym = reexport_symbol(module, &stem, star.span_start, star.span_end);
    let _: Ref<Module> = out.declare_ref(reexport_id, parent, sym, target_ref);
}

fn emit_default_alias(
    module: &ModuleFacts,
    parent: Option<TsId>,
    out: &mut Lowering<TsId>,
    declared: &DeclareSet,
    twins: &TwinPlan,
) {
    use crate::typescript::extract::LocalExport;
    let Some(LocalExport::Named {
        local_name,
        overload_count,
    }) = module.exports.locals.get("default")
    else {
        return;
    };
    if local_name == "default" {
        return;
    }
    let canonical = twins.canonical(&module.path).to_path_buf();
    for disc in 0..*overload_count {
        let target = TsId::new(canonical.clone(), local_name.clone(), disc);
        let default_id = TsId::new(canonical.clone(), "default", disc);
        if !declared.contains(&target) || !declared.contains(&default_id) {
            continue;
        }
        if out.is_declared(&default_id) {
            continue;
        }
        let sym = reexport_symbol(module, "default", 0, 0);
        let target_ref: Ref<Module> = out.refer(target);
        let _: Ref<Module> = out.declare_ref(default_id, parent.clone(), sym, target_ref);
    }
}

fn emit_inline_reexport(
    id: TsId,
    parent: Option<TsId>,
    sym: Symbol,
    module_request: &str,
    import_name: &str,
    out: &mut Lowering<TsId>,
    resolver: &Resolver,
    declared: &DeclareSet,
    twins: &TwinPlan,
) {
    if out.is_declared(&id) {
        return;
    }
    // The id's module is already the canonical path. Resolve relative to the
    // file that wrote the re-export, which is `sym.source`.
    let target_path = resolve_module_path(resolver, &sym.source, module_request);
    if let Some(tp) = target_path {
        let target_name = if import_name == "*" {
            MODULE_ROOT_NAME
        } else {
            import_name
        };
        let target_id = TsId::new(twins.canonical(&tp).to_path_buf(), target_name, 0);
        let target_ref: Ref<Module> = refer_declared(out, declared, target_id);
        let _: Ref<Module> = out.declare_ref(id, parent, sym, target_ref);
    } else if is_package_specifier(module_request) {
        let target_ref: Ref<Module> =
            out.refer_import(package_foreign_key(module_request, &sym.name));
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
    let resolved = resolver
        .resolve(parent_dir, specifier)
        .ok()?
        .into_path_buf();
    if resolved.is_file() && is_ts_module_path(&resolved) {
        Some(resolved)
    } else {
        None
    }
}

// ── One declare-set
// ───────────────────────────────────────────────────────────

/// Every id this package will `declare` or `declare_ref`, plus a name index
/// for nominal lowering.
///
/// Built from `ModuleFacts` before any `refer`. A name declared more than
/// once is omitted from the name index: guessing the wrong id is worse than
/// leaving it unresolved. The id set still contains each distinct id.
pub(crate) struct DeclareSet {
    ids: HashSet<TsId>,
    by_file: HashMap<(PathBuf, String), TsId>,
    unique: HashMap<String, TsId>,
}

impl DeclareSet {
    fn empty() -> Self {
        DeclareSet {
            ids: HashSet::new(),
            by_file: HashMap::new(),
            unique: HashMap::new(),
        }
    }

    fn contains(&self, id: &TsId) -> bool {
        self.ids.contains(id)
    }

    fn resolve(&self, module: &Path, name: &str) -> Option<TsId> {
        self.by_file
            .get(&(module.to_path_buf(), name.to_string()))
            .or_else(|| self.unique.get(name))
            .cloned()
    }

    fn build(
        modules: &[ModuleFacts],
        twins: &TwinPlan,
        export_index: &HashMap<&Path, &crate::typescript::extract::ExportTable>,
        resolver: &Resolver,
    ) -> Self {
        let mut ids = HashSet::new();
        let mut by_file: HashMap<(PathBuf, String), TsId> = HashMap::new();
        let mut file_clash: HashSet<(PathBuf, String)> = HashSet::new();
        let mut unique: HashMap<String, TsId> = HashMap::new();
        let mut clash: HashSet<String> = HashSet::new();
        let mut seen_module = HashSet::new();

        for module in modules {
            let parent = module_ts_id(module, twins);
            if seen_module.insert(parent.clone()) {
                ids.insert(parent.clone());
            }
            consider_owned(
                &module.declarations,
                Some(&parent),
                twins,
                &mut ids,
                &mut by_file,
                &mut file_clash,
                &mut unique,
                &mut clash,
                true,
            );
        }
        for key in file_clash {
            by_file.remove(&key);
        }
        for name in clash {
            unique.remove(&name);
        }

        for module in modules {
            use crate::typescript::extract::LocalExport;
            if let Some(LocalExport::Named {
                local_name,
                overload_count,
            }) = module.exports.locals.get("default")
            {
                if local_name == "default" {
                    continue;
                }
                let canonical = twins.canonical(&module.path).to_path_buf();
                for disc in 0..*overload_count {
                    let target = TsId::new(canonical.clone(), local_name.clone(), disc);
                    if ids.contains(&target) {
                        ids.insert(TsId::new(canonical.clone(), "default", disc));
                    }
                }
            }
        }

        ids.extend(reexport_ids(modules, export_index, resolver, twins));

        DeclareSet {
            ids,
            by_file,
            unique,
        }
    }
}

fn consider_owned(
    decls: &[DeclFact],
    parent: Option<&TsId>,
    twins: &TwinPlan,
    ids: &mut HashSet<TsId>,
    by_file: &mut HashMap<(PathBuf, String), TsId>,
    file_clash: &mut HashSet<(PathBuf, String)>,
    unique: &mut HashMap<String, TsId>,
    clash: &mut HashSet<String>,
    top_level: bool,
) {
    for decl in decls {
        if matches!(decl.body, DeclBody::Reexport { .. }) {
            continue;
        }
        if twins.is_skipped(decl, parent) {
            if let DeclBody::Namespace(body) = &decl.body {
                let id = decl_ts_id(decl, parent, twins);
                consider_owned(
                    &body.children,
                    Some(&id),
                    twins,
                    ids,
                    by_file,
                    file_clash,
                    unique,
                    clash,
                    false,
                );
            }
            continue;
        }
        let id = decl_ts_id(decl, parent, twins);
        ids.insert(id.clone());
        if top_level {
            let file_key = (id.module.clone(), decl.name.clone());
            if by_file.contains_key(&file_key) {
                file_clash.insert(file_key);
            } else {
                by_file.insert(file_key, id.clone());
            }
            if unique.contains_key(&decl.name) {
                clash.insert(decl.name.clone());
            } else {
                unique.insert(decl.name.clone(), id.clone());
            }
        }
        if let DeclBody::Namespace(body) = &decl.body {
            consider_owned(
                &body.children,
                Some(&id),
                twins,
                ids,
                by_file,
                file_clash,
                unique,
                clash,
                false,
            );
        }
    }
}

// ── Twin publications
// ─────────────────────────────────────────────────────────

/// Two files are twins when they share a parent directory and the same stem
/// after a whole-suffix strip (not `Path::extension`).
///
/// The kept declaration is borrowed from `ModuleFacts` for the duration of
/// [`TwinPlan::build`]. Identity is decided by comparing those facts in place.
/// The sealed entry never stores this skeleton, so materializing one string
/// per type node only allocated.
struct KeptBody<'a> {
    decl: &'a DeclFact,
}

/// `module -> qualified name -> (decl_index, span_start)`.
type SkipSet = HashMap<PathBuf, HashMap<String, HashSet<(u32, u32)>>>;
/// `module -> qualified name -> (decl_index, span_start) -> emitted
/// discriminant`.
type DiscMap = HashMap<PathBuf, HashMap<String, HashMap<(u32, u32), u32>>>;
/// `canonical module -> qualified name -> discriminant -> kept declaration`.
type Occupied<'a> = HashMap<PathBuf, HashMap<String, HashMap<u32, KeptBody<'a>>>>;

struct TwinPlan {
    canonical: HashMap<PathBuf, PathBuf>,
    module_extra: HashMap<PathBuf, Vec<String>>,
    extra_paths: HashMap<TsId, Vec<String>>,
    skip: SkipSet,
    disc: DiscMap,
}

impl TwinPlan {
    fn build(modules: &[ModuleFacts]) -> Self {
        let mut logical_first: HashMap<PathBuf, PathBuf> = HashMap::new();
        let mut canonical = HashMap::new();
        let mut module_extra: HashMap<PathBuf, Vec<String>> = HashMap::new();
        for module in modules {
            let logical = logical_module_path(&module.path);
            if let Some(first) = logical_first.get(&logical).cloned() {
                canonical.insert(module.path.clone(), first.clone());
                module_extra
                    .entry(first)
                    .or_default()
                    .push(module.path.display().to_string());
            } else {
                logical_first.insert(logical, module.path.clone());
                canonical.insert(module.path.clone(), module.path.clone());
            }
        }

        let mut occupied: Occupied<'_> = HashMap::new();
        let mut skip: SkipSet = HashMap::new();
        let mut disc: DiscMap = HashMap::new();
        let mut extra_paths = HashMap::new();
        // Enum variants are not declarations in this plan. They are emitted
        // later as `E::A` at discriminant `index`. A merged namespace member
        // of the same name would take that same id and `finish` would return
        // Duplicate. Reserve the variant discs first so the member moves.
        let mut reserved: HashMap<PathBuf, HashMap<String, HashSet<u32>>> = HashMap::new();
        for module in modules {
            let canonical_path = canonical
                .get(&module.path)
                .map(PathBuf::as_path)
                .unwrap_or(module.path.as_path());
            reserve_enum_variants(
                &module.declarations,
                None,
                canonical_path,
                &mut reserved,
            );
        }
        for module in modules {
            admit_decls(
                &module.declarations,
                None,
                &canonical,
                &reserved,
                &mut occupied,
                &mut skip,
                &mut disc,
                &mut extra_paths,
            );
        }

        TwinPlan {
            canonical,
            module_extra,
            extra_paths,
            skip,
            disc,
        }
    }

    fn canonical<'a>(&'a self, path: &'a Path) -> &'a Path {
        self.canonical
            .get(path)
            .map(PathBuf::as_path)
            .unwrap_or(path)
    }

    fn is_secondary(&self, path: &Path) -> bool {
        self.canonical
            .get(path)
            .is_some_and(|c| c.as_path() != path)
    }

    fn module_aliases(&self, path: &Path) -> Box<[String]> {
        let canonical = self.canonical(path);
        self.module_extra
            .get(canonical)
            .map(|paths| paths.clone().into_boxed_slice())
            .unwrap_or_else(|| Box::new([]))
    }

    fn discriminant(&self, decl: &DeclFact, qualified: &str) -> u32 {
        self.disc
            .get(decl.module.as_path())
            .and_then(|by_name| by_name.get(qualified))
            .and_then(|by_pos| by_pos.get(&(decl.decl_index, decl.span_start)))
            .copied()
            .unwrap_or(decl.decl_index)
    }

    fn is_skipped(&self, decl: &DeclFact, parent: Option<&TsId>) -> bool {
        let Some(by_name) = self.skip.get(decl.module.as_path()) else {
            return false;
        };
        let key = (decl.decl_index, decl.span_start);
        match parent {
            Some(p) if p.name != MODULE_ROOT_NAME => {
                let name = qualified_decl_name(decl, parent);
                by_name
                    .get(name.as_str())
                    .is_some_and(|set| set.contains(&key))
            }
            _ => by_name
                .get(decl.name.as_str())
                .is_some_and(|set| set.contains(&key)),
        }
    }
}

/// Variant ids of an enum whose own discriminant is 0 (`decl_index` 0, the
/// first declaration of that name). `child_name` then writes `Enum::Variant`
/// at discriminant `index`, which is exactly the id a merged namespace
/// member would otherwise claim.
fn reserve_enum_variants(
    decls: &[DeclFact],
    parent_qual: Option<&str>,
    canonical: &Path,
    reserved: &mut HashMap<PathBuf, HashMap<String, HashSet<u32>>>,
) {
    for decl in decls {
        let qual = match parent_qual {
            Some(parent) => format!("{parent}::{}", decl.name),
            None => decl.name.clone(),
        };
        if decl.decl_index == 0 {
            // `child_name` omits the parent's discriminant only when it is 0,
            // so these member ids are `Parent::name` at the discriminant
            // `emit_*` will use. A merged namespace member of that name would
            // otherwise take the same id.
            match &decl.body {
                DeclBody::Enum(body) => {
                    for (idx, variant) in body.variants.iter().enumerate() {
                        reserve_member(canonical, reserved, &qual, &variant.name, idx as u32);
                    }
                }
                DeclBody::Class(body) => {
                    for (idx, member) in body.members.iter().enumerate() {
                        match &member.kind {
                            MemberKind::Method(sigs) => {
                                for overload_idx in 0..sigs.len() {
                                    reserve_member(
                                        canonical,
                                        reserved,
                                        &qual,
                                        &member.name,
                                        (idx * 1000 + overload_idx) as u32,
                                    );
                                }
                            }
                            MemberKind::Property { .. } | MemberKind::Accessor { .. } => {
                                // Fields use the member index itself, not
                                // `index * 1000`. The first field is
                                // discriminant 0, which a merged namespace
                                // value of the same name also claims. A field
                                // whose index lands on a same-named method
                                // (`index * 1000`) moves, and the reservation
                                // has to follow that disc.
                                reserve_member(
                                    canonical,
                                    reserved,
                                    &qual,
                                    &member.name,
                                    class_field_disc(body, idx),
                                );
                            }
                            MemberKind::Constructor(_) => {
                                reserve_member(
                                    canonical,
                                    reserved,
                                    &qual,
                                    &member.name,
                                    (idx * 1000) as u32,
                                );
                            }
                            MemberKind::StaticBlock { name } => {
                                reserve_member(canonical, reserved, &qual, name, idx as u32);
                            }
                        }
                    }
                }
                DeclBody::Interface(body) => {
                    for (idx, method) in body.methods.iter().enumerate() {
                        reserve_member(
                            canonical,
                            reserved,
                            &qual,
                            &method.name,
                            (idx * 1000) as u32,
                        );
                    }
                }
                _ => {}
            }
        }
        if let DeclBody::Namespace(body) = &decl.body {
            reserve_enum_variants(&body.children, Some(&qual), canonical, reserved);
        }
    }
}

fn reserve_member(
    canonical: &Path,
    reserved: &mut HashMap<PathBuf, HashMap<String, HashSet<u32>>>,
    parent: &str,
    member: &str,
    disc: u32,
) {
    reserved
        .entry(canonical.to_path_buf())
        .or_default()
        .entry(format!("{parent}::{member}"))
        .or_default()
        .insert(disc);
}

fn admit_decls<'a>(
    decls: &'a [DeclFact],
    parent_qual: Option<&str>,
    canonical_of: &HashMap<PathBuf, PathBuf>,
    reserved: &HashMap<PathBuf, HashMap<String, HashSet<u32>>>,
    occupied: &mut Occupied<'a>,
    skip: &mut SkipSet,
    disc: &mut DiscMap,
    extra_paths: &mut HashMap<TsId, Vec<String>>,
) {
    for decl in decls {
        if matches!(decl.body, DeclBody::Reexport { .. }) {
            continue;
        }
        let canonical = canonical_of
            .get(&decl.module)
            .map(PathBuf::as_path)
            .unwrap_or(decl.module.as_path());
        let qual = match parent_qual {
            Some(p) => format!("{p}::{}", decl.name),
            None => decl.name.clone(),
        };
        let preferred = decl.decl_index;
        let hit = {
            let kept = occupied
                .get(canonical)
                .and_then(|by_name| by_name.get(qual.as_str()))
                .and_then(|by_disc| by_disc.get(&preferred));
            match kept {
                Some(kept) if kept.decl.module == decl.module && same_surface(kept.decl, decl) => {
                    AdmitHit::SameFileSameBody
                }
                Some(kept) if kept.decl.module == decl.module => AdmitHit::SameFileDifferent,
                Some(kept) if body_match(&kept.decl.body, &decl.body) => AdmitHit::TwinSameBody,
                Some(_) => AdmitHit::TwinDifferent,
                None => AdmitHit::Absent,
            }
        };
        let held_by_variant = reserved
            .get(canonical)
            .and_then(|by_name| by_name.get(qual.as_str()))
            .is_some_and(|discs| discs.contains(&preferred));
        match hit {
            AdmitHit::SameFileSameBody => {
                // The same body written twice in one file (interface merging,
                // a twin copy pasted beside the original). One entry is the
                // declaration.
                remember_skip(skip, &decl.module, &qual, preferred, decl.span_start);
            }
            AdmitHit::TwinSameBody => {
                remember_skip(skip, &decl.module, &qual, preferred, decl.span_start);
                extra_paths
                    .entry(TsId::new(canonical.to_path_buf(), qual.clone(), preferred))
                    .or_default()
                    .push(decl.module.display().to_string());
            }
            AdmitHit::SameFileDifferent | AdmitHit::TwinDifferent => {
                // Different bodies share a name. They are overloads, not one
                // id declared twice.
                let fresh = fresh_discriminant(
                    occupied
                        .get(canonical)
                        .and_then(|by_name| by_name.get(qual.as_str())),
                    reserved.get(canonical).and_then(|by_name| by_name.get(qual.as_str())),
                );
                remember_disc(disc, &decl.module, &qual, preferred, decl.span_start, fresh);
                insert_kept(occupied, canonical, &qual, fresh, decl);
            }
            AdmitHit::Absent if held_by_variant => {
                // `preferred` is an enum variant of this merged name. Keep
                // the variant's id and move this member off it.
                let fresh = fresh_discriminant(
                    occupied
                        .get(canonical)
                        .and_then(|by_name| by_name.get(qual.as_str())),
                    reserved.get(canonical).and_then(|by_name| by_name.get(qual.as_str())),
                );
                remember_disc(disc, &decl.module, &qual, preferred, decl.span_start, fresh);
                insert_kept(occupied, canonical, &qual, fresh, decl);
            }
            AdmitHit::Absent => {
                insert_kept(occupied, canonical, &qual, preferred, decl);
            }
        }
        if let DeclBody::Namespace(body) = &decl.body {
            admit_decls(
                &body.children,
                Some(&qual),
                canonical_of,
                reserved,
                occupied,
                skip,
                disc,
                extra_paths,
            );
        }
    }
}

enum AdmitHit {
    SameFileSameBody,
    SameFileDifferent,
    TwinSameBody,
    TwinDifferent,
    Absent,
}

fn insert_kept<'a>(
    occupied: &mut Occupied<'a>,
    canonical: &Path,
    qual: &str,
    disc: u32,
    decl: &'a DeclFact,
) {
    occupied
        .entry(canonical.to_path_buf())
        .or_default()
        .entry(qual.to_string())
        .or_default()
        .insert(disc, KeptBody { decl });
}

fn remember_skip(skip: &mut SkipSet, module: &Path, qual: &str, decl_index: u32, span_start: u32) {
    skip.entry(module.to_path_buf())
        .or_default()
        .entry(qual.to_string())
        .or_default()
        .insert((decl_index, span_start));
}

fn remember_disc(
    disc: &mut DiscMap,
    module: &Path,
    qual: &str,
    decl_index: u32,
    span_start: u32,
    fresh: u32,
) {
    disc.entry(module.to_path_buf())
        .or_default()
        .entry(qual.to_string())
        .or_default()
        .insert((decl_index, span_start), fresh);
}

/// A member's id name. Discriminant 0 keeps `Parent::member`. A later
/// overload of the same parent name includes its discriminant so its
/// members are not the first overload's members.
fn child_name(parent: &TsId, member: &str) -> String {
    if parent.discriminant == 0 {
        format!("{}::{}", parent.name, member)
    } else {
        format!("{}#{}::{}", parent.name, parent.discriminant, member)
    }
}

/// Parameter id for one function overload.
///
/// The common case folds the function's discriminant into the numeric half
/// (`disc * 1000 + index`). Two facts make that product unsafe:
///
/// - It does not fit in `u32` once a function's own discriminant is at least
///   `u32::MAX / 1000` — an interface method past member 4294, because the
///   method id is already `index * 1000`. `saturating_mul` then pinned every
///   such parameter at `u32::MAX`.
/// - Overload discriminants are adjacent. Parameter 1000 of one overload is
///   the same number as parameter 0 of the next (`disc * 1000 + 1000` versus
///   `(disc + 1) * 1000`).
///
/// Indices below 1000 keep the old id, so packages that already seal do not
/// move. A wider parameter list, or a product that does not fit, puts the
/// function discriminant in the name — the same trick `child_name` uses for
/// a parent overload — and keeps the parameter index as the discriminant.
fn param_id_for(owner: &TsId, param_name: &str, index: u32) -> TsId {
    let packed = (index < 1000).then(|| {
        owner
            .discriminant
            .checked_mul(1000)
            .and_then(|base| base.checked_add(index))
    });
    if let Some(disc) = packed.flatten() {
        return TsId::new(
            owner.module.clone(),
            format!("{}::param::{}", owner.name, param_name),
            disc,
        );
    }
    TsId::new(
        owner.module.clone(),
        format!(
            "{}#{}::param::{}",
            owner.name, owner.discriminant, param_name
        ),
        index,
    )
}

/// An interface property's discriminant.
///
/// The usual value is `2_000_000 + index`, so a property that never meets
/// the method lattice keeps the id it already sealed with. Method
/// discriminants are `index * 1000`, so method 2000 is the same number as
/// property 0. Same name, same id, `finish` rejects the package. Step off
/// that disc. The next integer is not a method disc: those are multiples
/// of 1000.
fn interface_property_disc(body: &InterfaceBody, prop_idx: usize) -> u32 {
    let name = &body.properties[prop_idx].name;
    let mut disc = 2_000_000 + prop_idx as u32;
    loop {
        let method_taken = body
            .methods
            .iter()
            .enumerate()
            .any(|(method_idx, method)| method.name == *name && (method_idx * 1000) as u32 == disc);
        let property_taken = body.properties.iter().enumerate().any(|(other, prop)| {
            other != prop_idx && prop.name == *name && 2_000_000 + other as u32 == disc
        });
        if !method_taken && !property_taken {
            return disc;
        }
        if disc == u32::MAX {
            return disc;
        }
        disc += 1;
    }
}

/// A class field's discriminant.
///
/// The usual value is the member index, so a class whose fields never meet
/// the method lattice keeps the ids it already sealed with. Method
/// discriminants are `index * 1000`, so member 1000's field is the same
/// number as member 1's method. Same name, same id, `finish` rejects the
/// package. Step off those discs (and off any overload of that method).
fn class_field_disc(body: &ClassBody, field_idx: usize) -> u32 {
    let name = &body.members[field_idx].name;
    let mut disc = field_idx as u32;
    loop {
        if !class_disc_taken(body, name, disc, field_idx) {
            return disc;
        }
        if disc == u32::MAX {
            return disc;
        }
        disc += 1;
    }
}

fn class_disc_taken(body: &ClassBody, name: &str, disc: u32, except_idx: usize) -> bool {
    for (idx, member) in body.members.iter().enumerate() {
        if idx == except_idx {
            continue;
        }
        let same_name = match &member.kind {
            MemberKind::StaticBlock { name: block } => block == name,
            _ => member.name == name,
        };
        if !same_name {
            continue;
        }
        match &member.kind {
            MemberKind::Method(sigs) => {
                if sigs
                    .iter()
                    .enumerate()
                    .any(|(overload_idx, _)| (idx * 1000 + overload_idx) as u32 == disc)
                {
                    return true;
                }
            }
            MemberKind::Constructor(_) => {
                if (idx * 1000) as u32 == disc {
                    return true;
                }
            }
            MemberKind::Property { .. }
            | MemberKind::Accessor { .. }
            | MemberKind::StaticBlock { .. } => {
                if idx as u32 == disc {
                    return true;
                }
            }
        }
    }
    body.index_signatures
        .iter()
        .enumerate()
        .any(|(idx_num, _)| {
            let index_member = if idx_num == 0 {
                "__index".to_string()
            } else {
                format!("__index_{idx_num}")
            };
            index_member == name && 3_000_000 + idx_num as u32 == disc
        })
}

fn child_id(parent: &TsId, member: &str, local: u32) -> TsId {
    // The parent overload lives in the name (`Parent#N::member`). Folding it
    // into the discriminant as well overflows `u32` once a parameter
    // multiplies that discriminant by 1000, and every parameter collapses
    // onto `u32::MAX`.
    TsId::new(parent.module.clone(), child_name(parent, member), local)
}

fn fresh_discriminant(
    by_disc: Option<&HashMap<u32, KeptBody<'_>>>,
    reserved: Option<&HashSet<u32>>,
) -> u32 {
    let mut disc = 0u32;
    loop {
        let taken = by_disc.is_some_and(|kept| kept.contains_key(&disc))
            || reserved.is_some_and(|held| held.contains(&disc));
        if !taken {
            return disc;
        }
        if disc == u32::MAX {
            return disc;
        }
        disc += 1;
    }
}

/// Whole suffixes, longest first. `Path::extension` of `kinds.d.ts` is `ts`.
fn logical_stem(file_name: &str) -> String {
    const SUFFIXES: &[&str] = &[
        ".d.ts", ".d.mts", ".d.cts", ".mjs", ".cjs", ".mts", ".cts", ".js", ".ts",
    ];
    for suffix in SUFFIXES {
        if file_name.len() > suffix.len() && file_name.ends_with(suffix) {
            return file_name[..file_name.len() - suffix.len()].to_string();
        }
    }
    file_name.to_string()
}

fn logical_module_path(path: &Path) -> PathBuf {
    let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
        return path.to_path_buf();
    };
    path.with_file_name(logical_stem(name))
}

fn specifier_stem(specifier: &str) -> String {
    let last = specifier.rsplit(['/', '\\']).next().unwrap_or(specifier);
    let stem = logical_stem(last);
    if stem.is_empty() || stem == "*" {
        "module".to_string()
    } else {
        stem
    }
}

fn is_package_specifier(specifier: &str) -> bool {
    !specifier.is_empty() && !specifier.starts_with('.') && !specifier.starts_with('/')
}

fn npm_package_name(specifier: &str) -> String {
    if let Some(rest) = specifier.strip_prefix('@') {
        let mut parts = rest.splitn(3, '/');
        let scope = parts.next().unwrap_or("");
        let name = parts.next().unwrap_or("");
        if name.is_empty() {
            specifier.to_string()
        } else {
            format!("@{scope}/{name}")
        }
    } else {
        specifier.split('/').next().unwrap_or(specifier).to_string()
    }
}

fn package_foreign_key(specifier: &str, display: &str) -> ForeignKey {
    ForeignKey::in_package(
        PackageLineageId::new(
            EcosystemId::new("npm"),
            PackageName::new(npm_package_name(specifier)),
        ),
        Box::<str>::from(specifier),
        Box::<str>::from(display),
    )
}

/// Visibility, docs, and body. Same-file copies collapse only when this
/// matches.
fn same_surface(a: &DeclFact, b: &DeclFact) -> bool {
    a.visibility == b.visibility && doc_match(&a.doc, &b.doc) && body_match(&a.body, &b.body)
}

fn doc_match(a: &DocFacts, b: &DocFacts) -> bool {
    opt_text(&a.doc, &b.doc)
        && a.ignore == b.ignore
        && match (a.deprecation.as_ref(), b.deprecation.as_ref()) {
            (None, None) => true,
            (Some(a), Some(b)) => opt_text(&a.note, &b.note) && a.since == b.since,
            _ => false,
        }
}

/// `None` and `Some("")` write no characters into the old skeleton.
fn opt_text(a: &Option<String>, b: &Option<String>) -> bool {
    match (a, b) {
        (None, None) => true,
        (Some(a), Some(b)) => a == b,
        (None, Some(text)) | (Some(text), None) => text.is_empty(),
    }
}

/// `None` and `Some("")` are the same discriminant / const value text.
fn opt_empty(a: &Option<String>, b: &Option<String>) -> bool {
    a.as_deref().unwrap_or("") == b.as_deref().unwrap_or("")
}

fn body_match(a: &DeclBody, b: &DeclBody) -> bool {
    match (a, b) {
        (DeclBody::Enum(a), DeclBody::Enum(b)) => {
            a.is_const == b.is_const
                && a.variants.len() == b.variants.len()
                && a.variants
                    .iter()
                    .zip(&b.variants)
                    .all(|(x, y)| variant_match(x, y))
        }
        (DeclBody::Function(a), DeclBody::Function(b)) => function_match(a, b),
        (DeclBody::Class(a), DeclBody::Class(b)) => {
            a.is_abstract == b.is_abstract
                && decorators_match(&a.decorators, &b.decorators)
                && generics_match(&a.generics, &b.generics)
                && types_match(&a.extends, &b.extends)
                && types_match(&a.implements, &b.implements)
                && a.members.len() == b.members.len()
                && a.members
                    .iter()
                    .zip(&b.members)
                    .all(|(x, y)| member_match(x, y))
                && indexes_match(&a.index_signatures, &b.index_signatures)
        }
        (DeclBody::Interface(a), DeclBody::Interface(b)) => {
            generics_match(&a.generics, &b.generics)
                && types_match(&a.extends, &b.extends)
                && a.methods.len() == b.methods.len()
                && a.methods
                    .iter()
                    .zip(&b.methods)
                    .all(|(x, y)| method_match(x, y))
                && a.properties.len() == b.properties.len()
                && a.properties
                    .iter()
                    .zip(&b.properties)
                    .all(|(x, y)| property_match(x, y))
                && a.call_signatures.len() == b.call_signatures.len()
                && a.call_signatures
                    .iter()
                    .zip(&b.call_signatures)
                    .all(|(x, y)| function_match(x, y))
                && indexes_match(&a.index_signatures, &b.index_signatures)
                && a.construct_signatures.len() == b.construct_signatures.len()
                && a.construct_signatures
                    .iter()
                    .zip(&b.construct_signatures)
                    .all(|(x, y)| function_match(x, y))
        }
        (DeclBody::TypeAlias(a), DeclBody::TypeAlias(b)) => {
            generics_match(&a.generics, &b.generics) && type_match(&a.target, &b.target)
        }
        (DeclBody::Const(a), DeclBody::Const(b)) => {
            opt_type_match(a.ty.as_ref(), b.ty.as_ref()) && opt_empty(&a.value, &b.value)
        }
        (DeclBody::Static(a), DeclBody::Static(b)) => {
            a.is_mutable == b.is_mutable
                && opt_type_match(a.ty.as_ref(), b.ty.as_ref())
                && opt_empty(&a.value, &b.value)
        }
        (DeclBody::Namespace(a), DeclBody::Namespace(b)) => a.is_ambient == b.is_ambient,
        (
            DeclBody::Reexport {
                module_request: a_mod,
                import_name: a_name,
            },
            DeclBody::Reexport {
                module_request: b_mod,
                import_name: b_name,
            },
        ) => a_mod == b_mod && a_name == b_name,
        _ => false,
    }
}

fn variant_match(a: &VariantFact, b: &VariantFact) -> bool {
    a.name == b.name && opt_empty(&a.discriminant, &b.discriminant)
}

fn decorators_match(a: &[AttrTok], b: &[AttrTok]) -> bool {
    a.len() == b.len() && a.iter().zip(b).all(|(x, y)| x.token == y.token)
}

fn modifiers_match(a: &MemberModifiers, b: &MemberModifiers) -> bool {
    a.accessibility == b.accessibility
        && a.is_static == b.is_static
        && a.is_readonly == b.is_readonly
        && a.is_optional == b.is_optional
        && a.is_abstract == b.is_abstract
}

fn flags_match(a: &ClassFlags, b: &ClassFlags) -> bool {
    a.declare == b.declare && a.override_ == b.override_ && a.definite == b.definite
}

fn generics_match(a: &[GenericParamOwned], b: &[GenericParamOwned]) -> bool {
    a.len() == b.len()
        && a.iter().zip(b).all(|(x, y)| {
            x.variance == y.variance
                && x.name == y.name
                && types_match(&x.bounds, &y.bounds)
                && opt_type_match(x.default.as_ref(), y.default.as_ref())
        })
}

fn function_match(a: &FunctionBody, b: &FunctionBody) -> bool {
    a.is_async == b.is_async
        && a.is_generator == b.is_generator
        && a.has_body == b.has_body
        && a.receiver == b.receiver
        && generics_match(&a.generics, &b.generics)
        && a.abstract_construct == b.abstract_construct
        && a.leading_doc == b.leading_doc
        && a.body_text == b.body_text
        && opt_type_match(a.this_ty.as_ref(), b.this_ty.as_ref())
        && params_match(&a.params, &b.params)
        && opt_type_match(a.return_type.as_ref(), b.return_type.as_ref())
}

fn params_match(a: &[ParamFact], b: &[ParamFact]) -> bool {
    a.len() == b.len()
        && a.iter().zip(b).all(|(x, y)| {
            x.is_readonly == y.is_readonly
                && decorators_match(&x.decorators, &y.decorators)
                && x.name == y.name
                && x.initializer == y.initializer
                && x.is_optional == y.is_optional
                && x.is_rest == y.is_rest
                && opt_type_match(x.ty.as_ref(), y.ty.as_ref())
        })
}

fn member_match(a: &MemberFact, b: &MemberFact) -> bool {
    modifiers_match(&a.modifiers, &b.modifiers)
        && a.signature_kind == b.signature_kind
        && flags_match(&a.class_flags, &b.class_flags)
        && a.initializer == b.initializer
        && decorators_match(&a.decorators, &b.decorators)
        && doc_match(&a.doc, &b.doc)
        && a.name == b.name
        && member_kind_match(&a.kind, &b.kind)
}

fn member_kind_match(a: &MemberKind, b: &MemberKind) -> bool {
    match (a, b) {
        (MemberKind::Method(a), MemberKind::Method(b)) => {
            a.len() == b.len() && a.iter().zip(b).all(|(x, y)| function_match(x, y))
        }
        (MemberKind::Constructor(a), MemberKind::Constructor(b)) => function_match(a, b),
        (MemberKind::Property { ty: a }, MemberKind::Property { ty: b })
        | (MemberKind::Accessor { ty: a }, MemberKind::Accessor { ty: b }) => {
            opt_type_match(a.as_ref(), b.as_ref())
        }
        (MemberKind::StaticBlock { name: a }, MemberKind::StaticBlock { name: b }) => a == b,
        _ => false,
    }
}

fn method_match(a: &MethodFact, b: &MethodFact) -> bool {
    modifiers_match(&a.modifiers, &b.modifiers)
        && doc_match(&a.doc, &b.doc)
        && a.is_overload == b.is_overload
        && a.signature_kind == b.signature_kind
        && a.name == b.name
        && function_match(&a.sig, &b.sig)
}

fn property_match(a: &PropertyFact, b: &PropertyFact) -> bool {
    modifiers_match(&a.modifiers, &b.modifiers)
        && doc_match(&a.doc, &b.doc)
        && a.name == b.name
        && opt_type_match(a.ty.as_ref(), b.ty.as_ref())
}

fn indexes_match(a: &[IndexSignatureFact], b: &[IndexSignatureFact]) -> bool {
    a.len() == b.len()
        && a.iter().zip(b).all(|(x, y)| {
            x.doc == y.doc
                && x.is_static == y.is_static
                && x.readonly == y.readonly
                && x.key_name == y.key_name
                && type_match(&x.key_ty, &y.key_ty)
                && type_match(&x.value_ty, &y.value_ty)
        })
}

fn types_match(a: &[TypeOwned], b: &[TypeOwned]) -> bool {
    a.len() == b.len() && a.iter().zip(b).all(|(x, y)| type_match(x, y))
}

fn opt_type_match(a: Option<&TypeOwned>, b: Option<&TypeOwned>) -> bool {
    match (a, b) {
        (None, None) => true,
        (Some(a), Some(b)) => type_match(a, b),
        _ => false,
    }
}

fn literal_match(a: &LiteralOwned, b: &LiteralOwned) -> bool {
    match (a, b) {
        (LiteralOwned::Bool(a), LiteralOwned::Bool(b)) => a == b,
        (LiteralOwned::Number(a), LiteralOwned::Number(b))
        | (LiteralOwned::String(a), LiteralOwned::String(b))
        | (LiteralOwned::BigInt(a), LiteralOwned::BigInt(b)) => a == b,
        (LiteralOwned::Null, LiteralOwned::Null)
        | (LiteralOwned::Undefined, LiteralOwned::Undefined) => true,
        _ => false,
    }
}

fn template_match(a: &TemplatePart, b: &TemplatePart) -> bool {
    match (a, b) {
        (TemplatePart::Literal(a), TemplatePart::Literal(b)) => a == b,
        (TemplatePart::Interpolated(a), TemplatePart::Interpolated(b)) => type_match(a, b),
        _ => false,
    }
}

fn fields_match(a: &[AnonFieldOwned], b: &[AnonFieldOwned]) -> bool {
    a.len() == b.len()
        && a.iter().zip(b).all(|(x, y)| {
            x.name == y.name
                && type_match(&x.ty, &y.ty)
                && x.optional == y.optional
                && x.readonly == y.readonly
        })
}

fn type_match(a: &TypeOwned, b: &TypeOwned) -> bool {
    match (a, b) {
        (TypeOwned::Any, TypeOwned::Any)
        | (TypeOwned::Never, TypeOwned::Never)
        | (TypeOwned::Unknown, TypeOwned::Unknown)
        | (TypeOwned::Void, TypeOwned::Void)
        | (TypeOwned::Undefined, TypeOwned::Undefined)
        | (TypeOwned::Null, TypeOwned::Null)
        | (TypeOwned::Bool, TypeOwned::Bool)
        | (TypeOwned::Number, TypeOwned::Number)
        | (TypeOwned::BigInt, TypeOwned::BigInt)
        | (TypeOwned::String, TypeOwned::String)
        | (TypeOwned::Symbol, TypeOwned::Symbol)
        | (TypeOwned::Object, TypeOwned::Object)
        | (TypeOwned::This, TypeOwned::This) => true,
        (TypeOwned::Primitive(a), TypeOwned::Primitive(b))
        | (TypeOwned::Unsupported(a), TypeOwned::Unsupported(b))
        | (TypeOwned::Nominal(a), TypeOwned::Nominal(b))
        | (TypeOwned::TypeVar(a), TypeOwned::TypeVar(b)) => a == b,
        (
            TypeOwned::Apply {
                base: a_base,
                args: a_args,
            },
            TypeOwned::Apply {
                base: b_base,
                args: b_args,
            },
        ) => type_match(a_base, b_base) && types_match(a_args, b_args),
        (TypeOwned::Union(a), TypeOwned::Union(b))
        | (TypeOwned::Intersection(a), TypeOwned::Intersection(b))
        | (TypeOwned::Tuple(a), TypeOwned::Tuple(b)) => types_match(a, b),
        (TypeOwned::Array(a), TypeOwned::Array(b)) => type_match(a, b),
        (TypeOwned::Function(a), TypeOwned::Function(b)) => function_match(a, b),
        (
            TypeOwned::NamedTupleElem {
                label: a_label,
                ty: a_ty,
            },
            TypeOwned::NamedTupleElem {
                label: b_label,
                ty: b_ty,
            },
        ) => a_label == b_label && type_match(a_ty, b_ty),
        (TypeOwned::Literal(a), TypeOwned::Literal(b)) => literal_match(a, b),
        (
            TypeOwned::Conditional {
                check: a_check,
                extends_ty: a_extends,
                then_ty: a_then,
                else_ty: a_else,
            },
            TypeOwned::Conditional {
                check: b_check,
                extends_ty: b_extends,
                then_ty: b_then,
                else_ty: b_else,
            },
        ) => {
            type_match(a_check, b_check)
                && type_match(a_extends, b_extends)
                && type_match(a_then, b_then)
                && type_match(a_else, b_else)
        }
        (
            TypeOwned::Mapped {
                key_var: a_key,
                source: a_source,
                value: a_value,
                readonly: a_readonly,
                optional: a_optional,
            },
            TypeOwned::Mapped {
                key_var: b_key,
                source: b_source,
                value: b_value,
                readonly: b_readonly,
                optional: b_optional,
            },
        ) => {
            a_key == b_key
                && type_match(a_source, b_source)
                && type_match(a_value, b_value)
                && a_readonly == b_readonly
                && a_optional == b_optional
        }
        (TypeOwned::TemplateLiteral(a), TypeOwned::TemplateLiteral(b)) => {
            a.len() == b.len() && a.iter().zip(b).all(|(x, y)| template_match(x, y))
        }
        (TypeOwned::ObjectLiteral(a), TypeOwned::ObjectLiteral(b)) => fields_match(a, b),
        _ => false,
    }
}

pub(crate) fn lower_type(
    ty: &TypeOwned,
    out: &mut Lowering<TsId>,
    names: &DeclareSet,
    module: &Path,
) -> Type {
    match ty {
        // TypeScript is the language that makes the `Any` / `Unknown` split
        // unarguable, because it ships both and they are *not* interchangeable:
        //
        // - `unknown` is the genuine top type. Every value is assignable to it and no member access
        //   or narrowing is permitted until you refine it.
        // - `any` is the escape hatch. It is assignable in both directions and disables checking
        //   entirely — `--noImplicitAny` exists precisely because it is the thing you want to find
        //   and remove.
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
            if let Some(id) = names.resolve(module, name) {
                // Kind marker is erased by `into_raw`. The id is the one
                // `emit_decl` will declare for this name.
                out.nominal::<Module>(id)
            } else {
                Type::unresolved_external(name.clone())
            }
        }
        TypeOwned::TypeVar(name) => {
            // Extraction treats every bare identifier as a type variable when
            // it has no generic-parameter set. A name this package declares
            // (`AxiosResponse`) is a nominal ref. A real parameter (`T`) is not
            // in the declaration index and stays a type variable.
            if let Some(id) = names.resolve(module, name) {
                out.nominal::<Module>(id)
            } else {
                Type::TypeVar(name.clone())
            }
        }
        TypeOwned::Apply { base, args } => {
            let base_ty = lower_type(base, out, names, module);
            let arg_tys: Vec<Type> = args
                .iter()
                .map(|t| lower_type(t, out, names, module))
                .collect();
            Type::Apply {
                base: Box::new(base_ty),
                args: arg_tys.into_boxed_slice(),
            }
        }
        TypeOwned::Union(arms) => Type::Union(
            arms.iter()
                .map(|t| lower_type(t, out, names, module))
                .collect::<Vec<_>>()
                .into_boxed_slice(),
        ),
        TypeOwned::Intersection(arms) => Type::Intersection(
            arms.iter()
                .map(|t| lower_type(t, out, names, module))
                .collect::<Vec<_>>()
                .into_boxed_slice(),
        ),
        TypeOwned::Tuple(members) => {
            let elems: Vec<TupleElement> = members
                .iter()
                .map(|m| match m {
                    TypeOwned::NamedTupleElem { label, ty } => TupleElement::Named {
                        label: label.clone(),
                        ty: lower_type(ty, out, names, module),
                    },
                    other => TupleElement::Positional(lower_type(other, out, names, module)),
                })
                .collect();
            Type::Tuple(elems.into_boxed_slice())
        }
        TypeOwned::Array(inner) => {
            // Model `T[]` as a Slice.
            Type::Slice(Box::new(lower_type(inner, out, names, module)))
        }
        // ── Named tuple element (item 10) ─────────────────────────────────
        // When NamedTupleElem appears outside a Tuple context (the
        // TSType::TSNamedTupleMember top-level path in types.rs), wrap it in a
        // single-element named tuple so the label is not lost.  The normal path
        // is through TypeOwned::Tuple where each element is matched above.
        TypeOwned::NamedTupleElem { label, ty } => Type::Tuple(
            [TupleElement::Named {
                label: label.clone(),
                ty: lower_type(ty, out, names, module),
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
                .map(|p| {
                    p.ty.as_ref()
                        .map_or(Type::UNANNOTATED, |t| lower_type(t, out, names, module))
                })
                .collect();
            let ret = body
                .return_type
                .as_ref()
                .map(|r| Box::new(lower_type(r, out, names, module)));
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
            check: Box::new(lower_type(check, out, names, module)),
            extends_ty: Box::new(lower_type(extends_ty, out, names, module)),
            then_ty: Box::new(lower_type(then_ty, out, names, module)),
            else_ty: Box::new(lower_type(else_ty, out, names, module)),
        },
        TypeOwned::Mapped {
            key_var,
            source,
            value,
            readonly,
            optional,
        } => Type::Mapped {
            key_var: key_var.clone(),
            source: Box::new(lower_type(source, out, names, module)),
            value: Box::new(lower_type(value, out, names, module)),
            readonly: *readonly,
            optional: *optional,
        },
        TypeOwned::TemplateLiteral(parts) => {
            use nudox_ir::kinds::ty::TemplatePart as IrPart;
            let ir_parts: Vec<IrPart> = parts
                .iter()
                .map(|p| match p {
                    crate::typescript::extract::TemplatePart::Literal(s) => {
                        IrPart::Literal(s.clone())
                    }
                    crate::typescript::extract::TemplatePart::Interpolated(ty) => {
                        IrPart::Interpolated(Box::new(lower_type(ty, out, names, module)))
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
                    ty: lower_type(&m.ty, out, names, module),
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

// ── Generic param lowering
// ─────────────────────────────────────────────────────

fn lower_generics(
    params: &[GenericParamOwned],
    out: &mut Lowering<TsId>,
    names: &DeclareSet,
    module: &Path,
) -> Vec<GenericParam> {
    params
        .iter()
        .map(|p| GenericParam::Type {
            name: p.name.clone(),
            bounds: p
                .bounds
                .iter()
                .map(|t| lower_type(t, out, names, module))
                .collect::<Vec<_>>()
                .into_boxed_slice(),
            default: p
                .default
                .as_ref()
                .map(|t| lower_type(t, out, names, module)),
            // Wire TS 4.7+ `in`/`out` declaration-site variance annotations.
            // `None` when no modifier was present (the common case).
            variance: p.variance,
        })
        .collect()
}

// ── Symbol construction
// ────────────────────────────────────────────────────────

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

// ── Accessibility → Visibility
// ────────────────────────────────────────────────

fn accessibility_to_visibility(acc: Accessibility) -> Visibility {
    match acc {
        Accessibility::Public => Visibility::Public,
        Accessibility::Protected => Visibility::Protected,
        Accessibility::Private | Accessibility::PrivateField => Visibility::Private,
    }
}

// ── FieldAttributes
// ───────────────────────────────────────────────────────────

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
    use std::path::Path;

    fn bare(ty: &TypeOwned) -> Type {
        let mut out = Lowering::new(nudox_ir::id::PackageId::path("t"), Symbol {
            name: "root".to_string(),
            visibility: Visibility::Public,
            documentation: String::new(),
            source: Path::new("t.ts").to_path_buf(),
            span: 0..0,
            aliases: Box::new([]),
            deprecation: None,
            doc_links: Box::new([]),
            attrs: Box::new([]),
            cfg: None,
        });
        lower_type(ty, &mut out, &DeclareSet::empty(), Path::new("t.ts"))
    }

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
        let unknown = bare(&TypeOwned::Unknown);
        let any = bare(&TypeOwned::Any);
        assert_eq!(unknown, Type::Any, "`unknown` is TypeScript's top type");
        assert_eq!(
            any,
            Type::Unknown(UnknownType::DynamicallyTyped),
            "`any` is the gradual-typing escape hatch, not a top type"
        );
        assert_ne!(
            unknown, any,
            "`unknown` and `any` must not share an encoding"
        );
    }

    /// An *implicit* any — a parameter with no annotation at all — is a third
    /// thing again, and the one `--noImplicitAny` reports.
    #[test]
    fn implicit_any_is_unannotated_not_written_any() {
        let unannotated = Type::UNANNOTATED;
        assert_ne!(
            unannotated,
            bare(&TypeOwned::Any),
            "`(x) => …` and `(x: any) => …` are different source"
        );
        assert_ne!(unannotated, bare(&TypeOwned::Unknown));
        assert_eq!(unannotated.to_string(), "?unannotated");
    }

    #[test]
    fn unresolved_nominal_preserves_external_name() {
        let ty = bare(&TypeOwned::Nominal("ImportedWidget".to_string()));
        assert_eq!(
            ty,
            Type::Unknown(nudox_ir::kinds::UnknownType::UnresolvedExternal {
                name: "ImportedWidget".to_string(),
            }),
            "an unresolved imported type must not be erased as a builtin"
        );
    }
}
