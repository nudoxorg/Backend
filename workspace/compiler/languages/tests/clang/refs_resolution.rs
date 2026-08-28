//! RED tests: C/C++ reference resolution — intra-package AND standard-library.
//!
//! Before this fix (see `src/clang/lower.rs`'s "Reference resolution" module
//! doc), `extract::resolve_type` kept only a `Named` type's display name and
//! discarded its declaration's USR, so `lower_type` — which had no access to
//! the `Lowering` sink at all — blanket-mapped every zero-arg `OracleType::Named`
//! to `Type::unresolved_external`, even a same-package `struct` field one line
//! above its own declaration. This file pins the fixed end state, one fixture
//! per case, asserting on the resolved `Ref`/`Type` shape — never on counts
//! (see `docs/AGENTS-DOCTRINE.md`'s "count-based tests cannot see field loss").
//!
//! Hermetic: every fixture is written fresh into a `tempfile::TempDir` and run
//! through the full `nudox_languages::produce` pipeline (`invoke` -> `lower`
//! -> `finish` -> `seal`), exactly like `tests/clang/real_package.rs`. Each
//! fixture is its own package/tempdir so names never collide across cases and
//! each test can look entries up by bare name.
//!
//! Requires libclang at runtime (`ClangProducer::invoke` loads it via
//! `clang-sys`'s `runtime` feature) — see `real_package.rs`'s own doc for why
//! these tests do not silently skip when it is unavailable: a missing
//! libclang here is loud (a panic naming the underlying error), not a quiet
//! green.

use nudox_ir::{
    apply::PristineIntroTable,
    change::{EcosystemId, IntroId, PackageLineageId, PackageName},
    entry::Entry,
    foreign::{ForeignOrigin, Unlinked},
    index::Ref,
    kinds::ty::{Primitive, Type},
    kinds::{Alias, Field, Record},
    vocab::ReferenceKind,
};
use nudox_languages::clang::ClangProducer;
use nudox_languages::{PackageSource, Produced, ProducerError, produce};

/// See `real_package.rs`'s `CLANG_SINGLETON` doc: `clang::Clang` allows only
/// one live instance per process, and `cargo test` runs every `#[test]` in
/// this binary concurrently on its own thread. Serialize acquisition across
/// this whole file's tests.
static CLANG_SINGLETON: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn lineage(name: &str) -> PackageLineageId {
    PackageLineageId::new(EcosystemId::new("cpp"), PackageName::new(name))
}

/// Write `files` (each a package-relative path + contents) into a fresh
/// tempdir and run the full pipeline over it.
fn produce_fixture(files: &[(&str, &str)], name: &str) -> Result<Produced, ProducerError> {
    let dir = tempfile::tempdir().expect("tempdir");
    for (rel, body) in files {
        let path = dir.path().join(rel);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("create fixture subdir");
        }
        std::fs::write(&path, body).expect("write fixture file");
    }
    let source = PackageSource::new(dir.path(), name, "0.0.0");
    produce(&ClangProducer::new(), &source, &lineage(name), &Unlinked)
}

/// The whole `#[source]` chain — `real_package.rs`'s `chain` helper, so a
/// hermetic parse failure is diagnosable rather than a bare `LoweringFailed`.
fn chain(err: &ProducerError) -> String {
    let mut links: Vec<String> = vec![err.to_string()];
    let mut cause = std::error::Error::source(err);
    while let Some(c) = cause {
        links.push(c.to_string());
        cause = c.source();
    }
    links.join(" <- ")
}

fn lower_fixture(files: &[(&str, &str)], name: &str) -> Produced {
    produce_fixture(files, name)
        .unwrap_or_else(|err| panic!("clang producer failed on `{name}`: {}", chain(&err)))
}

/// Find the one entry named `name`, regardless of kind. Fixtures in this file
/// use unique names across their whole small source, so a bare name lookup is
/// unambiguous — the same convention `tests/python/refs_resolution.rs` and
/// `real_package.rs` use.
fn find_by_name<'a>(table: &'a PristineIntroTable, name: &str) -> (IntroId, &'a Entry) {
    table
        .iter()
        .find(|(_, e)| e.sym().name == name)
        .unwrap_or_else(|| panic!("entry named `{name}` must be present in the sealed table"))
}

fn intro_id(table: &PristineIntroTable, name: &str) -> IntroId {
    find_by_name(table, name).0
}

/// The type of the one `Field` entry named `field_name`.
fn field_ty(table: &PristineIntroTable, field_name: &str) -> Type {
    let (_, entry) = find_by_name(table, field_name);
    entry
        .downcast::<Field>()
        .unwrap_or_else(|| panic!("`{field_name}` must be a Field entry"))
        .body()
        .ty
        .clone()
        .unwrap_or_else(|| panic!("field `{field_name}` must carry a type"))
}

// ── CC1: struct field naming another same-package struct ────────────────────

#[test]
fn cc1_struct_field_resolves_to_same_package_intro() {
    let _guard = CLANG_SINGLETON
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let produced = lower_fixture(
        &[(
            "cc1.c",
            "struct Inner{int x;}; struct Outer{ struct Inner in; };\n",
        )],
        "cc1-struct-field",
    );
    let table = produced.table;

    let inner_id = intro_id(&table, "Inner");
    match field_ty(&table, "in") {
        Type::Nominal(Ref::Intro(id)) => assert_eq!(
            id, inner_id,
            "field `Outer::in` must resolve to the same-package `Inner` struct"
        ),
        other => panic!("field `in` must be a resolved Nominal(Ref::Intro), got {other:?}"),
    }
}

// ── CC2: self-referential pointer field ──────────────────────────────────────

#[test]
fn cc2_self_referential_pointer_field_resolves_to_own_record() {
    let _guard = CLANG_SINGLETON
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let produced = lower_fixture(
        &[("cc2.c", "struct Node{int val; struct Node* next;};\n")],
        "cc2-self-ref",
    );
    let table = produced.table;

    let node_id = intro_id(&table, "Node");
    match field_ty(&table, "next") {
        Type::Primitive(Primitive::MutPointer(inner)) => match *inner {
            Type::Nominal(Ref::Intro(id)) => assert_eq!(
                id, node_id,
                "`Node::next` must point back at the same-package `Node` struct"
            ),
            other => panic!("MutPointer's inner type must be Nominal(Ref::Intro), got {other:?}"),
        },
        other => panic!("`next` must lower to Primitive::MutPointer(..), got {other:?}"),
    }
}

// ── CC3: base-class inheritance ──────────────────────────────────────────────

#[test]
fn cc3_inheritance_base_resolves_to_same_package_intro() {
    let _guard = CLANG_SINGLETON
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let produced = lower_fixture(
        &[(
            "cc3.cpp",
            "class Base{}; class Derived : public Base{};\n",
        )],
        "cc3-inheritance",
    );
    let table = produced.table;

    let base_id = intro_id(&table, "Base");
    let (_, derived_entry) = find_by_name(&table, "Derived");
    let derived = derived_entry
        .downcast::<Record>()
        .expect("Derived must be a Record entry");
    let super_types = &derived.body().super_types;
    assert_eq!(
        super_types.len(),
        1,
        "Derived must have exactly one super-type, got {super_types:?}"
    );
    match &super_types[0] {
        Type::Nominal(Ref::Intro(id)) => assert_eq!(
            *id, base_id,
            "Derived's base class must resolve to the same-package `Base` class"
        ),
        other => panic!("Derived's super-type must be Nominal(Ref::Intro), got {other:?}"),
    }
}

// ── CC4: `using` alias naming a same-package struct ──────────────────────────

#[test]
fn cc4_alias_target_resolves_to_same_package_intro() {
    let _guard = CLANG_SINGLETON
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let produced = lower_fixture(
        &[(
            "cc4.cpp",
            "struct Widget{}; using WidgetAlias = Widget;\n",
        )],
        "cc4-alias",
    );
    let table = produced.table;

    let widget_id = intro_id(&table, "Widget");
    let (_, alias_entry) = find_by_name(&table, "WidgetAlias");
    let alias = alias_entry
        .downcast::<Alias>()
        .expect("WidgetAlias must be an Alias entry");
    match &alias.body().target {
        Some(Type::Nominal(Ref::Intro(id))) => assert_eq!(
            *id, widget_id,
            "WidgetAlias's target must resolve to the same-package `Widget` struct"
        ),
        other => panic!("WidgetAlias target must be Some(Nominal(Ref::Intro)), got {other:?}"),
    }
}

// ── CC7: enum-typed field ─────────────────────────────────────────────────────

#[test]
fn cc7_enum_field_resolves_to_same_package_intro() {
    let _guard = CLANG_SINGLETON
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let produced = lower_fixture(
        &[(
            "cc7.cpp",
            "enum class Color{Red}; struct Pixel{ Color c; };\n",
        )],
        "cc7-enum-field",
    );
    let table = produced.table;

    let color_id = intro_id(&table, "Color");
    match field_ty(&table, "c") {
        Type::Nominal(Ref::Intro(id)) => assert_eq!(
            id, color_id,
            "field `Pixel::c` must resolve to the same-package `Color` enum"
        ),
        other => panic!("field `c` must be a resolved Nominal(Ref::Intro), got {other:?}"),
    }
}

// ── CC9: standard-library type ────────────────────────────────────────────────

/// `std::string` must lower to a *named* `Ref::Foreign` in the `"std"`
/// namespace — never a bare `UnresolvedExternal` (which carries no
/// `ForeignKey` and can never be linked by any later pass), and never a
/// same-package `Ref::Intro` (this producer does not declare `std::string`).
#[test]
fn cc9_std_type_resolves_to_named_foreign_ref_in_std_namespace() {
    let _guard = CLANG_SINGLETON
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let produced = lower_fixture(
        &[(
            "cc9.cpp",
            "#include <string>\nstruct Row{ std::string name; };\n",
        )],
        "cc9-std-type",
    );
    let table = produced.table;

    match field_ty(&table, "name") {
        Type::Nominal(Ref::Foreign { key, target }) => {
            match &key.origin {
                ForeignOrigin::Namespace { namespace, ecosystem } => {
                    assert_eq!(
                        &**namespace, "std",
                        "std::string's ForeignKey must carry the \"std\" namespace"
                    );
                    assert_eq!(
                        ecosystem.as_str(),
                        "cpp",
                        "the ecosystem must be \"cpp\", not a fabricated package registry"
                    );
                }
                other => panic!(
                    "std::string must carry a Namespace origin (no publishing package \
                     is knowable at this layer), got {other:?}"
                ),
            }
            assert!(
                key.display.contains("string"),
                "the ForeignKey must keep the source spelling; display={:?}",
                key.display
            );
            assert_eq!(
                target, None,
                "with no resolver supplied (Unlinked) the foreign ref must stay unlinked"
            );
        }
        Type::Unknown(other) => panic!(
            "std::string must be a named Ref::Foreign, not a bare unresolved gap — got \
             Unknown({other:?}); a bare unresolved-external carries no ForeignKey and can \
             never be linked by any later pass"
        ),
        other => panic!("field `name` must be Nominal(Ref::Foreign), got {other:?}"),
    }
}

// ── CC10: class-template application naming a same-package template ─────────

/// `Box<Widget>` where `template<class T> struct Box{};` is declared in the
/// same package must resolve BOTH the template base (`Box`) and the type
/// argument (`Widget`) to same-package `Ref::Intro`s.
///
/// Root cause this pins the fix for: for a `TypeKind::Record` whose
/// declaration is a class-template *specialization* (what `Box<Widget>`'s
/// `get_declaration()` returns), libclang reports the SPECIALIZATION's own
/// USR (empirically `c:@S@Box>#$@S@Widget` for this exact fixture) — not the
/// PRIMARY TEMPLATE declaration's USR (`c:@ST>1#T@Box`) that
/// `known_nominal_usrs` (`lower.rs`) indexes `Box` under. The two USRs never
/// match, so the base fell back to `Type::unresolved_external` even though
/// `Box` is declared one line above. `extract.rs`'s `named_decl_usr` must
/// normalize through `Entity::get_template` (libclang's
/// `clang_getSpecializedCursorTemplate`) to recover the primary template's
/// USR before `lower_named` looks it up.
#[test]
fn cc10_template_application_base_resolves_to_same_package_intro() {
    let _guard = CLANG_SINGLETON
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let produced = lower_fixture(
        &[(
            "cc10.cpp",
            "template<class T> struct Box{ T inner; };\n\
             struct Widget{};\n\
             struct Holder{ Box<Widget> b; };\n",
        )],
        "cc10-template-application",
    );
    let table = produced.table;

    let box_id = intro_id(&table, "Box");
    let widget_id = intro_id(&table, "Widget");

    match field_ty(&table, "b") {
        Type::Apply { base, args } => {
            match *base {
                Type::Nominal(Ref::Intro(id)) => assert_eq!(
                    id, box_id,
                    "Box<Widget>'s base must resolve to the same-package `Box` class template"
                ),
                other => panic!(
                    "Box<Widget>'s base must be Nominal(Ref::Intro) to `Box`, got {other:?}"
                ),
            }
            assert_eq!(args.len(), 1, "Box<Widget> must carry exactly one type argument");
            match &args[0] {
                Type::Nominal(Ref::Intro(id)) => assert_eq!(
                    *id, widget_id,
                    "Box<Widget>'s argument must resolve to the same-package `Widget` struct"
                ),
                other => panic!(
                    "Box<Widget>'s argument must be Nominal(Ref::Intro) to `Widget`, got {other:?}"
                ),
            }
        }
        other => panic!("field `b` must lower to Type::Apply, got {other:?}"),
    }
}

// ── CC5: cross-TU function call within the same package ─────────────────────

/// `helper` is *defined* in one translation unit and *declared* (via a shared
/// header) and *called* from another. `producer::merge_oracle` merges both
/// TUs' partial oracles (deduped by USR) before `lower_oracle` runs, and
/// `lower_oracle`'s occurrence loop already matches `reference.target` against
/// `oracle.functions` by USR across the whole merged package — so this proves
/// intra-package call resolution survives a real multi-file layout, not just
/// a single in-memory string. (`real_package.rs`'s module doc explains why a
/// same-directory quote-include needs no `compile_commands.json`: quote
/// search always includes the including file's own directory.)
#[test]
fn cc5_cross_tu_call_resolves_to_same_package_intro() {
    let _guard = CLANG_SINGLETON
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let produced = lower_fixture(
        &[
            (
                "mathhelpers.h",
                "#ifndef MATHHELPERS_H\n#define MATHHELPERS_H\nint helper(int x);\n#endif\n",
            ),
            (
                "helper_impl.c",
                "#include \"mathhelpers.h\"\nint helper(int x) { return x + 1; }\n",
            ),
            (
                "caller_impl.c",
                "#include \"mathhelpers.h\"\nint caller(int x) { return helper(x); }\n",
            ),
        ],
        "cc5-cross-tu-call",
    );
    let table = produced.table;

    let caller_id = intro_id(&table, "caller");
    let helper_id = intro_id(&table, "helper");

    let hit = produced.occurrences.iter().find(|(owner, occ)| {
        *owner == caller_id
            && occ.target.intro == helper_id
            && occ.kind == ReferenceKind::FunctionCall
    });
    assert!(
        hit.is_some(),
        "expected a FunctionCall occurrence owner=caller -> target=helper across the two \
         translation units; got occurrences={:?}",
        produced
            .occurrences
            .iter()
            .map(|(owner, occ)| (*owner, occ.target.intro, occ.kind))
            .collect::<Vec<_>>()
    );
}

// ── Regression: nothing here breaks an ordinary genuinely-external name ─────

/// A name libclang resolves to a real declaration that is neither this
/// package's nor `std`'s (a `#include`d project header outside the parsed
/// TU's own file, so it never enters `known`) must still fall back to the
/// honest `Type::unresolved_external`, exactly as before this fix — this test
/// guards against `lower_named` accidentally over-claiming `Ref::Intro` for
/// everything that merely has a USR.
#[test]
fn genuinely_external_non_std_name_still_falls_back_to_unresolved_external() {
    let _guard = CLANG_SINGLETON
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    // `FILE*` from <cstdio> is a real declaration this producer neither
    // declares nor recognizes as `std::`-namespaced (it lives at global scope
    // in the C library, not inside `namespace std`), so it must remain a
    // named, honest external gap.
    let produced = lower_fixture(
        &[(
            "regression.cpp",
            "#include <cstdio>\nstruct Logger{ FILE* out; };\n",
        )],
        "regression-external",
    );
    let table = produced.table;

    match field_ty(&table, "out") {
        Type::Primitive(Primitive::MutPointer(inner)) => match *inner {
            Type::Unknown(nudox_ir::kinds::UnknownType::UnresolvedExternal { name }) => {
                assert!(
                    name.contains("FILE"),
                    "the external gap must keep the source spelling; got {name:?}"
                );
            }
            Type::Nominal(Ref::Foreign { .. }) => {
                // Also acceptable: a future libclang/libc surfaces FILE under
                // a namespaced USR this producer's `is_std_usr` heuristic
                // matches. The only forbidden outcome is a same-package
                // Ref::Intro, which the outer match's fallthrough rejects.
            }
            other => panic!(
                "`FILE*`'s pointee must never resolve to a same-package Nominal(Ref::Intro); \
                 got {other:?}"
            ),
        },
        other => panic!("`out` must lower to Primitive::MutPointer(..), got {other:?}"),
    }
}
