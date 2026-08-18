//! Red-first specification: **`answer_channel`'s `capacity` must stop being a
//! lie** (contract task 9).
//!
//! # The defect
//!
//! `answer_channel(capacity, generation)` validates `capacity > 0`, documents
//! it as "the caller's expected burst size" — and then constructs
//! `flume::unbounded()` and never mentions `capacity` again
//! (`surface.rs:845-863`). Every call site therefore states a bound that
//! nothing enforces: `merge` asks for 256, the tests ask for 8, and all of them
//! get an unbounded queue. A producer that outruns its consumer grows the queue
//! until the process dies, and the parameter that looks like it would prevent
//! that does nothing at all.
//!
//! This is not hypothetical for this codebase. The producers are tight loops
//! over in-memory collections (a scored-symbol vector, a section's hit list);
//! the consumers are a GUI redraw loop and an axum body writer behind a socket.
//! The producer side is the fast side by construction.
//!
//! # Why the fix is not "make it `flume::bounded`"
//!
//! Every emit funnels through `Emitter::push`, which is a plain **non-async
//! fn** — and that is load-bearing, not incidental. `Serve::serve` is called
//! from lindsey's foreground thread, which has no Tokio reactor (only GPUI's
//! executor). A blocking `send` on a full channel would stall the UI thread on
//! a slow consumer, which is precisely the class of stall this whole contract
//! exists to design out. Swapping `unbounded()` for `bounded()` under an
//! unchanged `push` converts an out-of-memory bug into a deadlock, which is
//! worse: it is silent and it takes the foreground thread with it.
//!
//! # The policy this file pins
//!
//! Two emit paths, because there are genuinely two kinds of producer:
//!
//! * **`push` and friends stay non-blocking.** On a full channel the frame is
//!   dropped and the call returns [`EmitError::Lagged`]. Memory is bounded, no
//!   thread ever stalls, and a sync producer with no runtime keeps working.
//! * **`push_async` awaits room.** Async producers (the server's search tasks,
//!   the merge pump) get *real* backpressure with **no data loss** — the queue
//!   fills, the producer parks, and the stall propagates all the way back to
//!   the TCP socket, so a slow HTTP client genuinely slows the search that
//!   feeds it instead of buffering the whole result set in the server's heap.
//!
//! The two are not alternatives to choose between once; they are the correct
//! answer for two different call sites, and both exist for that reason.
//!
//! # Dropping is only acceptable because it is *reported*
//!
//! A dropped item is a missing search result. Silently returning fewer rows
//! than were found would be a correctness regression dressed up as a memory
//! fix, and exactly the failure mode
//! [`count-based-tests-cannot-see-field-loss`] warns about — the answer still
//! *looks* like an answer.
//!
//! So honesty is made **structural rather than a producer responsibility**: the
//! `Emitter` counts what it dropped, and `end()` rewrites its own summary to
//! [`Completeness::Partial`] when that count is non-zero. A producer cannot
//! forget to do this, cannot get it wrong, and cannot report `Complete` over a
//! truncated answer even by explicitly asking to — `end(Summary::complete(n))`
//! after an overflow must still arrive as `Partial`. This is the same
//! discipline `terminated`/[`EmitError::Finished`] already applies to the
//! terminal frame: the invariant lives in the type, not in every caller.
//!
//! # The reserve
//!
//! A bounded channel has a bootstrapping problem: the frame that *reports* the
//! overflow cannot be sent if the overflow filled the channel. So the queue is
//! built with headroom above `capacity`, and **items and notes are gated at
//! `capacity` while terminal and degradation frames may use the reserve**. An
//! answer must always be able to end, and always be able to say it degraded,
//! no matter how badly the consumer fell behind. Without this the fix would
//! turn a memory leak into a hang at the terminal frame.
//!
//! **Do not weaken these tests to make them pass.** In particular, do not make
//! `push` blocking to satisfy the no-loss tests, and do not drop the
//! auto-`Partial` rule to satisfy the summary tests — those two properties are
//! the entire point.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use heart::access::SourceId;
use heart::stream::WireError;
use heart::surface::{
    Completeness, Degradation, EmitError, Frame, Gen, Residence, Summary, answer_channel,
};
use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// Test surface
// ---------------------------------------------------------------------------

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

// ---------------------------------------------------------------------------
// 1. The bound is real
// ---------------------------------------------------------------------------

/// THE test. A producer that ignores every error and emits far more than
/// `capacity` into a channel nobody is draining must not grow without bound.
///
/// Deliberately emits 100_000 into a capacity of 4: if `capacity` is still
/// ignored this queues 100_000 frames and the assertion on queue length fails
/// by four orders of magnitude, which is unmistakable in the failure output.
#[test]
fn capacity_actually_bounds_the_queue() {
    let (tx, answer) = answer_channel::<Rows>(4, Gen(1));

    for id in 0..100_000 {
        let _ = tx.item(row(id), Residence::Local);
    }

    // Drain and count what is actually retained. Nothing is reading
    // concurrently, so every frame still queued is sitting in memory.
    let mut retained = 0usize;
    while answer.try_recv().is_some() {
        retained += 1;
    }

    assert!(
        retained <= 8,
        "capacity 4 (plus a small terminal reserve) must bound the queue, but \
         {retained} frames were retained — `capacity` is still being ignored"
    );
}

/// The bound must hold for notes too, or a chatty progress reporter reopens the
/// same leak through a different method.
#[test]
fn notes_are_bounded_by_the_same_budget() {
    let (tx, answer) = answer_channel::<Rows>(4, Gen(1));

    for i in 0..10_000 {
        let _ = tx.note(RowNote {
            stage: format!("stage-{i}"),
        });
    }

    let mut retained = 0usize;
    while answer.try_recv().is_some() {
        retained += 1;
    }

    assert!(
        retained <= 8,
        "notes must share the item budget; {retained} were retained"
    );
}

// ---------------------------------------------------------------------------
// 2. Overflow is reported, never silent
// ---------------------------------------------------------------------------

/// The distinction that keeps this from being a correctness regression: a
/// dropped frame is an *error return*, not a success.
#[test]
fn an_overflowing_emit_returns_lagged() {
    let (tx, _answer) = answer_channel::<Rows>(2, Gen(1));

    assert_eq!(tx.item(row(0), Residence::Local), Ok(()));
    assert_eq!(tx.item(row(1), Residence::Local), Ok(()));

    assert_eq!(
        tx.item(row(2), Residence::Local),
        Err(EmitError::Lagged),
        "the frame past capacity must be reported as dropped, not accepted"
    );
}

/// `Lagged` must be its own variant. Folding it into `Cancelled` would tell a
/// producer to stop working when the consumer is merely slow, and folding it
/// into `Finished` would claim the answer had ended when it has not — both are
/// lies a `match` at a call site would act on.
#[test]
fn lagged_is_distinct_from_cancelled_and_finished() {
    assert_ne!(EmitError::Lagged, EmitError::Cancelled);
    assert_ne!(EmitError::Lagged, EmitError::Finished);
}

/// A lagged emit says nothing about the consumer being gone — the producer
/// should keep going, and the very next emit must succeed once room appears.
#[test]
fn a_lagged_emitter_recovers_when_the_consumer_drains() {
    let (tx, answer) = answer_channel::<Rows>(2, Gen(1));

    assert_eq!(tx.item(row(0), Residence::Local), Ok(()));
    assert_eq!(tx.item(row(1), Residence::Local), Ok(()));
    assert_eq!(tx.item(row(2), Residence::Local), Err(EmitError::Lagged));

    // Consumer catches up.
    assert!(answer.try_recv().is_some());

    assert_eq!(
        tx.item(row(3), Residence::Local),
        Ok(()),
        "lag is transient; emitting must resume the moment room appears"
    );
}

// ---------------------------------------------------------------------------
// 3. Non-blocking is preserved
// ---------------------------------------------------------------------------

/// The property that makes this safe for lindsey's foreground thread: emitting
/// into a full channel from a plain thread with **no async runtime at all**
/// must return promptly rather than park.
///
/// If `push` were made blocking to avoid dropping frames, this test hangs
/// forever — which is the failure mode it exists to catch.
#[test]
fn push_never_blocks_the_calling_thread() {
    let (tx, _answer) = answer_channel::<Rows>(1, Gen(1));

    let start = Instant::now();
    for id in 0..50_000 {
        let _ = tx.item(row(id), Residence::Local);
    }
    let elapsed = start.elapsed();

    assert!(
        elapsed < Duration::from_secs(5),
        "50k non-blocking emits took {elapsed:?} — `push` appears to be blocking \
         on a full channel, which would stall the GUI foreground thread"
    );
}

// ---------------------------------------------------------------------------
// 4. The reserve: an answer can always end, and always report degradation
// ---------------------------------------------------------------------------

/// Without a reserve, the terminal frame cannot be sent once items have filled
/// the channel, and the consumer waits forever for an end that structurally
/// cannot arrive. This converts the old memory leak into a hang, so it must be
/// pinned explicitly.
#[test]
fn a_terminal_frame_fits_even_when_the_channel_is_saturated() {
    let (tx, answer) = answer_channel::<Rows>(2, Gen(1));

    for id in 0..1_000 {
        let _ = tx.item(row(id), Residence::Local);
    }

    assert_eq!(
        tx.end(Summary::complete(1_000)).map_err(|_| ()),
        Ok(()),
        "a saturated channel must still accept its terminal frame"
    );

    // And the consumer must actually be able to reach it.
    let mut saw_terminal = false;
    while let Some(frame) = answer.try_recv() {
        if matches!(frame, Frame::End(_)) {
            saw_terminal = true;
        }
    }
    assert!(
        saw_terminal,
        "the terminal frame must be reachable by the consumer, not stranded \
         behind dropped items"
    );
}

/// Same argument for `Failed`: an answer must be able to report that it broke
/// even if it broke *because* it was producing too fast.
#[test]
fn a_failure_frame_fits_even_when_the_channel_is_saturated() {
    let (tx, _answer) = answer_channel::<Rows>(2, Gen(1));

    for id in 0..1_000 {
        let _ = tx.item(row(id), Residence::Local);
    }

    assert!(
        tx.failed(WireError::Backend("boom".into())).is_ok(),
        "a saturated channel must still accept a failure frame"
    );
}

/// `Degraded` is the contract's honesty channel — it is how a partial answer
/// explains itself. It must never be the frame that gets dropped.
#[test]
fn a_degradation_fits_even_when_the_channel_is_saturated() {
    let (tx, _answer) = answer_channel::<Rows>(2, Gen(1));

    for id in 0..1_000 {
        let _ = tx.item(row(id), Residence::Local);
    }

    assert!(
        tx.degraded(Degradation {
            source: source(1),
            reason: WireError::Backend("upstream vanished".into()),
        })
        .is_ok(),
        "a saturated channel must still accept a degradation notice"
    );
}

/// The reserve is for terminal and degradation frames only. If items could
/// spend it, the reserve would already be gone by the time it was needed —
/// which is the same as not having one.
#[test]
fn items_cannot_spend_the_terminal_reserve() {
    let (tx, answer) = answer_channel::<Rows>(2, Gen(1));

    assert_eq!(tx.item(row(0), Residence::Local), Ok(()));
    assert_eq!(tx.item(row(1), Residence::Local), Ok(()));
    assert_eq!(
        tx.item(row(2), Residence::Local),
        Err(EmitError::Lagged),
        "items are gated at `capacity`, below the physical queue size"
    );

    // …and the reserve is still there, unspent, for the frame that needs it.
    assert!(tx.end(Summary::complete(2)).is_ok());
    drop(answer);
}

// ---------------------------------------------------------------------------
// 5. A truncated answer cannot claim to be complete
// ---------------------------------------------------------------------------

/// THE honesty test. The producer explicitly asks to report `Complete`; the
/// emitter must refuse, because it knows it dropped frames and the producer
/// does not.
#[test]
fn a_summary_cannot_report_complete_after_an_overflow() {
    let (tx, answer) = answer_channel::<Rows>(2, Gen(1));

    for id in 0..100 {
        let _ = tx.item(row(id), Residence::Local);
    }
    // The producer believes it delivered all 100.
    tx.end(Summary::complete(100)).expect("terminal must fit");

    let mut summary = None;
    while let Some(frame) = answer.try_recv() {
        if let Frame::End(s) = frame {
            summary = Some(s);
        }
    }

    let summary = summary.expect("the answer must end");
    assert!(
        !summary.is_complete(),
        "98 of 100 items were dropped; reporting `Complete` would make a \
         truncated answer indistinguishable from a whole one"
    );
}

/// Partiality must be *quantified*, not merely flagged: a consumer deciding
/// whether to show "showing 2 of 100" needs both numbers.
#[test]
fn an_overflowed_summary_reports_delivered_and_attempted() {
    let (tx, answer) = answer_channel::<Rows>(2, Gen(1));

    for id in 0..100 {
        let _ = tx.item(row(id), Residence::Local);
    }
    tx.end(Summary::complete(100)).expect("terminal must fit");

    let mut delivered_items = 0u64;
    let mut summary = None;
    while let Some(frame) = answer.try_recv() {
        match frame {
            Frame::Item(_) => delivered_items += 1,
            Frame::End(s) => summary = Some(s),
            _ => {}
        }
    }

    let summary = summary.expect("the answer must end");
    assert_eq!(
        summary.items, delivered_items,
        "`Summary::items` must count frames the consumer actually received, \
         not frames the producer attempted"
    );
    match summary.complete {
        Completeness::Partial { covered, total } => {
            assert_eq!(covered, delivered_items, "covered == what arrived");
            assert_eq!(
                total,
                Some(100),
                "total == what the producer attempted, so a consumer can render \
                 `covered of total`"
            );
        }
        other => unreachable!("asserted non-complete above, got {other:?}"),
    }
}

/// The other side of the same coin: an answer that dropped nothing must be
/// reported verbatim. A fix that marks everything `Partial` defensively would
/// pass every test above and make `Completeness` useless.
#[test]
fn a_summary_with_no_overflow_passes_through_untouched() {
    let (tx, answer) = answer_channel::<Rows>(64, Gen(1));

    for id in 0..10 {
        tx.item(row(id), Residence::Local).expect("well within capacity");
    }
    tx.end(Summary::complete(10)).expect("terminal must fit");

    let mut summary = None;
    while let Some(frame) = answer.try_recv() {
        if let Frame::End(s) = frame {
            summary = Some(s);
        }
    }

    assert_eq!(
        summary.expect("the answer must end"),
        Summary::complete(10),
        "no frames were dropped, so the producer's own summary must survive \
         verbatim — including its `Complete`"
    );
}

/// A producer that deliberately reports `Partial` must keep *its* numbers when
/// nothing overflowed. The emitter only overrides what it knows better.
#[test]
fn a_producer_declared_partial_summary_is_preserved() {
    let (tx, answer) = answer_channel::<Rows>(64, Gen(1));

    tx.item(row(0), Residence::Local).expect("within capacity");
    tx.end(Summary::partial(1, 1, Some(9_000)))
        .expect("terminal must fit");

    let mut summary = None;
    while let Some(frame) = answer.try_recv() {
        if let Frame::End(s) = frame {
            summary = Some(s);
        }
    }

    assert_eq!(
        summary.expect("the answer must end"),
        Summary::partial(1, 1, Some(9_000)),
        "the emitter must not overwrite a partiality the producer knows more \
         about than it does"
    );
}

// ---------------------------------------------------------------------------
// 6. Ordering is preserved under overflow
// ---------------------------------------------------------------------------

/// Dropping must not reorder. A consumer rendering a progressively-merged list
/// relies on arrival order being emission order; silently permuting it under
/// load would be a far subtler bug than losing rows.
#[test]
fn overflow_drops_frames_but_never_reorders_them() {
    let (tx, answer) = answer_channel::<Rows>(4, Gen(1));

    // Interleave emission and draining so the channel repeatedly fills and
    // empties — the case where a naive implementation could let a later frame
    // overtake an earlier one.
    let mut received = Vec::new();
    for id in 0..500 {
        let _ = tx.item(row(id), Residence::Local);
        if id % 3 == 0 {
            if let Some(Frame::Item(located)) = answer.try_recv() {
                received.push(located.value.id);
            }
        }
    }
    while let Some(frame) = answer.try_recv() {
        if let Frame::Item(located) = frame {
            received.push(located.value.id);
        }
    }

    assert!(
        received.windows(2).all(|w| w[0] < w[1]),
        "ids arrived out of emission order: {received:?}"
    );
}

// ---------------------------------------------------------------------------
// 7. `push_async` — real backpressure, no loss
// ---------------------------------------------------------------------------

/// The no-loss path. An async producer emitting 1_000 items through a capacity
/// of 4 must deliver **all 1_000** — it parks when the queue is full and
/// resumes as the consumer drains, rather than dropping the excess.
///
/// This is what makes the server path correct: search results are not
/// discretionary, and the server has a reactor to park on.
#[tokio::test]
async fn push_async_delivers_every_item_through_a_small_channel() {
    let (tx, answer) = answer_channel::<Rows>(4, Gen(1));

    let producer = tokio::spawn(async move {
        for id in 0..1_000 {
            tx.item_async(row(id), Residence::Local)
                .await
                .expect("an awaiting emit must not drop");
        }
        tx.end(Summary::complete(1_000)).expect("terminal must fit");
    });

    let mut ids = Vec::new();
    let mut summary = None;
    while let Some(frame) = answer.recv().await {
        match frame {
            Frame::Item(located) => ids.push(located.value.id),
            Frame::End(s) => summary = Some(s),
            _ => {}
        }
    }
    producer.await.expect("producer must not panic");

    assert_eq!(ids, (0..1_000).collect::<Vec<_>>(), "no item may be lost");
    assert_eq!(
        summary.expect("the answer must end"),
        Summary::complete(1_000),
        "nothing was dropped, so the answer is genuinely complete"
    );
}

/// Backpressure means the producer is *actually* held back — not that it races
/// ahead into a secretly-unbounded buffer. With a consumer that sleeps between
/// reads, the producer must not be able to finish before the consumer has taken
/// most of the items.
#[tokio::test]
async fn push_async_parks_the_producer_until_the_consumer_drains() {
    let (tx, answer) = answer_channel::<Rows>(2, Gen(1));

    let emitted = Arc::new(AtomicUsize::new(0));
    let emitted_in_task = Arc::clone(&emitted);

    let producer = tokio::spawn(async move {
        for id in 0..20 {
            tx.item_async(row(id), Residence::Local)
                .await
                .expect("an awaiting emit must not drop");
            emitted_in_task.fetch_add(1, Ordering::SeqCst);
        }
        let _ = tx.end(Summary::complete(20));
    });

    // Take exactly one frame, then look at how far the producer got. With a
    // capacity of 2 it cannot have run away.
    let _ = answer.recv().await;
    tokio::time::sleep(Duration::from_millis(50)).await;

    let ahead = emitted.load(Ordering::SeqCst);
    assert!(
        ahead <= 6,
        "producer emitted {ahead} items while the consumer took 1 — a capacity \
         of 2 is not holding it back, so the channel is still effectively \
         unbounded"
    );

    while answer.recv().await.is_some() {}
    producer.await.expect("producer must not panic");
}

/// A parked producer must be woken by cancellation, not left waiting forever
/// for room in a channel whose consumer is gone. Without this, dropping an
/// `Answer` (which search-as-you-type does on every keystroke) leaks a task per
/// abandoned query.
#[tokio::test]
async fn push_async_returns_cancelled_when_the_answer_is_dropped() {
    let (tx, answer) = answer_channel::<Rows>(1, Gen(1));

    // Saturate, so the next emit must park.
    let _ = tx.item(row(0), Residence::Local);

    let producer = tokio::spawn(async move { tx.item_async(row(1), Residence::Local).await });

    tokio::time::sleep(Duration::from_millis(20)).await;
    drop(answer);

    let outcome = tokio::time::timeout(Duration::from_secs(5), producer)
        .await
        .expect("a parked producer must be woken by cancellation, not hang")
        .expect("producer must not panic");

    assert_eq!(
        outcome,
        Err(EmitError::Cancelled),
        "a parked emit resolves to Cancelled once the consumer goes away"
    );
}

/// `push_async` must respect the terminal rule exactly as `push` does —
/// awaiting must not become a way to sneak a frame past a finished answer.
#[tokio::test]
async fn push_async_is_rejected_after_a_terminal_frame() {
    let (tx, _answer) = answer_channel::<Rows>(8, Gen(1));

    tx.end(Summary::complete(0)).expect("terminal must fit");

    assert_eq!(
        tx.item_async(row(0), Residence::Local).await,
        Err(EmitError::Finished),
        "awaiting must not bypass the already-ended check"
    );
}

// ---------------------------------------------------------------------------
// 8. The merge pump must not deadlock behind a slow consumer
// ---------------------------------------------------------------------------

/// `merge` is itself a producer into a bounded `answer_channel`, and it forwards
/// frames it did not create. Now that its output is genuinely bounded, a slow
/// downstream consumer must slow the pump — never wedge it, and never make it
/// spin dropping everything.
///
/// The pump is async, so it should be applying real backpressure: every item
/// from both sources must survive.
#[tokio::test]
async fn a_slow_consumer_slows_the_merge_without_losing_items() {
    use heart::surface::merge;

    let (tx_a, a) = answer_channel::<Rows>(8, Gen(1));
    let (tx_b, b) = answer_channel::<Rows>(8, Gen(1));

    tokio::spawn(async move {
        for id in 0..300 {
            let _ = tx_a.item_async(row(id * 2), Residence::Local).await;
        }
        let _ = tx_a.end(Summary::complete(300));
    });
    tokio::spawn(async move {
        for id in 0..300 {
            let _ = tx_b
                .item_async(row(id * 2 + 1), Residence::Remote { generation: heart::surface::GenerationId(1) })
                .await;
        }
        let _ = tx_b.end(Summary::complete(300));
    });

    let (merged, pump) = merge(Gen(1), vec![(source(1), a), (source(2), b)]);
    tokio::spawn(pump);

    let mut seen = 0usize;
    while let Some(frame) = merged.recv().await {
        if matches!(frame, Frame::Item(_)) {
            seen += 1;
            // A consumer that is deliberately slow relative to the producers.
            if seen % 50 == 0 {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        }
    }

    assert_eq!(
        seen, 600,
        "the merge must apply backpressure to its sources rather than dropping \
         frames its consumer was too slow to take"
    );
}
