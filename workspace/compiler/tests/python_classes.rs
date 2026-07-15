//! Pipeline part: **Python classes → surface IR** via the Pyrefly oracle
//! (`compiler::languages::python::item::lower_class`).
//!
//! Companion to `python_oracle.rs` (which proves function lowering). This test
//! proves the *class* lowering paths land on the right `Entry` variant and carry
//! RESOLVED field/method types rather than `None`/`Any`:
//!
//!   * a `@dataclass` subclass → `RecordType` with resolved field types, a
//!     method, and a populated `super_types`;
//!   * an `enum.Enum` → `SumType` with the expected variant names;
//!   * a `typing.Protocol` → `TraitDef` with its method.

use compiler::languages::python::context::PythonContext;
use ir::entry::Index;
use ir::kind::Entry;
use ir::record::{Field, FieldKey};
use ir::ty::Type;

const SNIPPET: &str = "\
from dataclasses import dataclass
from enum import Enum
from typing import Protocol


class Animal:
    legs: int

    def describe(self) -> str:
        return \"animal\"


@dataclass
class Dog(Animal):
    name: str
    age: int

    def speak(self) -> str:
        return \"woof\"


class Color(Enum):
    RED = 1
    GREEN = 2
    BLUE = 3


class Greeter(Protocol):
    def greet(self, name: str) -> str: ...
";

/// A type counts as "really resolved" when Pyrefly handed us a concrete class
/// reference (e.g. `builtins.str`) or a primitive — not an `Any`/`Infer` leak.
fn is_resolved(ty: &Type) -> bool {
    match ty {
        Type::Primitive(_) => true,
        Type::TypeReference(r) => !r.identifier.is_empty(),
        _ => false,
    }
}

fn record<'a>(index: &'a Index, name: &str) -> &'a ir::record::Record {
    index
        .entries_by_path
        .values()
        .find_map(|e| match e {
            Entry::RecordType(sym) if sym.name == name => Some(&sym.inner),
            _ => None,
        })
        .unwrap_or_else(|| panic!("RecordType `{name}` not found; entries: {}", dump(index)))
}

fn dump(index: &Index) -> String {
    let v: Vec<String> = index
        .entries_by_path
        .values()
        .map(|e| format!("{}:{}", e.kind_tag(), e.name()))
        .collect();
    v.join(", ")
}

fn field_named<'a>(rec: &'a ir::record::Record, name: &str) -> &'a ir::record::KnownField {
    rec.fields
        .iter()
        .find_map(|f| match f {
            Field::Known(k) if matches!(&k.key, FieldKey::Ident(i) if i == name) => Some(k),
            _ => None,
        })
        .unwrap_or_else(|| panic!("field `{name}` not found on record"))
}

#[test]
fn dataclass_lowers_to_record_with_resolved_fields_methods_and_supertypes() {
    let ctx = PythonContext::new();
    let handle = ctx.check_snippet("classes_mod", SNIPPET);
    let index = ctx.lower_handle(&handle);
    println!("entries: {}", dump(&index));

    let dog = record(&index, "Dog");

    // --- fields carry RESOLVED types (not None / Any) ---
    for fname in ["name", "age"] {
        let f = field_named(dog, fname);
        let ty = f
            .r#type
            .as_deref()
            .unwrap_or_else(|| panic!("field `{fname}` lowered with no type"));
        assert!(
            is_resolved(ty),
            "field `{fname}` is not a resolved type, got: {ty:?}"
        );
        println!("Dog.{fname} : {ty:?}");
    }

    // --- a method came through (shape list + standalone Function index entry) ---
    let methods = dog
        .methods
        .as_ref()
        .expect("Dog lowered with no methods (expected `speak`)");
    assert!(
        methods.iter().any(|_| true),
        "Dog should have at least one method"
    );
    println!("Dog methods: {}", methods.len());

    // C2: methods must also be Index Function entries under Class.method so
    // SymbolTable / resolution can find them by path.
    let speak_entry = index.entries_by_path.iter().find(|(p, e)| {
        matches!(e, Entry::Function(s) if s.name == "speak")
            && match p {
                ir::entry::NudoxPath::Local(pb) => {
                    pb.display().to_string().contains("Dog.speak")
                }
                _ => false,
            }
    });
    assert!(
        speak_entry.is_some(),
        "Dog.speak should be a standalone Function entry; entries: {}",
        dump(&index)
    );
    let members = dog
        .members
        .as_ref()
        .expect("Dog.members should list method paths");
    assert!(
        !members.is_empty(),
        "Dog.members should be non-empty after method index emission"
    );

    // --- super_types populated for the subclass (references `Animal`) ---
    let supers = dog
        .super_types
        .as_ref()
        .expect("Dog (subclass of Animal) lowered with no super_types");
    assert!(
        supers.iter().any(|t| matches!(t, Type::TypeReference(r) if r.identifier.contains("Animal"))),
        "Dog.super_types should reference Animal, got: {supers:?}"
    );
    println!("Dog super_types: {supers:?}");
}

#[test]
fn enum_lowers_to_sumtype_with_variants() {
    let ctx = PythonContext::new();
    let handle = ctx.check_snippet("classes_mod", SNIPPET);
    let index = ctx.lower_handle(&handle);

    let variants = index
        .entries_by_path
        .values()
        .find_map(|e| match e {
            Entry::SumType(sym) if sym.name == "Color" => Some(&sym.inner.variants),
            _ => None,
        })
        .unwrap_or_else(|| panic!("SumType `Color` not found; entries: {}", dump(&index)));

    let mut names: Vec<&str> = variants.iter().map(|v| v.name.as_str()).collect();
    names.sort();
    assert_eq!(
        names,
        ["BLUE", "GREEN", "RED"],
        "unexpected enum variants for Color"
    );
    println!("Color variants: {names:?}");
}

#[test]
fn protocol_lowers_to_traitdef_with_methods() {
    let ctx = PythonContext::new();
    let handle = ctx.check_snippet("classes_mod", SNIPPET);
    let index = ctx.lower_handle(&handle);

    let trait_def = index
        .entries_by_path
        .values()
        .find_map(|e| match e {
            Entry::TraitDef(sym) if sym.name == "Greeter" => Some(&sym.inner),
            _ => None,
        })
        .unwrap_or_else(|| panic!("TraitDef `Greeter` not found; entries: {}", dump(&index)));

    let methods = trait_def
        .required_methods
        .as_ref()
        .expect("Greeter protocol lowered with no required_methods");
    assert!(
        methods.iter().any(|m| m.name == "greet"),
        "Greeter should require a `greet` method, got: {:?}",
        methods.iter().map(|m| &m.name).collect::<Vec<_>>()
    );
    println!(
        "Greeter required_methods: {:?}",
        methods.iter().map(|m| &m.name).collect::<Vec<_>>()
    );
}
