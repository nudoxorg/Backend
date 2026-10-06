//! Live native project joins, fresh transactions, and admission witness guards.

use std::sync::atomic::AtomicBool;
use std::time::{Duration, Instant};

use backend_frontend_python::legacy::checker::{
    CheckerError, InferenceSite, InferredType, NativePythonProjectAuthority, PythonProjectControl,
    PythonProjectSource, SymbolOutcome,
};
use backend_semantic::vocabulary::PythonVersion;

#[test]
fn native_project_restores_cross_module_definitions_preserving_utf8() {
    let root =
        std::env::temp_dir().join(format!("nudox-python-project-test-{}", std::process::id()));
    std::fs::create_dir_all(&root).expect("fixture root");
    let checker = NativePythonProjectAuthority::admit().expect("compiled native producer");
    let sources = [
        PythonProjectSource {
            relative_path: "pkg/__init__.py",
            source: "",
        },
        PythonProjectSource {
            relative_path: "pkg/api.py",
            source: "# 🐍 non-ASCII must retain UTF-8 byte coordinates\nfrom . import models\n\ndef make():\n    return models.Point()\n",
        },
        PythonProjectSource {
            relative_path: "pkg/models.py",
            source: "class Point:\n    width = 42\n    written: str = 42\n\n    def __init__(self) -> None:\n        pass\n\n    def declared(self) -> str:\n        return 42\n\n    def label(self):\n        return 'point'\n",
        },
    ];
    let cancelled = AtomicBool::new(false);
    let report = checker
        .analyze_project(
            &root,
            "pkg",
            &sources,
            PythonVersion::Python314,
            PythonProjectControl {
                cancelled: &cancelled,
                deadline: Instant::now() + Duration::from_secs(30),
            },
        )
        .expect("native project pass");
    let api = report.module("pkg/api.py").expect("exact api report");
    let target = api
        .symbols
        .iter()
        .find_map(|symbol| match &symbol.outcome {
            SymbolOutcome::Definition { target, .. }
                if target.qualified_name.as_ref() == "pkg.models.Point" =>
            {
                Some((symbol, target))
            }
            _ => None,
        })
        .expect("native cross-file Point target");
    assert_eq!(target.1.relative_path.as_ref(), "pkg/models.py");
    assert!(!target.1.same_module);
    let callee = api
        .symbols
        .iter()
        .find_map(|symbol| match &symbol.outcome {
            SymbolOutcome::Definition {
                target,
                callee: Some(callee),
            } if target.qualified_name.as_ref() == "pkg.models.Point" => Some(callee),
            _ => None,
        })
        .expect("native constructor target distinct from class binding");
    assert_eq!(callee.qualified_name.as_ref(), "pkg.models.Point.__init__");
    assert_eq!(
        &sources[2].source[callee.name_span.start as usize..callee.name_span.end as usize],
        "__init__"
    );
    assert_eq!(
        &sources[1].source[target.0.span.start as usize..target.0.span.end as usize],
        "Point"
    );
    assert_eq!(
        &sources[2].source[target.1.name_span.start as usize..target.1.name_span.end as usize],
        "Point"
    );
    let models = report.module("pkg/models.py").expect("exact models report");
    assert!(
        models
            .inferences
            .iter()
            .any(|inference| inference.kind == InferenceSite::Return
                && inference.observed == InferredType::Str),
        "unwritten method return comes from native State"
    );
    assert!(
        models
            .inferences
            .iter()
            .any(|inference| inference.kind == InferenceSite::ClassField
                && inference.observed == InferredType::Integer),
        "class-body field comes from native State"
    );
    let written_source = sources[2].source;
    let declared = written_source.find("declared").expect("written function") as u32;
    let written = written_source.find("written").expect("written field") as u32;
    assert!(
        !models
            .inferences
            .iter()
            .any(|inference| inference.site.start == declared
                && inference.kind == InferenceSite::Return),
        "written returns remain authoritative"
    );
    assert!(
        !models
            .inferences
            .iter()
            .any(|inference| inference.site.start == written
                && inference.kind == InferenceSite::ClassField),
        "written fields remain authoritative"
    );
    assert!(api.inferences.iter().any(|inference| inference.kind == InferenceSite::Return
        && matches!(&inference.observed, InferredType::Named(name) if name.as_ref() == "pkg.models.Point")));
    report
        .witness()
        .validate_current()
        .expect("unchanged witness");
    std::fs::write(root.join("setup.cfg"), "[metadata]\nname = changed\n")
        .expect("new negative config probe");
    assert!(
        report.witness().validate_current().is_err(),
        "new config invalidates admitted negative probe"
    );
    let changed_configuration = checker
        .analyze_project(
            &root,
            "pkg",
            &sources,
            PythonVersion::Python314,
            PythonProjectControl {
                cancelled: &cancelled,
                deadline: Instant::now() + Duration::from_secs(30),
            },
        )
        .expect("fresh configuration transaction");
    assert_ne!(
        report.witness().fingerprint(),
        changed_configuration.witness().fingerprint(),
        "present configuration changes cross-run identity even with identical source bytes"
    );
    std::fs::remove_file(root.join("setup.cfg")).expect("restore absent config");
    // A fresh transaction gets the replacement captured bytes, despite the
    // original filesystem having none of the selected source files.
    let replacement = [
        sources[0],
        sources[1],
        PythonProjectSource {
            relative_path: "pkg/models.py",
            source: "# changed native definition coordinates\nclass Point:\n    width = 'wide'\n\n    def label(self):\n        return 42\n",
        },
    ];
    let restarted = checker
        .analyze_project(
            &root,
            "pkg",
            &replacement,
            PythonVersion::Python314,
            PythonProjectControl {
                cancelled: &cancelled,
                deadline: Instant::now() + Duration::from_secs(30),
            },
        )
        .expect("fresh replacement pass");
    let replacement_target = restarted
        .module("pkg/api.py")
        .expect("replacement api")
        .symbols
        .iter()
        .find_map(|symbol| match &symbol.outcome {
            SymbolOutcome::Definition { target, .. }
                if target.qualified_name.as_ref() == "pkg.models.Point" =>
            {
                Some(target)
            }
            _ => None,
        })
        .expect("fresh replacement target");
    assert_ne!(replacement_target.name_span, target.1.name_span);
    assert_eq!(
        &replacement[2].source[replacement_target.name_span.start as usize
            ..replacement_target.name_span.end as usize],
        "Point"
    );
    let models = restarted
        .module("pkg/models.py")
        .expect("replacement model module");
    assert!(
        models
            .inferences
            .iter()
            .any(|inference| inference.kind == InferenceSite::Return
                && inference.observed == InferredType::Integer)
    );
    assert!(
        models
            .inferences
            .iter()
            .any(|inference| inference.kind == InferenceSite::ClassField
                && inference.observed == InferredType::Str)
    );
    std::fs::remove_dir_all(root).expect("remove fixture root");
}

#[test]
fn cancelled_project_cannot_spawn_or_return_an_admitted_report() {
    let checker = NativePythonProjectAuthority::admit().expect("compiled native producer");
    let cancelled = AtomicBool::new(true);
    let result = checker.analyze_project(
        std::path::Path::new("/tmp"),
        "pkg",
        &[],
        PythonVersion::Python314,
        PythonProjectControl {
            cancelled: &cancelled,
            deadline: Instant::now() + Duration::from_secs(1),
        },
    );
    assert!(matches!(
        result,
        Err(CheckerError::Cancelled { phase: "project" })
    ));
}

#[test]
fn native_project_cannot_bind_uncaptured_files_or_external_configured_roots() {
    let root = std::env::temp_dir().join(format!(
        "nudox-python-project-closure-test-{}",
        std::process::id()
    ));
    std::fs::create_dir_all(&root).expect("fixture root");
    // This real file is deliberately outside the selected source frontier.
    std::fs::write(
        root.join("omitted.py"),
        "def answer():\n    return 'ambient'\n",
    )
    .expect("uncaptured original file");
    let sources = [PythonProjectSource {
        relative_path: "core.py",
        source: "import omitted\n\ndef result():\n    return omitted.answer()\n",
    }];
    let checker = NativePythonProjectAuthority::admit().expect("compiled native producer");
    let cancelled = AtomicBool::new(false);
    let refused = checker.analyze_project(
        &root,
        "pkg",
        &sources,
        PythonVersion::Python314,
        PythonProjectControl {
            cancelled: &cancelled,
            deadline: Instant::now() + Duration::from_secs(30),
        },
    );
    assert!(
        matches!(refused, Err(CheckerError::IncompleteSourceFrontier { candidate, .. }) if candidate == root.join("omitted.py")),
        "existing internal source omission is a typed incomplete frontier"
    );
    std::fs::remove_file(root.join("omitted.py")).expect("absent internal candidate");
    let report = checker
        .analyze_project(
            &root,
            "pkg",
            &sources,
            PythonVersion::Python314,
            PythonProjectControl {
                cancelled: &cancelled,
                deadline: Instant::now() + Duration::from_secs(30),
            },
        )
        .expect("genuinely absent import is unavailable");
    let module = report.module("core.py").expect("core module");
    assert!(
        module
            .imports
            .iter()
            .any(|import| import.module == "omitted" && !import.resolved)
    );
    assert!(
        report
            .diagnostics()
            .iter()
            .any(|diagnostic| diagnostic.kind.as_ref() == "missing-import")
    );
    std::fs::write(root.join("omitted.py"), "value = 42\n").expect("new candidate");
    assert!(
        report.witness().validate_current().is_err(),
        "new candidate invalidates admitted negative import probe"
    );
    std::fs::remove_file(root.join("omitted.py")).expect("restore candidate absence");
    let external = root
        .parent()
        .expect("fixture parent")
        .join("uncaptured-native-dependencies");
    std::fs::write(
        root.join(".pyrefly.toml"),
        format!("search-path = [{:?}]\n", external.to_string_lossy()),
    )
    .expect("explicit external root");
    let refused = checker.analyze_project(
        &root,
        "pkg",
        &sources,
        PythonVersion::Python314,
        PythonProjectControl {
            cancelled: &cancelled,
            deadline: Instant::now() + Duration::from_secs(30),
        },
    );
    assert!(
        matches!(refused, Err(CheckerError::UncapturedDependency { path }) if path == external),
        "external resolver roots are a typed refusal"
    );
    std::fs::remove_dir_all(root).expect("remove fixture root");
}
