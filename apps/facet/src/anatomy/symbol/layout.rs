//! The page's width, decided in one place.
//!
//! Everything that depends on how wide the room is comes from [`Layout::of`]:
//! the main column, where the rail sits, how case rows and method groups
//! are set, and the few lengths that follow the room. The arrangements are
//! `facet::fluid` ladders read through the region's [`Modes`] (held through
//! their hysteresis band, counted by an epoch that a `Flow` can FLIP on); the
//! lengths are fluid tokens. The board's container queries are on the
//! reader (its width, side padding included), so the ladders are read from
//! the reader's room: the page's room plus the reader's two gutters.

use crate::fluid::{Epoch, Modes, Room};
use crate::measure::Measure;
use crate::tokens::fluid::{Cells, READER_PAD, Rail, Rows, SYMBOL_CELLS, SYMBOL_LABEL, SYMBOL_PHONE, SYMBOL_PLACE, SYMBOL_RAIL, SYMBOL_ROWS, SYMBOL_SECTION, Screen};
use gpui::{Pixels, px};

/// The main column's cap at 100 % text.
pub const MAIN: f32 = 860.0;
/// The rail's width at 100 % text.
pub const RAIL: f32 = 280.0;
/// The gap between them at 100 % text.
pub const GAP: f32 = 56.0;
/// About how wide one character of code is at 100 % text.
const CODE_CHAR: f32 = 7.5;

/// How many characters of code fit on a place's row in a column `width` wide
/// (at 100 % text) whose file column is `place` wide: the glyph, the gaps and
/// the fill chip take their share; on a row too narrow to hold the code beside
/// its file, the code has a row to itself.
fn code_chars(width: f32, place: f32) -> usize {
    const GLYPH: f32 = 22.0;
    const GAPS_AND_FILL: f32 = 60.0 + 150.0;
    const LEAST_BESIDE: f32 = 200.0;
    let beside = width - GLYPH - place - GAPS_AND_FILL;
    let room = if beside < LEAST_BESIDE { width - GLYPH } else { beside };
    (room.max(0.0) / CODE_CHAR) as usize
}

/// Where the rail sits.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Side {
    /// At the right of the page, this wide.
    Beside(Pixels),
    /// Under the page, never above it.
    Below,
}

/// What the room decides.
#[derive(Clone, Copy, Debug)]
pub struct Layout {
    /// The main column's width.
    pub main: Pixels,
    /// Where the rail sits.
    pub side: Side,
    /// The gap between the page and the rail beside it.
    pub gap: Pixels,
    /// The vertical gap between sections, at 100 % text.
    pub section: f32,
    /// The width of the rail's label column, at 100 % text.
    pub label: f32,
    /// The width of a place's file column, at 100 % text.
    pub place: f32,
    /// How many characters of a code line fit beside a place's file.
    pub code: usize,
    /// Whether the room is a phone's.
    pub screen: Screen,
    /// How case and field rows are set.
    pub rows: Rows,
    /// How many columns method groups take.
    pub cells: Cells,
    /// How many times each arrangement has changed (a `Flow`'s epoch).
    pub epoch: Epochs,
}

/// How many times each of the page's arrangements has changed its mind: what
/// a [`Flow`](crate::motion::Flow) is told so the parts that moved spring to
/// their new places instead of jumping.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq)]
pub struct Epochs {
    /// The rail moved beside or below.
    pub rail: Epoch,
    /// Case and field rows stacked or spread.
    pub rows: Epoch,
    /// Method groups took one column or two.
    pub cells: Epoch,
    /// The room became a phone's or a window's.
    pub screen: Epoch,
}

fn design(value: Pixels, scale: f32) -> f32 {
    f32::from(value) / scale
}

impl Layout {
    /// The layout for a page `measure` wide, its modes remembered in `modes`.
    #[must_use]
    pub fn of(measure: &Measure, modes: &Modes) -> Self {
        let room = measure.fluid_room();
        let scale = measure.scale();
        // The reader is the page's room and its two gutters.
        let reader: Room = room.within(room.width() + READER_PAD.at(room) * 2.0);
        let rail = modes.settle(&SYMBOL_RAIL, reader);
        let (main, side) = match rail.mode {
            Rail::Beside => {
                let taken = px((RAIL + GAP) * scale);
                (px((MAIN * scale).min(f32::from(room.width() - taken))), Side::Beside(px(RAIL * scale)))
            }
            Rail::Below => (room.width(), Side::Below),
        };
        let (screen, rows, cells) = (modes.settle(&SYMBOL_PHONE, room), modes.settle(&SYMBOL_ROWS, reader), modes.settle(&SYMBOL_CELLS, reader));
        Self {
            main,
            side,
            gap: px(GAP * scale),
            section: design(SYMBOL_SECTION.at(room), scale),
            label: design(SYMBOL_LABEL.at(room), scale),
            place: design(SYMBOL_PLACE.at(room), scale),
            code: code_chars(design(main, scale), design(SYMBOL_PLACE.at(room), scale)),
            screen: screen.mode,
            rows: rows.mode,
            cells: cells.mode,
            epoch: Epochs { rail: rail.epoch, rows: rows.epoch, cells: cells.epoch, screen: screen.epoch },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::theme::Facet;

    fn at(width: f32) -> Measure {
        Measure::new(px(width), &Facet::default())
    }

    #[test]
    fn the_rail_moves_under_below_the_breakpoint_and_never_above() {
        let modes = Modes::new();
        let wide = Layout::of(&at(1096.0), &modes);
        assert!(matches!(wide.side, Side::Beside(_)), "a 1440 window: the reader is 1176 wide");
        assert!(f32::from(wide.main) <= MAIN + 0.5);
        let narrow = Layout::of(&at(700.0), &Modes::new());
        assert_eq!(narrow.side, Side::Below);
        assert!((f32::from(narrow.main) - 700.0).abs() < 0.5);
    }

    #[test]
    fn a_width_at_the_edge_does_not_flicker() {
        let modes = Modes::new();
        let side = |w: f32| matches!(Layout::of(&at(w), &modes).side, Side::Beside(_));
        // The reader's edge is 1100 (about 1040 of page): a 40 px band holds it.
        assert!(side(1200.0) && side(1030.0), "from wide, it stays beside through the band");
        assert!(!side(1000.0));
        assert!(!side(1030.0) && !side(1050.0), "from narrow, it stays under through the band");
        assert!(side(1070.0));
    }

    #[test]
    fn a_phone_is_a_screen_and_lengths_follow_the_room_without_a_cliff() {
        let modes = Modes::new();
        assert_eq!(Layout::of(&at(320.0), &modes).screen, Screen::Phone);
        let mut last = Layout::of(&at(320.0), &modes);
        for w in 321..2600 {
            let now = Layout::of(&at(w as f32), &modes);
            assert!((now.section - last.section).abs() < 0.2 && (now.label - last.label).abs() < 0.3 && (now.place - last.place).abs() < 0.6, "a step at {w}");
            last = now;
        }
        assert_eq!(last.screen, Screen::Window);
    }

    #[test]
    fn the_code_a_place_row_holds_grows_with_the_room_and_a_phone_gives_it_a_row_of_its_own() {
        let modes = Modes::new();
        let phone = Layout::of(&at(320.0), &modes).code;
        let tablet = Layout::of(&at(700.0), &modes).code;
        let wide = Layout::of(&at(1096.0), &modes).code;
        assert!((25..=45).contains(&phone), "a phone holds a line's worth beside nothing: {phone}");
        assert!(tablet > phone && wide > 45, "{phone} {tablet} {wide}");
    }

    #[test]
    fn a_mode_that_changes_is_an_epoch_so_the_parts_that_move_can_flow() {
        let modes = Modes::new();
        let start = Layout::of(&at(1200.0), &modes).epoch;
        assert_eq!(Layout::of(&at(1190.0), &modes).epoch, start, "a drag inside a mode is not an epoch");
        let below = Layout::of(&at(900.0), &modes).epoch;
        assert_ne!(below.rail, start.rail, "the rail moving under the page is one");
        assert_eq!(Layout::of(&at(880.0), &modes).epoch, below, "and the next width is not");
        assert_ne!(Layout::of(&at(320.0), &modes).epoch.screen, below.screen, "so is a phone");
    }
}
