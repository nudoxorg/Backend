//! Concrete, explicitly configured local compiler and durable-publication capability.

mod compiler;
mod config;
mod terminal;

pub use compiler::LocalCompiler;
pub use config::{
    LocalCompilerConfig, LocalCompilerControl, LocalCompilerScratch, LocalCompilerTimeout,
    LocalCompilerTimeoutError, LocalToolchainSet, LocalToolchainSetError,
    MAX_FRAGMENT_OUTPUT_BYTES, MAX_LOCAL_COMPILER_TIMEOUT, MAX_LOCAL_TOOLCHAINS,
    MAX_LOCALITY_OUTPUT_BYTES, MAX_MANIFEST_ENTRIES, MAX_MANIFEST_OUTPUT_BYTES,
    MAX_NATIVE_DIAGNOSTIC_OUTPUT_BYTES,
};
pub use terminal::{LocalCompilerOpenError, LocalCompilerPath};
