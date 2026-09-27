//! Reserve a modifier hint's measured footprint without adding invisible
//! content to the native scroll range. The viewport stays fixed even when
//! only part of the hint would fit below the core details.
use gpui::{
    AnyElement, App, AvailableSpace, Bounds, Element, ElementId, GlobalElementId,
    InspectorElementId, InteractiveElement, IntoElement, LayoutId, ParentElement, Pixels,
    ScrollHandle, StatefulInteractiveElement, Styled, Window, div, px, size,
};
use std::{cell::Cell, rc::Rc};

#[derive(Clone, Copy, Debug, Default)]
pub(super) struct Metrics {
    pub core: f32,
    pub hint: f32,
    pub budget: f32,
    pub reserved: f32,
}

pub(super) struct StableHints {
    pub core: Option<AnyElement>,
    pub hint: Option<AnyElement>,
    pub shown: bool,
    pub width: f32,
    pub budget: f32,
    pub scroll: ScrollHandle,
    pub metrics: Rc<Cell<Metrics>>,
    viewport: Option<AnyElement>,
}
impl StableHints {
    pub fn new(
        core: impl IntoElement,
        hint: impl IntoElement,
        shown: bool,
        width: f32,
        budget: f32,
        scroll: ScrollHandle,
        metrics: Rc<Cell<Metrics>>,
    ) -> Self {
        Self {
            core: Some(core.into_any_element()),
            hint: Some(hint.into_any_element()),
            shown,
            width,
            budget,
            scroll,
            metrics,
            viewport: None,
        }
    }
}
impl IntoElement for StableHints {
    type Element = Self;
    fn into_element(self) -> Self {
        self
    }
}
impl Element for StableHints {
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
        let mut core = self.core.take().expect("core requested once");
        let mut hint = self.hint.take().expect("hint requested once");
        // Use exactly the namespace the eventual native scroll container uses.
        // Each child requests its layout once; attaching those same nodes to
        // the viewport lets the final parent layout compute their positions.
        let (core_id, hint_id, core_h, hint_h) =
            window.with_element_namespace("graph-card-scroll", |window| {
                let core_id = core.request_layout(window, cx);
                let hint_id = hint.request_layout(window, cx);
                let available = size(
                    AvailableSpace::Definite(px(self.width)),
                    AvailableSpace::MaxContent,
                );
                window.compute_layout(core_id, available, cx);
                let core_h = f32::from(window.layout_bounds(core_id).size.height);
                window.compute_layout(hint_id, available, cx);
                let hint_h = f32::from(window.layout_bounds(hint_id).size.height);
                // layout_bounds caches absolute standalone coordinates. A
                // same-root compute clears that cache before these nodes
                // are attached under the real scroll parent. Do not query
                // bounds again until the final parent has laid them out.
                window.compute_layout(core_id, available, cx);
                window.compute_layout(hint_id, available, cx);
                (core_id, hint_id, core_h, hint_h)
            });
        let reserved = (core_h + 10.0 + hint_h).min(self.budget);
        self.metrics.set(Metrics {
            core: core_h,
            hint: hint_h,
            budget: self.budget,
            reserved,
        });
        let mut body = div().flex().flex_col().gap(px(10.0)).child(Requested {
            child: core,
            layout: core_id,
        });
        if self.shown {
            body = body.child(Requested {
                child: hint,
                layout: hint_id,
            });
        }
        let mut viewport = div()
            .id("graph-card-scroll")
            .w(px(self.width))
            .track_scroll(&self.scroll)
            .min_h(px(reserved))
            .max_h(px(self.budget))
            .overflow_y_scroll()
            .child(body)
            .into_any_element();
        let layout = viewport.request_layout(window, cx);
        self.viewport = Some(viewport);
        (layout, ())
    }
    fn prepaint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        _: Bounds<Pixels>,
        _: &mut (),
        window: &mut Window,
        cx: &mut App,
    ) {
        self.viewport
            .as_mut()
            .expect("requested viewport")
            .prepaint(window, cx);
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
        self.viewport
            .as_mut()
            .expect("requested viewport")
            .paint(window, cx);
    }
}

/// An already requested child retains its original identity and performs the
/// ordinary prepaint/paint path; only the layout request is forwarded once.
struct Requested {
    child: AnyElement,
    layout: LayoutId,
}
impl IntoElement for Requested {
    type Element = Self;
    fn into_element(self) -> Self {
        self
    }
}
impl Element for Requested {
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
        _: &mut Window,
        _: &mut App,
    ) -> (LayoutId, ()) {
        (self.layout, ())
    }
    fn prepaint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        _: Bounds<Pixels>,
        _: &mut (),
        window: &mut Window,
        cx: &mut App,
    ) {
        self.child.prepaint(window, cx);
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
