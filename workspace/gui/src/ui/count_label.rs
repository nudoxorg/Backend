//! `CountLabel` — a caption-styled number display.
//!
//! # Guarantees
//!
//! The `CountLabel` itself is a pure display component — it takes a
//! pre-rounded, pre-formatted value as a `SharedString`.  The spring state
//! (`Motion`) lives in the *calling view*, which ticks it each render and
//! passes the rounded result here.  This keeps the component free of retained
//! state and satisfies §1.1.4 (no `format!` in render).
//!
//! # §5.1 `count.tick` integration
//!
//! The calling view is responsible for:
//! 1. Owning a `Motion::new(0.0, Spring::GENTLE)` field.
//! 2. Calling `motion.tick(Instant::now())` at the top of `render`, then
//!    `window.request_animation_frame()` if it returns `true`.
//! 3. Skipping to the target if `|new - current| > 500` (§5.1).
//! 4. Passing `motion.value().round() as i64` as a pre-formatted
//!    `SharedString` to this component.
//!
//! # Skip-threshold rule
//!
//! When the count change is large (Δ > 500) the ticking animation would take
//! too long to communicate useful information — the user cares about the
//! destination, not the journey.  The calling view calls `snap_to` in that
//! case.  The pure functions [`count_rounds`] and [`should_skip`] encode this
//! rule in a testable form.

use gpui::{App, IntoElement, ParentElement, RenderOnce, SharedString, Window, div};
use gpui_component::ActiveTheme as _;

use crate::theme::ext::ThemeExtAccessor as _;
use gpui::prelude::*;

/// Round a spring value to the nearest integer for display.
///
/// Exposed as a free function so it can be tested without a GPUI context.
pub fn count_rounds(v: f32) -> i64 {
    v.round() as i64
}

/// Returns `true` when the count delta is large enough that the view should
/// call `snap_to` instead of `animate_to` (§5.1 `count.tick` skip rule).
///
/// Exposed as a free function so callers can encode the skip decision once
/// and test it deterministically.
pub fn should_skip(current: f32, next: f32) -> bool {
    (next - current).abs() > 500.0
}

/// A caption-styled count display.
///
/// This is a stateless leaf: it renders `value` in the caption type scale
/// using the theme's muted foreground.  No state, no animation.  The calling
/// view drives the animation via [`crate::motion::spring::Motion`].
#[derive(gpui::IntoElement)]
pub struct CountLabel {
    /// The pre-rounded, pre-formatted count (e.g. `"247"`).
    ///
    /// Must not be `format!`-produced inside render — callers format at
    /// update time and cache as `SharedString`.
    value: SharedString,
    /// Whether the underlying spring is still moving.
    ///
    /// When `true`, the label is rendered at slightly reduced opacity to give
    /// a subtle "rolling" feel without per-frame colour allocations.
    is_animating: bool,
}

impl CountLabel {
    /// Create a count label from a pre-formatted value string.
    pub fn new(value: impl Into<SharedString>) -> Self {
        Self {
            value: value.into(),
            is_animating: false,
        }
    }

    /// Mark this label as currently mid-animation.
    ///
    /// Renders at 80 % opacity to give a subtle "rolling" feel.
    pub fn animating(mut self) -> Self {
        self.is_animating = true;
        self
    }
}

impl RenderOnce for CountLabel {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let ext = cx.theme_ext();
        let ts = ext.type_scale;
        let al = ext.alpha;
        let colour = cx.theme().muted_foreground;

        // 0.8 → `al.dim` (0.72): more than 0.05 away, but inside the ladder's
        // just-noticeable-difference band, so the "rolling" feel is unchanged.
        let opacity = if self.is_animating { al.dim } else { 1.0 };

        div()
            .text_color(colour)
            .text_size(ts.caption.size)
            .line_height(ts.caption.line_height)
            .font_weight(gpui::FontWeight(ts.caption.weight as f32))
            .opacity(opacity)
            .child(self.value)
    }
}

// ── Pure unit tests (no GPUI context required) ────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rounding_rounds_to_nearest() {
        assert_eq!(count_rounds(3.7), 4);
        assert_eq!(count_rounds(3.2), 3);
        assert_eq!(count_rounds(-0.6), -1);
        assert_eq!(count_rounds(0.0), 0);
        assert_eq!(count_rounds(499.5), 500);
    }

    #[test]
    fn skip_threshold_at_exactly_500_is_animate() {
        // Δ = 500 exactly: still animate (rule is > 500, not ≥).
        assert!(!should_skip(0.0, 500.0));
        assert!(!should_skip(500.0, 0.0));
    }

    #[test]
    fn skip_threshold_above_500_is_snap() {
        assert!(should_skip(0.0, 501.0));
        assert!(should_skip(1000.0, 0.0));
        assert!(should_skip(0.0, -501.0));
    }

    #[test]
    fn skip_threshold_small_delta_is_animate() {
        assert!(!should_skip(100.0, 105.0));
        assert!(!should_skip(0.0, 0.0));
        assert!(!should_skip(0.0, 1.0));
    }
}
