//! `Federated<S>` — the type that makes the local/remote split invisible
//! (contract §2, task S4).
//!
//! # What this closes
//!
//! `merge` (`workspace/heart/surface/merge_impl.rs:170`) already fans one
//! request out across several already-constructed [`Answer<S>`]s and merges
//! their frames progressively. But a caller that wants to hand out one
//! `Arc<dyn Serve<S>>` — the object-safety trick [`Serve`]'s own doc comment
//! calls "the transparency mechanism itself" — cannot use `merge` directly:
//! `merge` takes `Answer<S>`s, not a request, so a caller holding `merge`
//! would still have to know how many sources exist and call `serve` on each
//! of them by hand. `Federated<S>` is the missing `Serve<S>` impl that closes
//! that gap: it holds the sources itself, and its own `serve` is what calls
//! each source's `serve`, hands the results to `merge`, and returns the
//! merged answer. `tests/federated.rs`'s module doc catalogues exactly what
//! this replaces in lindsey (two call paths, two item types, two renderings,
//! a user-visible "search remote" button).
//!
//! # Who drives the pump, and why that is injected rather than hard-coded
//!
//! [`merge`] deliberately does not spawn anything — it returns a [`MergePump`]
//! the caller must drive, so it stays runtime-agnostic (see `merge_impl.rs`'s
//! own module doc). The index server hands that pump to `tokio::spawn`.
//! lindsey (the GUI) has **no Tokio reactor at all** — only GPUI's executor —
//! so a `Federated` that called `tokio::spawn` internally would panic there
//! instead of merely misbehaving.
//!
//! But `Serve::serve` returns `Answer<S>` and nothing else — it cannot also
//! return the pump for the caller to spawn without putting the local/remote
//! seam straight back into the caller's face, which defeats the entire point
//! of `Federated` existing.
//!
//! So the executor is named exactly once, at construction: [`Federated::new`]
//! takes a `spawn` closure. The server passes `tokio::spawn`; lindsey passes a
//! closure over its background executor. After that, `serve` is an ordinary
//! `Serve<S>` call with no runtime assumptions anywhere in its own signature —
//! this is the one piece of host-specific knowledge, named once, instead of
//! leaking into every call site that wants a federated answer.
//!
//! # Zero sources, and the phantom source both edge cases share
//!
//! `Federated::serve` always appends one silent, always-complete, always-empty
//! phantom source (`Answer::empty`) to whatever real sources it was
//! constructed with, before handing the list to `merge` — see `serve`'s own
//! doc comment for the full reasoning (it exists to keep "every real source
//! failed" from tripping `merge_inner`'s terminal-`Failed` branch). That
//! phantom source is also what makes the zero-real-sources case trivially
//! correct: `merge` never actually sees an empty vector — with zero
//! configured sources, `merge` runs over exactly the one phantom, which ends
//! immediately with `Frame::End(Summary::complete(0))` — precisely
//! `a_federation_of_none_ends_immediately`'s expectation, via the same code
//! path as every other shape rather than a special case for it.

use std::sync::Arc;

use crate::access::SourceId;

use super::{Answer, Gen, MergePump, Serve, Surface, Timer, merge_with_deadline};

/// A `Serve<S>` that fans one request out to several `Serve<S>` sources and
/// merges their answers — see this module's own doc comment for the design.
pub struct Federated<S: Surface> {
    /// Precedence order, highest first — passed straight through to
    /// [`merge`], which derives precedence from vector position
    /// (`merge_impl.rs:172`). `Federated::new` does not reorder this; the
    /// order a caller supplies *is* the precedence.
    sources: Vec<(SourceId, Arc<dyn Serve<S>>)>,
    /// The executor a host injects once, at construction — see this module's
    /// doc comment for why this cannot be hard-coded to `tokio::spawn`.
    spawn: Arc<dyn Fn(MergePump) + Send + Sync>,
    /// How long an answer may stay open waiting for a source that never
    /// terminates. `None` — the default — preserves `merge`'s own behaviour
    /// exactly: wait indefinitely.
    deadline: Option<Timer>,
}

impl<S: Surface> Federated<S> {
    /// Build a federation over `sources`, in precedence order (highest
    /// first), driven by `spawn`.
    pub fn new(
        sources: Vec<(SourceId, Arc<dyn Serve<S>>)>,
        spawn: Arc<dyn Fn(MergePump) + Send + Sync>,
    ) -> Self {
        Self {
            sources,
            spawn,
            deadline: None,
        }
    }

    /// Bound how long an answer waits for sources that never terminate.
    ///
    /// # Why an offline-first host wants this
    ///
    /// A source that *fails* is already handled: it degrades, the healthy
    /// sources' rows stand, and the answer still ends. A source that **hangs**
    /// is not, because [`merge`] ends an answer only once every source has
    /// produced a terminal frame. `RemoteClient` sets a connect timeout but
    /// deliberately no whole-request timeout (`client/remote.rs:89-100` — a
    /// search stream is legitimately long-lived), so a wedged index leaves the
    /// merged answer permanently open. The user-visible result is specific and
    /// bad: local rows appear instantly and are perfectly usable, and the query
    /// never finishes.
    ///
    /// This bounds **waiting**, never **delivering**. Rows already received are
    /// kept; sources that beat the deadline are untouched and the answer is
    /// still `Complete`; only the ones still silent at expiry are reported as
    /// degraded. See `tests/offline_first.rs`.
    ///
    /// Opt-in on purpose: a host that has not thought about the right value
    /// should get today's behaviour rather than a silent timeout chosen for it.
    #[must_use]
    pub fn with_deadline(mut self, timer: Timer) -> Self {
        self.deadline = Some(timer);
        self
    }
}

impl<S: Surface> Serve<S> for Federated<S> {
    /// Call every source's `serve` with its own clone of `request` — `Surface`
    /// requires `Request: Clone` for exactly this — then hand the resulting
    /// per-source answers to [`merge`] and return the merged [`Answer`]
    /// immediately, before anything has been driven.
    ///
    /// Not `async`, and does not block: each source's `serve` is itself
    /// required to return immediately (`Serve`'s own doc comment), so calling
    /// all of them and then `merge` — which also only *sets up* the pump
    /// without polling it — costs no I/O and no waiting. The pump that
    /// actually drives frames through the merge is handed to the injected
    /// `spawn` closure rather than polled here, which is what lets this
    /// function return before any source has answered
    /// (`serve_returns_before_any_source_has_answered`).
    fn serve(&self, request: S::Request, generation: Gen) -> Answer<S> {
        let mut answers: Vec<(SourceId, Answer<S>)> = self
            .sources
            .iter()
            .map(|(id, source)| (*id, source.serve(request.clone(), generation)))
            .collect();

        // `merge_inner` (`merge_impl.rs:406`) treats "every source's terminal
        // frame was `Frame::Failed`" as a hard failure of the *whole merge* —
        // a terminal `Frame::Failed`, not `Frame::End`. That is exactly
        // right for a direct caller of `merge`: `tests/merge.rs`'s
        // `every_source_failing_fails_the_answer` pins it, and this must not
        // weaken that.
        //
        // But `Federated` makes offline-first callers a stronger promise
        // than `merge`'s own contract gives them (see this module's own doc
        // comment, and `Serve<S>`'s "never a `Result`" framing in
        // `surface.rs`): every real source being unreachable is offline, not
        // failure, and must still produce a terminating `Frame::End` a
        // caller can render — `every_source_failing_still_ends_the_answer`
        // in `tests/federated.rs` pins exactly this for the federation case.
        //
        // A silent, always-complete, always-empty phantom source closes that
        // gap without touching `merge`'s own behaviour: appending it means
        // `source_count` (as `merge_inner` counts it) is always one more
        // than the number of *real* sources that can possibly report
        // `Frame::Failed`, so `failures == source_count` can never hold no
        // matter how many real sources fail — `merge` falls through to its
        // ordinary `Completeness::Partial` path instead, which is exactly
        // the offline-first answer shape this type exists to guarantee. Its
        // `SourceId` is fresh and random and can never appear in a
        // `Frame::Degraded` (it never emits anything but its own immediate,
        // successful `End`), so it is entirely unobservable to a consumer of
        // the merged answer — it changes no `Summary::items` count and wins
        // no [`Surface::fuse`] contest.
        answers.push((SourceId::new_random(), Answer::empty(generation)));

        let (answer, pump) = merge_with_deadline(generation, answers, self.deadline.clone());
        (self.spawn)(pump);
        answer
    }
}
