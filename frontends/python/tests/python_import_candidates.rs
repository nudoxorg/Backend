//! Real native authority controls for candidate shape versus node membership.

use std::sync::atomic::AtomicBool;
use std::time::{Duration, Instant};

use backend_frontend_python::legacy::checker::{
    CheckerError, InferenceSite, InferredType, NativePythonProjectAuthority, PythonProjectControl,
    PythonProjectSource, PythonProjectSourceStatus,
};
use backend_semantic::vocabulary::PythonVersion;

fn control(cancelled: &AtomicBool) -> PythonProjectControl<'_> {
    PythonProjectControl {
        cancelled,
        deadline: Instant::now() + Duration::from_secs(30),
    }
}

#[test]
fn excluded_uninitialized_venv_is_membership_not_missing_python_source() {
    let root = std::env::temp_dir().join(format!("nudox-venv-candidate-{}", std::process::id()));
    std::fs::create_dir_all(root.join("venv/lib")).expect("excluded environment directory");
    let source =
        "import venv\nimport nudox_external_absent\n\ndef local_answer():\n    return 42\n";
    std::fs::write(root.join("core.py"), source).unwrap();
    let sources = [PythonProjectSource {
        relative_path: "core.py",
        source,
    }];
    let checker = NativePythonProjectAuthority::admit().unwrap();
    let cancelled = AtomicBool::new(false);
    let report = checker
        .analyze_project(
            &root,
            "pkg",
            &sources,
            PythonVersion::Python314,
            control(&cancelled),
        )
        .expect("an excluded namespace directory must not require Python source bytes");
    assert_eq!(
        report.source_status("core.py"),
        Some(&PythonProjectSourceStatus::Analyzed)
    );
    let module = report.module("core.py").unwrap();
    assert!(
        module
            .imports
            .iter()
            .any(|import| import.module == "venv" && import.resolved),
        "the native bundled standard-library module supplies import authority"
    );
    assert!(
        module
            .imports
            .iter()
            .any(|import| import.module == "nudox_external_absent" && !import.resolved)
    );
    assert!(
        module.inferences.iter().any(
            |fact| fact.kind == InferenceSite::Return && fact.observed == InferredType::Integer
        )
    );
    assert!(
        report
            .coverage_gaps()
            .iter()
            .any(|gap| gap.relative_path.as_ref() == "core.py")
    );
    report
        .witness()
        .validate_current(control(&cancelled))
        .unwrap();

    // An ignored descendant is not read as semantic authority or membership.
    std::fs::create_dir_all(root.join("venv/lib/opaque")).unwrap();
    std::fs::write(
        root.join("venv/lib/opaque/unselected.py"),
        "unselected = 42\n",
    )
    .unwrap();
    report
        .witness()
        .validate_current(control(&cancelled))
        .unwrap();

    // An actual root initializer is a source candidate, even under the ignored
    // directory. It cannot gain the bundled stdlib's authority through omission.
    let initializer = root.join("venv/__init__.py");
    std::fs::write(&initializer, "class LocalShadow:\n    pass\n").unwrap();
    assert!(
        report
            .witness()
            .validate_current(control(&cancelled))
            .is_err()
    );
    assert!(
        matches!(checker.analyze_project(&root, "pkg", &sources, PythonVersion::Python314, control(&cancelled)),
        Err(CheckerError::IncompleteSourceFrontier { candidate, .. }) if candidate == initializer)
    );
    std::fs::remove_file(initializer).unwrap();

    cancelled.store(true, std::sync::atomic::Ordering::Release);
    assert!(matches!(
        checker.analyze_project(
            &root,
            "pkg",
            &sources,
            PythonVersion::Python314,
            control(&cancelled)
        ),
        Err(CheckerError::Cancelled { phase: "project" })
    ));
    cancelled.store(false, std::sync::atomic::Ordering::Release);
    let expired = PythonProjectControl {
        cancelled: &cancelled,
        deadline: Instant::now(),
    };
    assert!(matches!(
        checker.analyze_project(&root, "pkg", &sources, PythonVersion::Python314, expired),
        Err(CheckerError::Deadline { phase: "project" })
    ));
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn namespace_directory_does_not_relax_present_python_leaf_capture() {
    let root = std::env::temp_dir().join(format!("nudox-source-candidate-{}", std::process::id()));
    std::fs::create_dir_all(root.join("namespace")).unwrap();
    let source = "import namespace\nimport omitted\n\ndef known():\n    return 'local'\n";
    std::fs::write(root.join("core.py"), source).unwrap();
    let sources = [PythonProjectSource {
        relative_path: "core.py",
        source,
    }];
    let checker = NativePythonProjectAuthority::admit().unwrap();
    let cancelled = AtomicBool::new(false);
    let report = checker
        .analyze_project(
            &root,
            "pkg",
            &sources,
            PythonVersion::Python314,
            control(&cancelled),
        )
        .unwrap();
    assert_eq!(
        report.source_status("core.py"),
        Some(&PythonProjectSourceStatus::Analyzed)
    );
    for leaf in ["omitted.py", "omitted.pyi"] {
        let path = root.join(leaf);
        std::fs::write(&path, "value: int = 1\n").unwrap();
        assert!(
            report
                .witness()
                .validate_current(control(&cancelled))
                .is_err()
        );
        assert!(
            matches!(checker.analyze_project(&root, "pkg", &sources, PythonVersion::Python314, control(&cancelled)),
            Err(CheckerError::IncompleteSourceFrontier { candidate, .. }) if candidate == path)
        );
        std::fs::remove_file(path).unwrap();
    }
    report
        .witness()
        .validate_current(control(&cancelled))
        .unwrap();
    std::fs::remove_dir_all(root).unwrap();
}

#[cfg(unix)]
#[test]
fn excluded_namespace_retargeting_never_grants_ambient_authority() {
    let root =
        std::env::temp_dir().join(format!("nudox-retarget-candidate-{}", std::process::id()));
    std::fs::create_dir_all(root.join("venv")).unwrap();
    std::fs::create_dir_all(root.join("vendor")).unwrap();
    std::fs::write(root.join("vendor/__init__.py"), "value = 42\n").unwrap();
    let source = "import venv\n";
    std::fs::write(root.join("core.py"), source).unwrap();
    let sources = [PythonProjectSource {
        relative_path: "core.py",
        source,
    }];
    let checker = NativePythonProjectAuthority::admit().unwrap();
    let cancelled = AtomicBool::new(false);
    let report = checker
        .analyze_project(
            &root,
            "pkg",
            &sources,
            PythonVersion::Python314,
            control(&cancelled),
        )
        .unwrap();
    std::fs::remove_dir(root.join("venv")).unwrap();
    std::os::unix::fs::symlink("vendor", root.join("venv")).unwrap();
    assert!(
        matches!(report.witness().validate_current(control(&cancelled)),
        Err(CheckerError::UncapturedDependency { path }) if path == root.join("venv"))
    );
    assert!(
        matches!(checker.analyze_project(&root, "pkg", &sources, PythonVersion::Python314, control(&cancelled)),
        Err(CheckerError::UncapturedDependency { path }) if path == root.join("venv"))
    );
    std::fs::remove_file(root.join("venv")).unwrap();
    std::fs::remove_dir_all(root).unwrap();
}
