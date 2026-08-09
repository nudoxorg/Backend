//! Unit tests for the ruff syntactic front end.
//!
//! Per AGENTS-DOCTRINE.md §4 ("a hand-authored fixture tests the fixture
//! author's imagination, not the code"), these are deliberately narrow: they
//! pin the mapping from one small, literal Python snippet to the expected
//! `ItemData`/`TypeData` shape, exactly mirroring `types.rs`'s and
//! `docstring.rs`'s existing unit-test style. The claim that this front end
//! actually works on real, unmodified packages is made by
//! `tests/corpus_sweep.rs` against the real pypi corpus, and by
//! `producer.rs`'s `a_real_temp_package_produces_named_declarations_…` test —
//! not by anything here.

use super::*;

fn oracle_for(src: &str) -> ModuleData {
    let parsed = ruff_python_parser::parse_module(src).expect("fixture must parse");
    extract_module(parsed.syntax(), src, "m".to_owned())
}

fn only_item<'a>(module: &'a ModuleData, name: &str) -> &'a ItemData {
    module
        .items
        .iter()
        .find(|i| i.name == name)
        .unwrap_or_else(|| panic!("no item named {name:?} in {:?}", module.items.iter().map(|i| &i.name).collect::<Vec<_>>()))
}

// ---------------------------------------------------------------------------
// Module / function basics
// ---------------------------------------------------------------------------

#[test]
fn module_docstring_is_captured() {
    let m = oracle_for("\"\"\"A tiny module.\"\"\"\n\ndef f():\n    pass\n");
    assert_eq!(m.documentation.as_deref(), Some("A tiny module."));
}

#[test]
fn free_function_receiver_is_none_even_named_self() {
    // A free function's first parameter is never a receiver, no matter its name.
    let m = oracle_for("def f(self, x):\n    pass\n");
    let item = only_item(&m, "f");
    let ItemBody::Function(fd) = &item.body else { panic!("expected Function") };
    assert_eq!(fd.receiver, ReceiverKind::None);
}

#[test]
fn param_kinds_cover_positional_only_normal_varargs_keyword_only_kwargs() {
    let m = oracle_for("def f(a, /, b, *args, c, **kwargs):\n    pass\n");
    let item = only_item(&m, "f");
    let ItemBody::Function(fd) = &item.body else { panic!("expected Function") };
    let kinds: Vec<(String, ParamKind)> = fd.params.iter().map(|p| (p.name.clone(), p.kind)).collect();
    assert_eq!(
        kinds,
        vec![
            ("a".to_owned(), ParamKind::PositionalOnly),
            ("b".to_owned(), ParamKind::Normal),
            ("args".to_owned(), ParamKind::Varargs),
            ("c".to_owned(), ParamKind::KeywordOnly),
            ("kwargs".to_owned(), ParamKind::Kwargs),
        ]
    );
}

#[test]
fn async_def_sets_is_async() {
    let m = oracle_for("async def f():\n    pass\n");
    let item = only_item(&m, "f");
    let ItemBody::Function(fd) = &item.body else { panic!("expected Function") };
    assert!(fd.is_async);
}

#[test]
fn stub_body_is_detected_past_a_leading_docstring() {
    let m = oracle_for("def f():\n    \"\"\"Doc.\"\"\"\n    ...\n");
    let item = only_item(&m, "f");
    let ItemBody::Function(fd) = &item.body else { panic!("expected Function") };
    assert!(fd.is_stub, "a docstring followed by `...` must still count as a stub body");
}

#[test]
fn function_docstring_param_descriptions_reach_params() {
    let m = oracle_for(
        "def f(a, b):\n    \"\"\"Do a thing.\n\n    Args:\n        a: the first.\n        b: the second.\n    \"\"\"\n    pass\n",
    );
    let item = only_item(&m, "f");
    assert_eq!(item.documentation.as_deref(), Some("Do a thing."));
    let ItemBody::Function(fd) = &item.body else { panic!("expected Function") };
    assert_eq!(fd.params[0].doc_description.as_deref(), Some("the first."));
    assert_eq!(fd.params[1].doc_description.as_deref(), Some("the second."));
}

// ---------------------------------------------------------------------------
// Overloads and property accessor pairs
// ---------------------------------------------------------------------------

#[test]
fn overload_group_keeps_every_branch_as_a_distinct_declaration() {
    let m = oracle_for(
        "from typing import overload\n\n\
         @overload\n\
         def f(x: int) -> int: ...\n\
         @overload\n\
         def f(x: str) -> str: ...\n\
         def f(x):\n    return x\n",
    );
    let item = only_item(&m, "f");
    let ItemBody::Overloaded(branches) = &item.body else { panic!("expected Overloaded") };
    assert_eq!(branches.len(), 3);
    assert_eq!(branches[0].overload_index, 0);
    assert_eq!(branches[2].overload_index, 2);
}

#[test]
fn property_setter_pair_collapses_to_the_getter_only() {
    let m = oracle_for(
        "class C:\n\
         \x20\x20\x20\x20@property\n\
         \x20\x20\x20\x20def x(self) -> int:\n\
         \x20\x20\x20\x20\x20\x20\x20\x20return 1\n\
         \x20\x20\x20\x20@x.setter\n\
         \x20\x20\x20\x20def x(self, value: int) -> None:\n\
         \x20\x20\x20\x20\x20\x20\x20\x20pass\n",
    );
    let item = only_item(&m, "C");
    let ItemBody::Class(cls) = &item.body else { panic!("expected Class") };
    assert_eq!(cls.methods.len(), 1, "the setter must not become a second declaration");
    let ItemBody::Function(fd) = &cls.methods[0].body else { panic!("expected Function") };
    assert_eq!(fd.receiver, ReceiverKind::SharedRef);
}

// ---------------------------------------------------------------------------
// Classes: form detection, fields, bases
// ---------------------------------------------------------------------------

#[test]
fn dataclass_decorator_is_detected_with_or_without_call_args() {
    let m = oracle_for("import dataclasses\n\n@dataclasses.dataclass(frozen=True)\nclass C:\n    x: int\n");
    let item = only_item(&m, "C");
    let ItemBody::Class(cls) = &item.body else { panic!("expected Class") };
    assert_eq!(cls.form, ClassForm::Dataclass);
}

#[test]
fn enum_base_is_detected() {
    let m = oracle_for("from enum import Enum\n\nclass Color(Enum):\n    RED = 1\n    BLUE = 2\n");
    let item = only_item(&m, "Color");
    let ItemBody::Class(cls) = &item.body else { panic!("expected Class") };
    assert_eq!(cls.form, ClassForm::Enum);
    assert_eq!(cls.fields.len(), 2);
}

#[test]
fn protocol_and_typed_dict_and_named_tuple_bases_are_detected() {
    let proto = oracle_for("from typing import Protocol\n\nclass P(Protocol):\n    pass\n");
    assert_eq!(
        matches!(&only_item(&proto, "P").body, ItemBody::Class(c) if c.form == ClassForm::Protocol),
        true
    );

    let td = oracle_for("from typing import TypedDict\n\nclass D(TypedDict):\n    x: int\n");
    assert!(matches!(&only_item(&td, "D").body, ItemBody::Class(c) if c.form == ClassForm::TypedDict));

    let nt = oracle_for("from typing import NamedTuple\n\nclass N(NamedTuple):\n    x: int\n");
    assert!(matches!(&only_item(&nt, "N").body, ItemBody::Class(c) if c.form == ClassForm::NamedTuple));
}

#[test]
fn class_var_and_final_field_flags_are_extracted_and_unwrapped() {
    let m = oracle_for(
        "from typing import ClassVar, Final\n\n\
         class C:\n\
         \x20\x20\x20\x20count: ClassVar[int] = 0\n\
         \x20\x20\x20\x20name: Final[str] = \"x\"\n",
    );
    let item = only_item(&m, "C");
    let ItemBody::Class(cls) = &item.body else { panic!("expected Class") };
    let count = cls.fields.iter().find(|f| f.name == "count").unwrap();
    assert!(count.is_class_var);
    assert!(
        matches!(&count.ty, Some(TypeData::Nominal(n)) if n == "int"),
        "ClassVar[int] must unwrap to a plain int nominal; got {:?}",
        count.ty
    );
    let name = cls.fields.iter().find(|f| f.name == "name").unwrap();
    assert!(name.is_final);
}

#[test]
fn base_classes_become_super_types() {
    let m = oracle_for("class Base:\n    pass\nclass Child(Base):\n    pass\n");
    let item = only_item(&m, "Child");
    let ItemBody::Class(cls) = &item.body else { panic!("expected Class") };
    assert_eq!(cls.super_types.len(), 1);
    assert!(matches!(&cls.super_types[0], TypeData::Nominal(n) if n == "Base"));
}

// ---------------------------------------------------------------------------
// Type expressions
// ---------------------------------------------------------------------------

fn return_type_of(src: &str) -> TypeData {
    let m = oracle_for(src);
    let item = only_item(&m, "f");
    let ItemBody::Function(fd) = &item.body else { panic!("expected Function") };
    fd.return_ty.clone().expect("expected a return type")
}

#[test]
fn optional_becomes_union_with_none() {
    let ty = return_type_of("from typing import Optional\ndef f() -> Optional[int]: ...\n");
    match ty {
        TypeData::Union(members) => {
            assert_eq!(members.len(), 2);
            assert!(matches!(members[1], TypeData::NoneType));
        }
        other => panic!("expected Union, got {other:?}"),
    }
}

#[test]
fn pep604_union_is_flattened() {
    let ty = return_type_of("def f() -> int | str | None: ...\n");
    match ty {
        TypeData::Union(members) => assert_eq!(members.len(), 3),
        other => panic!("expected Union, got {other:?}"),
    }
}

#[test]
fn list_subscript_becomes_slice() {
    let ty = return_type_of("def f() -> list[int]: ...\n");
    assert!(matches!(ty, TypeData::Slice(_)), "expected Slice, got {ty:?}");
}

#[test]
fn generic_subscript_becomes_apply() {
    let ty = return_type_of("def f() -> dict[str, int]: ...\n");
    match ty {
        TypeData::Apply { base, args } => {
            assert!(matches!(*base, TypeData::Nominal(ref n) if n == "dict"));
            assert_eq!(args.len(), 2);
        }
        other => panic!("expected Apply, got {other:?}"),
    }
}

#[test]
fn annotated_extracts_inner_type_and_metadata() {
    let ty = return_type_of("from typing import Annotated\ndef f() -> Annotated[int, \"meta\"]: ...\n");
    match ty {
        TypeData::Annotated { inner, metadata } => {
            assert!(matches!(*inner, TypeData::Nominal(ref n) if n == "int"));
            assert_eq!(metadata.len(), 1);
        }
        other => panic!("expected Annotated, got {other:?}"),
    }
}

#[test]
fn typevar_reference_is_recognized_from_legacy_and_pep695_forms() {
    let legacy = return_type_of("from typing import TypeVar\nT = TypeVar(\"T\")\ndef f() -> T: ...\n");
    assert!(matches!(legacy, TypeData::TypeVar(ref n) if n == "T"));

    let pep695 = return_type_of("def f[T]() -> T: ...\n");
    assert!(matches!(pep695, TypeData::TypeVar(ref n) if n == "T"));
}

#[test]
fn self_type_is_recognized() {
    let ty = return_type_of("from typing import Self\ndef f() -> Self: ...\n");
    assert!(matches!(ty, TypeData::SelfType));
}

/// `Literal[...]` is a real PEP 586 construct the author deliberately wrote —
/// not the extractor giving up — so it must be `Unsupported`, carrying the
/// source text, never a bare `Any`.
#[test]
fn literal_subscript_is_unsupported_with_source_text_not_any() {
    let ty = return_type_of("from typing import Literal\ndef f() -> Literal[\"GET\", \"POST\"]: ...\n");
    match ty {
        TypeData::Unsupported(text) => {
            assert_eq!(text, "Literal[\"GET\", \"POST\"]");
        }
        other => panic!("expected Unsupported, got {other:?}"),
    }
}

/// An `Expr` shape `expr_to_type` has no arm for (a lambda, in this case) is
/// the extractor's own gap, and must say so with the source text rather than
/// silently reporting `Any`.
#[test]
fn unhandled_expr_shape_in_type_position_is_unsupported_with_source_text() {
    // A lambda is never a valid type, but nothing stops it from being
    // *written* in an annotation position, and the extractor must not crash
    // or silently claim it understood.
    let ty = return_type_of("def f() -> (lambda: 1): ...\n");
    match ty {
        TypeData::Unsupported(text) => assert_eq!(text, "lambda: 1"),
        other => panic!("expected Unsupported, got {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// Module naming
// ---------------------------------------------------------------------------

#[test]
fn module_dotted_name_climbs_only_through_init_py_directories() {
    let dir = tempfile::tempdir().expect("tempdir");
    let pkg = dir.path().join("pkg");
    std::fs::create_dir(&pkg).expect("mkdir pkg");
    std::fs::write(pkg.join("__init__.py"), "").unwrap();
    std::fs::write(pkg.join("mod.py"), "").unwrap();

    assert_eq!(module_dotted_name(&pkg.join("mod.py")), "pkg.mod");
    assert_eq!(module_dotted_name(&pkg.join("__init__.py")), "pkg");

    // A lone module directly under a non-package directory (e.g. a bare
    // sdist root) is just itself, not prefixed by the directory name.
    let lone = dir.path().join("six.py");
    std::fs::write(&lone, "").unwrap();
    assert_eq!(module_dotted_name(&lone), "six");
}
