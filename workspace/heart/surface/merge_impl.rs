//! The one federating merge.
//!
//! # What this replaces
//!
//! Two things that are the same operation written twice, badly:
//!
//! * **Server side.** `index::server::search::merge_overlay_first` takes
//!   `IntoIterator<Item = Vec<Scored<T>>>` — it cannot start until every source
//!   has been fully collected, which is what forces `precise_hits` to drain each
//!   source's stream into a `Vec` and, transitively, what forces the `/search`
//!   handler's `try_collect`. It also awaits its sources in a `for` loop, so
//!   source 2 does not begin until source 1 has finished.
//! * **GUI side.** There is no merge at all: remote results land in a separate
//!   `RemoteStatus` field and render as a separately-labelled group, because
//!   nothing could combine a streaming local answer with a buffered remote one.
//!
//! Both are [`merge`], over [`Answer<S>`]. A merged answer is itself an
//! `Answer<S>`, so merges nest: the server federates its sources, the GUI
//! federates (that server, local), and nothing in the type notices.
//!
//! # The ordering decision: PROGRESSIVE
//!
//! You cannot have all three of
//!
//! 1. items stream out as soon as they are known,
//! 2. globally score-sorted output, and
//! 3. overlay-override — a higher-precedence source's copy of a key wins.
//!
//! To emit the base's `X@0.9` under (2) you must first know that no overlay
//! holds `X`, and you only know that once every overlay is exhausted — which
//! costs you (1) for every source but the first.
//!
//! **We give up (2).** Sources are polled concurrently; each source's own items
//! keep their relative order; across sources the output interleaves by arrival.
//! Overlay-override survives as a *supersede*: when a duplicate key arrives from
//! a higher-precedence source than the one that claimed it, the new copy is
//! re-emitted and the consumer replaces the row it already has. Consumers key by
//! [`Surface::key`] and replace — which is exactly the in-place merge the GUI's
//! search store already performs — so the final state is deterministic even
//! though arrival order is not.
//!
//! Without the supersede rule, overlay-override would silently lose whenever the
//! base answered first, which is both common and precisely when a stale row
//! outranking a corrected one matters most.
//!
//! # Degradation
//!
//! A source that fails does **not** fail the merge. It becomes a
//! [`Frame::Degraded`] and the terminal [`Frame::End`] reports
//! [`Completeness::Partial`], so "the remote was unreachable" is a value on a
//! still-successful answer rather than a separate status field a view has to
//! remember to render. Only losing *every* source produces a terminal
//! [`Frame::Failed`] — an empty `End` would claim "we looked and found nothing",
//! which is a different and much worse thing to tell a reader than "nothing
//! worked".

use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::task::{Context, Poll};

use futures::StreamExt as _;
use futures::stream::SelectAll;

use super::{
    Answer, Completeness, Degradation, Frame, Gen, StreamHandle, Summary, Surface, answer_channel,
};
use crate::access::SourceId;
use crate::stream::WireError;

/// Buffer between the merge and its consumer. Frames are small and the consumer
/// (a GUI drain loop, an axum body writer) is expected to keep up; this exists
/// to absorb bursts, not to provide backpressure.
const MERGE_CHANNEL_CAPACITY: usize = 256;

/// The driver returned alongside a merged [`Answer`].
///
/// A merge has to be *driven* — something must poll the source streams and
/// forward frames. Returning that work as a future the caller spawns, rather
/// than spawning it internally, is what keeps this runtime-agnostic: the server
/// hands it to `tokio::spawn`, and lindsey — which has no Tokio reactor, only
/// GPUI's executor (LR-9) — hands it to `cx.spawn`. A merge that reached for
/// `tokio::spawn` itself would be unusable in the GUI, which is half its reason
/// for existing.
///
/// Dropping the merged `Answer` completes this future promptly; it does not need
/// to be cancelled separately.
pub struct MergePump {
    inner: Pin<Box<dyn Future<Output = ()> + Send>>,
}

impl Future for MergePump {
    type Output = ();

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
        self.inner.as_mut().poll(cx)
    }
}

impl std::fmt::Debug for MergePump {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MergePump").finish_non_exhaustive()
    }
}

/// Merge `sources` — supplied in **precedence order**, highest first — into one
/// [`Answer`].
///
/// Returns the merged answer and the [`MergePump`] that drives it; the caller
/// must poll the pump (typically by spawning it) or no frames will flow.
///
/// Precedence is the position in `sources`: index 0 outranks index 1, and so on.
/// For the GUI that is `[local, remote]`; for the server it is the federation's
/// overlays followed by its definitive base. The [`SourceId`] paired with each
/// answer is what a [`Frame::Degraded`] names when that source drops out, so it
/// must identify the source meaningfully to whoever reads the degradation.
pub fn merge<S: Surface>(
    generation: Gen,
    sources: Vec<(SourceId, Answer<S>)>,
) -> (Answer<S>, MergePump) {
    merge_inner(generation, sources, None)
}

/// [`merge`], bounded to at most `limit` **distinct keys**.
///
/// # Why a bound is needed at all
///
/// Giving up the global sort (see the module docs) also gave up the post-merge
/// `truncate(limit)` the old `merge_overlay_first` performed: each source caps
/// its own contribution at `limit`, so a federation of `N` sources emits up to
/// `N × limit` items and a caller asking for a page of 30 receives 90. Dropping
/// the *ordering* guarantee was a deliberate trade; silently tripling the *page
/// size* was not.
///
/// # Why distinct keys and not emitted frames
///
/// A supersede re-emits a key the consumer already holds — it replaces a row
/// rather than adding one. Counting frames would let a run of supersedes starve
/// out genuinely new results, and would make the delivered result count depend
/// on the arrival order of duplicates. Counting distinct keys makes the bound
/// mean what a caller asking for "30 results" actually wants.
///
/// A supersede for an already-admitted key is therefore **always** let through,
/// even after the budget is spent: the consumer is holding a row this merge
/// already told it about, and refusing the better copy would strand a stale row
/// on screen permanently.
///
/// Hitting the bound makes the terminal [`Summary`] report
/// [`Completeness::Partial`] — "three results" and "three results, and we
/// stopped looking" are different answers and a consumer must be able to tell
/// them apart.
pub fn merge_bounded<S: Surface>(
    generation: Gen,
    sources: Vec<(SourceId, Answer<S>)>,
    limit: std::num::NonZeroUsize,
) -> (Answer<S>, MergePump) {
    merge_inner(generation, sources, Some(limit.get()))
}

fn merge_inner<S: Surface>(
    generation: Gen,
    sources: Vec<(SourceId, Answer<S>)>,
    limit: Option<usize>,
) -> (Answer<S>, MergePump) {
    let (out, answer) = answer_channel::<S>(MERGE_CHANNEL_CAPACITY, generation);

    // A `watch` rather than a `Notify`: the canceller may fire before the pump
    // ever reaches its `select!`, and `watch` retains the value so that race
    // resolves in favour of cancelling. `Notify::notify_waiters` would drop the
    // signal on the floor and leave the pump parked forever on sources that will
    // never produce anything again.
    let (cancel_tx, mut cancel_rx) = tokio::sync::watch::channel(false);
    let answer = answer.with_handle(StreamHandle::new(generation, move || {
        let _ = cancel_tx.send(true);
    }));

    let source_count = sources.len();
    let mut ids: Vec<SourceId> = Vec::with_capacity(source_count);
    let mut streams = SelectAll::new();
    for (rank, (id, source)) in sources.into_iter().enumerate() {
        ids.push(id);
        // Tag each frame with its source's precedence rank so the suppression
        // rule below can compare claimants without a second lookup.
        streams.push(source.into_stream().map(move |frame| (rank, frame)).boxed());
    }

    let pump = async move {
        // key -> precedence rank of the source whose copy is currently live.
        let mut claimed: HashMap<S::Key, usize> = HashMap::new();
        let mut saw_terminal = vec![false; source_count];
        let mut emitted: u64 = 0;
        let mut failures: usize = 0;
        let mut first_failure: Option<WireError> = None;
        let mut degraded = false;
        let mut partial = false;
        // Set when the budget refused a key that was genuinely available.
        let mut truncated = false;

        loop {
            let next = tokio::select! {
                // Cancellation wins ties: once the consumer is gone there is no
                // point forwarding another frame.
                biased;
                _ = cancel_rx.changed() => return,
                next = streams.next() => next,
            };
            let Some((rank, frame)) = next else {
                break; // every source stream has ended
            };

            let forwarded = match frame {
                Frame::Item(located) => {
                    let key = S::key(&located.value);
                    match claimed.get(&key) {
                        // First sighting of this key — the only case the budget
                        // applies to, because it is the only one that grows the
                        // consumer's result set.
                        None => {
                            if limit.is_some_and(|cap| claimed.len() >= cap) {
                                // Budget spent. Record that the answer is short
                                // of what was available so `End` can say so.
                                truncated = true;
                                Ok(())
                            } else {
                                claimed.insert(key, rank);
                                emitted += 1;
                                out.push(Frame::Item(located))
                            }
                        }
                        // A higher-precedence source's copy arrived late: re-emit
                        // so the consumer replaces the row it already painted.
                        // Deliberately NOT budget-checked — this replaces a row
                        // rather than adding one, and refusing it would strand a
                        // stale copy on screen forever.
                        Some(&holder) if rank < holder => {
                            claimed.insert(key, rank);
                            emitted += 1;
                            out.push(Frame::Item(located))
                        }
                        // A lower-or-equal-precedence duplicate: suppressed.
                        Some(_) => Ok(()),
                    }
                }
                Frame::Note(note) => out.push(Frame::Note(note)),
                Frame::Degraded(degradation) => {
                    // A source may itself be a federation; forward its
                    // degradation with the *original* source id, not this
                    // source's, so the reader learns which node actually failed.
                    degraded = true;
                    out.push(Frame::Degraded(degradation))
                }
                Frame::End(summary) => {
                    saw_terminal[rank] = true;
                    if !summary.is_complete() {
                        // Incompleteness is contagious upward: a merge over a
                        // partial source cannot honestly call itself complete.
                        partial = true;
                    }
                    Ok(())
                }
                Frame::Failed(error) => {
                    saw_terminal[rank] = true;
                    failures += 1;
                    degraded = true;
                    if first_failure.is_none() {
                        first_failure = Some(error.clone());
                    }
                    out.push(Frame::Degraded(Degradation {
                        source: ids[rank],
                        reason: error,
                    }))
                }
                // No catch-all arm, deliberately. `Frame` is
                // `#[non_exhaustive]` for *downstream* readers, but inside
                // `heart` the match is exhaustive — so adding a variant later
                // breaks this build and forces someone to decide how a merge
                // should treat it. A `_ => forward` here would compile forever
                // and quietly make that decision by default, which is exactly
                // the class of silence this module exists to remove.
            };

            if forwarded.is_err() {
                // The consumer went away (or the answer already ended). Stop.
                return;
            }
        }

        // A source whose stream ended without a terminal frame was truncated —
        // the producer was dropped mid-answer. That is the same failure the wire
        // decoder synthesizes a `Failed` for, and it must not be mistaken for a
        // source that legitimately finished with nothing to say.
        for (rank, seen) in saw_terminal.iter().enumerate() {
            if !seen {
                degraded = true;
                failures += 1;
                if first_failure.is_none() {
                    first_failure = Some(WireError::Internal(
                        "source ended without a terminal frame".to_owned(),
                    ));
                }
                if out
                    .push(Frame::Degraded(Degradation {
                        source: ids[rank],
                        reason: WireError::Internal(
                            "source ended without a terminal frame".to_owned(),
                        ),
                    }))
                    .is_err()
                {
                    return;
                }
            }
        }

        // Losing every source is not an empty answer.
        if source_count > 0 && failures == source_count {
            let error = first_failure
                .unwrap_or_else(|| WireError::Internal("every source failed".to_owned()));
            let _ = out.push(Frame::Failed(error));
            return;
        }

        let summary = if degraded || partial || truncated {
            Summary {
                items: emitted,
                // `total` is genuinely unknown: a source that dropped out never
                // said how much it was going to contribute. Claiming a total here
                // would be inventing one.
                complete: Completeness::Partial {
                    covered: emitted,
                    total: None,
                },
            }
        } else {
            Summary::complete(emitted)
        };
        let _ = out.push(Frame::End(summary));
    };

    (
        answer,
        MergePump {
            inner: Box::pin(pump),
        },
    )
}
