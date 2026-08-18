//! Red-first specification for the incremental NDJSON frame decoder (S1).
//!
//! This is the piece that replaces `heart::client::http`'s
//! `response.text().await?` — the third of the three buffer points that make
//! today's "streaming" search arrive all at once. The decoder turns a byte
//! stream into a `Frame<S>` stream, emitting each frame the moment its line is
//! complete rather than when the body is.
//!
//! Written before the implementation exists. **Do not weaken a test to make it
//! pass.**
//!
//! # What the decoder must preserve from `heart::stream`
//!
//! The terminal-frame discipline is the whole reason that envelope exists: a
//! stream that ends without `End` or `Failed` is *truncated*, and must be
//! distinguishable from a successful empty answer. Moving from "parse a whole
//! body" to "parse a live stream" must not lose that, and it must not lose the
//! distinction between a server-sent error (a value) and a malformed line (a
//! decode failure).

use std::time::Duration;

use bytes::Bytes;
use heart::stream::WireError;
use heart::surface::{
    Completeness, Frame, Located, Residence, Summary, Surface, decode_frames,
};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct Hit {
    id: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct Marker(String);

struct Hits;

impl Surface for Hits {
    const NAME: &'static str = "hits";
    const PATH: &'static str = "/hits";
    type Request = String;
    type Item = Hit;
    type Note = Marker;
    type Key = u32;
    fn key(item: &Self::Item) -> u32 {
        item.id
    }
}

/// Serialize a frame exactly the way the server writes it: one JSON value per
/// line. Building the fixture through the real serializer rather than by hand
/// means these tests exercise the actual wire form.
fn line(frame: &Frame<Hits>) -> String {
    let mut s = serde_json::to_string(frame).expect("frame serializes");
    s.push('\n');
    s
}

fn hit(id: u32) -> Frame<Hits> {
    Frame::Item(Located::new(Hit { id }, Residence::Remote {
        generation: heart::surface::GenerationId(1),
    }))
}

fn end(n: u64) -> Frame<Hits> {
    Frame::End(Summary::complete(n))
}

/// Feed the decoder one `Bytes` chunk per element.
///
/// Takes owned strings so the returned stream is `'static` — the decoder spawns
/// its pump, so a stream borrowing a local would not satisfy it.
/// Boxed with an explicit `'static` rather than `impl Stream`: the decoder
/// spawns its pump, so the byte stream must not borrow the caller's locals, and
/// an `impl Trait` return in edition 2024 would capture their lifetimes.
fn chunks(parts: Vec<&str>) -> futures::stream::BoxStream<'static, Result<Bytes, std::io::Error>> {
    use futures::StreamExt as _;
    futures::stream::iter(
        parts
            .into_iter()
            .map(|p| Ok(Bytes::from(p.to_owned())))
            .collect::<Vec<_>>(),
    )
    .boxed()
}

async fn decode_all(
    body: impl futures::Stream<Item = Result<Bytes, std::io::Error>> + Send + 'static,
) -> Vec<Frame<Hits>> {
    use futures::StreamExt as _;
    decode_frames::<Hits, _, _>(body).collect().await
}

// ---------------------------------------------------------------------------
// 1. Basic decoding
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_terminated_body_decodes_to_its_frames() {
    let body = format!("{}{}{}", line(&hit(1)), line(&hit(2)), line(&end(2)));
    let frames = decode_all(chunks(vec![&body])).await;

    assert_eq!(frames.len(), 3);
    assert!(matches!(&frames[0], Frame::Item(l) if l.value.id == 1));
    assert!(matches!(&frames[2], Frame::End(_)));
}

/// Chunk boundaries are a transport artifact and must be invisible. A frame
/// split across three TCP reads is still one frame.
#[tokio::test]
async fn frames_split_across_chunk_boundaries_are_reassembled() {
    let body = format!("{}{}", line(&hit(1)), line(&end(1)));
    let mid = body.len() / 2;
    let (a, b) = body.split_at(mid);

    let frames = decode_all(chunks(vec![a, b])).await;
    assert_eq!(frames.len(), 2);
    assert!(matches!(&frames[0], Frame::Item(l) if l.value.id == 1));
}

/// The pathological case: one byte per chunk.
#[tokio::test]
async fn a_body_delivered_one_byte_at_a_time_still_decodes() {
    let body = format!("{}{}", line(&hit(7)), line(&end(1)));
    let parts: Vec<String> = body.chars().map(|c| c.to_string()).collect();
    let stream = futures::stream::iter(
        parts
            .into_iter()
            .map(|p| Ok::<_, std::io::Error>(Bytes::from(p)))
            .collect::<Vec<_>>(),
    );

    let frames = decode_all(stream).await;
    assert_eq!(frames.len(), 2);
    assert!(matches!(&frames[0], Frame::Item(l) if l.value.id == 7));
}

/// Several frames arriving in one chunk must all come out.
#[tokio::test]
async fn multiple_frames_in_one_chunk_all_decode() {
    let body = format!("{}{}{}", line(&hit(1)), line(&hit(2)), line(&end(2)));
    let frames = decode_all(chunks(vec![&body])).await;
    assert_eq!(frames.len(), 3);
}

#[tokio::test]
async fn blank_lines_are_ignored() {
    let body = format!("{}\n\n{}", line(&hit(1)), line(&end(1)));
    let frames = decode_all(chunks(vec![&body])).await;
    assert_eq!(frames.len(), 2);
}

// ---------------------------------------------------------------------------
// 2. Incrementality — the reason this exists
// ---------------------------------------------------------------------------

/// THE test. If the decoder buffers the body, the first frame cannot arrive
/// before the last byte — which is exactly the behaviour
/// `response.text().await` has today.
#[tokio::test]
async fn the_first_frame_arrives_before_the_body_completes() {
    use futures::StreamExt as _;

    let (tx, rx) = flume::unbounded::<Result<Bytes, std::io::Error>>();
    tx.send(Ok(Bytes::from(line(&hit(1))))).unwrap();
    // The body is deliberately NOT finished — no End frame, sender still open.

    let mut frames = std::pin::pin!(decode_frames::<Hits, _, _>(rx.into_stream()));

    let first = tokio::time::timeout(Duration::from_secs(5), frames.next())
        .await
        .expect("the first frame must arrive before the body is complete")
        .expect("a frame");
    assert!(matches!(&first, Frame::Item(l) if l.value.id == 1));

    tx.send(Ok(Bytes::from(line(&end(1))))).unwrap();
    drop(tx);
    let rest: Vec<_> = frames.collect().await;
    assert!(matches!(rest.last(), Some(Frame::End(_))));
}

// ---------------------------------------------------------------------------
// 3. Terminal-frame discipline — inherited from `heart::stream`
// ---------------------------------------------------------------------------

/// A body that stops without a terminal frame is TRUNCATED. It must not decode
/// as a successful short answer — "the lines I received all parsed" is exactly
/// the false success the typed envelope was introduced to kill.
#[tokio::test]
async fn a_body_with_no_terminal_frame_yields_a_synthetic_failure() {
    let body = format!("{}{}", line(&hit(1)), line(&hit(2)));
    let frames = decode_all(chunks(vec![&body])).await;

    assert_eq!(frames.len(), 3, "two hits plus a synthesized terminal");
    match frames.last().expect("a terminal frame") {
        Frame::Failed(_) => {}
        other => panic!("a truncated body must end in Failed, got {other:?}"),
    }
}

/// A trailing partial line (the connection died mid-frame) is truncation, not a
/// decode error — the bytes were fine, there just were not enough of them.
#[tokio::test]
async fn a_trailing_partial_line_is_truncation() {
    let body = format!("{}{{\"item\":", line(&hit(1)));
    let frames = decode_all(chunks(vec![&body])).await;
    assert!(
        matches!(frames.last(), Some(Frame::Failed(_))),
        "a partial trailing line must end the stream as Failed, got {:?}",
        frames.last()
    );
}

/// The decoder stops at the terminal frame. Anything after it is a writer bug
/// that must not be silently absorbed into the result set.
#[tokio::test]
async fn frames_after_the_terminal_frame_are_not_emitted() {
    let body = format!("{}{}{}", line(&hit(1)), line(&end(1)), line(&hit(99)));
    let frames = decode_all(chunks(vec![&body])).await;

    assert_eq!(frames.len(), 2, "the post-terminal frame must not appear");
    assert!(matches!(&frames[1], Frame::End(_)));
    assert!(
        !frames
            .iter()
            .any(|f| matches!(f, Frame::Item(l) if l.value.id == 99)),
        "a frame after the terminal frame leaked into the output"
    );
}

/// A server-sent `Failed` is a VALUE the server chose to send, and is terminal.
/// It must not be confused with a decode failure.
#[tokio::test]
async fn a_server_sent_failure_is_terminal_and_typed() {
    let failed: Frame<Hits> = Frame::Failed(WireError::Backend("qdrant down".into()));
    let body = format!("{}{}", line(&hit(1)), line(&failed));
    let frames = decode_all(chunks(vec![&body])).await;

    assert_eq!(frames.len(), 2);
    match &frames[1] {
        Frame::Failed(WireError::Backend(detail)) => assert_eq!(detail, "qdrant down"),
        other => panic!("expected a typed Backend failure, got {other:?}"),
    }
}

/// `Degraded` is not terminal — decoding must continue past it.
#[tokio::test]
async fn a_degraded_frame_does_not_end_the_stream() {
    let degraded: Frame<Hits> = Frame::Degraded(heart::surface::Degradation {
        source: heart::access::SourceId::from_uuid(uuid::Uuid::from_bytes([1; 16])),
        reason: WireError::Timeout,
    });
    let body = format!("{}{}{}", line(&degraded), line(&hit(1)), line(&end(1)));
    let frames = decode_all(chunks(vec![&body])).await;

    assert_eq!(frames.len(), 3, "decoding must continue past Degraded");
    assert!(matches!(&frames[2], Frame::End(_)));
}

// ---------------------------------------------------------------------------
// 4. Malformed input
// ---------------------------------------------------------------------------

/// A malformed line is a decode failure, distinct from a server-reported error.
/// Both are terminal, but a caller retries them differently: a server error may
/// be transient, a malformed frame means the peer is broken.
#[tokio::test]
async fn a_malformed_line_ends_the_stream_as_a_decode_failure() {
    let body = format!("{}not json at all\n{}", line(&hit(1)), line(&end(1)));
    let frames = decode_all(chunks(vec![&body])).await;

    assert!(matches!(&frames[0], Frame::Item(_)));
    match frames.get(1) {
        Some(Frame::Failed(WireError::Internal(_) | WireError::BadRequest(_))) => {}
        other => panic!("a malformed line must surface as a typed Failed, got {other:?}"),
    }
    assert_eq!(
        frames.len(),
        2,
        "decoding must stop at the malformed line, not resume after it"
    );
}

/// A transport error mid-body is terminal and must be reported, not swallowed
/// into a short successful answer.
#[tokio::test]
async fn a_transport_error_mid_body_ends_the_stream_as_failed() {
    let good = line(&hit(1));
    let stream = futures::stream::iter(vec![
        Ok(Bytes::from(good)),
        Err(std::io::Error::other("connection reset")),
    ]);
    let frames = decode_all(stream).await;

    assert!(matches!(&frames[0], Frame::Item(_)));
    assert!(
        matches!(frames.last(), Some(Frame::Failed(_))),
        "a transport error must end the stream as Failed, got {:?}",
        frames.last()
    );
}

/// An empty body is truncation, NOT a successful zero-hit answer.
#[tokio::test]
async fn an_empty_body_is_truncation_not_an_empty_success() {
    let frames = decode_all(chunks(vec![""])).await;
    assert!(
        matches!(frames.as_slice(), [Frame::Failed(_)]),
        "an empty body must be Failed, got {frames:?}"
    );
}

/// A body that is *only* a terminal frame IS a legitimate zero-hit answer, and
/// must be distinguishable from the empty body above.
#[tokio::test]
async fn a_body_of_only_a_terminal_frame_is_a_genuine_empty_success() {
    let frames = decode_all(chunks(vec![&line(&end(0))])).await;
    match frames.as_slice() {
        [Frame::End(summary)] => {
            assert_eq!(summary.items, 0);
            assert_eq!(summary.complete, Completeness::Complete);
        }
        other => panic!("expected a single complete End, got {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// 5. Round-trip against the real writer
// ---------------------------------------------------------------------------

/// Everything a server can emit, the decoder must read back identically. This
/// is the pin that stops the writer and reader drifting.
#[tokio::test]
async fn every_frame_kind_survives_a_write_read_round_trip() {
    let written: Vec<Frame<Hits>> = vec![
        Frame::Note(Marker("columns".into())),
        hit(1),
        Frame::Degraded(heart::surface::Degradation {
            source: heart::access::SourceId::from_uuid(uuid::Uuid::from_bytes([9; 16])),
            reason: WireError::Timeout,
        }),
        hit(2),
        Frame::End(Summary::partial(2, 2, Some(50))),
    ];
    let body: String = written.iter().map(line).collect();

    let read = decode_all(chunks(vec![&body])).await;
    assert_eq!(read, written, "the wire round-trip changed the frames");
}
