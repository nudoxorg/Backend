use std::sync::atomic::AtomicBool;
use std::time::{Duration, Instant};

use backend_frontend_python::legacy::checker::{
    CheckerError, NativePythonProjectAuthority, PythonProjectBytesSource, PythonProjectControl,
    PythonProjectSourceStatus, SymbolOutcome,
};
use backend_semantic::vocabulary::PythonVersion;

#[test]
fn native_initialized_build_package_keeps_existing_imports_and_refuses_omitted_members()
-> Result<(), Box<dyn std::error::Error>> {
    let root =
        std::env::temp_dir().join(format!("nudox-python-build-package-{}", std::process::id()));
    std::fs::create_dir_all(&root)?;
    let tracker = b"class BuildTracker:\n    pass\n";
    let caller = b"from pip._internal.operations.build.build_tracker import BuildTracker\ntracker = BuildTracker()\n";
    let missing = b"from pip._internal.operations.build.absent import Missing\n";
    let members: [(&str, &[u8]); 12] = [
        ("pip/__init__.py", b""),
        ("pip/_internal/__init__.py", b""),
        ("pip/_internal/operations/__init__.py", b""),
        // All six paths omitted by the former directory-name policy.
        ("pip/_internal/operations/build/__init__.py", b""),
        ("pip/_internal/operations/build/build_tracker.py", tracker),
        ("pip/_internal/operations/build/metadata.py", b"value = 1\n"),
        (
            "pip/_internal/operations/build/metadata_editable.py",
            b"value = 2\n",
        ),
        ("pip/_internal/operations/build/wheel.py", b"value = 3\n"),
        (
            "pip/_internal/operations/build/wheel_editable.py",
            b"value = 4\n",
        ),
        ("pip/_internal/build_env/__init__.py", b""),
        ("pip/_internal/build_env/installer.py", caller),
        ("missing.py", missing),
    ];
    let sources = members
        .iter()
        .map(|(path, source)| PythonProjectBytesSource {
            relative_path: path,
            source,
        })
        .collect::<Vec<_>>();
    for (path, source) in members {
        let path = root.join(path);
        std::fs::create_dir_all(path.parent().ok_or("source parent")?)?;
        std::fs::write(path, source)?;
    }
    let checker = NativePythonProjectAuthority::admit()?;
    let cancelled = AtomicBool::new(false);
    let control = || PythonProjectControl {
        cancelled: &cancelled,
        deadline: Instant::now() + Duration::from_secs(30),
    };
    for _ in 0..2 {
        let report = checker.analyze_project_bytes(
            &root,
            "pip",
            &sources,
            PythonVersion::Python314,
            control(),
        )?;
        for source in &sources {
            assert_eq!(
                report.source_status(source.relative_path),
                Some(&PythonProjectSourceStatus::Analyzed)
            );
        }
        let module = report
            .module("pip/_internal/build_env/installer.py")
            .ok_or("caller")?;
        let targets = module
            .symbols
            .iter()
            .filter_map(|symbol| match &symbol.outcome {
                SymbolOutcome::Definition { target, .. } if symbol.target == "BuildTracker" => {
                    Some(target)
                }
                _ => None,
            })
            .collect::<Vec<_>>();
        assert!(
            !targets.is_empty(),
            "the existing package leaf must resolve"
        );
        for target in targets {
            assert_eq!(
                target.relative_path.as_ref(),
                "pip/_internal/operations/build/build_tracker.py"
            );
            assert_eq!(
                &tracker[target.name_span.start as usize..target.name_span.end as usize],
                b"BuildTracker"
            );
            assert!(!target.same_module);
        }
        assert!(
            report
                .diagnostics()
                .iter()
                .any(
                    |diagnostic| diagnostic.relative_path.as_ref() == "missing.py"
                        && diagnostic.severity.as_ref() == "ERROR"
                )
        );
        assert!(
            !report
                .module("missing.py")
                .ok_or("missing import caller")?
                .symbols
                .iter()
                .any(|symbol| matches!(&symbol.outcome, SymbolOutcome::Definition { .. }))
        );
        report.witness().validate_current(control())?;
    }
    let omitted = sources
        .iter()
        .copied()
        .filter(|source| source.relative_path != "pip/_internal/operations/build/build_tracker.py")
        .collect::<Vec<_>>();
    assert!(
        matches!(
            checker.analyze_project_bytes(&root, "pip", &omitted, PythonVersion::Python314, control()),
            Err(CheckerError::IncompleteSourceFrontier { candidate, .. })
                if candidate == root.join("pip/_internal/operations/build/build_tracker.py")
        ),
        "a real existing leaf omission remains a typed refusal"
    );
    std::fs::remove_dir_all(root)?;
    Ok(())
}

#[test]
fn native_declared_src_build_and_namespace_dist_keep_exact_leaf_coordinates()
-> Result<(), Box<dyn std::error::Error>> {
    let root = std::env::temp_dir().join(format!(
        "nudox-python-declared-build-{}",
        std::process::id()
    ));
    std::fs::create_dir_all(&root)?;
    std::fs::write(
        root.join("pyproject.toml"),
        b"[project]\nname = 'build'\n[tool.pyrefly]\nsearch-path = ['src']\n",
    )?;
    let entrypoint = b"def entrypoint() -> int:\n    return 1\n";
    let value = b"def namespace_value() -> int: ...\n";
    let members: [(&str, &[u8]); 5] = [
        ("src/build/__init__.py", b""),
        ("src/build/__main__.py", entrypoint),
        ("src/ns/dist/__init__.pyi", b""),
        ("src/ns/dist/api.pyi", value),
        ("caller.py", b"from build.__main__ import entrypoint\nfrom ns.dist.api import namespace_value\nx = entrypoint()\ny = namespace_value()\n"),
    ];
    for (path, bytes) in members {
        let path = root.join(path);
        std::fs::create_dir_all(path.parent().ok_or("parent")?)?;
        std::fs::write(path, bytes)?;
    }
    let sources = members
        .iter()
        .map(|(relative_path, source)| PythonProjectBytesSource {
            relative_path,
            source,
        })
        .collect::<Vec<_>>();
    let checker = NativePythonProjectAuthority::admit()?;
    let cancelled = AtomicBool::new(false);
    for _ in 0..2 {
        let control = || PythonProjectControl {
            cancelled: &cancelled,
            deadline: Instant::now() + Duration::from_secs(30),
        };
        let report = checker.analyze_project_bytes(
            &root,
            "build",
            &sources,
            PythonVersion::Python314,
            control(),
        )?;
        let caller = report.module("caller.py").ok_or("caller")?;
        for (name, expected, raw) in [
            ("entrypoint", "src/build/__main__.py", entrypoint.as_slice()),
            ("namespace_value", "src/ns/dist/api.pyi", value.as_slice()),
        ] {
            let target = caller
                .symbols
                .iter()
                .find_map(|symbol| match &symbol.outcome {
                    SymbolOutcome::Definition { target, .. } if symbol.target == name => {
                        Some(target)
                    }
                    _ => None,
                })
                .ok_or("declared source-root leaf did not resolve")?;
            assert_eq!(target.relative_path.as_ref(), expected);
            assert_eq!(
                &raw[target.name_span.start as usize..target.name_span.end as usize],
                name.as_bytes()
            );
            assert!(!target.same_module);
        }
        report.witness().validate_current(control())?;
        assert!(!root.join("src/__init__.py").exists());
        assert!(!root.join("src/ns/__init__.py").exists());
    }
    std::fs::remove_dir_all(root)?;
    Ok(())
}
