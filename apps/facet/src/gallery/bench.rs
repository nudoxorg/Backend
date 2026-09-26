//! The harness bench: a small, fully instrumented interactive scene the
//! verification battery exercises (hover lifts, press, focus walk, a chained
//! popup stack with exits, ⌘/⌥ reveal, text that follows scale and density),
//! and its canaries: the same scene with one deliberate defect each, which
//! `verify` must catch. Canaries are never listed in [`super::all`].

use super::Scene;
use crate::motion::{Motion, offset, spec};
use crate::probe::{self, StackEntry, StackPhase, StackSample, Target, TextOverflow};
use crate::theme::ActiveFacet;
use crate::tokens::{TypeRole, ty};
use crate::{Density, Typeset};
use gpui::{
    AnyView, App, AppContext, Context, FocusHandle, InteractiveElement, IntoElement, KeyDownEvent,
    MouseButton, ParentElement, Render, StatefulInteractiveElement, Styled, Window, div, px,
};

/// The gallery scene.
pub(crate) const SCENES: &[Scene] = &[Scene {
    id: "harness-bench",
    title: "Harness bench: hover lifts, press, focus walk, a chained popup stack, ⌘/⌥ reveal",
    size: (720, 440),
    build: |window, cx| build(Defect::None, window, cx),
}];

macro_rules! canaries {
    ($($id:literal => $defect:ident),* $(,)?) => {
        /// The bench with one deliberate defect each. `verify --canaries`
        /// expects every one of them to fail the check its id names.
        pub const CANARIES: &[Scene] = &[$(Scene {
            id: $id,
            title: "harness canary (a deliberate defect)",
            size: (720, 440),
            build: |window, cx| build(Defect::$defect, window, cx),
        }),*];
    };
}

canaries! {
    "canary-stuck-hover" => StuckHover,
    "canary-jump" => Jump,
    "canary-clipped-text" => ClippedText,
    "canary-leaked-frames" => LeakedFrames,
    "canary-stuck-popup" => StuckPopup,
    "canary-off-slot" => OffSlot,
    "canary-lagging-group" => LaggingGroup,
    "canary-low-contrast" => LowContrast,
    "canary-small-target" => SmallTarget,
    "canary-overlap" => Overlap,
    "canary-slow-frame" => SlowFrame,
    "canary-unrequested-motion" => UnrequestedMotion,
    "canary-panic" => Panic,
    "canary-hover-race" => HoverRace,
    "canary-offscreen" => Offscreen,
    "canary-dead-focus" => DeadFocus,
    "canary-wall-clock" => WallClock,
    "canary-residue" => Residue,
}

/// One deliberate defect.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Defect {
    /// A healthy bench.
    None,
    /// Hover is set on enter and never cleared.
    StuckHover,
    /// The hover lift is reset to rest halfway through, so it jumps.
    Jump,
    /// A label in a box too narrow for it, clipped without an ellipsis.
    ClippedText,
    /// The root asks for another frame on every render, forever.
    LeakedFrames,
    /// Esc marks a popup leaving but it never leaves.
    StuckPopup,
    /// The lifted plate settles 3 px away from its slot.
    OffSlot,
    /// One part of a grouped pair starts its motion a frame late.
    LaggingGroup,
    /// A caption in ink too close to its ground.
    LowContrast,
    /// A 12 px clickable dot.
    SmallTarget,
    /// Two labels painted over each other.
    Overlap,
    /// Render burns 150 ms while a popup is up.
    SlowFrame,
    /// A value that moves with the clock without asking for frames.
    UnrequestedMotion,
    /// Pressing `x` panics.
    Panic,
    /// A hover shorter than 120 ms leaves its underline behind.
    HoverRace,
    /// A focusable parked left of the viewport.
    Offscreen,
    /// ⌘K moves focus to a handle no element tracks.
    DeadFocus,
    /// A label whose width follows the wall clock.
    WallClock,
    /// The title's entrance settles at 97 % opacity when motion is on.
    Residue,
}

const NAMES: [&str; 4] = ["alpha", "beta", "gamma", "delta"];
const CAPTION: TypeRole = ty::SMALL;
const PLATE_W: f32 = 140.0;
const PLATE_H: f32 = 64.0;

/// The declared default script: sweep the plates, press one, open the
/// popup, chain a child, x-ray, close with Esc, park the pointer.
const SCRIPT: &str = "move 110,114 @0; move 270,114 @160; down 270,114 @320; up 270,114 @400; \
     key cmd-k @560; click 80,305 @820; hold alt @1000; release alt @1200; \
     key escape @1300; key escape @1400; move 700,420 @1500";

struct Popup {
    id: u64,
    parent: Option<u64>,
    leaving: bool,
}

struct Bench {
    defect: Defect,
    ghost: FocusHandle,
    motion: Motion,
    focus: FocusHandle,
    plates: Vec<FocusHandle>,
    hovered: Option<usize>,
    pressed: Option<usize>,
    on: [bool; 4],
    popups: Vec<Popup>,
    next_popup: u64,
    jumped: bool,
    hover_since: Option<std::time::Instant>,
    /// The hover underline under each plate, and when its hover began.
    marks: [Option<std::time::Instant>; 4],
    leftover: [bool; 4],
}

fn build(defect: Defect, window: &mut Window, cx: &mut App) -> AnyView {
    super::declare_script(SCRIPT, cx);
    super::storm::declare_keys(&["x"], cx);
    let view = cx.new(|cx| Bench {
        defect,
        ghost: cx.focus_handle(),
        motion: Motion::new(),
        focus: cx.focus_handle(),
        plates: (0..NAMES.len())
            .map(|index| {
                cx.focus_handle()
                    .tab_index(isize::try_from(index).unwrap_or(0))
                    .tab_stop(true)
            })
            .collect(),
        hovered: None,
        pressed: None,
        on: [false; 4],
        popups: Vec::new(),
        next_popup: 0,
        jumped: false,
        hover_since: None,
        marks: [None; 4],
        leftover: [false; 4],
    });
    let focus = view.read(cx).focus.clone();
    window.focus(&focus, cx);
    view.into()
}

impl Bench {
    fn open_popup(&mut self, parent: Option<u64>) {
        if self.popups.iter().filter(|popup| !popup.leaving).count() >= 3 {
            return;
        }
        let id = self.next_popup;
        self.next_popup += 1;
        self.popups.push(Popup {
            id,
            parent,
            leaving: false,
        });
    }

    /// Esc: the topmost live popup leaves. Returns whether one did.
    fn step_back(&mut self) -> bool {
        match self.popups.iter_mut().rev().find(|popup| !popup.leaving) {
            Some(popup) => {
                popup.leaving = true;
                true
            }
            None => false,
        }
    }

    fn close_all(&mut self) {
        for popup in &mut self.popups {
            popup.leaving = true;
        }
    }

    fn focused_plate(&self, window: &Window) -> Option<usize> {
        self.plates.iter().position(|handle| handle.is_focused(window))
    }

    fn key(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let stroke = &event.keystroke;
        let chord = (
            stroke.modifiers.platform,
            stroke.modifiers.shift,
            stroke.key.as_str(),
        );
        match chord {
            (false, false, "tab") => window.focus_next(cx),
            (false, true, "tab") => window.focus_prev(cx),
            (false, _, "j" | "down") => self.walk(1, window, cx),
            (false, _, "k" | "up") => self.walk(-1, window, cx),
            (true, _, "k") => {
                self.open_popup(None);
                if self.defect == Defect::DeadFocus {
                    window.focus(&self.ghost, cx);
                }
            }
            (false, _, "escape") => {
                if !self.step_back() {
                    window.focus(&self.focus, cx);
                }
            }
            (false, _, "enter" | "space") => {
                if let Some(index) = self.focused_plate(window) {
                    self.on[index] = !self.on[index];
                }
            }
            (false, _, "x") if self.defect == Defect::Panic => {
                panic!("canary-panic: the bench was told to panic on `x`")
            }
            _ => return,
        }
        cx.notify();
    }

    fn walk(&self, step: isize, window: &mut Window, cx: &mut App) {
        let count = isize::try_from(self.plates.len()).unwrap_or(1);
        let next = self.focused_plate(window).map_or(0, |index| {
            (isize::try_from(index).unwrap_or(0) + step).rem_euclid(count)
        });
        if let Some(handle) = self.plates.get(usize::try_from(next).unwrap_or(0)) {
            window.focus(handle, cx);
        }
    }

    fn stack(&self, opacities: &[(u64, f32)]) -> StackSample {
        StackSample {
            layer: "bench".to_owned(),
            entries: self
                .popups
                .iter()
                .map(|popup| {
                    let opacity = opacities
                        .iter()
                        .find(|(id, _)| *id == popup.id)
                        .map_or(0.0, |(_, value)| *value);
                    StackEntry {
                        key: format!("popup-{}", popup.id),
                        kind: "menu".to_owned(),
                        parent: popup.parent.map(|parent| format!("popup-{parent}")),
                        phase: if popup.leaving {
                            StackPhase::Leaving
                        } else if opacity < 1.0 {
                            StackPhase::Entering
                        } else {
                            StackPhase::Open
                        },
                        pinned: false,
                        bounds: None,
                    }
                })
                .collect(),
        }
    }
}

impl Render for Bench {
    #[allow(clippy::too_many_lines)]
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let facet = cx.facet();
        let palette = facet.palette();
        let scale = facet.text_scale;
        let gap = match facet.density {
            Density::Comfortable => 20.0,
            Density::Compact => 14.0,
            Density::Dense => 10.0,
        };
        if self.defect == Defect::LeakedFrames {
            window.request_animation_frame();
        }
        if self.defect == Defect::SlowFrame && !self.popups.is_empty() {
            // An expensive layout on every frame a popup is up.
            let started = std::time::Instant::now();
            while started.elapsed() < std::time::Duration::from_millis(150) {
                std::hint::spin_loop();
            }
        }
        let focused = self.focused_plate(window);
        let plates = NAMES
            .iter()
            .enumerate()
            .map(|(index, name)| {
                let hovered = self.hovered == Some(index);
                let pressed = self.pressed == Some(index);
                let key = ("bench.lift", index);
                let mut lift =
                    self.motion
                        .animate(key, if hovered { -4.0 } else { 0.0 }, spec::LIFT, window, cx);
                if self.defect == Defect::Jump && hovered && lift < -2.0 && !self.jumped {
                    self.jumped = true;
                    self.motion.set(key, 0.0);
                    lift = 0.0;
                }
                let press = self.motion.animate(
                    ("bench.press", index),
                    if pressed { 1.0 } else { 0.0 },
                    spec::PRESS,
                    window,
                    cx,
                );
                // The plate's shadow moves with it: one group.
                let shadow = probe::grouped(format!("bench.plate-{index}"), || {
                    let lift_again = self.motion.animate(
                        ("bench.shadow", index),
                        if hovered { 1.0 } else { 0.0 },
                        spec::LIFT,
                        window,
                        cx,
                    );
                    let lag = if self.defect == Defect::LaggingGroup && hovered {
                        // Start the second half of the pair 20 ms late.
                        let now = crate::motion::now(cx);
                        let since = *self.hover_since.get_or_insert(now);
                        now.saturating_duration_since(since)
                            >= std::time::Duration::from_millis(20)
                    } else {
                        true
                    };
                    let echo = self.motion.animate(
                        ("bench.echo", index),
                        if hovered && lag { 1.0 } else { 0.0 },
                        spec::LIFT,
                        window,
                        cx,
                    );
                    (lift_again, echo)
                });
                let label_box = if self.defect == Defect::ClippedText && index == 1 {
                    div().w(px(28.0)).overflow_hidden().whitespace_nowrap()
                } else {
                    div().whitespace_nowrap()
                };
                let border = if focused == Some(index) {
                    palette.peri.base.hsla()
                } else if self.on[index] {
                    palette.mint.base.hsla()
                } else {
                    palette.line2.hsla()
                };
                let fill = if press > 0.5 { palette.plate3 } else { palette.plate2 };
                // The painted face lifts; the hit area (hover, press, focus)
                // stays in its slot, so a pointer near an edge does not fall
                // off a plate that moved away from under it.
                let face = div()
                    .size_full()
                    .px(px(14.0))
                    .flex()
                    .items_center()
                    .bg(fill)
                    .border_1()
                    .border_color(border)
                    .opacity(0.9 + 0.1 * shadow.0.max(shadow.1).clamp(0.0, 1.0))
                    .child(probe::text(
                        ("bench.label", index),
                        *name,
                        ty::MONO_ROW,
                        scale,
                        TextOverflow::Clip,
                        label_box.typeset(ty::MONO_ROW, &facet).child(*name),
                    ));
                let plate = div()
                    .id(("bench.plate", index))
                    .relative()
                    .track_focus(&self.plates[index])
                    .w(px(PLATE_W))
                    .h(px(PLATE_H))
                    .on_hover(cx.listener(move |this, hovered: &bool, _, cx| {
                        let now = crate::motion::now(cx);
                        if *hovered {
                            this.marks[index] = Some(now);
                        } else if let Some(began) = this.marks[index].take() {
                            // The race: a short hover "forgets" to clear.
                            this.leftover[index] = this.defect == Defect::HoverRace
                                && now.saturating_duration_since(began)
                                    < std::time::Duration::from_millis(120);
                        }
                        if *hovered {
                            this.hovered = Some(index);
                        } else if this.hovered == Some(index)
                            && this.defect != Defect::StuckHover
                        {
                            this.hovered = None;
                            this.hover_since = None;
                        }
                        cx.notify();
                    }))
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, _, window, cx| {
                            this.pressed = Some(index);
                            window.focus(&this.plates[index], cx);
                            cx.stop_propagation();
                            cx.notify();
                        }),
                    )
                    .on_mouse_up(
                        MouseButton::Left,
                        cx.listener(move |this, _, _, cx| {
                            if this.pressed == Some(index) {
                                this.on[index] = !this.on[index];
                            }
                            this.pressed = None;
                            cx.notify();
                        }),
                    )
                    .on_mouse_up_out(
                        MouseButton::Left,
                        cx.listener(|this, _, _, cx| {
                            this.pressed = None;
                            cx.notify();
                        }),
                    )
                    .child(offset(face).y(px(lift)))
                    .when_live(self.marks[index].is_some() || self.leftover[index], |this| {
                        this.child(
                            div()
                                .absolute()
                                .bottom(px(0.0))
                                .left(px(0.0))
                                .w(px(PLATE_W))
                                .h(px(2.0))
                                .bg(palette.peri.base),
                        )
                    });
                let state = Target {
                    hovered,
                    pressed,
                    focused: focused == Some(index),
                    focusable: true,
                    clickable: true,
                };
                let mut column = div()
                    .flex()
                    .flex_col()
                    .gap(px(6.0))
                    .child(probe::target(format!("bench.plate-{index}"), state, plate));
                if facet.reveal.keys {
                    let cap = format!("{}", index + 1);
                    column = column.child(probe::text(
                        ("bench.cap", index),
                        cap.clone(),
                        ty::MONO_SMALL,
                        scale,
                        TextOverflow::Clip,
                        div()
                            .typeset(ty::MONO_SMALL, &facet)
                            .text_color(palette.ink2.hsla())
                            .child(cap),
                    ));
                }
                if facet.reveal.xray {
                    let caption = format!("{name} · {} uses", (index + 1) * 7);
                    column = column.child(probe::text(
                        ("bench.xray", index),
                        caption.clone(),
                        CAPTION,
                        scale,
                        TextOverflow::Wrap,
                        div()
                            .typeset(CAPTION, &facet)
                            .text_color(palette.ink2.hsla())
                            .child(caption),
                    ));
                }
                column
            })
            .collect::<Vec<_>>();

        // The popup stack: each popup fades and rises in, and fades out
        // before it is dropped.
        let mut opacities = Vec::new();
        let mut cards = Vec::new();
        let mut finished = Vec::new();
        for (depth, popup) in self.popups.iter().enumerate() {
            let target = if popup.leaving { 0.0 } else { 1.0 };
            let opacity = self.motion.animate_from(
                ("bench.popup", popup.id),
                0.0,
                target,
                spec::REVEAL,
                window,
                cx,
            );
            // The card rises 8 px into its laid-out place (layout motion:
            // it must settle exactly on its slot).
            let rest = if self.defect == Defect::OffSlot { 3.0 } else { 0.0 };
            let rise = self.motion.animate_from(
                ("bench.rise", popup.id),
                8.0,
                if popup.leaving { 8.0 } else { rest },
                spec::REVEAL,
                window,
                cx,
            );
            opacities.push((popup.id, opacity));
            if popup.leaving && opacity <= 0.0 && self.defect != Defect::StuckPopup {
                finished.push(popup.id);
                continue;
            }
            let id = popup.id;
            let depth_f = depth as f32;
            let leaving = popup.leaving;
            let card = probe::region(format!("popup-{id}"), || div()
                .id(("bench.card", id))
                .absolute()
                .left(px(40.0 + 170.0 * depth_f))
                .top(px(250.0 + 12.0 * depth_f))
                .w(px(220.0))
                .p(px(12.0))
                .flex()
                .flex_col()
                .gap(px(8.0))
                .bg(palette.plate3)
                .border_1()
                .border_color(palette.line2.hsla())
                .opacity(opacity.clamp(0.0, 1.0))
                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .child(probe::text(
                    ("bench.card.title", id),
                    "a popup",
                    ty::ROW,
                    scale,
                    TextOverflow::Wrap,
                    div().typeset(ty::ROW, &facet).child("a popup"),
                ))
                .child(
                    div()
                        .id(("bench.more", id))
                        .h(px(28.0))
                        .flex()
                        .items_center()
                        .text_color(palette.peri.base.hsla())
                        .when_live(!leaving, |this| {
                            this.on_click(cx.listener(move |this, _, _, cx| {
                                this.open_popup(Some(id));
                                cx.notify();
                            }))
                        })
                        .child(probe::text(
                            ("bench.more.label", id),
                            "more…",
                            ty::ROW,
                            scale,
                            TextOverflow::Wrap,
                            div().typeset(ty::ROW, &facet).child("more…"),
                        )),
                ));
            cards.push(probe::measure(
                format!("slot:bench.card-{id}"),
                offset(probe::measure(format!("paint:bench.card-{id}"), card)).y(px(rise)),
            ));
        }
        if !finished.is_empty() {
            self.popups.retain(|popup| !finished.contains(&popup.id));
            for id in finished {
                self.motion.replay(("bench.popup", id));
                self.motion.replay(("bench.rise", id));
            }
        }
        probe::record_stack(cx, || self.stack(&opacities));

        let mut extras = div().flex().gap(px(16.0)).items_center();
        if self.defect == Defect::LowContrast {
            extras = extras.child(probe::text(
                "bench.faint",
                "barely there",
                CAPTION,
                scale,
                TextOverflow::Wrap,
                div()
                    .typeset(CAPTION, &facet)
                    .text_color(palette.g2.hsla())
                    .child("barely there"),
            ));
        }
        if self.defect == Defect::SmallTarget {
            extras = extras.child(probe::target(
                "bench.dot",
                Target {
                    clickable: true,
                    ..Target::default()
                },
                div()
                    .id("bench.dot")
                    .w(px(12.0))
                    .h(px(12.0))
                    .bg(palette.peri.base)
                    .on_click(|_, _, _| {}),
            ));
        }
        if self.defect == Defect::Overlap {
            extras = extras.child(
                div()
                    .relative()
                    .w(px(160.0))
                    .h(px(20.0))
                    .child(div().absolute().left(px(0.0)).child(probe::text(
                        "bench.overlap.a",
                        "left label",
                        ty::ROW,
                        scale,
                        TextOverflow::Wrap,
                        div().typeset(ty::ROW, &facet).child("left label"),
                    )))
                    .child(div().absolute().left(px(30.0)).child(probe::text(
                        "bench.overlap.b",
                        "right label",
                        ty::ROW,
                        scale,
                        TextOverflow::Wrap,
                        div().typeset(ty::ROW, &facet).child("right label"),
                    ))),
            );
        }
        let mut parked = None;
        if self.defect == Defect::Offscreen {
            // On the root: Taffy places an absolute child against its
            // direct parent.
            parked = Some(probe::target(
                "bench.parked",
                Target {
                    focusable: true,
                    clickable: true,
                    ..Target::default()
                },
                div()
                    .id("bench.parked")
                    .absolute()
                    .left(px(-80.0))
                    .top(px(0.0))
                    .w(px(40.0))
                    .h(px(28.0))
                    .bg(palette.plate2),
            ));
        }
        if self.defect == Defect::WallClock {
            // Reads real time (microseconds: macOS has no finer clock), so
            // two captures of one script differ.
            let micros = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |since| since.subsec_micros());
            extras = extras.child(
                div()
                    .w(px(20.0 + f32::from(u16::try_from(micros % 40).unwrap_or(0))))
                    .h(px(6.0))
                    .bg(palette.amber.base),
            );
        }
        if self.defect == Defect::UnrequestedMotion {
            // Reads the clock directly and never asks for a frame.
            let seconds = crate::motion::now(cx)
                .saturating_duration_since(crate::motion::epoch(cx))
                .as_secs_f32();
            let drift = (seconds * 40.0).min(120.0);
            extras = extras.child(offset(probe::measure(
                "bench.drift",
                div().w(px(24.0)).h(px(24.0)).bg(palette.amber.base),
            ))
            .x(px(drift)));
            probe::record_track(cx, || probe::TrackSample {
                key: "bench.drift".to_owned(),
                kind: probe::TrackKind::Tween,
                value: drift,
                target: 120.0,
                velocity: 40.0,
                started_ms: 0.0,
                budget_ms: 3_000.0,
                at_ms: f64::from(seconds) * 1000.0,
                live: drift < 120.0,
                overshoot_ratio: 0.0,
                group: None,
            });
        }

        // The title fades in on boot.
        let fade = self
            .motion
            .animate_from("bench.title", 0.0, 1.0, spec::REVEAL, window, cx);
        let title = if self.defect == Defect::Residue && !crate::motion::reduced(cx) {
            fade * 0.97
        } else {
            fade
        };
        div()
            .id("harness-bench")
            .track_focus(&self.focus)
            .size_full()
            .relative()
            .bg(palette.g1)
            .p(px(40.0))
            .flex()
            .flex_col()
            .gap(px(gap))
            .on_key_down(cx.listener(Self::key))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, _, cx| {
                    this.close_all();
                    cx.notify();
                }),
            )
            .child(probe::text(
                "bench.title",
                "harness bench",
                ty::HEAD,
                scale,
                TextOverflow::Wrap,
                div()
                    .typeset(ty::HEAD, &facet)
                    .text_color(palette.ink1.hsla())
                    .opacity(title)
                    .child("harness bench"),
            ))
            .child(div().flex().flex_wrap().gap(px(gap)).children(plates))
            .child(
                div()
                    .flex()
                    .gap(px(12.0))
                    .items_center()
                    .child(probe::target(
                        "bench.trigger",
                        Target {
                            clickable: true,
                            ..Target::default()
                        },
                        div()
                            .id("bench.trigger")
                            .h(px(28.0))
                            .px(px(12.0))
                            .flex()
                            .items_center()
                            .bg(palette.plate2)
                            .border_1()
                            .border_color(palette.line2.hsla())
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.open_popup(None);
                                cx.notify();
                            }))
                            .child(probe::text(
                                "bench.trigger.label",
                                "open (⌘K)",
                                ty::BUTTON,
                                scale,
                                TextOverflow::Wrap,
                                div().typeset(ty::BUTTON, &facet).child("open (⌘K)"),
                            )),
                    ))
                    .child(extras),
            )
            .children(cards)
            .children(parked)
    }
}

/// `when` for stateful divs without pulling in the fluent trait's generics.
trait WhenLive: Sized {
    fn when_live(self, condition: bool, then: impl FnOnce(Self) -> Self) -> Self {
        if condition { then(self) } else { self }
    }
}

impl<T> WhenLive for T {}
