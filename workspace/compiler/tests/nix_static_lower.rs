//! Pipeline part: **lib-style Nix tree → surface IR** via the static layer
//! (`compiler::languages::nix::lower_package`).
//!
//! Drives the fully offline path against `tests/fixtures/nix/lib-style/`:
//!
//!   1. `mapAttrs` / `concatStringsSep` / `makeWrapper` lower as Functions
//!   2. RFC-145 / legacy `::` signatures parse into typed parameters
//!   3. Doc comments are recovered into markdown
//!   4. `attrsets.mapAttrs` appears as an inherit alias of `mapAttrs`

use std::path::{Path, PathBuf};

use compiler::languages::nix::lower_package;
use ir::entry::Index;
use ir::kind::Entry;
use ir::parameter::Parameter;
use ir::ty::Type;

fn fixture_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/nix/lib-style")
}

fn dump(index: &Index) -> String {
    let mut v: Vec<String> = index
        .entries_by_path
        .iter()
        .map(|(p, e)| format!("{}:{}", e.kind_tag(), path_display(p)))
        .collect();
    v.sort();
    v.join(", ")
}

fn path_display(path: &ir::entry::NudoxPath) -> String {
    match path {
        ir::entry::NudoxPath::Local(p) => p.display().to_string(),
        ir::entry::NudoxPath::External { path, dependency } => {
            format!("{dependency}:{}", path.display())
        }
    }
}

fn find_function<'a>(index: &'a Index, name: &str) -> &'a ir::kind::Symbol<ir::function::Function> {
    index
        .entries_by_path
        .values()
        .find_map(|e| match e {
            Entry::Function(sym) if sym.name == name => Some(sym),
            _ => None,
        })
        .unwrap_or_else(|| panic!("function `{name}` not found; entries: {}", dump(index)))
}

fn find_function_at<'a>(
    index: &'a Index,
    path_suffix: &str,
) -> &'a ir::kind::Symbol<ir::function::Function> {
    index
        .entries_by_path
        .iter()
        .find_map(|(p, e)| match e {
            Entry::Function(sym) if path_display(p).ends_with(path_suffix) => Some(sym),
            _ => None,
        })
        .unwrap_or_else(|| {
            panic!("function at `…{path_suffix}` not found; entries: {}", dump(index))
        })
}

fn param_type(p: &Parameter) -> Option<&Type> {
    match p {
        Parameter::Literal(lp) => lp.r#type.as_ref(),
        _ => None,
    }
}

fn has_function_pointer_type(ty: &Type) -> bool {
    matches!(ty, Type::FunctionPointer(_))
        || matches!(ty, Type::TypeReference(r) if r.identifier == "AttrSet" || r.identifier == "Derivation")
}

#[test]
fn lower_lib_style_functions_docs_sigs_and_inherit_alias() {
    let root = fixture_root();
    assert!(
        root.join("default.nix").is_file(),
        "fixture missing: {}",
        root.display()
    );

    let index = lower_package(Path::new(&root)).expect("lower_package must succeed statically");
    println!("entries: {}", dump(&index));

    // --- (1) core functions present ---------------------------------------
    let map_attrs = find_function(&index, "mapAttrs");
    let concat = find_function(&index, "concatStringsSep");
    let make_wrapper = find_function(&index, "makeWrapper");
    assert_eq!(map_attrs.name, "mapAttrs");
    assert_eq!(concat.name, "concatStringsSep");
    assert_eq!(make_wrapper.name, "makeWrapper");

    // --- (2) signatures attach typed parameters / return types ------------
    let map_params = map_attrs
        .inner
        .input_parameters
        .as_ref()
        .expect("mapAttrs should have formals");
    assert!(
        map_params.len() >= 2,
        "mapAttrs is curried `f: set:`; got {} params",
        map_params.len()
    );
    // First formal is `(String -> a -> b)` — a FunctionPointer.
    let f_ty = param_type(&map_params[0]).expect("mapAttrs.f should be typed from # Type");
    assert!(
        has_function_pointer_type(f_ty) || matches!(f_ty, Type::FunctionPointer(_)),
        "mapAttrs.f should be a function type, got {f_ty:?}"
    );
    let map_out = map_attrs
        .inner
        .output_parameters
        .as_ref()
        .and_then(|o| o.first())
        .and_then(param_type)
        .expect("mapAttrs should have a declared return type");
    assert!(
        matches!(map_out, Type::TypeReference(r) if r.identifier == "AttrSet"),
        "mapAttrs ret should be AttrSet, got {map_out:?}"
    );

    let concat_params = concat
        .inner
        .input_parameters
        .as_ref()
        .expect("concatStringsSep formals");
    assert!(concat_params.len() >= 2);
    let sep_ty = param_type(&concat_params[0]).expect("sep typed");
    assert!(
        matches!(sep_ty, Type::Primitive(ir::primitives::Primitive::String)),
        "sep should be String, got {sep_ty:?}"
    );
    let list_ty = param_type(&concat_params[1]).expect("strings typed");
    assert!(
        matches!(list_ty, Type::Slice(_)),
        "strings should be [String], got {list_ty:?}"
    );

    // makeWrapper: record-pattern formals get field types from the sig record.
    let mw_params = make_wrapper
        .inner
        .input_parameters
        .as_ref()
        .expect("makeWrapper formals");
    let name_param = mw_params
        .iter()
        .find_map(|p| match p {
            Parameter::Literal(lp) if lp.name == "name" => Some(lp),
            _ => None,
        })
        .expect("makeWrapper should have a `name` formal");
    assert!(
        matches!(
            name_param.r#type.as_ref(),
            Some(Type::Primitive(ir::primitives::Primitive::String))
        ),
        "makeWrapper.name should be String from record sig, got {:?}",
        name_param.r#type
    );
    let version_param = mw_params
        .iter()
        .find_map(|p| match p {
            Parameter::Literal(lp) if lp.name == "version" => Some(lp),
            _ => None,
        })
        .expect("makeWrapper should have a `version` formal");
    assert!(
        version_param.default_value.is_some(),
        "version should keep its `? \"0.0.0\"` default"
    );

    // --- (3) RFC-145 / legacy docs extracted ------------------------------
    let map_doc = map_attrs
        .documentation
        .as_deref()
        .expect("mapAttrs should carry RFC-145 body");
    assert!(
        map_doc.to_ascii_lowercase().contains("map a function")
            || map_doc.contains("attribute set"),
        "mapAttrs doc body missing description: {map_doc}"
    );

    let concat_doc = concat
        .documentation
        .as_deref()
        .expect("concatStringsSep should carry legacy block body");
    assert!(
        concat_doc.to_ascii_lowercase().contains("concatenate")
            || concat_doc.to_ascii_lowercase().contains("separator"),
        "concatStringsSep doc body missing description: {concat_doc}"
    );

    // --- (4) attrsets.mapAttrs inherit alias ------------------------------
    let alias = find_function_at(&index, "attrsets/mapAttrs");
    assert_eq!(alias.name, "mapAttrs");
    // Same signature surface as the canonical binding.
    assert!(
        alias.inner.input_parameters.is_some(),
        "inherit alias should still lower as a Function with formals"
    );
}
