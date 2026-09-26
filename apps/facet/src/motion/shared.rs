//! Shared elements: an element keyed the same in two places morphs between
//! them — a row's gem and name become the page hero (the descent), a hero
//! gem shrinks into its node in the graph while the camera flies out, and
//! the reverse of each.
//!
//! Every [`shared`] element records where it was painted, per window and
//! key. An element that appears for the first time (it has no element state
//! yet — a new route, a different view, a new parent) while another element
//! painted its key within the last second starts over those painted bounds
//! and glides to its own layout: the centre travels, the size scales through
//! the compositing layer, so text and paths are re-rasterized at every step,
//! never a stretched bitmap. The two ends may live in different views and
//! different parents; only the key ties them. A morph that is interrupted
//! (going back mid-descent) starts the next one from wherever the element is
//! painted mid-flight. If the destination moves while the morph runs (a
//! graph node under a flying camera), the morph follows it.
//!
//! [`shared_with`] builds the element knowing its morph progress, so the
//! arriving end can look like the departing one early on (a graph node drawn
//! as the hero gem it is shrinking from).
//!
//! ```ignore
//! // Symbol page:
//! shared(("gem", id), gem(kind).size(72.))
//! // Graph node, another view:
//! shared_with(("gem", id), move |m| node(kind).glyph(m.t < 0.6))
//! ```

use super::{epoch as motion_epoch, now, reduced, request_frame};
use crate::probe::{self, TrackKind, TrackSample};
use crate::tokens::motion::{Bezier, GLIDE, SCENE};
use gpui::{
    AnyElement, App, Bounds, ElementId, Global, GlobalElementId, InspectorElementId, IntoElement,
    LayerTransform, LayoutId, Pixels, Size, Window, WindowId, point,
};
use std::collections::HashMap;
use std::time::{Duration, Instant};

/// How the old size maps onto the new element.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Hash)]
pub enum Fit {
    /// Uniform scale matching heights (text: a 13 px name grows into a 44 px
    /// title without distortion).
    #[default]
    Height,
    /// Uniform scale matching widths.
    Width,
    /// Per-axis scale matching both (shapes whose aspect may change).
    Stretch,
}

/// A newcomer only morphs from a paint this recent.
const RECENT: Duration = Duration::from_millis(1_000);

#[derive(Clone, Debug)]
struct Painted {
    bounds: Bounds<Pixels>,
    owner: Option<GlobalElementId>,
    at: Instant,
}

#[derive(Default)]
struct Registry {
    painted: HashMap<(WindowId, ElementId), Painted>,
}

impl Global for Registry {}

/// A morph in flight.
#[derive(Clone, Copy, Debug)]
struct Morph {
    from: Bounds<Pixels>,
    start: Instant,
}

/// Kept in the element's own state: its morph, if one runs. Its presence
/// is what tells a returning element from a newcomer.
#[derive(Clone, Copy, Debug, Default)]
struct State {
    morph: Option<Morph>,
    /// Where its layout was last frame, and when (the destination may move
    /// while a morph runs: a graph node under a flying camera).
    laid: Option<(Bounds<Pixels>, Instant)>,
}

/// Where a morph stands, for [`shared_with`] builders.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Morphing {
    /// Eased progress: 0 over the departing element, 1 at rest.
    pub t: f32,
    /// The departing element's painted size, while morphing.
    pub from: Option<Size<Pixels>>,
}

impl Morphing {
    const REST: Self = Self { t: 1.0, from: None };

    /// The height this end is painted at now, if its own height is `own`
    /// (the default [`Fit::Height`] morph). Size-aware content (a gem that
    /// shows its glyph only when big enough) lays itself out at this height
    /// inside its own box and counter-scales by `own / painted`, so it draws
    /// exactly as a native element of the painted size.
    #[must_use]
    pub fn painted(&self, own: f32) -> f32 {
        self.from.map_or(own, |from| {
            f32::from(from.height) + (own - f32::from(from.height)) * self.t
        })
    }
}

type Build = Box<dyn FnOnce(Morphing) -> AnyElement>;

enum Content {
    Built(AnyElement),
    Deferred(Option<Build>),
}

/// An element that morphs from where its key was last painted. Build with
/// [`shared`] or [`shared_with`].
pub struct Shared {
    content: Content,
    key: ElementId,
    fit: Fit,
    duration: Duration,
    curve: Bezier,
    morph: Option<Morph>,
    transform: LayerTransform,
}

/// Records a real canvas endpoint in window coordinates, immediately before
/// handing its identity to a shared element. It obeys the same one-second
/// lifetime as element paints; it never impersonates an element owner.
/// Invalid or empty rectangles cannot seed a morph.
pub fn remember(key: impl Into<ElementId>, bounds: Bounds<Pixels>, window: &Window, cx: &mut App) {
    let values = [
        bounds.origin.x,
        bounds.origin.y,
        bounds.size.width,
        bounds.size.height,
    ];
    if values.iter().any(|value| !f32::from(*value).is_finite())
        || bounds.size.width <= gpui::px(0.0)
        || bounds.size.height <= gpui::px(0.0)
    {
        return;
    }
    let at = now(cx);
    cx.default_global::<Registry>().painted.insert(
        (window.window_handle().window_id(), key.into()),
        Painted {
            bounds,
            owner: None,
            at,
        },
    );
}

/// A disappeared canvas source must not leave a recent but now offscreen
/// endpoint that starts the new view from obsolete geometry.
pub fn forget(key: impl Into<ElementId>, window: &Window, cx: &mut App) {
    cx.default_global::<Registry>()
        .painted
        .remove(&(window.window_handle().window_id(), key.into()));
}

/// A captured endpoint retains its original identity, window and expiry.
/// Async layout cannot refresh an obsolete rectangle into a new source.
#[derive(Clone, Debug)]
pub struct Endpoint {
    key: ElementId,
    window: WindowId,
    painted: Painted,
}

/// Captures a recent actual paint without extending its lifetime.
#[must_use]
pub fn capture(key: impl Into<ElementId>, window: &Window, cx: &App) -> Option<Endpoint> {
    let key = key.into();
    let window = window.window_handle().window_id();
    let painted = cx
        .try_global::<Registry>()?
        .painted
        .get(&(window, key.clone()))?;
    (now(cx).saturating_duration_since(painted.at) <= RECENT).then(|| Endpoint {
        key,
        window,
        painted: painted.clone(),
    })
}

/// Restores a captured canvas handoff only for the requested exact identity,
/// original window and remaining lifetime. The original timestamp survives.
pub fn resume(
    endpoint: Endpoint,
    key: impl Into<ElementId>,
    window: &Window,
    cx: &mut App,
) -> bool {
    let key = key.into();
    let window = window.window_handle().window_id();
    if endpoint.key != key
        || endpoint.window != window
        || now(cx).saturating_duration_since(endpoint.painted.at) > RECENT
    {
        return false;
    }
    cx.default_global::<Registry>().painted.insert(
        (window, key),
        Painted {
            owner: None,
            ..endpoint.painted
        },
    );
    true
}

/// The recent actual bounds of this exact identity in this window.
#[must_use]
pub fn last_bounds(key: impl Into<ElementId>, window: &Window, cx: &App) -> Option<Bounds<Pixels>> {
    capture(key, window, cx).map(|endpoint| endpoint.painted.bounds)
}

/// Wraps `child` as the shared element `key`.
pub fn shared(key: impl Into<ElementId>, child: impl IntoElement) -> Shared {
    Shared::new(key.into(), Content::Built(child.into_any_element()))
}

/// The shared element `key`, built knowing its morph progress.
pub fn shared_with<E: IntoElement>(
    key: impl Into<ElementId>,
    build: impl FnOnce(Morphing) -> E + 'static,
) -> Shared {
    Shared::new(
        key.into(),
        Content::Deferred(Some(Box::new(move |morph| build(morph).into_any_element()))),
    )
}

impl Shared {
    fn new(key: ElementId, content: Content) -> Self {
        Self {
            content,
            key,
            fit: Fit::Height,
            duration: SCENE,
            curve: GLIDE,
            morph: None,
            transform: LayerTransform::IDENTITY,
        }
    }

    /// How the old size maps onto this element (default [`Fit::Height`]).
    #[must_use]
    pub fn fit(mut self, fit: Fit) -> Self {
        self.fit = fit;
        self
    }

    /// The morph's timing (default: the descent, 620 ms glide).
    #[must_use]
    pub fn timing(mut self, duration: Duration, curve: Bezier) -> Self {
        self.duration = duration;
        self.curve = curve;
        self
    }

    fn progress(&self, now: Instant) -> Option<(f32, f32)> {
        let morph = self.morph?;
        let run = now.saturating_duration_since(morph.start).as_secs_f32();
        let span = self.duration.as_secs_f32();
        let linear = if span <= 0.0 {
            1.0
        } else {
            (run / span).min(1.0)
        };
        (linear < 1.0).then(|| (linear, self.curve.ease(linear)))
    }

    /// Publishes the painted centre and height (`{key}.x/.y/.h`) against the
    /// layout's: continuous across the handoff between two elements (that
    /// is the point of a shared element), at rest exactly on layout. The
    /// velocity is the instantaneous one, `t·v_destination + (to − from)·
    /// ease′/D`. The step-response overshoot bound does not apply to a
    /// destination that can move while the morph runs (the harness's
    /// `f32::MAX` sentinel); the unit tests check the morph is exactly the
    /// eased blend towards the destination's current place.
    #[allow(clippy::cast_possible_truncation, clippy::too_many_arguments)]
    fn publish(
        &self,
        cx: &mut App,
        morph: Morph,
        painted: Bounds<Pixels>,
        laid: Bounds<Pixels>,
        (t, slope): (f32, f32),
        drift: (f32, f32, f32),
        now: Instant,
    ) {
        let live = slope != 0.0 || t < 1.0;
        let epoch = motion_epoch(cx);
        let millis = |at: Instant| at.saturating_duration_since(epoch).as_secs_f64() * 1000.0;
        let (started_ms, budget_ms, at_ms) = (
            millis(morph.start),
            self.duration.as_secs_f64() * 1000.0,
            millis(now),
        );
        let group = probe::current_group();
        let (from, to) = (morph.from, laid);
        let span = self.duration.as_secs_f32().max(1e-3);
        for (axis, value, target, start, moving) in [
            (
                "x",
                painted.center().x,
                to.center().x,
                from.center().x,
                drift.0,
            ),
            (
                "y",
                painted.center().y,
                to.center().y,
                from.center().y,
                drift.1,
            ),
            (
                "h",
                painted.size.height,
                to.size.height,
                from.size.height,
                drift.2,
            ),
        ] {
            let (value, target, start) = (f32::from(value), f32::from(target), f32::from(start));
            probe::record_track(cx, || TrackSample {
                key: format!("shared.{}.{axis}", self.key),
                kind: TrackKind::Tween,
                value: if live { value } else { target },
                target,
                velocity: if live {
                    t * moving + (target - start) * slope / span
                } else {
                    0.0
                },
                started_ms,
                budget_ms,
                at_ms,
                live,
                overshoot_ratio: f32::MAX,
                overshoot_absolute: 0.0,
                group: group.clone(),
            });
        }
    }

    fn child(&mut self) -> Option<&mut AnyElement> {
        match &mut self.content {
            Content::Built(child) => Some(child),
            Content::Deferred(_) => None,
        }
    }
}

/// The transform that paints `to` at `from`'s place and size, `t` of the way
/// home (t = 0: exactly over `from`; t = 1: identity).
fn morph(from: Bounds<Pixels>, to: Bounds<Pixels>, fit: Fit, t: f32) -> LayerTransform {
    let ratio = |a: Pixels, b: Pixels| f32::from(a) / f32::from(b).max(1e-3);
    let start = match fit {
        Fit::Height => {
            let s = ratio(from.size.height, to.size.height);
            Size {
                width: s,
                height: s,
            }
        }
        Fit::Width => {
            let s = ratio(from.size.width, to.size.width);
            Size {
                width: s,
                height: s,
            }
        }
        Fit::Stretch => Size {
            width: ratio(from.size.width, to.size.width),
            height: ratio(from.size.height, to.size.height),
        },
    };
    let lerp = |a: f32, b: f32| a + (b - a) * t;
    let scale = Size {
        width: lerp(start.width, 1.0),
        height: lerp(start.height, 1.0),
    };
    let (c_from, c_to) = (from.center(), to.center());
    let centre = point(
        c_to.x + (c_from.x - c_to.x) * (1.0 - t),
        c_to.y + (c_from.y - c_to.y) * (1.0 - t),
    );
    LayerTransform::scale_about(c_to, scale).then(LayerTransform::translation(centre - c_to))
}

impl IntoElement for Shared {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

impl gpui::Element for Shared {
    type RequestLayoutState = ();
    type PrepaintState = ();

    fn id(&self) -> Option<ElementId> {
        Some(self.key.clone())
    }

    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, Self::RequestLayoutState) {
        let now = now(cx);
        if let Some(owner) = id {
            let reduced = reduced(cx);
            let slot = (window.window_handle().window_id(), self.key.clone());
            let departing = cx
                .default_global::<Registry>()
                .painted
                .get(&slot)
                .filter(|painted| {
                    painted.owner.as_ref() != Some(owner)
                        && now.saturating_duration_since(painted.at) <= RECENT
                })
                .map(|painted| painted.bounds);
            // A newcomer (no state yet) morphs from the departing paint; a
            // returning element keeps its morph.
            self.morph = window.with_element_state::<State, _>(owner, |state, _| {
                let state = state.unwrap_or(State {
                    morph: departing
                        .filter(|_| !reduced)
                        .map(|from| Morph { from, start: now }),
                    laid: None,
                });
                (state.morph, state)
            });
        }
        let morphing = match (self.progress(now), self.morph) {
            (Some((_, t)), Some(morph)) => Morphing {
                t,
                from: Some(morph.from.size),
            },
            _ => Morphing::REST,
        };
        if let Content::Deferred(build) = &mut self.content
            && let Some(build) = build.take()
        {
            self.content = Content::Built(build(morphing));
        }
        let layout = self.child().map(|child| child.request_layout(window, cx));
        (
            layout.unwrap_or_else(|| window.request_layout(gpui::Style::default(), [], cx)),
            (),
        )
    }

    fn prepaint(
        &mut self,
        id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        window: &mut Window,
        cx: &mut App,
    ) -> Self::PrepaintState {
        let now = now(cx);
        let progress = self.progress(now);
        self.transform = match (progress, self.morph) {
            (Some((_, t)), Some(m)) => {
                // The source was recorded in window space. A rising page or
                // leaving map already has a parent transform: undo that for
                // the source, then let the parent move the destination.
                let from = window
                    .layer_transform()
                    .inverse()
                    .map_or(m.from, |inverse| inverse.apply_bounds(m.from));
                morph(from, bounds, self.fit, t)
            }
            _ => LayerTransform::IDENTITY,
        };
        if let Some(owner) = id.cloned() {
            // Register where it is painted, in window space.
            let painted = window
                .layer_transform()
                .apply_bounds(self.transform.apply_bounds(bounds));
            let slot = (window.window_handle().window_id(), self.key.clone());
            let registry = cx.default_global::<Registry>();
            registry.painted.insert(
                slot,
                Painted {
                    bounds: painted,
                    owner: Some(owner),
                    at: now,
                },
            );
            if registry.painted.len() > 512 {
                registry
                    .painted
                    .retain(|_, painted| now.saturating_duration_since(painted.at) <= RECENT);
            }
        }
        if let Some(m) = self.morph {
            let live = progress.is_some();
            if live {
                request_frame(window, cx);
            } else if let Some(owner) = id {
                // Finished: forget the morph (after one final sample below).
                window.with_element_state::<State, _>(owner, |state, _| {
                    (
                        (),
                        State {
                            morph: None,
                            ..state.unwrap_or_default()
                        },
                    )
                });
            }
            if probe::enabled(cx) {
                let outer = window.layer_transform();
                let painted = outer.apply_bounds(self.transform.apply_bounds(bounds));
                let laid = outer.apply_bounds(bounds);
                // The destination's own velocity, from its layout last frame.
                let drift = id.map_or((0.0, 0.0, 0.0), |owner| {
                    window.with_element_state::<State, _>(owner, |state, _| {
                        let mut state = state.unwrap_or_default();
                        let drift = state.laid.map_or((0.0, 0.0, 0.0), |(before, at)| {
                            let dt = now.saturating_duration_since(at).as_secs_f32();
                            if dt <= 0.0 {
                                return (0.0, 0.0, 0.0);
                            }
                            (
                                f32::from(laid.center().x - before.center().x) / dt,
                                f32::from(laid.center().y - before.center().y) / dt,
                                f32::from(laid.size.height - before.size.height) / dt,
                            )
                        });
                        state.laid = Some((laid, now));
                        (drift, state)
                    })
                });
                let ease = progress.map_or((1.0, 0.0), |(linear, t)| (t, self.curve.slope(linear)));
                self.publish(cx, m, painted, laid, ease, drift, now);
            }
        }
        let transform = self.transform;
        if let Some(child) = self.child() {
            window.with_layer_transform(transform, |window| child.prepaint(window, cx));
        }
    }

    fn paint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        _bounds: Bounds<Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        _prepaint: &mut Self::PrepaintState,
        window: &mut Window,
        cx: &mut App,
    ) {
        let transform = self.transform;
        if let Some(child) = self.child() {
            window.with_layer_transform(transform, |window| child.paint(window, cx));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{Fit, morph};
    use gpui::{Bounds, point, px, size};

    #[test]
    fn a_morph_starts_over_the_old_bounds_and_ends_on_layout() {
        let from = Bounds::new(point(px(20.0), px(300.0)), size(px(80.0), px(18.0)));
        let to = Bounds::new(point(px(40.0), px(60.0)), size(px(320.0), px(46.0)));
        let start = morph(from, to, Fit::Height, 0.0).apply_bounds(to);
        // Height fit: same height, same centre as the row.
        assert!((f32::from(start.size.height) - 18.0).abs() < 1e-3);
        assert!((f32::from(start.center().x) - f32::from(from.center().x)).abs() < 1e-3);
        assert!((f32::from(start.center().y) - f32::from(from.center().y)).abs() < 1e-3);
        let end = morph(from, to, Fit::Height, 1.0).apply_bounds(to);
        for (a, b) in [
            (end.origin.x, to.origin.x),
            (end.origin.y, to.origin.y),
            (end.size.width, to.size.width),
            (end.size.height, to.size.height),
        ] {
            assert!(
                (f32::from(a) - f32::from(b)).abs() < 1e-3,
                "{end:?} vs {to:?}"
            );
        }
        let stretched = morph(from, to, Fit::Stretch, 0.0).apply_bounds(to);
        assert!((f32::from(stretched.size.width) - 80.0).abs() < 1e-3);
    }

    #[test]
    fn a_rising_parent_keeps_the_window_space_source_exact() {
        let from = Bounds::new(point(px(80.0), px(300.0)), size(px(12.0), px(12.0)));
        let to = Bounds::new(point(px(300.0), px(200.0)), size(px(72.0), px(72.0)));
        let outer = gpui::LayerTransform::translation(point(px(0.0), px(24.0)));
        let local_from = outer
            .inverse()
            .expect("translation inverse")
            .apply_bounds(from);
        assert_eq!(
            outer.apply_bounds(morph(local_from, to, Fit::Height, 0.0).apply_bounds(to)),
            from
        );
        assert_eq!(
            outer.apply_bounds(morph(local_from, to, Fit::Height, 1.0).apply_bounds(to)),
            outer.apply_bounds(to)
        );
    }

    /// Two different views: a page with a hero gem, a graph with a node. The
    /// root swaps them; the node (another view, another parent) starts over
    /// the hero's painted bounds, its builder sees the morph progress from
    /// the first frame, it follows its own moving layout, and it rests
    /// exactly on it. Going back mid-morph starts from where it is painted.
    mod across_views {
        use super::super::{Morphing, shared, shared_with};
        use crate::motion::reset_epoch;
        use crate::tokens::motion::GLIDE;
        use gpui::{
            AnyElement, AnyView, App, AppContext, Bounds, Context, Entity, GlobalElementId,
            InspectorElementId, IntoElement, LayoutId, ParentElement, Pixels, Render, Styled,
            TestAppContext, VisualTestContext, Window, div, px,
        };
        use std::cell::RefCell;
        use std::collections::HashMap;
        use std::rc::Rc;
        use std::time::Duration;

        type Seen = Rc<RefCell<HashMap<&'static str, Bounds<Pixels>>>>;

        /// Records where its child is *painted* (layout mapped through the
        /// layer transform in effect), which a layout probe cannot see.
        struct Spy {
            name: &'static str,
            seen: Seen,
            child: AnyElement,
        }

        fn spy(name: &'static str, seen: &Seen, child: impl IntoElement) -> Spy {
            Spy {
                name,
                seen: Rc::clone(seen),
                child: child.into_any_element(),
            }
        }

        impl IntoElement for Spy {
            type Element = Self;

            fn into_element(self) -> Self::Element {
                self
            }
        }

        impl gpui::Element for Spy {
            type RequestLayoutState = ();
            type PrepaintState = ();

            fn id(&self) -> Option<gpui::ElementId> {
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
                (self.child.request_layout(window, cx), ())
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
                let painted = window.layer_transform().apply_bounds(bounds);
                self.seen.borrow_mut().insert(self.name, painted);
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

        struct Page {
            seen: Seen,
        }

        impl Render for Page {
            fn render(
                &mut self,
                _window: &mut Window,
                _cx: &mut Context<Self>,
            ) -> impl IntoElement {
                div().pl(px(300.0)).pt(px(200.0)).child(shared(
                    "gem",
                    spy("page.gem", &self.seen, div().size(px(72.0))),
                ))
            }
        }

        struct Graph {
            x: f32,
            seen: Seen,
            morphs: Rc<RefCell<Vec<Morphing>>>,
        }

        impl Render for Graph {
            fn render(
                &mut self,
                _window: &mut Window,
                _cx: &mut Context<Self>,
            ) -> impl IntoElement {
                let (seen, morphs) = (Rc::clone(&self.seen), Rc::clone(&self.morphs));
                // A different parent chain from the page's.
                div()
                    .pl(px(self.x))
                    .pt(px(40.0))
                    .child(div().child(shared_with("gem", move |morph| {
                        morphs.borrow_mut().push(morph);
                        spy("graph.node", &seen, div().size(px(12.0)))
                    })))
            }
        }

        struct Root {
            page: Entity<Page>,
            graph: Entity<Graph>,
            show_graph: bool,
        }

        impl Render for Root {
            fn render(
                &mut self,
                _window: &mut Window,
                _cx: &mut Context<Self>,
            ) -> impl IntoElement {
                let view: AnyView = if self.show_graph {
                    self.graph.clone().into()
                } else {
                    self.page.clone().into()
                };
                div().size_full().child(view)
            }
        }

        fn frame(cx: &mut VisualTestContext, seen: &Seen) -> HashMap<&'static str, Bounds<Pixels>> {
            seen.borrow_mut().clear();
            cx.update(|window, cx| {
                window.simulate_next_frame(cx);
                window.refresh();
                window.draw(cx).clear(cx);
            });
            seen.borrow().clone()
        }

        fn centre(frame: &HashMap<&'static str, Bounds<Pixels>>, key: &str) -> (f32, f32, f32) {
            let b = frame.get(key).unwrap_or_else(|| panic!("{key} painted"));
            let c = b.center();
            (f32::from(c.x), f32::from(c.y), f32::from(b.size.height))
        }

        #[gpui::test]
        fn a_canvas_endpoint_hands_its_actual_bounds_to_a_hero(cx: &mut TestAppContext) {
            let seen: Seen = Rc::default();
            let (_, cx) = cx.add_window_view({
                let seen = Rc::clone(&seen);
                |_, _| Page { seen }
            });
            let source = Bounds::new(
                gpui::point(px(84.0), px(312.0)),
                gpui::size(px(12.0), px(12.0)),
            );
            cx.update(|window, cx| {
                reset_epoch(cx);
                super::super::remember("gem", source, window, cx);
                assert_eq!(super::super::last_bounds("gem", window, cx), Some(source));
                assert_eq!(
                    super::super::last_bounds("another-declaration", window, cx),
                    None
                );
            });
            assert_eq!(centre(&frame(cx, &seen), "page.gem"), (90.0, 318.0, 12.0));
            cx.executor().advance_clock(Duration::from_millis(700));
            cx.run_until_parked();
            assert_eq!(centre(&frame(cx, &seen), "page.gem"), (336.0, 236.0, 72.0));
        }

        #[gpui::test]
        fn a_disappeared_or_invalid_source_cannot_seed_a_new_endpoint(cx: &mut TestAppContext) {
            let seen: Seen = Rc::default();
            let (_, cx) = cx.add_window_view({
                let seen = Rc::clone(&seen);
                |_, _| Page { seen }
            });
            cx.update(|window, cx| {
                reset_epoch(cx);
                let source = Bounds::new(
                    gpui::point(px(84.0), px(312.0)),
                    gpui::size(px(12.0), px(12.0)),
                );
                super::super::remember("gem", source, window, cx);
                super::super::remember("another-declaration", source, window, cx);
                super::super::forget("gem", window, cx);
                assert_eq!(super::super::last_bounds("gem", window, cx), None);
                assert_eq!(
                    super::super::last_bounds("another-declaration", window, cx),
                    Some(source)
                );
                super::super::remember(
                    "gem",
                    Bounds::new(gpui::point(px(f32::NAN), px(0.0)), source.size),
                    window,
                    cx,
                );
                assert_eq!(super::super::last_bounds("gem", window, cx), None);
            });
            assert_eq!(centre(&frame(cx, &seen), "page.gem"), (336.0, 236.0, 72.0));
        }

        #[gpui::test]
        fn reduced_motion_snaps_a_seeded_canvas_handoff(cx: &mut TestAppContext) {
            let seen: Seen = Rc::default();
            let (_, cx) = cx.add_window_view({
                let seen = Rc::clone(&seen);
                |_, _| Page { seen }
            });
            cx.update(|window, cx| {
                reset_epoch(cx);
                let mut facet = crate::theme::Facet::default();
                facet.reduced_motion = true;
                cx.set_global(facet);
                super::super::remember(
                    "gem",
                    Bounds::new(
                        gpui::point(px(1.0), px(1.0)),
                        gpui::size(px(12.0), px(12.0)),
                    ),
                    window,
                    cx,
                );
            });
            assert_eq!(centre(&frame(cx, &seen), "page.gem"), (336.0, 236.0, 72.0));
        }

        #[gpui::test]
        fn a_captured_endpoint_cannot_refresh_its_expiry_or_change_identity(
            cx: &mut TestAppContext,
        ) {
            let seen: Seen = Rc::default();
            let (_, cx) = cx.add_window_view({
                let seen = Rc::clone(&seen);
                |_, _| Page { seen }
            });
            let endpoint = cx.update(|window, cx| {
                reset_epoch(cx);
                super::super::remember(
                    "gem",
                    Bounds::new(
                        gpui::point(px(1.0), px(1.0)),
                        gpui::size(px(12.0), px(12.0)),
                    ),
                    window,
                    cx,
                );
                super::super::capture("gem", window, cx).expect("source")
            });
            cx.executor().advance_clock(Duration::from_millis(600));
            cx.run_until_parked();
            cx.update(|window, cx| {
                assert!(!super::super::resume(
                    endpoint.clone(),
                    "another-declaration",
                    window,
                    cx
                ));
                assert!(super::super::resume(endpoint.clone(), "gem", window, cx));
            });
            cx.executor().advance_clock(Duration::from_millis(401));
            cx.run_until_parked();
            cx.update(|window, cx| {
                assert!(!super::super::resume(endpoint, "gem", window, cx));
                assert_eq!(super::super::last_bounds("gem", window, cx), None);
            });
            assert_eq!(centre(&frame(cx, &seen), "page.gem"), (336.0, 236.0, 72.0));
        }

        #[gpui::test]
        fn a_hero_gem_becomes_a_graph_node_in_another_view_and_back(cx: &mut TestAppContext) {
            let seen: Seen = Rc::default();
            let morphs = Rc::new(RefCell::new(Vec::new()));
            let (root, cx) = cx.add_window_view({
                let (seen, morphs) = (Rc::clone(&seen), Rc::clone(&morphs));
                |_, cx| Root {
                    page: cx.new(|_| Page {
                        seen: Rc::clone(&seen),
                    }),
                    graph: cx.new(|_| Graph {
                        x: 40.0,
                        seen,
                        morphs,
                    }),
                    show_graph: false,
                }
            });
            cx.update(|_, cx| reset_epoch(cx));
            let hero = centre(&frame(cx, &seen), "page.gem");
            assert_eq!(hero, (336.0, 236.0, 72.0));

            root.update(cx, |root, cx| {
                root.show_graph = true;
                cx.notify();
            });
            let first = centre(&frame(cx, &seen), "graph.node");
            // Painted over the hero: same centre, the hero's size.
            assert!(
                (first.0 - hero.0).abs() < 0.01 && (first.1 - hero.1).abs() < 0.01,
                "{first:?}"
            );
            assert!((first.2 - 72.0).abs() < 0.01, "{first:?}");
            assert_eq!(
                morphs.borrow().first().map(|m| m.t),
                Some(0.0),
                "the builder knew from frame one"
            );

            // The node's layout moves while the morph runs (a flying camera);
            // every frame is exactly the glide from the hero to where the
            // node is laid out *now*.
            let mut last = first;
            for step in 1..=5_u8 {
                let x = 40.0 + f32::from(step) * 3.0;
                root.update(cx, |root, cx| {
                    root.graph.update(cx, |graph, _| graph.x = x);
                    cx.notify();
                });
                cx.executor().advance_clock(Duration::from_millis(40));
                cx.run_until_parked();
                let now = centre(&frame(cx, &seen), "graph.node");
                let t = GLIDE.ease(f32::from(step) * 40.0 / 620.0);
                let (node_x, node_y) = (x + 6.0, 46.0);
                let expected = (
                    node_x + (hero.0 - node_x) * (1.0 - t),
                    node_y + (hero.1 - node_y) * (1.0 - t),
                    12.0 + (72.0 - 12.0) * (1.0 - t),
                );
                assert!(
                    (now.0 - expected.0).abs() < 0.05
                        && (now.1 - expected.1).abs() < 0.05
                        && (now.2 - expected.2).abs() < 0.05,
                    "step {step}: {now:?} vs {expected:?}"
                );
                last = now;
            }
            // Back mid-morph: the hero starts where the node is painted now.
            root.update(cx, |root, cx| {
                root.show_graph = false;
                cx.notify();
            });
            let back = centre(&frame(cx, &seen), "page.gem");
            assert!(
                (back.0 - last.0).abs() < 0.5
                    && (back.1 - last.1).abs() < 0.5
                    && (back.2 - last.2).abs() < 0.5,
                "{back:?} vs {last:?}"
            );
            cx.executor().advance_clock(Duration::from_millis(700));
            cx.run_until_parked();
            let rest = centre(&frame(cx, &seen), "page.gem");
            assert_eq!(rest, hero, "rests exactly on its layout");
        }
    }
}
