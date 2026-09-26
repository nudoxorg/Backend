//! The key cap — the only one in FACET. A cut stone (`geo::CUT_STONE` at
//! the small end, the chamfer growing with the cap) in three voices:
//!
//! - **plain**: `plate2` with the resting bevel, `ink2` — the status footer,
//!   menus, a peek's key feet;
//! - **hot**: mint, `mint_ink` — a control's own key, risen while ⌘ is held;
//! - **hint**: `peri_hi`, mono 700 uppercase — hint mode's letters.
//!
//! [`keys`] sets a chord (`⌘ K`) as one group. [`KeyRise`] is how a cap
//! appears on a control while ⌘ is held: it rises a few px and fades in,
//! staggered by where it sits on the screen (a wave from the left edge),
//! painted over the control so nothing ever shifts.

use super::state::track;
use crate::Set;
use crate::measure::{Measure, Space};
use crate::motion::{Motion, Spec};
use crate::paint::{Bevel, Chamfer, Edge, Plate, cut};
use crate::theme::ActiveFacet;
use crate::tokens::motion::{BOUNCE, DROP, MICRO, QUICK};
use crate::tokens::{Face, TypeRole, geo};
use gpui::{
    AnyElement, App, Bounds, Div, Element, ElementId, GlobalElementId, Hsla, InspectorElementId,
    IntoElement, LayoutId, ParentElement, Pixels, RenderOnce, SharedString, Styled, Window, div,
    point, px,
};
use std::time::Duration;

/// Which voice a key cap speaks in.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Hash)]
pub enum KbdVoice {
    /// The resting cap: `plate2`, `ink2`.
    #[default]
    Plain,
    /// Mint: a control's own key, shown while ⌘ is held.
    Hot,
    /// Periwinkle, uppercase mono: a hint-mode label.
    Hint,
    /// A hairline outline and `ink3` mono, no plate: a key named in a
    /// sentence ("… `esc`").
    Quiet,
}

/// Cap sizes (edge length at 100 % text, comfortable density).
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Hash)]
pub enum KbdSize {
    /// 16 px: inside menu rows and dense footers.
    Small,
    /// 18 px: the default (`.kbd`).
    #[default]
    Medium,
    /// 26 px: keyboard legends.
    Large,
}

impl KbdSize {
    const fn base(self) -> f32 {
        match self {
            Self::Small => 16.0,
            Self::Medium => 18.0,
            Self::Large => 26.0,
        }
    }

    const fn role(self, voice: KbdVoice) -> TypeRole {
        let size = match self {
            Self::Small => 9.5,
            Self::Medium => 10.5,
            Self::Large => 12.5,
        };
        match voice {
            KbdVoice::Quiet => TypeRole {
                face: Face::Mono,
                weight: 500.0,
                size,
                line: size * 1.2,
                tracking: 0.0,
                italic: false,
            },
            KbdVoice::Hint => TypeRole {
                face: Face::Mono,
                weight: 700.0,
                size,
                line: size * 1.2,
                tracking: 0.04,
                italic: false,
            },
            KbdVoice::Plain | KbdVoice::Hot => TypeRole {
                face: Face::Ui,
                weight: 600.0,
                size,
                line: size * 1.2,
                tracking: 0.0,
                italic: false,
            },
        }
    }
}

/// One key cap: `kbd("K", &measure)`, `kbd("⌘", &measure).hot()`.
#[derive(IntoElement)]
pub struct Kbd {
    label: SharedString,
    voice: KbdVoice,
    size: KbdSize,
    measure: Measure,
}

/// A plain, medium key cap reading `label`, sized for `measure`.
#[must_use]
pub fn kbd(label: impl Into<SharedString>, measure: &Measure) -> Kbd {
    Kbd {
        label: label.into(),
        voice: KbdVoice::Plain,
        size: KbdSize::Medium,
        measure: *measure,
    }
}

impl Kbd {
    /// The voice.
    #[must_use]
    pub const fn voice(mut self, voice: KbdVoice) -> Self {
        self.voice = voice;
        self
    }

    /// Mint: this control's key while ⌘ is held.
    #[must_use]
    pub const fn hot(self) -> Self {
        self.voice(KbdVoice::Hot)
    }

    /// Periwinkle uppercase: a hint label.
    #[must_use]
    pub const fn hint(self) -> Self {
        self.voice(KbdVoice::Hint)
    }

    /// The cap size.
    #[must_use]
    pub const fn size(mut self, size: KbdSize) -> Self {
        self.size = size;
        self
    }

    /// The cap's height for its measure (it is at least this wide).
    #[must_use]
    pub fn side(&self) -> Pixels {
        self.measure.icon(self.size.base())
    }
}

/// `(fill, ink, edge)` for a voice.
fn tones(voice: KbdVoice, cx: &App) -> (Hsla, Hsla, Edge) {
    let palette = cx.palette();
    let rest = Edge::of(Bevel::Rest, palette);
    match voice {
        KbdVoice::Plain => (palette.plate2.into(), palette.ink2.into(), rest),
        KbdVoice::Hot => (palette.mint.base.into(), palette.mint_ink.into(), rest),
        KbdVoice::Hint => (palette.peri_hi.into(), palette.table.into(), rest),
        KbdVoice::Quiet => {
            let line: Hsla = palette.line2.into();
            (super::clear(palette.plate.into()), palette.ink3.into(), Edge { hi: line, lo: line, ..rest })
        }
    }
}

impl RenderOnce for Kbd {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let (fill, ink, edge) = tones(self.voice, cx);
        let side = self.side();
        let label = if self.voice == KbdVoice::Hint {
            SharedString::from(self.label.to_uppercase())
        } else {
            self.label
        };
        // The stone's chamfer grows with the cap: 3 px at 18, 5 px at 26.
        let chamfer = (f32::from(side) * 0.19).max(f32::from(geo::CUT_STONE) * self.measure.scale());
        cut()
            .chamfer(Chamfer::Px(chamfer))
            .edge(edge)
            .plate(Plate::Flat)
            .fill(fill)
            .flex()
            .flex_none()
            .items_center()
            .justify_center()
            .h(side)
            .min_w(side)
            .px(self.measure.space(Space::Tight))
            .child(
                div()
                    .set(self.size.role(self.voice), &self.measure)
                    .text_color(ink)
                    .whitespace_nowrap()
                    .child(label),
            )
    }
}

/// A chord (`⌘ K`, `⌥ →`) as one group of caps.
#[must_use]
pub fn keys(chord: &[&str], voice: KbdVoice, measure: &Measure) -> Div {
    div()
        .flex()
        .flex_none()
        .items_center()
        .gap(measure.space(Space::Hair).max(px(2.0)))
        .children(
            chord
                .iter()
                .map(|key| kbd(SharedString::from((*key).to_owned()), measure).voice(voice)),
        )
}

/// The window x (effective px) over which the ⌘ wave travels.
const WAVE_SPAN: f32 = 1400.0;
/// The latest a cap starts rising after ⌘ goes down.
const WAVE_DELAY: f32 = 140.0;

/// A cap (or anything) that rises into place while `shown`, staggered by
/// its x on the screen, and never takes part in layout: position it
/// absolutely over its control.
pub struct KeyRise {
    child: AnyElement,
    motion: Motion,
    key: ElementId,
    shown: bool,
    travel: f32,
    t: f32,
}

/// Wraps `child` so it rises while `shown`, animated in `motion` under
/// `id`'s `key` track.
pub(crate) fn key_rise(
    child: impl IntoElement,
    motion: &Motion,
    id: &ElementId,
    shown: bool,
    measure: &Measure,
) -> KeyRise {
    KeyRise {
        child: child.into_any_element(),
        motion: motion.clone(),
        key: track(id, "key"),
        shown,
        travel: 5.0 * measure.scale(),
        t: 0.0,
    }
}

impl IntoElement for KeyRise {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

impl Element for KeyRise {
    type RequestLayoutState = ();
    type PrepaintState = ();

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, ()) {
        (self.child.request_layout(window, cx), ())
    }

    fn prepaint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _request_layout: &mut (),
        window: &mut Window,
        cx: &mut App,
    ) {
        let spec = if self.shown {
            let x = (f32::from(bounds.origin.x) / cx.facet().text_scale / WAVE_SPAN).clamp(0.0, 1.0);
            #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
            let hold = Duration::from_millis((x * WAVE_DELAY) as u64);
            Spec::tween(QUICK, BOUNCE).delayed(hold)
        } else {
            Spec::tween(MICRO, DROP)
        };
        self.t = self.motion.animate(
            self.key.clone(),
            if self.shown { 1.0 } else { 0.0 },
            spec,
            window,
            cx,
        );
        if self.t <= 0.001 {
            return;
        }
        let (child, t, travel) = (&mut self.child, self.t, self.travel);
        window.with_element_offset(point(px(0.0), px(travel * (1.0 - t))), |window| {
            window.with_element_opacity(Some(t.clamp(0.0, 1.0)), |window| child.prepaint(window, cx));
        });
    }

    fn paint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        _bounds: Bounds<Pixels>,
        _request_layout: &mut (),
        _prepaint: &mut (),
        window: &mut Window,
        cx: &mut App,
    ) {
        if self.t <= 0.001 {
            return;
        }
        let (child, t) = (&mut self.child, self.t.clamp(0.0, 1.0));
        window.with_element_opacity(Some(t), |window| child.paint(window, cx));
    }
}

/// The hot cap a control shows while ⌘ is held, placed over its top edge
/// at the right (so it reads as the control's own key and shifts nothing).
pub(crate) fn key_badge(
    key: &SharedString,
    motion: &Motion,
    id: &ElementId,
    measure: &Measure,
) -> AnyElement {
    key_badge_at(key, motion, id, measure, true)
}

/// [`key_badge`], shown only while `visible` (a control whose room hides it).
pub(crate) fn key_badge_at(
    key: &SharedString,
    motion: &Motion,
    id: &ElementId,
    measure: &Measure,
    visible: bool,
) -> AnyElement {
    let shown = visible && measure.reveal().keys;
    let cap = kbd(key.clone(), measure).hot().size(KbdSize::Small);
    let side = cap.side();
    div()
        .absolute()
        .top(-side * 0.55)
        .right(-side * 0.3)
        .child(key_rise(cap, motion, id, shown, measure))
        .into_any_element()
}
