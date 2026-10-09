//! Live native project joins, fresh transactions, and admission witness guards.

use std::sync::atomic::AtomicBool;
use std::time::{Duration, Instant};

use backend_frontend_python::legacy::checker::{
    CheckerError, InferenceSite, InferredType, NativePythonProjectAuthority, PythonProjectControl,
    PythonProjectCoverageGapKind, PythonProjectSource, SymbolOutcome,
};
use backend_semantic::vocabulary::PythonVersion;

fn publication_control(cancelled: &AtomicBool) -> PythonProjectControl<'_> {
    PythonProjectControl {
        cancelled,
        deadline: Instant::now() + Duration::from_secs(30),
    }
}

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
        .validate_current(publication_control(&cancelled))
        .expect("unchanged witness");
    std::fs::write(root.join("setup.cfg"), "[metadata]\nname = changed\n")
        .expect("new negative config probe");
    assert!(
        report
            .witness()
            .validate_current(publication_control(&cancelled))
            .is_err(),
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
        report
            .witness()
            .validate_current(publication_control(&cancelled))
            .is_err(),
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

#[test]
fn native_compiled_dependency_is_typed_unavailable_with_a_membership_witness() {
    let root = std::env::temp_dir().join(format!(
        "nudox-python-compiled-import-test-{}",
        std::process::id()
    ));
    std::fs::create_dir_all(&root).expect("fixture root");
    let compiled = root.join("native_ext.pyx");
    std::fs::write(&compiled, "cdef int value = 42\n").expect("Cython source, not Python");
    let source = "import native_ext\n\ndef known():\n    return 42\n";
    let sources = [PythonProjectSource {
        relative_path: "core.py",
        source,
    }];
    let checker = NativePythonProjectAuthority::admit().expect("compiled native producer");
    let cancelled = AtomicBool::new(false);
    let report = checker
        .analyze_project(
            &root,
            "pkg",
            &sources,
            PythonVersion::Python314,
            publication_control(&cancelled),
        )
        .expect("an unavailable compiled extension does not refuse the Python project");
    assert!(
        report
            .module("core.py")
            .expect("core module")
            .imports
            .iter()
            .any(|import| import.module == "native_ext" && !import.resolved),
        "no fabricated native-extension binding"
    );
    let gap = report
        .coverage_gaps()
        .iter()
        .find(|gap| gap.kind == PythonProjectCoverageGapKind::UnavailableCompiledImport)
        .expect("typed compiled dependency coverage gap");
    assert_eq!(gap.relative_path.as_ref(), "core.py");
    assert_eq!(
        &source[gap.span.start as usize..gap.span.end as usize],
        "native_ext"
    );
    assert!(
        report
            .diagnostics()
            .iter()
            .any(|diagnostic| diagnostic.kind.as_ref() == "missing-import"),
        "actual native diagnostics remain available"
    );
    report
        .witness()
        .validate_current(publication_control(&cancelled))
        .expect("unchanged candidate witness");
    std::fs::remove_file(&compiled).expect("change compiled candidate membership");
    assert!(
        report
            .witness()
            .validate_current(publication_control(&cancelled))
            .is_err(),
        "compiled candidate presence remains publication-bound"
    );
    let absent = checker
        .analyze_project(
            &root,
            "pkg",
            &sources,
            PythonVersion::Python314,
            publication_control(&cancelled),
        )
        .expect("absent compiled candidate");
    assert_ne!(
        report.witness().fingerprint(),
        absent.witness().fingerprint(),
        "compiled candidate membership changes input identity"
    );
    assert!(
        absent
            .coverage_gaps()
            .iter()
            .any(|gap| gap.kind == PythonProjectCoverageGapKind::UnavailableImport)
    );
    std::fs::remove_dir_all(&root).expect("cleanup");
}

#[test]
fn namespace_and_wildcard_frontiers_refuse_unselected_internal_children() {
    let root = std::env::temp_dir().join(format!(
        "nudox-python-namespace-test-{}",
        std::process::id()
    ));
    std::fs::create_dir_all(root.join("namespace")).expect("namespace directory");
    std::fs::write(root.join("namespace/omitted.pyi"), "value: int\n")
        .expect("omitted namespace child");
    let checker = NativePythonProjectAuthority::admit().expect("compiled native producer");
    let cancelled = AtomicBool::new(false);
    for source in [
        "from namespace import *\n",
        "__import__('namespace.omitted')\n",
    ] {
        let sources = [
            PythonProjectSource {
                relative_path: "core.py",
                source,
            },
            PythonProjectSource {
                relative_path: "namespace/selected.py",
                source: "value = 1\n",
            },
        ];
        assert!(
            matches!(checker.analyze_project(&root, "pkg", &sources, PythonVersion::Python314,
            PythonProjectControl { cancelled: &cancelled, deadline: Instant::now() + Duration::from_secs(30) }),
            Err(CheckerError::IncompleteSourceFrontier { candidate, .. }) if candidate == root.join("namespace/omitted.pyi"))
        );
    }
    std::fs::remove_file(root.join("namespace/omitted.pyi")).expect("restore frontier");
    let sources = [
        PythonProjectSource {
            relative_path: "core.py",
            source: "from importlib import import_module as load\nfrom pkgutil import iter_modules as enumerate_modules\n__import__('external_absent')\nload('external_absent')\nenumerate_modules()\nfrom namespace import *\n",
        },
        PythonProjectSource {
            relative_path: "namespace/selected.py",
            source: "value = 1\n",
        },
    ];
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
        .expect("explicit partial dynamic coverage");
    assert_eq!(
        report
            .coverage_gaps()
            .iter()
            .filter(|gap| gap.kind == PythonProjectCoverageGapKind::DynamicImport)
            .count(),
        2
    );
    assert!(
        report
            .coverage_gaps()
            .iter()
            .any(|gap| gap.kind == PythonProjectCoverageGapKind::ModuleEnumeration)
    );
    assert!(
        report
            .coverage_gaps()
            .iter()
            .any(|gap| gap.kind == PythonProjectCoverageGapKind::WildcardImport)
    );
    std::fs::write(root.join("namespace/new_module.py"), "value = 42\n")
        .expect("new namespace member");
    assert!(
        report
            .witness()
            .validate_current(publication_control(&cancelled))
            .is_err(),
        "namespace membership absence is revalidated"
    );
    std::fs::remove_dir_all(root).expect("remove namespace fixture");
}

#[test]
fn native_unsupported_projection_keeps_its_constructor_and_original_range() {
    let root = std::env::temp_dir().join(format!(
        "nudox-python-unavailable-test-{}",
        std::process::id()
    ));
    std::fs::create_dir_all(&root).expect("fixture root");
    let checker = NativePythonProjectAuthority::admit().expect("compiled native producer");
    let cancelled = AtomicBool::new(false);
    let sources = [PythonProjectSource {
        relative_path: "core.py",
        source: "# 🐍\nunknown = ...\n",
    }];
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
        .expect("typed projection gap");
    let unavailable = report
        .module("core.py")
        .expect("module")
        .inferences
        .iter()
        .find(|inference| matches!(inference.observed, InferredType::Unavailable(_)))
        .expect("native unavailable constructor");
    let diagnostic = report
        .diagnostics()
        .iter()
        .find(|diagnostic| diagnostic.kind.as_ref() == "unavailable-type-projection")
        .expect("projection diagnostic");
    assert_eq!(diagnostic.span, unavailable.site);
    assert!(diagnostic.message.contains("Ellipsis"));
    assert_eq!(
        &sources[0].source[diagnostic.span.start as usize..diagnostic.span.end as usize],
        "unknown"
    );
    std::fs::remove_dir_all(root).expect("remove fixture");
}

#[cfg(unix)]
#[test]
fn source_frontier_ignores_asset_links_but_refuses_python_and_directory_aliases() {
    let root = std::env::temp_dir().join(format!("nudox-python-link-test-{}", std::process::id()));
    std::fs::create_dir_all(root.join("vendor")).expect("excluded source directory");
    std::fs::write(root.join("vendor/omitted.py"), "value = 1\n").expect("excluded source");
    std::fs::write(root.join("image.png"), b"asset").expect("asset");
    std::os::unix::fs::symlink("image.png", root.join("asset-link.png")).expect("asset link");
    let checker = NativePythonProjectAuthority::admit().expect("compiled native producer");
    let cancelled = AtomicBool::new(false);
    let sources = [PythonProjectSource {
        relative_path: "core.py",
        source: "value = 1\n",
    }];
    let report = checker
        .analyze_project(
            &root,
            "pkg",
            &sources,
            PythonVersion::Python314,
            publication_control(&cancelled),
        )
        .expect("irrelevant asset is outside Python coverage");
    report
        .witness()
        .validate_current(publication_control(&cancelled))
        .expect("asset remains irrelevant");
    let expired = PythonProjectControl {
        cancelled: &cancelled,
        deadline: Instant::now() - Duration::from_secs(1),
    };
    assert!(matches!(
        report.witness().validate_current(expired),
        Err(CheckerError::Deadline { phase: "project" })
    ));
    cancelled.store(true, std::sync::atomic::Ordering::Release);
    assert!(matches!(
        report
            .witness()
            .validate_current(publication_control(&cancelled)),
        Err(CheckerError::Cancelled { phase: "project" })
    ));
    cancelled.store(false, std::sync::atomic::Ordering::Release);
    std::os::unix::fs::symlink("image.png", root.join("unsafe.py")).expect("source alias");
    assert!(
        matches!(checker.analyze_project(&root, "pkg", &sources, PythonVersion::Python314, publication_control(&cancelled)), Err(CheckerError::UncapturedDependency { path }) if path == root.join("unsafe.py"))
    );
    std::fs::remove_file(root.join("unsafe.py")).expect("remove source alias");
    std::os::unix::fs::symlink("vendor", root.join("unsafe_namespace")).expect("namespace alias");
    assert!(
        matches!(checker.analyze_project(&root, "pkg", &sources, PythonVersion::Python314, publication_control(&cancelled)), Err(CheckerError::UncapturedDependency { path }) if path == root.join("unsafe_namespace"))
    );
    std::fs::remove_dir_all(root).expect("remove fixture");
}

#[test]
fn native_relative_import_aliases_retain_exact_selected_function_coordinates()
-> Result<(), Box<dyn std::error::Error>> {
    let root = std::env::temp_dir().join(format!(
        "nudox-python-function-coordinates-{}",
        std::process::id()
    ));
    std::fs::create_dir_all(&root)?;
    let sources = [
        PythonProjectSource {
            relative_path: "core/__init__.py",
            source: "",
        },
        PythonProjectSource {
            relative_path: "core/api/__init__.py",
            source: "",
        },
        PythonProjectSource {
            relative_path: "core/utils.py",
            source: "def generate_s3_authorization_headers(key):\n    return 'wrong module'\n",
        },
        PythonProjectSource {
            relative_path: "core/api/utils.py",
            source: "# UTF-8: 🐍\ndef generate_s3_authorization_headers(key):\n    return key\n",
        },
        PythonProjectSource {
            relative_path: "core/api/viewsets.py",
            source: "from . import utils as helper\nfrom .utils import generate_s3_authorization_headers as generate\nfrom external import generate_s3_authorization_headers as external\n\ndef generate_s3_authorization_headers(key):\n    return 'same name in caller'\n\ndef qualified(key):\n    return helper.generate_s3_authorization_headers(key)\n\ndef direct(key):\n    return generate(key)\n\ndef shadowed(helper, key):\n    return helper.generate_s3_authorization_headers(key)\n\ndef shadowed_local(generate_s3_authorization_headers, key):\n    return generate_s3_authorization_headers(key)\n\ndef unselected(key):\n    return external(key)\n",
        },
    ];
    let checker = NativePythonProjectAuthority::admit()?;
    let cancelled = AtomicBool::new(false);
    let report = checker.analyze_project(
        &root,
        "docs",
        &sources,
        PythonVersion::Python314,
        publication_control(&cancelled),
    )?;
    let caller = report
        .module("core/api/viewsets.py")
        .ok_or("caller report absent")?;
    let source = sources[4].source;
    let mut proven = 0;
    for symbol in &caller.symbols {
        eprintln!("native reference control: {symbol:?}");
    }
    for symbol in &caller.symbols {
        let position = symbol.span.start as usize;
        match &symbol.outcome {
            SymbolOutcome::Definition { target, .. } => {
                assert_eq!(
                    target.relative_path.as_ref(),
                    "core/api/utils.py",
                    "native occurrence {symbol:?}"
                );
                let coordinate =
                    backend_semantic::ir::PythonSourceCoordinate::decode(&target.source_coordinate)
                        .ok_or("native coordinate not admitted")?;
                assert_eq!(coordinate.0.path, "core/api/utils.py");
                assert_eq!(
                    coordinate.0.source,
                    *backend_semantic::ir::SourceIdentity::from_bytes(sources[3].source.as_bytes())
                        .ok_or("source extent")?
                        .identity
                );
                assert!(position < source.find("def shadowed").ok_or("shadowed control")?);
                proven += 1;
            }
            _ if position >= source.find("def shadowed").ok_or("shadowed control")? => {
                assert!(
                    matches!(symbol.outcome, SymbolOutcome::Unresolved),
                    "unproven callable {symbol:?}"
                );
            }
            _ => return Err("unambiguous selected relative import failed resolution".into()),
        }
    }
    assert_eq!(
        proven, 2,
        "qualified module alias and direct import alias must reach the exact local function"
    );
    assert!(
        caller
            .symbols
            .iter()
            .any(|symbol| symbol.span.start as usize
                >= source.find("def unselected").unwrap_or(source.len())
                && matches!(symbol.outcome, SymbolOutcome::Unresolved))
    );
    report
        .witness()
        .validate_current(publication_control(&cancelled))?;
    std::fs::remove_dir_all(root)?;
    Ok(())
}

#[test]
fn native_empty_package_initializers_are_not_named_declaration_targets()
-> Result<(), Box<dyn std::error::Error>> {
    let root = std::env::temp_dir().join(format!(
        "nudox-python-empty-package-target-{}",
        std::process::id()
    ));
    std::fs::create_dir_all(&root)?;
    let sources = [
        PythonProjectSource {
            relative_path: "pkg/__init__.py",
            source: "",
        },
        PythonProjectSource {
            relative_path: "pkg/cli/__init__.py",
            source: "",
        },
        PythonProjectSource {
            relative_path: "pkg/cli/argparser.py",
            source: "class HTTPieArgumentParser:\n    def __init__(self):\n        self.ready = True\n\ndef build_parser():\n    return HTTPieArgumentParser()\n",
        },
        PythonProjectSource {
            relative_path: "test_cli.py",
            source: "import pkg.cli.argparser\n\nclass TestArgumentParser:\n    def setup_method(self):\n        self.parser = pkg.cli.argparser.HTTPieArgumentParser()\n        self.second = pkg.cli.argparser.build_parser()\n\ndef invalid_module_call():\n    return pkg.cli()\n",
        },
    ];
    for source in &sources {
        let path = root.join(source.relative_path);
        std::fs::create_dir_all(path.parent().ok_or("source parent")?)?;
        std::fs::write(path, source.source)?;
    }
    let cancelled = AtomicBool::new(false);
    let report = NativePythonProjectAuthority::admit()?.analyze_project(
        &root,
        "pkg",
        &sources,
        PythonVersion::Python314,
        publication_control(&cancelled),
    )?;
    for source in &sources {
        assert!(report.module(source.relative_path).is_some());
    }
    let caller = report.module("test_cli.py").ok_or("caller report")?;
    let positive = sources[3]
        .source
        .find("HTTPieArgumentParser()")
        .ok_or("class call")?;
    let symbol = caller
        .symbols
        .iter()
        .find(|symbol| symbol.span.start as usize == positive)
        .ok_or("class call occurrence")?;
    let SymbolOutcome::Definition { target, callee } = &symbol.outcome else {
        return Err("qualified cross-file class call lost its native target".into());
    };
    assert_eq!(target.relative_path.as_ref(), "pkg/cli/argparser.py");
    assert_eq!(
        target.kind,
        backend_frontend_python::legacy::DeclarationKind::Class
    );
    let coordinate =
        backend_semantic::ir::PythonSourceCoordinate::decode(&target.source_coordinate)
            .ok_or("class coordinate")?;
    assert_eq!(coordinate.0.path, target.relative_path.as_ref());
    assert_eq!(
        coordinate.0.source,
        *backend_semantic::ir::SourceIdentity::from_bytes(sources[2].source.as_bytes())
            .ok_or("target source extent")?
            .identity
    );
    let manifest = sources
        .iter()
        .map(|source| {
            Ok((
                source.relative_path.to_owned(),
                *backend_semantic::ir::SourceIdentity::from_bytes(source.source.as_bytes())
                    .ok_or("manifest source extent")?
                    .identity,
            ))
        })
        .collect::<Result<Vec<_>, &'static str>>()?;
    assert_eq!(
        coordinate.0.program,
        backend_semantic::ir::python_program_identity(&manifest).ok_or("program identity")?
    );
    assert_eq!(
        callee.as_ref().ok_or("constructor callee")?.name.as_ref(),
        "__init__"
    );
    let function_call = sources[3]
        .source
        .find("build_parser()")
        .ok_or("function call")?;
    let function = caller
        .symbols
        .iter()
        .find_map(|symbol| match &symbol.outcome {
            SymbolOutcome::Definition { target, .. }
                if symbol.span.start as usize == function_call =>
            {
                Some(target)
            }
            _ => None,
        })
        .ok_or("cross-file function target")?;
    assert_eq!(function.relative_path.as_ref(), "pkg/cli/argparser.py");
    assert_eq!(
        function.kind,
        backend_frontend_python::legacy::DeclarationKind::Function
    );
    assert_eq!(function.name.as_ref(), "build_parser");
    let function_coordinate =
        backend_semantic::ir::PythonSourceCoordinate::decode(&function.source_coordinate)
            .ok_or("function coordinate")?;
    assert_eq!(function_coordinate.0.program, coordinate.0.program);
    assert_eq!(function_coordinate.0.source, coordinate.0.source);
    assert_eq!(
        &sources[2].source[function.name_span.start as usize..function.name_span.end as usize],
        "build_parser"
    );
    let module_call = sources[3].source.rfind("cli()").ok_or("module call")?;
    assert!(caller.symbols.iter().any(|symbol| {
        symbol.span.start as usize == module_call
            && matches!(symbol.outcome, SymbolOutcome::Unresolved)
    }));
    assert!(caller.symbols.iter().all(|symbol| {
        !matches!(&symbol.outcome, SymbolOutcome::Definition { target, .. }
            if target.kind == backend_frontend_python::legacy::DeclarationKind::Module)
    }));
    report
        .witness()
        .validate_current(publication_control(&cancelled))?;
    std::fs::remove_dir_all(root)?;
    Ok(())
}

#[test]
fn selected_package_root_preserves_real_module_leaf_and_native_call_coordinates()
-> Result<(), Box<dyn std::error::Error>> {
    let parent =
        std::env::temp_dir().join(format!("nudox-python-package-root-{}", std::process::id()));
    let root = parent.join("mealie");
    std::fs::create_dir_all(root.join("core/security"))?;
    // An ambient sibling is deliberately present, but is never mirrored or
    // admitted by the selected package's finite source frontier.
    std::fs::write(parent.join("outside.py"), "def outside(): return 99\n")?;
    let sources = [
        PythonProjectSource {
            relative_path: "__init__.py",
            source: "__version__ = \"develop\"\n",
        },
        PythonProjectSource {
            relative_path: "core/__init__.py",
            source: "",
        },
        PythonProjectSource {
            relative_path: "core/config.py",
            source: "def get_app_settings() -> int:\n    return 42\n",
        },
        PythonProjectSource {
            relative_path: "core/security/__init__.py",
            source: "",
        },
        PythonProjectSource {
            relative_path: "core/security/hasher.py",
            source: "from mealie.core.config import get_app_settings\nfrom ..config import get_app_settings as relative\nfrom outside import outside\n\ndef get_hasher():\n    return get_app_settings() + relative()\n\ndef uncaptured():\n    return outside()\n",
        },
    ];
    for source in &sources {
        std::fs::write(root.join(source.relative_path), source.source)?;
    }
    let cancelled = AtomicBool::new(false);
    let checker = NativePythonProjectAuthority::admit()?;
    let report = checker.analyze_project(
        &root,
        "mealie",
        &sources,
        PythonVersion::Python314,
        publication_control(&cancelled),
    )?;
    let caller = report
        .module("core/security/hasher.py")
        .ok_or("caller report")?;
    let call_source = sources[4].source;
    for spelling in ["get_app_settings() +", "relative()"] {
        let position = call_source.find(spelling).ok_or("call source")?;
        let symbol = caller
            .symbols
            .iter()
            .find(|symbol| symbol.span.start as usize == position)
            .ok_or("native call occurrence")?;
        let SymbolOutcome::Definition { target, .. } = &symbol.outcome else {
            return Err("selected package cross-file call unresolved".into());
        };
        assert_eq!(target.relative_path.as_ref(), "core/config.py");
        assert_eq!(
            target.qualified_name.as_ref(),
            "mealie.core.config.get_app_settings"
        );
        let coordinate =
            backend_semantic::ir::PythonSourceCoordinate::decode(&target.source_coordinate)
                .ok_or("native coordinate")?;
        assert_eq!(coordinate.0.path, "core/config.py");
        assert_eq!(
            coordinate.0.source,
            *backend_semantic::ir::SourceIdentity::from_bytes(sources[2].source.as_bytes())
                .ok_or("source identity")?
                .identity
        );
    }
    let position = call_source.rfind("outside()").ok_or("ambient call")?;
    assert!(
        caller
            .symbols
            .iter()
            .any(|symbol| symbol.span.start as usize == position
                && matches!(symbol.outcome, SymbolOutcome::Unresolved))
    );
    report
        .witness()
        .validate_current(publication_control(&cancelled))?;
    // Explicit parent search paths remain refused, even though the native
    // heuristic uses a separate, finite owned parent for this named package.
    std::fs::write(root.join("pyrefly.toml"), "search-path = [\"..\"]\n")?;
    assert!(matches!(
        checker.analyze_project(
            &root,
            "mealie",
            &sources,
            PythonVersion::Python314,
            publication_control(&cancelled)
        ),
        Err(CheckerError::UncapturedDependency { .. })
    ));
    std::fs::remove_dir_all(parent)?;
    Ok(())
}

#[cfg(any(unix, windows))]
#[test]
fn captured_baseline_preserves_native_cross_file_facts_and_unfiltered_diagnostics()
-> Result<(), Box<dyn std::error::Error>> {
    let root = std::env::temp_dir().join(format!(
        "nudox-python-captured-baseline-native-{}",
        std::process::id()
    ));
    std::fs::create_dir_all(root.join("src"))?;
    let sources = [
        PythonProjectSource {
            relative_path: "src/api.py",
            source: "def compute_checksum() -> int:\n    return 'wrong'\n",
        },
        PythonProjectSource {
            relative_path: "src/consumer.py",
            source: "from api import compute_checksum\n\ndef consume() -> int:\n    return compute_checksum()\n",
        },
    ];
    for source in &sources {
        std::fs::write(root.join(source.relative_path), source.source)?;
    }
    let configuration = "[tool.pyrefly]\npython-platform = \"linux\"\nsearch-path = [\"src\"]\nbaseline = \".pyrefly-baseline.json\"\n";
    std::fs::write(root.join("pyproject.toml"), configuration)?;
    let baseline = r#"{"errors":[{"column":12,"path":"src/api.py","name":"bad-return","concise_description":"retained native diagnostic","severity":"error"}]}"#;
    std::fs::write(root.join(".pyrefly-baseline.json"), baseline)?;
    let cancelled = AtomicBool::new(false);
    let checker = NativePythonProjectAuthority::admit()?;
    #[cfg(unix)]
    {
        let alias = root.with_extension("root-alias");
        std::os::unix::fs::symlink(&root, &alias)?;
        for spelling in [alias.clone(), alias.join(""), alias.join(".")] {
            let refusal = checker.analyze_project(
                &spelling,
                "paperless-control",
                &sources,
                PythonVersion::Python314,
                publication_control(&cancelled),
            );
            assert!(matches!(refusal, Err(CheckerError::PackageRoot { .. })));
        }
        std::fs::remove_file(&alias)?;
    }
    for _ in 0..2 {
        let report = checker.analyze_project(
            &root,
            "paperless-control",
            &sources,
            PythonVersion::Python314,
            publication_control(&cancelled),
        )?;
        let caller = report
            .module("src/consumer.py")
            .ok_or("native caller report")?;
        let position = sources[1]
            .source
            .rfind("compute_checksum()")
            .ok_or("call")?;
        let symbol = caller
            .symbols
            .iter()
            .find(|symbol| symbol.span.start as usize == position)
            .ok_or("native call occurrence")?;
        let SymbolOutcome::Definition { target, .. } = &symbol.outcome else {
            return Err("captured baseline changed native call binding".into());
        };
        assert_eq!(target.relative_path.as_ref(), "src/api.py");
        assert_eq!(target.name.as_ref(), "compute_checksum");
        assert_eq!(
            &sources[0].source[target.name_span.start as usize..target.name_span.end as usize],
            "compute_checksum"
        );
        // Existing raw diagnostics remain unfiltered: admitting baseline policy
        // bytes does not suppress or manufacture selected-source semantic facts.
        assert!(
            report
                .diagnostics()
                .iter()
                .any(
                    |diagnostic| diagnostic.relative_path.as_ref() == "src/api.py"
                        && diagnostic.kind.as_ref() == "bad-return"
                )
        );
        report
            .witness()
            .validate_current(publication_control(&cancelled))?;
        std::fs::write(root.join(".pyrefly-baseline.json"), "{\"errors\":[]}")?;
        assert!(
            report
                .witness()
                .validate_current(publication_control(&cancelled))
                .is_err()
        );
        std::fs::write(root.join(".pyrefly-baseline.json"), baseline)?;
    }
    std::fs::remove_dir_all(&root)?;
    Ok(())
}

#[test]
#[ignore = "requires the complete pinned Mealie879b Python package; native authority only"]
fn pinned_mealie_selected_package_preserves_all464_sources_and_known_call()
-> Result<(), Box<dyn std::error::Error>> {
    use backend_frontend_python::legacy::checker::is_ignored_python_source_directory;
    let root = std::path::PathBuf::from(
        std::env::var_os("NUDOX_TEST_PINNED_MEALIE_PACKAGE")
            .ok_or("explicit pinned package fixture is required")?,
    );
    assert!(root.is_absolute());
    assert_eq!(
        root.file_name().and_then(|leaf| leaf.to_str()),
        Some("mealie")
    );
    let mut pending = vec![root.clone()];
    let mut owned = Vec::new();
    while let Some(directory) = pending.pop() {
        for entry in std::fs::read_dir(directory)? {
            let entry = entry?;
            let kind = entry.file_type()?;
            if kind.is_dir() && !is_ignored_python_source_directory(&entry.file_name()) {
                pending.push(entry.path());
            } else if entry
                .path()
                .extension()
                .is_some_and(|ext| ext == "py" || ext == "pyi")
            {
                assert!(
                    kind.is_file() && !kind.is_symlink(),
                    "fixture source must be regular"
                );
                assert!(
                    entry.metadata()?.len() <= 1024 * 1024,
                    "bounded authentic fixture source"
                );
                let path = entry
                    .path()
                    .strip_prefix(&root)?
                    .to_str()
                    .ok_or("source path UTF-8")?
                    .to_owned();
                owned.push((path, std::fs::read_to_string(entry.path())?));
            }
        }
    }
    owned.sort_by(|left, right| left.0.cmp(&right.0));
    assert_eq!(owned.len(), 464, "all pinned package sources retained");
    assert_eq!(
        owned.iter().map(|(_, source)| source.len()).sum::<usize>(),
        1_564_950
    );
    assert_eq!(
        owned
            .iter()
            .find(|(path, _)| path == "__init__.py")
            .ok_or("root initializer")?
            .1,
        "__version__ = \"develop\"\n"
    );
    let sources = owned
        .iter()
        .map(|(path, source)| PythonProjectSource {
            relative_path: path,
            source,
        })
        .collect::<Vec<_>>();
    let target_source = owned
        .iter()
        .find(|(path, _)| path == "core/config.py")
        .ok_or("target source")?
        .1
        .as_str();
    let caller_source = owned
        .iter()
        .find(|(path, _)| path == "core/security/hasher.py")
        .ok_or("caller source")?
        .1
        .as_str();
    assert_eq!(&caller_source[1062..1078], "get_app_settings");
    assert_eq!(
        1 + caller_source[..1062]
            .bytes()
            .filter(|byte| *byte == b'\n')
            .count(),
        40
    );
    let target_name = target_source
        .find("def get_app_settings")
        .ok_or("target declaration")?
        + 4;
    assert_eq!(
        1 + target_source[..target_name]
            .bytes()
            .filter(|byte| *byte == b'\n')
            .count(),
        42
    );
    let manifest = sources
        .iter()
        .map(|source| {
            Ok((
                source.relative_path.to_owned(),
                *backend_semantic::ir::SourceIdentity::from_bytes(source.source.as_bytes())
                    .ok_or("source extent")?
                    .identity,
            ))
        })
        .collect::<Result<Vec<_>, &'static str>>()?;
    let program =
        backend_semantic::ir::python_program_identity(&manifest).ok_or("program identity")?;
    let cancelled = AtomicBool::new(false);
    let checker = NativePythonProjectAuthority::admit()?.with_timeout(Duration::from_secs(90));
    for _ in 0..2 {
        let control = PythonProjectControl {
            cancelled: &cancelled,
            deadline: Instant::now() + Duration::from_secs(90),
        };
        let report = checker.analyze_project(
            &root,
            "mealie",
            &sources,
            PythonVersion::Python314,
            control,
        )?;
        for source in &sources {
            assert!(report.module(source.relative_path).is_some());
        }
        let caller = report
            .module("core/security/hasher.py")
            .ok_or("caller report")?;
        let symbol = caller
            .symbols
            .iter()
            .find(|symbol| symbol.span.start == 1062 && symbol.span.end == 1078)
            .ok_or("exact native known call")?;
        let SymbolOutcome::Definition { target, .. } = &symbol.outcome else {
            return Err("pinned native call unresolved".into());
        };
        assert_eq!(target.relative_path.as_ref(), "core/config.py");
        assert_eq!(target.name_span.start as usize, target_name);
        let coordinate =
            backend_semantic::ir::PythonSourceCoordinate::decode(&target.source_coordinate)
                .ok_or("target native coordinate")?;
        assert_eq!(coordinate.0.path, "core/config.py");
        assert_eq!(coordinate.0.program, program);
        assert_eq!(
            coordinate.0.source,
            *backend_semantic::ir::SourceIdentity::from_bytes(target_source.as_bytes())
                .ok_or("target source identity")?
                .identity
        );
        report.witness().validate_current(control)?;
    }
    Ok(())
}
