//! Canonical native-tool rows and package semantic-authority ownership.

use std::{
    path::{Path, PathBuf},
    str,
};

use arrayvec::ArrayVec;
use backend_frontend_clang::ClangAuthorityEnvironment;
use backend_frontend_csharp::legacy::{CSharpOracle, DEFAULT_SOURCE_LIMIT};
use backend_frontend_go::legacy::oracle::{GoOracleChildEnvironment, GoOracleConfigurationError};
use backend_frontend_go::legacy::{GoOracle, GoOracleConfiguration};
use backend_frontend_java::legacy::harness::JdkToolchain;
use backend_frontend_python::legacy::Pyrefly;
use backend_frontend_python::legacy::checker::NativePythonProjectAuthority;
use backend_frontend_rust::legacy::{RustToolchain, SourceByteLimit};
use backend_frontend_typescript::legacy::Checker as TypeScriptChecker;
use backend_library::interface::PackageEcosystem;
use backend_semantic::vocabulary::NativeTool;

use super::paths::{TypeScriptHostSelection, canonicalize_existing, create_directory};
use super::{
    AUTHORITY_IMAGE_BYTES, LocalCompilerHost, LocalCompilerHostError, LocalHostDirectory,
    LocalHostEnvironment, LocalHostPathRole, LocalHostVariable, PACKAGE_SOURCE_BYTES, nonzero,
};
use crate::application::toolchain_probe::{
    ToolchainProbeError, ToolchainProbeLimits, ToolchainProbePrimary, probe_command,
};
use crate::application::typescript_host::{TypeScriptProjectHost, is_module_tsc_script};
use crate::application::{
    LocalRuntimeCSharpAuthority, LocalRuntimeGoAuthorityFailure, LocalRuntimeJavaAuthority,
    LocalRuntimePackageAuthority, LocalRuntimePackageRoot, LocalRuntimePythonCheckerAdmission,
    LocalRuntimePythonCheckerProbeFailure, LocalRuntimeRustAuthority, LocalRuntimeToolchain,
    PyreflyToolchainIdentity,
};

impl<Environment: LocalHostEnvironment> LocalCompilerHost<Environment> {
    pub(super) fn package_roots(
        &self,
        home: Option<&Path>,
    ) -> Result<
        (
            Box<[LocalRuntimePackageRoot]>,
            Option<LocalRuntimeGoAuthorityFailure>,
        ),
        LocalCompilerHostError,
    > {
        let mut roots = Vec::with_capacity(7);
        let mut go_failure = None;
        for (ecosystem, variable) in [
            (PackageEcosystem::Cargo, LocalHostVariable::NudoxCargoRoot),
            (PackageEcosystem::Npm, LocalHostVariable::NudoxNpmRoot),
            (PackageEcosystem::Pypi, LocalHostVariable::NudoxPypiRoot),
            (PackageEcosystem::Golang, LocalHostVariable::NudoxGoRoot),
            (PackageEcosystem::Maven, LocalHostVariable::NudoxMavenRoot),
            (PackageEcosystem::Nuget, LocalHostVariable::NudoxNugetRoot),
            (
                PackageEcosystem::Generic,
                LocalHostVariable::NudoxGenericRoot,
            ),
        ] {
            let selected = if ecosystem == PackageEcosystem::Golang {
                self.go_module_cache_directory(home, true)
            } else {
                self.directory(
                    variable,
                    LocalHostPathRole::PackageRoot(ecosystem),
                    self.package_root_candidates(home, ecosystem),
                )
            };
            match selected {
                Ok(Some(path)) => roots.push(LocalRuntimePackageRoot::new(ecosystem, path)?),
                Ok(None) => {}
                Err(error) if ecosystem == PackageEcosystem::Golang => {
                    go_failure = Some(go_authority_failure(&error));
                }
                Err(error) => return Err(error),
            }
        }
        Ok((roots.into_boxed_slice(), go_failure))
    }

    pub(super) fn package_authority(
        &self,
        home: Option<&Path>,
        executables: &NativeExecutables,
        typescript_host: TypeScriptHostSelection,
        jdk_root: Option<PathBuf>,
        go_module_cache: Option<&Path>,
        native_work_directory: &Path,
        probe_limits: ToolchainProbeLimits,
        go_discovery_failure: Option<LocalRuntimeGoAuthorityFailure>,
    ) -> Result<(LocalRuntimePackageAuthority, TypeScriptProjectHost), LocalCompilerHostError> {
        let libclang =
            self.file_or_directory(LocalHostVariable::LibclangPath, LocalHostPathRole::Libclang)?;
        let clang = match (executables.clang.as_deref(), libclang.as_deref()) {
            (Some(driver), Some(libclang)) => {
                Some(ClangAuthorityEnvironment::probe(driver, libclang)?)
            }
            _ => None,
        };
        let typescript_report = self
            .executable(
                LocalHostVariable::NudoxTypeScriptReportProgram,
                LocalHostPathRole::TypeScriptReportProgram,
                ArrayVec::new(),
            )?
            .or_else(|| typescript_host.report_program.clone());
        let (node, node_origin) = match typescript_host.node {
            Some(selection) => (Some(selection.path), Some(selection.origin)),
            None => (None, None),
        };
        let typescript_module_root = typescript_host.module_root;
        let installed_default_compiler = (!typescript_host.compiler_explicit)
            .then(|| executables.typescript.clone())
            .flatten();
        let explicit_module_root = typescript_host
            .module_root_explicit
            .then(|| typescript_module_root.clone())
            .flatten();
        let installed_default_module_root = (!typescript_host.compiler_explicit)
            .then(|| typescript_module_root.clone())
            .flatten();
        let typescript_project_host = TypeScriptProjectHost::new_with_node_origin(
            typescript_host
                .compiler_explicit
                .then(|| executables.typescript.clone())
                .flatten(),
            node.clone(),
            node_origin,
            home.map(Path::to_path_buf),
            explicit_module_root,
            typescript_report.clone(),
            probe_limits,
        )
        .with_installed_default(installed_default_compiler, installed_default_module_root)
        .with_installed_default_origin(typescript_host.compiler_origin)
        .with_bundled_application(typescript_host.bundled_application);
        let typescript = if executables.typescript.is_some() {
            match (typescript_report, node, typescript_module_root) {
                (Some(program), _, _) => Some(TypeScriptChecker::default().with_program(program)?),
                (None, Some(node), Some(module_root)) => {
                    Some(TypeScriptChecker::default().with_node(node, module_root)?)
                }
                (None, _, _) => None,
            }
        } else {
            None
        };
        let pyrefly = self.executable(
            LocalHostVariable::NudoxPyrefly,
            LocalHostPathRole::Pyrefly,
            ArrayVec::new(),
        )?;
        let python_checker = match pyrefly.as_deref() {
            Some(executable) => {
                match probe_command(NativeTool::Python, executable, &["--version"], probe_limits) {
                    Ok(output) => LocalRuntimePythonCheckerAdmission::Ready {
                        adapter: Pyrefly::from_executable(executable.to_path_buf())?,
                        proof: PyreflyToolchainIdentity::from_version_output(&output),
                    },
                    Err(error) => LocalRuntimePythonCheckerAdmission::ProbeFailed {
                        cause: python_checker_probe_failure(&error),
                    },
                }
            }
            None => LocalRuntimePythonCheckerAdmission::Native {
                authority: NativePythonProjectAuthority::admit()?,
            },
        };
        let rust = match (
            executables.rustc.as_deref(),
            executables.cargo.as_deref(),
            executables.cargo_home.as_deref(),
        ) {
            (Some(rustc), Some(cargo), Some(cargo_home)) => {
                Some(self.rust_authority(rustc, cargo, cargo_home, probe_limits)?)
            }
            _ => None,
        };
        let (go, go_unavailable) = match (executables.go.as_deref(), go_discovery_failure) {
            (Some(go), None) => {
                let admitted = (|| {
                    let go_oracle = self.executable(
                        LocalHostVariable::NudoxGoOracle,
                        LocalHostPathRole::GoOracle,
                        ArrayVec::new(),
                    )?;
                    let module_cache =
                        selected_go_module_cache(go_module_cache, native_work_directory)?;
                    let goroot = self.go_root(go, probe_limits)?;
                    let child_environment = GoOracleChildEnvironment::new(
                        go.to_path_buf(),
                        goroot,
                        module_cache,
                        native_work_directory.join("go-oracle-cache"),
                    )?;
                    let configuration = match go_oracle {
                        Some(oracle) => GoOracleConfiguration::oracle_binary(oracle)?,
                        None => GoOracleConfiguration::go_toolchain(go.to_path_buf())?,
                    };
                    GoOracle::default()
                        .with_configuration(configuration)
                        .with_child_environment(child_environment)
                        .map_err(LocalCompilerHostError::from)
                })();
                match admitted {
                    Ok(go) => (Some(go), None),
                    Err(error) => (None, Some(go_authority_failure(&error))),
                }
            }
            (Some(_), Some(failure)) => (None, Some(failure)),
            (None, failure) => (None, failure),
        };
        let java = match (executables.java.as_ref(), jdk_root) {
            (Some(_), Some(root)) => Some(LocalRuntimeJavaAuthority {
                toolchain: JdkToolchain::from_owned_root(root)?,
                classpath: Box::new([]),
            }),
            _ => None,
        };
        let roslyn_helper = self.executable(
            LocalHostVariable::NudoxRoslynHelper,
            LocalHostPathRole::RoslynHelper,
            ArrayVec::new(),
        )?;
        let csharp = match (executables.csharp.as_ref(), roslyn_helper) {
            (Some(_), Some(helper)) => Some(LocalRuntimeCSharpAuthority {
                producer: CSharpOracle::new(helper),
                assembly_name: None,
                reference_directory: None,
                define_symbols: Box::new([]),
                extra_usings: Box::new([]),
                implicit_usings: true,
                include_non_public: true,
                maximum_source_bytes: DEFAULT_SOURCE_LIMIT,
            }),
            _ => None,
        };
        Ok((
            LocalRuntimePackageAuthority {
                clang,
                typescript,
                python_checker,
                rust,
                go,
                go_unavailable,
                csharp,
                java,
                maximum_image_bytes: Some(nonzero(AUTHORITY_IMAGE_BYTES)),
            },
            typescript_project_host,
        ))
    }

    fn rust_authority(
        &self,
        rustc: &Path,
        cargo: &Path,
        cargo_home: &Path,
        probe_limits: ToolchainProbeLimits,
    ) -> Result<LocalRuntimeRustAuthority, LocalCompilerHostError> {
        let sysroot = match self.optional_absolute_path(LocalHostVariable::NudoxRustSysroot)? {
            Some(path) => self.validate_directory(
                LocalHostPathRole::RustSysroot,
                LocalHostVariable::NudoxRustSysroot,
                path,
            )?,
            None => {
                let output = probe_command(
                    NativeTool::Rustc,
                    rustc,
                    &["--print", "sysroot"],
                    probe_limits,
                )?;
                let path = match str::from_utf8(&output) {
                    Ok(text) => PathBuf::from(text.trim()),
                    Err(source) => {
                        return Err(LocalCompilerHostError::RustSysrootEncoding { output, source });
                    }
                };
                if path.as_os_str().is_empty() {
                    return Err(LocalCompilerHostError::RustSysrootEmpty {
                        compiler: rustc.to_path_buf().into_boxed_path(),
                    });
                }
                if !path.is_absolute() {
                    return Err(LocalCompilerHostError::RustSysrootRelative {
                        compiler: rustc.to_path_buf().into_boxed_path(),
                        sysroot: path.into_boxed_path(),
                    });
                }
                canonicalize_existing(LocalHostPathRole::RustSysroot, &path)?
            }
        };
        let toolchain = RustToolchain::from_paths_with_cargo(
            rustc.to_path_buf(),
            sysroot,
            cargo.to_path_buf(),
            cargo_home.to_path_buf(),
        )?;
        Ok(LocalRuntimeRustAuthority {
            toolchain,
            maximum_source_bytes: SourceByteLimit::from(PACKAGE_SOURCE_BYTES),
            all_features: true,
            no_default_features: false,
            features: Box::new([]),
            metadata_policy: self.rust_cargo_metadata_policy,
        })
    }

    fn go_root(
        &self,
        go: &Path,
        probe_limits: ToolchainProbeLimits,
    ) -> Result<PathBuf, LocalCompilerHostError> {
        let output = probe_command(NativeTool::GoCompiler, go, &["env", "GOROOT"], probe_limits)
            .map_err(LocalCompilerHostError::GoRootProbe)?;
        let path = match str::from_utf8(&output) {
            Ok(text) => PathBuf::from(text.trim()),
            Err(source) => {
                return Err(LocalCompilerHostError::GoRootEncoding { output, source });
            }
        };
        if path.as_os_str().is_empty() {
            return Err(LocalCompilerHostError::GoRootEmpty {
                compiler: go.to_path_buf().into_boxed_path(),
            });
        }
        if !path.is_absolute() {
            return Err(LocalCompilerHostError::GoRootRelative {
                compiler: go.to_path_buf().into_boxed_path(),
                goroot: path.into_boxed_path(),
            });
        }
        self.validate_directory(LocalHostPathRole::GoRoot, LocalHostVariable::NudoxGo, path)
    }
}

pub(super) fn go_authority_failure(
    error: &LocalCompilerHostError,
) -> LocalRuntimeGoAuthorityFailure {
    match error {
        LocalCompilerHostError::RelativeEnvironmentPath {
            variable: LocalHostVariable::NudoxGoRoot,
            ..
        }
        | LocalCompilerHostError::GoRootProbe(_)
        | LocalCompilerHostError::GoRootEncoding { .. }
        | LocalCompilerHostError::GoRootEmpty { .. }
        | LocalCompilerHostError::GoRootRelative { .. }
        | LocalCompilerHostError::ConfiguredPath {
            role:
                LocalHostPathRole::GoRoot | LocalHostPathRole::PackageRoot(PackageEcosystem::Golang),
            ..
        }
        | LocalCompilerHostError::ConfiguredPathKind {
            role:
                LocalHostPathRole::GoRoot | LocalHostPathRole::PackageRoot(PackageEcosystem::Golang),
            ..
        } => LocalRuntimeGoAuthorityFailure::GoRootUnavailable,
        LocalCompilerHostError::GoAuthority(GoOracleConfigurationError::ToolchainIdentity {
            ..
        }) => LocalRuntimeGoAuthorityFailure::ToolchainIdentityUnavailable,
        LocalCompilerHostError::GoAuthority(
            GoOracleConfigurationError::InvalidGoExecutable { .. }
            | GoOracleConfigurationError::RelativeExecutable { .. },
        )
        | LocalCompilerHostError::ConfiguredPath {
            role: LocalHostPathRole::Native(NativeTool::GoCompiler),
            ..
        }
        | LocalCompilerHostError::ConfiguredPathKind {
            role: LocalHostPathRole::Native(NativeTool::GoCompiler),
            ..
        }
        | LocalCompilerHostError::RelativeEnvironmentPath {
            variable: LocalHostVariable::NudoxGo,
            ..
        } => LocalRuntimeGoAuthorityFailure::ExecutableUnavailable,
        LocalCompilerHostError::CreateDirectory {
            directory: super::LocalHostDirectory::GoModuleCache,
            ..
        } => LocalRuntimeGoAuthorityFailure::ModuleCacheUnavailable,
        _ => LocalRuntimeGoAuthorityFailure::OracleUnavailable,
    }
}

/// An installed package cache is optional: the vendored oracle itself needs no downloads.
/// Project dependencies remain subject to the normal captured input admission policy.
fn selected_go_module_cache(
    selected: Option<&Path>,
    native_work_directory: &Path,
) -> Result<PathBuf, LocalCompilerHostError> {
    if let Some(selected) = selected {
        return Ok(selected.to_path_buf());
    }
    let private = native_work_directory.join("go-module-cache");
    create_directory(LocalHostDirectory::GoModuleCache, &private)?;
    Ok(private)
}

fn python_checker_probe_failure(
    error: &ToolchainProbeError,
) -> LocalRuntimePythonCheckerProbeFailure {
    match error {
        ToolchainProbeError::RelativeExecutable { .. } => {
            LocalRuntimePythonCheckerProbeFailure::RelativeExecutable
        }
        ToolchainProbeError::Spawn { .. } => LocalRuntimePythonCheckerProbeFailure::ExecutableStart,
        ToolchainProbeError::ProbeWorkerSpawn { .. } | ToolchainProbeError::ReaderSpawn { .. } => {
            LocalRuntimePythonCheckerProbeFailure::WorkerStart
        }
        ToolchainProbeError::MissingStream { .. } => {
            LocalRuntimePythonCheckerProbeFailure::MissingStream
        }
        ToolchainProbeError::Wait { .. } => {
            LocalRuntimePythonCheckerProbeFailure::ProcessObservation
        }
        ToolchainProbeError::Cleanup { action, .. } => {
            LocalRuntimePythonCheckerProbeFailure::Cleanup(*action)
        }
        ToolchainProbeError::Stream { .. } | ToolchainProbeError::Streams { .. } => {
            LocalRuntimePythonCheckerProbeFailure::StreamRead
        }
        ToolchainProbeError::Bounded {
            primary: ToolchainProbePrimary::Cancelled,
            ..
        } => LocalRuntimePythonCheckerProbeFailure::Cancelled,
        ToolchainProbeError::Bounded {
            primary: ToolchainProbePrimary::Deadline { .. },
            ..
        } => LocalRuntimePythonCheckerProbeFailure::TimedOut,
        ToolchainProbeError::Bounded {
            primary: ToolchainProbePrimary::OutputLimit { .. },
            ..
        } => LocalRuntimePythonCheckerProbeFailure::OutputLimit,
        ToolchainProbeError::MissingStatus { .. } => {
            LocalRuntimePythonCheckerProbeFailure::MissingStatus
        }
        ToolchainProbeError::Exit { status, .. } => {
            LocalRuntimePythonCheckerProbeFailure::ProcessExit {
                code: status.code(),
            }
        }
        ToolchainProbeError::Empty { .. } => LocalRuntimePythonCheckerProbeFailure::EmptyVersion,
        ToolchainProbeError::Resolution { .. }
        | ToolchainProbeError::TypeScriptInterpreterUnavailable { .. }
        | ToolchainProbeError::TypeScriptModuleRootUnavailable { .. }
        | ToolchainProbeError::TypeScriptModuleEntryMismatch { .. }
        | ToolchainProbeError::ExecutableWitness { .. }
        | ToolchainProbeError::ExecutableChanged { .. } => {
            LocalRuntimePythonCheckerProbeFailure::IdentityResolution
        }
    }
}

#[cfg(test)]
mod python_checker_probe_tests {
    use super::*;

    #[test]
    fn pyrefly_probe_failure_projection_keeps_only_bounded_typed_causes() {
        let cancelled = ToolchainProbeError::Bounded {
            tool: NativeTool::Python,
            primary: ToolchainProbePrimary::Cancelled,
        };
        assert_eq!(
            python_checker_probe_failure(&cancelled),
            LocalRuntimePythonCheckerProbeFailure::Cancelled,
        );
        let timeout = ToolchainProbeError::Bounded {
            tool: NativeTool::Python,
            primary: ToolchainProbePrimary::Deadline {
                timeout: std::time::Duration::from_secs(2),
            },
        };
        assert_eq!(
            python_checker_probe_failure(&timeout),
            LocalRuntimePythonCheckerProbeFailure::TimedOut,
        );
        let output_limit = ToolchainProbeError::Bounded {
            tool: NativeTool::Python,
            primary: ToolchainProbePrimary::OutputLimit {
                worker: backend_semantic::vocabulary::NativeWorker::StandardOutputReader,
                observed: 4097,
                maximum: 4096,
            },
        };
        assert_eq!(
            python_checker_probe_failure(&output_limit),
            LocalRuntimePythonCheckerProbeFailure::OutputLimit,
        );
        let empty = ToolchainProbeError::Empty {
            tool: NativeTool::Python,
        };
        assert_eq!(
            python_checker_probe_failure(&empty),
            LocalRuntimePythonCheckerProbeFailure::EmptyVersion,
        );
    }
}

#[derive(Clone, Debug)]
pub(super) struct NativeExecutables {
    pub(super) rustc: Option<PathBuf>,
    pub(super) cargo: Option<PathBuf>,
    pub(super) cargo_home: Option<PathBuf>,
    pub(super) clang: Option<PathBuf>,
    pub(super) python: Option<PathBuf>,
    pub(super) typescript: Option<PathBuf>,
    pub(super) go: Option<PathBuf>,
    pub(super) java: Option<PathBuf>,
    pub(super) csharp: Option<PathBuf>,
}

impl NativeExecutables {
    pub(super) fn toolchain_rows(
        &self,
        typescript_node: Option<&Path>,
        typescript_module_root: Option<&Path>,
    ) -> Box<[LocalRuntimeToolchain]> {
        self.toolchain_executables()
            .map(|(tool, executable)| match executable {
                Some(executable) => match (tool, typescript_node, typescript_module_root) {
                    (NativeTool::TypeScriptCompiler, Some(node), Some(module_root))
                        if is_module_tsc_script(executable, module_root) =>
                    {
                        LocalRuntimeToolchain::probing_typescript_script(
                            executable.to_path_buf(),
                            node.to_path_buf(),
                            module_root.to_path_buf(),
                        )
                    }
                    // A PATH-selected executable may be a platform wrapper around the package
                    // entrypoint (for example, Nix's shell `bin/tsc`). It is still the selected
                    // compiler command and must be launched by its own shebang, not passed to
                    // Node as if the wrapper were JavaScript. The project host separately uses
                    // the exact module-root API through the admitted Node runtime.
                    (NativeTool::TypeScriptCompiler, Some(_), Some(_)) => {
                        LocalRuntimeToolchain::probing(tool, executable.to_path_buf())
                    }
                    (NativeTool::TypeScriptCompiler, None, _) => LocalRuntimeToolchain::probe_failed(
                        tool,
                        crate::application::ToolchainProbeError::TypeScriptInterpreterUnavailable {
                            compiler: executable.to_path_buf(),
                        },
                    ),
                    (NativeTool::TypeScriptCompiler, Some(_), None) => {
                        LocalRuntimeToolchain::probe_failed(
                            tool,
                            crate::application::ToolchainProbeError::TypeScriptModuleRootUnavailable {
                                compiler: executable.to_path_buf(),
                            },
                        )
                    }
                    _ => LocalRuntimeToolchain::probing(tool, executable.to_path_buf()),
                },
                None => LocalRuntimeToolchain::unavailable(tool),
            })
            .collect::<Vec<_>>()
            .into_boxed_slice()
    }

    pub(super) fn admitted_toolchain_rows(
        &self,
        typescript_node: Option<&Path>,
        typescript_module_root: Option<&Path>,
        limits: ToolchainProbeLimits,
    ) -> Box<[LocalRuntimeToolchain]> {
        self.toolchain_rows(typescript_node, typescript_module_root)
            .into_vec()
            .into_iter()
            .map(|row| {
                let tool = row.tool;
                row.admit_pending(limits)
                    .unwrap_or_else(|failure| LocalRuntimeToolchain::probe_failed(tool, failure))
            })
            .collect::<Vec<_>>()
            .into_boxed_slice()
    }

    fn toolchain_executables(&self) -> impl Iterator<Item = (NativeTool, Option<&Path>)> {
        [
            (NativeTool::Rustc, self.rustc.as_deref()),
            (NativeTool::Clang, self.clang.as_deref()),
            (NativeTool::Python, self.python.as_deref()),
            (NativeTool::TypeScriptCompiler, self.typescript.as_deref()),
            (NativeTool::GoCompiler, self.go.as_deref()),
            (NativeTool::JavaCompiler, self.java.as_deref()),
            (NativeTool::CSharpCompiler, self.csharp.as_deref()),
        ]
        .into_iter()
    }
}

#[cfg(test)]
mod invocation_selection_tests {
    use super::{NativeExecutables, selected_go_module_cache};
    use crate::application::{
        LocalRuntimeToolchainState, ToolchainProbeError, ToolchainProbeLimits,
    };
    use backend_semantic::vocabulary::NativeTool;
    use std::{num::NonZeroUsize, path::PathBuf, time::Duration};

    #[test]
    fn go_without_installed_modules_uses_an_isolated_owner_cache() {
        let owner = std::env::temp_dir().join(format!(
            "nudox-go-private-cache-{}-{}",
            std::process::id(),
            super::super::NEXT_NATIVE_WORK.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
        ));
        std::fs::create_dir(&owner).expect("create unique owner");
        let private = selected_go_module_cache(None, &owner).expect("admit cold cache");
        assert_eq!(private, owner.join("go-module-cache"));
        assert!(private.is_dir());
        assert_eq!(std::fs::read_dir(&private).unwrap().count(), 0);

        let installed = owner.join("installed-modules");
        std::fs::create_dir(&installed).expect("create selected cache");
        let other_owner = owner.join("must-not-be-created");
        assert_eq!(
            selected_go_module_cache(Some(&installed), &other_owner).unwrap(),
            installed,
        );
        assert!(!other_owner.exists());
        std::fs::remove_dir_all(owner).expect("remove private test owner");
    }

    fn only_typescript(typescript: Option<PathBuf>) -> NativeExecutables {
        NativeExecutables {
            rustc: None,
            cargo: None,
            cargo_home: None,
            clang: None,
            python: None,
            typescript,
            go: None,
            java: None,
            csharp: None,
        }
    }

    #[test]
    fn global_typescript_script_without_node_is_a_typed_refusal() {
        let compiler = PathBuf::from("/selected/typescript/bin/tsc");
        let rows = only_typescript(Some(compiler.clone())).toolchain_rows(None, None);
        let typescript = rows
            .iter()
            .find(|row| row.tool == NativeTool::TypeScriptCompiler)
            .expect("fixed TypeScript row");

        assert_eq!(typescript.state, LocalRuntimeToolchainState::ProbeFailed);
        assert!(matches!(
            typescript.probe_failure(),
            Some(ToolchainProbeError::TypeScriptInterpreterUnavailable {
                compiler: observed
            }) if observed == &compiler
        ));
    }

    #[test]
    fn selected_node_keeps_global_typescript_compiler_pending() {
        let compiler = PathBuf::from("/selected/typescript/bin/tsc");
        let node = PathBuf::from("/selected/node/bin/node");
        let module_root = PathBuf::from("/selected/node_modules");
        let rows =
            only_typescript(Some(compiler.clone())).toolchain_rows(Some(&node), Some(&module_root));
        let typescript = rows
            .iter()
            .find(|row| row.tool == NativeTool::TypeScriptCompiler)
            .expect("fixed TypeScript row");

        assert_eq!(typescript.state, LocalRuntimeToolchainState::Probing);
        assert!(typescript.probe_failure().is_none());
    }

    #[cfg(unix)]
    #[test]
    fn host_wrapper_is_probed_as_an_executable_not_as_a_typescript_js_script() {
        use std::os::unix::fs::PermissionsExt;

        let root = std::env::temp_dir().join(format!(
            "nudox-typescript-wrapper-probe-{}-{}",
            std::process::id(),
            super::super::NEXT_NATIVE_WORK.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
        ));
        let module_root = root.join("lib/node_modules");
        let package = module_root.join("typescript");
        let module_entry = package.join("bin/tsc");
        std::fs::create_dir_all(module_entry.parent().expect("module entry parent"))
            .expect("create package entry directory");
        std::fs::write(
            package.join("package.json"),
            r#"{"name":"typescript","version":"5.9.3"}"#,
        )
        .expect("write selected module metadata");
        std::fs::write(&module_entry, "require('../lib/tsc.js');\n")
            .expect("write canonical JavaScript entrypoint");

        let compiler = root.join("bin/tsc");
        std::fs::create_dir_all(compiler.parent().expect("wrapper parent"))
            .expect("create wrapper directory");
        std::fs::write(
            &compiler,
            "#!/bin/sh\nif [ \"$1\" = '--version' ]; then printf 'Version 5.9.3\\n'; exit 0; fi\nexit 64\n",
        )
        .expect("write host executable wrapper");
        std::fs::set_permissions(&compiler, std::fs::Permissions::from_mode(0o755))
            .expect("make wrapper executable");

        let node = root.join("bin/node");
        std::fs::write(&node, "not used by direct wrapper invocation\n")
            .expect("write paired Node fixture");
        std::fs::set_permissions(&node, std::fs::Permissions::from_mode(0o755))
            .expect("make paired Node fixture executable");
        let compiler = std::fs::canonicalize(compiler).expect("canonical wrapper");
        let module_root = std::fs::canonicalize(module_root).expect("canonical module root");
        let node = std::fs::canonicalize(node).expect("canonical Node fixture");

        let mut rows = only_typescript(Some(compiler.clone()))
            .toolchain_rows(Some(&node), Some(&module_root))
            .into_vec();
        let pending = rows
            .iter()
            .find(|row| row.tool == NativeTool::TypeScriptCompiler)
            .expect("fixed TypeScript row");
        assert_eq!(pending.state, LocalRuntimeToolchainState::Probing);
        let limits = ToolchainProbeLimits::new(
            Duration::from_secs(2),
            NonZeroUsize::new(4096).expect("nonzero output bound"),
        )
        .expect("valid probe limits");
        let index = rows
            .iter()
            .position(|row| row.tool == NativeTool::TypeScriptCompiler)
            .expect("TypeScript row index");
        let admitted = rows
            .remove(index)
            .admit_pending(limits)
            .expect("direct TSC probe");
        assert_eq!(admitted.state, LocalRuntimeToolchainState::Ready);
        assert!(admitted.identity.is_some());
        assert!(admitted.probe_failure().is_none());

        assert!(module_entry.is_file());
        std::fs::remove_dir_all(root).expect("remove wrapper fixture");
    }

    #[cfg(unix)]
    #[test]
    fn module_owned_tsc_entry_uses_the_admitted_node_runtime() {
        use std::os::unix::fs::PermissionsExt;

        let root = std::env::temp_dir().join(format!(
            "nudox-typescript-module-entry-probe-{}-{}",
            std::process::id(),
            super::super::NEXT_NATIVE_WORK.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
        ));
        let module_root = root.join("node_modules");
        let package = module_root.join("typescript");
        let compiler = package.join("bin/tsc");
        std::fs::create_dir_all(compiler.parent().expect("compiler parent"))
            .expect("create package compiler directory");
        std::fs::create_dir_all(package.join("lib")).expect("create compiler module directory");
        std::fs::write(
            package.join("package.json"),
            r#"{"name":"typescript","version":"5.9.3"}"#,
        )
        .expect("write selected module metadata");
        std::fs::write(package.join("lib/tsc.js"), "module.exports = {};\n")
            .expect("write compiler library fixture");
        std::fs::write(
            &compiler,
            "if [ \"$1\" = '--version' ]; then printf 'Version 5.9.3\\n'; exit 0; fi\nexit 64\n",
        )
        .expect("write package JavaScript entrypoint");

        let node = root.join("bin/node");
        std::fs::create_dir_all(node.parent().expect("node parent"))
            .expect("create Node fixture directory");
        std::fs::write(
            &node,
            "#!/bin/sh\nif [ \"$1\" = '--version' ]; then printf 'v24.18.0\\n'; exit 0; fi\ncompiler=$1\nshift\nexec /bin/sh \"$compiler\" \"$@\"\n",
        )
        .expect("write deterministic Node wrapper");
        std::fs::set_permissions(&node, std::fs::Permissions::from_mode(0o755))
            .expect("make Node fixture executable");

        let compiler = std::fs::canonicalize(compiler).expect("canonical package tsc entry");
        let module_root = std::fs::canonicalize(module_root).expect("canonical module root");
        let node = std::fs::canonicalize(node).expect("canonical Node fixture");
        let mut rows = only_typescript(Some(compiler))
            .toolchain_rows(Some(&node), Some(&module_root))
            .into_vec();
        let limits = ToolchainProbeLimits::new(
            Duration::from_secs(2),
            NonZeroUsize::new(4096).expect("nonzero output bound"),
        )
        .expect("valid probe limits");
        let index = rows
            .iter()
            .position(|row| row.tool == NativeTool::TypeScriptCompiler)
            .expect("TypeScript row index");
        let admitted = rows
            .remove(index)
            .admit_pending(limits)
            .expect("Node TSC probe");
        assert_eq!(admitted.state, LocalRuntimeToolchainState::Ready);
        assert!(admitted.identity.is_some());
        assert!(admitted.probe_failure().is_none());

        std::fs::remove_dir_all(root).expect("remove package fixture");
    }
}
