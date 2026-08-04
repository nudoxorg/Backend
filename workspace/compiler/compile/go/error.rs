use std::path::PathBuf;
use std::process::ExitStatus;

use thiserror::Error;

/// Concrete error taxonomy for all Go module discovery, oracle materialization,
/// execution, and lowering failures. Every variant is explicit, carries
/// structured data (paths, captured outputs), and preserves sources via
/// `#[source]`. No bare `anyhow` and no `format!` used to manufacture
/// error payloads at construction sites.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum GoError {
    /// No `go.mod` discovered at or above the starting path.
    #[error("no go.mod found at or above {start}")]
    NoGoMod { start: PathBuf },

    /// I/O failure while reading a `go.mod` file.
    #[error("failed to read go.mod at {path}: {source}")]
    ReadGoMod {
        path:   PathBuf,
        #[source]
        source: std::io::Error,
    },

    /// `go.mod` lacked the required `module` directive.
    #[error("{path} has no `module` directive")]
    NoModuleDirective { path: PathBuf },

    /// `canonicalize` failed while resolving the module root for the oracle.
    #[error("failed to resolve module root {root}: {source}")]
    ResolveModuleRoot {
        root:   PathBuf,
        #[source]
        source: std::io::Error,
    },

    /// Resolving the Buck2-built oracle binary shipped as a resource
    /// alongside this executable failed.
    #[error("resolving the buck2-built go oracle failed — was this binary built by buck2?: {source}")]
    ResourceNotFound {
        #[source]
        source: std::io::Error,
    },

    /// Spawning the prebuilt oracle binary failed.
    #[error("spawning the go oracle failed: {source}")]
    SpawnOracle {
        #[source]
        source: std::io::Error,
    },

    /// The `go run` of the oracle exited non-zero. Full stderr captured.
    #[error("go oracle failed on {target} ({status}): {stderr}")]
    OracleExecution {
        target: PathBuf,
        status: String,
        stderr: String,
    },

    /// `serde_json` failed to parse the oracle's stdout. Captured stdout
    /// included for diagnostics (may be partial/truncated by the oracle).
    #[error("parsing oracle JSON output failed")]
    OracleOutputParse {
        #[source]
        source: serde_json::Error,
        stdout: Option<String>,
    },

    /// Wrapper that preserves the high-level "running oracle over <root>"
    /// context from `GoContext::load` while chaining the concrete cause.
    #[error("running the Go oracle over {root}")]
    RunOracle {
        root:   PathBuf,
        #[source]
        source: Box<GoError>,
    },

    /// Version request string could not be parsed as `latest`, semver, or prefix.
    #[error("unparseable Go version request `{requested}`")]
    UnparseableVersionRequest { requested: String },

    /// Oracle JSON schema violation detected by the defensive `validate` pass
    /// (missing required payload for a type kind).
    #[error("oracle schema error: {detail}")]
    OracleSchema { detail: &'static str },
}

/// Convenience alias so call sites keep the old `Result<T>` shape.
pub type Result<T> = std::result::Result<T, GoError>;
