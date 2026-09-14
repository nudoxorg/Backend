//! Verifies that the Python checker carries package context into its bounded
//! transaction and reports pyrefly import resolution rather than a local
//! spelling heuristic.
//!
//! The test requires the pyrefly authority. An absent tool is a typed
//! terminal, never a silent pass, so the suite cannot report green without
//! its toolchain.

use backend_compile::PythonVersion;
use backend_frontend_python::{CheckerError, Pyrefly, extract};
use std::path::{Path, PathBuf};

#[derive(Debug, thiserror::Error)]
enum PyreflyPackageError {
    #[error("pyrefly authority is unavailable: the uvx/pyrefly program did not resolve")]
    Unavailable,
    #[error("staged pyrefly package authority failed: {0}")]
    Checker(#[from] CheckerError),
}

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
fn package_imports_are_checked_in_the_staged_tree() -> Result<(), PyreflyPackageError> {
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
        return Err(PyreflyPackageError::Unavailable);
    }
    let result = checker.analyze_in_package(
        source,
        PythonVersion::Python314,
        &facts,
        &root,
        Path::new("pkg/main.py"),
    );
    let _ = std::fs::remove_dir_all(&root);
    let report = result?;
    let import = report
        .imports
        .iter()
        .find(|import| import.binding == "helper")
        .expect("helper import row");
    assert!(import.resolved, "pyrefly did not resolve pkg.helper");
    Ok(())
}

/// The absent-toolchain terminal is itself observable: a checker bound to a
/// nonexistent absolute program must return a typed spawn terminal rather
/// than a silent pass.
#[test]
fn absent_pyrefly_program_is_a_typed_spawn_terminal() -> Result<(), PyreflyPackageError> {
    let checker = Pyrefly::from_executable(PathBuf::from("/no/such/nudox-pyrefly"))
        .expect("absolute program path is admissible");
    let root = temp_root();
    std::fs::create_dir_all(&root).expect("package directory");
    let source = b"value = 1\n";
    let facts = extract(source, PythonVersion::Python314).expect("Ruff facts");
    let error = checker
        .analyze_in_package(
            source,
            PythonVersion::Python314,
            &facts,
            &root,
            Path::new("main.py"),
        )
        .expect_err("a nonexistent pyrefly program must not pass");
    let _ = std::fs::remove_dir_all(&root);
    assert!(
        matches!(
            error,
            CheckerError::Spawn { .. } | CheckerError::Exit { .. }
        ),
        "expected a typed spawn/exit terminal, got {error:?}"
    );
    Ok(())
}
