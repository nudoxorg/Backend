//! Unit tests for the Python emit layer.

use super::*;
use crate::oracle::{ConstData, FunctionData, ReceiverKind};
use nudox_ir::kinds::ty::Type;
use nudox_ir::kinds::Param;
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
    ModuleData { name: name.to_owned(), documentation: None, deprecation: None, items, span: 0..0 }
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
        span: 0..0,
        body: ItemBody::Function(FunctionData {
            overload_index: 0,
            span: 0..0,
            receiver: ReceiverKind::None,
            params: vec![],
            return_ty: None,
            return_span: None,
            generics: vec![],
            is_async,
            is_abstract: false,
            is_stub: false,
        }),
    }
}

#[test]
fn emit_empty_module() {
    let oracle = PythonOracle { modules: vec![simple_module("my_module", vec![])] };
    let mut sink: Lowering<PythonId> = Lowering::new(PackageId::path("pkg"), root_sym());
    emit_package(&oracle, &mut sink);
    let pkg = sink.finish().expect("finish must succeed");
    assert_eq!(pkg.iter().count(), 2);
}

#[test]
fn emit_function_in_module() {
    let oracle = PythonOracle {
        modules: vec![simple_module("my_module", vec![simple_function("my_module.greet", "greet", false)])],
    };
    let mut sink: Lowering<PythonId> = Lowering::new(PackageId::path("pkg"), root_sym());
    emit_package(&oracle, &mut sink);
    let pkg = sink.finish().expect("finish must succeed");
    assert_eq!(pkg.iter().count(), 3);
    assert!(pkg.iter().any(|(_, e)| e.sym().name == "greet"), "greet entry must be present");
}

#[test]
fn emit_async_function_has_async_modifier() {
    let oracle = PythonOracle {
        modules: vec![simple_module("my_module", vec![simple_function("my_module.fetch", "fetch", true)])],
    };
    let mut sink: Lowering<PythonId> = Lowering::new(PackageId::path("pkg"), root_sym());
    emit_package(&oracle, &mut sink);
    let pkg = sink.finish().expect("finish must succeed");

    let fetch = pkg.iter().find(|(_, e)| e.sym().name == "fetch").expect("fetch not found");
    let body = fetch.1.downcast::<Function>().expect("fetch must be a Function");
    assert!(body.body().modifiers.contains(&FnModifier::Async), "async function must carry FnModifier::Async");
}

#[test]
fn emit_overloads_as_separate_declarations() {
    use crate::oracle::ParamData;

    let params0 = vec![ParamData {
        name: "x".to_owned(), ty: Some(crate::oracle::TypeData::Nominal("int".to_owned())), kind: ParamKind::Normal, has_default: false, doc_description: None, span: 0..0,
    }];
    let params1 = vec![ParamData {
        name: "x".to_owned(), ty: Some(crate::oracle::TypeData::Nominal("str".to_owned())), kind: ParamKind::Normal, has_default: false, doc_description: None, span: 0..0,
    }];

    let overloaded_item = ItemData {
        id: PythonId::new("my_module.process"),
        parent: None,
        name: "process".to_owned(),
        is_private: false,
        documentation: None,
        deprecation: None,
        decorators: vec!["overload".to_owned()],
        span: 0..0,
        body: ItemBody::Overloaded(vec![
            FunctionData { overload_index: 0, span: 0..0, receiver: ReceiverKind::None, params: params0, return_ty: None, return_span: None, generics: vec![], is_async: false, is_abstract: false, is_stub: false },
            FunctionData { overload_index: 1, span: 1..1, receiver: ReceiverKind::None, params: params1, return_ty: None, return_span: None, generics: vec![], is_async: false, is_abstract: false, is_stub: false },
        ]),
    };

    let oracle = PythonOracle { modules: vec![simple_module("my_module", vec![overloaded_item])] };
    let mut sink: Lowering<PythonId> = Lowering::new(PackageId::path("pkg"), root_sym());
    emit_package(&oracle, &mut sink);
    let pkg = sink.finish().expect("finish must succeed");

    let overload_count = pkg.iter().filter(|(_, e)| e.sym().name == "process").count();
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
        span: 0..0,
        body: ItemBody::Function(FunctionData {
            overload_index: 0,
            span: 0..0,
            receiver: ReceiverKind::SharedRef,
            params: vec![],
            return_ty: Some(crate::oracle::TypeData::Nominal("float".to_owned())),
            return_span: None,
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
        span: 0..0,
        body: ItemBody::Class(ClassData {
            super_types: vec![], generics: vec![], form: ClassForm::Plain, fields: vec![], methods: vec![method_item], nested: vec![],
        }),
    };

    let oracle = PythonOracle { modules: vec![simple_module("my_module", vec![class_item])] };
    let mut sink: Lowering<PythonId> = Lowering::new(PackageId::path("pkg"), root_sym());
    emit_package(&oracle, &mut sink);
    let pkg = sink.finish().expect("finish must succeed");

    assert!(pkg.iter().any(|(_, e)| e.sym().name == "Point"), "Point class must be present");
    assert!(pkg.iter().any(|(_, e)| e.sym().name == "distance"), "distance method must be present");
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
        span: 0..0,
        body: ItemBody::Class(ClassData {
            super_types: vec![], generics: vec![], form: ClassForm::Enum,
            fields: vec![
                FieldData { name: "RED".to_owned(), ty: Some(crate::oracle::TypeData::Nominal("int".to_owned())), is_class_var: false, is_final: false, is_property: false, has_default: false, documentation: None, span: 0..0 },
                FieldData { name: "GREEN".to_owned(), ty: None, is_class_var: false, is_final: false, is_property: false, has_default: false, documentation: None, span: 0..0 },
            ],
            methods: vec![], nested: vec![],
        }),
    };

    let oracle = PythonOracle { modules: vec![simple_module("my_module", vec![color_item])] };
    let mut sink: Lowering<PythonId> = Lowering::new(PackageId::path("pkg"), root_sym());
    emit_package(&oracle, &mut sink);
    let pkg = sink.finish().expect("finish must succeed");

    let color_entry = pkg.iter().find(|(_, e)| e.sym().name == "Color").expect("Color not found");
    assert!(color_entry.1.downcast::<Enum>().is_some(), "Color must be emitted as Enum kind");

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
        span: 0..0,
        body: ItemBody::Alias(AliasData {
            target: Some(crate::oracle::TypeData::Apply {
                base: Box::new(crate::oracle::TypeData::Nominal("list".to_owned())),
                args: vec![crate::oracle::TypeData::Nominal("int".to_owned())],
            }),
            generics: vec![],
        }),
    };

    let oracle = PythonOracle { modules: vec![simple_module("my_module", vec![alias_item])] };
    let mut sink: Lowering<PythonId> = Lowering::new(PackageId::path("pkg"), root_sym());
    emit_package(&oracle, &mut sink);
    let pkg = sink.finish().expect("finish must succeed");

    let scores = pkg.iter().find(|(_, e)| e.sym().name == "Scores").expect("Scores not found");
    assert!(scores.1.downcast::<Alias>().is_some(), "Scores must be emitted as Alias kind");
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
        span: 0..0,
        body: ItemBody::Const(ConstData { ty: Some(crate::oracle::TypeData::Nominal("int".to_owned())), value: Some("3".to_owned()) }),
    };

    let oracle = PythonOracle { modules: vec![simple_module("my_module", vec![const_item])] };
    let mut sink: Lowering<PythonId> = Lowering::new(PackageId::path("pkg"), root_sym());
    emit_package(&oracle, &mut sink);
    let pkg = sink.finish().expect("finish must succeed");

    let max_r = pkg.iter().find(|(_, e)| e.sym().name == "MAX_RETRIES").expect("MAX_RETRIES not found");
    let const_body = max_r.1.downcast::<Const>().expect("MAX_RETRIES must be a Const");
    assert_eq!(const_body.body().value.as_deref(), Some("3"), "const value must be preserved");
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
        span: 0..0,
        body: ItemBody::Class(ClassData {
            super_types: vec![], generics: vec![], form: ClassForm::Protocol, fields: vec![],
            methods: vec![simple_function("my_module.Drawable.draw", "draw", false)],
            nested: vec![],
        }),
    };

    let oracle = PythonOracle { modules: vec![simple_module("my_module", vec![protocol_item])] };
    let mut sink: Lowering<PythonId> = Lowering::new(PackageId::path("pkg"), root_sym());
    emit_package(&oracle, &mut sink);
    let pkg = sink.finish().expect("finish must succeed");

    let drawable = pkg.iter().find(|(_, e)| e.sym().name == "Drawable").expect("Drawable not found");
    assert!(drawable.1.downcast::<Trait>().is_some(), "Protocol must be emitted as Trait kind");
}

#[test]
fn private_items_are_visibility_private() {
    let oracle = PythonOracle {
        modules: vec![simple_module("my_module", vec![simple_function("my_module._helper", "_helper", false)])],
    };
    let mut sink: Lowering<PythonId> = Lowering::new(PackageId::path("pkg"), root_sym());
    emit_package(&oracle, &mut sink);
    let pkg = sink.finish().expect("finish must succeed");

    let helper = pkg.iter().find(|(_, e)| e.sym().name == "_helper").expect("_helper not found");
    assert!(matches!(helper.1.sym().visibility, Visibility::Private), "_helper must be Private");
}

/// A method that returns another class **declared later in the same package**
/// must lower to `Type::Nominal` (not `Type::Any`), and `finish` must succeed.
#[test]
fn method_returning_sibling_class_resolves_to_nominal() {
    use crate::oracle::{ClassData, ClassForm, TypeData};

    let holder_item = ItemData {
        id: PythonId::new("my_pkg.Holder"),
        parent: None,
        name: "Holder".to_owned(),
        is_private: false,
        documentation: None,
        deprecation: None,
        decorators: vec![],
        span: 0..0,
        body: ItemBody::Class(ClassData {
            super_types: vec![], generics: vec![], form: ClassForm::Plain, fields: vec![],
            methods: vec![ItemData {
                id: PythonId::new("my_pkg.Holder.get_payload"),
                parent: Some(PythonId::new("my_pkg.Holder")),
                name: "get_payload".to_owned(),
                is_private: false,
                documentation: None,
                deprecation: None,
                decorators: vec![],
                span: 0..0,
                body: ItemBody::Function(FunctionData {
                    overload_index: 0, span: 0..0, receiver: ReceiverKind::SharedRef, params: vec![],
                    return_ty: Some(TypeData::Nominal("my_pkg.Payload".to_owned())),
                    return_span: None,
                    generics: vec![], is_async: false, is_abstract: false, is_stub: false,
                }),
            }],
            nested: vec![],
        }),
    };

    let payload_item = ItemData {
        id: PythonId::new("my_pkg.Payload"),
        parent: None,
        name: "Payload".to_owned(),
        is_private: false,
        documentation: None,
        deprecation: None,
        decorators: vec![],
        span: 0..0,
        body: ItemBody::Class(ClassData {
            super_types: vec![], generics: vec![], form: ClassForm::Plain, fields: vec![], methods: vec![], nested: vec![],
        }),
    };

    let oracle = PythonOracle { modules: vec![simple_module("my_pkg", vec![holder_item, payload_item])] };
    let mut sink: Lowering<PythonId> = Lowering::new(PackageId::path("pkg"), root_sym());
    emit_package(&oracle, &mut sink);
    let pkg = sink.finish().expect("forward-declared same-package nominal must not leave any Undeclared entries");

    assert!(pkg.iter().any(|(_, e)| e.sym().name == "Payload"), "Payload must be in the package");
    assert!(pkg.iter().any(|(_, e)| e.sym().name == "Holder"), "Holder must be in the package");
    assert!(pkg.iter().any(|(_, e)| e.sym().name == "get_payload"), "get_payload method must be in the package");
}

/// A same-package class used as a generic argument produces `Type::Apply`
/// whose base is `Type::Nominal`, not `Type::Any`.
#[test]
fn same_package_class_in_generic_apply_is_nominal() {
    use crate::oracle::{AliasData, ClassData, ClassForm, TypeData};

    let container_item = ItemData {
        id: PythonId::new("my_pkg.Container"),
        parent: None,
        name: "Container".to_owned(),
        is_private: false,
        documentation: None,
        deprecation: None,
        decorators: vec![],
        span: 0..0,
        body: ItemBody::Class(ClassData {
            super_types: vec![], generics: vec![], form: ClassForm::Plain, fields: vec![], methods: vec![], nested: vec![],
        }),
    };

    let item_class = ItemData {
        id: PythonId::new("my_pkg.Item"),
        parent: None,
        name: "Item".to_owned(),
        is_private: false,
        documentation: None,
        deprecation: None,
        decorators: vec![],
        span: 0..0,
        body: ItemBody::Class(ClassData {
            super_types: vec![], generics: vec![], form: ClassForm::Plain, fields: vec![], methods: vec![], nested: vec![],
        }),
    };

    let alias_item = ItemData {
        id: PythonId::new("my_pkg.MyAlias"),
        parent: None,
        name: "MyAlias".to_owned(),
        is_private: false,
        documentation: None,
        deprecation: None,
        decorators: vec![],
        span: 0..0,
        body: ItemBody::Alias(AliasData {
            target: Some(TypeData::Apply {
                base: Box::new(TypeData::Nominal("my_pkg.Container".to_owned())),
                args: vec![TypeData::Nominal("my_pkg.Item".to_owned())],
            }),
            generics: vec![],
        }),
    };

    let oracle = PythonOracle { modules: vec![simple_module("my_pkg", vec![container_item, item_class, alias_item])] };
    let mut sink: Lowering<PythonId> = Lowering::new(PackageId::path("pkg"), root_sym());
    emit_package(&oracle, &mut sink);
    let pkg = sink.finish().expect("finish must succeed");

    let alias_entry = pkg.iter().find(|(_, e)| e.sym().name == "MyAlias").expect("MyAlias not found");
    let alias_body = alias_entry.1.downcast::<Alias>().expect("MyAlias must be an Alias kind");

    match alias_body.body().target.as_ref() {
        Some(Type::Apply { base, args }) => {
            assert!(matches!(base.as_ref(), Type::Nominal(_)), "Apply base must be Nominal for same-package Container, got {base:?}");
            assert_eq!(args.len(), 1, "must have one type arg");
            assert!(matches!(args[0], Type::Nominal(_)), "Apply arg must be Nominal for same-package Item, got {:?}", args[0]);
        }
        other => panic!("expected Apply target, got {other:?}"),
    }
}

/// A keyword-only parameter (`def f(a, *, b)`) must carry
/// `ParamAttribute::KeywordOnly` — this attribute exists in `nudox-ir`
/// precisely for Python's bare-`*` calling convention (its own doc comment
/// cites `def f(a, *, b)`); the emit layer previously left it unset under a
/// stale comment claiming no such attribute existed.
#[test]
fn keyword_only_param_carries_keyword_only_attribute() {
    use crate::oracle::{ParamData, TypeData};
    use nudox_ir::kinds::ParamAttribute;

    let func_item = ItemData {
        id: PythonId::new("my_module.configure"),
        parent: None,
        name: "configure".to_owned(),
        is_private: false,
        documentation: None,
        deprecation: None,
        decorators: vec![],
        span: 0..0,
        body: ItemBody::Function(FunctionData {
            overload_index: 0,
            span: 0..0,
            receiver: ReceiverKind::None,
            params: vec![ParamData {
                name: "timeout".to_owned(),
                ty: Some(TypeData::Nominal("int".to_owned())),
                kind: ParamKind::KeywordOnly,
                has_default: false,
                doc_description: None,
                span: 0..0,
            }],
            return_ty: None,
            return_span: None,
            generics: vec![],
            is_async: false,
            is_abstract: false,
            is_stub: false,
        }),
    };

    let oracle = PythonOracle { modules: vec![simple_module("my_module", vec![func_item])] };
    let mut sink: Lowering<PythonId> = Lowering::new(PackageId::path("pkg"), root_sym());
    emit_package(&oracle, &mut sink);
    let pkg = sink.finish().expect("finish must succeed");

    let timeout = pkg.iter().find(|(_, e)| e.sym().name == "timeout").expect("timeout param not found");
    let param_body = timeout.1.downcast::<Param>().expect("timeout must be a Param");
    assert!(
        param_body.body().attributes.contains(&ParamAttribute::KeywordOnly),
        "keyword-only param must carry ParamAttribute::KeywordOnly, got {:?}",
        param_body.body().attributes
    );
}

/// A positional-only parameter (before Python's `/` marker) must NOT carry
/// `ParamAttribute::Inout` — that attribute means pass-by-mutable-reference
/// (Swift `inout` / C++ `&`), a concept Python does not have. The emit layer
/// previously reused `Inout` as "closest available", which actively
/// misreports the parameter's semantics rather than merely omitting them.
/// `nudox-ir` has no positional-only-specific `ParamAttribute` yet, so the
/// correct behaviour today is to emit no calling-convention attribute at all.
#[test]
fn positional_only_param_does_not_carry_inout() {
    use crate::oracle::{ParamData, TypeData};
    use nudox_ir::kinds::ParamAttribute;

    let func_item = ItemData {
        id: PythonId::new("my_module.build"),
        parent: None,
        name: "build".to_owned(),
        is_private: false,
        documentation: None,
        deprecation: None,
        decorators: vec![],
        span: 0..0,
        body: ItemBody::Function(FunctionData {
            overload_index: 0,
            span: 0..0,
            receiver: ReceiverKind::None,
            params: vec![ParamData {
                name: "raw".to_owned(),
                ty: Some(TypeData::Nominal("str".to_owned())),
                kind: ParamKind::PositionalOnly,
                has_default: false,
                doc_description: None,
                span: 0..0,
            }],
            return_ty: None,
            return_span: None,
            generics: vec![],
            is_async: false,
            is_abstract: false,
            is_stub: false,
        }),
    };

    let oracle = PythonOracle { modules: vec![simple_module("my_module", vec![func_item])] };
    let mut sink: Lowering<PythonId> = Lowering::new(PackageId::path("pkg"), root_sym());
    emit_package(&oracle, &mut sink);
    let pkg = sink.finish().expect("finish must succeed");

    let raw = pkg.iter().find(|(_, e)| e.sym().name == "raw").expect("raw param not found");
    let param_body = raw.1.downcast::<Param>().expect("raw must be a Param");
    assert!(
        !param_body.body().attributes.contains(&ParamAttribute::Inout),
        "positional-only param must NOT be mislabeled Inout (pass-by-reference is not a Python \
         concept), got {:?}",
        param_body.body().attributes
    );
}
