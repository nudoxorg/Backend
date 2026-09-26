//! Doors: every sub-part of every mark opens.
//!
//! A mark never owns a popup. When the pointer rests on one of its parts (a
//! tick, a stone, an arm, a spoke, a region, a cell) the mark reports that
//! part — its exact sub-rect in window coordinates and a builder for what it
//! opens — and when the pointer leaves, it reports that too. The float layer
//! (`overlay::float`, lane W-Float) decides hover intent, placement, chaining
//! and exits. Keyboard focus walks the same parts; Space opens the walked
//! part at once, Enter activates it (descends to its page).
//!
//! Every report also lands in a small per-window ledger ([`rested`]) that
//! storms and tests read: "one lens per mark" is checkable, not narrated.

use crate::measure::Measure;
use crate::overlay::float::{self, FloatKind, FloatRequest};
use gpui::{AnyElement, App, Bounds, ElementId, Global, Pixels, SharedString, Window};
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;

/// What resting on a part opens.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Hash)]
pub enum Opens {
    /// A one-line tooltip (a stone's name, an arm's count).
    #[default]
    Tip,
    /// A breakdown card: an aggregate one rung down (`overlay::lens`).
    Lens,
    /// A symbol card (`overlay::peek`).
    Peek,
}

pub use crate::overlay::float::Side;

/// Builds what part `index` opens, for a card `Measure` wide.
pub type Build = Rc<dyn Fn(usize, &Measure, &mut Window, &mut App) -> AnyElement>;
/// Descends into part `index` (Enter or click).
pub type Activate = Rc<dyn Fn(usize, &mut Window, &mut App)>;

/// What a mark's parts open, and what activating one does.
#[derive(Clone)]
pub struct Door {
    opens: Opens,
    build: Build,
    activate: Option<Activate>,
    side: Option<Side>,
}

impl Door {
    fn new(
        opens: Opens,
        build: impl Fn(usize, &Measure, &mut Window, &mut App) -> AnyElement + 'static,
    ) -> Self {
        Self {
            opens,
            build: Rc::new(build),
            activate: None,
            side: None,
        }
    }

    /// Parts open a lens card.
    #[must_use]
    pub fn lens(
        build: impl Fn(usize, &Measure, &mut Window, &mut App) -> AnyElement + 'static,
    ) -> Self {
        Self::new(Opens::Lens, build)
    }

    /// Parts open a tooltip.
    #[must_use]
    pub fn tip(build: impl Fn(usize, &Measure, &mut Window, &mut App) -> AnyElement + 'static) -> Self {
        Self::new(Opens::Tip, build)
    }

    /// Parts open a peek card.
    #[must_use]
    pub fn peek(
        build: impl Fn(usize, &Measure, &mut Window, &mut App) -> AnyElement + 'static,
    ) -> Self {
        Self::new(Opens::Peek, build)
    }

    /// Overrides the side the mark would pick.
    #[must_use]
    pub const fn side(mut self, side: Side) -> Self {
        self.side = Some(side);
        self
    }

    /// What Enter or a click on part `index` does.
    #[must_use]
    pub fn on_activate(mut self, activate: impl Fn(usize, &mut Window, &mut App) + 'static) -> Self {
        self.activate = Some(Rc::new(activate));
        self
    }

    /// What its parts open.
    #[must_use]
    pub const fn opens(&self) -> Opens {
        self.opens
    }

    /// Builds part `index`'s content (the scenes place lenses with this).
    pub fn build(&self, index: usize, measure: &Measure, window: &mut Window, cx: &mut App) -> AnyElement {
        (self.build)(index, measure, window, cx)
    }

    pub(crate) fn preferred(&self, fallback: Side) -> Side {
        self.side.unwrap_or(fallback)
    }
}

/// The float key of one part: `<mark>/part-<index>`.
#[must_use]
pub fn part_key(mark: &ElementId, index: usize) -> ElementId {
    ElementId::NamedChild(Arc::new(mark.clone()), SharedString::from(format!("part-{index}")))
}

/// One part currently reported as rested on.
#[derive(Clone, Debug, PartialEq)]
pub struct Rested {
    /// The part's index within its mark.
    pub part: usize,
    /// Its sub-rect, window coordinates.
    pub anchor: Bounds<Pixels>,
    /// What it opens.
    pub opens: Opens,
    /// The side it asked for.
    pub side: Side,
    /// Opened by the keyboard (no hover intent).
    pub keyboard: bool,
}

#[derive(Default)]
struct Ledger {
    rested: HashMap<ElementId, Rested>,
    rests: u64,
    leaves: u64,
}

impl Global for Ledger {}

/// Every mark with a rested part, and that part. A mark appears at most once
/// by construction (a new rest on a mark replaces its old one), so "one lens
/// per mark" is a property of this map plus the float layer's own stack.
#[must_use]
pub fn rested(cx: &App) -> Vec<(ElementId, Rested)> {
    let mut all: Vec<(ElementId, Rested)> = cx
        .try_global::<Ledger>()
        .map(|ledger| ledger.rested.iter().map(|(k, v)| (k.clone(), v.clone())).collect())
        .unwrap_or_default();
    all.sort_by_key(|(key, _)| key.to_string());
    all
}

/// `(rests, leaves)` reported since the app started.
#[must_use]
pub fn counts(cx: &App) -> (u64, u64) {
    cx.try_global::<Ledger>()
        .map_or((0, 0), |ledger| (ledger.rests, ledger.leaves))
}

impl Opens {
    const fn kind(self) -> FloatKind {
        match self {
            Self::Tip => FloatKind::Tip,
            Self::Lens => FloatKind::Lens,
            Self::Peek => FloatKind::Peek,
        }
    }
}

fn request(mark: &ElementId, part: usize, anchor: Bounds<Pixels>, side: Side, door: &Door) -> FloatRequest {
    let build = door.build.clone();
    FloatRequest::new(part_key(mark, part), anchor, door.opens.kind(), move |measure, window, cx| {
        build(part, measure, window, cx)
    })
    .side(side)
}

/// The pointer's report for `part` of `mark`, every pointer move while it is
/// on the mark (capture phase, before the layer's own listener): `hovered`
/// rests on the part (hover intent applies) or leaves it; a repeat keeps the
/// layer's record of the trigger alive.
#[allow(clippy::too_many_arguments)]
pub(crate) fn report(
    mark: &ElementId,
    part: usize,
    anchor: Bounds<Pixels>,
    side: Side,
    door: &Door,
    hovered: bool,
    window: &mut Window,
    cx: &mut App,
) {
    let ledger = cx.default_global::<Ledger>();
    let known = ledger.rested.get(mark).map(|r| (r.part, r.keyboard));
    if hovered {
        if known.map(|(p, _)| p) != Some(part) {
            ledger.rests += 1;
        }
        ledger.rested.insert(
            mark.clone(),
            Rested {
                part,
                anchor,
                opens: door.opens,
                side,
                keyboard: false,
            },
        );
    } else if known.is_some_and(|(p, keyboard)| p == part && !keyboard) {
        ledger.rested.remove(mark);
        ledger.leaves += 1;
    }
    float::report(
        &part_key(mark, part),
        anchor,
        hovered,
        || request(mark, part, anchor, side, door),
        window,
        cx,
    );
}

/// Opens `part` at once from the keyboard (Space): no hover intent, focus
/// moves into the card. A second Space on the open part pins it.
pub(crate) fn open(
    mark: &ElementId,
    part: usize,
    anchor: Bounds<Pixels>,
    side: Side,
    door: &Door,
    window: &mut Window,
    cx: &mut App,
) {
    let key = part_key(mark, part);
    if float::is_open(&key, window, cx) {
        float::pin_top(window, cx);
        return;
    }
    let ledger = cx.default_global::<Ledger>();
    if let Some(old) = ledger.rested.get(mark).map(|r| r.part)
        && old != part
    {
        ledger.leaves += 1;
        float::close(&part_key(mark, old), window, cx);
    }
    let ledger = cx.default_global::<Ledger>();
    ledger.rests += 1;
    ledger.rested.insert(
        mark.clone(),
        Rested {
            part,
            anchor,
            opens: door.opens,
            side,
            keyboard: true,
        },
    );
    let walker = window.focused(cx);
    float::open(request(mark, part, anchor, side, door), window, cx);
    // A lens or tip opened from a walked mark leaves focus on the mark, so
    // ←/→ keep walking and the card follows; a peek takes focus (it chains).
    if door.opens != Opens::Peek
        && let Some(handle) = walker
    {
        window.focus(&handle, cx);
    }
}

/// Closes whatever `mark` has open (Esc on the mark).
pub(crate) fn close(mark: &ElementId, window: &mut Window, cx: &mut App) -> bool {
    let ledger = cx.default_global::<Ledger>();
    let Some(old) = ledger.rested.remove(mark) else {
        return false;
    };
    ledger.leaves += 1;
    float::close(&part_key(mark, old.part), window, cx)
}

/// Every frame: the rested part's rect, so its card follows the mark.
pub(crate) fn anchor(mark: &ElementId, part: usize, rect: Bounds<Pixels>, window: &Window, cx: &mut App) {
    if let Some(rested) = cx.default_global::<Ledger>().rested.get_mut(mark)
        && rested.part == part
    {
        rested.anchor = rect;
    }
    float::anchor(&part_key(mark, part), rect, window, cx);
}

/// The part `mark` has open or rested, if any.
pub(crate) fn rested_part(mark: &ElementId, cx: &App) -> Option<(usize, bool)> {
    cx.try_global::<Ledger>()?
        .rested
        .get(mark)
        .map(|r| (r.part, r.keyboard))
}

/// Activates `part` (Enter / click).
pub(crate) fn activate(door: &Door, part: usize, window: &mut Window, cx: &mut App) {
    if let Some(activate) = door.activate.clone() {
        activate(part, window, cx);
    }
}
