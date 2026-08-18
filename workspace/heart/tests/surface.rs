//! Red-first specification for `heart::surface` — the one local/remote contract.
//!
//! Every test here is a pin on the API described in `docs/LOCAL-REMOTE-CONTRACT.md`
//! §1. They are written before the implementation exists; making them compile and
//! pass IS the S0 deliverable. Nothing here may be relaxed to make it pass —
//! if a test cannot be satisfied, that is a design bug to raise, not a test to
//! weaken.
//!
//! The through-line: **a caller holding `Arc<dyn Serve<S>>` must not be able to
//! tell whether it is talking to a local engine, a remote server, or a merge of
//! both.** Every pin below exists to make some way of telling them apart
//! unrepresentable.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use heart::access::SourceId;
use heart::stream::WireError;
use heart::surface::{
    Answer, Completeness, Degradation, Emitter, Frame, Gen, GenerationId, Located, Residence,
    Serve, StreamHandle, Summary, answer_channel,
};
use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// A test surface. Deliberately not `heart::Symbol` — the contract must be
// satisfiable by a type the `heart` crate has never heard of, which is what
// makes "add a surface, get a remote client free" possible.
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct Widget {
    id: u32,
    name: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct WidgetRequest {
    text: String,
}

/// The out-of-band metadata a widget search emits. This is the generalization of
/// `SearchEvent::{SectionState, Latency}` / `DocEvent::Head` / `QueryEvent::Columns`
/// — genuinely surface-specific, so it stays a distinct type; only the framing
/// around it is shared.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
enum WidgetNote {
    Columns(Vec<String>),
    Latency { millis: u64 },
}

struct Widgets;

impl heart::surface::Surface for Widgets {
    const NAME: &'static str = "widgets";
    const PATH: &'static str = "/widgets/search";
    type Request = WidgetRequest;
    type Item = Widget;
    type Note = WidgetNote;
    type Key = u32;
    fn key(item: &Self::Item) -> u32 {
        item.id
    }
}

fn widget(id: u32, name: &str) -> Widget {
    Widget {
        id,
        name: name.to_owned(),
    }
}

fn source(tag: u8) -> SourceId {
    SourceId::from_uuid(uuid::Uuid::from_bytes([tag; 16]))
}

// ---------------------------------------------------------------------------
// 1. Residence — the ONE locality vocabulary (contract §1.3)
// ---------------------------------------------------------------------------

/// The six competing encodings (`SourceRole`, `sync_endpoint`, the
/// `vector/{local,remote}` split, `RemoteRouteReason`, `Ir`/`ObjectAvailability`,
/// `wire::Provenance`) collapse into exactly these four variants.
#[test]
fn residence_has_four_variants_and_says_which_generation() {
    let local = Residence::Local;
    let synced = Residence::Synced {
        generation: GenerationId(7),
    };
    let remote = Residence::Remote {
        generation: GenerationId(7),
    };
    let stale = Residence::Stale {
        as_of: heart::query::UnixMilliseconds(1_700_000_000_000),
    };

    // Synced and Remote at the same generation are the SAME data — one is
    // materialized here, one is not. That distinction must survive, because it
    // is the difference between "offline-safe" and "needs the network".
    assert_ne!(synced, remote);
    assert_eq!(synced.generation(), Some(GenerationId(7)));
    assert_eq!(remote.generation(), Some(GenerationId(7)));
    assert_eq!(local.generation(), None);
    assert_eq!(stale.generation(), None);

    // Only Local and Synced can be served with the network down.
    assert!(local.is_offline_capable());
    assert!(synced.is_offline_capable());
    assert!(!remote.is_offline_capable());
    assert!(stale.is_offline_capable());
}

/// Residence must round-trip on the wire: a remote server tags the items it
/// sends, and the client must not silently re-tag them.
#[test]
fn residence_round_trips_by_variant() {
    for value in [
        Residence::Local,
        Residence::Synced {
            generation: GenerationId(1),
        },
        Residence::Remote {
            generation: GenerationId(2),
        },
        Residence::Stale {
            as_of: heart::query::UnixMilliseconds(42),
        },
    ] {
        let json = serde_json::to_string(&value).expect("residence serializes");
        let back: Residence = serde_json::from_str(&json).expect("residence deserializes");
        assert_eq!(value, back, "round-trip changed the variant");
    }
}

// ---------------------------------------------------------------------------
// 2. Located — residence rides on the ITEM, never on the call (contract §1.3)
// ---------------------------------------------------------------------------

/// This is the pin that makes transparency possible at all. If residence were a
/// property of the *answer*, a merged answer would need one label for rows from
/// two places — which is exactly why today's GUI has to render remote hits as a
/// separate group. Per-item residence makes a mixed answer representable.
#[test]
fn one_answer_can_carry_items_of_different_residence() {
    let rows = vec![
        Located::new(widget(1, "local-only"), Residence::Local),
        Located::new(
            widget(2, "remote-only"),
            Residence::Remote {
                generation: GenerationId(9),
            },
        ),
    ];

    assert_eq!(rows[0].residence, Residence::Local);
    assert_eq!(
        rows[1].residence,
        Residence::Remote {
            generation: GenerationId(9)
        }
    );
    // The value is reachable without matching on residence — callers that do not
    // care about locality must not be forced to.
    assert_eq!(rows[0].value.name, "local-only");
}

#[test]
fn located_maps_its_value_without_disturbing_residence() {
    let located = Located::new(widget(3, "x"), Residence::Local);
    let mapped: Located<u32> = located.map(|w| w.id);
    assert_eq!(mapped.value, 3);
    assert_eq!(mapped.residence, Residence::Local);
}

// ---------------------------------------------------------------------------
// 3. Completeness — a partial answer is a VALUE, not an error (contract §1.2)
// ---------------------------------------------------------------------------

/// Today "the remote was unreachable" lives in a separate `RemoteStatus` field
/// and "the semantic index is still building" lives in `SectionStatus::Building`.
/// One mechanism replaces both, and `End` cannot be constructed without saying
/// which it is.
#[test]
fn a_summary_must_state_completeness() {
    let complete = Summary::complete(3);
    assert_eq!(complete.items, 3);
    assert_eq!(complete.complete, Completeness::Complete);

    let partial = Summary::partial(3, 3, Some(100));
    assert_eq!(partial.items, 3);
    assert_eq!(
        partial.complete,
        Completeness::Partial {
            covered: 3,
            total: Some(100)
        }
    );
    assert!(!partial.is_complete());
    assert!(complete.is_complete());
}

/// A corpus whose total is not yet known is still legitimately partial. This is
/// the "semantic index is building and we don't know how big the corpus is"
/// case — it must not be forced to claim a total it doesn't have.
#[test]
fn partial_completeness_tolerates_an_unknown_total() {
    let partial = Summary::partial(5, 5, None);
    assert_eq!(
        partial.complete,
        Completeness::Partial {
            covered: 5,
            total: None
        }
    );
}

// ---------------------------------------------------------------------------
// 4. Frame — one envelope replacing twelve (contract §1.2)
// ---------------------------------------------------------------------------

/// `Frame` must be usable as a plain value type without dragging bounds onto the
/// surface marker. `Widgets` is a unit struct that is deliberately NOT `Clone`,
/// `Debug` or `PartialEq` — a derived impl on `Frame<S>` would demand those of
/// `S` and this test would not compile.
#[test]
fn frame_is_clone_debug_eq_without_requiring_them_of_the_surface() {
    let frame: Frame<Widgets> = Frame::Item(Located::new(widget(1, "a"), Residence::Local));
    let cloned = frame.clone();
    assert_eq!(frame, cloned);
    assert!(format!("{frame:?}").contains("Item"));
}

/// Externally tagged, single-key objects — the same discipline
/// `heart::stream::StreamFrame` already established, so every NDJSON line says
/// what it is before it says anything else.
#[test]
fn frames_serialize_as_externally_tagged_single_key_objects() {
    let item: Frame<Widgets> = Frame::Item(Located::new(widget(1, "a"), Residence::Local));
    let json = serde_json::to_string(&item).expect("item frame serializes");
    assert!(
        json.starts_with(r#"{"item":"#),
        "expected an `item`-tagged object, got {json}"
    );

    let end: Frame<Widgets> = Frame::End(Summary::complete(1));
    let json = serde_json::to_string(&end).expect("end frame serializes");
    assert!(
        json.starts_with(r#"{"end":"#),
        "expected an `end`-tagged object, got {json}"
    );

    let note: Frame<Widgets> = Frame::Note(WidgetNote::Latency { millis: 12 });
    let json = serde_json::to_string(&note).expect("note frame serializes");
    assert!(
        json.starts_with(r#"{"note":"#),
        "expected a `note`-tagged object, got {json}"
    );

    let degraded: Frame<Widgets> = Frame::Degraded(Degradation {
        source: source(0xAB),
        reason: WireError::Timeout,
    });
    let json = serde_json::to_string(&degraded).expect("degraded frame serializes");
    assert!(
        json.starts_with(r#"{"degraded":"#),
        "expected a `degraded`-tagged object, got {json}"
    );

    let failed: Frame<Widgets> = Frame::Failed(WireError::Timeout);
    let json = serde_json::to_string(&failed).expect("failed frame serializes");
    assert!(
        json.starts_with(r#"{"failed":"#),
        "expected a `failed`-tagged object, got {json}"
    );
}

#[test]
fn every_frame_variant_round_trips() {
    let frames: Vec<Frame<Widgets>> = vec![
        Frame::Note(WidgetNote::Columns(vec!["id".into(), "name".into()])),
        Frame::Item(Located::new(
            widget(7, "seven"),
            Residence::Synced {
                generation: GenerationId(3),
            },
        )),
        Frame::Degraded(Degradation {
            source: source(2),
            reason: WireError::Backend("qdrant down".into()),
        }),
        Frame::End(Summary::partial(2, 2, Some(9))),
        Frame::Failed(WireError::BadRequest("empty text".into())),
    ];
    for frame in frames {
        let line = serde_json::to_string(&frame).expect("frame serializes");
        let back: Frame<Widgets> = serde_json::from_str(&line).expect("frame deserializes");
        assert_eq!(frame, back, "round-trip changed the frame");
    }
}

/// `Degraded` is explicitly NOT terminal — this is the behavioural difference
/// that makes offline-first honest. A remote source dropping out must leave the
/// local answer standing.
#[test]
fn only_end_and_failed_are_terminal() {
    let note: Frame<Widgets> = Frame::Note(WidgetNote::Latency { millis: 1 });
    let item: Frame<Widgets> = Frame::Item(Located::new(widget(1, "a"), Residence::Local));
    let degraded: Frame<Widgets> = Frame::Degraded(Degradation {
        source: source(1),
        reason: WireError::Timeout,
    });
    let end: Frame<Widgets> = Frame::End(Summary::complete(0));
    let failed: Frame<Widgets> = Frame::Failed(WireError::Timeout);

    assert!(!note.is_terminal());
    assert!(!item.is_terminal());
    assert!(
        !degraded.is_terminal(),
        "a degraded source must not end the answer — local results still stand"
    );
    assert!(end.is_terminal());
    assert!(failed.is_terminal());
}

/// A reader that gets a frame kind it has never heard of must not fail to
/// compile or panic. `#[non_exhaustive]` forces the `_` arm at every match site.
#[test]
fn frame_is_non_exhaustive_so_readers_need_a_fallback_arm() {
    let frame: Frame<Widgets> = Frame::End(Summary::complete(0));
    let described = match &frame {
        Frame::Item(_) => "item",
        Frame::Note(_) => "note",
        Frame::Degraded(_) => "degraded",
        Frame::End(_) => "end",
        Frame::Failed(_) => "failed",
        _ => "unknown",
    };
    assert_eq!(described, "end");
}

// ---------------------------------------------------------------------------
// 5. Answer / Emitter — the one thing every backend returns (contract §1.4)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn an_answer_delivers_frames_in_order_as_they_are_emitted() {
    let (tx, answer) = answer_channel::<Widgets>(8, Gen(1));

    tx.item(widget(1, "a"), Residence::Local).unwrap();
    tx.note(WidgetNote::Latency { millis: 3 }).unwrap();
    tx.item(widget(2, "b"), Residence::Local).unwrap();
    tx.end(Summary::complete(2)).unwrap();

    let mut kinds = Vec::new();
    while let Some(frame) = answer.recv().await {
        kinds.push(match frame {
            Frame::Item(l) => format!("item:{}", l.value.id),
            Frame::Note(_) => "note".to_owned(),
            Frame::End(_) => "end".to_owned(),
            other => panic!("unexpected frame {other:?}"),
        });
    }
    assert_eq!(kinds, ["item:1", "note", "item:2", "end"]);
}

/// **The terminal frame ends the stream — not the emitter's liveness.**
///
/// A producer that emits `End` and then keeps its `Emitter` alive (parked in a
/// task, held in a struct, waiting on an unrelated select) must not leave the
/// consumer blocked forever on a stream that is semantically over. `recv` has to
/// resolve to `None` on the strength of the terminal frame alone.
///
/// This is the same rule `heart::stream` already enforces on the wire — a stream
/// that ends *must* carry a terminal frame, and anything after one is a protocol
/// violation. Here it is enforced in the type rather than by the reader.
#[tokio::test]
async fn recv_ends_at_the_terminal_frame_even_while_the_emitter_lives() {
    let (tx, answer) = answer_channel::<Widgets>(8, Gen(1));
    tx.item(widget(1, "a"), Residence::Local).unwrap();
    tx.end(Summary::complete(1)).unwrap();

    // `tx` is deliberately still alive and NOT dropped.
    let frames = tokio::time::timeout(std::time::Duration::from_secs(5), drain(answer))
        .await
        .expect("recv must terminate on the End frame, not block on a live emitter");

    assert_eq!(frames.len(), 2);
    assert!(matches!(frames[1], Frame::End(_)));
}

/// The converse half of the same rule: once a terminal frame has been emitted,
/// the emitter must refuse further frames rather than queue frames no consumer
/// will ever observe.
#[tokio::test]
async fn emitting_after_a_terminal_frame_is_rejected() {
    let (tx, _answer) = answer_channel::<Widgets>(8, Gen(1));
    tx.end(Summary::complete(0)).unwrap();

    assert!(
        tx.item(widget(1, "late"), Residence::Local).is_err(),
        "an item after End must be rejected, not silently dropped or queued"
    );
    assert!(
        tx.end(Summary::complete(0)).is_err(),
        "a second terminal frame must be rejected"
    );
}

/// The GUI drains synchronously from GPUI's executor: await one frame, then
/// `try_recv` the rest of the burst into one repaint. Both must work on the same
/// `Answer` — that is the whole reason the answer type is a flume receiver and
/// not an `impl Stream`.
#[tokio::test]
async fn an_answer_supports_the_gui_batched_drain_pattern() {
    let (tx, answer) = answer_channel::<Widgets>(64, Gen(1));
    for id in 0..100u32 {
        tx.item(widget(id, "burst"), Residence::Local).unwrap();
    }
    tx.end(Summary::complete(100)).unwrap();
    drop(tx);

    let mut batches = 0usize;
    let mut seen = 0usize;
    while let Some(_first) = answer.recv().await {
        batches += 1;
        seen += 1;
        while answer.try_recv().is_some() {
            seen += 1;
        }
    }
    assert_eq!(seen, 101, "every frame must be delivered exactly once");
    assert!(
        batches < 100,
        "a 100-frame burst must collapse into far fewer batches, got {batches}"
    );
}

/// The server side needs a `Stream` for `Body::from_stream`. Same `Answer`.
#[tokio::test]
async fn an_answer_converts_to_a_stream_for_the_server_side() {
    use futures::StreamExt as _;

    let (tx, answer) = answer_channel::<Widgets>(8, Gen(1));
    tx.item(widget(1, "a"), Residence::Local).unwrap();
    tx.end(Summary::complete(1)).unwrap();
    drop(tx);

    let collected: Vec<Frame<Widgets>> = answer.into_stream().collect().await;
    assert_eq!(collected.len(), 2);
    assert!(matches!(collected[1], Frame::End(_)));
}

#[tokio::test]
async fn an_answer_reports_the_generation_it_answers() {
    let (_tx, answer) = answer_channel::<Widgets>(1, Gen(42));
    assert_eq!(answer.generation(), Gen(42));
}

/// Trivial impls need to answer without spawning anything.
#[tokio::test]
async fn an_answer_can_be_constructed_already_finished() {
    let empty = Answer::<Widgets>::empty(Gen(1));
    let frames: Vec<_> = drain(empty).await;
    assert!(matches!(frames.as_slice(), [Frame::End(_)]));

    let failed = Answer::<Widgets>::failed(Gen(1), WireError::Timeout);
    let frames: Vec<_> = drain(failed).await;
    assert!(matches!(frames.as_slice(), [Frame::Failed(_)]));
}

// ---------------------------------------------------------------------------
// 6. Cancellation — dropping the answer stops the work (contract §1.4)
// ---------------------------------------------------------------------------

/// Search-as-you-type depends on this: the GUI drops the previous answer on every
/// keystroke and the abandoned query must actually stop, not just be ignored.
#[tokio::test]
async fn dropping_the_answer_signals_the_producer_to_stop() {
    let (tx, answer) = answer_channel::<Widgets>(4, Gen(1));
    assert!(!tx.is_cancelled(), "not cancelled while the answer is held");

    tx.item(widget(1, "a"), Residence::Local).unwrap();
    drop(answer);

    assert!(
        tx.is_cancelled(),
        "dropping the answer must be observable by the producer"
    );
    assert!(
        tx.item(widget(2, "b"), Residence::Local).is_err(),
        "emitting into a cancelled answer must fail, not silently succeed"
    );
}

/// The engine cancels real *work* (a spawned task, a Trustfall walk), not just
/// the channel. `StreamHandle` carries that canceller and fires it on drop —
/// the same contract `nudox_engine::runtime::StreamHandle` already has.
#[tokio::test]
async fn dropping_the_answer_runs_the_attached_canceller() {
    let cancelled = Arc::new(AtomicBool::new(false));
    let flag = Arc::clone(&cancelled);

    let (_tx, answer) = answer_channel::<Widgets>(4, Gen(1));
    let answer = answer.with_handle(StreamHandle::new(Gen(1), move || {
        flag.store(true, Ordering::SeqCst);
    }));

    assert!(!cancelled.load(Ordering::SeqCst));
    drop(answer);
    assert!(
        cancelled.load(Ordering::SeqCst),
        "the attached canceller must fire when the answer is dropped"
    );
}

#[test]
fn a_stream_handle_cancels_exactly_once_on_drop() {
    let count = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&count);
    {
        let _handle = StreamHandle::new(Gen(1), move || {
            counter.fetch_add(1, Ordering::SeqCst);
        });
    }
    assert_eq!(count.load(Ordering::SeqCst), 1);
}

// ---------------------------------------------------------------------------
// 7. Serve — object safety is the transparency mechanism (contract §2)
// ---------------------------------------------------------------------------

struct AlwaysLocal;
impl Serve<Widgets> for AlwaysLocal {
    fn serve(&self, request: WidgetRequest, generation: Gen) -> Answer<Widgets> {
        let (tx, answer) = answer_channel::<Widgets>(4, generation);
        tx.item(widget(1, &request.text), Residence::Local).ok();
        tx.end(Summary::complete(1)).ok();
        answer
    }
}

struct AlwaysRemote;
impl Serve<Widgets> for AlwaysRemote {
    fn serve(&self, request: WidgetRequest, generation: Gen) -> Answer<Widgets> {
        let (tx, answer) = answer_channel::<Widgets>(4, generation);
        tx.item(
            widget(2, &request.text),
            Residence::Remote {
                generation: GenerationId(1),
            },
        )
        .ok();
        tx.end(Summary::complete(1)).ok();
        answer
    }
}

/// THE pin. If `Serve<S>` is not object-safe, the GUI cannot hold one handle
/// that might be local, remote, or federated — and transparency becomes a
/// convention enforced by review instead of by the compiler.
#[tokio::test]
async fn serve_is_object_safe_so_a_caller_cannot_tell_local_from_remote() {
    let backends: Vec<Arc<dyn Serve<Widgets>>> = vec![Arc::new(AlwaysLocal), Arc::new(AlwaysRemote)];

    for backend in backends {
        let answer = backend.serve(
            WidgetRequest {
                text: "q".to_owned(),
            },
            Gen(1),
        );
        let frames = drain(answer).await;
        // Identical shape from both. The caller's code below is the same code.
        assert_eq!(frames.len(), 2);
        assert!(matches!(frames[0], Frame::Item(_)));
        assert!(matches!(frames[1], Frame::End(_)));
    }
}

/// `serve` must NOT be async: the GUI calls it on the foreground thread and
/// cannot await a constructor. It returns immediately; frames arrive later.
#[test]
fn serve_returns_without_awaiting() {
    let backend = AlwaysLocal;
    // No runtime, no `.await` — if `serve` were async this would not compile.
    let answer = backend.serve(
        WidgetRequest {
            text: "q".to_owned(),
        },
        Gen(1),
    );
    assert_eq!(answer.generation(), Gen(1));
}

// ---------------------------------------------------------------------------
// 8. Surface — the request/response type linkage (contract §1.1)
// ---------------------------------------------------------------------------

/// The hole this closes: today `Query.target` and `QueryEngine::Hit` are
/// unrelated, so `search.rs:104` has to check the pairing at runtime. Under
/// `Surface` the pairing is an associated type — a mismatch is a compile error
/// and the runtime check has nothing left to reject.
#[test]
fn a_surface_ties_its_request_item_and_path_together() {
    use heart::surface::Surface as _;
    assert_eq!(Widgets::NAME, "widgets");
    assert_eq!(Widgets::PATH, "/widgets/search");
    assert_eq!(Widgets::key(&widget(5, "five")), 5);
}

/// `S::Key` is what the federating merge dedups on. It must be hashable so the
/// merge can use a `HashSet` rather than an O(n²) scan.
#[test]
fn surface_keys_are_hashable_for_dedup() {
    use heart::surface::Surface as _;
    let mut hasher = DefaultHasher::new();
    Widgets::key(&widget(5, "five")).hash(&mut hasher);
    let a = hasher.finish();

    let mut hasher = DefaultHasher::new();
    Widgets::key(&widget(5, "different name")).hash(&mut hasher);
    let b = hasher.finish();

    assert_eq!(a, b, "identity is the key, not the whole item");
}

// ---------------------------------------------------------------------------
// helpers
// ---------------------------------------------------------------------------

async fn drain<S: heart::surface::Surface>(answer: Answer<S>) -> Vec<Frame<S>> {
    let mut out = Vec::new();
    while let Some(frame) = answer.recv().await {
        out.push(frame);
    }
    out
}

/// `Emitter` must be usable from a spawned task — producers are almost always
/// on another thread from the consumer.
#[tokio::test]
async fn an_emitter_is_send_and_usable_from_a_spawned_task() {
    let (tx, answer) = answer_channel::<Widgets>(4, Gen(1));
    tokio::spawn(async move {
        let _: &Emitter<Widgets> = &tx;
        tx.item(widget(1, "from-task"), Residence::Local).ok();
        tx.end(Summary::complete(1)).ok();
    });
    let frames = drain(answer).await;
    assert_eq!(frames.len(), 2);
}
