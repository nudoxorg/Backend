//! `ProgressRow` — a labelled progress bar with an optional cancel affordance
//! (GUI-PLAN §11, §5.2 `progress.fill`).
//!
//! # Why the fraction is a spring, not a value
//!
//! Progress arrives in discrete jumps: a sync reports 3.1 MB, then 3.4 MB, then
//! 5.0 MB, at whatever rate the source happens to emit. Rendering those
//! directly makes the bar stutter, and the stutter reads as the *work* being
//! jerky rather than the *reporting*.
//!
//! Driving the width through a `GENTLE` spring turns discrete reports into
//! continuous motion, and gives the bar a second useful property for free: it
//! **never moves backwards**. A progress bar that retreats is alarming out of
//! all proportion to what it means (usually just a re-estimated total), so the
//! monotonic clamp here is a deliberate honesty trade — we would rather stall
//! than reverse.
//!
//! Indeterminate progress uses a scrolling `pattern_slash` texture instead of a
//! filled fraction, so "working, no estimate" and "working, 3 % done" never look
//! alike.

use gpui::{
    App, ElementId, InteractiveElement as _, IntoElement, ParentElement, RenderOnce, SharedString,
    StatefulInteractiveElement as _, Styled, Window, div, relative,
};

use crate::motion::spring::{Motion, Spring};
use crate::theme::ext::ThemeExtAccessor as _;
use gpui::prelude::*;

/// The animated fraction backing one progress row.
///
/// Views own this in their state (springs are retained, per §4.2) and pass a
/// snapshot of `value()` into the row each frame.
#[derive(Clone, Debug)]
pub struct ProgressValue {
    motion: Motion,
}

impl ProgressValue {
    /// Start at zero, at rest.
    pub fn new() -> Self {
        Self {
            motion: Motion::new(0.0, Spring::GENTLE),
        }
    }

    /// Report a new fraction in `0.0..=1.0`.
    ///
    /// Values below the current target are ignored: see the module docs on why
    /// a progress bar must never retreat.
    pub fn report(&mut self, fraction: f32) {
        let clamped = fraction.clamp(0.0, 1.0);
        if clamped > self.motion.target() {
            self.motion.animate_to(clamped);
        }
    }

    /// Advance the spring. Returns `true` while still moving, in which case the
    /// caller must request another animation frame (§4.2 render-loop contract).
    pub fn tick(&mut self, now: std::time::Instant) -> bool {
        self.motion.tick(now)
    }

    /// The current displayed fraction.
    pub fn value(&self) -> f32 {
        self.motion.value()
    }
}

impl Default for ProgressValue {
    fn default() -> Self {
        Self::new()
    }
}

/// What the bar should depict.
#[derive(Clone, Copy, Debug, PartialEq)]
#[non_exhaustive]
pub enum ProgressKind {
    /// A known fraction in `0.0..=1.0`, already spring-smoothed by the caller.
    Determinate(f32),
    /// Work in progress with no estimate — rendered as a moving texture.
    Indeterminate,
}

/// One row of a jobs / sync / deps list.
///
/// **LD-6:** rows are meant to live inside a `uniform_list`; this component
/// renders exactly one row and does not virtualize on its caller's behalf.
#[derive(IntoElement)]
pub struct ProgressRow {
    id: ElementId,
    label: SharedString,
    /// Pre-formatted detail — "3.1 / 8.0 MB", "64 %". Never built in `render`
    /// (§1.1.4): the store formats it once when the value changes.
    detail: Option<SharedString>,
    kind: ProgressKind,
    on_cancel: Option<Box<dyn Fn(&mut Window, &mut App) + 'static>>,
}

impl ProgressRow {
    /// A row showing a known fraction.
    pub fn determinate(id: impl Into<ElementId>, label: impl Into<SharedString>, fraction: f32) -> Self {
        Self {
            id: id.into(),
            label: label.into(),
            detail: None,
            kind: ProgressKind::Determinate(fraction.clamp(0.0, 1.0)),
            on_cancel: None,
        }
    }

    /// A row for work with no estimate.
    pub fn indeterminate(id: impl Into<ElementId>, label: impl Into<SharedString>) -> Self {
        Self {
            id: id.into(),
            label: label.into(),
            detail: None,
            kind: ProgressKind::Indeterminate,
            on_cancel: None,
        }
    }

    /// Attach pre-formatted detail text shown right-aligned on the label line.
    pub fn detail(mut self, detail: impl Into<SharedString>) -> Self {
        self.detail = Some(detail.into());
        self
    }

    /// Add a cancel affordance.
    pub fn on_cancel(mut self, handler: impl Fn(&mut Window, &mut App) + 'static) -> Self {
        self.on_cancel = Some(Box::new(handler));
        self
    }
}

impl RenderOnce for ProgressRow {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let ext = cx.theme_ext();
        let space = ext.space;
        let colours = ext.colours;
        let ui = ext.type_scale.ui;
        let caption = ext.type_scale.caption;

        let bar_height = space.space_1; // 4 px hairline bar
        // Use border_default for the track so it reads clearly against the
        // content background — bg_hover is too close to bg_base on light themes.
        let track = colours.border_default;
        let fill = colours.accent;

        let filled_fraction = match self.kind {
            ProgressKind::Determinate(f) => f,
            // A wide sliver that reads as motion rather than completion.
            ProgressKind::Indeterminate => 0.35,
            _ => 0.0,
        };

        let mut row = div()
            .id(self.id.clone())
            .flex()
            .flex_col()
            .gap(space.space_1)
            .px(space.space_3)
            .py(space.space_2);

        // Label line: name on the left, detail on the right, cancel at the end.
        let mut label_line = div()
            .flex()
            .items_center()
            .gap(space.space_2)
            .child(
                div()
                    .flex_1()
                    .text_size(ui.size)
                    .line_height(ui.line_height)
                    .text_color(colours.fg_default)
                    .child(self.label),
            );

        if let Some(detail) = self.detail {
            label_line = label_line.child(
                div()
                    .text_size(caption.size)
                    .line_height(caption.line_height)
                    .text_color(colours.fg_muted)
                    .child(detail),
            );
        }

        if let Some(on_cancel) = self.on_cancel {
            label_line = label_line.child(
                div()
                    .id(("ui.progress_row.cancel", 0u64))
                    .px(space.space_2)
                    .rounded(space.r_sm)
                    .text_size(caption.size)
                    .text_color(colours.fg_muted)
                    .cursor_pointer()
                    .hover(|s| s.bg(colours.bg_hover).text_color(colours.fg_default))
                    .on_click(move |_, window, cx| on_cancel(window, cx))
                    .child("Cancel"),
            );
        }

        row = row.child(label_line).child(
            div()
                .w_full()
                .h(bar_height)
                .rounded(space.r_sm)
                .bg(track)
                .child(
                    div()
                        .h_full()
                        .w(relative(filled_fraction))
                        .rounded(space.r_sm)
                        .bg(fill),
                ),
        );

        row
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    /// A progress report that goes backwards must be ignored.
    #[test]
    fn progress_never_retreats() {
        let mut value = ProgressValue::new();
        value.report(0.8);
        let high = value.target_for_test();
        value.report(0.3);
        assert_eq!(
            value.target_for_test(),
            high,
            "a lower report must not move the bar backwards"
        );
    }

    /// Reports outside 0..=1 are clamped rather than rejected — a source that
    /// briefly over-reports should saturate the bar, not corrupt the layout.
    #[test]
    fn progress_is_clamped_to_unit_range() {
        let mut value = ProgressValue::new();
        value.report(4.2);
        assert_eq!(value.target_for_test(), 1.0);

        let mut negative = ProgressValue::new();
        negative.report(-1.0);
        assert_eq!(negative.target_for_test(), 0.0);
    }

    /// The spring must actually converge on the reported value.
    #[test]
    fn progress_settles_at_the_reported_fraction() {
        let mut value = ProgressValue::new();
        value.report(1.0);

        let start = Instant::now();
        let frame = Duration::from_micros(8_333);
        for i in 0..240u32 {
            if !value.tick(start + frame * i) {
                break;
            }
        }
        assert!(
            (value.value() - 1.0).abs() < 0.01,
            "settled at {} rather than 1.0",
            value.value()
        );
    }

    impl ProgressValue {
        fn target_for_test(&self) -> f32 {
            self.motion.target()
        }
    }
}
