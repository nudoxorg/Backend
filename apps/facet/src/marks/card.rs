//! What every mark shares: its type, the card's anatomy, and the trigger
//! that turns a quiet glyph into a door (hover rests open the card, the
//! keyboard opens it at once, a state sheet can pin it open).
//!
//! A mark's card is a float-layer peek that *unfurls* from the mark: the
//! mark's underline draws, becomes the card's top edge, and the body
//! unrolls from it ([`FloatRequest::unfurl`]).

use crate::measure::{Measure, Set};
use crate::theme::ActiveFacet;
use crate::motion::{Motion, spec};
use crate::overlay::float::{self, FloatKind, FloatRequest};
use crate::probe::{self, TextOverflow};
use crate::tokens::{Face, TypeRole};
use gpui::{
    AnyElement, App, Bounds, Div, Element, ElementId, FocusHandle, GlobalElementId, Hsla,
    InspectorElementId, InteractiveElement, IntoElement, KeyDownEvent, LayoutId, ParentElement,
    Pixels, Refineable, SharedString, StatefulInteractiveElement, Style, StyleRefinement, Styled,
    Window, div, px,
};
use std::cell::Cell;
use std::rc::Rc;

pub(crate) const fn role(face: Face, weight: f32, size: f32, line: f32) -> TypeRole {
    TypeRole {
        face,
        weight,
        size,
        line,
        tracking: 0.0,
        italic: matches!(face, Face::Serif),
    }
}

/// A mark's one word at rest (`crates.io`, `MIT/Apache-2.0`).
pub(crate) const WORD: TypeRole = role(Face::Ui, 400.0, 12.5, 16.0);
/// A dependency's name (a link).
pub(crate) const DEP: TypeRole = role(Face::Mono, 500.0, 12.5, 16.0);
/// A card's title.
pub(crate) const TITLE: TypeRole = role(Face::Ui, 600.0, 13.5, 17.0);
/// A card's title in mono (a version, a dependency).
pub(crate) const TITLE_MONO: TypeRole = role(Face::Mono, 600.0, 14.0, 18.0);
/// The quiet line under a card's title.
pub(crate) const PLACE: TypeRole = role(Face::Ui, 400.0, 12.0, 16.0);
/// A card's one sentence.
pub(crate) const SAY: TypeRole = role(Face::Serif, 400.0, 13.5, 19.0);
/// A reading (`28 releases behind · …`).
pub(crate) const READ: TypeRole = role(Face::Ui, 400.0, 12.5, 17.0);
/// A fact or key/value line.
pub(crate) const FACT: TypeRole = role(Face::Ui, 400.0, 12.0, 17.0);
/// A fact's number.
pub(crate) const FACT_NUM: TypeRole = role(Face::Mono, 600.0, 12.0, 17.0);
/// Code inside a fact.
pub(crate) const CODE: TypeRole = role(Face::Mono, 400.0, 11.5, 17.0);
/// A column head or a key.
pub(crate) const HEAD: TypeRole = role(Face::Ui, 500.0, 11.5, 15.0);
/// The foot line (the license hedge).
pub(crate) const FOOT: TypeRole = role(Face::Ui, 400.0, 11.5, 15.0);
/// A copyable line (mono; contextual alternates off, see `fonts::features`).
pub(crate) const LINE: TypeRole = role(Face::Mono, 500.0, 12.5, 16.0);

/// A card-internal length: `value` px at 100 % text and comfortable density.
pub(crate) fn k(measure: &Measure, value: f32) -> Pixels {
    px(value * measure.scale() * measure.density().space())
}

/// A text element the probe can read back (`key` is how tests find it).
pub(crate) fn text(
    key: impl Into<ElementId>,
    content: impl Into<SharedString>,
    role: TypeRole,
    measure: &Measure,
    color: impl Into<Hsla>,
) -> AnyElement {
    let content = content.into();
    let resolved = measure.role(role);
    probe::text(
        key,
        content.clone(),
        resolved,
        1.0,
        TextOverflow::Wrap,
        div().set(role, measure).text_color(color.into()).child(content),
    )
    .into_any_element()
}

/// A card's body: `width` px at 100 % text, the card's padding.
pub(crate) fn body(width: f32, measure: &Measure) -> Div {
    div()
        .w(k(measure, width).min(measure.width()))
        .flex()
        .flex_col()
        .pt(k(measure, 12.0))
        .px(k(measure, 14.0))
        .pb(k(measure, 13.0))
}

/// The builder every mark card is: content for the card's own measure.
pub(crate) type Content = Rc<dyn Fn(&Measure, &mut Window, &mut App) -> AnyElement>;

/// The float request for a mark's card at `anchor`: a peek that unfurls.
pub(crate) fn request(key: &ElementId, anchor: Bounds<Pixels>, content: &Content) -> FloatRequest {
    let content = content.clone();
    FloatRequest::new(key.clone(), anchor, FloatKind::Peek, move |measure, window, cx| {
        content(measure, window, cx)
    })
    .unfurl()
    .hang_from_start()
}

/// A mark's per-instance memory.
pub(crate) struct MarkState {
    pub focus: FocusHandle,
    pub hovered: bool,
    /// A state sheet's card was opened (once).
    pub sheet: Rc<Cell<bool>>,
}

/// A mark's live state this frame.
pub(crate) struct Live {
    /// Hovered, focused by keys, or its card open: 0..1.
    pub lit: f32,
    state: gpui::Entity<MarkState>,
    focus: FocusHandle,
    sheet: Rc<Cell<bool>>,
}

/// Reads a mark's state and animates its lit amount.
pub(crate) fn live(id: &ElementId, key: &ElementId, window: &mut Window, cx: &mut App) -> Live {
    let state = window.use_keyed_state(id.clone(), cx, |_, cx| MarkState {
        focus: cx.focus_handle().tab_stop(true),
        hovered: false,
        sheet: Rc::new(Cell::new(false)),
    });
    let open = float::is_open(key, window, cx);
    let (hovered, focus, sheet) = {
        let st = state.read(cx);
        (st.hovered, st.focus.clone(), st.sheet.clone())
    };
    let focused = focus.is_focused(window) && window.last_input_was_keyboard();
    let motion = Motion::scoped(ElementId::View(state.entity_id()), cx);
    let lit = motion.animate(
        ElementId::NamedChild(std::sync::Arc::new(id.clone()), "lit".into()),
        if hovered || open || focused { 1.0 } else { 0.0 },
        spec::HOVER,
        window,
        cx,
    );
    Live { lit, state, focus, sheet }
}

/// What Enter does on a mark that is also a link (it follows the link;
/// Space still opens the card).
pub(crate) type Activate = Rc<dyn Fn(&mut Window, &mut App)>;

/// Wraps a mark's glyph and word as a door: resting opens its card (unless
/// `card` is `None`: a quiet mark), Enter or Space opens it at once (Enter
/// follows `activate` instead when the mark is a link), and a state sheet
/// (`sheet`) opens it that many ms after the first paint.
pub(crate) fn door(
    id: &ElementId,
    key: &ElementId,
    live: &Live,
    card: Option<Content>,
    sheet: Option<u64>,
    activate: Option<Activate>,
    child: impl IntoElement,
) -> AnyElement {
    let hover_state = live.state.clone();
    let mut outer = div()
        .id(id.clone())
        .relative()
        .flex()
        .items_center()
        .on_hover(move |hovered, _window, cx| {
            hover_state.update(cx, |st, cx| {
                st.hovered = *hovered;
                cx.notify();
            });
        });
    let Some(card) = card else {
        return outer.child(child).into_any_element();
    };
    let key_request = key.clone();
    let key_card = card.clone();
    outer = outer.track_focus(&live.focus).on_key_down(move |event: &KeyDownEvent, window, cx| {
        let pressed = event.keystroke.key.as_str();
        if pressed == "enter"
            && let Some(activate) = &activate
        {
            activate(window, cx);
            cx.stop_propagation();
        } else if matches!(pressed, "enter" | "space") {
            let anchor = float::reported(&key_request, window, cx).unwrap_or_default();
            float::open(request(&key_request, anchor, &key_card), window, cx);
            cx.stop_propagation();
        }
    });
    let trigger_key = key.clone();
    let trigger_card = card.clone();
    let trigger = float::trigger(
        key.clone(),
        move |bounds| request(&trigger_key, bounds, &trigger_card),
        child,
    );
    outer = outer.child(trigger);
    if let Some(after) = sheet {
        let opened = live.sheet.clone();
        let anchor = SheetAnchor { key: key.clone(), card: card.clone(), after, opened, style: StyleRefinement::default() }
            .absolute()
            .top_0()
            .left_0()
            .size_full();
        outer = outer.child(anchor);
    }
    outer.into_any_element()
}

/// Opens an initially selected state card once its owner has measured bounds.
/// It paints nothing: the float needs the mark's first-frame layout bounds.
struct SheetAnchor {
    key: ElementId,
    card: Content,
    after: u64,
    opened: Rc<Cell<bool>>,
    style: StyleRefinement,
}

impl Styled for SheetAnchor {
    fn style(&mut self) -> &mut StyleRefinement { &mut self.style }
}

impl IntoElement for SheetAnchor {
    type Element = Self;
    fn into_element(self) -> Self { self }
}

impl Element for SheetAnchor {
    type RequestLayoutState = ();
    type PrepaintState = ();

    fn id(&self) -> Option<ElementId> { None }
    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> { None }

    fn request_layout(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, ()) {
        let mut style = Style::default();
        style.refine(&self.style);
        (window.request_layout(style, [], cx), ())
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
        if self.opened.replace(true) { return; }
        let key = self.key.clone();
        let card = self.card.clone();
        let after = self.after;
        if after == 0 {
            let request = request(&key, bounds, &card);
            window.defer(cx, move |window, cx| float::rest(request, window, cx));
        } else {
            window
                .spawn(cx, async move |cx| {
                    cx.background_executor().timer(std::time::Duration::from_millis(after)).await;
                    let _ = cx.update(|window, cx| {
                        let anchor = float::reported(&key, window, cx).unwrap_or(bounds);
                        float::rest(request(&key, anchor, &card), window, cx);
                    });
                })
                .detach();
        }
    }

    fn paint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        _bounds: Bounds<Pixels>,
        _request_layout: &mut (),
        _prepaint: &mut (),
        _window: &mut Window,
        _cx: &mut App,
    ) {}
}

/// A card shown in place under its mark (boards only): the same content on
/// the float layer's plate, with the unfurl's edge at rest along its top.
pub(crate) fn in_place(content: &Content, window: &mut Window, cx: &mut App) -> AnyElement {
    let facet = cx.facet();
    let palette = facet.palette();
    let measure = Measure::new(px(FloatKind::Peek.base_width() * facet.text_scale), &facet);
    let inner = content(&measure, window, cx);
    div()
        .relative()
        .mt(px(12.0 * facet.text_scale))
        .flex()
        .child(div().relative().child(float::plate(FloatKind::Peek, false, palette).child(inner)).child(
            div()
                .absolute()
                .top_0()
                .left(px(FloatKind::Peek.chamfer()))
                .right_0()
                .h(px(1.0))
                .bg(crate::controls::with_alpha(palette.peri.base.into(), 0.55)),
        ))
        .into_any_element()
}
