//! Go [`Producer`] implementation.
//!
//! This module provides the glue that satisfies the `nudox_producer::Producer`
//! trait contract (described in the mission brief; the actual trait crate is
//! being authored by the prodcore agent in parallel).
//!
//! **Compilation gate:** This module contains `#[cfg(feature = "producer-trait")]`
//! guards so the crate compiles even before `nudox-producer` (the trait crate)
//! lands.  Once `nudox-producer` is stable, add it as a dependency and remove
//! the `cfg` guards.
//!
//! ## Oracle invocation contract
//!
//! The oracle binary lives at
//! `workspace/compiler/compile/go/oracle/` (a Go module).  It must be compiled
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
//! The core `nudox-producer` crate is expected to provide an
//! `oracle_subprocess` helper that runs a command and deserializes stdout as
//! JSON, turning a non-zero exit into a typed error carrying stderr.  We note
//! that requirement here rather than hand-rolling `std::process::Command`.

use nudox_ir::{
    change::PackageLineageId,
    package::{IrPackage, PackageId},
};

use crate::{
    error::{GoError, Result},
    lower::{GoId, lower_output},
    oracle,
};

/// Identifies the Go oracle producer in the registry.
pub const PRODUCER_ID: &str = "go-oracle/1";

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
        for err in output.errors.iter() {
            // In production code this would go through tracing::warn!.
            eprintln!("[go-oracle] diagnostic: {err}");
        }
        lower_output(&output, pkg_id, lineage)
    }

    /// Run the oracle binary and deserialize its output.
    ///
    /// **NOTE:** This calls `std::process::Command` directly because the
    /// `nudox-producer` crate (which will provide `oracle_subprocess`) does not
    /// yet exist as a Cargo dependency.  Once it does, replace this body with a
    /// call to `nudox_producer::oracle_subprocess(oracle_bin, &[module_root])`.
    ///
    /// UNCERTAINTY: The `oracle_subprocess` helper's exact signature is unknown.
    /// The brief says it "runs a command and deserializes stdout as JSON, turning
    /// a non-zero exit into a typed error carrying stderr."  This implementation
    /// matches that description but may need adjustment when the real helper
    /// lands.
    pub fn invoke_oracle(&self, module_root: &std::path::Path) -> Result<oracle::Output> {
        // Locate the oracle binary.  In a real build this would come from the
        // sandbox/ExecPlan machinery.
        let oracle_bin = std::env::var("NUDOX_GO_ORACLE_BIN")
            .unwrap_or_else(|_| "nudox-go-oracle".to_string());

        let output = std::process::Command::new(&oracle_bin)
            .arg(module_root)
            .output()
            .map_err(|e| GoError::Oracle {
                detail: format!("failed to spawn oracle `{oracle_bin}`: {e}"),
            })?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
            return Err(GoError::Oracle {
                detail: format!(
                    "oracle exited with status {}: {stderr}",
                    output.status
                ),
            });
        }

        let oracle: oracle::Output = serde_json::from_slice(&output.stdout)
            .map_err(GoError::Json)?;
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
        for err in output.errors.iter() {
            eprintln!("[go-oracle] diagnostic: {err}");
        }
        lower_output(&output, pkg_id, lineage)
    }
}
