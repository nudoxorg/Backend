//! Canonical native-tool rows and package semantic-authority ownership.

use std::{
    path::{Path, PathBuf},
    str,
};

use arrayvec::ArrayVec;
use compiler_languages_csharp::{CSharpOracle, DEFAULT_SOURCE_LIMIT};
use compiler_languages_go::{GoOracle, GoOracleConfiguration};
use compiler_languages_java::harness::JdkToolchain;
use compiler_languages_python::Pyrefly;
use compiler_languages_rust::{RustToolchain, SourceByteLimit};
use compiler_languages_typescript::Checker as TypeScriptChecker;
use compiler_vocabulary::NativeTool;
use interface_core::PackageEcosystem;

use super::paths::canonicalize_existing;
use super::{
    AUTHORITY_IMAGE_BYTES, LocalCompilerHost, LocalCompilerHostError, LocalHostEnvironment,
    LocalHostPathRole, LocalHostVariable, PACKAGE_SOURCE_BYTES, nonzero,
};
use crate::toolchain_probe::{ToolchainProbeLimits, probe_command};
use crate::{
    LocalRuntimeCSharpAuthority, LocalRuntimeJavaAuthority, LocalRuntimePackageAuthority,
    LocalRuntimePackageRoot, LocalRuntimeRustAuthority, LocalRuntimeToolchain,
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
        probe_limits: ToolchainProbeLimits,
    ) -> Result<LocalRuntimePackageAuthority, LocalCompilerHostError> {
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
        let typescript = if executables.typescript.is_some() {
            match (typescript_report, node) {
                (Some(program), _) => Some(TypeScriptChecker::default().with_program(program)?),
                (None, Some(node)) => Some(TypeScriptChecker::default().with_node(node)?),
                (None, None) => None,
            }
        } else {
            None
        };
        let pyrefly = self.executable(
            LocalHostVariable::NudoxPyrefly,
            LocalHostPathRole::Pyrefly,
            self.auxiliary_candidates(home, "pyrefly"),
        )?;
        let python = match (executables.python.as_ref(), pyrefly) {
            (Some(_), Some(executable)) => Some(Pyrefly::from_executable(executable)?),
            _ => None,
        };
        let rust = executables
            .rustc
            .as_ref()
            .map(|rustc| self.rust_authority(rustc, probe_limits))
            .transpose()?;
        let go_oracle = self.executable(
            LocalHostVariable::NudoxGoOracle,
            LocalHostPathRole::GoOracle,
            ArrayVec::new(),
        )?;
        let go = match (executables.go.as_ref(), go_oracle) {
            (Some(_), Some(oracle)) => Some(
                GoOracle::default()
                    .with_configuration(GoOracleConfiguration::oracle_binary(oracle)?),
            ),
            (Some(go), None) => Some(
                GoOracle::default()
                    .with_configuration(GoOracleConfiguration::go_toolchain(go.clone())?),
            ),
            (None, _) => None,
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
            typescript,
            python,
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
        Ok(LocalRuntimeRustAuthority {
            toolchain: RustToolchain::from_paths(rustc.to_path_buf(), sysroot)?,
            maximum_source_bytes: SourceByteLimit::from(PACKAGE_SOURCE_BYTES),
            all_features: true,
            no_default_features: false,
            features: Box::new([]),
        })
    }
}

#[derive(Clone, Debug)]
pub(super) struct NativeExecutables {
    pub(super) rustc: Option<PathBuf>,
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
        limits: ToolchainProbeLimits,
    ) -> Result<Box<[LocalRuntimeToolchain]>, LocalCompilerHostError> {
        let mut rows = Vec::with_capacity(NativeTool::ALL.len());
        for (tool, executable) in [
            (NativeTool::Rustc, self.rustc.as_ref()),
            (NativeTool::Clang, self.clang.as_ref()),
            (NativeTool::Python, self.python.as_ref()),
            (NativeTool::TypeScriptCompiler, self.typescript.as_ref()),
            (NativeTool::GoCompiler, self.go.as_ref()),
            (NativeTool::JavaCompiler, self.java.as_ref()),
            (NativeTool::CSharpCompiler, self.csharp.as_ref()),
        ] {
            rows.push(match executable {
                Some(executable) => LocalRuntimeToolchain::probe(tool, executable.clone(), limits)
                    .map_err(|source| LocalCompilerHostError::ToolchainProbe {
                        tool,
                        executable: executable.clone().into_boxed_path(),
                        source,
                    })?,
                None => LocalRuntimeToolchain::unavailable(tool),
            });
        }
        Ok(rows.into_boxed_slice())
    }
}
