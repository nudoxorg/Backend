//! Canonical native-tool rows and package semantic-authority ownership.

use std::{
    path::{Path, PathBuf},
    str,
};

use arrayvec::ArrayVec;
use backend_frontend_clang::ClangAuthorityEnvironment;
use backend_frontend_csharp::legacy::{CSharpOracle, DEFAULT_SOURCE_LIMIT};
use backend_frontend_go::legacy::oracle::GoOracleChildEnvironment;
use backend_frontend_go::legacy::{GoOracle, GoOracleConfiguration};
use backend_frontend_java::legacy::harness::JdkToolchain;
use backend_frontend_python::legacy::Pyrefly;
use backend_frontend_rust::legacy::{RustToolchain, SourceByteLimit};
use backend_frontend_typescript::legacy::Checker as TypeScriptChecker;
use backend_library::interface::PackageEcosystem;
use backend_semantic::vocabulary::NativeTool;

use super::paths::canonicalize_existing;
use super::{
    AUTHORITY_IMAGE_BYTES, LocalCompilerHost, LocalCompilerHostError, LocalHostEnvironment,
    LocalHostPathRole, LocalHostVariable, PACKAGE_SOURCE_BYTES, nonzero,
};
use crate::application::toolchain_probe::{ToolchainProbeLimits, probe_command};
use crate::application::{
    LocalRuntimeCSharpAuthority, LocalRuntimeJavaAuthority, LocalRuntimePackageAuthority,
    LocalRuntimePackageRoot, LocalRuntimeRustAuthority, LocalRuntimeToolchain,
    PyreflyToolchainIdentity,
};

impl<Environment: LocalHostEnvironment> LocalCompilerHost<Environment> {
    pub(super) fn package_roots(
        &self,
        home: Option<&Path>,
    ) -> Result<Box<[LocalRuntimePackageRoot]>, LocalCompilerHostError> {
        let mut roots = Vec::with_capacity(7);
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
            let candidates = self.package_root_candidates(home, ecosystem);
            if let Some(path) = self.directory(
                variable,
                LocalHostPathRole::PackageRoot(ecosystem),
                candidates,
            )? {
                roots.push(LocalRuntimePackageRoot::new(ecosystem, path)?);
            }
        }
        Ok(roots.into_boxed_slice())
    }

    pub(super) fn package_authority(
        &self,
        home: Option<&Path>,
        executables: &NativeExecutables,
        jdk_root: Option<PathBuf>,
        go_module_cache: Option<&Path>,
        native_work_directory: &Path,
        probe_limits: ToolchainProbeLimits,
    ) -> Result<LocalRuntimePackageAuthority, LocalCompilerHostError> {
        let libclang =
            self.file_or_directory(LocalHostVariable::LibclangPath, LocalHostPathRole::Libclang)?;
        let clang = match (executables.clang.as_deref(), libclang.as_deref()) {
            (Some(driver), Some(libclang)) => {
                Some(ClangAuthorityEnvironment::probe(driver, libclang)?)
            }
            _ => None,
        };
        let typescript_report = self.executable(
            LocalHostVariable::NudoxTypeScriptReportProgram,
            LocalHostPathRole::TypeScriptReportProgram,
            ArrayVec::new(),
        )?;
        let node = self.executable(
            LocalHostVariable::NudoxTypeScriptNode,
            LocalHostPathRole::TypeScriptNode,
            self.auxiliary_candidates(home, "node"),
        )?;
        let typescript_module_root =
            self.typescript_module_root(executables.typescript.as_deref())?;
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
            self.auxiliary_candidates(home, "pyrefly"),
        )?;
        let python_toolchain_identity = pyrefly.as_deref().and_then(|executable| {
            probe_command(NativeTool::Python, executable, &["--version"], probe_limits)
                .ok()
                .map(|output| PyreflyToolchainIdentity::from_version_output(&output))
        });
        let python = match (executables.python.as_ref(), pyrefly) {
            (Some(_), Some(executable)) => Some(Pyrefly::from_executable(executable)?),
            _ => None,
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
        let go_oracle = self.executable(
            LocalHostVariable::NudoxGoOracle,
            LocalHostPathRole::GoOracle,
            ArrayVec::new(),
        )?;
        let go = match (executables.go.as_deref(), go_module_cache) {
            (Some(go), Some(module_cache)) => {
                let goroot = self.go_root(go, probe_limits)?;
                let child_environment = GoOracleChildEnvironment::new(
                    go.to_path_buf(),
                    goroot,
                    module_cache.to_path_buf(),
                    native_work_directory.join("go-oracle-cache"),
                )?;
                let configuration = match go_oracle {
                    Some(oracle) => GoOracleConfiguration::oracle_binary(oracle)?,
                    None => GoOracleConfiguration::go_toolchain(go.to_path_buf())?,
                };
                Some(
                    GoOracle::default()
                        .with_configuration(configuration)
                        .with_child_environment(child_environment)?,
                )
            }
            _ => None,
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
        Ok(LocalRuntimePackageAuthority {
            clang,
            typescript,
            python,
            python_toolchain_identity,
            rust,
            go,
            csharp,
            java,
            maximum_image_bytes: Some(nonzero(AUTHORITY_IMAGE_BYTES)),
        })
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
    pub(super) fn toolchain_rows(&self) -> Box<[LocalRuntimeToolchain]> {
        self.toolchain_executables()
            .map(|(tool, executable)| match executable {
                Some(executable) => LocalRuntimeToolchain::probing(tool, executable.to_path_buf()),
                None => LocalRuntimeToolchain::unavailable(tool),
            })
            .collect::<Vec<_>>()
            .into_boxed_slice()
    }

    pub(super) fn admitted_toolchain_rows(
        &self,
        limits: ToolchainProbeLimits,
    ) -> Box<[LocalRuntimeToolchain]> {
        self.toolchain_executables()
            .map(|(tool, executable)| match executable {
                Some(executable) => {
                    LocalRuntimeToolchain::probe(tool, executable.to_path_buf(), limits)
                        .unwrap_or_else(|_| LocalRuntimeToolchain::probe_failed(tool))
                }
                None => LocalRuntimeToolchain::unavailable(tool),
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
