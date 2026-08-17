//! Hermetic tests for the C/C++ producer.
//!
//! All tests parse small literal C/C++ source strings via libclang's unsaved-
//! file API.  No external build system is required.
//!
//! Tests that need libclang at runtime go through [`require_clang`], which
//! **fails the test** if libclang cannot be loaded — see that function's doc
//! comment for why a silent skip is not an acceptable default here.

use clang::{Clang, Index};

use crate::clang::{extract::extract_unsaved, lower::lower_oracle, oracle::OracleType};
use nudox_ir::{
    kind::Kind,
    kinds::{Record, ty::Type},
    lower::Lowering,
};
use std::path::Path;

// ── Helpers ───────────────────────────────────────────────────────────────────

/// Acquire a libclang handle for a hermetic test, or fail loudly.
///
/// `nudox-languages` links `clang-sys`'s `runtime` feature specifically
/// so that a *missing* libclang is a graceful, typed `Err` at the point of
/// use (`ClangProducer::invoke` → `ProducerError::OracleSpawn`) rather than a
/// `dyld` abort at process load (docs/AGENTS-DOCTRINE.md §8) — every test in this
/// file exercises exactly that "libclang present, drive it" path. libclang
/// is expected to be resolvable on every host that runs this suite: pinned
/// via `flake.nix`'s `LIBCLANG_PATH` inside `nix develop`, or via Xcode
/// Command Line Tools' `libclang.dylib` outside it. A test suite that
/// quietly returns "skip" instead of failing when that assumption breaks is
/// a suite that can never fail — the "screenshot suite that cannot fail"
/// defect (docs/AGENTS-DOCTRINE.md §8), applied to libclang instead of pixels.
///
/// This therefore **panics** by default when libclang cannot be loaded,
/// naming the underlying error. The only way to get the old silent-skip
/// behaviour is to opt in explicitly by setting
/// `NUDOX_ALLOW_CLANG_TEST_SKIP=1`, which downgrades the panic to a loud
/// `eprintln!` skip — for the rare host that genuinely has no libclang and
/// cannot get one. That is an opt-out you must ask for, never a default.
///
/// # Why this also returns a `MutexGuard`
///
/// `clang::Clang` is documented as allowing only **one instance in the
/// whole process at a time** (`Clang::new` fails with `"an instance of
/// Clang already exists"` otherwise) — a process-wide restriction, not a
/// per-thread one. `cargo test`'s default harness runs every `#[test]` fn
/// concurrently on its own thread, so without serialization every test in
/// this file races the others for that single slot: exactly one wins
/// `Clang::new()` and the rest see `Err("an instance of Clang already
/// exists")`. The old `try_clang` conflated that race with "libclang is
/// absent" and silently skipped either way, so this file's tests never
/// actually ran together — only whichever one happened to win the race did.
/// Serializing acquisition through `CLANG_SINGLETON` (held for the caller's
/// whole test body via the returned guard) turns "lost the race" back into
/// "didn't happen", instead of a second, misdiagnosed reason to skip.
static CLANG_SINGLETON: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn require_clang(test_name: &str) -> Option<(std::sync::MutexGuard<'static, ()>, Clang)> {
    let guard = CLANG_SINGLETON
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    match Clang::new() {
        Ok(c) => Some((guard, c)),
        Err(e) => {
            if std::env::var_os("NUDOX_ALLOW_CLANG_TEST_SKIP").is_some() {
                eprintln!(
                    "SKIP {test_name}: libclang unavailable ({e}) — \
                     NUDOX_ALLOW_CLANG_TEST_SKIP is set"
                );
                None
            } else {
                panic!(
                    "{test_name}: libclang unavailable ({e}). This suite requires \
                     libclang — run inside `nix develop` (flake.nix pins \
                     LIBCLANG_PATH) or install Xcode Command Line Tools. Set \
                     NUDOX_ALLOW_CLANG_TEST_SKIP=1 to skip this test instead of \
                     failing it."
                )
            }
        }
    }
}

/// Parse a C source snippet with the given virtual filename.
fn parse_c<'a>(index: &'a Index<'a>, source: &str) -> crate::clang::oracle::ClangOracle {
    extract_unsaved(index, Path::new("/tmp/test.c"), source, &["-std=c11"])
}

/// Parse a C++ source snippet.
fn parse_cpp<'a>(index: &'a Index<'a>, source: &str) -> crate::clang::oracle::ClangOracle {
    extract_unsaved(index, Path::new("/tmp/test.cpp"), source, &["-std=c++17"])
}

// ── Struct with fields ────────────────────────────────────────────────────────

#[test]
fn struct_with_fields() {
    let Some((_guard, clang)) = require_clang("struct_with_fields") else {
        return;
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
    assert!(matches!(
        x.ty,
        OracleType::Integer {
            signed: true,
            bits: 32
        }
    ));

    let y = oracle
        .fields
        .iter()
        .find(|f| f.name == "y" && f.parent_usr == Some(rec.usr.clone()))
        .expect("field y not found");
    assert!(matches!(
        y.ty,
        OracleType::Integer {
            signed: true,
            bits: 32
        }
    ));
}

// ── Function with params ──────────────────────────────────────────────────────

#[test]
fn function_with_params() {
    let Some((_guard, clang)) = require_clang("function_with_params") else {
        return;
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
    assert!(matches!(
        fun.ret,
        OracleType::Integer {
            signed: true,
            bits: 32
        }
    ));
}

// ── Overload pair — each is its OWN declaration ───────────────────────────────

#[test]
fn overload_pair_two_distinct_declarations() {
    let Some((_guard, clang)) = require_clang("overload_pair_two_distinct_declarations") else {
        return;
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
        overloads[0].usr, overloads[1].usr,
        "overloads must have distinct USRs"
    );

    // One takes int, the other double.
    let takes_int = overloads.iter().any(|f| {
        matches!(
            f.params.first().map(|p| &p.ty),
            Some(OracleType::Integer { .. })
        )
    });
    let takes_float = overloads.iter().any(|f| {
        matches!(
            f.params.first().map(|p| &p.ty),
            Some(OracleType::Float { .. })
        )
    });
    assert!(takes_int, "one overload must take int");
    assert!(takes_float, "one overload must take double");
}

// ── enum class with explicit discriminants ────────────────────────────────────

#[test]
fn enum_class_explicit_values() {
    let Some((_guard, clang)) = require_clang("enum_class_explicit_values") else {
        return;
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
    let Some((_guard, clang)) = require_clang("typedef_and_using_alias") else {
        return;
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
    assert!(matches!(
        td.target,
        OracleType::Integer { signed: false, .. }
    ));

    let ua = oracle
        .aliases
        .iter()
        .find(|a| a.name == "MyInt")
        .expect("MyInt not found");
    assert!(matches!(
        ua.target,
        OracleType::Integer {
            signed: true,
            bits: 32
        }
    ));
}

// ── Namespace ─────────────────────────────────────────────────────────────────

#[test]
fn namespace_emitted_as_module() {
    let Some((_guard, clang)) = require_clang("namespace_emitted_as_module") else {
        return;
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
    let Some((_guard, clang)) = require_clang("template_function_type_var") else {
        return;
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
    let Some((_guard, clang)) = require_clang("lowering_struct_passes_finish") else {
        return;
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
    let Some((_guard, clang)) = require_clang("union_emitted_as_record_union_form") else {
        return;
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

// ── C function pointer → Type::FunctionPointer ────────────────────────────────

/// A typedef for a function pointer `typedef int (*BinaryOp)(int, int)`.
///
/// libclang represents this as `MutPointer(FnPtr { ret, params })` — the `*`
/// in `(*BinaryOp)` is a real pointer. The oracle faithfully preserves that:
/// `OracleType::MutPointer(Box::new(OracleType::FnPtr { .. }))`.
///
/// The lowering chain is therefore:
///   `MutPointer(FnPtr { .. })` →
///   `Primitive::MutPointer(FunctionPointer { params: [i32, i32], ret: Some(i32), abi: None })`
///
/// This is the correct structural representation: the typedef names a pointer to
/// a function, not a bare function type. The `FunctionPointer` inside the pointer
/// carries the parameter and return types without degrading to `Type::Any`.
#[test]
fn c_function_pointer_lowers_to_function_pointer() {
    let Some((_guard, clang)) = require_clang("c_function_pointer_lowers_to_function_pointer") else {
        return;
    };
    let index = Index::new(&clang, false, false);

    // A typedef that aliases a function pointer type.
    let src = r#"
typedef int (*BinaryOp)(int, int);
"#;

    let oracle = parse_c(&index, src);

    // Find the alias for BinaryOp and check its target type.
    let alias = oracle
        .aliases
        .iter()
        .find(|a| a.name == "BinaryOp")
        .expect("BinaryOp typedef must be in the oracle");

    // libclang wraps function pointer typedefs as MutPointer(FnPtr { .. })
    // because the `(*Name)` syntax declares a pointer to the function type.
    assert!(
        matches!(&alias.target, OracleType::MutPointer(inner) if matches!(inner.as_ref(), OracleType::FnPtr { .. })),
        "BinaryOp target must be OracleType::MutPointer(FnPtr {{ .. }}); got {:?}",
        alias.target
    );

    // Lower the oracle and check the IR type.
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

    let alias_entry = pkg
        .iter()
        .find(|(_, e)| e.sym().name == "BinaryOp")
        .expect("BinaryOp must be in the IR");

    let kind = alias_entry.1.kind().as_owned_kind().expect("must be Owned");
    match kind {
        Kind::Alias(a) => {
            let target = a
                .target
                .as_ref()
                .expect("BinaryOp alias must have a target");
            // The target is Primitive::MutPointer(FunctionPointer { .. }).
            match target {
                Type::Primitive(nudox_ir::kinds::ty::Primitive::MutPointer(inner)) => {
                    assert!(
                        matches!(inner.as_ref(), Type::FunctionPointer { .. }),
                        "inner of MutPointer must be FunctionPointer; got {:?}",
                        inner
                    );
                    if let Type::FunctionPointer { params, ret, abi } = inner.as_ref() {
                        // int (*)(int, int) → 2 int params, int return, no ABI.
                        assert_eq!(params.len(), 2, "BinaryOp must have 2 params");
                        assert!(ret.is_some(), "BinaryOp must have a return type");
                        assert!(abi.is_none(), "BinaryOp has no explicit ABI");
                    }
                }
                other => panic!(
                    "BinaryOp alias target must be Primitive::MutPointer(FunctionPointer); got {other:?}"
                ),
            }
        }
        other => panic!("BinaryOp IR entry must be an Alias, got {other:?}"),
    }
}

/// A function accepting a void-returning function pointer `typedef void (*Callback)(void)`.
#[test]
fn void_function_pointer_has_none_return() {
    let Some((_guard, clang)) = require_clang("void_function_pointer_has_none_return") else {
        return;
    };
    let index = Index::new(&clang, false, false);

    let src = r#"
typedef void (*Callback)(void);
"#;

    let oracle = parse_c(&index, src);

    let alias = oracle
        .aliases
        .iter()
        .find(|a| a.name == "Callback")
        .expect("Callback");

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
        .find(|(_, e)| e.sym().name == "Callback")
        .expect("Callback in IR");
    let kind = entry.1.kind().as_owned_kind().expect("must be Owned");
    match kind {
        Kind::Alias(a) => {
            let target = a
                .target
                .as_ref()
                .expect("Callback alias must have a target");
            // MutPointer(FunctionPointer { ret: None, .. }) — see BinaryOp test for rationale.
            match target {
                Type::Primitive(nudox_ir::kinds::ty::Primitive::MutPointer(inner)) => {
                    match inner.as_ref() {
                        Type::FunctionPointer { ret, .. } => {
                            assert!(ret.is_none(), "void-returning fn ptr must have ret = None");
                        }
                        other => panic!(
                            "inner of Callback MutPointer must be FunctionPointer; got {other:?}"
                        ),
                    }
                }
                other => panic!(
                    "Callback target must be Primitive::MutPointer(FunctionPointer); got {other:?}"
                ),
            }
        }
        other => panic!("Callback must be Alias; got {other:?}"),
    }
    let _ = alias; // checked via oracle above
}
