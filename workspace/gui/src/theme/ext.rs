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
use gpui_component::ActiveTheme as _;

use crate::theme::tokens::{
    ColourRoles, ElevTokens, KindColours, SpaceTokens, TrustStyle, TrustTokens, TypeScale,
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
    /// Spacing and radius tokens (§10.1 — same in both themes).
    pub space: SpaceTokens,
    /// Type-scale tokens (§10.2 — same in both themes).
    pub type_scale: TypeScale,
    /// Semantic colour roles (§10.3).
    pub colours: ColourRoles,
    /// Trust-chrome colour tokens (§10.4).
    pub trust: TrustTokens,
    /// Elevation box-shadow tokens (§10.5).
    pub elev: ElevTokens,
    /// Per-kind badge colours at matched luminance (§10.3 tail).
    pub kind_colours: KindColours,
    /// Motion duration multiplier.  `1.0` = full fidelity, `0.5` = reduced,
    /// `0.0` = instant-cut (animations degrade gracefully — §6.1).
    pub motion_scale: f32,
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

    /// Initialise the global with the appropriate theme for the current
    /// gpui-component `ThemeMode`.  Call once in `main.rs` after
    /// `gpui_component::theme::init(cx)`.
    pub fn init(cx: &mut App) {
        use crate::theme::themes::{dark_theme, light_theme};
        let ext = if cx.theme().is_dark() {
            dark_theme()
        } else {
            light_theme()
        };
        cx.set_global(ext);
    }

    /// Switch the extension to match the new gpui-component theme mode.
    /// Call from the same `observe_global::<Theme>` hook that updates gpui-component.
    pub fn sync_to_theme(cx: &mut App) {
        use crate::theme::themes::{dark_theme, light_theme};
        let ext = if cx.theme().is_dark() {
            dark_theme()
        } else {
            light_theme()
        };
        cx.set_global(ext);
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

    /// `for_provenance` must be total: every `Provenance` variant returns a
    /// non-zero-alpha colour.
    #[test]
    fn for_provenance_is_total() {
        use crate::theme::themes::{dark_theme, light_theme};

        let variants = [
            Provenance::TrustedLocal,
            Provenance::SyncedLocal,
            Provenance::Remote,
            Provenance::Stale,
        ];

        for theme in [light_theme(), dark_theme()] {
            for p in variants {
                let style = theme.for_provenance(p);
                assert!(
                    style.colour.a > 0.0,
                    "for_provenance({p:?}) returned transparent colour in theme"
                );
            }
        }
    }

    /// `reduced_motion()` is true iff `motion_scale == 0.0`.
    #[test]
    fn reduced_motion_matches_scale() {
        use crate::theme::themes::light_theme;

        let mut ext = light_theme();
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
        use crate::theme::themes::light_theme;
        use std::time::Duration;

        let mut ext = light_theme();
        ext.motion_scale = 0.0;
        assert_eq!(ext.scale_duration(Duration::from_millis(160)), Duration::ZERO);

        ext.motion_scale = 0.5;
        assert_eq!(
            ext.scale_duration(Duration::from_millis(160)),
            Duration::from_millis(80)
        );
    }
}
