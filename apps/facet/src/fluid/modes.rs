//! What a region remembers between frames so its modes hold still: the mode
//! it is in per decision, how many times it changed, and the transition that
//! change plays through `facet::motion`. And the grid, the one mode that is a
//! count.

use super::ladder::{Ladder, ModeId, settle_index};
use super::ramp::Length;
use super::room::Room;
use crate::motion::{Motion, spec};
use gpui::{App, ElementId, Pixels, Window, px};
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

/// How many times a decision has changed its mind. Feed it to a
/// [`crate::motion::Flow`] epoch so the items of a layout that changed mode spring to
/// their new places (FLIP) instead of jumping.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq)]
pub struct Epoch(u32);

impl Epoch {
    /// The number of changes so far.
    #[must_use]
    pub const fn count(self) -> u32 {
        self.0
    }
}

#[derive(Clone, Copy, Debug)]
struct Memory {
    index: usize,
    from: Option<usize>,
    epoch: Epoch,
}

/// A region's memory of its modes. Cloning shares it, so a region's parts and
/// its pages read one; keep one per region (the shell, the reader, a graph)
/// and read every ladder and grid through it.
#[derive(Clone, Debug, Default)]
pub struct Modes {
    held: Rc<RefCell<HashMap<ModeId, Memory>>>,
}

/// The mode a ladder is in, and what changed to get there.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Settled<M> {
    id: ModeId,
    /// The mode now.
    pub mode: M,
    /// The mode it was in before its latest change (`None` before any).
    pub from: Option<M>,
    /// How many times it has changed.
    pub epoch: Epoch,
}

/// A grid's columns at a room.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Columns {
    /// How many columns sit side by side.
    pub count: usize,
    /// The room one column has.
    pub column: Room,
    /// The gap between columns.
    pub gap: Pixels,
    /// How many times the count has changed.
    pub epoch: Epoch,
}

impl Modes {
    /// No memory yet: the first read of each decision is its plain edge.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// The mode `ladder` puts a region of `room` in, held through its
    /// hysteresis band and remembered for the next frame.
    #[must_use]
    pub fn settle<M: Copy + PartialEq + 'static>(&self, ladder: &Ladder<M>, room: Room) -> Settled<M> {
        let mut held = self.held.borrow_mut();
        let before = held.get(&ladder.id()).copied();
        let index = ladder.settle_index(before.map(|memory| memory.index), room);
        let memory = advance(&mut held, ladder.id(), before, index);
        let rungs = ladder.rungs();
        Settled {
            id: ladder.id(),
            mode: rungs[index].mode(),
            from: memory.from.and_then(|from| rungs.get(from)).map(super::Rung::mode),
            epoch: memory.epoch,
        }
    }

    /// The columns `grid` gives a region of `room` with `gap` between them,
    /// held through its hysteresis band and remembered for the next frame.
    #[must_use]
    pub fn columns(&self, grid: &Grid, room: Room, gap: Pixels) -> Columns {
        let mut held = self.held.borrow_mut();
        let before = held.get(&grid.id()).copied();
        let count = grid.count(before.map(|memory| memory.index + 1), room, gap);
        let memory = advance(&mut held, grid.id(), before, count - 1);
        let column = grid.lay(count, room, gap);
        Columns {
            count,
            column,
            gap,
            epoch: memory.epoch,
        }
    }

    /// The memory a view keeps in `window` under `id`: for a view that is a
    /// function of its measure and has no entity of its own to hold one.
    pub fn keyed(id: impl Into<ElementId>, window: &mut Window, cx: &mut App) -> Self {
        window.use_keyed_state(id, cx, |_, _| Self::new()).read(cx).clone()
    }

    /// Forgets every decision: the next read of each is its plain edge.
    pub fn forget(&self) {
        self.held.borrow_mut().clear();
    }
}

fn advance(held: &mut HashMap<ModeId, Memory>, id: ModeId, before: Option<Memory>, index: usize) -> Memory {
    let next = match before {
        None => Memory {
            index,
            from: None,
            epoch: Epoch(0),
        },
        Some(memory) if memory.index == index => memory,
        Some(memory) => Memory {
            index,
            from: Some(memory.index),
            epoch: Epoch(memory.epoch.0.saturating_add(1)),
        },
    };
    held.insert(id, next);
    next
}

impl<M: Copy> Settled<M> {
    /// How far the latest change has come, 0.0 the frame it happened to 1.0
    /// once it has settled, played through `motion` (a spring, so a change
    /// retargeted mid-way carries its speed). A decision that never changed
    /// is settled; under reduced motion a change is settled at once.
    ///
    /// Draw the mode it came from at `1 - progress` and the mode it is in
    /// at `progress`.
    pub fn progress(&self, motion: &Motion, window: &mut Window, cx: &mut App) -> f32 {
        if self.epoch.count() == 0 {
            return 1.0;
        }
        let key = ElementId::NamedInteger(self.id.name().into(), u64::from(self.epoch.count()));
        motion.animate_from(key, 0.0, 1.0, spec::SETTLE, window, cx).clamp(0.0, 1.0)
    }
}

/// A grid: as many columns as fit, each at least `min` wide, at most `most`.
/// The count is the one thing about a grid that is discrete, so it is a
/// mode: it changes half a band past the edge and the layout epoch carries
/// the items across. Between changes the columns' width is continuous.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Grid {
    id: ModeId,
    min: Length,
    most: usize,
}

impl Grid {
    /// A grid of at most `most` columns (at least one), each at least `min`
    /// wide. Only design tokens are built (`pub(crate)`).
    ///
    /// # Panics
    /// At compile time (in a `const`), if `most` is 0.
    #[must_use]
    pub(crate) const fn new(id: ModeId, min: Length, most: usize) -> Self {
        assert!(most >= 1, "a grid has a column");
        Self { id, min, most }
    }

    /// The decision this grid makes.
    #[must_use]
    pub const fn id(&self) -> ModeId {
        self.id
    }

    /// How many columns of at least `min` fit `room` with `gap` between
    /// them, for a region that had `held` (no history: the plain edges).
    #[must_use]
    pub fn count(&self, held: Option<usize>, room: Room, gap: Pixels) -> usize {
        let min = f32::from(self.min.at(room));
        let gap = f32::from(gap);
        let need = |count: usize| min * to_f32(count) + gap * to_f32(count - 1);
        let held = held.map(|count| count.clamp(1, self.most) - 1);
        let band = super::HYSTERESIS.get() * room.scale();
        settle_index(held, self.most, f32::from(room.width()), band, |index| need(index + 1)) + 1
    }

    /// The room one of `count` columns has at `room`, `gap` apart.
    #[must_use]
    pub fn lay(&self, count: usize, room: Room, gap: Pixels) -> Room {
        let count = count.max(1);
        let column = (room.width() - gap * to_f32(count - 1)) / to_f32(count);
        room.within(column.max(px(0.0)))
    }
}

/// A column count as a float (counts are tiny).
#[allow(clippy::cast_precision_loss)]
const fn to_f32(count: usize) -> f32 {
    count as f32
}
