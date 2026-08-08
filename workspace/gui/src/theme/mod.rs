//! The lindsey design-system / theme layer (GUI-PLAN §10, LD-14).
//!
//! # Architecture
//!
//! ```text
//! ┌─ workspace/gui/src/theme/ ──────────────────────────────────────────────┐
//! │                                                                          │
//! │  tokens.rs   — every §10 token as strongly-typed Rust values            │
//! │                SpaceTokens, TypeScale, ColourRoles, TrustTokens,        │
//! │                ElevTokens, KindColours                                  │
//! │                                                                          │
//! │  kind.rs     — local mirror of KindDiscriminant (13 variants)           │
//! │                + colour(palette) mapping function                       │
//! │                                                                          │
//! │  ext.rs      — NudoxThemeExt GPUI Global + ThemeExtAccessor trait       │
//! │                + Provenance mirror + for_provenance() (LD-8)            │
//! │                                                                          │
//! │  themes/                                                                 │
//! │    light.rs  — light-theme data (ColourRoles + TrustTokens + Elev +     │
//! │                KindColours all filled)                                  │
//! │    dark.rs   — dark-theme data (same struct, different values)          │
//! │                                                                          │
//! └──────────────────────────────────────────────────────────────────────────┘
//! ```
//!
//! # Layering over gpui-component (LD-14)
//!
//! gpui-component's `Theme` global (registered via `gpui_component::theme::init`)
//! provides: font families, scrollbar colours, primary/secondary/danger semantic
//! colours, list/table zebra colours, radius, shadow bool, etc.
//!
//! `NudoxThemeExt` sits *beside* it (a second GPUI global) and adds the tokens
//! §10 requires that gpui-component lacks: trust chrome, elevation box-shadows,
//! per-kind colours, motion scale, and the nudox semantic role vocabulary
//! (`ColourRoles`).
//!
//! Views that need both call `cx.theme()` **and** `cx.theme_ext()`.  They never
//! reach into raw HSL values — every value a view could need has a name here.
//!
//! # The scales — read this before adding a number to a view
//!
//! Consistency here is not achieved by everyone being careful; it is achieved
//! by there being nowhere else to get a number from. Every value below has
//! exactly one definition, and a view that needs something not on a scale is
//! reporting a gap in the scale, not licence to write a literal.
//!
//! **Space** — one 4 px grid, `SpaceTokens::STANDARD`:
//! `space_1`..`space_8` = 4, 8, 12, 16, 20, 24, 32, 40 px.
//!
//! The reader has **one horizontal gutter: `space_4`**. Documentation
//! sections, disclosure headers, impl rows, the blanket subheading, the
//! version strip and the error bar all start at that left edge. They used to
//! use `space_4`, `space_3` and `space_2` respectively, which put three left
//! edges in one column. Narrow chrome (the outline rail) keeps its own tighter
//! `space_2` gutter — a 200 px rail and a 1150 px column should not share a
//! gutter — but it is internally consistent.
//!
//! **Radius** — `r_sm` 4 (chips, badges), `r_md` 6 (buttons, inputs, rows),
//! `r_lg` 10 (cards, panels), `r_xl` 14 (overlays, dialogs).
//!
//! **Type** — `TypeScale::STANDARD`, seven tokens, size/leading/weight:
//!
//! | token     | size | leading | weight | used for |
//! |-----------|------|---------|--------|----------|
//! | `display` | 20   | 28      | 600    | top-level doc headings |
//! | `title`   | 15   | 22      | 600    | panel headers, sub-headings |
//! | `prose`   | 15   | 24      | 400    | **reading text** |
//! | `ui`      | 13   | 20      | 400    | chrome, labels, buttons |
//! | `mono`    | 12.5 | 19      | 400    | code, signatures, paths |
//! | `dense`   | 12   | 16      | 400    | table rows, logs |
//! | `caption` | 11   | 16      | 500    | overlines, counts, hints |
//!
//! The split that matters is `prose` vs `ui`. Chrome type is tuned to be
//! compact and dismissable; reading type is tuned to be followed for minutes.
//! They want opposite things from leading, and while they shared one token the
//! documentation silently got the chrome answer — which is what "the prose
//! reads flat" meant.
//!
//! **Foreground rank in prose** — body is `fg_default`. Inline code
//! differentiates by *surface* (`bg_hover` chip), links by *hue* (`accent`)
//! plus an underline, emphasis by weight or slant. Nothing in a paragraph is
//! brighter than the sentence containing it. Before this, body was `fg_muted`
//! and inline code was `fg_default`, so a type name mentioned in passing
//! outranked the prose explaining it.
//!
//! **Rows** — one definition, [`NudoxThemeExt::row_height`]: a list row is one
//! line of its own type plus `space_2`. Never retype the expression; the two
//! places that need it per table (the outer extent and the inner item) must
//! agree or `uniform_list` clips.
//!
//! **Motion** — every duration is a named constant in `crate::motion::tokens`;
//! views never write `Duration::from_millis`. One-shot animations are capped at
//! 240 ms by a `const` assertion, springs settle in ≤ 700 ms, and at most three
//! infinite loops run at once (`LoopCensus`). Scaling is centralised:
//! `motion_scale == 0.0` turns every helper into an instant cut, so no call
//! site branches on reduced motion.
//!
//! # Initialisation (in `main.rs`)
//!
//! ```rust,ignore
//! // 1. init gpui-component (sets Theme global).
//! gpui_component::theme::init(cx);
//! // 2. init our extension (reads Theme mode to pick light/dark).
//! NudoxThemeExt::init(cx);
//! // 3. Observe system appearance changes and sync both globals.
//! cx.observe_global::<gpui_component::Theme>(|cx| {
//!     NudoxThemeExt::sync_to_theme(cx);
//! }).detach(); // app-lifetime observer — detach() is legal here per LD-18
//! ```

pub mod ext;
pub mod kind;
pub mod themes;
pub mod tokens;

// ── Re-exports ────────────────────────────────────────────────────────────────

pub use ext::{NudoxThemeExt, Provenance, ThemeExtAccessor};
pub use kind::LocalKindDiscriminant;
pub use themes::{dark_theme, light_theme};
pub use tokens::{
    ColourRoles, ElevLevel, ElevTokens, KindColours, SpaceTokens, SyntaxColours, TrustStyle,
    TrustTokens, TypeScale, TypeToken,
};

// ─────────────────────────────────────────────────────────────────────────────
// Module-level tests
// ─────────────────────────────────────────────────────────────────────────────
//
// These tests verify the *structural* properties of the design system rather
// than any particular view behaviour.  They are pure-Rust (no GPUI executor
// required) because NudoxThemeExt is a plain struct and the theme constructors
// are plain functions.

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{Hsla, Pixels, px};

    // ── §10.3 ColourRoles — no transparent/unset colours ─────────────────────

    fn assert_opaque(colour: Hsla, field: &str, theme: &str) {
        assert!(
            colour.a > 0.0,
            "Colour role `{field}` in {theme} theme has alpha == 0 (accidentally unset)"
        );
    }

    macro_rules! assert_roles_opaque {
        ($roles:expr, $theme:literal) => {{
            let r = &$roles;
            let t = $theme;
            assert_opaque(r.bg_base, "bg_base", t);
            assert_opaque(r.bg_raised, "bg_raised", t);
            assert_opaque(r.bg_overlay, "bg_overlay", t);
            assert_opaque(r.bg_hover, "bg_hover", t);
            assert_opaque(r.bg_active, "bg_active", t);
            assert_opaque(r.fg_default, "fg_default", t);
            assert_opaque(r.fg_muted, "fg_muted", t);
            assert_opaque(r.fg_faint, "fg_faint", t);
            assert_opaque(r.accent, "accent", t);
            assert_opaque(r.accent_fg_on, "accent_fg_on", t);
            assert_opaque(r.border_default, "border_default", t);
            assert_opaque(r.border_strong, "border_strong", t);
            assert_opaque(r.ring, "ring", t);
            assert_opaque(r.ok, "ok", t);
            assert_opaque(r.ok_fg, "ok_fg", t);
            assert_opaque(r.warn, "warn", t);
            assert_opaque(r.warn_fg, "warn_fg", t);
            assert_opaque(r.danger, "danger", t);
            assert_opaque(r.danger_fg, "danger_fg", t);
            assert_opaque(r.info, "info", t);
            assert_opaque(r.info_fg, "info_fg", t);
        }};
    }

    macro_rules! assert_trust_opaque {
        ($trust:expr, $theme:literal) => {{
            let tr = &$trust;
            let t = $theme;
            assert_opaque(tr.local.colour, "trust.local.colour", t);
            assert_opaque(tr.local.fg_on, "trust.local.fg_on", t);
            assert_opaque(tr.synced.colour, "trust.synced.colour", t);
            assert_opaque(tr.synced.fg_on, "trust.synced.fg_on", t);
            assert_opaque(tr.remote.colour, "trust.remote.colour", t);
            assert_opaque(tr.remote.fg_on, "trust.remote.fg_on", t);
            assert_opaque(tr.stale.colour, "trust.stale.colour", t);
            assert_opaque(tr.stale.fg_on, "trust.stale.fg_on", t);
        }};
    }

    macro_rules! assert_kind_opaque {
        ($kinds:expr, $theme:literal) => {{
            let k = &$kinds;
            let t = $theme;
            assert_opaque(k.module, "kind.module", t);
            assert_opaque(k.record, "kind.record", t);
            assert_opaque(k.field, "kind.field", t);
            assert_opaque(k.function, "kind.function", t);
            assert_opaque(k.alias, "kind.alias", t);
            assert_opaque(k.trait_, "kind.trait_", t);
            assert_opaque(k.impl_, "kind.impl_", t);
            assert_opaque(k.enum_, "kind.enum_", t);
            assert_opaque(k.variant, "kind.variant", t);
            assert_opaque(k.const_, "kind.const_", t);
            assert_opaque(k.static_, "kind.static_", t);
            assert_opaque(k.reexport, "kind.reexport", t);
            assert_opaque(k.param, "kind.param", t);
            assert_opaque(k.kind_fg_on, "kind.kind_fg_on", t);
        }};
    }

    /// Every colour role must be non-transparent in the light theme.
    #[test]
    fn light_all_roles_non_transparent() {
        let theme = light_theme();
        assert_roles_opaque!(theme.colours, "light");
        assert_trust_opaque!(theme.trust, "light");
        assert_kind_opaque!(theme.kind_colours, "light");
    }

    /// Every colour role must be non-transparent in the dark theme.
    #[test]
    fn dark_all_roles_non_transparent() {
        let theme = dark_theme();
        assert_roles_opaque!(theme.colours, "dark");
        assert_trust_opaque!(theme.trust, "dark");
        assert_kind_opaque!(theme.kind_colours, "dark");
    }

    // ── §10.2 TypeScale — monotonically sensible relationships ───────────────

    /// Every token's `line_height` must be ≥ `size` (line must contain the glyph).
    #[test]
    fn type_scale_line_heights_exceed_size() {
        let ts = TypeScale::STANDARD;
        for (name, token) in [
            ("display", ts.display),
            ("title", ts.title),
            ("ui", ts.ui),
            ("prose", ts.prose),
            ("dense", ts.dense),
            ("caption", ts.caption),
            ("mono", ts.mono),
        ] {
            // `PartialOrd` is implemented for `Pixels` (gpui/src/geometry.rs:2879)
            assert!(
                token.line_height >= token.size,
                "TypeToken `{name}`: line_height < size"
            );
        }
    }

    /// The `display` token must be the largest; `caption` must be the smallest.
    #[test]
    fn type_scale_display_is_largest_caption_is_smallest() {
        let ts = TypeScale::STANDARD;
        // PartialOrd on Pixels supports comparison operators directly.
        let sizes: [(&str, Pixels); 7] = [
            ("display", ts.display.size),
            ("title", ts.title.size),
            ("ui", ts.ui.size),
            ("prose", ts.prose.size),
            ("dense", ts.dense.size),
            ("mono", ts.mono.size),
            ("caption", ts.caption.size),
        ];
        let max_name = sizes.iter()
            .max_by(|a, b| a.1.partial_cmp(&b.1).unwrap())
            .unwrap().0;
        let min_name = sizes.iter()
            .min_by(|a, b| a.1.partial_cmp(&b.1).unwrap())
            .unwrap().0;
        assert_eq!(max_name, "display", "display should be the largest token");
        assert_eq!(min_name, "caption", "caption should be the smallest token");
    }

    /// Reading text must be set looser than chrome text.
    ///
    /// This is the invariant that item 3 of the craft pass was about: prose and
    /// UI chrome shared one token, so paragraphs inherited chrome's tight
    /// 1.54 leading and the page read flat. Encoding "prose leads looser than
    /// `ui`" as a test means a future tidy-up that collapses the two tokens
    /// back together fails here, with a reason, instead of quietly regressing
    /// every documentation page.
    #[test]
    fn prose_is_set_looser_than_chrome() {
        let ts = TypeScale::STANDARD;
        let prose_ratio = f32::from(ts.prose.line_height) / f32::from(ts.prose.size);
        let ui_ratio = f32::from(ts.ui.line_height) / f32::from(ts.ui.size);

        assert!(
            prose_ratio > ui_ratio,
            "prose leading ({prose_ratio:.2}) must exceed chrome leading ({ui_ratio:.2})"
        );
        assert!(
            prose_ratio >= 1.5,
            "prose leading ({prose_ratio:.2}) is below the 1.5 reading floor"
        );
        assert!(
            ts.prose.size > ts.ui.size,
            "reading text must not be smaller than chrome text"
        );
    }

    /// The measure has to be a real constraint, not a number that never binds.
    ///
    /// A `measure` wider than the window it lives in is indistinguishable from
    /// no measure at all — the constraint silently never applies, which is the
    /// same class of failure as doctrine §8's dropped `relative()`. 720 px at
    /// the 15 px prose size is ~75 characters, the top of the readable band.
    #[test]
    fn prose_measure_binds_within_a_reasonable_reader_column() {
        let sp = SpaceTokens::STANDARD;
        let ts = TypeScale::STANDARD;

        // A conservative average advance for a proportional face is ~0.5 em.
        let approx_chars = f32::from(sp.measure) / (f32::from(ts.prose.size) * 0.5);
        assert!(
            (55.0..=90.0).contains(&approx_chars),
            "measure yields ~{approx_chars:.0} characters per line, outside the 55–90 readable band"
        );
        // It must actually bind on the reader column the shots are taken at
        // (2880 physical / 2 = 1440 logical, less docks and the outline rail).
        assert!(
            sp.measure < px(1_000.0),
            "measure must be narrower than the reader column or it never applies"
        );
    }

    // ── §10.3 KindColours — all 13 pairwise distinct hues + luminance band ───

    fn kind_colours_as_array(k: &KindColours) -> [Hsla; 13] {
        [
            k.module, k.record, k.field, k.function, k.alias,
            k.trait_, k.impl_, k.enum_, k.variant, k.const_,
            k.static_, k.reexport, k.param,
        ]
    }

    /// All 13 kind colours must have pairwise-distinct hues.
    ///
    /// "Distinct" is defined as a minimum angular hue distance of 5° (0.0139 in
    /// the 0..1 hue range used by GPUI's `Hsla`).  This guards against
    /// accidentally assigning the same hue to two variants.
    #[test]
    fn kind_colours_pairwise_distinct_hue() {
        for (label, colours) in [("light", light_theme().kind_colours), ("dark", dark_theme().kind_colours)] {
            let arr = kind_colours_as_array(&colours);
            for i in 0..arr.len() {
                for j in (i + 1)..arr.len() {
                    let diff = (arr[i].h - arr[j].h).abs();
                    // Wrap: hues 0.0 and 1.0 are the same (red).
                    let diff = diff.min(1.0 - diff);
                    assert!(
                        diff > 5.0 / 360.0,
                        "{label} theme: kind colours [{i}] and [{j}] are too close in hue \
                         (diff = {:.1}°)", diff * 360.0
                    );
                }
            }
        }
    }

    /// All 13 kind colours must share a close lightness value (matched luminance).
    ///
    /// Band: ±0.05 around the intended lightness for each theme
    /// (light = 0.38, dark = 0.62).
    #[test]
    fn kind_colours_within_luminance_band() {
        let light = light_theme().kind_colours;
        let dark = dark_theme().kind_colours;

        let check = |colours: KindColours, expected_l: f32, theme: &str| {
            for (i, c) in kind_colours_as_array(&colours).iter().enumerate() {
                assert!(
                    (c.l - expected_l).abs() <= 0.05,
                    "{theme} theme kind colour [{i}]: lightness {:.3} is outside ±0.05 of {expected_l}",
                    c.l
                );
            }
        };

        check(light, 0.38, "light");
        check(dark, 0.62, "dark");
    }

    // ── for_provenance totality ───────────────────────────────────────────────

    /// `for_provenance` must cover all `Provenance` variants (compile-time via
    /// the exhaustive `match`; this runtime test verifies the colour is non-zero).
    #[test]
    fn for_provenance_total_in_both_themes() {
        let provs = [
            Provenance::TrustedLocal,
            Provenance::SyncedLocal,
            Provenance::Remote,
            Provenance::Stale,
        ];
        for theme in [light_theme(), dark_theme()] {
            for p in provs {
                let style = theme.for_provenance(p);
                assert!(style.colour.a > 0.0, "for_provenance({p:?}) returned transparent in theme");
            }
        }
    }

    // ── Light / dark expose identical field sets ──────────────────────────────
    // This is a compile-time guarantee: both themes call `NudoxThemeExt { … }`
    // with the same struct.  If a field is added to NudoxThemeExt and forgotten
    // in one theme file, the build fails.  The runtime test below only checks
    // that both theme constructors succeed.

    /// Both theme constructors must return without panicking.
    #[test]
    fn both_themes_construct_without_panic() {
        let _l = light_theme();
        let _d = dark_theme();
    }

    // ── Elevation tokens non-empty ────────────────────────────────────────────

    /// Every elevation level must have at least one shadow entry.
    #[test]
    fn elev_tokens_have_shadows() {
        for (label, theme) in [("light", light_theme()), ("dark", dark_theme())] {
            assert!(!theme.elev.raised.shadows.is_empty(), "{label} elev.raised has no shadows");
            assert!(!theme.elev.overlay.shadows.is_empty(), "{label} elev.overlay has no shadows");
            assert!(!theme.elev.toast.shadows.is_empty(), "{label} elev.toast has no shadows");
        }
    }

    // ── SpaceTokens grid correctness ──────────────────────────────────────────

    /// Space tokens must be on the 4 px grid and in ascending order.
    #[test]
    fn space_tokens_on_4px_grid() {

        let s = SpaceTokens::STANDARD;
        let spaces = [s.space_1, s.space_2, s.space_3, s.space_4, s.space_5, s.space_6, s.space_7, s.space_8];
        for (i, sp) in spaces.iter().enumerate() {
            // `Rem` is implemented for `Pixels` (gpui/src/geometry.rs:2699)
            let remainder = *sp % px(4.0);
            assert_eq!(
                remainder,
                px(0.0),
                "space_{} is not on the 4px grid", i + 1
            );
        }
        // ascending
        for i in 0..spaces.len() - 1 {
            assert!(
                spaces[i] < spaces[i + 1],
                "space_{} >= space_{}: not ascending",
                i + 1,
                i + 2
            );
        }
    }
}
