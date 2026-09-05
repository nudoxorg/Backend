use std::{
    io::Read,
    sync::atomic::{AtomicBool, Ordering},
    time::Duration,
};
use thiserror::Error;

/// Largest accepted registry listing response.
pub const MAX_PAGE_BYTES: usize = 512 * 1024;
/// Largest accepted source archive.
pub const MAX_ARCHIVE_BYTES: usize = 64 * 1024 * 1024;

/// A request crossing the explicitly injected network boundary.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TransportRequest<'a> {
    /// Fully constructed HTTPS URL.
    pub url: &'a str,
    /// Optional opaque ETag/ref digest supplied by the source.
    pub if_none_match: Option<&'a str>,
    /// Maximum response body admitted to `output`.
    pub maximum_bytes: usize,
}

/// Metadata retained from one bounded response.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TransportResponse {
    /// HTTP response status.
    pub status: u16,
    /// Opaque ETag supplied by the remote source, if any.
    pub etag: Option<String>,
}

/// Exact failure at the transport boundary.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum TransportFault {
    /// Cancellation was observed before the request completed.
    #[error("registry request cancelled")]
    Cancelled,
    /// The remote body exceeded the request's explicit bound.
    #[error("registry response exceeds bounded payload")]
    TooLarge,
    /// Connection, TLS, or stream I/O failed.
    #[error("registry transport failure")]
    Network,
}

/// The only network boundary accepted by acquisition. Tests supply deterministic local fixtures.
pub trait RegistryTransport {
    /// Fetches one bounded response into caller-owned storage.
    fn get(
        &mut self,
        request: TransportRequest<'_>,
        output: &mut Vec<u8>,
        cancelled: &AtomicBool,
    ) -> Result<TransportResponse, TransportFault>;
}

/// Production HTTP transport with no hidden global client or executor.
#[derive(Debug)]
pub struct UreqTransport {
    agent: ureq::Agent,
}

impl Default for UreqTransport {
    fn default() -> Self {
        let config = ureq::Agent::config_builder()
            .http_status_as_error(false)
            .timeout_global(Some(Duration::from_secs(30)))
            .build();
        Self {
            agent: config.new_agent(),
        }
    }
}

impl RegistryTransport for UreqTransport {
    fn get(
        &mut self,
        request: TransportRequest<'_>,
        output: &mut Vec<u8>,
        cancelled: &AtomicBool,
    ) -> Result<TransportResponse, TransportFault> {
        if cancelled.load(Ordering::Acquire) {
            return Err(TransportFault::Cancelled);
        }
        let mut call = self
            .agent
            .get(request.url)
            .header("User-Agent", "nudox-index-acquire/0.1");
        if let Some(etag) = request.if_none_match {
            call = call.header("If-None-Match", etag);
        }
        let response = call.call().map_err(|_| TransportFault::Network)?;
        let status = response.status().as_u16();
        let etag = response
            .headers()
            .get("etag")
            .and_then(|header| header.to_str().ok())
            .map(ToOwned::to_owned);
        if status == 304 {
            output.clear();
            return Ok(TransportResponse { status, etag });
        }
        let mut body = response.into_body();
        let bound = request
            .maximum_bytes
            .checked_add(1)
            .ok_or(TransportFault::TooLarge)?;
        let bound = u64::try_from(bound).map_err(|_| TransportFault::TooLarge)?;
        let mut reader = body.with_config().limit(bound).reader().take(bound);
        output.clear();
        let mut chunk = [0_u8; 8192];
        loop {
            if cancelled.load(Ordering::Acquire) {
                return Err(TransportFault::Cancelled);
            }
            let read = reader
                .read(&mut chunk)
                .map_err(|_| TransportFault::Network)?;
            if read == 0 {
                break;
            }
            if output
                .len()
                .checked_add(read)
                .is_none_or(|total| total > request.maximum_bytes)
            {
                return Err(TransportFault::TooLarge);
            }
            output.extend_from_slice(&chunk[..read]);
        }
        Ok(TransportResponse { status, etag })
    }
}
