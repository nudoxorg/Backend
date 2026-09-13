//! Verifies that the Python checker carries package context into its bounded
//! transaction and reports pyrefly import resolution rather than a local
//! spelling heuristic.

use backend_compile::PythonVersion;
use backend_frontend_python::{Pyrefly, extract};
use std::path::{Path, PathBuf};

fn temp_root() -> PathBuf {
    std::env::temp_dir().join(format!(
        "nudox-pyrefly-package-test-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |duration| duration.as_nanos())
    ))
}

#[test]
fn package_imports_are_checked_in_the_staged_tree() {
    let root = temp_root();
    let package = root.join("pkg");
    std::fs::create_dir_all(&package).expect("package directory");
    std::fs::write(package.join("__init__.py"), b"from .helper import VALUE\n")
        .expect("package init");
    std::fs::write(package.join("helper.py"), b"VALUE = 42\n").expect("helper module");
    let source = b"import pkg.helper as helper\n\nvalue = helper.VALUE\n";
    let facts = extract(source, PythonVersion::Python314).expect("Ruff facts");
    let checker = Pyrefly::uvx();
    if !checker.is_available() {
        let _ = std::fs::remove_dir_all(&root);
        return;
    }
    let result = checker.analyze_in_package(
        source,
        PythonVersion::Python314,
        &facts,
        &root,
        Path::new("pkg/main.py"),
    );
    let _ = std::fs::remove_dir_all(&root);
    let report = match result {
        Ok(report) => report,
        Err(error) => panic!("staged pyrefly package authority failed: {error}"),
    };
    let import = report
        .imports
        .iter()
        .find(|import| import.binding == "helper")
        .expect("helper import row");
    assert!(import.resolved, "pyrefly did not resolve pkg.helper");
}
