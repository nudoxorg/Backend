//! Native fallback for a reading room too short to retain one action per
//! relation group. The ordinary canvas columns remain the default.
//!
//! This element is the last chrome child: it builds after this frame's Find,
//! card and footer measurements, lays its one native list out once, and gives
//! GraphFrame the actual row/proxy rectangles before the canvas paints.

use super::super::prism::{RailEntry, RailGeometry, RailPlan, RailRowBounds};
use super::*;
use std::collections::BTreeMap;

pub(super) struct Rail {
    pub(super) view: Entity<GraphView>,
}

impl IntoElement for Rail {
    type Element = Self;
    fn into_element(self) -> Self {
        self
    }
}

impl Element for Rail {
    type RequestLayoutState = ();
    type PrepaintState = Option<AnyElement>;
    fn id(&self) -> Option<ElementId> {
        Some("graph-prism-rail".into())
    }
    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }
    fn request_layout(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, ()) {
        let mut style = Style::default();
        style.size.width = relative(1.0).into();
        style.size.height = relative(1.0).into();
        (window.request_layout(style, [], cx), ())
    }
    fn prepaint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        _: Bounds<Pixels>,
        _: &mut (),
        window: &mut Window,
        cx: &mut App,
    ) -> Option<AnyElement> {
        let (plan, scroll, reveal) = self.view.update(cx, |graph, cx| {
            let plan = graph
                .prism
                .as_ref()
                .filter(|prism| prism.target > 0.0 && !graph.state.find_open)
                .zip(graph.view.as_ref())
                .and_then(|(prism, view)| {
                    let reserved: Vec<_> = graph.chrome_bounds.values().copied().collect();
                    prism::rail_plan(
                        prism,
                        view,
                        graph.card_bounds,
                        &reserved,
                        cx.facet().text_scale,
                    )
                });
            let mut reveal = None;
            if let Some(plan) = &plan {
                let owner_changed = graph
                    .rail_plan
                    .as_ref()
                    .is_none_or(|old| old.owner != plan.owner);
                let changed = owner_changed
                    || graph
                        .rail_plan
                        .as_ref()
                        .is_none_or(|old| old.viewport != plan.viewport)
                    || graph.rail_text_scale != Some(cx.facet().text_scale);
                if changed {
                    // A new viewport cannot inherit a pending child index
                    // resolved against the previous native viewport. Retain
                    // same-owner manual offset, then reveal with this layout.
                    let offset = graph.rail_scroll.offset();
                    graph.rail_scroll = ScrollHandle::new();
                    if !owner_changed {
                        graph.rail_scroll.set_offset(offset);
                    }
                    reveal = graph.state.selected.and_then(|key| plan.child_index(key));
                }
            } else if graph.rail_plan.is_some() {
                graph.rail_scroll = ScrollHandle::new();
            }
            graph.rail_text_scale = plan.as_ref().map(|_| cx.facet().text_scale);
            graph.rail_plan = plan.clone();
            graph.rail_geometry = None;
            (plan, graph.rail_scroll.clone(), reveal)
        });
        let plan = plan?;
        let measured = Rc::new(RefCell::new(NativeMeasurements::default()));
        let mut child = self.view.update(cx, |graph, cx| {
            native_rows(graph, &plan, &scroll, measured.clone(), cx)
        });
        child.layout_as_root(
            plan.viewport.size.map(gpui::AvailableSpace::Definite),
            window,
            cx,
        );
        if let Some(index) = reveal {
            // These ids belong to the one native list root, which is never
            // reparented. Read actual layout before its one prepaint; neither
            // row heights nor a stale ScrollHandle viewport are estimated.
            let measured = measured.borrow();
            if let Some((root, row)) = measured.root.zip(measured.layouts.get(&index).copied()) {
                let root = window.layout_bounds(root);
                let row = window.layout_bounds(row);
                let top = row.top() - root.top();
                let bottom = row.bottom() - root.top();
                let height = plan.viewport.size.height;
                let mut offset = scroll.offset();
                if row.size.height > height || top + offset.y < px(0.0) {
                    offset.y = -top;
                } else if bottom + offset.y > height {
                    offset.y = height - bottom;
                }
                scroll.set_offset(offset);
            }
        }
        child.prepaint_at(plan.viewport.origin, window, cx);
        let viewport = scroll.bounds();
        let rows = measured.borrow().rows.values().cloned().collect();
        self.view.update(cx, |graph, cx| {
            graph.rail_geometry = Some(RailGeometry {
                owner: plan.owner,
                viewport,
                rows,
            });
            graph
                .chrome_bounds
                .insert("graph-prism-rail-bounds", viewport);
            crate::probe::record_bounds(cx, &"graph-prism-rail-bounds".into(), viewport);
            if crate::probe::enabled(cx) {
                if let Some(mut content) = scroll.bounds_for_item(0) {
                    for index in 1..plan.entries.len() {
                        if let Some(row) = scroll.bounds_for_item(index) {
                            content = content.union(&row);
                        }
                    }
                    crate::probe::record_scroll(
                        cx,
                        &"graph-prism-rail-scroll".into(),
                        viewport,
                        content,
                    );
                }
            }
        });
        Some(child)
    }
    fn paint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        _: Bounds<Pixels>,
        _: &mut (),
        child: &mut Option<AnyElement>,
        window: &mut Window,
        cx: &mut App,
    ) {
        if let Some(child) = child {
            child.paint(window, cx);
        }
    }
}

#[derive(Default)]
struct NativeMeasurements {
    root: Option<LayoutId>,
    layouts: BTreeMap<usize, LayoutId>,
    rows: BTreeMap<usize, RailRowBounds>,
}
type Measurements = Rc<RefCell<NativeMeasurements>>;

/// Measurement follows ordinary native layout; no detached child is measured
/// and reparented, and no geometry is queried or changed during paint.
struct MeasuredRow {
    child: AnyElement,
    output: Measurements,
    index: Option<usize>,
    proxy: bool,
}
impl IntoElement for MeasuredRow {
    type Element = Self;
    fn into_element(self) -> Self {
        self
    }
}
impl Element for MeasuredRow {
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
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, ()) {
        let id = self.child.request_layout(window, cx);
        if !self.proxy {
            let mut output = self.output.borrow_mut();
            if let Some(index) = self.index {
                output.layouts.insert(index, id);
            } else {
                output.root = Some(id);
            }
        }
        (id, ())
    }
    fn prepaint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut (),
        window: &mut Window,
        cx: &mut App,
    ) {
        self.child.prepaint(window, cx);
        let Some(index) = self.index else {
            return;
        };
        let mut output = self.output.borrow_mut();
        let entry = output.rows.entry(index).or_insert(RailRowBounds {
            entry_index: index,
            row: bounds,
            proxy: None,
        });
        if self.proxy {
            entry.proxy = Some(bounds);
        } else {
            entry.row = bounds;
        }
    }
    fn paint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        _: Bounds<Pixels>,
        _: &mut (),
        _: &mut (),
        window: &mut Window,
        cx: &mut App,
    ) {
        self.child.paint(window, cx);
    }
}

fn native_rows(
    graph: &GraphView,
    plan: &RailPlan,
    scroll: &ScrollHandle,
    measured: Measurements,
    cx: &mut Context<GraphView>,
) -> AnyElement {
    let facet = cx.facet();
    let palette = facet.palette();
    let width = f32::from(plan.viewport.size.width);
    let measure = Measure::new(px((width - 30.0).max(1.0)), &facet);
    let mut backing = palette.g0.hsla();
    backing.alpha = 0.94;
    let owner = plan.owner;
    let mut list = div()
        .id("graph-prism-rail-scroll")
        .w(plan.viewport.size.width)
        .h(plan.viewport.size.height)
        .overflow_y_scroll()
        .track_scroll(scroll)
        .flex()
        .flex_col()
        .bg(backing)
        .opacity(plan.e)
        // Native scrolling updates geometry in the next ordinary draw. Canvas
        // wheel handling is excluded by this measured foreground viewport.
        .on_scroll_wheel(cx.listener(|_, _, _, cx| cx.notify()));
    for (index, entry) in plan.entries.iter().enumerate() {
        let content: AnyElement = match entry {
            RailEntry::Head { word, more } => {
                let text = if *more > 0 {
                    format!("{} · {more} more", word.text())
                } else {
                    word.text().to_owned()
                };
                div()
                    .w_full()
                    .flex_shrink_0()
                    .py(px(4.0))
                    .pl(px(23.0))
                    .pr(px(4.0))
                    .child(graph_text(
                        format!("graph-prism-rail-head-{index}"),
                        text,
                        prism::roles::HEAD,
                        &measure,
                        prism::rail_head_tone(*word, palette),
                        crate::probe::TextOverflow::Ellipsis,
                    ))
                    .into_any_element()
            }
            RailEntry::Row {
                key,
                kind,
                text,
                note,
                more,
                ..
            } => {
                let selected = key.is_some_and(|key| graph.state.selected == Some(key));
                let hovered = key.is_some_and(|key| {
                    graph
                        .hover_slot
                        .and_then(|slot| {
                            graph.frame.as_ref().and_then(|frame| frame.slots.get(slot))
                        })
                        .and_then(|slot| slot.key)
                        == Some(key)
                });
                let role = if *more > 0 {
                    prism::roles::MORE
                } else {
                    prism::roles::NAME
                };
                let mut label = div()
                    .min_w(px(0.0))
                    .flex_1()
                    .flex()
                    .flex_col()
                    .child(graph_text(
                        format!("graph-prism-rail-name-{index}"),
                        text.clone(),
                        role,
                        &measure,
                        if selected || hovered {
                            palette.ink1
                        } else {
                            palette.ink2
                        },
                        crate::probe::TextOverflow::Ellipsis,
                    ));
                if let Some(note) = note {
                    label = label.child(graph_text(
                        format!("graph-prism-rail-note-{index}"),
                        note.clone(),
                        prism::roles::WHERE,
                        &measure,
                        palette.ink2,
                        crate::probe::TextOverflow::Ellipsis,
                    ));
                }
                let mut row = div()
                    .id(("graph-prism-rail-row", index))
                    .w_full()
                    .flex_shrink_0()
                    .flex()
                    .items_center()
                    .gap(px(8.0))
                    .py(px(4.0))
                    .cursor_pointer()
                    .when_selected(selected, palette);
                if let Some(kind) = kind {
                    row = row.child(MeasuredRow {
                        child: icons::kind_mark(
                            crate::anatomy::icon_kind(*kind),
                            KindSize::Sm,
                            palette,
                        ),
                        output: measured.clone(),
                        index: Some(index),
                        proxy: true,
                    });
                } else {
                    row = row.child(div().w(px(15.0)).flex_none());
                }
                row = row.child(label);
                if let Some(key) = key.filter(|_| plan.e >= 0.8) {
                    row = row
                        .on_mouse_move(cx.listener(
                            move |graph, event: &MouseMoveEvent, window, cx| {
                                graph.rail_pointer(owner, key, event, window, cx)
                            },
                        ))
                        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                        .on_click(cx.listener(
                            move |graph, event: &gpui::ClickEvent, window, cx| {
                                if graph.moving
                                    || graph.state.focus != Some(owner)
                                    || graph.rail_geometry.as_ref().is_none_or(|geometry| {
                                        geometry.owner != owner
                                            || !geometry.viewport.contains(&event.position())
                                    })
                                {
                                    return;
                                }
                                let Some(slot) = graph
                                    .frame
                                    .as_ref()
                                    .filter(|frame| frame.node == owner && frame.e >= 0.8)
                                    .and_then(|frame| frame.locate(key))
                                else {
                                    return;
                                };
                                graph.state.apply(Event::Walk(slot, key));
                                window.focus(&graph.focus_handle, cx);
                                graph.set_focus(Some(key.node), true, cx);
                            },
                        ));
                }
                row.into_any_element()
            }
        };
        list = list.child(MeasuredRow {
            child: content,
            output: measured.clone(),
            index: Some(index),
            proxy: false,
        });
    }
    MeasuredRow {
        child: list.into_any_element(),
        output: measured,
        index: None,
        proxy: false,
    }
    .into_any_element()
}

impl GraphView {
    pub(super) fn rail_pointer(
        &mut self,
        owner: NodeId,
        key: prism::SlotKey,
        event: &MouseMoveEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.moving
            || self.state.focus != Some(owner)
            || self
                .frame
                .as_ref()
                .is_none_or(|frame| frame.node != owner || frame.e < 0.8)
            || self.rail_geometry.as_ref().is_none_or(|geometry| {
                geometry.owner != owner || !geometry.viewport.contains(&event.position)
            })
        {
            return;
        }
        let at = (f32::from(event.position.x), f32::from(event.position.y));
        let selection_changed = self.pointer != Some(at) && self.state.selected.is_some();
        if self.pointer != Some(at) {
            self.state.selected = None;
            self.state.prism_sel = None;
        }
        self.pointer = Some(at);
        let slot = self
            .frame
            .as_ref()
            .filter(|frame| self.state.focus == Some(frame.node))
            .and_then(|frame| frame.locate(key));
        let before = (self.hover, self.hover_slot);
        self.set_hover(Some(key.node), slot);
        self.sync_peek(window, cx);
        if selection_changed || before != (self.hover, self.hover_slot) {
            cx.notify();
        }
    }
}
