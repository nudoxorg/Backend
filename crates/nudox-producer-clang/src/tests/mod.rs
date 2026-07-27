//! Hermetic tests for the C/C++ producer.
//!
//! All tests parse small literal C/C++ source strings via libclang's unsaved-
//! file API.  No external build system is required.
//!
//! Tests that need libclang at runtime are gated by `Clang::new()` — if
//! libclang is unavailable they are skipped with a descriptive message rather
//! than panicking.

use clang::{Clang, Index};

use crate::{
    extract::extract_unsaved,
    lower::lower_oracle,
    oracle::OracleType,
};
use nudox_ir::{
    kinds::Record,
    lower::Lowering,
};
use std::path::Path;

// ── Helpers ───────────────────────────────────────────────────────────────────

/// Try to acquire a libclang handle.  Returns `None` if libclang is absent.
fn try_clang() -> Option<Clang> {
    Clang::new().ok()
}

/// Parse a C source snippet with the given virtual filename.
fn parse_c<'a>(index: &'a Index<'a>, source: &str) -> crate::oracle::ClangOracle {
    extract_unsaved(
        index,
        Path::new("/tmp/test.c"),
        source,
        &["-std=c11"],
    )
}

/// Parse a C++ source snippet.
fn parse_cpp<'a>(index: &'a Index<'a>, source: &str) -> crate::oracle::ClangOracle {
    extract_unsaved(
        index,
        Path::new("/tmp/test.cpp"),
        source,
        &["-std=c++17"],
    )
}

// ── Struct with fields ────────────────────────────────────────────────────────

#[test]
fn struct_with_fields() {
    let clang = match try_clang() {
        Some(c) => c,
        None => {
            eprintln!("SKIP struct_with_fields: libclang unavailable");
            return;
        }
    };
    let index = Index::new(&clang, false, false);

    let src = r#"
struct Point {
    int x;
    int y;
};
"#;

    let oracle = parse_c(&index, src);

    let rec = oracle
        .records
        .iter()
        .find(|r| r.name == "Point")
        .expect("Point not found");

    assert_eq!(rec.name, "Point");
    assert!(!rec.is_class);
    assert!(!rec.is_union);

    // Fields are stored flat.
    let x = oracle
        .fields
        .iter()
        .find(|f| f.name == "x" && f.parent_usr == Some(rec.usr.clone()))
        .expect("field x not found");
    assert!(matches!(x.ty, OracleType::Integer { signed: true, bits: 32 }));

    let y = oracle
        .fields
        .iter()
        .find(|f| f.name == "y" && f.parent_usr == Some(rec.usr.clone()))
        .expect("field y not found");
    assert!(matches!(y.ty, OracleType::Integer { signed: true, bits: 32 }));
}

// ── Function with params ──────────────────────────────────────────────────────

#[test]
fn function_with_params() {
    let clang = match try_clang() {
        Some(c) => c,
        None => {
            eprintln!("SKIP function_with_params: libclang unavailable");
            return;
        }
    };
    let index = Index::new(&clang, false, false);

    let src = r#"
int add(int a, int b) { return a + b; }
"#;

    let oracle = parse_c(&index, src);

    let fun = oracle
        .functions
        .iter()
        .find(|f| f.name == "add")
        .expect("add not found");

    assert_eq!(fun.params.len(), 2);
    assert_eq!(fun.params[0].name, "a");
    assert_eq!(fun.params[1].name, "b");
    assert!(matches!(fun.ret, OracleType::Integer { signed: true, bits: 32 }));
}

// ── Overload pair — each is its OWN declaration ───────────────────────────────

#[test]
fn overload_pair_two_distinct_declarations() {
    let clang = match try_clang() {
        Some(c) => c,
        None => {
            eprintln!("SKIP overload_pair_two_distinct_declarations: libclang unavailable");
            return;
        }
    };
    let index = Index::new(&clang, false, false);

    let src = r#"
void print(int x) {}
void print(double x) {}
"#;

    let oracle = parse_cpp(&index, src);

    let overloads: Vec<_> = oracle
        .functions
        .iter()
        .filter(|f| f.name == "print")
        .collect();

    assert_eq!(
        overloads.len(),
        2,
        "expected 2 distinct overload declarations, got {}",
        overloads.len()
    );

    // Each must have a different USR.
    assert_ne!(
        overloads[0].usr,
        overloads[1].usr,
        "overloads must have distinct USRs"
    );

    // One takes int, the other double.
    let takes_int = overloads
        .iter()
        .any(|f| matches!(f.params.first().map(|p| &p.ty), Some(OracleType::Integer { .. })));
    let takes_float = overloads
        .iter()
        .any(|f| matches!(f.params.first().map(|p| &p.ty), Some(OracleType::Float { .. })));
    assert!(takes_int, "one overload must take int");
    assert!(takes_float, "one overload must take double");
}

// ── enum class with explicit discriminants ────────────────────────────────────

#[test]
fn enum_class_explicit_values() {
    let clang = match try_clang() {
        Some(c) => c,
        None => {
            eprintln!("SKIP enum_class_explicit_values: libclang unavailable");
            return;
        }
    };
    let index = Index::new(&clang, false, false);

    let src = r#"
enum class Color { Red = 1, Green = 2, Blue = 4 };
"#;

    let oracle = parse_cpp(&index, src);

    let e = oracle
        .enums
        .iter()
        .find(|e| e.name == "Color")
        .expect("Color enum not found");

    let variants: Vec<_> = oracle
        .variants
        .iter()
        .filter(|v| v.parent_usr == Some(e.usr.clone()))
        .collect();

    assert_eq!(variants.len(), 3);

    let red = variants.iter().find(|v| v.name == "Red").expect("Red");
    assert_eq!(red.discr, Some(1));

    let green = variants.iter().find(|v| v.name == "Green").expect("Green");
    assert_eq!(green.discr, Some(2));

    let blue = variants.iter().find(|v| v.name == "Blue").expect("Blue");
    assert_eq!(blue.discr, Some(4));
}

// ── typedef / using alias ─────────────────────────────────────────────────────

#[test]
fn typedef_and_using_alias() {
    let clang = match try_clang() {
        Some(c) => c,
        None => {
            eprintln!("SKIP typedef_and_using_alias: libclang unavailable");
            return;
        }
    };
    let index = Index::new(&clang, false, false);

    let src = r#"
typedef unsigned int uint32_t_alias;
using MyInt = int;
"#;

    let oracle = parse_cpp(&index, src);

    let td = oracle
        .aliases
        .iter()
        .find(|a| a.name == "uint32_t_alias")
        .expect("uint32_t_alias not found");
    assert!(matches!(td.target, OracleType::Integer { signed: false, .. }));

    let ua = oracle
        .aliases
        .iter()
        .find(|a| a.name == "MyInt")
        .expect("MyInt not found");
    assert!(matches!(ua.target, OracleType::Integer { signed: true, bits: 32 }));
}

// ── Namespace ─────────────────────────────────────────────────────────────────

#[test]
fn namespace_emitted_as_module() {
    let clang = match try_clang() {
        Some(c) => c,
        None => {
            eprintln!("SKIP namespace_emitted_as_module: libclang unavailable");
            return;
        }
    };
    let index = Index::new(&clang, false, false);

    let src = r#"
namespace math {
    int square(int x) { return x * x; }
}
"#;

    let oracle = parse_cpp(&index, src);

    let ns = oracle
        .namespaces
        .iter()
        .find(|n| n.name == "math")
        .expect("math namespace not found");

    // Functions inside the namespace must have math's USR as their parent.
    let sq = oracle
        .functions
        .iter()
        .find(|f| f.name == "square")
        .expect("square not found");

    assert_eq!(
        sq.parent_usr.as_deref(),
        Some(ns.usr.as_str()),
        "square must be parented to the math namespace"
    );
}

// ── Template function → TypeVar ───────────────────────────────────────────────

#[test]
fn template_function_type_var() {
    let clang = match try_clang() {
        Some(c) => c,
        None => {
            eprintln!("SKIP template_function_type_var: libclang unavailable");
            return;
        }
    };
    let index = Index::new(&clang, false, false);

    let src = r#"
template <typename T>
T identity(T x) { return x; }
"#;

    let oracle = parse_cpp(&index, src);

    let fun = oracle
        .functions
        .iter()
        .find(|f| f.name == "identity")
        .expect("identity not found");

    // Must have one generic param.
    assert_eq!(fun.generics.len(), 1, "expected 1 generic param");

    // The parameter type must be a TypeVar.
    let p = &fun.params[0];
    assert!(
        matches!(&p.ty, OracleType::TypeVar(_)),
        "param must be a TypeVar, got {:?}",
        p.ty
    );

    // Return type must also be a TypeVar.
    assert!(
        matches!(&fun.ret, OracleType::TypeVar(_)),
        "return must be a TypeVar, got {:?}",
        fun.ret
    );
}

// ── Lowering integration: struct → IR passes finish() ────────────────────────

#[test]
fn lowering_struct_passes_finish() {
    let clang = match try_clang() {
        Some(c) => c,
        None => {
            eprintln!("SKIP lowering_struct_passes_finish: libclang unavailable");
            return;
        }
    };
    let index = Index::new(&clang, false, false);

    let src = r#"
struct Vec2 {
    float x;
    float y;
};
"#;

    let oracle = parse_c(&index, src);

    let pkg_id = nudox_ir::package::PackageId::path("/tmp");
    let root_sym = nudox_ir::entry::Symbol {
        name: "test".to_owned(),
        visibility: nudox_ir::entry::Visibility::Public,
        documentation: String::new(),
        source: std::path::PathBuf::from("/tmp"),
        span: 0..0,
        aliases: Box::new([]),
        deprecation: None,
        doc_links: Box::new([]),
        attrs: Box::new([]),
        cfg: None,
    };

    let mut sink = Lowering::new(pkg_id, root_sym);
    lower_oracle(&oracle, &mut sink);
    let result = sink.finish();
    assert!(result.is_ok(), "finish() must succeed: {:?}", result.err());

    let pkg = result.unwrap();
    let names: Vec<_> = pkg.iter().map(|(_, e)| e.sym().name.clone()).collect();
    assert!(
        names.iter().any(|n| n == "Vec2"),
        "Vec2 must be in the IR: {:?}",
        names
    );
}

// ── Union round-trips as RecordForm::Union ───────────────────────────────────

#[test]
fn union_emitted_as_record_union_form() {
    let clang = match try_clang() {
        Some(c) => c,
        None => {
            eprintln!("SKIP union_emitted_as_record_union_form: libclang unavailable");
            return;
        }
    };
    let index = Index::new(&clang, false, false);

    let src = r#"
union Value {
    int i;
    float f;
};
"#;

    let oracle = parse_c(&index, src);

    let rec = oracle
        .records
        .iter()
        .find(|r| r.name == "Value")
        .expect("Value union not found");

    // Oracle correctly marks it as a union.
    assert!(rec.is_union, "oracle must mark the entity as a union");

    // ...and it survives lowering as a Union rather than collapsing to Struct.
    let pkg_id = nudox_ir::package::PackageId::path("/tmp");
    let root_sym = nudox_ir::entry::Symbol {
        name: "test".to_owned(),
        visibility: nudox_ir::entry::Visibility::Public,
        documentation: String::new(),
        source: std::path::PathBuf::from("/tmp"),
        span: 0..0,
        aliases: Box::new([]),
        deprecation: None,
        doc_links: Box::new([]),
        attrs: Box::new([]),
        cfg: None,
    };

    let mut sink = Lowering::new(pkg_id, root_sym);
    lower_oracle(&oracle, &mut sink);
    let pkg = sink.finish().expect("finish must succeed");

    let entry = pkg
        .iter()
        .find(|(_, e)| e.sym().name == "Value")
        .expect("Value must be in IR");

    let body = entry.1.downcast::<Record>().expect("must be a Record");
    assert_eq!(
        body.body().form,
        nudox_ir::kinds::RecordForm::Union,
        "a C union must lower to RecordForm::Union, not collapse to Struct"
    );
}
