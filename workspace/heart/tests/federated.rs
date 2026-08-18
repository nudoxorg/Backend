//! Red-first specification: **`Federated<S>`** — the type that makes the
//! local/remote split invisible (contract §2, task S4).
//!
//! # What this closes
//!
//! Today lindsey (the GUI) has two entirely separate search paths, and the
//! separation is visible in every layer:
//!
//! * **Two call paths.** Local search goes through `SearchEngine::search` →
//!   `flume::Receiver<SearchEvent>`, drained continuously
//!   (`gui/src/stores/search.rs:349`). Remote search is a *different* method,
//!   `search_remote` (`gui/src/stores/search.rs:388-477`), which spins up a
//!   private single-threaded Tokio runtime per call just to drive one
//!   `reqwest` future, and returns one `Vec` at the end.
//! * **Two item types and two lowering functions.** `PreparedRow::ingest` for
//!   local vs `PreparedRow::ingest_remote` for remote
//!   (`gui/src/stores/search_model.rs:336-377` and `414-446`).
//! * **Two renderings.** Local hits render through the shared row machinery;
//!   remote hits render as a separate, plainer list inside the zero-hit empty
//!   state (`gui/src/views/omni_search.rs:2465-2502`).
//! * **A user-visible button.** "Search remote INDEX"
//!   (`gui/src/views/omni_search.rs:2416`) — the user is asked to know, and
//!   care, which plane an answer might come from.
//!
//! `Federated<S>` replaces all of it with one `Arc<dyn Serve<S>>` that happens
//! to be several sources underneath. The caller cannot tell — not because it is
//! polite not to look, but because there is no method to look with.
//!
//! # The design problem this type actually solves: who drives the pump
//!
//! [`merge`] deliberately does not spawn anything. It returns a `MergePump` the
//! caller must drive, precisely so it can stay runtime-agnostic: the index
//! server hands the pump to `tokio::spawn`, and lindsey — which has **no Tokio
//! reactor at all**, only GPUI's executor (`gui/src/bridge/mod.rs:17`: "no
//! `block_on`", and the whole app's only `tokio::` usage is the throwaway
//! runtime this refactor deletes) — must hand it to `cx.background_executor()`.
//!
//! But `Serve::serve` returns `Answer<S>` and nothing else. It cannot return a
//! pump for the caller to spawn without putting the local/remote seam straight
//! back into the caller's face — a `Serve` impl that needs special handling is
//! not a transparent one.
//!
//! So the executor is **injected once, at construction**: `Federated::new`
//! takes a spawn function. The server passes `tokio::spawn`; lindsey passes a
//! closure over its background executor. After that, `serve` is an ordinary
//! `Serve<S>` call with no runtime assumptions anywhere in its signature. This
//! is the one piece of knowledge that genuinely differs between hosts, named
//! once, in one place, instead of leaking into every call site.
//!
//! # Precedence, and why it is not "remote is better"
//!
//! Sources are supplied in precedence order, highest first. Precedence decides
//! only which copy of a **duplicate** survives — it never reorders or delays
//! delivery, because the merge is progressive (the product decision recorded in
//! the contract: drop the global sort, show rows as they arrive). A lower-
//! precedence source that answers first still paints first; a higher-precedence
//! duplicate arriving later supersedes it in place via [`Surface::fuse`].
//!
//! **Do not weaken these tests to make them pass.** In particular, do not add a
//! method that reveals which source produced an item, and do not make
//! `Federated::serve` block, spawn a runtime, or require one.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use heart::access::SourceId;
use heart::stream::WireError;
use heart::surface::{
    Answer, Emitter, Federated, Frame, Gen, Residence, Serve, Summary, Surface as _,
    answer_channel,
};
use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// Test surface
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct Row {
    id: u32,
    /// Present only on "rich" copies — the fidelity axis `fuse` backfills.
    detail: Option<String>,
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

    /// Backfill `detail`, never erase it — the same rule `Symbols::fuse`
    /// applies to `signature`.
    fn fuse(winner: Self::Item, loser: Self::Item) -> Self::Item {
        Row {
            detail: winner.detail.or(loser.detail),
            ..winner
        }
    }
}

fn bare(id: u32) -> Row {
    Row { id, detail: None }
}

fn rich(id: u32, detail: &str) -> Row {
    Row {
        id,
        detail: Some(detail.to_owned()),
    }
}

fn source(tag: u8) -> SourceId {
    SourceId::from_uuid(uuid::Uuid::from_bytes([tag; 16]))
}

/// A `Serve` impl driven by a closure, so each test can script exactly what its
/// sources do. The closure runs on a spawned task, not inline, so a source that
/// takes its time does not block `serve` from returning.
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

/// The spawner a host injects. Under `#[tokio::test]` this is `tokio::spawn`;
/// lindsey passes a GPUI background-executor closure instead.
fn tokio_spawner() -> Arc<dyn Fn(heart::surface::MergePump) + Send + Sync> {
    Arc::new(|pump| {
        tokio::spawn(pump);
    })
}

async fn drain(answer: Answer<Rows>) -> (Vec<Row>, Option<Summary>, usize) {
    let mut items = Vec::new();
    let mut summary = None;
    let mut degradations = 0usize;
    while let Some(frame) = answer.recv().await {
        match frame {
            // **Upsert, not append** — see `Frame::Item`'s doc comment. A
            // repeat key is a supersede: `merge` re-emits the fused row so the
            // consumer replaces what it already painted. Appending here would
            // model a *broken* consumer, and would make these tests pass or
            // fail depending on which source happened to answer first.
            //
            // Replacing in place (rather than removing and pushing) keeps
            // first-seen order, which is what a progressive result list shows.
            Frame::Item(located) => {
                let key = Rows::key(&located.value);
                match items.iter_mut().find(|row| Rows::key(row) == key) {
                    Some(existing) => *existing = located.value,
                    None => items.push(located.value),
                }
            }
            Frame::End(s) => summary = Some(s),
            Frame::Degraded(_) => degradations += 1,
            _ => {}
        }
    }
    (items, summary, degradations)
}

fn request() -> RowRequest {
    RowRequest {
        text: "q".to_owned(),
    }
}

// ---------------------------------------------------------------------------
// 1. It is a `Serve<S>` — the transparency mechanism itself
// ---------------------------------------------------------------------------

/// THE structural test. A caller holds `Arc<dyn Serve<Rows>>` and cannot tell
/// whether it is one source or five. If this does not compile, the whole
/// premise is gone.
#[tokio::test]
async fn a_federation_is_indistinguishable_from_a_single_serve() {
    let single: Arc<dyn Serve<Rows>> = scripted(|tx| {
        tx.item(bare(1), Residence::Local).ok();
        tx.end(Summary::complete(1)).ok();
    });

    let federated: Arc<dyn Serve<Rows>> = Arc::new(Federated::new(
        vec![
            (
                source(1),
                scripted(|tx| {
                    tx.item(bare(1), Residence::Local).ok();
                    tx.end(Summary::complete(1)).ok();
                }) as Arc<dyn Serve<Rows>>,
            ),
            (
                source(2),
                scripted(|tx| {
                    tx.item(bare(2), Residence::Remote {
                        generation: heart::surface::GenerationId(1),
                    })
                    .ok();
                    tx.end(Summary::complete(1)).ok();
                }) as Arc<dyn Serve<Rows>>,
            ),
        ],
        tokio_spawner(),
    ));

    // Both are called through the identical API.
    for serve in [single, federated] {
        let answer = serve.serve(request(), Gen(1));
        let (items, summary, _) = drain(answer).await;
        assert!(!items.is_empty());
        assert!(summary.is_some(), "every answer ends");
    }
}

/// `serve` must return immediately — lindsey calls it on the foreground thread
/// and cannot await a constructor there. A federation that awaited its sources
/// before returning would stall the UI on every keystroke.
#[tokio::test]
async fn serve_returns_before_any_source_has_answered() {
    let federated = Federated::new(
        vec![(
            source(1),
            scripted(|tx| {
                std::thread::sleep(Duration::from_millis(300));
                tx.item(bare(1), Residence::Local).ok();
                tx.end(Summary::complete(1)).ok();
            }) as Arc<dyn Serve<Rows>>,
        )],
        tokio_spawner(),
    );

    let start = std::time::Instant::now();
    let answer = federated.serve(request(), Gen(1));
    let elapsed = start.elapsed();

    assert!(
        elapsed < Duration::from_millis(100),
        "serve blocked for {elapsed:?} waiting on a source; it must return \
         immediately with frames arriving afterward"
    );

    let (items, _, _) = drain(answer).await;
    assert_eq!(items, vec![bare(1)], "the slow source still lands");
}

/// The injected spawner is what makes this runtime-agnostic. It must actually
/// be used — a federation that reached for `tokio::spawn` internally would
/// panic in lindsey, which has no reactor.
#[tokio::test]
async fn the_injected_spawner_drives_the_merge() {
    let calls = Arc::new(AtomicUsize::new(0));
    let calls_in_spawner = Arc::clone(&calls);

    let spawner: Arc<dyn Fn(heart::surface::MergePump) + Send + Sync> = Arc::new(move |pump| {
        calls_in_spawner.fetch_add(1, Ordering::SeqCst);
        tokio::spawn(pump);
    });

    let federated = Federated::new(
        vec![(
            source(1),
            scripted(|tx| {
                tx.item(bare(1), Residence::Local).ok();
                tx.end(Summary::complete(1)).ok();
            }) as Arc<dyn Serve<Rows>>,
        )],
        spawner,
    );

    let answer = federated.serve(request(), Gen(1));
    let (items, _, _) = drain(answer).await;

    assert_eq!(items, vec![bare(1)]);
    assert_eq!(
        calls.load(Ordering::SeqCst),
        1,
        "exactly one pump per serve, driven by the injected spawner"
    );
}

// ---------------------------------------------------------------------------
// 2. Progressive delivery — no barrier
// ---------------------------------------------------------------------------

/// A fast source must not wait for a slow one. This is the product decision
/// recorded in the contract (progressive merge, no global sort): rows paint as
/// they arrive.
#[tokio::test]
async fn a_fast_source_is_not_held_up_by_a_slow_one() {
    let federated = Federated::new(
        vec![
            (
                source(1),
                scripted(|tx| {
                    std::thread::sleep(Duration::from_millis(400));
                    tx.item(bare(99), Residence::Local).ok();
                    tx.end(Summary::complete(1)).ok();
                }) as Arc<dyn Serve<Rows>>,
            ),
            (
                source(2),
                scripted(|tx| {
                    tx.item(bare(1), Residence::Local).ok();
                    tx.end(Summary::complete(1)).ok();
                }) as Arc<dyn Serve<Rows>>,
            ),
        ],
        tokio_spawner(),
    );

    let answer = federated.serve(request(), Gen(1));

    // The first item must arrive well before the slow source could have.
    let start = std::time::Instant::now();
    let first = answer.recv().await.expect("a frame must arrive");
    let elapsed = start.elapsed();

    assert!(
        matches!(first, Frame::Item(_)),
        "the first frame should be the fast source's item, got {first:?}"
    );
    assert!(
        elapsed < Duration::from_millis(300),
        "waited {elapsed:?} for the first item — the merge is barriering on all \
         sources instead of streaming progressively"
    );

    drain(answer).await;
}

// ---------------------------------------------------------------------------
// 3. Dedup, precedence, and fusion across the seam
// ---------------------------------------------------------------------------

/// The same row from two sources converges to ONE row. This is what makes the
/// seam invisible: without it every synced package renders twice, once per
/// plane.
///
/// # What "one row" does and does not mean
///
/// It is a claim about the consumer's converged **state**, not about how many
/// `Frame::Item`s crossed the channel. Both are legal outcomes and which one
/// occurs is a race:
///
/// * if the higher-precedence source answers first, the duplicate is
///   suppressed and exactly one frame is sent;
/// * if the lower-precedence source answers first, its copy is emitted, then
///   superseded — two frames, one row.
///
/// An earlier version of this test counted frames and so failed roughly one run
/// in four, entirely on scheduling. That is worth recording, because it is the
/// same mistake a consumer makes when it appends instead of upserting — and it
/// produces the same symptom, intermittently, in front of a user. `drain` above
/// therefore models a *correct* consumer (keyed upsert, per `Frame::Item`'s doc
/// comment), which is what makes this assertion order-independent.
#[tokio::test]
async fn the_same_row_from_two_sources_converges_to_one_row() {
    let federated = Federated::new(
        vec![
            (
                source(1),
                scripted(|tx| {
                    tx.item(bare(7), Residence::Local).ok();
                    tx.end(Summary::complete(1)).ok();
                }) as Arc<dyn Serve<Rows>>,
            ),
            (
                source(2),
                scripted(|tx| {
                    tx.item(bare(7), Residence::Remote {
                        generation: heart::surface::GenerationId(3),
                    })
                    .ok();
                    tx.end(Summary::complete(1)).ok();
                }) as Arc<dyn Serve<Rows>>,
            ),
        ],
        tokio_spawner(),
    );

    let (items, _, _) = drain(federated.serve(request(), Gen(1))).await;
    assert_eq!(
        items.len(),
        1,
        "one identity must yield one row, got {items:?}"
    );
}

/// A duplicate must never make a row **worse**. If the lower-precedence source
/// already delivered an enriched copy, the higher-precedence bare copy
/// superseding it must keep the enrichment — the user has already seen the
/// detail on screen, and watching it vanish is a visible regression.
#[tokio::test]
async fn superseding_a_duplicate_fuses_rather_than_replaces() {
    let federated = Federated::new(
        vec![
            // Highest precedence, but bare, and deliberately slower.
            (
                source(1),
                scripted(|tx| {
                    std::thread::sleep(Duration::from_millis(150));
                    tx.item(bare(7), Residence::Local).ok();
                    tx.end(Summary::complete(1)).ok();
                }) as Arc<dyn Serve<Rows>>,
            ),
            // Lower precedence, rich, arrives first.
            (
                source(2),
                scripted(|tx| {
                    tx.item(rich(7, "fn seven() -> u32"), Residence::Remote {
                        generation: heart::surface::GenerationId(3),
                    })
                    .ok();
                    tx.end(Summary::complete(1)).ok();
                }) as Arc<dyn Serve<Rows>>,
            ),
        ],
        tokio_spawner(),
    );

    let (items, _, _) = drain(federated.serve(request(), Gen(1))).await;

    let detail = items
        .iter()
        .find(|r| r.id == 7)
        .and_then(|r| r.detail.as_deref());
    assert_eq!(
        detail,
        Some("fn seven() -> u32"),
        "the bare higher-precedence copy erased a detail the user could already \
         see; fusion must backfill, not overwrite with nothing"
    );
}

// ---------------------------------------------------------------------------
// 4. Degradation — offline-first honesty
// ---------------------------------------------------------------------------

/// THE offline test, and the reason `Frame::Degraded` is not terminal. With the
/// index unreachable, local results must still arrive and the answer must still
/// succeed. Today this case is a `RemoteStatus::Unreachable` banner the user has
/// to interpret; it becomes an answer that is simply a little smaller.
#[tokio::test]
async fn one_source_failing_degrades_the_answer_without_failing_it() {
    let federated = Federated::new(
        vec![
            (
                source(1),
                scripted(|tx| {
                    tx.item(bare(1), Residence::Local).ok();
                    tx.end(Summary::complete(1)).ok();
                }) as Arc<dyn Serve<Rows>>,
            ),
            (
                source(2),
                scripted(|tx| {
                    tx.failed(WireError::Timeout).ok();
                }) as Arc<dyn Serve<Rows>>,
            ),
        ],
        tokio_spawner(),
    );

    let (items, summary, degradations) = drain(federated.serve(request(), Gen(1))).await;

    assert_eq!(items, vec![bare(1)], "the healthy source still answers");
    assert_eq!(degradations, 1, "the failure is reported, not hidden");
    let summary = summary.expect("the answer must still end successfully");
    assert!(
        !summary.is_complete(),
        "an answer missing a source is not complete"
    );
}

/// Every source failing is still not a failed answer — it is an empty, honestly
/// degraded one. A `Result`-shaped API would be forced to return `Err` here and
/// throw away the (correct) knowledge that the corpus was simply unreachable.
#[tokio::test]
async fn every_source_failing_still_ends_the_answer() {
    let federated = Federated::new(
        vec![
            (
                source(1),
                scripted(|tx| {
                    tx.failed(WireError::Timeout).ok();
                }) as Arc<dyn Serve<Rows>>,
            ),
            (
                source(2),
                scripted(|tx| {
                    tx.failed(WireError::Backend("index down".into())).ok();
                }) as Arc<dyn Serve<Rows>>,
            ),
        ],
        tokio_spawner(),
    );

    let (items, summary, degradations) = drain(federated.serve(request(), Gen(1))).await;

    assert!(items.is_empty());
    assert_eq!(degradations, 2);
    assert!(
        summary.is_some(),
        "the answer must terminate, not hang, when every source is gone"
    );
}

// ---------------------------------------------------------------------------
// 5. Degenerate shapes
// ---------------------------------------------------------------------------

/// A federation of one must behave exactly like that one source. This is what
/// lets a host with no index configured build the same object rather than
/// branching on a special "local only" path — the branch that produced two
/// call paths in the first place.
#[tokio::test]
async fn a_federation_of_one_is_a_passthrough() {
    let federated = Federated::new(
        vec![(
            source(1),
            scripted(|tx| {
                tx.item(rich(1, "detail"), Residence::Local).ok();
                tx.item(bare(2), Residence::Local).ok();
                tx.end(Summary::complete(2)).ok();
            }) as Arc<dyn Serve<Rows>>,
        )],
        tokio_spawner(),
    );

    let (items, summary, degradations) = drain(federated.serve(request(), Gen(1))).await;
    assert_eq!(items, vec![rich(1, "detail"), bare(2)]);
    assert_eq!(summary, Some(Summary::complete(2)));
    assert_eq!(degradations, 0);
}

/// No sources at all must end immediately, not hang. A host that has not
/// finished configuring anything yet still calls `serve`.
#[tokio::test]
async fn a_federation_of_none_ends_immediately() {
    let federated: Federated<Rows> = Federated::new(vec![], tokio_spawner());

    let answer = tokio::time::timeout(
        Duration::from_secs(5),
        drain(federated.serve(request(), Gen(1))),
    )
    .await
    .expect("an empty federation must not hang");

    let (items, summary, _) = answer;
    assert!(items.is_empty());
    assert_eq!(summary, Some(Summary::complete(0)));
}

// ---------------------------------------------------------------------------
// 6. Cancellation
// ---------------------------------------------------------------------------

/// Search-as-you-type drops the previous `Answer` on every keystroke. That must
/// stop the work in every source — otherwise a fast typist accumulates one
/// abandoned in-flight query per character.
#[tokio::test]
async fn dropping_the_answer_cancels_every_source() {
    let cancelled = Arc::new(AtomicUsize::new(0));

    let make = |flag: Arc<AtomicUsize>| {
        scripted(move |tx: Emitter<Rows>| {
            // Emit nothing; wait for the consumer to go away.
            for _ in 0..200 {
                if tx.is_cancelled() {
                    flag.fetch_add(1, Ordering::SeqCst);
                    return;
                }
                std::thread::sleep(Duration::from_millis(10));
            }
        }) as Arc<dyn Serve<Rows>>
    };

    let federated = Federated::new(
        vec![
            (source(1), make(Arc::clone(&cancelled))),
            (source(2), make(Arc::clone(&cancelled))),
        ],
        tokio_spawner(),
    );

    let answer = federated.serve(request(), Gen(1));
    tokio::time::sleep(Duration::from_millis(50)).await;
    drop(answer);

    // Give the sources a moment to observe the disconnect.
    tokio::time::sleep(Duration::from_millis(300)).await;

    assert_eq!(
        cancelled.load(Ordering::SeqCst),
        2,
        "both sources must observe cancellation when the consumer drops the answer"
    );
}

// ---------------------------------------------------------------------------
// 7. The generation is carried through
// ---------------------------------------------------------------------------

/// The generation a caller asked for must be the generation the answer reports,
/// or a stale answer from a previous keystroke could be mistaken for the
/// current one.
#[tokio::test]
async fn the_answer_reports_the_requested_generation() {
    let federated = Federated::new(
        vec![(
            source(1),
            scripted(|tx| {
                tx.end(Summary::complete(0)).ok();
            }) as Arc<dyn Serve<Rows>>,
        )],
        tokio_spawner(),
    );

    let answer = federated.serve(request(), Gen(42));
    assert_eq!(answer.generation(), Gen(42));
}
