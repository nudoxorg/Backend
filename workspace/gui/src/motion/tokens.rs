//! The motion token vocabulary (GUI-PLAN §5) — single source of truth.
//!
//! Every animation duration in lindsey is a named constant defined here.
//! Views refer to tokens, never to raw `Duration::from_millis(...)` literals.
//!
//! # How to use
//!
//! ```ignore
//! let tokens = cx.global::<MotionTokens>();
//! el.with_animation(id, Animation::new(tokens.scaled(RISE_IN)), ...)
//! ```
//!
//! [`MotionTokens::scaled`] multiplies by `MotionTokens.scale` so all
//! animations automatically honour the user's reduced-motion preference.
//! When `scale == 0.0`, `scaled` returns `Duration::ZERO`; helpers in
//! `declarative.rs` detect this and call `snap_to` instead of `animate_to`.
//!
//! # Budget enforcement (§5.4)
//!
//! - Every declarative (one-shot) animation duration must be ≤ 240 ms.
//! - Every spring must settle in ≤ 700 ms.
//! - The `const` assertions below enforce the first rule at compile time.
//!   Spring settlement is enforced by the unit tests in `spring.rs`.
//!
//! # Vocabulary coverage
//!
//! All tables in §5.1, §5.2, and §5.3 are represented. Tokens that are
//! handled by GPUI's built-in `hover:`/`active:` style states (zero-cost,
//! no timer) are documented here for completeness but have no numeric
//! duration — their "animation" is instantaneous state-driven style.

use std::time::Duration;

use gpui::Global;

use crate::motion::permits::{LoopCensus, LoopPermit, MAX_LOOP_PERMITS};

// ── §5.1 Micro-feedback ───────────────────────────────────────────────────────
// (pre-scale durations; all ≤ 240 ms per §5.4)

/// `focus.ring` — keyboard focus arrival, opacity 0→1, ring inset 3→1.5 px.
pub const FOCUS_RING: Duration = Duration::from_millis(140);

/// `count.tick` — displayed count rolls to new value (spring GENTLE ≤ 600 ms).
/// The spring duration is enforced by the settlement test in spring.rs, not here.
/// This constant marks the *intent* of the token for documentation; the actual
/// spring is wired as `Motion::GENTLE` in the view.
pub const COUNT_TICK_SPRING_HINT: Duration = Duration::from_millis(600);

/// `status.breathe` — backend/job dot glow pulse (infinite `.repeat()`).
pub const STATUS_BREATHE: Duration = Duration::from_millis(2_000);

/// `tooltip.in` — tooltip fade after 450 ms hover dwell.
pub const TOOLTIP_IN: Duration = Duration::from_millis(100);

/// `chevron.twirl` is a spring (DEFAULT), not a declarative animation.
/// Documented here for cross-reference; no `Duration` constant needed.

// `hover.tint` and `press.sink` are GPUI `hover:`/`active:` style states
// (zero-cost instant swaps). No timer needed; not representable as a
// declarative duration.

// ── §5.2 Content arrival ──────────────────────────────────────────────────────

/// `skeleton.shimmer` — shimmer on skeleton blocks while loading (`.repeat()`).
pub const SKELETON_SHIMMER: Duration = Duration::from_millis(1_200);

/// `content.crossfade` — stale→fresh content overlay crossfade (outgoing leg).
pub const CONTENT_CROSSFADE_OUT: Duration = Duration::from_millis(100);

/// `content.crossfade` — incoming content fade-in leg.
pub const CONTENT_CROSSFADE_IN: Duration = Duration::from_millis(140);

/// `row.cascade` — per-row entrance in a new result generation.
/// Add stagger per row index (see `STAGGER_STEP` and `MAX_STAGGER_ROWS`).
pub const ROW_CASCADE: Duration = Duration::from_millis(160);

/// `row.cascade` — stagger delay increment per row index.
pub const STAGGER_STEP: Duration = Duration::from_millis(16);

/// `row.cascade` — maximum number of rows that receive stagger delay.
/// Rows beyond this index all start together (avoiding 8+ row delays).
pub const MAX_STAGGER_ROWS: usize = 8;

/// `row.cascade` — generation entrance window. Rows older than this
/// render bare (no animation); scrolling never re-animates (§4.1 identity rule).
pub const ROW_CASCADE_WINDOW: Duration = Duration::from_millis(400);

/// `section.arrive` — streamed doc section entrance (same as `rise_in`).
pub const SECTION_ARRIVE: Duration = Duration::from_millis(160);

/// `highlight.sweep` — syntax highlight token color fade (§5.2).
pub const HIGHLIGHT_SWEEP: Duration = Duration::from_millis(180);

/// `badge.pop` — trust/status badge value change (soft overshoot).
pub const BADGE_POP: Duration = Duration::from_millis(180);

/// `edge.flow` — animated dash offset on graph edges (`.repeat()`, linear).
pub const EDGE_FLOW: Duration = Duration::from_millis(900);

/// `empty.state` — empty state illustration entrance.
pub const EMPTY_STATE: Duration = Duration::from_millis(200);

// toast.in/out are springs (SNAPPY). See §5.3.

// graph.settle is a spring (GENTLE per node). See §5.2.

// ── §5.3 Navigation & shell ───────────────────────────────────────────────────

/// `overlay.out` — closing overlays/modals (declarative fade, exit is simpler
/// than entry per §5.3).
pub const OVERLAY_OUT: Duration = Duration::from_millis(120);

/// `page.handoff` — outgoing context fades when opening a symbol.
pub const PAGE_HANDOFF_OUT: Duration = Duration::from_millis(80);

/// `page.handoff` — incoming header rise after handoff.
pub const PAGE_HANDOFF_IN: Duration = Duration::from_millis(160);

/// `nav.flash` — tab-title background pulse on back/forward land.
pub const NAV_FLASH: Duration = Duration::from_millis(240);

// overlay.in is a spring (SNAPPY: opacity + 4 px rise). See §5.3.
// dock.slide is a spring (DEFAULT on width/height). See §5.3.
// tab.switch active-underline is a spring (SNAPPY). See §5.3.
// tab.reorder siblings are springs (DEFAULT). See §5.3.
// banner.drop is a spring (SNAPPY on height + fade). See §5.3.

// ── §5.4 Budget assertions ────────────────────────────────────────────────────

// Every declarative (one-shot) animation must be ≤ 240 ms.
// Springs are excluded — their settlement is tested dynamically in spring.rs.
const _: () = {
    macro_rules! assert_le_240ms {
        ($token:expr, $name:expr) => {
            assert!(
                $token.as_millis() <= 240,
                concat!(
                    "motion token ",
                    $name,
                    " exceeds the 240 ms declarative cap (§5.4)"
                ),
            );
        };
    }

    // §5.1
    assert_le_240ms!(FOCUS_RING, "FOCUS_RING");
    assert_le_240ms!(TOOLTIP_IN, "TOOLTIP_IN");

    // §5.2 (one-shot durations only; shimmer/breathe/edge-flow are infinite loops)
    assert_le_240ms!(CONTENT_CROSSFADE_OUT, "CONTENT_CROSSFADE_OUT");
    assert_le_240ms!(CONTENT_CROSSFADE_IN, "CONTENT_CROSSFADE_IN");
    assert_le_240ms!(ROW_CASCADE, "ROW_CASCADE");
    assert_le_240ms!(STAGGER_STEP, "STAGGER_STEP");
    assert_le_240ms!(SECTION_ARRIVE, "SECTION_ARRIVE");
    assert_le_240ms!(HIGHLIGHT_SWEEP, "HIGHLIGHT_SWEEP");
    assert_le_240ms!(BADGE_POP, "BADGE_POP");
    assert_le_240ms!(EMPTY_STATE, "EMPTY_STATE");

    // §5.3
    assert_le_240ms!(OVERLAY_OUT, "OVERLAY_OUT");
    assert_le_240ms!(PAGE_HANDOFF_OUT, "PAGE_HANDOFF_OUT");
    assert_le_240ms!(PAGE_HANDOFF_IN, "PAGE_HANDOFF_IN");
    assert_le_240ms!(NAV_FLASH, "NAV_FLASH");
};

// ── Max-loop-permits const assertion ─────────────────────────────────────────

const _: () = {
    assert!(
        MAX_LOOP_PERMITS == 3,
        "§5.4 requires exactly 3 concurrent infinite-loop permits"
    );
};

// ── MotionTokens ─────────────────────────────────────────────────────────────

/// Global motion configuration carried by every `MotionTokens`-aware helper.
///
/// `MotionTokens` is a GPUI global (`App::set_global`) — one per window (all
/// windows share the same instance in v1). It is updated by `SettingsStore`
/// when `motion_scale` or the OS reduced-motion flag changes.
///
/// The struct intentionally has no GPUI dependency so it can be constructed
/// in tests.
#[derive(Debug)]
pub struct MotionTokens {
    /// Multiplier applied to every animation duration. Ranges:
    /// - `1.0` — full fidelity (default)
    /// - `0.5` — reduced motion (dimmer, shorter)
    /// - `0.0` — off; all helpers become instant cuts
    pub scale: f32,

    /// Loop-animation census. Enforces the cap of [`MAX_LOOP_PERMITS`] (§5.4).
    pub loop_census: LoopCensus,
}

impl Default for MotionTokens {
    fn default() -> Self {
        Self::new(1.0)
    }
}

impl MotionTokens {
    /// Create `MotionTokens` with the given scale and an empty census.
    pub fn new(scale: f32) -> Self {
        Self {
            scale: scale.clamp(0.0, 1.0),
            loop_census: LoopCensus::new(),
        }
    }

    /// Scale `duration` by `self.scale`.
    ///
    /// Returns `Duration::ZERO` when `scale == 0.0`. The declarative helpers
    /// in `declarative.rs` detect a zero duration and call `snap_to` instead
    /// of `animate_to`, so every animation is inherently §6.1-correct.
    pub fn scaled(&self, duration: Duration) -> Duration {
        if self.scale == 0.0 {
            Duration::ZERO
        } else {
            duration.mul_f32(self.scale)
        }
    }

    /// Try to acquire a loop-animation permit (§5.4).
    ///
    /// Returns `Some(LoopPermit)` if fewer than `MAX_LOOP_PERMITS` infinite
    /// loops are currently active; `None` otherwise. The caller must render
    /// the element statically at its midpoint value when `None` is returned.
    ///
    /// Keep the permit alive for the duration of the animation — drop it when
    /// the animated element is unmounted.
    pub fn acquire_loop_slot(&self) -> Option<LoopPermit> {
        self.loop_census.acquire()
    }

    /// Number of loop permits currently held (for the HUD census display §25.2).
    pub fn active_loops(&self) -> u8 {
        self.loop_census.active()
    }
}

// Register MotionTokens as a GPUI global so views can access it via
// `cx.global::<MotionTokens>()`. `SettingsStore` calls
// `MotionTokens::set_global(cx, ...)` on startup and on `SettingsChanged`.
impl Global for MotionTokens {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scaled_returns_zero_at_scale_zero() {
        let t = MotionTokens::new(0.0);
        assert_eq!(t.scaled(ROW_CASCADE), Duration::ZERO);
        assert_eq!(t.scaled(SKELETON_SHIMMER), Duration::ZERO);
    }

    #[test]
    fn scaled_returns_full_at_scale_one() {
        let t = MotionTokens::new(1.0);
        assert_eq!(t.scaled(ROW_CASCADE), ROW_CASCADE);
        assert_eq!(t.scaled(OVERLAY_OUT), OVERLAY_OUT);
    }

    #[test]
    fn scaled_halves_at_reduced_motion() {
        let t = MotionTokens::new(0.5);
        let half = t.scaled(Duration::from_millis(200));
        assert_eq!(half.as_millis(), 100);
    }

    #[test]
    fn loop_census_cap_via_tokens() {
        let t = MotionTokens::new(1.0);
        let p1 = t.acquire_loop_slot();
        let p2 = t.acquire_loop_slot();
        let p3 = t.acquire_loop_slot();
        let p4 = t.acquire_loop_slot();

        assert!(p1.is_some());
        assert!(p2.is_some());
        assert!(p3.is_some());
        assert!(p4.is_none(), "fourth slot must be denied");
        assert_eq!(t.active_loops(), 3);

        drop(p1);
        assert_eq!(t.active_loops(), 2);
        let p5 = t.acquire_loop_slot();
        assert!(
            p5.is_some(),
            "slot released by drop must be available again"
        );
    }
}
