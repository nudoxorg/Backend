//! The splitter: the seam between two regions that can be dragged.
//!
//! At rest it is nothing but the hit zone (the region's own hairline is the
//! seam); under the pointer a periwinkle bar and a cut-stone grip rise out of
//! it. While dragging:
//!
//! - the panel follows the pointer through a snappy spring (weight, not
//!   lag), and a readout plate beside the pointer says the width;
//! - past the maximum the panel resists (it keeps moving, less and less) and
//!   the readout turns amber; let go and it settles back to the limit;
//! - below the minimum, a panel that can collapse snaps to its spine as the
//!   pointer crosses the collapse line (and springs back out if the pointer
//!   returns); the readout says so;
//! - release commits and the width settles with a critically damped spring.
//!
//! Double-click resets to the default; with keyboard focus ←/→ nudge by 8 px
//! (⇧ 32), Home/End jump to the limits, Enter collapses or restores.
//!
//! The drag lives in a per-window store keyed by the splitter's id, so the
//! panel it sizes can ask [`split_width`] for this frame's width anywhere in
//! the tree before building its own content.

use super::state::track;
use crate::Set;
use crate::measure::Measure;
use crate::motion::{Motion, SNAPPY, spec};
use crate::paint::geom::{Fill, Poly};
use crate::paint::{Bevel, Chamfer, Edge, Plate, cut, mix};
use crate::theme::ActiveFacet;
use crate::tokens::{Face, TypeRole, geo};
use gpui::{
    App, ClickEvent, ColorExt, DispatchPhase, ElementId, EntityId, FocusHandle, Global, Hsla,
    InteractiveElement, IntoElement, KeyDownEvent, MouseButton, MouseDownEvent, MouseMoveEvent,
    MouseUpEvent, ParentElement, RenderOnce, SharedString, StatefulInteractiveElement, Styled,
    Window, canvas, div, px,
};
use std::collections::HashMap;
use std::rc::Rc;

/// A resizable panel's limits and current width, in logical px.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SplitModel {
    /// The committed width.
    pub width: f32,
    /// Narrowest while open.
    pub min: f32,
    /// Widest.
    pub max: f32,
    /// Double-click restores this.
    pub default: f32,
    /// Collapsed to this width (the spine); `None` never collapses.
    pub collapsed: Option<f32>,
    /// Whether it is collapsed now.
    pub is_collapsed: bool,
}

impl SplitModel {
    /// The shelf: 264 px, 200–420, collapsing to the 42 px spine.
    #[must_use]
    pub fn shelf(width: f32, is_collapsed: bool) -> Self {
        Self {
            width,
            min: f32::from(geo::SHELF_MIN),
            max: f32::from(geo::SHELF_MAX),
            default: f32::from(geo::SHELF),
            collapsed: Some(f32::from(geo::KSPINE)),
            is_collapsed,
        }
    }

    /// The width the panel should settle at.
    #[must_use]
    pub fn resting(&self) -> f32 {
        match (self.is_collapsed, self.collapsed) {
            (true, Some(spine)) => spine,
            _ => self.width.clamp(self.min, self.max),
        }
    }

    /// Where a collapsing panel gives way: halfway between the spine and
    /// the minimum.
    #[must_use]
    pub fn collapse_line(&self) -> Option<f32> {
        self.collapsed.map(|spine| (spine + self.min) * 0.5)
    }
}

/// What a drag, a click or a key asks for.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum SplitEvent {
    /// A new committed width (already inside the limits).
    Resize(f32),
    /// Collapse to the spine.
    Collapse,
    /// Restore from the spine to the committed width.
    Expand,
    /// Back to the default width (double-click).
    Reset,
}

/// How far past a limit the panel may be pulled, px (it never gets there).
const GIVE: f32 = 28.0;

/// Past a limit the panel keeps moving, less and less.
fn resist(over: f32) -> f32 {
    GIVE * (1.0 - (-over / GIVE).exp())
}

/// Where a drag has pulled the panel: its displayed width, the width a
/// release would commit, and whether it has crossed the collapse line.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Pull {
    /// The width to show now (resisting past the limits).
    pub shown: f32,
    /// The width a release commits (inside the limits).
    pub commit: f32,
    /// Past the collapse line.
    pub collapse: bool,
    /// Pressing on a limit.
    pub at_limit: bool,
}

/// Resolves a raw pointer-derived width against a model.
#[must_use]
pub fn pull(model: &SplitModel, raw: f32) -> Pull {
    if let (Some(spine), Some(line)) = (model.collapsed, model.collapse_line())
        && raw < line
    {
        return Pull {
            shown: spine,
            commit: model.width.clamp(model.min, model.max),
            collapse: true,
            at_limit: false,
        };
    }
    if raw > model.max {
        Pull {
            shown: model.max + resist(raw - model.max),
            commit: model.max,
            collapse: false,
            at_limit: true,
        }
    } else if raw < model.min {
        Pull {
            shown: model.min - resist(model.min - raw),
            commit: model.min,
            collapse: false,
            at_limit: true,
        }
    } else {
        Pull {
            shown: raw,
            commit: raw,
            collapse: false,
            at_limit: false,
        }
    }
}

/// Which side of the handle the panel is on (its width grows toward the
/// handle).
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Hash)]
pub enum PanelSide {
    /// The panel is left of the handle (the shelf).
    #[default]
    Left,
    /// The panel is right of the handle (the pins column).
    Right,
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct Dragging {
    origin: f32,
    start: f32,
    now: f32,
    y: f32,
}

struct Split {
    focus: FocusHandle,
    hovered: bool,
    drag: Option<Dragging>,
    view: Option<EntityId>,
}

#[derive(Default)]
struct Splits(HashMap<ElementId, Split>);

impl Global for Splits {}

fn split<'a>(id: &ElementId, cx: &'a mut App) -> &'a mut Split {
    let focus = cx.focus_handle().tab_stop(true);
    cx.default_global::<Splits>()
        .0
        .entry(id.clone())
        .or_insert_with(|| Split {
            focus,
            hovered: false,
            drag: None,
            view: None,
        })
}

fn update(id: &ElementId, cx: &mut App, f: impl FnOnce(&mut Split) -> bool) {
    let (changed, view) = {
        let state = split(id, cx);
        (f(state), state.view)
    };
    if changed && let Some(view) = view {
        cx.notify(view);
    }
}

fn motion(id: &ElementId, cx: &mut App) -> Motion {
    Motion::scoped(ElementId::NamedChild(std::sync::Arc::new(id.clone()), "split".into()), cx)
}

fn raw_width(drag: &Dragging, side: PanelSide) -> f32 {
    let delta = drag.now - drag.origin;
    drag.start + if side == PanelSide::Left { delta } else { -delta }
}

/// This frame's width for the panel the splitter `id` sizes: following the
/// drag through a spring, then settling on the committed width.
pub fn split_width(
    id: &ElementId,
    model: &SplitModel,
    side: PanelSide,
    window: &mut Window,
    cx: &mut App,
) -> gpui::Pixels {
    let drag = split(id, cx).drag;
    let (target, follow) = drag.map_or((model.resting(), false), |drag| {
        let pulled = pull(model, raw_width(&drag, side));
        (pulled.shown, !pulled.collapse)
    });
    let spec = if follow {
        super::GRIP.into()
    } else if drag.is_some() {
        SNAPPY.into()
    } else {
        spec::SETTLE
    };
    px(motion(id, cx).animate(track(id, "width"), target, spec, window, cx))
}

type Change = Rc<dyn Fn(SplitEvent, &mut Window, &mut App)>;

/// The draggable seam: `splitter("shelf-split", model, &measure)`.
#[derive(IntoElement)]
pub struct Splitter {
    id: ElementId,
    model: SplitModel,
    side: PanelSide,
    measure: Measure,
    on_change: Option<Change>,
}

/// A splitter for `model`, the panel on the left.
#[must_use]
pub fn splitter(id: impl Into<ElementId>, model: SplitModel, measure: &Measure) -> Splitter {
    Splitter {
        id: id.into(),
        model,
        side: PanelSide::Left,
        measure: *measure,
        on_change: None,
    }
}

impl Splitter {
    /// Which side the panel is on.
    #[must_use]
    pub const fn side(mut self, side: PanelSide) -> Self {
        self.side = side;
        self
    }

    /// Called when a release, a double-click or a key commits a change.
    #[must_use]
    pub fn on_change(mut self, handler: impl Fn(SplitEvent, &mut Window, &mut App) + 'static) -> Self {
        self.on_change = Some(Rc::new(handler));
        self
    }
}

const READOUT: TypeRole = TypeRole {
    face: Face::Mono,
    weight: 600.0,
    size: 12.0,
    line: 16.0,
    tracking: 0.0,
    italic: false,
};

const UNIT: TypeRole = TypeRole {
    face: Face::Mono,
    weight: 400.0,
    size: 10.5,
    line: 16.0,
    tracking: 0.0,
    italic: false,
};

impl RenderOnce for Splitter {
    #[allow(clippy::too_many_lines)]
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let palette = cx.palette();
        let measure = self.measure;
        let id = self.id.clone();
        let side = self.side;
        let model = self.model;
        let view = window.current_view();
        let (focus, hovered, drag) = {
            let state = split(&id, cx);
            state.view = Some(view);
            (state.focus.clone(), state.hovered, state.drag)
        };
        let focused = focus.is_focused(window) && window.last_input_was_keyboard();
        let motion = motion(&id, cx);
        let lit = motion.animate(
            track(&id, "lit"),
            if hovered || drag.is_some() || focused { 1.0 } else { 0.0 },
            spec::REVEAL,
            window,
            cx,
        );
        let pulled = drag.map(|drag| pull(&model, raw_width(&drag, side)));
        let readout_t = motion.animate(
            track(&id, "readout"),
            if drag.is_some() { 1.0 } else { 0.0 },
            spec::REVEAL,
            window,
            cx,
        );
        let limit_t = motion.animate(
            track(&id, "limit"),
            if pulled.is_some_and(|p| p.at_limit) { 1.0 } else { 0.0 },
            spec::HOVER,
            window,
            cx,
        );
        let s = measure.scale();
        let hit = f32::from(geo::SPLIT_HIT) * s.max(1.0);

        let peri: Hsla = palette.peri.base.into();
        let peri_hi: Hsla = palette.peri_hi.into();
        let amber: Hsla = palette.amber.base.into();
        let bar_color = mix(peri, amber, limit_t);
        let grip_color = mix(peri_hi, amber, limit_t);
        let bar = canvas(
            |_, _, _| {},
            move |bounds, (), window, _| {
                if lit <= 0.0 {
                    return;
                }
                let cx0 = f32::from(bounds.center().x);
                let (y0, h) = (f32::from(bounds.origin.y), f32::from(bounds.size.height));
                let width = 1.0 + 2.0 * lit;
                let mut fill = Fill::new();
                fill.poly(&Poly::rect(cx0 - width * 0.5, y0, width, h));
                fill.paint(window, bar_color.opacity(lit));
                // The grip: a cut stone at the middle.
                let (gw, gh) = (5.0 * s, 28.0 * s);
                let gy = y0 + (h - gh) * 0.5;
                let grip = Poly::chamfer(cx0 - gw * 0.5, gy, gw, gh, 2.0 * s);
                let mut fill = Fill::new();
                fill.poly(&grip);
                fill.paint(window, grip_color.opacity(lit));
            },
        )
        .size_full();

        // The readout: a small cut plate beside the pointer while dragging.
        let readout = pulled.filter(|_| readout_t > 0.0).map(|pulled| {
            let (words, unit) = if pulled.collapse {
                (SharedString::from("spine"), SharedString::default())
            } else {
                #[allow(clippy::cast_possible_truncation)]
                let shown = pulled.commit.round() as i32;
                (SharedString::from(shown.to_string()), SharedString::from("px"))
            };
            let bevel = Edge::of(Bevel::Peri, palette).mix(Edge::of(Bevel::Amber, palette), limit_t);
            let y = drag.map_or(0.0, |drag| drag.y);
            div()
                .absolute()
                .left(px(hit * 0.5 + 6.0 * s))
                .top(px(y - 14.0 * s))
                .child(
                    cut()
                        .chamfer(Chamfer::Px(6.0 * s))
                        .edge(bevel)
                        .plate(Plate::Flat)
                        .fill(palette.plate3)
                        .floating()
                        .flex()
                        .items_baseline()
                        .gap(px(4.0 * s))
                        .px(px(10.0 * s))
                        .py(px(5.0 * s))
                        .opacity(readout_t)
                        .child(
                            div()
                                .set(READOUT, &measure)
                                .text_color(palette.ink0.hsla())
                                .whitespace_nowrap()
                                .child(words),
                        )
                        .child(
                            div()
                                .set(UNIT, &measure)
                                .text_color(palette.ink3.hsla())
                                .child(unit),
                        ),
                )
        });

        // While dragging, the whole window moves the seam.
        let listen = {
            let id = id.clone();
            let on_change = self.on_change.clone();
            canvas(
                |_, _, _| {},
                move |bounds, (), window, _| {
                    let top = f32::from(bounds.origin.y);
                    let moved = id.clone();
                    window.on_mouse_event(move |event: &MouseMoveEvent, phase, _window, cx| {
                        if phase != DispatchPhase::Bubble {
                            return;
                        }
                        let x = f32::from(event.position.x);
                        let y = f32::from(event.position.y) - top;
                        update(&moved, cx, |state| match state.drag {
                            Some(drag) if drag.now != x || drag.y != y => {
                                state.drag = Some(Dragging { now: x, y, ..drag });
                                true
                            }
                            _ => false,
                        });
                    });
                    let released = id.clone();
                    let change = on_change.clone();
                    window.on_mouse_event(move |_event: &MouseUpEvent, phase, window, cx| {
                        if phase != DispatchPhase::Bubble {
                            return;
                        }
                        let Some(drag) = split(&released, cx).drag else {
                            return;
                        };
                        update(&released, cx, |state| {
                            state.drag = None;
                            true
                        });
                        let pulled = pull(&model, raw_width(&drag, side));
                        let event = if pulled.collapse {
                            (!model.is_collapsed).then_some(SplitEvent::Collapse)
                        } else if model.is_collapsed {
                            Some(SplitEvent::Expand)
                        } else {
                            ((pulled.commit - model.width).abs() > 0.01)
                                .then_some(SplitEvent::Resize(pulled.commit))
                        };
                        if let (Some(event), Some(change)) = (event, &change) {
                            change(event, window, cx);
                        }
                    });
                },
            )
            .size_full()
        };

        let hovered_id = id.clone();
        let down_id = id.clone();
        let click_change = self.on_change.clone();
        let key_change = self.on_change.clone();
        let mut handle = div()
            .id(id.clone())
            .relative()
            .flex_none()
            .h_full()
            .w(px(hit))
            .mx(px(-hit * 0.5))
            .cursor_col_resize()
            .track_focus(&focus)
            .tab_index(0)
            .child(div().absolute().inset_0().child(bar))
            .on_hover(move |inside, _window, cx| {
                let inside = *inside;
                update(&hovered_id, cx, |state| {
                    let changed = state.hovered != inside;
                    state.hovered = inside;
                    changed
                });
            })
            .on_mouse_down(MouseButton::Left, move |event: &MouseDownEvent, _window, cx| {
                let x = f32::from(event.position.x);
                let start = model.resting();
                update(&down_id, cx, |state| {
                    state.drag = Some(Dragging {
                        origin: x,
                        start,
                        now: x,
                        y: 0.0,
                    });
                    true
                });
            })
            .on_click(move |event: &ClickEvent, window, cx| {
                if event.click_count() == 2
                    && let Some(change) = &click_change
                {
                    change(SplitEvent::Reset, window, cx);
                }
            })
            .on_key_down(move |event: &KeyDownEvent, window, cx| {
                let step = if event.keystroke.modifiers.shift { 32.0 } else { 8.0 };
                let toward = if side == PanelSide::Left { 1.0 } else { -1.0 };
                let event = match event.keystroke.key.as_str() {
                    "right" => Some(SplitEvent::Resize((model.width + step * toward).clamp(model.min, model.max))),
                    "left" => Some(SplitEvent::Resize((model.width - step * toward).clamp(model.min, model.max))),
                    "home" => Some(SplitEvent::Resize(model.min)),
                    "end" => Some(SplitEvent::Resize(model.max)),
                    "enter" if model.collapsed.is_some() => Some(if model.is_collapsed {
                        SplitEvent::Expand
                    } else {
                        SplitEvent::Collapse
                    }),
                    _ => None,
                };
                if let (Some(event), Some(change)) = (event, &key_change) {
                    change(event, window, cx);
                    cx.stop_propagation();
                }
            });
        if drag.is_some() {
            handle = handle.child(div().absolute().inset_0().child(listen));
        }
        handle.children(readout)
    }
}

#[cfg(test)]
mod tests {
    use super::{GIVE, SplitModel, pull};

    #[test]
    fn a_pull_inside_the_limits_is_the_pointer() {
        let model = SplitModel::shelf(264.0, false);
        let p = pull(&model, 300.0);
        assert!((p.shown - 300.0).abs() < 1e-4 && (p.commit - 300.0).abs() < 1e-4);
        assert!(!p.at_limit && !p.collapse);
    }

    #[test]
    fn past_the_maximum_the_panel_resists_and_commits_the_limit() {
        let model = SplitModel::shelf(264.0, false);
        let near = pull(&model, 430.0);
        let far = pull(&model, 900.0);
        assert!(near.shown > 420.0 && near.shown < 430.0, "{near:?}");
        assert!(far.shown > near.shown && far.shown <= 420.0 + GIVE, "{far:?}");
        assert!((far.commit - 420.0).abs() < 1e-4 && far.at_limit);
    }

    #[test]
    fn crossing_the_collapse_line_snaps_to_the_spine() {
        let model = SplitModel::shelf(264.0, false);
        // Between the minimum and the line: resisting, not collapsing.
        let resisting = pull(&model, 150.0);
        assert!(!resisting.collapse && resisting.shown < 200.0 && resisting.shown > 150.0);
        let collapsing = pull(&model, 110.0);
        assert!(collapsing.collapse);
        assert!((collapsing.shown - 42.0).abs() < 1e-4);
        // A model that cannot collapse only resists.
        let fixed = SplitModel {
            collapsed: None,
            ..model
        };
        assert!(!pull(&fixed, 10.0).collapse);
    }
}
