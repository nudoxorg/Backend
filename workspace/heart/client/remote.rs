//! `RemoteClient` — the ONE blanket `Serve<S>` impl every remote surface gets
//! for free.
//!
//! # The error class this closes
//!
//! [`super::http::NudoxClient`] hand-writes one method per route
//! (`search`/`search_packages`/`add_package`/`readyz`) and covers 4 of the
//! server's 17 (contract §0.6). Each new surface needed a new method, a new
//! bespoke response shape, and a new way for the client and server to drift.
//! `RemoteClient` needs none of that: `impl<S: Surface> Serve<S> for
//! RemoteClient` requires only `S::PATH`, `S::Request: Serialize`, and
//! `Frame<S>: DeserializeOwned` — all supplied by [`crate::surface::Surface`]
//! itself — so adding a surface yields its remote client with no new code at
//! all (contract §2.1). It also closes the other half of §0.2: `NudoxClient`
//! reads the *whole* response with `response.text().await` before decoding
//! anything; this streams via [`crate::surface::decode_frames`], so a caller
//! gets the first frame before the last byte of the response has arrived.
//!
//! # Carrying forward `NudoxClient::Error`'s reasoning
//!
//! `NudoxClient`'s `Error` enum exists to let a caller `match` on failure
//! *class* rather than parse prose: transport failure vs. a non-2xx status
//! vs. a malformed frame vs. a server-reported failure vs. a truncated
//! stream. `Serve::serve` cannot return a `Result` — it must return
//! immediately, and the answer arrives on the channel — so there is nowhere
//! for a `heart::client::http::Error` to live here. The same *reasoning*
//! survives anyway, folded into [`crate::stream::WireError`], the type every
//! [`Frame::Failed`] already carries:
//!
//! | `NudoxClient::Error` | Here |
//! |---|---|
//! | `Transport` (connect/timeout) | [`WireError::Timeout`] / [`WireError::Backend`], classified by [`classify_transport_error`] |
//! | `Status` (non-2xx before streaming) | [`WireError::BadRequest`] (4xx) / [`WireError::Backend`] (5xx), by [`classify_status_error`] |
//! | `Decode` (malformed line) | [`WireError::Internal`], synthesized by [`crate::surface::decode_frames`] itself |
//! | `Wire` (server-sent failure) | passed through unchanged — it is already a [`Frame::Failed`] value on the wire |
//! | `Truncated` / `Protocol` | [`WireError::Internal`], synthesized by `decode_frames` |
//!
//! Every one of these becomes exactly one [`Frame::Failed`] frame instead of
//! a distinct `Err` variant — a caller already has to treat any `Failed` as
//! "no usable answer", so collapsing the *reporting channel* (always
//! `Frame::Failed`) while keeping the *classification* (the `WireError`
//! variant inside it) loses no information a caller could act on.
//!
//! # Why `NudoxClient::search` is not reimplemented as a shim here
//!
//! The brief for this change allows leaving `NudoxClient` untouched if
//! shimming it over `RemoteClient` "turns out to be invasive" — it does, for
//! a reason specific to *today's* server rather than to this client. The
//! live `/search` handler (`index/server/http/handlers/search.rs`) still
//! writes `heart::stream::StreamFrame` (tagged `hit`/`error`/`end`), not the
//! new `Frame<S>` (tagged `note`/`item`/`degraded`/`end`/`failed`) this
//! module speaks — migrating the writer is `LOCAL-REMOTE-CONTRACT.md`'s S2/S3
//! work, not S1's. Routing `NudoxClient::search` through a `Symbols: Surface`
//! and `RemoteClient` today would silently stop talking to the server that
//! is actually running. `NudoxClient` is left exactly as it was; it starts
//! sharing this transport once the server speaks `Frame<S>`.

use std::sync::Arc;
use std::time::Duration;

use tokio::sync::Notify;
use url::Url;

use crate::client::http::Error as ClientError;
use crate::stream::WireError;
use crate::surface::{Answer, Emitter, Frame, Gen, Serve, StreamHandle, Surface, answer_channel, decode_frames};

/// How many frames [`Answer`]'s underlying channel is sized for. A genuine
/// bound now (`answer_channel`'s own doc comment, contract task 9) — `pump`
/// below forwards items through [`Emitter::item_async`], so a slow local
/// consumer parks this task for room rather than losing frames once it fills.
const ANSWER_CAPACITY: usize = 64;

/// A client bound to one `nudox-serve` base URL, answering *any* [`Surface`]
/// through the single blanket [`Serve`] impl below.
///
/// Cheap to clone: the inner [`reqwest::Client`] is an `Arc` handle, and
/// `Url` clones cheaply too. Construct once at startup and share, same as
/// [`super::http::NudoxClient`].
#[derive(Debug, Clone)]
pub struct RemoteClient {
    base: Url,
    http: reqwest::Client,
}

impl RemoteClient {
    /// Bind a client to `base` (e.g. `http://127.0.0.1:8080`).
    ///
    /// Only a connect timeout is set — deliberately **no** whole-request
    /// timeout. [`super::http::NudoxClient`] sets both, which was correct for
    /// a call that had to buffer a bounded answer and return; `RemoteClient`
    /// exists so a caller can hold a streaming answer open for as long as the
    /// server keeps producing results (a large corpus, a slow semantic
    /// index), and a blanket request timeout would sever that connection out
    /// from under a still-progressing, still-useful answer. A caller that
    /// wants a hard deadline drops the `Answer` — that is what dropping it is
    /// *for* (see [`Serve::serve`]'s impl below).
    pub fn new(base: Url) -> Result<Self, ClientError> {
        let http = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(5))
            .user_agent(concat!("nudox-client/", env!("CARGO_PKG_VERSION")))
            .build()?;
        Ok(Self { base, http })
    }

    /// Parse `base` from a string and bind.
    pub fn connect(base: &str) -> Result<Self, ClientError> {
        Self::new(Url::parse(base)?)
    }
}

impl<S: Surface> Serve<S> for RemoteClient {
    /// Begin answering `request` at `generation`. Returns immediately — the
    /// actual HTTP round trip runs on a spawned task, pumping decoded
    /// [`Frame`]s into the paired [`Emitter`] as they arrive.
    ///
    /// # Cancellation
    ///
    /// The brief mechanism is [`Emitter::is_cancelled`]: it flips true the
    /// instant the paired `Answer` is dropped, because `Answer` holds the
    /// receiving half of the same `flume` channel `Emitter::is_cancelled`
    /// inspects. That alone is enough to stop *emitting further frames*, but
    /// it is a poll — it only gets checked between frames the pump loop is
    /// already awake to handle, and would leave the loop parked forever on
    /// `frames.next().await` if the server goes quiet (no more bytes, no
    /// close) after the `Answer` is dropped. So this also attaches a
    /// [`StreamHandle`] whose canceller wakes a [`Notify`] the pump loop
    /// races against every await point via `tokio::select!` — the same
    /// "drop = cancel real work, not just the channel" contract
    /// `StreamHandle`'s own doc comment describes for exactly this case (a
    /// spawned task). Together: `is_cancelled` is the simple check between
    /// frames: the `Notify` is what makes the abort *reactive* even while
    /// blocked mid-network-read, which is the case that actually matters for
    /// "dropping the `Answer` must abort the in-flight request" — a search
    /// abandoned by a keystroke must free the connection promptly, not
    /// whenever the server next happens to send a byte.
    fn serve(&self, request: S::Request, generation: Gen) -> Answer<S> {
        let (emitter, answer) = answer_channel::<S>(ANSWER_CAPACITY, generation);

        let url = match self.base.join(S::PATH) {
            Ok(url) => url,
            Err(err) => {
                // Synchronous failure, before anything was spawned — report
                // it the same way every other failure arrives: as the one
                // terminal frame on this answer.
                let _ = emitter.failed(WireError::Internal(format!(
                    "invalid endpoint {:?}: {err}",
                    S::PATH
                )));
                return answer;
            }
        };

        let cancelled = Arc::new(Notify::new());
        let canceller = Arc::clone(&cancelled);
        let answer =
            answer.with_handle(StreamHandle::new(generation, move || canceller.notify_one()));

        let http = self.http.clone();
        tokio::spawn(pump::<S>(http, url, request, emitter, cancelled));

        answer
    }
}

/// The spawned body of [`Serve::serve`]: POST `request` as JSON, decode the
/// NDJSON response incrementally, and forward every [`Frame`] to `emitter` —
/// racing each blocking point against `cancelled` so dropping the paired
/// `Answer` frees the connection promptly rather than at the pump loop's
/// next convenience.
async fn pump<S: Surface>(
    http: reqwest::Client,
    url: Url,
    request: S::Request,
    emitter: Emitter<S>,
    cancelled: Arc<Notify>,
) {
    use futures::StreamExt as _;

    if emitter.is_cancelled() {
        return;
    }

    let response = tokio::select! {
        biased;
        () = cancelled.notified() => return,
        result = http.post(url).json(&request).send() => result,
    };

    let response = match response {
        Ok(response) => response,
        Err(err) => {
            let _ = emitter.failed(classify_transport_error(&err));
            return;
        }
    };

    let status = response.status();
    if !status.is_success() {
        // A non-2xx status arrives before any frame does — read the (small,
        // truncated) body for diagnostics and report it as the one frame
        // this answer will ever carry.
        let body = tokio::select! {
            biased;
            () = cancelled.notified() => return,
            body = response.text() => body.unwrap_or_default(),
        };
        let _ = emitter.failed(classify_status_error(status, &body));
        return;
    }

    // `bytes_stream()`, not `.text()` — this is the buffer point S1 exists to
    // remove (contract §3(c)). `reqwest::Error` already satisfies
    // `decode_frames`'s `E: Display + Send + 'static` bound, so the stream
    // plugs straight in with no wrapper type.
    let mut frames = Box::pin(decode_frames::<S, _, _>(response.bytes_stream()));

    loop {
        if emitter.is_cancelled() {
            // Dropping `frames` here drops the reqwest byte stream (and with
            // it the response body / underlying connection) — this is the
            // "stop pumping and drop the reqwest future" the brief calls for.
            return;
        }

        let next = tokio::select! {
            biased;
            () = cancelled.notified() => return,
            frame = frames.next() => frame,
        };

        let Some(frame) = next else {
            // The decoder itself is exhaustive about ending with a terminal
            // frame (see its own doc comment) — reaching a plain `None` here
            // would mean it didn't, which is a bug in `decode_frames`, not a
            // case this loop needs its own fallback for.
            return;
        };

        // `Frame` is `#[non_exhaustive]` for callers in *other* crates; this
        // match is exhaustive on purpose — `heart` itself sees every variant
        // decode_frames can produce today, and a future variant is a
        // deliberate cross-cutting change to this file, not something a `_`
        // arm should silently swallow.
        match frame {
            Frame::Note(note) => {
                let _ = emitter.note(note);
            }
            Frame::Item(located) => {
                // Awaited, not the sync `item`: `pump` is always spawned
                // (`Serve::serve`'s `tokio::spawn(pump::<S>(...))` above), so
                // it can park for real backpressure instead of dropping a
                // search result a local consumer was merely slow to take.
                let _ = emitter.item_async(located.value, located.residence).await;
            }
            Frame::Degraded(degradation) => {
                let _ = emitter.degraded(degradation);
            }
            Frame::End(summary) => {
                let _ = emitter.end(summary);
                return;
            }
            Frame::Failed(error) => {
                let _ = emitter.failed(error);
                return;
            }
        }
    }
}

/// Classify a `reqwest` transport failure (connect refused, DNS, TLS, a
/// timed-out connect, or a request that failed to build) into the
/// [`WireError`] class a caller would retry differently.
fn classify_transport_error(err: &reqwest::Error) -> WireError {
    if err.is_timeout() {
        WireError::Timeout
    } else if err.is_builder() {
        // The request never left this process — e.g. `S::Request` failed to
        // serialize. Not a backend/network condition a retry would fix.
        WireError::Internal(format!("could not build request: {err}"))
    } else {
        WireError::Backend(format!("request failed: {err}"))
    }
}

/// Classify a non-2xx HTTP status received *before* streaming began.
/// 4xx is the caller's request being rejected (retrying the same request
/// will not help, same reasoning as [`super::http::NudoxClient`]'s `Status`
/// handling for a bad query); everything else is treated as a backend
/// condition that may be transient.
fn classify_status_error(status: reqwest::StatusCode, body: &str) -> WireError {
    let detail = format!(
        "server returned {status}: {}",
        body.chars().take(2048).collect::<String>()
    );
    if status.is_client_error() {
        WireError::BadRequest(detail)
    } else {
        WireError::Backend(detail)
    }
}
