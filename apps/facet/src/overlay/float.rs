//! The float layer: one per window, rendered once as the root's last
//! child. Everything that floats goes through it — tips, peeks, lenses,
//! menus — so hover intent, placement, chaining, pinning, exits and focus
//! are written exactly once.
//!
//! Triggers never own popups. They report rest and leave with the exact
//! sub-rect the pointer is on (a tick, a word, a row):
//!
//! ```ignore
//! float::rest(FloatRequest::new(key, bounds, FloatKind::Peek, move |measure, window, cx| {
//!     peek::card(&data, measure, window, cx)
//! }), window, cx);
//! // …and on hover end:
//! float::leave(&key, window, cx);
//! ```
//!
//! The layer does the rest:
//!
//! - **Hover intent.** A cold rest waits ([`FloatKind::rest_delay`]); the
//!   layer tracks trigger hover (reported) and card hover (its own plates)
//!   itself, so moving *into* a card keeps it and moving on dismisses it.
//! - **Warm sweep.** Once a kind is open (or closed < 300 ms ago), resting
//!   on another trigger of that kind swaps at once: the same card morphs to
//!   the new anchor, size and content.
//! - **Aim protection.** While the pointer travels from a trigger towards
//!   its card, rests on other triggers are deferred, so a diagonal move into
//!   a card never loses it.
//! - **Placement.** Preferred side, flip, shift to stay 8 px inside, height
//!   cap with internal scroll, a hairline connector to the anchor rect; a
//!   full-width bottom sheet at `Room::Narrow`.
//! - **Chain.** A rest on a trigger inside an open card opens a child beside
//!   it with the crumb row and the focus bevel; three deep, then the deepest
//!   card offers "open". [`step_back`] (Esc) closes one.
//! - **Exits.** Cards stay in the model while they leave; asking again
//!   mid-exit reverses from where the card is.
//! - **Pins.** [`pin_top`] (Space) pins the deepest peek/lens (dedup by
//!   key); [`pinned_column`] renders them.
//! - **Focus.** [`open`] (keyboard, click) moves focus into the card and
//!   restores it when the card closes.
//!
//! Content is built every frame from the request's closure, with a
//! [`Measure`] for the card's own width (base width × text scale, clamped
//! to the viewport), so cards scale with text and re-flow on resize.

mod model;
pub mod place;
#[cfg(all(test, feature = "gallery"))]
mod storm;

pub use model::{AIM_IDLE, Card, DEEPEN, MAX_DEPTH, Model, Pending, Pin, Presence, WARM};

use crate::measure::{Measure, Space};
use crate::motion::{self, Motion, spec};
use crate::paint::{Bevel, CutPaint, Edge, paint_cut};
use crate::probe::{self, TrackKind, TrackSample};
use crate::theme::ActiveFacet;
use crate::tokens::{Palette, ty};
use crate::{Set, icons};
use gpui::{
    AnyElement, App, Bounds, BoxShadow, ColorExt, ContentMask, Element, ElementId,
    EntityId, FocusHandle, Global, GlobalElementId, Hitbox, HitboxBehavior, Hsla,
    InspectorElementId, InteractiveElement, IntoElement, KeyDownEvent, Keystroke, LayoutId,
    MouseDownEvent, MouseExitEvent, MouseMoveEvent, ParentElement, Pixels, Point,
    ScrollWheelEvent, SharedString, Size, StatefulInteractiveElement, Style, Styled, Task, Window,
    WindowId, deferred, div, fill, point, px, size,
};
use place::Hang;
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::time::Instant;

/// What floats.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub enum FloatKind {
    /// A tooltip: one per window, no chain, not hoverable.
    Tip,
    /// A symbol, package, file or version one rung up.
    Peek,
    /// An aggregate broken down.
    Lens,
    /// A context or dropdown menu (opened with [`open`]).
    Menu,
}

/// The preferred side; the layer flips and shifts at the edges.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub enum Side {
    /// Above the anchor.
    Above,
    /// Below the anchor.
    Below,
    /// Right of the anchor.
    Right,
    /// Left of the anchor.
    Left,
}

/// Where a card's content is being drawn (content may adapt).
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Hash)]
pub enum Surface {
    /// Not inside the layer.
    #[default]
    Page,
    /// A floating card.
    Card,
    /// The narrow-room bottom sheet.
    Sheet,
    /// A row in the pinned column or the ⌘P stack.
    Pinned,
}

/// Builds a card's content for the card's measure.
pub type Content = Rc<dyn Fn(&Measure, &mut Window, &mut App) -> AnyElement>;

/// One rest or open: what to show, for which trigger, anchored where.
#[derive(Clone)]
pub struct FloatRequest {
    /// The exact trigger: a tick, a word, a row.
    pub key: ElementId,
    /// Window coordinates of that sub-rect.
    pub anchor: Bounds<Pixels>,
    /// What floats.
    pub kind: FloatKind,
    /// The preferred side.
    pub side: Side,
    /// Builds the content.
    pub content: Content,
}

impl FloatRequest {
    /// A request on the kind's default side (tips above, the rest below).
    pub fn new(
        key: impl Into<ElementId>,
        anchor: Bounds<Pixels>,
        kind: FloatKind,
        content: impl Fn(&Measure, &mut Window, &mut App) -> AnyElement + 'static,
    ) -> Self {
        Self {
            key: key.into(),
            anchor,
            kind,
            side: match kind {
                FloatKind::Tip => Side::Above,
                FloatKind::Peek | FloatKind::Lens | FloatKind::Menu => Side::Below,
            },
            content: Rc::new(content),
        }
    }

    /// The preferred side.
    #[must_use]
    pub const fn side(mut self, side: Side) -> Self {
        self.side = side;
        self
    }
}

impl std::fmt::Debug for FloatRequest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FloatRequest")
            .field("key", &self.key)
            .field("anchor", &self.anchor)
            .field("kind", &self.kind)
            .field("side", &self.side)
            .finish_non_exhaustive()
    }
}

/// Paint priority of the layer among deferred draws (above popovers and
/// everything else that defers).
pub const PRIORITY: usize = 1_000;

// ------------------------------------------------------------------ state

type KeyHandler = Rc<dyn Fn(&Keystroke, &mut Window, &mut App) -> bool>;
type Follow = Rc<dyn Fn(&ElementId, &mut Window, &mut App)>;

struct CardFocus {
    handle: FocusHandle,
    restore: Option<FocusHandle>,
}

#[derive(Clone, Copy)]
struct Report {
    bounds: Bounds<Pixels>,
    hovered: bool,
    seq: u64,
    frame: u64,
    /// The view that prepainted it (set by [`anchor`]).
    view: Option<EntityId>,
}

struct Layer {
    model: Model,
    host: Option<EntityId>,
    timer: Option<(Instant, Task<()>)>,
    motion: Motion,
    focus: HashMap<u64, CardFocus>,
    /// Handles of cards that closed, with where they restored to: a later
    /// restore that points at one walks on through it.
    retired: Vec<CardFocus>,
    triggers: HashMap<ElementId, Report>,
    seq: u64,
    frame: u64,
    viewport: Option<Size<Pixels>>,
    building: Option<u64>,
    build: Build,
    crumbs_taken: bool,
    keys: Option<KeyHandler>,
    follow: Option<Follow>,
    stack_open: bool,
    caps: HashMap<u64, Pixels>,
    /// Each card's trigger key and anchor as last placed: an anchor that
    /// moves under the same key drags its card rigidly.
    placed: HashMap<u64, (ElementId, Bounds<Pixels>)>,
}

impl Layer {
    fn new() -> Self {
        Self {
            model: Model::new(),
            host: None,
            timer: None,
            motion: Motion::new(),
            focus: HashMap::new(),
            retired: Vec::new(),
            triggers: HashMap::new(),
            seq: 1,
            frame: 1,
            viewport: None,
            building: None,
            build: Build::default(),
            crumbs_taken: false,
            keys: None,
            follow: None,
            stack_open: false,
            caps: HashMap::new(),
            placed: HashMap::new(),
        }
    }
}

#[derive(Default)]
struct Layers(HashMap<WindowId, Rc<RefCell<Layer>>>);

impl Global for Layers {}

fn state(window: &Window, cx: &mut App) -> Rc<RefCell<Layer>> {
    let id = window.window_handle().window_id();
    cx.default_global::<Layers>()
        .0
        .entry(id)
        .or_insert_with(|| Rc::new(RefCell::new(Layer::new())))
        .clone()
}

/// Runs `f` on this window's model at the executor clock, then applies the
/// consequences (focus restore, the next timer, a repaint).
fn with_model<R>(window: &mut Window, cx: &mut App, f: impl FnOnce(&mut Model, Instant) -> R) -> R {
    let layer = state(window, cx);
    let now = motion::now(cx);
    let reduced = motion::reduced(cx);
    let result = {
        let mut layer = layer.borrow_mut();
        layer.model.set_reduced(reduced);
        f(&mut layer.model, now)
    };
    settle(&layer, window, cx);
    result
}

/// After any change: restore focus from closed cards, arm the timer for the
/// next deadline, and repaint the host.
fn settle(layer: &Rc<RefCell<Layer>>, window: &mut Window, cx: &mut App) {
    let now = motion::now(cx);
    let (closed, host, deadline) = {
        let mut state = layer.borrow_mut();
        let closed = state.model.take_closed();
        let mut restores = Vec::new();
        for id in closed {
            if let Some(focus) = state.focus.remove(&id) {
                restores.push(focus);
            }
        }
        (restores, state.host, state.model.next_deadline(now))
    };
    restore_focus(layer, closed, window, cx);
    arm(layer, deadline, now, window, cx);
    match host {
        Some(host) => cx.notify(host),
        None => window.refresh(),
    }
}

fn restore_focus(
    layer: &Rc<RefCell<Layer>>,
    closed: Vec<CardFocus>,
    window: &mut Window,
    cx: &mut App,
) {
    for focus in &closed {
        if !focus.handle.is_focused(window) {
            continue;
        }
        // Walk back through every card that is gone — closed in this same
        // batch, or closed earlier — to the first live restore target.
        let mut target = focus.restore.clone();
        for _ in 0..=MAX_DEPTH + 1 {
            let Some(handle) = target.clone() else { break };
            if let Some(dead) = closed.iter().find(|card| card.handle == handle) {
                target = dead.restore.clone();
                continue;
            }
            let retired = layer
                .borrow()
                .retired
                .iter()
                .find(|card| card.handle == handle)
                .map(|card| card.restore.clone());
            if let Some(restore) = retired {
                target = restore;
                continue;
            }
            let owner = layer
                .borrow()
                .focus
                .iter()
                .find(|(_, card)| card.handle == handle)
                .map(|(id, _)| *id);
            match owner {
                Some(id) if !layer.borrow().model.card(id).is_some_and(Card::is_open) => {
                    target = layer
                        .borrow_mut()
                        .focus
                        .remove(&id)
                        .and_then(|card| card.restore);
                }
                _ => break,
            }
        }
        if std::env::var("FLOAT_TRACE").is_ok() {
            eprintln!("  restore from {:?} -> {:?}", focus.handle, target);
        }
        match target {
            Some(handle) => window.focus(&handle, cx),
            None => window.blur(),
        }
    }
    let mut state = layer.borrow_mut();
    state.retired.extend(closed);
    let excess = state.retired.len().saturating_sub(32);
    state.retired.drain(..excess);
}

fn arm(
    layer: &Rc<RefCell<Layer>>,
    deadline: Option<Instant>,
    now: Instant,
    window: &mut Window,
    cx: &mut App,
) {
    let current = layer.borrow().timer.as_ref().map(|(at, _)| *at);
    if current == deadline {
        return;
    }
    let Some(at) = deadline else {
        layer.borrow_mut().timer = None;
        return;
    };
    let delay = at.saturating_duration_since(now);
    let weak = Rc::downgrade(layer);
    let task = window.spawn(cx, async move |cx| {
        cx.background_executor().timer(delay).await;
        let _ = cx.update(|window, cx| {
            if let Some(layer) = weak.upgrade() {
                layer.borrow_mut().timer = None;
                let now = motion::now(cx);
                layer.borrow_mut().model.tick(now);
                settle(&layer, window, cx);
            }
        });
    });
    layer.borrow_mut().timer = Some((at, task));
}

// ------------------------------------------------------------------ API

/// A trigger reports that the pointer rests on it (hover intent applies).
pub fn rest(request: FloatRequest, window: &mut Window, cx: &mut App) {
    with_model(window, cx, |model, now| model.rest(request, now));
}

/// Opens at once (keyboard or click): no delay, sticky, focus moves in.
pub fn open(request: FloatRequest, window: &mut Window, cx: &mut App) {
    let takes_focus = request.kind != FloatKind::Tip;
    let opened = with_model(window, cx, |model, now| model.open(request, now));
    // A tip never takes focus.
    let opened = opened.filter(|_| takes_focus);
    // Read the focus after the model ran: opening can close the card that
    // held focus (and its settle has already moved focus to a live target).
    let restore = window.focused(cx);
    if let Some(id) = opened {
        let layer = state(window, cx);
        let handle = {
            let mut layer = layer.borrow_mut();
            // A reused card keeps its original restore target.
            match layer.focus.get(&id) {
                Some(focus) => focus.handle.clone(),
                None => {
                    let handle = cx.focus_handle();
                    layer.focus.insert(
                        id,
                        CardFocus {
                            handle: handle.clone(),
                            restore,
                        },
                    );
                    handle
                }
            }
        };
        if std::env::var("FLOAT_TRACE").is_ok() {
            eprintln!("  open -> card #{id} focus {:?}", handle);
        }
        window.focus(&handle, cx);
    }
}

/// A trigger reports that the pointer left it.
pub fn leave(key: &ElementId, window: &mut Window, cx: &mut App) {
    with_model(window, cx, |model, now| model.leave(key, now));
}

/// Esc: closes the deepest card. Returns whether anything closed.
pub fn step_back(window: &mut Window, cx: &mut App) -> bool {
    with_model(window, cx, |model, now| model.step_back(now))
}

/// Space: pins the deepest peek or lens. Returns whether one was pinned.
pub fn pin_top(window: &mut Window, cx: &mut App) -> bool {
    with_model(window, cx, |model, now| model.pin_top(now))
}

/// Unpins the card pinned from `key`.
pub fn unpin(key: &ElementId, window: &mut Window, cx: &mut App) -> bool {
    with_model(window, cx, |model, now| model.unpin(key, now))
}

/// Closes everything that floats (pins stay). The shell calls this on
/// navigation.
pub fn close_all(window: &mut Window, cx: &mut App) -> bool {
    with_model(window, cx, |model, now| model.close_all(now))
}

/// Closes the card opened from `key` (a menu after its selection).
pub fn close(key: &ElementId, window: &mut Window, cx: &mut App) -> bool {
    with_model(window, cx, |model, now| model.close_key(key, now))
}

/// ⌥→: follows the deepest card (the shell's navigation callback receives
/// its trigger key) and closes the chain. Returns whether anything followed.
pub fn follow(window: &mut Window, cx: &mut App) -> bool {
    let layer = state(window, cx);
    let (key, callback) = {
        let layer = layer.borrow();
        (layer.model.top().map(|card| card.key.clone()), layer.follow.clone())
    };
    let (Some(key), Some(callback)) = (key, callback) else {
        return false;
    };
    callback(&key, window, cx);
    close_all(window, cx);
    true
}

/// Installs the shell's navigation callback for [`follow`].
pub fn on_follow(
    callback: impl Fn(&ElementId, &mut Window, &mut App) + 'static,
    window: &mut Window,
    cx: &mut App,
) {
    state(window, cx).borrow_mut().follow = Some(Rc::new(callback));
}

/// The layer's own keys, for the shell's key handler (and the cards' own):
/// Esc steps back, Space pins, ⌥→ follows. Returns whether it was handled.
pub fn handle_key(keystroke: &Keystroke, window: &mut Window, cx: &mut App) -> bool {
    if super::hint::handle_key(keystroke, window, cx) {
        return true;
    }
    let mods = keystroke.modifiers;
    let plain = !mods.control && !mods.platform && !mods.alt && !mods.shift;
    match keystroke.key.as_str() {
        "escape" if plain => step_back(window, cx),
        "space" if plain => pin_top(window, cx),
        "right" if mods.alt && !mods.platform && !mods.control => follow(window, cx),
        "p" if mods.platform && !mods.alt && !mods.control => toggle_pins(window, cx),
        _ => false,
    }
}

/// ⌘P: shows or hides the pinned stack (when there is no pinned column).
pub fn toggle_pins(window: &mut Window, cx: &mut App) -> bool {
    let layer = state(window, cx);
    let any = {
        let mut layer = layer.borrow_mut();
        let any = !layer.model.pins().is_empty();
        layer.stack_open = any && !layer.stack_open;
        any
    };
    settle(&layer, window, cx);
    any
}

/// Repaints the layer (overlay state outside the model changed: a menu's
/// selection, a toast, the dialog, hint labels).
pub fn refresh(window: &mut Window, cx: &mut App) {
    let layer = state(window, cx);
    settle(&layer, window, cx);
}

/// Finishes every entrance and exit now (the harness's settle, and scenes
/// that show a state rather than its arrival).
pub fn settle_now(window: &mut Window, cx: &mut App) {
    with_model(window, cx, |model, now| model.snap(now));
}

/// The rect a tracked trigger last reported (window coordinates).
#[must_use]
pub fn reported(key: &ElementId, window: &Window, cx: &mut App) -> Option<Bounds<Pixels>> {
    state(window, cx)
        .borrow()
        .triggers
        .get(key)
        .map(|report| report.bounds)
}

/// Whether a card for `key` is open (triggers may draw themselves "lit").
#[must_use]
pub fn is_open(key: &ElementId, window: &Window, cx: &mut App) -> bool {
    state(window, cx)
        .borrow()
        .model
        .cards()
        .any(|card| card.is_open() && card.key == *key)
}

/// The keys of the pinned cards, newest first.
#[must_use]
pub fn pins(window: &Window, cx: &mut App) -> Vec<ElementId> {
    state(window, cx)
        .borrow()
        .model
        .pins()
        .iter()
        .filter(|pin| !pin.leaving)
        .map(|pin| pin.key.clone())
        .collect()
}

/// Called by content while it is built: the card's name (the crumb row of
/// its children and its pinned row use it).
pub fn title(title: impl Into<SharedString>, window: &Window, cx: &mut App) {
    let layer = state(window, cx);
    let mut layer = layer.borrow_mut();
    if let Some(id) = layer.building {
        layer.model.title(id, title.into());
    }
}

/// What the card being built is: where it is drawn, its chain level, the
/// crumbs above it, whether a still pointer deepened it, whether it holds
/// keyboard focus. Content reads this (with `Measure::reveal` for ⌘ / ⌥) to
/// decide which of its sections show. Outside a build: the default (page).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Build {
    /// Where it is drawn.
    pub surface: Surface,
    /// Its chain level (`None`: a tip, a pin, or not a card).
    pub level: Option<usize>,
    /// The titles of the cards above it, root first, then its own.
    pub crumbs: Vec<SharedString>,
    /// Rested on again: show what ⌥ would show.
    pub deep: bool,
    /// Opened from the keyboard: it holds focus.
    pub focused: bool,
}

/// The build context of the card being built (see [`Build`]).
#[must_use]
pub fn build(window: &Window, cx: &mut App) -> Build {
    state(window, cx).borrow().build.clone()
}

/// Where the content being built is drawn.
#[must_use]
pub fn surface(window: &Window, cx: &mut App) -> Surface {
    state(window, cx).borrow().build.surface
}

/// Content that draws the crumb itself takes it (the layer then does not):
/// the ancestors' titles, root first, then the card's own title if it has
/// already given one with [`title`].
pub fn take_crumbs(window: &Window, cx: &mut App) -> Vec<SharedString> {
    let layer = state(window, cx);
    let mut layer = layer.borrow_mut();
    layer.crumbs_taken = true;
    let mut crumbs = layer.build.crumbs.clone();
    let own = layer
        .building
        .and_then(|id| layer.model.card(id))
        .and_then(|card| card.title.clone());
    if !crumbs.is_empty()
        && let Some(own) = own
    {
        crumbs.push(own);
    }
    crumbs
}

/// Called by content while it is built: a key handler that runs before the
/// layer's own (a menu's arrows, type-ahead, Enter). Return `true` to
/// consume the key.
pub fn on_key(
    handler: impl Fn(&Keystroke, &mut Window, &mut App) -> bool + 'static,
    window: &Window,
    cx: &mut App,
) {
    state(window, cx).borrow_mut().keys = Some(Rc::new(handler));
}

/// A tracked trigger's rect this frame. Triggers that call this (the
/// [`trigger`] element, rich-text words, `.tip()`) re-anchor their open card
/// when they move and let the layer close it when they are gone.
pub fn anchor(key: &ElementId, bounds: Bounds<Pixels>, window: &Window, cx: &mut App) {
    let view = window.current_view();
    let layer = state(window, cx);
    let mut layer = layer.borrow_mut();
    let frame = layer.frame;
    let seq = layer.seq;
    let entry = layer.triggers.entry(key.clone()).or_insert(Report {
        bounds,
        hovered: false,
        seq,
        frame,
        view: Some(view),
    });
    entry.bounds = bounds;
    entry.frame = frame;
    entry.view = Some(view);
    if entry.seq < seq {
        entry.seq = seq;
    }
    layer.model.reanchor(key, bounds);
}

/// A tracked trigger's hover report from its mouse-move listener (capture
/// phase). Emits [`rest`] / [`leave`] on change; the layer's own listener
/// (which runs after every trigger's) closes cards whose tracked trigger
/// did not report.
pub fn report(
    key: &ElementId,
    bounds: Bounds<Pixels>,
    hovered: bool,
    request: impl FnOnce() -> FloatRequest,
    window: &mut Window,
    cx: &mut App,
) {
    let layer = state(window, cx);
    let was = {
        let mut layer = layer.borrow_mut();
        let (seq, frame) = (layer.seq, layer.frame);
        let entry = layer.triggers.entry(key.clone()).or_insert(Report {
            bounds,
            hovered: false,
            seq,
            frame,
            view: None,
        });
        let was = entry.hovered;
        entry.hovered = hovered;
        entry.bounds = bounds;
        entry.seq = seq;
        was
    };
    if hovered && !was {
        rest(request(), window, cx);
    } else if !hovered && was {
        leave(key, window, cx);
    }
}

// ------------------------------------------------------------------ the layer

/// The layer element: the window root's last child.
pub fn layer(window: &mut Window, cx: &mut App) -> AnyElement {
    let layer = state(window, cx);
    layer.borrow_mut().host = Some(window.current_view());
    deferred(LayerElement {
        layer,
        cards: Vec::new(),
        pins: None,
        extras: Vec::new(),
        viewport: size(px(0.0), px(0.0)),
        hitboxes: Vec::new(),
    })
    .with_priority(PRIORITY)
    .into_any_element()
}

struct CardDraw {
    id: u64,
    kind: FloatKind,
    level: Option<usize>,
    side: Side,
    anchor: Bounds<Pixels>,
    presence: f32,
    open: bool,
    sheet: bool,
    hidden: bool,
    focus: f32,
    swap: f32,
    element: AnyElement,
    layout: LayoutId,
    painted: Bounds<Pixels>,
    connector: Option<(Point<Pixels>, Point<Pixels>)>,
    /// Grows the card out of its anchor while it enters or leaves
    /// (identity at rest).
    grow: gpui::LayerTransform,
}

struct LayerElement {
    layer: Rc<RefCell<Layer>>,
    cards: Vec<CardDraw>,
    pins: Option<(AnyElement, LayoutId)>,
    /// Toasts, the dialog and hint labels: placed by their own layout
    /// inside the layer's viewport-sized box, painted over the cards.
    extras: Vec<AnyElement>,
    viewport: Size<Pixels>,
    hitboxes: Vec<(u64, Hitbox)>,
}

impl IntoElement for LayerElement {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

/// The rise distance of an entering card, px at 100 % text.
const RISE: f32 = 6.0;

/// At or under this effective window width, peeks and lenses are sheets.
pub const SHEET_FROM: f32 = 480.0;

/// The plate a card of `kind` sits on, for a card shown in place (boards,
/// docs) rather than floated: the same chamfer, fill, bevel and shadow the
/// layer paints.
#[must_use]
pub fn plate(kind: FloatKind, focused: bool, palette: &Palette) -> crate::paint::Cut {
    crate::paint::cut()
        .chamfer(crate::paint::Chamfer::Px(kind.chamfer()))
        .fill(palette_plate(kind, palette))
        .bevel(if focused { Bevel::Focus } else { Bevel::Rest })
        .floating()
}

fn palette_plate(kind: FloatKind, palette: &Palette) -> Hsla {
    match kind {
        FloatKind::Peek | FloatKind::Lens => palette.glass.into(),
        FloatKind::Tip | FloatKind::Menu => palette.plate3.into(),
    }
}

impl LayerElement {
    /// Builds a card's element: crumb row, content, overflow strip, inside
    /// a scroll container capped to the room it had last frame.
    #[allow(clippy::too_many_arguments)]
    fn build_card(
        &self,
        card: &Card,
        width: f32,
        cap: Option<Pixels>,
        surface: Surface,
        crumbs: Vec<SharedString>,
        window: &mut Window,
        cx: &mut App,
    ) -> AnyElement {
        let facet = cx.facet();
        let measure = Measure::new(px(width), &facet);
        {
            let mut layer = self.layer.borrow_mut();
            let focused = layer.focus.contains_key(&card.id);
            layer.building = Some(card.id);
            layer.build = Build {
                surface,
                level: card.level,
                crumbs: crumbs.clone(),
                deep: card.deep,
                focused,
            };
            layer.crumbs_taken = false;
            layer.keys = None;
        }
        let content = (card.content)(&measure, window, cx);
        let (keys, focus, title, taken) = {
            let mut layer = self.layer.borrow_mut();
            layer.building = None;
            layer.build = Build::default();
            let keys = layer.keys.take();
            let focus = layer.focus.get(&card.id).map(|focus| focus.handle.clone());
            let title = layer.model.card(card.id).and_then(|card| card.title.clone());
            (keys, focus, title, layer.crumbs_taken)
        };
        let palette = facet.palette();
        // Absolutely positioned inside the layer's viewport-sized box, so
        // it shrink-wraps its content (up to the card width) instead of
        // filling the box.
        let mut body = div()
            .id(("float-card", card.id))
            .absolute()
            .top_0()
            .left_0()
            .flex()
            .flex_col()
            .max_w(px(width));
        if let Some(cap) = cap {
            body = body.max_h(cap).overflow_y_scroll();
        }
        if surface == Surface::Sheet {
            body = body.w(px(width));
        }
        if !crumbs.is_empty() && !taken {
            body = body.child(crumb_row(&crumbs, title.as_ref(), &measure, palette));
        }
        body = body.child(content);
        if card.overflow {
            body = body.child(overflow_strip(&measure, palette));
        }
        if let Some(handle) = focus {
            body = body.track_focus(&handle);
        }
        body = body.on_key_down(move |event: &KeyDownEvent, window, cx| {
            let consumed = keys
                .as_ref()
                .is_some_and(|keys| keys(&event.keystroke, window, cx))
                || handle_key(&event.keystroke, window, cx);
            if consumed {
                cx.stop_propagation();
            }
        });
        body.into_any_element()
    }
}

/// The crumb row the layer draws above a child card's content (content
/// that places its own crumb takes it instead): the path in mono ink4,
/// separated by `›`.
fn crumb_row(
    crumbs: &[SharedString],
    own: Option<&SharedString>,
    measure: &Measure,
    palette: &Palette,
) -> AnyElement {
    crumb_line(crumbs.iter().chain(own), measure, palette)
        .pt(measure.space(Space::Roomy) + px(2.0))
        .px(measure.space(Space::Gutter))
        .into_any_element()
}

/// A crumb path as one line of text (`A › B › C`), mono ink4.
pub fn crumb_line<'a>(
    crumbs: impl IntoIterator<Item = &'a SharedString>,
    measure: &Measure,
    palette: &Palette,
) -> gpui::Div {
    let text = crumbs
        .into_iter()
        .map(SharedString::as_ref)
        .collect::<Vec<&str>>()
        .join("  ›  ");
    div()
        .set(CRUMB, measure)
        .text_color(palette.ink4.hsla())
        .truncate()
        .child(text)
}

const CRUMB: crate::tokens::TypeRole = crate::tokens::TypeRole {
    face: crate::tokens::Face::Mono,
    weight: 400.0,
    size: 11.0,
    line: 14.0,
    tracking: 0.0,
    italic: false,
};

fn overflow_strip(measure: &Measure, palette: &Palette) -> AnyElement {
    div()
        .flex()
        .items_center()
        .gap(measure.space(Space::Base))
        .mt(measure.space(Space::Base))
        .px(measure.space(Space::Roomy) + px(2.0))
        .py(measure.space(Space::Base))
        .border_t_1()
        .border_color(palette.line1.hsla())
        .child(crate::controls::kbd("↵", measure))
        .child(
            div()
                .set(ty::CAPTION, measure)
                .text_color(palette.ink2.hsla())
                .child("Three deep. Open it as a page to go further."),
        )
        .into_any_element()
}

impl Element for LayerElement {
    type RequestLayoutState = ();
    type PrepaintState = ();

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }

    #[allow(clippy::too_many_lines)]
    fn request_layout(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, ()) {
        let now = motion::now(cx);
        let facet = cx.facet();
        let viewport = window.viewport_size();
        self.viewport = viewport;
        // Peeks and lenses become a bottom sheet in a phone-width window
        // (effective width at or under 480).
        let narrow = f32::from(viewport.width) / facet.text_scale <= SHEET_FROM;
        let cards: Vec<Card> = {
            let mut layer = self.layer.borrow_mut();
            layer.model.set_reduced(motion::reduced(cx));
            layer.model.tick(now);
            layer.model.cards().cloned().collect()
        };
        let deepest_open = cards
            .iter()
            .filter(|card| card.is_open() && card.level.is_some())
            .filter_map(|card| card.level)
            .max();
        let mut ids = Vec::new();
        for card in &cards {
            let sheet = narrow && matches!(card.kind, FloatKind::Peek | FloatKind::Lens);
            // In the sheet only the deepest card shows; the others wait.
            let hidden = sheet && card.is_open() && card.level.is_some() && card.level != deepest_open;
            let width = if sheet {
                f32::from(viewport.width)
            } else {
                (card.kind.base_width() * facet.text_scale)
                    .min(f32::from(viewport.width) - 2.0 * place::MARGIN)
                    .max(1.0)
            };
            let cap = self
                .layer
                .borrow()
                .caps
                .get(&card.id)
                .map(|cap| (*cap).max(px(24.0)));
            let crumbs: Vec<SharedString> = match card.level {
                Some(level) if level > 0 => {
                    // The layer re-reads titles after each parent's build.
                    let layer = self.layer.borrow();
                    let chain = layer.model.chain();
                    chain
                        .iter()
                        .filter(|other| other.level.is_some_and(|l| l < level))
                        .filter_map(|other| other.title.clone())
                        .collect()
                }
                _ => Vec::new(),
            };
            let surface = if sheet { Surface::Sheet } else { Surface::Card };
            let mut element = self.build_card(card, width, cap, surface, crumbs, window, cx);
            let layout = element.request_layout(window, cx);
            ids.push(layout);
            let presence = card.presence.value(now);
            let focus_target = if card.is_open()
                && card.level.is_some()
                && (card.level.is_some_and(|level| level > 0) || card.sticky)
                && card.level == deepest_open
            {
                1.0
            } else {
                0.0
            };
            let motion = self.layer.borrow().motion.clone();
            let focus = motion.animate(("float-focus", card.id), focus_target, spec::HOVER, window, cx);
            let swap = card.swapped.map_or(1.0, |at| {
                let run = now.saturating_duration_since(at).as_secs_f32();
                (run / 0.12).clamp(0.0, 1.0)
            });
            self.cards.push(CardDraw {
                id: card.id,
                kind: card.kind,
                level: card.level,
                side: card.side,
                anchor: card.anchor,
                presence,
                open: card.is_open(),
                sheet,
                hidden,
                focus,
                swap: 0.35 + 0.65 * swap,
                element,
                layout,
                painted: Bounds::default(),
                connector: None,
                grow: gpui::LayerTransform::IDENTITY,
            });
            if card.presence.live(now) || swap < 1.0 {
                motion::request_frame(window, cx);
            }
            publish_presence(card, presence, now, cx);
        }
        // The ⌘P stack, when open and there is no pinned column.
        let stack_open = self.layer.borrow().stack_open;
        if stack_open {
            let width = (300.0 * facet.text_scale).min(f32::from(viewport.width) - 2.0 * place::MARGIN);
            let measure = Measure::new(px(width), &facet);
            let mut element = pinned_stack(&self.layer, &measure, window, cx);
            let layout = element.request_layout(window, cx);
            ids.push(layout);
            self.pins = Some((element, layout));
        }
        let whole = facet.measure(viewport.width);
        let extras = [
            super::toast::element(&whole, window, cx),
            super::dialog::element(&whole, window, cx),
            super::hint::element(&whole, window, cx),
        ];
        for mut element in extras.into_iter().flatten() {
            ids.push(element.request_layout(window, cx));
            self.extras.push(element);
        }
        let style = Style {
            position: gpui::Position::Absolute,
            inset: gpui::Edges {
                top: px(0.0).into(),
                left: px(0.0).into(),
                right: gpui::Length::Auto,
                bottom: gpui::Length::Auto,
            },
            size: size(viewport.width.into(), viewport.height.into()),
            ..Style::default()
        };
        (window.request_layout(style, ids, cx), ())
    }

    #[allow(clippy::too_many_lines)]
    fn prepaint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        _bounds: Bounds<Pixels>,
        _request_layout: &mut (),
        window: &mut Window,
        cx: &mut App,
    ) {
        let now = motion::now(cx);
        let scale = cx.facet().text_scale;
        let viewport = self.viewport;
        let (motion_store, resized) = {
            let mut layer = self.layer.borrow_mut();
            let resized = layer.viewport.is_some_and(|last| last != viewport);
            layer.viewport = Some(viewport);
            // Stale cards: re-anchored by a report this frame, or closed.
            let frame = layer.frame;
            let reports: HashMap<ElementId, Bounds<Pixels>> = layer
                .triggers
                .iter()
                .filter(|(_, report)| report.frame == frame)
                .map(|(key, report)| (key.clone(), report.bounds))
                .collect();
            layer.model.resolve_stale(|key| reports.get(key).copied(), now);
            (layer.motion.clone(), resized)
        };
        let mut parents: HashMap<usize, Bounds<Pixels>> = HashMap::new();
        let mut caps: Vec<(u64, bool, Pixels)> = Vec::new();
        for index in 0..self.cards.len() {
            let (id, kind, level, side, sheet, open, presence) = {
                let card = &self.cards[index];
                (card.id, card.kind, card.level, card.side, card.sheet, card.open, card.presence)
            };
            // Anchors can move during this very prepaint (a parent's words
            // report as the parent is prepainted), so read the live one.
            let anchor = {
                let layer = self.layer.borrow();
                layer
                    .model
                    .card(id)
                    .map_or(self.cards[index].anchor, |card| card.anchor)
            };
            self.cards[index].anchor = anchor;
            // An anchor whose centre has left the viewport takes its card
            // with it (the card plays its exit rather than clinging to the
            // edge after the thing it describes).
            let on_screen = Bounds::new(point(px(0.0), px(0.0)), viewport).contains(&anchor.center());
            if open && !on_screen && !sheet {
                let mut layer = self.layer.borrow_mut();
                let key = layer.model.card(id).map(|card| card.key.clone());
                if let Some(key) = key {
                    layer.model.close_key(&key, now);
                }
            }
            let dragged = {
                let mut layer = self.layer.borrow_mut();
                let key = layer.model.card(id).map(|card| card.key.clone());
                match (key, layer.placed.get(&id).cloned()) {
                    (Some(key), Some((last_key, last_anchor))) => {
                        let moved = last_key == key && last_anchor.origin != anchor.origin;
                        layer.placed.insert(id, (key, anchor));
                        moved
                    }
                    (Some(key), None) => {
                        layer.placed.insert(id, (key, anchor));
                        false
                    }
                    (None, _) => false,
                }
            };
            let natural = window.layout_bounds(self.cards[index].layout);
            let hang = if sheet {
                Hang::Sheet
            } else {
                match level.and_then(|level| level.checked_sub(1)).and_then(|p| parents.get(&p)) {
                    Some(parent) => Hang::Parent(*parent),
                    None => Hang::Anchor,
                }
            };
            let placed = place::place_with_gap(anchor, natural.size, side, hang, viewport, kind.gap() * scale);
            caps.push((id, placed.capped.is_some(), placed.room));
            let target = placed.bounds;
            let key = |channel: &'static str| ElementId::NamedInteger(channel.into(), id);
            let keys = [key("float-x"), key("float-y"), key("float-w"), key("float-h")];
            let values = [
                f32::from(target.origin.x),
                f32::from(target.origin.y),
                f32::from(target.size.width),
                f32::from(target.size.height),
            ];
            let mut out = [0.0_f32; 4];
            for (slot, (key, value)) in keys.iter().zip(values).enumerate() {
                // A resize, a leaving card, or its own anchor moving (a
                // scrolled word, a node under a flying camera): the card
                // tracks directly. Only a warm swap morphs on the spring.
                if resized || !open || (dragged && slot < 2) {
                    motion_store.set(key.clone(), value);
                }
                out[slot] = motion_store.animate(key.clone(), value, spec::FOLLOW, window, cx);
            }
            let rise = if sheet { 24.0 } else { RISE * scale };
            let drift = (1.0 - presence) * rise;
            let dy = drift;
            let tail = if sheet { 14.0 } else { 0.0 };
            let painted = Bounds::new(
                point(px(out[0]), px(out[1] + dy)),
                size(px(out[2].max(1.0)), px(out[3].max(1.0) + tail)),
            );
            self.cards[index].painted = painted;
            // A child's hairline leaves from its parent's edge (never across
            // the parent's text), level with the anchor word.
            let from = match hang {
                Hang::Parent(parent) => {
                    let x = if placed.side == Side::Left { parent.left() } else { parent.right() };
                    Bounds::new(point(x, anchor.origin.y), size(px(0.0), anchor.size.height))
                }
                Hang::Anchor | Hang::Sheet => anchor,
            };
            // Peeks and lenses hang off a word or a tick by a hairline; a
            // menu sits on its button and a tip beside its mark.
            let hangs = matches!(kind, FloatKind::Peek | FloatKind::Lens);
            self.cards[index].connector = if sheet || !hangs {
                None
            } else {
                place::connector(from, painted, placed.side)
            };
            if let Some(level) = level
                && open
            {
                parents.insert(level, painted);
            }
            {
                let mut layer = self.layer.borrow_mut();
                layer.model.painted(id, painted);
            }
            let hidden = self.cards[index].hidden;
            // Open cards occlude what is under them; leaving ones do not.
            if !hidden && open && presence > 0.05 && kind != FloatKind::Tip {
                let hitbox = window.insert_hitbox(painted, HitboxBehavior::BlockMouse);
                self.hitboxes.push((id, hitbox));
            }
            let shift = painted.origin - natural.origin;
            // A hidden card (behind the sheet) still prepaints so its words
            // keep reporting, and a leaving card still draws; both prepaint
            // under an empty mask so nothing in them can be hovered.
            let mask = if hidden || !open {
                Bounds::new(painted.origin, size(px(0.0), px(0.0)))
            } else {
                painted
            };
            // The card grows out of the side that faces its anchor.
            let facing = match (sheet, placed.side) {
                (true, _) | (false, Side::Above) => point(painted.center().x, painted.bottom()),
                (false, Side::Below) => point(painted.center().x, painted.top()),
                (false, Side::Right) => point(painted.left(), painted.center().y),
                (false, Side::Left) => point(painted.right(), painted.center().y),
            };
            let grown = 0.97 + 0.03 * presence.clamp(0.0, 1.0);
            let grow = if grown >= 1.0 {
                gpui::LayerTransform::IDENTITY
            } else {
                gpui::LayerTransform::scale_about(facing, size(grown, grown))
            };
            self.cards[index].grow = grow;
            let element = &mut self.cards[index].element;
            let fade = presence.clamp(0.0, 1.0);
            window.with_layer_transform(grow, |window| {
                window.with_group_opacity(painted, fade, |window| {
                    window.with_content_mask(Some(ContentMask { bounds: mask }), |window| {
                        window.with_element_offset(shift, |window| element.prepaint(window, cx));
                    });
                });
            });
        }
        for element in &mut self.extras {
            element.prepaint(window, cx);
        }
        super::hint::finish_collecting(window, cx);
        if let Some((element, layout)) = self.pins.as_mut() {
            let natural = window.layout_bounds(*layout);
            let origin = point(
                viewport.width - natural.size.width - px(place::MARGIN),
                px(58.0),
            );
            window.insert_hitbox(Bounds::new(origin, natural.size), HitboxBehavior::BlockMouse);
            window.with_element_offset(origin - natural.origin, |window| element.prepaint(window, cx));
        }
        let mut layer = self.layer.borrow_mut();
        // A tracked trigger that did not report this frame while another
        // trigger of its own view did is gone (that view re-rendered without
        // it); a cached view reports nothing and proves nothing.
        let frame = layer.frame;
        let rendered: std::collections::HashSet<EntityId> = layer
            .triggers
            .values()
            .filter(|report| report.frame == frame)
            .filter_map(|report| report.view)
            .collect();
        let gone: Vec<ElementId> = layer
            .triggers
            .iter()
            .filter(|(_, report)| {
                report.frame < frame && report.view.is_some_and(|view| rendered.contains(&view))
            })
            .map(|(key, _)| key.clone())
            .collect();
        if !gone.is_empty() {
            layer.triggers.retain(|key, _| !gone.contains(key));
            layer.model.triggers_gone(|key| gone.contains(key), now);
        }
        // Closes decided while drawing restore focus right after the draw.
        if layer.model.has_closed() {
            let handle = window.window_handle();
            let weak = Rc::downgrade(&self.layer);
            cx.defer(move |cx| {
                let _ = handle.update(cx, |_, window, cx| {
                    if let Some(layer) = weak.upgrade() {
                        settle(&layer, window, cx);
                    }
                });
            });
        }
        // A capped card keeps its cap (it lays out scrolled at that height)
        // until its room grows past it; then it re-measures once.
        for (id, capped, room) in caps {
            let stored = layer.caps.get(&id).copied();
            let next = match (capped, stored) {
                (true, _) => Some(room),
                (false, Some(cap)) if room > cap + px(1.0) => None,
                (false, stored) => stored,
            };
            if next != stored {
                match next {
                    Some(cap) => layer.caps.insert(id, cap),
                    None => layer.caps.remove(&id),
                };
                motion::request_frame(window, cx);
            }
        }
        let live: Vec<u64> = layer.model.cards().map(|card| card.id).collect();
        layer.caps.retain(|id, _| live.contains(id));
        layer.placed.retain(|id, _| live.contains(id));
        motion_store.retain(|key| match key {
            ElementId::NamedInteger(_, id) => live.contains(id),
            _ => true,
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
        let facet = cx.facet();
        let palette = facet.palette();
        for draw in &mut self.cards {
            if draw.hidden {
                continue;
            }
            let t = draw.presence.clamp(0.0, 1.0);
            // The anchor stays lit while its card is open.
            if draw.level.is_some() && draw.kind != FloatKind::Menu {
                let line: Hsla = palette.peri.base.into();
                let anchor = draw.anchor;
                let underline = Bounds::new(
                    point(anchor.origin.x, anchor.origin.y + anchor.size.height - px(1.5)),
                    size(anchor.size.width, px(1.5)),
                );
                window.paint_quad(fill(underline, line.opacity(t)));
            }
            if let Some((from, to)) = draw.connector {
                let line: Hsla = palette.line3.into();
                let rect = Bounds::from_corners(
                    point(from.x.min(to.x), from.y.min(to.y)),
                    point(from.x.max(to.x) + px(1.0), from.y.max(to.y) + px(1.0)),
                );
                window.paint_quad(fill(rect, line.opacity(t)));
            }
            let chamfer = if draw.sheet { 14.0 } else { draw.kind.chamfer() };
            // One group: plate, bevel and content composite once and fade
            // together (a translucent plate never shows the bevel through).
            let painted = draw.painted;
            let grow = draw.grow;
            window.with_layer_transform(grow, |window| window.with_group_opacity(painted, t, |window| {
                let shadow: Hsla = palette.shadow.into();
                window.paint_chamfer_shadows(
                    draw.painted,
                    motion::compositing::chamfers(chamfer),
                    &[
                        BoxShadow {
                            color: shadow.opacity(0.86),
                            offset: point(px(0.0), px(18.0)),
                            blur_radius: px(15.0),
                            spread_radius: px(0.0),
                            inset: false,
                        },
                        BoxShadow {
                            color: shadow.opacity(0.5),
                            offset: point(px(0.0), px(2.0)),
                            blur_radius: px(3.0),
                            spread_radius: px(0.0),
                            inset: false,
                        },
                    ],
                );
                let rest = Edge::of(Bevel::Rest, palette);
                let focus = Edge::of(Bevel::Focus, palette);
                let spec = CutPaint {
                    chamfer,
                    edge: rest.mix(focus, draw.focus),
                    fill: Some(palette_plate(draw.kind, palette)),
                    ..CutPaint::new(palette)
                };
                paint_cut(window, draw.painted, &spec, palette);
                let element = &mut draw.element;
                window.with_element_opacity(Some(draw.swap), |window| {
                    window.with_content_mask(Some(ContentMask { bounds: draw.painted }), |window| {
                        element.paint(window, cx);
                    });
                });
            }));
        }
        if let Some((element, _)) = self.pins.as_mut() {
            element.paint(window, cx);
        }
        for element in &mut self.extras {
            element.paint(window, cx);
        }
        self.listen(window);
        let mut layer = self.layer.borrow_mut();
        layer.frame += 1;
    }
}

impl LayerElement {
    /// The layer's own window listeners. Registered last, so in the capture
    /// phase they run after every trigger has reported.
    fn listen(&self, window: &mut Window) {
        let layer = Rc::downgrade(&self.layer);
        window.on_mouse_event({
            let layer = layer.clone();
            move |event: &MouseMoveEvent, phase, window, cx| {
                if phase != gpui::DispatchPhase::Capture {
                    return;
                }
                let Some(layer) = layer.upgrade() else { return };
                let now = motion::now(cx);
                {
                    let mut state = layer.borrow_mut();
                    let seq = state.seq;
                    let gone: Vec<ElementId> = state
                        .triggers
                        .iter()
                        .filter(|(_, report)| report.seq != seq)
                        .map(|(key, _)| key.clone())
                        .collect();
                    state.model.triggers_gone(|key| gone.contains(key), now);
                    state.triggers.retain(|_, report| report.seq == seq);
                    state.seq += 1;
                    state.model.pointer_at(Some(event.position), now);
                }
                settle(&layer, window, cx);
            }
        });
        window.on_mouse_event({
            let layer = layer.clone();
            move |_: &MouseExitEvent, phase, window, cx| {
                if phase != gpui::DispatchPhase::Capture {
                    return;
                }
                let Some(layer) = layer.upgrade() else { return };
                let now = motion::now(cx);
                layer.borrow_mut().model.pointer_at(None, now);
                settle(&layer, window, cx);
            }
        });
        window.on_mouse_event({
            let layer = layer.clone();
            move |event: &MouseDownEvent, phase, window, cx| {
                if phase != gpui::DispatchPhase::Capture {
                    return;
                }
                let Some(layer) = layer.upgrade() else { return };
                let now = motion::now(cx);
                let closed = layer.borrow_mut().model.press(event.position, now);
                if closed {
                    settle(&layer, window, cx);
                }
            }
        });
        window.on_mouse_event({
            move |event: &ScrollWheelEvent, phase, window, cx| {
                if phase != gpui::DispatchPhase::Capture {
                    return;
                }
                let Some(layer) = layer.upgrade() else { return };
                let now = motion::now(cx);
                layer.borrow_mut().model.scroll(event.position, now);
                settle(&layer, window, cx);
            }
        });
    }
}

fn publish_presence(card: &Card, value: f32, now: Instant, cx: &mut App) {
    if !probe::enabled(cx) {
        return;
    }
    let epoch = motion::epoch(cx);
    let millis = |at: Instant| at.saturating_duration_since(epoch).as_secs_f64() * 1000.0;
    let presence = card.presence;
    let live = presence.live(now);
    probe::record_track(cx, || TrackSample {
        key: format!("float-{}.presence", card.id),
        kind: TrackKind::Tween,
        value,
        target: presence.target(),
        velocity: 0.0,
        started_ms: millis(presence.since()),
        budget_ms: presence.span().as_secs_f64() * 1000.0,
        at_ms: millis(now),
        live,
        overshoot_ratio: 0.0,
        group: Some(format!("float-{}", card.id)),
    });
}


// ------------------------------------------------------------------ pins

/// Renders one pin in its row form: the content decides what a pinned row
/// holds ([`Surface::Pinned`]); the unpin mark shows on hover.
fn pin_row(
    pin: &Pin,
    layer: &Rc<RefCell<Layer>>,
    measure: &Measure,
    window: &mut Window,
    cx: &mut App,
) -> AnyElement {
    let palette = cx.facet().palette();
    let now = motion::now(cx);
    {
        let mut state = layer.borrow_mut();
        state.build = Build {
            surface: Surface::Pinned,
            ..Build::default()
        };
        state.building = None;
    }
    let content = (pin.content)(measure, window, cx);
    layer.borrow_mut().build = Build::default();
    let t = pin.presence.value(now);
    if pin.presence.live(now) {
        motion::request_frame(window, cx);
    }
    let key = pin.key.clone();
    let group: SharedString = format!("pin-{}", pin.key).into();
    let row = div()
        .id(ElementId::NamedChild(std::sync::Arc::new(key.clone()), "pin".into()))
        .group(group.clone())
        .relative()
        .flex()
        .items_center()
        .py(measure.space(Space::Base))
        .px(measure.space(Space::Snug))
        .hover(|style| style.bg(palette.tint.hsla()))
        .child(div().flex_1().min_w_0().child(content))
        .child(
            div()
                .id(ElementId::NamedChild(std::sync::Arc::new(key.clone()), "unpin".into()))
                .invisible()
                .group_hover(group, |style| style.visible())
                .cursor_pointer()
                .child(icons::ui(icons::Icon::Pin, icons::IconSize::S12, palette.ink3))
                .on_click(move |_, window, cx| {
                    unpin(&key, window, cx);
                }),
        );
    motion::offset(row)
        .y(px((1.0 - t) * -6.0 * measure.scale()))
        .opacity(t)
        .into_any_element()
}

const PINS_HEAD: crate::tokens::TypeRole = crate::tokens::TypeRole {
    face: crate::tokens::Face::Ui,
    weight: 500.0,
    size: 11.5,
    line: 14.0,
    tracking: 0.0,
    italic: false,
};

fn pins_header(measure: &Measure, palette: &Palette) -> AnyElement {
    let mut head = div()
        .flex()
        .items_center()
        .gap(measure.space(Space::Snug))
        .mb(measure.space(Space::Snug))
        .set(PINS_HEAD, measure)
        .text_color(palette.ink3.hsla())
        .child(icons::ui(icons::Icon::Pin, icons::IconSize::S12, palette.ink3))
        .child("Pinned");
    if measure.reveal().keys {
        head = head.child(div().flex_1()).child(crate::controls::keys(
            &["⌘", "P"],
            crate::controls::KbdVoice::Plain,
            measure,
        ));
    }
    head.into_any_element()
}

/// The pinned column's contents (the shell's chrome draws the column frame
/// and shows it at `Room::Vast`): "Pinned", then one row per pin, newest
/// first. Keys show only while ⌘ is held.
pub fn pinned_column(measure: &Measure, window: &mut Window, cx: &mut App) -> AnyElement {
    let layer = state(window, cx);
    let palette = cx.facet().palette();
    let pins: Vec<Pin> = layer.borrow().model.pins().to_vec();
    let mut column = div()
        .flex()
        .flex_col()
        .w_full()
        .gap(measure.space(Space::Tight))
        .child(pins_header(measure, palette));
    for pin in &pins {
        column = column.child(pin_row(pin, &layer, measure, window, cx));
    }
    column.into_any_element()
}

fn pinned_stack(
    layer: &Rc<RefCell<Layer>>,
    measure: &Measure,
    window: &mut Window,
    cx: &mut App,
) -> AnyElement {
    let palette = cx.facet().palette();
    let pins: Vec<Pin> = layer.borrow().model.pins().to_vec();
    let inner = measure.inset(measure.space(Space::Roomy));
    let mut stack = crate::paint::cut()
        .chamfer(crate::paint::Chamfer::Float)
        .fill(palette.glass)
        .floating()
        .w(measure.width())
        .flex()
        .flex_col()
        .gap(measure.space(Space::Tight))
        .p(measure.space(Space::Roomy))
        .child(pins_header(&inner, palette));
    for pin in &pins {
        stack = stack.child(pin_row(pin, layer, &inner, window, cx));
    }
    stack.into_any_element()
}

// ------------------------------------------------------------------ triggers

/// A tracked trigger around `child`: reports hover (rest/leave) and its own
/// bounds to the layer every pointer move and every frame, so its card
/// re-anchors when it moves and closes when it is gone.
pub fn trigger(
    key: impl Into<ElementId>,
    request: impl Fn(Bounds<Pixels>) -> FloatRequest + 'static,
    child: impl IntoElement,
) -> Trigger {
    Trigger {
        key: key.into(),
        request: Rc::new(request),
        child: child.into_any_element(),
    }
}

/// See [`trigger`].
pub struct Trigger {
    key: ElementId,
    request: Rc<dyn Fn(Bounds<Pixels>) -> FloatRequest>,
    child: AnyElement,
}

impl IntoElement for Trigger {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

impl Element for Trigger {
    type RequestLayoutState = ();
    type PrepaintState = (Hitbox, Bounds<Pixels>);

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
    ) -> (Hitbox, Bounds<Pixels>) {
        self.child.prepaint(window, cx);
        let hitbox = window.insert_hitbox(bounds, HitboxBehavior::Normal);
        anchor(&self.key, bounds, window, cx);
        // In hint mode every trigger is a target: its code opens the float.
        let request = self.request.clone();
        super::hint::target(bounds, move |window, cx| open(request(bounds), window, cx), window, cx);
        (hitbox, bounds)
    }

    fn paint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        _bounds: Bounds<Pixels>,
        _request_layout: &mut (),
        prepaint: &mut (Hitbox, Bounds<Pixels>),
        window: &mut Window,
        cx: &mut App,
    ) {
        self.child.paint(window, cx);
        let (hitbox, bounds) = prepaint.clone();
        let key = self.key.clone();
        let request = self.request.clone();
        window.on_mouse_event(move |_: &MouseMoveEvent, phase, window, cx| {
            if phase != gpui::DispatchPhase::Capture {
                return;
            }
            let hovered = hitbox.is_hovered(window);
            let request = request.clone();
            report(&key, bounds, hovered, move || request(bounds), window, cx);
        });
    }
}
