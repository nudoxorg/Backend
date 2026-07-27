//! Generation tokens — the identity axis of every query slot.
//!
//! A `Gen` is an opaque monotonically increasing integer that is stamped onto
//! every event that answers a slotted query (search, symbol open, package browse,
//! project resolve). Stores discard any event whose `gen` does not match the
//! **current** gen of the slot that owns the query. This makes staleness
//! structurally impossible: superseding a query atomically increments the generation, so
//! every in-flight event from the previous request simply falls through the stale
//! guard and is dropped before it can corrupt visible state.
//!
//! ## Design choices
//!
//! - **One `GenSource` per query slot, not one global counter.** Slots are
//!   independent; sharing a counter would couple their lifetimes and make
//!   debugging harder (you would need to know "what other slot bumped the
//!   counter?").
//!
//! - **No timestamps.** Timestamps are non-deterministic and make unit tests
//!   order-sensitive. `GenSource::next` produces `1, 2, 3, …` — deterministic
//!   and reproducible regardless of wall-clock speed.
//!
//! - **No global state.** There is nothing to initialise; the kernel is
//!   instantiated wherever a slot is declared and dropped with it.

use std::fmt;

/// An opaque monotonically increasing generation token.
///
/// Every stream event that answers a slotted query is tagged with the `Gen`
/// that was current when the query was issued. The store's drain closure
/// compares incoming `gen` values against the slot's current gen and drops
/// stale ones before they reach state.
///
/// `Gen(0)` is never issued by `GenSource::next` (the source starts at `0`
/// and returns `1` on the first call), so `Gen(0)` can be used as a
/// "not yet started" sentinel if needed.
///
/// # One `Gen`, not two
///
/// This is a **re-export** of `nudox_engine::wire::Gen`, not a parallel type.
///
/// A generation is only meaningful because both ends agree on it: the store
/// stamps a query, the engine echoes the stamp on every event, and the drain
/// closure compares them to drop stale answers. Defining a GUI-side `Gen`
/// alongside the wire one meant a lossy `Gen(other.0)` conversion at every
/// crossing — and a conversion is exactly the place where an off-by-one or a
/// swapped argument stops being a type error and becomes a stale row that
/// nobody notices.
///
/// Same argument as LR-1 for `SymbolKey`: an identifier shared across a seam
/// belongs to the protocol, and the protocol is `nudox-engine::wire`.
pub use nudox_engine::wire::Gen;

/// Per-slot generation counter.
///
/// Own one of these for each independent query slot in a store. Call
/// `next()` each time a new query supersedes the previous one; the returned
/// `Gen` is what you stamp onto the request and compare against incoming
/// events.
///
/// ```
/// # use lindsey::bridge::generation::{Gen, GenSource};
/// let mut src = GenSource::new();
/// let g1 = src.next();
/// let g2 = src.next();
/// assert!(g2 > g1);
/// ```
pub struct GenSource(u64);

impl GenSource {
    /// Create a new counter starting at zero (first `next()` call yields `Gen(1)`).
    pub fn new() -> Self {
        Self(0)
    }

    /// Advance the counter and return the next generation token.
    ///
    /// This is the only mutation point; the counter never goes backward.
    pub fn next(&mut self) -> Gen {
        self.0 += 1;
        Gen(self.0)
    }

    /// Peek at the most recently issued gen without advancing.
    ///
    /// Returns `Gen(0)` if `next` has never been called.
    pub fn current(&self) -> Gen {
        Gen(self.0)
    }
}

impl Default for GenSource {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn starts_at_one() {
        let mut src = GenSource::new();
        assert_eq!(src.next(), Gen(1));
    }

    #[test]
    fn strictly_monotonic() {
        let mut src = GenSource::new();
        let gens: Vec<Gen> = (0..100).map(|_| src.next()).collect();
        for w in gens.windows(2) {
            assert!(w[1] > w[0], "gen must be strictly increasing");
        }
    }

    #[test]
    fn independent_sources_do_not_interfere() {
        let mut a = GenSource::new();
        let mut b = GenSource::new();
        let ga = a.next();
        let gb = b.next();
        // Both start at 1 — they are independent, not shared.
        assert_eq!(ga, gb, "independent sources start at the same sequence");
        // Advancing one does not affect the other.
        let _ = a.next();
        let _ = a.next();
        assert_eq!(b.next(), Gen(2));
    }

    #[test]
    fn current_without_next_is_zero() {
        let src = GenSource::new();
        assert_eq!(src.current(), Gen(0));
    }

    #[test]
    fn current_reflects_last_issued() {
        let mut src = GenSource::new();
        let g = src.next();
        assert_eq!(src.current(), g);
    }
}
