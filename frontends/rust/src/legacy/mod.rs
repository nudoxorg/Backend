//! Provides Rust's in-process semantic authority for the compiler.
//! Loads one Cargo graph under an explicit Rust edition and extracts only HIR-backed facts.
//! Keeps rust-analyzer implementation values within a bounded, non-serialized transaction.
//!
//! CANONICAL AUTHORITY PATH: this `legacy` module IS the production
//! native-authority lane. The engine driver imports its symbols directly from
//! this module; there is no intervening adapter. The crate-level
//! `syntax_frontend()` constructor is the separate, documented structural
//! baseline and never substitutes for this authority.

use std::{path::PathBuf, process::Command};

mod authority;
mod purl;

pub use self::authority::{
    ByteSpan, MAX_RUST_WORKSPACE_SESSION_SOURCES, ModuleDeclaration, RustAnalysisControl,
    RustAuthority, RustAuthorityError, RustDeclaration, RustDefinition, RustFeatureControl,
    RustFieldAccess, RustInferredExpression, RustMethodCall, RustProject, RustReexport,
    RustSourceScope, RustWorkspace, RustWorkspaceFile, RustWorkspaceFrontierId,
    RustWorkspaceSessionCache, RustWorkspaceSessionKey, RustWorkspaceSessionLease,
    RustWorkspaceSessionStats, SemanticKind, SourceByteLimit, SourceOrigin,
};
pub use self::purl::{RustLocatedPackage, RustPackageUrl, RustPurlError, manifest_edition};

/// The pinned rust-analyzer HIR facade this authority borrows from.
///
/// Re-exported so downstream lane projections share exactly the rust-analyzer
/// version this authority was compiled against, without widening the
/// dependency graph or re-pinning salsa-coupled crates elsewhere.
pub use ra_ap_hir;

/// The pinned rust-analyzer inference database type behind [`RustAuthority`].
pub use ra_ap_ide_db;

/// The pinned Rust syntax tree this authority parses and spans.
///
/// Re-exported for the same version-lock reason as [`ra_ap_hir`]: every
/// consumer of a borrowed [`RustAuthority`] must address the exact
/// `ra_ap_syntax` release this authority parsed with.
pub use ra_ap_syntax;

/// Versioned identity of the isolated Rust/Cargo child-process environment.
/// Changing its admitted variables or path construction invalidates existing
/// compiler authority identities even when the tool executables are unchanged.
pub const RUST_PACKAGE_CHILD_ENVIRONMENT_POLICY_ID_V1: &str = "rust-package-child-environment.v1";

/// Native compiler identity, Cargo cache roots, and sysroot accepted for one
/// Rust authority transaction.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RustToolchain {
    /// Caller-selected compiler executable.
    pub tool: PathBuf,
    /// Sysroot reported by exactly that compiler executable.
    pub sysroot: PathBuf,
    /// Caller-selected Cargo executable used by package metadata and RA.
    /// `None` is retained only for compatibility callers; package authority
    /// rejects a toolchain without this closed process configuration.
    pub cargo: Option<PathBuf>,
    /// Explicit Cargo home holding the admitted dependency cache/config.
    pub cargo_home: Option<PathBuf>,
    /// Rustup home derived from an admitted rustup sysroot when applicable.
    pub rustup_home: Option<PathBuf>,
    /// Optional explicit rustup toolchain selection for proxy executables.
    pub rustup_toolchain: Option<String>,
}

impl RustToolchain {
    /// Admits caller-selected absolute Rust compiler and sysroot paths
    /// without running a discovery child or consulting ambient tool state.
    pub fn from_paths(tool: PathBuf, sysroot: PathBuf) -> Result<Self, LoadError> {
        if !tool.is_absolute() {
            return Err(LoadError::RelativeTool { tool });
        }
        if !sysroot.is_absolute() {
            return Err(LoadError::RelativeSysroot { sysroot });
        }
        if !tool.is_file() {
            return Err(LoadError::InvalidTool { path: tool });
        }
        if !sysroot.is_dir() {
            return Err(LoadError::InvalidSysroot { path: sysroot });
        }
        let (rustup_home, rustup_toolchain) = rustup_selection(&sysroot);
        Ok(Self {
            tool,
            sysroot,
            cargo: None,
            cargo_home: None,
            rustup_home,
            rustup_toolchain,
        })
    }

    /// Admits caller-selected Rust and Cargo executables, sysroot, and Cargo
    /// home without consulting ambient environment variables.
    pub fn from_paths_with_cargo(
        tool: PathBuf,
        sysroot: PathBuf,
        cargo: PathBuf,
        cargo_home: PathBuf,
    ) -> Result<Self, LoadError> {
        let mut toolchain = Self::from_paths(tool, sysroot)?;
        if !cargo.is_absolute() {
            return Err(LoadError::RelativeCargo { cargo });
        }
        if !cargo_home.is_absolute() {
            return Err(LoadError::RelativeCargoHome { cargo_home });
        }
        if !cargo.is_file() {
            return Err(LoadError::InvalidCargo { path: cargo });
        }
        if !cargo_home.is_dir() {
            return Err(LoadError::InvalidCargoHome { path: cargo_home });
        }
        toolchain.cargo = Some(cargo);
        toolchain.cargo_home = Some(cargo_home);
        Ok(toolchain)
    }

    /// Admits the rustup selector required by an explicitly selected proxy
    /// executable. The spelling is passed as `RUSTUP_TOOLCHAIN` in isolated
    /// Cargo/Rust child environments.
    pub fn with_rustup_toolchain(
        mut self,
        rustup_toolchain: impl Into<String>,
    ) -> Result<Self, LoadError> {
        let rustup_toolchain = rustup_toolchain.into();
        if rustup_toolchain.trim().is_empty() || rustup_toolchain.contains('\0') {
            return Err(LoadError::InvalidRustupToolchain);
        }
        self.rustup_toolchain = Some(rustup_toolchain);
        Ok(self)
    }

    /// Returns a stable, absolute search path containing only the admitted
    /// Rust and Cargo executable directories.
    pub(crate) fn authority_path(&self) -> Result<String, LoadError> {
        let cargo = self
            .cargo
            .as_ref()
            .ok_or(LoadError::MissingCargoConfiguration)?;
        let cargo_home = self
            .cargo_home
            .as_ref()
            .ok_or(LoadError::MissingCargoConfiguration)?;
        let mut paths = Vec::new();
        for path in [
            cargo_home.join("bin"),
            cargo
                .parent()
                .unwrap_or_else(|| std::path::Path::new("/"))
                .to_path_buf(),
            self.tool
                .parent()
                .unwrap_or_else(|| std::path::Path::new("/"))
                .to_path_buf(),
            self.sysroot.join("bin"),
        ] {
            if !paths.contains(&path) {
                paths.push(path);
            }
        }
        std::env::join_paths(paths)
            .map(|path| path.to_string_lossy().into_owned())
            .map_err(|_| LoadError::InvalidAuthorityPath)
    }

    /// Discovers a compatibility toolchain from the caller-selected Rust
    /// compiler and conventional Cargo installation.
    ///
    /// Production package authority should use [`Self::from_paths_with_cargo`]
    /// with host-admitted paths. This compatibility helper captures the
    /// conventional Cargo executable/cache and rustup selection once; later
    /// authority subprocesses use those exact paths in an isolated env.
    ///
    /// # Errors
    ///
    /// Returns [`LoadError`] when the executable cannot report a usable sysroot.
    pub fn discover(tool: impl Into<PathBuf>) -> Result<Self, LoadError> {
        let requested_tool = tool.into();
        let tool = resolve_executable(&requested_tool).ok_or_else(|| {
            LoadError::ExecutableUnavailable {
                tool: requested_tool.clone(),
            }
        })?;
        let mut command = Command::new(&tool);
        let output = command
            .args(["--print", "sysroot"])
            .output()
            .map_err(|source| LoadError::SysrootQuery {
                tool: tool.clone(),
                source,
            })?;
        if !output.status.success() {
            return Err(LoadError::SysrootUnavailable { tool });
        }
        let sysroot = PathBuf::from(String::from_utf8_lossy(&output.stdout).trim());
        if !sysroot.is_dir() {
            return Err(LoadError::InvalidSysroot { path: sysroot });
        }
        let cargo_hint = std::env::var_os("CARGO")
            .map(PathBuf::from)
            .unwrap_or_else(|| {
                tool.parent()
                    .unwrap_or_else(|| std::path::Path::new("."))
                    .join("cargo")
            });
        let cargo = resolve_executable(&cargo_hint)
            .or_else(|| resolve_executable(std::path::Path::new("cargo")))
            .ok_or_else(|| LoadError::ExecutableUnavailable { tool: cargo_hint })?;
        let cargo_home = std::env::var_os("CARGO_HOME")
            .map(PathBuf::from)
            .or_else(|| {
                std::env::var_os("HOME")
                    .map(PathBuf::from)
                    .map(|home| home.join(".cargo"))
            })
            .ok_or(LoadError::MissingCargoHome)?;
        let cargo_home = if cargo_home.is_absolute() {
            cargo_home
        } else {
            std::env::current_dir()
                .map_err(LoadError::CurrentDirectory)?
                .join(cargo_home)
        };
        let mut toolchain = Self::from_paths_with_cargo(tool, sysroot, cargo, cargo_home)?;
        if let Some(selector) = std::env::var_os("RUSTUP_TOOLCHAIN") {
            toolchain = toolchain.with_rustup_toolchain(selector.to_string_lossy().into_owned())?;
        }
        Ok(toolchain)
    }
}

/// Failure to establish the native toolchain required by rust-analyzer HIR.
#[derive(Debug, thiserror::Error)]
pub enum LoadError {
    /// A relative compiler path would defer authority selection to ambient
    /// process search state.
    #[error("Rust compiler path is not absolute: {tool}")]
    RelativeTool {
        /// Rejected compiler path.
        tool: PathBuf,
    },
    /// A relative sysroot path cannot be retained as host authority.
    #[error("Rust sysroot path is not absolute: {sysroot}")]
    RelativeSysroot {
        /// Rejected sysroot path.
        sysroot: PathBuf,
    },
    /// A relative Cargo path would defer executable selection to ambient state.
    #[error("Cargo executable path is not absolute: {cargo}")]
    RelativeCargo {
        /// Rejected Cargo executable path.
        cargo: PathBuf,
    },
    /// A relative Cargo home would select cache/config from ambient state.
    #[error("Cargo home path is not absolute: {cargo_home}")]
    RelativeCargoHome {
        /// Rejected Cargo home path.
        cargo_home: PathBuf,
    },
    /// The explicit Cargo executable does not name a regular file.
    #[error("configured Cargo executable is not a file: {path}")]
    InvalidCargo {
        /// Exact unusable Cargo executable path.
        path: PathBuf,
    },
    /// The explicit Cargo home does not name an existing directory.
    #[error("configured Cargo home is not a directory: {path}")]
    InvalidCargoHome {
        /// Exact unusable Cargo home path.
        path: PathBuf,
    },
    /// Package authority requires Cargo and Cargo home to close process inputs.
    #[error("Rust package authority requires an explicit Cargo executable and Cargo home")]
    MissingCargoConfiguration,
    /// A NUL-containing or empty rustup selector cannot be passed to a process.
    #[error("invalid explicit RUSTUP_TOOLCHAIN value")]
    InvalidRustupToolchain,
    /// The admitted executable directories cannot be encoded as PATH.
    #[error("Rust authority search path could not be encoded")]
    InvalidAuthorityPath,
    /// No executable could be resolved from the compatibility discovery input.
    #[error("cannot resolve Rust tool executable: {tool}")]
    ExecutableUnavailable {
        /// Requested executable spelling.
        tool: PathBuf,
    },
    /// Compatibility discovery needs an explicit or conventional Cargo home.
    #[error("cannot resolve Cargo home from CARGO_HOME or HOME")]
    MissingCargoHome,
    /// The current directory could not be read while resolving a relative path.
    #[error("cannot resolve Rust tool path from the current directory: {0}")]
    CurrentDirectory(#[source] std::io::Error),
    /// The explicit compiler path does not name a regular file.
    #[error("configured Rust compiler is not a file: {path}")]
    InvalidTool {
        /// Exact unusable compiler path.
        path: PathBuf,
    },
    /// The selected compiler process could not be started.
    #[error("cannot run {tool} for Rust sysroot discovery: {source}")]
    SysrootQuery {
        /// Exact executable selected by the caller.
        tool: PathBuf,
        /// Original operating-system failure.
        #[source]
        source: std::io::Error,
    },
    /// The compiler did not successfully report a sysroot.
    #[error("{tool} did not provide a Rust sysroot")]
    SysrootUnavailable {
        /// Exact executable selected by the caller.
        tool: PathBuf,
    },
    /// The reported sysroot does not name an accessible directory.
    #[error("reported Rust sysroot is not a directory: {path}")]
    InvalidSysroot {
        /// Exact unusable path returned by the compiler.
        path: PathBuf,
    },
}

fn resolve_executable(requested: &std::path::Path) -> Option<PathBuf> {
    let candidate = if requested.is_absolute() || requested.components().count() > 1 {
        Some(if requested.is_absolute() {
            requested.to_path_buf()
        } else {
            std::env::current_dir().ok()?.join(requested)
        })
    } else {
        let path = std::env::var_os("PATH")?;
        std::env::split_paths(&path)
            .map(|directory| directory.join(requested))
            .find(|candidate| candidate.is_file())
    }?;
    candidate.canonicalize().ok().filter(|path| path.is_file())
}

fn rustup_selection(sysroot: &std::path::Path) -> (Option<PathBuf>, Option<String>) {
    let mut previous = sysroot;
    for ancestor in sysroot.ancestors().skip(1) {
        if ancestor.file_name() == Some(std::ffi::OsStr::new("toolchains")) {
            let rustup_home = ancestor.parent().map(PathBuf::from);
            let selector = previous
                .file_name()
                .map(|name| name.to_string_lossy().into_owned());
            return (rustup_home, selector);
        }
        previous = ancestor;
    }
    (None, None)
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::{LoadError, RustToolchain};

    #[test]
    fn explicit_toolchain_rejects_relative_compiler_before_sysroot_admission() {
        let error = RustToolchain::from_paths(PathBuf::from("rustc"), PathBuf::from("/sysroot"))
            .expect_err("relative compiler must not enter host authority");
        assert!(matches!(
            error,
            LoadError::RelativeTool { tool } if tool == PathBuf::from("rustc")
        ));
    }

    #[test]
    fn authority_path_contains_only_admitted_tool_directories() {
        let root = std::env::temp_dir().join(format!("rust-authority-path-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let cargo_home = root.join("cargo-home");
        let sysroot = root.join("sysroot");
        let tool_dir = root.join("rust/bin");
        let cargo_dir = root.join("cargo/bin");
        std::fs::create_dir_all(&cargo_home).expect("cargo home fixture");
        std::fs::create_dir_all(&sysroot).expect("sysroot fixture");
        std::fs::create_dir_all(&tool_dir).expect("rustc directory fixture");
        std::fs::create_dir_all(&cargo_dir).expect("cargo directory fixture");
        let rustc = tool_dir.join("rustc");
        let cargo = cargo_dir.join("cargo");
        std::fs::write(&rustc, b"").expect("rustc fixture");
        std::fs::write(&cargo, b"").expect("cargo fixture");
        let toolchain = RustToolchain::from_paths_with_cargo(
            rustc,
            sysroot.clone(),
            cargo.clone(),
            cargo_home.clone(),
        )
        .expect("explicit Rust authority paths");
        let path = toolchain.authority_path().expect("stable admitted PATH");
        assert_eq!(
            std::env::split_paths(std::ffi::OsStr::new(&path)).collect::<Vec<_>>(),
            vec![
                cargo_home.join("bin"),
                cargo.parent().expect("cargo parent").to_path_buf(),
                tool_dir,
                sysroot.join("bin"),
            ]
        );
        let _ = std::fs::remove_dir_all(root);
    }
}
