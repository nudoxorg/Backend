//! Opt-in replay of the complete real HTTPie capture, without vendoring an SDK.
//! The fixture root is the ordinary prepared app containing its real `venv/`.

use std::path::{Component, PathBuf};
use std::sync::atomic::AtomicBool;
use std::time::{Duration, Instant};

use backend_frontend_python::legacy::checker::{
    NativePythonProjectAuthority, PYTHON_PACKAGE_CHILD_ENVIRONMENT_POLICY_ID_V1,
    PythonProjectBytesSource, PythonProjectControl,
};
use backend_semantic::vocabulary::PythonVersion;
use sha2::{Digest, Sha256};

#[test]
#[ignore = "requires the byte-pinned real HTTPie 5b604c37 capture and its ordinary uninitialized venv, supplied by the guarded native lane"]
fn real_httpie_133_sources_beside_normal_venv_keep_exact_native_frontier() {
    let root = PathBuf::from(std::env::var_os("NUDOX_HTTPIE_CAPTURE_ROOT").expect(
        "set NUDOX_HTTPIE_CAPTURE_ROOT to the existing real HTTPie prepared checkout; do not fabricate SDK source",
    ));
    assert!(root.is_absolute());
    assert!(std::fs::symlink_metadata(&root).unwrap().is_dir());
    assert!(
        std::fs::symlink_metadata(root.join("venv"))
            .unwrap()
            .is_dir()
    );
    assert!(!root.join("venv/__init__.py").exists());
    assert!(!root.join("venv/__init__.pyi").exists());
    assert!(
        root.join("venv/pyvenv.cfg").is_file(),
        "ordinary Python environment, not an empty stand-in"
    );

    let mut owned = Vec::new();
    for line in include_str!("fixtures/httpie-captured-frontier.tsv")
        .lines()
        .filter(|line| !line.starts_with('#'))
    {
        let (path, rest) = line.split_once('\t').unwrap();
        let (bytes, sha256) = rest.split_once('\t').unwrap();
        let path_buf = PathBuf::from(path);
        assert!(
            path_buf
                .components()
                .all(|component| matches!(component, Component::Normal(_)))
        );
        let actual = std::fs::read(root.join(&path_buf)).unwrap();
        assert_eq!(actual.len(), bytes.parse::<usize>().unwrap(), "{path}");
        assert_eq!(
            format!("{:x}", Sha256::digest(&actual)),
            sha256,
            "{path}: source differs from actual captured app pin"
        );
        owned.push((path, actual));
    }
    assert_eq!(owned.len(), 133);
    assert!(owned.windows(2).all(|pair| pair[0].0 < pair[1].0));
    for (path, expected) in [
        (
            "setup.cfg",
            "42231fbc2916bd3fa5f21262d6c1fcbd432990d5de659bc6808b4fd6d538f029",
        ),
        (
            "LICENSE",
            "89a72f18e678d9db71553f9a30bc0c436c71a61496849af9cd477871b70c31ef",
        ),
    ] {
        assert_eq!(
            format!(
                "{:x}",
                Sha256::digest(std::fs::read(root.join(path)).unwrap())
            ),
            expected,
            "{path}"
        );
    }
    let sources = owned
        .iter()
        .map(|(relative_path, source)| PythonProjectBytesSource {
            relative_path,
            source,
        })
        .collect::<Vec<_>>();
    let cancelled = AtomicBool::new(false);
    let control = || PythonProjectControl {
        cancelled: &cancelled,
        deadline: Instant::now() + Duration::from_secs(120),
    };
    let checker = NativePythonProjectAuthority::admit().expect("real compiled native authority");
    eprintln!(
        "HTTPie app_pin=5b604c37c6c67e18e7c3e9aee6c88a8c22b98345 source_count=133 policy={PYTHON_PACKAGE_CHILD_ENVIRONMENT_POLICY_ID_V1}"
    );
    let report = checker
        .analyze_project_bytes(
            &root,
            "httpie",
            &sources,
            PythonVersion::Python314,
            control(),
        )
        .unwrap_or_else(|error| {
            panic!("original typed native refusal (not first-source relabelling): {error:#?}")
        });
    for source in &sources {
        assert!(
            report.source_status(source.relative_path).is_some(),
            "selected status {}",
            source.relative_path
        );
        assert!(
            report.module(source.relative_path).is_some(),
            "selected report {}",
            source.relative_path
        );
    }
    let profiling = report.module("extras/profiling/run.py").unwrap();
    assert!(
        profiling
            .imports
            .iter()
            .any(|import| import.module == "venv" && import.resolved),
        "stdlib venv must retain native authority beside the ordinary excluded environment"
    );
    let mut exact_local_targets = 0;
    for source in &sources {
        for symbol in &report.module(source.relative_path).unwrap().symbols {
            if let backend_frontend_python::legacy::checker::SymbolOutcome::Definition {
                target,
                callee,
            } = &symbol.outcome
            {
                for target in std::iter::once(target).chain(callee.iter()) {
                    let raw = owned
                        .iter()
                        .find(|(path, _)| *path == target.relative_path.as_ref())
                        .expect("no uncaptured SDK target becomes a captured project definition");
                    assert_eq!(
                        &raw.1[target.name_span.start as usize..target.name_span.end as usize],
                        target.name.as_bytes()
                    );
                    exact_local_targets += 1;
                }
            }
        }
    }
    assert!(
        exact_local_targets > 0,
        "native captured local facts remain available"
    );
    report
        .witness()
        .validate_current(control())
        .expect("all original selected sources, config and candidate membership remain exact");
    eprintln!(
        "HTTPie native_report source_count=133 exact_local_targets={exact_local_targets} dependency_gaps={} diagnostics={} fingerprint={:?}",
        report.coverage_gaps().len(),
        report.diagnostics().len(),
        report.witness().fingerprint().as_bytes()
    );
}
