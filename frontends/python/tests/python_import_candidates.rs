//! Real native authority controls for candidate shape versus node membership.

use std::sync::atomic::AtomicBool;
use std::time::{Duration, Instant};

use backend_frontend_python::legacy::checker::{
    CheckerError, InferenceSite, InferredType, NativePythonProjectAuthority, PythonProjectControl,
    PythonProjectCoverageGapKind, PythonProjectPhase, PythonProjectSource,
    PythonProjectSourceStatus,
};
use backend_semantic::vocabulary::PythonVersion;

fn control(cancelled: &AtomicBool) -> PythonProjectControl<'_> {
    PythonProjectControl {
        cancelled,
        deadline: Instant::now() + Duration::from_secs(30),
    }
}

#[test]
fn unavailable_dependency_cycles_keep_other_owners_without_reflexive_causes() {
    let root = std::env::temp_dir().join(format!("nudox-unavailable-cycle-{}", std::process::id()));
    std::fs::create_dir_all(&root).unwrap();
    let left = "import opaque_left\nfrom right import right\n\ndef left():\n    return opaque_left.value, right\n";
    let right = "import opaque_right\nfrom left import left\n\ndef right():\n    return opaque_right.value, left\n";
    let consumer = "from left import left\n\ndef consume():\n    return left()\n";
    let sources = [
        PythonProjectSource {
            relative_path: "left.py",
            source: left,
        },
        PythonProjectSource {
            relative_path: "right.py",
            source: right,
        },
        PythonProjectSource {
            relative_path: "consumer.py",
            source: consumer,
        },
    ];
    for source in &sources {
        std::fs::write(root.join(source.relative_path), source.source).unwrap();
    }
    for module in ["opaque_left", "opaque_right"] {
        std::fs::write(root.join(format!("{module}.pyx")), b"opaque compiled bytes").unwrap();
    }
    let cancelled = AtomicBool::new(false);
    let report = NativePythonProjectAuthority::admit()
        .unwrap()
        .analyze_project(
            &root,
            "cycle",
            &sources,
            PythonVersion::Python314,
            control(&cancelled),
        )
        .unwrap();
    for (path, other, binding) in [
        ("left.py", "right.py", "opaque_left"),
        ("right.py", "left.py", "opaque_right"),
    ] {
        let Some(PythonProjectSourceStatus::UnavailableCompiledImport {
            imports,
            dependencies,
        }) = report.source_status(path)
        else {
            panic!("direct compiled cause for {path}")
        };
        assert_eq!(imports.len(), 1);
        assert_eq!(imports[0].binding.as_ref(), binding);
        assert_eq!(
            dependencies
                .iter()
                .map(|path| path.as_ref())
                .collect::<Vec<_>>(),
            [other],
            "the other genuine cycle owner survives; own root is not repeated"
        );
        assert!(report.module(path).unwrap().inferences.is_empty());
        assert!(report.module(path).unwrap().symbols.is_empty());
    }
    let Some(PythonProjectSourceStatus::UnavailableDependency { dependencies }) =
        report.source_status("consumer.py")
    else {
        panic!("consumer native reverse closure")
    };
    assert_eq!(
        dependencies
            .iter()
            .map(|path| path.as_ref())
            .collect::<Vec<_>>(),
        ["left.py", "right.py"]
    );
    report
        .witness()
        .validate_current(control(&cancelled))
        .unwrap();
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn direct_compiled_import_preserves_other_unavailable_graph_causes() {
    let root = std::env::temp_dir().join(format!(
        "nudox-compiled-secondary-cause-{}",
        std::process::id()
    ));
    std::fs::create_dir_all(&root).unwrap();
    let alpha = "import opaque_alpha\n\ndef alpha():\n    return opaque_alpha.value\n";
    let zeta = "import opaque_zeta\n\ndef zeta():\n    return opaque_zeta.value\n";
    let mixed = "import venv\nfrom zeta import zeta\nfrom alpha import alpha\n\ndef mixed():\n    return venv.EnvBuilder(), alpha(), zeta()\n";
    let consumer = "from mixed import mixed\n\ndef consumer():\n    return mixed()\n";
    let independent = "def answer():\n    return 42\n";
    let sources = [
        PythonProjectSource {
            relative_path: "alpha.py",
            source: alpha,
        },
        PythonProjectSource {
            relative_path: "zeta.py",
            source: zeta,
        },
        PythonProjectSource {
            relative_path: "mixed.py",
            source: mixed,
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
    for source in &sources {
        std::fs::write(root.join(source.relative_path), source.source).unwrap();
    }
    for name in ["opaque_alpha.pyx", "opaque_zeta.pyd", "venv.pyx"] {
        std::fs::write(root.join(name), b"opaque compiled candidate").unwrap();
    }
    let cancelled = AtomicBool::new(false);
    let report = NativePythonProjectAuthority::admit()
        .unwrap()
        .analyze_project(
            &root,
            "secondary",
            &sources,
            PythonVersion::Python314,
            control(&cancelled),
        )
        .unwrap();
    let Some(PythonProjectSourceStatus::UnavailableCompiledImport {
        imports,
        dependencies,
    }) = report.source_status("mixed.py")
    else {
        panic!("direct compiled import must retain its own cause and actual dependency causes")
    };
    assert_eq!(imports.len(), 1);
    assert_eq!(imports[0].binding.as_ref(), "venv");
    assert_eq!(imports[0].candidates.as_ref(), [root.join("venv.pyx")]);
    assert_eq!(
        dependencies
            .iter()
            .map(|path| path.as_ref())
            .collect::<Vec<_>>(),
        ["alpha.py", "zeta.py"]
    );
    assert!(
        !dependencies.iter().any(|path| path.as_ref() == "mixed.py"),
        "direct cause must not invent a graph self-edge"
    );
    for (path, binding) in [("alpha.py", "opaque_alpha"), ("zeta.py", "opaque_zeta")] {
        let Some(PythonProjectSourceStatus::UnavailableCompiledImport {
            imports,
            dependencies,
        }) = report.source_status(path)
        else {
            panic!("actual compiled root {path}")
        };
        assert_eq!(imports.len(), 1);
        assert_eq!(imports[0].binding.as_ref(), binding);
        assert!(
            dependencies.is_empty(),
            "root {path} has no unavailable graph dependency"
        );
    }
    let Some(PythonProjectSourceStatus::UnavailableDependency { dependencies }) =
        report.source_status("consumer.py")
    else {
        panic!("consumer must keep the actual transitive native dependency closure")
    };
    assert_eq!(
        dependencies
            .iter()
            .map(|path| path.as_ref())
            .collect::<Vec<_>>(),
        ["alpha.py", "mixed.py", "zeta.py"]
    );
    for path in ["alpha.py", "zeta.py", "mixed.py", "consumer.py"] {
        assert!(report.module(path).unwrap().inferences.is_empty(), "{path}");
        assert!(report.module(path).unwrap().symbols.is_empty(), "{path}");
        assert!(
            !report
                .diagnostics()
                .iter()
                .any(|diagnostic| diagnostic.relative_path.as_ref() == path),
            "{path}: opaque candidates cannot certify semantic errors"
        );
    }
    assert_eq!(
        report.source_status("independent.py"),
        Some(&PythonProjectSourceStatus::Analyzed)
    );
    assert!(
        report
            .module("independent.py")
            .unwrap()
            .inferences
            .iter()
            .any(
                |fact| fact.kind == InferenceSite::Return && fact.observed == InferredType::Integer
            )
    );
    report
        .witness()
        .validate_current(control(&cancelled))
        .unwrap();
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn comma_separated_child_imports_keep_distinct_compiled_probe_identity() {
    let root = std::env::temp_dir().join(format!(
        "nudox-compiled-alias-probes-{}",
        std::process::id()
    ));
    std::fs::create_dir_all(root.join("pkg")).unwrap();
    let source = "from pkg import a as first, b as second\n";
    let local = "def answer():\n    return 42\n";
    std::fs::write(root.join("core.py"), source).unwrap();
    std::fs::write(root.join("pkg/b.py"), local).unwrap();
    let compiled = root.join("pkg/a.pyx");
    std::fs::write(&compiled, b"opaque compiled candidate").unwrap();
    let sources = [
        PythonProjectSource {
            relative_path: "core.py",
            source,
        },
        PythonProjectSource {
            relative_path: "pkg/b.py",
            source: local,
        },
    ];
    let cancelled = AtomicBool::new(false);
    let checker = NativePythonProjectAuthority::admit().unwrap();
    let report = checker
        .analyze_project(
            &root,
            "pkg",
            &sources,
            PythonVersion::Python314,
            control(&cancelled),
        )
        .unwrap();
    let Some(PythonProjectSourceStatus::UnavailableCompiledImport { imports, .. }) =
        report.source_status("core.py")
    else {
        panic!("actual compiled candidate must quarantine the direct source")
    };
    assert_eq!(
        imports.len(),
        1,
        "one compiled child must not classify its sibling binding as compiled"
    );
    let first = &imports[0];
    assert_eq!(first.binding.as_ref(), "first");
    assert_eq!(first.module.as_ref(), "pkg");
    assert_eq!(
        &source[first.binding_span.start as usize..first.binding_span.end as usize],
        "first"
    );
    assert_eq!(
        &source[first.module_span.start as usize..first.module_span.end as usize],
        "pkg"
    );
    assert_eq!(first.candidates.as_ref(), [compiled.clone()]);
    let gaps = report
        .coverage_gaps()
        .iter()
        .filter(|gap| {
            gap.relative_path.as_ref() == "core.py"
                && gap.kind == PythonProjectCoverageGapKind::UnavailableCompiledImport
        })
        .collect::<Vec<_>>();
    assert_eq!(
        gaps.len(),
        1,
        "shared module coordinates do not duplicate coverage gaps"
    );
    assert_eq!(gaps[0].span, first.module_span);
    assert_eq!(
        report.source_status("pkg/b.py"),
        Some(&PythonProjectSourceStatus::Analyzed)
    );
    assert!(!report.module("pkg/b.py").unwrap().inferences.is_empty());
    assert!(report.module("core.py").unwrap().inferences.is_empty());
    assert!(report.module("core.py").unwrap().symbols.is_empty());
    report
        .witness()
        .validate_current(control(&cancelled))
        .unwrap();
    let other = root.join("pkg/b.pyd");
    std::fs::write(&other, b"second opaque compiled candidate").unwrap();
    assert!(
        report
            .witness()
            .validate_current(control(&cancelled))
            .is_err()
    );
    let both = checker
        .analyze_project(
            &root,
            "pkg",
            &sources,
            PythonVersion::Python314,
            control(&cancelled),
        )
        .unwrap();
    let Some(PythonProjectSourceStatus::UnavailableCompiledImport { imports, .. }) =
        both.source_status("core.py")
    else {
        panic!("both candidate probes must remain unavailable")
    };
    assert_eq!(imports.len(), 2);
    assert_eq!(imports[0].binding.as_ref(), "first");
    assert_eq!(imports[1].binding.as_ref(), "second");
    assert_ne!(imports[0].binding_span, imports[1].binding_span);
    assert_eq!(imports[0].module_span, imports[1].module_span);
    assert_eq!(imports[1].candidates.as_ref(), [other]);
    assert_eq!(
        both.coverage_gaps()
            .iter()
            .filter(|gap| gap.relative_path.as_ref() == "core.py"
                && gap.kind == PythonProjectCoverageGapKind::UnavailableCompiledImport)
            .count(),
        1
    );
    both.witness()
        .validate_current(control(&cancelled))
        .unwrap();
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn local_compiled_stdlib_collision_never_claims_mirror_fallback_authority() {
    let root = std::env::temp_dir().join(format!(
        "nudox-compiled-stdlib-shadow-{}",
        std::process::id()
    ));
    std::fs::create_dir_all(root.join("venv")).unwrap();
    let source = "import venv\n\ndef builder():\n    return venv.EnvBuilder()\n\ndef wrong():\n    return venv.nudox_nonexistent\n";
    let consumer =
        "from core import builder\n\ndef consume():\n    return builder().nudox_nonexistent\n";
    let independent = "import json\n\ndef local_answer():\n    return 42\n\ndef wrong() -> int:\n    return 'bad'\n";
    let broken = "import venv\ndef broken(:\n";
    for (name, bytes) in [
        ("core.py", source),
        ("consumer.py", consumer),
        ("independent.py", independent),
        ("broken.py", broken),
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
        PythonProjectSource {
            relative_path: "broken.py",
            source: broken,
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
    for path in ["core.py", "consumer.py"] {
        assert!(
            baseline
                .diagnostics()
                .iter()
                .any(|diagnostic| diagnostic.relative_path.as_ref() == path
                    && diagnostic.kind.as_ref() == "missing-attribute"),
            "the actual bundled fallback produces a semantic diagnostic for {path} before shadowing"
        );
    }
    assert!(
        baseline
            .diagnostics()
            .iter()
            .any(
                |diagnostic| diagnostic.relative_path.as_ref() == "independent.py"
                    && diagnostic.kind.as_ref() == "bad-return"
            )
    );
    assert_eq!(
        baseline.source_status("broken.py"),
        Some(&PythonProjectSourceStatus::UnavailableSyntax)
    );
    assert!(
        baseline
            .diagnostics()
            .iter()
            .any(
                |diagnostic| diagnostic.relative_path.as_ref() == "broken.py"
                    && diagnostic.kind.as_ref() == "parse-error"
            )
    );
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
        assert!(
            matches!(shadow.source_status("core.py"), Some(PythonProjectSourceStatus::UnavailableCompiledImport { imports, .. }) if imports.len() == 1 && imports[0].binding.as_ref() == "venv" && imports[0].candidates.as_ref() == [candidate.clone()]),
            "the direct compiled refusal has actual candidates, not a fictitious graph self-edge"
        );
        assert!(
            matches!(shadow.source_status("consumer.py"), Some(PythonProjectSourceStatus::UnavailableDependency { dependencies }) if dependencies.iter().any(|dependency| dependency.as_ref() == "core.py")),
            "the reverse dependent is reached through the committed native graph"
        );
        for path in ["core.py", "consumer.py"] {
            let quarantined = shadow.module(path).unwrap();
            assert!(
                quarantined.inferences.is_empty(),
                "no precise fallback type escapes from {path}"
            );
            assert!(
                quarantined.symbols.is_empty(),
                "no precise fallback definition/callee escapes from {path}"
            );
            assert!(
                !shadow
                    .diagnostics()
                    .iter()
                    .any(|diagnostic| diagnostic.relative_path.as_ref() == path),
                "no fallback-based semantic diagnostic escapes from quarantined {path}"
            );
        }
        assert_eq!(
            shadow.source_status("independent.py"),
            Some(&PythonProjectSourceStatus::Analyzed)
        );
        assert!(
            shadow
                .diagnostics()
                .iter()
                .any(
                    |diagnostic| diagnostic.relative_path.as_ref() == "independent.py"
                        && diagnostic.kind.as_ref() == "bad-return"
                ),
            "an independent real semantic diagnostic remains available"
        );
        assert_eq!(
            shadow.source_status("broken.py"),
            Some(&PythonProjectSourceStatus::UnavailableSyntax)
        );
        assert!(
            shadow
                .diagnostics()
                .iter()
                .any(
                    |diagnostic| diagnostic.relative_path.as_ref() == "broken.py"
                        && diagnostic.kind.as_ref() == "parse-error"
                ),
            "actual native parser errors survive the quarantine"
        );
        assert!(
            shadow
                .diagnostics()
                .iter()
                .filter(|diagnostic| diagnostic.relative_path.as_ref() == "broken.py")
                .all(|diagnostic| diagnostic.kind.as_ref() == "parse-error")
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
        assert_eq!(
            directory.source_status("core.py"),
            Some(&PythonProjectSourceStatus::Analyzed)
        );
        assert!(!directory.module("core.py").unwrap().inferences.is_empty());
        assert!(
            !directory
                .coverage_gaps()
                .iter()
                .any(|gap| gap.relative_path.as_ref() == "core.py"
                    && gap.kind == PythonProjectCoverageGapKind::UnavailableCompiledImport)
        );
        directory
            .witness()
            .validate_current(control(&cancelled))
            .unwrap();
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
        Err(CheckerError::ProjectDeadline {
            phase: PythonProjectPhase::SourceCapture
        })
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
