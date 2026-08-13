//! The typed streaming envelope for NDJSON search responses.
//!
//! # The error class this closes
//!
//! `POST /search` streams its ranked hits as NDJSON — one JSON value per line —
//! so a large result set never materialises in one frame. The pre-envelope wire
//! carried each line as a *bare* `Scored<Symbol>`, which made three different
//! outcomes indistinguishable to the reader:
//!
//! * a hit,
//! * a mid-stream server failure (there was nowhere on the wire to *say* one
//!   happened, so the writer's only option was a non-JSON line, which the reader
//!   then reported as "malformed hit N"), and
//! * a truncated stream (the connection dropped after three hits) — which read
//!   as a *successful* three-hit search, because "the lines I got all parsed" is
//!   all a bare-line reader can check.
//!
//! [`StreamFrame`] makes every line a well-typed frame. A stream error is now a
//! *value* ([`StreamFrame::Error`]) rather than a decode failure, and a stream
//! that ends **must** carry a terminal [`StreamFrame::End`] or
//! [`StreamFrame::Error`] — so the absence of one is a distinguishable, typed
//! error (see `heart::client::http::ClientError::Truncated`) instead of a silent
//! success. "Ended without a terminal frame" is unrepresentable as "ok".
//!
//! # Why this is not behind the `client` feature
//!
//! These are pure serde vocabulary types — they add no dependencies beyond
//! `serde`/`thiserror`, both already base dependencies of `heart`. Both sides of
//! the wire need them: the **server** (`index`, which does *not* enable the
//! `client` feature) has to *emit* frames, and the client has to *read* them.
//! Gating them behind `client` would leave the server unable to name the type it
//! serialises. Only the reqwest-backed *reader* stays in the `client`-gated
//! `heart::client::http`.

use serde::{Deserialize, Serialize};

use crate::score::Scored;
use crate::symbol::Symbol;

/// One line of a search NDJSON response.
///
/// Serialized **externally tagged** (`{"hit": …}` / `{"error": …}` /
/// `{"end": …}`), so every line is a single-key object that says what it is
/// before it says anything else — the reader never has to guess a bare value's
/// role. Generic over the hit payload `T` (`Scored<Symbol>` for `/search`), so
/// the envelope is reusable by any streaming ranked surface without re-deriving
/// the framing.
///
/// `#[non_exhaustive]`: a server that learns to emit a new frame kind (a
/// progress marker, say) must not make an older client fail to compile or panic
/// — readers match with an explicit fallback (see the reader in
/// `heart::client::http`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum StreamFrame<T> {
    /// One ranked hit.
    Hit(T),
    /// The stream is terminating because the server failed *after* it began
    /// answering. Carries a structured [`WireError`], not a rendered string, so
    /// the reader decides retry/offline/degraded on a typed variant.
    Error(WireError),
    /// The stream is terminating normally. Its presence is what distinguishes a
    /// complete answer from a truncated one.
    End(StreamSummary),
}

/// The terminal frame of a successful stream.
///
/// A `#[must_use]`-adjacent design: the *reader* treats a stream with no
/// terminal frame as [`crate::client::http::ClientError::Truncated`], so a
/// summary that never arrives cannot be mistaken for a zero-hit success.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct StreamSummary {
    /// How many [`StreamFrame::Hit`] frames preceded this one. The reader can
    /// cross-check its own count against this to catch a lossy transport.
    pub hits: u64,
}

impl StreamSummary {
    /// A summary over `hits` delivered rows.
    pub const fn new(hits: u64) -> Self {
        Self { hits }
    }
}

/// A structured, transport-independent failure a search surface can report
/// *on the wire* — as a [`StreamFrame::Error`] value or a whole-response error.
///
/// The `Display` text is for humans; the **variant** is what a client switches
/// on to decide whether to retry, fall back offline, or show a degraded result.
/// Stringly-typed errors made that decision impossible without parsing prose;
/// this makes it a `match`.
///
/// `#[non_exhaustive]`: a newer server may name a failure an older client has no
/// arm for; clients fold the unknown into their most conservative decision
/// rather than failing to compile.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, thiserror::Error)]
#[serde(tag = "kind", content = "detail", rename_all = "snake_case")]
#[non_exhaustive]
pub enum WireError {
    /// A backing store (index/vector/catalog) was unavailable. Usually transient
    /// — a retry may succeed.
    #[error("backend unavailable: {0}")]
    Backend(String),
    /// The query was rejected as malformed or unsatisfiable. Retrying the *same*
    /// query will not help; the caller must change it.
    #[error("query rejected: {0}")]
    BadRequest(String),
    /// The search exceeded its time budget. A narrower query or a retry may help.
    #[error("search timed out")]
    Timeout,
    /// An unclassified server-internal error. Degraded; a retry may help.
    #[error("internal error: {0}")]
    Internal(String),
}

/// The concrete symbol-search frame instantiation, named once so the server
/// writer and the client reader cannot disagree about the payload type.
pub type SymbolFrame = StreamFrame<Scored<Symbol>>;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::score::Score;

    #[test]
    fn frames_are_externally_tagged_single_key_objects() {
        let end: StreamFrame<i32> = StreamFrame::End(StreamSummary::new(2));
        assert_eq!(serde_json::to_string(&end).unwrap(), r#"{"end":{"hits":2}}"#);

        let err: StreamFrame<i32> =
            StreamFrame::Error(WireError::BadRequest("empty text".into()));
        assert_eq!(
            serde_json::to_string(&err).unwrap(),
            r#"{"error":{"kind":"bad_request","detail":"empty text"}}"#
        );

        let hit: StreamFrame<i32> = StreamFrame::Hit(7);
        assert_eq!(serde_json::to_string(&hit).unwrap(), r#"{"hit":7}"#);
    }

    #[test]
    fn a_hit_frame_round_trips_a_scored_value() {
        let frame: StreamFrame<Scored<String>> =
            StreamFrame::Hit(Scored::new("axum".to_owned(), Score::try_new(0.9).unwrap()));
        let line = serde_json::to_string(&frame).unwrap();
        let back: StreamFrame<Scored<String>> = serde_json::from_str(&line).unwrap();
        assert_eq!(frame, back);
    }

    #[test]
    fn wire_error_round_trips_by_variant_not_prose() {
        for err in [
            WireError::Backend("qdrant down".into()),
            WireError::BadRequest("bad cursor".into()),
            WireError::Timeout,
            WireError::Internal("boom".into()),
        ] {
            let json = serde_json::to_string(&err).unwrap();
            let back: WireError = serde_json::from_str(&json).unwrap();
            assert_eq!(err, back);
        }
    }
}
