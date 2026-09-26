//! The float layer's model: every card, pending intent, pin and deadline,
//! as plain data driven by events and an explicit clock. No window, no
//! element: the layer feeds it pointer positions, rests, leaves, keys and
//! painted bounds, and renders what it holds. Everything here is
//! deterministic in `now`, which is what the storm test leans on.
//!
//! # Vocabulary
//!
//! - A **card** is one floating plate: a tip, or a chain card (peek, lens,
//!   menu). Cards stay in the model after they close, in a closing state,
//!   until their exit settles; a card that is asked for again while it is
//!   leaving reverses from where it is.
//! - The **chain** is the open chain cards by level: 0 is the root (a
//!   trigger on the page), 1 and 2 are children opened by resting on a
//!   trigger inside the card one level up. Levels are contiguous.
//! - A card is **held** while its trigger is hovered, the pointer is on the
//!   card, a child is held, or the pointer is aiming at it. An unheld hover
//!   card closes after its kind's grace. Sticky cards (opened from the
//!   keyboard or a click) ignore hover and close on Esc, an outside press or
//!   an explicit close.
//! - **Warm**: once a card of a kind is open at a level (or one closed there
//!   less than [`WARM`] ago), resting on another trigger of that kind swaps
//!   at once: the same card morphs to the new anchor and content.
//! - **Aim**: while the pointer travels from a card's trigger towards the
//!   card (inside the triangle from its last point to the card's near
//!   edge), rests on other triggers at that level are deferred; they apply
//!   only if the pointer stops on them for [`AIM_IDLE`].

use super::{Content, FloatKind, FloatRequest, Side};
use crate::paint::cut;
use crate::tokens::motion::{Bezier, DROP, GLIDE};
use gpui::{Bounds, ElementId, Pixels, Point, SharedString, point, px};
use std::collections::HashMap;
use std::time::{Duration, Instant};

/// Chains stop at three cards; a fourth rest offers "open" instead.
pub const MAX_DEPTH: usize = 3;
/// A kind stays warm this long after its last card closed.
pub const WARM: Duration = Duration::from_millis(300);
/// A deferred rest applies once the pointer has not moved for this long.
pub const AIM_IDLE: Duration = Duration::from_millis(160);
/// Resting still on an open card (or its trigger) this long deepens it: the
/// card shows what ⌥ would show ("rest again").
pub const DEEPEN: Duration = Duration::from_millis(600);

const fn ms(value: u64) -> Duration {
    Duration::from_millis(value)
}

impl FloatKind {
    /// Whether cards of this kind chain (everything but the tip).
    #[must_use]
    pub const fn chains(self) -> bool {
        !matches!(self, Self::Tip)
    }

    /// How long a pointer rests before a cold card rises.
    #[must_use]
    pub const fn rest_delay(self) -> Duration {
        match self {
            Self::Tip => ms(450),
            Self::Peek | Self::Lens => ms(350),
            Self::Menu => ms(0),
        }
    }

    /// How long an unheld card waits before it closes.
    #[must_use]
    pub const fn grace(self) -> Duration {
        match self {
            Self::Tip => ms(90),
            Self::Peek | Self::Lens => ms(160),
            Self::Menu => ms(220),
        }
    }

    /// Entrance duration (full; a reversal takes the remaining share).
    #[must_use]
    pub const fn enter(self) -> Duration {
        match self {
            Self::Tip => ms(140),
            Self::Peek | Self::Lens => ms(200),
            Self::Menu => ms(160),
        }
    }

    /// Exit duration (full).
    #[must_use]
    pub const fn exit(self) -> Duration {
        match self {
            Self::Tip => ms(110),
            Self::Peek | Self::Lens => ms(150),
            Self::Menu => ms(120),
        }
    }

    /// The card's width at 100 % text, before clamping to the viewport.
    #[must_use]
    pub const fn base_width(self) -> f32 {
        match self {
            Self::Tip => 280.0,
            Self::Peek => 392.0,
            Self::Lens => 340.0,
            Self::Menu => 248.0,
        }
    }

    /// The gap between the anchor and the card (the connector spans it).
    #[must_use]
    pub const fn gap(self) -> f32 {
        match self {
            Self::Tip => 6.0,
            Self::Peek => 12.0,
            Self::Lens => 14.0,
            Self::Menu => 4.0,
        }
    }

    /// The plate's chamfer.
    #[must_use]
    pub const fn chamfer(self) -> f32 {
        match self {
            Self::Tip => 6.0,
            Self::Peek => 10.0,
            Self::Lens => 8.0,
            Self::Menu => 9.0,
        }
    }
}

impl Side {
    /// The side across the anchor.
    #[must_use]
    pub const fn opposite(self) -> Self {
        match self {
            Self::Above => Self::Below,
            Self::Below => Self::Above,
            Self::Right => Self::Left,
            Self::Left => Self::Right,
        }
    }
}

/// A reversible 0..1 presence, driven from the moment it was last
/// retargeted (so a remount never replays it, and a reversal continues from
/// the value it had).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Presence {
    from: f32,
    to: f32,
    since: Instant,
    duration: Duration,
}

impl Presence {
    /// Entering from 0 at `now`.
    #[must_use]
    pub fn entering(now: Instant, duration: Duration) -> Self {
        Self {
            from: 0.0,
            to: 1.0,
            since: now,
            duration,
        }
    }

    fn curve(&self) -> Bezier {
        if self.to >= self.from { GLIDE } else { DROP }
    }

    /// The value at `now`.
    #[must_use]
    pub fn value(&self, now: Instant) -> f32 {
        if self.duration.is_zero() {
            return self.to;
        }
        let run = now.saturating_duration_since(self.since).as_secs_f32();
        let progress = (run / self.duration.as_secs_f32()).clamp(0.0, 1.0);
        self.from + (self.to - self.from) * self.curve().ease(progress)
    }

    /// Where it is heading.
    #[must_use]
    pub const fn target(&self) -> f32 {
        self.to
    }

    /// The rate of change at `now`, in units per second: the same easing
    /// [`value`](Self::value) samples, differentiated (matches
    /// `motion::store`'s own tween velocity, `(to - from) * slope / span`),
    /// so a probe reading this alongside `value` sees one consistent curve
    /// instead of a real move reported at velocity 0.
    #[must_use]
    pub fn velocity(&self, now: Instant) -> f32 {
        if self.duration.is_zero() {
            return 0.0;
        }
        let span = self.duration.as_secs_f32();
        let run = now.saturating_duration_since(self.since).as_secs_f32();
        if run >= span {
            return 0.0;
        }
        let progress = (run / span).clamp(0.0, 1.0);
        (self.to - self.from) * self.curve().slope(progress) / span
    }

    /// When the current segment ends.
    #[must_use]
    pub fn ends(&self) -> Instant {
        self.since + self.duration
    }

    /// When the current segment started.
    #[must_use]
    pub const fn since(&self) -> Instant {
        self.since
    }

    /// The current segment's length.
    #[must_use]
    pub const fn span(&self) -> Duration {
        self.duration
    }

    /// Whether it still moves at `now`.
    #[must_use]
    pub fn live(&self, now: Instant) -> bool {
        (self.to - self.from).abs() > f32::EPSILON && now < self.ends()
    }

    /// Heads for `to` from wherever it is now, taking the share of `full`
    /// that the remaining distance is of the whole way.
    pub fn retarget(&mut self, to: f32, full: Duration, now: Instant) {
        if (self.to - to).abs() <= f32::EPSILON {
            return;
        }
        let value = self.value(now);
        *self = Self {
            from: value,
            to,
            since: now,
            duration: full.mul_f32((to - value).abs().clamp(0.0, 1.0)),
        };
    }
}

/// One floating card, open or leaving.
#[derive(Clone)]
pub struct Card {
    /// Stable for the card's life, across warm swaps (motion keys use it).
    pub id: u64,
    /// The trigger it currently belongs to.
    pub key: ElementId,
    /// Tip, peek, lens or menu.
    pub kind: FloatKind,
    /// The preferred side.
    pub side: Side,
    /// The trigger's rect, window coordinates.
    pub anchor: Bounds<Pixels>,
    /// Builds the content for a measure.
    pub content: Content,
    /// Chain level (`None` for tips).
    pub level: Option<usize>,
    /// Opened from the keyboard or a click: ignores hover.
    pub sticky: bool,
    /// Entrance/exit.
    pub presence: Presence,
    /// Leaving; removed once its exit settles.
    pub closing: bool,
    /// The trigger reports hover.
    pub trigger_hover: bool,
    /// The pointer is on the plate.
    pub card_hover: bool,
    /// When an unheld card closes.
    pub leave_at: Option<Instant>,
    /// Where the plate was last painted.
    pub painted: Option<Bounds<Pixels>>,
    /// When the content was last swapped by a warm sweep.
    pub swapped: Option<Instant>,
    /// Content scrolled under it; re-anchored or closed at the next paint.
    pub stale: bool,
    /// A rest inside this (deepest) card asked for a fourth level.
    pub overflow: bool,
    /// The name the content gave itself (the crumb uses it).
    pub title: Option<SharedString>,
    /// When it was last shown (opened, reopened or swapped).
    pub shown: Instant,
    /// The pointer rested on it again: it shows its deeper sections.
    pub deep: bool,
}

impl Card {
    /// Whether the card counts as open (not leaving).
    #[must_use]
    pub const fn is_open(&self) -> bool {
        !self.closing
    }

    /// Whether `at` is on the plate as last painted (cut corners excluded).
    #[must_use]
    pub fn covers(&self, at: Point<Pixels>) -> bool {
        self.painted
            .is_some_and(|bounds| cut::contains(bounds, self.kind.chamfer(), at))
    }
}

/// A rest waiting for its delay (or for the pointer to stop aiming).
#[derive(Clone)]
pub struct Pending {
    /// The request.
    pub request: FloatRequest,
    /// The chain level it will open at (`None` for a tip).
    pub level: Option<usize>,
    /// When it applies.
    pub due: Instant,
    /// Deferred by aim protection rather than a cold rest.
    pub aimed: bool,
}

/// A pinned card.
#[derive(Clone)]
pub struct Pin {
    /// The trigger key it was pinned from (pins dedupe by it).
    pub key: ElementId,
    /// Peek or lens.
    pub kind: FloatKind,
    /// Its content builder (rendered in the pinned-row form).
    pub content: Content,
    /// The card's title, if it gave one.
    pub title: Option<SharedString>,
    /// Entrance/exit in the pinned column.
    pub presence: Presence,
    /// Being unpinned.
    pub leaving: bool,
}

#[derive(Clone, Copy, Debug)]
struct Aim {
    card: u64,
    origin: Point<Pixels>,
    moved: Instant,
}

/// Everything the layer holds for one window.
#[derive(Default)]
pub struct Model {
    cards: Vec<Card>,
    pending: Option<Pending>,
    tip_pending: Option<Pending>,
    warm: HashMap<(FloatKind, Option<usize>), Instant>,
    pins: Vec<Pin>,
    pointer: Option<Point<Pixels>>,
    aim: Option<Aim>,
    next_id: u64,
    reduced: bool,
    closed: Vec<u64>,
    finished_presence: Vec<(u64, Presence)>,
    pressed_closed: Option<(ElementId, Instant)>,
    moved: Option<Instant>,
}

impl Model {
    /// An empty model.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Reduced motion: presences snap.
    pub fn set_reduced(&mut self, reduced: bool) {
        self.reduced = reduced;
    }

    fn duration(&self, full: Duration) -> Duration {
        if self.reduced { Duration::ZERO } else { full }
    }

    /// Every card, open or leaving, in paint order (tips last).
    pub fn cards(&self) -> impl Iterator<Item = &Card> {
        let chain = self.cards.iter().filter(|card| card.level.is_some());
        let tips = self.cards.iter().filter(|card| card.level.is_none());
        let mut chain: Vec<&Card> = chain.collect();
        // Leaving cards under open ones, then by level.
        chain.sort_by_key(|card| (card.is_open(), card.level));
        chain.into_iter().chain(tips)
    }

    /// Mutable access to a card by id.
    pub fn card_mut(&mut self, id: u64) -> Option<&mut Card> {
        self.cards.iter_mut().find(|card| card.id == id)
    }

    /// A card by id.
    #[must_use]
    pub fn card(&self, id: u64) -> Option<&Card> {
        self.cards.iter().find(|card| card.id == id)
    }

    /// The open chain, root first.
    #[must_use]
    pub fn chain(&self) -> Vec<&Card> {
        let mut chain: Vec<&Card> = self
            .cards
            .iter()
            .filter(|card| card.is_open() && card.level.is_some())
            .collect();
        chain.sort_by_key(|card| card.level);
        chain
    }

    /// The deepest open chain card.
    #[must_use]
    pub fn top(&self) -> Option<&Card> {
        self.chain().last().copied()
    }

    /// The open tip, if any.
    #[must_use]
    pub fn tip(&self) -> Option<&Card> {
        self.cards
            .iter()
            .find(|card| card.is_open() && card.level.is_none())
    }

    /// The pins, newest first (including ones still leaving).
    #[must_use]
    pub fn pins(&self) -> &[Pin] {
        &self.pins
    }

    /// The pending chain rest.
    #[must_use]
    pub const fn pending(&self) -> Option<&Pending> {
        self.pending.as_ref()
    }

    /// The pending tip rest.
    #[must_use]
    pub const fn tip_pending(&self) -> Option<&Pending> {
        self.tip_pending.as_ref()
    }

    /// The last pointer position.
    #[must_use]
    pub const fn pointer(&self) -> Option<Point<Pixels>> {
        self.pointer
    }

    /// Whether cards closed since the last [`Model::take_closed`].
    #[must_use]
    pub fn has_closed(&self) -> bool {
        !self.closed.is_empty()
    }

    /// Ids of cards that closed since the last call (focus restore).
    pub fn take_closed(&mut self) -> Vec<u64> {
        std::mem::take(&mut self.closed)
    }

    /// Exit segments retired since the last drain. Keep their real timing
    /// long enough for the layer to publish the terminal sample.
    pub(crate) fn take_finished_presence(&mut self) -> Vec<(u64, Presence)> {
        std::mem::take(&mut self.finished_presence)
    }

    /// Whether anything is open, pending or still moving.
    #[must_use]
    pub fn is_idle(&self, now: Instant) -> bool {
        self.pending.is_none()
            && self.tip_pending.is_none()
            && self
                .cards
                .iter()
                .all(|card| card.is_open() && !card.presence.live(now))
            && self
                .pins
                .iter()
                .all(|pin| !pin.leaving && !pin.presence.live(now))
    }

    /// Whether nothing needs another frame or timer at `now`.
    #[must_use]
    pub fn is_settled(&self, now: Instant) -> bool {
        self.next_deadline(now).is_none()
            && self.cards.iter().all(|card| !card.presence.live(now))
            && self.pins.iter().all(|pin| !pin.presence.live(now))
    }

    fn fresh_id(&mut self) -> u64 {
        self.next_id += 1;
        self.next_id
    }

    // ---------------------------------------------------------------- events

    /// A trigger reports that the pointer rests on it.
    pub fn rest(&mut self, request: FloatRequest, now: Instant) {
        self.tick(now);
        if request.kind == FloatKind::Tip {
            self.rest_tip(request, now);
        } else {
            self.rest_chain(request, now);
        }
        self.tick(now);
    }

    fn rest_tip(&mut self, request: FloatRequest, now: Instant) {
        if let Some(card) = self
            .cards
            .iter_mut()
            .find(|card| card.is_open() && card.level.is_none() && card.key == request.key)
        {
            card.trigger_hover = true;
            card.anchor = request.anchor;
            card.content = request.content;
            card.side = request.side;
            self.tip_pending = None;
            return;
        }
        if let Some(pending) = self
            .tip_pending
            .as_mut()
            .filter(|pending| pending.request.key == request.key)
        {
            pending.request = request;
            return;
        }
        let warm = self.cards.iter().any(|card| card.level.is_none())
            || self.is_warm_since(FloatKind::Tip, None, now);
        if warm {
            self.show(request, None, false, now);
        } else {
            let due = now + request.kind.rest_delay();
            self.tip_pending = Some(Pending {
                request,
                level: None,
                due,
                aimed: false,
            });
        }
    }

    fn rest_chain(&mut self, request: FloatRequest, now: Instant) {
        if let Some(card) = self
            .cards
            .iter_mut()
            .find(|card| card.is_open() && card.level.is_some() && card.key == request.key)
        {
            card.trigger_hover = true;
            card.anchor = request.anchor;
            card.content = request.content;
            card.side = request.side;
            let level = card.level;
            // Back on the open card's own trigger: a deferred sibling loses.
            if self
                .pending
                .as_ref()
                .is_some_and(|pending| pending.level == level)
            {
                self.pending = None;
            }
            return;
        }
        let level = self.level_for(request.anchor);
        if level >= MAX_DEPTH {
            if let Some(deepest) = self
                .cards
                .iter_mut()
                .find(|card| card.is_open() && card.level == Some(MAX_DEPTH - 1))
            {
                deepest.overflow = true;
            }
            return;
        }
        if let Some(pending) = self
            .pending
            .as_mut()
            .filter(|pending| pending.request.key == request.key)
        {
            pending.request = request;
            pending.level = Some(level);
            return;
        }
        if self.aiming_at(level, now) {
            self.pending = Some(Pending {
                request,
                level: Some(level),
                due: now + AIM_IDLE,
                aimed: true,
            });
            return;
        }
        let warm =
            self.cards.iter().any(|card| {
                card.is_open() && card.level == Some(level) && card.kind == request.kind
            }) || self.is_warm_since(request.kind, Some(level), now);
        if warm {
            self.show(request, Some(level), false, now);
        } else {
            let due = now + request.kind.rest_delay();
            self.pending = Some(Pending {
                request,
                level: Some(level),
                due,
                aimed: false,
            });
        }
    }

    /// Opens at once, sticky (keyboard or click). Returns the card id, or
    /// `None` when the chain is full or this is the press that just closed
    /// the same trigger's card (a toggle).
    pub fn open(&mut self, request: FloatRequest, now: Instant) -> Option<u64> {
        self.tick(now);
        if self
            .pressed_closed
            .as_ref()
            .is_some_and(|(key, at)| *key == request.key && *at == now)
        {
            return None;
        }
        let level = if request.kind.chains() {
            let level = self.level_for(request.anchor);
            if level >= MAX_DEPTH {
                return None;
            }
            Some(level)
        } else {
            None
        };
        let id = self.show(request, level, true, now);
        self.tick(now);
        Some(id)
    }

    /// A trigger reports that the pointer left it.
    pub fn leave(&mut self, key: &ElementId, now: Instant) {
        self.tick(now);
        if self
            .pending
            .as_ref()
            .is_some_and(|pending| pending.request.key == *key && !pending.aimed)
        {
            self.pending = None;
        }
        if self
            .tip_pending
            .as_ref()
            .is_some_and(|pending| pending.request.key == *key)
        {
            self.tip_pending = None;
        }
        for card in &mut self.cards {
            if card.is_open() && card.key == *key {
                card.trigger_hover = false;
            }
        }
        self.refresh_holds(now);
        self.tick(now);
    }

    /// The pointer moved (or left the window: `None`).
    pub fn pointer_at(&mut self, at: Option<Point<Pixels>>, now: Instant) {
        self.tick(now);
        let moved = at != self.pointer;
        self.pointer = at;
        if moved {
            self.moved = Some(now);
        }
        for card in &mut self.cards {
            card.card_hover =
                card.is_open() && card.level.is_some() && at.is_some_and(|at| card.covers(at));
        }
        if moved {
            self.update_aim(now);
        }
        // An aimed rest waits while the pointer keeps travelling, applies
        // once it stops or leaves the triangle, and is dropped once the
        // pointer reaches a card.
        let aiming = self.aim.is_some();
        let on_card = self.cards.iter().any(|card| card.card_hover);
        if let Some(pending) = self.pending.as_mut().filter(|pending| pending.aimed) {
            if on_card {
                self.pending = None;
            } else if aiming {
                if moved {
                    pending.due = now + AIM_IDLE;
                }
            } else {
                pending.due = now;
            }
        }
        self.refresh_holds(now);
        self.tick(now);
    }

    /// A mouse press at `at`. Outside every card it closes everything that
    /// floats (pins stay) and returns `true`.
    pub fn press(&mut self, at: Point<Pixels>, now: Instant) -> bool {
        self.tick(now);
        if self
            .cards
            .iter()
            .any(|card| card.is_open() && card.covers(at))
        {
            return false;
        }
        let pressed = self
            .cards
            .iter()
            .find(|card| card.is_open() && card.sticky && card.anchor.contains(&at))
            .map(|card| card.key.clone());
        let any = self.close_all(now);
        if let Some(key) = pressed {
            self.pressed_closed = Some((key, now));
        }
        any
    }

    /// Content scrolled under the pointer at `at` (not inside a card):
    /// tips close; hover cards go stale until their trigger re-anchors.
    pub fn scroll(&mut self, at: Point<Pixels>, now: Instant) {
        self.tick(now);
        if self
            .cards
            .iter()
            .any(|card| card.is_open() && card.covers(at))
        {
            return;
        }
        self.tip_pending = None;
        let tips: Vec<usize> = self
            .cards
            .iter()
            .enumerate()
            .filter(|(_, card)| card.is_open() && card.level.is_none())
            .map(|(index, _)| index)
            .collect();
        for index in tips {
            self.close_index(index, now);
        }
        for card in &mut self.cards {
            if card.is_open() && card.level.is_some() {
                card.stale = true;
            }
        }
        if self.pending.is_some() {
            self.pending = None;
        }
        self.tick(now);
    }

    /// Resolves stale cards after a paint: `anchor(key)` is the trigger's
    /// rect as reported this frame, if it reported. Stale cards whose
    /// trigger reported re-anchor; the rest close.
    pub fn resolve_stale(
        &mut self,
        anchor: impl Fn(&ElementId) -> Option<Bounds<Pixels>>,
        now: Instant,
    ) {
        let stale: Vec<(usize, Option<Bounds<Pixels>>)> = self
            .cards
            .iter()
            .enumerate()
            .filter(|(_, card)| card.is_open() && card.stale)
            .map(|(index, card)| (index, anchor(&card.key)))
            .collect();
        for (index, reported) in stale {
            match reported {
                Some(bounds) => {
                    let card = &mut self.cards[index];
                    card.anchor = bounds;
                    card.stale = false;
                }
                None => {
                    if self.cards[index].is_open() {
                        self.close_index(index, now);
                    }
                }
            }
        }
        self.tick(now);
    }

    /// A tracked trigger's current rect (re-anchors its open card).
    pub fn reanchor(&mut self, key: &ElementId, bounds: Bounds<Pixels>) {
        for card in &mut self.cards {
            if card.is_open() && card.key == *key {
                card.anchor = bounds;
                card.stale = false;
            }
        }
    }

    /// Closes open cards whose tracked trigger is gone.
    pub fn triggers_gone(&mut self, gone: impl Fn(&ElementId) -> bool, now: Instant) {
        let doomed: Vec<usize> = self
            .cards
            .iter()
            .enumerate()
            .filter(|(_, card)| card.is_open() && gone(&card.key))
            .map(|(index, _)| index)
            .collect();
        for index in doomed {
            if self.cards[index].is_open() {
                self.close_index(index, now);
            }
        }
        if self
            .pending
            .as_ref()
            .is_some_and(|pending| gone(&pending.request.key))
        {
            self.pending = None;
        }
        if self
            .tip_pending
            .as_ref()
            .is_some_and(|pending| gone(&pending.request.key))
        {
            self.tip_pending = None;
        }
        self.tick(now);
    }

    /// Esc: closes the deepest chain card (and any tip). Returns whether
    /// anything closed.
    pub fn step_back(&mut self, now: Instant) -> bool {
        self.tick(now);
        let mut any = false;
        if let Some(index) = self
            .cards
            .iter()
            .position(|card| card.is_open() && card.level.is_none())
        {
            self.close_index(index, now);
            any = true;
        }
        self.tip_pending = None;
        self.pending = None;
        let deepest = self
            .cards
            .iter()
            .enumerate()
            .filter(|(_, card)| card.is_open() && card.level.is_some())
            .max_by_key(|(_, card)| card.level)
            .map(|(index, _)| index);
        if let Some(index) = deepest {
            self.close_index(index, now);
            any = true;
        }
        if let Some(parent) = self
            .cards
            .iter_mut()
            .filter(|card| card.is_open() && card.level.is_some())
            .max_by_key(|card| card.level)
        {
            // The parent is where the user now is: it stays until left.
            parent.overflow = false;
            if !parent.sticky {
                parent.leave_at = None;
            }
        }
        self.tick(now);
        any
    }

    /// Space: pins the deepest peek or lens (dedup by key) and closes it.
    pub fn pin_top(&mut self, now: Instant) -> bool {
        self.tick(now);
        let Some(index) = self
            .cards
            .iter()
            .enumerate()
            .filter(|(_, card)| {
                card.is_open() && matches!(card.kind, FloatKind::Peek | FloatKind::Lens)
            })
            .max_by_key(|(_, card)| card.level)
            .map(|(index, _)| index)
        else {
            return false;
        };
        let card = &self.cards[index];
        let pin = Pin {
            key: card.key.clone(),
            kind: card.kind,
            content: card.content.clone(),
            title: card.title.clone(),
            presence: Presence::entering(now, self.duration(FloatKind::Peek.enter())),
            leaving: false,
        };
        self.pins.retain(|existing| existing.key != pin.key);
        self.pins.insert(0, pin);
        self.close_index(index, now);
        self.tick(now);
        true
    }

    /// Unpins `key` (its row leaves). Returns whether it was pinned.
    pub fn unpin(&mut self, key: &ElementId, now: Instant) -> bool {
        let exit = self.duration(FloatKind::Peek.exit());
        let mut any = false;
        for pin in &mut self.pins {
            if pin.key == *key && !pin.leaving {
                pin.leaving = true;
                pin.presence.retarget(0.0, exit, now);
                any = true;
            }
        }
        self.tick(now);
        any
    }

    /// Closes every card (pins stay). Returns whether anything was open.
    pub fn close_all(&mut self, now: Instant) -> bool {
        self.pending = None;
        self.tip_pending = None;
        let open: Vec<usize> = self
            .cards
            .iter()
            .enumerate()
            .filter(|(_, card)| card.is_open())
            .map(|(index, _)| index)
            .collect();
        let any = !open.is_empty();
        for index in open {
            if self.cards[index].is_open() {
                self.close_index(index, now);
            }
        }
        self.aim = None;
        self.tick(now);
        any
    }

    /// Closes the card whose trigger is `key`. Returns whether one was open.
    pub fn close_key(&mut self, key: &ElementId, now: Instant) -> bool {
        match self
            .cards
            .iter()
            .position(|card| card.is_open() && card.key == *key)
        {
            Some(index) => {
                self.close_index(index, now);
                self.tick(now);
                true
            }
            None => false,
        }
    }

    /// Finishes every entrance and exit at once (a fresh boot into the
    /// current state; the harness's "settle").
    pub fn snap(&mut self, now: Instant) {
        for card in &mut self.cards {
            let target = card.presence.target();
            card.presence = Presence {
                from: target,
                to: target,
                since: now,
                duration: Duration::ZERO,
            };
        }
        for pin in &mut self.pins {
            let target = pin.presence.target();
            pin.presence = Presence {
                from: target,
                to: target,
                since: now,
                duration: Duration::ZERO,
            };
        }
        self.tick(now);
    }

    /// Records where a card's plate was painted.
    pub fn painted(&mut self, id: u64, bounds: Bounds<Pixels>) {
        if let Some(card) = self.card_mut(id) {
            card.painted = Some(bounds);
        }
    }

    /// Records the title a card's content gave itself.
    pub fn title(&mut self, id: u64, title: SharedString) {
        if let Some(card) = self.card_mut(id) {
            card.title = Some(title.clone());
            let key = card.key.clone();
            for pin in &mut self.pins {
                if pin.key == key {
                    pin.title = Some(title.clone());
                }
            }
        }
    }

    // ------------------------------------------------------------ time

    /// Applies every deadline due at `now` and forgets settled exits.
    pub fn tick(&mut self, now: Instant) {
        // Pending rests whose time has come.
        if let Some(pending) = self.pending.take_if(|pending| pending.due <= now) {
            let level = pending.level.unwrap_or(0);
            let parent_open = level == 0
                || self
                    .cards
                    .iter()
                    .any(|card| card.is_open() && card.level == Some(level - 1));
            if parent_open && level < MAX_DEPTH {
                self.show(pending.request, Some(level), false, now);
            }
        }
        if let Some(pending) = self.tip_pending.take_if(|pending| pending.due <= now) {
            self.show(pending.request, None, false, now);
        }
        self.refresh_holds(now);
        // Unheld hover cards whose grace ran out (deepest first, so a
        // parent's close takes its children with it only once).
        let mut due: Vec<(usize, Option<usize>)> = self
            .cards
            .iter()
            .enumerate()
            .filter(|(_, card)| {
                card.is_open() && !card.sticky && card.leave_at.is_some_and(|at| at <= now)
            })
            .map(|(index, card)| (index, card.level))
            .collect();
        due.sort_by_key(|(_, level)| std::cmp::Reverse(*level));
        for (index, _) in due {
            if self.cards[index].is_open() {
                self.close_index(index, now);
            }
        }
        // A still pointer on an open card (or its trigger) deepens it.
        for index in 0..self.cards.len() {
            if let Some(at) = self.deepens_at(index)
                && at <= now
            {
                self.cards[index].deep = true;
            }
        }
        // Exits that settled.
        let finished = &mut self.finished_presence;
        self.cards.retain(|card| {
            let keep = card.is_open() || card.presence.live(now) || card.presence.value(now) > 0.0;
            if !keep {
                finished.push((card.id, card.presence));
            }
            keep
        });
        self.pins
            .retain(|pin| !pin.leaving || pin.presence.live(now) || pin.presence.value(now) > 0.0);
        self.warm
            .retain(|_, at| now.saturating_duration_since(*at) < WARM);
        if let Some(aim) = self.aim
            && (now.saturating_duration_since(aim.moved) >= AIM_IDLE
                || !self
                    .cards
                    .iter()
                    .any(|card| card.id == aim.card && card.is_open()))
        {
            self.aim = None;
        }
        if self
            .pressed_closed
            .as_ref()
            .is_some_and(|(_, at)| *at != now)
        {
            self.pressed_closed = None;
        }
    }

    /// The next moment something changes without an event.
    #[must_use]
    pub fn next_deadline(&self, now: Instant) -> Option<Instant> {
        let mut next: Option<Instant> = None;
        let mut take = |at: Instant| {
            next = Some(next.map_or(at, |next| next.min(at)));
        };
        if let Some(pending) = &self.pending {
            take(pending.due);
        }
        if let Some(pending) = &self.tip_pending {
            take(pending.due);
        }
        for card in &self.cards {
            if let Some(at) = card.leave_at
                && card.is_open()
                && !card.sticky
            {
                take(at);
            }
            if !card.is_open() {
                take(card.presence.ends().max(now));
            }
        }
        for pin in &self.pins {
            if pin.leaving {
                take(pin.presence.ends().max(now));
            }
        }
        if let Some(aim) = &self.aim {
            take(aim.moved + AIM_IDLE);
        }
        for index in 0..self.cards.len() {
            if let Some(at) = self.deepens_at(index) {
                take(at);
            }
        }
        for at in self.warm.values() {
            take(*at + WARM);
        }
        next
    }

    // ------------------------------------------------------------ internals

    /// When the card at `index` deepens if the pointer stays still.
    fn deepens_at(&self, index: usize) -> Option<Instant> {
        let card = &self.cards[index];
        let resting = card.is_open()
            && !card.deep
            && card.level.is_some()
            && (card.trigger_hover || card.card_hover);
        resting.then(|| self.moved.map_or(card.shown, |moved| moved.max(card.shown)) + DEEPEN)
    }

    fn is_warm_since(&self, kind: FloatKind, level: Option<usize>, now: Instant) -> bool {
        self.warm
            .get(&(kind, level))
            .is_some_and(|at| now.saturating_duration_since(*at) < WARM)
            || self
                .cards
                .iter()
                .any(|card| !card.is_open() && card.kind == kind && card.level == level)
    }

    /// The level a rest on `anchor` opens at: one below the deepest open
    /// card whose painted plate holds the anchor's centre, else the root.
    fn level_for(&self, anchor: Bounds<Pixels>) -> usize {
        let centre = anchor.center();
        self.cards
            .iter()
            .filter(|card| card.is_open() && card.level.is_some() && card.covers(centre))
            .filter_map(|card| card.level)
            .max()
            .map_or(0, |level| level + 1)
    }

    /// Shows `request` at `level`: reuses (morphs) the card of the same kind
    /// already there or leaving there, else opens a new one; closes every
    /// other open card at that level or deeper.
    fn show(
        &mut self,
        request: FloatRequest,
        level: Option<usize>,
        sticky: bool,
        now: Instant,
    ) -> u64 {
        let reuse = self
            .cards
            .iter()
            .enumerate()
            .filter(|(_, card)| card.level == level && card.kind == request.kind)
            .max_by_key(|(_, card)| (card.is_open(), card.presence.value(now).to_bits()))
            .map(|(index, _)| index);
        let doomed: Vec<usize> = self
            .cards
            .iter()
            .enumerate()
            .filter(|(index, card)| {
                Some(*index) != reuse
                    && card.is_open()
                    && match (card.level, level) {
                        (Some(card_level), Some(level)) => card_level >= level,
                        (None, None) => true,
                        _ => false,
                    }
            })
            .map(|(index, _)| index)
            .collect();
        for index in doomed {
            if self.cards[index].is_open() {
                self.close_index(index, now);
            }
        }
        if self
            .pending
            .as_ref()
            .is_some_and(|pending| level.is_some() && pending.level >= level)
        {
            self.pending = None;
        }
        if level.is_none() {
            self.tip_pending = None;
        }
        let enter = self.duration(request.kind.enter());
        let id = match reuse {
            Some(index) => {
                let card = &mut self.cards[index];
                let swapped = card.key != request.key;
                card.key = request.key;
                card.anchor = request.anchor;
                card.content = request.content;
                card.side = request.side;
                card.sticky = sticky;
                card.trigger_hover = !sticky;
                card.leave_at = None;
                card.stale = false;
                card.overflow = false;
                if swapped {
                    card.title = None;
                    card.swapped = Some(now);
                    card.deep = false;
                    card.shown = now;
                }
                if card.closing {
                    card.closing = false;
                    card.deep = false;
                    card.shown = now;
                    card.presence.retarget(1.0, enter, now);
                }
                card.id
            }
            None => {
                let id = self.fresh_id();
                self.cards.push(Card {
                    id,
                    key: request.key,
                    kind: request.kind,
                    side: request.side,
                    anchor: request.anchor,
                    content: request.content,
                    level,
                    sticky,
                    presence: Presence::entering(now, enter),
                    closing: false,
                    trigger_hover: !sticky,
                    card_hover: false,
                    leave_at: None,
                    painted: None,
                    swapped: None,
                    stale: false,
                    overflow: false,
                    title: None,
                    shown: now,
                    deep: false,
                });
                id
            }
        };
        if let Some(parent_level) = level.and_then(|level| level.checked_sub(1))
            && let Some(parent) = self
                .cards
                .iter_mut()
                .find(|card| card.is_open() && card.level == Some(parent_level))
        {
            parent.overflow = false;
        }
        id
    }

    fn close_index(&mut self, index: usize, now: Instant) {
        let exit = self.duration(self.cards[index].kind.exit());
        let (id, kind, level) = {
            let card = &mut self.cards[index];
            if card.closing {
                return;
            }
            card.closing = true;
            card.presence.retarget(0.0, exit, now);
            card.leave_at = None;
            card.trigger_hover = false;
            card.card_hover = false;
            card.stale = false;
            (card.id, card.kind, card.level)
        };
        self.closed.push(id);
        self.warm.insert((kind, level), now);
        if self.aim.is_some_and(|aim| aim.card == id) {
            self.aim = None;
        }
        // Children go with their parent.
        if let Some(level) = level {
            let deeper: Vec<usize> = self
                .cards
                .iter()
                .enumerate()
                .filter(|(_, card)| card.is_open() && card.level.is_some_and(|l| l > level))
                .map(|(index, _)| index)
                .collect();
            for index in deeper {
                self.close_index(index, now);
            }
            if self
                .pending
                .as_ref()
                .is_some_and(|pending| pending.level.is_some_and(|l| l > level))
            {
                self.pending = None;
            }
        }
    }

    /// Recomputes which hover cards are held and arms or disarms their
    /// close deadlines.
    fn refresh_holds(&mut self, now: Instant) {
        let aim = self
            .aim
            .filter(|aim| now.saturating_duration_since(aim.moved) < AIM_IDLE)
            .map(|aim| aim.card);
        // Deepest first: a held child holds its parent.
        let mut order: Vec<usize> = (0..self.cards.len())
            .filter(|index| self.cards[*index].is_open())
            .collect();
        order.sort_by_key(|index| std::cmp::Reverse(self.cards[*index].level));
        let mut held_levels: Vec<usize> = Vec::new();
        for index in order {
            let card = &self.cards[index];
            let child_held = card
                .level
                .is_some_and(|level| held_levels.contains(&(level + 1)));
            let held = card.sticky
                || card.trigger_hover
                || card.card_hover
                || child_held
                || aim == Some(card.id)
                || self
                    .pending
                    .as_ref()
                    .is_some_and(|pending| pending.aimed && pending.level == card.level);
            if held && let Some(level) = card.level {
                held_levels.push(level);
            }
            let grace = card.kind.grace();
            let card = &mut self.cards[index];
            if held {
                card.leave_at = None;
            } else if card.leave_at.is_none() {
                card.leave_at = Some(now + grace);
            }
        }
    }

    fn update_aim(&mut self, now: Instant) {
        let Some(at) = self.pointer else {
            self.aim = None;
            return;
        };
        // The deepest open hover card whose trigger the pointer is on sets
        // a fresh apex.
        let over_trigger = self
            .cards
            .iter()
            .filter(|card| card.is_open() && card.level.is_some() && !card.sticky)
            .filter(|card| grow(card.anchor, 2.0).contains(&at))
            .max_by_key(|card| card.level)
            .map(|card| card.id);
        if let Some(card) = over_trigger {
            self.aim = Some(Aim {
                card,
                origin: at,
                moved: now,
            });
            return;
        }
        let Some(aim) = self.aim else {
            return;
        };
        let Some(card) = self
            .cards
            .iter()
            .find(|card| card.id == aim.card && card.is_open())
        else {
            self.aim = None;
            return;
        };
        if card.covers(at) {
            self.aim = None;
            return;
        }
        let heading = card
            .painted
            .is_some_and(|plate| toward(aim.origin, plate, at));
        self.aim = heading.then_some(Aim {
            card: aim.card,
            origin: at,
            moved: now,
        });
    }

    /// Whether the pointer is aiming at an open hover card at `level`.
    fn aiming_at(&self, level: usize, now: Instant) -> bool {
        self.aim.is_some_and(|aim| {
            now.saturating_duration_since(aim.moved) < AIM_IDLE
                && self
                    .cards
                    .iter()
                    .any(|card| card.id == aim.card && card.is_open() && card.level == Some(level))
        })
    }
}

fn grow(bounds: Bounds<Pixels>, by: f32) -> Bounds<Pixels> {
    Bounds::new(
        point(bounds.origin.x - px(by), bounds.origin.y - px(by)),
        gpui::size(
            bounds.size.width + px(2.0 * by),
            bounds.size.height + px(2.0 * by),
        ),
    )
}

/// Whether `to` lies in the triangle from `from` to the near edge of
/// `plate` (widened a little), i.e. the move `from → to` heads into the card.
fn toward(from: Point<Pixels>, plate: Bounds<Pixels>, to: Point<Pixels>) -> bool {
    let f = |value: Pixels| f32::from(value);
    let (x0, y0) = (f(plate.origin.x), f(plate.origin.y));
    let (x1, y1) = (x0 + f(plate.size.width), y0 + f(plate.size.height));
    let (ox, oy) = (f(from.x), f(from.y));
    let slack = 14.0;
    let (a, b) = if oy <= y0 {
        ((x0 - slack, y0), (x1 + slack, y0))
    } else if oy >= y1 {
        ((x0 - slack, y1), (x1 + slack, y1))
    } else if ox <= x0 {
        ((x0, y0 - slack), (x0, y1 + slack))
    } else {
        ((x1, y0 - slack), (x1, y1 + slack))
    };
    // The apex sits a hair behind the last point so a move along the
    // triangle's edge (or a 1 px wobble) still counts.
    let (dx, dy) = ((a.0 + b.0) / 2.0 - ox, (a.1 + b.1) / 2.0 - oy);
    let len = (dx * dx + dy * dy).sqrt().max(1e-3);
    let apex = (ox - dx / len * 3.0, oy - dy / len * 3.0);
    let p = (f(to.x), f(to.y));
    let sign = |p1: (f32, f32), p2: (f32, f32), p3: (f32, f32)| {
        (p1.0 - p3.0) * (p2.1 - p3.1) - (p2.0 - p3.0) * (p1.1 - p3.1)
    };
    let d1 = sign(p, apex, a);
    let d2 = sign(p, a, b);
    let d3 = sign(p, b, apex);
    let negative = d1 < 0.0 || d2 < 0.0 || d3 < 0.0;
    let positive = d1 > 0.0 || d2 > 0.0 || d3 > 0.0;
    !(negative && positive)
}

#[cfg(test)]
pub(crate) mod tests;
