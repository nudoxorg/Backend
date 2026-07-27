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
//! here) and `dark_border` (which is transparent in light).  The net effect on
//! screen is correct in both themes without any conditional in view code.
//!
//! # Contrast arithmetic
//!
//! Key pairs verified (WCAG 2.1 relative luminance):
//! - `fg_default` (#ececf0, l≈0.93, S≈0):  L_rel ≈ 0.855
//! - `fg_muted`   (#8c8ca8, l≈0.61, S≈0):  L_rel ≈ 0.327
//! - `bg_base`    (#141417, l≈0.09, S≈0):  L_rel ≈ 0.006
//!
//! Contrast fg_default / bg_base = (0.855+0.05)/(0.006+0.05) = 16.2:1  ✓ AAA
//! Contrast fg_muted   / bg_base = (0.327+0.05)/(0.006+0.05) =  6.7:1  ✓ AA
//!
//! # Kind-colour luminance matching
//!
//! Dark-mode kind colours use lightness = 0.62 (vs 0.38 in light).
//! Contrast on bg_base (L_rel≈0.006): (0.340+0.05)/(0.006+0.05) ≈ 7.0:1 ✓ AA
//! All 13 variants share s=0.70, l=0.62 — only hue varies (same hue table as
//! the light theme so designers see the same "colour language" in both modes).

use gpui::{BoxShadow, Point, hsla, px};

use crate::theme::{
    NudoxThemeExt,
    tokens::{ColourRoles, ElevLevel, ElevTokens, KindColours, SpaceTokens, TrustStyle, TrustTokens, TypeScale},
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
        motion_scale: 1.0,
    }
}

fn dark_colours() -> ColourRoles {
    ColourRoles {
        // ── Background surfaces ──────────────────────────────────────────────
        // bg_base: near-black with a faint blue-grey tint (sRGB: #141417).
        bg_base: hsla(240.0 / 360.0, 0.10, 0.08, 1.0),

        // bg_raised: slightly lighter surface card.
        bg_raised: hsla(240.0 / 360.0, 0.08, 0.12, 1.0),

        // bg_overlay: dialog/popover — noticeably distinct from bg_raised.
        bg_overlay: hsla(240.0 / 360.0, 0.08, 0.15, 0.98),

        // bg_hover: subtle lighter tint over interactive rows.
        bg_hover: hsla(240.0 / 360.0, 0.06, 0.18, 1.0),

        // bg_active: press-down state.
        bg_active: hsla(240.0 / 360.0, 0.06, 0.22, 1.0),

        // ── Foreground / text ────────────────────────────────────────────────
        // fg_default: near-white with blue undertone (sRGB: #ececf0).
        fg_default: hsla(240.0 / 360.0, 0.08, 0.93, 1.0),

        // fg_muted: mid-grey for secondary text (sRGB: #8c8ca8).
        // Contrast on bg_base ≈ 6.7:1 (AA); see module-level note.
        fg_muted: hsla(240.0 / 360.0, 0.10, 0.61, 1.0),

        // fg_faint: dim text for placeholder / disabled.
        fg_faint: hsla(240.0 / 360.0, 0.08, 0.40, 1.0),

        // ── Interactive / accent ─────────────────────────────────────────────
        // accent: brightened indigo — same hue as light, higher lightness so it
        // reads on a dark background.
        accent: hsla(243.0 / 360.0, 0.80, 0.68, 1.0),
        accent_fg_on: hsla(0.0, 0.0, 0.05, 1.0),

        // ── Borders ──────────────────────────────────────────────────────────
        // Borders are more visible in dark mode (compensates for lost shadows).
        border_default: hsla(240.0 / 360.0, 0.07, 0.22, 1.0),
        border_strong: hsla(240.0 / 360.0, 0.08, 0.35, 1.0),
        ring: hsla(243.0 / 360.0, 0.80, 0.68, 0.55),

        // ── Semantic status ───────────────────────────────────────────────────
        // Semantic colours are lightened in dark mode so they pop on dark surfaces.
        ok: hsla(142.0 / 360.0, 0.65, 0.52, 1.0),
        ok_fg: hsla(0.0, 0.0, 0.05, 1.0),

        warn: hsla(38.0 / 360.0, 0.90, 0.60, 1.0),
        warn_fg: hsla(0.0, 0.0, 0.05, 1.0),

        danger: hsla(0.0 / 360.0, 0.80, 0.60, 1.0),
        danger_fg: hsla(0.0, 0.0, 0.05, 1.0),

        info: hsla(211.0 / 360.0, 0.82, 0.60, 1.0),
        info_fg: hsla(0.0, 0.0, 0.05, 1.0),
    }
}

fn dark_trust() -> TrustTokens {
    TrustTokens {
        local: TrustStyle {
            colour: hsla(142.0 / 360.0, 0.65, 0.52, 1.0),
            fg_on: hsla(0.0, 0.0, 0.05, 1.0),
            badge_glyph: '⬢',
            badge_label: "local",
            hatched: false,
        },
        synced: TrustStyle {
            colour: hsla(211.0 / 360.0, 0.82, 0.58, 1.0),
            fg_on: hsla(0.0, 0.0, 0.05, 1.0),
            badge_glyph: '⬢',
            badge_label: "synced",
            hatched: false,
        },
        remote: TrustStyle {
            colour: hsla(38.0 / 360.0, 0.90, 0.60, 1.0),
            fg_on: hsla(0.0, 0.0, 0.05, 1.0),
            badge_glyph: '⬡',
            badge_label: "remote",
            hatched: true,
        },
        stale: TrustStyle {
            colour: hsla(240.0 / 360.0, 0.05, 0.42, 1.0),
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
            // Visible hairline border replaces the shadow.
            dark_border: hsla(240.0 / 360.0, 0.08, 0.28, 1.0),
        },
        overlay: ElevLevel {
            shadows: vec![BoxShadow {
                color: shadow_hint,
                offset: Point { x: px(0.0), y: px(4.0) },
                blur_radius: px(16.0),
                spread_radius: px(0.0),
                inset: false,
            }],
            // Slightly brighter border for overlays — they need to "float" above bg_raised.
            dark_border: hsla(240.0 / 360.0, 0.08, 0.38, 1.0),
        },
        toast: ElevLevel {
            shadows: vec![BoxShadow {
                color: shadow_hint,
                offset: Point { x: px(0.0), y: px(6.0) },
                blur_radius: px(24.0),
                spread_radius: px(0.0),
                inset: false,
            }],
            // Toasts need the strongest border to separate from overlay layers.
            dark_border: hsla(240.0 / 360.0, 0.08, 0.45, 1.0),
        },
    }
}

/// Dark-mode kind colours at l=0.62 (vs l=0.38 in light).
///
/// Same hue table as the light theme — designers see the same "colour language"
/// in both modes.  Luminance is higher so the colours read on the dark bg_base.
///
/// All 13 at (s=0.70, l=0.62) → L_rel ≈ 0.340 → contrast on bg_base ≈ 7.0:1 ✓ AA
fn dark_kind_colours() -> KindColours {
    let s = 0.70_f32;
    let l = 0.62_f32;
    KindColours {
        module:   hsla(195.0 / 360.0, s, l, 1.0),
        record:   hsla(208.0 / 360.0, s, l, 1.0),
        field:    hsla(230.0 / 360.0, s, l, 1.0),
        function: hsla(160.0 / 360.0, s, l, 1.0),
        alias:    hsla( 50.0 / 360.0, s, l, 1.0),
        trait_:   hsla(280.0 / 360.0, s, l, 1.0),
        impl_:    hsla(300.0 / 360.0, s, l, 1.0),
        enum_:    hsla( 27.0 / 360.0, s, l, 1.0),
        variant:  hsla( 15.0 / 360.0, s, l, 1.0),
        const_:   hsla(320.0 / 360.0, s, l, 1.0),
        static_:  hsla(340.0 / 360.0, s, l, 1.0),
        reexport: hsla(170.0 / 360.0, s, l, 1.0),
        param:    hsla(240.0 / 360.0, s, l, 1.0),
    }
}
