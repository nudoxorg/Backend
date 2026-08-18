//! `GoProducer` — oracle invocation and lowering, and its
//! `crate::Producer` implementation.
//!
//! `GoProducer` implements [`crate::Producer`] below (see the `impl
//! Producer for GoProducer` block): `invoke` runs the compiled
//! `nudox-go-oracle` subprocess via [`crate::oracle::run_json`] and
//! `lower` feeds its deserialized output through [`crate::go::lower::lower_into`].
//! Registering it with `ProducerRegistry` (`crates/nudox-store/src/source/producer.rs`)
//! is out of scope for this crate — see that file's `ProducerRegistry::with_all_available`
//! doc comment for the current registration state.
//!
//! The inherent methods below (`lower_bytes`, `invoke_oracle`, `produce`)
//! predate the trait impl and stay for the tests and call sites that use
//! them directly without going through `crate::produce`; they run
//! the identical `invoke -> lower` shape.
//!
//! ## Oracle invocation contract
//!
//! The oracle binary lives at
//! `workspace/compiler/languages/oracle/go/` (a Go module).  It must be compiled
//! separately (`go build -o nudox-go-oracle ./...` from that directory) before
//! being usable.  The expected command contract:
//!
//! ```text
//! nudox-go-oracle <module-root-dir>
//! ```
//!
//! stdout: the oracle JSON document (`oracle::Output`).
//! stderr: diagnostics (non-fatal); logged as warnings.
//! exit code: 0 on success, non-zero on fatal error.
//!
//! Both the `Producer::invoke` impl and the inherent `invoke_oracle` method
//! locate the binary the same way: the `NUDOX_GO_ORACLE_BIN` environment
//! variable if set, else the bare name `nudox-go-oracle` resolved against
//! `PATH`.

use crate::{PackageSource, Producer, ProducerError, ProducerId};
use nudox_ir::{
    body::Language,
    change::PackageLineageId,
    lower::Lowering,
    package::{IrPackage, PackageId},
};

use crate::go::{
    error::{self, Result},
    lower::{GoId, lower_into, lower_output},
    oracle,
};

/// The environment variable that points at the Go oracle binary.
///
/// Named once and used both to *read* the override and to *report* it in a
/// spawn failure, so the message can never name a variable the lookup does not
/// actually consult — which would be worse than naming none.
pub const ORACLE_BIN_ENV: &str = "NUDOX_GO_ORACLE_BIN";

/// Resolve the oracle binary: [`ORACLE_BIN_ENV`] if set, else the bare
/// name `nudox-go-oracle` resolved against `PATH`. Shared by `Producer::invoke`
/// and the inherent `invoke_oracle` so the two paths cannot drift apart.
fn oracle_binary() -> String {
    std::env::var(ORACLE_BIN_ENV).unwrap_or_else(|_| "nudox-go-oracle".to_string())
}

/// Identifies the Go oracle producer in the registry.
pub const PRODUCER_ID: &str = "go-oracle/2";

/// The Go module producer — oracle subprocess → IR lowering.
#[derive(Debug, Default, Clone, Copy)]
pub struct GoProducer;

impl GoProducer {
    /// Deserialize raw oracle JSON bytes and lower them into an [`IrPackage`].
    ///
    /// This is the pure-Rust, test-friendly entry point: it does NOT invoke the
    /// oracle subprocess.  The subprocess bridge lives in `invoke_oracle`.
    pub fn lower_bytes(
        &self,
        oracle_json: &[u8],
        pkg_id: PackageId,
        lineage: &PackageLineageId,
    ) -> Result<IrPackage<GoId>> {
        let output: oracle::Output = serde_json::from_slice(oracle_json)?;
        for err in &output.errors {
            // In production code this would go through tracing::warn!.
            eprintln!("[go-oracle] diagnostic: {err}");
        }
        lower_output(&output, pkg_id, lineage)
    }

    /// Run the oracle binary and deserialize its output.
    ///
    /// This is the pre-trait entry point, kept for direct callers that want
    /// `crate::go::error::Error` rather than `crate::ProducerError` (the
    /// error type `Producer::invoke` below must return). Both resolve the
    /// binary via the shared [`oracle_binary`] helper, so they cannot name
    /// two different binaries.
    pub fn invoke_oracle(&self, module_root: &std::path::Path) -> Result<oracle::Output> {
        let oracle_bin = oracle_binary();

        let output = std::process::Command::new(&oracle_bin)
            .arg(module_root)
            .output()
            .map_err(error::Error::Oracle)?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
            return Err(error::Error::Oracle(std::io::Error::other(format!(
                "oracle exited with status {}: {stderr}",
                output.status
            ))));
        }

        let oracle: oracle::Output =
            serde_json::from_slice(&output.stdout).map_err(error::Error::Json)?;
        Ok(oracle)
    }

    /// Full pipeline: invoke oracle then lower.
    pub fn produce(
        &self,
        module_root: &std::path::Path,
        pkg_id: PackageId,
        lineage: &PackageLineageId,
    ) -> Result<IrPackage<GoId>> {
        let output = self.invoke_oracle(module_root)?;
        for err in &output.errors {
            eprintln!("[go-oracle] diagnostic: {err}");
        }
        lower_output(&output, pkg_id, lineage)
    }
}

// ---------------------------------------------------------------------------
// Producer
// ---------------------------------------------------------------------------

impl Producer for GoProducer {
    type Id = GoId;
    type Oracle = oracle::Output;

    const ID: ProducerId = ProducerId(PRODUCER_ID);
    const LANGUAGE: Language = Language::Go;

    fn invoke(&self, src: &PackageSource) -> std::result::Result<oracle::Output, ProducerError> {
        let oracle_bin = oracle_binary();
        // `ORACLE_BIN_ENV` is passed so a host that does not have the oracle —
        // the normal state of a packaged `lindsey.app`, which does not ship it
        // — is told which variable points at one, instead of only that a
        // spawn failed.
        let output: oracle::Output = crate::oracle::run_json_with_override(
            Self::ID.0,
            oracle_bin,
            [src.root()],
            Some(ORACLE_BIN_ENV),
        )?;

        // A binary older than this build under-reports silently: every field
        // is `#[serde(default)]`, so its missing `references`/`implements`
        // arrive as empty lists and `refs`/`subtypes` answer "nothing here"
        // for the whole language. Failing loudly is the only way that reads as
        // a stale binary rather than an empty package — see
        // `oracle::Output::staleness`.
        if let Some(stale) = output.staleness() {
            return Err(ProducerError::OracleExit {
                command: format!("{} {}", Self::ID.0, oracle_binary()),
                code: "stale".to_owned(),
                stderr: stale.to_string(),
            });
        }

        Ok(output)
    }

    fn lower(
        &self,
        oracle: &oracle::Output,
        out: &mut Lowering<GoId>,
    ) -> std::result::Result<(), ProducerError> {
        for err in &oracle.errors {
            // In production code this would go through tracing::warn!.
            eprintln!("[go-oracle] diagnostic: {err}");
        }
        // `lower_into` cannot name a single failing package up front (it
        // walks every package in the oracle output before any error can
        // surface); the module path is the closest thing to a package label
        // this crate's `Error` carries at this boundary.
        let package = oracle
            .module
            .as_ref()
            .map(|m| m.path.clone())
            .unwrap_or_default();
        lower_into(oracle, out).map_err(|e| ProducerError::LoweringFailed {
            package,
            source: Box::new(e),
        })
    }
}
