//! `StreamSlot<T>` — the state machine that every data surface lives inside.
//!
//! ## The Display truth table
//!
//! Every slot exposes a single method, `display()`, that maps the cross-product
//! of `(Option<T>, Phase)` onto a `Display<'_, T>` variant. The table is the
//! contract between the data layer and the design layer: §5.2 of GUI-PLAN maps
//! each `Display` variant to a named motion token, and every view that renders a
//! slot does so by matching `display()` — it **cannot** forget a state, because
//! the compiler enumerates the variants.
//!
//! ```text
//! ┌───────────────┬────────────────────┬─────────────────────────────────────────┐
//! │  value        │  phase             │  Display variant                        │
//! ├───────────────┼────────────────────┼─────────────────────────────────────────┤
//! │  None         │  Idle              │  Empty                                  │
//! │  None         │  Loading { since } │  Skeleton { show_after: since }         │
//! │  None         │  Streaming { .. }  │  Skeleton { show_after: Instant::now()} │
//! │  None         │  Ready { .. }      │  (unreachable — Ready implies a value)  │
//! │  None         │  Failed { .. }     │  Error(&Error)                      │
//! │  Some(v)      │  Idle              │  (unreachable — Idle clears value)      │
//! │  Some(v)      │  Loading { .. }    │  Stale(v) — dim 70 % + shimmer strip   │
//! │  Some(v)      │  Streaming { .. }  │  Partial(v) — content + progress cues  │
//! │  Some(v)      │  Ready { .. }      │  Fresh(v)                               │
//! │  Some(v)      │  Failed { .. }     │  StaleWithError(v, &Error)          │
//! └───────────────┴────────────────────┴─────────────────────────────────────────┘
//! ```
//!
//! The two "unreachable" cells are prevented by the state machine methods:
//! `begin_loading` never clears the value (stale-while-revalidate, LD-15), and
//! `complete` is only called after at least one `first_event` call, which means
//! there is always a value by the time `Ready` is reached (the `apply_*` callback
//! in the drain closure populates it when `Streaming` transitions).
//!
//! In practice a caller that drives the slot correctly will never produce those
//! cells. If they are reached, `display()` returns the nearest safe variant rather
//! than panicking.
//!
//! ## Anti-flicker grace periods
//!
//! Fast operations (locally served content) typically complete in < 50 ms. If
//! the UI showed a skeleton for every request, users would see brief flashes of
//! empty space even for near-instant loads. Two grace periods prevent this:
//!
//! - **Skeleton grace (120 ms):** `Phase::Loading` is set immediately, but views
//!   should only render a skeleton *after* the `show_skeleton()` boundary has
//!   passed. `StreamSlot::show_skeleton()` returns `false` until 120 ms elapse.
//!
//! - **Spinner grace (300 ms):** Spinners for indeterminate waits should only
//!   appear after `show_spinner()` returns `true` (300 ms after `Loading`).
//!
//! Both boundaries are tested via `Phase::skeleton_grace_elapsed()` and
//! `Phase::spinner_grace_elapsed()`. Views call these once per render; they never
//! do their own `Instant` arithmetic.

use std::fmt;
use std::time::{Duration, Instant};

use crate::bridge::generation::Gen;
use crate::bridge::handle::StreamHandle;

// ── Grace-period constants ────────────────────────────────────────────────────

/// Minimum time in `Loading` before a skeleton should be shown (§8.1 anti-flicker).
///
/// Fast local requests typically complete before this elapses; the skeleton never
/// flashes for them.
pub const SKELETON_GRACE: Duration = Duration::from_millis(120);

/// Minimum time in `Loading` or `Streaming` before a spinner should be shown (§5.1).
///
/// Spinners communicate indeterminate waits; showing one for sub-300 ms operations
/// would make fast paths feel slower.
pub const SPINNER_GRACE: Duration = Duration::from_millis(300);

// ── Phase ─────────────────────────────────────────────────────────────────────

/// The lifecycle state of a streaming query slot.
///
/// Phases advance forward through `Idle → Loading → Streaming → Ready` (the
/// happy path) or terminate in `Failed`. A new query on an occupied slot restarts
/// from `Loading` without clearing the stored value (stale-while-revalidate,
/// LD-15 / §8.1).
///
/// `#[non_exhaustive]` is required by LR-12: adding a new phase (e.g.
/// `Paused`, `Cancelled`) must not break existing match arms.
#[derive(Clone, Copy, Debug, PartialEq)]
#[non_exhaustive]
pub enum Phase {
    /// No query has been issued yet, or the slot has been explicitly reset.
    Idle,
    /// A request is out; no events have arrived yet.
    Loading {
        /// Timestamp of the transition into `Loading`.
        since: Instant,
    },
    /// Partial content is arriving.
    Streaming {
        /// Timestamp of the first received event.
        first_at: Instant,
    },
    /// The stream closed cleanly; all content has arrived.
    Ready {
        /// Timestamp of completion.
        at: Instant,
    },
    /// The stream terminated with an error. The slot's `value` may still be
    /// populated with stale content from a prior generation.
    Failed {
        /// Timestamp of failure.
        at: Instant,
    },
}

impl Phase {
    /// Returns `true` if the 120 ms skeleton grace period has elapsed.
    ///
    /// Views call this during render; they never compute `Instant` arithmetic
    /// directly. Returns `false` for every phase other than `Loading` and
    /// `Streaming` (no grace needed if there is nothing to show or the slot
    /// already has content).
    pub fn skeleton_grace_elapsed(&self) -> bool {
        match self {
            Phase::Loading { since } => since.elapsed() >= SKELETON_GRACE,
            Phase::Streaming { first_at } => first_at.elapsed() >= SKELETON_GRACE,
            _ => false,
        }
    }

    /// Returns `true` if the 300 ms spinner grace period has elapsed.
    ///
    /// Views use this to decide whether to show an indeterminate spinner.
    /// Like `skeleton_grace_elapsed`, only meaningful during `Loading` and
    /// `Streaming`.
    pub fn spinner_grace_elapsed(&self) -> bool {
        match self {
            Phase::Loading { since } => since.elapsed() >= SPINNER_GRACE,
            Phase::Streaming { first_at } => first_at.elapsed() >= SPINNER_GRACE,
            _ => false,
        }
    }

    /// Returns `true` when the slot is doing active work (Loading or Streaming).
    pub fn is_active(&self) -> bool {
        matches!(self, Phase::Loading { .. } | Phase::Streaming { .. })
    }
}

// ── Error ─────────────────────────────────────────────────────────────────────

/// A terminal error from a stream.
///
/// The variants are intentionally vague here; the engine crate owns the concrete
/// error taxonomy (`EngineError`). GUI-PLAN §8.1 / LD-16 requires every slot to
/// have a designed error state with a retry affordance. `Error` is the
/// bridge crate's representation of that; it carries a displayable message and an
/// optional structured tag so views can branch without string-matching.
///
/// `#[non_exhaustive]` per LR-12.
#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub enum Error {
    /// The underlying channel or task was cancelled before the stream completed.
    Cancelled,
    /// The engine returned an error it considers retryable (transient network
    /// failure, index not yet ready, etc.).
    Transient {
        /// Human-readable description, pre-formatted by the engine.
        message: String,
    },
    /// A permanent failure that a retry cannot resolve (missing package, auth
    /// error, schema mismatch, etc.).
    Permanent {
        /// Human-readable description.
        message: String,
    },
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Cancelled => write!(f, "stream cancelled"),
            Error::Transient { message } => write!(f, "transient error: {message}"),
            Error::Permanent { message } => write!(f, "error: {message}"),
        }
    }
}

impl std::error::Error for Error {}

// ── Display ───────────────────────────────────────────────────────────────────

/// What a view should render for a given slot state.
///
/// This is the complete contract between the data layer and the design layer.
/// Every arm is reachable (see the truth table in the module docs); the match
/// in `StreamSlot::display()` is exhaustive by construction.
///
/// Motion token mappings (GUI-PLAN §5.2):
///
/// | Variant          | Motion token(s)                                           |
/// |------------------|-----------------------------------------------------------|
/// | `Empty`          | `empty.state` entrance on first mount                     |
/// | `Skeleton`       | `skeleton.shimmer` after grace period; layout from plan   |
/// | `Stale`          | dim 70 % opacity + shimmer strip while Loading            |
/// | `Partial`        | content + arriving affordances (count tickers, etc.)      |
/// | `Fresh`          | no special motion; `content.crossfade` from `Stale`       |
/// | `Error`          | `danger` accent; no motion (errors must be calm)          |
/// | `StaleWithError` | stale content visible at 70 % + slim retry bar at bottom  |
pub enum Display<'slot, T> {
    /// No query has been issued; render the designed empty state.
    Empty,
    /// The slot is loading from cold (no prior value). The `show_after`
    /// timestamp is the start of `Loading`; views should show a skeleton
    /// only after `SKELETON_GRACE` has elapsed from that instant.
    Skeleton {
        /// When the skeleton should first appear (= when `Loading` began).
        show_after: Instant,
    },
    /// A prior value exists but a refresh is in progress. Render the stale
    /// value at 70 % opacity with a shimmer strip to signal activity.
    ///
    /// Never discard this in favour of a skeleton — that would cause the
    /// "content disappears and reappears" anti-pattern (LD-15).
    Stale(&'slot T),
    /// Partial content is arriving. Render whatever has arrived so far;
    /// add arriving affordances (streaming indicator, ticking counts).
    Partial(&'slot T),
    /// The stream completed cleanly. Render the value normally.
    Fresh(&'slot T),
    /// Cold-load failure (no prior value to show). Render a full-page error
    /// state with a retry affordance (LD-16).
    Error(&'slot Error),
    /// There is stale content *and* the refresh failed. Show the stale
    /// content so the user can still read it, and surface the error in a
    /// slim retry bar — do not obliterate the page.
    StaleWithError(&'slot T, &'slot Error),
}

// ── StreamSlot ────────────────────────────────────────────────────────────────

/// A typed query slot that combines phase tracking, value storage, and
/// stream ownership.
///
/// Stores own one `StreamSlot<T>` per logical query surface (one for search
/// results, one per open symbol tab, one per package-browse pane). The slot:
///
/// 1. Tracks the current lifecycle `Phase`.
/// 2. Holds the most recent successful `value`, surviving across reloads
///    (stale-while-revalidate, LD-15 / GUI-PLAN §8.1).
/// 3. Carries the `StreamHandle` whose `Drop` cancels the in-flight query.
/// 4. Records the current `Gen` so the drain closure can filter stale events.
///
/// ## Driving a slot (the §7.4 shape)
///
/// ```text
/// pub fn query(&mut self, cx: &mut Context<Self>) {
///     self.slot.generation = self.gens.next();       // 1. advance gen
///     self.slot.begin_loading();              // 2. transition to Loading
///     let (handle, rx) = engine.search(..);
///     self.slot.handle = Some(handle);        // 3. drops old handle → cancel
///     self.drain_task = drain(cx, rx, |store, event, cx| {
///         if event.generation() != store.slot.generation { return } // 4. stale guard
///         store.apply(event, cx);
///     });
///     cx.notify();
/// }
/// ```
///
/// Four lines of policy; memorise the shape once, apply it everywhere.
pub struct StreamSlot<T> {
    /// Current lifecycle phase.
    pub phase: Phase,
    /// Most recent successful value. Survives across `begin_loading` calls
    /// (stale-while-revalidate). Set to `None` only by `reset`.
    pub value: Option<T>,
    /// Terminal error from the most recent stream, if any.
    pub error: Option<Error>,
    /// Generation that owns the current query. The drain closure compares
    /// incoming event gens against this value and drops mismatches.
    pub generation: Gen,
    /// Ownership token for the in-flight stream. Dropping this cancels the
    /// underlying engine-side query (§2.3 / `handle.rs`).
    pub handle: Option<StreamHandle>,
}

impl<T> StreamSlot<T> {
    /// Create a new slot in the `Idle` phase with no value.
    pub fn new() -> Self {
        Self {
            phase: Phase::Idle,
            value: None,
            error: None,
            generation: Gen(0),
            handle: None,
        }
    }

    // ── State transitions (call these from store methods, not from views) ──

    /// Transition to `Loading`.
    ///
    /// Clears `error` but **preserves `value`** — this is the stale-while-
    /// revalidate contract (LD-15). The old value will be shown at 70 %
    /// opacity until the new stream completes.
    pub fn begin_loading(&mut self) {
        self.phase = Phase::Loading {
            since: Instant::now(),
        };
        self.error = None;
        // value is intentionally NOT cleared.
    }

    /// Transition from `Loading` to `Streaming` on receipt of the first event.
    ///
    /// Idempotent after the first call: subsequent calls while already
    /// `Streaming` or `Ready` are ignored.
    pub fn first_event(&mut self) {
        if matches!(self.phase, Phase::Loading { .. }) {
            self.phase = Phase::Streaming {
                first_at: Instant::now(),
            };
        }
    }

    /// Mark the stream as complete.
    ///
    /// Transitions to `Ready` and releases the stream handle (so its engine-
    /// side task can be garbage-collected). The `value` must have been set
    /// by the drain closure before calling this.
    pub fn complete(&mut self) {
        self.phase = Phase::Ready { at: Instant::now() };
        self.handle = None;
    }

    /// Mark the stream as failed.
    ///
    /// Transitions to `Failed`, stores the error, and releases the handle.
    /// The `value` from any previous generation is preserved so `display()`
    /// can return `StaleWithError` instead of discarding readable content.
    pub fn fail(&mut self, e: Error) {
        self.phase = Phase::Failed { at: Instant::now() };
        self.error = Some(e);
        self.handle = None;
    }

    /// Reset to `Idle` and clear all state.
    ///
    /// This drops the handle (cancelling any in-flight query) and clears the
    /// stored value. Only appropriate when navigating away from a surface
    /// entirely (e.g. closing a tab).
    pub fn reset(&mut self) {
        self.phase = Phase::Idle;
        self.value = None;
        self.error = None;
        self.handle = None;
        // gen is left as-is; it will be advanced by the next begin_loading.
    }

    // ── Grace-period helpers (convenience wrappers) ────────────────────────

    /// Whether the skeleton should now be shown (120 ms grace elapsed).
    ///
    /// Views should call this in their render body and only paint a skeleton
    /// element when it returns `true`.
    pub fn show_skeleton(&self) -> bool {
        self.phase.skeleton_grace_elapsed()
    }

    /// Whether a spinner should now be shown (300 ms grace elapsed).
    pub fn show_spinner(&self) -> bool {
        self.phase.spinner_grace_elapsed()
    }

    // ── The one function views call ────────────────────────────────────────

    /// Map the current `(value, phase)` pair onto a `Display` variant.
    ///
    /// This is the **only** function a view needs to call. It implements the
    /// truth table documented in the module docs, producing the correct
    /// `Display` variant for every reachable state. The match is exhaustive:
    /// the compiler guarantees no state is forgotten.
    pub fn display(&self) -> Display<'_, T> {
        match (&self.value, self.phase) {
            // ── No prior value ─────────────────────────────────────────────
            (None, Phase::Idle) => Display::Empty,

            (None, Phase::Loading { since }) => Display::Skeleton { show_after: since },

            // The stream sent something but the drain closure hasn't applied
            // the value yet (brief race window). Use the current instant so
            // the skeleton grace starts now.
            (None, Phase::Streaming { .. }) => Display::Skeleton {
                show_after: Instant::now(),
            },

            // Cold-start failure: nothing to show but the error.
            (None, Phase::Failed { .. }) => {
                // Safety: fail() always sets self.error; this arm is only
                // reached via fail(), so unwrap is structurally guaranteed.
                Display::Error(self.error.as_ref().unwrap_or_else(|| {
                    // Belt-and-suspenders: should not happen, but avoids panic.
                    // A review finding if this is ever triggered in practice.
                    static FALLBACK: Error = Error::Cancelled;
                    &FALLBACK
                }))
            }

            // None + Ready is structurally prevented (complete() only called
            // after the drain sets the value), but handle it safely.
            (None, Phase::Ready { .. }) => Display::Empty,

            // ── Prior value exists ─────────────────────────────────────────
            // Refresh in progress — dim and shimmer the stale content.
            (Some(v), Phase::Loading { .. }) => Display::Stale(v),

            // Partial new content arriving — show it with streaming cues.
            (Some(v), Phase::Streaming { .. }) => Display::Partial(v),

            // Stream completed cleanly.
            (Some(v), Phase::Ready { .. }) => Display::Fresh(v),

            // Refresh failed — stale content + slim error bar.
            (Some(v), Phase::Failed { .. }) => Display::StaleWithError(
                v,
                self.error.as_ref().unwrap_or_else(|| {
                    static FALLBACK: Error = Error::Cancelled;
                    &FALLBACK
                }),
            ),

            // Some + Idle is prevented by reset() clearing the value, but
            // handle defensively.
            (Some(v), Phase::Idle) => Display::Stale(v),
        }
    }
}

impl<T> Default for StreamSlot<T> {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };

    // Helper: a slot with a value already set.
    fn slot_with_value(v: &str) -> StreamSlot<String> {
        StreamSlot {
            phase: Phase::Idle,
            value: Some(v.to_string()),
            error: None,
            generation: Gen(1),
            handle: None,
        }
    }

    // ── Display truth table ────────────────────────────────────────────────

    #[test]
    fn none_idle_is_empty() {
        let slot: StreamSlot<String> = StreamSlot::new();
        assert!(matches!(slot.display(), Display::Empty));
    }

    #[test]
    fn none_loading_is_skeleton() {
        let mut slot: StreamSlot<String> = StreamSlot::new();
        slot.begin_loading();
        assert!(matches!(slot.display(), Display::Skeleton { .. }));
    }

    #[test]
    fn none_streaming_is_skeleton() {
        let mut slot: StreamSlot<String> = StreamSlot::new();
        slot.begin_loading();
        slot.first_event();
        // No value assigned yet.
        assert!(matches!(slot.display(), Display::Skeleton { .. }));
    }

    #[test]
    fn none_failed_is_error() {
        let mut slot: StreamSlot<String> = StreamSlot::new();
        slot.begin_loading();
        slot.fail(Error::Cancelled);
        assert!(matches!(slot.display(), Display::Error(_)));
    }

    #[test]
    fn some_loading_is_stale() {
        let mut slot = slot_with_value("old");
        slot.begin_loading();
        assert!(matches!(slot.display(), Display::Stale(_)));
    }

    #[test]
    fn some_streaming_is_partial() {
        let mut slot = slot_with_value("partial");
        slot.begin_loading();
        slot.first_event();
        // Simulate drain applying a partial value.
        slot.value = Some("partial".to_string());
        assert!(matches!(slot.display(), Display::Partial(_)));
    }

    #[test]
    fn some_ready_is_fresh() {
        let mut slot = slot_with_value("done");
        slot.begin_loading();
        slot.first_event();
        slot.complete();
        assert!(matches!(slot.display(), Display::Fresh(_)));
    }

    #[test]
    fn some_failed_is_stale_with_error() {
        let mut slot = slot_with_value("old");
        slot.begin_loading();
        slot.fail(Error::Transient {
            message: "network".to_string(),
        });
        assert!(matches!(slot.display(), Display::StaleWithError(_, _)));
    }

    // ── State machine invariants ──────────────────────────────────────────

    #[test]
    fn begin_loading_preserves_value() {
        let mut slot = slot_with_value("stale");
        slot.begin_loading();
        assert!(
            slot.value.is_some(),
            "value must survive begin_loading (LD-15)"
        );
    }

    #[test]
    fn begin_loading_clears_error() {
        let mut slot: StreamSlot<String> = StreamSlot::new();
        slot.begin_loading();
        slot.fail(Error::Cancelled);
        slot.begin_loading();
        assert!(slot.error.is_none(), "error cleared on new loading cycle");
    }

    #[test]
    fn complete_drops_handle() {
        let counter = Arc::new(AtomicUsize::new(0));
        let c = counter.clone();
        let mut slot: StreamSlot<String> = StreamSlot::new();
        slot.begin_loading();
        slot.handle = Some(crate::bridge::handle::StreamHandle::new(
            Gen(1),
            move || {
                c.fetch_add(1, Ordering::SeqCst);
            },
        ));
        slot.value = Some("done".to_string());
        slot.complete();
        // Handle was dropped by complete().
        assert!(slot.handle.is_none());
        assert_eq!(
            counter.load(Ordering::SeqCst),
            1,
            "canceller fired on complete"
        );
    }

    #[test]
    fn first_event_is_idempotent_after_streaming() {
        let mut slot: StreamSlot<String> = StreamSlot::new();
        slot.begin_loading();
        slot.first_event();
        let phase_after_first = slot.phase;
        slot.first_event();
        assert_eq!(
            slot.phase, phase_after_first,
            "second first_event is a no-op"
        );
    }

    #[test]
    fn reset_clears_everything() {
        let mut slot = slot_with_value("old");
        slot.begin_loading();
        slot.fail(Error::Cancelled);
        slot.reset();
        assert!(matches!(slot.phase, Phase::Idle));
        assert!(slot.value.is_none());
        assert!(slot.error.is_none());
        assert!(slot.handle.is_none());
    }

    // ── Grace period tests ─────────────────────────────────────────────────

    #[test]
    fn skeleton_grace_false_immediately_after_loading() {
        let mut slot: StreamSlot<String> = StreamSlot::new();
        slot.begin_loading();
        // Immediately after begin_loading, 120 ms has not elapsed.
        assert!(!slot.show_skeleton());
    }

    #[test]
    fn spinner_grace_false_immediately_after_loading() {
        let mut slot: StreamSlot<String> = StreamSlot::new();
        slot.begin_loading();
        assert!(!slot.show_spinner());
    }

    #[test]
    fn grace_elapsed_for_old_phase() {
        // Construct a Loading phase with a timestamp well in the past.
        let old_since = Instant::now() - Duration::from_millis(500);
        let phase = Phase::Loading { since: old_since };
        assert!(phase.skeleton_grace_elapsed(), "120 ms has elapsed");
        assert!(phase.spinner_grace_elapsed(), "300 ms has elapsed");
    }
}
