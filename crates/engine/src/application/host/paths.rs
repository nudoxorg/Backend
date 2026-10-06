//! Finite platform path selection and canonical filesystem admission.

use std::{
    ffi::OsStr,
    fs, io,
    path::{Path, PathBuf},
};

use arrayvec::ArrayVec;
use backend_library::interface::PackageEcosystem;
use backend_semantic::vocabulary::NativeTool;

use super::{
    LocalCompilerHost, LocalCompilerHostError, LocalHostDiscovery, LocalHostEnvironment,
    LocalHostVariable, PLATFORM_PATH_CAPACITY,
};
use crate::application::typescript_host::TypeScriptSelectionOrigin;

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

#[derive(Clone, Debug)]
pub(super) struct TypeScriptNodeSelection {
    pub(super) path: PathBuf,
    pub(super) origin: TypeScriptSelectionOrigin,
}

/// Closed file or directory authority role.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LocalHostPathRole {
    /// One registry-selected native compiler executable.
    Native(NativeTool),
    /// Explicit libclang shared-library file or containing directory.
    Libclang,
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
    /// Explicit Cargo executable paired with the selected Rust compiler.
    Cargo,
    /// Explicit Cargo home containing registry and cache state.
    CargoHome,
    /// Go root reported by the selected Go compiler.
    GoRoot,
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
    /// Regular file or directory.
    FileOrDirectory,
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

    /// Locates the Node runtime used by request-scoped project TypeScript admission.
    ///
    /// Node is a host runtime for a compiler package already selected beneath the exact
    /// project root; it is not a globally admitted native compiler capability. Keep its
    /// discovery finite even when the long-running service uses `ExplicitOnly`, so ordinary
    /// local projects can use their own `node_modules/typescript` without enabling ambient
    /// `PATH` search or discovering other native compilers.
    pub(super) fn typescript_node_executable(
        &self,
        home: Option<&Path>,
    ) -> Result<Option<TypeScriptNodeSelection>, LocalCompilerHostError> {
        let variable = LocalHostVariable::NudoxTypeScriptNode;
        let role = LocalHostPathRole::TypeScriptNode;
        if let Some(path) = self.optional_absolute_path(variable)? {
            return self.validate_file(role, variable, path).map(|path| {
                Some(TypeScriptNodeSelection {
                    path,
                    origin: TypeScriptSelectionOrigin::ExplicitConfiguration,
                })
            });
        }
        if let Some(path) = first_existing_on_search_path(role, self.environment.search_path())? {
            return Ok(Some(TypeScriptNodeSelection {
                path,
                origin: TypeScriptSelectionOrigin::OrdinarySearchPath,
            }));
        }
        first_existing(role, typescript_node_candidates(home)).map(|path| {
            path.map(|path| TypeScriptNodeSelection {
                path,
                origin: TypeScriptSelectionOrigin::PlatformLocation,
            })
        })
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

    pub(super) fn file_or_directory(
        &self,
        variable: LocalHostVariable,
        role: LocalHostPathRole,
    ) -> Result<Option<PathBuf>, LocalCompilerHostError> {
        let Some(path) = self.optional_absolute_path(variable)? else {
            return Ok(None);
        };
        let metadata =
            fs::metadata(&path).map_err(|source| LocalCompilerHostError::ConfiguredPath {
                role,
                variable,
                path: path.clone().into_boxed_path(),
                source,
            })?;
        if !metadata.is_file() && !metadata.is_dir() {
            return Err(LocalCompilerHostError::ConfiguredPathKind {
                role,
                variable,
                path: path.into_boxed_path(),
                expected: LocalHostPathKind::FileOrDirectory,
            });
        }
        canonicalize_existing(role, &path).map(Some)
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
        canonicalize_executable_existing(role, &path)
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
                return canonicalize_executable_existing(role, &path).map(Some);
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

fn first_existing_on_search_path(
    role: LocalHostPathRole,
    search_path: Option<std::ffi::OsString>,
) -> Result<Option<PathBuf>, LocalCompilerHostError> {
    const MAX_SEARCH_PATH_BYTES: usize = 64 * 1024;
    const MAX_SEARCH_PATH_ENTRIES: usize = 256;

    let Some(search_path) = search_path else {
        return Ok(None);
    };
    if search_path.len() > MAX_SEARCH_PATH_BYTES {
        return Ok(None);
    }
    for directory in std::env::split_paths(&search_path).take(MAX_SEARCH_PATH_ENTRIES) {
        if !directory.is_absolute() {
            continue;
        }
        let path = directory.join(if cfg!(windows) { "node.exe" } else { "node" });
        match fs::metadata(&path) {
            Ok(metadata) if metadata.is_file() => {
                return canonicalize_executable_existing(role, &path).map(Some);
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

/// Resolves a selected executable file to the path a child process should execute.
///
/// Links are resolved to a real file except for a rustup proxy, which must keep its own name:
/// resolving `rustc` to the `rustup` binary it links to starts the toolchain manager, which
/// rejects `--print sysroot` and reports its own version for `--version`.
pub(super) fn canonicalize_executable_existing(
    role: LocalHostPathRole,
    path: &Path,
) -> Result<PathBuf, LocalCompilerHostError> {
    backend_frontend_rust::legacy::canonical_executable(path).map_err(|source| {
        LocalCompilerHostError::Canonicalize {
            role,
            path: path.to_path_buf().into_boxed_path(),
            source,
        }
    })
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

fn typescript_node_candidates(home: Option<&Path>) -> ArrayVec<PathBuf, PLATFORM_PATH_CAPACITY> {
    let mut candidates = ArrayVec::new();
    if let Some(home) = home {
        push_candidate(&mut candidates, home.join(".local/bin/node"));
        push_candidate(&mut candidates, home.join(".nix-profile/bin/node"));
        if let Some(user) = home.file_name().filter(|name| single_component(name)) {
            push_candidate(
                &mut candidates,
                Path::new("/etc/profiles/per-user")
                    .join(user)
                    .join("bin/node"),
            );
        }
    }
    push_candidate(&mut candidates, PathBuf::from("/opt/homebrew/bin/node"));
    push_candidate(&mut candidates, PathBuf::from("/usr/local/bin/node"));
    push_candidate(
        &mut candidates,
        PathBuf::from("/run/current-system/sw/bin/node"),
    );
    push_candidate(
        &mut candidates,
        PathBuf::from("/nix/var/nix/profiles/default/bin/node"),
    );
    push_candidate(&mut candidates, PathBuf::from("/usr/bin/node"));
    candidates
}

fn single_component(value: &OsStr) -> bool {
    !value.is_empty() && Path::new(value).components().count() == 1
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt as _;

    struct TestEnvironment {
        home: PathBuf,
        node: Option<PathBuf>,
        search_path: Option<std::ffi::OsString>,
    }

    impl LocalHostEnvironment for TestEnvironment {
        fn value(&self, variable: LocalHostVariable) -> Option<std::ffi::OsString> {
            match variable {
                LocalHostVariable::Home => Some(self.home.as_os_str().to_os_string()),
                LocalHostVariable::NudoxTypeScriptNode => self
                    .node
                    .as_ref()
                    .map(|path| path.as_os_str().to_os_string()),
                _ => None,
            }
        }

        fn search_path(&self) -> Option<std::ffi::OsString> {
            self.search_path.clone()
        }
    }

    fn private_test_directory(name: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "backend-typescript-host-{name}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock after epoch")
                .as_nanos(),
        ));
        fs::create_dir_all(&path).expect("create private test directory");
        fs::set_permissions(&path, fs::Permissions::from_mode(0o700))
            .expect("make test directory private");
        path
    }

    fn executable(path: &Path) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("create executable parent");
        }
        fs::write(path, b"#!/bin/sh\nexit 0\n").expect("write executable fixture");
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))
            .expect("make executable fixture executable");
    }

    #[test]
    fn explicit_only_service_finds_node_from_the_finite_user_host_locations() {
        let root = private_test_directory("node-runtime");
        let home = root.join("home with spaces");
        let node = home.join(".local/bin/node");
        executable(&node);

        let host = LocalCompilerHost::new(
            TestEnvironment {
                home: home.clone(),
                node: None,
                search_path: None,
            },
            LocalHostDiscovery::ExplicitOnly,
        );
        assert_eq!(
            host.typescript_node_executable(Some(&home))
                .expect("finite node admission")
                .map(|selection| selection.path),
            Some(fs::canonicalize(&node).expect("canonical node")),
            "project TypeScript gets a finite Node runtime without enabling ambient PATH discovery",
        );

        fs::remove_dir_all(root).expect("remove test directory");
    }

    #[test]
    fn explicit_node_override_precedes_finite_host_locations() {
        let root = private_test_directory("node-override");
        let home = root.join("home");
        let candidate = home.join(".local/bin/node");
        let explicit = root.join("chosen node");
        executable(&candidate);
        executable(&explicit);

        let host = LocalCompilerHost::new(
            TestEnvironment {
                home: home.clone(),
                node: Some(explicit.clone()),
                search_path: None,
            },
            LocalHostDiscovery::ExplicitOnly,
        );
        assert_eq!(
            host.typescript_node_executable(Some(&home))
                .expect("explicit node admission")
                .map(|selection| selection.path),
            Some(fs::canonicalize(&explicit).expect("canonical explicit node")),
        );

        fs::remove_dir_all(root).expect("remove test directory");
    }

    #[test]
    fn project_node_can_be_selected_from_process_path_then_pinned() {
        let root = private_test_directory("node-path-search");
        let home = root.join("home");
        let directory = root.join("ordinary path node");
        let node = directory.join("node");
        executable(&node);
        let search_path = std::env::join_paths([&directory]).expect("encode test PATH");

        let host = LocalCompilerHost::new(
            TestEnvironment {
                home,
                node: None,
                search_path: Some(search_path),
            },
            LocalHostDiscovery::ExplicitOnly,
        );
        let selection = host
            .typescript_node_executable(None)
            .expect("PATH Node is canonicalized during host admission")
            .expect("Node from PATH is selected");
        assert_eq!(
            selection.path,
            fs::canonicalize(&node).expect("canonical node"),
        );
        assert_eq!(
            selection.origin,
            TypeScriptSelectionOrigin::OrdinarySearchPath
        );
        fs::remove_dir_all(root).expect("remove test directory");
    }
}
