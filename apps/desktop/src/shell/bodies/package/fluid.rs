//! Every width decision of the package page's crest, in one place.
//!
//! The crest (licence, heads-up, weight, advisories) is a row of four cells.
//! How many share a row is the only structural choice: four when the room is
//! wide, two by two when it is middling, one under the other on a phone.
//! Everything else is continuous: a cell is exactly the room its row leaves
//! it, so a drag of the window moves every edge with the pointer, and the
//! licence's share and the gaps ease with the width (the measure's own fluid
//! curve). The one discrete change, the number of columns, has hysteresis so
//! a window sitting on the edge does not flicker, and the caller plays it as a
//! layout epoch (`facet::motion::Flow`), so cells spring to their new places.
//!
//! When `facet`'s fluid-layout primitive lands this is the one function to
//! swap: nothing else in the page decides a width from a breakpoint.

use facet::{Measure, Space};
use gpui::{Pixels, px};

/// How many cells of the crest share a row.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(super) enum Columns {
    /// Four in a row: the licence a little wider than the rest.
    Four,
    /// Two by two.
    Two,
    /// One under the other.
    One,
}

/// The effective width (px at 100 % text) from which each arrangement holds.
const FOUR_FROM: f32 = 980.0;
const TWO_FROM: f32 = 420.0;
/// How far past its edge an arrangement is kept before it changes.
const STICKY: f32 = 24.0;
/// The licence's share of a four-cell row, and its bounds at 100 % text.
const STAMP_SHARE: f32 = 0.30;
const STAMP_MIN: f32 = 260.0;
const STAMP_MAX: f32 = 360.0;

/// Where the crest's cells sit at a width.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct Tracks {
    /// The arrangement.
    pub columns: Columns,
    /// The gap between cells, both ways.
    pub gap: Pixels,
    /// The licence stamp's width.
    pub stamp: Pixels,
    /// Every other cell's width.
    pub cell: Pixels,
    /// How far the heads-up hand may fan out from its own left edge.
    pub spread: Pixels,
}

/// The arrangement at effective width `w`, keeping `keep` while `w` stays
/// within [`STICKY`] of its edge.
fn arrangement(w: f32, keep: Option<Columns>) -> Columns {
    let by_edge = if w >= FOUR_FROM {
        Columns::Four
    } else if w >= TWO_FROM {
        Columns::Two
    } else {
        Columns::One
    };
    let holds = |columns: Columns| match columns {
        Columns::Four => w >= FOUR_FROM - STICKY,
        Columns::Two => w >= TWO_FROM - STICKY && w < FOUR_FROM + STICKY,
        Columns::One => w < TWO_FROM + STICKY,
    };
    match keep {
        Some(kept) if holds(kept) => kept,
        _ => by_edge,
    }
}

/// The crest's tracks in a container `measure` describes; `keep` is the
/// arrangement the last frame used.
pub(super) fn tracks(measure: &Measure, keep: Option<Columns>) -> Tracks {
    let scale = measure.scale();
    let width = f32::from(measure.width());
    let gap = f32::from(measure.space(Space::Roomy));
    let columns = arrangement(width / scale, keep);
    let (stamp, cell, spread) = match columns {
        Columns::Four => {
            let stamp = (width * STAMP_SHARE).clamp(STAMP_MIN * scale, STAMP_MAX * scale).min(width);
            let cell = ((width - stamp - gap * 3.0) / 3.0).max(0.0);
            (stamp, cell, width - stamp - gap)
        }
        Columns::Two => {
            let half = ((width - gap) / 2.0).max(0.0);
            (half, half, half)
        }
        Columns::One => (width, width, width),
    };
    Tracks { columns, gap: px(gap), stamp: px(stamp), cell: px(cell), spread: px(spread) }
}

#[cfg(test)]
mod tests {
    use super::*;
    use facet::Facet;

    fn at(width: f32, keep: Option<Columns>) -> Tracks {
        tracks(&Facet::default().measure(px(width)), keep)
    }

    #[test]
    fn a_row_of_cells_never_takes_more_than_the_room_from_a_phone_to_a_big_screen() {
        let mut keep = None;
        for width in 240..=2600 {
            let t = at(width as f32, keep);
            keep = Some(t.columns);
            let (stamp, cell, gap) = (f32::from(t.stamp), f32::from(t.cell), f32::from(t.gap));
            let used = match t.columns {
                Columns::Four => stamp + 3.0 * cell + 3.0 * gap,
                Columns::Two => 2.0 * cell + gap,
                Columns::One => cell,
            };
            assert!(used <= width as f32 + 0.5, "{width}px: {:?} uses {used}px", t.columns);
            assert!(f32::from(t.spread) <= width as f32 + 0.5, "{width}px: the hand may fan {:?}px", t.spread);
            assert!(cell >= 0.0 && stamp > 0.0);
        }
    }

    #[test]
    fn growing_the_window_moves_every_edge_a_little_and_changes_the_arrangement_only_at_its_edges() {
        let mut last: Option<Tracks> = None;
        let mut keep = None;
        let mut changes = Vec::new();
        for width in 240..=2600 {
            let t = at(width as f32, keep);
            keep = Some(t.columns);
            if let Some(before) = last {
                if before.columns == t.columns {
                    // Within an arrangement a pixel of window is at most a pixel and a bit of any cell.
                    assert!((f32::from(t.cell) - f32::from(before.cell)).abs() <= 1.0, "{width}px: a cell jumped {:?} to {:?}", before.cell, t.cell);
                    assert!((f32::from(t.stamp) - f32::from(before.stamp)).abs() <= 1.0, "{width}px: the stamp jumped");
                } else {
                    changes.push((width, before.columns, t.columns));
                }
            }
            last = Some(t);
        }
        assert_eq!(changes.iter().map(|(_, from, to)| (*from, *to)).collect::<Vec<_>>(), [(Columns::One, Columns::Two), (Columns::Two, Columns::Four)], "{changes:?}");
    }

    #[test]
    fn a_window_resting_on_an_edge_does_not_flicker() {
        // Straddle each edge by a few pixels, back and forth: the arrangement never changes.
        for edge in [TWO_FROM, FOUR_FROM] {
            let start = at(edge + 4.0, None).columns;
            let mut keep = Some(start);
            for step in 0..200 {
                let width = edge + if step % 2 == 0 { -8.0 } else { 8.0 };
                let t = at(width, keep);
                assert_eq!(t.columns, start, "{width}px flipped a window resting on {edge}px");
                keep = Some(t.columns);
            }
        }
    }

    #[test]
    fn leaving_an_edge_by_more_than_the_sticky_margin_does_change_it() {
        assert_eq!(at(FOUR_FROM - STICKY - 1.0, Some(Columns::Four)).columns, Columns::Two);
        assert_eq!(at(FOUR_FROM + STICKY + 1.0, Some(Columns::Two)).columns, Columns::Four);
        assert_eq!(at(TWO_FROM - STICKY - 1.0, Some(Columns::Two)).columns, Columns::One);
        assert_eq!(at(TWO_FROM + STICKY + 1.0, Some(Columns::One)).columns, Columns::Two);
    }

    #[test]
    fn text_at_twice_the_size_folds_the_row_as_a_window_half_as_wide_would() {
        let wide = 1400.0;
        let normal = at(wide, None).columns;
        assert_eq!(normal, Columns::Four);
        // 200 % text scales widths, so the same 1400 px reads as 700 effective px.
        let big = tracks(&Facet { text_scale: 2.0, ..Facet::default() }.measure(px(wide)), None).columns;
        assert_eq!(big, Columns::Two);
    }
}
