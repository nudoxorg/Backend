//! The palette — the primitive every theme is composed from.
//!
//! # Why this layer exists
//!
//! The governing rule of this design system is that consistency is achieved by
//! there being *nowhere else to get a number from*. Until this module existed
//! that rule was enforced on views and violated by the theme layer itself.
//!
//! `themes/light.rs` and `themes/dark.rs` each wrote ~90 independent
//! `hsla(…)` literals. `tokens.rs` claimed "palette primitives are private to
//! `themes/light.rs` and `themes/dark.rs`" — there were none; there were only
//! literals. The cost was not hypothetical: in the dark theme the triple
//! `hsla(243/360, 0.82, 0.70, 1.0)` appeared three times — as `colours.accent`,
//! as `colours.ring` (with a different alpha) and as `syntax.kw` — kept in
//! agreement by a comment saying they should agree. That is the exact failure
//! mode the rule forbids, happening inside the layer whose job is to prevent
//! it, and it is why a "third theme" cost 90 hand-picked colours instead of a
//! handful of decisions.
//!
//! # The model
//!
//! ```text
//!   RampSpec  (hue, chroma, solid)          ← what a theme *decides*
//!        │
//!        ▼  Ramp::generate(spec, appearance)
//!   Ramp = 12 perceptual steps               ← the primitive
//!        │
//!        ▼  resolve::roles(&palette)          ← ONE role table, all themes
//!   ColourRoles / TrustTokens / KindColours   ← what a *view* asks for
//! ```
//!
//! A view never sees a `Ramp`. It asks for `fg_muted` exactly as before. What
//! changed is where `fg_muted` comes from: one shared assignment
//! (`neutral[TextLow]`) instead of one hand-picked literal per theme.
//!
//! # Why twelve steps, and why these twelve
//!
//! The step semantics are Radix Colors' (<https://www.radix-ui.com/colors/docs/palette-composition/understanding-the-scale>),
//! which zed's own theme crate also adopts —
//! `crates/theme/src/scale.rs` in the vendored checkout defines
//! `ColorScale::step_1()`..`step_12()` with the same intent per step, and
//! builds every semantic role by indexing into it
//! (`PlayerColor { cursor: blue().dark().step_9(), … }`). Borrowing a scale
//! that two mature systems independently converged on is worth more than
//! inventing a twelfth one.
//!
//! The one thing this module does *not* copy from Radix is numeric step names.
//! [`Step`] is an enum of intents — `Step::Solid`, `Step::BorderSubtle`,
//! `Step::TextLow` — because `p.accent.get(Step::Solid)` states what the caller
//! wanted and `p.accent.step_9()` does not, and because a wrong intent is
//! visible in review while a wrong integer is not.
//!
//! # Why a ramp is three numbers
//!
//! `RampSpec` is `(hue, chroma, solid)`. Hue and chroma place the family;
//! `solid` is the lightness of [`Step::Solid`], the one step a designer really
//! does pick by eye, because "how light does this hue have to be before it
//! reads as *the* colour" is a property of the hue and not of the scale. Every
//! other step is derived. Six ramps × three numbers replaces ~90 literals.

use gpui::{Hsla, hsla};

// ─────────────────────────────────────────────────────────────────────────────
// Appearance
// ─────────────────────────────────────────────────────────────────────────────

/// Which direction the scale runs.
///
/// This is not "light theme / dark theme" — it is the single fact a ramp needs
/// in order to know whether [`Step::AppBg`] is nearly white or nearly black.
/// Two themes can share an appearance and differ in every hue.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Appearance {
    /// Backgrounds are light, text is dark, and elevation is expressed by
    /// getting *brighter* — a popover is white on a grey page.
    Light,
    /// Backgrounds are dark, text is light, and elevation is expressed by
    /// getting brighter as well — but from a much darker floor.
    Dark,
}

impl Appearance {
    /// Whether this appearance wants light text on dark surfaces.
    #[inline]
    pub fn is_dark(self) -> bool {
        matches!(self, Appearance::Dark)
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Steps
// ─────────────────────────────────────────────────────────────────────────────

/// One position on a [`Ramp`], named by what it is *for*.
///
/// The twelve intents are Radix's; the names are ours. A caller that cannot
/// find its intent here is describing a gap in the scale, not a reason to index
/// by number — the same rule the rest of this design system runs on, applied to
/// the palette itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum Step {
    /// The page itself. Nothing sits behind this.
    AppBg = 0,
    /// A background that is *almost* the page: zebra striping, sunken wells,
    /// the inside of a scroll track.
    SubtleBg = 1,
    /// A UI element at rest: a chip, an unpressed button, a card.
    ElementBg = 2,
    /// The same element under the pointer.
    ElementHover = 3,
    /// The same element pressed, or selected.
    ElementActive = 4,
    /// A separator you are not supposed to notice: rules between rows.
    BorderSubtle = 5,
    /// The visible edge of a component; the resting focus ring.
    Border = 6,
    /// A border that is doing work: a selected row's edge, an active input.
    BorderStrong = 7,
    /// The hue at full strength, on an opaque fill. The brand step.
    Solid = 8,
    /// `Solid` under the pointer.
    SolidHover = 9,
    /// Text that must be legible but must not shout — AA on [`Step::AppBg`].
    TextLow = 10,
    /// Text that carries the sentence — AAA on [`Step::AppBg`].
    TextHigh = 11,
}

impl Step {
    /// Every step, in scale order. Used by the palette-coverage test, which
    /// asserts that no resolved role colour exists outside the palette.
    pub const ALL: [Step; 12] = [
        Step::AppBg,
        Step::SubtleBg,
        Step::ElementBg,
        Step::ElementHover,
        Step::ElementActive,
        Step::BorderSubtle,
        Step::Border,
        Step::BorderStrong,
        Step::Solid,
        Step::SolidHover,
        Step::TextLow,
        Step::TextHigh,
    ];
}

// ─────────────────────────────────────────────────────────────────────────────
// Ramp specification
// ─────────────────────────────────────────────────────────────────────────────

/// Whether a ramp is a grey or a colour.
///
/// # Why this is not just "chroma near zero"
///
/// It is tempting to say a neutral is a chromatic ramp with low chroma and use
/// one saturation curve for both. That produces the wrong greys. A chromatic
/// ramp wants its backgrounds *washed out* relative to its solid step (a 6 %
/// indigo tint behind an 82 % indigo chip), so its curve rises steeply toward
/// [`Step::Solid`]. A neutral wants the *opposite*: a nearly constant, very
/// small amount of hue at every step, because the tint is the theme's
/// temperature and it should not drift as surfaces stack. Running greys through
/// the chromatic curve makes `bg_base` and `border_default` disagree about how
/// warm the theme is, which reads as dirt.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RampKind {
    /// A grey. Hue is a constant faint temperature, not a colour.
    Neutral,
    /// A colour. Hue is the point.
    Chromatic,
}

/// The three numbers that define a ramp.
///
/// Everything else about the twelve steps is derived, identically for every
/// theme, by [`Ramp::generate`].
#[derive(Debug, Clone, Copy, PartialEq, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RampSpec {
    /// Hue in degrees, 0–360. Stored in degrees rather than GPUI's 0–1 turns
    /// because a theme file is written by a person and "243" is a hue while
    /// "0.675" is a number someone will get wrong.
    pub hue: f32,
    /// Peak saturation, 0–1, reached at [`Step::Solid`].
    pub chroma: f32,
    /// Lightness of [`Step::Solid`], 0–1 — the one step picked by eye.
    ///
    /// Omitted for a [`RampKind::Neutral`] ramp, whose solid step is a mid-grey
    /// derived from the appearance rather than chosen: see
    /// [`NEUTRAL_SOLID_L`]. Requiring it there would be asking the theme author
    /// for a number the generator then ignores.
    #[serde(default = "default_solid")]
    pub solid: f32,
    /// Grey or colour. See [`RampKind`].
    #[serde(default = "default_ramp_kind")]
    pub kind: RampKind,
}

fn default_ramp_kind() -> RampKind {
    RampKind::Chromatic
}

/// Placeholder for a neutral's unused `solid`. Never reaches a pixel — the
/// generator substitutes [`NEUTRAL_SOLID_L`] for neutral ramps.
fn default_solid() -> f32 {
    0.5
}

impl RampSpec {
    /// A chromatic ramp — the common case, so it is the short constructor.
    pub const fn colour(hue: f32, chroma: f32, solid: f32) -> Self {
        Self {
            hue,
            chroma,
            solid,
            kind: RampKind::Chromatic,
        }
    }

    /// A neutral ramp. `chroma` here is the theme's *temperature*: how much of
    /// `hue` is present in every grey. Values above ~0.15 stop being grey.
    pub const fn neutral(hue: f32, chroma: f32) -> Self {
        Self {
            hue,
            chroma,
            // A neutral's solid step is a mid-grey, not a brand colour, so it
            // is derived from the appearance rather than picked. The value is
            // overwritten in `generate`; it is present only so the struct has
            // one shape.
            solid: 0.5,
            kind: RampKind::Neutral,
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// The curves
// ─────────────────────────────────────────────────────────────────────────────

/// Lightness of steps 1–8 and 11–12 in a light appearance.
///
/// Steps 9 and 10 are holes: `Solid` comes from the spec and `SolidHover` is
/// derived from it. `f32::NAN` would be the honest marker but it propagates;
/// the generator never reads these two entries, and the test
/// `solid_steps_come_from_the_spec_not_the_curve` pins that.
const LIGHT_L: [f32; 12] = [
    0.970, // AppBg          — the page
    0.948, // SubtleBg
    0.928, // ElementBg
    0.906, // ElementHover
    0.880, // ElementActive
    0.855, // BorderSubtle
    0.815, // Border
    0.710, // BorderStrong
    0.000, // Solid          — from spec
    0.000, // SolidHover     — derived
    0.470, // TextLow        — AA on AppBg
    0.115, // TextHigh       — AAA on AppBg
];

/// Lightness of steps 1–8 and 11–12 in a dark appearance.
const DARK_L: [f32; 12] = [
    0.080, // AppBg
    0.108, // SubtleBg
    0.132, // ElementBg
    0.166, // ElementHover
    0.200, // ElementActive
    0.240, // BorderSubtle
    0.290, // Border
    0.380, // BorderStrong
    0.000, // Solid          — from spec
    0.000, // SolidHover     — derived
    0.600, // TextLow
    0.930, // TextHigh
];

/// Saturation of each step as a fraction of `RampSpec::chroma`, for a
/// chromatic ramp. Backgrounds carry a hint of the hue; the solid steps carry
/// all of it; text carries most of it so a link still reads as the accent.
const CHROMATIC_S: [f32; 12] = [
    0.16, 0.18, 0.22, 0.26, 0.30, 0.34, 0.40, 0.55, 1.00, 1.00, 0.95, 0.60,
];

/// Saturation of each step as a fraction of `RampSpec::chroma`, for a neutral.
/// Nearly flat on purpose — see [`RampKind`].
const NEUTRAL_S: [f32; 12] = [
    1.00, 0.95, 0.90, 0.85, 0.80, 0.75, 0.72, 0.80, 0.60, 0.60, 0.85, 0.90,
];

/// Lightness of a neutral ramp's [`Step::Solid`], per appearance.
///
/// A grey's solid step is the "filled but not coloured" fill — a disabled
/// button, a `Kbd` cap, a placeholder block. It has to sit clear of both the
/// borders below it and the text above it, which is a property of the
/// appearance and not of the theme.
const NEUTRAL_SOLID_L: [f32; 2] = [0.560 /* light */, 0.420 /* dark */];

/// How far [`Step::SolidHover`] moves from [`Step::Solid`], and in which
/// direction. Light appearances darken on hover, dark appearances lighten:
/// both directions are "more contrast against the page".
const SOLID_HOVER_DELTA: f32 = 0.055;

// ─────────────────────────────────────────────────────────────────────────────
// Ramp
// ─────────────────────────────────────────────────────────────────────────────

/// Twelve steps of one hue family.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Ramp {
    steps: [Hsla; 12],
}

impl Ramp {
    /// Build a ramp from its three numbers and an appearance.
    ///
    /// This is the only place in the program where a colour is *computed*.
    /// Everything downstream selects from what this produces.
    pub fn generate(spec: RampSpec, appearance: Appearance) -> Ramp {
        let (curve, neutral_solid) = match appearance {
            Appearance::Light => (&LIGHT_L, NEUTRAL_SOLID_L[0]),
            Appearance::Dark => (&DARK_L, NEUTRAL_SOLID_L[1]),
        };
        let sat_curve = match spec.kind {
            RampKind::Neutral => &NEUTRAL_S,
            RampKind::Chromatic => &CHROMATIC_S,
        };
        let solid_l = match spec.kind {
            RampKind::Neutral => neutral_solid,
            RampKind::Chromatic => spec.solid,
        };
        // Hover moves the solid step *away* from the page background, which is
        // down in light and up in dark.
        let hover_l = match appearance {
            Appearance::Light => (solid_l - SOLID_HOVER_DELTA).max(0.0),
            Appearance::Dark => (solid_l + SOLID_HOVER_DELTA).min(1.0),
        };

        let h = spec.hue / 360.0;
        let mut steps = [hsla(0.0, 0.0, 0.0, 1.0); 12];
        for (i, slot) in steps.iter_mut().enumerate() {
            let l = match i {
                8 => solid_l,
                9 => hover_l,
                _ => curve[i],
            };
            *slot = hsla(h, (spec.chroma * sat_curve[i]).clamp(0.0, 1.0), l, 1.0);
        }
        Ramp { steps }
    }

    /// The colour at `step`.
    #[inline]
    pub fn get(&self, step: Step) -> Hsla {
        self.steps[step as usize]
    }

    /// `get(step)` with a different alpha.
    ///
    /// Alpha belongs to the *use*, not to the step — the same border colour is
    /// opaque on a panel and translucent over a code block — so it is applied
    /// here rather than baked into the ramp. Callers pass a rung of
    /// [`crate::theme::tokens::AlphaTokens`], never a literal.
    #[inline]
    pub fn at(&self, step: Step, alpha: f32) -> Hsla {
        let mut c = self.get(step);
        c.a = alpha;
        c
    }

    /// Every step, for tests that need to prove a colour came from here.
    #[inline]
    pub fn steps(&self) -> &[Hsla; 12] {
        &self.steps
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Interpolation
// ─────────────────────────────────────────────────────────────────────────────

/// Blend two theme colours, `t` of the way from `from` to `to`.
///
/// # Why this lives in the palette
///
/// `motion/color.rs` animates a colour by springing each HSLA channel
/// independently and reassembling them with an `Hsla { .. }` struct literal.
/// That is the only place outside this module that ever *constructed* a colour,
/// and `tests/theme_law.rs` caught it — correctly. The literal was not a
/// design decision leaking into a view, but the rule does not have an exception
/// for "it is only interpolating", and carving one would mean the guard could
/// no longer answer "where do colours come from?" with a single module.
///
/// So the operation moves here rather than the guard relaxing. Anything that
/// needs a colour between two theme colours asks the palette for it.
///
/// # Hue takes the short way round
///
/// Interpolating hue linearly from 350° to 10° sweeps *backwards* through the
/// entire wheel — cyan, green, yellow — to travel 20°. This picks the shorter
/// arc, which is what "between these two colours" means to a reader watching
/// it happen.
pub fn mix(from: Hsla, to: Hsla, t: f32) -> Hsla {
    let t = t.clamp(0.0, 1.0);
    let lerp = |a: f32, b: f32| a + (b - a) * t;

    // Hue is on a circle: take whichever direction is shorter.
    let mut delta = to.h - from.h;
    if delta > 0.5 {
        delta -= 1.0;
    } else if delta < -0.5 {
        delta += 1.0;
    }

    hsla(
        (from.h + delta * t).rem_euclid(1.0),
        lerp(from.s, to.s).clamp(0.0, 1.0),
        lerp(from.l, to.l).clamp(0.0, 1.0),
        lerp(from.a, to.a).clamp(0.0, 1.0),
    )
}

/// Assemble a colour from four already-animated channels.
///
/// The escape hatch for [`crate::motion::color::MotionColor`], which springs
/// each channel separately and therefore has four numbers rather than two
/// colours and a `t`. Named `from_channels` rather than exposing `hsla` so that
/// a grep for the constructor still lands here, in the palette, with this
/// comment attached — and so a caller who could have used [`mix`] is nudged
/// toward it.
pub fn from_channels(h: f32, s: f32, l: f32, a: f32) -> Hsla {
    hsla(
        h.rem_euclid(1.0),
        s.clamp(0.0, 1.0),
        l.clamp(0.0, 1.0),
        a.clamp(0.0, 1.0),
    )
}

// ─────────────────────────────────────────────────────────────────────────────
// Surfaces
// ─────────────────────────────────────────────────────────────────────────────

/// The stack of background planes, as lightness values on the neutral hue.
///
/// # Why these are not ramp steps
///
/// Because "further from the page" and "more contrast with the page" are the
/// same direction in a dark theme and *opposite* directions in a light one. A
/// light-theme sidebar recedes by going grey while a light-theme popover
/// advances by going white; both are one step of elevation, in opposite
/// directions on the ramp. Forcing them onto one monotonic scale is how you end
/// up with a popover that looks sunken.
///
/// So the elevation stack is four numbers a theme states outright. They are
/// still data, they are still on the neutral hue, and there are four of them
/// rather than ninety.
#[derive(Debug, Clone, Copy, PartialEq, serde::Deserialize)]
pub struct SurfaceSpec {
    /// Below the page: the inside of a well, the bottom dock's body.
    pub sunken: f32,
    /// The page. Should agree with `neutral[AppBg]` unless a theme has a reason.
    pub base: f32,
    /// Chrome and cards — docks, toolbars, the status bar, a raised panel.
    pub raised: f32,
    /// Popovers, dialogs, the command palette, the search overlay.
    ///
    /// **Opaque by contract.** A near-opaque overlay is worse than either an
    /// opaque or a properly translucent one: at α 0.98 the pane behind bleeds
    /// through at 2 %, which is not a material, it is illegible grey text in
    /// the middle of an empty panel. That was a real, shipped defect —
    /// visible in `tests/shots/fixtures/04-search-hits.png`, where the pane's
    /// "Open a symbol to get started" empty state prints faintly through the
    /// search overlay. `Palette::surface` forces α = 1.0 for exactly this
    /// reason and [`crate::theme::palette::tests`] pins it.
    pub overlay: f32,
}

/// The elevation planes, resolved.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Surfaces {
    /// Below the page.
    pub sunken: Hsla,
    /// The page.
    pub base: Hsla,
    /// Chrome and cards.
    pub raised: Hsla,
    /// Popovers and dialogs. Always opaque.
    pub overlay: Hsla,
}

// ─────────────────────────────────────────────────────────────────────────────
// Palette
// ─────────────────────────────────────────────────────────────────────────────

/// Hues for the thirteen symbol kinds, plus the plane they all sit on.
///
/// # Why one plane
///
/// Kind badges have to be told apart *preattentively*, in a dense column, by a
/// reader who is looking for something else. That works when hue varies and
/// luminance does not: vary luminance too and the badges acquire a value
/// hierarchy, so some kinds look important and others look disabled, which is a
/// claim the data does not support. Fixing `s` and `l` for all thirteen and
/// varying only `h` is what makes them scan as thirteen labels rather than as a
/// ranking — and it is also what keeps them distinguishable in greyscale, since
/// in greyscale they collapse to one shade instead of to a false ordering.
#[derive(Debug, Clone, Copy, PartialEq, serde::Deserialize)]
pub struct KindSpec {
    /// The thirteen hues in degrees, in `LocalKindDiscriminant` order.
    pub hues: [f32; 13],
    /// The saturation every kind colour shares.
    pub chroma: f32,
    /// The lightness every kind colour shares.
    pub lightness: f32,
}

/// A resolved palette: everything a theme has to choose, already computed.
///
/// This is carried on [`crate::theme::NudoxThemeExt`] so that a test can assert
/// the property the whole restructure exists to guarantee — that every resolved
/// role colour is a colour this palette produced, and therefore that no literal
/// survived anywhere in the theme layer.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Palette {
    /// Which way the scale runs.
    pub appearance: Appearance,
    /// Greys: surfaces, borders, body text.
    pub neutral: Ramp,
    /// The interactive hue: links, focus, selection, keywords.
    pub accent: Ramp,
    /// Success.
    pub ok: Ramp,
    /// Warning.
    pub warn: Ramp,
    /// Error.
    pub danger: Ramp,
    /// Information.
    pub info: Ramp,
    /// Provenance chrome, one ramp per trust level (LD-8).
    pub trust_local: Ramp,
    /// See [`Palette::trust_local`].
    pub trust_synced: Ramp,
    /// See [`Palette::trust_local`].
    pub trust_remote: Ramp,
    /// See [`Palette::trust_local`].
    pub trust_stale: Ramp,
    /// The thirteen kind colours, already on their shared plane.
    pub kinds: [Hsla; 13],
    /// Text that sits on top of a [`Step::Solid`] fill of any chromatic ramp.
    ///
    /// One value rather than one per ramp, because all the solid steps share a
    /// lightness band by construction, so the answer is the same for all of
    /// them — and a per-badge conditional is a per-badge chance to be wrong.
    pub on_solid: Hsla,
    /// The elevation planes.
    pub surfaces: Surfaces,
}

impl Palette {
    /// Resolve a palette from the ramp specs a theme states.
    pub fn generate(
        appearance: Appearance,
        neutral: RampSpec,
        accent: RampSpec,
        ok: RampSpec,
        warn: RampSpec,
        danger: RampSpec,
        info: RampSpec,
        trust: [RampSpec; 4],
        kinds: KindSpec,
        surfaces: SurfaceSpec,
    ) -> Palette {
        let g = |s: RampSpec| Ramp::generate(s, appearance);
        let neutral_ramp = g(neutral);

        // Text on a solid fill. The solid steps of every chromatic ramp sit in
        // one lightness band per appearance, so this is decidable once: a light
        // theme's solids are dark enough to need white on them, a dark theme's
        // are bright enough to need near-black.
        let on_solid = match appearance {
            Appearance::Light => hsla(0.0, 0.0, 1.0, 1.0),
            Appearance::Dark => hsla(0.0, 0.0, 0.05, 1.0),
        };

        let kind_h = |deg: f32| hsla(deg / 360.0, kinds.chroma, kinds.lightness, 1.0);
        let mut kind_colours = [hsla(0.0, 0.0, 0.0, 1.0); 13];
        for (slot, deg) in kind_colours.iter_mut().zip(kinds.hues.iter()) {
            *slot = kind_h(*deg);
        }

        // Surfaces ride the neutral hue at the neutral's own temperature, so
        // they cannot drift from the borders and text that sit on them.
        let neutral_h = neutral.hue / 360.0;
        let neutral_s = neutral.chroma;
        let surface = |l: f32, a: f32| hsla(neutral_h, neutral_s, l, a);

        Palette {
            appearance,
            neutral: neutral_ramp,
            accent: g(accent),
            ok: g(ok),
            warn: g(warn),
            danger: g(danger),
            info: g(info),
            trust_local: g(trust[0]),
            trust_synced: g(trust[1]),
            trust_remote: g(trust[2]),
            trust_stale: g(trust[3]),
            kinds: kind_colours,
            on_solid,
            surfaces: Surfaces {
                sunken: surface(surfaces.sunken, 1.0),
                base: surface(surfaces.base, 1.0),
                raised: surface(surfaces.raised, 1.0),
                // Opaque by contract — see `SurfaceSpec::overlay`.
                overlay: surface(surfaces.overlay, 1.0),
            },
        }
    }

    /// Every colour this palette can produce, for the coverage test.
    ///
    /// A resolved role colour whose RGB is not in here came from somewhere
    /// else, which is the thing this whole module exists to make impossible.
    pub fn all_colours(&self) -> Vec<Hsla> {
        let mut out = Vec::with_capacity(12 * 10 + 13 + 5);
        for ramp in [
            &self.neutral,
            &self.accent,
            &self.ok,
            &self.warn,
            &self.danger,
            &self.info,
            &self.trust_local,
            &self.trust_synced,
            &self.trust_remote,
            &self.trust_stale,
        ] {
            out.extend_from_slice(ramp.steps());
        }
        out.extend_from_slice(&self.kinds);
        out.push(self.on_solid);
        out.push(self.surfaces.sunken);
        out.push(self.surfaces.base);
        out.push(self.surfaces.raised);
        out.push(self.surfaces.overlay);
        out
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Tests
// ─────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn accent_spec() -> RampSpec {
        RampSpec::colour(243.0, 0.82, 0.70)
    }

    /// The solid step must be the value the theme asked for, not a value the
    /// curve supplied. If this ever regresses, every theme's brand colour
    /// silently becomes the same colour.
    #[test]
    fn solid_steps_come_from_the_spec_not_the_curve() {
        let ramp = Ramp::generate(accent_spec(), Appearance::Dark);
        let solid = ramp.get(Step::Solid);
        assert!(
            (solid.l - 0.70).abs() < 1e-6,
            "Step::Solid lightness {} did not come from the spec's 0.70",
            solid.l
        );
        assert!(
            (solid.s - 0.82).abs() < 1e-6,
            "Step::Solid saturation {} is not the spec's peak chroma",
            solid.s
        );
    }

    /// Hover must move away from the page, in whichever direction that is.
    #[test]
    fn solid_hover_moves_away_from_the_page_in_both_appearances() {
        let dark = Ramp::generate(accent_spec(), Appearance::Dark);
        assert!(
            dark.get(Step::SolidHover).l > dark.get(Step::Solid).l,
            "in a dark appearance the hovered solid must be lighter"
        );
        let light = Ramp::generate(RampSpec::colour(243.0, 0.80, 0.48), Appearance::Light);
        assert!(
            light.get(Step::SolidHover).l < light.get(Step::Solid).l,
            "in a light appearance the hovered solid must be darker"
        );
    }

    /// Backgrounds must march monotonically away from the page so that stacking
    /// two surfaces always produces a visible edge.
    #[test]
    fn background_steps_are_monotonic_in_both_appearances() {
        for appearance in [Appearance::Light, Appearance::Dark] {
            let ramp = Ramp::generate(RampSpec::neutral(240.0, 0.11), appearance);
            let bg = [
                Step::AppBg,
                Step::SubtleBg,
                Step::ElementBg,
                Step::ElementHover,
                Step::ElementActive,
                Step::BorderSubtle,
                Step::Border,
                Step::BorderStrong,
            ];
            for pair in bg.windows(2) {
                let (a, b) = (ramp.get(pair[0]), ramp.get(pair[1]));
                match appearance {
                    Appearance::Light => assert!(
                        b.l < a.l,
                        "{appearance:?}: {:?} ({}) must be darker than {:?} ({})",
                        pair[1],
                        b.l,
                        pair[0],
                        a.l
                    ),
                    Appearance::Dark => assert!(
                        b.l > a.l,
                        "{appearance:?}: {:?} ({}) must be lighter than {:?} ({})",
                        pair[1],
                        b.l,
                        pair[0],
                        a.l
                    ),
                }
            }
        }
    }

    /// A neutral must stay grey. The `RampKind` split exists precisely so a
    /// neutral does not inherit the chromatic curve's climb toward `Solid`.
    #[test]
    fn a_neutral_ramp_never_becomes_a_colour() {
        for appearance in [Appearance::Light, Appearance::Dark] {
            let ramp = Ramp::generate(RampSpec::neutral(240.0, 0.12), appearance);
            for step in Step::ALL {
                let s = ramp.get(step).s;
                assert!(
                    s <= 0.15,
                    "{appearance:?} neutral {step:?} has saturation {s}, which is a colour"
                );
            }
        }
    }

    /// Text steps must clear WCAG AA against the page.
    ///
    /// `TextLow` is the floor the design leans on hardest — it is what body
    /// metadata, counts and paths are set in — so it is the one worth pinning.
    #[test]
    fn text_steps_clear_wcag_aa_against_the_app_background() {
        for appearance in [Appearance::Light, Appearance::Dark] {
            let ramp = Ramp::generate(RampSpec::neutral(240.0, 0.11), appearance);
            let bg = ramp.get(Step::AppBg);
            for (step, floor) in [(Step::TextLow, 4.5_f32), (Step::TextHigh, 7.0)] {
                let ratio = contrast_ratio(ramp.get(step), bg);
                assert!(
                    ratio >= floor,
                    "{appearance:?} {step:?} contrast {ratio:.2}:1 is below {floor}:1"
                );
            }
        }
    }

    /// An overlay surface is opaque, always. This is the ghost-text defect,
    /// pinned so it cannot come back as "just 2 % of translucency".
    #[test]
    fn overlay_surfaces_are_opaque() {
        for appearance in [Appearance::Light, Appearance::Dark] {
            let p = tiny_palette(appearance);
            assert_eq!(
                p.surfaces.overlay.a, 1.0,
                "{appearance:?} overlay surface must be fully opaque: anything less \
                 lets the view behind print through as illegible mud"
            );
        }
    }

    /// The kinds share one plane, so they scan as labels and not as a ranking.
    #[test]
    fn kind_colours_share_one_luminance_plane() {
        let p = tiny_palette(Appearance::Dark);
        let first = p.kinds[0];
        for (i, c) in p.kinds.iter().enumerate() {
            assert!(
                (c.l - first.l).abs() < 1e-6 && (c.s - first.s).abs() < 1e-6,
                "kind {i} left the shared plane: l={} s={}",
                c.l,
                c.s
            );
        }
    }

    // ── helpers ──────────────────────────────────────────────────────────────

    fn tiny_palette(appearance: Appearance) -> Palette {
        Palette::generate(
            appearance,
            RampSpec::neutral(240.0, 0.11),
            RampSpec::colour(243.0, 0.82, 0.70),
            RampSpec::colour(142.0, 0.68, 0.54),
            RampSpec::colour(38.0, 0.92, 0.62),
            RampSpec::colour(0.0, 0.82, 0.62),
            RampSpec::colour(211.0, 0.84, 0.62),
            [
                RampSpec::colour(148.0, 0.65, 0.50),
                RampSpec::colour(205.0, 0.80, 0.58),
                RampSpec::colour(38.0, 0.90, 0.62),
                RampSpec::colour(240.0, 0.06, 0.44),
            ],
            KindSpec {
                hues: [
                    192.0, 206.0, 220.0, 158.0, 48.0, 276.0, 290.0, 26.0, 14.0, 318.0, 338.0,
                    168.0, 238.0,
                ],
                chroma: 0.76,
                lightness: 0.62,
            },
            SurfaceSpec {
                sunken: 0.05,
                base: 0.08,
                raised: 0.13,
                overlay: 0.17,
            },
        )
    }

    /// WCAG 2.1 relative luminance, done properly (sRGB linearisation), because
    /// the `l²` approximation the old theme docs used is wrong by enough to
    /// pass a colour that fails.
    fn relative_luminance(c: Hsla) -> f32 {
        let rgba = gpui::Rgba::from(c);
        let lin = |u: f32| {
            if u <= 0.03928 {
                u / 12.92
            } else {
                ((u + 0.055) / 1.055).powf(2.4)
            }
        };
        0.2126 * lin(rgba.r) + 0.7152 * lin(rgba.g) + 0.0722 * lin(rgba.b)
    }

    fn contrast_ratio(a: Hsla, b: Hsla) -> f32 {
        let (la, lb) = (relative_luminance(a), relative_luminance(b));
        let (hi, lo) = if la > lb { (la, lb) } else { (lb, la) };
        (hi + 0.05) / (lo + 0.05)
    }
}
