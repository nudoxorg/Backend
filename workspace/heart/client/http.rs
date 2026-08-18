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
use crate::client::remote::RemoteClient;
use crate::query::Query;
#[cfg(test)]
use crate::stream::StreamFrame;
use crate::stream::WireError;
use crate::surface::{Frame, Gen, Serve as _, Symbols};
use crate::{PackageHit, Page, Scored, Symbol};
use url::Url;

/// A client bound to one `nudox-serve` base URL.
///
/// Cheap to clone (the inner [`reqwest::Client`] is an `Arc` handle, and
/// [`RemoteClient`] clones just as cheaply — see its own doc comment).
/// Construct once at startup and share.
#[derive(Debug, Clone)]
pub struct NudoxClient {
    base: Url,
    http: reqwest::Client,
    /// The `Serve<Symbols>` transport [`NudoxClient::search`] is now a thin
    /// shim over (contract §2.1/S1). Every other method here
    /// (`readyz`/`search_packages`/`add_package`) still speaks its own
    /// hand-written request/response shape directly through `http` — this
    /// crate's `/search` route is the first (and, until S3 ports the rest of
    /// this client's routes onto `Surface`, only) one that speaks `Frame<S>`
    /// on the wire, which is what `RemoteClient` requires.
    remote: RemoteClient,
}

/// Why a client call failed.
///
/// A **structured** enum, not a rendered string: the failure *class* is what a
/// caller switches on to decide retry vs. offline vs. degraded (see the GUI's
/// `RemoteStatus` mapping). The `Display` text is for humans only. The three
/// stream-specific variants below are the whole point of the typed envelope —
/// they let a caller tell a server error from a truncated stream from a
/// malformed line, all of which the old bare-NDJSON reader collapsed into a
/// single [`Error::Decode`].
///
/// `#[non_exhaustive]`: adding a failure class must not break a caller's
/// `match` — callers fold the unknown into their most conservative decision.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
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
    /// value; see [`Error::Wire`]).
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

pub use self::Error as ClientError;

impl NudoxClient {
    /// Bind a client to `base` (e.g. `http://127.0.0.1:8080`). Uses sensible
    /// connect/read timeouts so a dead server surfaces as an error, not a hang.
    pub fn new(base: Url) -> Result<Self, Error> {
        let http = reqwest::Client::builder()
            .connect_timeout(std::time::Duration::from_secs(5))
            .timeout(std::time::Duration::from_secs(30))
            .user_agent(concat!("nudox-client/", env!("CARGO_PKG_VERSION")))
            .build()?;
        let remote = RemoteClient::new(base.clone())?;
        Ok(Self { base, http, remote })
    }

    /// Parse `base` from a string and bind.
    pub fn connect(base: &str) -> Result<Self, Error> {
        Self::new(Url::parse(base)?)
    }

    /// The server's readiness (`GET /readyz`): whether every backing store is up
    /// and which, if any, are degraded. This is the client-side "am I connected?"
    /// probe the GUI status bar renders.
    pub async fn readyz(&self) -> Result<HealthDto, Error> {
        let url = self.base.join("readyz")?;
        let response = self.http.get(url).send().await?;
        self.json(response).await
    }

    /// Symbol search (`POST /search`): send the one query algebra, read the
    /// ranked hits. `query.target` must be [`crate::query::Target::Symbols`].
    ///
    /// # This is now a shim over [`RemoteClient`]
    ///
    /// The server's `/search` route moved from the ad hoc
    /// [`StreamFrame`]/[`read_hit_stream`] envelope this type used to decode
    /// by hand onto [`crate::surface::Frame<Symbols>`] — `heart::surface`'s
    /// one envelope, shared with every other search surface
    /// (`LOCAL-REMOTE-CONTRACT.md` S1/S2). `RemoteClient`'s blanket
    /// `Serve<Symbols>` impl already speaks that envelope (streaming, via
    /// [`crate::surface::decode_frames`], not `response.text()`), so this
    /// method's whole body is now "drive that `Serve` call and fold its
    /// frames into the `Vec` this signature has always returned" — the exact
    /// shim `LOCAL-REMOTE-CONTRACT.md`'s S1 phase deferred to whoever
    /// unbuffered the server (S2), because routing this through `RemoteClient`
    /// before the server spoke `Frame<Symbols>` would have silently stopped
    /// talking to it (see [`RemoteClient`]'s own module doc comment on why
    /// S1 originally left this method untouched).
    ///
    /// The public signature — `Result<Vec<Scored<Symbol>>, Error>` — is
    /// unchanged, so every existing caller (the GUI's `SearchStore`, this
    /// crate's own tests) keeps compiling and keeps its "collect everything,
    /// then render" behaviour; a caller that wants the progressive/streaming
    /// behaviour `Answer<Symbols>` actually offers should call
    /// `RemoteClient::serve` directly instead of through this shim.
    ///
    /// `Frame::Failed` becomes [`Error::Wire`] (carrying how many hits arrived
    /// first, same as the old mid-stream [`StreamFrame::Error`] case); an
    /// answer whose stream ends with **no** terminal frame at all (the
    /// `flume` channel disconnecting without an `End`/`Failed` ever being
    /// sent — `RemoteClient::serve`'s own pump does not do this in practice,
    /// but nothing in `Answer`'s type forbids a future `Serve<S>` impl from
    /// dropping a channel silently) still surfaces as [`Error::Truncated`],
    /// carrying forward the same "no terminal frame is never a silent
    /// success" rule the old NDJSON reader enforced.
    pub async fn search(&self, query: &Query) -> Result<Vec<Scored<Symbol>>, Error> {
        let answer: crate::surface::Answer<Symbols> = self.remote.serve(query.clone(), Gen(0));
        let mut hits = Vec::new();
        while let Some(frame) = answer.recv().await {
            match frame {
                Frame::Item(located) => hits.push(located.value),
                Frame::End(_) => return Ok(hits),
                Frame::Failed(error) => {
                    return Err(Error::Wire {
                        error,
                        hits_before: hits.len(),
                    });
                }
                // `Frame` is `#[non_exhaustive]`; `Degraded` and any future
                // frame kind fold into "keep reading" — a degraded federated
                // source does not fail this collection, same as everywhere
                // else `heart::surface::merge`'s semantics apply.
                _ => {}
            }
        }
        Err(Error::Truncated {
            hits_before: hits.len(),
        })
    }

    /// Package search (`POST /packages/search`): the lexical package surface.
    ///
    /// The server answers with a single JSON [`Page`] of [`PackageHit`]s (not a
    /// stream), so this decodes the whole page. `PackageHit` is the one shape
    /// both sides name — schema drift is a compile error at the server's
    /// projection, not a runtime `Value` index-panic here.
    pub async fn search_packages(&self, query: &Query) -> Result<Page<PackageHit>, Error> {
        let url = self.base.join("packages/search")?;
        let response = self.http.post(url).json(query).send().await?;
        self.json(response).await
    }

    /// Request that the server index a package (`POST /packages`). Returns `Ok`
    /// once the server has accepted the request (2xx); the actual indexing is
    /// asynchronous (queue-driven).
    pub async fn add_package(&self, dto: &AddPackageDto) -> Result<(), Error> {
        let url = self.base.join("packages")?;
        let response = self.http.post(url).json(dto).send().await?;
        let _ = self.checked(response).await?;
        Ok(())
    }

    /// Deserialize a single JSON body after checking the status.
    async fn json<T: serde::de::DeserializeOwned>(
        &self,
        response: reqwest::Response,
    ) -> Result<T, Error> {
        let body = self.checked(response).await?;
        Ok(serde_json::from_str(&body)?)
    }

    /// Turn a non-2xx response into a typed [`Error::Status`]; otherwise
    /// return the body text.
    async fn checked(&self, response: reqwest::Response) -> Result<String, Error> {
        let status = response.status();
        let body = response.text().await?;
        if status.is_success() {
            Ok(body)
        } else {
            Err(Error::Status {
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
/// an `End` or `Error` frame returns [`Error::Truncated`], so "the lines I
/// received all parsed" can no longer masquerade as a complete answer. A server
/// [`StreamFrame::Error`] becomes a typed [`Error::Wire`] value (carrying
/// the count of hits that preceded it); a line that fails to parse as a frame is
/// a [`Error::Decode`] — the three outcomes the bare-line reader could not
/// tell apart.
///
/// `#[cfg(test)]`: [`NudoxClient::search`] — the only production caller this
/// ever had — is now a shim over [`RemoteClient`]/[`crate::surface::decode_frames`]
/// (see that method's own doc comment), so nothing outside this module's own
/// tests constructs the pre-`Frame<S>` [`StreamFrame`] envelope over the wire
/// anymore. The function (and the tests exercising it below) stay as a pinned
/// spec of that older envelope's decode rules — `StreamFrame` itself is still
/// live vocabulary elsewhere in this crate — rather than being deleted outright.
#[cfg(test)]
fn read_hit_stream<T>(body: &str) -> Result<Vec<Scored<T>>, Error>
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
            return Err(Error::Protocol {
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
        Some(Err(error)) => Err(Error::Wire {
            error,
            hits_before: hits.len(),
        }),
        None => Err(Error::Truncated {
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
        let body = format!(
            "{}\n{}\n{}",
            hit_line("a", 0.9),
            hit_line("b", 0.5),
            end_line(2)
        );
        let hits = read_hit_stream::<String>(&body).expect("terminated stream is ok");
        assert_eq!(hits.len(), 2);
        assert_eq!(hits[0].value, "a");
    }

    #[test]
    fn a_stream_with_no_terminal_frame_is_truncated_not_empty_success() {
        // Two good hits, then the connection drops — no End/Error frame.
        let body = format!("{}\n{}", hit_line("a", 0.9), hit_line("b", 0.5));
        match read_hit_stream::<String>(&body) {
            Err(Error::Truncated { hits_before }) => assert_eq!(hits_before, 2),
            other => panic!("expected Truncated, got {other:?}"),
        }
    }

    #[test]
    fn a_mid_stream_error_frame_is_a_typed_value_not_a_decode_failure() {
        let error: StreamFrame<Scored<String>> =
            StreamFrame::Error(WireError::Backend("qdrant down".into()));
        let body = format!(
            "{}\n{}",
            hit_line("a", 0.9),
            serde_json::to_string(&error).unwrap()
        );
        match read_hit_stream::<String>(&body) {
            Err(Error::Wire { error, hits_before }) => {
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
            Err(Error::Decode(_))
        ));
    }
}
