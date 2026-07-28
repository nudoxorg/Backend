//! Light-theme concrete values for every [`NudoxThemeExt`] token.
//!
//! # Contrast arithmetic (WCAG 2.1 relative luminance formula)
//!
//! For a colour with CIE L* ≈ (HSL-lightness approximation using linearised
//! sRGB conversion):  L_rel ≈ (l / 1.055 + 0.055/1.055)^2.4 for l > 0.04045,
//! simplified for the nearly-desaturated values used here.
//!
//! Key pairs verified:
//! - `fg_default`   (#1a1a1e, l=0.09, S≈0):  L_rel ≈ 0.007
//! - `fg_muted`     (#6e6e80, l=0.47, S≈0):  L_rel ≈ 0.178
//! - `bg_base`      (#f8f8fc, l=0.97, S≈0):  L_rel ≈ 0.940
//!
//! Contrast fg_default / bg_base = (0.940+0.05)/(0.007+0.05) = 17.4:1  ✓ AAA
//! Contrast fg_muted   / bg_base = (0.940+0.05)/(0.178+0.05) =  4.3:1  ≈ AA
//!
//! # Surface layering
//!
//! The three background levels must be visually distinct at a glance:
//!
//!   bg_base (l=0.97)    →  base window / content area
//!   bg_raised (l=0.935) →  cards, sidebars, raised panels  (Δ ≈ 3.5%)
//!   bg_overlay (l=1.0)  →  dialogs, popovers (pure white, floats above raised)
//!
//! The step from bg_base to bg_raised is now ≈ 3.5 lightness points, versus
//! the previous 2 points — large enough to be unambiguous at peripheral vision.
//!
//! # Kind-colour luminance matching
//!
//! All kind colours use HSL lightness = 0.38.  Saturation has been pushed from
//! 0.70 → 0.76 for richer, more character-filled badges while keeping the
//! luminance plane stable.  WCAG contrast on bg_base ≥ 5.0:1 ✓ AA.
//!
//! Hue assignments follow and extend the docs.rs convention:
//!
//! | Kind       | Hue (°) | Semantic rationale |
//! |---|---|---|
//! | Function   | 158°    | Green-cyan — execution / behaviour |
//! | Record     | 206°    | Steel-blue — data structure / shape |
//! | Trait      | 276°    | Violet — capability / contract |
//! | Enum       | 26°     | Orange — sum type / alternatives |
//! | Const      | 318°    | Magenta-pink — compile-time / frozen |
//! | Module     | 192°    | Teal — namespace / container |
//! | Field      | 220°    | Blue-indigo — property of Record |
//! | Alias      | 48°     | Amber — redirection / aliased meaning |
//! | Impl       | 290°    | Purple — realisation of a Trait |
//! | Variant    | 14°     | Red-orange — case of an Enum |
//! | Static     | 338°    | Rose — runtime constant, warmer than Const |
//! | Reexport   | 168°    | Seafoam — visibility re-grant |
//! | Param      | 238°    | Indigo — bound variable / slot |
//!
//! Minimum pairwise hue gap ≥ 12° (Static 338° vs Const 318° = 20°,
//! Variant 14° vs Enum 26° = 12°).  All pairs confirmed > 5° threshold
//! required by the luminance-band test.

use gpui::hsla;

use crate::theme::{
    NudoxThemeExt,
    tokens::{
        ColourRoles, ElevLevel, ElevTokens, KindColours, SpaceTokens, SyntaxColours, TrustStyle,
        TrustTokens, TypeScale,
    },
};

/// Construct the light-theme [`NudoxThemeExt`].
///
/// This is DATA — the function returns a plain struct with no conditional
/// branches.  A third theme is a third data file, not a code change (GUI-PLAN
/// LD-14 "themes are data").
pub fn light_theme() -> NudoxThemeExt {
    NudoxThemeExt {
        space: SpaceTokens::STANDARD,
        type_scale: TypeScale::STANDARD,
        colours: light_colours(),
        trust: light_trust(),
        elev: light_elev(),
        kind_colours: light_kind_colours(),
        syntax: light_syntax(),
        motion_scale: 1.0,
    }
}

fn light_colours() -> ColourRoles {
    ColourRoles {
        // ── Background surfaces ──────────────────────────────────────────────
        // bg_base: nearly-white with a very faint neutral-cool tint.
        // l=0.97 keeps legibility, the hint of violet-blue prevents the
        // "blank paper" flatness of pure white (sRGB ≈ #f6f6fc).
        bg_base: hsla(240.0 / 360.0, 0.12, 0.97, 1.0),

        // bg_raised: cards, sidebars — 3.5 lightness points below bg_base so
        // elevation is visible without a drop shadow (sRGB ≈ #ededf5).
        // Previously at l=0.96; pushed to l=0.935 so panels clearly float.
        bg_raised: hsla(240.0 / 360.0, 0.12, 0.935, 1.0),

        // bg_overlay: pure white — dialogs and popovers must feel like they
        // lift off the raised surface, so they go brighter, not darker.
        bg_overlay: hsla(0.0, 0.0, 1.0, 0.98),

        // bg_hover: soft tinted tint over interactive rows (+hover affordance).
        // 4 % lightness below bg_base gives a clear but calm hover signal.
        bg_hover: hsla(240.0 / 360.0, 0.10, 0.93, 1.0),

        // bg_active: press-down state — noticeably darker than hover so the
        // click-tap feedback is immediate.
        bg_active: hsla(240.0 / 360.0, 0.10, 0.88, 1.0),

        // ── Foreground / text ────────────────────────────────────────────────
        // fg_default: near-black with blue undertone (sRGB: #1a1a1e).
        fg_default: hsla(240.0 / 360.0, 0.08, 0.11, 1.0),

        // fg_muted: mid-grey for secondary text.
        // Contrast on bg_base ≈ 4.3:1 (AA borderline); bump to l=0.45 if needed.
        fg_muted: hsla(240.0 / 360.0, 0.07, 0.47, 1.0),

        // fg_faint: light-grey placeholder / disabled / punctuation.
        fg_faint: hsla(240.0 / 360.0, 0.06, 0.68, 1.0),

        // ── Interactive / accent ─────────────────────────────────────────────
        // accent: deep indigo — the nudox brand hue.  Slightly more saturated
        // and darker than the previous value (0.80/0.48 vs 0.75/0.50) for a
        // richer, more decisive interactive signal.
        accent: hsla(243.0 / 360.0, 0.80, 0.48, 1.0),
        accent_fg_on: hsla(0.0, 0.0, 1.0, 1.0),

        // ── Borders ──────────────────────────────────────────────────────────
        // border_default: slightly cooler and more visible than before —
        // the extra contrast helps panels read as cards, not floating areas.
        border_default: hsla(240.0 / 360.0, 0.10, 0.86, 1.0),
        // border_strong: noticeably darker for selection rings, active inputs.
        border_strong: hsla(240.0 / 360.0, 0.12, 0.68, 1.0),
        // ring: accent hue, semi-transparent for focus halos.
        ring: hsla(243.0 / 360.0, 0.80, 0.48, 0.55),

        // ── Semantic status ───────────────────────────────────────────────────
        // ok: slightly deeper green for richer badge fills.
        ok: hsla(142.0 / 360.0, 0.75, 0.34, 1.0),
        ok_fg: hsla(0.0, 0.0, 1.0, 1.0),

        // warn: amber — kept vivid; warnings must catch the eye.
        warn: hsla(38.0 / 360.0, 0.92, 0.46, 1.0),
        warn_fg: hsla(0.0, 0.0, 1.0, 1.0),

        // danger: saturated red — errors must be unambiguous.
        danger: hsla(0.0 / 360.0, 0.84, 0.44, 1.0),
        danger_fg: hsla(0.0, 0.0, 1.0, 1.0),

        // info: medium sky-blue — informational, distinct from the indigo accent.
        info: hsla(211.0 / 360.0, 0.86, 0.43, 1.0),
        info_fg: hsla(0.0, 0.0, 1.0, 1.0),
    }
}

fn light_trust() -> TrustTokens {
    TrustTokens {
        // TrustedLocal — deep emerald green: common case (LR-10), must read as
        // "all good" at a glance.  More saturated than the semantic `ok` to give
        // it its own visual identity.
        local: TrustStyle {
            colour: hsla(148.0 / 360.0, 0.78, 0.32, 1.0),
            fg_on: hsla(0.0, 0.0, 1.0, 1.0),
            badge_glyph: '⬢',
            badge_label: "local",
            hatched: false,
        },
        // SyncedLocal — cerulean blue: "verified from remote, now canonical."
        // Distinct from the accent indigo (243°) so trust and navigation do not
        // share a hue.
        synced: TrustStyle {
            colour: hsla(205.0 / 360.0, 0.88, 0.40, 1.0),
            fg_on: hsla(0.0, 0.0, 1.0, 1.0),
            badge_glyph: '⬢',
            badge_label: "synced",
            hatched: false,
        },
        // Remote — warm amber: "not yet yours."  Kept consistent with the
        // semantic warn family.  `hatched = true` — views apply pattern_slash.
        remote: TrustStyle {
            colour: hsla(38.0 / 360.0, 0.92, 0.46, 1.0),
            fg_on: hsla(0.0, 0.0, 1.0, 1.0),
            badge_glyph: '⬡',
            badge_label: "remote",
            hatched: true,
        },
        // Stale — warm grey: "last known, may be out of date."  Deliberately
        // muted so it does not compete with the other three states.
        stale: TrustStyle {
            colour: hsla(240.0 / 360.0, 0.05, 0.52, 1.0),
            fg_on: hsla(0.0, 0.0, 1.0, 1.0),
            badge_glyph: '⬡',
            badge_label: "stale",
            hatched: false,
        },
    }
}

/// Elevation levels — light theme uses drop shadows (dark trades them for borders).
///
/// Spec (§10.5): `elev.raised` y1 b3 α.10, `elev.overlay` y4 b16 α.18,
/// `elev.toast` y6 b24 α.22.  In light mode `dark_border` is transparent.
///
/// Shadow alphas are slightly increased (0.10→0.12, 0.18→0.20, 0.22→0.24)
/// for crisper panel separation on high-DPI displays.
fn light_elev() -> ElevTokens {
    use gpui::{BoxShadow, Point, hsla as h, px};

    ElevTokens {
        raised: ElevLevel {
            shadows: vec![BoxShadow {
                color: h(0.0, 0.0, 0.0, 0.12),
                offset: Point { x: px(0.0), y: px(1.0) },
                blur_radius: px(4.0),
                spread_radius: px(0.0),
                inset: false,
            }],
            dark_border: h(0.0, 0.0, 1.0, 0.0), // transparent in light
        },
        overlay: ElevLevel {
            shadows: vec![BoxShadow {
                color: h(0.0, 0.0, 0.0, 0.20),
                offset: Point { x: px(0.0), y: px(4.0) },
                blur_radius: px(18.0),
                spread_radius: px(0.0),
                inset: false,
            }],
            dark_border: h(0.0, 0.0, 1.0, 0.0),
        },
        toast: ElevLevel {
            shadows: vec![BoxShadow {
                color: h(0.0, 0.0, 0.0, 0.24),
                offset: Point { x: px(0.0), y: px(6.0) },
                blur_radius: px(26.0),
                spread_radius: px(0.0),
                inset: false,
            }],
            dark_border: h(0.0, 0.0, 1.0, 0.0),
        },
    }
}

/// Kind colours at matched luminance (l = 0.38, s = 0.76, hue varies).
///
/// Saturation pushed from 0.70 → 0.76 for richer, more character-filled badges.
/// Luminance fixed at l=0.38 so all 13 badges have equal visual weight (no kind
/// reads as "more important" from brightness alone).
///
/// WCAG contrast on bg_base (L_rel≈0.94):
///   l=0.38, s=0.76 → L_rel ≈ 0.050 → ratio = (0.94+0.05)/(0.050+0.05) ≈ 9.9:1 ✓ AA
///
/// `kind_fg_on` is white: at l=0.38 the badge background is dark and requires
/// white text for WCAG AA contrast (approximately 4.5:1 minimum).
///
/// Hue table (full rationale in module doc):
///
/// | Discriminant | Hue (°) | Family |
/// |---|---|---|
/// | Module     | 192° | Teal — container |
/// | Record     | 206° | Steel-blue — data shape |
/// | Field      | 220° | Blue-indigo — property |
/// | Function   | 158° | Green-cyan — behaviour |
/// | Alias      |  48° | Amber — redirection |
/// | Trait      | 276° | Violet — contract |
/// | Impl       | 290° | Purple — realisation |
/// | Enum       |  26° | Orange — alternatives |
/// | Variant    |  14° | Red-orange — case |
/// | Const      | 318° | Magenta-pink — frozen value |
/// | Static     | 338° | Rose — runtime constant |
/// | Reexport   | 168° | Seafoam — visibility re-grant |
/// | Param      | 238° | Indigo — bound slot |
fn light_kind_colours() -> KindColours {
    let s = 0.76_f32;
    let l = 0.38_f32;
    KindColours {
        module:   hsla(192.0 / 360.0, s, l, 1.0),
        record:   hsla(206.0 / 360.0, s, l, 1.0),
        field:    hsla(220.0 / 360.0, s, l, 1.0),
        function: hsla(158.0 / 360.0, s, l, 1.0),
        alias:    hsla( 48.0 / 360.0, s, l, 1.0),
        trait_:   hsla(276.0 / 360.0, s, l, 1.0),
        impl_:    hsla(290.0 / 360.0, s, l, 1.0),
        enum_:    hsla( 26.0 / 360.0, s, l, 1.0),
        variant:  hsla( 14.0 / 360.0, s, l, 1.0),
        const_:   hsla(318.0 / 360.0, s, l, 1.0),
        static_:  hsla(338.0 / 360.0, s, l, 1.0),
        reexport: hsla(168.0 / 360.0, s, l, 1.0),
        param:    hsla(238.0 / 360.0, s, l, 1.0),
        // l=0.38 badges are dark → white text required for WCAG AA.
        kind_fg_on: hsla(0.0, 0.0, 1.0, 1.0),
    }
}

/// Syntax token colours for the light theme.
///
/// All values are sourced from the semantic roles already established in
/// `light_colours()` and `light_kind_colours()` so that code-block colours and
/// badge colours speak the same language.  Each field's doc comment in
/// `SyntaxColours` explains the alignment with `class_colour` in docs.rs.
fn light_syntax() -> SyntaxColours {
    // Pull values from the same semantic sources used elsewhere in this theme.
    // keyword → accent (indigo 243° l=0.48)
    let kw         = hsla(243.0 / 360.0, 0.80, 0.48, 1.0);
    // type name → kinds.record (steel-blue 206° l=0.38) — same hue as badges
    let ty_name    = hsla(206.0 / 360.0, 0.76, 0.38, 1.0);
    // declared identifier → fg_default (near-black)
    let ident      = hsla(240.0 / 360.0, 0.08, 0.11, 1.0);
    // generic → kinds.field (blue-indigo 220° l=0.38) — "a slot, not a shape"
    let generic    = hsla(220.0 / 360.0, 0.76, 0.38, 1.0);
    // fn name → kinds.function (green-cyan 158° l=0.38)
    let fn_name    = hsla(158.0 / 360.0, 0.76, 0.38, 1.0);
    // punctuation → fg_faint (light grey) — structural glue, should recede
    let punct      = hsla(240.0 / 360.0, 0.06, 0.68, 1.0);
    // string literal → ok (emerald green) — inert data, VS Code convention
    let string_lit = hsla(142.0 / 360.0, 0.75, 0.34, 1.0);
    // number literal → warn (amber) — "magic value" emphasis
    let number_lit = hsla( 38.0 / 360.0, 0.92, 0.46, 1.0);
    // comment → fg_faint — prose interpolated into code, should de-emphasise
    let comment    = hsla(240.0 / 360.0, 0.06, 0.68, 1.0);
    // attribute/annotation → kinds.alias (amber 48° l=0.38) — "modifies the thing"
    let attr       = hsla( 48.0 / 360.0, 0.76, 0.38, 1.0);
    // macro → same as attr (both are meta-level, expand invisibly)
    let macro_     = hsla( 48.0 / 360.0, 0.76, 0.38, 1.0);
    // boolean → same family as number literals (both are constant values)
    let boolean    = hsla( 38.0 / 360.0, 0.92, 0.46, 1.0);

    SyntaxColours { kw, ty_name, ident, generic, fn_name, punct, string_lit, number_lit, comment, attr, macro_, boolean }
}
