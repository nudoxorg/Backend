//! Presence: keyed enter and exit for lists, stacks and floats.
//!
//! A view hands [`Presence::sync`] the keys that should be on screen, in
//! order, every time it renders. Presence diffs them against what it holds
//! and returns the items to draw this frame — the wanted keys in the wanted
//! order, plus every key that is still leaving, holding the slot it had.
//!
//! # The model
//!
//! Every item travels along one [`Act`] at a time — its way in or its way out
//! — at a progress `s` in `0..=1` that moves linearly on the executor clock
//! (the act's keyframes do the easing). Interruptions never pick a new act:
//!
//! - a key removed while it enters plays its way in **backwards** from where
//!   it is and is dropped at `s = 0`;
//! - a key that returns while it leaves plays its way out **backwards** from
//!   where it is and is present again at `s = 0`;
//!
//! so a pose never jumps, and no act ever restarts because of a reversal.
//! [`Presence::is_settled`] is true only when nothing is entering or leaving.
//!
//! # Room
//!
//! Each act carries a pose track (translate, scale, opacity) and a room
//! track: how much of its natural main-axis size the item's slot takes.
//! Wrap the item's element in [`Item::slot`]: the slot reserves
//! `natural × share + px`, so a list opens (with the board's 5 px make-room
//! overshoot) as a row drops in and closes after a row has faded. At rest a
//! slot is layout-transparent: it returns its child's own layout node.
//!
//! ```ignore
//! let rows = self.presence.sync(self.rows.iter().map(|row| row.id), window, cx);
//! div().flex().flex_col().children(rows.into_iter().map(|item| {
//!     let row = self.row(&item.key);
//!     item.slot(row)
//! }))
//! ```

use super::keys::{self, Keys, Mix, Pose};
use super::{epoch, now, reduced, request_frame};
use crate::probe::{self, TrackKind, TrackSample};
use crate::tokens::motion::{DROP, EMPH, GLIDE, QUICK, SCENE, SNAP, STD};
use gpui::{
    AbsoluteLength, AlignItems, AnyElement, App, Bounds, DefiniteLength, Display, ElementId,
    FlexDirection, Global, GlobalElementId, InspectorElementId, IntoElement, LayoutId, Length,
    Pixels, SharedString, Style, Window, layer, point, px,
};
use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use std::time::{Duration, Instant};

/// How much of its natural main-axis size a slot takes: `share` of it plus
/// `px` (the board's make-room overshoot is an absolute 5 px, not a share).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Extent {
    /// Fraction of the natural size (may overshoot 1).
    pub share: f32,
    /// Absolute extra, in px.
    pub px: f32,
}

impl Extent {
    /// Takes no room.
    pub const NONE: Self = Self { share: 0.0, px: 0.0 };
    /// Takes exactly its natural size.
    pub const FULL: Self = Self { share: 1.0, px: 0.0 };

    /// `share` of the natural size.
    #[must_use]
    pub const fn share(share: f32) -> Self {
        Self { share, px: 0.0 }
    }

    /// Whether this is exactly the natural size (the slot steps aside).
    #[must_use]
    pub fn is_full(self) -> bool {
        (self.share - 1.0).abs() < 1e-5 && self.px.abs() < 1e-4
    }

    /// The size a slot reserves for an item whose natural size is `natural`.
    #[must_use]
    pub fn of(self, natural: Pixels) -> Pixels {
        (natural * self.share + px(self.px)).max(Pixels::ZERO)
    }
}

impl Default for Extent {
    /// An empty room track leaves layout alone.
    fn default() -> Self {
        Self::FULL
    }
}

impl Mix for Extent {
    fn mix(self, to: Self, t: f32) -> Self {
        Self {
            share: self.share.mix(to.share, t),
            px: self.px.mix(to.px, t),
        }
    }
}

/// One way in or out: a pose track and a room track, both sampled at the
/// same progress over `duration` (their own durations are ignored). A way
/// in runs from absent (`s = 0`) to [`Pose::REST`] and full room (`s = 1`);
/// a way out runs from rest (`s = 0`) to absent (`s = 1`).
#[derive(Clone, Debug, PartialEq)]
pub struct Act {
    /// Time from one end to the other.
    pub duration: Duration,
    /// The item's pose along the act.
    pub pose: Keys<Pose>,
    /// The item's slot along the act.
    pub room: Keys<Extent>,
}

impl Act {
    /// The pose and room at progress `s`.
    #[must_use]
    pub fn at(&self, s: f32) -> (Pose, Extent) {
        (self.pose.at(s), self.room.at(s))
    }

    /// The same act over another duration.
    #[must_use]
    pub fn with_duration(mut self, duration: Duration) -> Self {
        self.duration = duration;
        self
    }
}

const fn faded(y: f32, scale: f32) -> Pose {
    Pose {
        x: 0.0,
        y,
        sx: scale,
        sy: scale,
        rotate: 0.0,
        opacity: 0.0,
    }
}

const fn extent(share: f32, px: f32) -> Extent {
    Extent { share, px }
}

/// The board's make-room choreography inside a drop-in: the list opens with
/// the `make-room` timing (EMPH, 5 px past at 55 %) while the card falls
/// (SCENE, lands at 45 %). 0.337 = 0.55 × 380 / 620; 0.613 = 380 / 620.
static DROP_ROOM: [(f32, Extent); 4] = [
    (0.0, Extent::NONE),
    (0.337, extent(1.0, 5.0)),
    (0.613, Extent::FULL),
    (1.0, Extent::FULL),
];
static OPEN_ROOM: [(f32, Extent); 3] = [
    (0.0, Extent::NONE),
    (0.6, Extent::FULL),
    (1.0, Extent::FULL),
];
static KEEP_ROOM: [(f32, Extent); 2] = [(0.0, Extent::FULL), (1.0, Extent::FULL)];
/// Out: hold the room while the item fades, then close (make-room reversed).
static CLOSE_ROOM: [(f32, Extent); 3] = [
    (0.0, Extent::FULL),
    (0.3, Extent::FULL),
    (1.0, Extent::NONE),
];
/// Out: fade, sink 6 px and shrink 2 % in the first 42 %, accelerating.
static LEAVE_POSE: [(f32, Pose); 3] = [
    (0.0, Pose::REST),
    (0.42, faded(6.0, 0.98)),
    (1.0, faded(6.0, 0.98)),
];
static FADE_IN_POSE: [(f32, Pose); 2] = [(0.0, faded(0.0, 1.0)), (1.0, Pose::REST)];
static FADE_OUT_POSE: [(f32, Pose); 2] = [(0.0, Pose::REST), (1.0, faded(0.0, 1.0))];
static POP_OUT_POSE: [(f32, Pose); 2] = [(0.0, Pose::REST), (1.0, faded(0.0, 0.6))];

/// Named acts. Ways in end at rest; ways out start there.
pub mod act {
    use super::{
        Act, CLOSE_ROOM, DROP, DROP_ROOM, EMPH, FADE_IN_POSE, FADE_OUT_POSE, GLIDE, KEEP_ROOM,
        Keys, LEAVE_POSE, OPEN_ROOM, POP_OUT_POSE, QUICK, SCENE, SNAP, STD, keys,
    };

    /// `drop-in`: falls from above squashed tall, lands wide at 45 %,
    /// rebounds and settles, while the list makes room (5 px past at 55 %
    /// of its own 380 ms).
    pub const DROP_IN: Act = Act {
        duration: SCENE,
        pose: keys::DROP_IN,
        room: Keys::new(SCENE, &DROP_ROOM, GLIDE),
    };
    /// `rise`: 10 px up into place with a fade; the room opens first.
    pub const RISE: Act = Act {
        duration: STD,
        pose: keys::RISE,
        room: Keys::new(STD, &OPEN_ROOM, GLIDE),
    };
    /// `pop`: from 60 % and clear to 108 % and back; the room opens first.
    pub const POP: Act = Act {
        duration: STD,
        pose: keys::POP,
        room: Keys::new(STD, &OPEN_ROOM, GLIDE),
    };
    /// A float's way in (peek cards, popovers): a small lift, no room.
    pub const PEEK: Act = Act {
        duration: QUICK,
        pose: keys::PEEK,
        room: Keys::new(QUICK, &KEEP_ROOM, GLIDE),
    };
    /// A plain fade in, no room (tooltips).
    pub const FADE_IN: Act = Act {
        duration: QUICK,
        pose: Keys::new(QUICK, &FADE_IN_POSE, GLIDE),
        room: Keys::new(QUICK, &KEEP_ROOM, GLIDE),
    };
    /// `leave`: fades, sinks 6 px and shrinks 2 % (accelerating away), then
    /// the list closes over it.
    pub const LEAVE: Act = Act {
        duration: EMPH,
        pose: Keys::new(EMPH, &LEAVE_POSE, DROP),
        room: Keys::new(EMPH, &CLOSE_ROOM, GLIDE),
    };
    /// A plain fade out, no room (tooltips, floats).
    pub const FADE_OUT: Act = Act {
        duration: QUICK,
        pose: Keys::new(QUICK, &FADE_OUT_POSE, DROP),
        room: Keys::new(QUICK, &KEEP_ROOM, GLIDE),
    };
    /// Shrinks to 60 % and clears, no room (the reverse of a pop, for
    /// floats that were popped).
    pub const POP_OUT: Act = Act {
        duration: QUICK,
        pose: Keys::new(QUICK, &POP_OUT_POSE, SNAP),
        room: Keys::new(QUICK, &KEEP_ROOM, GLIDE),
    };
}

/// Where an item stands.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub enum Phase {
    /// Coming in (or coming back while it was leaving).
    Entering,
    /// At rest.
    Present,
    /// Going out (or going back while it was entering). Draw it, but it is
    /// no longer part of the data: don't make it interactive.
    Leaving,
}

/// The main axis of the list a slot sits in.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Hash)]
pub enum Axis {
    /// Rows stacked top to bottom: the slot animates its height.
    #[default]
    Vertical,
    /// Items side by side: the slot animates its width.
    Horizontal,
}

/// A key to sync, optionally with its own ways in and out (a float that is
/// a tip leaves differently from one that is a menu).
#[derive(Clone, Debug)]
pub struct Entry {
    /// The stable key.
    pub key: ElementId,
    /// Its way in, if not the presence's default.
    pub enter: Option<Act>,
    /// Its way out, if not the presence's default.
    pub exit: Option<Act>,
}

impl Entry {
    /// An entry with the presence's default acts.
    pub fn new(key: impl Into<ElementId>) -> Self {
        Self {
            key: key.into(),
            enter: None,
            exit: None,
        }
    }

    /// Enters with `act`.
    #[must_use]
    pub fn enter(mut self, act: Act) -> Self {
        self.enter = Some(act);
        self
    }

    /// Leaves with `act`.
    #[must_use]
    pub fn exit(mut self, act: Act) -> Self {
        self.exit = Some(act);
        self
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Way {
    In,
    Out,
}

#[derive(Clone, Copy, Debug)]
enum State {
    Present,
    Moving {
        way: Way,
        forward: bool,
        /// Progress when this segment started.
        from: f32,
        start: Instant,
        /// Held at `from` this long first (stagger).
        hold: Duration,
    },
}

#[derive(Clone, Debug)]
struct Record {
    key: ElementId,
    enter: Act,
    exit: Act,
    state: State,
    /// A live sample went to the probe; the next settle sends a final one.
    reported: bool,
}

impl Record {
    fn act(&self, way: Way) -> &Act {
        match way {
            Way::In => &self.enter,
            Way::Out => &self.exit,
        }
    }

    /// Progress along the current act at `now`, and whether it reached the
    /// end it is heading to.
    fn progress(&self, now: Instant) -> (f32, bool) {
        let State::Moving {
            way,
            forward,
            from,
            start,
            hold,
        } = self.state
        else {
            return (1.0, true);
        };
        let run = now.saturating_duration_since(start);
        let active = run.saturating_sub(hold);
        let span = self.act(way).duration.as_secs_f32();
        let travelled = if span <= 0.0 {
            1.0
        } else {
            active.as_secs_f32() / span
        };
        if forward {
            let s = (from + travelled).min(1.0);
            (s, s >= 1.0)
        } else {
            let s = (from - travelled).max(0.0);
            (s, s <= 0.0)
        }
    }

    /// Whether the item belongs to the data (entering, present, returning).
    fn alive(&self) -> bool {
        match self.state {
            State::Present => true,
            State::Moving { way, forward, .. } => (way == Way::In) == forward,
        }
    }

    fn phase(&self) -> Phase {
        match self.state {
            State::Present => Phase::Present,
            State::Moving { .. } if self.alive() => Phase::Entering,
            State::Moving { .. } => Phase::Leaving,
        }
    }

    /// Turns around from where it is.
    fn reverse(&mut self, now: Instant) {
        if let State::Moving { way, forward, .. } = self.state {
            let (s, _) = self.progress(now);
            self.state = State::Moving {
                way,
                forward: !forward,
                from: s,
                start: now,
                hold: Duration::ZERO,
            };
        }
    }

    /// Starts leaving (a no-op if already leaving).
    fn leave(&mut self, now: Instant) {
        match self.state {
            State::Present => {
                self.state = State::Moving {
                    way: Way::Out,
                    forward: true,
                    from: 0.0,
                    start: now,
                    hold: Duration::ZERO,
                };
            }
            State::Moving { .. } if self.alive() => self.reverse(now),
            State::Moving { .. } => {}
        }
    }

    fn sample(&self, now: Instant) -> Sampled {
        match self.state {
            State::Present => Sampled {
                pose: Pose::REST,
                room: Extent::FULL,
                presence: 1.0,
                target: 1.0,
                velocity: 0.0,
                live: false,
            },
            State::Moving { way, forward, .. } => {
                let (s, _) = self.progress(now);
                let act = self.act(way);
                let (pose, room) = act.at(s);
                let presence = match way {
                    Way::In => s,
                    Way::Out => 1.0 - s,
                };
                let rate = if act.duration.is_zero() {
                    0.0
                } else {
                    1.0 / act.duration.as_secs_f32()
                };
                let toward_presence = (way == Way::In) == forward;
                Sampled {
                    pose,
                    room,
                    presence,
                    target: if toward_presence { 1.0 } else { 0.0 },
                    velocity: if toward_presence { rate } else { -rate },
                    live: true,
                }
            }
        }
    }
}

/// An item that came to rest or left since the probe last looked.
#[derive(Clone, Debug)]
struct Finished {
    key: ElementId,
    presence: f32,
    ended: Option<(Act, f32)>,
}

impl Finished {
    fn of(record: &Record, presence: f32) -> Self {
        let ended = match record.state {
            State::Moving { way, forward, .. } => {
                Some((record.act(way).clone(), if forward { 1.0 } else { 0.0 }))
            }
            State::Present => None,
        };
        Self {
            key: record.key.clone(),
            presence,
            ended,
        }
    }
}

#[derive(Clone, Copy, Debug)]
struct Sampled {
    pose: Pose,
    room: Extent,
    presence: f32,
    target: f32,
    velocity: f32,
    live: bool,
}

/// The pure diffing model, on an explicit clock (tests drive it directly).
#[derive(Clone, Debug)]
pub(crate) struct Model {
    records: Vec<Record>,
    enter: Act,
    exit: Act,
    stagger: Duration,
    appear: bool,
    synced: bool,
    /// Items that settled or left since the last sample, for the probe:
    /// the key, its final presence, and the act and progress it ended on.
    finished: Vec<Finished>,
    /// The natural main-axis size each item's slot last measured.
    naturals: HashMap<ElementId, Pixels>,
}

impl Model {
    pub(crate) fn new() -> Self {
        Self {
            records: Vec::new(),
            enter: act::DROP_IN,
            exit: act::LEAVE,
            stagger: Duration::ZERO,
            appear: false,
            synced: false,
            finished: Vec::new(),
            naturals: HashMap::new(),
        }
    }

    /// Moves every item to `now`: finished ways in come to rest, finished
    /// ways out are dropped.
    pub(crate) fn advance(&mut self, now: Instant) {
        let finished = &mut self.finished;
        self.records.retain_mut(|record| {
            if matches!(record.state, State::Present) {
                return true;
            }
            let (_, done) = record.progress(now);
            if !done {
                return true;
            }
            let alive = record.alive();
            if record.reported {
                finished.push(Finished::of(record, if alive { 1.0 } else { 0.0 }));
                record.reported = false;
            }
            if alive {
                record.state = State::Present;
            }
            alive
        });
    }

    pub(crate) fn sync(&mut self, entries: Vec<Entry>, now: Instant, reduced: bool) {
        self.advance(now);
        let first = !self.synced;
        self.synced = true;

        // Wanted keys, first occurrence wins.
        let mut seen = HashSet::with_capacity(entries.len());
        let wanted: Vec<Entry> = entries
            .into_iter()
            .filter(|entry| seen.insert(entry.key.clone()))
            .collect();

        // Fast path: exactly the same alive keys in the same order and
        // nothing leaving — only the acts may have changed.
        let unchanged = self.records.len() == wanted.len()
            && self
                .records
                .iter()
                .zip(&wanted)
                .all(|(record, entry)| record.key == entry.key && record.alive());
        if unchanged {
            for (record, entry) in self.records.iter_mut().zip(wanted) {
                if let Some(exit) = entry.exit {
                    record.exit = exit;
                }
            }
            return;
        }

        // The previous order, then the records by key.
        let order: Vec<ElementId> = self.records.iter().map(|r| r.key.clone()).collect();
        let mut previous: HashMap<ElementId, Record> = std::mem::take(&mut self.records)
            .into_iter()
            .map(|record| (record.key.clone(), record))
            .collect();

        // The wanted items, in the wanted order.
        let mut arrivals = 0_u32;
        let mut alive: Vec<Record> = Vec::with_capacity(wanted.len());
        for entry in wanted {
            let record = match previous.remove(&entry.key) {
                Some(mut record) => {
                    if let Some(exit) = entry.exit {
                        record.exit = exit;
                    }
                    if !record.alive() {
                        if reduced {
                            record.state = State::Present;
                        } else {
                            record.reverse(now);
                        }
                    }
                    record
                }
                None => {
                    let quiet = reduced || (first && !self.appear);
                    let hold = self.stagger * arrivals;
                    if !quiet {
                        arrivals += 1;
                    }
                    Record {
                        key: entry.key,
                        enter: entry.enter.unwrap_or_else(|| self.enter.clone()),
                        exit: entry.exit.unwrap_or_else(|| self.exit.clone()),
                        state: if quiet {
                            State::Present
                        } else {
                            State::Moving {
                                way: Way::In,
                                forward: true,
                                from: 0.0,
                                start: now,
                                hold,
                            }
                        },
                        reported: false,
                    }
                }
            };
            alive.push(record);
        }

        // Everything left over leaves, holding its slot: it stays right
        // after the last item before it (in the previous order) that is
        // still wanted, and leavers that shared an anchor keep their order.
        let mut anchor: Option<ElementId> = None;
        let mut after: HashMap<Option<ElementId>, Vec<Record>> = HashMap::new();
        for key in order {
            let Some(mut record) = previous.remove(&key) else {
                anchor = Some(key);
                continue;
            };
            if reduced {
                if record.reported {
                    self.finished.push(Finished::of(&record, 0.0));
                }
                continue;
            }
            record.leave(now);
            after.entry(anchor.clone()).or_default().push(record);
        }

        let mut records = Vec::with_capacity(alive.len() + after.values().map(Vec::len).sum::<usize>());
        records.extend(after.remove(&None).unwrap_or_default());
        for record in alive {
            let key = Some(record.key.clone());
            records.push(record);
            if let Some(leavers) = after.remove(&key) {
                records.extend(leavers);
            }
        }
        self.records = records;
        if self.naturals.len() > self.records.len() {
            let held: HashSet<&ElementId> = self.records.iter().map(|r| &r.key).collect();
            self.naturals.retain(|key, _| held.contains(key));
        }
    }
}

impl Model {
    /// Whether anything is entering or leaving at `now`.
    pub(crate) fn is_settled(&self, now: Instant) -> bool {
        self.records.iter().all(|record| match record.state {
            State::Present => true,
            State::Moving { .. } => record.progress(now).1,
        })
    }

    /// Jumps everything to where it is heading.
    pub(crate) fn settle(&mut self) {
        let finished = &mut self.finished;
        self.records.retain_mut(|record| {
            let alive = record.alive();
            if record.reported {
                finished.push(Finished::of(record, if alive { 1.0 } else { 0.0 }));
                record.reported = false;
            }
            record.state = State::Present;
            alive
        });
    }

    fn samples(&self, now: Instant) -> impl Iterator<Item = (&Record, Sampled)> + '_ {
        self.records
            .iter()
            .map(move |record| (record, record.sample(now)))
    }

    #[cfg(test)]
    pub(crate) fn keys(&self) -> impl Iterator<Item = &ElementId> + '_ {
        self.records.iter().map(|record| &record.key)
    }

    fn natural(&self, key: &ElementId) -> Option<Pixels> {
        self.naturals.get(key).copied()
    }

    fn measure(&mut self, key: &ElementId, natural: Pixels) {
        if let Some(size) = self.naturals.get_mut(key) {
            *size = natural;
        } else if self.records.iter().any(|record| &record.key == key) {
            self.naturals.insert(key.clone(), natural);
        }
    }
}

struct Inner {
    scope: SharedString,
    axis: Axis,
    model: Model,
}

/// Keyed enter and exit (see the [module docs](self)). Cloning shares the
/// presence; hold it in the view that renders the list.
#[derive(Clone)]
pub struct Presence {
    inner: Rc<RefCell<Inner>>,
}

/// One item to draw this frame.
#[derive(Clone)]
pub struct Item {
    /// The item's key.
    pub key: ElementId,
    /// Where it stands.
    pub phase: Phase,
    /// How present it is, `0..=1` (0 = gone, 1 = at rest).
    pub presence: f32,
    /// Its pose this frame (rest once present).
    pub pose: Pose,
    /// Its slot this frame (full once present).
    pub room: Extent,
    /// Its position in this frame's list (leavers included).
    pub index: usize,
    axis: Axis,
    inner: Rc<RefCell<Inner>>,
}

impl Item {
    /// Whether the item is leaving (draw it; don't make it interactive).
    #[must_use]
    pub fn is_leaving(&self) -> bool {
        self.phase == Phase::Leaving
    }

    /// Wraps the item's element in its slot: the room it reserves and the
    /// pose it is painted at — translated, scaled about its centre, and
    /// faded as one surface (a compositing layer). Layout-transparent and
    /// paint-transparent at rest.
    pub fn slot(&self, child: impl IntoElement) -> Slot {
        let pose = self.pose;
        Slot {
            child: layer(child)
                .translate(point(px(pose.x), px(pose.y)))
                .scale_xy(pose.sx, pose.sy)
                .opacity(pose.opacity)
                .into_any_element(),
            key: self.key.clone(),
            inner: Rc::clone(&self.inner),
            room: self.room,
            axis: self.axis,
            layout: SlotLayout::Through,
        }
    }
}

impl Presence {
    /// An empty presence. `scope` names its tracks in the probe ledger
    /// (`{scope}.{key}.presence`). The defaults: drop in, leave, no stagger,
    /// and the first sync's keys appear without animating.
    pub fn new(scope: impl Into<SharedString>) -> Self {
        Self {
            inner: Rc::new(RefCell::new(Inner {
                scope: scope.into(),
                axis: Axis::Vertical,
                model: Model::new(),
            })),
        }
    }

    /// The presence registered under `scope`, shared by every view that asks
    /// for it — for lists whose view can be rebuilt mid-exit.
    pub fn scoped(scope: impl Into<SharedString>, cx: &mut App) -> Self {
        let scope = scope.into();
        cx.default_global::<Scopes>()
            .0
            .entry(scope.clone())
            .or_insert_with(|| Self::new(scope))
            .clone()
    }

    /// The default way in.
    #[must_use]
    pub fn enter(self, act: Act) -> Self {
        self.inner.borrow_mut().model.enter = act;
        self
    }

    /// The default way out.
    #[must_use]
    pub fn exit(self, act: Act) -> Self {
        self.inner.borrow_mut().model.exit = act;
        self
    }

    /// Delays each arrival in one sync by this much after the previous one.
    #[must_use]
    pub fn stagger(self, stagger: Duration) -> Self {
        self.inner.borrow_mut().model.stagger = stagger;
        self
    }

    /// Whether the first sync's keys animate in (default: they are simply
    /// there).
    #[must_use]
    pub fn appear(self, appear: bool) -> Self {
        self.inner.borrow_mut().model.appear = appear;
        self
    }

    /// The main axis slots animate.
    #[must_use]
    pub fn axis(self, axis: Axis) -> Self {
        self.inner.borrow_mut().axis = axis;
        self
    }

    /// Diffs `keys` (wanted, in order) against what is held and returns this
    /// frame's items. Call from `render` every time.
    pub fn sync<K: Into<ElementId>>(
        &self,
        keys: impl IntoIterator<Item = K>,
        window: &mut Window,
        cx: &mut App,
    ) -> Vec<Item> {
        self.sync_entries(keys.into_iter().map(Entry::new), window, cx)
    }

    /// [`Presence::sync`] with per-item acts.
    pub fn sync_entries(
        &self,
        entries: impl IntoIterator<Item = Entry>,
        window: &mut Window,
        cx: &mut App,
    ) -> Vec<Item> {
        let now = now(cx);
        let reduced = reduced(cx);
        let entries = entries.into_iter().collect();
        self.inner.borrow_mut().model.sync(entries, now, reduced);
        let items = self.items_at(now, cx);
        if items.iter().any(|item| item.phase != Phase::Present) {
            request_frame(window, cx);
        }
        items
    }

    fn items_at(&self, now: Instant, cx: &mut App) -> Vec<Item> {
        let recording = probe::enabled(cx);
        let (items, reports) = {
            let mut inner = self.inner.borrow_mut();
            let axis = inner.axis;
            let mut reports = Vec::new();
            let items = inner
                .model
                .samples(now)
                .enumerate()
                .map(|(index, (record, sample))| {
                    if recording && sample.live {
                        reports.push(Report::live(record, sample, now));
                    }
                    Item {
                        key: record.key.clone(),
                        phase: record.phase(),
                        presence: sample.presence,
                        pose: sample.pose,
                        room: sample.room,
                        index,
                        axis,
                        inner: Rc::clone(&self.inner),
                    }
                })
                .collect::<Vec<_>>();
            let finished = std::mem::take(&mut inner.model.finished);
            for record in &mut inner.model.records {
                if recording && !matches!(record.state, State::Present) {
                    record.reported = true;
                }
            }
            if recording {
                reports.extend(finished.into_iter().map(Report::settled));
            }
            (items, (inner.scope.clone(), reports))
        };
        if recording {
            publish(cx, &reports.0, reports.1, now);
        }
        items
    }

    /// Whether nothing is entering or leaving.
    #[must_use]
    pub fn is_settled(&self, cx: &App) -> bool {
        self.inner.borrow().model.is_settled(now(cx))
    }

    /// Jumps every item to where it is heading (leavers are dropped).
    pub fn settle(&self) {
        self.inner.borrow_mut().model.settle();
    }

    /// Items held (leavers included).
    #[must_use]
    pub fn len(&self) -> usize {
        self.inner.borrow().model.records.len()
    }

    /// Whether nothing is held.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

#[derive(Default)]
struct Scopes(HashMap<SharedString, Presence>);

impl Global for Scopes {}

/// One probe report: the presence scalar plus every pose/room channel the
/// item's act moves.
struct Report {
    key: ElementId,
    value: f32,
    target: f32,
    velocity: f32,
    live: bool,
    started: Option<Instant>,
    budget: Duration,
    /// The act, the progress now, the progress it heads to, and ds/dt.
    travel: Option<(Act, f32, f32, f32)>,
}

impl Report {
    fn live(record: &Record, sample: Sampled, now: Instant) -> Self {
        let State::Moving {
            way,
            forward,
            from,
            start,
            hold,
        } = record.state
        else {
            return Self::settled(Finished::of(record, sample.presence));
        };
        let act = record.act(way);
        let distance = if forward { 1.0 - from } else { from };
        let (s, _) = record.progress(now);
        let span = act.duration.as_secs_f32();
        let held = now.saturating_duration_since(start) < hold;
        let rate = if held || span <= 0.0 {
            0.0
        } else if forward {
            1.0 / span
        } else {
            -1.0 / span
        };
        Self {
            key: record.key.clone(),
            value: sample.presence,
            target: sample.target,
            velocity: if held { 0.0 } else { sample.velocity },
            live: sample.live,
            started: Some(start),
            budget: hold + act.duration.mul_f32(distance),
            travel: Some((act.clone(), s, if forward { 1.0 } else { 0.0 }, rate)),
        }
    }

    /// The final, at-rest sample of an item that finished: every channel
    /// its act moved reports the act's end, not live.
    fn settled(finished: Finished) -> Self {
        Self {
            key: finished.key,
            value: finished.presence,
            target: finished.presence,
            velocity: 0.0,
            live: false,
            started: None,
            budget: Duration::ZERO,
            travel: finished.ended.map(|(act, end)| (act, end, end, 0.0)),
        }
    }
}

type Channel = (&'static str, fn(Pose, Extent) -> f32);

const CHANNELS: [Channel; 6] = [
    ("x", |pose, _| pose.x),
    ("y", |pose, _| pose.y),
    ("sx", |pose, _| pose.sx),
    ("sy", |pose, _| pose.sy),
    ("opacity", |pose, _| pose.opacity),
    ("room", |_, room| room.share),
];

/// Whether `read` varies anywhere along `act`. Between keyframes a track
/// only interpolates its keyframes, so checking every keyframe offset of
/// both tracks is exact.
fn varies(act: &Act, read: fn(Pose, Extent) -> f32) -> bool {
    let value = |t: f32| {
        let (pose, room) = act.at(t);
        read(pose, room)
    };
    let first = value(0.0);
    act.pose
        .frames
        .iter()
        .map(|(t, _)| *t)
        .chain(act.room.frames.iter().map(|(t, _)| *t))
        .any(|t| (value(t) - first).abs() > 1e-6)
}

fn publish(cx: &mut App, scope: &SharedString, reports: Vec<Report>, now: Instant) {
    let epoch = epoch(cx);
    let millis = |at: Instant| at.saturating_duration_since(epoch).as_secs_f64() * 1000.0;
    let group = probe::current_group();
    for report in reports {
        let base = format!("{scope}.{}", report.key);
        let started_ms = report.started.map_or(0.0, millis);
        let budget_ms = report.budget.as_secs_f64() * 1000.0;
        let at_ms = millis(now);
        probe::record_track(cx, || TrackSample {
            key: format!("{base}.presence"),
            kind: TrackKind::Keys,
            value: report.value,
            target: report.target,
            velocity: report.velocity,
            started_ms,
            budget_ms,
            at_ms,
            live: report.live,
            overshoot_ratio: 0.0,
            group: group.clone(),
        });
        let Some((act, s, end, rate)) = &report.travel else {
            continue;
        };
        let (pose, room) = act.at(*s);
        let (end_pose, end_room) = act.at(*end);
        let (next_pose, next_room) = act.at((s + rate * 1e-3).clamp(0.0, 1.0));
        let start = 1.0 - end;
        for (name, read) in CHANNELS {
            if !varies(act, read) {
                continue;
            }
            let overshoot = overshoot(act, read, start, *end);
            probe::record_track(cx, || TrackSample {
                key: format!("{base}.{name}"),
                kind: TrackKind::Keys,
                value: read(pose, room),
                target: read(end_pose, end_room),
                velocity: (read(next_pose, next_room) - read(pose, room)) * 1000.0,
                started_ms,
                budget_ms,
                at_ms,
                live: report.live,
                overshoot_ratio: overshoot,
                group: group.clone(),
            });
        }
    }
}

/// How far a channel travels past either end of its act (keyframed
/// rebounds, squash and stretch), as a fraction of its start-to-end span.
fn overshoot(act: &Act, read: fn(Pose, Extent) -> f32, start: f32, end: f32) -> f32 {
    let value = |t: f32| {
        let (pose, room) = act.at(t);
        read(pose, room)
    };
    let (from, to) = (value(start), value(end));
    let span = (to - from).abs();
    let (low, high) = (from.min(to), from.max(to));
    let excess = act
        .pose
        .frames
        .iter()
        .map(|(t, _)| *t)
        .chain(act.room.frames.iter().map(|(t, _)| *t))
        .map(|t| {
            let v = value(t);
            (low - v).max(v - high).max(0.0)
        })
        .fold(0.0, f32::max);
    if span <= 1e-6 { 0.0 } else { excess / span }
}

enum SlotLayout {
    /// At rest: the child's own layout node, untouched.
    Through,
    /// Moving: an outer box of the reserved size around an inner box that
    /// keeps the child's natural size (measured for the next frame).
    Wrapped { inner: LayoutId },
}

/// An item's slot: reserves its room around the item's layer. Build with
/// [`Item::slot`].
pub struct Slot {
    child: AnyElement,
    key: ElementId,
    inner: Rc<RefCell<Inner>>,
    room: Extent,
    axis: Axis,
    layout: SlotLayout,
}

impl IntoElement for Slot {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

fn length(value: Pixels) -> Length {
    Length::Definite(DefiniteLength::Absolute(AbsoluteLength::Pixels(value)))
}

impl gpui::Element for Slot {
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
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, Self::RequestLayoutState) {
        let child = self.child.request_layout(window, cx);
        if self.room.is_full() {
            self.layout = SlotLayout::Through;
            return (child, ());
        }
        let natural = self.inner.borrow().model.natural(&self.key);
        let reserved = match natural {
            Some(natural) => self.room.of(natural),
            // Never measured: only an empty slot can be exact now; this
            // frame measures the child for the next.
            None if self.room.share <= 0.0 && self.room.px <= 0.0 => Pixels::ZERO,
            None => {
                self.layout = SlotLayout::Through;
                return (child, ());
            }
        };
        let inner_style = Style {
            display: Display::Flex,
            flex_direction: FlexDirection::Column,
            flex_shrink: 0.0,
            ..Style::default()
        };
        let inner = window.request_layout(inner_style, [child], cx);
        let mut outer = Style {
            display: Display::Flex,
            flex_shrink: 0.0,
            ..Style::default()
        };
        match self.axis {
            Axis::Vertical => {
                outer.flex_direction = FlexDirection::Column;
                outer.align_items = Some(AlignItems::Stretch);
                outer.size.height = length(reserved);
                outer.min_size.height = length(Pixels::ZERO);
            }
            Axis::Horizontal => {
                outer.flex_direction = FlexDirection::Row;
                outer.align_items = Some(AlignItems::FlexStart);
                outer.size.width = length(reserved);
                outer.min_size.width = length(Pixels::ZERO);
            }
        }
        self.layout = SlotLayout::Wrapped { inner };
        (window.request_layout(outer, [inner], cx), ())
    }

    fn prepaint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        window: &mut Window,
        cx: &mut App,
    ) -> Self::PrepaintState {
        let natural = match self.layout {
            SlotLayout::Through => bounds.size,
            SlotLayout::Wrapped { inner } => window.layout_bounds(inner).size,
        };
        let main = match self.axis {
            Axis::Vertical => natural.height,
            Axis::Horizontal => natural.width,
        };
        self.inner.borrow_mut().model.measure(&self.key, main);
        self.child.prepaint(window, cx);
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
        self.child.paint(window, cx);
    }
}

#[cfg(test)]
#[allow(clippy::float_cmp, clippy::cast_precision_loss)]
mod tests {
    use super::{Entry, Extent, Model, Phase, act};
    use crate::motion::keys::Pose;
    use gpui::ElementId;
    use std::collections::HashSet;
    use std::time::{Duration, Instant};

    fn keys(names: &[u64]) -> Vec<Entry> {
        names.iter().map(|&n| Entry::new(ElementId::Integer(n))).collect()
    }

    fn id(n: u64) -> ElementId {
        ElementId::Integer(n)
    }

    fn ms(n: u64) -> Duration {
        Duration::from_millis(n)
    }

    /// (key, phase, presence, pose, room) for every held item at `now`.
    fn look(model: &Model, now: Instant) -> Vec<(ElementId, Phase, f32, Pose, Extent)> {
        model
            .samples(now)
            .map(|(record, sample)| {
                (
                    record.key.clone(),
                    record.phase(),
                    sample.presence,
                    sample.pose,
                    sample.room,
                )
            })
            .collect()
    }

    fn order(model: &Model) -> Vec<ElementId> {
        model.keys().cloned().collect()
    }

    #[test]
    fn a_removed_key_holds_its_slot_until_its_exit_settles() {
        let t0 = Instant::now();
        let mut model = Model::new();
        model.sync(keys(&[1, 2, 3]), t0, false);
        assert!(model.is_settled(t0), "the first sync is quiet by default");
        model.sync(keys(&[1, 3]), t0, false);
        assert_eq!(order(&model), [id(1), id(2), id(3)]);
        let exit = act::LEAVE.duration;
        let before = t0 + exit - ms(1);
        model.sync(keys(&[1, 3]), before, false);
        let seen = look(&model, before);
        assert_eq!(seen[1].0, id(2));
        assert_eq!(seen[1].1, Phase::Leaving);
        assert!(seen[1].2 > 0.0 && seen[1].2 < 0.01, "{}", seen[1].2);
        assert!(!model.is_settled(before));
        model.sync(keys(&[1, 3]), t0 + exit, false);
        assert_eq!(order(&model), [id(1), id(3)]);
        assert!(model.is_settled(t0 + exit));
    }

    #[test]
    fn a_key_returning_mid_exit_reverses_from_where_it_is() {
        let t0 = Instant::now();
        let mut model = Model::new();
        model.sync(keys(&[1, 2]), t0, false);
        model.sync(keys(&[1]), t0, false);
        let mid = t0 + ms(100);
        let leaving = look(&model, mid)[1].clone();
        assert_eq!(leaving.1, Phase::Leaving);
        model.sync(keys(&[1, 2]), mid, false);
        let back = look(&model, mid)[1].clone();
        assert_eq!(back.1, Phase::Entering);
        // Same instant, same place: nothing jumped and nothing restarted
        // (a restart would put it at the drop-in's first pose, 140 px up).
        assert_eq!(back.2, leaving.2);
        assert_eq!(back.3, leaving.3);
        assert_eq!(back.4, leaving.4);
        // It retraces its way out: 100 ms back to rest, not a full entrance.
        let almost = mid + ms(99);
        assert_eq!(look(&model, almost)[1].1, Phase::Entering);
        model.sync(keys(&[1, 2]), mid + ms(100), false);
        let rest = look(&model, mid + ms(100))[1].clone();
        assert_eq!(rest.1, Phase::Present);
        assert!(rest.3.is_rest());
        assert_eq!(rest.4, Extent::FULL);
    }

    #[test]
    fn an_arrival_removed_mid_entrance_plays_its_way_in_backwards() {
        let t0 = Instant::now();
        let mut model = Model::new();
        model.sync(keys(&[1]), t0, false);
        model.sync(keys(&[1, 2]), t0, false);
        let mid = t0 + ms(200);
        let coming = look(&model, mid)[1].clone();
        model.sync(keys(&[1]), mid, false);
        let going = look(&model, mid)[1].clone();
        assert_eq!(going.1, Phase::Leaving);
        assert_eq!((going.2, going.3, going.4), (coming.2, coming.3, coming.4));
        // Retraces the drop-in: 100 ms later it is where it was at 100 ms in.
        let pose_at_100 = act::DROP_IN.pose.at(100.0 / 620.0);
        let back = look(&model, mid + ms(100))[1].3;
        assert!((back.y - pose_at_100.y).abs() < 1e-3, "{back:?} vs {pose_at_100:?}");
        model.sync(keys(&[1]), mid + ms(200), false);
        assert_eq!(order(&model), [id(1)]);
    }

    #[test]
    fn stagger_holds_later_arrivals_at_their_start() {
        let t0 = Instant::now();
        let mut model = Model::new();
        model.stagger = ms(50);
        model.sync(keys(&[]), t0, false);
        model.sync(keys(&[1, 2, 3]), t0, false);
        let seen = look(&model, t0 + ms(40));
        assert!(seen[0].2 > 0.0);
        assert_eq!(seen[1].2, 0.0, "held for 50 ms");
        assert_eq!(seen[2].2, 0.0, "held for 100 ms");
        assert_eq!(seen[2].4, Extent::NONE, "a held arrival takes no room yet");
        let settled = t0 + ms(100) + act::DROP_IN.duration;
        assert!(!model.is_settled(settled - ms(1)));
        assert!(model.is_settled(settled));
    }

    #[test]
    fn reduced_motion_is_instant_both_ways() {
        let t0 = Instant::now();
        let mut model = Model::new();
        model.sync(keys(&[1, 2]), t0, true);
        model.sync(keys(&[2, 3]), t0, true);
        assert_eq!(order(&model), [id(2), id(3)]);
        assert!(model.is_settled(t0));
        assert!(look(&model, t0).iter().all(|item| item.1 == Phase::Present));
    }

    #[test]
    fn appear_animates_the_first_sync() {
        let t0 = Instant::now();
        let mut model = Model::new();
        model.appear = true;
        model.sync(keys(&[1]), t0, false);
        assert_eq!(look(&model, t0)[0].1, Phase::Entering);
        assert_eq!(look(&model, t0)[0].3, act::DROP_IN.pose.at(0.0));
    }

    /// xorshift64*: a tiny seeded generator, so a failing storm replays.
    struct Rng(u64);

    impl Rng {
        fn next(&mut self) -> u64 {
            self.0 ^= self.0 >> 12;
            self.0 ^= self.0 << 25;
            self.0 ^= self.0 >> 27;
            self.0.wrapping_mul(0x2545_f491_4f6c_dd1d)
        }

        fn below(&mut self, n: u64) -> u64 {
            self.next() % n.max(1)
        }
    }

    /// The fastest any presence scalar may move: 1 per shortest act.
    fn max_rate() -> f32 {
        [act::DROP_IN, act::LEAVE, act::RISE, act::FADE_OUT]
            .iter()
            .map(|act| 1.0 / act.duration.as_secs_f32())
            .fold(0.0, f32::max)
    }

    fn storm(seed: u64) {
        let mut rng = Rng(seed);
        let start = Instant::now();
        let mut now = start;
        let mut model = Model::new();
        model.stagger = ms(rng.below(3) * 20);
        model.appear = rng.below(2) == 0;
        if rng.below(2) == 0 {
            model.exit = act::FADE_OUT;
            model.enter = act::RISE;
        }
        let mut wanted: Vec<u64> = (0..6).collect();
        model.sync(keys(&wanted), now, false);
        let mut before = look(&model, now);
        let mut before_at = now;
        for step in 0..3_000 {
            // Several syncs per frame are common: time advances only
            // sometimes, and by frame-ish amounts.
            if rng.below(3) == 0 {
                now += ms(rng.below(45));
            }
            let reorder = match rng.below(10) {
                0..=2 => {
                    // Remove one.
                    if !wanted.is_empty() {
                        let ix = rng.below(wanted.len() as u64) as usize;
                        wanted.remove(ix);
                    }
                    false
                }
                3..=5 => {
                    // Insert one (often a key that is leaving right now).
                    let key = rng.below(12);
                    if wanted.contains(&key) {
                        false
                    } else {
                        let ix = rng.below(wanted.len() as u64 + 1) as usize;
                        wanted.insert(ix, key);
                        // A returning leaver goes to its wanted place: a move.
                        before.iter().any(|item| item.0 == id(key))
                    }
                }
                6 => {
                    // Swap two.
                    if wanted.len() > 1 {
                        let a = rng.below(wanted.len() as u64) as usize;
                        let b = rng.below(wanted.len() as u64) as usize;
                        wanted.swap(a, b);
                        a != b
                    } else {
                        false
                    }
                }
                7 => {
                    // Duplicates in the input: the first occurrence wins.
                    let mut noisy = wanted.clone();
                    if let Some(&first) = wanted.first() {
                        noisy.push(first);
                    }
                    model.sync(keys(&noisy), now, false);
                    false
                }
                _ => false,
            };
            model.sync(keys(&wanted), now, false);
            let after = look(&model, now);

            // 1. The wanted keys, in the wanted order, are exactly the
            //    items that are not leaving.
            let alive: Vec<ElementId> = after
                .iter()
                .filter(|item| item.1 != Phase::Leaving)
                .map(|item| item.0.clone())
                .collect();
            let want: Vec<ElementId> = wanted.iter().map(|&n| id(n)).collect();
            assert_eq!(alive, want, "seed {seed} step {step}: alive order");
            // 2. No duplicates.
            let unique: HashSet<&ElementId> = after.iter().map(|item| &item.0).collect();
            assert_eq!(unique.len(), after.len(), "seed {seed} step {step}: duplicate");
            // 3. Leavers come only from what was drawn before.
            let drawn: HashSet<&ElementId> = before.iter().map(|item| &item.0).collect();
            for item in after.iter().filter(|item| item.1 == Phase::Leaving) {
                assert!(drawn.contains(&item.0), "seed {seed} step {step}: {:?} left from nowhere", item.0);
            }
            // 4. Continuity: presence moves no faster than the fastest act,
            //    and not at all within one instant (a reversal never jumps).
            let dt = now.saturating_duration_since(before_at).as_secs_f32();
            for item in &after {
                if let Some(old) = before.iter().find(|old| old.0 == item.0) {
                    let step_limit = dt * max_rate() + 1e-4;
                    assert!(
                        (item.2 - old.2).abs() <= step_limit,
                        "seed {seed} step {step}: {:?} jumped {} -> {} in {dt}s",
                        item.0,
                        old.2,
                        item.2
                    );
                    if dt == 0.0 {
                        assert_eq!(item.3, old.3, "seed {seed} step {step}: pose jumped");
                        assert_eq!(item.4, old.4, "seed {seed} step {step}: room jumped");
                    }
                }
            }
            // 5. Without a reorder, every item kept on both sides keeps its
            //    relative place (survivors keep order; leavers hold slots).
            if !reorder {
                let kept_after: Vec<&ElementId> =
                    after.iter().map(|i| &i.0).filter(|k| drawn.contains(k)).collect();
                let now_keys: HashSet<&ElementId> = after.iter().map(|i| &i.0).collect();
                let kept_before: Vec<&ElementId> =
                    before.iter().map(|i| &i.0).filter(|k| now_keys.contains(k)).collect();
                assert_eq!(kept_after, kept_before, "seed {seed} step {step}: slots moved");
            }
            before = after;
            before_at = now;
        }

        // A neutral tail: settle == fresh.
        model.sync(keys(&wanted), now, false);
        now += ms(2_000);
        model.sync(keys(&wanted), now, false);
        assert!(model.is_settled(now), "seed {seed}: still moving after the tail");
        let mut fresh = Model::new();
        fresh.sync(keys(&wanted), now, false);
        assert_eq!(look(&model, now), look(&fresh, now), "seed {seed}: settle != fresh");
        assert_eq!(model.records.len(), wanted.len(), "seed {seed}: leaked leavers");
        assert!(model.naturals.len() <= wanted.len(), "seed {seed}: leaked sizes");
    }

    #[test]
    fn storms_keep_order_continuity_and_settle_to_fresh() {
        for seed in 1..=40_u64 {
            storm(seed.wrapping_mul(0x9e37_79b9_7f4a_7c15) | 1);
        }
    }

    /// The list, drawn through GPUI's test platform: real layout, the real
    /// frame gate, bounds read back from the probe ledger.
    mod drawn {
        use crate::motion::presence::{Presence, act};
        use crate::motion::{frames_requested, reset_epoch};
        use crate::probe;
        use gpui::{
            Context, ElementId, IntoElement, ParentElement, Render, Styled, TestAppContext,
            VisualTestContext, Window, div, px,
        };
        use std::time::Duration;

        struct List {
            presence: Presence,
            keys: Vec<u64>,
        }

        impl Render for List {
            fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
                let items = self
                    .presence
                    .sync(self.keys.iter().map(|&k| ElementId::Integer(k)), window, cx);
                div().flex().flex_col().children(items.into_iter().map(|item| {
                    probe::measure(
                        ElementId::Name(format!("slot-{}", item.key).into()),
                        item.slot(div().w(px(200.0)).h(px(30.0))),
                    )
                }))
            }
        }

        /// One frame: run the gate's callbacks, draw, return the ledger.
        fn frame(cx: &mut VisualTestContext) -> (usize, probe::Ledger) {
            cx.update(|window, cx| {
                let callbacks = window.simulate_next_frame(cx);
                window.refresh();
                window.draw(cx).clear(cx);
                (callbacks, probe::take(cx))
            })
        }

        fn advance(cx: &mut VisualTestContext, millis: u64) {
            cx.executor().advance_clock(Duration::from_millis(millis));
            cx.run_until_parked();
        }

        fn slot(ledger: &probe::Ledger, key: u64) -> Option<(f32, f32)> {
            ledger
                .bounds(&format!("slot-{key}"))
                .map(|bounds| (bounds.y, bounds.height))
        }

        #[gpui::test]
        fn a_leaving_row_collapses_its_slot_then_the_list_closes_and_rests(
            cx: &mut TestAppContext,
        ) {
            let (view, cx) = cx.add_window_view(|_, _| List {
                presence: Presence::new("list").exit(act::LEAVE),
                keys: vec![1, 2, 3],
            });
            cx.update(|_, cx| {
                probe::enable(cx);
                reset_epoch(cx);
            });
            let (_, ledger) = frame(cx);
            let top = slot(&ledger, 1).expect("row 1").0;
            assert_eq!(slot(&ledger, 2).map(|s| s.1), Some(30.0));
            assert_eq!(slot(&ledger, 3).map(|s| s.0 - top), Some(60.0));

            view.update(cx, |list, cx| {
                list.keys = vec![1, 3];
                cx.notify();
            });
            let mut previous = f32::MAX;
            let mut heights = Vec::new();
            for _ in 0..10 {
                let (_, ledger) = frame(cx);
                let Some((_, height)) = slot(&ledger, 2) else {
                    break;
                };
                let below = slot(&ledger, 3).expect("row 3").0 - top;
                // The row below sits exactly under the leaving slot.
                assert!((below - 30.0 - height).abs() < 0.01, "{below} vs {height}");
                assert!(height <= previous + 0.01, "the slot only closes: {heights:?}");
                previous = height;
                heights.push(height);
                advance(cx, 40);
            }
            assert!(heights.len() >= 8, "held its slot until the exit settled: {heights:?}");
            assert!(heights.first().is_some_and(|h| (*h - 30.0).abs() < 0.01), "{heights:?}");
            assert!(heights.iter().any(|h| *h > 0.5 && *h < 29.5), "it animated: {heights:?}");
            advance(cx, 400);
            let (_, ledger) = frame(cx);
            assert_eq!(slot(&ledger, 2), None, "the leaver is dropped");
            assert_eq!(slot(&ledger, 3).map(|s| s.0 - top), Some(30.0));
            // Settled: the gate schedules nothing more.
            let requested = cx.update(|_, cx| frames_requested(cx));
            for _ in 0..3 {
                advance(cx, 40);
                let (callbacks, _) = frame(cx);
                assert_eq!(callbacks, 0, "a settled list requests no frames");
            }
            assert_eq!(cx.update(|_, cx| frames_requested(cx)), requested);
            assert!(view.read_with(cx, |list, cx| list.presence.is_settled(cx)));
        }

        #[gpui::test]
        fn an_arrival_opens_from_nothing_past_its_height_and_rests(cx: &mut TestAppContext) {
            let (view, cx) = cx.add_window_view(|_, _| List {
                presence: Presence::new("list"),
                keys: vec![1],
            });
            cx.update(|_, cx| {
                probe::enable(cx);
                reset_epoch(cx);
            });
            frame(cx);
            view.update(cx, |list, cx| {
                list.keys = vec![1, 2];
                cx.notify();
            });
            let mut heights = Vec::new();
            for _ in 0..20 {
                let (_, ledger) = frame(cx);
                heights.push(slot(&ledger, 2).expect("row 2").1);
                advance(cx, 40);
            }
            assert!(heights[0] < 0.01, "starts closed: {heights:?}");
            let peak = heights.iter().copied().fold(0.0, f32::max);
            assert!(peak > 32.0 && peak < 35.5, "the board's 5 px make-room overshoot: {heights:?}");
            assert!((heights[19] - 30.0).abs() < 0.01, "rests at its height: {heights:?}");
        }
    }
}
