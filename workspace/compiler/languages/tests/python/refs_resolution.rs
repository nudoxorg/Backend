//! RED tests: Python reference resolution — intra-package AND cross-package.
//!
//! Pins the desired END STATE. Today (`python/types.rs` `lower_nominal`):
//!   - case 3 emits `Type::Unknown(UnresolvedLocalName { name })` for a short
//!     name the package declares under a longer id — left dangling, never
//!     resolved to the local declaration.
//!   - case 4 emits `Type::Unknown(UnresolvedExternal { name })` for a
//!     genuinely external type, and Python builds NO `ForeignKey` anywhere, so
//!     a cross-package type can never be linked.
//!
//! Requirement:
//!   (a) a name the package itself declares resolves to a same-package
//!       `Ref::Intro` (no `UnresolvedLocalName` survives);
//!   (b) a type from another distribution is a *named* `Ref::Foreign`
//!       (a pypi-namespace `ForeignKey`), never a bare `UnresolvedExternal`.
//!
//! Hermetic: in-process pyrefly, offline; no installed deps.

use std::fs;

use nudox_ir::{
    change::{EcosystemId, IntroId, PackageLineageId, PackageName, StableRef},
    entry::EntryInner,
    foreign::{ForeignKey, ForeignOrigin, ForeignResolver, Resolution, Unlinked},
    index::Ref,
    kind::Kind,
    kinds::GenericParam,
    kinds::ty::{Type, UnknownType},
};
use nudox_languages::python::PythonProducer;
use nudox_languages::{PackageSource, produce};

#[derive(Clone)]
struct ResolveEveryForeign(StableRef);
impl ForeignResolver for ResolveEveryForeign {
    fn resolve(&self, _key: &ForeignKey) -> Resolution {
        Resolution::Resolved(self.0.clone())
    }
}

fn write_pkg(files: &[(&str, &str)]) -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("create python package tempdir");
    fs::write(
        dir.path().join("pyproject.toml"),
        "[project]\nname = \"refs-fixture\"\nversion = \"0.0.0\"\n",
    )
    .expect("write pyproject.toml");
    for (name, body) in files {
        let path = dir.path().join(name);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("create package subdir");
        }
        fs::write(path, body).expect("write source file");
    }
    dir
}

fn lineage() -> PackageLineageId {
    PackageLineageId::new(EcosystemId::new("pypi"), PackageName::new("refs-fixture"))
}

fn param_ty<'a>(
    table: &'a nudox_ir::apply::PristineIntroTable,
    param_name: &str,
) -> Type {
    let (_, entry) = table
        .iter()
        .find(|(_, e)| {
            e.sym().name == param_name
                && matches!(e.kind(), EntryInner::Owned(Kind::Param(_)))
        })
        .unwrap_or_else(|| panic!("param `{param_name}` must be present as a Param entry"));
    let EntryInner::Owned(Kind::Param(p)) = entry.kind() else {
        unreachable!()
    };
    p.ty.clone()
        .unwrap_or_else(|| panic!("param `{param_name}` must carry a type annotation"))
}

/// The first generic type parameter's first bound, as lowered onto a
/// [`nudox_ir::kinds::Function`] entry (`GenericParam::Type { bounds, .. }`)
/// — i.e. what a PEP 695 `def f[T: Bound](...)` clause produced.
fn function_generic_bound<'a>(
    table: &'a nudox_ir::apply::PristineIntroTable,
    function_name: &str,
) -> Type {
    let (_, entry) = table
        .iter()
        .find(|(_, e)| {
            e.sym().name == function_name
                && matches!(e.kind(), EntryInner::Owned(Kind::Function(_)))
        })
        .unwrap_or_else(|| panic!("function `{function_name}` must be present as a Function entry"));
    let EntryInner::Owned(Kind::Function(f)) = entry.kind() else {
        unreachable!()
    };
    let generic = f
        .generics
        .first()
        .unwrap_or_else(|| panic!("function `{function_name}` must declare a generic parameter"));
    match generic {
        GenericParam::Type { bounds, .. } => bounds
            .first()
            .cloned()
            .unwrap_or_else(|| panic!("function `{function_name}`'s generic param must carry a bound")),
        other => panic!("expected GenericParam::Type, got {other:?}"),
    }
}

// ── (a) intra-package name resolves to Ref::Intro (no UnresolvedLocalName) ──

#[test]
fn python_intra_package_name_resolves_to_intro() {
    let dir = write_pkg(&[
        ("refs_fixture/__init__.py", ""),
        ("refs_fixture/models.py", "class Widget:\n    pass\n"),
        (
            "refs_fixture/service.py",
            "from refs_fixture.models import Widget\n\n\ndef make(w: Widget) -> None:\n    pass\n",
        ),
    ]);
    let source = PackageSource::new(dir.path(), "refs-fixture", "0.0.0");
    let produced = produce(&PythonProducer, &source, &lineage(), &Unlinked)
        .expect("the intra-package Python fixture must lower");
    let table = produced.table;

    let widget_id: IntroId = table
        .iter()
        .find(|(_, e)| e.sym().name == "Widget")
        .map(|(id, _)| id)
        .expect("the `Widget` class must be present");

    // No dangling within-package name may survive.
    let mut local_names = Vec::new();
    for (_, entry) in table.iter() {
        entry.for_each_unknown(|u| {
            if let UnknownType::UnresolvedLocalName { name } = u {
                local_names.push(name.clone());
            }
        });
    }
    assert!(
        local_names.is_empty(),
        "a name the package itself declares must resolve to a same-package Ref::Intro, \
         but these survived as UnresolvedLocalName: {local_names:?}"
    );

    match param_ty(&table, "w") {
        Type::Nominal(Ref::Intro(id)) => assert_eq!(
            id, widget_id,
            "param `w: Widget` must resolve to the same-package Widget class"
        ),
        other => panic!(
            "param `w: Widget` must be a resolved same-package Nominal(Intro), got {other:?}"
        ),
    }
}

// ── (regression P1) builtins/stdlib must NOT become fabricated pypi Foreign ─

/// A builtin container/type (`dict`, `list`, `object`, `type`) is NOT a
/// third-party distribution. After the cross-package fix these must not fall
/// through into case 4 and mint a bogus `ForeignKey::in_namespace(pypi, ..)` —
/// that would claim `dict` is published by a pypi package named `dict`.
#[test]
fn python_builtins_are_not_fabricated_pypi_foreign() {
    let dir = write_pkg(&[
        ("refs_fixture/__init__.py", ""),
        (
            "refs_fixture/b.py",
            "def f(a: dict, b: list, c: object, d: type) -> None:\n    pass\n",
        ),
    ]);
    let source = PackageSource::new(dir.path(), "refs-fixture", "0.0.0");
    let produced = produce(&PythonProducer, &source, &lineage(), &Unlinked)
        .expect("the builtins fixture must lower");
    let table = produced.table;

    for name in ["a", "b", "c", "d"] {
        let ty = param_ty(&table, name);
        if let Type::Nominal(Ref::Foreign { key, .. }) = &ty {
            if let ForeignOrigin::Namespace { ecosystem, .. } = &key.origin {
                assert_ne!(
                    ecosystem.as_str(),
                    "pypi",
                    "builtin `{name}` lowered to a fabricated pypi Ref::Foreign ({:?}) — a \
                     language builtin is not a third-party distribution; it must be a \
                     Primitive or an in_universe key, never in_namespace(pypi)",
                    key.path
                );
            }
        }
    }
}

// ── (P13) stdlib is not third-party pypi, end to end through the real oracle ─

/// `pathlib.Path` and `os.PathLike`, as they actually come back from pyrefly
/// through the real in-process oracle (not a hand-built `TypeData`), must be
/// tagged `"python-stdlib"`, never `"pypi"` — stdlib ships with every CPython
/// install under no pypi project of that name.
#[test]
fn python_stdlib_types_are_not_fabricated_pypi_foreign() {
    let dir = write_pkg(&[
        ("refs_fixture/__init__.py", ""),
        (
            "refs_fixture/s.py",
            "import pathlib\nimport os\n\n\ndef f(a: pathlib.Path, b: os.PathLike) -> None:\n    pass\n",
        ),
    ]);
    let source = PackageSource::new(dir.path(), "refs-fixture", "0.0.0");
    let produced = produce(&PythonProducer, &source, &lineage(), &Unlinked)
        .expect("the stdlib fixture must lower");
    let table = produced.table;

    for name in ["a", "b"] {
        match param_ty(&table, name) {
            Type::Nominal(Ref::Foreign { key, .. }) => {
                assert!(
                    !matches!(&key.origin, ForeignOrigin::Namespace { ecosystem, .. } if ecosystem.as_str() == "pypi"),
                    "stdlib param `{name}` lowered to a fabricated pypi Ref::Foreign ({:?}) — \
                     the standard library is not a third-party distribution",
                    key.path
                );
            }
            other => panic!("param `{name}` must be a Nominal(Foreign), got {other:?}"),
        }
    }
}

// ── (P2/P6/P7) generic container bases must not be Foreign, args resolve ────

/// `dict[str, Widget]` where `Widget` is declared in this very package: the
/// builtin base must not be a fabricated pypi Foreign, and the arg `Widget`
/// must still resolve to the same-package `Ref::Intro` — the builtin-base
/// short-circuit in the `Apply` lowering must not skip arg resolution.
#[test]
fn python_builtin_generic_base_does_not_block_arg_resolution() {
    let dir = write_pkg(&[
        ("refs_fixture/__init__.py", ""),
        ("refs_fixture/models.py", "class Widget:\n    pass\n"),
        (
            "refs_fixture/service.py",
            "from refs_fixture.models import Widget\n\n\ndef make(w: dict[str, Widget]) -> None:\n    pass\n",
        ),
    ]);
    let source = PackageSource::new(dir.path(), "refs-fixture", "0.0.0");
    let produced = produce(&PythonProducer, &source, &lineage(), &Unlinked)
        .expect("the builtin-generic fixture must lower");
    let table = produced.table;

    let widget_id: IntroId = table
        .iter()
        .find(|(_, e)| e.sym().name == "Widget")
        .map(|(id, _)| id)
        .expect("the `Widget` class must be present");

    match param_ty(&table, "w") {
        Type::Apply { base, args } => {
            if let Type::Nominal(Ref::Foreign { key, .. }) = base.as_ref() {
                assert!(
                    !matches!(&key.origin, ForeignOrigin::Namespace { ecosystem, .. } if ecosystem.as_str() == "pypi"),
                    "dict's base must not be a fabricated pypi Ref::Foreign, got {:?}",
                    key.origin
                );
            }
            assert_eq!(args.len(), 2, "dict[str, Widget] must carry 2 type args");
            match &args[1] {
                Type::Nominal(Ref::Intro(id)) => assert_eq!(
                    *id, widget_id,
                    "the `Widget` arg must resolve to the same-package Widget class"
                ),
                other => panic!(
                    "dict's second arg `Widget` must be a resolved same-package \
                     Nominal(Intro), got {other:?}"
                ),
            }
        }
        other => panic!("param `w: dict[str, Widget]` must lower to Type::Apply, got {other:?}"),
    }
}

// ── (b) cross-distribution type is a named Ref::Foreign (pypi key) ──────────

#[test]
fn python_cross_package_type_is_named_foreign_key() {
    let dir = write_pkg(&[
        ("refs_fixture/__init__.py", ""),
        (
            "refs_fixture/client.py",
            "import acme\n\n\ndef use(x: acme.Gadget) -> None:\n    pass\n",
        ),
    ]);
    let source = PackageSource::new(dir.path(), "refs-fixture", "0.0.0");

    let foreign_target = StableRef::new(
        PackageLineageId::new(EcosystemId::new("pypi"), PackageName::new("acme")),
        IntroId::from_raw([0x5a; 32]),
    );
    let resolver = ResolveEveryForeign(foreign_target.clone());
    let produced = produce(&PythonProducer, &source, &lineage(), &resolver)
        .expect("the cross-package Python fixture must lower");
    let table = produced.table;

    match param_ty(&table, "x") {
        Type::Nominal(Ref::Foreign { key, target }) => {
            assert!(
                matches!(
                    key.origin,
                    ForeignOrigin::Namespace { .. } | ForeignOrigin::Package(_)
                ),
                "a cross-distribution pypi type must carry a placeable ForeignKey origin, got {:?}",
                key.origin
            );
            assert!(
                key.path.contains("Gadget")
                    || key.path.contains("acme")
                    || key.display.contains("Gadget"),
                "the ForeignKey must name the external type; path={:?} display={:?}",
                key.path,
                key.display
            );
            assert_eq!(
                target, Some(foreign_target),
                "with a populated resolver the foreign type must LINK — proving the key is \
                 join-ready, not a display string"
            );
        }
        Type::Unknown(UnknownType::UnresolvedExternal { name }) => panic!(
            "cross-distribution type `acme.Gadget` must be a named Ref::Foreign (pypi \
             ForeignKey), but it survived as UnresolvedExternal({name:?}) — Python builds no \
             ForeignKey, so the corpus pass can never link it"
        ),
        other => panic!("param `x` type must be a Nominal(Foreign), got {other:?}"),
    }
}

// ── (P3) root re-export spelling must not become a self-referential Foreign ─

/// A package declares `refs_fixture.core.Context` and re-exports it at the
/// package root (`refs_fixture/__init__.py` does `from .core import
/// Context`), so external — and, as here, same-package — code can spell it
/// `refs_fixture.Context`. That spelling is qualified with the package's OWN
/// top-level module name, not a foreign namespace: `lower_nominal`'s case 4
/// used to mint a `Ref::Foreign` whose namespace is the package's own name,
/// which is nonsense (a package cannot import from itself). It must resolve
/// within-package — `Ref::Intro` if the exact declaration can be picked,
/// `UnresolvedLocalName` otherwise — never a `pypi` `Ref::Foreign` rooted at
/// `refs_fixture`.
#[test]
fn python_root_reexport_spelling_is_not_self_referential_foreign() {
    let dir = write_pkg(&[
        (
            "refs_fixture/__init__.py",
            "from refs_fixture.core import Context\n",
        ),
        ("refs_fixture/core.py", "class Context:\n    pass\n"),
        (
            "refs_fixture/client.py",
            "import refs_fixture\n\n\ndef use(x: refs_fixture.Context) -> None:\n    pass\n",
        ),
    ]);
    let source = PackageSource::new(dir.path(), "refs-fixture", "0.0.0");
    let produced = produce(&PythonProducer, &source, &lineage(), &Unlinked)
        .expect("the root re-export fixture must lower");
    let table = produced.table;

    match param_ty(&table, "x") {
        Type::Nominal(Ref::Foreign { key, .. }) => {
            let self_referential = matches!(
                &key.origin,
                ForeignOrigin::Namespace { ecosystem, namespace }
                    if ecosystem.as_str() == "pypi" && &**namespace == "refs_fixture"
            );
            assert!(
                !self_referential,
                "param `x: refs_fixture.Context` lowered to a self-referential pypi \
                 Ref::Foreign whose namespace is the package's own name ({:?}) — a package \
                 cannot import from itself",
                key
            );
        }
        Type::Nominal(Ref::Intro(_)) => {} // ideal outcome
        Type::Unknown(UnknownType::UnresolvedLocalName { .. }) => {} // honest fallback
        other => panic!(
            "param `x: refs_fixture.Context` must resolve within-package (Intro or \
             UnresolvedLocalName) or at least not a self-referential Foreign, got {other:?}"
        ),
    }
}

// ── (P4) aliased dotted import keys on the alias, not the distribution ──────

/// `import numpy as np` then `def f(x: np.ndarray)`: the durable
/// cross-package join key must be the real distribution spelling
/// (`numpy.ndarray`), never the local alias spelling (`np.ndarray`) — a
/// resolver keyed by real package names can never find a `np` distribution.
#[test]
fn python_aliased_module_import_keys_on_real_distribution_name() {
    let dir = write_pkg(&[
        ("refs_fixture/__init__.py", ""),
        (
            "refs_fixture/arrays.py",
            "import numpy as np\n\n\ndef f(x: np.ndarray) -> None:\n    pass\n",
        ),
    ]);
    let source = PackageSource::new(dir.path(), "refs-fixture", "0.0.0");
    let produced = produce(&PythonProducer, &source, &lineage(), &Unlinked)
        .expect("the aliased-module fixture must lower");
    let table = produced.table;

    match param_ty(&table, "x") {
        Type::Nominal(Ref::Foreign { key, .. }) => {
            assert_eq!(
                &*key.path, "numpy.ndarray",
                "param `x: np.ndarray` must key on the real distribution spelling \
                 `numpy.ndarray`, not the local alias `np.ndarray`; got path={:?}",
                key.path
            );
        }
        other => panic!("param `x: np.ndarray` must be a Nominal(Foreign), got {other:?}"),
    }
}

// ── (P12) relative-import alias as a type annotation resolves in-package ────

/// `from .core import Context as Ctx` then `def f(x: Ctx)`: `Ctx` must
/// resolve within-package to `refs_fixture.core.Context` — `Ref::Intro` or
/// `UnresolvedLocalName` — never a pypi `Ref::Foreign` naming a distribution
/// called `Ctx` (there is no such distribution; `Ctx` is a purely local
/// binding introduced by the `as` clause).
#[test]
fn python_relative_import_alias_resolves_in_package() {
    let dir = write_pkg(&[
        ("refs_fixture/__init__.py", ""),
        ("refs_fixture/core.py", "class Context:\n    pass\n"),
        (
            "refs_fixture/client.py",
            "from .core import Context as Ctx\n\n\ndef f(x: Ctx) -> None:\n    pass\n",
        ),
    ]);
    let source = PackageSource::new(dir.path(), "refs-fixture", "0.0.0");
    let produced = produce(&PythonProducer, &source, &lineage(), &Unlinked)
        .expect("the relative-import-alias fixture must lower");
    let table = produced.table;

    let context_id: IntroId = table
        .iter()
        .find(|(_, e)| e.sym().name == "Context")
        .map(|(id, _)| id)
        .expect("the `Context` class must be present");

    match param_ty(&table, "x") {
        Type::Nominal(Ref::Intro(id)) => assert_eq!(
            id, context_id,
            "param `x: Ctx` must resolve to the same-package Context class"
        ),
        Type::Unknown(UnknownType::UnresolvedLocalName { name }) => assert_eq!(
            name, "Context",
            "param `x: Ctx` fell back to UnresolvedLocalName, but not for the aliased \
             target's real name — got {name:?}"
        ),
        Type::Nominal(Ref::Foreign { key, .. }) => panic!(
            "param `x: Ctx` lowered to a pypi Ref::Foreign ({:?}) — `Ctx` is a local alias \
             for a same-package class, not a third-party distribution named `Ctx`",
            key
        ),
        other => panic!(
            "param `x: Ctx` must resolve within-package (Intro or UnresolvedLocalName), \
             got {other:?}"
        ),
    }
}

// ── (P14) PEP 695 generic bound/default must also be alias-rewritten ───────

/// `import acme as ac` then `def f[T: ac.Base](x: T) -> T`: the TypeVar
/// bound is a type position exactly like a param/return/field annotation,
/// but PEP 695 `[T: ...]` clauses are extracted through a separate code
/// path (`pep695_generics`) than `expr_to_type`'s other call sites. Before
/// this fix that path never ran the bound through `rewrite_aliases`, so the
/// bound stayed keyed on the local alias `ac.Base` — a spelling no
/// distribution actually publishes — instead of the real distribution name
/// `acme.Base`.
#[test]
fn python_pep695_bound_aliased_cross_package_keys_on_real_distribution_name() {
    let dir = write_pkg(&[
        ("refs_fixture/__init__.py", ""),
        (
            "refs_fixture/client.py",
            "import acme as ac\n\n\ndef f[T: ac.Base](x: T) -> T:\n    return x\n",
        ),
    ]);
    let source = PackageSource::new(dir.path(), "refs-fixture", "0.0.0");

    let foreign_target = StableRef::new(
        PackageLineageId::new(EcosystemId::new("pypi"), PackageName::new("acme")),
        IntroId::from_raw([0x5a; 32]),
    );
    let resolver = ResolveEveryForeign(foreign_target.clone());
    let produced = produce(&PythonProducer, &source, &lineage(), &resolver)
        .expect("the PEP 695 aliased-bound fixture must lower");
    let table = produced.table;

    match function_generic_bound(&table, "f") {
        Type::Nominal(Ref::Foreign { key, target }) => {
            assert!(
                !key.path.contains(".ac.") && !key.path.starts_with("ac."),
                "TypeVar bound `T: ac.Base` must key on the real distribution spelling \
                 (`acme.Base`), not the local alias `ac`; got path={:?}",
                key.path
            );
            assert!(
                key.path.contains("Base") || key.display.contains("Base"),
                "the ForeignKey must still name the bound type; path={:?} display={:?}",
                key.path,
                key.display
            );
            assert_eq!(
                target,
                Some(foreign_target),
                "with a populated resolver the bound's foreign type must LINK — proving the \
                 key is join-ready, not a display string"
            );
        }
        Type::Unknown(UnknownType::UnresolvedExternal { name }) => panic!(
            "TypeVar bound `T: ac.Base` must be a named Ref::Foreign (pypi ForeignKey), but \
             it survived as UnresolvedExternal({name:?}) — the alias was never rewritten \
             before lowering"
        ),
        other => panic!(
            "TypeVar bound `T: ac.Base` must be a resolved Nominal(Foreign), got {other:?}"
        ),
    }
}

/// `from .core import Context as Ctx` then `def f[T: Ctx](x: T) -> T`: `Ctx`
/// is a purely local, within-package alias — the bound must resolve
/// in-package (`Ref::Intro` or, failing exact resolution,
/// `UnresolvedLocalName` naming the real class), never a pypi `Ref::Foreign`
/// keyed on a distribution called `Ctx` (no such distribution exists).
#[test]
fn python_pep695_bound_relative_import_alias_resolves_in_package() {
    let dir = write_pkg(&[
        ("refs_fixture/__init__.py", ""),
        ("refs_fixture/core.py", "class Context:\n    pass\n"),
        (
            "refs_fixture/client.py",
            "from .core import Context as Ctx\n\n\ndef f[T: Ctx](x: T) -> T:\n    return x\n",
        ),
    ]);
    let source = PackageSource::new(dir.path(), "refs-fixture", "0.0.0");
    let produced = produce(&PythonProducer, &source, &lineage(), &Unlinked)
        .expect("the PEP 695 relative-import-alias bound fixture must lower");
    let table = produced.table;

    let context_id: IntroId = table
        .iter()
        .find(|(_, e)| e.sym().name == "Context")
        .map(|(id, _)| id)
        .expect("the `Context` class must be present");

    match function_generic_bound(&table, "f") {
        Type::Nominal(Ref::Intro(id)) => assert_eq!(
            id, context_id,
            "TypeVar bound `T: Ctx` must resolve to the same-package Context class"
        ),
        Type::Unknown(UnknownType::UnresolvedLocalName { name }) => assert_eq!(
            name, "Context",
            "TypeVar bound `T: Ctx` fell back to UnresolvedLocalName, but not for the \
             aliased target's real name — got {name:?}"
        ),
        Type::Nominal(Ref::Foreign { key, .. }) => panic!(
            "TypeVar bound `T: Ctx` lowered to a pypi Ref::Foreign ({:?}) — `Ctx` is a local \
             alias for a same-package class, not a third-party distribution named `Ctx`",
            key
        ),
        other => panic!(
            "TypeVar bound `T: Ctx` must resolve within-package (Intro or \
             UnresolvedLocalName), got {other:?}"
        ),
    }
}
