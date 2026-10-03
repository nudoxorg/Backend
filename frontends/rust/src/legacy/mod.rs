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
    ByteSpan, CargoMetadataIncompleteCause, CargoMetadataPreflightError,
    MAX_RUST_WORKSPACE_SESSION_SOURCES, ModuleDeclaration, RustActiveHirRootInventory,
    RustAnalysisControl, RustAuthority, RustAuthorityError, RustCargoMetadataPolicy,
    RustDeclaration, RustDefinition, RustFeatureControl, RustFieldAccess, RustInferredExpression,
    RustMethodCall, RustProject, RustReexport, RustSourceScope, RustWorkspace,
    RustWorkspaceEditorBufferObserver, RustWorkspaceFile, RustWorkspaceFilesystemOperation,
    RustWorkspaceFilesystemOutcome, RustWorkspaceReadFrontierComplete,
    RustWorkspaceReadFrontierGap, RustWorkspaceReadFrontierGaps, RustWorkspaceReadFrontierObserver,
    RustWorkspaceReadFrontierSealReport, RustWorkspaceReadFrontierSummary, RustWorkspaceSessionKey,
    RustWorkspaceSessionLane, RustWorkspaceSessionLease, RustWorkspaceSessionStats, SemanticKind,
    SourceByteLimit, SourceOrigin,
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

/// Resolves a requested tool spelling to the file a child process should execute.
///
/// An absolute or multi-component spelling is used as given; a bare name is searched in `PATH`.
/// A bare name also matches `name` plus the host executable suffix, because Windows installs
/// `rustc.exe` and a process search that adds `.exe` implicitly does not exist here.
fn resolve_executable(requested: &std::path::Path) -> Option<PathBuf> {
    resolve_executable_in(requested, std::env::var_os("PATH").as_deref())
}

/// [`resolve_executable`] against an explicit search path; `None` is an unset `PATH`, which only
/// a bare name needs.
fn resolve_executable_in(
    requested: &std::path::Path,
    search_path: Option<&std::ffi::OsStr>,
) -> Option<PathBuf> {
    let candidate = if requested.is_absolute() || requested.components().count() > 1 {
        Some(if requested.is_absolute() {
            requested.to_path_buf()
        } else {
            std::env::current_dir().ok()?.join(requested)
        })
    } else {
        std::env::split_paths(search_path?)
            .flat_map(|directory| {
                let bare = directory.join(requested);
                let suffixed = (!std::env::consts::EXE_SUFFIX.is_empty()
                    && requested.extension().is_none())
                .then(|| {
                    let mut name = requested.as_os_str().to_owned();
                    name.push(std::env::consts::EXE_SUFFIX);
                    directory.join(name)
                });
                [Some(bare), suffixed]
            })
            .flatten()
            .find(|candidate| candidate.is_file())
    }?;
    canonical_executable(&candidate)
        .ok()
        .filter(|path| path.is_file())
}

/// Resolves `path` to the real file behind it, except for a rustup proxy.
///
/// Symbolic links are resolved so the identity of a tool names a real file. The one exception
/// is rustup's: `rustc`, `cargo`, `rustdoc` and the other proxies are links to a single
/// multi-call `rustup` binary that decides what to do from the name it was started under.
/// Resolving such a link to its target would start the toolchain manager itself, which answers
/// `--print sysroot` with "unexpected argument" and `--version` with its own version. The proxy
/// therefore keeps its own file name inside its canonical directory.
///
/// # Errors
///
/// Returns the operating-system error when `path` or its directory cannot be resolved.
pub fn canonical_executable(path: &std::path::Path) -> std::io::Result<PathBuf> {
    let canonical = path.canonicalize()?;
    let link_name = path.file_name();
    let is_proxy = is_rustup_binary(&canonical)
        && link_name.is_some_and(|name| !is_rustup_binary(std::path::Path::new(name)));
    let (Some(name), true) = (link_name, is_proxy) else {
        return Ok(canonical);
    };
    let directory = path
        .parent()
        .filter(|directory| !directory.as_os_str().is_empty())
        .unwrap_or_else(|| std::path::Path::new("."))
        .canonicalize()?;
    Ok(directory.join(name))
}

/// Whether `path` names the rustup binary itself, by file stem.
fn is_rustup_binary(path: &std::path::Path) -> bool {
    path.file_stem()
        .is_some_and(|stem| stem.eq_ignore_ascii_case("rustup"))
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

    /// A temporary directory whose children are removed on drop.
    struct Scratch(PathBuf);

    impl Scratch {
        fn new(label: &str) -> Self {
            let path = std::env::temp_dir().join(format!(
                "nudox-rust-executable-{label}-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map_or(0, |elapsed| elapsed.as_nanos())
            ));
            std::fs::create_dir_all(&path).expect("scratch directory");
            Self(path.canonicalize().expect("resolve scratch directory"))
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// Creates a file symlink. Windows needs Developer Mode or the symbolic-link privilege, and
    /// a refusal fails the test with the operating-system error rather than skipping it.
    fn symlink_file(original: &std::path::Path, link: &std::path::Path) -> std::io::Result<()> {
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(original, link)
        }
        #[cfg(windows)]
        {
            std::os::windows::fs::symlink_file(original, link)
        }
    }

    fn named(stem: &str) -> String {
        format!("{stem}{}", std::env::consts::EXE_SUFFIX)
    }

    /// The failure that stopped the Windows owner: rustup installs `rustc` as a link to its
    /// multi-call binary, and resolving the link ran `rustup --print sysroot`.
    #[test]
    fn a_rustup_proxy_keeps_its_own_name_in_the_canonical_directory() {
        let scratch = Scratch::new("proxy");
        let rustup = scratch.0.join(named("rustup"));
        std::fs::write(&rustup, b"multi-call binary").expect("rustup fixture");
        let proxy = scratch.0.join(named("rustc"));
        symlink_file(&rustup, &proxy).expect("proxy symlink");

        let resolved = super::canonical_executable(&proxy).expect("resolve the proxy");

        assert_eq!(resolved, scratch.0.join(named("rustc")));
        assert!(resolved.is_file(), "the proxy name still opens the binary");
    }

    #[test]
    fn rustup_itself_resolves_to_its_canonical_file() {
        let scratch = Scratch::new("rustup-itself");
        let rustup = scratch.0.join(named("rustup"));
        std::fs::write(&rustup, b"multi-call binary").expect("rustup fixture");
        let alias = scratch.0.join(named("rustup-alias"));
        symlink_file(&rustup, &alias).expect("alias symlink");

        assert_eq!(
            super::canonical_executable(&rustup).expect("resolve rustup"),
            rustup.canonicalize().expect("canonical rustup")
        );
        // A differently named link to rustup is a proxy too, whatever its name.
        assert_eq!(
            super::canonical_executable(&alias).expect("resolve alias"),
            alias
        );
    }

    #[test]
    fn any_other_link_resolves_to_its_target() {
        let scratch = Scratch::new("other-link");
        let target = scratch.0.join(named("compiler-18"));
        std::fs::write(&target, b"compiler").expect("target fixture");
        let link = scratch.0.join(named("compiler"));
        symlink_file(&target, &link).expect("link");

        assert_eq!(
            super::canonical_executable(&link).expect("resolve the link"),
            target.canonicalize().expect("canonical target")
        );
    }

    #[test]
    fn a_regular_file_resolves_to_itself_and_a_missing_one_is_an_error() {
        let scratch = Scratch::new("plain");
        let tool = scratch.0.join(named("rustc"));
        std::fs::write(&tool, b"compiler").expect("tool fixture");

        assert_eq!(
            super::canonical_executable(&tool).expect("resolve the file"),
            tool.canonicalize().expect("canonical file")
        );
        assert!(super::canonical_executable(&scratch.0.join(named("absent"))).is_err());
    }

    /// A bare name is found through `PATH` with the host executable suffix.
    #[test]
    fn a_bare_name_is_found_with_the_host_executable_suffix() {
        let scratch = Scratch::new("path-search");
        std::fs::write(scratch.0.join(named("nudox-fixture-tool")), b"tool").expect("tool");
        let found = super::resolve_executable_in(
            std::path::Path::new("nudox-fixture-tool"),
            Some(&std::env::join_paths([scratch.0.as_path()]).expect("search path")),
        )
        .expect("the suffixed file is found");
        assert_eq!(found, scratch.0.join(named("nudox-fixture-tool")));
    }

    /// An explicit path never consults `PATH`; only a bare name needs one.
    #[test]
    fn an_unset_path_refuses_a_bare_name_but_not_an_explicit_path() {
        let scratch = Scratch::new("unset-path");
        let tool = scratch.0.join(named("nudox-fixture-tool"));
        std::fs::write(&tool, b"tool").expect("tool");
        assert_eq!(
            super::resolve_executable_in(&tool, None),
            Some(tool.canonicalize().expect("canonical tool"))
        );
        assert_eq!(
            super::resolve_executable_in(std::path::Path::new("nudox-fixture-tool"), None),
            None
        );
    }
}
