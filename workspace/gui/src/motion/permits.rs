//! RAII loop-permit enforcement (GUI-PLAN §5.4).
//!
//! The motion budget caps concurrent infinite-loop animations at **3** on
//! screen at any time. Shimmer skeletons, status-breathing dots, and spinners
//! all count toward this cap.
//!
//! The enforcement mechanism is an RAII [`LoopPermit`]. Callers request a
//! permit from [`LoopCensus`] before starting a `.repeat()` animation; if
//! the cap is full, `acquire` returns `None` and the caller renders the
//! element statically at its midpoint value instead.
//!
//! Dropping a `LoopPermit` decrements the census. The census lives in
//! `MotionTokens` (see `tokens.rs`) so the cap is global to the window.
//!
//! # Why `Cell<u8>` and not `AtomicU8`?
//!
//! `MotionTokens` is a GPUI global, accessed only from the UI thread
//! (GPUI's `Window`/`App` types are not `Send`). A `Cell<u8>` is sufficient
//! and avoids the overhead of atomic operations in the hot render path.

use std::cell::Cell;
use std::rc::Rc;

/// Maximum concurrent infinite-loop animations allowed on screen (§5.4).
pub const MAX_LOOP_PERMITS: u8 = 3;

/// Shared, reference-counted loop census counter.
///
/// Kept separately from `MotionTokens` so that `LoopPermit` can hold a
/// clone of the `Rc` and decrement on drop without borrowing `MotionTokens`.
#[derive(Clone, Debug, Default)]
pub struct LoopCensus {
    count: Rc<Cell<u8>>,
}

impl LoopCensus {
    /// Create a new census with zero active loops.
    pub fn new() -> Self {
        Self {
            count: Rc::new(Cell::new(0)),
        }
    }

    /// Try to acquire one loop permit.
    ///
    /// Returns `Some(LoopPermit)` if the census is below [`MAX_LOOP_PERMITS`],
    /// `None` otherwise. The caller must hold the permit for the lifetime of
    /// the loop; dropping it releases the slot.
    pub fn acquire(&self) -> Option<LoopPermit> {
        let current = self.count.get();
        if current >= MAX_LOOP_PERMITS {
            return None;
        }
        self.count.set(current + 1);
        Some(LoopPermit {
            census: Rc::clone(&self.count),
        })
    }

    /// Number of permits currently held.
    pub fn active(&self) -> u8 {
        self.count.get()
    }
}

/// RAII guard that holds one loop-animation slot.
///
/// Returned by [`LoopCensus::acquire`]. Dropping this releases the slot,
/// allowing the next caller to acquire it.
///
/// Callers that receive `Some(permit)` must keep the permit alive for as
/// long as the `.repeat()` animation is visible. In practice: store it in
/// the element's render state or in the view's state struct alongside the
/// animation ID.
pub struct LoopPermit {
    census: Rc<Cell<u8>>,
}

impl Drop for LoopPermit {
    fn drop(&mut self) {
        let n = self.census.get();
        // Saturating sub guards against logic bugs that double-drop (should
        // never happen with proper ownership, but a panic here would crash
        // the render thread).
        self.census.set(n.saturating_sub(1));
    }
}

// LoopPermit is intentionally not Clone or Copy — one permit = one slot.

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn census_caps_at_three() {
        let census = LoopCensus::new();

        let p1 = census.acquire();
        let p2 = census.acquire();
        let p3 = census.acquire();
        let p4 = census.acquire();

        assert!(p1.is_some(), "first permit should succeed");
        assert!(p2.is_some(), "second permit should succeed");
        assert!(p3.is_some(), "third permit should succeed");
        assert!(p4.is_none(), "fourth permit should fail (cap = 3)");
        assert_eq!(census.active(), 3);
    }

    #[test]
    fn permit_releases_on_drop() {
        let census = LoopCensus::new();

        {
            let _p1 = census.acquire().expect("should acquire");
            let _p2 = census.acquire().expect("should acquire");
            assert_eq!(census.active(), 2);
        }

        // After block: both permits dropped.
        assert_eq!(census.active(), 0);

        // Can acquire again.
        let p = census.acquire();
        assert!(p.is_some());
        assert_eq!(census.active(), 1);
    }

    #[test]
    fn census_never_exceeds_max() {
        let census = LoopCensus::new();
        let mut permits = Vec::new();

        for _ in 0..10 {
            if let Some(p) = census.acquire() {
                permits.push(p);
            }
        }

        assert_eq!(
            census.active(),
            MAX_LOOP_PERMITS,
            "census must never exceed MAX_LOOP_PERMITS"
        );
        assert_eq!(permits.len(), MAX_LOOP_PERMITS as usize);

        // Drop all.
        drop(permits);
        assert_eq!(census.active(), 0);
    }
}
