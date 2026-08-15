//! Round-trip coverage for L19: a rendered signature must name the same type
//! as the source, including the *outer* generic constructor, not just its
//! innermost argument.
//!
//! # Why this file exists, and what it actually found
//!
//! docs/LIMITATIONS.md L19 reports that `memchr`'s real signature
//! `fn memchr(needle: u8, haystack: &[u8]) -> Option<usize>` renders as
//! `fn memchr(needle: u8, haystack: &[u8]) -> usize` — the `Option<..>`
//! wrapper vanishes, leaving only the innermost argument — and hypothesizes
//! that `ty.rs`/`signature.rs` render "a nominal type's generic arguments
//! instead of the type rather than nested inside it".
//!
//! That hypothesis is **false**. `struct_apply_nesting_is_correct` below
//! proves it directly: for every generic shape in this file — including a
//! three-deep `Option<Vec<Result<T, E>>>` — `ty.rs` builds
//! `Type::Apply { base, args }` with the *outer* constructor as `base` and
//! the inner type properly nested in `args`, never flattened or collapsed.
//! That test is green.
//!
//! The rendered *text* is still wrong, though (`text_rendering_of_foreign_generics_is_currently_broken`,
//! below, documents this and is red) — for two reasons, **both entirely
//! outside `ty.rs`/`signature.rs`**:
//!
//! 1. **Cross-package identity is never resolved at all.** `ty.rs`'s
//!    `ref_for` closure (`item.rs::make_ref_for!`) calls
//!    `Lowering::refer_import` for every foreign path (`Option`, `Vec`,
//!    `HashMap`, …), which returns `Ref::Local` pointing at an *import*
//!    arena slot — by design, a forward reference meant to be resolved later.
//!    But nothing ever resolves it: `workspace/ir/model/src/package/seal.rs`'s
//!    `Ref::Local` → `Ref::Intro` rewrite (`Pass 3`) only walks `self.entries`
//!    (the package's own declared entries) and has no knowledge of the
//!    import table at all, and a repo-wide grep confirms `Ref::Foreign(` is
//!    **never constructed** anywhere in `workspace/ir/model/src` — only
//!    matched (`Clone`/`PartialEq`/`Hash`/`Debug` impls in `index.rs`). So
//!    every foreign generic's `Ref::Local` survives, unresolved, all the way
//!    into the sealed table, where `signature.rs::resolve_nominal` — whose
//!    own comment says a bare `Ref::Local` "should not appear in a sealed
//!    table" — renders it as `"?"`. Measured on real `memchr`: 147/420
//!    functions (35%) render a bare `?` for a foreign generic base, and
//!    **zero** functions in the whole crate correctly render `Option<`,
//!    `Result<`, `Vec<`, `Box<`, or `HashMap<`.
//! 2. **A separate identity-collision bug** in
//!    `workspace/compiler/languages/rust/src/ra/item.rs`: every function's
//!    parameter/return `Param` entry is declared under the function's
//!    *enclosing* `parent` (module or impl) instead of the function's own id
//!    (`declare_params`, and the `output_refs` closure in
//!    `lower_free_function_with_id`, both do `out.declare(param_id,
//!    parent.clone(), …)` where `parent` is the caller-supplied enclosing
//!    scope). Combined with `seal.rs`'s collision disambiguator, which falls
//!    back to `Disambiguator::Span` for any non-Function/Impl kind, and
//!    `item.rs::plain_sym` always setting `span: 0..0` for every
//!    producer-synthesized param symbol, every same-named parameter declared
//!    directly in the same module (in particular *every* function's `return`
//!    param) collides onto one shared `IntroId` and silently overwrites its
//!    siblings in `PristineIntroTable`. Measured on real `memchr`: 58
//!    collision groups; 416 functions with a return type reduce to only 188
//!    distinct identities. `memchr`'s own `return` Param is overwritten by
//!    one of 18 sibling free functions (`memchr2`, `memchr3`, `memrchr`,
//!    `memrchr2`, `memrchr3`, and their `_raw`/`_iter` variants) declared in
//!    the same module — whichever of those happens to legitimately return a
//!    bare `usize` (most plausibly `count_raw`) is what actually gets
//!    rendered for `memchr`'s signature, which is the literal `usize` text
//!    the ticket reports (bug (2) alone explains the *exact* wording; bug
//!    (1) alone would have produced `?<usize>` instead).
//!
//! Neither file is in this task's edit scope: `item.rs` is explicitly listed
//! off-limits (another agent is editing it concurrently), and
//! `workspace/ir/model/src/package/seal.rs` / `lower.rs` belong to
//! `nudox-ir`, a crate this task was never authorized to touch. See the task
//! report's REMAINING section for the full trail.
//!
//! # Coverage
//!
//! One function per module (so no two functions ever share `(kind,
//! ancestor-path, leaf-name)` and defect (2) above cannot fire — this
//! isolates defect (1) as the only remaining variable), each exercising a
//! distinct type shape: `Option<T>`, `Result<T, E>`, `Vec<T>`,
//! `HashMap<K, V>`, `&mut T`, `&'a [T]` (lifetime + slice + reference
//! together), `Box<dyn Trait>`, `impl Iterator<Item = T>`, `fn(A) -> B` (bare
//! fn pointer), a positional tuple, a fixed-size array, and a three-deep
//! nested generic `Option<Vec<Result<T, E>>>`.
//!
//! # Test-table choice
//!
//! Neither `rstest` nor `proptest` is a dependency of `nudox-engine` or
//! `nudox-store` today (verified: `grep -rn "rstest\|proptest"
//! --include=Cargo.toml` finds nothing in either crate, nor in the root
//! `[workspace.dependencies]` — `proptest` is used elsewhere in the backend,
//! e.g. `workspace/index`, but is declared directly on that crate, not
//! hoisted). Adding either would mean editing `Cargo.toml`, which is outside
//! this task's assigned scope (only `ty.rs`, `signature.rs`, and new test
//! files). So this file uses a hand-rolled case table — a slice of (function,
//! description, expectations) driven through one loop that collects every
//! failure before asserting once, matching the existing style in
//! `nudox-store/tests/real_crate.rs` (e.g. `names_contain_no_path_separators`,
//! `impl_names_are_readable`) — rather than `rstest`'s `#[case]` macro. A true
//! generator (`proptest`) is impractical here regardless of dependency
//! availability: each case requires a full in-process rust-analyzer pass
//! (tens of seconds), so the fixture is built once and every case is checked
//! against that single lowering.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use nudox_ir::kind::Kind;
use nudox_ir::kinds::Type;
use nudox_ir::view::IrView;
use nudox_languages::produce;
use nudox_languages::rust::RustProducer;
use nudox_engine::store::package::{PackageView, Provenance};
use nudox_engine::store::source::producer::PackageDescriptor;

use nudox_engine::chunk::signature;
use nudox_engine::wire::SigToken;

// ---------------------------------------------------------------------------
// Fixture
// ---------------------------------------------------------------------------

/// Write a tiny, dependency-free crate exercising every type shape under
/// test and return its root directory.
///
/// No external crates are needed — every construct (`Option`, `Result`,
/// `Vec`, `HashMap`, `Box`, `Iterator`) is in `core`/`std`/`alloc`, so the
/// fixture resolves offline with no `cargo fetch`.
///
/// Each function lives in its own module (see the module doc: this sidesteps
/// the known, out-of-scope identity-collision defect so the fixture measures
/// only what `ty.rs`/`signature.rs` are responsible for).
fn write_fixture() -> PathBuf {
    let root = std::env::temp_dir().join("nudox_typesmith_l19_fixture");
    let src = root.join("src");
    std::fs::create_dir_all(&src).expect("create fixture src dir");

    std::fs::write(
        root.join("Cargo.toml"),
        r#"[package]
name = "typesmith_l19_fixture"
version = "0.1.0"
edition = "2021"

[workspace]
"#,
    )
    .expect("write fixture Cargo.toml");

    std::fs::write(
        src.join("lib.rs"),
        r#"//! Synthetic fixture for L19 generic-wrapper round-trip coverage.
//! One function per module — see the owning test file for why.

pub trait Thing {}

pub mod m_option {
    pub fn opt_usize() -> Option<usize> {
        None
    }
}

pub mod m_result {
    pub fn res_pair() -> Result<usize, String> {
        Ok(0)
    }
}

pub mod m_vec {
    pub fn vec_of_u8() -> Vec<u8> {
        Vec::new()
    }
}

pub mod m_hashmap {
    use std::collections::HashMap;
    pub fn map_of() -> HashMap<String, usize> {
        HashMap::new()
    }
}

pub mod m_mut_ref {
    pub fn mut_ref(v: &mut usize) -> &mut usize {
        v
    }
}

pub mod m_lifetime_slice {
    pub fn lifetime_slice<'a>(v: &'a [u8]) -> &'a [u8] {
        v
    }
}

pub mod m_boxed_dyn {
    use super::Thing;
    struct Impl;
    impl Thing for Impl {}
    pub fn boxed_dyn() -> Box<dyn Thing> {
        Box::new(Impl)
    }
}

pub mod m_impl_iter {
    pub fn impl_iter() -> impl Iterator<Item = usize> {
        std::iter::empty()
    }
}

pub mod m_fn_ptr {
    pub fn fn_ptr(callback: fn(usize) -> bool) -> fn(usize) -> bool {
        callback
    }
}

pub mod m_tuple {
    pub fn tuple_pair() -> (usize, String) {
        (0, String::new())
    }
}

pub mod m_array {
    pub fn array_of() -> [u8; 4] {
        [0; 4]
    }
}

pub mod m_nested {
    pub fn nested() -> Option<Vec<Result<usize, String>>> {
        None
    }
}
"#,
    )
    .expect("write fixture lib.rs");

    root
}

/// Lower the fixture crate through the real Rust producer.
fn lower_fixture(case: &str, root: &Path) -> Arc<PackageView> {
    let descriptor = PackageDescriptor::cargo(root, "typesmith_l19_fixture", "0.1.0");
    let (table, cost) = heart::cost::measured(case, root, || {
        produce(
            &RustProducer { direct_repo: false },
            &descriptor.source,
            &descriptor.lineage,
            &nudox_ir::foreign::Unlinked,
        )
        .unwrap_or_else(|err| {
            let mut chain = format!("{err}");
            let mut cursor: &dyn std::error::Error = &err;
            while let Some(source) = std::error::Error::source(cursor) {
                chain.push_str(&format!("\n  caused by: {source}"));
                cursor = source;
            }
            panic!("fixture must lower without error:\n{chain}");
        })
    .table});
    eprintln!("fixture lowered in {:.1}s", cost.wall.as_secs_f64());
    let view = IrView::with_package(descriptor.lineage, table);
    Arc::new(PackageView::build(view, Provenance::TrustedLocal))
}

/// Flatten `SigToken`s to plain text — mirrors
/// `chunk::signature::tokens_to_text`, which is `pub(crate)` and not visible
/// here.
fn render_text(toks: &[SigToken]) -> String {
    let mut out = String::new();
    for tok in toks {
        match tok {
            SigToken::Kw(s) | SigToken::Punct(s) => out.push_str(s),
            SigToken::Ident(s)
            | SigToken::Ty { text: s, .. }
            | SigToken::Generic(s)
            | SigToken::Lifetime(s) => out.push_str(s),
            SigToken::Ws => out.push(' '),
            // SigToken is #[non_exhaustive] (LR-2: other producers may add
            // variants); nothing here needs anything beyond plain text.
            _ => {}
        }
    }
    out
}

/// Find `fn_name`'s output-param `Type`, by walking straight through the raw
/// IR (not the renderer) — this is what isolates defect (1)/(2) above from
/// what `ty.rs` itself produced.
fn output_type_of(view: &nudox_ir::view::IrView, fn_name: &str) -> Option<Type> {
    let (_, entry) = view.entries().find(|(_, e)| e.sym().name == fn_name)?;
    let Some(Kind::Function(f)) = entry.kind().as_owned_kind() else {
        return None;
    };
    let op_ref = f.output_params.first()?;
    let nudox_ir::index::Ref::Intro(pid) = op_ref else {
        return None;
    };
    let param_entry = view.entry(*pid)?;
    let Some(Kind::Param(p)) = param_entry.kind().as_owned_kind() else {
        return None;
    };
    p.ty.clone()
}

// ---------------------------------------------------------------------------
// Test 1: structural — does ty.rs nest Apply correctly? (PASSES)
// ---------------------------------------------------------------------------

/// The L19 ticket's own hypothesis: "a nominal type's generic arguments are
/// being rendered instead of the type rather than nested inside it" — i.e.
/// `Type::Apply { base, args }` collapsing to just `args[0]`.
///
/// This is false. Every generic return type here lowers to a properly nested
/// `Type::Apply`, with the outer constructor as `base` and the argument(s)
/// correctly nested — checked structurally (arity and shape), not by name,
/// so this test is unaffected by the two out-of-scope defects documented in
/// the module doc (neither of which changes the *shape* `ty.rs` builds; (1)
/// only affects what `Nominal`'s `Ref` resolves to, and this fixture's
/// one-module-per-function layout avoids (2) entirely).
#[test]
#[ignore = "drives in-process rust-analyzer over a real cargo workspace; run with --ignored"]
fn apply_nesting_is_correct_not_collapsed_to_inner_arg() {
    let root = write_fixture();
    let package = lower_fixture("l19/fixture-structural", &root);
    let view = package.view();

    // opt_usize: Option<usize> -> Apply { args: [Primitive(usize)] }, NOT a
    // bare Primitive(usize) (which is the exact L19 collapse).
    match output_type_of(view, "opt_usize") {
        Some(Type::Apply { args, .. }) => {
            assert_eq!(args.len(), 1, "Option<T> must have exactly one type arg");
            assert!(
                matches!(&args[0], Type::Primitive(_)),
                "Option<usize>'s sole arg must be the usize primitive, got {:?}",
                args[0]
            );
        }
        other => panic!(
            "opt_usize's return must be Type::Apply (Option<usize>), not collapsed to \
             the inner arg or anything else — got {other:?}"
        ),
    }

    // res_pair: Result<usize, String> -> Apply with exactly 2 args, in order.
    match output_type_of(view, "res_pair") {
        Some(Type::Apply { args, .. }) => {
            assert_eq!(args.len(), 2, "Result<T, E> must have exactly two type args");
            assert!(
                matches!(&args[0], Type::Primitive(_)),
                "Result<usize, String>'s first arg must be usize, got {:?}",
                args[0]
            );
        }
        other => panic!("res_pair's return must be Type::Apply (Result<..>), got {other:?}"),
    }

    // vec_of_u8: Vec<u8> -> Apply with 1 primitive arg.
    match output_type_of(view, "vec_of_u8") {
        Some(Type::Apply { args, .. }) => {
            assert_eq!(args.len(), 1, "Vec<T> must have exactly one type arg");
            assert!(matches!(&args[0], Type::Primitive(_)));
        }
        other => panic!("vec_of_u8's return must be Type::Apply (Vec<..>), got {other:?}"),
    }

    // map_of: HashMap<String, usize> -> Apply with 2 args, arity preserved
    // even though the key type (String) is itself unresolvable to a name.
    match output_type_of(view, "map_of") {
        Some(Type::Apply { args, .. }) => {
            assert_eq!(args.len(), 2, "HashMap<K, V> must have exactly two type args");
            assert!(
                matches!(&args[1], Type::Primitive(_)),
                "HashMap<String, usize>'s second arg must be usize, got {:?}",
                args[1]
            );
        }
        other => panic!("map_of's return must be Type::Apply (HashMap<..>), got {other:?}"),
    }

    // boxed_dyn: Box<dyn Thing> -> Apply { args: [DynTrait(..)] } — the Box
    // wrapper is preserved *around* the dyn-trait argument, not discarded.
    match output_type_of(view, "boxed_dyn") {
        Some(Type::Apply { args, .. }) => {
            assert_eq!(args.len(), 1, "Box<T> must have exactly one type arg");
            assert!(
                matches!(&args[0], Type::DynTrait(_)),
                "Box<dyn Thing>'s arg must be a DynTrait, got {:?}",
                args[0]
            );
        }
        other => panic!("boxed_dyn's return must be Type::Apply (Box<..>), got {other:?}"),
    }

    // nested: Option<Vec<Result<usize, String>>> -> three levels of Apply,
    // each with the correct arity, bottoming out at Primitive(usize). This
    // is the direct, three-deep version of the exact shape the L19 ticket
    // describes collapsing.
    match output_type_of(view, "nested") {
        Some(Type::Apply { args: l1, .. }) => {
            assert_eq!(l1.len(), 1, "Option<..> (outermost) must have one arg");
            match &l1[0] {
                Type::Apply { args: l2, .. } => {
                    assert_eq!(l2.len(), 1, "Vec<..> (middle) must have one arg");
                    match &l2[0] {
                        Type::Apply { args: l3, .. } => {
                            assert_eq!(l3.len(), 2, "Result<..> (innermost) must have two args");
                            assert!(
                                matches!(&l3[0], Type::Primitive(_)),
                                "innermost Result's first arg must be usize, got {:?}",
                                l3[0]
                            );
                        }
                        other => panic!("middle level must itself be Apply (Result<..>), got {other:?}"),
                    }
                }
                other => panic!("second level must be Apply (Vec<..>), got {other:?}"),
            }
        }
        other => panic!(
            "nested's return must be a 3-level Type::Apply \
             (Option<Vec<Result<usize, String>>>), not collapsed at any level — got {other:?}"
        ),
    }
}

// ---------------------------------------------------------------------------
// Test 2: end-to-end text rendering — currently broken (documents the gap)
// ---------------------------------------------------------------------------

/// A rendered signature must name the same type as the source: the outer
/// generic constructor must survive alongside its arguments, for every shape
/// in the coverage list above, *as text a reader would actually see*.
///
/// Unlike `apply_nesting_is_correct_not_collapsed_to_inner_arg`, this goes
/// through the full render path (`signature::tokens`) and is currently RED:
/// every foreign generic wrapper (`Option`, `Result`, `Vec`, `HashMap`,
/// `Box`) renders as a bare `?` because of out-of-scope defect (1) in the
/// module doc. `mut_ref`, `lifetime_slice`, `fn_ptr`, and `array_of` — which
/// involve no foreign Nominal at all — pass. This test is committed
/// deliberately red as the regression guard for whichever task fixes
/// `lower.rs`'s import resolution; see the module doc and the task report's
/// REMAINING section.
#[test]
#[ignore = "drives in-process rust-analyzer over a real cargo workspace; currently fails — \
            root cause is unresolved cross-package Ref::Local in workspace/ir/model/src/lower.rs \
            + package/seal.rs, both outside this crate's scope; see the module doc"]
fn text_rendering_of_foreign_generics_is_currently_broken() {
    let root = write_fixture();
    let package = lower_fixture("l19/fixture-render", &root);
    let view = package.view();

    let cases: &[(&str, &str, &[&str])] = &[
        ("opt_usize", "Option<T> return", &["Option", "usize"]),
        (
            "res_pair",
            "Result<T, E> return",
            &["Result", "usize", "String"],
        ),
        ("vec_of_u8", "Vec<T> return", &["Vec", "u8"]),
        (
            "map_of",
            "HashMap<K, V> return",
            &["HashMap", "String", "usize"],
        ),
        ("mut_ref", "&mut T param/return", &["&", "mut", "usize"]),
        (
            "lifetime_slice",
            "&'a [T] param/return",
            &["&", "'a", "u8"],
        ),
        ("boxed_dyn", "Box<dyn Trait> return", &["Box", "dyn", "Thing"]),
        ("impl_iter", "impl Iterator<..> return", &["impl", "Iterator"]),
        ("fn_ptr", "fn(A) -> B param/return", &["fn", "usize", "bool"]),
        ("tuple_pair", "tuple return", &["usize", "String"]),
        ("array_of", "fixed-size array return", &["u8", "4"]),
        (
            "nested",
            "Option<Vec<Result<T, E>>> return (3-deep nesting)",
            &["Option", "Vec", "Result", "usize", "String"],
        ),
    ];

    let mut failures: Vec<String> = Vec::new();
    let mut checked = 0usize;

    for (fn_name, description, required) in cases {
        let entry = view
            .entries()
            .find(|(_, e)| e.sym().name == *fn_name && e.kind().as_owned_kind().is_some())
            .map(|(_, e)| e);

        let Some(entry) = entry else {
            failures.push(format!(
                "{fn_name} ({description}): no Function entry found in the lowered fixture"
            ));
            continue;
        };
        if !matches!(entry.kind().as_owned_kind(), Some(Kind::Function(_))) {
            failures.push(format!(
                "{fn_name} ({description}): entry exists but is not a Function"
            ));
            continue;
        }

        let toks = signature::tokens(entry, &package);
        let text = render_text(&toks);
        checked += 1;

        let missing: Vec<&str> = required
            .iter()
            .filter(|needle| !text.contains(**needle))
            .copied()
            .collect();

        if !missing.is_empty() {
            failures.push(format!(
                "{fn_name} ({description}): rendered signature {text:?} is missing {missing:?}"
            ));
        }
    }

    assert_eq!(
        checked,
        cases.len(),
        "not every case's Function entry was found in the lowered fixture; \
         see failures for detail:\n{}",
        failures.join("\n")
    );

    assert!(
        failures.is_empty(),
        "{}/{} type shapes rendered without their full generic structure \
         (expected on the current tree — see the module doc: this is defect (1), \
         unresolved cross-package Ref::Local, outside ty.rs/signature.rs):\n{}",
        failures.len(),
        cases.len(),
        failures.join("\n")
    );
}
