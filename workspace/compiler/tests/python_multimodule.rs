//! Pipeline part: **whole Python package → surface IR** via the Pyrefly oracle
//! (`compiler::languages::python::context::PythonContext::lower_package`).
//!
//! Companion to `python_oracle.rs` (single snippet) and `python_classes.rs`.
//! This drives the *multi-module* path: a real package laid out on disk is
//! discovered, all of its modules are loaded into ONE committable transaction
//! and checked together, and the merged `Index` is asserted to:
//!
//!   1. contain entries from BOTH modules (`pkg.a`'s class + `pkg.b`'s fn);
//!   2. have its `Entry::Module`s' `members` populated from the children;
//!   3. carry a CROSS-MODULE reference that resolved — `pkg.b.make_foo`'s
//!      return type is `Foo`, which is only knowable if the import
//!      `from pkg.a import Foo` resolved across modules during checking.
//!
//! If the modules were checked independently (or the inference transaction were
//! thrown away before lowering), the cross-module return type would collapse to
//! `Any` and assertion (3) would fail.

use std::fs;
use std::path::Path;

use compiler::languages::python::context::PythonContext;
use ir::entry::{Index, NudoxPath};
use ir::kind::Entry;
use ir::parameter::Parameter;
use ir::ty::Type;

/// Lay down a small package under `root`:
///   pkg/__init__.py   (empty)
///   pkg/a.py          (class Foo)
///   pkg/b.py          (imports Foo from pkg.a; defines make_foo() -> Foo)
fn write_fixture(root: &Path) {
    let pkg = root.join("pkg");
    fs::create_dir_all(&pkg).unwrap();
    fs::write(pkg.join("__init__.py"), "").unwrap();
    fs::write(
        pkg.join("a.py"),
        "class Foo:\n    value: int\n\n    def get(self) -> int:\n        return self.value\n",
    )
    .unwrap();
    fs::write(
        pkg.join("b.py"),
        "from pkg.a import Foo\n\n\ndef make_foo() -> Foo:\n    return Foo()\n",
    )
    .unwrap();
}

fn dump(index: &Index) -> String {
    let mut v: Vec<String> = index
        .entries_by_path
        .values()
        .map(|e| format!("{}:{}", e.kind_tag(), e.name()))
        .collect();
    v.sort();
    v.join(", ")
}

fn find_function<'a>(index: &'a Index, name: &str) -> &'a ir::function::Function {
    index
        .entries_by_path
        .values()
        .find_map(|e| match e {
            Entry::Function(sym) if sym.name == name => Some(&sym.inner),
            _ => None,
        })
        .unwrap_or_else(|| panic!("function `{name}` not found; entries: {}", dump(index)))
}

fn module_entry<'a>(index: &'a Index, qualname: &str) -> &'a ir::kind::Symbol<ir::module::Module> {
    index
        .entries_by_path
        .values()
        .find_map(|e| match e {
            Entry::Module(sym) if sym.name == qualname => Some(sym),
            _ => None,
        })
        .unwrap_or_else(|| panic!("module `{qualname}` not found; entries: {}", dump(index)))
}

fn param_type(p: &Parameter) -> &Type {
    match p {
        Parameter::Literal(lp) => lp.r#type.as_ref().expect("parameter lowered without a type"),
        other => panic!("unexpected non-literal parameter: {other:?}"),
    }
}

#[test]
fn lower_package_merges_modules_wires_members_and_resolves_cross_module_ref() {
    let tmp = tempfile::tempdir().expect("create temp dir");
    let root = tmp.path();
    write_fixture(root);

    let ctx = PythonContext::new();
    let index = ctx.lower_package(root);
    println!("entries: {}", dump(&index));

    // --- (1) entries from BOTH modules are present ----------------------
    let foo = index
        .entries_by_path
        .values()
        .find_map(|e| match e {
            Entry::RecordType(sym) if sym.name == "Foo" => Some(sym),
            _ => None,
        })
        .unwrap_or_else(|| panic!("RecordType `Foo` (from pkg.a) missing; entries: {}", dump(&index)));
    let make_foo = find_function(&index, "make_foo"); // from pkg.b

    // --- (2) module entries carry populated `members` -------------------
    let mod_a = module_entry(&index, "pkg.a");
    let members_a = mod_a
        .inner
        .members
        .as_ref()
        .expect("pkg.a module lowered with no members");
    assert!(
        members_a.contains(&foo.path),
        "pkg.a.members should include Foo's path; members: {members_a:?}, foo: {:?}",
        foo.path
    );
    println!("pkg.a members: {members_a:?}");

    let mod_b = module_entry(&index, "pkg.b");
    let members_b = mod_b
        .inner
        .members
        .as_ref()
        .expect("pkg.b module lowered with no members");
    assert!(
        members_b.contains(&make_foo_path(&index)),
        "pkg.b.members should include make_foo's path; members: {members_b:?}"
    );

    // --- (3) the package's top-level module is a root -------------------
    // `pkg` (from __init__.py) has no parent package among the discovered
    // modules, so it must appear in root_ids; `pkg.a`/`pkg.b` must not.
    let root_names: Vec<&str> = index
        .root_ids
        .iter()
        .filter_map(|id| index.entries_by_path.get(id))
        .map(|e| e.name())
        .collect();
    assert!(
        root_names.contains(&"pkg"),
        "expected `pkg` among root_ids, got: {root_names:?}"
    );
    assert!(
        !root_names.contains(&"pkg.a") && !root_names.contains(&"pkg.b"),
        "submodules must not be root_ids, got: {root_names:?}"
    );

    // --- (4) CROSS-MODULE reference resolved ----------------------------
    // make_foo's return type is `Foo`, defined in the *other* module pkg.a.
    let ret = make_foo
        .output_parameters
        .as_ref()
        .and_then(|v| v.first())
        .map(param_type)
        .expect("make_foo lowered with no return type");
    match ret {
        Type::TypeReference(r) => assert!(
            r.identifier.contains("Foo"),
            "make_foo return should reference cross-module `Foo`, got `{}`",
            r.identifier
        ),
        other => panic!(
            "make_foo return should be a resolved reference to Foo (cross-module), got: {other:?}"
        ),
    }
    println!("make_foo -> {ret:?}  (cross-module reference resolved)");
}

/// The storage path of `make_foo` (a `Local("pkg.b::make_foo")`).
fn make_foo_path(index: &Index) -> NudoxPath {
    index
        .entries_by_path
        .iter()
        .find_map(|(p, e)| match e {
            Entry::Function(sym) if sym.name == "make_foo" => Some(p.clone()),
            _ => None,
        })
        .expect("make_foo path")
}
