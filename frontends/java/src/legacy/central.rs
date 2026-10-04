//! Maven Central HTTPS release-artifact adapter.
//!
//! SHA-1 sidecar verification is deliberately not performed here: no approved
//! SHA-1 dependency exists in this workspace. HTTPS transport is the only
//! integrity mechanism in this checkpoint.

use crate::legacy::purl::MavenCoordinates;
use std::io::Read;
use thiserror::Error;

const MAX_BYTES: u64 = 512 * 1024 * 1024;

/// Maven Central client with an injectable repository base URL.
pub struct Central {
    base_url: String,
    agent: ureq::Agent,
}
impl Default for Central {
    fn default() -> Self {
        Self::new("https://repo1.maven.org/maven2")
    }
}
impl Central {
    /// Constructs a client for `base_url` (normally Maven Central).
    pub fn new(base_url: &str) -> Self {
        let config = ureq::Agent::config_builder()
            .http_status_as_error(false)
            .build();
        Self {
            base_url: base_url.trim_end_matches('/').to_owned(),
            agent: config.new_agent(),
        }
    }
    /// Builds the release JAR URL.
    pub fn jar_url(&self, coords: &MavenCoordinates<'_>) -> String {
        self.url(coords, "")
    }
    /// Builds the sources JAR URL.
    pub fn sources_jar_url(&self, coords: &MavenCoordinates<'_>) -> String {
        self.url(coords, "-sources")
    }
    fn url(&self, c: &MavenCoordinates<'_>, suffix: &str) -> String {
        format!(
            "{}/{}/{}/{}/{}-{}{}.jar",
            self.base_url,
            c.group.replace('.', "/"),
            c.artifact,
            c.version,
            c.artifact,
            c.version,
            suffix
        )
    }
    /// Fetches into caller-owned storage, rejecting bodies above 512 MiB.
    pub fn fetch(&self, url: &str, output: &mut Vec<u8>) -> Result<(), FetchError> {
        fetch_with(&self.agent, url, output)
    }
}

/// Closed network failure categories; the underlying ureq error is intentionally translated.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NetworkKind {
    /// Connection, DNS, or other transport-level failure.
    Transport,
    /// `ureq` reported a TLS-layer error while making the request.
    Blocked,
    /// HTTP parsing or protocol handling failed before a usable response.
    Protocol,
}
/// Typed Central fetch failure.
#[derive(Debug, Error, Eq, PartialEq)]
pub enum FetchError {
    #[error("Maven artifact not found: {url}")]
    /// Central returned HTTP 404 for the requested artifact.
    NotFound {
        /// Complete URL requested for the missing artifact.
        url: String,
    },
    #[error("Maven artifact forbidden: {url}")]
    /// Central returned HTTP 403 for the requested artifact.
    Forbidden {
        /// Complete URL requested for the forbidden artifact.
        url: String,
    },
    #[error("unexpected Maven status {status} for {url}")]
    /// Central returned a status outside the accepted 2xx, 403, and 404 cases.
    UnexpectedStatus {
        /// HTTP response status code returned by Central.
        status: u16,
        /// Complete URL requested for the artifact.
        url: String,
    },
    #[error("Maven network failure for {url}: {kind:?}")]
    /// No artifact result could be obtained through the classified network path.
    Network {
        /// Complete URL whose request failed.
        url: String,
        /// Closed transport, blocking, or protocol category.
        kind: NetworkKind,
    },
    #[error("Maven response exceeds 512 MiB: {url}")]
    /// The successful response body exceeded the adapter's hard size bound.
    TooLarge {
        /// Complete URL whose response exceeded the limit.
        url: String,
    },
}

fn fetch_with(agent: &ureq::Agent, url: &str, output: &mut Vec<u8>) -> Result<(), FetchError> {
    let response = agent
        .get(url)
        .header("User-Agent", "nudox-java-lane/0.1")
        .call()
        .map_err(|error| FetchError::Network {
            url: url.to_owned(),
            kind: network_kind(&error),
        })?;
    let status = response.status().as_u16();
    if status == 404 {
        return Err(FetchError::NotFound {
            url: url.to_owned(),
        });
    }
    if status == 403 {
        return Err(FetchError::Forbidden {
            url: url.to_owned(),
        });
    }
    if !(200..300).contains(&status) {
        return Err(FetchError::UnexpectedStatus {
            status,
            url: url.to_owned(),
        });
    }
    let mut body = response.into_body();
    let mut body = body
        .with_config()
        .limit(MAX_BYTES + 1)
        .reader()
        .take(MAX_BYTES + 1);
    output.clear();
    body.read_to_end(output).map_err(|_| FetchError::Network {
        url: url.to_owned(),
        kind: NetworkKind::Transport,
    })?;
    if output.len() as u64 > MAX_BYTES {
        return Err(FetchError::TooLarge {
            url: url.to_owned(),
        });
    }
    Ok(())
}
fn network_kind(error: &ureq::Error) -> NetworkKind {
    match error {
        ureq::Error::StatusCode(_) | ureq::Error::Protocol(_) | ureq::Error::Http(_) => {
            NetworkKind::Protocol
        }
        ureq::Error::Tls(_) => NetworkKind::Blocked,
        ureq::Error::Io(_) | ureq::Error::HostNotFound | ureq::Error::ConnectionFailed => {
            NetworkKind::Transport
        }
        _ => NetworkKind::Transport,
    }
}
