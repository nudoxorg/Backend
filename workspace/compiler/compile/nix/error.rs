//! Concrete error taxonomy for Nix flake acquisition, static analysis, and
//! hermetic evaluation. Every variant carries structured data (paths, URLs,
//! captured detail) and preserves sources via `#[source]`. No bare `anyhow`.

use std::path::PathBuf;

use thiserror::Error;

/// A failure anywhere in the Nix producer pipeline.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum NixError {
    // ── acquisition / traversal (FlakeHub, github, registry) ──────────────
    /// A FlakeHub HTTP request failed (connect, status, or transport).
    #[error("FlakeHub request to {url} failed: {detail}")]
    FlakeHubRequest { url: String, detail: String },

    /// A FlakeHub JSON response could not be parsed into our mirror types.
    #[error("failed to parse FlakeHub response from {url}")]
    FlakeHubDecode {
        url:    String,
        #[source]
        source: serde_json::Error,
    },

    /// No release satisfied the requested version constraint.
    #[error("no FlakeHub release for {org}/{project} matching {constraint}")]
    NoMatchingRelease {
        org:        String,
        project:    String,
        constraint: String,
    },

    /// The resolved release was yanked and the request was not an exact pin.
    #[error("FlakeHub release {org}/{project}@{version} is yanked")]
    YankedRelease {
        org:     String,
        project: String,
        version: String,
    },

    /// Downloading (following the redirect chain) or writing the tarball failed.
    #[error("failed to fetch flake tarball from {url}: {detail}")]
    TarballFetch { url: String, detail: String },

    /// The downloaded tarball's SHA-256 did not match a pinned expectation.
    #[error("tarball hash mismatch for {url}: expected {expected}, got {actual}")]
    HashMismatch {
        url:      String,
        expected: String,
        actual:   String,
    },

    /// Unpacking the `.tar.gz` failed.
    #[error("failed to extract flake tarball into {dest}")]
    Extract {
        dest:   PathBuf,
        #[source]
        source: std::io::Error,
    },

    // ── input materialization (flake.lock) ────────────────────────────────
    /// `flake.lock` was present but could not be parsed as the locked node graph.
    #[error("failed to parse {path}")]
    LockParse {
        path:   PathBuf,
        #[source]
        source: serde_json::Error,
    },

    /// A locked input could not be materialized (fetch/verify/extract).
    #[error("failed to materialize locked input `{name}`")]
    MaterializeInput {
        name:   String,
        #[source]
        source: Box<NixError>,
    },

    // ── static layer (rnix) ───────────────────────────────────────────────
    /// Reading a `.nix` source file failed.
    #[error("failed to read Nix source {path}: {source}")]
    ReadSource {
        path:   PathBuf,
        #[source]
        source: std::io::Error,
    },

    /// `rnix` reported a hard parse error on a file (recoverable errors are
    /// tolerated; only a fully unusable parse surfaces here).
    #[error("failed to parse {path}: {detail}")]
    Parse { path: PathBuf, detail: String },

    // ── dynamic layer (snix-eval) ─────────────────────────────────────────
    /// Constructing or driving the snix evaluator failed at a level that
    /// aborts the whole producer (per-node eval errors degrade to documented
    /// gaps and never reach here).
    #[error("Nix evaluation failed: {detail}")]
    Eval { detail: String },

    /// The producer-wide wall-clock evaluation budget was exceeded.
    #[error("Nix evaluation timed out after {seconds}s")]
    EvalTimeout { seconds: u64 },

    // ── misc ──────────────────────────────────────────────────────────────
    /// A path escaped the hermetic root through the whitelist filesystem.
    #[error("path {path} escapes the hermetic flake root")]
    PathEscape { path: PathBuf },

    /// Generic I/O not covered by a more specific variant.
    #[error("I/O error in the Nix producer")]
    Io(#[from] std::io::Error),
}

/// Convenience alias so call sites keep the `Result<T>` shape used by the
/// other producers.
pub type Result<T> = std::result::Result<T, NixError>;
