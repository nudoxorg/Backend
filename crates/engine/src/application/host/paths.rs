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
    /// Isolated module cache for a Go owner without an installed package cache.
    GoModuleCache,
}

#[derive(Clone, Debug)]
pub(super) struct TypeScriptNodeSelection {
    pub(super) path: PathBuf,
    pub(super) origin: TypeScriptSelectionOrigin,
}

#[derive(Clone, Debug)]
pub(super) struct TypeScriptHostSelection {
    pub(super) compiler: Option<PathBuf>,
    pub(super) compiler_explicit: bool,
    pub(super) compiler_origin: TypeScriptSelectionOrigin,
    pub(super) node: Option<TypeScriptNodeSelection>,
    pub(super) module_root: Option<PathBuf>,
    pub(super) module_root_explicit: bool,
    pub(super) report_program: Option<PathBuf>,
    pub(super) bundled_application: Option<PathBuf>,
}

/// The exact TypeScript SDK resources admitted from one relocatable application bundle.
#[derive(Clone, Debug)]
pub(crate) struct BundledTypeScriptSdk {
    pub(crate) compiler: PathBuf,
    pub(crate) node: Option<PathBuf>,
    pub(crate) module_root: PathBuf,
    pub(crate) proof: BundleTypeScriptResourceProof,
    pub(crate) origin: TypeScriptSelectionOrigin,
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
        if self.discovery == LocalHostDiscovery::ExplicitOnly
            || self.discovery == LocalHostDiscovery::ClosedSnapshot
        {
            return Ok(None);
        }
        if self.discovery == LocalHostDiscovery::InstalledTools
            && let Some(name) = installed_tool_name(variable)
            && let Some(path) =
                first_executable_on_search_path(role, self.environment.search_path(), name)?
        {
            return Ok(Some(path));
        }
        first_existing(role, candidates)
    }

    /// Selects one complete installed or bundled TypeScript fallback for every local surface.
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
        let module_root_explicit =
            self.optional_absolute_path(LocalHostVariable::NudoxTypeScriptModuleRoot)?
            .is_some();
        // Freeze the locator without hashing unused SDK resources. Closed owners never infer
        // it from their own executable, which may be a different sibling or distribution.
        let bundled_application = if self.discovery == LocalHostDiscovery::ClosedSnapshot {
            self.optional_absolute_path(LocalHostVariable::NudoxTypeScriptBundledApplication)?
        } else {
            recognized_bundle_application(std::env::current_exe().ok().as_deref())
        };
        let captured_default_compiler = if self.discovery == LocalHostDiscovery::ClosedSnapshot {
            self.optional_absolute_path(LocalHostVariable::NudoxTypeScriptDefaultCompiler)?
                .map(|path| {
                    self.validate_file(
                        role,
                        LocalHostVariable::NudoxTypeScriptDefaultCompiler,
                        path,
                    )
                })
                .transpose()?
        } else {
            None
        };
        let compiler_candidates = match explicit_compiler.as_ref() {
            Some(path) => vec![self.validate_file(role, variable, path.clone())?],
            None if captured_default_compiler.is_some() => {
                captured_default_compiler
                .clone()
                .into_iter()
                .collect()
            }
            None if self.discovery == LocalHostDiscovery::ClosedSnapshot => Vec::new(),
            None => typescript_compiler_candidates(home, self.environment.search_path()),
        };

        let explicit_report = self.executable(
            LocalHostVariable::NudoxTypeScriptReportProgram,
            LocalHostPathRole::TypeScriptReportProgram,
            ArrayVec::new(),
        )?;

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
            if explicit_compiler.is_some()
                || (module_root.is_some() && (node.is_some() || bundled_application.is_some())) {
                return Ok(TypeScriptHostSelection {
                    compiler: Some(compiler),
                    compiler_explicit: explicit_compiler.is_some(),
                    compiler_origin: if explicit_compiler.is_some() {
                        TypeScriptSelectionOrigin::ExplicitConfiguration
                    } else {
                        TypeScriptSelectionOrigin::InstalledHostSelection
                    },
                    node,
                    module_root,
                    module_root_explicit,
                    report_program: explicit_report.clone(),
                    bundled_application,
                });
            }
        }

        Ok(TypeScriptHostSelection {
            compiler: None,
            compiler_explicit: false,
            compiler_origin: TypeScriptSelectionOrigin::InstalledHostSelection,
            node: self.typescript_node_executable(home, None)?,
            module_root: self.typescript_module_root(None)?,
            module_root_explicit,
            report_program: explicit_report,
            bundled_application,
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
        if self.discovery == LocalHostDiscovery::ClosedSnapshot {
            for (variable, origin) in [
                (LocalHostVariable::NudoxTypeScriptBundledNode, TypeScriptSelectionOrigin::ValidatedApplicationBundle,
                ),
                (LocalHostVariable::NudoxTypeScriptDefaultNode, TypeScriptSelectionOrigin::InstalledHostSelection,
                ),
            ] {
                if let Some(path) = self.optional_absolute_path(variable)? {
                    return self.validate_file(role, variable, path).map(|path| Some(TypeScriptNodeSelection { path, origin }));
                }
            }
            return Ok(None);
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
        if self.discovery == LocalHostDiscovery::ExplicitOnly
            || self.discovery == LocalHostDiscovery::ClosedSnapshot
        {
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
        self.validate_file_or_directory(role, variable, path)
            .map(Some)
    }

    fn validate_file_or_directory(
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
        if !metadata.is_file() && !metadata.is_dir() {
            return Err(LocalCompilerHostError::ConfiguredPathKind {
                role,
                variable,
                path: path.into_boxed_path(),
                expected: LocalHostPathKind::FileOrDirectory,
            });
        }
        canonicalize_existing(role, &path)
    }

    /// Admits only explicitly named object roles before inferred cache state is realized.
    /// Location hints remain paths: their optional state directories may not exist yet.
    pub(super) fn canonical_configured_selection_path(
        &self,
        variable: LocalHostVariable,
        path: PathBuf,
    ) -> Result<PathBuf, LocalCompilerHostError> {
        use LocalHostPathKind::{Directory, File, FileOrDirectory};
        use LocalHostPathRole::{Native, PackageRoot};
        let (role, kind) = match variable {
            LocalHostVariable::NudoxDataRoot
            | LocalHostVariable::Home
            | LocalHostVariable::XdgDataHome
            | LocalHostVariable::LocalAppData => return Ok(path),
            LocalHostVariable::NudoxRustc => (Native(NativeTool::Rustc), File),
            LocalHostVariable::NudoxRustSysroot => (LocalHostPathRole::RustSysroot, Directory),
            LocalHostVariable::NudoxCargo => (LocalHostPathRole::Cargo, File),
            LocalHostVariable::NudoxCargoHome => (LocalHostPathRole::CargoHome, Directory),
            LocalHostVariable::NudoxClang => (Native(NativeTool::Clang), File),
            LocalHostVariable::LibclangPath => (LocalHostPathRole::Libclang, FileOrDirectory),
            LocalHostVariable::NudoxPython => (Native(NativeTool::Python), File),
            LocalHostVariable::NudoxTypeScriptCompiler
            | LocalHostVariable::NudoxTypeScriptDefaultCompiler => {
                (Native(NativeTool::TypeScriptCompiler), File)
            }
            LocalHostVariable::NudoxGo => (Native(NativeTool::GoCompiler), File),
            LocalHostVariable::NudoxJavaCompiler => (Native(NativeTool::JavaCompiler), File),
            LocalHostVariable::NudoxDotnet => (Native(NativeTool::CSharpCompiler), File),
            LocalHostVariable::NudoxTypeScriptNode
            | LocalHostVariable::NudoxTypeScriptDefaultNode
            | LocalHostVariable::NudoxTypeScriptBundledNode => {
                (LocalHostPathRole::TypeScriptNode, File)
            }
            LocalHostVariable::NudoxTypeScriptModuleRoot
            | LocalHostVariable::NudoxTypeScriptDefaultModuleRoot => {
                (LocalHostPathRole::TypeScriptModuleRoot, Directory)
            }
            LocalHostVariable::NudoxTypeScriptBundledApplication => {
                (LocalHostPathRole::TypeScriptNode, File)
            }
            LocalHostVariable::NudoxTypeScriptReportProgram => {
                (LocalHostPathRole::TypeScriptReportProgram, File)
            }
            LocalHostVariable::NudoxPyrefly => (LocalHostPathRole::Pyrefly, File),
            LocalHostVariable::NudoxGoOracle => (LocalHostPathRole::GoOracle, File),
            LocalHostVariable::NudoxJdk => (LocalHostPathRole::JdkRoot, Directory),
            LocalHostVariable::NudoxRoslynHelper => (LocalHostPathRole::RoslynHelper, File),
            LocalHostVariable::NudoxCargoRoot => (PackageRoot(PackageEcosystem::Cargo), Directory),
            LocalHostVariable::NudoxNpmRoot => (PackageRoot(PackageEcosystem::Npm), Directory),
            LocalHostVariable::NudoxPypiRoot => (PackageRoot(PackageEcosystem::Pypi), Directory),
            LocalHostVariable::NudoxGoRoot => (PackageRoot(PackageEcosystem::Golang), Directory),
            LocalHostVariable::NudoxMavenRoot => (PackageRoot(PackageEcosystem::Maven), Directory),
            LocalHostVariable::NudoxNugetRoot => (PackageRoot(PackageEcosystem::Nuget), Directory),
            LocalHostVariable::NudoxGenericRoot => {
                (PackageRoot(PackageEcosystem::Generic), Directory)
            }
        };
        match kind {
            File => self.validate_file(role, variable, path),
            Directory => self.validate_directory(role, variable, path),
            FileOrDirectory => self.validate_file_or_directory(role, variable, path),
        }
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
    /// compiler. Exact compiler-relative roots remain valid in explicit-only mode because they
    /// derive from that selected compiler rather than an ambient search. No unrelated module root
    /// is selected when the compiler does not identify one.
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

        if self.discovery == LocalHostDiscovery::ClosedSnapshot {
            return self
                .optional_absolute_path(LocalHostVariable::NudoxTypeScriptDefaultModuleRoot)?
                .map(|path| {
                    self.validate_directory(
                        LocalHostPathRole::TypeScriptModuleRoot,
                        LocalHostVariable::NudoxTypeScriptDefaultModuleRoot,
                        path,
                    )
                })
                .transpose();
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
        if ecosystem == PackageEcosystem::Golang {
            if let Some(cache) = self.environment.go_module_cache().map(PathBuf::from)
                && cache.is_absolute()
            {
                push_candidate(&mut candidates, cache);
            }
            if let Some(gopath) = self.environment.go_path() {
                for root in std::env::split_paths(&gopath).take(PLATFORM_PATH_CAPACITY) {
                    if root.is_absolute() {
                        push_candidate(&mut candidates, root.join("pkg/mod"));
                    }
                }
            }
            if let Some(home) = home {
                push_candidate(&mut candidates, home.join("go/pkg/mod"));
            }
            return candidates;
        }
        let Some(home) = home else {
            return candidates;
        };
        match ecosystem {
            PackageEcosystem::Cargo => {
                push_candidate(&mut candidates, home.join(".cargo/registry/src"));
            }
            PackageEcosystem::Golang => unreachable!("handled above"),
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

fn installed_tool_name(variable: LocalHostVariable) -> Option<&'static str> {
    match variable {
        LocalHostVariable::NudoxPython => Some(if cfg!(windows) {
            "python.exe"
        } else {
            "python3"
        }),
        LocalHostVariable::NudoxGo => Some(if cfg!(windows) { "go.exe" } else { "go" }),
        LocalHostVariable::NudoxPyrefly => Some(if cfg!(windows) {
            "pyrefly.exe"
        } else {
            "pyrefly"
        }),
        _ => None,
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
const MAX_BUNDLE_HELPER_RECEIPT_BYTES: u64 = 16 * 1024 * 1024;
const MAX_BUNDLE_TYPESCRIPT_PACKAGE_FILES: usize = 512;
const MAX_BUNDLE_TYPESCRIPT_PACKAGE_BYTES: u64 = 96 * 1024 * 1024;

/// Admits only the helper receipt metadata; package bytes remain lazy until that SDK wins.
fn admitted_bundle_helper_receipt(
    bundle_root: &Path,
    manifest_path: &Path,
    manifest: &serde_json::Value,
    inventory: &serde_json::Map<String, serde_json::Value>,
    proof: &mut BundleTypeScriptResourceProof,
) -> Result<Option<(PathBuf, serde_json::Value)>, LocalCompilerHostError> {
    let Some(helpers) = manifest.get("compiler_helpers") else {
        return Ok(None);
    };
    if helpers
        .get("files")
        .and_then(serde_json::Value::as_object)
        .is_none()
    {
        return Err(LocalCompilerHostError::BundleManifest {
            path: manifest_path.to_path_buf().into_boxed_path(),
            message: "compiler helper manifest is missing its file receipt".into(),
        });
    }

    let helper_receipt_relative = "Contents/Resources/Build Evidence/compiler-helpers-receipt.json";
    let helper_receipt_path = bundle_root.join(helper_receipt_relative);
    let helper_receipt_record = inventory.get(helper_receipt_relative).ok_or_else(|| {
        LocalCompilerHostError::BundleManifest {
            path: manifest_path.to_path_buf().into_boxed_path(),
            message: "manifest inventory does not contain the compiler-helper receipt".into(),
        }
    })?;
    validate_bundle_inventory_file(
        helper_receipt_record,
        &helper_receipt_path,
        MAX_BUNDLE_HELPER_RECEIPT_BYTES,
        manifest_path,
    )?;
    let (_, helper_receipt_digest) =
        sha256_file(&helper_receipt_path, MAX_BUNDLE_HELPER_RECEIPT_BYTES)?;
    if helpers
        .get("receipt_sha256")
        .and_then(serde_json::Value::as_str)
        != Some(helper_receipt_digest.as_str())
    {
        return Err(LocalCompilerHostError::BundleManifest {
            path: manifest_path.to_path_buf().into_boxed_path(),
            message: "compiler-helper receipt digest differs from the bundle manifest".into(),
        });
    }
    let helper_receipt_bytes = read_bounded(&helper_receipt_path, MAX_BUNDLE_HELPER_RECEIPT_BYTES)
        .map_err(|source| LocalCompilerHostError::BundleManifest {
            path: helper_receipt_path.clone().into_boxed_path(),
            message: source.to_string().into_boxed_str(),
        })?;
    let helper_proof = BundleTypeScriptResourceProof::from_bytes(
        &helper_receipt_path,
        MAX_BUNDLE_HELPER_RECEIPT_BYTES,
        &helper_receipt_bytes,
    )?;
    if helper_proof.files[0].2 != helper_receipt_digest {
        return Err(LocalCompilerHostError::BundleManifest {
            path: helper_receipt_path.clone().into_boxed_path(),
            message: "helper receipt changed between inventory admission and parsing".into(),
        });
    }
    proof.files.extend(helper_proof.files);
    let helper_receipt: serde_json::Value =
        serde_json::from_slice(&helper_receipt_bytes).map_err(|source| {
            LocalCompilerHostError::BundleManifest {
                path: helper_receipt_path.clone().into_boxed_path(),
                message: source.to_string().into_boxed_str(),
            }
        })?;
    if helper_receipt
        .get("schema")
        .and_then(serde_json::Value::as_u64)
        != Some(1)
        || helper_receipt.get("source") != manifest.get("source")
        || helper_receipt.get("files") != helpers.get("files")
        || helper_receipt.get("tools") != helpers.get("tools")
        || helper_receipt
            .get("target")
            .and_then(serde_json::Value::as_str)
            != manifest
                .get("target")
                .and_then(|target| target.get("triple"))
                .and_then(serde_json::Value::as_str)
    {
        return Err(LocalCompilerHostError::BundleManifest {
            path: helper_receipt_path.clone().into_boxed_path(),
            message: "compiler-helper receipt differs from the admitted bundle metadata".into(),
        });
    }
    Ok(Some((helper_receipt_path, helper_receipt)))
}

fn selected_bundle_node_version(
    receipt_path: &Path,
    receipt: &serde_json::Value,
    runtime_record: &serde_json::Value,
) -> Result<Box<str>, LocalCompilerHostError> {
    let file_digest = receipt["files"]["typescript/node/bin/node"].as_str();
    let tool_digest = receipt["tools"]["node"]["sha256"].as_str();
    let inventory_digest = runtime_record["sha256"].as_str();
    if file_digest.is_none() || file_digest != tool_digest || file_digest != inventory_digest {
        return Err(LocalCompilerHostError::BundleManifest {
            path: receipt_path.to_path_buf().into_boxed_path(),
            message: "selected Node digest differs between bundle inventory, SDK receipt and tool identity".into(),
        });
    }
    receipt["tools"]["node"]["version"]
        .as_str()
        .filter(|version| !version.is_empty())
        .map(Into::into)
        .ok_or_else(|| LocalCompilerHostError::BundleManifest {
            path: receipt_path.to_path_buf().into_boxed_path(),
            message: "selected SDK Node has no recorded version identity".into(),
        })
}

/// Discovers a Node runtime only for an executable directly installed in a recognized Nudox
/// macOS application bundle. The bundle's own finite inventory must bind both this executable
/// and the runtime bytes before the runtime is considered a candidate.
fn bundled_typescript_node(
    executable: Option<&Path>,
) -> Result<Option<(PathBuf, BundleTypeScriptResourceProof)>, LocalCompilerHostError> {
    if let Some(executable) = executable
        && let Some(root) = standalone_package_root(executable)
    {
        return standalone_typescript_runtime(executable, &root)
            .map(|runtime| runtime.map(|(node, proof)| (node, proof)));
    }
    let Some(executable) = executable else {
        return Ok(None);
    };
    let canonical = fs::canonicalize(executable).map_err(|source| LocalCompilerHostError::BundleManifest {
            path: executable.to_path_buf().into_boxed_path(),
            message: source.to_string().into_boxed_str(),
        })?;
    if canonical != executable {
        return Err(LocalCompilerHostError::BundleManifest {
            path: executable.to_path_buf().into_boxed_path(),
            message: "captured application locator resolves through a new alias".into(),
        });
    }
    let executable = canonical;
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
    let mut proof = BundleTypeScriptResourceProof::from_bytes(
        &manifest_path,
        MAX_BUNDLE_MANIFEST_BYTES,
        &manifest_bytes,
    )?;
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
    retain_selected_inventory_file(
        &mut proof, runtime_record,
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
    if let Some((receipt_path, receipt)) = admitted_bundle_helper_receipt(
        bundle_root, &manifest_path, &manifest, inventory, &mut proof,
    )? {
        proof.expected_node_version = Some(selected_bundle_node_version(
            &receipt_path, &receipt, runtime_record,
        )?);
    }
    proof.validate_current()?;
    Ok(Some((canonical_runtime, proof)))
}

/// Resolves the compiler, Node runtime, module root, and report program as one receipt-bound SDK.
/// Every path is derived from the recognized application bundle, so the archive remains
/// relocatable and selection never falls back to a build-machine store path.
pub(crate) fn bundled_typescript_sdk(
    executable: Option<&Path>,
    runtime_required: bool,
) -> Result<Option<BundledTypeScriptSdk>, LocalCompilerHostError> {
    if let Some(executable) = executable
        && let Some(root) = standalone_package_root(executable)
    {
        return standalone_typescript_sdk(executable, &root, runtime_required);
    }
    let Some(executable) = executable else {
        return Ok(None);
    };
    let canonical = fs::canonicalize(executable).map_err(|source| LocalCompilerHostError::BundleManifest {
            path: executable.to_path_buf().into_boxed_path(),
            message: source.to_string().into_boxed_str(),
        })?;
    if canonical != executable {
        return Err(LocalCompilerHostError::BundleManifest {
            path: executable.to_path_buf().into_boxed_path(),
            message: "captured application locator resolves through a new alias".into(),
        });
    }
    let executable = canonical;
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

    let manifest_path = contents.join("Resources/build-manifest.json");
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
    let mut proof = BundleTypeScriptResourceProof::from_bytes(
        &manifest_path,
        MAX_BUNDLE_MANIFEST_BYTES,
        &manifest_bytes,
    )?;
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
    let executable_relative = executable
        .strip_prefix(bundle_root)
        .ok()
        .and_then(Path::to_str)
        .map(|path| path.replace('\\', "/"))
        .ok_or_else(|| LocalCompilerHostError::BundleManifest {
            path: manifest_path.clone().into_boxed_path(),
            message: "current executable is outside the bundle inventory".into(),
        })?;
    let executable_record = inventory.get(&executable_relative).ok_or_else(|| {
        LocalCompilerHostError::BundleManifest {
            path: manifest_path.clone().into_boxed_path(),
            message: "manifest inventory does not contain the current application executable"
                .into(),
        }
    })?;
    validate_bundle_inventory_file(
        executable_record,
        &bundle_root.join(&executable_relative),
        MAX_BUNDLE_EXECUTABLE_BYTES,
        &manifest_path,
    )?;

    // Older/partial app bundles can still provide their separately validated Node runtime,
    // while only a manifest with the complete helper receipt may supply a default SDK.
    let Some(helpers) = manifest.get("compiler_helpers") else {
        return Ok(None);
    };
    let Some(version) = helpers
        .get("tools")
        .and_then(|tools| tools.get("typescript"))
        .and_then(|typescript| typescript.get("version"))
        .and_then(serde_json::Value::as_str)
        .filter(|version| !version.is_empty())
    else {
        return Ok(None);
    };
    let (helper_receipt_path, helper_receipt) = admitted_bundle_helper_receipt(
        bundle_root, &manifest_path, &manifest, inventory, &mut proof,
    )?.expect("complete SDK has compiler helper metadata");
    let receipt_typescript_version = helper_receipt
        .get("tools")
        .and_then(|tools| tools.get("typescript"))
        .and_then(|typescript| typescript.get("version"))
        .and_then(serde_json::Value::as_str);
    if receipt_typescript_version != Some(version) {
        return Err(LocalCompilerHostError::BundleManifest {
            path: helper_receipt_path.into_boxed_path(),
            message: "TypeScript version differs between helper and bundle receipts".into(),
        });
    }

    let node_receipt_digest = helper_receipt
        .get("files")
        .and_then(|files| files.get("typescript/node/bin/node"))
        .and_then(serde_json::Value::as_str);
    let node_tool_digest = helper_receipt
        .get("tools")
        .and_then(|tools| tools.get("node"))
        .and_then(|node| node.get("sha256"))
        .and_then(serde_json::Value::as_str);
    if node_receipt_digest.is_none() || node_receipt_digest != node_tool_digest {
        return Err(LocalCompilerHostError::BundleManifest {
            path: helper_receipt_path.into_boxed_path(),
            message:
                "bundled Node digest differs between the TypeScript SDK receipt and tool identity"
                    .into(),
        });
    }

    let helper_root_relative = "Contents/Resources/Helpers";
    let package_relative = "typescript/node_modules/typescript";
    let package_prefix = format!("{package_relative}/");
    let receipt_files = helper_receipt
        .get("files")
        .and_then(serde_json::Value::as_object)
        .expect("receipt files were compared with the required helper inventory");
    let mut expected_package_files = std::collections::BTreeSet::new();
    let mut package_bytes = 0_u64;
    let mut package_file_count = 0_usize;
    for (relative, receipt_digest) in receipt_files {
        let Some(package_file) = relative.strip_prefix(&package_prefix) else {
            continue;
        };
        if package_file.is_empty()
            || relative.contains('\\')
            || Path::new(relative)
                .components()
                .any(|component| !matches!(component, std::path::Component::Normal(_)))
            || !expected_package_files.insert(package_file.to_owned())
        {
            return Err(LocalCompilerHostError::BundleManifest {
                path: helper_receipt_path.clone().into_boxed_path(),
                message: "TypeScript package receipt contains an unsafe or duplicate path".into(),
            });
        }
        package_file_count += 1;
        if package_file_count > MAX_BUNDLE_TYPESCRIPT_PACKAGE_FILES {
            return Err(LocalCompilerHostError::BundleManifest {
                path: helper_receipt_path.clone().into_boxed_path(),
                message: "TypeScript package receipt exceeds its file-count bound".into(),
            });
        }
        let bundle_relative = format!("{helper_root_relative}/{relative}");
        let file_record = inventory.get(&bundle_relative).ok_or_else(|| {
            LocalCompilerHostError::BundleManifest {
                path: manifest_path.clone().into_boxed_path(),
                message: "bundle file inventory omits a receipted TypeScript package file".into(),
            }
        })?;
        let top_level_digest = file_record
            .get("sha256")
            .and_then(serde_json::Value::as_str);
        if receipt_digest.as_str() != top_level_digest {
            return Err(LocalCompilerHostError::BundleManifest {
                path: manifest_path.clone().into_boxed_path(),
                message: "TypeScript package digest differs between bundle receipts".into(),
            });
        }
        let path = bundle_root.join(&bundle_relative);
        retain_selected_inventory_file(
            &mut proof, file_record,
            &path,
            MAX_BUNDLE_TYPESCRIPT_PACKAGE_BYTES,
            &manifest_path,
        )?;
        let metadata =
            fs::metadata(&path).map_err(|source| LocalCompilerHostError::BundleManifest {
                path: path.clone().into_boxed_path(),
                message: source.to_string().into_boxed_str(),
            })?;
        package_bytes = package_bytes.saturating_add(metadata.len());
        if package_bytes > MAX_BUNDLE_TYPESCRIPT_PACKAGE_BYTES {
            return Err(LocalCompilerHostError::BundleManifest {
                path: manifest_path.clone().into_boxed_path(),
                message: "TypeScript package exceeds its total-byte bound".into(),
            });
        }
    }
    if !expected_package_files.contains("package.json")
        || !expected_package_files.contains("bin/tsc")
        || !expected_package_files.contains("lib/typescript.js")
    {
        return Err(LocalCompilerHostError::BundleManifest {
            path: helper_receipt_path.clone().into_boxed_path(),
            message:
                "TypeScript package receipt omits package metadata, compiler script or Compiler API"
                    .into(),
        });
    }

    let package_root =
        bundle_root.join("Contents/Resources/Helpers/typescript/node_modules/typescript");
    let package_root_canonical = fs::canonicalize(&package_root).map_err(|source| {
        LocalCompilerHostError::BundleManifest {
            path: package_root.clone().into_boxed_path(),
            message: source.to_string().into_boxed_str(),
        }
    })?;
    if package_root_canonical != package_root {
        return Err(LocalCompilerHostError::BundleManifest {
            path: package_root.into_boxed_path(),
            message: "TypeScript package tree resolves through a symlink".into(),
        });
    }
    let mut actual_package_files = std::collections::BTreeSet::new();
    let expected_directories = expected_package_files
        .iter()
        .flat_map(|relative| {
            Path::new(relative)
                .ancestors()
                .skip(1)
                .filter(|parent| !parent.as_os_str().is_empty())
                .map(|parent| parent.to_string_lossy().into_owned())
                .collect::<Vec<_>>()
        })
        .collect::<std::collections::BTreeSet<_>>();
    collect_bundle_typescript_files(&package_root, &package_root,
        &expected_directories, &mut actual_package_files,
    )?;
    if actual_package_files != expected_package_files {
        return Err(LocalCompilerHostError::BundleManifest {
            path: package_root.into_boxed_path(),
            message: "installed TypeScript package tree differs from its complete receipt".into(),
        });
    }
    proof.package_closures.push((package_root.clone(), expected_package_files));
    let package_manifest_path = package_root.join("package.json");
    let package_manifest_bytes =
        read_bounded(&package_manifest_path, 64 * 1024).map_err(|source| {
            LocalCompilerHostError::BundleManifest {
                path: package_manifest_path.clone().into_boxed_path(),
                message: source.to_string().into_boxed_str(),
            }
        })?;
    let package_manifest: serde_json::Value = serde_json::from_slice(&package_manifest_bytes)
        .map_err(|source| LocalCompilerHostError::BundleManifest {
            path: package_manifest_path.clone().into_boxed_path(),
            message: source.to_string().into_boxed_str(),
        })?;
    if package_manifest
        .get("version")
        .and_then(serde_json::Value::as_str)
        != Some(version)
    {
        return Err(LocalCompilerHostError::BundleManifest {
            path: package_manifest_path.into_boxed_path(),
            message: "installed TypeScript package version differs from its SDK receipt".into(),
        });
    }

    let compiler = package_root.join("bin/tsc");
    let module_root = bundle_root.join("Contents/Resources/Helpers/typescript/node_modules");
    let node = if runtime_required {
        let runtime_relative = "Contents/Resources/Helpers/typescript/node/bin/node";
        let runtime_path = bundle_root.join(runtime_relative);
        let runtime_record =
        inventory
            .get(runtime_relative)
            .ok_or_else(|| {
            LocalCompilerHostError::BundleManifest {
                path: manifest_path.clone().into_boxed_path(),
                message: "bundle inventory omits the selected Node runtime".into(),
            }
        })?;
        proof.expected_node_version = Some(selected_bundle_node_version(
            &helper_receipt_path, &helper_receipt, runtime_record,
        )?);
    retain_selected_inventory_file(
            &mut proof, runtime_record,
        &runtime_path,
            MAX_BUNDLED_NODE_BYTES,
        &manifest_path,
    )?;
    let node =
            canonicalize_executable_existing(LocalHostPathRole::TypeScriptNode, &runtime_path)?;
    if node != runtime_path {
        return Err(LocalCompilerHostError::BundleManifest {
            path: runtime_path.into_boxed_path(),
            message: "selected bundled Node resolves through a symlink".into(),
        });
    }
        Some(node)
    } else {
        None
    };
    proof.validate_current()?;
    Ok(Some(BundledTypeScriptSdk {
        compiler,
        node,
        module_root,
        proof,
        origin: TypeScriptSelectionOrigin::ValidatedApplicationBundle,
    }))
}

/// A closed resource locator does not grant SDK authority until request admission.
fn recognized_bundle_application(executable: Option<&Path>) -> Option<PathBuf> {
    let executable = fs::canonicalize(executable?).ok()?;
    if standalone_package_root(&executable).is_some() {
        return Some(executable);
    }
    let macos = executable.parent()?;
    let contents = macos.parent()?;
    let bundle = contents.parent()?;
    (macos.file_name() == Some(OsStr::new("MacOS"))
        && contents.file_name() == Some(OsStr::new("Contents"))
        && bundle.file_name()?.to_str()?.ends_with(".app")
        && matches!(
            executable.file_name()?.to_str()?,
            "backend-desktop" | "backend-cli" | "backend-mcp" | "backend-locald"
        ))
    .then_some(executable)
}

/// Small immutable receipt evidence for the selected resources. Compiler, package tree and
/// Node object/content witnesses are retained by TypeScriptProjectWitness itself.
#[derive(Clone, Debug)]
pub(crate) struct BundleTypeScriptResourceProof {
    files: Vec<(PathBuf, u64, String, fs::Metadata)>,
    pub(crate) expected_node_version: Option<Box<str>>,
    package_closures: Vec<(PathBuf, std::collections::BTreeSet<String>)>,
}

impl BundleTypeScriptResourceProof {
    fn from_bytes(path: &Path, maximum: u64, bytes: &[u8]) -> Result<Self, LocalCompilerHostError> {
        let metadata = fs::symlink_metadata(path).map_err(|source| {
            LocalCompilerHostError::BundleManifest {
                path: path.to_path_buf().into_boxed_path(),
                message: source.to_string()
                        .into_boxed_str(),
                }
        })?;
        let digest = format!("{:x}", Sha256::digest(bytes));
        let proof = Self {
            files: vec![(path.to_path_buf(),
            maximum, digest, metadata)],
            expected_node_version: None,
            package_closures: Vec::new(),
        };
        proof.validate_current()?;
        Ok(proof)
    }

    pub(crate) fn validate_current(&self) -> Result<(), LocalCompilerHostError> {
        for (root, expected) in &self.package_closures {
            if fs::canonicalize(root).ok().as_deref() != Some(root.as_path()) {
                return Err(LocalCompilerHostError::BundleManifest {
                    path: root.clone().into_boxed_path(), message: "selected SDK package root changed after admission".into(),
                });
            }
            let directories = expected.iter().flat_map(|relative| {
                Path::new(relative).ancestors().skip(1).filter(|parent| !parent.as_os_str().is_empty())
                    .map(|parent| parent.to_string_lossy().into_owned()).collect::<Vec<_>>()
            }).collect::<std::collections::BTreeSet<_>>();
            let mut actual = std::collections::BTreeSet::new();
            collect_bundle_typescript_files(root, root, &directories, &mut actual)?;
            if &actual != expected {
                return Err(LocalCompilerHostError::BundleManifest {
                    path: root.clone().into_boxed_path(), message: "selected SDK package member closure changed after admission".into(),
                });
            }
        }
        for (path, maximum, expected, identity) in &self.files {
            let (_, observed) = sha256_file(path, *maximum)?;
    let metadata = fs::symlink_metadata(path).map_err(|source| {
                LocalCompilerHostError::BundleManifest {
                    path: path.clone().into_boxed_path(),
                    message: source.to_string().into_boxed_str(),
                }
            })?;
            if &observed != expected
                || !same_file_identity(identity, &metadata)
                || fs::canonicalize(path).ok().as_deref() != Some(path.as_path()) {
            return Err(LocalCompilerHostError::BundleManifest {
                path: path.clone().into_boxed_path(),
                message: "selected SDK receipt changed after admission".into(),
            });
        }
    }
    Ok(())
    }

    pub(crate) fn fingerprint(&self) -> [u8; 32] {
        let mut hash = Sha256::new();
        hash.update(b"nudox.typescript.selected-resource-receipts.v1\0");
        for (path, _, digest, _) in &self.files {
            let bytes = path.as_os_str().as_encoded_bytes();
            hash.update((bytes.len() as u64).to_be_bytes());
            hash.update(bytes);
            hash.update(digest.as_bytes());
        }
        hash.finalize().into()
    }
}

pub(crate) fn bundled_typescript_runtime(
    application: &Path,
) -> Result<
    Option<(
        PathBuf,
        TypeScriptSelectionOrigin,
        BundleTypeScriptResourceProof,
    )>,
    LocalCompilerHostError,
> {
    let origin = if standalone_package_root(application).is_some() {
        TypeScriptSelectionOrigin::ValidatedStandalonePackage
    } else {
        TypeScriptSelectionOrigin::ValidatedApplicationBundle
    };
    bundled_typescript_node(Some(application))
        .map(|runtime| runtime.map(|(node, proof)| (node, origin, proof)))
}

fn standalone_package_root(executable: &Path) -> Option<PathBuf> {
    let bin = executable.parent()?;
    let root = bin.parent()?;
    (bin.file_name() == Some(OsStr::new("bin"))
        && matches!(
            executable.file_name()?.to_str()?,
            "backend-cli" | "backend-mcp" | "backend-locald"
        )
        && root.join("packaging-manifest.json").is_file())
    .then(|| root.to_path_buf())
}

#[derive(Clone, Copy)]
enum LinuxLoaderDependency {
    System,
    Bundled,
}

fn linux_loader_dependency(name: &str) -> Option<LinuxLoaderDependency> {
    if name.is_empty() || name.contains('/') || name.contains('\\') || matches!(name, "." | "..") {
        return None;
    }
    Some(if include_str!("../../../../../tools/package/linux_system_sonames.txt")
        .lines().any(|system| system == name) {
        LinuxLoaderDependency::System
    } else {
        LinuxLoaderDependency::Bundled
    })
}

struct StandaloneTypeScriptResources {
    root: PathBuf,
    manifest_path: PathBuf,
    manifest: serde_json::Value,
    proof: BundleTypeScriptResourceProof,
}

fn standalone_resources(
    executable: &Path,
    root: &Path,
) -> Result<Option<StandaloneTypeScriptResources>, LocalCompilerHostError> {
    let manifest_path = root.join("packaging-manifest.json");
    let bytes = read_bounded(&manifest_path, MAX_BUNDLE_MANIFEST_BYTES).map_err(|source| {
        LocalCompilerHostError::BundleManifest {
            path: manifest_path.clone().into_boxed_path(),
            message: source.to_string().into_boxed_str(),
        }
    })?;
    let mut proof = BundleTypeScriptResourceProof::from_bytes(
        &manifest_path,
        MAX_BUNDLE_MANIFEST_BYTES,
        &bytes,
    )?;
    let manifest: serde_json::Value = serde_json::from_slice(&bytes).map_err(|source| {
        LocalCompilerHostError::BundleManifest {
            path: manifest_path.clone().into_boxed_path(),
            message: source.to_string().into_boxed_str(),
        }
    })?;
    let refuse = |message: &str| LocalCompilerHostError::BundleManifest {
        path: manifest_path.clone().into_boxed_path(),
        message: message.into(),
    };
    if manifest.get("schema").and_then(serde_json::Value::as_str)
        != Some("nudox.linux-portable-package.v1")
    {
        return Err(refuse("unsupported standalone application package"));
    }
    let Some(sdk) = manifest.get("typescript_sdk") else {
        return Ok(None);
    };
    if sdk.get("bundled").and_then(serde_json::Value::as_bool) == Some(false) {
        return Ok(None);
    }
    if sdk.get("schema").and_then(serde_json::Value::as_str) != Some("nudox.typescript-sdk.v1")
        || sdk.get("bundled").and_then(serde_json::Value::as_bool) != Some(true)
        || sdk.get("root").and_then(serde_json::Value::as_str) != Some("share/nudox/typescript")
    {
        return Err(refuse(
            "standalone SDK has an unsupported resource contract",
        ));
    }
    let executable_name = executable
        .file_name()
        .and_then(OsStr::to_str)
        .ok_or_else(|| refuse("unsupported application executable"))?;
    let entries = manifest
        .get("executables")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| refuse("package omits application executable inventory"))?;
    let records = entries
        .iter()
        .filter(|record| {
            record.get("name").and_then(serde_json::Value::as_str) == Some(executable_name)
        })
        .collect::<Vec<_>>();
    if records.len() != 1 {
        return Err(refuse("application executable inventory is ambiguous"));
    }
    let record = records[0];
    if record
        .get("packaged_path")
        .and_then(serde_json::Value::as_str)
        != Some(format!("bin/{executable_name}").as_str())
        || fs::canonicalize(executable).ok().as_deref() != Some(executable)
    {
        return Err(refuse(
            "application executable is outside its canonical package path",
        ));
    }
    validate_bundle_inventory_file(
        &serde_json::json!({"kind":"file", "size_bytes":record.get("packaged_bytes"), "sha256":record.get("packaged_sha256")}),
        executable,
        MAX_BUNDLE_EXECUTABLE_BYTES,
        &manifest_path,
    )?;
    let build_path = root.join("build-manifest.json");
    let build_bytes = read_bounded(&build_path, MAX_BUNDLE_MANIFEST_BYTES).map_err(|source| {
        LocalCompilerHostError::BundleManifest {
            path: build_path.clone().into_boxed_path(),
            message: source.to_string().into_boxed_str(),
        }
    })?;
    let build_proof = BundleTypeScriptResourceProof::from_bytes(
        &build_path,
        MAX_BUNDLE_MANIFEST_BYTES,
        &build_bytes,
    )?;
    if manifest
        .get("build_manifest_sha256")
        .and_then(serde_json::Value::as_str)
        != Some(build_proof.files[0].2.as_str())
    {
        return Err(refuse(
            "standalone SDK package does not bind its public build manifest",
        ));
    }
    let build: serde_json::Value = serde_json::from_slice(&build_bytes).map_err(|source| {
        LocalCompilerHostError::BundleManifest {
            path: build_path.clone().into_boxed_path(),
            message: source.to_string().into_boxed_str(),
        }
    })?;
    if build.get("source") != manifest.get("source") {
        return Err(refuse(
            "standalone package source differs from its build receipt",
        ));
    }
    proof.files.extend(build_proof.files);
    proof.validate_current()?;
    Ok(Some(StandaloneTypeScriptResources {
        root: root.to_path_buf(),
        manifest_path,
        manifest,
        proof,
    }))
}

fn standalone_typescript_runtime(
    executable: &Path,
    root: &Path,
) -> Result<Option<(PathBuf, BundleTypeScriptResourceProof)>, LocalCompilerHostError> {
    let Some(mut resources) = standalone_resources(executable, root)? else {
        return Ok(None);
    };
    let node = resources.admit_node()?;
    resources.proof.validate_current()?;
    Ok(Some((node, resources.proof)))
}

impl StandaloneTypeScriptResources {
    fn admit_node(&mut self) -> Result<PathBuf, LocalCompilerHostError> {
        let refuse = |message: &str| LocalCompilerHostError::BundleManifest {
            path: self.manifest_path.clone().into_boxed_path(),
            message: message.into(),
        };
        let sdk = &self.manifest["typescript_sdk"];
        self.proof.expected_node_version = Some(
            sdk["node_version"]
                .as_str()
                .ok_or_else(|| refuse("standalone Node has no recorded version identity"))?
                .into(),
        );

        let relative = "share/nudox/typescript/node/bin/node";
        let node = self.root.join(relative);
        let node_record = &sdk["files"][relative];
        if sdk["node"]["packaged_path"].as_str() != Some(relative)
            || sdk["node"]["packaged_sha256"] != node_record["sha256"]
        {
            return Err(refuse("standalone Node differs from its SDK identity"));
        }
        retain_selected_inventory_file(
            &mut self.proof, node_record,
            &node,
            MAX_BUNDLED_NODE_BYTES,
            &self.manifest_path,
        )?;
        if canonicalize_executable_existing(LocalHostPathRole::TypeScriptNode, &node)? != node {
            return Err(refuse("standalone SDK Node resolves through a symlink"));
        }
        // Bind only the non-system loader closure reachable from this selected Node.
        let libraries = self
            .manifest
            .get("libraries")
            .and_then(serde_json::Value::as_object)
            .ok_or_else(|| refuse("standalone package omits its loader closure"))?;
        let mut pending = sdk["node"]["packaged_elf"]["needed"]
            .as_array()
            .ok_or_else(|| refuse("standalone Node omits its ELF dependency identity"))?
            .clone();
        let mut seen = std::collections::BTreeSet::new();
        let mut dependency_bytes = 0u64;
        while let Some(dependency) = pending.pop() {
            let name = dependency
                .as_str()
                .ok_or_else(|| refuse("invalid Node dependency identity"))?;
            match linux_loader_dependency(name) {
                Some(LinuxLoaderDependency::System) => continue,
                Some(LinuxLoaderDependency::Bundled) => {},
                None => return Err(refuse("Node dependency contains an unsafe loader path")),
            }
            if !seen.insert(name.to_owned()) {
                continue;
            }
            if seen.len() > 64 {
                return Err(refuse("Node dependency closure exceeds its finite bound"));
            }
            let record = libraries
                .get(name)
                .ok_or_else(|| refuse("Node requires an unreceipted non-system library"))?;
            let relative = format!("lib/{name}");
            if record["packaged_path"].as_str() != Some(relative.as_str()) {
                return Err(refuse("Node dependency path escaped the package"));
            }
            let member_bytes = record["packaged_bytes"].as_u64()
                .ok_or_else(|| refuse("Node dependency has no bounded byte identity"))?;
            dependency_bytes = dependency_bytes.checked_add(member_bytes)
                .filter(|bytes| *bytes <= 512 * 1024 * 1024)
                .ok_or_else(|| refuse("Node dependency closure exceeds its aggregate byte bound"))?;
            let path = self.root.join(relative);
            validate_bundle_inventory_file(
                &serde_json::json!({"kind":"file", "size_bytes":record.get("packaged_bytes"), "sha256":record.get("packaged_sha256")}),
                &path,
                96 * 1024 * 1024,
                &self.manifest_path,
            )?;
            let metadata = fs::symlink_metadata(&path).map_err(|source| {
                LocalCompilerHostError::BundleManifest {
                    path: path.clone().into_boxed_path(),
                    message: source.to_string().into_boxed_str(),
                }
            })?;
            self.proof.files.push((
                path,
                96 * 1024 * 1024,
                record["packaged_sha256"]
                    .as_str()
                    .ok_or_else(|| refuse("invalid Node dependency hash"))?
                    .to_owned(),
                metadata,
            ));
            pending.extend(
                record["needed"]
                    .as_array()
                    .ok_or_else(|| refuse("Node dependency omits its transitive identity"))?
                    .iter()
                    .cloned(),
            );
        }
        Ok(node)
    }
}

fn standalone_typescript_sdk(
    executable: &Path,
    root: &Path,
    runtime_required: bool,
) -> Result<Option<BundledTypeScriptSdk>, LocalCompilerHostError> {
    let Some(mut resources) = standalone_resources(executable, root)? else {
        return Ok(None);
    };
    let sdk = &resources.manifest["typescript_sdk"];
    let files = sdk["files"]
        .as_object()
        .ok_or_else(|| LocalCompilerHostError::BundleManifest {
            path: resources.manifest_path.clone().into_boxed_path(),
            message: "standalone SDK omits its file inventory".into(),
        })?;
    let package = resources
        .root
        .join("share/nudox/typescript/node_modules/typescript");
    if fs::canonicalize(&package).ok().as_deref() != Some(package.as_path()) {
        return Err(LocalCompilerHostError::BundleManifest {
            path: package.into_boxed_path(),
            message: "standalone compiler package resolves through a symlink".into(),
        });
    }
    let prefix = "share/nudox/typescript/node_modules/typescript/";
    let mut expected = std::collections::BTreeSet::new();
    let mut total = 0_u64;
    for (relative, record) in files {
        let Some(member) = relative.strip_prefix(prefix) else {
            continue;
        };
        if member.is_empty()
            || member.contains('\\')
            || Path::new(member)
                .components()
                .any(|part| !matches!(part, std::path::Component::Normal(_)))
            || !expected.insert(member.to_owned())
            || expected.len() > MAX_BUNDLE_TYPESCRIPT_PACKAGE_FILES
        {
            return Err(LocalCompilerHostError::BundleManifest {
                path: resources.manifest_path.clone().into_boxed_path(),
                message: "standalone compiler receipt contains an unsafe path or exceeds its bound"
                    .into(),
            });
        }
        total = total.saturating_add(record["size_bytes"].as_u64().unwrap_or(u64::MAX));
        if total > MAX_BUNDLE_TYPESCRIPT_PACKAGE_BYTES {
            return Err(LocalCompilerHostError::BundleManifest {
                path: resources.manifest_path.clone().into_boxed_path(),
                message: "standalone compiler exceeds its receipt byte bound".into(),
            });
        }
        retain_selected_inventory_file(
            &mut resources.proof, record,
            &resources.root.join(relative),
            MAX_BUNDLE_TYPESCRIPT_PACKAGE_BYTES,
            &resources.manifest_path,
        )?;
    }
    if !["package.json", "bin/tsc", "lib/typescript.js"]
        .iter()
        .all(|file| expected.contains(*file))
    {
        return Err(LocalCompilerHostError::BundleManifest {
            path: resources.manifest_path.clone().into_boxed_path(),
            message: "standalone SDK omits the compiler script or Compiler API".into(),
        });
    }
    let directories = expected
        .iter()
        .flat_map(|relative| {
            Path::new(relative)
                .ancestors()
                .skip(1)
                .filter(|parent| !parent.as_os_str().is_empty())
                .map(|parent| parent.to_string_lossy().into_owned())
                .collect::<Vec<_>>()
        })
        .collect::<std::collections::BTreeSet<_>>();
    let mut actual = std::collections::BTreeSet::new();
    collect_bundle_typescript_files(&package, &package, &directories, &mut actual)?;
    if actual != expected {
        return Err(LocalCompilerHostError::BundleManifest {
            path: package.into_boxed_path(),
            message: "standalone compiler tree differs from its complete receipt".into(),
        });
    }
    resources.proof.package_closures.push((package.clone(), expected));
    let package_json: serde_json::Value = serde_json::from_slice(
        &read_bounded(&package.join("package.json"), 64 * 1024).map_err(|source| {
            LocalCompilerHostError::BundleManifest {
                path: package.join("package.json").into_boxed_path(),
                message: source.to_string().into_boxed_str(),
            }
        })?,
    )
    .map_err(|source| LocalCompilerHostError::BundleManifest {
        path: package.join("package.json").into_boxed_path(),
        message: source.to_string().into_boxed_str(),
    })?;
    if package_json["name"].as_str() != Some("typescript")
        || package_json["version"] != sdk["typescript_version"]
    {
        return Err(LocalCompilerHostError::BundleManifest {
            path: package.join("package.json").into_boxed_path(),
            message: "standalone Compiler API version differs from its SDK receipt".into(),
        });
    }
    let node = if runtime_required {
        Some(resources.admit_node()?)
    } else {
        None
    };
    resources.proof.validate_current()?;
    Ok(Some(BundledTypeScriptSdk {
        compiler: package.join("bin/tsc"),
        module_root: package
            .parent()
            .expect("compiler package parent")
            .to_path_buf(),
        node,
        proof: resources.proof,
        origin: TypeScriptSelectionOrigin::ValidatedStandalonePackage,
    }))
}

fn collect_bundle_typescript_files(
    directory: &Path,
    package_root: &Path,
    expected_directories: &std::collections::BTreeSet<String>,
    files: &mut std::collections::BTreeSet<String>,
) -> Result<(), LocalCompilerHostError> {
    let entries =
        fs::read_dir(directory).map_err(|source| LocalCompilerHostError::BundleManifest {
            path: directory.to_path_buf().into_boxed_path(),
            message: source.to_string().into_boxed_str(),
        })?;
    for entry in entries {
        let entry = entry.map_err(|source| LocalCompilerHostError::BundleManifest {
            path: directory.to_path_buf().into_boxed_path(),
            message: source.to_string().into_boxed_str(),
        })?;
        let path = entry.path();
        let metadata = fs::symlink_metadata(&path).map_err(|source| {
            LocalCompilerHostError::BundleManifest {
                path: path.clone().into_boxed_path(),
                message: source.to_string().into_boxed_str(),
            }
        })?;
        if metadata.file_type().is_symlink() {
            return Err(LocalCompilerHostError::BundleManifest {
                path: path.into_boxed_path(),
                message: "TypeScript package tree contains a symlink".into(),
            });
        }
        if metadata.is_dir() {
            let relative = path
                .strip_prefix(package_root)
                .ok()
                .and_then(Path::to_str)
                .ok_or_else(|| LocalCompilerHostError::BundleManifest {
                    path: path.clone().into_boxed_path(),
                    message: "nonportable SDK directory".into(),
                })?;
            if !expected_directories.contains(relative) {
                return Err(LocalCompilerHostError::BundleManifest {
                    path: path.into_boxed_path(),
                    message: "SDK directory is absent from its finite receipt".into(),
                });
            }
            collect_bundle_typescript_files(&path, package_root, expected_directories, files)?;
        } else if metadata.is_file() {
            let relative = path
                .strip_prefix(package_root)
                .ok()
                .and_then(Path::to_str)
                .map(|path| path.replace('\\', "/"))
                .ok_or_else(|| LocalCompilerHostError::BundleManifest {
                    path: path.clone().into_boxed_path(),
                    message: "TypeScript package path is not a portable relative path".into(),
                })?;
            if files.len() >= MAX_BUNDLE_TYPESCRIPT_PACKAGE_FILES || !files.insert(relative) {
                return Err(LocalCompilerHostError::BundleManifest {
                    path: path.into_boxed_path(),
                    message: "TypeScript package tree exceeds its file-count bound".into(),
                });
            }
        } else {
            return Err(LocalCompilerHostError::BundleManifest {
                path: path.into_boxed_path(),
                message: "TypeScript package tree contains a special file".into(),
            });
        }
    }
    Ok(())
}

fn retain_selected_inventory_file(
    proof: &mut BundleTypeScriptResourceProof,
    record: &serde_json::Value,
    path: &Path,
    maximum_bytes: u64,
    manifest: &Path,
) -> Result<(), LocalCompilerHostError> {
    validate_bundle_inventory_file(record, path, maximum_bytes, manifest)?;
    let expected = record["sha256"].as_str().expect("validated inventory digest").to_owned();
    let identity = fs::symlink_metadata(path).map_err(|source| LocalCompilerHostError::BundleManifest {
        path: path.to_path_buf().into_boxed_path(), message: source.to_string().into_boxed_str(),
    })?;
    proof.files.push((path.to_path_buf(), maximum_bytes, expected, identity));
    Ok(())
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

#[cfg(unix)]
fn open_regular_resource(
    path: &Path,
    maximum_bytes: u64,
    before: &fs::Metadata,
) -> io::Result<fs::File> {
    use rustix::fs::{Mode, OFlags};
    // NONBLOCK prevents a regular-file-to-FIFO race from waiting before fstat;
    // NOFOLLOW closes the corresponding final-component symlink race.
    let descriptor = rustix::fs::open(
        path, OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC, Mode::empty(),
    )?;
    let file = fs::File::from(descriptor);
    let opened = file.metadata()?;
    if !opened.is_file() || opened.len() > maximum_bytes || !same_file_identity(before, &opened) {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "resource is not the admitted bounded regular object"));
    }
    Ok(file)
}

#[cfg(not(unix))]
fn open_regular_resource(
    _path: &Path,
    _maximum_bytes: u64,
    _before: &fs::Metadata,
) -> io::Result<fs::File> {
    Err(io::Error::new(io::ErrorKind::Unsupported, "bundled SDK resources require platform no-follow regular-file admission"))
}

fn read_bounded(path: &Path, maximum_bytes: u64) -> io::Result<Vec<u8>> {
    let before = fs::symlink_metadata(path)?;
    if !before.is_file() || before.file_type().is_symlink() || before.len() > maximum_bytes {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "resource is not a bounded regular file"));
    }
    let mut file = open_regular_resource(path, maximum_bytes, &before)?;
    let mut bytes = Vec::new();
    (&mut file).take(maximum_bytes.saturating_add(1)).read_to_end(&mut bytes)?;
    let after = file.metadata()?;
    let current = fs::symlink_metadata(path)?;
    if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > maximum_bytes
        || !same_file_identity(&before, &after) || !same_file_identity(&before, &current)
        || current.file_type().is_symlink() || current.len() != after.len()
        || after.len() != u64::try_from(bytes.len()).unwrap_or(u64::MAX)
        || before.modified().ok() != after.modified().ok()
    {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "resource changed during bounded read"));
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
    let identity_error = |source: io::Error| LocalCompilerHostError::BundleManifest {
        path: path.to_path_buf().into_boxed_path(),
        message: source.to_string().into_boxed_str(),
    };
    let path_identity = backend_platform::FileIdentity::of_path_nofollow(path)
        .map_err(&identity_error)?;
    let mut file =
        open_regular_resource(path, maximum_bytes, &path_metadata).map_err(|source| LocalCompilerHostError::BundleManifest {
            path: path.to_path_buf().into_boxed_path(),
            message: source.to_string().into_boxed_str(),
        })?;
    let opened_metadata =
        file.metadata()
            .map_err(|source| LocalCompilerHostError::BundleManifest {
                path: path.to_path_buf().into_boxed_path(),
                message: source.to_string().into_boxed_str(),
            })?;
    let opened_identity = backend_platform::FileIdentity::of_file(&file)
        .map_err(&identity_error)?;
    if !opened_metadata.is_file()
        || opened_metadata.len() > maximum_bytes
        || path_identity != opened_identity
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
        || opened_identity != backend_platform::FileIdentity::of_file(&file).map_err(&identity_error)?
        || opened_identity != backend_platform::FileIdentity::of_path_nofollow(path).map_err(&identity_error)?
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
        fs::canonicalize(path).expect("canonical private test directory")
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
            bundled_typescript_node(Some(&executable)).expect("validate bundle Node")
                .map(|(node, _)| node),
            Some(fs::canonicalize(&runtime).expect("canonical bundled Node")),
        );
        assert!(bundled_typescript_node(Some(&manifest))
            .expect("unrecognized executable path is ignored")
            .is_none());

        fs::write(&runtime, b"modified Node").expect("change bundled Node bytes");
        assert!(matches!(
            bundled_typescript_node(Some(&executable)),
            Err(LocalCompilerHostError::BundleManifest { .. })
        ));
        fs::remove_dir_all(root).expect("remove test directory");
    }

    #[test]
    fn bundled_typescript_sdk_is_a_complete_receipt_bound_relative_resource_set() {
        let root = private_test_directory("typescript-bundle-sdk");
        let bundle = root.join("Nudox.app");
        let executable = bundle.join("Contents/MacOS/backend-mcp");
        let helper_root = bundle.join("Contents/Resources/Helpers/typescript");
        let node = helper_root.join("node/bin/node");
        let compiler = helper_root.join("node_modules/typescript/bin/tsc");
        let package_json = helper_root.join("node_modules/typescript/package.json");
        let typescript_api = helper_root.join("node_modules/typescript/lib/typescript.js");
        let compiler_wrapper = helper_root.join("tsc");
        let report_program = helper_root.join("report-program");
        let driver = helper_root.join("checker-main.cjs");
        for path in [
            &executable,
            &node,
            &compiler,
            &package_json,
            &typescript_api,
            &compiler_wrapper,
            &report_program,
            &driver,
        ] {
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent).expect("create SDK fixture directory");
            }
        }
        fs::write(&executable, b"fixture executable").expect("write executable");
        fs::write(&node, b"fixture node").expect("write bundled Node");
        fs::write(&compiler, b"#!/usr/bin/env node\n").expect("write package tsc entrypoint");
        fs::write(&package_json, br#"{"version":"5.7.2"}"#).expect("write package metadata");
        fs::write(&typescript_api, b"module.exports = { version: '5.7.2' };\n")
            .expect("write compiler API");
        fs::write(&compiler_wrapper, b"#!/bin/sh\nexit 0\n").expect("write tsc wrapper");
        fs::write(&report_program, b"#!/bin/sh\nexit 0\n").expect("write report wrapper");
        fs::write(&driver, b"'use strict';\n").expect("write report driver");
        for path in [&node, &report_program] {
            fs::set_permissions(path, fs::Permissions::from_mode(0o700))
                .expect("make SDK runtime executable");
        }

        let helper_relative_files = [
            "typescript/node/bin/node",
            "typescript/node_modules/typescript/bin/tsc",
            "typescript/node_modules/typescript/package.json",
            "typescript/node_modules/typescript/lib/typescript.js",
        ];
        let helper_file_hashes = helper_relative_files
            .iter()
            .map(|relative| {
                let path = bundle.join("Contents/Resources/Helpers").join(relative);
                let (_, digest) = sha256_file(&path, MAX_BUNDLE_EXECUTABLE_BYTES)
                    .expect("hash receipted helper file");
                ((*relative).to_owned(), serde_json::Value::String(digest))
            })
            .collect::<serde_json::Map<_, _>>();
        let node_digest = helper_file_hashes
            .get("typescript/node/bin/node")
            .and_then(serde_json::Value::as_str)
            .expect("Node digest");
        let driver_digest = sha256_file(&driver, MAX_BUNDLE_EXECUTABLE_BYTES)
            .expect("hash report driver")
            .1;
        let tools = serde_json::json!({
            "node": {"version":"v22.0.0", "sha256":node_digest},
            "typescript": {"version":"5.7.2"}
        });
        let source =
            serde_json::json!({"git_revision":"source-revision", "git_tree":"source-tree"});
        let helper_receipt = serde_json::json!({
            "schema":1,
            "source":source,
            "target":"aarch64-apple-darwin",
            "files":helper_file_hashes,
            "tools":tools,
            "typescript_driver_sha256":driver_digest,
        });
        let helper_receipt_bytes =
            serde_json::to_vec(&helper_receipt).expect("serialize helper receipt");
        let helper_receipt_path =
            bundle.join("Contents/Resources/Build Evidence/compiler-helpers-receipt.json");
        fs::create_dir_all(helper_receipt_path.parent().expect("receipt parent"))
            .expect("create receipt directory");
        fs::write(&helper_receipt_path, &helper_receipt_bytes).expect("write helper receipt");
        let receipt_digest = sha256_file(&helper_receipt_path, MAX_BUNDLE_HELPER_RECEIPT_BYTES)
            .expect("hash helper receipt")
            .1;

        let inventory_entry = |path: &Path| {
            let (size_bytes, sha256) =
                sha256_file(path, MAX_BUNDLE_EXECUTABLE_BYTES).expect("hash bundle fixture");
            serde_json::json!({
                "kind":"file",
                "size_bytes":size_bytes,
                "sha256":sha256,
            })
        };
        let mut inventory = serde_json::Map::new();
        let bundle_paths = [
            ("Contents/MacOS/backend-mcp", &executable),
            ("Contents/Resources/Helpers/typescript/node/bin/node", &node),
            (
                "Contents/Resources/Helpers/typescript/node_modules/typescript/bin/tsc",
                &compiler,
            ),
            (
                "Contents/Resources/Helpers/typescript/node_modules/typescript/package.json",
                &package_json,
            ),
            (
                "Contents/Resources/Helpers/typescript/node_modules/typescript/lib/typescript.js",
                &typescript_api,
            ),
            (
                "Contents/Resources/Helpers/typescript/tsc",
                &compiler_wrapper,
            ),
            (
                "Contents/Resources/Helpers/typescript/report-program",
                &report_program,
            ),
            (
                "Contents/Resources/Helpers/typescript/checker-main.cjs",
                &driver,
            ),
        ];
        for (relative, path) in bundle_paths {
            inventory.insert(relative.to_owned(), inventory_entry(path));
        }
        inventory.insert(
            "Contents/Resources/Build Evidence/compiler-helpers-receipt.json".to_owned(),
            inventory_entry(&helper_receipt_path),
        );
        let helpers = serde_json::json!({
            "receipt_sha256":receipt_digest,
            "files":helper_receipt["files"],
            "tools":tools,
            "typescript_driver_sha256":driver_digest,
        });
        let manifest = serde_json::json!({
            "schema":1,
            "product":"Nudox",
            "source":source,
            "target":{"triple":"aarch64-apple-darwin"},
            "bundle":{"identifier":"dev.nudox.desktop"},
            "compiler_helpers":helpers,
            "files":inventory,
        });
        let manifest_path = bundle.join("Contents/Resources/build-manifest.json");
        let manifest_bytes = serde_json::to_vec(&manifest).expect("serialize bundle manifest");
        fs::write(&manifest_path, &manifest_bytes).expect("write bundle manifest");

        let admitted = bundled_typescript_sdk(Some(&executable), true)
            .expect("validate bundled TypeScript SDK")
            .expect("complete SDK is available");
        assert_eq!(
            admitted.compiler,
            fs::canonicalize(&compiler).expect("canonical compiler")
        );
        assert_eq!(
            admitted.node,
            Some(fs::canonicalize(&node).expect("canonical Node"))
        );
        assert_eq!(
            admitted.module_root,
            fs::canonicalize(helper_root.join("node_modules")).expect("canonical module root")
        );
        admitted.proof
            .validate_current().expect("selected SDK receipt witness");
        let original_api_bytes = fs::read(&typescript_api).unwrap();
        fs::write(&typescript_api, b"same version, changed selected Compiler API").unwrap();
        assert!(admitted.proof.validate_current().is_err());
        fs::write(&typescript_api, &original_api_bytes).unwrap();
        fs::write(&node, b"same version, changed selected Node").unwrap();
        assert!(admitted.proof.validate_current().is_err());
        fs::write(&node, b"fixture node").unwrap();
        let added_api = helper_root.join("node_modules/typescript/lib/added-after-admission.js");
        fs::write(&added_api, b"unreceipted member").unwrap();
        assert!(admitted.proof.validate_current().is_err());
        fs::remove_file(&added_api).unwrap();

        // Unselected wrapper and driver assets cannot change this default checker recipe.
        fs::write(&report_program, b"broken unused wrapper").expect("alter unused report wrapper");
        fs::write(&driver, b"broken unused driver").expect("alter unused bundle driver");
        bundled_typescript_sdk(Some(&executable), true)
            .expect("embedded report driver selected instead");
        // A selected external Node must not cause unrelated bundled Node bytes to be read.
        fs::write(&node, b"broken unused bundled Node").expect("alter unselected Node");
        let external_runtime_sdk = bundled_typescript_sdk(Some(&executable), false)
            .expect("SDK package remains usable with selected external Node")
            .unwrap();
        assert!(external_runtime_sdk.node.is_none());
        assert!(bundled_typescript_sdk(Some(&executable), true).is_err());
        fs::write(&node, b"different fixture node").expect("change Node with matching outer inventory");
        let mut incoherent_node_manifest = manifest.clone();
        incoherent_node_manifest["files"]["Contents/Resources/Helpers/typescript/node/bin/node"] = inventory_entry(&node);
        fs::write(&manifest_path, serde_json::to_vec(&incoherent_node_manifest).unwrap()).unwrap();
        assert!(bundled_typescript_sdk(Some(&executable), true).is_err());
        assert!(bundled_typescript_node(Some(&executable)).is_err());
        // An external selected runtime leaves this unused bundled Node outside authority.
        assert!(bundled_typescript_sdk(Some(&executable), false).is_ok());
        fs::write(&manifest_path, &manifest_bytes).expect("restore coherent Node inventory");
        fs::write(&node, b"fixture node").expect("restore bundle Node");
        let extra = helper_root.join("node_modules/typescript/unreceipted-empty-directory");
        fs::create_dir(&extra).expect("create extra empty directory");
        assert!(bundled_typescript_sdk(Some(&executable), true).is_err());
        fs::remove_dir(&extra).expect("remove extra directory");

        let mut incompatible_manifest: serde_json::Value =
            serde_json::from_slice(&manifest_bytes).expect("read fixture manifest");
        incompatible_manifest["target"]["triple"] =
            serde_json::Value::String("x86_64-apple-darwin".to_owned());
        fs::write(
            &manifest_path,
            serde_json::to_vec(&incompatible_manifest).expect("serialize mismatched target"),
        )
        .expect("write mismatched target manifest");
        assert!(matches!(
            bundled_typescript_sdk(Some(&executable), true),
            Err(LocalCompilerHostError::BundleManifest { .. })
        ));
        fs::write(&manifest_path, &manifest_bytes).expect("restore bundle manifest");

        fs::write(&typescript_api, b"module.exports = {};\n").expect("tamper with SDK tree");
        let selected_runtime = bundled_typescript_node(Some(&executable))
            .expect("unused project SDK package does not block selected bundle runtime").unwrap();
        assert_eq!(selected_runtime.1.expected_node_version.as_deref(), Some("v22.0.0"));
        fs::write(&helper_receipt_path, b"changed selected receipt").unwrap();
        assert!(selected_runtime.1.validate_current().is_err());
        fs::write(&helper_receipt_path, &helper_receipt_bytes).unwrap();
        assert!(matches!(
            bundled_typescript_sdk(Some(&executable), true),
            Err(LocalCompilerHostError::BundleManifest { .. })
        ));
        fs::remove_dir_all(root).expect("remove test directory");
    }

    #[test]
    fn standalone_selected_package_proof_refuses_added_members_after_admission() {
        let root = private_test_directory("standalone-package-closure");
        let package = root.join("share/nudox/typescript/node_modules/typescript");
        fs::create_dir_all(package.join("lib")).unwrap();
        let api = package.join("lib/typescript.js");
        fs::write(&api, b"receipted API bytes").unwrap();
        let manifest = root.join("packaging-manifest.json");
        fs::write(&manifest, b"{}").unwrap();
        let mut proof = BundleTypeScriptResourceProof::from_bytes(&manifest, 1024, b"{}").unwrap();
        let (size, digest) = sha256_file(&api, 1024).unwrap();
        retain_selected_inventory_file(&mut proof,
            &serde_json::json!({"kind":"file", "size_bytes":size, "sha256":digest}),
            &api, 1024, &manifest).unwrap();
        proof.package_closures.push((package.clone(), ["lib/typescript.js".to_owned()].into_iter().collect()));
        proof.validate_current().unwrap();
        let added = package.join("lib/added-after-admission.js");
        fs::write(&added, b"unreceipted standalone package member").unwrap();
        assert!(proof.validate_current().is_err());
        fs::remove_file(&added).unwrap();
        proof.validate_current().unwrap();
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn standalone_node_receipt_uses_shared_system_loader_classification() {
        // Admission controls only; these fixture bytes are never executed as an SDK.
        let root = private_test_directory("standalone-system-loader");
        let node = root.join("share/nudox/typescript/node/bin/node");
        executable(&node);
        let (bytes, digest) = sha256_file(&node, MAX_BUNDLED_NODE_BYTES).unwrap();
        let manifest_path = root.join("packaging-manifest.json");
        let mut manifest = serde_json::json!({
            "typescript_sdk": {
                "node_version": "v24.18.0",
                "files": {"share/nudox/typescript/node/bin/node": {
                    "kind":"file", "size_bytes":bytes, "sha256":digest}},
                "node": {"packaged_path":"share/nudox/typescript/node/bin/node",
                         "packaged_sha256":digest, "packaged_elf":{"needed":["libanl.so.1"]}}
            },
            "libraries": {}
        });
        let metadata = serde_json::to_vec(&manifest).unwrap();
        fs::write(&manifest_path, &metadata).unwrap();
        let mut resources = StandaloneTypeScriptResources {
            root: root.clone(), manifest_path: manifest_path.clone(), manifest: manifest.clone(),
            proof: BundleTypeScriptResourceProof::from_bytes(&manifest_path, MAX_BUNDLE_MANIFEST_BYTES, &metadata).unwrap(),
        };
        assert_eq!(resources.admit_node().unwrap(), node);
        fs::write(&node, b"same version, changed selected standalone Node").unwrap();
        assert!(resources.proof.validate_current().is_err());
        executable(&node);
        manifest["typescript_sdk"]["node"]["packaged_elf"]["needed"] = serde_json::json!(["libgcc_s.so.1"]);
        resources.manifest = manifest;
        assert!(matches!(resources.admit_node(), Err(LocalCompilerHostError::BundleManifest { .. })));
        // An oversized recorded closure is refused before opening any library, even
        // when a manifest supplies a digest for a missing or sparse payload.
        resources.manifest["libraries"]["libgcc_s.so.1"] = serde_json::json!({
            "packaged_path":"lib/libgcc_s.so.1", "packaged_bytes":512u64 * 1024 * 1024 + 1,
            "packaged_sha256":"0".repeat(64), "needed":[]
        });
        assert!(matches!(resources.admit_node(),
            Err(LocalCompilerHostError::BundleManifest { message, .. })
                if message.contains("aggregate byte bound")));
        assert!(!root.join("lib/libgcc_s.so.1").exists());
        fs::remove_dir_all(root).unwrap();
    }

    #[derive(Clone, Default)]
    struct InstalledToolsEnvironment {
        values: Vec<(LocalHostVariable, std::ffi::OsString)>,
        search_path: Option<std::ffi::OsString>,
        go_module_cache: Option<std::ffi::OsString>,
        go_path: Option<std::ffi::OsString>,
    }

    impl InstalledToolsEnvironment {
        fn set(&mut self, variable: LocalHostVariable, path: &Path) {
            self.values
                .push((variable, path.as_os_str().to_os_string()));
        }
    }

    impl LocalHostEnvironment for InstalledToolsEnvironment {
        fn value(&self, variable: LocalHostVariable) -> Option<std::ffi::OsString> {
            self.values
                .iter()
                .find(|(selected, _)| *selected == variable)
                .map(|(_, value)| value.clone())
        }

        fn search_path(&self) -> Option<std::ffi::OsString> {
            self.search_path.clone()
        }

        fn go_module_cache(&self) -> Option<std::ffi::OsString> {
            self.go_module_cache.clone()
        }

        fn go_path(&self) -> Option<std::ffi::OsString> {
            self.go_path.clone()
        }
    }

    fn installed_fixture(
        name: &str,
        include_pyrefly: bool,
    ) -> (PathBuf, InstalledToolsEnvironment) {
        let root = private_test_directory(name);
        let home = root.join("home");
        let bin = root.join("bin");
        let module_root = root.join("lib/node_modules");
        let compiler = module_root.join("typescript/bin/tsc");
        executable(&compiler);
        executable(&bin.join("node"));
        executable(&bin.join("python3"));
        executable(&bin.join("go"));
        if include_pyrefly {
            executable(&bin.join("pyrefly"));
        }
        let go_module_cache = root.join("go/pkg/mod");
        fs::create_dir_all(&home).expect("create home fixture");
        fs::create_dir_all(&go_module_cache).expect("create Go module cache fixture");
        let compiler_bin = compiler.parent().expect("compiler bin directory");
        let search_path =
            std::env::join_paths([compiler_bin, bin.as_path()]).expect("join fixture PATH");
        let mut environment = InstalledToolsEnvironment {
            search_path: Some(search_path),
            go_module_cache: Some(go_module_cache.as_os_str().to_os_string()),
            ..InstalledToolsEnvironment::default()
        };
        environment.set(LocalHostVariable::Home, &home);
        (root, environment)
    }

    #[test]
    fn installed_tool_capture_closes_one_paired_selection_and_repeats_its_witness() {
        let (root, environment) = installed_fixture("installed-tools", true);
        let host = LocalCompilerHost::new(environment.clone(), LocalHostDiscovery::InstalledTools);
        let first = host
            .capture_installed_selection()
            .expect("capture installed compilers");
        let restarted = LocalCompilerHost::new(environment, LocalHostDiscovery::InstalledTools)
            .capture_installed_selection()
            .expect("repeat installed compiler capture");

        let snapshot = first.snapshot();
        assert_eq!(
            first.source(),
            super::super::LocalCompilerHostSelectionSource::CapturedInstalledTools
        );
        assert_eq!(
            snapshot.path(LocalHostVariable::NudoxTypeScriptCompiler),
            None,
        );
        assert_eq!(
            snapshot.path(LocalHostVariable::NudoxTypeScriptDefaultCompiler),
            Some(
                fs::canonicalize(root.join("lib/node_modules/typescript/bin/tsc"))
                    .unwrap()
                    .as_path()
            ),
        );
        assert_eq!(
            snapshot.path(LocalHostVariable::NudoxTypeScriptDefaultNode),
            Some(fs::canonicalize(root.join("bin/node")).unwrap().as_path()),
        );
        assert_eq!(
            snapshot.path(LocalHostVariable::NudoxTypeScriptDefaultModuleRoot),
            Some(
                fs::canonicalize(root.join("lib/node_modules"))
                    .unwrap()
                    .as_path()
            ),
        );
        assert_eq!(
            snapshot.path(LocalHostVariable::NudoxPython),
            Some(
                fs::canonicalize(root.join("bin/python3"))
                    .unwrap()
                    .as_path()
            ),
        );
        assert_eq!(
            snapshot.path(LocalHostVariable::NudoxPyrefly),
            Some(
                fs::canonicalize(root.join("bin/pyrefly"))
                    .unwrap()
                    .as_path()
            ),
        );
        assert_eq!(
            snapshot.path(LocalHostVariable::NudoxGo),
            Some(fs::canonicalize(root.join("bin/go")).unwrap().as_path()),
        );
        assert_eq!(
            snapshot.path(LocalHostVariable::NudoxGoRoot),
            Some(fs::canonicalize(root.join("go/pkg/mod")).unwrap().as_path()),
        );
        assert!(first.issues().is_empty());
        assert_eq!(first.fingerprint(), restarted.fingerprint());
        assert_eq!(first.fingerprint_hex(), restarted.fingerprint_hex(),);
        let incoming = super::super::LocalCompilerHostSelection::from_closed_snapshot(
            first.snapshot().clone(),
        )
        .expect("wrap incoming closed snapshot");
        assert_ne!(first.fingerprint(), incoming.fingerprint());
        let receipt = first.encode_receipt().expect("encode status receipt");
        assert!(receipt.contains("captured_installed_tools"));
        assert!(receipt.contains(&first.fingerprint_hex()));
        fs::remove_dir_all(root).expect("remove fixture");
    }

    #[test]
    fn go_only_refusals_survive_frozen_capture_and_closed_snapshot_handoff() {
        use crate::application::{ClosedLocalHostEnvironmentSnapshot, LocalCompilerHostSelection,
            LocalRuntimeGoAuthorityFailure as Failure,
        };
        let (root, environment) = installed_fixture("closed-go-only-failures", false);
        for (variable, cause) in [(LocalHostVariable::NudoxGo, Failure::ExecutableUnavailable),
            (LocalHostVariable::NudoxGoRoot, Failure::GoRootUnavailable),
            (LocalHostVariable::NudoxGoOracle, Failure::OracleUnavailable),
        ] {
            for bad in [root.join("missing-object"), PathBuf::from("relative-object"), PathBuf::new(),
            ] {
                let mut configured = environment.clone();
                configured.set(variable, &bad);
                let captured = LocalCompilerHost::new(configured, LocalHostDiscovery::InstalledTools)
                    .capture_installed_selection().expect("Go-only failure cannot abort capture");
                assert_eq!(captured.go_authority_failure(), Some(cause));
                let closed = ClosedLocalHostEnvironmentSnapshot::parse(&captured.snapshot().encode().unwrap(),
                ).unwrap();
                let incoming = LocalCompilerHostSelection::from_closed_snapshot(closed).unwrap();
                assert_eq!(incoming.go_authority_failure(), Some(cause));
                assert_eq!(incoming.snapshot().path(variable), None);
                assert!(incoming.snapshot().path(LocalHostVariable::NudoxTypeScriptDefaultCompiler).is_some());
                assert!(incoming.snapshot().path(LocalHostVariable::NudoxTypeScriptDefaultNode).is_some());
                assert!(incoming.snapshot().path(LocalHostVariable::NudoxPython).is_some());
            }
        }
        // A previously captured cause must survive the temporary frozen host construction.
        let retained = LocalCompilerHost::new(environment, LocalHostDiscovery::InstalledTools)
            .with_go_authority_failure(Some(Failure::ToolchainIdentityUnavailable))
            .capture_installed_selection().unwrap();
        assert_eq!(retained.go_authority_failure(), Some(Failure::ToolchainIdentityUnavailable));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn invalid_explicit_go_root_is_retained_without_blocking_installed_selection() {
        let (root, mut environment) = installed_fixture("installed-tools-invalid-go-root", false);
        let invalid_root = root.join("go-root-file");
        fs::write(&invalid_root, b"not a directory").expect("create invalid Go root");
        environment.set(LocalHostVariable::NudoxGoRoot, &invalid_root);

        let selection = LocalCompilerHost::new(environment, LocalHostDiscovery::InstalledTools)
            .capture_installed_selection()
            .expect("optional Go root failure must not block other tool capture");

        assert!(
            selection
                .snapshot()
                .path(LocalHostVariable::NudoxGo)
                .is_some()
        );
        assert!(
            selection
                .snapshot()
                .path(LocalHostVariable::NudoxGoRoot)
                .is_none()
        );
        assert_eq!(
            selection.go_authority_failure(),
            Some(crate::application::LocalRuntimeGoAuthorityFailure::GoRootUnavailable),
        );
        assert!(selection.issues().contains(
            &super::super::LocalCompilerHostSelectionIssue::GoAuthorityUnavailable {
                cause: crate::application::LocalRuntimeGoAuthorityFailure::GoRootUnavailable,
            }
        ));
        let receipt = selection.encode_receipt().expect("encode typed failure");
        let receipt: serde_json::Value = serde_json::from_str(&receipt).expect("typed receipt JSON");
        let failure: crate::application::LocalRuntimeGoAuthorityFailure =
            serde_json::from_value(receipt["go_failure"].clone()).expect("typed Go cause");
        assert_eq!(failure, crate::application::LocalRuntimeGoAuthorityFailure::GoRootUnavailable);
        fs::remove_dir_all(root).expect("remove fixture");
    }

    #[test]
    fn invalid_explicit_go_executable_is_deferred_to_the_go_language_lane() {
        let (root, mut environment) = installed_fixture("installed-tools-invalid-go", false);
        let invalid_go = root.join("missing/go");
        environment.set(LocalHostVariable::NudoxGo, &invalid_go);

        let selection = LocalCompilerHost::new(environment, LocalHostDiscovery::InstalledTools)
            .capture_installed_selection()
            .expect("optional Go path failure must not block other tool capture");

        assert!(
            selection
                .snapshot()
                .path(LocalHostVariable::NudoxGo)
                .is_none()
        );
        assert_eq!(
            selection.go_authority_failure(),
            Some(crate::application::LocalRuntimeGoAuthorityFailure::ExecutableUnavailable),
        );
        assert!(selection.issues().contains(
            &super::super::LocalCompilerHostSelectionIssue::GoAuthorityUnavailable {
                cause: crate::application::LocalRuntimeGoAuthorityFailure::ExecutableUnavailable,
            }
        ));
        fs::remove_dir_all(root).expect("remove fixture");
    }

    #[test]
    fn closed_node_roles_round_trip_exact_paths_and_keep_truthful_origins() {
        use super::super::ClosedLocalHostEnvironmentSnapshot;
        struct Closed(ClosedLocalHostEnvironmentSnapshot);
        impl LocalHostEnvironment for Closed {
            fn value(&self, variable: LocalHostVariable) -> Option<std::ffi::OsString> {
                self.0
                    .path(variable).map(|path| path.as_os_str().to_owned())
            }
        }
        let root = private_test_directory("closed-node-origin");
        let node = root.join("bin/node"); executable(&node);
        for (role, origin) in [
            (LocalHostVariable::NudoxTypeScriptNode, TypeScriptSelectionOrigin::ExplicitConfiguration,
            ),
            (LocalHostVariable::NudoxTypeScriptDefaultNode, TypeScriptSelectionOrigin::InstalledHostSelection,
            ),
            (LocalHostVariable::NudoxTypeScriptBundledNode, TypeScriptSelectionOrigin::ValidatedApplicationBundle,
            ),
        ] {
            let snapshot = ClosedLocalHostEnvironmentSnapshot::from_paths([(role, node.clone())]).expect("one Node role");
            let encoded = snapshot.encode().expect("closed Node encoding");
            let decoded = ClosedLocalHostEnvironmentSnapshot::parse(&encoded).expect("closed Node decoding");
            assert_eq!(snapshot, decoded);
            assert_eq!(encoded, decoded.encode().unwrap(), "same authority inputs retain exact canonical bytes");
            let host = LocalCompilerHost::new(Closed(decoded), LocalHostDiscovery::ClosedSnapshot);
            let selected = host.typescript_node_executable(None, None).expect("closed Node selection").expect("Node");
            assert_eq!(selected.path, node);
            assert_eq!(selected.origin, origin);
            let repeated = host.typescript_node_executable(None, None).unwrap().unwrap();
            assert_eq!(selected.path, repeated.path); assert_eq!(selected.origin, repeated.origin);
        }
        assert!(matches!(ClosedLocalHostEnvironmentSnapshot::from_paths([
            (LocalHostVariable::NudoxTypeScriptNode, node.clone()),
            (LocalHostVariable::NudoxTypeScriptDefaultNode, node),
        ]), Err(super::super::ClosedLocalHostEnvironmentSnapshotError::ConflictingTypeScriptNodeRoles)));
        fs::remove_dir_all(root).expect("owned fixture cleanup");

    }

    #[test]
    fn installed_capture_preserves_bad_explicit_overrides_and_reports_missing_pyrefly() {
        let (root, mut environment) = installed_fixture("installed-tools-invalid", false);
        let missing_node = root.join("absent/node");
        environment.set(LocalHostVariable::NudoxTypeScriptNode, &missing_node);
        let error = LocalCompilerHost::new(environment.clone(), LocalHostDiscovery::InstalledTools)
            .capture_installed_selection()
            .expect_err("invalid explicit Node must not be replaced by PATH");
        assert!(matches!(
            error,
            LocalCompilerHostError::ConfiguredPath {
                variable: LocalHostVariable::NudoxTypeScriptNode,
                ..
            }
        ));

        environment
            .values
            .retain(|(variable, _)| *variable != LocalHostVariable::NudoxTypeScriptNode);
        let selection = LocalCompilerHost::new(environment, LocalHostDiscovery::InstalledTools)
            .capture_installed_selection()
            .expect("capture with optional Pyrefly absent");
        assert!(!selection.issues().contains(
            &super::super::LocalCompilerHostSelectionIssue::MissingPythonInterpreter
        ));
        assert!(!selection.issues().contains(
            &super::super::LocalCompilerHostSelectionIssue::MissingPyreflyChecker
        ));
        assert!(!selection.issues().contains(
            &super::super::LocalCompilerHostSelectionIssue::MissingGoModuleCache
        ));
        assert!(
            selection
                .snapshot()
                .path(LocalHostVariable::NudoxPyrefly)
                .is_none()
        );
        fs::remove_dir_all(root).expect("remove fixture");
    }

    #[test]
    fn closed_snapshot_never_uses_path_or_compiler_relative_fallbacks() {
        let (root, environment) = installed_fixture("closed-host-selection", true);
        let compiler = root.join("lib/node_modules/typescript/bin/tsc");
        let mut closed_environment = environment.clone();
        closed_environment.values.clear();
        closed_environment.set(LocalHostVariable::NudoxTypeScriptCompiler, &compiler);
        let host = LocalCompilerHost::new(closed_environment, LocalHostDiscovery::ClosedSnapshot);
        let selection = host
            .typescript_host_selection(None)
            .expect("inspect closed TypeScript roles");
        assert!(selection.compiler.is_some());
        assert!(selection.compiler_explicit);
        assert!(selection.node.is_none());
        assert!(selection.module_root.is_none());

        let mut captured_default = environment;
        captured_default.values.clear();
        captured_default.set(LocalHostVariable::NudoxTypeScriptDefaultCompiler, &compiler);
        captured_default.set(
            LocalHostVariable::NudoxTypeScriptNode,
            &root.join("bin/node"),
        );
        captured_default.set(
            LocalHostVariable::NudoxTypeScriptModuleRoot,
            &root.join("lib/node_modules"),
        );
        let host = LocalCompilerHost::new(captured_default, LocalHostDiscovery::ClosedSnapshot);
        let selection = host
            .typescript_host_selection(None)
            .expect("inspect captured default TypeScript roles");
        assert_eq!(selection.compiler.as_deref(), Some(compiler.as_path()));
        assert!(!selection.compiler_explicit);
        assert_eq!(
            selection.node.as_ref().map(|node| node.path.as_path()),
            Some(root.join("bin/node").as_path())
        );
        assert_eq!(
            selection.module_root.as_deref(),
            Some(root.join("lib/node_modules").as_path())
        );
        assert!(
            host.executable(
                LocalHostVariable::NudoxPython,
                LocalHostPathRole::Native(NativeTool::Python),
                ArrayVec::new(),
            )
            .expect("closed Python role")
            .is_none()
        );
        fs::remove_dir_all(root).expect("remove fixture");
    }

    #[test]
    fn resource_open_refuses_fifo_and_link_swaps_after_regular_metadata_admission() {
        let root = private_test_directory("bundle-resource-open-race");
        let resource = root.join("resource");
        fs::write(&resource, b"admitted regular bytes").unwrap();
        let before = fs::symlink_metadata(&resource).unwrap();
        fs::remove_file(&resource).unwrap();
        #[cfg(target_os = "macos")]
        // The pinned rustix version has no Darwin mkfifo API. The system POSIX
        // fixture helper is bounded to this private path and its child is reaped.
        assert!(std::process::Command::new("/usr/bin/mkfifo").arg(&resource).status().unwrap().success());
        #[cfg(not(target_os = "macos"))]
        rustix::fs::mkfifo(&resource, rustix::fs::Mode::from_raw_mode(0o600)).unwrap();
        assert!(open_regular_resource(&resource, 1024, &before).is_err());
        assert!(read_bounded(&resource, 1024).is_err());
        assert!(sha256_file(&resource, 1024).is_err());
        fs::remove_file(&resource).unwrap();
        let outside = root.join("outside");
        fs::write(&outside, b"unadmitted external bytes").unwrap();
        std::os::unix::fs::symlink(&outside, &resource).unwrap();
        assert!(open_regular_resource(&resource, 1024, &before).is_err());
        assert!(read_bounded(&resource, 1024).is_err());
        fs::remove_dir_all(root).unwrap();
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
