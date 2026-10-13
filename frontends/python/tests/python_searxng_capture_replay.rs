//! Opt-in native replay of every byte-pinned SearXNG Python source, including docs.

use std::path::{Component, Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::time::{Duration, Instant};

use backend_frontend_python::legacy::checker::{
    NativePythonProjectAuthority, PYTHON_PACKAGE_CHILD_ENVIRONMENT_POLICY_ID_V1,
    PythonProjectBytesSource, SymbolOutcome,
};
use backend_semantic::vocabulary::PythonVersion;
use sha2::{Digest, Sha256};

#[cfg(unix)]
#[test]
#[ignore = "requires the original SearXNG ad46ebd7 checkout and template symlink, supplied by a guarded native lane"]
fn real_searxng_407_sources_keep_native_authority_beside_template_alias() {
    let root = PathBuf::from(
        std::env::var_os("NUDOX_SEARXNG_CAPTURE_ROOT")
            .expect("set NUDOX_SEARXNG_CAPTURE_ROOT to the original genuine SearXNG checkout"),
    );
    assert!(root.is_absolute());
    assert!(std::fs::symlink_metadata(&root).unwrap().is_dir());
    let alias = root.join("utils/templates/etc/apache2");
    assert!(std::fs::symlink_metadata(&alias).unwrap().is_symlink());
    assert_eq!(std::fs::read_link(&alias).unwrap(), Path::new("httpd"));
    assert_eq!(
        format!(
            "{:x}",
            Sha256::digest(std::fs::read(root.join("pyrightconfig.json")).unwrap())
        ),
        "546ab5b161d95b0a59e9030effda8ef0162435b34f9e8f9b759fb97a676e7753",
        "the original Pyright include/environment declarations must remain intact"
    );
    let mut owned = Vec::new();
    for line in include_str!("fixtures/searxng-captured-frontier.tsv")
        .lines()
        .filter(|line| !line.starts_with('#'))
    {
        let (path, rest) = line.split_once('\t').unwrap();
        let (bytes, sha256) = rest.split_once('\t').unwrap();
        assert!(
            Path::new(path)
                .components()
                .all(|part| matches!(part, Component::Normal(_)))
        );
        let actual = std::fs::read(root.join(path)).unwrap();
        assert_eq!(actual.len(), bytes.parse::<usize>().unwrap(), "{path}");
        assert_eq!(
            format!("{:x}", Sha256::digest(&actual)),
            sha256,
            "{path}: original captured bytes changed"
        );
        owned.push((path, actual));
    }
    assert_eq!(owned.len(), 407, "406 Python modules and one actual stub");
    assert!(owned.windows(2).all(|pair| pair[0].0 < pair[1].0));
    assert_eq!(
        owned
            .iter()
            .find(|(path, _)| *path == "docs/conf.py")
            .unwrap()
            .1
            .len(),
        7502
    );
    let sources = owned
        .iter()
        .map(|(relative_path, source)| PythonProjectBytesSource {
            relative_path,
            source,
        })
        .collect::<Vec<_>>();
    let cancelled = AtomicBool::new(false);
    let control = || backend_frontend_python::legacy::checker::PythonProjectControl {
        cancelled: &cancelled,
        deadline: Instant::now() + Duration::from_secs(120),
    };
    let checker = NativePythonProjectAuthority::admit().expect("actual native State authority");
    for version in [PythonVersion::Python313, PythonVersion::Python314] {
        let report = checker.analyze_project_bytes(&root, "searxng", &sources, version, control())
            .unwrap_or_else(|error| panic!("exact original-source native refusal, without first-file relabelling: {error:#?}"));
        let mut local_targets = 0;
        let mut cross_file_targets = 0;
        for source in &sources {
            assert!(
                report.source_status(source.relative_path).is_some(),
                "every original source has an explicit status: {}",
                source.relative_path
            );
            let module = report
                .module(source.relative_path)
                .expect("every original source has its report");
            for symbol in &module.symbols {
                if let SymbolOutcome::Definition { target, callee } = &symbol.outcome {
                    for target in std::iter::once(target).chain(callee.iter()) {
                        let raw = owned.iter().find(|(path, _)| *path == target.relative_path.as_ref())
                            .expect("no template alias or external dependency receives captured-source coordinates");
                        assert_eq!(
                            &raw.1[target.name_span.start as usize..target.name_span.end as usize],
                            target.name.as_bytes()
                        );
                        local_targets += 1;
                        cross_file_targets += usize::from(!target.same_module);
                    }
                }
            }
        }
        assert!(
            local_targets > 0 && cross_file_targets > 0,
            "actual native cross-file authority remains available"
        );
        report
            .witness()
            .validate_current(control())
            .expect("original inputs and opaque template alias remain exact");
        eprintln!(
            "SearXNG app_pin=ad46ebd7d87666edb5f116c5b274b47f689ac6cd profile={version:?} sources=407 local_targets={local_targets} cross_file_targets={cross_file_targets} gaps={} diagnostics={} policy={PYTHON_PACKAGE_CHILD_ENVIRONMENT_POLICY_ID_V1}",
            report.coverage_gaps().len(),
            report.diagnostics().len()
        );
    }
}
