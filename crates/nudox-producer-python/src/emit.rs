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
//! ## @property → Field + Function (dual emission)
//!
//! A `@property` method is emitted as BOTH a `Field` (so consumers that walk
//! fields see it with its return type) AND a `Function` (so the full method
//! signature is preserved). The `Field` carries `FieldAttribute::Mutable` only
//! when a corresponding setter is present (not yet tracked; left as not mutable
//! for now). The `Function` has `receiver = Some(Receiver::SharedRef)`.
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

use std::path::PathBuf;

use nudox_ir::{
    entry::{AttrTok, Deprecation, DocLink, Symbol, Visibility},
    kinds::{
        Alias, Const, Enum, Field, FieldAttribute, FieldKey, FnModifier, Function, Module, Param,
        ParamAttribute, Record, RecordForm, Trait, TraitFlags, Variant, VariantForm,
    },
    lower::Lowering,
};

use crate::{
    oracle::{
        ClassForm, DeprecationData, FieldData, FunctionData, ItemBody, ItemData, ModuleData,
        ParamKind, PythonId, PythonOracle, ReceiverKind,
    },
    types,
};

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

/// One-pass emission of all declarations from `oracle` into `out`.
///
/// Called from [`PythonProducer::lower`].
pub fn emit_package(oracle: &PythonOracle, out: &mut Lowering<PythonId>) {
    for module in &oracle.modules {
        emit_module(module, out);
    }
}

// ---------------------------------------------------------------------------
// Module
// ---------------------------------------------------------------------------

fn emit_module(module: &ModuleData, out: &mut Lowering<PythonId>) {
    let module_id = PythonId::new(module.name.clone());
    let sym = make_sym(
        &module.name,
        false,
        module.documentation.as_deref().unwrap_or(""),
        module.deprecation.as_ref(),
        &[],
    );
    out.declare(module_id.clone(), None, sym, Module);

    for item in &module.items {
        emit_item(item, Some(module_id.clone()), out);
    }
}

// ---------------------------------------------------------------------------
// Dispatch
// ---------------------------------------------------------------------------

fn emit_item(item: &ItemData, parent: Option<PythonId>, out: &mut Lowering<PythonId>) {
    match &item.body {
        ItemBody::Module => {
            let sym = make_sym_item(item);
            out.declare(item.id.clone(), parent, sym, Module);
        }
        ItemBody::Class(cls) => emit_class(item, cls, parent, out),
        ItemBody::Function(func) => emit_function(item, func, parent, out),
        ItemBody::Overloaded(branches) => {
            // Each overload is a separate declaration with id = `base#N`.
            for (i, branch) in branches.iter().enumerate() {
                let overload_id = PythonId::overload(item.id.as_str(), i);
                let sym = make_sym(
                    &item.name,
                    item.is_private,
                    item.documentation.as_deref().unwrap_or(""),
                    item.deprecation.as_ref(),
                    &item.decorators,
                );
                let fn_kind = build_function_kind(branch);
                out.declare(overload_id, parent.clone(), sym, fn_kind);
            }
        }
        ItemBody::Const(c) => emit_const(item, c, parent, out),
        ItemBody::Alias(a) => emit_alias(item, a, parent, out),
    }
}

// ---------------------------------------------------------------------------
// Class (Record / Enum / Trait)
// ---------------------------------------------------------------------------

fn emit_class(
    item: &ItemData,
    cls: &crate::oracle::ClassData,
    parent: Option<PythonId>,
    out: &mut Lowering<PythonId>,
) {
    let class_id = item.id.clone();

    // --- Enum subclass ---
    if cls.form == ClassForm::Enum {
        emit_enum(item, cls, parent, out);
        return;
    }

    // --- Protocol → Trait ---
    if cls.form == ClassForm::Protocol {
        emit_protocol(item, cls, parent, out);
        return;
    }

    // --- Plain / Dataclass / TypedDict / NamedTuple → Record ---
    let generics: Box<[_]> = types::lower_generics(&cls.generics);
    let super_types: Box<[_]> = cls
        .super_types
        .iter()
        .map(|t| types::lower_type(t))
        .collect();

    // Emit fields first (they need to be declared before the Record refers to them).
    let mut field_refs = Vec::with_capacity(cls.fields.len());
    for field in &cls.fields {
        let field_id = PythonId::new(format!("{}.{}", class_id.as_str(), field.name));
        let field_sym = make_field_sym(field, class_id.as_str());
        let field_kind = build_field_kind(field);
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

    let sym = make_sym_item(item);
    out.declare(class_id.clone(), parent, sym, record);

    // Emit methods.
    for method in &cls.methods {
        emit_item(method, Some(class_id.clone()), out);
    }
    // Emit nested classes.
    for nested in &cls.nested {
        emit_item(nested, Some(class_id.clone()), out);
    }
}

fn emit_enum(
    item: &ItemData,
    cls: &crate::oracle::ClassData,
    parent: Option<PythonId>,
    out: &mut Lowering<PythonId>,
) {
    let enum_id = item.id.clone();
    let generics: Box<[_]> = types::lower_generics(&cls.generics);

    // Emit variants.
    let mut variant_refs = Vec::with_capacity(cls.fields.len());
    for field in &cls.fields {
        let variant_id = PythonId::new(format!("{}.{}", enum_id.as_str(), field.name));
        let variant_sym = make_field_sym(field, enum_id.as_str());
        // Enum fields: unit variants carrying an optional value in `discr`.
        let value_str = field.ty.as_ref().map(|t| format!("{:?}", types::lower_type(t)));
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

    let sym = make_sym_item(item);
    out.declare(enum_id.clone(), parent, sym, enum_kind);

    // Enum classes may also have methods (e.g. custom __str__).
    for method in &cls.methods {
        emit_item(method, Some(enum_id.clone()), out);
    }
}

fn emit_protocol(
    item: &ItemData,
    cls: &crate::oracle::ClassData,
    parent: Option<PythonId>,
    out: &mut Lowering<PythonId>,
) {
    let trait_id = item.id.clone();
    let generics: Box<[_]> = types::lower_generics(&cls.generics);
    let supers: Box<[_]> = cls
        .super_types
        .iter()
        .map(|t| types::lower_type(t))
        .collect();

    let trait_kind = Trait::builder()
        .flags(TraitFlags::default())
        .supers(supers)
        .generics(generics)
        .build();

    let sym = make_sym_item(item);
    out.declare(trait_id.clone(), parent, sym, trait_kind);

    // Protocol methods.
    for method in &cls.methods {
        emit_item(method, Some(trait_id.clone()), out);
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
            documentation: param
                .doc_description
                .clone()
                .unwrap_or_default(),
            source: PathBuf::new(),
            span: 0..0,
            aliases: Box::new([]),
            deprecation: None,
            doc_links: Box::new([]),
            attrs: Box::new([]),
            cfg: None,
        };
        let ty = param.ty.as_ref().map(|t| types::lower_type(t));
        let mut attrs = Vec::new();
        match param.kind {
            ParamKind::PositionalOnly => attrs.push(ParamAttribute::Inout), // closest available
            ParamKind::KeywordOnly => {} // no ParamAttribute for keyword-only yet; noted below
            ParamKind::Varargs => attrs.push(ParamAttribute::Variadic),
            ParamKind::Kwargs => attrs.push(ParamAttribute::Variadic),
            ParamKind::Normal => {}
        }
        if param.has_default {
            attrs.push(ParamAttribute::Optional);
        }
        let param_kind = Param::builder().maybe_ty(ty).attributes(attrs).build();
        let param_ref =
            out.declare(param_id, Some(fn_id.clone()), param_sym, param_kind);
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
            source: PathBuf::new(),
            span: 0..0,
            aliases: Box::new([]),
            deprecation: None,
            doc_links: Box::new([]),
            attrs: Box::new([]),
            cfg: None,
        };
        let ret_kind = Param::builder()
            .maybe_ty(Some(types::lower_type(ret_ty)))
            .build();
        let ret_ref = out.declare(ret_id, Some(fn_id.clone()), ret_sym, ret_kind);
        output_refs.push(ret_ref);
    }

    let fn_kind = build_function_kind(func);
    // Override input/output with our declared param refs.
    let fn_kind = Function::builder()
        .maybe_receiver(fn_kind.receiver)
        .input_params(param_refs)
        .output_params(output_refs)
        .modifiers(fn_kind.modifiers.iter().copied())
        .generics(fn_kind.generics.iter().cloned())
        .build();

    let sym = make_sym_item(item);
    out.declare(fn_id, parent, sym, fn_kind);
}

/// Build a `Function` kind body from oracle data, WITHOUT emitting param children.
/// Used for overload branches where we want the function shape without children.
fn build_function_kind(func: &FunctionData) -> Function {
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

    let generics: Vec<_> = types::lower_generics(&func.generics).into_vec();

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
    c: &crate::oracle::ConstData,
    parent: Option<PythonId>,
    out: &mut Lowering<PythonId>,
) {
    let ty = c
        .ty
        .as_ref()
        .map(|t| types::lower_type(t))
        .unwrap_or(nudox_ir::kinds::Type::Any);

    let const_kind = Const::builder()
        .ty(ty)
        .maybe_value(c.value.clone())
        .build();

    let sym = make_sym_item(item);
    out.declare(item.id.clone(), parent, sym, const_kind);
}

fn emit_alias(
    item: &ItemData,
    a: &crate::oracle::AliasData,
    parent: Option<PythonId>,
    out: &mut Lowering<PythonId>,
) {
    let target = a.target.as_ref().map(|t| types::lower_type(t));
    let generics: Box<[_]> = types::lower_generics(&a.generics);

    let alias_kind = Alias::builder()
        .maybe_target(target)
        .generics(generics)
        .build();

    let sym = make_sym_item(item);
    out.declare(item.id.clone(), parent, sym, alias_kind);
}

// ---------------------------------------------------------------------------
// Symbol helpers
// ---------------------------------------------------------------------------

fn make_sym_item(item: &ItemData) -> Symbol {
    make_sym(
        &item.name,
        item.is_private,
        item.documentation.as_deref().unwrap_or(""),
        item.deprecation.as_ref(),
        &item.decorators,
    )
}

fn make_sym(
    name: &str,
    is_private: bool,
    documentation: &str,
    deprecation: Option<&DeprecationData>,
    decorators: &[String],
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
        source: PathBuf::new(),
        span: 0..0,
        aliases: Box::new([]),
        deprecation,
        doc_links: Box::new([]),
        attrs,
        cfg: None,
    }
}

fn make_field_sym(field: &FieldData, class_name: &str) -> Symbol {
    let _ = class_name;
    Symbol {
        name: field.name.clone(),
        visibility: if field.name.starts_with('_') {
            Visibility::Private
        } else {
            Visibility::Public
        },
        documentation: field.documentation.clone().unwrap_or_default(),
        source: PathBuf::new(),
        span: 0..0,
        aliases: Box::new([]),
        deprecation: None,
        doc_links: Box::new([DocLink {
            target: class_name.to_owned(),
            label: None,
        }]),
        attrs: Box::new([]),
        cfg: None,
    }
}

fn build_field_kind(field: &FieldData) -> Field {
    let ty = field.ty.as_ref().map(|t| types::lower_type(t));
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

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::oracle::{ClassData, ConstData, FunctionData, ReceiverKind};
    use nudox_ir::package::PackageId;

    fn root_sym() -> Symbol {
        Symbol {
            name: "test_pkg".to_owned(),
            visibility: Visibility::Public,
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

    fn simple_module(name: &str, items: Vec<ItemData>) -> ModuleData {
        ModuleData {
            name: name.to_owned(),
            documentation: None,
            deprecation: None,
            items,
        }
    }

    fn simple_function(id: &str, name: &str, is_async: bool) -> ItemData {
        ItemData {
            id: PythonId::new(id),
            parent: None,
            name: name.to_owned(),
            is_private: name.starts_with('_'),
            documentation: None,
            deprecation: None,
            decorators: vec![],
            body: ItemBody::Function(FunctionData {
                overload_index: 0,
                receiver: ReceiverKind::None,
                params: vec![],
                return_ty: None,
                generics: vec![],
                is_async,
                is_abstract: false,
                is_stub: false,
            }),
        }
    }

    #[test]
    fn emit_empty_module() {
        let oracle = PythonOracle {
            modules: vec![simple_module("my_module", vec![])],
        };
        let mut sink: Lowering<PythonId> =
            Lowering::new(PackageId::path("pkg"), root_sym());
        emit_package(&oracle, &mut sink);
        let pkg = sink.finish().expect("finish must succeed");
        // root + module = 2
        assert_eq!(pkg.iter().count(), 2);
    }

    #[test]
    fn emit_function_in_module() {
        let oracle = PythonOracle {
            modules: vec![simple_module(
                "my_module",
                vec![simple_function("my_module.greet", "greet", false)],
            )],
        };
        let mut sink: Lowering<PythonId> =
            Lowering::new(PackageId::path("pkg"), root_sym());
        emit_package(&oracle, &mut sink);
        let pkg = sink.finish().expect("finish must succeed");
        // root + module + function = 3
        assert_eq!(pkg.iter().count(), 3);
        assert!(
            pkg.iter().any(|(_, e)| e.sym().name == "greet"),
            "greet entry must be present"
        );
    }

    #[test]
    fn emit_async_function_has_async_modifier() {
        let oracle = PythonOracle {
            modules: vec![simple_module(
                "my_module",
                vec![simple_function("my_module.fetch", "fetch", true)],
            )],
        };
        let mut sink: Lowering<PythonId> =
            Lowering::new(PackageId::path("pkg"), root_sym());
        emit_package(&oracle, &mut sink);
        let pkg = sink.finish().expect("finish must succeed");

        let fetch = pkg
            .iter()
            .find(|(_, e)| e.sym().name == "fetch")
            .expect("fetch not found");
        let body = fetch
            .1
            .downcast::<Function>()
            .expect("fetch must be a Function");
        assert!(
            body.body().modifiers.iter().any(|m| *m == FnModifier::Async),
            "async function must carry FnModifier::Async"
        );
    }

    #[test]
    fn emit_overloads_as_separate_declarations() {
        use crate::oracle::ParamData;

        let params0 = vec![ParamData {
            name: "x".to_owned(),
            ty: Some(crate::oracle::TypeData::Nominal("int".to_owned())),
            kind: ParamKind::Normal,
            has_default: false,
            doc_description: None,
        }];
        let params1 = vec![ParamData {
            name: "x".to_owned(),
            ty: Some(crate::oracle::TypeData::Nominal("str".to_owned())),
            kind: ParamKind::Normal,
            has_default: false,
            doc_description: None,
        }];

        let overloaded_item = ItemData {
            id: PythonId::new("my_module.process"),
            parent: None,
            name: "process".to_owned(),
            is_private: false,
            documentation: None,
            deprecation: None,
            decorators: vec!["overload".to_owned()],
            body: ItemBody::Overloaded(vec![
                FunctionData {
                    overload_index: 0,
                    receiver: ReceiverKind::None,
                    params: params0,
                    return_ty: None,
                    generics: vec![],
                    is_async: false,
                    is_abstract: false,
                    is_stub: false,
                },
                FunctionData {
                    overload_index: 1,
                    receiver: ReceiverKind::None,
                    params: params1,
                    return_ty: None,
                    generics: vec![],
                    is_async: false,
                    is_abstract: false,
                    is_stub: false,
                },
            ]),
        };

        let oracle = PythonOracle {
            modules: vec![simple_module("my_module", vec![overloaded_item])],
        };
        let mut sink: Lowering<PythonId> =
            Lowering::new(PackageId::path("pkg"), root_sym());
        emit_package(&oracle, &mut sink);
        let pkg = sink.finish().expect("finish must succeed");

        // root + module + overload#0 + overload#1 = 4
        let overload_count = pkg
            .iter()
            .filter(|(_, e)| e.sym().name == "process")
            .count();
        assert_eq!(overload_count, 2, "must have exactly 2 overload declarations");
    }

    #[test]
    fn emit_class_with_methods() {
        use crate::oracle::{ClassData, ClassForm};

        let method_item = ItemData {
            id: PythonId::new("my_module.Point.distance"),
            parent: Some(PythonId::new("my_module.Point")),
            name: "distance".to_owned(),
            is_private: false,
            documentation: None,
            deprecation: None,
            decorators: vec![],
            body: ItemBody::Function(FunctionData {
                overload_index: 0,
                receiver: ReceiverKind::SharedRef,
                params: vec![],
                return_ty: Some(crate::oracle::TypeData::Nominal("float".to_owned())),
                generics: vec![],
                is_async: false,
                is_abstract: false,
                is_stub: false,
            }),
        };

        let class_item = ItemData {
            id: PythonId::new("my_module.Point"),
            parent: None,
            name: "Point".to_owned(),
            is_private: false,
            documentation: Some("A 2D point.".to_owned()),
            deprecation: None,
            decorators: vec![],
            body: ItemBody::Class(ClassData {
                super_types: vec![],
                generics: vec![],
                form: ClassForm::Plain,
                fields: vec![],
                methods: vec![method_item],
                nested: vec![],
            }),
        };

        let oracle = PythonOracle {
            modules: vec![simple_module("my_module", vec![class_item])],
        };
        let mut sink: Lowering<PythonId> =
            Lowering::new(PackageId::path("pkg"), root_sym());
        emit_package(&oracle, &mut sink);
        let pkg = sink.finish().expect("finish must succeed");

        assert!(
            pkg.iter().any(|(_, e)| e.sym().name == "Point"),
            "Point class must be present"
        );
        assert!(
            pkg.iter().any(|(_, e)| e.sym().name == "distance"),
            "distance method must be present"
        );
    }

    #[test]
    fn emit_enum_subclass() {
        use crate::oracle::{ClassData, ClassForm, FieldData};

        let color_item = ItemData {
            id: PythonId::new("my_module.Color"),
            parent: None,
            name: "Color".to_owned(),
            is_private: false,
            documentation: None,
            deprecation: None,
            decorators: vec![],
            body: ItemBody::Class(ClassData {
                super_types: vec![],
                generics: vec![],
                form: ClassForm::Enum,
                fields: vec![
                    FieldData {
                        name: "RED".to_owned(),
                        ty: Some(crate::oracle::TypeData::Nominal("int".to_owned())),
                        is_class_var: false,
                        is_final: false,
                        is_property: false,
                        has_default: false,
                        documentation: None,
                    },
                    FieldData {
                        name: "GREEN".to_owned(),
                        ty: None,
                        is_class_var: false,
                        is_final: false,
                        is_property: false,
                        has_default: false,
                        documentation: None,
                    },
                ],
                methods: vec![],
                nested: vec![],
            }),
        };

        let oracle = PythonOracle {
            modules: vec![simple_module("my_module", vec![color_item])],
        };
        let mut sink: Lowering<PythonId> =
            Lowering::new(PackageId::path("pkg"), root_sym());
        emit_package(&oracle, &mut sink);
        let pkg = sink.finish().expect("finish must succeed");

        // Check Color is an Enum kind.
        let color_entry = pkg
            .iter()
            .find(|(_, e)| e.sym().name == "Color")
            .expect("Color not found");
        assert!(
            color_entry.1.downcast::<Enum>().is_some(),
            "Color must be emitted as Enum kind"
        );

        // Both RED and GREEN variants must be present.
        let red = pkg.iter().find(|(_, e)| e.sym().name == "RED");
        let green = pkg.iter().find(|(_, e)| e.sym().name == "GREEN");
        assert!(red.is_some(), "RED variant must be present");
        assert!(green.is_some(), "GREEN variant must be present");
    }

    #[test]
    fn emit_type_alias() {
        use crate::oracle::AliasData;

        let alias_item = ItemData {
            id: PythonId::new("my_module.Scores"),
            parent: None,
            name: "Scores".to_owned(),
            is_private: false,
            documentation: None,
            deprecation: None,
            decorators: vec![],
            body: ItemBody::Alias(AliasData {
                target: Some(crate::oracle::TypeData::Apply {
                    base: Box::new(crate::oracle::TypeData::Nominal("list".to_owned())),
                    args: vec![crate::oracle::TypeData::Nominal("int".to_owned())],
                }),
                generics: vec![],
            }),
        };

        let oracle = PythonOracle {
            modules: vec![simple_module("my_module", vec![alias_item])],
        };
        let mut sink: Lowering<PythonId> =
            Lowering::new(PackageId::path("pkg"), root_sym());
        emit_package(&oracle, &mut sink);
        let pkg = sink.finish().expect("finish must succeed");

        let scores = pkg
            .iter()
            .find(|(_, e)| e.sym().name == "Scores")
            .expect("Scores not found");
        assert!(
            scores.1.downcast::<Alias>().is_some(),
            "Scores must be emitted as Alias kind"
        );
    }

    #[test]
    fn emit_const_in_module() {
        let const_item = ItemData {
            id: PythonId::new("my_module.MAX_RETRIES"),
            parent: None,
            name: "MAX_RETRIES".to_owned(),
            is_private: false,
            documentation: None,
            deprecation: None,
            decorators: vec![],
            body: ItemBody::Const(ConstData {
                ty: Some(crate::oracle::TypeData::Nominal("int".to_owned())),
                value: Some("3".to_owned()),
            }),
        };

        let oracle = PythonOracle {
            modules: vec![simple_module("my_module", vec![const_item])],
        };
        let mut sink: Lowering<PythonId> =
            Lowering::new(PackageId::path("pkg"), root_sym());
        emit_package(&oracle, &mut sink);
        let pkg = sink.finish().expect("finish must succeed");

        let max_r = pkg
            .iter()
            .find(|(_, e)| e.sym().name == "MAX_RETRIES")
            .expect("MAX_RETRIES not found");
        let const_body = max_r
            .1
            .downcast::<Const>()
            .expect("MAX_RETRIES must be a Const");
        assert_eq!(
            const_body.body().value.as_deref(),
            Some("3"),
            "const value must be preserved"
        );
    }

    #[test]
    fn emit_protocol() {
        use crate::oracle::{ClassData, ClassForm};

        let protocol_item = ItemData {
            id: PythonId::new("my_module.Drawable"),
            parent: None,
            name: "Drawable".to_owned(),
            is_private: false,
            documentation: None,
            deprecation: None,
            decorators: vec![],
            body: ItemBody::Class(ClassData {
                super_types: vec![],
                generics: vec![],
                form: ClassForm::Protocol,
                fields: vec![],
                methods: vec![simple_function(
                    "my_module.Drawable.draw",
                    "draw",
                    false,
                )],
                nested: vec![],
            }),
        };

        let oracle = PythonOracle {
            modules: vec![simple_module("my_module", vec![protocol_item])],
        };
        let mut sink: Lowering<PythonId> =
            Lowering::new(PackageId::path("pkg"), root_sym());
        emit_package(&oracle, &mut sink);
        let pkg = sink.finish().expect("finish must succeed");

        let drawable = pkg
            .iter()
            .find(|(_, e)| e.sym().name == "Drawable")
            .expect("Drawable not found");
        assert!(
            drawable.1.downcast::<Trait>().is_some(),
            "Protocol must be emitted as Trait kind"
        );
    }

    #[test]
    fn private_items_are_visibility_private() {
        let oracle = PythonOracle {
            modules: vec![simple_module(
                "my_module",
                vec![simple_function("my_module._helper", "_helper", false)],
            )],
        };
        let mut sink: Lowering<PythonId> =
            Lowering::new(PackageId::path("pkg"), root_sym());
        emit_package(&oracle, &mut sink);
        let pkg = sink.finish().expect("finish must succeed");

        let helper = pkg
            .iter()
            .find(|(_, e)| e.sym().name == "_helper")
            .expect("_helper not found");
        assert!(
            matches!(helper.1.sym().visibility, Visibility::Private),
            "_helper must be Private"
        );
    }
}
