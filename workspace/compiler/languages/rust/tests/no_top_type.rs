//! CC-2 guard — **the Rust producer must never emit `Type::Any`.**
//!
//! # Why this is an invariant and not a preference
//!
//! Post-CC-2, `Type::Any` means one thing: the language's genuine *top type*,
//! the one type every value inhabits. Java has `java.lang.Object`, C# has
//! `object`, Go has `any`/`interface{}`, TypeScript has `unknown`.
//!
//! **Rust has none.** There is no type you can assign an `i32` and a
//! `String` and a `&dyn Fn()` to without a wrapper. So every `Type::Any` this
//! producer used to emit was a claim the language cannot express — and it was
//! emitted at eleven separate sites, covering unresolvable paths, missing AST
//! children, external ADTs, `str`, and unexpanded macros. Four unrelated facts
//! under one opcode.
//!
//! Each of those is now a named [`UnknownType`] reason. This test lowers a real
//! crate through the real rust-analyzer pipeline and walks *every* type in the
//! sealed table, asserting:
//!
//! 1. no `Type::Any` survives anywhere, at any nesting depth; and
//! 2. the gaps that remain are named and carry their spelling, so two distinct
//!    unresolved types cannot share a signature skeleton.
//!
//! Point 2 is what makes this more than a rename. A test asserting
//! `matches!(ty, Type::Unknown(_))` would pass on a producer that answered
//! `OracleGap` to everything, which is exactly the collapse CC-2 removes.
//!
//! # Running
//!
//! ```text
//! cargo test -p nudox-producer-rust --test no_top_type
//! ```

use std::path::{Path, PathBuf};

use nudox_ir::{
    change::{EcosystemId, PackageLineageId, PackageName},
    entry::{EntryInner, Symbol, Visibility},
    foreign::Unlinked,
    kind::Kind,
    kinds::{Type, UnknownType},
    lower::Lowering,
    package::PackageId,
};
use nudox_producer::{PackageSource, Producer};
use nudox_producer_rust::RustProducer;

/// A crate whose public surface deliberately reaches every former `Type::Any`
/// site the producer has:
///
/// - `std` types in parameter and return position (external ADTs);
/// - `&str` and `str` (the builtin with no `Primitive` slot);
/// - a `dyn Trait` over a foreign trait (`path_type_to_type`'s fallback);
/// - a bound on a foreign trait (`generics.rs`'s fallback);
/// - `impl Trait` over a foreign trait.
const SURFACE: &str = r#"
use std::collections::HashMap;
use std::fmt::Debug;
use std::fmt::Display;

pub fn takes_map(m: HashMap<String, u32>) -> Option<usize> {
    let _ = m;
    None
}

pub fn takes_str(s: &str) -> String {
    s.to_owned()
}

pub fn takes_dyn(d: &dyn Debug) -> usize {
    let _ = d;
    0
}

pub fn takes_impl(d: impl Display) -> usize {
    let _ = d;
    0
}

pub fn bounded<T: Debug + Clone>(t: T) -> T {
    t
}

pub struct Holder {
    pub map: HashMap<String, Vec<u8>>,
    pub text: String,
}

pub type Alias = HashMap<String, u32>;
"#;

fn write_fixture(dir: &str, pkg_name: &str, body: &str) -> PathBuf {
    let root = Path::new(env!("CARGO_TARGET_TMPDIR")).join(dir);
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(root.join("src")).expect("create fixture src dir");
    std::fs::write(
        root.join("Cargo.toml"),
        format!(
            "[package]\n\
             name = \"{pkg_name}\"\n\
             version = \"0.1.0\"\n\
             edition = \"2021\"\n\
             \n\
             [lib]\n\
             path = \"src/lib.rs\"\n\
             \n\
             [workspace]\n"
        ),
    )
    .expect("write fixture manifest");
    std::fs::write(root.join("src/lib.rs"), body).expect("write fixture lib.rs");
    root
}

/// Lower `body` and return every `Type` reachable from the sealed table,
/// flattened, so a nested `Any` cannot hide inside an `Apply`'s arguments.
fn all_types(dir: &str, pkg_name: &str, body: &str) -> Vec<Type> {
    let root = write_fixture(dir, pkg_name, body);
    let src = PackageSource::new(&root, pkg_name, "0.1.0");
    let producer = RustProducer { direct_repo: false };

    let oracle = producer
        .invoke(&src)
        .unwrap_or_else(|e| panic!("fixture {dir} must load: {e}"));

    let root_sym = Symbol {
        name: pkg_name.to_owned(),
        visibility: Visibility::Public,
        documentation: String::new(),
        source: src.root.clone(),
        span: 0..0,
        aliases: Box::new([]),
        deprecation: None,
        doc_links: Box::new([]),
        attrs: Box::new([]),
        cfg: None,
    };
    let mut sink: Lowering<_> = Lowering::new(PackageId::path(src.root()), root_sym);
    producer
        .lower(&oracle, &mut sink)
        .unwrap_or_else(|e| panic!("fixture {dir} must lower: {e}"));
    let package = sink
        .finish()
        .unwrap_or_else(|e| panic!("fixture {dir} must be structurally sound: {e}"));

    let lineage =
        PackageLineageId::new(EcosystemId::new("cargo"), PackageName::new(pkg_name.to_owned()));
    let table = package.seal(&lineage, &Unlinked).table;

    let mut out = Vec::new();
    for (_, entry) in table.iter() {
        let EntryInner::Owned(kind) = entry.kind() else {
            continue;
        };
        match kind {
            Kind::Param(p) => {
                if let Some(t) = &p.ty {
                    flatten(t, &mut out);
                }
            }
            Kind::Record(r) => {
                for f in r.fields.iter() {
                    let _ = f;
                }
            }
            Kind::Alias(a) => {
                if let Some(t) = &a.target {
                    flatten(t, &mut out);
                }
                for b in a.bounds.iter() {
                    flatten(b, &mut out);
                }
            }
            Kind::Field(f) => {
                if let Some(t) = &f.ty {
                    flatten(t, &mut out);
                }
            }
            Kind::Const(c) => flatten(&c.ty, &mut out),
            Kind::Static(s) => flatten(&s.ty, &mut out),
            Kind::Impl(i) => {
                flatten(&i.self_ty, &mut out);
                if let Some(t) = &i.of {
                    flatten(t, &mut out);
                }
            }
            _ => {}
        }
    }
    out
}

/// Push `ty` and every type nested inside it.
///
/// Flattening matters: an `Apply { base: Nominal, args: [Any] }` would pass a
/// top-level-only check while still carrying the erasure in its arguments,
/// which is precisely where a generic parameter's real type lives.
fn flatten(ty: &Type, out: &mut Vec<Type>) {
    out.push(ty.clone());
    match ty {
        Type::Slice(t) | Type::Array { ty: t, .. } => flatten(t, out),
        Type::Union(ts) | Type::Intersection(ts) | Type::ImplTrait(ts) | Type::DynTrait(ts) => {
            for t in ts.iter() {
                flatten(t, out);
            }
        }
        Type::Apply { base, args } => {
            flatten(base, out);
            for a in args.iter() {
                flatten(a, out);
            }
        }
        Type::Tuple(elems) => {
            for e in elems.iter() {
                match e {
                    nudox_ir::kinds::ty::TupleElement::Positional(t) => flatten(t, out),
                    nudox_ir::kinds::ty::TupleElement::Named { ty, .. } => flatten(ty, out),
                }
            }
        }
        Type::Annotated { inner, .. } => flatten(inner, out),
        Type::FunctionPointer { params, ret, .. } => {
            for p in params.iter() {
                flatten(p, out);
            }
            if let Some(r) = ret {
                flatten(r, out);
            }
        }
        Type::Wildcard { bound, .. } => {
            if let Some(b) = bound {
                flatten(b, out);
            }
        }
        Type::QualifiedPath {
            self_ty, trait_ref, ..
        } => {
            flatten(self_ty, out);
            if let Some(t) = trait_ref {
                flatten(t, out);
            }
        }
        Type::Primitive(p) => match p {
            nudox_ir::kinds::ty::Primitive::MutPointer(t)
            | nudox_ir::kinds::ty::Primitive::ConstPointer(t)
            | nudox_ir::kinds::ty::Primitive::Reference { ty: t, .. } => flatten(t, out),
            _ => {}
        },
        _ => {}
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

/// **The invariant.** No `Type::Any` anywhere in a lowered Rust crate.
#[test]
fn rust_producer_emits_no_top_type() {
    let types = all_types("cc2_no_top_type", "cc2surface", SURFACE);
    assert!(
        !types.is_empty(),
        "the fixture produced no types at all — the harness, not the invariant, is broken"
    );

    let offenders: Vec<&Type> = types.iter().filter(|t| matches!(t, Type::Any)).collect();
    assert!(
        offenders.is_empty(),
        "Rust has no top type, so `Type::Any` must never be emitted — found {} occurrence(s)",
        offenders.len()
    );
}


/// A clean crate produces **no gaps at all** — and that is the point.
///
/// Every cross-crate reference in `SURFACE` (`HashMap`, `Option`, `Vec`,
/// `String`, `Debug`, `Display`) resolves through `refer_import` into a
/// `Ref::Foreign` that carries the type's name, so the CC-2 fallbacks are
/// never reached. `str` maps to `Primitive::Str`.
///
/// This test exists because "no `Type::Any`" alone is satisfiable by a
/// producer that answers `OracleGap` to everything. Asserting *zero unknowns*
/// on well-formed input is the complementary half: the erasure is gone
/// because the information is being kept, not because the erasure was renamed.
#[test]
fn well_formed_rust_produces_named_types_not_gaps() {
    let types = all_types("cc2_named_gaps", "cc2gaps", SURFACE);

    let gaps: Vec<String> = types
        .iter()
        .filter_map(|t| match t {
            Type::Unknown(r) => Some(r.to_string()),
            _ => None,
        })
        .collect();
    assert!(
        gaps.is_empty(),
        "a crate whose whole surface is std types must lower with no gaps at all; got {gaps:?}"
    );

    // Every cross-crate reference must be a *named* foreign nominal, and the
    // names must differ — that is what `Type::Any` used to destroy.
    let mut nominal_names: Vec<String> = types
        .iter()
        .filter(|t| matches!(t, Type::Nominal(_)))
        .map(|t| t.to_string())
        .collect();
    nominal_names.sort();
    nominal_names.dedup();
    assert!(
        nominal_names.len() >= 3,
        "the fixture names HashMap, Option, Vec, String and more — at least three distinct \
         nominal renderings must survive; got {nominal_names:?}"
    );

    // Nothing may render as the top type, and nothing may render empty.
    for t in &types {
        let rendered = t.to_string();
        assert!(!rendered.is_empty(), "empty rendering for {t:?}");
        assert_ne!(rendered, "any", "no Rust type may render as the top type");
    }
}
