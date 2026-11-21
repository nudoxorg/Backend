//! Bounded, read-only owner dependency observations.
use serde::{Deserialize, Serialize};
use std::io::{self, Read, Write};
use std::path::Path;
use std::time::{Duration, Instant};

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
    let deadline = Instant::now().checked_add(timeout).ok_or_else(|| {
        io::Error::new(io::ErrorKind::InvalidInput, "timeout exceeds clock range")
    })?;
    let path = path.as_ref();
    let mut stream = backend_replication::connect_local_timeout(path, remaining(deadline)?)?;
    stream.set_read_timeout(Some(remaining(deadline)?))?;
    stream.set_write_timeout(Some(remaining(deadline)?))?;
    backend_replication::AuthenticatedLocalPeer::authenticate(&stream, path)
        .map_err(|error| io::Error::other(format!("owner authentication: {error}")))?;
    remaining(deadline)?;
    let mut request = Vec::with_capacity(4 + REQUEST.len());
    request.extend_from_slice(&(REQUEST.len() as u32).to_be_bytes());
    request.extend_from_slice(REQUEST);
    write_until(&mut stream, &request, deadline)?;
    let mut length = [0; 4];
    read_until(&mut stream, &mut length, deadline)?;
    let length = u32::from_be_bytes(length) as usize;
    if length > 8192 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "metrics response exceeds bound",
        ));
    }
    let mut body = vec![0; length];
    read_until(&mut stream, &mut body, deadline)?;
    remaining(deadline)?;
    let snapshot = serde_json::from_slice(&body)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    remaining(deadline)?;
    Ok(snapshot)
}

#[cfg(any(unix, windows))]
fn remaining(deadline: Instant) -> io::Result<Duration> {
    deadline
        .checked_duration_since(Instant::now())
        .filter(|d| !d.is_zero())
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::TimedOut,
                "dependency snapshot deadline elapsed",
            )
        })
}

#[cfg(any(unix, windows))]
fn read_until(
    stream: &mut backend_replication::LocalStream,
    mut buf: &mut [u8],
    deadline: Instant,
) -> io::Result<()> {
    while !buf.is_empty() {
        stream.set_read_timeout(Some(remaining(deadline)?))?;
        match stream.read(buf) {
            Ok(0) => {
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "truncated dependency snapshot",
                ));
            }
            Ok(n) => buf = &mut buf[n..],
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(error) => return Err(error),
        }
    }
    Ok(())
}

#[cfg(any(unix, windows))]
fn write_until(
    stream: &mut backend_replication::LocalStream,
    mut buf: &[u8],
    deadline: Instant,
) -> io::Result<()> {
    while !buf.is_empty() {
        stream.set_write_timeout(Some(remaining(deadline)?))?;
        match stream.write(buf) {
            Ok(0) => {
                return Err(io::Error::new(
                    io::ErrorKind::WriteZero,
                    "incomplete dependency request",
                ));
            }
            Ok(n) => buf = &buf[n..],
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(error) => return Err(error),
        }
    }
    Ok(())
}
