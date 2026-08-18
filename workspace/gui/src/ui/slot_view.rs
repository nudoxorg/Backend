//! `slot_view` — the universal data-surface renderer (GUI-PLAN §8.1 / §11).
//!
//! `slot_view` makes it impossible for a view to forget a loading state, because
//! the compiler enumerates them: every arm of `Display<T>` must be handled or
//! the code does not compile. The seven states map directly onto the §5.2 motion
//! vocabulary, and all timing logic is delegated to `StreamSlot`'s helpers so
//! this function contains zero `Instant` arithmetic.
//!
//! # LD-15 guarantee
//!
//! Stale content is never discarded when a refresh starts. The `Stale` and
//! `StaleWithError` arms always render the prior value at 70 % opacity; a skeleton
//! is only shown on a cold load (`Skeleton` arm, no prior value).
//!
//! # Skeleton shimmer and the loop-permit system
//!
//! gpui-component's `Skeleton` component drives its own shimmer internally and is
//! not counted against the §5.4 loop-permit census. Our manual `shimmer()` helper
//! (for the stale-overlay strip) *does* require a permit; callers pass
//! `shimmer_permit` and the function applies animation only when `Some`.
//!
//! # Spinner grace
//!
//! A spinner appears in the `Partial` arm only after `StreamSlot::show_spinner()`
//! returns `true` (300 ms after `Loading`), preventing flicker on fast loads.

use gpui::{AnyElement, App, IntoElement, Window, div, prelude::FluentBuilder as _};
use gpui_component::{ActiveTheme as _, StyledExt as _, skeleton::Skeleton, spinner::Spinner};

use crate::bridge::slot::{Display as SlotDisplay, Error, SKELETON_GRACE, StreamSlot};
use crate::motion::declarative::shimmer;
use crate::motion::permits::LoopPermit;
use crate::motion::tokens::MotionTokens;
use crate::theme::tokens::AlphaTokens;
use crate::ui::error_state::ErrorState;

// ── SlotContentState ─────────────────────────────────────────────────────────

/// Tells a content renderer what state the slot is currently in.
///
/// Passed as the second argument to the `render_content` closure so that
/// content can add its own affordances (e.g. a streaming indicator, dimming)
/// without needing access to the slot itself.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SlotContentState {
    /// The stream completed cleanly. Render normally.
    Fresh,
    /// Partial content is still arriving. The caller may add streaming cues.
    Partial,
    /// A prior value is shown while a refresh is in progress (LD-15).
    Stale,
}

// ── slot_view ────────────────────────────────────────────────────────────────

/// Render a [`StreamSlot`] into the correct visual frame for its current phase.
///
/// This is the single entry point every data surface uses. Matching on
/// `slot.display()` here (rather than in each view) means no view can
/// accidentally forget the `Skeleton`, `Stale`, or `Error` states.
///
/// # Arguments
///
/// - `slot` — the slot to render.
/// - `motion` — current motion tokens (used for the spinner colour).
/// - `shimmer_permit` — a `LoopPermit` acquired from `MotionTokens::acquire_loop_slot()`.
///   When `Some`, the stale-overlay shimmer strip animates. When `None` (census full),
///   the strip renders at its midpoint opacity (0.55). The *caller's view* owns the
///   permit in its state field so it persists across frames.
/// - `render_content` — closure that renders the slot's value. Receives the value and
///   a [`SlotContentState`] so it can add its own affordances.
///
/// # LD-15 guarantee
///
/// `begin_loading` on the slot never clears the stored value. When a refresh
/// starts while content is visible, this function renders `Stale(v)` — the
/// old content dimmed to 70 % — never a blank skeleton.
pub fn slot_view<T, F>(
    slot: &StreamSlot<T>,
    _motion: &MotionTokens,
    shimmer_permit: Option<LoopPermit>,
    render_content: F,
) -> AnyElement
where
    F: Fn(&T, SlotContentState) -> AnyElement,
{
    match slot.display() {
        // ── No prior value, no request yet ───────────────────────────────────
        SlotDisplay::Empty => div().into_any_element(),

        // ── Cold load: skeleton placeholder ──────────────────────────────────
        SlotDisplay::Skeleton { show_after } => {
            if show_after.elapsed() < SKELETON_GRACE {
                // Grace period has not elapsed — render a transparent spacer so
                // layout is stable but no skeleton flashes for fast local loads.
                div().w_full().into_any_element()
            } else {
                // Grace elapsed: show shimmer skeleton blocks.
                // gpui-component's Skeleton drives its own animation internally;
                // it is not counted against the LoopPermit census.
                div()
                    .w_full()
                    .v_flex()
                    .gap_2()
                    .child(Skeleton::new().w_full().h_6())
                    .child(Skeleton::new().w_full().h_4())
                    .child(Skeleton::new().w_3_4().h_4())
                    .into_any_element()
            }
        }

        // ── Prior value, refresh in progress — dim + shimmer strip ───────────
        SlotDisplay::Stale(v) => {
            let content = render_content(v, SlotContentState::Stale);
            // Shimmer strip at the bottom of the stale content.
            let strip = Skeleton::new().w_full().h_1();
            let animated_strip: AnyElement = if shimmer_permit.is_some() {
                // Hold permit for this frame by dropping it at end of this arm.
                // The CALLER re-acquires each frame (or keeps it in state).
                shimmer(strip, "ui.slot_view.stale_strip").into_any_element()
            } else {
                // Census full: static at midpoint opacity (§5.4 load shedding).
                // No `cx` in this free function, so the ladder is reached via
                // its canonical constant rather than `ext.alpha` — the value
                // is the same in every theme by construction.
                strip.opacity(AlphaTokens::STANDARD.half).into_any_element()
            };

            div()
                .w_full()
                .v_flex()
                .child(
                    div()
                        .w_full()
                        .opacity(AlphaTokens::STANDARD.dim)
                        .child(content),
                )
                .child(animated_strip)
                .into_any_element()
        }

        // ── Partial content arriving ──────────────────────────────────────────
        SlotDisplay::Partial(v) => {
            let content = render_content(v, SlotContentState::Partial);
            // Show a spinner only after the 300 ms grace period (§5.1).
            let show_spinner = slot.show_spinner();
            div()
                .w_full()
                .v_flex()
                .child(content)
                .when(show_spinner, |el| {
                    el.child(div().flex().justify_end().p_1().child(Spinner::new()))
                })
                .into_any_element()
        }

        // ── Stream complete, fresh content ────────────────────────────────────
        SlotDisplay::Fresh(v) => render_content(v, SlotContentState::Fresh),

        // ── Cold-load error: no prior value ───────────────────────────────────
        SlotDisplay::Error(e) => {
            // Delegate to ErrorState. No retry callback here; caller wraps
            // slot_view and owns the retry action.
            ErrorState::new(e.clone(), None).into_any_element()
        }

        // ── Refresh failed but prior content is still readable ────────────────
        SlotDisplay::StaleWithError(v, e) => {
            let content = render_content(v, SlotContentState::Stale);
            // Slim error bar at the bottom (LD-16: errors are states, not dialogs).
            let error_bar = slim_error_bar(e);

            div()
                .w_full()
                .v_flex()
                .child(
                    div()
                        .w_full()
                        .opacity(AlphaTokens::STANDARD.dim)
                        .child(content),
                )
                .child(error_bar)
                .into_any_element()
        }
    }
}

// ── Slim error bar ────────────────────────────────────────────────────────────

/// A one-line error bar for `StaleWithError` — keeps the page readable while
/// surfacing the failure (LD-16).
fn slim_error_bar(error: &Error) -> impl IntoElement {
    // We need cx for tokens but this is a free function without cx.
    // Use a RenderOnce wrapper to get cx at render time.
    SlimErrorBar {
        message: error_message(error),
    }
}

fn error_message(error: &Error) -> SharedString {
    match error {
        Error::Cancelled => SharedString::from("Request cancelled"),
        Error::Transient { message } => SharedString::from(message.as_str()),
        Error::Permanent { message } => SharedString::from(message.as_str()),
    }
}

use gpui::prelude::*;
use gpui::{IntoElement as _, RenderOnce, SharedString};

#[derive(IntoElement)]
struct SlimErrorBar {
    message: SharedString,
}

impl RenderOnce for SlimErrorBar {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        use crate::theme::ext::ThemeExtAccessor as _;
        let ext = cx.theme_ext();
        let theme = cx.theme();
        let sp = ext.space;
        let ts = ext.type_scale;
        let al = ext.alpha;

        div()
            .w_full()
            .h_flex()
            .gap(sp.space_2)
            .px(sp.space_3)
            .py(sp.space_1)
            .bg(theme.danger.opacity(al.hairline))
            .border_t_1()
            .border_color(theme.danger.opacity(al.tint))
            .child(
                div()
                    .text_size(ts.dense.size)
                    .line_height(ts.dense.line_height)
                    .text_color(theme.danger)
                    .child(self.message),
            )
    }
}
