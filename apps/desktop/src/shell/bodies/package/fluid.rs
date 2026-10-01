//! Every width decision of the package page's crest, in one place: the crest
//! (licence, heads-up, weight, advisories) is a row of four cells, and how
//! many share a row is `facet::tokens::fluid::CREST`'s decision (four when
//! the room is wide, two by two when it is middling, one under the other on a
//! phone), held through its hysteresis band by the region's `Modes`.
//!
//! Everything else is continuous: a cell is exactly the room its row leaves
//! it, so a drag of the window moves every edge with the pointer, and the
//! licence's share and the gaps ease with the width (the measure's own fluid
//! curve). The one discrete change, the number of columns, is played by the
//! caller as a layout epoch (`facet::motion::Flow`), so the cells spring to
//! their new places instead of jumping.

use facet::fluid::{Epoch, Modes};
use facet::tokens::fluid::{CREST, Crest};
use facet::{Measure, Space};
use gpui::{Pixels, px};

/// The licence's share of a four-cell row, and its bounds at 100 % text.
const STAMP_SHARE: f32 = 0.30;
const STAMP_MIN: f32 = 260.0;
const STAMP_MAX: f32 = 360.0;

/// Where the crest's cells sit at a width.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct Tracks {
    /// The arrangement.
    pub crest: Crest,
    /// How many times the arrangement has changed (a `Flow` epoch).
    pub epoch: Epoch,
    /// The gap between cells, both ways.
    pub gap: Pixels,
    /// The licence stamp's width.
    pub stamp: Pixels,
    /// Every other cell's width.
    pub cell: Pixels,
    /// How far the heads-up hand may fan out from its own left edge.
    pub spread: Pixels,
}

/// The crest's tracks in a container `measure` describes, its arrangement
/// held by `modes`.
pub(super) fn tracks(measure: &Measure, modes: &Modes) -> Tracks {
    let settled = modes.settle(&CREST, measure.fluid_room());
    layout(measure, settled.mode, settled.epoch)
}

/// The tracks of arrangement `crest` at `measure`'s width.
pub(super) fn layout(measure: &Measure, crest: Crest, epoch: Epoch) -> Tracks {
    let scale = measure.scale();
    let width = f32::from(measure.width());
    let gap = f32::from(measure.space(Space::Roomy));
    let (stamp, cell, spread) = match crest {
        Crest::Four => {
            let stamp = (width * STAMP_SHARE).clamp(STAMP_MIN * scale, STAMP_MAX * scale).min(width);
            let cell = ((width - stamp - gap * 3.0) / 3.0).max(0.0);
            (stamp, cell, width - stamp - gap)
        }
        Crest::Two => {
            let half = ((width - gap) / 2.0).max(0.0);
            (half, half, half)
        }
        Crest::One => (width, width, width),
    };
    Tracks { crest, epoch, gap: px(gap), stamp: px(stamp), cell: px(cell), spread: px(spread) }
}

#[cfg(test)]
mod tests {
    use super::*;
    use facet::Facet;

    fn at(width: f32, modes: &Modes) -> Tracks {
        tracks(&Facet::default().measure(px(width)), modes)
    }

    #[test]
    fn a_row_of_cells_never_takes_more_than_the_room_from_a_phone_to_a_big_screen() {
        let modes = Modes::new();
        for width in 240..=2600 {
            let t = at(width as f32, &modes);
            let (stamp, cell, gap) = (f32::from(t.stamp), f32::from(t.cell), f32::from(t.gap));
            let used = match t.crest {
                Crest::Four => stamp + 3.0 * cell + 3.0 * gap,
                Crest::Two => 2.0 * cell + gap,
                Crest::One => cell,
            };
            assert!(used <= width as f32 + 0.5, "{width}px: {:?} uses {used}px", t.crest);
            assert!(f32::from(t.spread) <= width as f32 + 0.5, "{width}px: the hand may fan {:?}px", t.spread);
            assert!(cell >= 0.0 && stamp > 0.0);
        }
    }

    #[test]
    fn growing_the_window_moves_every_edge_a_little_and_changes_the_arrangement_only_at_its_edges() {
        let modes = Modes::new();
        let mut last: Option<Tracks> = None;
        let mut changes = Vec::new();
        for width in 240..=2600 {
            let t = at(width as f32, &modes);
            if let Some(before) = last {
                if before.crest == t.crest {
                    // Within an arrangement a pixel of window is at most a pixel and a bit of any cell.
                    assert!((f32::from(t.cell) - f32::from(before.cell)).abs() <= 1.0, "{width}px: a cell jumped {:?} to {:?}", before.cell, t.cell);
                    assert!((f32::from(t.stamp) - f32::from(before.stamp)).abs() <= 1.0, "{width}px: the stamp jumped");
                } else {
                    changes.push((width, before.crest, t.crest));
                }
            }
            last = Some(t);
        }
        assert_eq!(changes.iter().map(|(_, from, to)| (*from, *to)).collect::<Vec<_>>(), [(Crest::One, Crest::Two), (Crest::Two, Crest::Four)], "{changes:?}");
    }

    #[test]
    fn a_window_resting_on_an_edge_does_not_flicker() {
        let band = CREST.band().get();
        for edge in CREST.thresholds().map(|edge| edge.get()) {
            let modes = Modes::new();
            let start = at(edge + band * 0.25, &modes).crest;
            for step in 0..200 {
                let width = edge + if step % 2 == 0 { -band * 0.25 } else { band * 0.25 };
                assert_eq!(at(width, &modes).crest, start, "{width}px flipped a window resting on {edge}px");
            }
        }
    }

    #[test]
    fn text_at_twice_the_size_folds_the_row_as_a_window_half_as_wide_would() {
        let wide = 1400.0;
        assert_eq!(at(wide, &Modes::new()).crest, Crest::Four);
        // 200 % text scales widths, so the same 1400 px reads as 700 effective px.
        let big = tracks(&Facet { text_scale: 2.0, ..Facet::default() }.measure(px(wide)), &Modes::new()).crest;
        assert_eq!(big, Crest::Two);
    }
}
