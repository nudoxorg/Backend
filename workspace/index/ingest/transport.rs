//! [`FeedTransport`]: the HTTP seam every network-fetching follower goes
//! through, so tests substitute recorded fixtures for live calls
//! (REGISTRYLESS-PLAN §7.1; the deliverable's fixture requirement).
//!
//! The workspace HTTP client is `reqwest` 0.12 (`registry`, `server`,
//! `vector-remote` all use it). [`HttpTransport`] is the live blocking
//! adapter; this crate keeps the trait **synchronous and blocking-shaped** so
//! the pure follower logic and the driver stay runtime-agnostic and
//! unit-testable. This keeps the fetch path free of a tokio dependency; only
//! `main.rs` picks a runtime for the process.

use std::collections::BTreeMap;
use std::time::Duration;

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
        Self {
            url: url.into(),
            if_none_match: None,
        }
    }

    /// A conditional GET carrying the prior ETag.
    pub fn conditional(url: impl Into<String>, etag: Option<String>) -> Self {
        Self {
            url: url.into(),
            if_none_match: etag,
        }
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
    pub fn with(
        mut self,
        url: impl Into<String>,
        body: impl Into<Vec<u8>>,
        etag: Option<&str>,
    ) -> Self {
        self.entries
            .insert(url.into(), (body.into(), etag.map(str::to_owned)));
        self
    }
}

impl FeedTransport for FixtureTransport {
    fn fetch(&self, request: &FeedRequest) -> Result<FeedResponse, Error> {
        let Some((body, etag)) = self.entries.get(&request.url) else {
            return Err(Error::Status {
                url: request.url.clone(),
                status: 404,
            });
        };
        // Honor conditional GET: a matching ETag yields 304, exactly like a real
        // server, so the ETag short-circuit path is exercised end to end.
        if let (Some(requested), Some(current)) = (&request.if_none_match, etag)
            && requested == current
        {
            return Ok(FeedResponse::NotModified);
        }
        Ok(FeedResponse::Modified {
            body: body.clone(),
            etag: etag.clone(),
        })
    }
}

/// Blocking HTTP [`FeedTransport`] using `reqwest::blocking`.
///
/// The ingest library stays synchronous; this is the live network
/// implementation so followers are not fixture-only (docs/ISSUES.md L48-ic).
#[derive(Debug, Clone)]
pub struct HttpTransport {
    client: reqwest::blocking::Client,
}

impl HttpTransport {
    /// A client with a nudox ingest User-Agent and a 30s timeout.
    pub fn new() -> Self {
        let client = reqwest::blocking::Client::builder()
            .user_agent(concat!("nudox-ingest/", env!("CARGO_PKG_VERSION")))
            .timeout(Duration::from_secs(30))
            .build()
            .expect("http transport client construction cannot fail with valid params");
        Self { client }
    }
}

impl Default for HttpTransport {
    fn default() -> Self {
        Self::new()
    }
}

impl FeedTransport for HttpTransport {
    fn fetch(&self, request: &FeedRequest) -> Result<FeedResponse, Error> {
        let mut http_request = self.client.get(&request.url);
        if let Some(etag) = &request.if_none_match {
            http_request = http_request.header(reqwest::header::IF_NONE_MATCH, etag.as_str());
        }
        let response = http_request.send().map_err(|err| Error::Request {
            url: request.url.clone(),
            message: err.to_string(),
        })?;
        let status = response.status();
        if status == reqwest::StatusCode::NOT_MODIFIED {
            return Ok(FeedResponse::NotModified);
        }
        if status.is_success() {
            let etag = response
                .headers()
                .get(reqwest::header::ETAG)
                .and_then(|value| value.to_str().ok())
                .map(str::to_owned);
            let body = response.bytes().map_err(|err| Error::Request {
                url: request.url.clone(),
                message: err.to_string(),
            })?;
            return Ok(FeedResponse::Modified {
                body: body.to_vec(),
                etag,
            });
        }
        Err(Error::Status {
            url: request.url.clone(),
            status: status.as_u16(),
        })
    }
}

#[cfg(test)]
mod l48_live_http_transport {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::thread;

    /// docs/ISSUES.md L48-ic: `FixtureTransport` is the only `FeedTransport`.
    /// A blocking HTTP implementation must exist so the ingest binary can
    /// fetch real feeds rather than recorded fixtures.
    #[test]
    fn http_transport_fetches_and_honors_etag() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback");
        let addr = listener.local_addr().expect("local addr");
        let url = format!("http://{addr}/feed");

        thread::spawn(move || {
            for (i, incoming) in listener.incoming().enumerate() {
                let mut stream = incoming.expect("accept");
                let mut buf = [0u8; 1024];
                let n = stream.read(&mut buf).unwrap_or(0);
                let req = String::from_utf8_lossy(&buf[..n]);
                let if_none = req.lines().find_map(|line| {
                    let lower = line.to_ascii_lowercase();
                    lower
                        .strip_prefix("if-none-match:")
                        .map(|v| v.trim().to_string())
                });
                let (status, extra) = if if_none.as_deref() == Some("\"abc\"") {
                    ("304 Not Modified", "")
                } else {
                    ("200 OK", "ETag: \"abc\"\r\n")
                };
                let body = if status.starts_with("200") {
                    b"feed-body" as &[u8]
                } else {
                    b""
                };
                let resp = format!(
                    "HTTP/1.1 {status}\r\nContent-Length: {}\r\n{extra}Connection: close\r\n\r\n",
                    body.len()
                );
                let _ = stream.write_all(resp.as_bytes());
                let _ = stream.write_all(body);
                if i >= 1 {
                    break;
                }
            }
        });

        let transport = HttpTransport::new();
        let first = transport.fetch(&FeedRequest::get(&url)).expect("first GET");
        match first {
            FeedResponse::Modified { body, etag } => {
                assert_eq!(body, b"feed-body");
                assert_eq!(etag.as_deref(), Some("\"abc\""));
            }
            other @ FeedResponse::NotModified => panic!("first GET must be Modified, got {other:?}"),
        }

        let second = transport
            .fetch(&FeedRequest::conditional(&url, Some("\"abc\"".into())))
            .expect("conditional GET");
        assert!(
            matches!(second, FeedResponse::NotModified),
            "matching If-None-Match must be 304, got {second:?}"
        );
    }
}
