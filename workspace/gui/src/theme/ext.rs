//! `NudoxThemeExt` — the GPUI global that carries all nudox-specific design
//! tokens on top of gpui-component's `Theme` (GUI-PLAN LD-14).
//!
//! # Layering model
//!
//! gpui-component's `Theme` global (verified at
//! `gpui-component@c112e7b/crates/ui/src/theme/mod.rs:109` — `impl Global for Theme`)
//! is the base.  Views call `cx.theme()` (the `ActiveTheme` trait from
//! `gpui-component`, mod.rs:32–41) for generic UI tokens: backgrounds, borders,
//! foreground, scrollbar, shadow bool, font families, etc.
//!
//! `NudoxThemeExt` is a *second* GPUI global alongside `Theme`.  It carries
//! what §10 needs that gpui-component does not provide: trust chrome, lineage
//! colours, kind colours, motion scale, elevation box-shadows, and the nudox
//! role vocabulary (`ColourRoles`).
//!
//! Views that need nudox tokens call `cx.theme_ext()` instead of (or in
//! addition to) `cx.theme()`.  The accessor trait mirrors exactly how
//! gpui-component exposes `Theme` — one trait, one method, zero runtime cost.
//!
//! # LD-14 compliance
//!
//! - We do not fork, subclass, or reimplement gpui-component's `Theme`.
//! - Anything gpui-component already provides (font families, scrollbar colours,
//!   border radius, etc.) is consumed from `cx.theme()`.
//! - Only what §10 specifies and gpui-component lacks is added here.
//!
//! # `for_provenance` — the LD-8 "trust is chrome" function
//!
//! Every view that renders a provenance badge calls exactly one function:
//! `cx.theme_ext().for_provenance(provenance)`.  This returns a [`TrustStyle`]
//! with the colour, badge glyph, label, and hatching flag.  No view contains
//! a `match` on `Provenance`.  The match lives here, in data, keyed by theme.

use gpui::{App, Global};
use gpui_component::dock::TitleStyle;

use crate::theme::palette::{Appearance, Palette};
use crate::theme::tokens::{
    AlphaTokens, ColourRoles, ElevTokens, KindColours, SpaceTokens, SyntaxColours, TrustStyle,
    TrustTokens, TypeScale,
};

// ─────────────────────────────────────────────────────────────────────────────
// Provenance mirror
// ─────────────────────────────────────────────────────────────────────────────

/// Local mirror of `nudox_engine::wire::Provenance` (§L2.1).
///
/// TODO(wire): when `nudox-engine` becomes a dependency of this crate, replace
/// this enum with `use nudox_engine::wire::Provenance` and the `for_provenance`
/// signature with `&nudox_engine::wire::Provenance`.  Track at:
/// `crates/nudox-engine/src/wire/mod.rs`.
///
/// Discriminant values must stay in sync with the wire definition.
/// `#[non_exhaustive]` because new provenance kinds may be added.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum Provenance {
    /// Produced on this machine from source we can see. The *common* case (LR-10).
    TrustedLocal,
    /// Fetched and verified from a remote generation.
    SyncedLocal,
    /// Served remotely, not yet materialised locally.
    Remote,
    /// Last-known-good, served while offline.
    Stale,
}

// ─────────────────────────────────────────────────────────────────────────────
// NudoxThemeExt
// ─────────────────────────────────────────────────────────────────────────────

/// The nudox design-system extension, registered as a GPUI global.
///
/// Carry `#[derive(Clone)]` so the themes module can hand an owned value to
/// `cx.set_global()`, and tests can copy it freely without Arc indirection.
#[derive(Debug, Clone)]
pub struct NudoxThemeExt {
    /// Stable identifier of the live theme — `"ink-dark"`, `"paper-light"`.
    ///
    /// Carried on the theme rather than looked up from the registry so that a
    /// view can label what it is rendering without reaching for a second
    /// global, and so a test can assert which theme produced a frame.
    pub theme_key: gpui::SharedString,
    /// The live theme's display name, for the status bar and the cycle toast.
    pub theme_name: gpui::SharedString,
    /// One line saying what the live theme is for.
    pub theme_blurb: gpui::SharedString,
    /// Which direction the scale runs.
    ///
    /// Replaces `cx.theme().is_dark()` as the answer to "am I in the dark?".
    /// The gpui-component call was answering a question about *that* global's
    /// mode, which used to be the input to our theme choice and is now an
    /// output of it — asking it was a cycle waiting to be closed the wrong way.
    pub appearance: Appearance,
    /// Spacing and radius tokens (§10.1 — same in every theme: geometry does
    /// not change with colour).
    pub space: SpaceTokens,
    /// Type-scale tokens (§10.2 — same in every theme).
    pub type_scale: TypeScale,
    /// The transparency ladder (same in every theme).
    pub alpha: AlphaTokens,
    /// Semantic colour roles (§10.3).
    pub colours: ColourRoles,
    /// Trust-chrome colour tokens (§10.4).
    pub trust: TrustTokens,
    /// Elevation box-shadow tokens (§10.5).
    pub elev: ElevTokens,
    /// Per-kind badge colours at matched luminance (§10.3 tail).
    pub kind_colours: KindColours,
    /// Syntax token colour assignments (§10.6).
    ///
    /// Used by `SignatureLine`, code-block renderers, and any other component
    /// that needs to colour-code lexical token classes.  The values align with
    /// the `class_colour` function in `views/symbol_page/docs.rs` so that
    /// signatures and code blocks share one colour vocabulary.
    pub syntax: SyntaxColours,
    /// Motion duration multiplier.  `1.0` = full fidelity, `0.5` = reduced,
    /// `0.0` = instant-cut (animations degrade gracefully — §6.1).
    pub motion_scale: f32,
    /// The palette every token above was selected from.
    ///
    /// Carried so that the property the restructure exists to guarantee is
    /// *checkable*: `tests/theme_law.rs` walks every resolved role and asserts
    /// it is a colour this palette produced. Without the palette on the theme
    /// that assertion would have to re-derive it and could re-derive it wrongly.
    pub palette: Palette,
}

impl Global for NudoxThemeExt {}

impl NudoxThemeExt {
    /// Return the theme extension from app context.  Panics if not initialised —
    /// the same contract as `Theme::global`.  Call `NudoxThemeExt::init` in
    /// `main.rs` before opening the window.
    #[inline(always)]
    pub fn global(cx: &App) -> &NudoxThemeExt {
        cx.global::<NudoxThemeExt>()
    }

    /// Install the bundled themes and make the first one live.
    ///
    /// # What this replaced, and why the old shape was backwards
    ///
    /// This used to be:
    ///
    /// ```rust,ignore
    /// let ext = if cx.theme().is_dark() { dark_theme() } else { light_theme() };
    /// cx.set_global(ext);
    /// ```
    ///
    /// — our palette chosen by asking gpui-component what mode *it* was in,
    /// with a companion `sync_to_theme` wired to `observe_global::<Theme>` to
    /// keep chasing it. Three things follow from that direction of flow, and
    /// all three were real:
    ///
    /// - The set of possible themes was exactly two, because the input was a
    ///   boolean. A third theme had nothing to be selected *by*.
    /// - gpui-component's own colours were never ours. Its `Kbd`, `Button` and
    ///   scrollbars read its `Theme`, which we only ever read from, so half the
    ///   pixels in the window were outside the design system.
    /// - `ThemeMode::default()` is `Light` and `main.rs` never chose, so the
    ///   application's appearance was a property of the host machine.
    ///
    /// The flow is now palette → both globals. See
    /// [`crate::theme::registry::ThemeRegistry`].
    pub fn init(cx: &mut App) -> Result<(), crate::theme::spec::ThemeLoadError> {
        crate::theme::registry::ThemeRegistry::init(cx)
    }

    // ── LD-8  "trust is chrome" ──────────────────────────────────────────────

    /// The single canonical function for trust chrome (LD-8).
    ///
    /// Every view that needs to render a provenance badge calls this.
    /// No view contains a `match` on `Provenance` — the mapping is here,
    /// in theme data, and varies freely between themes.
    pub fn for_provenance(&self, p: Provenance) -> TrustStyle {
        match p {
            Provenance::TrustedLocal => self.trust.local,
            Provenance::SyncedLocal => self.trust.synced,
            Provenance::Remote => self.trust.remote,
            Provenance::Stale => self.trust.stale,
        }
    }

    // ── Panel chrome ──────────────────────────────────────────────────────────

    /// The colour pairing for a docked panel's header bar (the "Project" /
    /// "Editor" / "Outline" row at the top of a single-panel dock).
    ///
    /// `gpui_component::dock::Panel::title_style` defaults to `None` when a
    /// panel does not override it. `TabPanel::render_title_bar` only calls
    /// `.text_color(..)` on the header when a `TitleStyle` is actually
    /// returned — with `None` the header text is left at GPUI's own
    /// `TextStyle::default()`, which is opaque black
    /// (`gpui::style::TextStyle::default().color == black()`), regardless of
    /// theme. That is a *missing* role, not a mis-picked colour: nothing in
    /// this crate had ever named "panel header foreground" as a thing that
    /// needed a themed value, so every single-panel dock header rendered
    /// unthemed black text on a near-black base surface.
    ///
    /// Every `Panel::title_style` override in this crate must return this
    /// value (never hand-roll `TitleStyle { .. }` at the call site) so the
    /// header text always matches the same base-surface/default-foreground
    /// pairing used everywhere else, in both themes.
    pub fn panel_title_style(&self) -> TitleStyle {
        TitleStyle {
            background: self.colours.bg_base,
            foreground: self.colours.fg_default,
        }
    }

    /// Whether the live theme is a dark one.
    ///
    /// Prefer this to `cx.theme().is_dark()`: that asks gpui-component, which
    /// is now downstream of this value rather than upstream of it.
    #[inline(always)]
    pub fn is_dark(&self) -> bool {
        self.appearance.is_dark()
    }

    // ── Derived geometry ─────────────────────────────────────────────────────

    /// The height of one row in a virtualized list whose text is set in
    /// `token`.
    ///
    /// # Why this is a method and not five copies of an expression
    ///
    /// `uniform_list` requires every row to be exactly the same height, and it
    /// is told the list's total extent separately (`count * row_height`). Those
    /// two numbers are computed in different scopes — the outer one before the
    /// closure, the inner one inside it — so the formula was being written out
    /// twice per table, four times in `refs.rs` alone, plus variants in the
    /// project panel and the symbol header. Any two of them drifting clips rows
    /// and desynchronises the scrollbar from the content, and nothing catches
    /// it but a screenshot.
    ///
    /// Parameterising by the type token is what makes one definition serve all
    /// of them: a row is one line of its own text plus a `space_2` of vertical
    /// breathing room, whether that text is `dense` chrome or a `mono`
    /// signature. Changing the rhythm is now one edit rather than a search.
    #[inline]
    pub fn row_height(&self, token: crate::theme::tokens::TypeToken) -> gpui::Pixels {
        token.line_height + self.space.space_2
    }

    /// The tallest a disclosure section's body may get before it scrolls, for a
    /// table whose rows are set in `token`.
    ///
    /// # Why this is derived and not a constant
    ///
    /// A cap over a list of fixed-height rows has to be a whole number of those
    /// rows, or it clips one in half — which is exactly what `px(400.0)` did to
    /// the References table (see [`crate::theme::tokens::SpaceTokens::section_rows`]).
    /// Deriving it here, from the same [`NudoxThemeExt::row_height`] the table
    /// lays its rows out with, means the two cannot disagree: change the row
    /// rhythm and the cap moves with it.
    #[inline]
    pub fn section_max_h(&self, token: crate::theme::tokens::TypeToken) -> gpui::Pixels {
        self.row_height(token) * self.space.section_rows
    }

    // ── Motion helpers (§6.1) ────────────────────────────────────────────────

    /// Whether animations should be fully suppressed (instant-cut everywhere).
    ///
    /// True when `motion_scale == 0.0`.  Motion helpers check this before
    /// scheduling any declarative or spring animation.
    #[inline(always)]
    pub fn reduced_motion(&self) -> bool {
        self.motion_scale == 0.0
    }

    /// Scale a `std::time::Duration` by the current `motion_scale`.
    ///
    /// Returns `Duration::ZERO` when `motion_scale == 0.0`, which causes
    /// `Animation` to snap and `Motion::animate_to` to call `snap_to`.
    #[inline(always)]
    pub fn scale_duration(&self, d: std::time::Duration) -> std::time::Duration {
        if self.motion_scale <= 0.0 {
            std::time::Duration::ZERO
        } else {
            d.mul_f32(self.motion_scale)
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Accessor trait
// ─────────────────────────────────────────────────────────────────────────────

/// Extension trait that adds `cx.theme_ext()` to any GPUI context.
///
/// Mirrors the `ActiveTheme` trait from gpui-component
/// (`gpui-component@c112e7b/crates/ui/src/theme/mod.rs:32–41`):
///
/// ```rust,ignore
/// pub trait ActiveTheme {
///     fn theme(&self) -> &Theme;
/// }
/// impl ActiveTheme for App {
///     fn theme(&self) -> &Theme { Theme::global(self) }
/// }
/// ```
///
/// We add the same pattern for our extension.
pub trait ThemeExtAccessor {
    /// Returns a reference to the nudox theme extension global.
    ///
    /// Panics if [`NudoxThemeExt::init`] has not been called — the same
    /// contract as `cx.theme()` from gpui-component.
    fn theme_ext(&self) -> &NudoxThemeExt;
}

impl ThemeExtAccessor for App {
    #[inline(always)]
    fn theme_ext(&self) -> &NudoxThemeExt {
        NudoxThemeExt::global(self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::theme::resolve::resolve;
    use crate::theme::spec::parse_bundled;

    fn every_theme() -> Vec<NudoxThemeExt> {
        parse_bundled()
            .expect("bundled themes parse")
            .iter()
            .map(resolve)
            .collect()
    }

    /// `for_provenance` must be total: every `Provenance` variant returns a
    /// non-zero-alpha colour, in every theme — not just in the two that used to
    /// be the only ones that could exist.
    #[test]
    fn for_provenance_is_total_in_every_bundled_theme() {
        let variants = [
            Provenance::TrustedLocal,
            Provenance::SyncedLocal,
            Provenance::Remote,
            Provenance::Stale,
        ];
        for theme in every_theme() {
            for p in variants {
                let style = theme.for_provenance(p);
                assert!(
                    style.colour.a > 0.0,
                    "{}: for_provenance({p:?}) returned a transparent colour",
                    theme.theme_key
                );
            }
        }
    }

    /// The four trust levels must be told apart by hue, in every theme.
    ///
    /// Provenance is the one piece of chrome whose whole job is to be
    /// distinguishable at a glance (LD-8). A theme author is free to recolour
    /// it and not free to collapse it, and before the palette restructure
    /// nothing said so — `remote` and `warn` shared a hue in both shipped
    /// themes by copy-paste, which is fine, but nothing would have caught
    /// `local` and `synced` doing the same.
    #[test]
    fn trust_levels_stay_distinguishable_in_every_theme() {
        for theme in every_theme() {
            let hues = [
                ("local", theme.trust.local.colour),
                ("synced", theme.trust.synced.colour),
                ("remote", theme.trust.remote.colour),
                ("stale", theme.trust.stale.colour),
            ];
            for i in 0..hues.len() {
                for j in (i + 1)..hues.len() {
                    let (an, a) = hues[i];
                    let (bn, b) = hues[j];
                    // `stale` is deliberately desaturated, so hue distance is
                    // meaningless for it; saturation is what separates it.
                    let separated = {
                        let d = (a.h - b.h).abs();
                        d.min(1.0 - d) > 10.0 / 360.0 || (a.s - b.s).abs() > 0.3
                    };
                    assert!(
                        separated,
                        "{}: trust levels `{an}` and `{bn}` are not distinguishable",
                        theme.theme_key
                    );
                }
            }
        }
    }

    /// `reduced_motion()` is true iff `motion_scale == 0.0`.
    #[test]
    fn reduced_motion_matches_scale() {
        let mut ext = every_theme().remove(0);
        ext.motion_scale = 1.0;
        assert!(!ext.reduced_motion());

        ext.motion_scale = 0.5;
        assert!(!ext.reduced_motion());

        ext.motion_scale = 0.0;
        assert!(ext.reduced_motion());
    }

    /// `scale_duration` clamps to zero when `motion_scale == 0`.
    #[test]
    fn scale_duration_zero() {
        use std::time::Duration;

        let mut ext = every_theme().remove(0);
        ext.motion_scale = 0.0;
        assert_eq!(
            ext.scale_duration(Duration::from_millis(160)),
            Duration::ZERO
        );

        ext.motion_scale = 0.5;
        assert_eq!(
            ext.scale_duration(Duration::from_millis(160)),
            Duration::from_millis(80)
        );
    }
}
