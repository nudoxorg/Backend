//! Declarative animation helpers (GUI-PLAN §4.1).
//!
//! This is the "declarative tier": one-shot or infinite-loop animations that
//! are a pure function of time since mount. They use GPUI's `Animation` +
//! `AnimationExt::with_animation` machinery, which self-invalidates until done
//! (or forever for `.repeat()`). No GPUI entity needs to be notified — the
//! `AnimationElement` does it internally by calling `window.request_animation_frame()`.
//!
//! # When to use the declarative tier vs springs
//!
//! - **Declarative**: animation has a fixed endpoint and cannot be interrupted
//!   mid-flight (entrances, fades, shimmer, badge pop, nav flash). The easing
//!   curve is chosen once; the target is baked in.
//! - **Springs** (`spring.rs`/`motion2d.rs`): the target can change at any time
//!   (dock toggle, selection bar, overlay) and velocity must be preserved across
//!   retarget. Springs live in view state; declarative animations do not.
//!
//! # MotionScale and §6.1
//!
//! Every helper accepts `&MotionTokens` and calls `tokens.scaled(duration)`.
//! When `scale == 0.0`, `scaled` returns `Duration::ZERO`. Every helper
//! detects `Duration::ZERO` and falls back to a static element at its final
//! state (opacity 1, zero offset), so the state change is still communicated
//! but without any time dimension. No call site ever branches on scale.
//!
//! # Entrance identity rule (§4.1 — read this before using `rise_in`)
//!
//! **Critical with `uniform_list`:** `uniform_list` mounts and unmounts rows
//! on scroll. Keying an entrance animation to the row index would re-run the
//! animation every time the row scrolls back into view. Instead, entrances are
//! keyed to *(generation, index)* and only fire within a 400 ms window after the
//! generation's first page landed (`ROW_CASCADE_WINDOW`). The caller is
//! responsible for the `gen_age < ROW_CASCADE_WINDOW` check; these helpers
//! wrap bare elements with no opinion about that gate.
//!
//! Use [`entrance_id`] to build the stable element ID per the naming convention.

use std::time::Duration;

// `Element` is imported for its `into_any` method: `AnimationElement<E>` gets
// `into_any` from `Element` (gpui `element.rs:123`), not from `IntoElement`,
// and a trait method is only callable with its trait in scope.
use gpui::{Animation, AnimationExt, Element, ElementId, IntoElement, Styled, px};

use crate::motion::tokens::{
    EMPTY_STATE, MAX_STAGGER_ROWS, MotionTokens, OVERLAY_OUT, ROW_CASCADE, SECTION_ARRIVE,
    SKELETON_SHIMMER, STAGGER_STEP,
};

// ── Core helpers ─────────────────────────────────────────────────────────────

/// Fade + 6 px rise entrance (GUI-PLAN §4.1 `rise_in`).
///
/// Applies `ease_out_quint`: fast attack, long soft tail — the right curve
/// for content arriving from below. Uses `mt` (margin-top) for the rise
/// because it is a leaf-local layout property; the relayout is bounded to the
/// element and its parent row, not the full window (LD-5).
///
/// When `tokens.scale == 0.0` the element renders at its final state
/// immediately (opacity 1, no offset) — scale-0 is an instant cut.
///
/// # Arguments
/// - `el` — any `IntoElement + Styled` leaf element.
/// - `id` — stable `ElementId`; use [`entrance_id`] for rows.
/// - `tokens` — the current `MotionTokens` (carries scale + loop census).
pub fn rise_in<E: IntoElement + Styled + 'static>(
    el: E,
    id: impl Into<ElementId>,
    tokens: &MotionTokens,
) -> gpui::AnyElement {
    let dur = tokens.scaled(SECTION_ARRIVE); // same duration as `section.arrive`

    if dur == Duration::ZERO {
        // Scale-0: instant cut — return element at its settled state.
        // We wrap in a trivial animation that finishes in 0 ms. However since
        // `Duration::ZERO` is not meaningful for an Animation, we render the
        // element directly at opacity 1 with no offset by just returning it
        // unchanged. Use the unstable `EitherElement` trick by boxing.
        // `.opacity(1.0)` / `.opacity(0.0)` throughout this file are animation
        // identity endpoints — fully shown / fully hidden — not tints, so they
        // stay off the `AlphaTokens` ladder deliberately.
        return el
            .opacity(1.0)
            .with_animation(id, Animation::new(Duration::from_millis(1)), |el, _t| {
                el.opacity(1.0)
            })
            .into_any();
    }

    el.with_animation(
        id,
        Animation::new(dur).with_easing(gpui::ease_out_quint()),
        |el, t| {
            // At t=0: opacity 0, 6 px below final position.
            // At t=1: opacity 1, 0 px offset.
            // `mt` is the macro-generated margin-top setter (gpui_macros styles.rs:713).
            el.opacity(t).mt(px(6.0 * (1.0 - t)))
        },
    )
    .into_any()
}

/// Fade-in only — no rise (GUI-PLAN §5.1 `tooltip.in`, §5.3 `overlay.out`).
///
/// Use for: tooltips (must feel weightless), overlay exit fades, log row
/// entrance while flooding. The absence of vertical movement is intentional
/// for contexts where spatial stability matters more than theatricality.
///
/// Accepts a custom `duration` so callers can use any token constant.
pub fn fade_in<E: IntoElement + Styled + 'static>(
    el: E,
    id: impl Into<ElementId>,
    duration: Duration,
    tokens: &MotionTokens,
) -> gpui::AnyElement {
    let dur = tokens.scaled(duration);

    if dur == Duration::ZERO {
        return el
            .opacity(1.0)
            .with_animation(id, Animation::new(Duration::from_millis(1)), |el, _t| {
                el.opacity(1.0)
            })
            .into_any();
    }

    el.with_animation(
        id,
        Animation::new(dur).with_easing(gpui::linear),
        |el, t| el.opacity(t),
    )
    .into_any()
}

/// Fade-out — renders at opacity `1 - t` over `duration`.
///
/// Used for `overlay.out` (120 ms `ease_in_cubic`) and `page.handoff` outgoing
/// context fade (80 ms). The caller should remove the element from the tree
/// once the animation completes (GPUI's `oneshot = true` by default).
///
/// When `tokens.scale == 0.0`, returns the element at opacity 0 (already gone).
pub fn fade_out<E: IntoElement + Styled + 'static>(
    el: E,
    id: impl Into<ElementId>,
    duration: Duration,
    tokens: &MotionTokens,
) -> gpui::AnyElement {
    let dur = tokens.scaled(duration);

    if dur == Duration::ZERO {
        return el
            .opacity(0.0)
            .with_animation(id, Animation::new(Duration::from_millis(1)), |el, _t| {
                el.opacity(0.0)
            })
            .into_any();
    }

    // ease_in_cubic from gpui-component (animation.rs:38): slow start, fast end
    // — exits must feel swift, not linger.
    use gpui_component::animation::ease_in_cubic;
    el.with_animation(
        id,
        Animation::new(dur).with_easing(move |t| ease_in_cubic(t)),
        |el, t| el.opacity(1.0 - t),
    )
    .into_any()
}

/// Shimmer for skeleton loading states (GUI-PLAN §4.1 `skeleton.shimmer`).
///
/// Applies an infinite `pulsating_between(0.35, 0.75)` alpha loop. The
/// caller **must** hold a `LoopPermit` acquired from `tokens.acquire_loop_slot()`
/// before calling this; if the permit was denied (census full, §5.4), the
/// element should render at opacity `0.55` (the midpoint) with no animation.
///
/// This function does not itself check the census — the caller decides which
/// element gets to shimmer vs. render static. This design allows the caller
/// to hold the permit for as long as the element is mounted.
pub fn shimmer<E: IntoElement + Styled + 'static>(
    el: E,
    id: impl Into<ElementId>,
) -> impl IntoElement {
    el.with_animation(
        id,
        Animation::new(SKELETON_SHIMMER)
            .repeat()
            .with_easing(gpui::pulsating_between(0.35, 0.75)),
        |el, alpha| el.opacity(alpha),
    )
}

/// Staggered entrance for a row at position `ix` in a new result generation.
///
/// Computes a delay fraction from the row index (capped at `MAX_STAGGER_ROWS`)
/// and wraps the entrance easing with [`delayed`]. The combined animation
/// duration is `ROW_CASCADE + stagger_offset` but since GPUI `Animation`
/// covers the full `[0,1]` range, the delay is folded into the easing via the
/// `delayed` trick so the `Animation::new(dur)` duration remains `ROW_CASCADE`.
///
/// Use this **only** while inside the entrance window (`gen_age < ROW_CASCADE_WINDOW`).
/// Outside that window, render the element bare (no animation wrapper).
///
/// When `tokens.scale == 0.0` the element renders settled immediately.
pub fn row_enter<E: IntoElement + Styled + 'static>(
    el: E,
    id: impl Into<ElementId>,
    ix: usize,
    tokens: &MotionTokens,
) -> gpui::AnyElement {
    let dur = tokens.scaled(ROW_CASCADE);

    if dur == Duration::ZERO {
        return el
            .opacity(1.0)
            .with_animation(id, Animation::new(Duration::from_millis(1)), |el, _t| {
                el.opacity(1.0)
            })
            .into_any();
    }

    // Stagger delay as a fraction of the animation duration.
    // Cap at MAX_STAGGER_ROWS so row 9 and beyond have the same start time as row 8.
    let stagger_count = ix.min(MAX_STAGGER_ROWS);
    let stagger_dur = STAGGER_STEP * stagger_count as u32;
    let total_dur = dur + tokens.scaled(stagger_dur);

    // delay_frac = stagger_dur / total_dur (i.e. what fraction of the total
    // animation is "waiting for my slot").
    let delay_frac = if total_dur.is_zero() {
        0.0
    } else {
        stagger_dur.as_secs_f32() / total_dur.as_secs_f32()
    };

    let easing = delayed(delay_frac, gpui::ease_out_quint());

    el.with_animation(
        id,
        Animation::new(total_dur).with_easing(easing),
        |el, t| el.opacity(t).mt(px(6.0 * (1.0 - t))),
    )
    .into_any()
}

/// Empty state entrance — larger, calmer version of `rise_in` (§5.2).
///
/// Uses `EMPTY_STATE` (200 ms), which is the longest permitted declarative
/// duration. Empty states are designed screens (LD-16); they deserve an
/// entrance that lands rather than snaps.
pub fn empty_state_enter<E: IntoElement + Styled + 'static>(
    el: E,
    id: impl Into<ElementId>,
    tokens: &MotionTokens,
) -> gpui::AnyElement {
    let dur = tokens.scaled(EMPTY_STATE);

    if dur == Duration::ZERO {
        return el
            .opacity(1.0)
            .with_animation(id, Animation::new(Duration::from_millis(1)), |el, _t| {
                el.opacity(1.0)
            })
            .into_any();
    }

    el.with_animation(
        id,
        Animation::new(dur).with_easing(gpui::ease_out_quint()),
        |el, t| el.opacity(t).mt(px(10.0 * (1.0 - t))),
    )
    .into_any()
}

/// Overlay-exit fade (§5.3 `overlay.out` — 120 ms).
///
/// A thin wrapper over [`fade_out`] with the canonical duration.
pub fn overlay_out<E: IntoElement + Styled + 'static>(
    el: E,
    id: impl Into<ElementId>,
    tokens: &MotionTokens,
) -> gpui::AnyElement {
    fade_out(el, id, OVERLAY_OUT, tokens)
}

// ── Easing combinator ────────────────────────────────────────────────────────

/// Delay the start of `ease` by `delay_frac` of the animation's duration (§4.1).
///
/// When `t ≤ delay_frac`, returns `0.0` — the animation has not yet started.
/// When `t > delay_frac`, maps the remaining time `(t - delay_frac) /
/// (1 - delay_frac)` through `ease`. This allows a single `Animation` to
/// encode both a delay and an eased motion, which is more efficient than
/// `with_animations` chains for uniform stagger patterns.
///
/// # Arguments
/// - `delay_frac`: fraction of total duration to spend at `0.0`. Must be in
///   `[0.0, 1.0)`.
/// - `ease`: any `Fn(f32) -> f32` easing function (typically `ease_out_quint()`).
///
/// # Example
/// ```ignore
/// // 25 % delay, then `ease_out_quint` for the rest:
/// let easing = delayed(0.25, gpui::ease_out_quint());
/// Animation::new(dur).with_easing(easing)
/// ```
pub fn delayed(
    delay_frac: f32,
    ease: impl Fn(f32) -> f32 + 'static,
) -> impl Fn(f32) -> f32 + 'static {
    move |t| {
        if t <= delay_frac {
            0.0
        } else if (1.0 - delay_frac).abs() < f32::EPSILON {
            // Edge case: delay_frac ≈ 1.0 — nothing can run; return 1.0 at the end.
            if t >= 1.0 { 1.0 } else { 0.0 }
        } else {
            ease((t - delay_frac) / (1.0 - delay_frac))
        }
    }
}

// ── Entrance ID builder ───────────────────────────────────────────────────────

/// Build a stable `ElementId` for a row entrance animation, keyed on
/// `(gen_epoch, ix)` — NOT on row visibility.
///
/// # The problem this solves (§4.1 entrance identity rule)
///
/// `uniform_list` mounts and unmounts rows as the user scrolls. If the
/// `AnimationElement`'s id were keyed only on row index (e.g. `("row", ix)`),
/// GPUI would find the existing `AnimationState` from the last time the row
/// was visible — and if that state showed a completed animation, the row would
/// not animate again when a *new generation* appears at the same index. Worse,
/// if the state was in-progress, re-keying the same id to a new row would
/// resume the old animation mid-flight.
///
/// The fix is to include `gen_epoch` in the id. When a new generation
/// arrives, all ids change, so GPUI allocates fresh `AnimationState` for each
/// row (start = now, animation_ix = 0). When the user scrolls and the same
/// row unmounts and remounts *within the same generation*, the `AnimationState`
/// is stale (animation already done), so GPUI returns `delta = 1.0` and the
/// element renders at its settled state — correct, no replay.
///
/// The 400 ms generation window (`ROW_CASCADE_WINDOW`) provides a belt-and-
/// suspenders guarantee: even if the state somehow were replayed, the caller
/// gate ensures no animation runs on old generations.
///
/// # Arguments
/// - `namespace`: a `'static str` prefix, e.g. `"search.row.enter"`. Must be
///   unique per list (different lists sharing a namespace would collide in
///   GPUI's `GlobalElementId` map).
/// - `gen_epoch`: the generation's first-arrival instant serialised as a
///   stable `u64` (e.g. `generation.0` from `bridge::generation::Gen`). Using the
///   monotonic gen counter rather than `Instant::now()` ensures the id is
///   stable across multiple renders within the same generation.
/// - `ix`: zero-based row index within the generation.
pub fn entrance_id(namespace: &'static str, gen_epoch: u64, ix: usize) -> ElementId {
    // We need a single ElementId that encodes three fields. GPUI's
    // `NamedInteger` encodes (name, u64) so we XOR-pack gen_epoch and ix
    // together. We shift gen_epoch up by 20 bits (allowing up to ~1 M rows
    // per gen before collision) and OR in ix. This is safe for the expected
    // values: gen counters fit in 44 bits and row counts fit in 20 bits.
    //
    // An alternative would be `ElementId::NamedChild` chaining, but that
    // allocates an Arc; the bit-pack avoids the allocation in the hot render path.
    let packed = (gen_epoch << 20) | (ix as u64 & 0xF_FFFF);
    ElementId::named_usize(namespace, packed as usize)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn delayed_returns_zero_during_delay() {
        let ease = delayed(0.25, gpui::ease_out_quint());
        assert_eq!(ease(0.0), 0.0);
        assert_eq!(ease(0.1), 0.0);
        assert_eq!(ease(0.25), 0.0);
    }

    #[test]
    fn delayed_runs_ease_after_delay() {
        let ease = delayed(0.25, gpui::ease_out_quint());
        // At t=1.0, should equal ease_out_quint()(1.0) = 1.0.
        let at_end = ease(1.0);
        assert!(
            (at_end - 1.0).abs() < 1e-5,
            "delayed at t=1 should be 1.0, got {at_end}"
        );

        // At t=0.625 (halfway through the active portion), should be ~ease(0.5).
        let reference = gpui::ease_out_quint()(0.5);
        let actual = ease(0.625);
        assert!(
            (actual - reference).abs() < 1e-5,
            "delayed at t=0.625 should equal ease(0.5)={reference}, got {actual}"
        );
    }

    #[test]
    fn delayed_zero_delay_is_passthrough() {
        let base = gpui::ease_out_quint();
        let wrapped = delayed(0.0, gpui::ease_out_quint());
        for t in [0.0f32, 0.25, 0.5, 0.75, 1.0] {
            let expected = base(t);
            let actual = wrapped(t);
            assert!(
                (actual - expected).abs() < 1e-5,
                "delayed(0.0) should be passthrough at t={t}: expected {expected}, got {actual}"
            );
        }
    }

    #[test]
    fn entrance_id_different_gens_differ() {
        let a = entrance_id("search.row.enter", 1, 0);
        let b = entrance_id("search.row.enter", 2, 0);
        assert_ne!(a, b, "different gens must produce different ids");
    }

    #[test]
    fn entrance_id_same_gen_ix_same() {
        let a = entrance_id("search.row.enter", 42, 7);
        let b = entrance_id("search.row.enter", 42, 7);
        assert_eq!(a, b, "same gen+ix must produce the same id");
    }

    #[test]
    fn entrance_id_different_ix_differ() {
        let a = entrance_id("search.row.enter", 5, 0);
        let b = entrance_id("search.row.enter", 5, 1);
        assert_ne!(a, b, "different row indices in same gen must differ");
    }

    #[test]
    fn entrance_id_different_namespace_differ() {
        let a = entrance_id("search.row.enter", 1, 0);
        let b = entrance_id("doc.section.enter", 1, 0);
        assert_ne!(a, b, "different namespaces must not collide");
    }
}
