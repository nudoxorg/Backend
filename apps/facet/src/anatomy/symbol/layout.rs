//! The page's width, decided in one place.
//!
//! Everything that depends on how wide the room is comes from [`Layout::of`]:
//! the main column, where the rail sits, and the few scalars that follow the
//! container (the label column, the place column, the vertical rhythm).
//! Scalars interpolate with the room; the one structural change (the rail
//! beside the page or under it) has a hysteresis band so a width that hovers
//! at the edge does not flicker. A fluid-layout primitive can replace this
//! function without touching a part.

use super::host::Spots;
use crate::measure::Measure;
use gpui::{Pixels, px};

/// The main column's cap at 100 % text.
pub const MAIN: f32 = 860.0;
/// The rail's width at 100 % text.
pub const RAIL: f32 = 280.0;
/// The gap between them at 100 % text.
pub const GAP: f32 = 56.0;
/// The room (effective px) at which the rail comes beside the page.
pub const ENTER: f32 = 1120.0;
/// The room (effective px) at which it goes back under.
pub const LEAVE: f32 = 1080.0;

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
    /// Where the room sits on the fluid curve, 0 (phone) to 1 (wide).
    pub t: f32,
    /// The vertical gap between sections, at 100 % text.
    pub section: f32,
    /// The width of the rail's label column, at 100 % text.
    pub label: f32,
    /// The width of a place's file column, at 100 % text.
    pub place: f32,
    /// The room is a phone's: rows stack and chips wrap.
    pub phone: bool,
}

fn lerp(low: f32, high: f32, t: f32) -> f32 {
    low + (high - low) * t
}

impl Layout {
    /// The layout for a room `measure` wide, remembering (in `spots`) which
    /// side of the rail's breakpoint it last drew.
    #[must_use]
    pub fn of(measure: &Measure, spots: &Spots) -> Self {
        let scale = measure.scale();
        let width = f32::from(measure.width());
        let effective = measure.effective();
        let beside = match spots.beside() {
            Some(true) => effective >= LEAVE,
            Some(false) => effective >= ENTER,
            None => effective >= ENTER,
        };
        spots.set_beside(beside);
        let t = measure.t();
        let (main, side) = if beside {
            let rail = RAIL * scale;
            (px((MAIN * scale).min(width - (RAIL + GAP) * scale)), Side::Beside(px(rail)))
        } else {
            (px(width), Side::Below)
        };
        Self {
            main,
            side,
            gap: px(GAP * scale),
            t,
            section: lerp(24.0, 34.0, t),
            label: lerp(58.0, 84.0, t),
            place: lerp(132.0, 210.0, t),
            phone: effective < 480.0,
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
        let spots = Spots::new();
        let wide = Layout::of(&at(1440.0), &spots);
        assert!(matches!(wide.side, Side::Beside(_)));
        assert!(f32::from(wide.main) <= MAIN + 0.5);
        let narrow = Layout::of(&at(800.0), &Spots::new());
        assert_eq!(narrow.side, Side::Below);
        assert!((f32::from(narrow.main) - 800.0).abs() < 0.5);
    }

    #[test]
    fn a_width_at_the_edge_does_not_flicker() {
        let spots = Spots::new();
        // Coming from wide, the rail stays beside until 1080.
        assert!(matches!(Layout::of(&at(1200.0), &spots).side, Side::Beside(_)));
        assert!(matches!(Layout::of(&at(1100.0), &spots).side, Side::Beside(_)));
        assert_eq!(Layout::of(&at(1070.0), &spots).side, Side::Below);
        // Coming from narrow, it stays under until 1120.
        assert_eq!(Layout::of(&at(1100.0), &spots).side, Side::Below);
        assert!(matches!(Layout::of(&at(1130.0), &spots).side, Side::Beside(_)));
    }

    #[test]
    fn scalars_follow_the_room_without_a_cliff() {
        let spots = Spots::new();
        let mut last = Layout::of(&at(320.0), &spots);
        assert!(last.phone);
        for w in 321..2600 {
            let now = Layout::of(&at(w as f32), &spots);
            assert!((now.section - last.section).abs() < 0.2 && (now.label - last.label).abs() < 0.3 && (now.place - last.place).abs() < 0.6, "a step at {w}");
            if w == 480 {
                assert!(!now.phone);
            }
            last = now;
        }
    }
}
