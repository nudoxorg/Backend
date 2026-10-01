//! The shell's structural layout, as a pure function of the window's room,
//! the user's shelf preferences and the modes the window was in.
//!
//! Only structure is decided here (is there a shelf, a spine, a drawer, a
//! pins column, and how wide are they at rest). Every region then lays itself
//! out from its own measured room. The decisions are `facet::fluid` modes
//! ([`DOCK`], [`PINS`]), made on the window's *design* width (width ÷ text
//! scale), so 200 % text on a 1440 px window behaves exactly like a 720 px
//! window: the shelf becomes a spine instead of text overflowing its box.
//! The modes are held through a hysteresis band, so a window resting on a
//! threshold does not flip them every frame; the caller keeps the
//! [`Modes`] between frames.
//!
//! No width is compared with a number here, and no frame constant is
//! defined here: the sizes are `facet::tokens::geo` and the thresholds are
//! `facet::tokens::fluid`.

use facet::fluid::{Modes, Room, Settled};
use facet::tokens::fluid::{DOCK, DRAWER_STRIP, Dock, PINS, Pins};
use facet::tokens::geo;
use gpui::{Pixels, px};

/// What sits in the shelf column.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub enum ShelfMode {
    /// The full shelf inline.
    Shelf,
    /// The 42 px kspine inline.
    Spine,
    /// Nothing inline (zen, or a drawer over the page on request).
    Hidden,
}

/// The inputs of one layout decision.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FrameInput {
    /// The window: its content width and the text scale.
    pub window: Room,
    /// The user wants the shelf open (⌘\).
    pub shelf_open: bool,
    /// Zen: one page, no shelf, no pins (⌘.).
    pub zen: bool,
    /// Preferred shelf width at 100 % text (the splitter).
    pub shelf_width: Pixels,
    /// Something is pinned (the pins column only exists for pins).
    pub pinned: bool,
}

/// One resolved shell layout.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Frame {
    /// The window's room.
    pub room: Room,
    /// How the shelf sits in the window, and what it changed from.
    pub dock: Settled<Dock>,
    /// What sits in the shelf column.
    pub shelf: ShelfMode,
    /// Whether the full shelf opens over the reader when asked (the window is
    /// too narrow to hold it inline).
    pub shelf_overlays: bool,
    /// Width of the shelf column at rest.
    pub shelf_width: Pixels,
    /// Width of the full shelf wherever it is drawn (inline or over).
    pub shelf_body: Pixels,
    /// Width of the shelf when it opens over the page: the shelf's own, less
    /// the strip of page left beside it to click on.
    pub drawer: Pixels,
    /// Whether the pins column is present.
    pub pins: bool,
    /// Width of the pins column at rest.
    pub pins_width: Pixels,
    /// Titlebar height.
    pub titlebar: Pixels,
    /// Status bar height.
    pub status: Pixels,
}

impl Frame {
    /// Resolves the layout for `input`, holding `modes` through their
    /// hysteresis bands.
    #[must_use]
    pub fn resolve(input: FrameInput, modes: &Modes) -> Self {
        let scale = input.window.scale();
        let preferred = input.shelf_width.max(geo::SHELF_MIN).min(geo::SHELF_MAX);
        let dock = modes.settle(&DOCK, input.window);
        let shelf = if input.zen || dock.mode == Dock::Drawer {
            ShelfMode::Hidden
        } else if dock.mode == Dock::Spine || !input.shelf_open {
            ShelfMode::Spine
        } else {
            ShelfMode::Shelf
        };
        let shelf_body = preferred * scale;
        let shelf_width = match shelf {
            ShelfMode::Shelf => shelf_body,
            ShelfMode::Spine => geo::KSPINE * scale,
            ShelfMode::Hidden => px(0.0),
        };
        // Pinned peeks get a third column only on a window wide enough that
        // the page keeps its full measure beside them (the calm Peeks board).
        let pins =
            !input.zen && input.pinned && modes.settle(&PINS, input.window).mode == Pins::Column;
        Self {
            room: input.window,
            dock,
            shelf,
            shelf_overlays: !input.zen && dock.mode != Dock::Shelf,
            shelf_width,
            shelf_body,
            drawer: shelf_body
                .min((input.window.width() - DRAWER_STRIP.at(input.window)).max(px(0.0))),
            pins,
            pins_width: if pins { geo::PINS * scale } else { px(0.0) },
            titlebar: geo::TITLEBAR * scale,
            status: geo::STATUS * scale,
        }
    }

    /// Width left for the reader.
    #[must_use]
    pub fn reader_width(&self, window: Pixels) -> Pixels {
        (window - self.shelf_width - self.pins_width).max(px(0.0))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn input(width: f32, scale: f32) -> FrameInput {
        FrameInput {
            window: Room::new(px(width), scale),
            shelf_open: true,
            zen: false,
            shelf_width: geo::SHELF,
            pinned: true,
        }
    }

    /// A window that has just opened at `width`: no history.
    fn at(width: f32, scale: f32) -> Frame {
        Frame::resolve(input(width, scale), &Modes::new())
    }

    fn near(a: Pixels, b: f32) -> bool {
        (f32::from(a) - b).abs() < 0.01
    }

    #[test]
    fn the_flow_targets_hold_at_every_width() {
        // 2560: shelf, reader and the third column of pins.
        let vast = at(2560.0, 1.0);
        assert_eq!((vast.shelf, vast.pins), (ShelfMode::Shelf, true));
        assert!(near(vast.reader_width(px(2560.0)), 2560.0 - 264.0 - 320.0));
        // 1440 and 1100: the shelf stays, no pins.
        for width in [1440.0, 1100.0] {
            let frame = at(width, 1.0);
            assert_eq!(
                (frame.shelf, frame.pins),
                (ShelfMode::Shelf, false),
                "{width}"
            );
        }
        // 760: the shelf is a spine.
        assert_eq!(at(760.0, 1.0).shelf, ShelfMode::Spine);
        assert!(near(at(760.0, 1.0).shelf_width, 42.0));
        // 480: nothing inline; the shelf opens over the reader on request.
        let narrow = at(480.0, 1.0);
        assert_eq!(narrow.shelf, ShelfMode::Hidden);
        assert!(narrow.shelf_overlays);
    }

    #[test]
    fn a_phone_gets_a_drawer_that_leaves_a_strip_of_page_to_click() {
        for width in [320.0, 360.0, 390.0] {
            let frame = at(width, 1.0);
            assert_eq!(frame.dock.mode, Dock::Drawer, "{width}");
            assert_eq!(frame.shelf, ShelfMode::Hidden);
            assert!(frame.shelf_overlays);
            assert!(
                f32::from(frame.drawer) <= width - 47.9,
                "{width}: the drawer covers the whole page ({:?})",
                frame.drawer
            );
            assert!(
                f32::from(frame.drawer) >= 200.0,
                "{width}: the drawer is a sliver ({:?})",
                frame.drawer
            );
        }
        // A roomy window's drawer is the shelf's own width.
        assert!(near(at(620.0, 1.0).drawer, 264.0));
    }

    #[test]
    fn two_hundred_percent_text_behaves_like_half_the_window() {
        let big = at(1440.0, 2.0);
        assert_eq!(big.shelf, at(720.0, 1.0).shelf);
        assert_eq!(big.shelf, ShelfMode::Spine);
        // The spine and the chrome scale with the text.
        assert!(near(big.shelf_width, 84.0));
        assert!(near(big.titlebar, 100.0));
        // 85 % text on 900 px is 1059 effective: a shelf.
        assert_eq!(at(900.0, 0.85).shelf, ShelfMode::Shelf);
    }

    #[test]
    fn pins_need_a_pin_and_nineteen_hundred_effective_px() {
        // 1700 px is a big window, but not wide enough for a third column.
        assert!(!at(1700.0, 1.0).pins);
        assert!(at(1900.0, 1.0).pins);
        // 200 % text on 2560 px is 1280 effective: no pins.
        assert!(!at(2560.0, 2.0).pins);
        // Nothing pinned, no column, however wide.
        let empty = Frame::resolve(
            FrameInput {
                pinned: false,
                ..input(2560.0, 1.0)
            },
            &Modes::new(),
        );
        assert!(!empty.pins);
    }

    #[test]
    fn zen_and_the_shelf_toggle() {
        let zen = Frame::resolve(
            FrameInput {
                zen: true,
                ..input(2560.0, 1.0)
            },
            &Modes::new(),
        );
        assert_eq!((zen.shelf, zen.pins), (ShelfMode::Hidden, false));
        let closed = Frame::resolve(
            FrameInput {
                shelf_open: false,
                pinned: false,
                ..input(1440.0, 1.0)
            },
            &Modes::new(),
        );
        assert_eq!(closed.shelf, ShelfMode::Spine);
    }

    #[test]
    fn the_splitter_is_clamped_and_scaled() {
        let frame = Frame::resolve(
            FrameInput {
                shelf_width: px(900.0),
                pinned: false,
                ..input(1440.0, 1.25)
            },
            &Modes::new(),
        );
        assert!(near(frame.shelf_width, 420.0 * 1.25));
    }

    #[test]
    fn a_window_resting_on_a_threshold_does_not_flip_the_shelf() {
        // Dragged narrower across 900: the shelf holds until 16 px past the
        // edge, then becomes a spine, and does not come back until 16 px
        // past it the other way.
        let modes = Modes::new();
        let shelf = |width: f32| Frame::resolve(input(width, 1.0), &modes).shelf;
        assert_eq!(shelf(1000.0), ShelfMode::Shelf);
        assert_eq!(shelf(890.0), ShelfMode::Shelf, "inside the band: held");
        assert_eq!(shelf(883.0), ShelfMode::Spine, "past it: a spine");
        assert_eq!(
            shelf(910.0),
            ShelfMode::Spine,
            "inside the band again: held"
        );
        assert_eq!(shelf(917.0), ShelfMode::Shelf, "past it: a shelf");
        // Ten px either side of the edge, for 200 frames: no change at all.
        for frame in 0..200 {
            let width = 900.0 + if frame % 2 == 0 { -10.0 } else { 10.0 };
            assert_eq!(shelf(width), ShelfMode::Shelf, "flipped at {width}");
        }
    }
}
