//! Bounded, read-only owner dependency observations.
use serde::{Deserialize, Serialize};
use std::io::{self, Read, Write};
use std::path::Path;
use std::time::Duration;

/// Exact independently versioned request, outside the library DTO grammar.
pub const REQUEST: &[u8] = b"LDM1\x01";
/// Cumulative histogram bounds in seconds; the final bucket is +Inf.
pub const BOUNDS_SECONDS: [f64; 7] = [0.001, 0.005, 0.01, 0.05, 0.1, 0.5, 1.0];
/// One outcome's completed operations and cumulative latency histogram.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Outcome {
    /// Number of completed calls.
    pub count: u64,
    /// Sum of elapsed nanoseconds for completed calls.
    pub duration_nanos: u64,
    /// Cumulative counts at `BOUNDS_SECONDS` followed by positive infinity.
    pub buckets: [u64; 8],
}
/// Counts and latency for both outcomes of an operation.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Operation {
    /// Calls that returned success.
    pub success: Outcome,
    /// Calls that returned an error.
    pub error: Outcome,
}
/// The initialized view journal's fixed operation set.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Snapshot {
    /// Recovery of the selected view journal.
    pub recover: Operation,
    /// Journal frame append and durability.
    pub append: Operation,
    /// Snapshot replacement and durability.
    pub replace: Operation,
}

/// Reads a snapshot from an authenticated local owner. `None` means this owner
/// has no initialized view journal; transport errors are never converted to zero.
///
/// # Errors
/// Returns an error if the owner cannot be authenticated, contacted, or decoded.
#[cfg(any(unix, windows))]
pub fn read(path: impl AsRef<Path>, timeout: Duration) -> io::Result<Option<Snapshot>> {
    if timeout.is_zero() {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "zero timeout"));
    }
    let path = path.as_ref();
    let mut stream = backend_replication::connect_local_timeout(path, timeout)?;
    stream.set_read_timeout(Some(timeout))?;
    stream.set_write_timeout(Some(timeout))?;
    backend_replication::AuthenticatedLocalPeer::authenticate(&stream, path)
        .map_err(|error| io::Error::other(format!("owner authentication: {error}")))?;
    stream.write_all(&(REQUEST.len() as u32).to_be_bytes())?;
    stream.write_all(REQUEST)?;
    let mut length = [0; 4];
    stream.read_exact(&mut length)?;
    let length = u32::from_be_bytes(length) as usize;
    if length > 8192 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "metrics response exceeds bound",
        ));
    }
    let mut body = vec![0; length];
    stream.read_exact(&mut body)?;
    serde_json::from_slice(&body).map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))
}
