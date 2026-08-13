//! The connecting client — the piece of `heart::client` that actually *talks to*
//! a running `nudox-serve` (the `index::server` composition) over HTTP.
//!
//! Everything else in [`crate::client`] is transport-free vocabulary (DTOs,
//! authz witnesses, the query algebra). This module is the one place a real
//! transport (`reqwest`) enters, so it is behind the off-by-default `client`
//! feature: a consumer that only needs the wire *types* (the server itself,
//! `index`) never pulls `reqwest`, while a caller that needs to *reach* the
//! server (the GUI, a CLI) opts in with `features = ["client"]`.
//!
//! The wire is the domain: `POST /search` takes the one [`crate::query::Query`]
//! algebra directly and streams NDJSON pages of [`crate::Scored<crate::Symbol>`]
//! (INDEX-PLAN §9), so this client serializes/deserializes exactly the shared
//! `heart` types — no bespoke request/response structs.

use crate::client::dto::{AddPackageDto, HealthDto};
use crate::query::Query;
use crate::{Scored, Symbol};
use url::Url;

/// A client bound to one `nudox-serve` base URL.
///
/// Cheap to clone (the inner [`reqwest::Client`] is an `Arc` handle). Construct
/// once at startup and share.
#[derive(Debug, Clone)]
pub struct NudoxClient {
    base: Url,
    http: reqwest::Client,
}

/// Why a client call failed.
#[derive(Debug, thiserror::Error)]
pub enum ClientError {
    /// The base URL could not be joined with an endpoint path.
    #[error("invalid endpoint url: {0}")]
    Url(#[from] url::ParseError),
    /// The HTTP request itself failed (connect/timeout/transport).
    #[error("request failed: {0}")]
    Transport(#[from] reqwest::Error),
    /// The server answered with a non-success status.
    #[error("server returned {status}: {body}")]
    Status {
        /// The HTTP status code.
        status: u16,
        /// The (possibly truncated) response body, for diagnostics.
        body: String,
    },
    /// A response line/body could not be decoded into the expected type.
    #[error("decode error: {0}")]
    Decode(#[from] serde_json::Error),
}

impl NudoxClient {
    /// Bind a client to `base` (e.g. `http://127.0.0.1:8080`). Uses sensible
    /// connect/read timeouts so a dead server surfaces as an error, not a hang.
    pub fn new(base: Url) -> Result<Self, ClientError> {
        let http = reqwest::Client::builder()
            .connect_timeout(std::time::Duration::from_secs(5))
            .timeout(std::time::Duration::from_secs(30))
            .user_agent(concat!("nudox-client/", env!("CARGO_PKG_VERSION")))
            .build()?;
        Ok(Self { base, http })
    }

    /// Parse `base` from a string and bind.
    pub fn connect(base: &str) -> Result<Self, ClientError> {
        Self::new(Url::parse(base)?)
    }

    /// The server's readiness (`GET /readyz`): whether every backing store is up
    /// and which, if any, are degraded. This is the client-side "am I connected?"
    /// probe the GUI status bar renders.
    pub async fn readyz(&self) -> Result<HealthDto, ClientError> {
        let url = self.base.join("readyz")?;
        let response = self.http.get(url).send().await?;
        self.json(response).await
    }

    /// Symbol search (`POST /search`): send the one query algebra, collect the
    /// NDJSON stream of ranked hits. `query.target` must be [`crate::query::Target::Symbols`].
    pub async fn search(&self, query: &Query) -> Result<Vec<Scored<Symbol>>, ClientError> {
        let url = self.base.join("search")?;
        let response = self.http.post(url).json(query).send().await?;
        self.ndjson(response).await
    }

    /// Package search (`POST /packages/search`): the lexical package surface,
    /// also NDJSON. Returns the raw hit lines' JSON so callers that key on a
    /// specific hit shape can decode them without this crate hard-coding it.
    pub async fn search_packages(&self, query: &Query) -> Result<Vec<serde_json::Value>, ClientError> {
        let url = self.base.join("packages/search")?;
        let response = self.http.post(url).json(query).send().await?;
        self.ndjson(response).await
    }

    /// Request that the server index a package (`POST /packages`). Returns `Ok`
    /// once the server has accepted the request (2xx); the actual indexing is
    /// asynchronous (queue-driven).
    pub async fn add_package(&self, dto: &AddPackageDto) -> Result<(), ClientError> {
        let url = self.base.join("packages")?;
        let response = self.http.post(url).json(dto).send().await?;
        let _ = self.checked(response).await?;
        Ok(())
    }

    /// Deserialize a single JSON body after checking the status.
    async fn json<T: serde::de::DeserializeOwned>(
        &self,
        response: reqwest::Response,
    ) -> Result<T, ClientError> {
        let body = self.checked(response).await?;
        Ok(serde_json::from_str(&body)?)
    }

    /// Deserialize an NDJSON body (one JSON value per non-empty line) after
    /// checking the status.
    async fn ndjson<T: serde::de::DeserializeOwned>(
        &self,
        response: reqwest::Response,
    ) -> Result<Vec<T>, ClientError> {
        let body = self.checked(response).await?;
        body.lines()
            .filter(|line| !line.trim().is_empty())
            .map(|line| serde_json::from_str(line).map_err(ClientError::from))
            .collect()
    }

    /// Turn a non-2xx response into a typed [`ClientError::Status`]; otherwise
    /// return the body text.
    async fn checked(&self, response: reqwest::Response) -> Result<String, ClientError> {
        let status = response.status();
        let body = response.text().await?;
        if status.is_success() {
            Ok(body)
        } else {
            Err(ClientError::Status {
                status: status.as_u16(),
                body: body.chars().take(2048).collect(),
            })
        }
    }
}
