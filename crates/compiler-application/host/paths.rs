//! Finite platform path selection and canonical filesystem admission.

use std::{
    ffi::OsStr,
    fs, io,
    path::{Path, PathBuf},
};

use arrayvec::ArrayVec;
use compiler_vocabulary::NativeTool;
use interface_core::PackageEcosystem;

use super::{
    LocalCompilerHost, LocalCompilerHostError, LocalHostDiscovery, LocalHostEnvironment,
    LocalHostVariable, PLATFORM_PATH_CAPACITY,
};

/// Closed filesystem role retained by host setup diagnostics.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LocalHostDirectory {
    /// Durable application data root.
    DataRoot,
    /// Immutable compiler artifact directory.
    Artifacts,
    /// Durable publication journal directory.
    Journal,
    /// Parent for process-unique native work owners.
    NativeWorkParent,
    /// One process-unique empty native work owner.
    NativeWork,
}

/// Closed file or directory authority role.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LocalHostPathRole {
    /// One registry-selected native compiler executable.
    Native(NativeTool),
    /// Node runtime for the vendored TypeScript authority driver.
    TypeScriptNode,
    /// Node module root containing the TypeScript compiler API.
    TypeScriptModuleRoot,
    /// Direct TypeScript report producer.
    TypeScriptReportProgram,
    /// Pyrefly authority executable.
    Pyrefly,
    /// Direct Go authority producer.
    GoOracle,
    /// Rust sysroot paired with the selected compiler.
    RustSysroot,
    /// Java development kit root.
    JdkRoot,
    /// Published Roslyn helper assembly.
    RoslynHelper,
    /// One ecosystem's local package store.
    PackageRoot(PackageEcosystem),
}

/// Expected filesystem object kind for a configured host path.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LocalHostPathKind {
    /// Regular file.
    File,
    /// Directory.
    Directory,
}

impl<Environment: LocalHostEnvironment> LocalCompilerHost<Environment> {
    pub(super) fn executable(
        &self,
        variable: LocalHostVariable,
        role: LocalHostPathRole,
        candidates: ArrayVec<PathBuf, PLATFORM_PATH_CAPACITY>,
    ) -> Result<Option<PathBuf>, LocalCompilerHostError> {
        if let Some(path) = self.optional_absolute_path(variable)? {
            return self.validate_file(role, variable, path).map(Some);
        }
        if self.discovery == LocalHostDiscovery::ExplicitOnly {
            return Ok(None);
        }
        first_existing(role, candidates)
    }

    pub(super) fn directory(
        &self,
        variable: LocalHostVariable,
        role: LocalHostPathRole,
        candidates: ArrayVec<PathBuf, PLATFORM_PATH_CAPACITY>,
    ) -> Result<Option<PathBuf>, LocalCompilerHostError> {
        if let Some(path) = self.optional_absolute_path(variable)? {
            return self.validate_directory(role, variable, path).map(Some);
        }
        if self.discovery == LocalHostDiscovery::ExplicitOnly {
            return Ok(None);
        }
        first_existing_directory(role, candidates)
    }

    pub(super) fn optional_absolute_path(
        &self,
        variable: LocalHostVariable,
    ) -> Result<Option<PathBuf>, LocalCompilerHostError> {
        let Some(value) = self.environment.value(variable) else {
            return Ok(None);
        };
        let path = PathBuf::from(value);
        if !path.is_absolute() {
            return Err(LocalCompilerHostError::RelativeEnvironmentPath {
                variable,
                path: path.into_boxed_path(),
            });
        }
        Ok(Some(path))
    }

    pub(super) fn required_absolute_path(
        &self,
        variable: LocalHostVariable,
    ) -> Result<PathBuf, LocalCompilerHostError> {
        self.optional_absolute_path(variable)?
            .ok_or(LocalCompilerHostError::RequiredEnvironmentPath { variable })
    }

    fn validate_file(
        &self,
        role: LocalHostPathRole,
        variable: LocalHostVariable,
        path: PathBuf,
    ) -> Result<PathBuf, LocalCompilerHostError> {
        let metadata =
            fs::metadata(&path).map_err(|source| LocalCompilerHostError::ConfiguredPath {
                role,
                variable,
                path: path.clone().into_boxed_path(),
                source,
            })?;
        if !metadata.is_file() {
            return Err(LocalCompilerHostError::ConfiguredPathKind {
                role,
                variable,
                path: path.into_boxed_path(),
                expected: LocalHostPathKind::File,
            });
        }
        canonicalize_existing(role, &path)
    }

    pub(super) fn validate_directory(
        &self,
        role: LocalHostPathRole,
        variable: LocalHostVariable,
        path: PathBuf,
    ) -> Result<PathBuf, LocalCompilerHostError> {
        let metadata =
            fs::metadata(&path).map_err(|source| LocalCompilerHostError::ConfiguredPath {
                role,
                variable,
                path: path.clone().into_boxed_path(),
                source,
            })?;
        if !metadata.is_dir() {
            return Err(LocalCompilerHostError::ConfiguredPathKind {
                role,
                variable,
                path: path.into_boxed_path(),
                expected: LocalHostPathKind::Directory,
            });
        }
        canonicalize_existing(role, &path)
    }

    pub(super) fn executable_candidates(
        &self,
        home: Option<&Path>,
        tool: NativeTool,
    ) -> ArrayVec<PathBuf, PLATFORM_PATH_CAPACITY> {
        let name = match tool {
            NativeTool::Rustc => "rustc",
            NativeTool::Clang => "clang",
            NativeTool::Python => "python3",
            NativeTool::TypeScriptCompiler => "tsc",
            NativeTool::GoCompiler => "go",
            NativeTool::JavaCompiler => "javac",
            NativeTool::CSharpCompiler => "dotnet",
        };
        let mut candidates = self.auxiliary_candidates(home, name);
        if tool == NativeTool::GoCompiler {
            push_candidate(&mut candidates, PathBuf::from("/usr/local/go/bin/go"));
        }
        if tool == NativeTool::Clang {
            push_candidate(&mut candidates, PathBuf::from("/usr/bin/clang"));
        }
        if tool == NativeTool::Python {
            push_candidate(&mut candidates, PathBuf::from("/usr/bin/python3"));
        }
        if tool == NativeTool::CSharpCompiler {
            push_candidate(
                &mut candidates,
                PathBuf::from("/usr/local/share/dotnet/dotnet"),
            );
        }
        candidates
    }

    pub(super) fn auxiliary_candidates(
        &self,
        home: Option<&Path>,
        name: &str,
    ) -> ArrayVec<PathBuf, PLATFORM_PATH_CAPACITY> {
        let mut candidates = ArrayVec::new();
        push_candidate(&mut candidates, Path::new("/opt/homebrew/bin").join(name));
        push_candidate(&mut candidates, Path::new("/usr/local/bin").join(name));
        if let Some(home) = home {
            push_candidate(&mut candidates, home.join(".local/bin").join(name));
            push_candidate(&mut candidates, home.join(".nix-profile/bin").join(name));
            if let Some(user) = home.file_name().filter(|name| single_component(name)) {
                push_candidate(
                    &mut candidates,
                    Path::new("/etc/profiles/per-user")
                        .join(user)
                        .join("bin")
                        .join(name),
                );
            }
        }
        push_candidate(
            &mut candidates,
            Path::new("/run/current-system/sw/bin").join(name),
        );
        push_candidate(
            &mut candidates,
            Path::new("/nix/var/nix/profiles/default/bin").join(name),
        );
        candidates
    }

    pub(super) fn java_candidates(
        &self,
        home: Option<&Path>,
        jdk_root: Option<&Path>,
    ) -> ArrayVec<PathBuf, PLATFORM_PATH_CAPACITY> {
        let mut candidates = ArrayVec::new();
        if let Some(root) = jdk_root {
            push_candidate(&mut candidates, root.join("bin/javac"));
        }
        for candidate in self.auxiliary_candidates(home, "javac") {
            push_candidate(&mut candidates, candidate);
        }
        candidates
    }

    pub(super) fn jdk_candidates(
        &self,
        _home: Option<&Path>,
    ) -> ArrayVec<PathBuf, PLATFORM_PATH_CAPACITY> {
        let mut candidates = ArrayVec::new();
        push_candidate(
            &mut candidates,
            PathBuf::from("/opt/homebrew/opt/openjdk/libexec/openjdk.jdk/Contents/Home"),
        );
        push_candidate(
            &mut candidates,
            PathBuf::from("/Library/Java/JavaVirtualMachines/openjdk.jdk/Contents/Home"),
        );
        candidates
    }

    /// Locates the module root paired with one already-admitted TypeScript
    /// compiler. Exact compiler-relative roots remain valid in explicit-only
    /// mode because they derive from that selected compiler rather than an
    /// ambient search. Platform roots are considered only under platform
    /// discovery.
    pub(super) fn typescript_module_root(
        &self,
        compiler: Option<&Path>,
    ) -> Result<Option<PathBuf>, LocalCompilerHostError> {
        if let Some(configured) =
            self.optional_absolute_path(LocalHostVariable::NudoxTypeScriptModuleRoot)?
        {
            return self
                .validate_directory(
                    LocalHostPathRole::TypeScriptModuleRoot,
                    LocalHostVariable::NudoxTypeScriptModuleRoot,
                    configured,
                )
                .map(Some);
        }

        let mut candidates = ArrayVec::new();
        if let Some(compiler) = compiler {
            if let Some(binary_directory) = compiler.parent() {
                if let Some(compiler_root) = binary_directory.parent() {
                    push_candidate(&mut candidates, compiler_root.join("lib/node_modules"));
                    if compiler_root.file_name() == Some(OsStr::new("typescript")) {
                        if let Some(module_root) = compiler_root.parent() {
                            push_candidate(&mut candidates, module_root.to_path_buf());
                        }
                    }
                }
            }
        }
        if self.discovery == LocalHostDiscovery::PlatformDefaults {
            push_candidate(
                &mut candidates,
                PathBuf::from("/opt/homebrew/lib/node_modules"),
            );
            push_candidate(
                &mut candidates,
                PathBuf::from("/usr/local/lib/node_modules"),
            );
        }
        first_existing_directory(LocalHostPathRole::TypeScriptModuleRoot, candidates)
    }

    pub(super) fn package_root_candidates(
        &self,
        home: Option<&Path>,
        ecosystem: PackageEcosystem,
    ) -> ArrayVec<PathBuf, PLATFORM_PATH_CAPACITY> {
        let mut candidates = ArrayVec::new();
        let Some(home) = home else {
            return candidates;
        };
        match ecosystem {
            PackageEcosystem::Cargo => {
                push_candidate(&mut candidates, home.join(".cargo/registry/src"));
            }
            PackageEcosystem::Golang => {
                push_candidate(&mut candidates, home.join("go/pkg/mod"));
            }
            PackageEcosystem::Maven => {
                push_candidate(&mut candidates, home.join(".m2/repository"));
            }
            PackageEcosystem::Nuget => {
                push_candidate(&mut candidates, home.join(".nuget/packages"));
            }
            PackageEcosystem::Npm | PackageEcosystem::Pypi | PackageEcosystem::Generic => {}
        }
        candidates
    }
}

pub(super) fn create_directory(
    directory: LocalHostDirectory,
    path: &Path,
) -> Result<(), LocalCompilerHostError> {
    fs::create_dir_all(path).map_err(|source| LocalCompilerHostError::CreateDirectory {
        directory,
        path: path.to_path_buf().into_boxed_path(),
        source,
    })
}

fn first_existing(
    role: LocalHostPathRole,
    candidates: ArrayVec<PathBuf, PLATFORM_PATH_CAPACITY>,
) -> Result<Option<PathBuf>, LocalCompilerHostError> {
    for path in candidates {
        match fs::metadata(&path) {
            Ok(metadata) if metadata.is_file() => {
                return canonicalize_existing(role, &path).map(Some);
            }
            Ok(_) => continue,
            Err(source) if source.kind() == io::ErrorKind::NotFound => continue,
            Err(source) => {
                return Err(LocalCompilerHostError::Canonicalize {
                    role,
                    path: path.into_boxed_path(),
                    source,
                });
            }
        }
    }
    Ok(None)
}

fn first_existing_directory(
    role: LocalHostPathRole,
    candidates: ArrayVec<PathBuf, PLATFORM_PATH_CAPACITY>,
) -> Result<Option<PathBuf>, LocalCompilerHostError> {
    for path in candidates {
        match fs::metadata(&path) {
            Ok(metadata) if metadata.is_dir() => {
                return canonicalize_existing(role, &path).map(Some);
            }
            Ok(_) => continue,
            Err(source) if source.kind() == io::ErrorKind::NotFound => continue,
            Err(source) => {
                return Err(LocalCompilerHostError::Canonicalize {
                    role,
                    path: path.into_boxed_path(),
                    source,
                });
            }
        }
    }
    Ok(None)
}

pub(super) fn canonicalize_existing(
    role: LocalHostPathRole,
    path: &Path,
) -> Result<PathBuf, LocalCompilerHostError> {
    fs::canonicalize(path).map_err(|source| LocalCompilerHostError::Canonicalize {
        role,
        path: path.to_path_buf().into_boxed_path(),
        source,
    })
}

fn push_candidate(candidates: &mut ArrayVec<PathBuf, PLATFORM_PATH_CAPACITY>, candidate: PathBuf) {
    if candidates.len() < candidates.capacity() && !candidates.iter().any(|path| path == &candidate)
    {
        candidates.push(candidate);
    }
}

fn single_component(value: &OsStr) -> bool {
    !value.is_empty() && Path::new(value).components().count() == 1
}
