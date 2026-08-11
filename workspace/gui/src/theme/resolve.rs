//! The role table — the one place a step becomes a role.
//!
//! # Why this is one function and not one per theme
//!
//! Before this module there were two functions, `light_theme()` and
//! `dark_theme()`, and each independently decided what `border_default` meant.
//! They could disagree, and they did: the dark theme gave `border_default` and
//! `bg_active` the *same* value (`l = 0.24`), so a selected row's edge vanished
//! into its own fill, while the light theme kept them two steps apart. Nothing
//! caught it, because "these two roles must differ" was not written down
//! anywhere — it was implied by two independent lists of numbers.
//!
//! With one table the question "what is `border_default`?" has one answer for
//! every theme that will ever exist: `neutral[Border]`. A theme can move where
//! `Border` *is*; it cannot move what `border_default` *means*. That is the
//! difference between a design system and a pair of colour lists.
//!
//! # Reading the table
//!
//! Each assignment below is `role = ramp[Step]`. The step names say what the
//! step is for (see [`Step`]), so an assignment that reads oddly — a border
//! taken from a text step, say — is visible as prose rather than as a number
//! nobody can check.

use gpui::{BoxShadow, Point, px};

use crate::theme::ext::NudoxThemeExt;
use crate::theme::palette::{Appearance, Palette, Ramp, Step};
use crate::theme::spec::ThemeSpec;
use crate::theme::tokens::{
    AlphaTokens, ColourRoles, ElevLevel, ElevTokens, KindColours, SpaceTokens, SyntaxColours,
    TrustStyle, TrustTokens, TypeScale,
};

/// Turn a theme's decisions into the tokens views consume.
///
/// This is the whole of the theme layer's logic. Everything above it is data;
/// everything below it is a view asking for a role by name.
pub fn resolve(spec: &ThemeSpec) -> NudoxThemeExt {
    let p = spec.palette();
    NudoxThemeExt {
        theme_key: spec.key.clone().into(),
        theme_name: spec.name.clone().into(),
        theme_blurb: spec.blurb.clone().into(),
        appearance: spec.appearance,
        space: SpaceTokens::STANDARD,
        type_scale: TypeScale::STANDARD,
        alpha: AlphaTokens::STANDARD,
        colours: roles(&p),
        trust: trust(&p),
        elev: elevation(&p),
        kind_colours: kinds(&p),
        syntax: syntax(&p),
        motion_scale: 1.0,
        palette: p,
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Colour roles
// ─────────────────────────────────────────────────────────────────────────────

fn roles(p: &Palette) -> ColourRoles {
    let n = &p.neutral;
    let a = &p.accent;
    ColourRoles {
        // ── Surfaces ─────────────────────────────────────────────────────────
        // The four elevation planes come from `Surfaces`, not from the neutral
        // ramp, because in a light appearance "further from the page" and "more
        // contrast with the page" point in opposite directions — a sidebar
        // recedes to grey while a popover advances to white. See `SurfaceSpec`.
        bg_sunken: p.surfaces.sunken,
        bg_base: p.surfaces.base,
        bg_raised: p.surfaces.raised,
        bg_overlay: p.surfaces.overlay,

        // Hover and active are *not* elevation — they are the same plane
        // reacting to the pointer, so they do ride the neutral ramp, where the
        // direction is unambiguous in both appearances.
        bg_hover: n.get(Step::ElementHover),
        bg_active: n.get(Step::ElementActive),

        // ── Text ─────────────────────────────────────────────────────────────
        fg_default: n.get(Step::TextHigh),
        fg_muted: n.get(Step::TextLow),
        // Placeholder and disabled text. `BorderStrong` rather than a text step
        // on purpose: this is the rank *below* legible-secondary, and the whole
        // point is that it does not read as something you are meant to finish
        // reading. It sits at the top of the non-text band by construction.
        fg_faint: n.get(Step::BorderStrong),

        // ── Interactive ──────────────────────────────────────────────────────
        accent: a.get(Step::Solid),
        accent_hover: a.get(Step::SolidHover),
        // Accent text — a link in a paragraph. `TextLow` of the accent ramp,
        // not `Solid`: a solid step is tuned to be a *fill*, and using a fill
        // colour as text is how links end up failing contrast on the page.
        accent_text: a.get(Step::TextLow),
        // The wash behind a selected row, a chosen chip, a drop target.
        accent_wash: a.get(Step::ElementBg),
        accent_wash_hover: a.get(Step::ElementHover),
        accent_fg_on: p.on_solid,

        // ── Borders ──────────────────────────────────────────────────────────
        border_subtle: n.get(Step::BorderSubtle),
        border_default: n.get(Step::Border),
        border_strong: n.get(Step::BorderStrong),
        ring: a.at(Step::Solid, AlphaTokens::STANDARD.veil),

        // ── Status ───────────────────────────────────────────────────────────
        ok: p.ok.get(Step::Solid),
        ok_fg: p.on_solid,
        warn: p.warn.get(Step::Solid),
        warn_fg: p.on_solid,
        danger: p.danger.get(Step::Solid),
        danger_fg: p.on_solid,
        info: p.info.get(Step::Solid),
        info_fg: p.on_solid,

        // ── Scrim ────────────────────────────────────────────────────────────
        // The dimming layer behind a modal. This used to be `gpui::black()`
        // written inline in `workspace/shell.rs` — the only raw colour
        // constructor left in any view, and the reason a light theme's scrim
        // was as heavy as a dark theme's when it should be lighter.
        //
        // Taken from the *darkest* end of the neutral ramp in both appearances
        // (`TextHigh` in light is near-black; in dark `AppBg` is), so the scrim
        // is always the theme's own darkest neutral rather than an absolute
        // black that belongs to no theme.
        scrim: match p.appearance {
            Appearance::Light => n.at(Step::TextHigh, AlphaTokens::STANDARD.scrim),
            Appearance::Dark => n.at(Step::AppBg, AlphaTokens::STANDARD.scrim_dark),
        },
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Trust chrome (LD-8)
// ─────────────────────────────────────────────────────────────────────────────

/// Glyph and label are *not* theme data.
///
/// A theme may recolour provenance; it may not rename it. "local" means the
/// same thing in every theme, and letting a theme file change the word would
/// make the vocabulary a property of the reader's colour preference. So the
/// non-colour half of `TrustStyle` lives here, in code, and only the ramp comes
/// from the spec.
fn trust_style(ramp: &Ramp, on_solid: gpui::Hsla, glyph: char, label: &'static str, hatched: bool) -> TrustStyle {
    TrustStyle {
        colour: ramp.get(Step::Solid),
        fg_on: on_solid,
        wash: ramp.get(Step::ElementBg),
        badge_glyph: glyph,
        badge_label: label,
        hatched,
    }
}

fn trust(p: &Palette) -> TrustTokens {
    TrustTokens {
        local: trust_style(&p.trust_local, p.on_solid, '⬢', "local", false),
        synced: trust_style(&p.trust_synced, p.on_solid, '⬢', "synced", false),
        remote: trust_style(&p.trust_remote, p.on_solid, '⬡', "remote", true),
        stale: trust_style(&p.trust_stale, p.on_solid, '⬡', "stale", false),
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Elevation
// ─────────────────────────────────────────────────────────────────────────────

/// Shadows in light, borders in dark — one function, branching on appearance.
///
/// # Why the branch is here and not in the data
///
/// It was in the data: `light_elev()` wrote real shadow alphas and a
/// transparent `dark_border`, `dark_elev()` wrote near-transparent shadows and
/// a real border, and every future theme would have had to remember which half
/// to fill in for its appearance. That is not a decision a theme makes — it is
/// a consequence of the appearance, because a dark shadow on a dark surface is
/// invisible and there is no theme for which that stops being true.
///
/// So it is derived. A theme states its appearance and gets the right
/// elevation strategy; it cannot state a wrong one.
fn elevation(p: &Palette) -> ElevTokens {
    let al = AlphaTokens::STANDARD;
    let dark = p.appearance.is_dark();

    // The shadow colour is the theme's own darkest neutral, not black: a warm
    // theme's shadows should be warm, or the whole page reads as two
    // temperatures stacked.
    let shade = |alpha: f32| {
        let mut c = p.neutral.get(if dark { Step::AppBg } else { Step::TextHigh });
        // In dark appearances shadows do almost nothing; keep them present but
        // negligible rather than removing the draw, so the geometry of a raised
        // surface does not change between themes.
        c.a = if dark { al.hairline } else { alpha };
        c
    };

    let level = |y: f32, blur: f32, alpha: f32, border_step: Step| ElevLevel {
        shadows: vec![BoxShadow {
            color: shade(alpha),
            offset: Point {
                x: px(0.0),
                y: px(y),
            },
            blur_radius: px(blur),
            spread_radius: px(0.0),
            inset: false,
        }],
        // In a light appearance the shadow carries the separation and the
        // border would double it, so the border is the subtle step. In a dark
        // appearance the border *is* the separation.
        border: if dark {
            p.neutral.get(border_step)
        } else {
            p.neutral.get(Step::BorderSubtle)
        },
    };

    ElevTokens {
        raised: level(1.0, 4.0, al.wash, Step::Border),
        overlay: level(4.0, 18.0, al.tint, Step::BorderStrong),
        toast: level(6.0, 26.0, al.veil, Step::BorderStrong),
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Kinds
// ─────────────────────────────────────────────────────────────────────────────

fn kinds(p: &Palette) -> KindColours {
    let k = &p.kinds;
    KindColours {
        module: k[0],
        record: k[1],
        field: k[2],
        function: k[3],
        alias: k[4],
        trait_: k[5],
        impl_: k[6],
        enum_: k[7],
        variant: k[8],
        const_: k[9],
        static_: k[10],
        reexport: k[11],
        param: k[12],
        kind_fg_on: p.on_solid,
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Syntax
// ─────────────────────────────────────────────────────────────────────────────

/// Syntax colours, derived rather than restated.
///
/// # The bug this shape removes
///
/// `dark_syntax()` opened with the comment "keyword → accent (indigo 243°,
/// dark-mode lightness 0.70)" and then wrote `hsla(243.0/360.0, 0.82, 0.70,
/// 1.0)` — a *copy* of `colours.accent`, correct only for as long as somebody
/// kept the comment true. The same file did it for `ty_name` (a copy of
/// `kinds.record`), `ident` (`fg_default`), `generic` (`kinds.field`),
/// `fn_name` (`kinds.function`), `punct` and `comment` (`fg_faint`),
/// `string_lit` (`ok`), `number_lit` and `boolean` (`warn`), `attr` and
/// `macro_` (`kinds.alias`): twelve values, none of them independent, all of
/// them retyped.
///
/// Here they are the expressions the comments described. `pub` in a signature
/// and `pub` in a code block are now the same colour because they are the same
/// expression, not because two literals agree today.
fn syntax(p: &Palette) -> SyntaxColours {
    let k = kinds(p);
    let n = &p.neutral;
    SyntaxColours {
        kw: p.accent.get(Step::TextLow),
        ty_name: k.record,
        ident: n.get(Step::TextHigh),
        generic: k.field,
        fn_name: k.function,
        punct: n.get(Step::BorderStrong),
        string_lit: p.ok.get(Step::Solid),
        number_lit: p.warn.get(Step::Solid),
        comment: n.get(Step::BorderStrong),
        attr: k.alias,
        macro_: k.alias,
        boolean: p.warn.get(Step::Solid),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::theme::spec::parse_bundled;

    /// The invariant the old two-function shape could not state: a border must
    /// be distinguishable from the fill it borders, in every theme.
    ///
    /// The dark theme shipped with `border_default` and `bg_active` at the same
    /// lightness, so a selected row had no edge. One table makes that a
    /// structural property rather than a coincidence of two hand-written lists.
    #[test]
    fn borders_are_distinguishable_from_the_fills_they_border() {
        for spec in parse_bundled().expect("bundled themes parse") {
            let t = resolve(&spec);
            let c = t.colours;
            for (fill_name, fill) in [
                ("bg_base", c.bg_base),
                ("bg_raised", c.bg_raised),
                ("bg_overlay", c.bg_overlay),
                ("bg_hover", c.bg_hover),
                ("bg_active", c.bg_active),
            ] {
                let delta = (c.border_default.l - fill.l).abs();
                assert!(
                    delta > 0.015,
                    "{}: border_default (l={:.3}) is indistinguishable from {fill_name} \
                     (l={:.3})",
                    spec.key,
                    c.border_default.l,
                    fill.l
                );
            }
        }
    }

    /// Text ranks must actually rank. `fg_default` outranks `fg_muted` outranks
    /// `fg_faint`, measured as distance from the page, in every theme and both
    /// appearances.
    #[test]
    fn foreground_ranks_are_ordered_in_every_theme() {
        for spec in parse_bundled().expect("bundled themes parse") {
            let t = resolve(&spec);
            let c = t.colours;
            let from_page = |x: gpui::Hsla| (x.l - c.bg_base.l).abs();
            assert!(
                from_page(c.fg_default) > from_page(c.fg_muted),
                "{}: fg_default must be further from the page than fg_muted",
                spec.key
            );
            assert!(
                from_page(c.fg_muted) > from_page(c.fg_faint),
                "{}: fg_muted must be further from the page than fg_faint",
                spec.key
            );
        }
    }

    /// Syntax colours are expressions over the palette, so they cannot drift
    /// from the roles their doc comments name.
    #[test]
    fn syntax_colours_equal_the_roles_they_claim_to_match() {
        for spec in parse_bundled().expect("bundled themes parse") {
            let t = resolve(&spec);
            assert_eq!(
                t.syntax.ty_name, t.kind_colours.record,
                "{}: syntax.ty_name must be the record kind colour",
                spec.key
            );
            assert_eq!(
                t.syntax.fn_name, t.kind_colours.function,
                "{}: syntax.fn_name must be the function kind colour",
                spec.key
            );
            assert_eq!(
                t.syntax.ident, t.colours.fg_default,
                "{}: syntax.ident must be the default foreground",
                spec.key
            );
            assert_eq!(
                t.syntax.comment, t.colours.fg_faint,
                "{}: syntax.comment must be the faint foreground",
                spec.key
            );
        }
    }

    /// Elevation strategy is a consequence of appearance, not a theme choice.
    #[test]
    fn dark_appearances_carry_their_separation_in_the_border() {
        for spec in parse_bundled().expect("bundled themes parse") {
            let t = resolve(&spec);
            let overlay_border = t.elev.overlay.border;
            let raised_border = t.elev.raised.border;
            if spec.appearance.is_dark() {
                assert!(
                    (overlay_border.l - raised_border.l).abs() > 0.02,
                    "{}: a dark theme must separate elevation levels by border strength",
                    spec.key
                );
            }
        }
    }
}
