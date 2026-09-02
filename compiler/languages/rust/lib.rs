//! Provides Rust's in-process semantic authority for the compiler.
//! Loads one Cargo graph under an explicit Rust edition and extracts only HIR-backed facts.
//! Keeps rust-analyzer implementation values within a bounded, non-serialized transaction.

use std::{path::PathBuf, process::Command};

mod authority;

pub use authority::{
    ByteSpan, RustAnalysisControl, RustAuthority, RustAuthorityError, RustDeclaration,
    RustDefinition, RustMethodCall, RustProject, SemanticKind, SourceByteLimit, SourceOrigin,
};

/// Native compiler identity and sysroot accepted for one Rust authority transaction.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RustToolchain {
    /// Caller-selected compiler executable.
    pub tool: PathBuf,
    /// Sysroot reported by exactly that compiler executable.
    pub sysroot: PathBuf,
}

impl RustToolchain {
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
