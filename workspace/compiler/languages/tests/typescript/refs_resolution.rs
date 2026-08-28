//! RED tests: TypeScript reference resolution — intra-package AND cross-package.
//!
//! These pin the desired END STATE, not today's behavior. They fail today for
//! TWO stacked reasons, both observed by the diagnostic that produced this file:
//!
//!   1. The EXTRACT layer (`typescript/extract`) classifies every named type
//!      *reference* — `owner: User`, `u: User`, `g: Foo` — as
//!      `TypeOwned::TypeVar("User")`, i.e. a generic type parameter, instead of
//!      a nominal reference to a declared type. So the reference identity is
//!      thrown away before lowering even runs.
//!   2. `typescript/emit.rs`'s `lower_type` helper is sink-less: even for a
//!      correctly-classified `TypeOwned::Nominal` it can only emit
//!      `Type::Unknown(UnresolvedExternal { name })`, never a `Ref`.
//!
//! Requirement being specified:
//!   (a) an intra-module named type resolves to a same-package `Ref::Intro`;
//!   (b) a type imported from another package is a *named* `Ref::Foreign`
//!       (an npm-namespace `ForeignKey`) that a resolver can LINK — never an
//!       `UnresolvedExternal` and never a spurious `TypeVar`.
//!
//! Probes use interface FIELD types — the mainline `lower_type` position — so a
//! fix is validated structurally rather than through one narrow alias path.
//!
//! Hermetic: literal TypeScript on disk, OXC in-process, no npm/network.

use std::fs;

use nudox_ir::{
    change::{EcosystemId, IntroId, PackageLineageId, PackageName, StableRef},
    entry::EntryInner,
    foreign::{ForeignKey, ForeignOrigin, ForeignResolver, Resolution, Unlinked},
    index::Ref,
    kind::Kind,
    kinds::ty::{Type, UnknownType},
};
use nudox_languages::typescript::TypescriptProducer;
use nudox_languages::{PackageSource, produce};

/// Links every foreign key to one target — proves the producer emitted a
/// *linkable* `Ref::Foreign`, not a dead `UnresolvedExternal`/`TypeVar`.
#[derive(Clone)]
struct ResolveEveryForeign(StableRef);
impl ForeignResolver for ResolveEveryForeign {
    fn resolve(&self, _key: &ForeignKey) -> Resolution {
        Resolution::Resolved(self.0.clone())
    }
}

fn write_pkg(files: &[(&str, &str)]) -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("create npm package tempdir");
    fs::write(
        dir.path().join("package.json"),
        r#"{"name":"refs-fixture","version":"1.0.0","types":"index.d.ts"}"#,
    )
    .expect("write package.json");
    for (name, body) in files {
        fs::write(dir.path().join(name), body).expect("write source file");
    }
    dir
}

fn lineage() -> PackageLineageId {
    PackageLineageId::new(EcosystemId::new("npm"), PackageName::new("refs-fixture"))
}

/// The lowered type of the named FIELD entry.
fn field_ty(table: &nudox_ir::apply::PristineIntroTable, field: &str) -> Type {
    let (_, entry) = table
        .iter()
        .find(|(_, e)| {
            e.sym().name == field && matches!(e.kind(), EntryInner::Owned(Kind::Field(_)))
        })
        .unwrap_or_else(|| panic!("field `{field}` must be present as a Field entry"));
    let EntryInner::Owned(Kind::Field(f)) = entry.kind() else {
        unreachable!()
    };
    f.ty.clone()
        .unwrap_or_else(|| panic!("field `{field}` must carry a type"))
}

// ── (a) intra-module type ref must resolve to Ref::Intro ────────────────────

#[test]
fn ts_intra_module_type_ref_resolves_to_intro() {
    let dir = write_pkg(&[(
        "index.ts",
        "export interface User { id: number; }\n\
         export interface Wrapper { owner: User; }\n",
    )]);
    let source = PackageSource::new(dir.path(), "refs-fixture", "1.0.0");
    let produced = produce(&TypescriptProducer::new(), &source, &lineage(), &Unlinked)
        .expect("the intra-module TS fixture must lower");
    let table = produced.table;

    let user_id: IntroId = table
        .iter()
        .find(|(_, e)| e.sym().name == "User")
        .map(|(id, _)| id)
        .expect("the `User` interface declaration must be present");

    match field_ty(&table, "owner") {
        Type::Nominal(Ref::Intro(id)) => assert_eq!(
            id, user_id,
            "`owner: User` must resolve to the SAME-package User declaration's Intro id"
        ),
        Type::Unknown(UnknownType::UnresolvedExternal { name }) => panic!(
            "intra-module type `User` survived as UnresolvedExternal({name:?}) — the \
             sink-less `lower_type` defect"
        ),
        Type::TypeVar(name) => panic!(
            "intra-module type `User` was misclassified as a generic TypeVar({name:?}) by the \
             extract layer — a reference to a declared interface is NOT a type parameter; it \
             must resolve to Ref::Intro"
        ),
        other => panic!("`owner` field type must be a resolved Nominal(Intro), got {other:?}"),
    }
}

// ── (b) cross-package import type ref must be a named Ref::Foreign ──────────

#[test]
fn ts_cross_package_type_ref_is_named_foreign_key() {
    let dir = write_pkg(&[(
        "index.ts",
        "import { Foo } from \"external-dep\";\n\
         export interface Holder { g: Foo; }\n",
    )]);
    let source = PackageSource::new(dir.path(), "refs-fixture", "1.0.0");

    let foreign_target = StableRef::new(
        PackageLineageId::new(EcosystemId::new("npm"), PackageName::new("external-dep")),
        IntroId::from_raw([0x5a; 32]),
    );
    let resolver = ResolveEveryForeign(foreign_target.clone());
    let produced = produce(&TypescriptProducer::new(), &source, &lineage(), &resolver)
        .expect("the cross-package TS fixture must lower");
    let table = produced.table;

    match field_ty(&table, "g") {
        Type::Nominal(Ref::Foreign { key, target }) => {
            assert!(
                matches!(
                    key.origin,
                    ForeignOrigin::Namespace { .. } | ForeignOrigin::Package(_)
                ),
                "a cross-package npm type must carry a placeable ForeignKey origin, got {:?}",
                key.origin
            );
            assert!(
                key.path.contains("Foo") || key.display.contains("Foo"),
                "the ForeignKey must name the imported type `Foo`; path={:?} display={:?}",
                key.path,
                key.display
            );
            assert_eq!(
                target, Some(foreign_target),
                "with a populated resolver the foreign type must LINK — proving the key is \
                 join-ready, not merely a display string"
            );
        }
        Type::Unknown(UnknownType::UnresolvedExternal { name }) => panic!(
            "cross-package type `Foo` survived as UnresolvedExternal({name:?}) — no ForeignKey \
             means the corpus link pass can never join it"
        ),
        Type::TypeVar(name) => panic!(
            "cross-package type `Foo` was misclassified as a generic TypeVar({name:?}) — an \
             imported type must become a named Ref::Foreign"
        ),
        other => panic!("`g` field type must be a Nominal(Foreign), got {other:?}"),
    }
}

// ── Adversarial-audit RED tests (T1-T10) ─────────────────────────────────────
//
// A prior fix made the (a)/(b) cases above pass: intra-module refs resolve to
// `Ref::Intro`, bare cross-package imports resolve to a named `Ref::Foreign`.
// An adversarial audit found the fix incomplete on real edge cases — aliased
// imports keying on the wrong name, qualified/namespaced names never being
// split before resolution, sibling generic params and `infer` names not
// reaching bounds/defaults, dynamic `import(...)` types discarding their
// specifier, and global lib types reporting as typos. Each test below pins
// one such case.

fn find_function<'a>(
    table: &'a nudox_ir::apply::PristineIntroTable,
    name: &str,
) -> &'a nudox_ir::kinds::Function {
    let (_, entry) = table
        .iter()
        .find(|(_, e)| e.sym().name == name && matches!(e.kind(), EntryInner::Owned(Kind::Function(_))))
        .unwrap_or_else(|| panic!("function `{name}` must be present as a Function entry"));
    let EntryInner::Owned(Kind::Function(f)) = entry.kind() else {
        unreachable!()
    };
    f
}

// ── T1: aliased named import keys the ForeignKey on the EXPORT name ─────────

#[test]
fn ts_aliased_import_keys_foreign_ref_on_export_name_not_local_alias() {
    let dir = write_pkg(&[(
        "index.ts",
        "import { Foo as Bar } from \"external-dep\";\n\
         export interface Holder { g: Bar; }\n",
    )]);
    let source = PackageSource::new(dir.path(), "refs-fixture", "1.0.0");
    let foreign_target = StableRef::new(
        PackageLineageId::new(EcosystemId::new("npm"), PackageName::new("external-dep")),
        IntroId::from_raw([0x11; 32]),
    );
    let resolver = ResolveEveryForeign(foreign_target.clone());
    let produced = produce(&TypescriptProducer::new(), &source, &lineage(), &resolver)
        .expect("the aliased-import TS fixture must lower");
    let table = produced.table;

    match field_ty(&table, "g") {
        Type::Nominal(Ref::Foreign { key, target }) => {
            assert!(
                key.path.contains("Foo") || key.display.contains("Foo"),
                "an aliased import (`import {{ Foo as Bar }}`) must key the ForeignKey on the \
                 EXPORT name `Foo`, not the use-site local alias `Bar`; path={:?} display={:?}",
                key.path,
                key.display,
            );
            assert!(
                !key.path.contains("Bar") && !key.display.contains("Bar"),
                "the ForeignKey must not leak the use-site local alias `Bar`; path={:?} \
                 display={:?}",
                key.path,
                key.display,
            );
            assert_eq!(
                target,
                Some(foreign_target),
                "a correctly-keyed foreign ref must still link"
            );
        }
        other => panic!("`g` field type must be Nominal(Foreign), got {other:?}"),
    }
}

// ── T2: default import keys the ForeignKey on "default" ─────────────────────

#[test]
fn ts_default_import_keys_foreign_ref_on_default_not_local_name() {
    let dir = write_pkg(&[(
        "index.ts",
        "import Widget from \"external-dep\";\n\
         export interface Holder { g: Widget; }\n",
    )]);
    let source = PackageSource::new(dir.path(), "refs-fixture", "1.0.0");
    let foreign_target = StableRef::new(
        PackageLineageId::new(EcosystemId::new("npm"), PackageName::new("external-dep")),
        IntroId::from_raw([0x22; 32]),
    );
    let resolver = ResolveEveryForeign(foreign_target.clone());
    let produced = produce(&TypescriptProducer::new(), &source, &lineage(), &resolver)
        .expect("the default-import TS fixture must lower");
    let table = produced.table;

    match field_ty(&table, "g") {
        Type::Nominal(Ref::Foreign { key, target }) => {
            assert!(
                key.path.contains("default") || key.display.contains("default"),
                "a default import (`import Widget from \"pkg\"`) must key the ForeignKey on \
                 TypeScript's own default-export name `default`, not the local name `Widget`; \
                 path={:?} display={:?}",
                key.path,
                key.display,
            );
            assert!(
                !key.path.contains("Widget") && !key.display.contains("Widget"),
                "the ForeignKey must not leak the local name `Widget`; path={:?} display={:?}",
                key.path,
                key.display,
            );
            assert_eq!(
                target,
                Some(foreign_target),
                "a correctly-keyed foreign ref must still link"
            );
        }
        other => panic!("`g` field type must be Nominal(Foreign), got {other:?}"),
    }
}

// ── T3: namespace-qualified cross-package name resolves to a Foreign member ─

#[test]
fn ts_namespace_qualified_cross_package_type_resolves_to_foreign_member() {
    let dir = write_pkg(&[(
        "index.ts",
        "import * as ns from \"external-dep\";\n\
         export interface Holder { g: ns.Thing; }\n",
    )]);
    let source = PackageSource::new(dir.path(), "refs-fixture", "1.0.0");
    let foreign_target = StableRef::new(
        PackageLineageId::new(EcosystemId::new("npm"), PackageName::new("external-dep")),
        IntroId::from_raw([0x33; 32]),
    );
    let resolver = ResolveEveryForeign(foreign_target.clone());
    let produced = produce(&TypescriptProducer::new(), &source, &lineage(), &resolver)
        .expect("the namespace-qualified cross-package TS fixture must lower");
    let table = produced.table;

    match field_ty(&table, "g") {
        Type::Nominal(Ref::Foreign { key, target }) => {
            assert!(
                key.path.contains("Thing") || key.display.contains("Thing"),
                "`ns.Thing` must key the ForeignKey on the member `Thing`; path={:?} \
                 display={:?}",
                key.path,
                key.display,
            );
            assert_eq!(
                target,
                Some(foreign_target),
                "a correctly-keyed foreign ref must still link"
            );
        }
        Type::Unknown(UnknownType::UnresolvedExternal { name }) => panic!(
            "namespace-qualified cross-package type `ns.Thing` survived as \
             UnresolvedExternal({name:?}) — the dotted name was never split into \
             (binding, member) before resolution"
        ),
        other => panic!("`g` field type must be Nominal(Foreign), got {other:?}"),
    }
}

// ── T4: namespace-qualified relative import resolves to sibling's Intro ─────

#[test]
fn ts_namespace_qualified_relative_import_resolves_to_sibling_intro() {
    let dir = write_pkg(&[
        ("sibling.ts", "export interface Local { id: number; }\n"),
        (
            "index.ts",
            "import * as m from \"./sibling\";\n\
             export interface Holder { f: m.Local; }\n",
        ),
    ]);
    let source = PackageSource::new(dir.path(), "refs-fixture", "1.0.0");
    let produced = produce(&TypescriptProducer::new(), &source, &lineage(), &Unlinked)
        .expect("the namespace-qualified relative-import TS fixture must lower");
    let table = produced.table;

    let local_id: IntroId = table
        .iter()
        .find(|(_, e)| e.sym().name == "Local")
        .map(|(id, _)| id)
        .expect("the sibling's `Local` interface declaration must be present");

    match field_ty(&table, "f") {
        Type::Nominal(Ref::Intro(id)) => assert_eq!(
            id, local_id,
            "`f: m.Local` must resolve to the sibling module's `Local` declaration"
        ),
        Type::Unknown(UnknownType::UnresolvedExternal { name }) => panic!(
            "namespace-qualified relative import `m.Local` survived as \
             UnresolvedExternal({name:?})"
        ),
        other => panic!("`f` field type must be Nominal(Intro), got {other:?}"),
    }
}

// ── T5: a sibling generic type parameter is visible in bound + default ──────

#[test]
fn ts_generic_param_bound_and_default_reference_sibling_type_param() {
    let dir = write_pkg(&[(
        "index.ts",
        "export function f<T, U extends T = T>(x: U): void {}\n",
    )]);
    let source = PackageSource::new(dir.path(), "refs-fixture", "1.0.0");
    let produced = produce(&TypescriptProducer::new(), &source, &lineage(), &Unlinked)
        .expect("the sibling-generic-bound TS fixture must lower");
    let table = produced.table;

    let func = find_function(&table, "f");
    let (bounds, default) = func
        .generics
        .iter()
        .find_map(|g| match g {
            nudox_ir::kinds::GenericParam::Type {
                name,
                bounds,
                default,
                ..
            } if name == "U" => Some((bounds.to_vec(), default.clone())),
            _ => None,
        })
        .expect("type parameter `U` must be present on function `f`");

    assert_eq!(
        bounds,
        vec![Type::TypeVar("T".to_string())],
        "`U extends T` must classify the bound `T` as a sibling TypeVar, not a Nominal/\
         UnresolvedExternal reference to an undeclared type named `T`; got {bounds:?}"
    );
    assert_eq!(
        default,
        Some(Type::TypeVar("T".to_string())),
        "`U extends T = T` must classify the default `T` as a sibling TypeVar too; got \
         {default:?}"
    );
}

// ── T6: `import("pkg").Thing` resolves to a named Foreign, not Unresolved ───

#[test]
fn ts_dynamic_import_type_resolves_to_named_foreign_key() {
    let dir = write_pkg(&[(
        "index.ts",
        "export interface Holder { f: import(\"external-dep\").Thing; }\n",
    )]);
    let source = PackageSource::new(dir.path(), "refs-fixture", "1.0.0");
    let foreign_target = StableRef::new(
        PackageLineageId::new(EcosystemId::new("npm"), PackageName::new("external-dep")),
        IntroId::from_raw([0x44; 32]),
    );
    let resolver = ResolveEveryForeign(foreign_target.clone());
    let produced = produce(&TypescriptProducer::new(), &source, &lineage(), &resolver)
        .expect("the dynamic-import-type TS fixture must lower");
    let table = produced.table;

    match field_ty(&table, "f") {
        Type::Nominal(Ref::Foreign { key, target }) => {
            assert!(
                key.path.contains("Thing") || key.display.contains("Thing"),
                "`import(\"external-dep\").Thing` must key the ForeignKey on `Thing`; path={:?} \
                 display={:?}",
                key.path,
                key.display,
            );
            assert_eq!(
                target,
                Some(foreign_target),
                "a correctly-keyed foreign ref must still link"
            );
        }
        Type::Unknown(UnknownType::UnresolvedExternal { name }) => panic!(
            "`import(\"external-dep\").Thing` survived as UnresolvedExternal({name:?}) — the \
             module specifier was discarded before it reached emit.rs"
        ),
        other => panic!("`f` field type must be Nominal(Foreign), got {other:?}"),
    }
}

// ── T7: `infer U` is a TypeVar in the conditional's then-branch ─────────────

#[test]
fn ts_infer_introduced_name_is_typevar_in_conditional_then_branch() {
    let dir = write_pkg(&[(
        "index.ts",
        "export type ElementOf<T> = T extends Array<infer U> ? U : never;\n",
    )]);
    let source = PackageSource::new(dir.path(), "refs-fixture", "1.0.0");
    let produced = produce(&TypescriptProducer::new(), &source, &lineage(), &Unlinked)
        .expect("the infer-conditional TS fixture must lower");
    let table = produced.table;

    let (_, entry) = table
        .iter()
        .find(|(_, e)| {
            e.sym().name == "ElementOf" && matches!(e.kind(), EntryInner::Owned(Kind::Alias(_)))
        })
        .expect("type alias `ElementOf` must be present as an Alias entry");
    let EntryInner::Owned(Kind::Alias(alias)) = entry.kind() else {
        unreachable!()
    };
    match alias
        .target
        .as_ref()
        .expect("`ElementOf`'s alias must carry a target type")
    {
        Type::Conditional { then_ty, .. } => assert_eq!(
            then_ty.as_ref(),
            &Type::TypeVar("U".to_string()),
            "the `infer U` binding introduced in the extends clause must make the \
             then-branch `U` a TypeVar, not a Nominal/UnresolvedExternal reference to an \
             undeclared type named `U`; got {then_ty:?}"
        ),
        other => panic!("`ElementOf`'s target must be a Conditional type, got {other:?}"),
    }
}

// ── T8: global lib generic types are named Foreign, not typo-shaped ─────────

#[test]
fn ts_global_lib_generic_base_is_foreign_not_unresolved() {
    let dir = write_pkg(&[(
        "index.ts",
        "export interface Holder { items: Array<string>; }\n",
    )]);
    let source = PackageSource::new(dir.path(), "refs-fixture", "1.0.0");
    let produced = produce(&TypescriptProducer::new(), &source, &lineage(), &Unlinked)
        .expect("the global-lib-type TS fixture must lower");
    let table = produced.table;

    match field_ty(&table, "items") {
        Type::Apply { base, .. } => match base.as_ref() {
            Type::Nominal(Ref::Foreign { key, .. }) => {
                assert!(
                    key.display.as_ref() == "Array" || key.path.contains("Array"),
                    "the ts-stdlib `Array` base must carry a named ForeignKey; path={:?} \
                     display={:?}",
                    key.path,
                    key.display,
                );
            }
            Type::Unknown(UnknownType::UnresolvedExternal { name }) => panic!(
                "global lib type `Array` survived as UnresolvedExternal({name:?}) — no \
                 ts-stdlib tier exists"
            ),
            other => panic!("`items: Array<string>`'s Apply base must be Nominal(Foreign), got {other:?}"),
        },
        other => panic!("`items: Array<string>` must lower to Type::Apply, got {other:?}"),
    }
}

// ── T9: a locally-declared namespace member resolves via its qualified name ─

#[test]
fn ts_local_namespace_qualified_name_resolves_to_nested_intro() {
    let dir = write_pkg(&[(
        "index.ts",
        "export namespace NS { export interface Foo { id: number; } }\n\
         export interface Holder { f: NS.Foo; }\n",
    )]);
    let source = PackageSource::new(dir.path(), "refs-fixture", "1.0.0");
    let produced = produce(&TypescriptProducer::new(), &source, &lineage(), &Unlinked)
        .expect("the local-namespace-qualified TS fixture must lower");
    let table = produced.table;

    let foo_id: IntroId = table
        .iter()
        .find(|(_, e)| {
            e.sym().name == "Foo" && matches!(e.kind(), EntryInner::Owned(Kind::Trait(_)))
        })
        .map(|(id, _)| id)
        .expect("the namespace-nested `Foo` interface declaration must be present");

    match field_ty(&table, "f") {
        Type::Nominal(Ref::Intro(id)) => assert_eq!(
            id, foo_id,
            "`f: NS.Foo` must resolve to the namespace-nested `Foo` declaration"
        ),
        Type::Unknown(UnknownType::UnresolvedExternal { name }) => panic!(
            "local namespace-qualified type `NS.Foo` survived as UnresolvedExternal({name:?}) \
             — the dotted name was never resolved against the namespace's own members"
        ),
        other => panic!("`f` field type must be Nominal(Intro), got {other:?}"),
    }
}

// ── T10: qualified heritage (`extends ns.Base`) is a named Foreign super ────

#[test]
fn ts_qualified_heritage_super_is_named_foreign_ref() {
    let dir = write_pkg(&[(
        "index.ts",
        "import * as ns from \"external-dep\";\n\
         export class C extends ns.Base {}\n",
    )]);
    let source = PackageSource::new(dir.path(), "refs-fixture", "1.0.0");
    let foreign_target = StableRef::new(
        PackageLineageId::new(EcosystemId::new("npm"), PackageName::new("external-dep")),
        IntroId::from_raw([0x55; 32]),
    );
    let resolver = ResolveEveryForeign(foreign_target.clone());
    let produced = produce(&TypescriptProducer::new(), &source, &lineage(), &resolver)
        .expect("the qualified-heritage TS fixture must lower");
    let table = produced.table;

    let (_, entry) = table
        .iter()
        .find(|(_, e)| {
            e.sym().name == "C" && matches!(e.kind(), EntryInner::Owned(Kind::Record(_)))
        })
        .expect("class `C` must be present as a Record entry");
    let EntryInner::Owned(Kind::Record(rec)) = entry.kind() else {
        unreachable!()
    };
    let super_ty = rec
        .super_types
        .first()
        .expect("`C extends ns.Base` must carry one super type");

    match super_ty {
        Type::Nominal(Ref::Foreign { key, target }) => {
            assert!(
                key.path.contains("Base") || key.display.contains("Base"),
                "`extends ns.Base` must key the super type's ForeignKey on `Base`; path={:?} \
                 display={:?}",
                key.path,
                key.display,
            );
            assert_eq!(
                *target,
                Some(foreign_target),
                "a correctly-keyed foreign super ref must still link"
            );
        }
        other => panic!("`C`'s super type must be Nominal(Foreign), got {other:?}"),
    }
}
