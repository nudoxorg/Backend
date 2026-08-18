//! One-pass IR emission: walk [`PythonOracle`] and emit into [`Lowering`].
//!
//! # One-pass guarantee
//!
//! `emit_package` iterates over modules, then items, in one pass. No
//! intermediate tree is built. Parents may be declared after children
//! (Lowering is order-independent); the only constraint is that every id
//! referred to must ultimately be declared.
//!
//! # Design decisions
//!
//! ## @property → Function only (see `lib.rs` for the "@property decision")
//!
//! A `@property` method is emitted as a single `Function` child of its class,
//! carrying `AttrTok { token: "property", .. }` like any other decorator. No
//! dual `Field` is emitted — `lib.rs`'s module docs record why. (An earlier
//! version of this comment described a dual `Field` + `Function` emission
//! that was never implemented; this file's actual dispatch in `emit_item`
//! routes every class method, `@property` included, through the single
//! `emit_function` path below with no property-specific branch.)
//!
//! ## @classmethod → receiver = None, decorator "classmethod"
//!
//! A class method has no instance receiver; the `cls` parameter is a
//! conventional Python name that the IR models as a regular parameter
//! (dropping it after the receiver analysis). The `Symbol.attrs` list records
//! `AttrTok { token: "classmethod", arg: None }` so downstream consumers can
//! distinguish class methods from static methods.
//!
//! ## @staticmethod → receiver = None (no cls/self)
//!
//! Static methods have no receiver. The `Symbol.attrs` list records
//! `AttrTok { token: "staticmethod", arg: None }`.
//!
//! ## @overload — each branch its own declaration
//!
//! Per the mission brief: "each overload is its own declaration, never folded".
//! `ItemBody::Overloaded(branches)` emits one `Function` per branch, each with
//! its own `PythonId` (e.g. `module.fn_name#0`, `module.fn_name#1`). The
//! parent is the containing module/class.
//!
//! ## Enum subclasses → Enum + Variant
//!
//! `ClassForm::Enum` → `Enum` kind with one `Variant` per field.
//!
//! ## Protocol → Trait
//!
//! `ClassForm::Protocol` → `Trait` kind (flags default; `supers` from base classes).
//!
//! ## TypedDict / NamedTuple / Dataclass → Record with appropriate form
//!
//! All map to `Record { form: RecordForm::Struct, … }`.
//!
//! ## Module-level `TypeVar` — skipped
//!
//! `TypeVar` / `ParamSpec` / `TypeVarTuple` *values* are not public API surface
//! and are filtered before reaching this layer (handled in the oracle invoke
//! step). TypeVar *uses* in type annotations lower to `Type::TypeVar(name)`.

use std::collections::HashSet;
use std::path::PathBuf;

use nudox_ir::{
    entry::{AttrTok, Deprecation, DocLink, Symbol, Visibility},
    kinds::{
        Alias, Const, Enum, Field, FieldAttribute, FieldKey, FnModifier, Function, Module, Param,
        ParamAttribute, Record, RecordForm, Trait, TraitFlags, Variant, VariantForm,
    },
    lower::Lowering,
    vocab::{Confidence, ReferenceKind, RelSpan},
};

use crate::python::{
    oracle::{
        ClassForm, DeprecationData, FieldData, FunctionData, ItemBody, ItemData, ModuleData,
        ParamKind, PythonId, PythonOracle, ReceiverKind,
    },
    types::{self, KnownIds},
};

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

/// One-pass emission of all declarations from `oracle` into `out`.
///
/// Called from [`PythonProducer::lower`].
pub fn emit_package(oracle: &PythonOracle, out: &mut Lowering<PythonId>) {
    // Build the set of all fully-qualified IDs declared in this package so
    // that `lower_type` can resolve same-package nominals to real Refs — and,
    // when it cannot, can still tell a locally-declared short name apart from
    // a genuinely external one. See [`KnownIds`].
    let known_ids = KnownIds::new(collect_known_ids(oracle));

    for module in &oracle.modules {
        emit_module(module, out, &known_ids);
        for reference in &module.references {
            if known_ids.contains(reference.target.as_str()) {
                let Ok(start) = u32::try_from(reference.span.start) else {
                    continue;
                };
                let Ok(end) = u32::try_from(reference.span.end) else {
                    continue;
                };
                out.record_occurrence(
                    reference.owner.clone(),
                    reference.target.clone(),
                    ReferenceKind::FunctionCall,
                    Confidence::Syntactic,
                    RelSpan::new(start, end),
                );
            }
        }
    }
}

/// Walk the oracle and collect every PythonId string that will be declared.
///
/// This is a cheap pre-pass (no allocation per item, just string clones) that
/// lets `lower_type` answer "is this name same-package?" without requiring a
/// two-pass emission loop.
fn collect_known_ids(oracle: &PythonOracle) -> HashSet<String> {
    let mut ids = HashSet::new();
    for module in &oracle.modules {
        ids.insert(module.name.clone());
        for item in &module.items {
            collect_item_ids(item, &mut ids);
        }
    }
    ids
}

fn collect_item_ids(item: &ItemData, ids: &mut HashSet<String>) {
    ids.insert(item.id.0.clone());
    match &item.body {
        ItemBody::Class(cls) => {
            for field in &cls.fields {
                ids.insert(format!("{}.{}", item.id.as_str(), field.name));
            }
            for method in &cls.methods {
                collect_item_ids(method, ids);
            }
            for nested in &cls.nested {
                collect_item_ids(nested, ids);
            }
        }
        ItemBody::Function(func) => {
            for param in &func.params {
                if param.name != "self" && param.name != "cls" {
                    ids.insert(format!("{}.{}", item.id.as_str(), param.name));
                }
            }
        }
        ItemBody::Overloaded(branches) => {
            for (i, _branch) in branches.iter().enumerate() {
                ids.insert(format!("{}#{i}", item.id.as_str()));
            }
        }
        ItemBody::Module | ItemBody::Const(_) | ItemBody::Alias(_) => {}
    }
}

// ---------------------------------------------------------------------------
// Module
// ---------------------------------------------------------------------------

fn emit_module(module: &ModuleData, out: &mut Lowering<PythonId>, known_ids: &KnownIds) {
    let module_id = PythonId::new(module.name.clone());
    let sym = make_sym(
        &module.name,
        false,
        module.documentation.as_deref().unwrap_or(""),
        module.deprecation.as_ref(),
        &[],
        module.span.clone(),
        &module.source,
    );
    out.declare(module_id.clone(), None, sym, Module);

    for item in &module.items {
        emit_item(
            item,
            Some(module_id.clone()),
            out,
            known_ids,
            &module.source,
        );
    }
}

// ---------------------------------------------------------------------------
// Dispatch
// ---------------------------------------------------------------------------

fn emit_item(
    item: &ItemData,
    parent: Option<PythonId>,
    out: &mut Lowering<PythonId>,
    known_ids: &KnownIds,
    source: &std::path::Path,
) {
    match &item.body {
        ItemBody::Module => {
            let sym = make_sym_item(item, source);
            out.declare(item.id.clone(), parent, sym, Module);
        }
        ItemBody::Class(cls) => emit_class(item, cls, parent, out, known_ids, source),
        ItemBody::Function(func) => emit_function(item, func, parent, out, known_ids, source),
        ItemBody::Overloaded(branches) => {
            // Each overload is a separate declaration with id = `base#N`,
            // and — unlike `name`/`documentation`/`decorators`, which are
            // shared from the group's first branch (`syntax.rs`'s
            // `build_function_group`) — each branch carries its *own* span,
            // because each is its own `def` statement in the source.
            for (i, branch) in branches.iter().enumerate() {
                let overload_id = PythonId::overload(item.id.as_str(), i);
                let sym = make_sym(
                    &item.name,
                    item.is_private,
                    item.documentation.as_deref().unwrap_or(""),
                    item.deprecation.as_ref(),
                    &item.decorators,
                    branch.span.clone(),
                    source,
                );
                let fn_kind = build_function_kind(branch, out, known_ids);
                out.declare(overload_id, parent.clone(), sym, fn_kind);
            }
        }
        ItemBody::Const(c) => emit_const(item, c, parent, out, known_ids, source),
        ItemBody::Alias(a) => emit_alias(item, a, parent, out, known_ids, source),
    }
}

// ---------------------------------------------------------------------------
// Class (Record / Enum / Trait)
// ---------------------------------------------------------------------------

fn emit_class(
    item: &ItemData,
    cls: &crate::python::oracle::ClassData,
    parent: Option<PythonId>,
    out: &mut Lowering<PythonId>,
    known_ids: &KnownIds,
    source: &std::path::Path,
) {
    let class_id = item.id.clone();

    // --- Enum subclass ---
    if cls.form == ClassForm::Enum {
        emit_enum(item, cls, parent, out, known_ids, source);
        return;
    }

    // --- Protocol → Trait ---
    if cls.form == ClassForm::Protocol {
        emit_protocol(item, cls, parent, out, known_ids, source);
        return;
    }

    // --- Plain / Dataclass / TypedDict / NamedTuple → Record ---
    let generics: Box<[_]> = types::lower_generics(&cls.generics, out, known_ids);
    let mut super_types: Vec<_> = Vec::with_capacity(cls.super_types.len());
    for t in &cls.super_types {
        super_types.push(types::lower_type(t, out, known_ids));
    }

    // Emit fields first (they need to be declared before the Record refers to them).
    let mut field_refs = Vec::with_capacity(cls.fields.len());
    for field in &cls.fields {
        let field_id = PythonId::new(format!("{}.{}", class_id.as_str(), field.name));
        let field_sym = make_field_sym(field, class_id.as_str(), source);
        let field_kind = build_field_kind(field, out, known_ids);
        let field_ref = out.declare(field_id, Some(class_id.clone()), field_sym, field_kind);
        field_refs.push(field_ref);
    }

    let record = Record::builder()
        .form(match cls.form {
            ClassForm::TypedDict | ClassForm::NamedTuple => RecordForm::Struct,
            _ => RecordForm::Struct,
        })
        .fields(field_refs)
        .super_types(super_types)
        .generics(generics)
        .build();

    let sym = make_sym_item(item, source);
    out.declare(class_id.clone(), parent, sym, record);

    // Emit methods.
    for method in &cls.methods {
        emit_item(method, Some(class_id.clone()), out, known_ids, source);
    }
    // Emit nested classes.
    for nested in &cls.nested {
        emit_item(nested, Some(class_id.clone()), out, known_ids, source);
    }
}

fn emit_enum(
    item: &ItemData,
    cls: &crate::python::oracle::ClassData,
    parent: Option<PythonId>,
    out: &mut Lowering<PythonId>,
    known_ids: &KnownIds,
    source: &std::path::Path,
) {
    let enum_id = item.id.clone();
    let generics: Box<[_]> = types::lower_generics(&cls.generics, out, known_ids);

    // Emit variants.
    let mut variant_refs = Vec::with_capacity(cls.fields.len());
    for field in &cls.fields {
        let variant_id = PythonId::new(format!("{}.{}", enum_id.as_str(), field.name));
        let variant_sym = make_field_sym(field, enum_id.as_str(), source);
        // Enum fields: unit variants carrying an optional value in `discr`.
        let value_str = field
            .ty
            .as_ref()
            .map(|t| format!("{:?}", types::lower_type(t, out, known_ids)));
        let variant_kind = Variant::builder()
            .form(VariantForm::Unit)
            .maybe_discr(value_str)
            .build();
        let variant_ref = out.declare(variant_id, Some(enum_id.clone()), variant_sym, variant_kind);
        variant_refs.push(variant_ref);
    }

    let enum_kind = Enum::builder()
        .variants(variant_refs)
        .generics(generics)
        .build();

    let sym = make_sym_item(item, source);
    out.declare(enum_id.clone(), parent, sym, enum_kind);

    // Enum classes may also have methods (e.g. custom __str__).
    for method in &cls.methods {
        emit_item(method, Some(enum_id.clone()), out, known_ids, source);
    }
}

fn emit_protocol(
    item: &ItemData,
    cls: &crate::python::oracle::ClassData,
    parent: Option<PythonId>,
    out: &mut Lowering<PythonId>,
    known_ids: &KnownIds,
    source: &std::path::Path,
) {
    let trait_id = item.id.clone();
    let generics: Box<[_]> = types::lower_generics(&cls.generics, out, known_ids);
    let mut supers: Vec<_> = Vec::with_capacity(cls.super_types.len());
    for t in &cls.super_types {
        supers.push(types::lower_type(t, out, known_ids));
    }

    let trait_kind = Trait::builder()
        .flags(TraitFlags::default())
        .supers(supers)
        .generics(generics)
        .build();

    let sym = make_sym_item(item, source);
    out.declare(trait_id.clone(), parent, sym, trait_kind);

    // Protocol methods.
    for method in &cls.methods {
        emit_item(method, Some(trait_id.clone()), out, known_ids, source);
    }
}

// ---------------------------------------------------------------------------
// Function / Method
// ---------------------------------------------------------------------------

fn emit_function(
    item: &ItemData,
    func: &FunctionData,
    parent: Option<PythonId>,
    out: &mut Lowering<PythonId>,
    known_ids: &KnownIds,
    source: &std::path::Path,
) {
    let fn_id = item.id.clone();

    // Emit parameters as child entries.
    let mut param_refs = Vec::with_capacity(func.params.len());
    for param in &func.params {
        // Skip `self` / `cls` — they're reflected in the receiver field.
        if param.name == "self" || param.name == "cls" {
            continue;
        }
        let param_id = PythonId::new(format!("{}.{}", fn_id.as_str(), param.name));
        let param_sym = Symbol {
            name: param.name.clone(),
            visibility: Visibility::Private,
            documentation: param.doc_description.clone().unwrap_or_default(),
            source: source.to_path_buf(),
            span: param.span.clone(),
            aliases: Box::new([]),
            deprecation: None,
            doc_links: Box::new([]),
            attrs: Box::new([]),
            cfg: None,
        };
        // 19,741 of the 87,101 censused annotation positions (22.7%) are
        // unannotated parameters. Under `Param::ty = None` the IR could not
        // represent that at all — `None` is also what a producer emits when it
        // has not looked. `Unannotated` says the source wrote nothing, which
        // is a different and checkable claim.
        let ty = Some(match &param.ty {
            Some(t) => types::lower_type(t, out, known_ids),
            None => nudox_ir::kinds::Type::UNANNOTATED,
        });
        let mut attrs = Vec::new();
        match param.kind {
            // `nudox_ir::kinds::ParamAttribute` has no positional-only
            // counterpart to `KeywordOnly` (it models Swift/C++-style
            // reference passing, not Python's `/`-marker calling-convention
            // restriction). Previously this pushed `ParamAttribute::Inout`
            // as "closest available", which actively misreports a Python
            // positional-only parameter as pass-by-mutable-reference — a
            // concept Python does not have. Emitting no attribute is more
            // honest than emitting a wrong one. See `lib.rs`'s "Constructs
            // not yet representable" for the upstream `nudox-ir` gap this
            // depends on (out of this crate's scope to add).
            ParamKind::PositionalOnly => {}
            // `ParamAttribute::KeywordOnly` exists precisely for this case
            // (its own doc comment cites Python's `def f(a, *, b)`) — it was
            // previously left unused here under a comment claiming no such
            // attribute existed.
            ParamKind::KeywordOnly => attrs.push(ParamAttribute::KeywordOnly),
            ParamKind::Varargs => attrs.push(ParamAttribute::Variadic),
            ParamKind::Kwargs => attrs.push(ParamAttribute::Kwargs),
            ParamKind::Normal => {}
        }
        if param.has_default {
            attrs.push(ParamAttribute::Optional);
        }
        let param_kind = Param::builder().maybe_ty(ty).attributes(attrs).build();
        let param_ref = out.declare(param_id, Some(fn_id.clone()), param_sym, param_kind);
        param_refs.push(param_ref);
    }

    // Return type: emit as a Param child with empty name (IR convention).
    let mut output_refs = Vec::new();
    if let Some(ret_ty) = &func.return_ty {
        let ret_id = PythonId::new(format!("{}.__return__", fn_id.as_str()));
        let ret_sym = Symbol {
            name: String::new(),
            visibility: Visibility::Private,
            documentation: String::new(),
            source: source.to_path_buf(),
            // `return_span` is `Some` whenever the source itself wrote a
            // `-> ReturnType` annotation (`syntax.rs::function_data`). The
            // one case it can be `None` while `return_ty` is `Some` is the
            // pyrefly semantic tier inferring a return type for a function
            // that wrote none at all (`context.rs::merge_type`) — there is
            // no annotation text to point at, so fall back to the whole
            // function's own span rather than fabricate a more precise
            // location than the source actually has.
            span: func
                .return_span
                .clone()
                .unwrap_or_else(|| item.span.clone()),
            aliases: Box::new([]),
            deprecation: None,
            doc_links: Box::new([]),
            attrs: Box::new([]),
            cfg: None,
        };
        let ret_kind = Param::builder()
            .maybe_ty(Some(types::lower_type(ret_ty, out, known_ids)))
            .build();
        let ret_ref = out.declare(ret_id, Some(fn_id.clone()), ret_sym, ret_kind);
        output_refs.push(ret_ref);
    }

    let fn_kind = build_function_kind(func, out, known_ids);
    // Override input/output with our declared param refs.
    let fn_kind = Function::builder()
        .maybe_receiver(fn_kind.receiver)
        .input_params(param_refs)
        .output_params(output_refs)
        .modifiers(fn_kind.modifiers.iter().copied())
        .generics(fn_kind.generics.iter().cloned())
        .build();

    let sym = make_sym_item(item, source);
    out.declare(fn_id, parent, sym, fn_kind);
}

/// Build a `Function` kind body from oracle data, WITHOUT emitting param children.
/// Used for overload branches where we want the function shape without children.
fn build_function_kind(
    func: &FunctionData,
    out: &mut Lowering<PythonId>,
    known_ids: &KnownIds,
) -> Function {
    let receiver = match func.receiver {
        ReceiverKind::SharedRef => Some(nudox_ir::kinds::Receiver::SharedRef),
        ReceiverKind::ClassMethod => None, // modeled via attrs
        ReceiverKind::Static => None,
        ReceiverKind::None => None,
    };

    let mut modifiers: Vec<FnModifier> = Vec::new();
    if func.is_async {
        modifiers.push(FnModifier::Async);
    }

    let generics: Vec<_> = types::lower_generics(&func.generics, out, known_ids).into_vec();

    Function::builder()
        .maybe_receiver(receiver)
        .modifiers(modifiers)
        .generics(generics)
        .is_defaulted(func.is_abstract || func.is_stub)
        .build()
}

// ---------------------------------------------------------------------------
// Const / Alias
// ---------------------------------------------------------------------------

fn emit_const(
    item: &ItemData,
    c: &crate::python::oracle::ConstData,
    parent: Option<PythonId>,
    out: &mut Lowering<PythonId>,
    known_ids: &KnownIds,
    source: &std::path::Path,
) {
    // A module-level constant with no annotation is *unannotated*, not
    // dynamic and not `object`. `Type::Any` here asserted the source had
    // written something it had not.
    let ty =
        c.ty.as_ref()
            .map(|t| types::lower_type(t, out, known_ids))
            .unwrap_or(nudox_ir::kinds::Type::UNANNOTATED);

    let const_kind = Const::builder()
        .ty(ty.clone())
        .maybe_value(c.value.clone().map(|source| {
            nudox_ir::build::ConstExpr::builder()
                .ty(ty)
                .source(source)
                .build()
        }))
        .build();

    let sym = make_sym_item(item, source);
    out.declare(item.id.clone(), parent, sym, const_kind);
}

fn emit_alias(
    item: &ItemData,
    a: &crate::python::oracle::AliasData,
    parent: Option<PythonId>,
    out: &mut Lowering<PythonId>,
    known_ids: &KnownIds,
    source: &std::path::Path,
) {
    let target = a
        .target
        .as_ref()
        .map(|t| types::lower_type(t, out, known_ids));
    let generics: Box<[_]> = types::lower_generics(&a.generics, out, known_ids);

    let alias_kind = Alias::builder()
        .maybe_target(target)
        .generics(generics)
        .build();

    let sym = make_sym_item(item, source);
    out.declare(item.id.clone(), parent, sym, alias_kind);
}

// ---------------------------------------------------------------------------
// Symbol helpers
// ---------------------------------------------------------------------------

fn make_sym_item(item: &ItemData, source: &std::path::Path) -> Symbol {
    make_sym(
        &item.name,
        item.is_private,
        item.documentation.as_deref().unwrap_or(""),
        item.deprecation.as_ref(),
        &item.decorators,
        item.span.clone(),
        source,
    )
}

fn make_sym(
    name: &str,
    is_private: bool,
    documentation: &str,
    deprecation: Option<&DeprecationData>,
    decorators: &[String],
    span: std::ops::Range<usize>,
    source: &std::path::Path,
) -> Symbol {
    let visibility = if is_private {
        Visibility::Private
    } else {
        Visibility::Public
    };
    let deprecation = deprecation.map(|d| Deprecation {
        note: d.note.clone(),
        since: d.since.clone(),
    });
    let attrs: Box<[AttrTok]> = decorators
        .iter()
        .map(|d| AttrTok {
            token: d.clone(),
            arg: None,
        })
        .collect();
    Symbol {
        name: name.to_owned(),
        visibility,
        documentation: documentation.to_owned(),
        source: source.to_path_buf(),
        span,
        aliases: Box::new([]),
        deprecation,
        doc_links: Box::new([]),
        attrs,
        cfg: None,
    }
}

fn make_field_sym(field: &FieldData, class_name: &str, source: &std::path::Path) -> Symbol {
    let _ = class_name;
    Symbol {
        name: field.name.clone(),
        visibility: if field.name.starts_with('_') {
            Visibility::Private
        } else {
            Visibility::Public
        },
        documentation: field.documentation.clone().unwrap_or_default(),
        source: source.to_path_buf(),
        span: field.span.clone(),
        aliases: Box::new([]),
        deprecation: None,
        doc_links: Box::new([DocLink {
            target: class_name.to_owned(),
            label: None,
            source_span: None,
        }]),
        attrs: Box::new([]),
        cfg: None,
    }
}

fn build_field_kind(
    field: &FieldData,
    out: &mut Lowering<PythonId>,
    known_ids: &KnownIds,
) -> Field {
    // `Field::ty = None` meant "the producer has nothing to say", which is
    // indistinguishable from "the producer never ran". A Python field with no
    // annotation is a *fact about the source*, so record it as one.
    let ty = Some(match &field.ty {
        Some(t) => types::lower_type(t, out, known_ids),
        None => nudox_ir::kinds::Type::UNANNOTATED,
    });
    let mut attrs = Vec::new();
    if !field.is_final && !field.is_class_var {
        attrs.push(FieldAttribute::Mutable);
    }
    if field.has_default {
        attrs.push(FieldAttribute::Optional);
    }
    if field.is_class_var {
        attrs.push(FieldAttribute::Static);
    }
    Field::builder()
        .key(FieldKey::Named)
        .maybe_ty(ty)
        .attributes(attrs)
        .build()
}

#[cfg(test)]
mod tests;
