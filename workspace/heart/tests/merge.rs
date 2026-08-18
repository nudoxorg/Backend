//! Red-first specification for `heart::surface::merge` — the ONE federating
//! merge, shared by the server's source federation (S2) and the GUI's
//! local+remote federation (S4).
//!
//! Written before the implementation exists. Making these pass IS the
//! deliverable. **Do not weaken a test to make it pass** — if one cannot be
//! satisfied, that is a design bug to raise.
//!
//! # The semantics being pinned: PROGRESSIVE
//!
//! You cannot have all three of (a) items stream out as soon as they are known,
//! (b) globally score-sorted output, and (c) overlay-override, because emitting
//! the base's `X@0.9` requires knowing no overlay holds `X`, which is only known
//! once every overlay is exhausted.
//!
//! We give up (b). Sources are polled **concurrently**; each source's own items
//! keep their relative order; across sources the output interleaves by arrival.
//! Overlay-override survives as a **supersede**: a duplicate key from a
//! higher-precedence source is re-emitted with `supersedes` set, and the
//! consumer replaces its existing row. That is exactly the in-place `Merge`
//! model the GUI's search store already implements, so the final state is
//! deterministic even though arrival order is not.
//!
//! # Why this lives in `heart` and not in `index`
//!
//! The server merges its federated sources and the GUI merges local+remote.
//! Those are the same operation over the same type (`Answer<S>`), so there is
//! one implementation. A merge written in `index` could not serve the GUI.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize};
use std::time::Duration;

use heart::access::SourceId;
use heart::stream::WireError;
use heart::surface::{
    Answer, Completeness, Degradation, Emitter, Frame, Gen, GenerationId, Located, Residence,
    Summary, Surface, answer_channel, merge, merge_bounded,
};
use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// Test surface
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct Row {
    id: u32,
    body: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct RowNote(String);

struct Rows;

impl Surface for Rows {
    const NAME: &'static str = "rows";
    const PATH: &'static str = "/rows";
    type Request = String;
    type Item = Row;
    type Note = RowNote;
    type Key = u32;
    fn key(item: &Self::Item) -> u32 {
        item.id
    }
}

fn row(id: u32, body: &str) -> Row {
    Row {
        id,
        body: body.to_owned(),
    }
}

fn source(tag: u8) -> SourceId {
    SourceId::from_uuid(uuid::Uuid::from_bytes([tag; 16]))
}

/// Build a source that emits `items` then ends. Returns the answer, ready to
/// hand to `merge`.
fn finished_source(items: Vec<(Row, Residence)>) -> Answer<Rows> {
    let count = items.len() as u64;
    // Capacity must cover the whole burst: nothing drains this answer until
    // it is handed to `merge`, and every item below is pushed synchronously
    // via `.expect("emit")`. `capacity` is now an enforced bound (contract
    // task 9), not an ignored hint — a fixed `64` would panic on
    // `suppression_scales_to_a_large_duplicate_set`'s 2_000-item bursts.
    let (tx, answer) = answer_channel::<Rows>(count.max(1) as usize, Gen(1));
    for (item, residence) in items {
        tx.item(item, residence).expect("emit");
    }
    tx.end(Summary::complete(count)).expect("end");
    answer
}

/// Drive a merge to completion, returning every frame it produced.
async fn run(sources: Vec<(SourceId, Answer<Rows>)>) -> Vec<Frame<Rows>> {
    let (answer, pump) = merge::<Rows>(Gen(1), sources);
    let pumping = tokio::spawn(pump);
    let mut out = Vec::new();
    while let Some(frame) = answer.recv().await {
        out.push(frame);
    }
    pumping.await.expect("pump completes");
    out
}

fn items(frames: &[Frame<Rows>]) -> Vec<Located<Row>> {
    frames
        .iter()
        .filter_map(|f| match f {
            Frame::Item(l) => Some(l.clone()),
            _ => None,
        })
        .collect()
}

fn terminal(frames: &[Frame<Rows>]) -> &Frame<Rows> {
    frames.last().expect("a merge always ends with a terminal frame")
}

// ---------------------------------------------------------------------------
// 1. The basics
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_merge_of_disjoint_sources_yields_every_item_exactly_once() {
    let frames = run(vec![
        (
            source(1),
            finished_source(vec![(row(1, "a"), Residence::Local)]),
        ),
        (
            source(2),
            finished_source(vec![(
                row(2, "b"),
                Residence::Remote {
                    generation: GenerationId(1),
                },
            )]),
        ),
    ])
    .await;

    let mut ids: Vec<u32> = items(&frames).into_iter().map(|l| l.value.id).collect();
    ids.sort_unstable();
    assert_eq!(ids, vec![1, 2]);
    assert!(matches!(terminal(&frames), Frame::End(_)));
}

/// Residence is carried through untouched. The merge must not re-tag a remote
/// item as local (or vice versa) — that tag is the only thing telling a consumer
/// whether a row is offline-capable.
#[tokio::test]
async fn a_merge_preserves_each_items_residence() {
    let frames = run(vec![
        (
            source(1),
            finished_source(vec![(row(1, "local"), Residence::Local)]),
        ),
        (
            source(2),
            finished_source(vec![(
                row(2, "remote"),
                Residence::Remote {
                    generation: GenerationId(7),
                },
            )]),
        ),
    ])
    .await;

    let rows = items(&frames);
    let local = rows.iter().find(|l| l.value.id == 1).expect("row 1");
    let remote = rows.iter().find(|l| l.value.id == 2).expect("row 2");
    assert_eq!(local.residence, Residence::Local);
    assert_eq!(
        remote.residence,
        Residence::Remote {
            generation: GenerationId(7)
        }
    );
}

#[tokio::test]
async fn a_merge_of_no_sources_ends_immediately_and_completely() {
    let frames = run(vec![]).await;
    assert_eq!(frames.len(), 1);
    match terminal(&frames) {
        Frame::End(summary) => {
            assert_eq!(summary.items, 0);
            assert_eq!(summary.complete, Completeness::Complete);
        }
        other => panic!("expected End, got {other:?}"),
    }
}

#[tokio::test]
async fn a_merge_ends_only_once_every_source_has_ended() {
    let (tx_slow, slow) = answer_channel::<Rows>(8, Gen(1));
    let fast = finished_source(vec![(row(1, "fast"), Residence::Local)]);

    let (answer, pump) = merge::<Rows>(Gen(1), vec![(source(1), fast), (source(2), slow)]);
    let pumping = tokio::spawn(pump);

    // The fast source's item arrives while the slow source is still open...
    let first = answer.recv().await.expect("an item before the merge ends");
    assert!(matches!(first, Frame::Item(_)));

    // ...and no terminal frame is produced yet.
    assert!(
        answer.try_recv().is_none(),
        "the merge must not end while a source is still open"
    );

    tx_slow.item(row(2, "slow"), Residence::Local).expect("emit");
    tx_slow.end(Summary::complete(1)).expect("end");

    let rest = collect_rest(&answer).await;
    assert!(matches!(rest.last(), Some(Frame::End(_))));
    pumping.await.expect("pump completes");
}

async fn collect_rest(answer: &Answer<Rows>) -> Vec<Frame<Rows>> {
    let mut out = Vec::new();
    while let Some(frame) = answer.recv().await {
        out.push(frame);
    }
    out
}

// ---------------------------------------------------------------------------
// 2. Progressive delivery — THE property this whole design exists for
// ---------------------------------------------------------------------------

/// The load-bearing test. If this passes trivially because everything was
/// collected first, the merge is not a merge — it is a `Vec` with extra steps,
/// which is exactly what `search.rs:112`'s `try_collect` does today.
#[tokio::test]
async fn items_are_delivered_before_any_source_is_exhausted() {
    let (tx_a, a) = answer_channel::<Rows>(8, Gen(1));
    let (tx_b, b) = answer_channel::<Rows>(8, Gen(1));

    let (answer, pump) = merge::<Rows>(Gen(1), vec![(source(1), a), (source(2), b)]);
    let pumping = tokio::spawn(pump);

    tx_a.item(row(1, "early"), Residence::Local).expect("emit");

    // Neither source has ended. The item must still come through.
    let frame = tokio::time::timeout(Duration::from_secs(5), answer.recv())
        .await
        .expect("an item must arrive before any source ends")
        .expect("a frame");
    match frame {
        Frame::Item(l) => assert_eq!(l.value.id, 1),
        other => panic!("expected the early item, got {other:?}"),
    }

    tx_a.end(Summary::complete(1)).ok();
    tx_b.end(Summary::complete(0)).ok();
    let _ = collect_rest(&answer).await;
    pumping.await.expect("pump completes");
}

/// Sources must be polled **concurrently**. Today `precise_hits` awaits each
/// source in a `for` loop, so source 2 does not start until source 1 finishes —
/// a latency bug independent of streaming. Two sources that each take ~300ms
/// must complete in ~300ms, not ~600ms.
#[tokio::test]
async fn sources_are_polled_concurrently_not_sequentially() {
    fn slow_source(id: u32, delay: Duration) -> Answer<Rows> {
        let (tx, answer) = answer_channel::<Rows>(8, Gen(1));
        tokio::spawn(async move {
            tokio::time::sleep(delay).await;
            tx.item(row(id, "slow"), Residence::Local).ok();
            tx.end(Summary::complete(1)).ok();
        });
        answer
    }

    let started = std::time::Instant::now();
    let frames = run(vec![
        (source(1), slow_source(1, Duration::from_millis(300))),
        (source(2), slow_source(2, Duration::from_millis(300))),
    ])
    .await;
    let elapsed = started.elapsed();

    assert_eq!(items(&frames).len(), 2);
    assert!(
        elapsed < Duration::from_millis(550),
        "two concurrent 300ms sources must finish in ~300ms, took {elapsed:?} — \
         sources are being awaited sequentially"
    );
}

/// A slow source must never hold back a fast one. This is the local-first
/// property: local results paint immediately even when the remote is crawling.
#[tokio::test]
async fn a_slow_source_does_not_delay_a_fast_one() {
    let (tx_slow, slow) = answer_channel::<Rows>(8, Gen(1));
    let fast = finished_source(vec![(row(1, "fast"), Residence::Local)]);

    let (answer, pump) = merge::<Rows>(Gen(1), vec![(source(1), slow), (source(2), fast)]);
    let pumping = tokio::spawn(pump);

    // `slow` is precedence 0 and has produced nothing. The precedence-1 item
    // must not wait on it.
    let frame = tokio::time::timeout(Duration::from_secs(5), answer.recv())
        .await
        .expect("the fast source must not be blocked behind the slow one")
        .expect("a frame");
    assert!(matches!(frame, Frame::Item(_)));

    tx_slow.end(Summary::complete(0)).ok();
    let _ = collect_rest(&answer).await;
    pumping.await.expect("pump completes");
}

// ---------------------------------------------------------------------------
// 3. Overlay-override, as supersede
// ---------------------------------------------------------------------------

/// A duplicate key from a LOWER-precedence source, arriving after a
/// higher-precedence source already claimed it, is suppressed.
#[tokio::test]
async fn a_lower_precedence_duplicate_is_suppressed() {
    let (tx_hi, hi) = answer_channel::<Rows>(8, Gen(1));
    let (tx_lo, lo) = answer_channel::<Rows>(8, Gen(1));

    let (answer, pump) = merge::<Rows>(Gen(1), vec![(source(1), hi), (source(2), lo)]);
    let pumping = tokio::spawn(pump);

    tx_hi.item(row(1, "overlay"), Residence::Local).expect("emit");
    let first = answer.recv().await.expect("the overlay row");
    assert!(matches!(&first, Frame::Item(l) if l.value.body == "overlay"));

    // The base's copy of the same key arrives second and must be dropped.
    tx_lo
        .item(
            row(1, "base"),
            Residence::Remote {
                generation: GenerationId(1),
            },
        )
        .expect("emit");
    tx_hi.end(Summary::complete(1)).ok();
    tx_lo.end(Summary::complete(1)).ok();

    let rest = collect_rest(&answer).await;
    let bodies: Vec<String> = items(&rest).into_iter().map(|l| l.value.body).collect();
    assert!(
        !bodies.contains(&"base".to_owned()),
        "the base's copy must be suppressed once the overlay claimed the key, got {bodies:?}"
    );
    pumping.await.expect("pump completes");
}

/// The converse, and the reason `supersedes` has to exist. Progressive delivery
/// means the base's copy can arrive FIRST. When the overlay's copy shows up
/// later it must be re-emitted so the consumer can replace the row it already
/// painted — otherwise overlay-override silently loses whenever the remote is
/// faster, which is precisely when it matters least to be wrong and most likely
/// to happen.
#[tokio::test]
async fn a_higher_precedence_duplicate_arriving_late_supersedes() {
    let (tx_hi, hi) = answer_channel::<Rows>(8, Gen(1));
    let (tx_lo, lo) = answer_channel::<Rows>(8, Gen(1));

    let (answer, pump) = merge::<Rows>(Gen(1), vec![(source(1), hi), (source(2), lo)]);
    let pumping = tokio::spawn(pump);

    // The base answers first.
    tx_lo
        .item(
            row(1, "base"),
            Residence::Remote {
                generation: GenerationId(1),
            },
        )
        .expect("emit");
    let first = answer.recv().await.expect("the base row");
    assert!(matches!(&first, Frame::Item(l) if l.value.body == "base"));

    // The overlay's copy of the same key arrives late and must be re-emitted.
    tx_hi.item(row(1, "overlay"), Residence::Local).expect("emit");
    tx_hi.end(Summary::complete(1)).ok();
    tx_lo.end(Summary::complete(1)).ok();

    let rest = collect_rest(&answer).await;
    let superseding = items(&rest)
        .into_iter()
        .find(|l| l.value.body == "overlay")
        .expect("the overlay copy must be re-emitted to supersede the base copy");
    assert_eq!(superseding.residence, Residence::Local);
    pumping.await.expect("pump completes");
}

/// Whichever order the two sources happen to answer in, the final state a
/// key-replacing consumer arrives at must be the same. This is what buys back
/// determinism after giving up global ordering.
#[tokio::test]
async fn the_final_state_is_the_same_whichever_source_answers_first() {
    async fn final_body_for(overlay_first: bool) -> String {
        let (tx_hi, hi) = answer_channel::<Rows>(8, Gen(1));
        let (tx_lo, lo) = answer_channel::<Rows>(8, Gen(1));
        let (answer, pump) = merge::<Rows>(Gen(1), vec![(source(1), hi), (source(2), lo)]);
        let pumping = tokio::spawn(pump);

        let emit_hi = || tx_hi.item(row(1, "overlay"), Residence::Local);
        let emit_lo = || {
            tx_lo.item(
                row(1, "base"),
                Residence::Remote {
                    generation: GenerationId(1),
                },
            )
        };
        if overlay_first {
            emit_hi().ok();
            tokio::task::yield_now().await;
            emit_lo().ok();
        } else {
            emit_lo().ok();
            tokio::task::yield_now().await;
            emit_hi().ok();
        }
        tx_hi.end(Summary::complete(1)).ok();
        tx_lo.end(Summary::complete(1)).ok();

        let mut frames = Vec::new();
        while let Some(f) = answer.recv().await {
            frames.push(f);
        }
        pumping.await.expect("pump completes");

        // A consumer that keys by `S::key` and replaces — the GUI's model.
        let mut state = std::collections::HashMap::new();
        for located in items(&frames) {
            state.insert(Rows::key(&located.value), located.value.body.clone());
        }
        state.remove(&1).expect("row 1 present")
    }

    assert_eq!(final_body_for(true).await, "overlay");
    assert_eq!(
        final_body_for(false).await,
        "overlay",
        "the overlay must win regardless of arrival order"
    );
}

// ---------------------------------------------------------------------------
// 4. Degradation — a failing source must not fail the answer
// ---------------------------------------------------------------------------

/// The behavioural fix that makes offline-first honest. Today a remote failure
/// lands in a separate `RemoteStatus` field the view has to remember to render.
/// Here it is a frame on a still-successful stream.
#[tokio::test]
async fn one_source_failing_degrades_the_answer_but_does_not_fail_it() {
    let (tx_bad, bad) = answer_channel::<Rows>(8, Gen(1));
    let good = finished_source(vec![(row(1, "good"), Residence::Local)]);

    let (answer, pump) = merge::<Rows>(Gen(1), vec![(source(1), good), (source(0xBD), bad)]);
    let pumping = tokio::spawn(pump);
    tx_bad.failed(WireError::Backend("qdrant down".into())).ok();

    let mut frames = Vec::new();
    while let Some(f) = answer.recv().await {
        frames.push(f);
    }
    pumping.await.expect("pump completes");

    assert_eq!(items(&frames).len(), 1, "the good source's row survives");

    let degraded = frames
        .iter()
        .find_map(|f| match f {
            Frame::Degraded(d) => Some(d),
            _ => None,
        })
        .expect("a failing source must surface as Degraded");
    assert_eq!(degraded.source, source(0xBD));

    match terminal(&frames) {
        Frame::End(summary) => assert!(
            !summary.is_complete(),
            "an answer that lost a source must report itself Partial, not Complete"
        ),
        other => panic!("expected End (degraded, not failed), got {other:?}"),
    }
}

/// If EVERY source fails there is no answer, and that must be terminal `Failed`
/// rather than a cheerful empty `End` — the exact "zero hits" vs "nothing
/// worked" confusion the typed envelope exists to prevent.
#[tokio::test]
async fn every_source_failing_fails_the_answer() {
    let (tx_a, a) = answer_channel::<Rows>(8, Gen(1));
    let (tx_b, b) = answer_channel::<Rows>(8, Gen(1));

    let (answer, pump) = merge::<Rows>(Gen(1), vec![(source(1), a), (source(2), b)]);
    let pumping = tokio::spawn(pump);
    tx_a.failed(WireError::Timeout).ok();
    tx_b.failed(WireError::Backend("down".into())).ok();

    let mut frames = Vec::new();
    while let Some(f) = answer.recv().await {
        frames.push(f);
    }
    pumping.await.expect("pump completes");

    assert!(
        matches!(terminal(&frames), Frame::Failed(_)),
        "all sources failing must be terminal Failed, got {:?}",
        terminal(&frames)
    );
}

/// A source that reports its own partial coverage makes the merged answer
/// partial too — incompleteness is contagious upward and must not be lost.
#[tokio::test]
async fn a_partial_source_makes_the_merged_answer_partial() {
    let (tx, partial) = answer_channel::<Rows>(8, Gen(1));
    tx.item(row(1, "a"), Residence::Local).ok();
    tx.end(Summary::partial(1, 1, Some(100))).ok();

    let complete = finished_source(vec![(row(2, "b"), Residence::Local)]);

    let frames = run(vec![(source(1), partial), (source(2), complete)]).await;
    match terminal(&frames) {
        Frame::End(summary) => assert!(
            !summary.is_complete(),
            "a partial source must make the merge partial"
        ),
        other => panic!("expected End, got {other:?}"),
    }
}

/// A `Degraded` frame from within a source is forwarded, not swallowed — a
/// source may itself be a federation.
#[tokio::test]
async fn nested_degradation_is_forwarded() {
    let (tx, inner) = answer_channel::<Rows>(8, Gen(1));
    tx.degraded(Degradation {
        source: source(0xEE),
        reason: WireError::Timeout,
    })
    .ok();
    tx.end(Summary::partial(0, 0, None)).ok();

    let frames = run(vec![(source(1), inner)]).await;
    assert!(
        frames
            .iter()
            .any(|f| matches!(f, Frame::Degraded(d) if d.source == source(0xEE))),
        "an inner Degraded must be forwarded with its original source id"
    );
}

// ---------------------------------------------------------------------------
// 5. Notes, counting, cancellation
// ---------------------------------------------------------------------------

#[tokio::test]
async fn notes_from_every_source_are_forwarded() {
    let (tx_a, a) = answer_channel::<Rows>(8, Gen(1));
    tx_a.note(RowNote("from-a".into())).ok();
    tx_a.end(Summary::complete(0)).ok();

    let (tx_b, b) = answer_channel::<Rows>(8, Gen(1));
    tx_b.note(RowNote("from-b".into())).ok();
    tx_b.end(Summary::complete(0)).ok();

    let frames = run(vec![(source(1), a), (source(2), b)]).await;
    let notes: Vec<String> = frames
        .iter()
        .filter_map(|f| match f {
            Frame::Note(RowNote(text)) => Some(text.clone()),
            _ => None,
        })
        .collect();
    assert!(notes.contains(&"from-a".to_owned()));
    assert!(notes.contains(&"from-b".to_owned()));
}

/// `Summary.items` counts item frames the merge actually emitted — suppressed
/// duplicates are not counted, superseding re-emissions are. A consumer
/// cross-checks its own count against this to catch a lossy transport, so it has
/// to describe what was sent, not what was theoretically available.
#[tokio::test]
async fn the_summary_counts_emitted_items_not_source_items() {
    let overlay = finished_source(vec![(row(1, "overlay"), Residence::Local)]);
    let base = finished_source(vec![
        (
            row(1, "dup"),
            Residence::Remote {
                generation: GenerationId(1),
            },
        ),
        (
            row(2, "unique"),
            Residence::Remote {
                generation: GenerationId(1),
            },
        ),
    ]);

    let frames = run(vec![(source(1), overlay), (source(2), base)]).await;
    let emitted = items(&frames).len() as u64;
    match terminal(&frames) {
        Frame::End(summary) => assert_eq!(
            summary.items, emitted,
            "the summary must match the number of item frames actually sent"
        ),
        other => panic!("expected End, got {other:?}"),
    }
}

/// Dropping the merged answer must stop the merge and release every source —
/// this is what makes search-as-you-type cancellation actually free the remote
/// request rather than leaving it running.
#[tokio::test]
async fn dropping_the_merged_answer_cancels_every_source() {
    let (tx_a, a) = answer_channel::<Rows>(8, Gen(1));
    let (tx_b, b) = answer_channel::<Rows>(8, Gen(1));

    let (answer, pump) = merge::<Rows>(Gen(1), vec![(source(1), a), (source(2), b)]);
    let pumping = tokio::spawn(pump);

    assert!(!tx_a.is_cancelled());
    assert!(!tx_b.is_cancelled());

    drop(answer);
    pumping.await.expect("the pump must finish when the answer is dropped");

    assert!(
        tx_a.is_cancelled() && tx_b.is_cancelled(),
        "dropping the merged answer must cancel every upstream source"
    );
}

/// A merge is itself an `Answer`, so it can be an input to another merge. The
/// server federates its sources; the GUI federates (that server, local). Nothing
/// in the type should notice.
#[tokio::test]
async fn a_merge_can_be_a_source_of_another_merge() {
    let inner_a = finished_source(vec![(row(1, "a"), Residence::Local)]);
    let inner_b = finished_source(vec![(row(2, "b"), Residence::Local)]);
    let (inner, inner_pump) = merge::<Rows>(Gen(1), vec![(source(1), inner_a), (source(2), inner_b)]);
    let inner_pumping = tokio::spawn(inner_pump);

    let outer_c = finished_source(vec![(
        row(3, "c"),
        Residence::Remote {
            generation: GenerationId(1),
        },
    )]);

    let frames = run(vec![(source(3), inner), (source(4), outer_c)]).await;
    inner_pumping.await.expect("inner pump completes");

    let mut ids: Vec<u32> = items(&frames).into_iter().map(|l| l.value.id).collect();
    ids.sort_unstable();
    assert_eq!(ids, vec![1, 2, 3]);
}

/// Suppression must not be O(n²): the merge keys on `S::Key`, which the
/// `Surface` trait requires to be `Hash + Eq` for exactly this reason.
#[tokio::test]
async fn suppression_scales_to_a_large_duplicate_set() {
    let overlay = finished_source(
        (0..2_000u32)
            .map(|id| (row(id, "overlay"), Residence::Local))
            .collect(),
    );
    let base = finished_source(
        (0..2_000u32)
            .map(|id| {
                (
                    row(id, "base"),
                    Residence::Remote {
                        generation: GenerationId(1),
                    },
                )
            })
            .collect(),
    );

    let started = std::time::Instant::now();
    let frames = run(vec![(source(1), overlay), (source(2), base)]).await;
    let elapsed = started.elapsed();

    assert_eq!(
        items(&frames).len(),
        2_000,
        "every base row duplicates an overlay row and must be suppressed"
    );
    assert!(
        elapsed < Duration::from_secs(5),
        "4000 items took {elapsed:?} — suppression is not using the hash key"
    );
}

// ---------------------------------------------------------------------------
// 5b. Bounding the answer — distinct keys, not emitted frames
// ---------------------------------------------------------------------------
//
// Giving up the global sort was a deliberate trade. Giving up the *page size*
// was not: with no post-merge truncation, a federation of N sources each capped
// at `limit` emits up to `N × limit` items, so a caller asking for 30 gets 90.
//
// The cap counts **distinct keys**, not emitted frames, because a supersede
// (§3) re-emits a key the consumer already holds — it replaces a row rather than
// adding one. Counting frames would let a run of supersedes silently starve out
// genuinely new results.

#[tokio::test]
async fn a_bounded_merge_admits_at_most_limit_distinct_keys() {
    let a = finished_source((0..10u32).map(|id| (row(id, "a"), Residence::Local)).collect());
    let b = finished_source(
        (100..110u32)
            .map(|id| {
                (
                    row(id, "b"),
                    Residence::Remote {
                        generation: GenerationId(1),
                    },
                )
            })
            .collect(),
    );

    let (answer, pump) = merge_bounded::<Rows>(
        Gen(1),
        vec![(source(1), a), (source(2), b)],
        std::num::NonZeroUsize::new(6).unwrap(),
    );
    let pumping = tokio::spawn(pump);
    let mut frames = Vec::new();
    while let Some(f) = answer.recv().await {
        frames.push(f);
    }
    pumping.await.expect("pump completes");

    let distinct: std::collections::HashSet<u32> = items(&frames)
        .into_iter()
        .map(|l| Rows::key(&l.value))
        .collect();
    assert_eq!(
        distinct.len(),
        6,
        "a bounded merge must admit exactly `limit` distinct keys, got {distinct:?}"
    );
}

/// A supersede must still get through after the cap is reached — the consumer
/// already has that row and is waiting to be told the better copy exists.
/// Refusing it would leave a stale row on screen permanently.
#[tokio::test]
async fn a_supersede_is_admitted_even_once_the_limit_is_reached() {
    let (tx_hi, hi) = answer_channel::<Rows>(16, Gen(1));
    let (tx_lo, lo) = answer_channel::<Rows>(16, Gen(1));

    let (answer, pump) = merge_bounded::<Rows>(
        Gen(1),
        vec![(source(1), hi), (source(2), lo)],
        std::num::NonZeroUsize::new(2).unwrap(),
    );
    let pumping = tokio::spawn(pump);

    // The base fills the whole budget first. Sources are polled concurrently,
    // so "emitted first" is not "admitted first" — drain the two base rows off
    // the merged answer before the overlay speaks, or the ordering this test is
    // about is decided by a race rather than by the merge.
    for id in 0..2u32 {
        tx_lo
            .item(
                row(id, "base"),
                Residence::Remote {
                    generation: GenerationId(1),
                },
            )
            .expect("emit");
    }
    let mut frames = Vec::new();
    for _ in 0..2 {
        frames.push(answer.recv().await.expect("a base row"));
    }
    assert_eq!(items(&frames).len(), 2, "budget is now spent");

    // Now the overlay's better copy of key 0 arrives late, plus a brand-new
    // key 99 that must be refused because the budget is spent.
    tx_hi.item(row(0, "overlay"), Residence::Local).expect("emit");
    tx_hi.item(row(99, "overflow"), Residence::Local).expect("emit");
    tx_hi.end(Summary::complete(2)).ok();
    tx_lo.end(Summary::complete(2)).ok();

    while let Some(f) = answer.recv().await {
        frames.push(f);
    }
    pumping.await.expect("pump completes");

    let bodies: Vec<String> = items(&frames).into_iter().map(|l| l.value.body).collect();
    assert!(
        bodies.contains(&"overlay".to_owned()),
        "a supersede of an already-admitted key must pass the cap, got {bodies:?}"
    );
    assert!(
        !bodies.contains(&"overflow".to_owned()),
        "a NEW key must be refused once the cap is spent, got {bodies:?}"
    );

    let distinct: std::collections::HashSet<u32> = items(&frames)
        .into_iter()
        .map(|l| Rows::key(&l.value))
        .collect();
    assert_eq!(distinct.len(), 2, "still exactly `limit` distinct keys");
}

/// Hitting the cap makes the answer incomplete — there were more results than
/// were delivered, and a consumer that renders "3 results" must be able to tell
/// that from "3 results, and we stopped looking".
#[tokio::test]
async fn hitting_the_limit_reports_the_answer_as_partial() {
    let a = finished_source((0..10u32).map(|id| (row(id, "a"), Residence::Local)).collect());
    let (answer, pump) = merge_bounded::<Rows>(
        Gen(1),
        vec![(source(1), a)],
        std::num::NonZeroUsize::new(3).unwrap(),
    );
    let pumping = tokio::spawn(pump);
    let mut frames = Vec::new();
    while let Some(f) = answer.recv().await {
        frames.push(f);
    }
    pumping.await.expect("pump completes");

    match terminal(&frames) {
        Frame::End(summary) => assert!(
            !summary.is_complete(),
            "a truncated answer must report Partial, not Complete"
        ),
        other => panic!("expected End, got {other:?}"),
    }
}

/// An answer that fits inside its budget is still Complete — the cap must not
/// make every bounded merge claim it was truncated.
#[tokio::test]
async fn staying_under_the_limit_is_still_complete() {
    let a = finished_source((0..2u32).map(|id| (row(id, "a"), Residence::Local)).collect());
    let (answer, pump) = merge_bounded::<Rows>(
        Gen(1),
        vec![(source(1), a)],
        std::num::NonZeroUsize::new(10).unwrap(),
    );
    let pumping = tokio::spawn(pump);
    let mut frames = Vec::new();
    while let Some(f) = answer.recv().await {
        frames.push(f);
    }
    pumping.await.expect("pump completes");

    match terminal(&frames) {
        Frame::End(summary) => assert!(summary.is_complete(), "under budget is complete"),
        other => panic!("expected End, got {other:?}"),
    }
}

/// The unbounded `merge` must keep working — it is what nested/inner merges use,
/// where the outer merge owns the budget.
#[tokio::test]
async fn the_unbounded_merge_still_admits_everything() {
    let a = finished_source((0..25u32).map(|id| (row(id, "a"), Residence::Local)).collect());
    let frames = run(vec![(source(1), a)]).await;
    assert_eq!(items(&frames).len(), 25);
}

// ---------------------------------------------------------------------------
// 6. Sanity: the helpers themselves
// ---------------------------------------------------------------------------

/// Guards the tests above: if `finished_source` silently produced nothing, most
/// assertions here would pass vacuously.
#[tokio::test]
async fn the_test_helper_actually_produces_items() {
    let answer = finished_source(vec![(row(9, "x"), Residence::Local)]);
    let mut seen = 0;
    while let Some(frame) = answer.recv().await {
        if matches!(frame, Frame::Item(_)) {
            seen += 1;
        }
    }
    assert_eq!(seen, 1);
}

#[allow(dead_code)]
fn assert_types_are_send() {
    fn is_send<T: Send>() {}
    is_send::<Answer<Rows>>();
    is_send::<Emitter<Rows>>();
    is_send::<Frame<Rows>>();
    let _ = (AtomicBool::new(false), AtomicUsize::new(0), Arc::new(()));
}
