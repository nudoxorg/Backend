//! Provides Rust's in-process semantic authority for the compiler.
//! Loads one Cargo graph under an explicit Rust edition and extracts only HIR-backed facts.
//! Keeps rust-analyzer implementation values within a bounded, non-serialized transaction.

use std::{path::PathBuf, process::Command};

mod authority;
mod purl;

pub use self::authority::{
    ByteSpan, ModuleDeclaration, RustAnalysisControl, RustAuthority, RustAuthorityError,
    RustDeclaration, RustDefinition, RustFeatureControl, RustFieldAccess, RustMethodCall,
    RustProject, SemanticKind, SourceByteLimit, SourceOrigin,
};
pub use self::purl::{RustLocatedPackage, RustPackageUrl, RustPurlError};

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

/// Native compiler identity and sysroot accepted for one Rust authority transaction.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RustToolchain {
    /// Caller-selected compiler executable.
    pub tool: PathBuf,
    /// Sysroot reported by exactly that compiler executable.
    pub sysroot: PathBuf,
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
        Ok(Self { tool, sysroot })
    }

    /// Discovers the sysroot of one caller-selected Rust compiler.
    ///
    /// # Errors
    ///
    /// Returns [`LoadError`] when the executable cannot report a usable sysroot.
    pub fn discover(tool: impl Into<PathBuf>) -> Result<Self, LoadError> {
        let tool = tool.into();
        let output = Command::new(&tool)
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
        Ok(Self { tool, sysroot })
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
}
