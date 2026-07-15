//! Max-fidelity assertions for the Python IR producer.
//!
//! These tests pin the *contract* the producer must satisfy after the
//! fidelity pass — not Debug snapshots. They cover the critical gaps that
//! historically golden-wronged generics as `Any` and left methods / defaults
//! / literals unrecoverable.

use compiler::graph::symtab::SymbolTable;
use compiler::languages::python::context::PythonContext;
use ir::entry::{Index, NudoxPath};
use ir::function::Function;
use ir::kind::{Entry, TypedBinding};
use ir::parameter::{Parameter, ParameterAttribute};
use ir::record::{Field, FieldKey};
use ir::ty::{LiteralKind, Type};

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn lower(module: &str, src: &str) -> Index {
    let ctx = PythonContext::new();
    let handle = ctx.check_snippet(module, src);
    ctx.lower_handle(&handle)
}

fn dump(index: &Index) -> String {
    let mut v: Vec<String> = index
        .entries_by_path
        .iter()
        .map(|(p, e)| format!("{p:?}:{}:{}", e.kind_tag(), e.name()))
        .collect();
    v.sort();
    v.join(", ")
}

fn find_record<'a>(index: &'a Index, name: &str) -> &'a ir::record::Record {
    index
        .entries_by_path
        .values()
        .find_map(|e| match e {
            Entry::RecordType(sym) if sym.name == name => Some(&sym.inner),
            _ => None,
        })
        .unwrap_or_else(|| panic!("RecordType `{name}` not found; entries: {}", dump(index)))
}

fn find_function<'a>(index: &'a Index, name: &str) -> &'a Function {
    index
        .entries_by_path
        .values()
        .find_map(|e| match e {
            Entry::Function(sym) if sym.name == name => Some(&sym.inner),
            _ => None,
        })
        .unwrap_or_else(|| panic!("Function `{name}` not found; entries: {}", dump(index)))
}

fn find_function_at<'a>(index: &'a Index, path_suffix: &str) -> &'a Function {
    index
        .entries_by_path
        .iter()
        .find_map(|(p, e)| {
            let key = match p {
                NudoxPath::Local(pb) => pb.display().to_string(),
                NudoxPath::External { path, dependency } => {
                    format!("{}::{}", dependency, path.display())
                }
            };
            match e {
                Entry::Function(sym) if key.ends_with(path_suffix) || key == path_suffix => {
                    Some(&sym.inner)
                }
                _ => None,
            }
        })
        .unwrap_or_else(|| {
            panic!("Function path ending `{path_suffix}` not found; entries: {}", dump(index))
        })
}

fn field_ty<'a>(rec: &'a ir::record::Record, name: &str) -> &'a Type {
    rec.fields
        .iter()
        .find_map(|f| match f {
            Field::Known(k) if matches!(&k.key, FieldKey::Ident(i) if i == name) => {
                k.r#type.as_deref()
            }
            _ => None,
        })
        .unwrap_or_else(|| panic!("field `{name}` missing or untyped on record"))
}

fn lit_param(p: &Parameter) -> &ir::parameter::LiteralParameter {
    match p {
        Parameter::Literal(lp) => lp,
        other => panic!("expected Literal parameter, got {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// 1. Box[T] field is GenericParam T, not Any; Box.generics non-empty
// ---------------------------------------------------------------------------

const GENERICS: &str = "\
from typing import TypeVar, Generic

T = TypeVar('T')

class Box(Generic[T]):
    value: T

def identity(x: T) -> T:
    return x
";

#[test]
fn box_field_is_generic_param_and_generics_populated() {
    let index = lower("fid_generics", GENERICS);
    let box_rec = find_record(&index, "Box");

    // Field `value: T` must be GenericParam { name: "T" }, never Any.
    let value_ty = field_ty(box_rec, "value");
    match value_ty {
        Type::GenericParam(g) => {
            assert_eq!(g.name, "T", "Box.value should be GenericParam T, got {g:?}");
        }
        other => panic!("Box.value should be GenericParam T, got {other:?}"),
    }

    // Declaration-site generics on the Record must be non-empty.
    let generics = box_rec
        .generics
        .as_ref()
        .expect("Box.generics should be Some(...)");
    assert!(
        !generics.params.is_empty(),
        "Box.generics.params should be non-empty"
    );
    let names: Vec<String> = generics
        .params
        .iter()
        .filter_map(|p| match p {
            Parameter::Type(tp) => tp.name.clone(),
            _ => None,
        })
        .collect();
    assert!(
        names.iter().any(|n| n == "T"),
        "Box.generics should introduce T, got {names:?}"
    );
}

// ---------------------------------------------------------------------------
// 2. identity(x: T) -> T generics + params GenericParam
// ---------------------------------------------------------------------------

#[test]
fn identity_generics_and_param_types() {
    let index = lower("fid_generics", GENERICS);
    let identity = find_function(&index, "identity");

    // Declaration-site generics on the Function.
    let generics = identity
        .generics
        .as_ref()
        .expect("identity.generics should be Some(...)");
    assert!(
        !generics.params.is_empty(),
        "identity.generics.params should be non-empty"
    );

    let inputs = identity
        .input_parameters
        .as_ref()
        .expect("identity should have input parameters");
    assert_eq!(inputs.len(), 1);
    let x = lit_param(&inputs[0]);
    assert_eq!(x.name, "x");
    match x.r#type.as_ref() {
        Some(Type::GenericParam(g)) => assert_eq!(g.name, "T"),
        other => panic!("identity.x should be GenericParam T, got {other:?}"),
    }

    let ret = identity
        .output_parameters
        .as_ref()
        .and_then(|o| o.first())
        .map(lit_param)
        .expect("identity should have a return parameter");
    match ret.r#type.as_ref() {
        Some(Type::GenericParam(g)) => assert_eq!(g.name, "T"),
        other => panic!("identity return should be GenericParam T, got {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// 3. Class method has Entry::Function path
// ---------------------------------------------------------------------------

const METHODS: &str = "\
class Counter:
    count: int

    def increment(self) -> None:
        self.count += 1

    def value(self) -> int:
        return self.count
";

#[test]
fn class_method_has_function_index_entry() {
    let index = lower("fid_methods", METHODS);
    let counter = find_record(&index, "Counter");

    // Members list on the Record points at method entries.
    let members = counter
        .members
        .as_ref()
        .expect("Counter.members should list method paths");
    assert!(
        !members.is_empty(),
        "Counter.members should be non-empty"
    );

    // Standalone Function entry at `fid_methods::Counter.increment`.
    let _inc = find_function_at(&index, "Counter.increment");
    let _val = find_function_at(&index, "Counter.value");

    // And the shape list still has methods.
    let methods = counter
        .methods
        .as_ref()
        .expect("Counter.methods shape list should be populated");
    assert!(
        methods.len() >= 2,
        "expected at least increment + value on methods list, got {}",
        methods.len()
    );
}

// ---------------------------------------------------------------------------
// 4. Default values present on params
// ---------------------------------------------------------------------------

const DEFAULTS: &str = "\
def greet(name: str, count: int = 1) -> str:
    return name * count

def maybe(value: int, fallback: str = 'none') -> str:
    return str(value) if value else fallback
";

#[test]
fn parameter_defaults_are_populated() {
    let index = lower("fid_defaults", DEFAULTS);
    let greet = find_function(&index, "greet");
    let inputs = greet
        .input_parameters
        .as_ref()
        .expect("greet should have params");
    let count = inputs
        .iter()
        .map(lit_param)
        .find(|p| p.name == "count")
        .expect("greet.count param");
    assert!(
        count.default_value.is_some(),
        "greet.count should have a default_value, got None"
    );
    // Optional attribute should also be set for defaulted params.
    let attrs = count.attributes.as_ref().expect("count should have attributes");
    assert!(
        attrs.contains(&ParameterAttribute::Optional),
        "defaulted param should carry Optional, got {attrs:?}"
    );

    let maybe = find_function(&index, "maybe");
    let fallback = maybe
        .input_parameters
        .as_ref()
        .expect("maybe params")
        .iter()
        .map(lit_param)
        .find(|p| p.name == "fallback")
        .expect("maybe.fallback");
    assert!(
        fallback.default_value.is_some(),
        "maybe.fallback should have a default_value"
    );
}

// ---------------------------------------------------------------------------
// 5. Literal["r"] preserves value
// ---------------------------------------------------------------------------

const LITERALS: &str = "\
from typing import Literal

def open_mode(mode: Literal['r', 'w']) -> str:
    return mode

MODE: Literal['r'] = 'r'
";

#[test]
fn literal_preserves_value() {
    let index = lower("fid_literals", LITERALS);

    // Function param type should carry Literal values, not bare String.
    let open_mode = find_function(&index, "open_mode");
    let mode = lit_param(
        &open_mode
            .input_parameters
            .as_ref()
            .expect("open_mode params")[0],
    );
    let ty = mode.r#type.as_ref().expect("mode type");
    // Union of two literals, or a single literal — both preserve values.
    fn contains_literal_r(ty: &Type) -> bool {
        match ty {
            Type::Literal(lit) => {
                lit.kind == LiteralKind::String && (lit.value == "r" || lit.value.contains('r'))
            }
            Type::Union(members) => members.iter().any(contains_literal_r),
            _ => false,
        }
    }
    assert!(
        contains_literal_r(ty),
        "open_mode.mode should preserve Literal['r'], got {ty:?}"
    );

    // Module constant MODE: Literal['r']
    let mode_const = index
        .entries_by_path
        .values()
        .find_map(|e| match e {
            Entry::Constant(sym) if sym.name == "MODE" => Some(sym),
            _ => None,
        })
        .expect("MODE constant");
    let tb: &TypedBinding = &mode_const.inner;
    let ty = tb.ty.as_ref().expect("MODE.ty should be set");
    assert!(
        contains_literal_r(ty),
        "MODE should be Literal['r'], got {ty:?}"
    );
}

// ---------------------------------------------------------------------------
// 6. Module constant TypedBinding.ty set
// ---------------------------------------------------------------------------

const CONSTANTS: &str = "\
MAX_SIZE: int = 1024
DEFAULT_NAME: str = 'world'
ENABLED: bool = True
";

#[test]
fn module_constant_typed_binding_ty_set() {
    let index = lower("fid_constants", CONSTANTS);
    for name in ["MAX_SIZE", "DEFAULT_NAME", "ENABLED"] {
        let sym = index
            .entries_by_path
            .values()
            .find_map(|e| match e {
                Entry::Constant(s) if s.name == name => Some(s),
                _ => None,
            })
            .unwrap_or_else(|| panic!("Constant `{name}` not found; entries: {}", dump(&index)));
        assert!(
            sym.inner.ty.is_some(),
            "Constant `{name}` TypedBinding.ty should be Some(...)"
        );
    }
}

// ---------------------------------------------------------------------------
// 7. SymbolTable resolves Class.method
// ---------------------------------------------------------------------------

#[test]
fn symbol_table_resolves_class_method() {
    let index = lower("fid_methods", METHODS);
    let table = SymbolTable::build(&index);

    // path_segments expands `fid_methods::Counter.increment` →
    // ["fid_methods", "Counter", "increment"], so exact fq is joined with `::`.
    let resolved = table
        .resolve_exact("fid_methods::Counter::increment")
        .or_else(|| table.resolve_suffix("increment"));
    assert!(
        resolved.is_some(),
        "SymbolTable should resolve Counter.increment; \
         exact=fid_methods::Counter::increment or suffix=increment. entries: {}",
        dump(&index)
    );
}

// ---------------------------------------------------------------------------
// Bonus: pos-only / kw-only / *args / **kwargs attributes
// ---------------------------------------------------------------------------

const VARARGS: &str = "\
def mixed(pos_only: int, /, normal: str, *args: int, kw_only: bool = False, **kwargs: str) -> None:
    pass
";

#[test]
fn param_kind_attributes_distinguish_varargs_and_kwargs() {
    let index = lower("fid_varargs", VARARGS);
    let mixed = find_function(&index, "mixed");
    let inputs = mixed
        .input_parameters
        .as_ref()
        .expect("mixed params");

    let by_name: std::collections::HashMap<&str, &ir::parameter::LiteralParameter> = inputs
        .iter()
        .map(lit_param)
        .map(|p| (p.name.as_str(), p))
        .collect();

    let pos_only = by_name.get("pos_only").expect("pos_only");
    assert!(
        pos_only
            .attributes
            .as_ref()
            .is_some_and(|a| a.contains(&ParameterAttribute::PositionalOnly)),
        "pos_only should be PositionalOnly, got {:?}",
        pos_only.attributes
    );

    let args = by_name.get("args").expect("*args");
    assert!(
        args.attributes
            .as_ref()
            .is_some_and(|a| a.contains(&ParameterAttribute::Variadic)),
        "*args should be Variadic, got {:?}",
        args.attributes
    );

    let kw_only = by_name.get("kw_only").expect("kw_only");
    assert!(
        kw_only
            .attributes
            .as_ref()
            .is_some_and(|a| a.contains(&ParameterAttribute::KeywordOnly)),
        "kw_only should be KeywordOnly, got {:?}",
        kw_only.attributes
    );

    let kwargs = by_name.get("kwargs").expect("**kwargs");
    assert!(
        kwargs
            .attributes
            .as_ref()
            .is_some_and(|a| a.contains(&ParameterAttribute::KwVariadic)),
        "**kwargs should be KwVariadic (not plain Variadic), got {:?}",
        kwargs.attributes
    );
}

// ---------------------------------------------------------------------------
// Bonus: single-snippet wire_members populates Module.members
// ---------------------------------------------------------------------------

#[test]
fn lower_handle_wires_module_members() {
    let index = lower("fid_wire", "def hello() -> str:\n    return 'hi'\n");
    let module = index
        .entries_by_path
        .values()
        .find_map(|e| match e {
            Entry::Module(s) => Some(s),
            _ => None,
        })
        .expect("module entry");
    let members = module
        .inner
        .members
        .as_ref()
        .expect("Module.members should be wired on the single-snippet path");
    assert!(
        members.iter().any(|p| match p {
            NudoxPath::Local(pb) => pb.display().to_string().contains("hello"),
            _ => false,
        }),
        "Module.members should include hello, got {members:?}"
    );
}

// ---------------------------------------------------------------------------
// Nested classes emit entries under Outer.members
// ---------------------------------------------------------------------------

const NESTED: &str = "\
class Outer:
    class Inner:
        x: int

    def method(self) -> int:
        return 0
";

#[test]
fn nested_class_is_index_entry_under_members() {
    let index = lower("fid_nested", NESTED);
    let outer = find_record(&index, "Outer");
    let members = outer
        .members
        .as_ref()
        .expect("Outer.members should list nested class + methods");

    // Nested class path: fid_nested::Outer.Inner
    let has_inner_member = members.iter().any(|p| match p {
        NudoxPath::Local(pb) => pb.display().to_string().contains("Outer.Inner"),
        _ => false,
    });
    assert!(
        has_inner_member,
        "Outer.members should include Outer.Inner, got {members:?}"
    );

    let inner = index
        .entries_by_path
        .values()
        .find_map(|e| match e {
            Entry::RecordType(sym) if sym.name == "Inner" => Some(sym),
            _ => None,
        })
        .expect("Inner RecordType entry missing");
    let path = match &inner.path {
        NudoxPath::Local(pb) => pb.display().to_string(),
        NudoxPath::External { path, dependency } => {
            format!("{}::{}", dependency, path.display())
        }
    };
    assert!(
        path.contains("Outer.Inner"),
        "Inner path should be under Outer, got {path}"
    );
}

// ---------------------------------------------------------------------------
// Docstring Returns: → output_parameters description
// ---------------------------------------------------------------------------

const RETURNS_DOC: &str = "\
def add(a: int, b: int) -> int:
    \"\"\"Add two numbers.

    Args:
        a: first.
        b: second.

    Returns:
        the sum of a and b.
    \"\"\"
    return a + b
";

#[test]
fn returns_docstring_on_output_parameter() {
    let index = lower("fid_returns", RETURNS_DOC);
    let add = find_function(&index, "add");
    let ret = add
        .output_parameters
        .as_ref()
        .and_then(|o| o.first())
        .map(lit_param)
        .expect("add return param");
    assert!(
        ret.description
            .as_deref()
            .is_some_and(|d| d.contains("sum of a and b")),
        "return description should carry Returns: text, got {:?}",
        ret.description
    );
}

// ---------------------------------------------------------------------------
// Protocol generics populated
// ---------------------------------------------------------------------------

const PROTOCOL_GENERIC: &str = "\
from typing import Protocol, TypeVar

T = TypeVar('T')

class Boxable(Protocol[T]):
    def get(self) -> T: ...
";

#[test]
fn protocol_generics_populated() {
    let index = lower("fid_proto", PROTOCOL_GENERIC);
    let trait_def = index
        .entries_by_path
        .values()
        .find_map(|e| match e {
            Entry::TraitDef(sym) if sym.name == "Boxable" => Some(&sym.inner),
            _ => None,
        })
        .unwrap_or_else(|| panic!("TraitDef Boxable missing; entries: {}", dump(&index)));

    let generics = trait_def
        .generics
        .as_ref()
        .expect("Boxable.generics should be Some(...)");
    assert!(
        !generics.params.is_empty(),
        "Boxable.generics.params should be non-empty"
    );
    let names: Vec<String> = generics
        .params
        .iter()
        .filter_map(|p| match p {
            Parameter::Type(tp) => tp.name.clone(),
            _ => None,
        })
        .collect();
    assert!(
        names.iter().any(|n| n == "T"),
        "Boxable.generics should introduce T, got {names:?}"
    );

    // Method should also be an index Function entry.
    let _get = find_function_at(&index, "Boxable.get");
}
