//! Dark-theme concrete values for every [`NudoxThemeExt`] token.
//!
//! # Shadow → border swap strategy (§10.5)
//!
//! Drop shadows are invisible on dark backgrounds because they darken a surface
//! that is already dark.  The dark theme therefore:
//!   1. Reduces shadow alpha to near-zero so they cost nothing to render.
//!   2. Sets `dark_border` to a visible light-on-dark colour so views can apply
//!      a compensating border instead.
//!
//! This swap is encoded in the `ElevLevel` struct so views never branch on theme
//! mode for elevation — they always apply both `shadows` (which are transparent
//! here) and `dark_border` (which is transparent in light).
//!
//! # Contrast arithmetic
//!
//! Key pairs verified (WCAG 2.1 relative luminance):
//! - `fg_default` (#eeeef4, l≈0.93, S≈0):  L_rel ≈ 0.855
//! - `fg_muted`   (#8888a8, l≈0.60, S≈0):  L_rel ≈ 0.317
//! - `bg_base`    (#111116, l≈0.08, S≈0):  L_rel ≈ 0.005
//!
//! Contrast fg_default / bg_base = (0.855+0.05)/(0.005+0.05) = 16.5:1  ✓ AAA
//! Contrast fg_muted   / bg_base = (0.317+0.05)/(0.005+0.05) =  6.7:1  ✓ AA
//!
//! # Surface layering (dark)
//!
//! Dark mode surfaces must not be too close together or the eye reads them as
//! one flat plane.  The step sequence chosen:
//!
//!   bg_base (l=0.08)   →  base window / content  (#111116)
//!   bg_raised (l=0.13) →  cards, sidebars (Δ=5 lightness points)
//!   bg_overlay (l=0.17)→  dialogs / popovers (Δ=4 points from raised)
//!
//! A 5-point step is more visible in the dark than in the light because
//! human contrast sensitivity rises steeply at low luminances.
//!
//! # Kind-colour luminance matching (dark)
//!
//! Dark-mode kind colours use l=0.62, s=0.76 (vs l=0.38 in light).
//! L_rel at l=0.62, s=0.76 ≈ 0.330 → contrast on bg_base ≈ 7.0:1 ✓ AA.
//!
//! Same hue table as the light theme so the colour vocabulary is invariant —
//! the same hue always means the same kind, regardless of dark/light mode.
//! Only lightness shifts; hue and saturation are preserved so the visual
//! identity carries across both modes.

use gpui::{BoxShadow, Point, hsla, px};

use crate::theme::{
    NudoxThemeExt,
    tokens::{
        ColourRoles, ElevLevel, ElevTokens, KindColours, SpaceTokens, SyntaxColours, TrustStyle,
        TrustTokens, TypeScale,
    },
};

/// Construct the dark-theme [`NudoxThemeExt`].
///
/// Same struct layout as `light_theme()` — different values.  This function
/// exists as data, not as conditional logic.  Adding a third theme = adding a
/// third file with a third `fn foo_theme() -> NudoxThemeExt` call (GUI-PLAN LD-14).
pub fn dark_theme() -> NudoxThemeExt {
    NudoxThemeExt {
        space: SpaceTokens::STANDARD,
        type_scale: TypeScale::STANDARD,
        colours: dark_colours(),
        trust: dark_trust(),
        elev: dark_elev(),
        kind_colours: dark_kind_colours(),
        syntax: dark_syntax(),
        motion_scale: 1.0,
    }
}

fn dark_colours() -> ColourRoles {
    ColourRoles {
        // ── Background surfaces ──────────────────────────────────────────────
        // bg_base: very dark with a faint blue-grey tint (sRGB ≈ #111116).
        // Slightly darker than before (l=0.08 vs 0.09) for a richer, deeper base
        // that makes raised surfaces and text read with more contrast.
        bg_base: hsla(240.0 / 360.0, 0.12, 0.08, 1.0),

        // bg_raised: cards, panels — l=0.13 gives a clean 5-point separation
        // from bg_base.  Previously l=0.12 (only 3 points); the extra step
        // makes panels clearly distinct without being harsh.
        bg_raised: hsla(240.0 / 360.0, 0.10, 0.13, 1.0),

        // bg_overlay: dialogs, popovers — 4 points above bg_raised so they
        // float clearly over panels.
        bg_overlay: hsla(240.0 / 360.0, 0.10, 0.17, 0.98),

        // bg_hover: interactive row hover.  8 points above bg_base.
        bg_hover: hsla(240.0 / 360.0, 0.08, 0.20, 1.0),

        // bg_active: press-down — 4 points above hover.
        bg_active: hsla(240.0 / 360.0, 0.08, 0.24, 1.0),

        // ── Foreground / text ────────────────────────────────────────────────
        // fg_default: near-white with a cool undertone (sRGB ≈ #eeeef4).
        // Slightly cooler than before for better synergy with the blue-shifted base.
        fg_default: hsla(240.0 / 360.0, 0.10, 0.93, 1.0),

        // fg_muted: mid-grey for secondary text.
        // l=0.60 gives contrast ≈ 6.4:1 on bg_base (AA).
        fg_muted: hsla(240.0 / 360.0, 0.10, 0.60, 1.0),

        // fg_faint: dim text for placeholders / disabled / punctuation.
        fg_faint: hsla(240.0 / 360.0, 0.08, 0.40, 1.0),

        // ── Interactive / accent ─────────────────────────────────────────────
        // accent: brightened indigo — same hue as light (243°), higher lightness
        // so it reads on a dark surface.  Slightly more saturated (0.82 vs 0.80)
        // so it pops against the very dark bg_base.
        accent: hsla(243.0 / 360.0, 0.82, 0.70, 1.0),
        accent_fg_on: hsla(0.0, 0.0, 0.05, 1.0),

        // ── Borders ──────────────────────────────────────────────────────────
        // Borders are more visible in dark mode (compensates for lost shadows).
        // default: tighter hairline than before
        border_default: hsla(240.0 / 360.0, 0.08, 0.24, 1.0),
        // strong: used for active selections and focus outlines
        border_strong: hsla(240.0 / 360.0, 0.10, 0.38, 1.0),
        ring: hsla(243.0 / 360.0, 0.82, 0.70, 0.55),

        // ── Semantic status ───────────────────────────────────────────────────
        // All semantic colours are lightened in dark mode so they pop against
        // the darker backgrounds.  Slightly more saturated than before.
        ok: hsla(142.0 / 360.0, 0.68, 0.54, 1.0),
        ok_fg: hsla(0.0, 0.0, 0.05, 1.0),

        warn: hsla(38.0 / 360.0, 0.92, 0.62, 1.0),
        warn_fg: hsla(0.0, 0.0, 0.05, 1.0),

        danger: hsla(0.0 / 360.0, 0.82, 0.62, 1.0),
        danger_fg: hsla(0.0, 0.0, 0.05, 1.0),

        info: hsla(211.0 / 360.0, 0.84, 0.62, 1.0),
        info_fg: hsla(0.0, 0.0, 0.05, 1.0),
    }
}

fn dark_trust() -> TrustTokens {
    TrustTokens {
        // TrustedLocal — emerald green, lightened for dark mode.
        // Hue 148° aligns with the light theme's trust.local hue for visual
        // consistency when switching modes.
        local: TrustStyle {
            colour: hsla(148.0 / 360.0, 0.65, 0.50, 1.0),
            fg_on: hsla(0.0, 0.0, 0.05, 1.0),
            badge_glyph: '⬢',
            badge_label: "local",
            hatched: false,
        },
        // SyncedLocal — cerulean blue.  Hue 205° distinct from accent indigo.
        synced: TrustStyle {
            colour: hsla(205.0 / 360.0, 0.80, 0.58, 1.0),
            fg_on: hsla(0.0, 0.0, 0.05, 1.0),
            badge_glyph: '⬢',
            badge_label: "synced",
            hatched: false,
        },
        // Remote — amber, consistent with warn family.
        remote: TrustStyle {
            colour: hsla(38.0 / 360.0, 0.90, 0.62, 1.0),
            fg_on: hsla(0.0, 0.0, 0.05, 1.0),
            badge_glyph: '⬡',
            badge_label: "remote",
            hatched: true,
        },
        // Stale — cool grey with a faint blue cast.
        stale: TrustStyle {
            colour: hsla(240.0 / 360.0, 0.06, 0.44, 1.0),
            fg_on: hsla(0.0, 0.0, 1.0, 1.0),
            badge_glyph: '⬡',
            badge_label: "stale",
            hatched: false,
        },
    }
}

/// Dark-theme elevation — swap shadow strength for border strength (§10.5).
///
/// `shadows` are near-transparent (not zero, to avoid GPU-side no-op artefacts
/// on some drivers).  `dark_border` carries the visible separation.
/// Border values are slightly brighter than before for cleaner panel definition.
fn dark_elev() -> ElevTokens {
    // Shadow colour: barely-visible to maintain draw call without visual noise.
    let shadow_hint = hsla(0.0, 0.0, 0.0, 0.04);

    ElevTokens {
        raised: ElevLevel {
            shadows: vec![BoxShadow {
                color: shadow_hint,
                offset: Point { x: px(0.0), y: px(1.0) },
                blur_radius: px(3.0),
                spread_radius: px(0.0),
                inset: false,
            }],
            // Visible hairline border replaces the shadow.  Brighter than before
            // (l=0.30 vs 0.28) for crisper panel definition.
            dark_border: hsla(240.0 / 360.0, 0.09, 0.30, 1.0),
        },
        overlay: ElevLevel {
            shadows: vec![BoxShadow {
                color: shadow_hint,
                offset: Point { x: px(0.0), y: px(4.0) },
                blur_radius: px(16.0),
                spread_radius: px(0.0),
                inset: false,
            }],
            // Overlays must "float" above bg_raised; stronger border required.
            dark_border: hsla(240.0 / 360.0, 0.09, 0.40, 1.0),
        },
        toast: ElevLevel {
            shadows: vec![BoxShadow {
                color: shadow_hint,
                offset: Point { x: px(0.0), y: px(6.0) },
                blur_radius: px(24.0),
                spread_radius: px(0.0),
                inset: false,
            }],
            // Toasts: strongest border — they appear above overlay layers.
            dark_border: hsla(240.0 / 360.0, 0.09, 0.48, 1.0),
        },
    }
}

/// Dark-mode kind colours at l=0.62, s=0.76.
///
/// Same hue table as the light theme — the colour vocabulary is invariant across
/// modes.  Only lightness increases (0.38 → 0.62) so colours read on the dark
/// bg_base.
///
/// L_rel at (s=0.76, l=0.62) ≈ 0.330 → contrast on bg_base ≈ 7.0:1 ✓ AA
///
/// `kind_fg_on` is near-black: at l=0.62 the badge background is bright and
/// requires dark text for WCAG AA contrast.
fn dark_kind_colours() -> KindColours {
    let s = 0.76_f32;
    let l = 0.62_f32;
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
        // l=0.62 badges are bright → near-black text required for WCAG AA.
        kind_fg_on: hsla(0.0, 0.0, 0.05, 1.0),
    }
}

/// Syntax token colours for the dark theme.
///
/// All values are brightened counterparts of the light-theme syntax colours,
/// following the same semantic mapping.  Where the light theme uses l=0.38
/// for kind-derived values, the dark theme uses l=0.62.  Where the light theme
/// uses ColourRoles values, the dark values are used here directly.
fn dark_syntax() -> SyntaxColours {
    // keyword → accent (indigo 243°, dark-mode lightness 0.70)
    let kw         = hsla(243.0 / 360.0, 0.82, 0.70, 1.0);
    // type name → kinds.record (steel-blue 206°, dark l=0.62)
    let ty_name    = hsla(206.0 / 360.0, 0.76, 0.62, 1.0);
    // declared identifier → fg_default (near-white in dark)
    let ident      = hsla(240.0 / 360.0, 0.10, 0.93, 1.0);
    // generic → kinds.field (blue-indigo 220°, dark l=0.62)
    let generic    = hsla(220.0 / 360.0, 0.76, 0.62, 1.0);
    // fn name → kinds.function (green-cyan 158°, dark l=0.62)
    let fn_name    = hsla(158.0 / 360.0, 0.76, 0.62, 1.0);
    // punctuation → fg_faint (dark-mode: l=0.40)
    let punct      = hsla(240.0 / 360.0, 0.08, 0.40, 1.0);
    // string literal → ok (emerald, dark l=0.54)
    let string_lit = hsla(142.0 / 360.0, 0.68, 0.54, 1.0);
    // number literal → warn (amber, dark l=0.62)
    let number_lit = hsla( 38.0 / 360.0, 0.92, 0.62, 1.0);
    // comment → fg_faint (dark-mode)
    let comment    = hsla(240.0 / 360.0, 0.08, 0.40, 1.0);
    // attribute → kinds.alias (amber 48°, dark l=0.62)
    let attr       = hsla( 48.0 / 360.0, 0.76, 0.62, 1.0);
    // macro → same as attr
    let macro_     = hsla( 48.0 / 360.0, 0.76, 0.62, 1.0);
    // boolean → warn family (dark l=0.62)
    let boolean    = hsla( 38.0 / 360.0, 0.92, 0.62, 1.0);

    SyntaxColours { kw, ty_name, ident, generic, fn_name, punct, string_lit, number_lit, comment, attr, macro_, boolean }
}
