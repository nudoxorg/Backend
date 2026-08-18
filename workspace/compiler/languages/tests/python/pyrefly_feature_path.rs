//! Feature-path regression test for the in-process pyrefly enrichment tier.
#![cfg(feature = "pyrefly")]

use std::path::Path;

use nudox_languages::PackageSource;
use nudox_languages::python::context;
use nudox_languages::python::oracle::{ItemBody, PythonOracle, TypeData};

fn return_type(oracle: &PythonOracle, id: &str) -> Option<TypeData> {
    oracle.modules.iter().flat_map(|m| &m.items).find_map(|item| {
        (item.id.0 == id).then(|| match &item.body {
            ItemBody::Function(function) => function.return_ty.clone(),
            _ => None,
        })?
    })
}

#[test]
fn pyrefly_fills_an_unannotated_return_in_a_real_package() {
    let dir = tempfile::tempdir().expect("create temporary package");
    let package = dir.path().join("feature_fixture");
    std::fs::create_dir(&package).expect("create package directory");
    std::fs::write(
        package.join("__init__.py"),
        "class Token:\n    pass\n\n\
         def make_token():\n    return Token()\n",
    )
    .expect("write Python fixture");

    let source = PackageSource::new(&package, "feature_fixture", "0.1.0");
    let oracle = context::invoke_oracle(&source).expect("pyrefly feature path");
    let inferred = return_type(&oracle, "feature_fixture.make_token")
        .expect("unannotated function must receive an inferred return type");

    assert!(
        matches!(inferred, TypeData::Nominal(ref name) if name.ends_with("Token")),
        "expected Token inference, got {inferred:?}"
    );
    assert!(
        Path::new(&oracle.modules[0].source).exists(),
        "the enriched oracle must retain source provenance"
    );
}
