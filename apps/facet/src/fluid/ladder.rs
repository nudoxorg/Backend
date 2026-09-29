//! Modes: the few layout changes that genuinely change what is drawn (a shelf
//! beside the page or a drawer over it, a rail beside or below), decided from
//! the room, with hysteresis so a window resting on a threshold cannot flip
//! them back and forth.

use super::room::{Design, Room};

/// Every discrete layout decision in the app, by name. The set is closed: a
/// region remembers the mode it is in per id, so two decisions can never
/// share a memory by spelling their key the same way.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum ModeId {
    /// The shell's shelf: beside the page, a spine, or a drawer.
    Dock,
    /// The shell's pinned peeks: floating, or a third column.
    Pins,
    /// The titlebar's controls, from every one down to the bare few.
    Bar,
    /// The gallery titlebar's thread of beads.
    Beads,
    /// A page's margin notes: beside their block, or under it.
    Notes,
    /// The Library's project grid.
    Library,
    /// Find's results and its detail card.
    Find,
    /// Compare's row detail, beside its rows or under them.
    Compare,
    /// Compare's release columns, side by side or stacked.
    CompareColumns,
    /// The graph's rail: beside the canvas, or under it.
    Rail,
    /// The graph's focus card: beside the map, or a sheet under it.
    Card,
    /// The graph's relation columns: two around the focus, or merged.
    Reading,
    /// Ask (⌘K): a floating panel, or a sheet across the window.
    Ask,
    /// The symbol page's rail: beside the page, or under it.
    SymbolRail,
    /// The symbol page's case and field rows: stacked or in columns.
    SymbolRows,
    /// The symbol page's cells: one column or two.
    SymbolCells,
    /// The symbol page on a phone.
    SymbolPhone,
    /// The symbol page's relations prism: one column on a rail, or columns.
    SymbolPrism,
    /// The package page's crest: one cell to a row, two, or four.
    Crest,
    /// The package page's cards: as many columns as fit.
    Folio,
    /// A gallery scene's own columns (lab pages, the marks gallery).
    Lab,
}

impl ModeId {
    /// A stable name, for motion keys and reports.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Dock => "dock",
            Self::Pins => "pins",
            Self::Bar => "bar",
            Self::Beads => "beads",
            Self::Notes => "notes",
            Self::Library => "library",
            Self::Find => "find",
            Self::Compare => "compare",
            Self::CompareColumns => "compare-columns",
            Self::Rail => "rail",
            Self::Card => "card",
            Self::Reading => "reading",
            Self::Ask => "ask",
            Self::SymbolRail => "symbol-rail",
            Self::SymbolRows => "symbol-rows",
            Self::SymbolCells => "symbol-cells",
            Self::SymbolPhone => "symbol-phone",
            Self::SymbolPrism => "symbol-prism",
            Self::Crest => "crest",
            Self::Folio => "folio",
            Self::Lab => "lab",
        }
    }
}

/// How far a mode is held past its edge before it changes, in design px:
/// half of this on each side of a threshold. Wide enough that a trackpad's
/// jitter, or a window resting on the edge, stays in one mode; narrow enough
/// that a deliberate drag never feels stuck.
pub const HYSTERESIS: Design = Design::px(32.0);

/// One mode of a ladder, and the room from which it holds.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Rung<M> {
    mode: M,
    from: Design,
}

impl<M: Copy> Rung<M> {
    /// The mode.
    #[must_use]
    pub const fn mode(&self) -> M {
        self.mode
    }

    /// The room (in design px) from which it holds.
    #[must_use]
    pub const fn from(&self) -> Design {
        self.from
    }
}

/// The mode `mode` holds from `from` design px up.
#[must_use]
pub const fn rung<M>(mode: M, from: f32) -> Rung<M> {
    Rung {
        mode,
        from: Design::px(from),
    }
}

/// An ordered set of modes, narrowest first, each holding from a room up.
///
/// Read it without history ([`Ladder::at`]) for a fresh window, a test or a
/// gallery scene; read it with the mode a region is in
/// ([`Ladder::settle`], or [`super::Modes::settle`] which remembers it) for a
/// live one.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Ladder<M: 'static> {
    id: ModeId,
    rungs: &'static [Rung<M>],
    band: Design,
}

impl<M: Copy + PartialEq + 'static> Ladder<M> {
    /// A ladder over `rungs`: the first holds from 0, the rest from strictly
    /// wider rooms. Only design tokens are built (`pub(crate)`).
    ///
    /// # Panics
    /// At compile time (in a `const`), if `rungs` is empty, the first does
    /// not start at 0, or they do not strictly widen.
    #[must_use]
    pub(crate) const fn new(id: ModeId, rungs: &'static [Rung<M>]) -> Self {
        assert!(!rungs.is_empty(), "a ladder needs a mode");
        assert!(rungs[0].from.get() == 0.0, "a ladder's first mode holds from 0");
        let mut i = 1;
        while i < rungs.len() {
            assert!(
                rungs[i - 1].from.get() < rungs[i].from.get(),
                "a ladder's modes must strictly widen"
            );
            i += 1;
        }
        Self {
            id,
            rungs,
            band: HYSTERESIS,
        }
    }

    /// The same ladder held through `band` design px around each threshold.
    #[must_use]
    pub(crate) const fn banded(self, band: f32) -> Self {
        Self {
            band: Design::px(band),
            ..self
        }
    }

    /// The decision this ladder makes.
    #[must_use]
    pub const fn id(&self) -> ModeId {
        self.id
    }

    /// The modes, narrowest first.
    #[must_use]
    pub const fn rungs(&self) -> &'static [Rung<M>] {
        self.rungs
    }

    /// The band held around each threshold.
    #[must_use]
    pub const fn band(&self) -> Design {
        self.band
    }

    /// The widths (design px) at which the ladder changes mode, narrowest
    /// first (the first rung's 0 is not one).
    pub fn thresholds(&self) -> impl Iterator<Item = Design> + '_ {
        self.rungs.iter().skip(1).map(|rung| rung.from)
    }

    /// The mode at `room`, with no history.
    #[must_use]
    pub fn at(&self, room: Room) -> M {
        self.rungs[self.settle_index(None, room)].mode
    }

    /// The mode at `room` for a region that was in `held`: it stays there
    /// until the room is half a band past the edge.
    #[must_use]
    pub fn settle(&self, held: Option<M>, room: Room) -> M {
        let index = held.and_then(|mode| self.index_of(mode));
        self.rungs[self.settle_index(index, room)].mode
    }

    pub(crate) fn index_of(&self, mode: M) -> Option<usize> {
        self.rungs.iter().position(|rung| rung.mode == mode)
    }

    pub(crate) fn settle_index(&self, held: Option<usize>, room: Room) -> usize {
        settle_index(held, self.rungs.len(), room.design().get(), self.band.get(), |index| {
            self.rungs[index].from.get()
        })
    }
}

/// The index of the step a region is on, where step `i` (from 1) is entered
/// at `enter(i)` design px going up and left again below `enter(i)` less half
/// the `band`. With no `held` step there is no history: the plain edges.
pub(crate) fn settle_index(
    held: Option<usize>,
    steps: usize,
    width: f32,
    band: f32,
    enter: impl Fn(usize) -> f32,
) -> usize {
    let half = band / 2.0;
    let Some(held) = held else {
        let mut index = 0;
        while index + 1 < steps && width >= enter(index + 1) {
            index += 1;
        }
        return index;
    };
    let mut index = held.min(steps.saturating_sub(1));
    while index + 1 < steps && width >= enter(index + 1) + half {
        index += 1;
    }
    while index > 0 && width < enter(index) - half {
        index -= 1;
    }
    index
}
