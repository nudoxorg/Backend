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

use gpui::{
    Animation, AnimationExt, App, ElementId, Hsla, IntoElement, ParentElement, RenderOnce,
    StyleRefinement, Styled, Window, bounce, div, ease_in_out, prelude::FluentBuilder as _,
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
        // fg_on is always dark text on the kind colours (matched luminance 0.55/0.62).
        // We pick from the theme's foreground-on-accent token as a reasonable proxy.
        let fg_on = cx.theme().accent_foreground;
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

        // badge.pop: opacity 0→1 with a single soft overshoot (bounce).
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
                Animation::new(dur)
                    .with_easing(bounce(ease_in_out)),
                |el, t| el.opacity(t),
            )
            .into_any()
    }
}
