//! An element built inside layout, with the window at hand: a body that
//! answers this frame's hover target (the reach bar's scrub, the history's
//! label) needs the window, which a page's builders do not have.

use gpui::{
    AnyElement, App, Bounds, Element, ElementId, Global, GlobalElementId, InspectorElementId,
    IntoElement, LayoutId, MouseMoveEvent, Pixels, SharedString, Window,
};
use std::collections::HashSet;

/// An element built inside layout, with the window at hand.
pub(super) struct Lazy {
    build: Option<Box<dyn FnOnce(&mut Window, &mut App) -> AnyElement>>,
    child: Option<AnyElement>,
}

pub(super) fn lazy(build: impl FnOnce(&mut Window, &mut App) -> AnyElement + 'static) -> Lazy {
    Lazy {
        build: Some(Box::new(build)),
        child: None,
    }
}

impl IntoElement for Lazy {
    type Element = Self;
    fn into_element(self) -> Self::Element {
        self
    }
}

impl Element for Lazy {
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
        let mut child = self
            .build
            .take()
            .map_or_else(|| gpui::Empty.into_any_element(), |build| build(window, cx));
        let layout = child.request_layout(window, cx);
        self.child = Some(child);
        (layout, ())
    }

    fn prepaint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        _: Bounds<Pixels>,
        (): &mut (),
        window: &mut Window,
        cx: &mut App,
    ) {
        if let Some(child) = self.child.as_mut() {
            child.prepaint(window, cx);
        }
    }

    fn paint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        _: Bounds<Pixels>,
        (): &mut (),
        (): &mut (),
        window: &mut Window,
        cx: &mut App,
    ) {
        if let Some(child) = self.child.as_mut() {
            child.paint(window, cx);
        }
    }
}

// ------------------------------------------------------------------ spreads

/// The elements the pointer is over, by key (one process-wide set: only one
/// pointer).
#[derive(Default)]
struct Over(HashSet<SharedString>);

impl Global for Over {}

/// An element that knows whether the pointer is over it, **whatever is
/// inside it**: `build(true)` while the pointer is in its bounds (which
/// include what the build opened), else `build(false)`. A stack of marks
/// spreads into named tokens that are doors of their own: the one hover
/// grammar has a single target at a time, so the spread cannot be a
/// hoverable that holds hoverables; it watches the pointer itself.
pub(super) struct Spreads {
    key: SharedString,
    build: Option<Box<dyn FnOnce(bool) -> AnyElement>>,
    child: Option<AnyElement>,
}

/// See [`Spreads`].
pub(super) fn spreads(
    key: impl Into<SharedString>,
    build: impl FnOnce(bool) -> AnyElement + 'static,
) -> Spreads {
    Spreads {
        key: key.into(),
        build: Some(Box::new(build)),
        child: None,
    }
}

impl IntoElement for Spreads {
    type Element = Self;
    fn into_element(self) -> Self::Element {
        self
    }
}

impl Element for Spreads {
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
        let open = cx
            .try_global::<Over>()
            .is_some_and(|over| over.0.contains(&self.key));
        let mut child = self
            .build
            .take()
            .map_or_else(|| gpui::Empty.into_any_element(), |build| build(open));
        let layout = child.request_layout(window, cx);
        self.child = Some(child);
        (layout, ())
    }

    fn prepaint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        _: Bounds<Pixels>,
        (): &mut (),
        window: &mut Window,
        cx: &mut App,
    ) {
        if let Some(child) = self.child.as_mut() {
            child.prepaint(window, cx);
        }
    }

    fn paint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        (): &mut (),
        (): &mut (),
        window: &mut Window,
        cx: &mut App,
    ) {
        if let Some(child) = self.child.as_mut() {
            child.paint(window, cx);
        }
        let key = self.key.clone();
        window.on_mouse_event(move |event: &MouseMoveEvent, phase, window, cx| {
            if phase != gpui::DispatchPhase::Bubble {
                return;
            }
            let inside = bounds.contains(&event.position);
            let over = cx.default_global::<Over>();
            let now = over.0.contains(&key);
            if inside != now {
                if inside {
                    if over.0.len() >= 64 {
                        over.0.clear();
                    }
                    over.0.insert(key.clone());
                } else {
                    over.0.remove(&key);
                }
                window.refresh();
            }
        });
        let _ = cx;
    }
}
