//! Captured production native configuration admission retains exact refusals.

use std::sync::atomic::AtomicBool;
use std::time::{Duration, Instant};

use backend_frontend_python::legacy::checker::{
    CheckerError, NativePythonProjectAuthority, PythonProjectConfigurationFault,
    PythonProjectControl, PythonProjectSource, SymbolOutcome,
};
use backend_semantic::ir::SourceIdentity;
use backend_semantic::vocabulary::PythonVersion;

fn control(cancelled: &AtomicBool) -> PythonProjectControl<'_> {
    PythonProjectControl {
        cancelled,
        deadline: Instant::now() + Duration::from_secs(30),
    }
}

#[test]
fn captured_unsupported_and_invalid_configurations_refuse_with_exact_source_binding() {
    let root = std::env::temp_dir().join(format!(
        "nudox-python-captured-config-refusal-{}",
        std::process::id()
    ));
    std::fs::create_dir_all(&root).expect("fixture root");
    let path = root.join("pyproject.toml");
    let checker = NativePythonProjectAuthority::admit().expect("compiled native authority");
    let sources = [PythonProjectSource {
        relative_path: "pkg/__init__.py",
        source: "value = 1\n",
    }];
    let cancelled = AtomicBool::new(false);
    for version in [PythonVersion::Python313, PythonVersion::Python314] {
        for (configuration, unsupported) in [
            (
                "# 🐍 CRLF\r\n[tool.pyrefly]\r\npreset = 'all'\r\n[tool.pyrefly.errors]\r\nmissing-override-decorator = false\r\nimplicit-bool = false\r\nunused-call-result = false\r\n",
                true,
            ),
            ("[tool.pyrefly]\npreset = ['all']\n", false),
        ] {
            std::fs::write(&path, configuration).expect("exact captured config");
            let error = checker
                .analyze_project(&root, "pkg", &sources, version, control(&cancelled))
                .expect_err("native config is never stripped to pass");
            let CheckerError::ProjectConfiguration {
                path: original_path,
                source_identity,
                faults,
                ..
            } = error
            else {
                panic!("typed native configuration fault")
            };
            assert_eq!(original_path, path);
            assert_eq!(
                source_identity,
                *SourceIdentity::from_bytes(configuration.as_bytes())
                    .expect("raw source identity")
                    .identity
            );
            let options = faults
                .iter()
                .filter_map(|fault| match fault {
                    PythonProjectConfigurationFault::UnsupportedDiagnosticOption {
                        option,
                        value,
                        value_span,
                    } => {
                        assert_eq!(
                            &configuration[value_span.start as usize..value_span.end as usize],
                            value.as_ref()
                        );
                        Some(option.as_ref())
                    }
                    _ => None,
                })
                .collect::<Vec<_>>();
            if unsupported {
                assert_eq!(
                    options,
                    [
                        "tool.pyrefly.errors.implicit-bool",
                        "tool.pyrefly.errors.unused-call-result"
                    ]
                );
            } else {
                assert!(options.is_empty());
                assert!(faults.iter().any(|fault| matches!(fault, PythonProjectConfigurationFault::InvalidNativeConfiguration { message, .. } if message.contains("preset"))));
            }
            assert_eq!(
                std::fs::read(&path).expect("raw config preserved"),
                configuration.as_bytes()
            );
        }
    }
    std::fs::remove_dir_all(root).expect("fixture cleanup");
}

#[test]
fn captured_supported_native_configuration_preserves_cross_file_definition() {
    let root = std::env::temp_dir().join(format!(
        "nudox-python-captured-config-positive-{}",
        std::process::id()
    ));
    std::fs::create_dir_all(&root).expect("fixture root");
    let configuration = "[tool.pyrefly]\npreset = 'all'\n[tool.pyrefly.errors]\nmissing-override-decorator = false\n";
    std::fs::write(root.join("pyproject.toml"), configuration).expect("supported config");
    let checker = NativePythonProjectAuthority::admit().expect("compiled native authority");
    let caller = "from .api import run\nrun()\n";
    let sources = [
        PythonProjectSource {
            relative_path: "pkg/__init__.py",
            source: "",
        },
        PythonProjectSource {
            relative_path: "pkg/api.py",
            source: "def run() -> int:\n    return 1\n",
        },
        PythonProjectSource {
            relative_path: "pkg/client.py",
            source: caller,
        },
    ];
    let cancelled = AtomicBool::new(false);
    for version in [PythonVersion::Python313, PythonVersion::Python314] {
        let report = checker
            .analyze_project(&root, "pkg", &sources, version, control(&cancelled))
            .expect("native supported config admission");
        let symbols = &report
            .module("pkg/client.py")
            .expect("caller report")
            .symbols;
        let call_start = caller.rfind("run()").expect("call site") as u32;
        assert!(symbols.iter().any(|symbol| matches!(&symbol.outcome,
            SymbolOutcome::Definition { target, .. }
                if symbol.span.start == call_start && target.relative_path.as_ref() == "pkg/api.py"
                && target.name_span.start == 4 && target.name_span.end == 7
        )));
        report
            .witness()
            .validate_current(control(&cancelled))
            .expect("current exact config witness");
    }
    assert_eq!(
        std::fs::read(root.join("pyproject.toml")).expect("config retained"),
        configuration.as_bytes()
    );
    std::fs::remove_dir_all(root).expect("fixture cleanup");
}
