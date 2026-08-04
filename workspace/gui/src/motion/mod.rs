//! Motion system — GUI-PLAN §4–§6.
//!
//! The motion system is split into two tiers (§4 / LD-4):
//!
//! ## Tier 1 — Declarative animations (`declarative.rs`)
//!
//! Based on GPUI's `Animation` / `AnimationExt::with_animation`. Used for
//! animations that are a pure function of time since mount and cannot be
//! interrupted: row entrances, shimmer skeletons, badge pop, nav flash, exit
//! fades. These are zero-maintenance: `AnimationElement` self-invalidates
//! until done (or forever for `.repeat()`), so no view entity is notified.
//!
//! ## Tier 2 — Retained spring physics (`spring.rs`, `motion2.rs`, `color.rs`)
//!
//! For anything whose target can change mid-flight: dock widths, collapse/
//! expand, pan/zoom, count tickers, graph node positions. Uses critically-
//! damped springs with velocity preservation across retarget. No GPUI
//! dependency in this tier — all physics is plain `f32` arithmetic and
//! testable without a window.
//!
//! ## Token vocabulary (`tokens.rs`)
//!
//! Single source of truth for every animation duration in the app. Views
//! consume tokens, never raw `Duration::from_millis(...)`. All durations are
//! pre-scale; `MotionTokens::scaled` multiplies by the user's `MotionScale`.
//!
//! ## Loop permits (`permits.rs`)
//!
//! RAII mechanism enforcing the §5.4 cap of 3 concurrent infinite-loop
//! animations. See [`permits::LoopCensus`] and [`tokens::MotionTokens::acquire_loop_slot`].
//!
//! ## The render-loop contract (§4.2)
//!
//! Every view that uses retained springs follows this exact pattern:
//!
//! ```rust,ignore
//! impl Render for MyView {
//!     fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
//!         let now = std::time::Instant::now();
//!         let mut animating = false;
//!         animating |= self.my_motion.tick(now);
//!         if animating {
//!             window.request_animation_frame(); // notifies only THIS entity
//!         }
//!         // ... build elements using self.my_motion.value()
//!     }
//! }
//! ```
//!
//! `window.request_animation_frame` (verified: `window.rs:2169`) notifies only
//! the currently-rendering entity on the next frame — a settling dock animates
//! alone, not the whole window.

pub mod color;
pub mod declarative;
pub mod motion2;
pub mod permits;
pub mod spring;
pub mod tokens;

pub use color::MotionColor;
pub use declarative::{
    delayed, empty_state_enter, entrance_id, fade_in, fade_out, overlay_out, rise_in,
    row_enter, shimmer,
};
pub use motion2::Motion2;
pub use permits::{LoopCensus, LoopPermit, MAX_LOOP_PERMITS};
pub use spring::{Motion, Spring};
pub use tokens::{
    MotionTokens,
    // §5.1 micro-feedback
    FOCUS_RING, COUNT_TICK_SPRING_HINT, STATUS_BREATHE, TOOLTIP_IN,
    // §5.2 content arrival
    SKELETON_SHIMMER, CONTENT_CROSSFADE_OUT, CONTENT_CROSSFADE_IN,
    ROW_CASCADE, STAGGER_STEP, MAX_STAGGER_ROWS, ROW_CASCADE_WINDOW,
    SECTION_ARRIVE, HIGHLIGHT_SWEEP, BADGE_POP, EDGE_FLOW, EMPTY_STATE,
    // §5.3 navigation & shell
    OVERLAY_OUT, PAGE_HANDOFF_OUT, PAGE_HANDOFF_IN, NAV_FLASH,
};
