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
        let position = symbol.span.start as usize;
        match &symbol.outcome {
            SymbolOutcome::Definition { target, .. } => {
                assert_eq!(target.relative_path.as_ref(), "core/api/utils.py");
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
            _ if position >= source.find("def shadowed").ok_or("shadowed control")? => {}
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
