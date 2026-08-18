//! Red-first specification: **a hung source must not hold an answer open
//! forever** — the last gap in the offline-first guarantee.
//!
//! # The gap
//!
//! `Federated` already degrades correctly when a source *fails*: the failure
//! becomes a `Frame::Degraded`, the healthy sources' rows stand, and the answer
//! still ends (`tests/federated.rs`). And `RemoteClient` sets a 5-second
//! **connect** timeout, so an index that is down or refusing connections
//! resolves quickly into exactly that path.
//!
//! But `RemoteClient` deliberately sets **no whole-request timeout**
//! (`client/remote.rs:89-100`), and that decision is correct on its own terms:
//! a search stream is legitimately long-lived, and a blanket request timeout
//! would sever a healthy connection mid-answer.
//!
//! Meanwhile `merge` ends the answer only once **every** source has produced a
//! terminal frame (`saw_terminal[rank]`, `merge_impl.rs`). So a source that
//! connects and then simply stops — a half-open TCP connection, a wedged
//! server, a laptop that changed networks mid-query — leaves the merged answer
//! **permanently unterminated**.
//!
//! The user-visible shape of that is specific and bad: local rows appear
//! instantly and are perfectly usable, and the query never finishes. A spinner
//! that never stops, over results that are already correct and complete. The
//! offline-first promise is "if the index is down, the app is flawless by
//! itself" — an app that never stops loading does not meet it.
//!
//! # Why the fix belongs here and not in the HTTP client
//!
//! A transport-level timeout cannot tell "this stream is slow because the
//! corpus is large" from "this stream is dead", and `client/remote.rs` says so.
//! The federation can: it knows how long the *whole answer* has been open and
//! that other sources have already finished. So the bound is a **per-answer
//! deadline** applied by the merge — on expiry, every source that has not
//! terminated is reported as degraded and the answer ends.
//!
//! This is a bound on *waiting*, never on *delivering*: frames that already
//! arrived are kept, and a source that beats the deadline is untouched. It is
//! the same shape as `Completeness::Partial` — an honest smaller answer rather
//! than an error.
//!
//! **Do not weaken these tests to make them pass.** In particular, do not
//! implement the deadline by dropping rows that already arrived, and do not
//! give it a default so short that a legitimately slow first search is cut off.

use std::sync::Arc;
use std::time::{Duration, Instant};

use heart::access::SourceId;
use heart::surface::{
    Answer, Emitter, Federated, Frame, Gen, MergePump, Residence, Serve, Summary, Timer,
    answer_channel,
};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct Row {
    id: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct RowRequest {
    text: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct RowNote {
    stage: String,
}

struct Rows;

impl heart::surface::Surface for Rows {
    const NAME: &'static str = "rows";
    const PATH: &'static str = "/rows/search";
    type Request = RowRequest;
    type Item = Row;
    type Note = RowNote;
    type Key = u32;
    fn key(item: &Self::Item) -> u32 {
        item.id
    }
}

fn row(id: u32) -> Row {
    Row { id }
}

fn source(tag: u8) -> SourceId {
    SourceId::from_uuid(uuid::Uuid::from_bytes([tag; 16]))
}

struct Scripted<F>(Arc<F>)
where
    F: Fn(Emitter<Rows>) + Send + Sync + 'static;

impl<F> Serve<Rows> for Scripted<F>
where
    F: Fn(Emitter<Rows>) + Send + Sync + 'static,
{
    fn serve(&self, _request: RowRequest, generation: Gen) -> Answer<Rows> {
        let (tx, answer) = answer_channel::<Rows>(64, generation);
        let script = Arc::clone(&self.0);
        std::thread::spawn(move || script(tx));
        answer
    }
}

fn scripted<F>(f: F) -> Arc<dyn Serve<Rows>>
where
    F: Fn(Emitter<Rows>) + Send + Sync + 'static,
{
    Arc::new(Scripted(Arc::new(f)))
}

fn spawner() -> Arc<dyn Fn(MergePump) + Send + Sync> {
    Arc::new(|pump| {
        tokio::spawn(pump);
    })
}

/// A Tokio-backed timer. A real host injects its own — lindsey passes a GPUI
/// `background_executor().timer(..)` — which is exactly why this is a closure
/// rather than a `Duration`: the merge pump is runtime-agnostic and
/// `tokio::time::sleep` would panic on GPUI's executor.
fn timer(after: Duration) -> Timer {
    Arc::new(move || Box::pin(tokio::time::sleep(after)))
}

fn request() -> RowRequest {
    RowRequest {
        text: "q".to_owned(),
    }
}

/// A healthy local source: answers immediately and terminates.
fn healthy_local() -> Arc<dyn Serve<Rows>> {
    scripted(|tx| {
        tx.item(row(1), Residence::Local).ok();
        tx.item(row(2), Residence::Local).ok();
        tx.end(Summary::complete(2)).ok();
    })
}

/// A source that connects, says nothing, and never terminates — the wedged
/// index. It holds its `Emitter` (so the channel is not disconnected) until the
/// consumer goes away.
fn wedged_remote() -> Arc<dyn Serve<Rows>> {
    scripted(|tx: Emitter<Rows>| {
        for _ in 0..600 {
            if tx.is_cancelled() {
                return;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    })
}

async fn drain(answer: Answer<Rows>) -> (Vec<Row>, Option<Summary>, usize) {
    use heart::surface::Surface as _;
    let mut rows: Vec<Row> = Vec::new();
    let mut summary = None;
    let mut degradations = 0usize;
    while let Some(frame) = answer.recv().await {
        match frame {
            Frame::Item(located) => {
                let key = Rows::key(&located.value);
                match rows.iter_mut().find(|r| Rows::key(r) == key) {
                    Some(existing) => *existing = located.value,
                    None => rows.push(located.value),
                }
            }
            Frame::End(s) => summary = Some(s),
            Frame::Degraded(_) => degradations += 1,
            _ => {}
        }
    }
    (rows, summary, degradations)
}

// ---------------------------------------------------------------------------
// 1. The gap
// ---------------------------------------------------------------------------

/// THE test. A wedged remote must not hold the answer open. Without a deadline
/// this hangs until the outer timeout fires — which is precisely the "spinner
/// that never stops over results that are already correct" failure.
#[tokio::test]
async fn a_wedged_source_cannot_hold_the_answer_open_forever() {
    let federated = Federated::new(
        vec![
            (source(1), healthy_local()),
            (source(2), wedged_remote()),
        ],
        spawner(),
    )
    .with_deadline(timer(Duration::from_millis(300)));

    let answer = federated.serve(request(), Gen(1));

    let outcome = tokio::time::timeout(Duration::from_secs(5), drain(answer))
        .await
        .expect("the answer must terminate on its own, not hang");

    let (rows, summary, degradations) = outcome;

    assert_eq!(
        rows,
        vec![row(1), row(2)],
        "the healthy source's rows must survive the deadline untouched"
    );
    assert_eq!(
        degradations, 1,
        "the wedged source must be reported as degraded"
    );
    let summary = summary.expect("the answer must end");
    assert!(
        !summary.is_complete(),
        "an answer that gave up on a source is not complete"
    );
}

/// The deadline bounds *waiting*, so it must actually fire near its value
/// rather than being enforced by some much longer fallback.
#[tokio::test]
async fn the_deadline_is_honoured_promptly() {
    let federated = Federated::new(
        vec![
            (source(1), healthy_local()),
            (source(2), wedged_remote()),
        ],
        spawner(),
    )
    .with_deadline(timer(Duration::from_millis(200)));

    let start = Instant::now();
    drain(federated.serve(request(), Gen(1))).await;
    let elapsed = start.elapsed();

    assert!(
        elapsed < Duration::from_secs(2),
        "answer took {elapsed:?} against a 200ms deadline"
    );
}

// ---------------------------------------------------------------------------
// 2. It must not damage the healthy path
// ---------------------------------------------------------------------------

/// A source that finishes inside the deadline must be entirely unaffected — no
/// spurious degradation, and a genuinely `Complete` summary.
#[tokio::test]
async fn a_source_that_beats_the_deadline_is_untouched() {
    let federated = Federated::new(
        vec![
            (source(1), healthy_local()),
            (
                source(2),
                scripted(|tx| {
                    std::thread::sleep(Duration::from_millis(50));
                    tx.item(row(3), Residence::Remote {
                        generation: heart::surface::GenerationId(1),
                    })
                    .ok();
                    tx.end(Summary::complete(1)).ok();
                }),
            ),
        ],
        spawner(),
    )
    .with_deadline(timer(Duration::from_secs(30)));

    let (rows, summary, degradations) = drain(federated.serve(request(), Gen(1))).await;

    assert_eq!(rows.len(), 3, "every row from both sources must arrive");
    assert_eq!(degradations, 0, "nothing degraded — both sources finished");
    assert!(
        summary.expect("the answer must end").is_complete(),
        "a fully-answered federation is complete"
    );
}

/// Rows delivered *before* the deadline must be kept. The deadline bounds how
/// long we wait, never what we have already been given — dropping them would
/// turn a slow source into a data-loss bug.
#[tokio::test]
async fn rows_delivered_before_the_deadline_survive_it() {
    let federated = Federated::new(
        vec![(
            source(1),
            scripted(|tx: Emitter<Rows>| {
                tx.item(row(7), Residence::Local).ok();
                // …then wedge without terminating.
                for _ in 0..600 {
                    if tx.is_cancelled() {
                        return;
                    }
                    std::thread::sleep(Duration::from_millis(10));
                }
            }),
        )],
        spawner(),
    )
    .with_deadline(timer(Duration::from_millis(200)));

    let (rows, summary, degradations) = drain(federated.serve(request(), Gen(1))).await;

    assert_eq!(rows, vec![row(7)], "the delivered row must survive");
    assert_eq!(degradations, 1);
    assert!(summary.is_some(), "the answer must still end");
}

/// No deadline configured must keep today's behaviour exactly — this is opt-in,
/// so a host that has not thought about it is not silently given a timeout.
#[tokio::test]
async fn without_a_deadline_behaviour_is_unchanged() {
    let federated = Federated::new(vec![(source(1), healthy_local())], spawner());

    let (rows, summary, degradations) = drain(federated.serve(request(), Gen(1))).await;
    assert_eq!(rows, vec![row(1), row(2)]);
    assert_eq!(degradations, 0);
    assert_eq!(summary, Some(Summary::complete(2)));
}

// ---------------------------------------------------------------------------
// 3. The offline-first guarantee, stated end to end
// ---------------------------------------------------------------------------

/// The whole promise in one test: with the index contributing nothing usable —
/// wedged, not merely down — the local plane still produces a complete,
/// navigable, promptly-terminating answer.
#[tokio::test]
async fn the_local_plane_alone_produces_a_usable_answer() {
    let federated = Federated::new(
        vec![
            (source(1), healthy_local()),
            (source(2), wedged_remote()),
        ],
        spawner(),
    )
    .with_deadline(timer(Duration::from_millis(250)));

    let start = Instant::now();
    let answer = federated.serve(request(), Gen(1));

    // The first row must arrive immediately — not after the deadline.
    let first = answer.recv().await.expect("a frame must arrive");
    assert!(
        matches!(first, Frame::Item(_)),
        "local rows must not wait on the remote, got {first:?}"
    );
    assert!(
        start.elapsed() < Duration::from_millis(150),
        "first row took {:?} — local delivery is being gated on the remote",
        start.elapsed()
    );

    let (rest, summary, _) = drain(answer).await;
    assert!(!rest.is_empty());
    assert!(summary.is_some(), "and the answer finishes");
}
