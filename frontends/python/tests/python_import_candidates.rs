//! Real native authority controls for candidate shape versus node membership.

use std::sync::atomic::AtomicBool;
use std::time::{Duration, Instant};

use backend_frontend_python::legacy::checker::{
    CheckerError, InferenceSite, InferredType, NativePythonProjectAuthority, PythonProjectControl,
    PythonProjectCoverageGapKind, PythonProjectSource, PythonProjectSourceStatus,
};
use backend_semantic::vocabulary::PythonVersion;

fn control(cancelled: &AtomicBool) -> PythonProjectControl<'_> {
    PythonProjectControl {
        cancelled,
        deadline: Instant::now() + Duration::from_secs(30),
    }
}

#[test]
fn local_compiled_stdlib_collision_never_claims_mirror_fallback_authority() {
    let root = std::env::temp_dir().join(format!(
        "nudox-compiled-stdlib-shadow-{}",
        std::process::id()
    ));
    std::fs::create_dir_all(root.join("venv")).unwrap();
    let source = "import venv\n\ndef builder():\n    return venv.EnvBuilder()\n";
    let consumer = "from core import builder\n\ndef consume():\n    return builder()\n";
    let independent = "import json\n\ndef local_answer():\n    return 42\n";
    for (name, bytes) in [
        ("core.py", source),
        ("consumer.py", consumer),
        ("independent.py", independent),
    ] {
        std::fs::write(root.join(name), bytes).unwrap();
    }
    let sources = [
        PythonProjectSource {
            relative_path: "core.py",
            source,
        },
        PythonProjectSource {
            relative_path: "consumer.py",
            source: consumer,
        },
        PythonProjectSource {
            relative_path: "independent.py",
            source: independent,
        },
    ];
    let checker = NativePythonProjectAuthority::admit().unwrap();
    let cancelled = AtomicBool::new(false);
    let analyze = || {
        checker
            .analyze_project(
                &root,
                "pkg",
                &sources,
                PythonVersion::Python314,
                control(&cancelled),
            )
            .unwrap()
    };
    let baseline = analyze();
    assert!(
        baseline
            .module("core.py")
            .unwrap()
            .imports
            .iter()
            .any(|import| import.module == "venv" && import.resolved)
    );
    assert_eq!(
        baseline.source_status("core.py"),
        Some(&PythonProjectSourceStatus::Analyzed)
    );
    assert_eq!(
        baseline.source_status("consumer.py"),
        Some(&PythonProjectSourceStatus::Analyzed)
    );
    assert!(!baseline.module("core.py").unwrap().inferences.is_empty());
    assert!(
        !baseline
            .module("consumer.py")
            .unwrap()
            .inferences
            .is_empty()
    );
    baseline
        .witness()
        .validate_current(control(&cancelled))
        .unwrap();
    for suffix in ["pyx", "pyd"] {
        let candidate = root.join(format!("venv.{suffix}"));
        std::fs::write(&candidate, b"opaque local compiled-module bytes").unwrap();
        assert!(
            baseline
                .witness()
                .validate_current(control(&cancelled))
                .is_err(),
            "positive compiled membership invalidates the earlier negative witness"
        );
        let shadow = analyze();
        assert_ne!(
            baseline.witness().fingerprint(),
            shadow.witness().fingerprint(),
            "the changed candidate enters the recipe's authority fingerprint"
        );
        let module = shadow.module("core.py").unwrap();
        assert!(
            module
                .imports
                .iter()
                .any(|import| import.module == "venv" && !import.resolved),
            "a mirrored standard-library fallback is not authority for the shadowed original import"
        );
        for path in ["core.py", "consumer.py"] {
            assert!(
                matches!(shadow.source_status(path), Some(PythonProjectSourceStatus::UnavailableDependency { dependencies }) if dependencies.iter().any(|dependency| dependency.as_ref() == "core.py")),
                "the actual importing source and native reverse-dependent are quarantined"
            );
            let quarantined = shadow.module(path).unwrap();
            assert!(
                quarantined.inferences.is_empty(),
                "no precise fallback type escapes from {path}"
            );
            assert!(
                quarantined.symbols.is_empty(),
                "no precise fallback definition/callee escapes from {path}"
            );
        }
        assert_eq!(
            shadow.source_status("independent.py"),
            Some(&PythonProjectSourceStatus::Analyzed)
        );
        let independent_report = shadow.module("independent.py").unwrap();
        assert!(
            independent_report
                .imports
                .iter()
                .any(|import| import.module == "json" && import.resolved),
            "an independent source keeps its standard-library authority"
        );
        assert!(
            independent_report
                .inferences
                .iter()
                .any(|inference| inference.kind == InferenceSite::Return
                    && inference.observed == InferredType::Integer),
            "independent local facts remain available"
        );
        let gap = shadow
            .coverage_gaps()
            .iter()
            .find(|gap| gap.kind == PythonProjectCoverageGapKind::UnavailableCompiledImport)
            .expect(
                "the actual compiled candidate is an explicit gap even when mirror lookup succeeds",
            );
        assert_eq!(gap.relative_path.as_ref(), "core.py");
        assert_eq!(
            &source[gap.span.start as usize..gap.span.end as usize],
            "venv"
        );
        shadow
            .witness()
            .validate_current(control(&cancelled))
            .unwrap();
        // Unsupported compiled bytes are not read or used as authority. Their
        // presence and kind, rather than an invented content identity, are bound.
        std::fs::write(&candidate, b"changed opaque candidate bytes").unwrap();
        shadow
            .witness()
            .validate_current(control(&cancelled))
            .unwrap();
        std::fs::remove_file(&candidate).unwrap();
        std::fs::create_dir(&candidate).unwrap();
        assert!(
            shadow
                .witness()
                .validate_current(control(&cancelled))
                .is_err(),
            "file-to-directory retarget invalidates compiled membership"
        );
        let directory = analyze();
        assert!(
            directory
                .module("core.py")
                .unwrap()
                .imports
                .iter()
                .any(|import| import.module == "venv" && import.resolved),
            "a directory with a compiled suffix is not a compiled module"
        );
        std::fs::remove_dir(&candidate).unwrap();
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink("venv", &candidate).unwrap();
            assert!(
                matches!(shadow.witness().validate_current(control(&cancelled)), Err(CheckerError::UncapturedDependency { path }) if path == candidate)
            );
            assert!(
                matches!(checker.analyze_project(&root, "pkg", &sources, PythonVersion::Python314, control(&cancelled)), Err(CheckerError::UncapturedDependency { path }) if path == candidate)
            );
            std::fs::remove_file(&candidate).unwrap();
        }
        assert!(
            shadow
                .witness()
                .validate_current(control(&cancelled))
                .is_err(),
            "compiled presence is not forgotten after removal"
        );
        baseline
            .witness()
            .validate_current(control(&cancelled))
            .unwrap();
        let restored = analyze();
        assert!(
            restored
                .module("core.py")
                .unwrap()
                .imports
                .iter()
                .any(|import| import.module == "venv" && import.resolved)
        );
        assert_eq!(
            baseline.witness().fingerprint(),
            restored.witness().fingerprint()
        );
    }
    std::fs::remove_dir_all(root).unwrap();
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
