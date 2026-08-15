//! [`FeedTransport`]: the HTTP seam every network-fetching follower goes
//! through, so tests substitute recorded fixtures for live calls
//! (REGISTRYLESS-PLAN §7.1; the deliverable's fixture requirement).
//!
//! The workspace HTTP client is `reqwest` 0.12 (`registry`, `server`,
//! `vector-remote` all use it). The live implementation belongs in the binary /
//! deployment layer where an async runtime exists; this crate keeps the trait
//! **synchronous and blocking-shaped** so the pure follower logic and the
//! driver stay runtime-agnostic and unit-testable. A blocking `reqwest` adapter
//! (or an async one wrapped by the caller) satisfies it. This keeps the whole
//! ingestor library free of a tokio dependency; only `main.rs` picks a runtime.

use std::collections::BTreeMap;

/// A conditional GET against a feed endpoint.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FeedRequest {
    /// The absolute URL to fetch.
    pub url: String,
    /// The `If-None-Match` ETag from the last successful crawl, when known.
    /// When the server answers `304 Not Modified` the follower short-circuits
    /// (REGISTRYLESS-PLAN §7.1; the ETag 304 requirement).
    pub if_none_match: Option<String>,
}

impl FeedRequest {
    /// An unconditional GET (no prior ETag).
    pub fn get(url: impl Into<String>) -> Self {
        Self { url: url.into(), if_none_match: None }
    }

    /// A conditional GET carrying the prior ETag.
    pub fn conditional(url: impl Into<String>, etag: Option<String>) -> Self {
        Self { url: url.into(), if_none_match: etag }
    }
}

/// A feed response: either fresh bytes with a new ETag, or a `304` telling the
/// follower nothing changed since `if_none_match`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FeedResponse {
    /// `200 OK`: the body bytes and the server's `ETag`, when present.
    Modified {
        /// The response body.
        body: Vec<u8>,
        /// The server's `ETag` header, to store as the next `if_none_match`.
        etag: Option<String>,
    },
    /// `304 Not Modified`: the follower keeps its watermark and emits nothing.
    NotModified,
}

/// Why a feed fetch failed.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// The request could not be sent or the connection failed.
    #[error("feed request to {url} failed: {message}")]
    Request {
        /// The URL that failed.
        url: String,
        /// The underlying error message.
        message: String,
    },
    /// The server answered with a non-success, non-304 status.
    #[error("feed {url} answered HTTP {status}")]
    Status {
        /// The URL.
        url: String,
        /// The HTTP status code.
        status: u16,
    },
}

/// Backwards-compatible alias: the transport error (now [`Error`]).
pub use self::Error as TransportError;

/// The HTTP transport a follower fetches through. All network IO lives behind
/// this trait so followers stay pure and tests use fixtures.
pub trait FeedTransport: Send + Sync {
    /// Perform one conditional GET.
    fn fetch(&self, request: &FeedRequest) -> Result<FeedResponse, Error>;
}

/// A fixture transport: a `url → (body, etag)` table with ETag-aware `304`
/// semantics, for tests (REGISTRYLESS-PLAN §7.1 recorded-slice requirement).
#[derive(Debug, Default)]
pub struct FixtureTransport {
    entries: BTreeMap<String, (Vec<u8>, Option<String>)>,
}

impl FixtureTransport {
    /// A fresh, empty fixture table.
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a fixture body (and its ETag) for a URL. Returns `self` for
    /// builder-style setup.
    pub fn with(mut self, url: impl Into<String>, body: impl Into<Vec<u8>>, etag: Option<&str>) -> Self {
        self.entries
            .insert(url.into(), (body.into(), etag.map(str::to_owned)));
        self
    }
}

impl FeedTransport for FixtureTransport {
    fn fetch(&self, request: &FeedRequest) -> Result<FeedResponse, Error> {
        let Some((body, etag)) = self.entries.get(&request.url) else {
            return Err(Error::Status { url: request.url.clone(), status: 404 });
        };
        // Honor conditional GET: a matching ETag yields 304, exactly like a real
        // server, so the ETag short-circuit path is exercised end to end.
        if let (Some(requested), Some(current)) = (&request.if_none_match, etag)
            && requested == current
        {
            return Ok(FeedResponse::NotModified);
        }
        Ok(FeedResponse::Modified { body: body.clone(), etag: etag.clone() })
    }
}
