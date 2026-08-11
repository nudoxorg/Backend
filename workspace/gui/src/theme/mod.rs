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
pub mod palette;
pub mod registry;
pub mod resolve;
pub mod spec;
pub mod tokens;

// ── Re-exports ────────────────────────────────────────────────────────────────

pub use ext::{NudoxThemeExt, Provenance, ThemeExtAccessor};
pub use kind::LocalKindDiscriminant;
pub use palette::{Appearance, Palette, Ramp, RampSpec, Step};
pub use registry::ThemeRegistry;
pub use resolve::resolve;
pub use spec::{ThemeLoadError, ThemeSpec, parse_bundled};
pub use tokens::{
    AlphaTokens, ColourRoles, ElevLevel, ElevTokens, KindColours, SpaceTokens, SyntaxColours,
    TrustStyle, TrustTokens, TypeScale, TypeToken,
};

/// Resolve a bundled theme by key, outside of any GPUI context.
///
/// # Why this exists
///
/// Unit tests and pure-Rust helpers need a fully-populated `NudoxThemeExt`
/// without booting an app. They used to call `themes::dark_theme()`, a
/// hand-written constructor that no longer exists. Going through the bundle
/// means a test is asserting against a theme the application can actually be
/// in — a hand-written test theme is a fixture, and doctrine §4 is explicit
/// about what fixtures test.
///
/// Returns `None` for an unknown key rather than panicking, so a caller that
/// mistypes gets a compile-visible `unwrap` at its own call site instead of a
/// panic from inside the theme layer.
pub fn theme_by_key(key: &str) -> Option<NudoxThemeExt> {
    parse_bundled()
        .ok()?
        .iter()
        .find(|s| s.key == key)
        .map(resolve)
}

/// The theme the application starts in — first in cycle order.
///
/// The pure-Rust equivalent of "what the reader sees before they touch
/// anything", for tests that need *a* theme and do not care which.
pub fn default_theme() -> NudoxThemeExt {
    let specs = parse_bundled().expect(
        "the bundled themes are compiled in and covered by \
         tests/theme_law.rs::bundled_themes_all_parse; a failure here means that \
         test is not running",
    );
    resolve(&specs[0])
}

// ─────────────────────────────────────────────────────────────────────────────
// Module-level tests
// ─────────────────────────────────────────────────────────────────────────────
//
// These verify *structural* properties of the design system rather than any
// particular view behaviour, and they now run over **every bundled theme**
// rather than over the two that used to be the only ones expressible. That is
// the real dividend of the restructure: an invariant asserted over a set the
// theme author extends is a guard; asserted over two hand-written constants it
// is a spot check.

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{Hsla, Pixels, px};

    fn all_themes() -> Vec<NudoxThemeExt> {
        parse_bundled()
            .expect("bundled themes parse")
            .iter()
            .map(resolve)
            .collect()
    }

    // ── §10.3 ColourRoles — no transparent/unset colours ─────────────────────

    /// Every colour role must be opaque, in every theme.
    ///
    /// The scrim is the one deliberate exception: it exists to be translucent,
    /// and a fully opaque scrim would hide the window it is dimming.
    #[test]
    fn all_roles_opaque_except_the_scrim_and_the_ring() {
        for t in all_themes() {
            let r = t.colours;
            let cases: [(&str, Hsla); 24] = [
                ("bg_sunken", r.bg_sunken),
                ("bg_base", r.bg_base),
                ("bg_raised", r.bg_raised),
                ("bg_overlay", r.bg_overlay),
                ("bg_hover", r.bg_hover),
                ("bg_active", r.bg_active),
                ("fg_default", r.fg_default),
                ("fg_muted", r.fg_muted),
                ("fg_faint", r.fg_faint),
                ("accent", r.accent),
                ("accent_hover", r.accent_hover),
                ("accent_text", r.accent_text),
                ("accent_wash", r.accent_wash),
                ("accent_wash_hover", r.accent_wash_hover),
                ("accent_fg_on", r.accent_fg_on),
                ("border_subtle", r.border_subtle),
                ("border_default", r.border_default),
                ("border_strong", r.border_strong),
                ("ok", r.ok),
                ("warn", r.warn),
                ("danger", r.danger),
                ("info", r.info),
                ("ok_fg", r.ok_fg),
                ("danger_fg", r.danger_fg),
            ];
            for (field, colour) in cases {
                assert_eq!(
                    colour.a, 1.0,
                    "{}: colour role `{field}` is not opaque (a = {})",
                    t.theme_key, colour.a
                );
            }
            assert!(
                r.scrim.a > 0.0 && r.scrim.a < 1.0,
                "{}: the scrim must be translucent, not {}",
                t.theme_key,
                r.scrim.a
            );
        }
    }

    /// Trust chrome and kind badges must be opaque in every theme.
    #[test]
    fn trust_and_kind_colours_are_opaque_in_every_theme() {
        for t in all_themes() {
            for (name, style) in [
                ("local", t.trust.local),
                ("synced", t.trust.synced),
                ("remote", t.trust.remote),
                ("stale", t.trust.stale),
            ] {
                assert_eq!(style.colour.a, 1.0, "{}: trust.{name} not opaque", t.theme_key);
                assert_eq!(style.fg_on.a, 1.0, "{}: trust.{name}.fg_on not opaque", t.theme_key);
                assert_eq!(style.wash.a, 1.0, "{}: trust.{name}.wash not opaque", t.theme_key);
            }
            for (i, c) in kind_colours_as_array(&t.kind_colours).iter().enumerate() {
                assert_eq!(c.a, 1.0, "{}: kind colour [{i}] not opaque", t.theme_key);
            }
        }
    }

    // ── §10.2 TypeScale ──────────────────────────────────────────────────────

    /// Every token's `line_height` must be ≥ `size` (line must contain glyph).
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
        let sizes: [(&str, Pixels); 7] = [
            ("display", ts.display.size),
            ("title", ts.title.size),
            ("ui", ts.ui.size),
            ("prose", ts.prose.size),
            ("dense", ts.dense.size),
            ("mono", ts.mono.size),
            ("caption", ts.caption.size),
        ];
        let max_name = sizes.iter().max_by(|a, b| a.1.partial_cmp(&b.1).unwrap()).unwrap().0;
        let min_name = sizes.iter().min_by(|a, b| a.1.partial_cmp(&b.1).unwrap()).unwrap().0;
        assert_eq!(max_name, "display", "display should be the largest token");
        assert_eq!(min_name, "caption", "caption should be the smallest token");
    }

    /// Reading text must be set looser than chrome text.
    ///
    /// Encoding "prose leads looser than `ui`" as a test means a future
    /// tidy-up that collapses the two tokens back together fails here, with a
    /// reason, instead of quietly regressing every documentation page.
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
    #[test]
    fn prose_measure_binds_within_a_reasonable_reader_column() {
        let sp = SpaceTokens::STANDARD;
        let ts = TypeScale::STANDARD;

        let approx_chars = f32::from(sp.measure) / (f32::from(ts.prose.size) * 0.5);
        assert!(
            (55.0..=90.0).contains(&approx_chars),
            "measure yields ~{approx_chars:.0} characters per line, outside the 55–90 band"
        );
        assert!(
            sp.measure < px(1_000.0),
            "measure must be narrower than the reader column or it never applies"
        );
    }

    // ── §10.3 KindColours ────────────────────────────────────────────────────

    fn kind_colours_as_array(k: &KindColours) -> [Hsla; 13] {
        [
            k.module, k.record, k.field, k.function, k.alias,
            k.trait_, k.impl_, k.enum_, k.variant, k.const_,
            k.static_, k.reexport, k.param,
        ]
    }

    /// All 13 kind colours must have pairwise-distinct hues, in every theme.
    #[test]
    fn kind_colours_pairwise_distinct_hue() {
        for t in all_themes() {
            let arr = kind_colours_as_array(&t.kind_colours);
            for i in 0..arr.len() {
                for j in (i + 1)..arr.len() {
                    let diff = (arr[i].h - arr[j].h).abs();
                    let diff = diff.min(1.0 - diff);
                    assert!(
                        diff > 5.0 / 360.0,
                        "{}: kind colours [{i}] and [{j}] are too close in hue ({:.1}°)",
                        t.theme_key,
                        diff * 360.0
                    );
                }
            }
        }
    }

    /// A kind hue means the same kind in every theme.
    ///
    /// This is the invariant that makes the colour vocabulary learnable: a
    /// reader who has learned that green means `function` must not have to
    /// relearn it when they change theme. A theme may restate the hues — the
    /// field is in the spec — but every bundled theme is expected to agree,
    /// and this test is what makes disagreement a decision rather than a slip.
    #[test]
    fn kind_hues_are_the_same_in_every_bundled_theme() {
        let themes = all_themes();
        let reference: Vec<f32> = kind_colours_as_array(&themes[0].kind_colours)
            .iter()
            .map(|c| c.h)
            .collect();
        for t in &themes[1..] {
            for (i, c) in kind_colours_as_array(&t.kind_colours).iter().enumerate() {
                assert!(
                    (c.h - reference[i]).abs() < 1e-6,
                    "{}: kind [{i}] hue {:.4} differs from `{}`'s {:.4}; a hue must mean \
                     the same kind in every theme",
                    t.theme_key,
                    c.h,
                    themes[0].theme_key,
                    reference[i]
                );
            }
        }
    }

    /// All 13 kind colours must share one lightness plane, in every theme.
    #[test]
    fn kind_colours_within_luminance_band() {
        for t in all_themes() {
            let arr = kind_colours_as_array(&t.kind_colours);
            let expected = arr[0].l;
            for (i, c) in arr.iter().enumerate() {
                assert!(
                    (c.l - expected).abs() <= 1e-6,
                    "{}: kind colour [{i}] lightness {:.3} left the shared plane {expected:.3}",
                    t.theme_key,
                    c.l
                );
            }
        }
    }

    // ── for_provenance totality ───────────────────────────────────────────────

    /// `for_provenance` must cover all `Provenance` variants in every theme.
    #[test]
    fn for_provenance_total_in_every_theme() {
        let provs = [
            Provenance::TrustedLocal,
            Provenance::SyncedLocal,
            Provenance::Remote,
            Provenance::Stale,
        ];
        for t in all_themes() {
            for p in provs {
                let style = t.for_provenance(p);
                assert!(
                    style.colour.a > 0.0,
                    "{}: for_provenance({p:?}) returned transparent",
                    t.theme_key
                );
            }
        }
    }

    /// Every bundled theme constructs, and they are all distinct.
    ///
    /// "All distinct" matters because the cycle keybinding is only meaningful
    /// if pressing it changes something. Two theme files that resolve to the
    /// same colours would make one press of the key look like a dropped input.
    #[test]
    fn every_bundled_theme_constructs_and_differs_from_the_others() {
        let themes = all_themes();
        assert!(themes.len() >= 2, "the cycle needs at least two themes");
        for i in 0..themes.len() {
            for j in (i + 1)..themes.len() {
                assert_ne!(
                    themes[i].colours, themes[j].colours,
                    "themes `{}` and `{}` resolve to identical colour roles",
                    themes[i].theme_key, themes[j].theme_key
                );
            }
        }
    }

    // ── Elevation ─────────────────────────────────────────────────────────────

    /// Every elevation level must have a shadow and a real border.
    ///
    /// The border half is new. It used to be `dark_border`, transparent in
    /// light themes, which meant a light-theme popover had no edge at all —
    /// visible in any light-mode frame as a white panel dissolving into a
    /// near-white page.
    #[test]
    fn elevation_levels_have_both_a_shadow_and_a_border() {
        for t in all_themes() {
            for (name, level) in [
                ("raised", &t.elev.raised),
                ("overlay", &t.elev.overlay),
                ("toast", &t.elev.toast),
            ] {
                assert!(
                    !level.shadows.is_empty(),
                    "{}: elev.{name} has no shadows",
                    t.theme_key
                );
                assert!(
                    level.border.a > 0.0,
                    "{}: elev.{name} has a fully transparent border",
                    t.theme_key
                );
            }
        }
    }

    // ── SpaceTokens ───────────────────────────────────────────────────────────

    /// Space tokens must be on the 4 px grid and in ascending order.
    #[test]
    fn space_tokens_on_4px_grid() {
        let s = SpaceTokens::STANDARD;
        let spaces = [s.space_1, s.space_2, s.space_3, s.space_4, s.space_5, s.space_6, s.space_7, s.space_8];
        for (i, sp) in spaces.iter().enumerate() {
            assert_eq!(*sp % px(4.0), px(0.0), "space_{} is not on the 4px grid", i + 1);
        }
        for i in 0..spaces.len() - 1 {
            assert!(
                spaces[i] < spaces[i + 1],
                "space_{} >= space_{}: not ascending",
                i + 1,
                i + 2
            );
        }
    }

    /// A caret must be thicker than a hairline, or it reads as an artefact.
    #[test]
    fn the_caret_is_not_a_hairline() {
        let s = SpaceTokens::STANDARD;
        assert!(
            s.caret_width > s.border_width,
            "caret_width ({:?}) must exceed border_width ({:?}); the search field \
             drew its caret with border_width and it read as a rendering glitch",
            s.caret_width,
            s.border_width
        );
    }
}
