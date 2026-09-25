//! The titlebar: the window's trail.
//!
//! Left to right: the traffic lights' inset, the shelf toggle, the **bead
//! thread** — where you came from (older beads smaller and fainter, joined by
//! strands, the last one tied to "here" by a mint strand), the **here
//! capsule** (kind mark, name, path; on Orbit the capsule is the ask field),
//! the forward half (a dashed strand, hollow beads) — and the trail and inbox
//! buttons.
//!
//! It degrades by the effective width of the window (width ÷ text scale),
//! never by popping: below 1100 the oldest bead leaves, below 760 every bead
//! leaves and the capsule grows to fill the thread, below 520 the buttons
//! leave. Every piece arrives and leaves through `motion::Presence` (its slot
//! opens or closes while it fades), and the capsule's growth is a flex
//! factor on a spring, so a window dragged across a breakpoint flows.
//!
//! Descending adds a bead: it pops in just before the capsule, the older
//! beads each shrink a size and fade a step, and the thread makes room.

use crate::Set;
use crate::controls::{IconButtonSize, icon_button};
use crate::icons::{self, Icon, IconSize, Kind, Stroke, variant_path};
use crate::measure::Measure;
use crate::motion::presence::{Act, Axis, Extent};
use crate::motion::{Keys, Motion, Pose, Presence, spec};
use crate::overlay::tooltip::Tipped;
use crate::paint::geom::{Fill, Poly, pt};
use crate::paint::{Bevel, Chamfer, Edge, Plate, cut, mix};
use crate::theme::ActiveFacet;
use crate::tokens::motion::{GLIDE, STD};
use crate::tokens::{Face, Palette, TypeRole, geo};
use gpui::{
    AnyElement, App, ElementId, Hsla, InteractiveElement, IntoElement, ParentElement, Pixels,
    RenderOnce, SharedString, StatefulInteractiveElement, Styled, Window, canvas, div, layer, px,
    svg,
};
use std::rc::Rc;

/// The window's traffic lights.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Lights {
    /// The platform draws them: leave this much room (logical px, not
    /// scaled with text).
    Inset(Pixels),
    /// Paint stand-ins (galleries and captures, where no platform draws).
    StandIn,
    /// No lights (full screen, Windows/Linux).
    None,
}

/// One bead of the thread: a page you were on (or can go forward to).
#[derive(Clone, Debug, PartialEq)]
pub struct Bead {
    /// The page's stable identity.
    pub key: ElementId,
    /// Its kind (the bead's hue is the kind's family).
    pub kind: Kind,
    /// Its name (the bead's tooltip).
    pub name: SharedString,
}

/// What the capsule holds.
#[derive(Clone, Debug, PartialEq)]
pub enum Here {
    /// The page you are on.
    Page {
        /// Its kind mark.
        kind: Kind,
        /// Its name, in mono.
        name: SharedString,
        /// Where it lives (`present › glyph`).
        path: SharedString,
    },
    /// A page that is not a symbol (Settings): an icon, a title, a subtitle.
    Place {
        /// The icon.
        icon: Icon,
        /// The title.
        name: SharedString,
        /// The section.
        path: SharedString,
    },
    /// Orbit: the capsule is the ask field.
    Ask {
        /// The placeholder.
        prompt: SharedString,
    },
}

/// A button at the right end.
#[derive(Clone, Debug, PartialEq)]
pub struct TitleButton {
    /// Its id (and the key its click reports).
    pub id: ElementId,
    /// The icon.
    pub icon: Icon,
    /// Its name (tooltip).
    pub label: SharedString,
    /// Its key, shown while ⌘ is held.
    pub key: Option<SharedString>,
    /// Lit (mint): there is something new.
    pub on: bool,
}

/// Everything the titlebar shows.
#[derive(Clone, Debug, PartialEq)]
pub struct TitlebarData {
    /// The traffic lights.
    pub lights: Lights,
    /// The shelf toggle's state (`None`: no toggle).
    pub shelf_open: Option<bool>,
    /// Where you came from, oldest first.
    pub behind: Vec<Bead>,
    /// The capsule.
    pub here: Here,
    /// Where you can go forward to, nearest first.
    pub ahead: Vec<Bead>,
    /// The buttons at the right (trail, inbox).
    pub buttons: Vec<TitleButton>,
}

type OnKey = Rc<dyn Fn(&ElementId, &mut Window, &mut App)>;
type OnClick = Rc<dyn Fn(&mut Window, &mut App)>;

/// The titlebar for a window whose width `measure` measures.
#[derive(IntoElement)]
pub struct Titlebar {
    id: ElementId,
    data: TitlebarData,
    measure: Measure,
    on_shelf: Option<OnClick>,
    on_here: Option<OnClick>,
    on_bead: Option<OnKey>,
    on_button: Option<OnKey>,
}

/// The titlebar showing `data`, sized for the window's `measure`.
#[must_use]
pub fn titlebar(id: impl Into<ElementId>, data: TitlebarData, measure: &Measure) -> Titlebar {
    Titlebar {
        id: id.into(),
        data,
        measure: *measure,
        on_shelf: None,
        on_here: None,
        on_bead: None,
        on_button: None,
    }
}

impl Titlebar {
    /// The shelf toggle was clicked (or ⌘\).
    #[must_use]
    pub fn on_shelf(mut self, handler: impl Fn(&mut Window, &mut App) + 'static) -> Self {
        self.on_shelf = Some(Rc::new(handler));
        self
    }

    /// The capsule was clicked (ask, or jump).
    #[must_use]
    pub fn on_here(mut self, handler: impl Fn(&mut Window, &mut App) + 'static) -> Self {
        self.on_here = Some(Rc::new(handler));
        self
    }

    /// A bead was clicked: walk the thread to its page.
    #[must_use]
    pub fn on_bead(mut self, handler: impl Fn(&ElementId, &mut Window, &mut App) + 'static) -> Self {
        self.on_bead = Some(Rc::new(handler));
        self
    }

    /// A right-hand button was clicked.
    #[must_use]
    pub fn on_button(
        mut self,
        handler: impl Fn(&ElementId, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.on_button = Some(Rc::new(handler));
        self
    }
}

/// Which pieces a titlebar shows at an effective width (the breakpoints of
/// the flow targets).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Plan {
    /// How many beads behind "here" are drawn.
    pub behind: usize,
    /// Whether the forward half is drawn.
    pub ahead: bool,
    /// Whether the capsule fills the thread (no beads).
    pub fill: bool,
    /// Whether the shelf toggle and the right-hand buttons are drawn.
    pub buttons: bool,
}

impl Plan {
    /// The plan for an effective window width. The breakpoints are the
    /// flow targets' container queries (`max-width` is inclusive): at 1100
    /// the oldest bead is already gone, at 760 every bead, at 520 the
    /// buttons.
    #[must_use]
    pub fn at(effective: f32) -> Self {
        let beads = effective > 760.0;
        Self {
            behind: if effective > 1100.0 {
                3
            } else if beads {
                2
            } else {
                0
            },
            ahead: beads,
            fill: !beads,
            buttons: effective > 520.0,
        }
    }
}

const NAME: TypeRole = TypeRole {
    face: Face::Mono,
    weight: 650.0,
    size: 12.5,
    line: 16.0,
    tracking: 0.0,
    italic: false,
};
const PATH: TypeRole = TypeRole {
    face: Face::Ui,
    weight: 400.0,
    size: 11.5,
    line: 16.0,
    tracking: 0.0,
    italic: false,
};
const PROMPT: TypeRole = TypeRole {
    face: Face::Ui,
    weight: 400.0,
    size: 13.0,
    line: 16.0,
    tracking: 0.0,
    italic: false,
};

/// A piece's way in and out of the titlebar: the slot opens as it fades in
/// (and closes after it fades out), with no vertical travel.
static IN_POSE: [(f32, Pose); 2] = [
    (
        0.0,
        Pose {
            x: 0.0,
            y: 0.0,
            sx: 0.6,
            sy: 0.6,
            rotate: 0.0,
            opacity: 0.0,
        },
    ),
    (1.0, Pose::REST),
];
static IN_ROOM: [(f32, Extent); 3] = [
    (0.0, Extent::NONE),
    (0.55, Extent::FULL),
    (1.0, Extent::FULL),
];
static OUT_POSE: [(f32, Pose); 3] = [
    (0.0, Pose::REST),
    (
        0.45,
        Pose {
            x: 0.0,
            y: 0.0,
            sx: 0.6,
            sy: 0.6,
            rotate: 0.0,
            opacity: 0.0,
        },
    ),
    (
        1.0,
        Pose {
            x: 0.0,
            y: 0.0,
            sx: 0.6,
            sy: 0.6,
            rotate: 0.0,
            opacity: 0.0,
        },
    ),
];
static OUT_ROOM: [(f32, Extent); 3] = [
    (0.0, Extent::FULL),
    (0.35, Extent::FULL),
    (1.0, Extent::NONE),
];
const ARRIVE: Act = Act {
    duration: STD,
    pose: Keys::new(STD, &IN_POSE, GLIDE),
    room: Keys::new(STD, &IN_ROOM, GLIDE),
};
const DEPART: Act = Act {
    duration: STD,
    pose: Keys::new(STD, &OUT_POSE, GLIDE),
    room: Keys::new(STD, &OUT_ROOM, GLIDE),
};

fn presence(id: &ElementId, part: &str, cx: &mut App) -> Presence {
    Presence::scoped(format!("{id}.{part}"), cx)
        .axis(Axis::Horizontal)
        .enter(ARRIVE)
        .exit(DEPART)
}

/// A bead's size and strength by how far behind "here" it is.
fn bead_look(age: usize) -> (f32, f32) {
    match age {
        0 | 1 | 2 => (9.0, 0.82),
        _ => (7.0, 0.55),
    }
}

/// A painted bead: a diamond of half-diagonal `r`, filled or hollow.
fn bead_art(color: Hsla, r: f32, hollow: bool, box_px: f32, s: f32) -> AnyElement {
    canvas(
        |_, _, _| {},
        move |bounds, (), window, _| {
            let c = bounds.center();
            let (cx, cy) = (f32::from(c.x), f32::from(c.y));
            let poly = Poly::new([pt(cx, cy - r), pt(cx + r, cy), pt(cx, cy + r), pt(cx - r, cy)]);
            let mut fill = Fill::new();
            if hollow {
                for piece in poly.offset(-0.75 * s).stroke_ring(1.5 * s) {
                    fill.poly(&piece);
                }
            } else {
                fill.poly(&poly);
            }
            fill.paint(window, color);
        },
    )
    .flex_none()
    .size(px(box_px))
    .into_any_element()
}

/// A strand: a short line, solid or dashed.
fn strand(color: Hsla, width: f32, dashed: bool, s: f32) -> AnyElement {
    canvas(
        |_, _, _| {},
        move |bounds, (), window, _| {
            let y = f32::from(bounds.center().y) - 0.75 * s;
            let x0 = f32::from(bounds.origin.x);
            let w = f32::from(bounds.size.width);
            let mut fill = Fill::new();
            if dashed {
                let mut x = x0;
                while x < x0 + w {
                    fill.poly(&Poly::rect(x, y, (3.0 * s).min(x0 + w - x), 1.5 * s));
                    x += 6.0 * s;
                }
            } else {
                fill.poly(&Poly::rect(x0, y, w, 1.5 * s));
            }
            fill.paint(window, color);
        },
    )
    .flex_none()
    .w(px(width))
    .h(px(22.0 * s))
    .into_any_element()
}

/// The painted stand-in for the platform's lights (galleries only).
fn stand_in_lights(palette: &'static Palette, s: f32) -> AnyElement {
    canvas(
        |_, _, _| {},
        move |bounds, (), window, _| {
            let cy = f32::from(bounds.center().y);
            let x0 = f32::from(bounds.origin.x);
            let r = 6.0 * s;
            for (i, tone) in [palette.coral.base, palette.amber.base, palette.mint.base]
                .into_iter()
                .enumerate()
            {
                #[allow(clippy::cast_precision_loss)]
                let cx = x0 + r + (i as f32) * 20.0 * s;
                let points = (0..20).map(|k| {
                    #[allow(clippy::cast_precision_loss)]
                    let a = (k as f32) / 20.0 * std::f32::consts::TAU;
                    pt(cx + r * a.cos(), cy + r * a.sin())
                });
                let mut fill = Fill::new();
                fill.poly(&Poly::new(points));
                fill.paint(window, Hsla::from(tone));
            }
        },
    )
    .flex_none()
    .w(px(52.0 * s))
    .h(px(12.0 * s))
    .into_any_element()
}

fn kind_glyph(kind: Kind, size: Pixels, palette: &Palette) -> AnyElement {
    svg()
        .path(variant_path(kind.path(), Stroke::width(1.8)))
        .size(size)
        .flex_none()
        .text_color(kind.hue(palette))
        .into_any_element()
}

impl RenderOnce for Titlebar {
    #[allow(clippy::too_many_lines)]
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let palette = cx.palette();
        let measure = self.measure;
        let s = measure.scale();
        let plan = Plan::at(measure.effective());
        let id = self.id.clone();
        let motion = Motion::scoped(ElementId::NamedChild(std::sync::Arc::new(id.clone()), "tb".into()), cx);
        let height = px(f32::from(geo::TITLEBAR) * s);

        // --- left: lights and the shelf toggle -------------------------
        let lights = match self.data.lights {
            Lights::Inset(width) => Some(div().flex_none().w(width).into_any_element()),
            Lights::StandIn => Some(
                div()
                    .flex_none()
                    .mr(px(8.0 * s))
                    .child(stand_in_lights(palette, s))
                    .into_any_element(),
            ),
            Lights::None => None,
        };
        let left_keys: Vec<&str> = if plan.buttons && self.data.shelf_open.is_some() {
            vec!["toggle"]
        } else {
            vec![]
        };
        let left = presence(&id, "left", cx).sync(left_keys, window, cx);
        let toggle = self.data.shelf_open.map(|open| {
            let mut button = icon_button(
                ElementId::NamedChild(std::sync::Arc::new(id.clone()), "shelf".into()),
                Icon::SideL,
                "Toggle shelf",
                &measure,
            )
            .on(open)
            .key("\\");
            if let Some(handler) = self.on_shelf.clone() {
                button = button.on_click(move |window, cx| handler(window, cx));
            }
            button
        });

        // --- the thread ------------------------------------------------
        let shown_behind: Vec<Bead> = {
            let n = self.data.behind.len();
            self.data.behind[n.saturating_sub(plan.behind)..].to_vec()
        };
        let behind_keys: Vec<ElementId> = shown_behind.iter().map(|bead| bead.key.clone()).collect();
        let behind = presence(&id, "behind", cx).sync(behind_keys, window, cx);
        let ahead_keys: Vec<ElementId> = if plan.ahead {
            self.data.ahead.iter().take(1).map(|bead| bead.key.clone()).collect()
        } else {
            vec![]
        };
        let ahead = presence(&id, "ahead", cx).sync(ahead_keys, window, cx);
        let bead_box = 22.0 * s;
        let strand_w = 18.0 * s;
        let live_w = 26.0 * s;
        let ink3: Hsla = palette.ink3.into();
        let mint: Hsla = palette.mint.base.into();

        let mut thread = div()
            .flex()
            .flex_1()
            .min_w(px(0.0))
            .h_full()
            .items_center()
            .justify_center();
        let count = self.data.behind.len();
        for item in &behind {
            let Some(bead) = self
                .data
                .behind
                .iter()
                .find(|bead| bead.key == item.key)
                .or_else(|| shown_behind.iter().find(|bead| bead.key == item.key))
            else {
                // A leaving bead we no longer have data for: hold its slot.
                thread = thread.child(item.slot(div().w(px(bead_box + strand_w))));
                continue;
            };
            let age = count
                - self
                    .data
                    .behind
                    .iter()
                    .position(|other| other.key == bead.key)
                    .unwrap_or(0);
            let (size, strength) = bead_look(age);
            // Older beads shrink and fade a step as the thread grows.
            let r = motion.animate(
                ElementId::NamedChild(std::sync::Arc::new(bead.key.clone()), "r".into()),
                size * 0.5 * std::f32::consts::SQRT_2 * s,
                spec::LIFT,
                window,
                cx,
            );
            let alpha = motion.animate(
                ElementId::NamedChild(std::sync::Arc::new(bead.key.clone()), "a".into()),
                strength,
                spec::REVEAL,
                window,
                cx,
            );
            let hue = super::with_alpha(bead.kind.hue(palette), alpha);
            let bead_id = ElementId::NamedChild(std::sync::Arc::new(bead.key.clone()), "bead".into());
            let hovered = window.use_keyed_state(bead_id.clone(), cx, |_, _| false);
            let grow = motion.animate(
                ElementId::NamedChild(std::sync::Arc::new(bead.key.clone()), "grow".into()),
                if *hovered.read(cx) && !item.is_leaving() { 1.25 } else { 1.0 },
                spec::LIFT,
                window,
                cx,
            );
            let mut button = div()
                .id(bead_id)
                .flex_none()
                .cursor_pointer()
                .child(layer(bead_art(hue, r, false, bead_box, s)).scale(grow))
                .on_hover(move |inside, _window, cx| {
                    let inside = *inside;
                    hovered.update(cx, |value, cx| {
                        if *value != inside {
                            *value = inside;
                            cx.notify();
                        }
                    });
                });
            if let (Some(handler), false) = (self.on_bead.clone(), item.is_leaving()) {
                let key = bead.key.clone();
                button = button.on_click(move |_, window, cx| handler(&key, window, cx));
            }
            let button = button.tip(bead.name.clone());
            let is_last = age == 1;
            let tie = if is_last {
                strand(mint, live_w, false, s)
            } else {
                strand(super::with_alpha(ink3, 0.7), strand_w, false, s)
            };
            thread = thread.child(
                item.slot(div().flex().flex_none().items_center().child(button).child(tie)),
            );
        }

        // The capsule.
        let fill_t = motion.animate("fill", if plan.fill { 1.0 } else { 0.0 }, spec::SETTLE, window, cx);
        let here_id = ElementId::NamedChild(std::sync::Arc::new(id.clone()), "here".into());
        thread = thread.child(capsule(
            &here_id,
            &self.data.here,
            fill_t,
            &measure,
            self.on_here.clone(),
            window,
            cx,
        ));

        for item in &ahead {
            let bead = self.data.ahead.iter().find(|bead| bead.key == item.key);
            let piece = match bead {
                Some(bead) => div()
                    .flex()
                    .flex_none()
                    .items_center()
                    .child(strand(palette.ink4.into(), strand_w, true, s))
                    .child({
                        let mut button = div()
                            .id(ElementId::NamedChild(
                                std::sync::Arc::new(bead.key.clone()),
                                "ahead".into(),
                            ))
                            .flex_none()
                            .cursor_pointer()
                            .child(bead_art(
                                super::with_alpha(bead.kind.hue(palette), 0.7),
                                11.0 * 0.5 * std::f32::consts::SQRT_2 * s,
                                true,
                                bead_box,
                                s,
                            ));
                        if let (Some(handler), false) = (self.on_bead.clone(), item.is_leaving()) {
                            let key = bead.key.clone();
                            button = button.on_click(move |_, window, cx| handler(&key, window, cx));
                        }
                        button
                    })
                    .into_any_element(),
                None => div().w(px(bead_box + strand_w)).into_any_element(),
            };
            thread = thread.child(item.slot(piece));
        }

        // --- right: the buttons -----------------------------------------
        let right_keys: Vec<&str> = if plan.buttons && !self.data.buttons.is_empty() {
            vec!["buttons"]
        } else {
            vec![]
        };
        let right = presence(&id, "right", cx).sync(right_keys, window, cx);
        let buttons = div()
            .flex()
            .flex_none()
            .items_center()
            .gap(px(6.0 * s))
            .children(self.data.buttons.iter().map(|spec| {
                let mut button = icon_button(spec.id.clone(), spec.icon, spec.label.clone(), &measure)
                    .size(IconButtonSize::Medium)
                    .on(spec.on);
                if let Some(key) = &spec.key {
                    button = button.key(key.clone());
                }
                if let Some(handler) = self.on_button.clone() {
                    let key = spec.id.clone();
                    button = button.on_click(move |window, cx| handler(&key, window, cx));
                }
                button
            }));

        let mut bar = div()
            .id(id)
            .flex()
            .flex_none()
            .items_center()
            .w(measure.width())
            .h(height)
            .gap(px(6.0 * s))
            .pl(px(14.0 * s))
            .pr(px(12.0 * s))
            .border_b_1()
            .border_color(palette.line1.hsla())
            .children(lights);
        if let (Some(item), Some(toggle)) = (left.first(), toggle) {
            bar = bar.child(item.slot(div().flex_none().child(toggle)));
        }
        bar = bar.child(thread);
        if let Some(item) = right.first() {
            bar = bar.child(item.slot(buttons));
        }
        bar
    }
}

/// The here capsule (or the ask field): a cut plate that fills its share of
/// the thread (`fill_t`: 0 = at most 430 px, centred; 1 = all of it).
#[allow(clippy::too_many_arguments)]
fn capsule(
    id: &ElementId,
    here: &Here,
    fill_t: f32,
    measure: &Measure,
    on_click: Option<OnClick>,
    window: &mut Window,
    cx: &mut App,
) -> AnyElement {
    let palette = cx.palette();
    let s = measure.scale();
    let motion = Motion::scoped(ElementId::NamedChild(std::sync::Arc::new(id.clone()), "m".into()), cx);
    let hovered = window.use_keyed_state(id.clone(), cx, |_, _| false);
    let hover_t = motion.animate(
        "hover",
        if *hovered.read(cx) { 1.0 } else { 0.0 },
        spec::HOVER,
        window,
        cx,
    );
    let rest = Edge::of(Bevel::Rest, palette);
    let edge = rest.mix(Edge::of(Bevel::Peri, palette), hover_t * 0.35);
    let fill = mix(palette.plate2.into(), palette.plate3.into(), hover_t);
    let icon_px = measure.icon(14.0);
    let ghost: Hsla = palette.ink4.into();
    let (min_w, basis) = match here {
        Here::Ask { .. } => (px(220.0 * s), px(460.0 * s)),
        _ => (px(200.0 * s), px(430.0 * s)),
    };
    let mut plate = cut()
        .chamfer(Chamfer::Px(10.0 * s))
        .edge(edge)
        .plate(Plate::Flat)
        .fill(fill)
        .flex()
        .items_center()
        .gap(px(9.0 * s))
        .pl(px(12.0 * s))
        .pr(px(8.0 * s))
        .h(px(34.0 * s))
        .w_full();
    plate = match here {
        Here::Page { kind, name, path } => plate
            .child(kind_glyph(*kind, measure.icon(14.0), palette))
            .child(
                div()
                    .flex_none()
                    .set(NAME, measure)
                    .text_color(palette.ink0.hsla())
                    .whitespace_nowrap()
                    .child(name.clone()),
            )
            .child(
                div()
                    .flex_1()
                    .min_w(px(0.0))
                    .overflow_hidden()
                    .whitespace_nowrap()
                    .text_ellipsis()
                    .set(PATH, measure)
                    .text_color(palette.ink3.hsla())
                    .child(path.clone()),
            )
            .child(icons::ui(Icon::Search, IconSize::S14, ghost).size(icon_px)),
        Here::Place { icon, name, path } => plate
            .child(icons::ui(*icon, IconSize::S14, palette.ink2).size(icon_px))
            .child(
                div()
                    .flex_none()
                    .set(NAME, measure)
                    .text_color(palette.ink0.hsla())
                    .whitespace_nowrap()
                    .child(name.clone()),
            )
            .child(
                div()
                    .flex_1()
                    .min_w(px(0.0))
                    .overflow_hidden()
                    .whitespace_nowrap()
                    .text_ellipsis()
                    .set(PATH, measure)
                    .text_color(palette.ink3.hsla())
                    .child(path.clone()),
            ),
        Here::Ask { prompt } => plate
            .child(icons::ui(Icon::Search, IconSize::S14, palette.ink2).size(icon_px))
            .child(
                div()
                    .flex_1()
                    .min_w(px(0.0))
                    .overflow_hidden()
                    .whitespace_nowrap()
                    .text_ellipsis()
                    .set(PROMPT, measure)
                    .text_color(palette.ink3.hsla())
                    .child(prompt.clone()),
            ),
    };
    let key_cap = crate::controls::kbd::key_badge(&SharedString::from("K"), &motion, id, measure);
    let hovered_set = hovered.clone();
    let mut plate = plate
        .relative()
        .child(key_cap)
        .id(id.clone())
        .cursor_pointer()
        .on_hover(move |inside, _window, cx| {
            let inside = *inside;
            hovered_set.update(cx, |value, cx| {
                if *value != inside {
                    *value = inside;
                    cx.notify();
                }
            });
        });
    if let Some(handler) = on_click {
        plate = plate.on_click(move |_, window, cx| handler(window, cx));
    }
    // The capsule's share of the thread: a flex factor on a spring, so the
    // switch between "centred, at most 430" and "fills" is a glide.
    div()
        .flex_basis(basis)
        .flex_grow(fill_t.max(0.0))
        .flex_shrink(1.0)
        .min_w(min_w)
        .child(plate)
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::Plan;

    #[test]
    fn the_thread_degrades_at_the_flow_targets_breakpoints() {
        let wide = Plan::at(1440.0);
        assert_eq!((wide.behind, wide.ahead, wide.fill, wide.buttons), (3, true, false, true));
        // `max-width: 1100px` is inclusive: at exactly 1100 the oldest bead is gone.
        assert_eq!(Plan::at(1100.0).behind, 2);
        assert_eq!(Plan::at(1100.5).behind, 3);
        let slim = Plan::at(760.0);
        assert_eq!((slim.behind, slim.ahead, slim.fill), (0, false, true));
        assert!(Plan::at(761.0).ahead);
        assert!(!Plan::at(520.0).buttons && Plan::at(521.0).buttons);
        // 200 % text on a 1440 window behaves like 720.
        assert_eq!(Plan::at(1440.0 / 2.0), Plan::at(720.0));
    }
}
