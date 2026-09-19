//! Owner-side worker connection and authenticated transport setup.

use super::{AsyncWorkerTransport, Duration, ProcessError, TransportLimits};
use std::net::{SocketAddr, TcpStream};
#[cfg(any(unix, windows))]
use backend_engine::LocalStream as UnixStream;

/// Connects to a configured worker and completes the transport handshake.
///
/// The worker capability manifest is exchanged only after the stream has
/// authenticated with the profile's configured authority secret.  A failed
/// connection remains a route observation for the caller, which can then use
/// the retained local fallback.
pub(super) fn connect_worker(
    endpoint: &std::path::Path,
    secret: [u8; 32],
    capabilities: &backend_engine::CapabilityManifest,
    timeout: Duration,
    limits: TransportLimits,
) -> Result<Box<dyn backend_engine::RemoteTransport>, ProcessError> {
    if let Some(value) = endpoint
        .to_str()
        .and_then(|value| value.strip_prefix("tcp://"))
    {
        let address = value.parse::<SocketAddr>().map_err(|_| {
            ProcessError::Profile(format!("worker TCP endpoint has invalid address: {value}"))
        })?;
        let stream = TcpStream::connect_timeout(&address, timeout).map_err(|error| {
            ProcessError::Profile(format!("connect worker TCP endpoint {address}: {error}"))
        })?;
        let authority = backend_engine::TcpAuthority::new(secret);
        let max_record = limits
            .max_frame
            .checked_add(4)
            .ok_or_else(|| ProcessError::Profile("worker TCP record limit overflow".to_owned()))?;
        let stream =
            crate::worker_transport::authenticate_tcp(stream, &authority, timeout, max_record)
                .map_err(|error| {
                    ProcessError::Profile(format!("authenticate worker TCP peer: {error}"))
                })?;
        let transport =
            AsyncWorkerTransport::new(stream, capabilities, limits, timeout).map_err(|error| {
                ProcessError::Profile(format!("configure worker TCP transport: {error}"))
            })?;
        return Ok(Box::new(transport));
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = (endpoint, secret, capabilities, timeout, limits);
        return Err(ProcessError::Profile(
            "worker Unix transport is unavailable on this platform".to_owned(),
        ));
    }
    #[cfg(any(unix, windows))]
    let stream = UnixStream::connect(endpoint).map_err(|error| {
        ProcessError::Profile(format!(
            "connect worker endpoint {}: {error}",
            endpoint.display()
        ))
    })?;
    #[cfg(any(unix, windows))]
    let stream = crate::worker_transport::authenticate_unix(stream, timeout)
        .map_err(|error| ProcessError::Profile(format!("authenticate worker peer: {error}")))?;
    #[cfg(any(unix, windows))]
    let transport = AsyncWorkerTransport::new(stream, capabilities, limits, timeout)
        .map_err(|error| ProcessError::Profile(format!("configure worker transport: {error}")))?;
    #[cfg(any(unix, windows))]
    return Ok(Box::new(transport));
}
