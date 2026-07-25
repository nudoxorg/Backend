//! Oracle subprocess helper.
//!
//! Three of the seven language producers (Go, C#, Java) are structured as
//! external toolchain processes that write a JSON document to stdout.  This
//! module provides the single shared helper they all use: run a command,
//! capture stdout/stderr, treat a non-zero exit as a typed error, and
//! deserialize stdout as JSON into `T`.
//!
//! # Usage pattern
//!
//! ```no_run
//! use nudox_producer::oracle;
//! use serde::Deserialize;
//!
//! #[derive(Deserialize)]
//! struct GoOutput { /* … */ }
//!
//! let output: GoOutput = oracle::run_json("go-oracle/1", "go", &["run", "./oracle", "/pkg/root"])?;
//! # Ok::<(), nudox_producer::ProducerError>(())
//! ```
//!
//! # Memory
//!
//! `run_json` captures the full stdout into a `Vec<u8>` (one allocation),
//! then hands the slice to `serde_json::from_slice` so the deserialized
//! types can borrow `&str` fields from the heap buffer if needed.  The
//! `Vec<u8>` is dropped after deserialization; the returned `T` is the only
//! survivor.

use std::{
    ffi::OsStr,
    process::{Command, Stdio},
};

use serde::de::DeserializeOwned;

use crate::ProducerError;

/// Run an oracle command, capture stdout/stderr, and deserialize stdout as JSON.
///
/// * `producer_label` — the [`ProducerId`] string; embedded in error messages
///   so a failure trace names the producer.
/// * `program` — the binary to run.
/// * `args` — the arguments; each element is passed as a separate `arg`.
///
/// On success returns `T` deserialized from the command's stdout.
///
/// # Errors
///
/// * [`ProducerError::OracleSpawn`] — the process could not be started.
/// * [`ProducerError::OracleExit`] — the process exited with a non-zero status.
///   The captured stderr is included so the caller can log it.
/// * [`ProducerError::Decode`] — the process succeeded but stdout could not be
///   deserialized as `T`.
///
/// [`ProducerId`]: crate::ProducerId
pub fn run_json<T, P, A, I>(
    producer_label: &str,
    program: P,
    args: I,
) -> Result<T, ProducerError>
where
    T: DeserializeOwned,
    P: AsRef<OsStr>,
    A: AsRef<OsStr>,
    I: IntoIterator<Item = A>,
{
    let program_ref = program.as_ref();

    // Build the command label for diagnostics before consuming the iterator.
    let label = format!(
        "{} {}",
        producer_label,
        program_ref.to_string_lossy()
    );

    let output = Command::new(program_ref)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .map_err(|e| ProducerError::OracleSpawn {
            command: label.clone(),
            reason: e.to_string(),
        })?;

    if !output.status.success() {
        let code = output
            .status
            .code()
            .map(|c| c.to_string())
            .unwrap_or_else(|| "signal".to_owned());
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_owned();
        return Err(ProducerError::OracleExit {
            command: label,
            code,
            stderr,
        });
    }

    serde_json::from_slice::<T>(&output.stdout).map_err(|e| ProducerError::Decode {
        // `label` is still available here (not moved into OracleExit above
        // because that branch returns early).
        package: label,
        reason: e.to_string(),
    })
}
