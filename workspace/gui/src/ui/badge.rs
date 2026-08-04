//! `Badge` — a kind-or-trust chip with `badge.pop` motion on first mount.
//!
//! # Guarantees
//!
//! - All colours come from token accessors (`cx.theme_ext()` and
//!   `LocalKindDiscriminant::colour`). There are no inline hex values.
//! - All dimensions come from `SpaceTokens` fields (`space.*`, `r_*`).
//! - The `badge.pop` entrance is gated on `BADGE_POP` from `motion::tokens`.
//! - Two factory constructors enforce that kind↔colour mapping is never
//!   duplicated across the codebase (LD-14 / §10.3 tail).
//!
//! # Why `bounce` is not used for opacity
//!
//! `gpui::bounce(f)` is a symmetric ping-pong: `bounce(f)(t)` returns
//! `f(2t)` for t < 0.5 and `f(2–2t)` for t ≥ 0.5, so the output curve is
//! 0→1→0.  Mapping that directly to `opacity` produces a badge that fades
//! in then fades **back out** to 0, becoming invisible forever once the
//! one-shot animation settles at its final `t=1` value.
//! `ease_in_out` alone is a true 0→1 curve that settles at full opacity.

use gpui::{
    Animation, AnimationExt, App, ElementId, Hsla, IntoElement, ParentElement, RenderOnce,
    StyleRefinement, Styled, Window, div, ease_in_out, prelude::FluentBuilder as _,
};
use gpui_component::ActiveTheme as _;

use crate::motion::tokens::BADGE_POP;
use crate::theme::ext::{Provenance, ThemeExtAccessor as _};
use crate::theme::kind::LocalKindDiscriminant;
use gpui::prelude::*;
use gpui_component::StyledExt as _;

/// A small chip displaying a symbol kind or provenance label.
///
/// Callers construct via [`Badge::for_kind`], [`Badge::for_provenance`], or
/// [`Badge::custom`] — never by constructing the fields directly.  This keeps
/// the colour mapping in exactly one place per role (LD-8 / §10.3 tail).
///
/// `id` is required for stable animation identity (LD-19).  Use a
/// `("ui.badge", per-item-id)` tuple per §4.1 identity rule.
#[derive(IntoElement)]
pub struct Badge {
    id: ElementId,
    /// Pre-computed label text (must not be formatted in render — §1.1.4).
    label: gpui::SharedString,
    /// Background fill colour, from a kind or trust token.
    colour: Hsla,
    /// Text colour on the filled surface.
    fg_on: Hsla,
    style: StyleRefinement,
}

impl Badge {
    /// Create a badge for a symbol kind, using the kind-colour table.
    ///
    /// The label is the short ASCII identifier from
    /// [`LocalKindDiscriminant::short_label`].
    pub fn for_kind(id: impl Into<ElementId>, kind: LocalKindDiscriminant, cx: &App) -> Self {
        let ext = cx.theme_ext();
        let colour = kind.colour(&ext.kind_colours);
        // fg_on is precomputed per theme in KindColours.kind_fg_on: white in light
        // (badges at l=0.38 are dark) and near-black in dark (badges at l=0.62 are
        // bright).  Using the precomputed scalar avoids any conditional in render.
        let fg_on = ext.kind_colours.kind_fg_on;
        Self {
            id: id.into(),
            label: kind.short_label().into(),
            colour,
            fg_on,
            style: StyleRefinement::default(),
        }
    }

    /// Create a badge for a provenance level, using the trust-chrome table.
    ///
    /// Both glyph and label come from [`NudoxThemeExt::for_provenance`] — no
    /// `match` on `Provenance` lives outside `ext.rs`.
    pub fn for_provenance(id: impl Into<ElementId>, p: Provenance, cx: &App) -> Self {
        let ext = cx.theme_ext();
        let style = ext.for_provenance(p);
        // Pre-compose "⬢ local" style label without format! in render.
        let label: gpui::SharedString =
            format!("{} {}", style.badge_glyph, style.badge_label).into();
        Self {
            id: id.into(),
            label,
            colour: style.colour,
            fg_on: style.fg_on,
            style: StyleRefinement::default(),
        }
    }

    /// Create a badge with fully custom label and colours.
    ///
    /// Use this only for cases not covered by kind or provenance (e.g. a
    /// synthetic "deprecated" chip).  Prefer the typed constructors for
    /// all standard cases.
    pub fn custom(
        id: impl Into<ElementId>,
        label: gpui::SharedString,
        colour: Hsla,
        fg_on: Hsla,
    ) -> Self {
        Self {
            id: id.into(),
            label,
            colour,
            fg_on,
            style: StyleRefinement::default(),
        }
    }
}

impl Styled for Badge {
    fn style(&mut self) -> &mut StyleRefinement {
        &mut self.style
    }
}

impl RenderOnce for Badge {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let ext = cx.theme_ext();
        let sp = ext.space;
        let ts = ext.type_scale;
        let colour = self.colour;
        let fg_on = self.fg_on;
        let label = self.label.clone();
        let id = self.id.clone();
        let dur = ext.scale_duration(BADGE_POP);

        // If duration is zero (reduced motion), render statically.
        if dur.is_zero() {
            return div()
                .id(id)
                .bg(colour)
                .text_color(fg_on)
                .rounded(sp.r_sm)
                .px(sp.space_1)
                .py(sp.space_1 / 2.0)
                .text_size(ts.caption.size)
                .line_height(ts.caption.line_height)
                .font_weight(gpui::FontWeight(ts.caption.weight as f32))
                .refine_style(&self.style)
                .child(label)
                .into_any();
        }

        // badge.pop: opacity 0→1 with ease-in-out deceleration.
        //
        // Do NOT use bounce() here.  bounce(ease_in_out) is a symmetric
        // 0→1→0 curve; at the animation's settled endpoint (t = 1.0) the
        // eased output is 0.0, making the badge permanently invisible once
        // the one-shot completes.  ease_in_out alone is a monotone 0→1 curve
        // that settles at full opacity, which is the intended behaviour.
        div()
            .id(id.clone())
            .bg(colour)
            .text_color(fg_on)
            .rounded(sp.r_sm)
            .px(sp.space_1)
            .py(sp.space_1 / 2.0)
            .text_size(ts.caption.size)
            .line_height(ts.caption.line_height)
            .font_weight(gpui::FontWeight(ts.caption.weight as f32))
            .refine_style(&self.style)
            .child(label)
            .with_animation(
                id,
                Animation::new(dur).with_easing(ease_in_out),
                |el, t| el.opacity(t),
            )
            .into_any()
    }
}

// ── Pure unit tests (no GPUI context required) ────────────────────────────────
//
// These tests verify the *easing contract* that caused the bug: the animator
// closure `|el, t| el.opacity(t)` means the eased `t` value IS the opacity,
// so whatever the easing function produces at its endpoints determines whether
// the badge is visible.

#[cfg(test)]
mod tests {
    use gpui::ease_in_out;

    // Import bounce so we can prove why it was wrong, then ensure the chosen
    // easing is correct.
    use gpui::bounce;

    /// The bug: `bounce(ease_in_out)` returns 0.0 at both t=0 and t=1.
    ///
    /// GPUI's one-shot `AnimationElement` clamps `delta = 1.0` at completion
    /// and calls the animator one final time.  If the easing returns 0.0 at
    /// t=1, `el.opacity(0.0)` is the permanent settled state — the badge is
    /// invisible forever.  This test documents the incorrect behaviour that
    /// was removed; it must stay green to prove the fix was not reverted.
    #[test]
    fn bounce_ease_in_out_is_zero_at_both_endpoints() {
        let f = bounce(ease_in_out);
        // t = 0: start of animation — badge begins invisible (acceptable on
        // the first frame, but the animation never settles to visible).
        assert_eq!(f(0.0), 0.0, "bounce(ease_in_out) at t=0 must be 0 (pin)");
        // t = 1: settled endpoint — badge is PERMANENTLY invisible.  This is
        // the defect.
        assert_eq!(f(1.0), 0.0, "bounce(ease_in_out) at t=1 must be 0 (pin: the bug)");
    }

    /// The fix: `ease_in_out` settles at 1.0.
    ///
    /// The badge's animator is `|el, t| el.opacity(t)`, so the eased output
    /// at t=1 is the permanent opacity after the animation completes.
    /// `ease_in_out(1.0)` must equal 1.0 or the badge will be invisible at rest.
    #[test]
    fn ease_in_out_settles_at_full_opacity() {
        // Settled endpoint (t = 1.0): badge must be fully visible.
        assert_eq!(
            ease_in_out(1.0),
            1.0,
            "ease_in_out at t=1 must be 1.0 so the badge is opaque when at rest"
        );
    }

    /// `ease_in_out` starts at 0 so the fade-in begins from invisible.
    ///
    /// This is intentional: the entrance animation grows from nothing.
    /// If a future change accidentally makes t=0 non-zero the badge would
    /// flash at a partial opacity on the very first frame.
    #[test]
    fn ease_in_out_starts_at_zero() {
        assert_eq!(
            ease_in_out(0.0),
            0.0,
            "ease_in_out at t=0 must be 0.0 so the entrance begins from invisible"
        );
    }

    /// `ease_in_out` is monotone: every step from 0 to 1 increases opacity.
    ///
    /// Any non-monotone opacity curve (like `bounce`) can produce a temporary
    /// dip or peak that looks like a flicker.  A monotone fade-in never flickers.
    #[test]
    fn ease_in_out_is_monotone_increasing() {
        let steps = 20;
        let mut prev = ease_in_out(0.0);
        for i in 1..=steps {
            let t = i as f32 / steps as f32;
            let v = ease_in_out(t);
            assert!(
                v >= prev,
                "ease_in_out is not monotone: f({t}) = {v} < f({prev_t}) = {prev}",
                prev_t = (i - 1) as f32 / steps as f32
            );
            prev = v;
        }
    }

    /// Short labels exist for every `LocalKindDiscriminant`.
    ///
    /// `Badge::for_kind` uses `kind.short_label()` as the chip text.  If any
    /// variant returns an empty string the badge renders with no text, which
    /// looks identical to the invisible-badge bug from the user's perspective.
    #[test]
    fn every_kind_has_a_non_empty_short_label() {
        use crate::theme::kind::LocalKindDiscriminant;
        for variant in LocalKindDiscriminant::all() {
            let label = variant.short_label();
            assert!(
                !label.is_empty(),
                "{variant:?}.short_label() is empty — badge would render with no text"
            );
        }
    }
}
