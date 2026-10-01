//! C/C++ project identity and compilation-argument authority.
//!
//! This module owns the one project identity surface for the real C/C++
//! native fact lane: it checks an immutable project root and entry
//! translation unit, then supplies the exact libclang argument vector the
//! entry must parse under. Fact collection itself lives in [`crate::legacy`];
//! tree-sitter remains available through [`crate::syntax_frontend`] as a
//! syntax baseline and is never used to manufacture a semantic claim here.

use crate::{compile_commands::CompileCommands, system_includes};
use std::{
    ffi::OsString,
    fs,
    path::{Path, PathBuf},
};

/// The selected Clang toolchain facts used by one project authority.
///
/// The driver is canonicalized before probing. Its resource directory,
/// optional sysroot, and system include paths are probed with a cleared child
/// environment and retained here, so all projects in one scope use the same
/// compiler selection. `libclang_path` is the exact canonical library file
/// selected from the configured file or directory. clang-sys 1.9.1 cannot
/// load an arbitrary library path through a public API, so this type can
/// verify an already-loaded library but cannot safely cause the in-process
/// load itself.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ClangAuthorityEnvironment {
    driver: PathBuf,
    resource_dir: PathBuf,
    sysroot: Option<PathBuf>,
    libclang_selector: PathBuf,
    libclang_path: PathBuf,
    system_include_dirs: Vec<PathBuf>,
}

impl ClangAuthorityEnvironment {
    /// Probes one explicitly selected driver and binds it to an explicit
    /// libclang file or directory.
    ///
    /// `driver` must name an absolute executable. `libclang_path` must name
    /// an absolute regular file or directory. The driver is run with an empty
    /// environment for every probe; no shell, `PATH`, or ambient Clang
    /// selector participates.
    pub fn probe(
        driver: impl AsRef<Path>,
        libclang_path: impl AsRef<Path>,
    ) -> Result<Self, ClangAuthorityError> {
        let driver = checked_tool_file(driver.as_ref(), "Clang driver")?;
        let configured_libclang = checked_libclang_selector(libclang_path.as_ref())?;
        let (libclang_path, libclang_selector) = resolve_libclang_path(&configured_libclang)?;
        let facts =
            system_includes::probe(&driver).map_err(|source| ClangAuthorityError::DriverProbe {
                driver: driver.clone(),
                detail: source.to_string().into_boxed_str(),
            })?;
        Ok(Self {
            driver,
            resource_dir: facts.resource_dir,
            sysroot: facts.sysroot,
            libclang_selector,
            libclang_path,
            system_include_dirs: facts.system_include_dirs,
        })
    }

    /// Canonical executable selected for this Clang authority.
    #[must_use]
    pub fn driver(&self) -> &Path {
        &self.driver
    }

    /// Canonical builtin resource directory reported by the selected driver.
    #[must_use]
    pub fn resource_dir(&self) -> &Path {
        &self.resource_dir
    }

    /// Canonical sysroot reported by the selected driver, if it has one: its
    /// `-print-sysroot` answer, or for a driver that rejects that query (Apple's
    /// Clang, the Nix wrapper) the `-isysroot` its verbose dump names.
    #[must_use]
    pub fn sysroot(&self) -> Option<&Path> {
        self.sysroot.as_deref()
    }

    /// Exact canonical libclang shared-library file selected from the
    /// configured file or search directory.
    #[must_use]
    pub fn libclang_path(&self) -> &Path {
        &self.libclang_path
    }

    /// Canonical angle-bracket include directories reported by the selected driver.
    #[must_use]
    pub fn system_include_dirs(&self) -> &[PathBuf] {
        &self.system_include_dirs
    }

    /// Explicit variables to pass to an environment-cleared local worker.
    ///
    /// The caller must clear the child's environment before adding these
    /// entries. clang-sys will then resolve libclang from the configured path
    /// in that worker process. This does not load or configure libclang in the
    /// current process.
    #[must_use]
    pub fn child_environment(&self) -> [(OsString, OsString); 2] {
        [
            (
                OsString::from("NUDOX_CLANG"),
                self.driver.as_os_str().to_owned(),
            ),
            (
                OsString::from("LIBCLANG_PATH"),
                self.libclang_selector.as_os_str().to_owned(),
            ),
        ]
    }

    /// Confirms that this thread already has the explicitly configured
    /// libclang loaded.
    ///
    /// The clang-sys dynamic loader has no public API for selecting a library
    /// path directly. This method therefore never loads a library or falls
    /// back to the process environment; it returns a typed unsupported error
    /// until the caller has loaded libclang through its explicit local
    /// process configuration.
    pub fn loaded_libclang(&self) -> Result<LoadedLibclang, ClangAuthorityError> {
        let loaded = clang_sys::get_library().ok_or_else(|| {
            ClangAuthorityError::ExplicitLibclangLoadUnsupported {
                configured: self.libclang_path.clone(),
            }
        })?;
        let path =
            loaded
                .path()
                .canonicalize()
                .map_err(|source| ClangAuthorityError::ToolPathIo {
                    kind: "loaded libclang",
                    path: loaded.path().to_path_buf(),
                    source,
                })?;
        if path != self.libclang_path {
            return Err(ClangAuthorityError::LoadedLibclangMismatch {
                configured: self.libclang_path.clone(),
                loaded: path,
            });
        }
        Ok(LoadedLibclang { path })
    }

    /// Loads the explicitly configured local libclang when the current
    /// process environment selects the same canonical library.
    ///
    /// clang-sys only exposes environment-driven dynamic loading. This method
    /// checks `LIBCLANG_PATH` against the typed selection before invoking that
    /// loader and verifies the resulting loaded path before returning. A
    /// missing or hostile selector fails closed before libclang is used.
    pub fn load_configured_libclang(&self) -> Result<LoadedLibclang, ClangAuthorityError> {
        if !clang_sys::is_loaded() {
            let selected = std::env::var_os("LIBCLANG_PATH")
                .map(PathBuf::from)
                .ok_or_else(|| ClangAuthorityError::ExplicitLibclangLoadUnsupported {
                    configured: self.libclang_path.clone(),
                })?;
            let (selected, _) = resolve_libclang_path(&selected)?;
            if selected != self.libclang_path {
                return Err(ClangAuthorityError::LibclangEnvironmentMismatch {
                    configured: self.libclang_path.clone(),
                    environment: selected,
                });
            }
            clang_sys::load().map_err(|detail| ClangAuthorityError::LibclangLoad {
                configured: self.libclang_path.clone(),
                detail: detail.into_boxed_str(),
            })?;
        }
        self.loaded_libclang()
    }

    /// Runs a synchronous libclang operation on this thread after confirming
    /// the exact configured library is loaded in this thread's clang-sys TLS.
    ///
    /// Keep the native call inside `operation`; a later thread hop would not
    /// inherit clang-sys's thread-local library binding.
    pub fn with_loaded_libclang<T>(
        &self,
        operation: impl FnOnce() -> T,
    ) -> Result<T, ClangAuthorityError> {
        self.load_configured_libclang()?;
        Ok(operation())
    }
}

/// A libclang path confirmed against the explicit configured authority.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LoadedLibclang {
    path: PathBuf,
}

impl LoadedLibclang {
    /// Canonical path of the libclang library already loaded on this thread.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }
}

/// A checked C/C++ semantic authority rooted at one immutable project path and
/// bound to the one entry translation unit being compiled.
///
/// The entry is retained so the cross-file reference plane can be keyed
/// relative to the package root: libclang resolves project-local `#include`s
/// against their real on-disk paths, and only paths beneath this root yield a
/// stable cross-fragment identity (see `cursor_file_identity`).
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ClangProject {
    root: PathBuf,
    entry: PathBuf,
    environment: ClangAuthorityEnvironment,
}

impl ClangProject {
    /// Opens a project root and its exact entry translation unit using the
    /// selected Clang and libclang paths from the process configuration.
    ///
    /// This compatibility constructor requires both `NUDOX_CLANG` and
    /// `LIBCLANG_PATH`; it never reads `NUDOX_CLANG_DRIVER` or searches
    /// `PATH`. Production authorities should construct a typed environment
    /// through [`ClangAuthorityEnvironment::probe`] and call
    /// [`ClangProject::open_with_environment`].
    ///
    /// The root is not followed through a symlink, and the entry must be a
    /// regular file canonically contained beneath it, so no foreign fragment
    /// key can be minted from an out-of-project path.
    ///
    /// # Errors
    /// Returns [`ClangAuthorityError`] when `root` is not an absolute regular
    /// directory, when `entry` is not an absolute regular file, when either
    /// cannot be canonicalized, or when `entry` escapes `root`.
    pub fn open(
        root: impl AsRef<Path>,
        entry: impl AsRef<Path>,
    ) -> Result<Self, ClangAuthorityError> {
        let driver = std::env::var_os("NUDOX_CLANG").ok_or(
            ClangAuthorityError::MissingSelectedEnvironment {
                variable: "NUDOX_CLANG",
            },
        )?;
        let libclang_path = std::env::var_os("LIBCLANG_PATH").ok_or(
            ClangAuthorityError::MissingSelectedEnvironment {
                variable: "LIBCLANG_PATH",
            },
        )?;
        let environment = ClangAuthorityEnvironment::probe(driver, libclang_path)?;
        Self::open_with_environment(root, entry, environment)
    }

    /// Opens a project root and its exact entry translation unit under one
    /// explicit, immutable Clang toolchain environment.
    pub fn open_with_environment(
        root: impl AsRef<Path>,
        entry: impl AsRef<Path>,
        environment: ClangAuthorityEnvironment,
    ) -> Result<Self, ClangAuthorityError> {
        let requested = root.as_ref();
        if !requested.is_absolute() {
            return Err(ClangAuthorityError::RelativePath {
                path: requested.to_path_buf(),
            });
        }
        let metadata =
            fs::symlink_metadata(requested).map_err(|source| ClangAuthorityError::RootIo {
                path: requested.to_path_buf(),
                source,
            })?;
        if metadata.file_type().is_symlink() || !metadata.is_dir() {
            return Err(ClangAuthorityError::RootNotDirectory {
                path: requested.to_path_buf(),
            });
        }
        let root = requested
            .canonicalize()
            .map_err(|source| ClangAuthorityError::RootIo {
                path: requested.to_path_buf(),
                source,
            })?;
        let entry = checked_file(entry.as_ref())?;
        if !entry.starts_with(&root) {
            return Err(ClangAuthorityError::EntryOutsideRoot { root, entry });
        }
        Ok(Self {
            root,
            entry,
            environment,
        })
    }

    /// Returns the canonical project root retained by this authority.
    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Returns the canonical entry translation unit retained by this authority.
    #[must_use]
    pub fn entry(&self) -> &Path {
        &self.entry
    }

    /// Returns the exact toolchain environment selected for this project.
    #[must_use]
    pub fn environment(&self) -> &ClangAuthorityEnvironment {
        &self.environment
    }

    /// Returns the exact libclang argument vector for the entry translation
    /// unit: system includes, resource directory, and sysroot from this
    /// project's selected driver, followed by compilation-database arguments
    /// or per-extension defaults.
    #[must_use]
    pub fn arguments(&self) -> Vec<String> {
        let database = CompileCommands::load(&self.root);
        let selected = database
            .as_ref()
            .and_then(|commands| commands.args_for(&self.entry));
        let cpp_package = package_prefers_cpp(&self.root);
        let defaults = default_arguments(&self.entry, cpp_package);
        let selected = selected.as_deref().unwrap_or(&defaults);
        let selected = without_toolchain_authority_args(selected);
        let mut authority = vec![
            "-resource-dir".to_owned(),
            self.environment.resource_dir.to_string_lossy().into_owned(),
        ];
        if let Some(sysroot) = &self.environment.sysroot {
            authority.push(format!("--sysroot={}", sysroot.display()));
        }
        system_includes::args(&self.environment.system_include_dirs)
            .into_iter()
            .chain(authority)
            .chain(selected)
            .collect()
    }
}

/// Removes driver-selection flags from a compilation database before the
/// selected toolchain facts are prepended to the final argument vector.
fn without_toolchain_authority_args(arguments: &[String]) -> Vec<String> {
    let mut sanitized = Vec::with_capacity(arguments.len());
    let mut index = 0;
    while index < arguments.len() {
        let argument = arguments[index].as_str();
        if matches!(argument, "-resource-dir" | "--sysroot" | "-isysroot") {
            index += 2;
            continue;
        }
        if argument.starts_with("-resource-dir=")
            || argument.starts_with("--sysroot=")
            || argument.starts_with("-isysroot=")
        {
            index += 1;
            continue;
        }
        sanitized.push(arguments[index].clone());
        index += 1;
    }
    sanitized
}

/// Typed failure from the executable Clang authority.
#[derive(Debug, thiserror::Error)]
pub enum ClangAuthorityError {
    /// An explicit runtime selection variable required by the compatibility constructor was absent.
    #[error("explicit Clang selection is missing required environment variable {variable}")]
    MissingSelectedEnvironment {
        /// Exact selector required for the legacy constructor.
        variable: &'static str,
    },
    /// A caller supplied a relative path.
    #[error("Clang authority path is relative: {path}", path = path.display())]
    RelativePath {
        /// Caller-selected path.
        path: PathBuf,
    },
    /// The project root could not be inspected.
    #[error("cannot inspect Clang project root {path}: {source}", path = path.display())]
    RootIo {
        /// Path involved in the filesystem operation.
        path: PathBuf,
        /// Original filesystem failure.
        #[source]
        source: std::io::Error,
    },
    /// The project root was not a real directory.
    #[error("Clang project root is not a directory: {path}", path = path.display())]
    RootNotDirectory {
        /// Caller-selected root path.
        path: PathBuf,
    },
    /// The entry translation unit was not contained beneath the project root.
    #[error(
        "Clang entry {entry} is outside project root {root}",
        entry = entry.display(),
        root = root.display()
    )]
    EntryOutsideRoot {
        /// Canonical project root.
        root: PathBuf,
        /// Canonical entry path that escaped the root.
        entry: PathBuf,
    },
    /// A source file disappeared or changed type during discovery.
    #[error("Clang source path is not a regular file: {path}", path = path.display())]
    InvalidSource {
        /// Source path that failed the regular-file check.
        path: PathBuf,
    },
    /// A configured driver or libclang path could not be inspected.
    #[error("cannot inspect {kind} path {path}: {source}", path = path.display())]
    ToolPathIo {
        /// Configured tool kind.
        kind: &'static str,
        /// Path involved in the filesystem operation.
        path: PathBuf,
        /// Original filesystem failure.
        #[source]
        source: std::io::Error,
    },
    /// A configured tool path did not have the required file or directory type.
    #[error("{kind} path has the wrong file type: {path}", path = path.display())]
    InvalidToolPath {
        /// Configured tool kind.
        kind: &'static str,
        /// Path that failed the file type check.
        path: PathBuf,
    },
    /// The selected Clang driver did not provide a valid toolchain probe.
    #[error(
        "cannot establish selected Clang driver authority at {driver}: {detail}",
        driver = driver.display()
    )]
    DriverProbe {
        /// Canonical selected executable.
        driver: PathBuf,
        /// Exact probe failure.
        detail: Box<str>,
    },
    /// clang-sys has no safe explicit-path loader and this thread has not loaded libclang yet.
    #[error("explicit in-process libclang loading is unsupported for configured path {configured}", configured = configured.display())]
    ExplicitLibclangLoadUnsupported {
        /// Canonical configured file or directory.
        configured: PathBuf,
    },
    /// This thread loaded a library outside the explicitly selected file or directory.
    #[error("loaded libclang {loaded} does not match selected authority {configured}", loaded = loaded.display(), configured = configured.display())]
    LoadedLibclangMismatch {
        /// Canonical configured file or directory.
        configured: PathBuf,
        /// Canonical library path clang-sys loaded.
        loaded: PathBuf,
    },
    /// The process selector resolves to a different library than the typed authority.
    #[error("LIBCLANG_PATH resolves to {environment}, expected selected libclang {configured}", environment = environment.display(), configured = configured.display())]
    LibclangEnvironmentMismatch {
        /// Exact canonical library selected by the authority.
        configured: PathBuf,
        /// Exact canonical library selected by the process environment.
        environment: PathBuf,
    },
    /// The explicitly checked dynamic loader could not open the selected library.
    #[error("could not load selected libclang {configured}: {detail}", configured = configured.display())]
    LibclangLoad {
        /// Exact canonical library selected by the authority.
        configured: PathBuf,
        /// clang-sys loader error details.
        detail: Box<str>,
    },
    /// The configured libclang directory did not contain a supported shared library.
    #[error("no supported libclang shared library was found under {configured}", configured = configured.display())]
    NoLibclangLibrary {
        /// Configured file or directory that could not be resolved.
        configured: PathBuf,
    },
}

fn checked_tool_file(path: &Path, kind: &'static str) -> Result<PathBuf, ClangAuthorityError> {
    if !path.is_absolute() {
        return Err(ClangAuthorityError::RelativePath {
            path: path.to_path_buf(),
        });
    }
    let canonical = path
        .canonicalize()
        .map_err(|source| ClangAuthorityError::ToolPathIo {
            kind,
            path: path.to_path_buf(),
            source,
        })?;
    if !canonical.is_file() {
        return Err(ClangAuthorityError::InvalidToolPath {
            kind,
            path: canonical,
        });
    }
    Ok(canonical)
}

/// Retains a clang-sys-compatible selector while canonicalizing its directory.
/// For an explicit library symlink, preserving its filename matters on macOS:
/// clang-sys recognizes `libclang.dylib`, while canonicalization may expose a
/// versioned target filename that its selector does not accept.
fn checked_libclang_selector(path: &Path) -> Result<PathBuf, ClangAuthorityError> {
    if !path.is_absolute() {
        return Err(ClangAuthorityError::RelativePath {
            path: path.to_path_buf(),
        });
    }
    let metadata = fs::metadata(path).map_err(|source| ClangAuthorityError::ToolPathIo {
        kind: "libclang",
        path: path.to_path_buf(),
        source,
    })?;
    if metadata.is_dir() {
        return path
            .canonicalize()
            .map_err(|source| ClangAuthorityError::ToolPathIo {
                kind: "libclang",
                path: path.to_path_buf(),
                source,
            });
    }
    if metadata.is_file() && is_libclang_filename(path) {
        let parent = path
            .parent()
            .ok_or_else(|| ClangAuthorityError::InvalidToolPath {
                kind: "libclang shared library",
                path: path.to_path_buf(),
            })?;
        let canonical_parent =
            parent
                .canonicalize()
                .map_err(|source| ClangAuthorityError::ToolPathIo {
                    kind: "libclang parent",
                    path: parent.to_path_buf(),
                    source,
                })?;
        let name = path
            .file_name()
            .ok_or_else(|| ClangAuthorityError::InvalidToolPath {
                kind: "libclang shared library",
                path: path.to_path_buf(),
            })?;
        return Ok(canonical_parent.join(name));
    }
    Err(ClangAuthorityError::InvalidToolPath {
        kind: "libclang shared library",
        path: path.to_path_buf(),
    })
}

/// Resolves the same versioned filename family clang-sys searches when the
/// configured value is a directory, then records the exact canonical file.
fn resolve_libclang_path(path: &Path) -> Result<(PathBuf, PathBuf), ClangAuthorityError> {
    if !path.is_absolute() {
        return Err(ClangAuthorityError::RelativePath {
            path: path.to_path_buf(),
        });
    }
    if path.is_file() {
        let exact = path
            .canonicalize()
            .map_err(|source| ClangAuthorityError::ToolPathIo {
                kind: "libclang shared library",
                path: path.to_path_buf(),
                source,
            })?;
        return Ok((exact, path.to_path_buf()));
    }

    let configured = path
        .canonicalize()
        .map_err(|source| ClangAuthorityError::ToolPathIo {
            kind: "libclang directory",
            path: path.to_path_buf(),
            source,
        })?;
    let mut directories = vec![configured.clone()];
    #[cfg(target_os = "windows")]
    if configured.file_name().is_some_and(|name| name == "lib") {
        if let Some(parent) = configured.parent() {
            directories.push(parent.join("bin"));
        }
    }
    let mut found = Vec::new();
    for directory in directories {
        let entries =
            fs::read_dir(&directory).map_err(|source| ClangAuthorityError::ToolPathIo {
                kind: "libclang directory",
                path: directory.clone(),
                source,
            })?;
        for entry in entries {
            let entry = entry.map_err(|source| ClangAuthorityError::ToolPathIo {
                kind: "libclang directory entry",
                path: directory.clone(),
                source,
            })?;
            let candidate = entry.path();
            if !candidate.is_file() || !is_libclang_filename(&candidate) {
                continue;
            }
            let canonical =
                candidate
                    .canonicalize()
                    .map_err(|source| ClangAuthorityError::ToolPathIo {
                        kind: "libclang candidate",
                        path: candidate.clone(),
                        source,
                    })?;
            let filename = candidate
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or_default()
                .to_owned();
            found.push((
                library_version(&filename),
                libclang_filename_rank(&filename),
                canonical,
                candidate,
                filename,
            ));
        }
    }
    found.sort_by(|left, right| {
        right
            .0
            .cmp(&left.0)
            .then_with(|| left.1.cmp(&right.1))
            .then_with(|| left.3.cmp(&right.3))
    });
    found
        .first()
        .map(|(_, _, exact, selector, _)| (exact.clone(), selector.clone()))
        .ok_or(ClangAuthorityError::NoLibclangLibrary { configured })
}

#[cfg(target_os = "linux")]
fn is_libclang_filename(path: &Path) -> bool {
    let Some(filename) = path.file_name().and_then(|name| name.to_str()) else {
        return false;
    };
    filename == "libclang.so"
        || (filename.starts_with("libclang.so.") && !filename.contains("-cpp."))
        || (filename.starts_with("libclang-")
            && filename.ends_with(".so")
            && !filename.contains("-cpp."))
        || (filename.starts_with("libclang-")
            && filename.contains(".so.")
            && !filename.contains("-cpp."))
}

#[cfg(target_os = "linux")]
fn libclang_filename_rank(filename: &str) -> u8 {
    if filename == "libclang.so" {
        0
    } else if filename.starts_with("libclang-") && filename.ends_with(".so") {
        1
    } else if filename.starts_with("libclang.so.") {
        2
    } else {
        3
    }
}

#[cfg(target_os = "macos")]
fn is_libclang_filename(path: &Path) -> bool {
    path.file_name()
        .is_some_and(|name| name == "libclang.dylib")
}

#[cfg(target_os = "macos")]
fn libclang_filename_rank(_: &str) -> u8 {
    0
}

#[cfg(target_os = "windows")]
fn is_libclang_filename(path: &Path) -> bool {
    path.file_name()
        .is_some_and(|name| name == "libclang.dll" || name == "clang.dll")
}

#[cfg(target_os = "windows")]
fn libclang_filename_rank(filename: &str) -> u8 {
    if filename == "libclang.dll" { 0 } else { 1 }
}

#[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
fn is_libclang_filename(path: &Path) -> bool {
    path.file_name().is_some_and(|name| {
        name == format!(
            "{}clang{}",
            std::env::consts::DLL_PREFIX,
            std::env::consts::DLL_SUFFIX
        )
    })
}

#[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
fn libclang_filename_rank(_: &str) -> u8 {
    0
}

fn library_version(filename: &str) -> Vec<u32> {
    let version = filename
        .strip_prefix("libclang.so.")
        .or_else(|| {
            filename
                .strip_prefix("libclang-")
                .and_then(|rest| rest.strip_suffix(".so"))
        })
        .or_else(|| {
            filename
                .strip_prefix("libclang-")
                .and_then(|rest| rest.split_once(".so."))
                .map(|(_, version)| version)
        });
    version
        .map(|version| {
            version
                .split('.')
                .map(|part| part.parse::<u32>().unwrap_or_default())
                .collect()
        })
        .unwrap_or_default()
}

fn checked_file(path: &Path) -> Result<PathBuf, ClangAuthorityError> {
    if !path.is_absolute() {
        return Err(ClangAuthorityError::RelativePath {
            path: path.to_path_buf(),
        });
    }
    let metadata = fs::symlink_metadata(path).map_err(|source| ClangAuthorityError::RootIo {
        path: path.to_path_buf(),
        source,
    })?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(ClangAuthorityError::InvalidSource {
            path: path.to_path_buf(),
        });
    }
    path.canonicalize()
        .map_err(|source| ClangAuthorityError::RootIo {
            path: path.to_path_buf(),
            source,
        })
}

/// The per-extension parse arguments for a file absent from the compilation
/// database.
///
/// C++ header extensions are classified as C++ deliberately: libclang infers
/// the language from the file name alone, and `.hpp`/`.hh`/`.hxx`/`.h++` are
/// unambiguously C++ there, so a C default contradicts the file's own
/// language. Measured against the real-package corpus: the amalgamated
/// single-header entries `nlohmann-json/single_include/nlohmann/json.hpp`
/// (919,975 bytes) and `catch2/single_include/catch2/catch.hpp` (657,411
/// bytes) were handed `-std=c11`, which the driver rejects with the fatal
/// `invalid argument '-std=c11' not allowed with 'C++'` before a single
/// declaration is visited, and `clang_parseTranslationUnit2` reports that
/// rejection as `CXError_ASTReadError`. `.h` stays in the C arm on purpose
/// when the package is C-only: it is genuinely ambiguous (C projects and C++
/// projects both use it), and the C default matches libclang's own extension
/// inference for it. A `.h` next to C++ sources, or in a header-only tree whose
/// headers look like C++, uses the C++ arm instead.
fn default_arguments(path: &Path, cpp_package: bool) -> Vec<String> {
    if prefers_cpp(path, cpp_package) {
        vec!["-std=c++17".to_owned(), "-x".to_owned(), "c++".to_owned()]
    } else {
        vec!["-std=c11".to_owned()]
    }
}

fn prefers_cpp(path: &Path, cpp_package: bool) -> bool {
    is_cpp_source(path) || is_cpp_header(path) || (is_c_header(path) && cpp_package)
}

fn package_prefers_cpp(root: &Path) -> bool {
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
                continue;
            }
            if is_cpp_source(&path)
                || is_cpp_header(&path)
                || (is_c_header(&path) && header_looks_like_cpp(&path))
            {
                return true;
            }
        }
    }
    false
}

fn is_cpp_source(path: &Path) -> bool {
    matches!(
        path.extension().and_then(|extension| extension.to_str()),
        Some("cc" | "cpp" | "cxx" | "C" | "c++" | "mm")
    )
}

fn is_cpp_header(path: &Path) -> bool {
    matches!(
        path.extension().and_then(|extension| extension.to_str()),
        Some("hpp" | "hxx" | "hh" | "h++" | "H")
    )
}

fn is_c_header(path: &Path) -> bool {
    matches!(
        path.extension().and_then(|extension| extension.to_str()),
        Some("h")
    )
}

/// C++ tokens that are not C. A comment that mentions `class` can false-trigger;
/// a header-only C library that only uses `struct` does not.
fn header_looks_like_cpp(path: &Path) -> bool {
    let Ok(text) = fs::read_to_string(path) else {
        return false;
    };
    let head: String = text.chars().take(64 * 1024).collect();
    [
        "namespace ",
        "namespace\t",
        "template<",
        "template <",
        "constexpr",
        "nullptr",
    ]
    .iter()
    .any(|token| head.contains(token))
        || head.contains("class ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    #[cfg(unix)]
    #[test]
    fn same_version_drivers_keep_their_resource_and_sysroot_authorities_distinct() {
        let temp = tempfile::tempdir().expect("temporary fixture");
        let first = fixture_toolchain(temp.path(), "first");
        let second = fixture_toolchain(temp.path(), "second");
        let first_environment = ClangAuthorityEnvironment::probe(
            &first.0,
            first.1.parent().expect("library directory"),
        )
        .expect("first toolchain probe");
        let second_environment =
            ClangAuthorityEnvironment::probe(&second.0, &second.1).expect("second toolchain probe");

        assert_eq!(driver_version(&first.0), driver_version(&second.0));
        assert_ne!(first_environment.driver(), second_environment.driver());
        assert_ne!(
            first_environment.resource_dir(),
            second_environment.resource_dir()
        );
        assert_ne!(first_environment.sysroot(), second_environment.sysroot());
        assert_ne!(
            first_environment.system_include_dirs(),
            second_environment.system_include_dirs()
        );
        assert_eq!(
            first_environment.libclang_path(),
            first.1.canonicalize().unwrap()
        );
        assert_eq!(
            second_environment.libclang_path(),
            second.1.canonicalize().unwrap()
        );

        let package = temp.path().join("first-package");
        fs::create_dir_all(&package).expect("package root");
        let entry = package.join("entry.cpp");
        fs::write(&entry, "int main() { return 0; }\n").expect("entry source");
        let project =
            ClangProject::open_with_environment(&package, &entry, first_environment.clone())
                .expect("selected project");
        let arguments = project.arguments();
        assert!(arguments.windows(2).any(|pair| {
            pair == [
                "-resource-dir",
                first_environment.resource_dir().to_string_lossy().as_ref(),
            ]
        }));
        assert!(arguments.iter().any(|argument| {
            argument
                == &format!(
                    "--sysroot={}",
                    first_environment.sysroot().unwrap().display()
                )
        }));
        assert!(arguments.windows(2).any(|pair| {
            pair == [
                "-isystem",
                first_environment.system_include_dirs()[0]
                    .to_string_lossy()
                    .as_ref(),
            ]
        }));
    }

    #[cfg(unix)]
    #[test]
    fn hostile_parent_selectors_cannot_redirect_the_explicit_driver_probe() {
        let temp = tempfile::tempdir().expect("temporary fixture");
        let selected = fixture_toolchain(temp.path(), "selected");
        let hostile = fixture_toolchain(temp.path(), "hostile");
        let root = temp.path().to_string_lossy().into_owned();
        let output = Command::new(std::env::current_exe().expect("test executable"))
            .args([
                "--exact",
                "authority::tests::hostile_parent_environment_child",
                "--nocapture",
            ])
            .env_clear()
            .env("CLANG_AUTHORITY_TEST_ROOT", root)
            .env("NUDOX_CLANG", &hostile.0)
            .env("NUDOX_CLANG_DRIVER", &hostile.0)
            .env("LIBCLANG_PATH", &hostile.1)
            .env("PATH", temp.path().join("no-tools-here"))
            .output()
            .expect("isolated child test");
        assert!(
            output.status.success(),
            "hostile environment child failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );

        // The child resolves only the paths passed through this explicit test
        // channel. All ambient selectors above point at the hostile fixture.
        let explicit = ClangAuthorityEnvironment::probe(&selected.0, &selected.1)
            .expect("explicit selected toolchain");
        assert_eq!(explicit.driver(), selected.0.canonicalize().unwrap());
        assert_eq!(explicit.libclang_path(), selected.1.canonicalize().unwrap());
        assert_ne!(explicit.resource_dir(), Path::new("/opt/hostile/resource"));
    }

    #[cfg(unix)]
    #[test]
    fn hostile_parent_environment_child() {
        let Ok(root) = std::env::var("CLANG_AUTHORITY_TEST_ROOT") else {
            return;
        };
        let root = PathBuf::from(root);
        let environment = ClangAuthorityEnvironment::probe(
            root.join("selected-driver"),
            root.join("selected").join(libclang_file_name()),
        )
        .expect("the explicit selected toolchain wins over hostile parent variables");
        assert_eq!(
            environment.driver(),
            root.join("selected-driver").canonicalize().unwrap()
        );
        assert_eq!(
            environment.resource_dir(),
            root.join("selected/resource").canonicalize().unwrap()
        );
        assert_eq!(
            environment.libclang_path(),
            root.join("selected")
                .join(libclang_file_name())
                .canonicalize()
                .unwrap()
        );
        assert_eq!(
            environment.child_environment()[0].1.as_os_str(),
            environment.driver().as_os_str()
        );
        assert!(matches!(
            environment.load_configured_libclang(),
            Err(ClangAuthorityError::LibclangEnvironmentMismatch { .. })
        ));
    }

    /// The Nix wrapper and Apple's Clang answer `-print-sysroot` with "unknown
    /// argument" (exit 1) and name the SDK as `-isysroot` in their verbose
    /// dump. The authority a desktop owner boots on must still carry that SDK.
    #[cfg(unix)]
    #[test]
    fn a_driver_that_rejects_print_sysroot_still_hands_libclang_its_sysroot() {
        use std::os::unix::fs::PermissionsExt as _;

        let temp = tempfile::tempdir().expect("temporary fixture");
        let (driver, libclang) = fixture_toolchain(temp.path(), "wrapper");
        let sysroot = temp.path().join("wrapper").join("sysroot");
        let include = temp.path().join("wrapper").join("include");
        let resource = temp.path().join("wrapper").join("resource");
        let script = format!(
            "#!/bin/sh\ncase \"$1\" in\n  -print-resource-dir) printf '%s\\n' {};;\n  -print-sysroot) echo \"clang: error: unknown argument: '-print-sysroot'\" >&2; exit 1;;\n  -v) printf ' \"/x/clang\" -cc1 -isysroot %s -x c++ -\\n#include <...> search starts here:\\n %s\\nEnd of search list.\\n' {} {} >&2;;\n  *) exit 2;;\nesac\n",
            shell_quote(&resource),
            shell_quote(&sysroot),
            shell_quote(&include),
        );
        fs::write(&driver, script).expect("rewrite the driver as the wrapper");
        fs::set_permissions(&driver, fs::Permissions::from_mode(0o755)).expect("executable driver");

        let environment = ClangAuthorityEnvironment::probe(&driver, &libclang)
            .expect("a driver that rejects -print-sysroot is admitted");

        assert_eq!(
            environment.sysroot(),
            Some(sysroot.canonicalize().unwrap().as_path())
        );
        let package = temp.path().join("package");
        fs::create_dir_all(&package).expect("package root");
        let entry = package.join("entry.cpp");
        fs::write(&entry, "int main() { return 0; }\n").expect("entry source");
        let project = ClangProject::open_with_environment(&package, &entry, environment)
            .expect("selected project");
        assert!(
            project.arguments().iter().any(|argument| argument
                == &format!("--sysroot={}", sysroot.canonicalize().unwrap().display())),
            "libclang is given the SDK the driver names: {:?}",
            project.arguments()
        );
    }

    #[cfg(unix)]
    fn fixture_toolchain(root: &Path, name: &str) -> (PathBuf, PathBuf) {
        use std::os::unix::fs::PermissionsExt as _;

        let fixture = root.join(name);
        let resource = fixture.join("resource");
        let sysroot = fixture.join("sysroot");
        let include = fixture.join("include");
        fs::create_dir_all(&resource).expect("resource dir");
        fs::create_dir_all(&sysroot).expect("sysroot dir");
        fs::create_dir_all(&include).expect("include dir");
        let driver = root.join(format!("{name}-driver"));
        let script = format!(
            "#!/bin/sh\ncase \"$1\" in\n  --version) echo 'clang version 18.1.0';;\n  -print-resource-dir) printf '%s\\n' {};;\n  -print-sysroot) printf '%s\\n' {};;\n  -v) printf '#include <...> search starts here:\\n %s\\nEnd of search list.\\n' {} >&2;;\n  *) exit 2;;\nesac\n",
            shell_quote(&resource),
            shell_quote(&sysroot),
            shell_quote(&include),
        );
        fs::write(&driver, script).expect("driver script");
        let mut permissions = fs::metadata(&driver).unwrap().permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&driver, permissions).expect("executable driver");

        let libclang = fixture.join(libclang_file_name());
        fs::write(&libclang, b"fixture library").expect("library marker");
        (driver, libclang)
    }

    #[cfg(unix)]
    fn shell_quote(path: &Path) -> String {
        format!("'{}'", path.to_string_lossy().replace('\'', "'\\''"))
    }

    #[cfg(target_os = "linux")]
    fn libclang_file_name() -> &'static str {
        "libclang.so.18.1"
    }

    #[cfg(target_os = "macos")]
    fn libclang_file_name() -> &'static str {
        "libclang.dylib"
    }

    #[cfg(unix)]
    fn driver_version(driver: &Path) -> String {
        let output = Command::new(driver)
            .arg("--version")
            .env_clear()
            .output()
            .expect("driver version");
        String::from_utf8_lossy(&output.stdout).trim().to_owned()
    }
}
