//! Ordinary lifecycle falsifiers for typed local-host compiler admission.

use std::{ffi::OsString, fs, path::PathBuf};

use backend_engine::application::{
    LocalCompilerHost, LocalCompilerHostError, LocalHostDiscovery, LocalHostEnvironment,
    LocalHostVariable,
};
use backend_library::interface::{
    CompilerCapability, CompilerReadiness, CompilerToolFailure, CompilerToolIssue,
    CompilerToolRequirement,
};
use backend_semantic::vocabulary::{LanguageProfile, NativeTool, PythonVersion};

#[derive(Clone)]
struct ExplicitDataRoot {
    root: PathBuf,
}

impl LocalHostEnvironment for ExplicitDataRoot {
    fn value(&self, variable: LocalHostVariable) -> Option<OsString> {
        (variable == LocalHostVariable::NudoxDataRoot).then(|| self.root.clone().into_os_string())
    }
}

#[derive(Clone)]
struct PythonToolEnvironment {
    root: PathBuf,
    python: Option<PathBuf>,
    pyrefly: Option<PathBuf>,
    search_path: OsString,
}

impl LocalHostEnvironment for PythonToolEnvironment {
    fn value(&self, variable: LocalHostVariable) -> Option<OsString> {
        match variable {
            LocalHostVariable::NudoxDataRoot => Some(self.root.clone().into_os_string()),
            LocalHostVariable::NudoxPython => self.python.clone().map(PathBuf::into_os_string),
            LocalHostVariable::NudoxPyrefly => self.pyrefly.clone().map(PathBuf::into_os_string),
            _ => None,
        }
    }

    fn search_path(&self) -> Option<OsString> {
        Some(self.search_path.clone())
    }
}

#[cfg(unix)]
fn executable_script(directory: &std::path::Path, name: &str, body: &str) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;

    let path = directory.join(name);
    fs::write(&path, body).expect("write fake tool");
    fs::set_permissions(&path, fs::Permissions::from_mode(0o700))
        .expect("make fake tool executable");
    path
}

fn fresh_root(label: &str) -> PathBuf {
    static NEXT_ROOT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let ordinal = NEXT_ROOT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    std::env::temp_dir().join(format!(
        "nudox-local-host-{label}-{}-{ordinal}",
        std::process::id()
    ))
}

#[test]
fn explicit_only_host_starts_a_ready_owner_without_ambient_tool_lookup() {
    let root = fresh_root("ready");
    let host = LocalCompilerHost::new(
        ExplicitDataRoot { root: root.clone() },
        LocalHostDiscovery::ExplicitOnly,
    );
    let client = host
        .open()
        .expect("an explicit durable root admits an honestly tool-unavailable runtime");
    assert_eq!(client.readiness(), CompilerReadiness::Ready);
    drop(client);
    fs::remove_dir_all(root).expect("the stopped owner releases its exact fixture root");
}

#[cfg(unix)]
#[test]
fn python_checker_setup_is_explicit_typed_and_distinct_from_python_interpreter() {
    let root = fresh_root("python-checker");
    let bin = root.join("path-bin");
    fs::create_dir_all(&bin).expect("create tool fixture directory");
    let python_path = executable_script(
        &bin,
        "python3",
        "#!/bin/sh\nprintf '%s\n' 'Python 3.14.0'\nexit 0\n",
    );
    let pyrefly_ok = executable_script(
        &bin,
        "pyrefly",
        "#!/bin/sh\nprintf '%s\n' 'pyrefly 1.2.0'\nexit 0\n",
    );
    let pyrefly_bad = executable_script(
        &bin,
        "pyrefly-bad",
        "#!/bin/sh\nprintf '%s\n' 'broken checker'\nexit 19\n",
    );
    let pyrefly_changed = executable_script(
        &bin,
        "pyrefly-changed",
        "#!/bin/sh\nprintf '%s\n' 'pyrefly 1.2.1'\nexit 0\n",
    );
    let search_path = std::env::join_paths([&bin]).expect("one isolated search path");
    let inspect = |python: Option<PathBuf>, pyrefly: Option<PathBuf>| {
        let host = LocalCompilerHost::new(
            PythonToolEnvironment {
                root: root.clone(),
                python,
                pyrefly,
                search_path: search_path.clone(),
            },
            LocalHostDiscovery::ExplicitOnly,
        );
        host.inspect_capabilities()
            .expect("closed Python capability inspection")
            .for_profile(LanguageProfile::Python(PythonVersion::Python314))
    };

    let missing_interpreter = inspect(None, Some(pyrefly_ok.clone()));
    assert_eq!(
        missing_interpreter.setup_issue(),
        Some(CompilerToolIssue {
            requirement: CompilerToolRequirement::Native(NativeTool::Python),
            failure: CompilerToolFailure::Missing,
        })
    );

    // The compiled State producer works with no external checker/interpreter.
    let native = inspect(None, None);
    assert_eq!(
        native.state(),
        backend_engine::application::LocalCompilerCapabilityState::Ready
    );
    assert_eq!(native.setup_issue(), None);
    assert!(native.toolchain_identity().is_some());
    assert!(native.local_authority_fingerprint().is_some());
    assert!(native.manifest().is_some());
    assert_ne!(
        native.toolchain_identity(),
        inspect(Some(python_path.clone()), Some(pyrefly_ok.clone())).toolchain_identity(),
        "compiled native producer and external interpreter are distinct identities"
    );

    let failed_probe = inspect(Some(python_path.clone()), Some(pyrefly_bad));
    assert_eq!(
        failed_probe.setup_issue(),
        Some(CompilerToolIssue {
            requirement: CompilerToolRequirement::PythonChecker,
            failure: CompilerToolFailure::ProbeFailed,
        })
    );

    let admitted = inspect(Some(python_path), Some(pyrefly_ok));
    assert_eq!(
        admitted.state(),
        backend_engine::application::LocalCompilerCapabilityState::Ready
    );
    assert_eq!(admitted.setup_issue(), None);
    assert!(admitted.toolchain_identity().is_some());
    assert!(admitted.local_authority_fingerprint().is_some());
    assert!(admitted.manifest().is_some());

    let changed_checker = inspect(Some(bin.join("python3")), Some(pyrefly_changed));
    assert_eq!(changed_checker.state(), admitted.state());
    assert_eq!(
        changed_checker.toolchain_identity(),
        admitted.toolchain_identity()
    );
    assert_ne!(
        changed_checker.local_authority_fingerprint(),
        admitted.local_authority_fingerprint(),
        "the exact Pyrefly version remains bound separately from Python",
    );

    assert!(
        !root.join("artifacts").exists(),
        "inspection must not create artifacts"
    );
    assert!(
        !root.join("journal").exists(),
        "inspection must not create a journal"
    );
    fs::remove_dir_all(&root).expect("remove isolated tool fixture");
}

#[test]
fn relative_data_root_is_rejected_with_its_typed_variable_and_path() {
    let root = PathBuf::from("relative-host-root");
    let host = LocalCompilerHost::new(
        ExplicitDataRoot { root: root.clone() },
        LocalHostDiscovery::ExplicitOnly,
    );
    let error = match host.open() {
        Ok(_client) => panic!("relative storage cannot become durable publication authority"),
        Err(error) => error,
    };
    assert!(matches!(
        error,
        LocalCompilerHostError::RelativeEnvironmentPath {
            variable: LocalHostVariable::NudoxDataRoot,
            path,
        } if path.as_ref() == root
    ));
}

/// Selects exactly the variables a user would export for an explicit Rust toolchain.
#[derive(Clone)]
struct ExplicitRust {
    root: PathBuf,
    rustc: PathBuf,
    cargo: PathBuf,
    cargo_home: PathBuf,
}

impl LocalHostEnvironment for ExplicitRust {
    fn value(&self, variable: LocalHostVariable) -> Option<OsString> {
        match variable {
            LocalHostVariable::NudoxDataRoot => Some(self.root.clone().into_os_string()),
            LocalHostVariable::NudoxRustc => Some(self.rustc.clone().into_os_string()),
            LocalHostVariable::NudoxCargo => Some(self.cargo.clone().into_os_string()),
            LocalHostVariable::NudoxCargoHome => Some(self.cargo_home.clone().into_os_string()),
            _ => None,
        }
    }
}

/// Finds `tool` the way a shell does: the first `PATH` entry holding it under the host
/// executable suffix.
fn on_path(tool: &str) -> Option<PathBuf> {
    let name = format!("{tool}{}", std::env::consts::EXE_SUFFIX);
    std::env::split_paths(&std::env::var_os("PATH")?)
        .map(|directory| directory.join(&name))
        .find(|candidate| candidate.is_file())
}

/// The owner used to refuse to start with "Rustc version probe exited with exit code: 1" when
/// `NUDOX_RUSTC` named a rustup proxy: the host resolved the proxy link to the `rustup` binary,
/// which rejects `--print sysroot`. Selecting the toolchain as a user exports it must start.
#[test]
fn explicit_rust_toolchain_selected_through_its_proxies_starts_a_ready_owner() {
    let rustc = on_path("rustc").expect("a cargo test environment has rustc on PATH");
    let cargo = std::env::var_os("CARGO")
        .map(PathBuf::from)
        .filter(|path| path.is_absolute() && path.is_file())
        .or_else(|| on_path("cargo"))
        .expect("a cargo test environment provides cargo");
    let cargo_home = std::env::var_os("CARGO_HOME")
        .map(PathBuf::from)
        .filter(|path| path.is_absolute() && path.is_dir())
        .or_else(|| {
            let home = std::env::var_os("USERPROFILE").or_else(|| std::env::var_os("HOME"))?;
            Some(PathBuf::from(home).join(".cargo")).filter(|path| path.is_dir())
        })
        .expect("a cargo test environment has a Cargo home");
    let root = fresh_root("rust-proxy");
    let host = LocalCompilerHost::new(
        ExplicitRust {
            root: root.clone(),
            rustc,
            cargo,
            cargo_home,
        },
        LocalHostDiscovery::ExplicitOnly,
    );
    let client = host
        .open()
        .unwrap_or_else(|error| panic!("an explicit Rust toolchain must start the owner: {error}"));
    assert_eq!(client.readiness(), CompilerReadiness::Ready);
    drop(client);
    fs::remove_dir_all(root).expect("the stopped owner releases its exact fixture root");
}

#[test]
fn compiled_python_authority_is_ready_with_empty_path_and_no_external_tools() {
    let root = fresh_root("native-python-empty-path");
    let host = LocalCompilerHost::new(
        PythonToolEnvironment {
            root: root.clone(),
            python: None,
            pyrefly: None,
            search_path: OsString::new(),
        },
        LocalHostDiscovery::ExplicitOnly,
    );
    let native = host
        .inspect_capabilities()
        .expect("actual compiled producer admission")
        .for_profile(LanguageProfile::Python(PythonVersion::Python314));
    assert_eq!(
        native.state(),
        backend_engine::application::LocalCompilerCapabilityState::Ready
    );
    assert_eq!(native.setup_issue(), None);
    assert!(native.manifest().is_some());
    assert!(
        !root.exists(),
        "inspection does not create durable setup state"
    );
}

#[test]
fn compiled_python_authority_lowers_missing_external_imports_with_empty_path() {
    use backend_engine::application::{OwnedPackageSource, OwnedPackageSourceSet};
    use backend_library::interface::{
        CorrelationId, GenerateTarget, PackageCompileRequest, PackageUrl,
    };
    use backend_semantic::vocabulary::Stage;

    let root = fresh_root("native-python-package");
    let package_root = root.join("project");
    fs::create_dir_all(package_root.join("src/sample")).expect("source root");
    let modules = [
        (
            "setup.py",
            "import sys\n\nif sys.version_info < (3, 10):\n    sys.stderr.write('Requires Python 3.10 or later.\\n')\n    sys.exit(1)\n\nfrom setuptools import setup\n\nsetup()\n",
        ),
        ("src/sample/__init__.py", "from .session import Session\n"),
        (
            "src/sample/session.py",
            "from absent_dependency import missing\n\nclass Session:\n    def label(self):\n        return 'session'\n",
        ),
    ];
    for (path, source) in &modules {
        fs::write(package_root.join(path), source).expect("exact source bytes");
    }
    let host = LocalCompilerHost::new(
        PythonToolEnvironment {
            root: root.join("compiler"),
            python: None,
            pyrefly: None,
            search_path: OsString::new(),
        },
        LocalHostDiscovery::ExplicitOnly,
    );
    let client = host
        .open()
        .expect("compiled native owner without external tools");
    let request = PackageCompileRequest::new(
        GenerateTarget {
            correlation: CorrelationId(97),
            profile: LanguageProfile::Python(PythonVersion::Python314),
            stage: Stage::LowerIr,
        },
        PackageUrl::try_from("pkg:pypi/sample@1.0.0".to_owned()).expect("package URL"),
    )
    .expect("package profile");
    let sources = modules
        .iter()
        .map(|(path, source)| OwnedPackageSource::new(path, source).expect("exact module source"))
        .collect::<Vec<_>>()
        .into_boxed_slice();
    let staged = client
        .compile_package_sources_staged(
            OwnedPackageSourceSet::new(request.clone(), package_root.clone(), sources.clone())
                .expect("complete selected frontier"),
        )
        .unwrap_or_else(|error| {
            panic!("compiled native package must retain unavailable dependency coverage: {error:?}")
        });
    assert!(
        staged.output_object_count() > 0,
        "real canonical staged output"
    );
    // A publication owner binds its candidate to an exact opaque scan claim.
    // Native Python project admission must not silently replace those roots.
    let claim = backend_semantic::ir::SemanticInputWitness::claimed_state(
        [0x71; 32],
        backend_version::ScopeRoot::from_bytes([0x83; 32]),
        backend_version::Coverage::Partial,
    );
    let claimed = client
        .compile_package_sources_staged(
            OwnedPackageSourceSet::new(request, package_root, sources)
                .expect("same exact selected frontier")
                .with_input_claim(claim),
        )
        .expect("native authority preserves the caller's exact attempt claim");
    assert_eq!(claimed.input_witness(), claim);
    assert!(claimed.plane_execution_identity().is_some());
    assert_ne!(
        claimed.plane_execution_identity(),
        staged.plane_execution_identity(),
        "execution identity must remain bound to the exact input provenance"
    );
    drop(claimed);
    drop(staged);
    drop(client);
    fs::remove_dir_all(root).expect("the stopped owner releases its exact fixture root");
}
