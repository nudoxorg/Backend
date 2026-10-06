//! Finite platform path selection and canonical filesystem admission.

use std::{
    ffi::OsStr,
    fs, io,
    io::Read,
    path::{Path, PathBuf},
};

use arrayvec::ArrayVec;
use backend_library::interface::PackageEcosystem;
use backend_semantic::vocabulary::NativeTool;
use sha2::{Digest, Sha256};

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

#[derive(Clone, Debug)]
pub(super) struct TypeScriptHostSelection {
    pub(super) compiler: Option<PathBuf>,
    pub(super) node: Option<TypeScriptNodeSelection>,
    pub(super) module_root: Option<PathBuf>,
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

    /// Selects one complete global TypeScript fallback for every local service surface.
    ///
    /// Project-local TypeScript is still selected first by `TypeScriptProjectHost`. This host
    /// tuple is used only when that project has no local compiler. An automatically discovered
    /// `tsc` is admitted only when its package root and a Node runtime are both available, so a
    /// PATH hit cannot create a half-configured global checker.
    pub(super) fn typescript_host_selection(
        &self,
        home: Option<&Path>,
    ) -> Result<TypeScriptHostSelection, LocalCompilerHostError> {
        let variable = LocalHostVariable::NudoxTypeScriptCompiler;
        let role = LocalHostPathRole::Native(NativeTool::TypeScriptCompiler);
        let explicit_compiler = self.optional_absolute_path(variable)?;
        let compiler_candidates = match explicit_compiler.as_ref() {
            Some(path) => vec![self.validate_file(role, variable, path.clone())?],
            None => typescript_compiler_candidates(home, self.environment.search_path()),
        };

        for candidate in compiler_candidates {
            let compiler = if explicit_compiler.is_some() {
                candidate
            } else {
                let mut one = ArrayVec::new();
                push_candidate(&mut one, candidate);
                let Some(compiler) = first_existing(role, one)? else {
                    continue;
                };
                compiler
            };
            let module_root = self.typescript_module_root(Some(&compiler))?;
            let node = self.typescript_node_executable(home, Some(&compiler))?;
            if explicit_compiler.is_some() || (module_root.is_some() && node.is_some()) {
                return Ok(TypeScriptHostSelection {
                    compiler: Some(compiler),
                    node,
                    module_root,
                });
            }
        }

        Ok(TypeScriptHostSelection {
            compiler: None,
            node: self.typescript_node_executable(home, None)?,
            module_root: self.typescript_module_root(None)?,
        })
    }

    /// Locates the Node runtime used by request-scoped project TypeScript admission.
    ///
    /// Node is paired with either a project-local TypeScript package or the global fallback
    /// selected by [`Self::typescript_host_selection`]. Keep discovery limited to this host:
    /// the service does not enable PATH discovery for other native compilers.
    pub(super) fn typescript_node_executable(
        &self,
        home: Option<&Path>,
        compiler: Option<&Path>,
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
        if let Some(directory) = compiler.and_then(Path::parent) {
            let candidate = directory.join(if cfg!(windows) { "node.exe" } else { "node" });
            let mut candidates = ArrayVec::new();
            push_candidate(&mut candidates, candidate);
            if let Some(path) = first_existing(role, candidates)? {
                return Ok(Some(TypeScriptNodeSelection {
                    path,
                    origin: TypeScriptSelectionOrigin::PairedHostInstall,
                }));
            }
        }
        if let Some(path) = bundled_typescript_node(std::env::current_exe().ok().as_deref())? {
            return self.validate_file(role, variable, path).map(|path| {
                Some(TypeScriptNodeSelection {
                    path,
                    origin: TypeScriptSelectionOrigin::ValidatedApplicationBundle,
                })
            });
        }
        if let Some(path) = first_executable_on_search_path(
            role,
            self.environment.search_path(),
            if cfg!(windows) { "node.exe" } else { "node" },
        )? {
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

fn first_executable_on_search_path(
    role: LocalHostPathRole,
    search_path: Option<std::ffi::OsString>,
    executable: &str,
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
        let path = directory.join(executable);
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

fn typescript_compiler_candidates(
    home: Option<&Path>,
    search_path: Option<std::ffi::OsString>,
) -> Vec<PathBuf> {
    const MAX_SEARCH_PATH_BYTES: usize = 64 * 1024;
    const MAX_SEARCH_PATH_ENTRIES: usize = 256;

    let mut candidates = Vec::new();
    if let Some(search_path) = search_path.filter(|path| path.len() <= MAX_SEARCH_PATH_BYTES) {
        for directory in std::env::split_paths(&search_path)
            .take(MAX_SEARCH_PATH_ENTRIES)
            .filter(|directory| directory.is_absolute())
        {
            let candidate = directory.join(if cfg!(windows) { "tsc.exe" } else { "tsc" });
            if !candidates.contains(&candidate) {
                candidates.push(candidate);
            }
        }
    }
    #[cfg(not(windows))]
    if let Some(home) = home {
        let candidate = home.join(".local/bin/tsc");
        if !candidates.contains(&candidate) {
            candidates.push(candidate);
        }
    }
    #[cfg(not(windows))]
    for candidate in [
        PathBuf::from("/opt/homebrew/bin/tsc"),
        PathBuf::from("/usr/local/bin/tsc"),
        PathBuf::from("/usr/bin/tsc"),
    ] {
        if !candidates.contains(&candidate) {
            candidates.push(candidate);
        }
    }
    candidates
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
    #[cfg(not(windows))]
    if let Some(home) = home {
        push_candidate(&mut candidates, home.join(".local/bin/node"));
    }
    #[cfg(not(windows))]
    for path in [
        "/opt/homebrew/bin/node",
        "/usr/local/bin/node",
        "/usr/bin/node",
    ] {
        push_candidate(&mut candidates, PathBuf::from(path));
    }
    #[cfg(windows)]
    push_candidate(
        &mut candidates,
        PathBuf::from("C:/Program Files/nodejs/node.exe"),
    );
    candidates
}

fn single_component(value: &OsStr) -> bool {
    !value.is_empty() && Path::new(value).components().count() == 1
}

const MAX_BUNDLE_MANIFEST_BYTES: u64 = 4 * 1024 * 1024;
const MAX_BUNDLE_EXECUTABLE_BYTES: u64 = 512 * 1024 * 1024;
const MAX_BUNDLED_NODE_BYTES: u64 = 512 * 1024 * 1024;

/// Discovers a Node runtime only for an executable directly installed in a recognized Nudox
/// macOS application bundle. The bundle's own finite inventory must bind both this executable
/// and the runtime bytes before the runtime is considered a candidate.
fn bundled_typescript_node(
    executable: Option<&Path>,
) -> Result<Option<PathBuf>, LocalCompilerHostError> {
    let Some(executable) = executable else {
        return Ok(None);
    };
    let Ok(executable) = fs::canonicalize(executable) else {
        return Ok(None);
    };
    let Some(macos) = executable.parent() else {
        return Ok(None);
    };
    let Some(contents) = macos.parent() else {
        return Ok(None);
    };
    let Some(bundle_root) = contents.parent() else {
        return Ok(None);
    };
    if macos.file_name() != Some(OsStr::new("MacOS"))
        || contents.file_name() != Some(OsStr::new("Contents"))
        || !bundle_root
            .file_name()
            .and_then(OsStr::to_str)
            .is_some_and(|name| name.ends_with(".app"))
        || !matches!(
            executable.file_name().and_then(OsStr::to_str),
            Some("backend-desktop" | "backend-cli" | "backend-mcp" | "backend-locald")
        )
    {
        return Ok(None);
    }

    let resources = contents.join("Resources");
    let manifest_path = resources.join("build-manifest.json");
    if fs::canonicalize(&manifest_path).ok().as_deref() != Some(manifest_path.as_path()) {
        return Err(LocalCompilerHostError::BundleManifest {
            path: manifest_path.into_boxed_path(),
            message: "bundle manifest is not at its canonical resource path".into(),
        });
    }
    let manifest_metadata = fs::symlink_metadata(&manifest_path).map_err(|source| {
        LocalCompilerHostError::BundleManifest {
            path: manifest_path.clone().into_boxed_path(),
            message: source.to_string().into_boxed_str(),
        }
    })?;
    if !manifest_metadata.is_file() || manifest_metadata.file_type().is_symlink() {
        return Err(LocalCompilerHostError::BundleManifest {
            path: manifest_path.into_boxed_path(),
            message: "expected a regular non-symlink bundle manifest".into(),
        });
    }
    let manifest_bytes =
        read_bounded(&manifest_path, MAX_BUNDLE_MANIFEST_BYTES).map_err(|source| {
            LocalCompilerHostError::BundleManifest {
                path: manifest_path.clone().into_boxed_path(),
                message: source.to_string().into_boxed_str(),
            }
        })?;
    let manifest: serde_json::Value =
        serde_json::from_slice(&manifest_bytes).map_err(|source| {
            LocalCompilerHostError::BundleManifest {
                path: manifest_path.clone().into_boxed_path(),
                message: source.to_string().into_boxed_str(),
            }
        })?;
    if manifest.get("schema").and_then(serde_json::Value::as_u64) != Some(1)
        || manifest.get("product").and_then(serde_json::Value::as_str) != Some("Nudox")
        || manifest
            .get("bundle")
            .and_then(|bundle| bundle.get("identifier"))
            .and_then(serde_json::Value::as_str)
            != Some("dev.nudox.desktop")
    {
        return Err(LocalCompilerHostError::BundleManifest {
            path: manifest_path.into_boxed_path(),
            message: "manifest does not describe a supported Nudox app bundle".into(),
        });
    }
    let inventory = manifest
        .get("files")
        .and_then(serde_json::Value::as_object)
        .ok_or_else(|| LocalCompilerHostError::BundleManifest {
            path: manifest_path.clone().into_boxed_path(),
            message: "manifest is missing its file inventory".into(),
        })?;

    let runtime_relative = "Contents/Resources/Helpers/typescript/node/bin/node";
    let executable_relative = executable
        .strip_prefix(bundle_root)
        .ok()
        .and_then(Path::to_str)
        .map(|path| path.replace('\\', "/"))
        .ok_or_else(|| LocalCompilerHostError::BundleManifest {
            path: manifest_path.clone().into_boxed_path(),
            message: "current executable is outside the bundle inventory".into(),
        })?;
    let Some(executable_record) = inventory.get(&executable_relative) else {
        return Err(LocalCompilerHostError::BundleManifest {
            path: manifest_path.into_boxed_path(),
            message: "manifest inventory does not contain the current application executable"
                .into(),
        });
    };
    validate_bundle_inventory_file(
        executable_record,
        &bundle_root.join(&executable_relative),
        MAX_BUNDLE_EXECUTABLE_BYTES,
        &manifest_path,
    )?;
    let runtime = bundle_root.join(runtime_relative);
    let Some(runtime_record) = inventory.get(runtime_relative) else {
        return Err(LocalCompilerHostError::BundleManifest {
            path: manifest_path.into_boxed_path(),
            message: "manifest inventory does not contain the bundled TypeScript Node runtime"
                .into(),
        });
    };
    validate_bundle_inventory_file(
        runtime_record,
        &runtime,
        MAX_BUNDLED_NODE_BYTES,
        &manifest_path,
    )?;
    let canonical_runtime =
        fs::canonicalize(&runtime).map_err(|source| LocalCompilerHostError::BundleManifest {
            path: runtime.clone().into_boxed_path(),
            message: source.to_string().into_boxed_str(),
        })?;
    if canonical_runtime != runtime || !canonical_runtime.starts_with(bundle_root) {
        return Err(LocalCompilerHostError::BundleManifest {
            path: runtime.into_boxed_path(),
            message: "bundled TypeScript Node runtime resolves through a symlink".into(),
        });
    }
    Ok(Some(canonical_runtime))
}

fn validate_bundle_inventory_file(
    record: &serde_json::Value,
    path: &Path,
    maximum_bytes: u64,
    manifest: &Path,
) -> Result<(), LocalCompilerHostError> {
    let mismatch = || LocalCompilerHostError::BundleManifest {
        path: manifest.to_path_buf().into_boxed_path(),
        message: format!("bundle file inventory does not match {:?}", path).into_boxed_str(),
    };
    if record.get("kind").and_then(serde_json::Value::as_str) != Some("file") {
        return Err(mismatch());
    }
    let metadata = fs::symlink_metadata(path).map_err(|_| mismatch())?;
    if !metadata.is_file() || metadata.file_type().is_symlink() || metadata.len() > maximum_bytes {
        return Err(mismatch());
    }
    let (length, digest) = sha256_file(path, maximum_bytes)?;
    if record.get("size_bytes").and_then(serde_json::Value::as_u64) != Some(length)
        || record.get("sha256").and_then(serde_json::Value::as_str) != Some(digest.as_str())
    {
        return Err(mismatch());
    }
    Ok(())
}

fn read_bounded(path: &Path, maximum_bytes: u64) -> io::Result<Vec<u8>> {
    let file = fs::File::open(path)?;
    if file.metadata()?.len() > maximum_bytes {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "file exceeds byte limit",
        ));
    }
    let mut bytes = Vec::new();
    file.take(maximum_bytes.saturating_add(1))
        .read_to_end(&mut bytes)?;
    if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > maximum_bytes {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "file exceeds byte limit",
        ));
    }
    Ok(bytes)
}

fn sha256_file(path: &Path, maximum_bytes: u64) -> Result<(u64, String), LocalCompilerHostError> {
    let path_metadata =
        fs::symlink_metadata(path).map_err(|source| LocalCompilerHostError::BundleManifest {
            path: path.to_path_buf().into_boxed_path(),
            message: source.to_string().into_boxed_str(),
        })?;
    if !path_metadata.is_file()
        || path_metadata.file_type().is_symlink()
        || path_metadata.len() > maximum_bytes
    {
        return Err(LocalCompilerHostError::BundleManifest {
            path: path.to_path_buf().into_boxed_path(),
            message: format!("file exceeds its {maximum_bytes}-byte hash bound").into_boxed_str(),
        });
    }
    let mut file =
        fs::File::open(path).map_err(|source| LocalCompilerHostError::BundleManifest {
            path: path.to_path_buf().into_boxed_path(),
            message: source.to_string().into_boxed_str(),
        })?;
    let opened_metadata =
        file.metadata()
            .map_err(|source| LocalCompilerHostError::BundleManifest {
                path: path.to_path_buf().into_boxed_path(),
                message: source.to_string().into_boxed_str(),
            })?;
    if !opened_metadata.is_file()
        || opened_metadata.len() > maximum_bytes
        || !same_file_identity(&path_metadata, &opened_metadata)
    {
        return Err(LocalCompilerHostError::BundleManifest {
            path: path.to_path_buf().into_boxed_path(),
            message: "file identity changed before hashing".into(),
        });
    }
    let (length, digest) = sha256_reader(&mut file, maximum_bytes, path)?;
    let after_handle =
        file.metadata()
            .map_err(|source| LocalCompilerHostError::BundleManifest {
                path: path.to_path_buf().into_boxed_path(),
                message: source.to_string().into_boxed_str(),
            })?;
    let after_path =
        fs::symlink_metadata(path).map_err(|source| LocalCompilerHostError::BundleManifest {
            path: path.to_path_buf().into_boxed_path(),
            message: source.to_string().into_boxed_str(),
        })?;
    if after_path.file_type().is_symlink()
        || !same_file_identity(&opened_metadata, &after_handle)
        || !same_file_identity(&opened_metadata, &after_path)
        || after_handle.len() != length
        || after_path.len() != length
    {
        return Err(LocalCompilerHostError::BundleManifest {
            path: path.to_path_buf().into_boxed_path(),
            message: "file changed while hashing".into(),
        });
    }
    Ok((length, digest))
}

fn sha256_reader(
    reader: &mut impl Read,
    maximum_bytes: u64,
    path: &Path,
) -> Result<(u64, String), LocalCompilerHostError> {
    let mut digest = Sha256::new();
    let mut total = 0_u64;
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let remaining = maximum_bytes.saturating_add(1).saturating_sub(total);
        if remaining == 0 {
            break;
        }
        let read_limit =
            usize::try_from(remaining.min(buffer.len() as u64)).unwrap_or(buffer.len());
        let read = reader.read(&mut buffer[..read_limit]).map_err(|source| {
            LocalCompilerHostError::BundleManifest {
                path: path.to_path_buf().into_boxed_path(),
                message: source.to_string().into_boxed_str(),
            }
        })?;
        if read == 0 {
            break;
        }
        total = total.saturating_add(u64::try_from(read).unwrap_or(u64::MAX));
        if total > maximum_bytes {
            return Err(LocalCompilerHostError::BundleManifest {
                path: path.to_path_buf().into_boxed_path(),
                message: format!("file grew beyond its {maximum_bytes}-byte hash bound")
                    .into_boxed_str(),
            });
        }
        digest.update(&buffer[..read]);
    }
    let digest = digest
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    Ok((total, digest))
}

#[cfg(unix)]
fn same_file_identity(left: &fs::Metadata, right: &fs::Metadata) -> bool {
    use std::os::unix::fs::MetadataExt;

    left.dev() == right.dev() && left.ino() == right.ino()
}

#[cfg(windows)]
fn same_file_identity(left: &fs::Metadata, right: &fs::Metadata) -> bool {
    use std::os::windows::fs::MetadataExt;

    left.volume_serial_number() == right.volume_serial_number()
        && left.file_index() == right.file_index()
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt as _;

    struct TestEnvironment {
        home: PathBuf,
        node: Option<PathBuf>,
        compiler: Option<PathBuf>,
        module_root: Option<PathBuf>,
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
                LocalHostVariable::NudoxTypeScriptCompiler => self
                    .compiler
                    .as_ref()
                    .map(|path| path.as_os_str().to_os_string()),
                LocalHostVariable::NudoxTypeScriptModuleRoot => self
                    .module_root
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
                compiler: None,
                module_root: None,
                search_path: None,
            },
            LocalHostDiscovery::ExplicitOnly,
        );
        assert_eq!(
            host.typescript_node_executable(Some(&home), None)
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
        let path_node = root.join("ordinary path/node");
        executable(&candidate);
        executable(&explicit);
        executable(&path_node);
        let search_path = std::env::join_paths([path_node.parent().expect("PATH directory")])
            .expect("encode fixture PATH");

        let host = LocalCompilerHost::new(
            TestEnvironment {
                home: home.clone(),
                node: Some(explicit.clone()),
                compiler: None,
                module_root: None,
                search_path: Some(search_path),
            },
            LocalHostDiscovery::ExplicitOnly,
        );
        let selection = host
            .typescript_node_executable(Some(&home), None)
            .expect("explicit node admission")
            .expect("explicit Node is selected");
        assert_eq!(
            selection.path,
            fs::canonicalize(&explicit).expect("canonical explicit node"),
        );
        assert_eq!(
            selection.origin,
            TypeScriptSelectionOrigin::ExplicitConfiguration
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
                compiler: None,
                module_root: None,
                search_path: Some(search_path),
            },
            LocalHostDiscovery::ExplicitOnly,
        );
        let selection = host
            .typescript_node_executable(None, None)
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

    #[test]
    fn explicit_only_service_discovers_a_paired_global_typescript_host_from_path() {
        let root = private_test_directory("typescript-paired-install");
        let home = root.join("home");
        let module_root = root.join("prefix/lib/node_modules");
        let compiler = module_root.join("typescript/bin/tsc");
        let node = compiler.with_file_name("node");
        executable(&compiler);
        executable(&node);
        let search_path = std::env::join_paths([compiler.parent().expect("compiler bin")])
            .expect("encode fixture PATH");

        let host = LocalCompilerHost::new(
            TestEnvironment {
                home,
                node: None,
                compiler: None,
                module_root: None,
                search_path: Some(search_path),
            },
            LocalHostDiscovery::ExplicitOnly,
        );
        let selection = host
            .typescript_host_selection(None)
            .expect("admit global TypeScript toolchain");
        let selected_compiler = selection.compiler.expect("PATH contains tsc");
        assert_eq!(
            selected_compiler,
            fs::canonicalize(&compiler).expect("canonical tsc")
        );
        assert_eq!(
            selection.module_root,
            Some(fs::canonicalize(&module_root).expect("canonical module root")),
        );
        let node_selection = selection.node.expect("compiler sibling provides Node");
        assert_eq!(
            node_selection.path,
            fs::canonicalize(&node).expect("canonical Node")
        );
        assert_eq!(
            node_selection.origin,
            TypeScriptSelectionOrigin::PairedHostInstall
        );

        fs::remove_dir_all(root).expect("remove test directory");
    }

    #[test]
    fn explicit_typescript_compiler_and_module_root_override_discovery() {
        let root = private_test_directory("typescript-explicit-precedence");
        let home = root.join("home");
        let path_compiler = root.join("path/typescript/bin/tsc");
        let explicit_compiler = root.join("chosen/typescript/bin/tsc");
        let explicit_module_root = root.join("chosen/node_modules");
        executable(&path_compiler);
        executable(&explicit_compiler);
        fs::create_dir_all(&explicit_module_root).expect("create selected module root");
        let search_path = std::env::join_paths([path_compiler.parent().expect("PATH directory")])
            .expect("encode fixture PATH");

        let host = LocalCompilerHost::new(
            TestEnvironment {
                home,
                node: None,
                compiler: Some(explicit_compiler.clone()),
                module_root: Some(explicit_module_root.clone()),
                search_path: Some(search_path),
            },
            LocalHostDiscovery::ExplicitOnly,
        );
        let selection = host
            .typescript_host_selection(None)
            .expect("admit configured compiler");
        let selected_compiler = selection.compiler.expect("explicit compiler is selected");
        assert_eq!(
            selected_compiler,
            fs::canonicalize(&explicit_compiler).expect("canonical tsc")
        );
        assert_eq!(
            selection.module_root,
            Some(fs::canonicalize(&explicit_module_root).expect("canonical module root")),
        );

        fs::remove_dir_all(root).expect("remove test directory");
    }

    #[test]
    fn bundled_node_requires_current_executable_and_runtime_inventory_matches() {
        let root = private_test_directory("typescript-bundle-node");
        let bundle = root.join("Nudox.app");
        let executable = bundle.join("Contents/MacOS/backend-mcp");
        let runtime = bundle.join("Contents/Resources/Helpers/typescript/node/bin/node");
        fs::create_dir_all(executable.parent().expect("executable parent"))
            .expect("create app executable directory");
        fs::create_dir_all(runtime.parent().expect("runtime parent"))
            .expect("create bundled runtime directory");
        fs::write(&executable, b"fixture executable").expect("write executable");
        fs::write(&runtime, b"fixture node").expect("write Node runtime");
        let manifest = bundle.join("Contents/Resources/build-manifest.json");
        let inventory_entry = |path: &Path| {
            let (size_bytes, sha256) =
                sha256_file(path, MAX_BUNDLE_EXECUTABLE_BYTES).expect("hash inventory fixture");
            serde_json::json!({
                "kind": "file",
                "size_bytes": size_bytes,
                "sha256": sha256,
            })
        };
        let inventory = serde_json::json!({
            "Contents/MacOS/backend-mcp": inventory_entry(&executable),
            "Contents/Resources/Helpers/typescript/node/bin/node": inventory_entry(&runtime),
        });
        fs::write(
            &manifest,
            serde_json::to_vec(&serde_json::json!({
                "schema": 1,
                "product": "Nudox",
                "bundle": {"identifier": "dev.nudox.desktop"},
                "files": inventory,
            }))
            .expect("serialize fixture manifest"),
        )
        .expect("write fixture manifest");

        assert_eq!(
            bundled_typescript_node(Some(&executable)).expect("validate bundle Node"),
            Some(fs::canonicalize(&runtime).expect("canonical bundled Node")),
        );
        assert_eq!(
            bundled_typescript_node(Some(&manifest))
                .expect("unrecognized executable path is ignored"),
            None,
        );

        fs::write(&runtime, b"modified Node").expect("change bundled Node bytes");
        assert!(matches!(
            bundled_typescript_node(Some(&executable)),
            Err(LocalCompilerHostError::BundleManifest { .. })
        ));
        fs::remove_dir_all(root).expect("remove test directory");
    }

    #[test]
    fn bundle_hashing_refuses_oversized_and_growing_payloads_with_bounded_reads() {
        use std::io::Read as IoRead;

        let root = private_test_directory("typescript-bundle-size-cap");
        let oversized = root.join("oversized-node");
        fs::write(&oversized, b"0123456789").expect("write oversized fixture");
        assert!(matches!(
            sha256_file(&oversized, 8),
            Err(LocalCompilerHostError::BundleManifest { .. })
        ));

        struct GrowingReader {
            remaining: usize,
            consumed: usize,
        }

        impl IoRead for GrowingReader {
            fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
                let length = buffer.len().min(self.remaining);
                buffer[..length].fill(b'x');
                self.remaining -= length;
                self.consumed += length;
                Ok(length)
            }
        }

        let mut growing = GrowingReader {
            remaining: 1024,
            consumed: 0,
        };
        assert!(matches!(
            sha256_reader(&mut growing, 8, Path::new("changing-bundle-payload")),
            Err(LocalCompilerHostError::BundleManifest { .. })
        ));
        assert_eq!(growing.consumed, 9);
        fs::remove_dir_all(root).expect("remove size-cap fixture");
    }
}
