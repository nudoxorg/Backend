//! `Progressive<T>` — an append-only accumulator for streamed documents.
//!
//! ## Why append-only?
//!
//! A symbol page, a refs list, and a timeline all arrive as ordered event
//! streams. Naively storing them in a `Vec` that could be modified arbitrarily
//! (insert, remove, reorder) would allow the UI to be jumpy: a section that
//! arrived first might be displaced by something that arrived later, causing
//! scroll-position jumps and re-layouts while the user is reading.
//!
//! The protocol invariant from GUI-PLAN §9.3 states that sections arrive in
//! `section_plan` order and are **append-only during a generation**. The GUI
//! never reflows earlier parts because a later part arrived. `Progressive<T>`
//! makes this invariant structural: there is no `remove`, no `insert_at`, and
//! no `IndexMut`. The only way to change a `Progressive` is to `push` a new
//! part onto the end or to `reset` it (which starts a new generation).
//!
//! The result is calm streamed reading: the user's scroll position is stable,
//! sections fade in at the bottom, and nothing that was already visible moves.
//!
//! ## `arrived` timestamps
//!
//! Each part carries an `Instant` recording when it was pushed. This drives the
//! entrance-animation windows from GUI-PLAN §4.1: sections only animate their
//! entrance within a 400 ms window after arrival; scrolling back to an already-
//! arrived section never replays the animation.
//!
//! `Progressive::arrival_window_open(idx, window)` answers whether part `idx`
//! should still animate (its arrival timestamp is within `window` of now).

use std::time::{Duration, Instant};

/// Append-only streamed document accumulator.
///
/// The type parameter `T` is the section or event type being accumulated
/// (e.g. `RenderSection`, `RefsPage`, `LineageEvent`).
///
/// ## Invariants maintained by this type
///
/// - `parts.len() == arrived.len()` at all times.
/// - `arrived` timestamps are monotonically non-decreasing.
/// - No part can be removed or replaced once pushed.
/// - `complete` transitions from `false` to `true` exactly once per
///   generation; it never goes back to `false` without a `reset`.
pub struct Progressive<T> {
    /// The accumulated parts in arrival order.
    ///
    /// Read-only after construction; use `push` to extend.
    parts: Vec<T>,
    /// Arrival timestamps parallel to `parts`.
    ///
    /// `arrived[i]` is the `Instant` at which `parts[i]` was pushed.
    arrived: Vec<Instant>,
    /// Whether the stream has closed cleanly (all parts received).
    complete: bool,
}

impl<T> Progressive<T> {
    /// Create an empty accumulator.
    pub fn new() -> Self {
        Self {
            parts: Vec::new(),
            arrived: Vec::new(),
            complete: false,
        }
    }

    /// Create an empty accumulator with a pre-allocated capacity.
    ///
    /// Use when the `section_plan` provides a part count.
    pub fn with_capacity(cap: usize) -> Self {
        Self {
            parts: Vec::with_capacity(cap),
            arrived: Vec::with_capacity(cap),
            complete: false,
        }
    }

    // ── Mutation (the narrow API that enforces append-only) ───────────────

    /// Append a new part with the current timestamp as its arrival instant.
    ///
    /// Panics in debug builds if `complete` is `true` — the protocol
    /// invariant states that no parts arrive after `Done`.
    pub fn push(&mut self, part: T) {
        self.push_at(part, Instant::now());
    }

    /// Append a new part with an explicit arrival instant.
    ///
    /// Used in tests where deterministic timestamps matter.
    ///
    /// Panics in debug builds if `complete` is `true`.
    pub fn push_at(&mut self, part: T, at: Instant) {
        debug_assert!(
            !self.complete,
            "push_at called after complete() — this is a protocol violation"
        );
        self.parts.push(part);
        self.arrived.push(at);
    }

    /// Mark the stream as complete (all parts received).
    ///
    /// After `complete()`, `push` will panic in debug builds. This is
    /// intentional: the engine must not emit parts after `Done`.
    pub fn complete(&mut self) {
        self.complete = true;
    }

    /// Reset to empty, ready for a new generation.
    ///
    /// Called by the store when a new query supersedes the previous one and
    /// the slot value is replaced.
    pub fn reset(&mut self) {
        self.parts.clear();
        self.arrived.clear();
        self.complete = false;
    }

    // ── Read access ────────────────────────────────────────────────────────

    /// The accumulated parts, in arrival order.
    pub fn parts(&self) -> &[T] {
        &self.parts
    }

    /// Arrival timestamps, parallel to `parts`.
    pub fn arrived(&self) -> &[Instant] {
        &self.arrived
    }

    /// Whether the stream has been marked complete.
    pub fn is_complete(&self) -> bool {
        self.complete
    }

    /// Number of parts received so far.
    pub fn len(&self) -> usize {
        self.parts.len()
    }

    /// `true` if no parts have been received yet.
    pub fn is_empty(&self) -> bool {
        self.parts.is_empty()
    }

    /// Access part `idx` and its arrival instant together.
    pub fn get(&self, idx: usize) -> Option<(&T, Instant)> {
        self.parts.get(idx).map(|p| (p, self.arrived[idx]))
    }

    // ── Animation window helper ────────────────────────────────────────────

    /// Returns `true` if part `idx` should still animate its entrance.
    ///
    /// Parts animate their entrance only within `window` of their arrival
    /// (GUI-PLAN §4.1 — default 400 ms). After the window expires,
    /// scrolling back to an already-arrived section must not replay the
    /// animation.
    ///
    /// Returns `false` for out-of-bounds indices.
    pub fn arrival_window_open(&self, idx: usize, window: Duration) -> bool {
        self.arrived
            .get(idx)
            .map(|t| t.elapsed() < window)
            .unwrap_or(false)
    }

    /// The `Instant` at which the most recent part arrived, if any.
    pub fn last_arrived(&self) -> Option<Instant> {
        self.arrived.last().copied()
    }
}

impl<T> Default for Progressive<T> {
    fn default() -> Self {
        Self::new()
    }
}

/// `Progressive` does not implement `IndexMut` — that would violate the
/// append-only invariant by allowing in-place mutation of arrived parts.
///
/// If a part needs to be updated (e.g. a highlight sweep enriching a code
/// section), the store holds the update data separately and merges at render
/// time, leaving the `Progressive` untouched.
impl<T> std::ops::Index<usize> for Progressive<T> {
    type Output = T;
    fn index(&self, idx: usize) -> &T {
        &self.parts[idx]
    }
}

impl<T: std::fmt::Debug> std::fmt::Debug for Progressive<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Progressive")
            .field("len", &self.parts.len())
            .field("complete", &self.complete)
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn push_preserves_order() {
        let mut p: Progressive<u32> = Progressive::new();
        p.push(10);
        p.push(20);
        p.push(30);
        assert_eq!(p.parts(), &[10, 20, 30]);
    }

    #[test]
    fn arrived_timestamps_parallel_to_parts() {
        let mut p: Progressive<&str> = Progressive::new();
        let t0 = Instant::now();
        p.push_at("a", t0);
        let t1 = t0 + Duration::from_millis(10);
        p.push_at("b", t1);
        let t2 = t1 + Duration::from_millis(10);
        p.push_at("c", t2);
        assert_eq!(p.len(), 3);
        assert_eq!(p.arrived().len(), 3);
        assert_eq!(p.arrived()[0], t0);
        assert_eq!(p.arrived()[1], t1);
        assert_eq!(p.arrived()[2], t2);
    }

    #[test]
    fn len_matches_parts() {
        let mut p: Progressive<i32> = Progressive::new();
        assert_eq!(p.len(), 0);
        p.push(1);
        assert_eq!(p.len(), 1);
        p.push(2);
        assert_eq!(p.len(), 2);
    }

    #[test]
    fn complete_sets_flag() {
        let mut p: Progressive<u8> = Progressive::new();
        assert!(!p.is_complete());
        p.complete();
        assert!(p.is_complete());
    }

    #[test]
    fn reset_clears_all_state() {
        let mut p: Progressive<String> = Progressive::new();
        p.push("hello".to_string());
        p.push("world".to_string());
        p.complete();
        p.reset();
        assert!(p.is_empty());
        assert!(!p.is_complete());
        assert_eq!(p.arrived().len(), 0);
    }

    #[test]
    fn get_returns_part_and_timestamp() {
        let t = Instant::now();
        let mut p: Progressive<&str> = Progressive::new();
        p.push_at("hello", t);
        let (part, arrived) = p.get(0).unwrap();
        assert_eq!(*part, "hello");
        assert_eq!(arrived, t);
    }

    #[test]
    fn get_out_of_bounds_is_none() {
        let p: Progressive<u32> = Progressive::new();
        assert!(p.get(0).is_none());
    }

    #[test]
    fn index_access_works() {
        let mut p: Progressive<i32> = Progressive::new();
        p.push(99);
        assert_eq!(p[0], 99);
    }

    #[test]
    fn arrival_window_open_within_window() {
        let mut p: Progressive<u8> = Progressive::new();
        p.push(0);
        // Immediately after push, a 400 ms window should still be open.
        assert!(p.arrival_window_open(0, Duration::from_millis(400)));
    }

    #[test]
    fn arrival_window_closed_for_old_part() {
        let old_time = Instant::now() - Duration::from_millis(500);
        let mut p: Progressive<u8> = Progressive::new();
        p.push_at(0, old_time);
        assert!(!p.arrival_window_open(0, Duration::from_millis(400)));
    }

    #[test]
    fn arrival_window_false_for_oob() {
        let p: Progressive<u8> = Progressive::new();
        assert!(!p.arrival_window_open(99, Duration::from_millis(400)));
    }

    #[test]
    #[should_panic(expected = "push_at called after complete()")]
    #[cfg(debug_assertions)]
    fn push_after_complete_panics_in_debug() {
        let mut p: Progressive<u8> = Progressive::new();
        p.complete();
        p.push(1); // should panic
    }

    #[test]
    fn last_arrived_none_when_empty() {
        let p: Progressive<u8> = Progressive::new();
        assert!(p.last_arrived().is_none());
    }

    #[test]
    fn last_arrived_reflects_most_recent() {
        let t0 = Instant::now();
        let t1 = t0 + Duration::from_millis(5);
        let mut p: Progressive<u8> = Progressive::new();
        p.push_at(0, t0);
        p.push_at(1, t1);
        assert_eq!(p.last_arrived(), Some(t1));
    }
}
