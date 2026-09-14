//! Typed terminals for local-host path, authority, resource, and worker admission.

use std::{io, path::Path, str};

use backend_frontend_go::legacy::GoOracleConfigurationError;
use backend_frontend_java::legacy::harness::HarnessError;
use backend_frontend_python::legacy::PyreflyExecutableError;
use backend_frontend_rust::legacy::LoadError as RustLoadError;
use backend_frontend_typescript::legacy::TypeScriptCheckerProgramError;
use backend_semantic::vocabulary::NativeTool;
use backend_store::journal::PublicationLimitError;
use thiserror::Error;

use super::{LocalHostDirectory, LocalHostPathKind, LocalHostPathRole, LocalHostVariable};
use crate::application::{
    LocalCompilerRuntimeConfigurationError, LocalCompilerRuntimeOpenError,
    LocalCompilerScratchError, LocalCompilerTimeoutError, LocalPackageRootError,
    ToolchainProbeError, ToolchainProbeLimitError,
};

/// Exact failure while admitting and starting a process-local compiler owner.
#[derive(Debug, Error)]
pub enum LocalCompilerHostError {
    /// No platform data-root authority was available.
    #[error("no durable local compiler data root is configured for this platform")]
    DataRootUnavailable,
    /// A platform-required path variable was absent.
    #[error("required local host path {variable:?} is not configured")]
    RequiredEnvironmentPath {
        /// Missing closed variable.
        variable: LocalHostVariable,
    },
    /// A configured environment path was relative.
    #[error("configured local host path {variable:?} is relative: {path:?}")]
    RelativeEnvironmentPath {
        /// Closed variable carrying the path.
        variable: LocalHostVariable,
        /// Exact rejected path.
        path: Box<Path>,
    },
    /// A configured path could not be inspected.
    #[error("configured {role:?} path from {variable:?} is unusable: {path:?}")]
    ConfiguredPath {
        /// Closed authority role.
        role: LocalHostPathRole,
        /// Closed source variable.
        variable: LocalHostVariable,
        /// Exact configured path.
        path: Box<Path>,
        /// Original filesystem cause.
        #[source]
        source: io::Error,
    },
    /// A configured path named the wrong object kind.
    #[error("configured {role:?} path from {variable:?} is not a {expected:?}: {path:?}")]
    ConfiguredPathKind {
        /// Closed authority role.
        role: LocalHostPathRole,
        /// Closed source variable.
        variable: LocalHostVariable,
        /// Exact configured path.
        path: Box<Path>,
        /// Required object kind.
        expected: LocalHostPathKind,
    },
    /// An existing selected path could not be canonicalized.
    #[error("selected {role:?} path could not be canonicalized: {path:?}")]
    Canonicalize {
        /// Closed authority role.
        role: LocalHostPathRole,
        /// Exact selected path.
        path: Box<Path>,
        /// Original filesystem cause.
        #[source]
        source: io::Error,
    },
    /// A named runtime storage directory could not be created.
    #[error("could not create local compiler {directory:?} directory at {path:?}")]
    CreateDirectory {
        /// Closed storage role.
        directory: LocalHostDirectory,
        /// Exact selected path.
        path: Box<Path>,
        /// Original filesystem cause.
        #[source]
        source: io::Error,
    },
    /// Every bounded process-unique native-work name already existed.
    #[error("could not reserve native work below {parent:?} in {attempts} attempts")]
    NativeWorkCapacity {
        /// Exact parent directory.
        parent: Box<Path>,
        /// Fixed attempted-name count.
        attempts: u64,
    },
    /// One selected executable could not establish an exact version identity.
    #[error("could not probe {tool:?} executable at {executable:?}")]
    ToolchainProbe {
        /// Closed native tool.
        tool: NativeTool,
        /// Exact canonical executable.
        executable: Box<Path>,
        /// Complete bounded probe cause.
        #[source]
        source: ToolchainProbeError,
    },
    /// Rust's selected compiler returned non-UTF-8 sysroot bytes.
    #[error("Rust compiler returned a non-UTF-8 sysroot")]
    RustSysrootEncoding {
        /// Complete bounded output bytes.
        output: Box<[u8]>,
        /// Exact decoding fault.
        #[source]
        source: str::Utf8Error,
    },
    /// Rust's selected compiler returned no sysroot path.
    #[error("Rust compiler at {compiler:?} returned an empty sysroot")]
    RustSysrootEmpty {
        /// Exact selected compiler.
        compiler: Box<Path>,
    },
    /// Rust's selected compiler returned a relative sysroot path.
    #[error("Rust compiler at {compiler:?} returned relative sysroot {sysroot:?}")]
    RustSysrootRelative {
        /// Exact selected compiler.
        compiler: Box<Path>,
        /// Exact rejected output path.
        sysroot: Box<Path>,
    },
    /// Bounded native probe policy was invalid.
    #[error(transparent)]
    ProbeLimits(#[from] ToolchainProbeLimitError),
    /// A raw Rust sysroot query failed under bounded process control.
    #[error(transparent)]
    RustSysrootProbe(#[from] ToolchainProbeError),
    /// Rust authority rejected the paired compiler/sysroot.
    #[error(transparent)]
    RustAuthority(#[from] RustLoadError),
    /// TypeScript authority rejected its explicit producer.
    #[error(transparent)]
    TypeScriptAuthority(#[from] TypeScriptCheckerProgramError),
    /// Pyrefly authority rejected its explicit executable.
    #[error(transparent)]
    PythonAuthority(#[from] PyreflyExecutableError),
    /// Go authority rejected its explicit producer.
    #[error(transparent)]
    GoAuthority(#[from] GoOracleConfigurationError),
    /// Java authority rejected its explicit JDK.
    #[error(transparent)]
    JavaAuthority(#[from] HarnessError),
    /// A package-root row was invalid.
    #[error(transparent)]
    PackageRoot(#[from] LocalPackageRootError),
    /// Runtime storage paths were invalid.
    #[error(transparent)]
    RuntimePath(#[from] crate::application::LocalCompilerOpenError),
    /// Compiler timeout policy was invalid.
    #[error(transparent)]
    Timeout(#[from] LocalCompilerTimeoutError),
    /// Publication resource policy was invalid.
    #[error(transparent)]
    PublicationLimits(#[from] PublicationLimitError),
    /// Compiler scratch allocation or width validation failed.
    #[error(transparent)]
    Scratch(#[from] LocalCompilerScratchError),
    /// Canonical runtime tables were invalid.
    #[error(transparent)]
    RuntimeConfiguration(#[from] LocalCompilerRuntimeConfigurationError),
    /// The compiler worker or durable publisher could not start.
    #[error(transparent)]
    RuntimeOpen(#[from] LocalCompilerRuntimeOpenError),
}
