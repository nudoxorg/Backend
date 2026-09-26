//! `flow-lab`: scripted scenes for layout motion and compositing.
//!
//! - `flow-presence`: rows drop in (staggered) while the list makes room, a
//!   row leaves, returns mid-exit and retraces, two leave together.
//! - `flow-list`: Presence + FLIP: reorders flow, arrivals drop in, leavers
//!   close, all at once and interrupted mid-flight.
//! - `flow-reflow`: a page stepping 1440 → 1100 → 760 → 480 (discrete steps
//!   are epochs), then dragged back out (tracks; breakpoints flow).
//! - `flow-scale`: text scale 100 → 150 %, then density comfortable →
//!   compact → dense: the page reflows as one.
//! - `flow-descent`: a row's gem and name become the page hero, and back
//!   (⌘[), then a descent interrupted by going back mid-morph.
//! - `flow-fade`: one card faded as a group (compositing layer) beside the
//!   same card faded per primitive.
//! - `flow-shadows`: floating cut plates with the chamfered shadow beside the
//!   rounded box shadow it replaces.
//!
//! Every scene runs a script on executor timers, so a headless capture at a
//! virtual time is byte-for-byte reproducible and a window shows the same
//! choreography live.

use super::flight::{Camera, Flights, Path};
use super::flow::Flow;
use super::presence::{Phase, Presence, act};
use super::shared::{Morphing, shared, shared_with};
use super::{LINEAR, Motion, Spec, offset};
use crate::fonts::Typeset;
use crate::gallery::Scene;
use crate::icons::Kind;
use crate::measure::{Measure, Room, Set, Space};
use crate::paint::{Bevel, Chamfer, cut, gem};
use crate::probe;
use crate::theme::{ActiveFacet, Facet, set_facet};
use crate::tokens::{Palette, ty};
use crate::Density;
use gpui::{
    AnyElement, AnyView, AppContext, BoxShadow, Context, ElementId, Entity, Hsla, IntoElement,
    ParentElement, Render, SharedString, Styled, Window, canvas, div, layer, point, px,
};
use gpui::prelude::FluentBuilder as _;
use std::time::Duration;

pub(crate) const SCENES: &[Scene] = &[
    Scene {
        id: "flow-presence",
        title: "Presence: rows drop in (staggered) while the list makes room, a row leaves, returns mid-exit and retraces, two leave together",
        size: (520, 560),
        build: |_, cx| cx.new(PresenceLab::new).into(),
    },
    Scene {
        id: "flow-list",
        title: "Presence + FLIP: reorders flow from where rows were painted, arrivals drop in, leavers close, interrupted mid-flight",
        size: (520, 620),
        build: |_, cx| cx.new(ListLab::new).into(),
    },
    Scene {
        id: "flow-reflow",
        title: "Reflow: 1440 -> 1100 -> 760 -> 480 in steps (epochs), then dragged out (tracks; breakpoints flow)",
        size: (1480, 860),
        build: |_, cx| cx.new(ReflowLab::new).into(),
    },
    Scene {
        id: "flow-scale",
        title: "Text scale 100 -> 150 %, then density comfortable -> compact -> dense: the page reflows as one",
        size: (1000, 760),
        build: |_, cx| cx.new(ScaleLab::new).into(),
    },
    Scene {
        id: "flow-descent",
        title: "Descent: a row's gem and name become the page hero, back on cmd-[, and a descent interrupted mid-morph",
        size: (720, 560),
        build: |_, cx| cx.new(DescentLab::new).into(),
    },
    Scene {
        id: "flow-graph",
        title: "Page <-> graph: the hero gem shrinks into its node while the camera flies out (van Wijk path); a node click flies in and the node blooms into its page; cmd-[ goes back",
        size: (960, 640),
        build: |_, cx| cx.new(GraphLab::new).into(),
    },
    Scene {
        id: "flow-fade",
        title: "Group opacity (left, compositing layer) vs per-primitive opacity (right): the same card at the same alpha",
        size: (760, 300),
        build: |_, cx| cx.new(FadeLab::new).into(),
    },
    Scene {
        id: "flow-shadows",
        title: "Floating cut plates: the chamfered shadow (top) vs the rounded box shadow it replaces (bottom)",
        size: (760, 520),
        build: |_, cx| cx.new(|_| ShadowLab).into(),
    },
];

type Step<V> = Box<dyn FnOnce(&mut V, &mut Context<V>)>;

/// A timeline of edits, each applied at its virtual time on the executor
/// clock and followed by a notify.
fn script<V: 'static>(cx: &mut Context<V>, steps: Vec<(u64, Step<V>)>) {
    cx.spawn(async move |this, cx| {
        let mut at = 0;
        for (when, step) in steps {
            let wait = when.saturating_sub(at);
            at = when;
            if wait > 0 {
                cx.background_executor()
                    .timer(Duration::from_millis(wait))
                    .await;
            }
            if this
                .update(cx, |view, cx| {
                    step(view, cx);
                    cx.notify();
                })
                .is_err()
            {
                break;
            }
        }
    })
    .detach();
}

fn step<V: 'static>(at: u64, edit: impl FnOnce(&mut V, &mut Context<V>) + 'static) -> (u64, Step<V>) {
    (at, Box::new(edit))
}

const CRATES: [&str; 8] = [
    "serde", "serde_json", "toml", "ron", "tokio", "anyhow", "rayon", "regex",
];

const KINDS: [Kind; 8] = [
    Kind::Package,
    Kind::Struct,
    Kind::Trait,
    Kind::Enum,
    Kind::Function,
    Kind::Module,
    Kind::Macro,
    Kind::Class,
];

fn caption(text: &'static str, facet: &Facet) -> gpui::Div {
    div()
        .h(px(40.0))
        .typeset(ty::MONO_SMALL, facet)
        .text_color(facet.palette().ink3.hsla())
        .child(text)
}

/// A list row: a cut plate with a gem, a mono name and a trailing value.
fn row(index: usize, bevel: Bevel, trailing: SharedString, facet: &Facet) -> impl IntoElement {
    let palette = facet.palette();
    cut()
        .chamfer(Chamfer::Sm)
        .bevel(bevel)
        .h(px(40.0))
        .px(px(12.0))
        .flex()
        .items_center()
        .gap(px(10.0))
        .typeset(ty::MONO_ROW, facet)
        .text_color(palette.ink1.hsla())
        .child(gem(KINDS[index % KINDS.len()]).size(18.0))
        .child(div().flex_1().child(CRATES[index % CRATES.len()]))
        .child(
            div()
                .typeset(ty::MONO_SMALL, facet)
                .text_color(palette.ink3.hsla())
                .child(trailing),
        )
}

fn bevel_of(phase: Phase) -> Bevel {
    match phase {
        Phase::Entering => Bevel::Hot,
        Phase::Present => Bevel::Rest,
        Phase::Leaving => Bevel::Ghost,
    }
}

fn index_of(key: &ElementId) -> usize {
    match key {
        ElementId::Integer(n) => usize::try_from(*n).unwrap_or(0),
        _ => 0,
    }
}

// --- flow-presence ---------------------------------------------------------

struct PresenceLab {
    presence: Presence,
    rows: Vec<usize>,
}

impl PresenceLab {
    fn new(cx: &mut Context<Self>) -> Self {
        script(
            cx,
            vec![
                // Two arrive (staggered) while serde_json leaves.
                step(100, |lab: &mut Self, _| lab.rows = vec![0, 4, 2, 3, 5]),
                // serde_json comes back mid-exit: it retraces, never restarts.
                step(300, |lab: &mut Self, _| lab.rows = vec![0, 1, 4, 2, 3, 5]),
                // Two neighbours leave together; the list closes once.
                step(900, |lab: &mut Self, _| lab.rows = vec![0, 1, 4, 5]),
            ],
        );
        Self {
            presence: Presence::new("flow-presence").stagger(Duration::from_millis(70)),
            rows: vec![0, 1, 2, 3],
        }
    }
}

impl Render for PresenceLab {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let facet = cx.facet();
        let items = self.presence.sync(
            self.rows.iter().map(|&row| ElementId::Integer(row as u64)),
            window,
            cx,
        );
        let rows = items.into_iter().map(|item| {
            let index = index_of(&item.key);
            let plate = row(
                index,
                bevel_of(item.phase),
                format!("{:.2}", item.presence).into(),
                &facet,
            );
            // The slot is the list's gap too, so the gap opens and closes
            // with the row.
            probe::measure(
                ElementId::Name(format!("flow-presence.slot.{}", CRATES[index]).into()),
                item.slot(div().pb(px(10.0)).child(plate)),
            )
        });
        div()
            .size_full()
            .bg(facet.palette().g1)
            .px(px(56.0))
            .py(px(36.0))
            .flex()
            .flex_col()
            .child(caption(
                "presence \u{b7} drop-in, make room, leave, return mid-exit",
                &facet,
            ))
            .children(rows)
    }
}

// --- flow-list -------------------------------------------------------------

struct ListLab {
    presence: Presence,
    flow: Flow,
    rows: Vec<usize>,
    version: u64,
}

impl ListLab {
    fn new(cx: &mut Context<Self>) -> Self {
        script(
            cx,
            vec![
                // A reorder: rows flow from where they were painted.
                step(100, |lab: &mut Self, _| {
                    lab.rows = vec![2, 0, 3, 1];
                    lab.version += 1;
                }),
                // One arrives, one leaves (no reorder: nothing flows, the
                // slots make and close room).
                step(700, |lab: &mut Self, _| lab.rows = vec![2, 4, 0, 1]),
                // Reversed, and interrupted by another reorder mid-flight.
                step(1200, |lab: &mut Self, _| {
                    lab.rows = vec![1, 0, 4, 2];
                    lab.version += 1;
                }),
                step(1320, |lab: &mut Self, _| {
                    lab.rows = vec![4, 2, 1, 0];
                    lab.version += 1;
                }),
                // Everything at once: two arrive, one leaves, the rest reorder.
                step(1900, |lab: &mut Self, _| {
                    lab.rows = vec![5, 0, 4, 6, 1];
                    lab.version += 1;
                }),
            ],
        );
        Self {
            presence: Presence::new("flow-list.presence")
                .enter(act::RISE)
                .stagger(Duration::from_millis(60)),
            flow: Flow::new("flow-list.flip"),
            rows: vec![0, 1, 2, 3],
            version: 0,
        }
    }
}

impl Render for ListLab {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let facet = cx.facet();
        self.flow.epoch(self.version);
        let items = self.presence.sync(
            self.rows.iter().map(|&row| ElementId::Integer(row as u64)),
            window,
            cx,
        );
        let rows = items.into_iter().map(|item| {
            let index = index_of(&item.key);
            let plate = row(
                index,
                bevel_of(item.phase),
                format!("#{}", self.rows.iter().position(|&r| r == index).map_or(0, |p| p + 1)).into(),
                &facet,
            );
            let key = item.key.clone();
            item.slot(
                div().pb(px(10.0)).child(self.flow.item(
                    key,
                    probe::measure(
                        ElementId::Name(format!("flow-list.row.{}", CRATES[index]).into()),
                        plate,
                    ),
                )),
            )
        });
        div()
            .size_full()
            .bg(facet.palette().g1)
            .px(px(56.0))
            .py(px(36.0))
            .flex()
            .flex_col()
            .child(caption(
                "flip \u{b7} reorder, arrive, leave, interrupt",
                &facet,
            ))
            .children(rows)
    }
}

// --- a responsive page (flow-reflow, flow-scale) ---------------------------

const NOTES: [&str; 3] = [
    "Every symbol is a page.",
    "Margins fold into the flow below 1100.",
    "Breakpoints flow; drags track.",
];

/// A page laid out for `measure`: a title, a grid of cards (columns from the
/// room), and a margin column that folds under the grid below `Wide`. Every
/// card and note is a flow item.
fn page(flow: &Flow, measure: &Measure, facet: &Facet) -> AnyElement {
    let palette = facet.palette();
    let wide = measure.room() >= Room::Wide;
    let margin_width = px(250.0 * facet.text_scale);
    let gap = measure.space(Space::Base);
    let grid_measure = if wide {
        measure.within(measure.width() - margin_width - measure.space(Space::Gutter))
    } else {
        *measure
    };
    let (count, column) = grid_measure.columns(220.0, Space::Base, 4);
    let card = |index: usize| {
        flow.item(
            ElementId::Name(format!("card-{index}").into()),
            cut()
                .chamfer(Chamfer::Md)
                .w(column.width())
                .p(column.space(Space::Snug))
                .flex()
                .flex_col()
                .gap(column.space(Space::Tight))
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(column.space(Space::Tight))
                        .child(gem(KINDS[index]).size(f32::from(column.icon(20.0))))
                        .child(
                            div()
                                .set(ty::HEAD, &column)
                                .text_color(palette.ink0.hsla())
                                .child(CRATES[index]),
                        ),
                )
                .when(facet.density != Density::Dense, |card| {
                    card.child(
                        div()
                            .set(ty::SMALL, &column)
                            .text_color(palette.ink2.hsla())
                            .child("Serialize and deserialize, zero-copy where it can."),
                    )
                }),
        )
    };
    // A grid of exactly `count` columns, with the gap the columns were
    // measured with. (A wrapping row of cards that exactly fill it breaks
    // lines by float rounding: mid-drag the last card flickered between rows.)
    #[allow(clippy::cast_possible_truncation)]
    let grid = div()
        .grid()
        .grid_cols(count as u16)
        .gap(grid_measure.space(Space::Base))
        .w(grid_measure.width())
        .children((0..6).map(card));
    let notes = div()
        .flex()
        .flex_col()
        .gap(gap)
        .when(wide, |notes| notes.w(margin_width))
        .children(NOTES.iter().enumerate().map(|(index, note)| {
            flow.item(
                ElementId::Name(format!("note-{index}").into()),
                div()
                    .pl(px(10.0))
                    .border_l_1()
                    .border_color(palette.line3.hsla())
                    .set(ty::CAPTION, measure)
                    .text_color(palette.ink2.hsla())
                    .child(*note),
            )
        }));
    let title = flow.item(
        "title",
        div()
            .set(ty::DISPLAY, measure)
            .text_color(palette.ink0.hsla())
            .child("serde"),
    );
    let body = if wide {
        div()
            .flex()
            .gap(measure.space(Space::Gutter))
            .child(grid)
            .child(notes)
    } else {
        div().flex().flex_col().gap(gap).child(grid).child(notes)
    };
    div()
        .w(measure.width())
        .flex()
        .flex_col()
        .gap(measure.space(Space::Roomy))
        .child(title)
        .child(body)
        .into_any_element()
}

struct ReflowLab {
    motion: Motion,
    flow: Flow,
    width: f32,
    step: u32,
    drag: bool,
}

impl ReflowLab {
    fn new(cx: &mut Context<Self>) -> Self {
        let stepped = |width: f32| {
            move |lab: &mut Self, _: &mut Context<Self>| {
                lab.width = width;
                lab.step += 1;
            }
        };
        script(
            cx,
            vec![
                step(100, stepped(1100.0)),
                step(800, stepped(760.0)),
                step(1500, stepped(480.0)),
                // Dragged back out over 1.2 s: every frame's width is
                // followed; crossing a room class is an epoch; the column
                // count changes undeclared (flow absorbs those jumps).
                step(2200, |lab: &mut Self, _| {
                    lab.drag = true;
                    lab.width = 1300.0;
                }),
            ],
        );
        Self {
            motion: Motion::new(),
            flow: Flow::new("flow-reflow"),
            width: 1440.0,
            step: 0,
            drag: false,
        }
    }
}

impl Render for ReflowLab {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let facet = cx.facet();
        // The steps are discrete (not a motion); only the drag is animated,
        // from the last step's width.
        let width = if self.drag {
            self.motion.animate_from(
                "drag",
                480.0,
                self.width,
                Spec::tween(Duration::from_millis(1_200), LINEAR),
                window,
                cx,
            )
        } else {
            self.width
        };
        let measure = Measure::new(px(width), &facet);
        // Steps are discrete (a shelf collapsing): epochs. The drag is not;
        // only the room class it crosses is.
        self.flow.epoch((self.step, measure.room()));
        div()
            .size_full()
            .bg(facet.palette().g1)
            .p(px(20.0))
            .flex()
            .flex_col()
            .child(caption(
                "reflow \u{b7} 1440 \u{2192} 1100 \u{2192} 760 \u{2192} 480 (epochs), then a drag",
                &facet,
            ))
            .child(page(&self.flow, &measure, &facet))
    }
}

struct ScaleLab {
    flow: Flow,
}

impl ScaleLab {
    fn new(cx: &mut Context<Self>) -> Self {
        let restyle = |edit: fn(&mut Facet)| {
            move |_: &mut Self, cx: &mut Context<Self>| {
                let mut facet = cx.facet();
                edit(&mut facet);
                set_facet(facet, cx);
            }
        };
        script(
            cx,
            vec![
                step(100, restyle(|facet| facet.text_scale = 1.5)),
                step(900, restyle(|facet| facet.density = Density::Compact)),
                step(1500, restyle(|facet| facet.density = Density::Dense)),
            ],
        );
        Self {
            flow: Flow::new("flow-scale"),
        }
    }
}

impl Render for ScaleLab {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let facet = cx.facet();
        let measure = Measure::new(px(940.0), &facet);
        self.flow.epoch((facet.text_scale.to_bits(), facet.density, measure.room()));
        div()
            .size_full()
            .bg(facet.palette().g1)
            .p(px(30.0))
            .flex()
            .flex_col()
            .child(caption(
                "text scale 100 \u{2192} 150 %, density comfortable \u{2192} compact \u{2192} dense",
                &facet,
            ))
            .child(page(&self.flow, &measure, &facet))
    }
}

// --- flow-descent ------------------------------------------------------------

struct DescentLab {
    open: Option<usize>,
    /// The page body is a presence owned by this (always mounted) view, so
    /// a page closed mid-rise fades its body out instead of dropping it.
    bodies: Presence,
}

/// The page body's way in: held while the hero lands (240 ms), then a 10 px
/// rise with a fade (380 ms). No room: the body is placed absolutely.
static BODY_IN_POSE: [(f32, crate::Pose); 3] = [
    (0.0, crate::Pose { x: 0.0, y: 10.0, sx: 1.0, sy: 1.0, rotate: 0.0, opacity: 0.0 }),
    (0.387, crate::Pose { x: 0.0, y: 10.0, sx: 1.0, sy: 1.0, rotate: 0.0, opacity: 0.0 }),
    (1.0, crate::Pose::REST),
];
static BODY_ROOM: [(f32, super::presence::Extent); 2] = [
    (0.0, super::presence::Extent::FULL),
    (1.0, super::presence::Extent::FULL),
];
const BODY_IN: super::presence::Act = super::presence::Act {
    duration: Duration::from_millis(620),
    pose: super::keys::Keys::new(Duration::from_millis(620), &BODY_IN_POSE, crate::tokens::motion::GLIDE),
    room: super::keys::Keys::new(Duration::from_millis(620), &BODY_ROOM, crate::tokens::motion::GLIDE),
};

impl DescentLab {
    fn new(cx: &mut Context<Self>) -> Self {
        script(
            cx,
            vec![
                step(100, |lab: &mut Self, _| lab.open = Some(2)),
                // cmd-[: back to the list; the hero morphs home.
                step(900, |lab: &mut Self, _| lab.open = None),
                step(1700, |lab: &mut Self, _| lab.open = Some(4)),
                // Back again mid-morph: it turns around from where it is.
                step(1950, |lab: &mut Self, _| lab.open = None),
            ],
        );
        Self {
            open: None,
            bodies: Presence::new("flow-descent.body")
                .enter(BODY_IN)
                .exit(act::FADE_OUT),
        }
    }
}

impl Render for DescentLab {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let facet = cx.facet();
        let palette = facet.palette();
        // Page bodies: the open page's rises in; a closed one fades out.
        let bodies = self
            .bodies
            .sync(self.open.map(|index| ElementId::Integer(index as u64)), window, cx)
            .into_iter()
            .map(|item| {
                div().absolute().top(px(184.0)).left(px(36.0)).child(item.slot(
                    div()
                        .w(px(620.0))
                        .typeset(ty::LEDE, &facet)
                        .text_color(palette.ink2.hsla())
                        .child("A framework for serializing and deserializing Rust data structures efficiently and generically."),
                ))
            })
            .collect::<Vec<_>>();
        let base = div().size_full().relative().bg(palette.g1).p(px(36.0)).flex().flex_col();
        let base = match self.open {
            None => {
                let rows = (0..6).map(|index| {
                    cut()
                        .chamfer(Chamfer::Sm)
                        .h(px(44.0))
                        .px(px(12.0))
                        .mb(px(10.0))
                        .flex()
                        .items_center()
                        .gap(px(10.0))
                        .child(shared(
                            ElementId::Name(format!("gem-{index}").into()),
                            gem(KINDS[index]).size(18.0),
                        ))
                        .child(shared(
                            ElementId::Name(format!("name-{index}").into()),
                            div()
                                .typeset(ty::MONO_ROW, &facet)
                                .text_color(palette.ink1.hsla())
                                .child(CRATES[index]),
                        ))
                });
                base.child(caption("descent \u{b7} orbit", &facet)).children(rows)
            }
            Some(index) => {
                base.child(caption("descent \u{b7} package", &facet))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(20.0))
                            .child(shared(
                                ElementId::Name(format!("gem-{index}").into()),
                                gem(KINDS[index]).size(72.0),
                            ))
                            .child(shared(
                                ElementId::Name(format!("name-{index}").into()),
                                div()
                                    .typeset(ty::HERO, &facet)
                                    .text_color(palette.ink0.hsla())
                                    .child(CRATES[index]),
                            )),
                    )
            }
        };
        base.children(bodies)
    }
}

// --- flow-fade ---------------------------------------------------------------

struct FadeLab {
    motion: Motion,
    low: bool,
}

impl FadeLab {
    fn new(cx: &mut Context<Self>) -> Self {
        script(
            cx,
            vec![
                step(100, |lab: &mut Self, _| lab.low = true),
                step(1100, |lab: &mut Self, _| lab.low = false),
            ],
        );
        Self {
            motion: Motion::new(),
            low: false,
        }
    }
}

fn fade_card(facet: &Facet, label: &'static str) -> impl IntoElement {
    let palette = facet.palette();
    cut()
        .chamfer(Chamfer::Md)
        .bevel(Bevel::Hot)
        .w(px(300.0))
        .p(px(16.0))
        .flex()
        .flex_col()
        .gap(px(8.0))
        .child(
            div()
                .flex()
                .items_center()
                .gap(px(10.0))
                .child(gem(Kind::Trait).size(28.0))
                .child(
                    div()
                        .typeset(ty::HEAD, facet)
                        .text_color(palette.ink0.hsla())
                        .child("Serialize"),
                ),
        )
        .child(
            div()
                .typeset(ty::MONO_SMALL, facet)
                .text_color(palette.ink2.hsla())
                .child(label),
        )
}

impl Render for FadeLab {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let facet = cx.facet();
        let alpha = self.motion.animate(
            "alpha",
            if self.low { 0.35 } else { 1.0 },
            Spec::tween(Duration::from_millis(620), crate::tokens::motion::GLIDE),
            window,
            cx,
        );
        div()
            .size_full()
            .bg(facet.palette().g1)
            .p(px(36.0))
            .flex()
            .flex_col()
            .child(caption("group opacity (left) vs per primitive (right)", &facet))
            .child(
                div()
                    .flex()
                    .gap(px(40.0))
                    .child(layer(fade_card(&facet, "layer(..).opacity(a)")).opacity(alpha))
                    .child(offset(fade_card(&facet, "per-primitive opacity(a)")).opacity(alpha)),
            )
    }
}

// --- flow-shadows ------------------------------------------------------------

struct ShadowLab;

fn floating_plate(chamfer: Chamfer, width: f32, label: &'static str, facet: &Facet) -> impl IntoElement {
    cut()
        .chamfer(chamfer)
        .floating()
        .w(px(width))
        .h(px(90.0))
        .p(px(14.0))
        .typeset(ty::MONO_SMALL, facet)
        .text_color(facet.palette().ink2.hsla())
        .child(label)
}

/// The old rounded box shadow under a plate-shaped quad, for comparison.
fn rounded_plate(chamfer: f32, width: f32, palette: &'static Palette) -> impl IntoElement {
    let (shadow, plate) = (Hsla::from(palette.shadow), Hsla::from(palette.plate));
    canvas(
        |_, _, _| {},
        move |bounds, (), window, _| {
            let shadows = [BoxShadow {
                color: shadow,
                offset: point(px(0.0), px(18.0)),
                blur_radius: px(15.0),
                spread_radius: px(0.0),
                inset: false,
            }];
            window.paint_drop_shadows(bounds, gpui::Corners::all(px(chamfer * 0.5)), &shadows);
            let mut path = gpui::PathBuilder::fill();
            let (x, y) = (bounds.origin.x, bounds.origin.y);
            let (w, h) = (bounds.size.width, bounds.size.height);
            let c = px(chamfer);
            path.move_to(point(x + c, y));
            path.line_to(point(x + w, y));
            path.line_to(point(x + w, y + h - c));
            path.line_to(point(x + w - c, y + h));
            path.line_to(point(x, y + h));
            path.line_to(point(x, y + c));
            path.close();
            if let Ok(path) = path.build() {
                window.paint_path(path, plate);
            }
        },
    )
    .w(px(width))
    .h(px(90.0))
}

impl Render for ShadowLab {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let facet = cx.facet();
        let palette = facet.palette();
        div()
            .size_full()
            .bg(palette.g1)
            .p(px(40.0))
            .flex()
            .flex_col()
            .gap(px(60.0))
            .child(caption("chamfered shadow (top) vs rounded box shadow (bottom)", &facet))
            .child(
                div()
                    .flex()
                    .gap(px(40.0))
                    .child(floating_plate(Chamfer::Float, 180.0, "float 10", &facet))
                    .child(floating_plate(Chamfer::Md, 200.0, "md 14", &facet))
                    .child(floating_plate(Chamfer::Lg, 220.0, "lg 22", &facet)),
            )
            .child(
                div()
                    .flex()
                    .gap(px(40.0))
                    .child(rounded_plate(10.0, 180.0, palette))
                    .child(rounded_plate(14.0, 200.0, palette))
                    .child(rounded_plate(22.0, 220.0, palette)),
            )
    }
}

// --- flow-graph --------------------------------------------------------------

/// Graph nodes: the crates, laid out on a golden-angle spiral in world units.
const NODES: usize = 20;

fn node_name(index: usize) -> &'static str {
    const NAMES: [&str; NODES] = [
        "serde", "serde_json", "toml", "ron", "tokio", "anyhow", "rayon", "regex", "clap",
        "syn", "quote", "rand", "bytes", "hyper", "axum", "tracing", "thiserror", "itertools",
        "smallvec", "once_cell",
    ];
    NAMES[index % NODES]
}

#[allow(clippy::cast_precision_loss)]
fn node_at(index: usize) -> (f64, f64) {
    let r = 44.0 * (index as f64 + 0.6).sqrt();
    let angle = index as f64 * 2.399_963;
    (r * angle.cos(), r * angle.sin())
}

fn node_edges() -> impl Iterator<Item = (usize, usize)> {
    (1..NODES).flat_map(|i| [(i, (i * 7) % i.max(1)), (i, i / 3)])
}

/// The overview framing, and a close framing on one node.
const OVERVIEW: Camera = Camera::new(0.0, 0.0, 520.0);

fn close_on(index: usize) -> Camera {
    let (x, y) = node_at(index);
    Camera::new(x, y, 120.0)
}

const GEM_KEY: &str = "graph.gem";
const NAME_KEY: &str = "graph.name";

fn shared_key(prefix: &'static str, index: usize) -> ElementId {
    ElementId::NamedInteger(prefix.into(), index as u64)
}

/// A name that crossfades between the page's display title and the graph's
/// mono label while it morphs, so the typeface never pops. `title_end`: this
/// end is the title (the other the label). The departing typeface is set at
/// this end's line height, so the morph's scale (which maps this end's box
/// onto the departing box) restores it exactly at t = 0.
fn crossfaded_name(name: &'static str, morph: Morphing, title_end: bool, facet: &Facet) -> AnyElement {
    let palette = facet.palette();
    let (title, label) = ((ty::HERO, palette.ink0), (ty::MONO_SMALL, palette.ink1));
    let ((own, own_ink), (other, other_ink)) = if title_end { (title, label) } else { (label, title) };
    let text = |role: crate::tokens::TypeRole, ink: crate::tokens::Tone| {
        div()
            .h(px(role.line))
            .typeset(role, facet)
            .text_color(ink.hsla())
            .whitespace_nowrap()
            .child(name)
    };
    // The departing typeface fades out over the first 40 % of the morph.
    let old = (1.0 - morph.t / 0.4).clamp(0.0, 1.0);
    if morph.from.is_none() || old <= 0.0 {
        return text(own, own_ink).into_any_element();
    }
    // The morph scales this box by `from / own.line`. The departing face is
    // shaped at its real size (a variable font's optical size changes its
    // widths, so shaping small and scaling up would not match) and
    // counter-scaled by the inverse, so it is exactly itself at t = 0.
    let from = morph.from.map_or(other.line, |from| f32::from(from.height));
    let counter = own.line / from;
    // Centred on this end's box: the morph maps box centres onto each other.
    div()
        .relative()
        .child(layer(text(own, own_ink)).opacity(1.0 - old))
        .child(
            div()
                .absolute()
                .inset_0()
                .flex()
                .justify_center()
                .items_center()
                .child(layer(text(other, other_ink)).scale(counter).opacity(old)),
        )
        .into_any_element()
}

/// A gem in an `own` px box that draws as a native gem of its painted size
/// while it morphs (its glyph and facets follow the size it is seen at).
fn morphing_gem(kind: Kind, own: f32, morph: Morphing) -> AnyElement {
    let painted = morph.painted(own).max(1.0);
    div()
        .size(px(own))
        .child(
            layer(gem(kind).size(painted))
                .scale(own / painted)
                .origin(0.0, 0.0),
        )
        .into_any_element()
}

struct GraphPage {
    index: usize,
    motion: Motion,
}

impl Render for GraphPage {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let facet = cx.facet();
        let palette = facet.palette();
        let index = self.index;
        let body = self.motion.animate_from(
            "graph.body",
            0.0,
            1.0,
            Spec::tween(Duration::from_millis(380), crate::tokens::motion::GLIDE)
                .delayed(Duration::from_millis(300)),
            window,
            cx,
        );
        let name_facet = facet;
        div()
            .size_full()
            .bg(palette.g1)
            .p(px(48.0))
            .flex()
            .flex_col()
            .gap(px(24.0))
            .child(caption("page \u{b7} \u{2318}G for the graph", &facet))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(20.0))
                    .child(shared_with(shared_key(GEM_KEY, index), move |morph| {
                        morphing_gem(KINDS[index % KINDS.len()], 72.0, morph)
                    }))
                    .child(shared_with(shared_key(NAME_KEY, index), move |morph| {
                        crossfaded_name(node_name(index), morph, true, &name_facet)
                    })),
            )
            .child(
                offset(
                    div()
                        .max_w(px(620.0))
                        .typeset(ty::LEDE, &facet)
                        .text_color(palette.ink2.hsla())
                        .child("Every symbol is a page; every page is a node. The graph is one zoom out."),
                )
                .y(px(10.0 * (1.0 - body)))
                .opacity(body),
            )
    }
}

struct GraphField {
    flights: Flights,
    target: Camera,
    focus: usize,
}

impl GraphField {
    /// Arrives framed tight on `focus` and flies out to the overview.
    fn arrive(&mut self, focus: usize) {
        self.focus = focus;
        self.flights.jump("camera", close_on(focus));
        self.target = OVERVIEW;
    }
}

impl Render for GraphField {
    #[allow(clippy::cast_possible_truncation)]
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let facet = cx.facet();
        let palette = facet.palette();
        let shot = self.flights.fly("camera", self.target, window, cx);
        let (view_w, view_h) = (960.0_f64, 640.0_f64);
        let camera = shot.camera;
        let zoom = view_w / camera.w;
        let project = move |(x, y): (f64, f64)| {
            (
                ((x - camera.x) * zoom + view_w / 2.0) as f32,
                ((y - camera.y) * zoom + view_h / 2.0) as f32,
            )
        };
        let line = palette.line3;
        let edges = canvas(
            |_, _, _| {},
            move |bounds, (), window, _| {
                for (a, b) in node_edges() {
                    let (pa, pb) = (project(node_at(a)), project(node_at(b)));
                    let mut path = gpui::PathBuilder::stroke(px(1.0));
                    path.move_to(bounds.origin + point(px(pa.0), px(pa.1)));
                    path.line_to(bounds.origin + point(px(pb.0), px(pb.1)));
                    if let Ok(path) = path.build() {
                        window.paint_path(path, line);
                    }
                }
            },
        )
        .absolute()
        .size_full();
        let label_facet = facet;
        let nodes = (0..NODES).map(|index| {
            let (x, y) = project(node_at(index));
            let focused = index == self.focus;
            div()
                .absolute()
                .left(px(x - 7.0))
                .top(px(y - 7.0))
                .flex()
                .flex_col()
                .items_center()
                .child(shared_with(shared_key(GEM_KEY, index), move |morph| {
                    morphing_gem(KINDS[index % KINDS.len()], 14.0, morph)
                }))
                .child(div().absolute().top(px(18.0)).child(if focused {
                    shared_with(shared_key(NAME_KEY, index), move |morph| {
                        crossfaded_name(node_name(index), morph, false, &label_facet)
                    })
                    .into_any_element()
                } else {
                    div()
                        .typeset(ty::MONO_SMALL, &label_facet)
                        .text_color(label_facet.palette().ink3.hsla())
                        .whitespace_nowrap()
                        .child(node_name(index))
                        .into_any_element()
                }))
        });
        div()
            .size_full()
            .relative()
            .bg(palette.g1)
            .child(edges)
            .children(nodes)
            .child(div().absolute().top(px(24.0)).left(px(48.0)).child(caption(
                "graph \u{b7} click a node to descend \u{b7} \u{2318}[ back",
                &facet,
            )))
    }
}

struct GraphLab {
    page: Entity<GraphPage>,
    graph: Entity<GraphField>,
    showing_graph: bool,
}

impl GraphLab {
    fn new(cx: &mut Context<Self>) -> Self {
        let page = cx.new(|_| GraphPage {
            index: 0,
            motion: Motion::new(),
        });
        let graph = cx.new(|_| GraphField {
            flights: Flights::new(),
            target: OVERVIEW,
            focus: 0,
        });
        script(
            cx,
            vec![
                // Cmd-G: the hero gem shrinks into its node as the camera flies out.
                step(100, |lab: &mut Self, cx| {
                    lab.graph.update(cx, |graph, _| graph.arrive(0));
                    lab.showing_graph = true;
                }),
                // A click on tokio: the camera swoops in; on landing the node
                // blooms into its page.
                step(1500, |lab: &mut Self, cx| {
                    let target = close_on(4);
                    let flight = Path::new(OVERVIEW, target).duration();
                    lab.graph.update(cx, |graph, _| {
                        graph.focus = 4;
                        graph.target = target;
                    });
                    cx.spawn(async move |lab, cx| {
                        cx.background_executor().timer(flight).await;
                        let _ = lab.update(cx, |lab, cx| {
                            lab.page.update(cx, |page, _| {
                                // A new page: its body rises in again.
                                page.index = 4;
                                page.motion.replay("graph.body");
                            });
                            lab.showing_graph = false;
                            cx.notify();
                        });
                    })
                    .detach();
                }),
                // Cmd-[: back to the graph, out from tokio.
                step(3400, |lab: &mut Self, cx| {
                    lab.graph.update(cx, |graph, _| graph.arrive(4));
                    lab.showing_graph = true;
                }),
            ],
        );
        Self {
            page,
            graph,
            showing_graph: false,
        }
    }
}

impl Render for GraphLab {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        let view: AnyView = if self.showing_graph {
            self.graph.clone().into()
        } else {
            self.page.clone().into()
        };
        div().size_full().child(view)
    }
}
