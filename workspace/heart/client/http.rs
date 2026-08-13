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
use crate::stream::{StreamFrame, WireError};
use crate::{Page, PackageHit, Scored, Symbol};
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
///
/// A **structured** enum, not a rendered string: the failure *class* is what a
/// caller switches on to decide retry vs. offline vs. degraded (see the GUI's
/// `RemoteStatus` mapping). The `Display` text is for humans only. The three
/// stream-specific variants below are the whole point of the typed envelope —
/// they let a caller tell a server error from a truncated stream from a
/// malformed line, all of which the old bare-NDJSON reader collapsed into a
/// single [`ClientError::Decode`].
///
/// `#[non_exhaustive]`: adding a failure class must not break a caller's
/// `match` — callers fold the unknown into their most conservative decision.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ClientError {
    /// The base URL could not be joined with an endpoint path.
    #[error("invalid endpoint url: {0}")]
    Url(#[from] url::ParseError),
    /// The HTTP request itself failed (connect/timeout/transport). The server
    /// may simply be unreachable.
    #[error("request failed: {0}")]
    Transport(#[from] reqwest::Error),
    /// The server answered with a non-success status *before* streaming began.
    #[error("server returned {status}: {body}")]
    Status {
        /// The HTTP status code.
        status: u16,
        /// The (possibly truncated) response body, for diagnostics.
        body: String,
    },
    /// A response line/body could not be decoded into the expected type — a
    /// *malformed* frame, distinct from a server-reported error (which is a
    /// value; see [`ClientError::Wire`]).
    #[error("decode error: {0}")]
    Decode(#[from] serde_json::Error),
    /// The server reported a structured failure *mid-stream*, after emitting
    /// `hits_before` hit(s). This is a [`WireError`] *value* the server chose to
    /// send — not a transport or decode failure — so `error` carries the typed
    /// class and `hits_before` distinguishes "errored after 3 hits" from
    /// "errored before answering".
    #[error("server errored after {hits_before} hit(s): {error}")]
    Wire {
        /// The structured wire failure the server sent.
        error: WireError,
        /// How many hits arrived before the error frame.
        hits_before: usize,
    },
    /// The stream ended with **no** terminal frame after `hits_before` hit(s).
    /// The connection dropped (or the server crashed) mid-answer; the result is
    /// incomplete and must not be treated as a successful, possibly-empty page.
    #[error("stream truncated after {hits_before} hit(s): no terminal frame")]
    Truncated {
        /// How many hits arrived before the stream was cut.
        hits_before: usize,
    },
    /// The stream carried frames *after* its terminal frame — a protocol
    /// violation on the server side. Reported rather than silently ignored so a
    /// broken writer is visible.
    #[error("protocol violation: frames after the terminal frame ({hits_before} hit(s))")]
    Protocol {
        /// How many hits arrived before the terminal frame.
        hits_before: usize,
    },
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

    /// Symbol search (`POST /search`): send the one query algebra, read the
    /// typed NDJSON [`StreamFrame`] stream of ranked hits. `query.target` must be
    /// [`crate::query::Target::Symbols`].
    ///
    /// On success every line was a well-typed frame terminated by a
    /// [`StreamFrame::End`]. A mid-stream [`StreamFrame::Error`] surfaces as
    /// [`ClientError::Wire`] (a *value*, carrying how many hits preceded it); a
    /// stream with no terminal frame surfaces as [`ClientError::Truncated`] —
    /// never as a spurious empty success.
    pub async fn search(&self, query: &Query) -> Result<Vec<Scored<Symbol>>, ClientError> {
        let url = self.base.join("search")?;
        let response = self.http.post(url).json(query).send().await?;
        let body = self.checked(response).await?;
        read_hit_stream::<Symbol>(&body)
    }

    /// Package search (`POST /packages/search`): the lexical package surface.
    ///
    /// The server answers with a single JSON [`Page`] of [`PackageHit`]s (not a
    /// stream), so this decodes the whole page. `PackageHit` is the one shape
    /// both sides name — schema drift is a compile error at the server's
    /// projection, not a runtime `Value` index-panic here.
    pub async fn search_packages(&self, query: &Query) -> Result<Page<PackageHit>, ClientError> {
        let url = self.base.join("packages/search")?;
        let response = self.http.post(url).json(query).send().await?;
        self.json(response).await
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

/// Read a typed hit stream from a line-delimited NDJSON body of
/// [`StreamFrame`]s.
///
/// The terminal-frame requirement is enforced *here*: a body that ends without
/// an `End` or `Error` frame returns [`ClientError::Truncated`], so "the lines I
/// received all parsed" can no longer masquerade as a complete answer. A server
/// [`StreamFrame::Error`] becomes a typed [`ClientError::Wire`] value (carrying
/// the count of hits that preceded it); a line that fails to parse as a frame is
/// a [`ClientError::Decode`] — the three outcomes the bare-line reader could not
/// tell apart.
fn read_hit_stream<T>(body: &str) -> Result<Vec<Scored<T>>, ClientError>
where
    T: serde::de::DeserializeOwned,
{
    let mut hits: Vec<Scored<T>> = Vec::new();
    // `None` while no terminal frame has been seen; `Some(Ok/Err)` once one has.
    let mut terminal: Option<Result<(), WireError>> = None;

    for line in body.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if terminal.is_some() {
            // A well-formed stream stops at its terminal frame; anything after
            // it is a writer bug we refuse to silently absorb.
            return Err(ClientError::Protocol {
                hits_before: hits.len(),
            });
        }
        match serde_json::from_str::<StreamFrame<Scored<T>>>(line)? {
            StreamFrame::Hit(scored) => hits.push(scored),
            StreamFrame::Error(error) => terminal = Some(Err(error)),
            StreamFrame::End(_summary) => terminal = Some(Ok(())),
        }
    }

    match terminal {
        Some(Ok(())) => Ok(hits),
        Some(Err(error)) => Err(ClientError::Wire {
            error,
            hits_before: hits.len(),
        }),
        None => Err(ClientError::Truncated {
            hits_before: hits.len(),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::score::Score;
    use crate::stream::{StreamFrame, StreamSummary};

    fn hit_line(id: &str, score: f32) -> String {
        // Build a frame the same way the server does, so the test exercises the
        // real serialisation rather than a hand-written string.
        let frame: StreamFrame<Scored<String>> =
            StreamFrame::Hit(Scored::new(id.to_owned(), Score::try_new(score).unwrap()));
        serde_json::to_string(&frame).unwrap()
    }

    fn end_line(hits: u64) -> String {
        let frame: StreamFrame<Scored<String>> = StreamFrame::End(StreamSummary::new(hits));
        serde_json::to_string(&frame).unwrap()
    }

    #[test]
    fn a_terminated_stream_yields_its_hits() {
        let body = format!("{}\n{}\n{}", hit_line("a", 0.9), hit_line("b", 0.5), end_line(2));
        let hits = read_hit_stream::<String>(&body).expect("terminated stream is ok");
        assert_eq!(hits.len(), 2);
        assert_eq!(hits[0].value, "a");
    }

    #[test]
    fn a_stream_with_no_terminal_frame_is_truncated_not_empty_success() {
        // Two good hits, then the connection drops — no End/Error frame.
        let body = format!("{}\n{}", hit_line("a", 0.9), hit_line("b", 0.5));
        match read_hit_stream::<String>(&body) {
            Err(ClientError::Truncated { hits_before }) => assert_eq!(hits_before, 2),
            other => panic!("expected Truncated, got {other:?}"),
        }
    }

    #[test]
    fn a_mid_stream_error_frame_is_a_typed_value_not_a_decode_failure() {
        let error: StreamFrame<Scored<String>> =
            StreamFrame::Error(WireError::Backend("qdrant down".into()));
        let body = format!("{}\n{}", hit_line("a", 0.9), serde_json::to_string(&error).unwrap());
        match read_hit_stream::<String>(&body) {
            Err(ClientError::Wire { error, hits_before }) => {
                assert_eq!(hits_before, 1, "one hit preceded the error");
                assert!(matches!(error, WireError::Backend(_)));
            }
            other => panic!("expected Wire, got {other:?}"),
        }
    }

    #[test]
    fn a_malformed_line_is_a_decode_error_distinct_from_a_server_error() {
        let body = format!("{}\nnot json at all", hit_line("a", 0.9));
        assert!(matches!(
            read_hit_stream::<String>(&body),
            Err(ClientError::Decode(_))
        ));
    }
}
