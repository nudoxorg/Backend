//! The shell's structural layout, as a pure function of the window's width,
//! the text scale and the user's shelf preferences.
//!
//! Only structure is decided here (is there a shelf, a spine, a pins
//! column, and how wide are they at rest). Every region then lays itself out
//! from its own measured width. Decisions are made on the *effective* width
//! (`width ÷ text scale`), so 200 % text on a 1440 px window behaves exactly
//! like a 720 px window: the shelf becomes a spine instead of text
//! overflowing its box (`Nudox-Design-System/v4/shots/flow-sheet.png`).

use facet::Room;

/// Resting shelf width at 100 % text.
pub(crate) const SHELF: f32 = 264.0;
/// Narrowest resizable shelf.
pub(crate) const SHELF_MIN: f32 = 200.0;
/// Widest resizable shelf.
pub(crate) const SHELF_MAX: f32 = 420.0;
/// The collapsed shelf.
pub(crate) const KSPINE: f32 = 42.0;
/// The third column of pinned peeks.
pub(crate) const PINS: f32 = 320.0;
/// Titlebar height at 100 % text.
pub(crate) const TITLEBAR: f32 = 50.0;
/// Status bar height at 100 % text.
pub(crate) const STATUS: f32 = 26.0;
/// Below this effective width the shelf is a spine.
pub(crate) const SHELF_SPINE: f32 = 900.0;
/// Below this effective width even the spine is on request only.
pub(crate) const SPINE_OVERLAY: f32 = 640.0;
/// From this effective width a pinned peek gets its own column.
pub(crate) const PINS_FROM: f32 = 1900.0;

/// What sits in the shelf column.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub enum ShelfMode {
    /// The full shelf inline.
    Shelf,
    /// The 42 px kspine inline.
    Spine,
    /// Nothing inline (zen, or too narrow for a spine).
    Hidden,
}

/// The inputs of one layout decision.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FrameInput {
    /// Window content width, px.
    pub width: f32,
    /// Text scale (1.0 = 100 %).
    pub scale: f32,
    /// The user wants the shelf open (⌘\).
    pub shelf_open: bool,
    /// Zen: one page, no shelf, no pins (⌘.).
    pub zen: bool,
    /// Preferred shelf width at 100 % text (the splitter).
    pub shelf_width: f32,
    /// Something is pinned (the pins column only exists for pins).
    pub pinned: bool,
}

/// One resolved shell layout.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Frame {
    /// The window's room class on effective width.
    pub room: Room,
    /// What sits in the shelf column.
    pub shelf: ShelfMode,
    /// Whether the full shelf opens over the reader when asked (the window is
    /// too narrow to hold it inline).
    pub shelf_overlays: bool,
    /// Width of the shelf column at rest, px.
    pub shelf_width: f32,
    /// Width of the full shelf wherever it is drawn (inline or over), px.
    pub shelf_body: f32,
    /// Whether the pins column is present.
    pub pins: bool,
    /// Width of the pins column at rest, px.
    pub pins_width: f32,
    /// Titlebar height, px.
    pub titlebar: f32,
    /// Status bar height, px.
    pub status: f32,
}

impl Frame {
    /// Resolves the layout for `input`.
    #[must_use]
    pub fn resolve(input: FrameInput) -> Self {
        let scale = input.scale.clamp(0.5, 4.0);
        let effective = input.width / scale;
        let preferred = input.shelf_width.clamp(SHELF_MIN, SHELF_MAX);
        let shelf = if input.zen || effective < SPINE_OVERLAY {
            ShelfMode::Hidden
        } else if effective < SHELF_SPINE || !input.shelf_open {
            ShelfMode::Spine
        } else {
            ShelfMode::Shelf
        };
        let shelf_width = match shelf {
            ShelfMode::Shelf => preferred * scale,
            ShelfMode::Spine => KSPINE * scale,
            ShelfMode::Hidden => 0.0,
        };
        // Pinned peeks get a third column only on a window wide enough that
        // the page keeps its full measure beside them (the calm Peeks board).
        let pins = !input.zen && input.pinned && effective >= PINS_FROM;
        Self {
            room: Room::of(effective),
            shelf,
            shelf_overlays: !input.zen && effective < SHELF_SPINE,
            shelf_width,
            shelf_body: preferred * scale,
            pins,
            pins_width: if pins { PINS * scale } else { 0.0 },
            titlebar: TITLEBAR * scale,
            status: STATUS * scale,
        }
    }

    /// Width left for the reader, px.
    #[must_use]
    pub fn reader_width(&self, window: f32) -> f32 {
        (window - self.shelf_width - self.pins_width).max(0.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(width: f32, scale: f32) -> Frame {
        Frame::resolve(FrameInput {
            width,
            scale,
            shelf_open: true,
            zen: false,
            shelf_width: SHELF,
            pinned: true,
        })
    }

    #[test]
    fn the_flow_targets_hold_at_every_width() {
        // 2560: shelf, reader and the third column of pins.
        let vast = at(2560.0, 1.0);
        assert_eq!((vast.shelf, vast.pins), (ShelfMode::Shelf, true));
        assert!((vast.reader_width(2560.0) - (2560.0 - 264.0 - 320.0)).abs() < 0.01);
        // 1440 and 1100: the shelf stays, no pins.
        for width in [1440.0, 1100.0] {
            let frame = at(width, 1.0);
            assert_eq!((frame.shelf, frame.pins), (ShelfMode::Shelf, false), "{width}");
        }
        // 760: the shelf is a spine.
        assert_eq!(at(760.0, 1.0).shelf, ShelfMode::Spine);
        assert!((at(760.0, 1.0).shelf_width - 42.0).abs() < 0.01);
        // 480: nothing inline; the shelf opens over the reader on request.
        let narrow = at(480.0, 1.0);
        assert_eq!(narrow.shelf, ShelfMode::Hidden);
        assert!(narrow.shelf_overlays);
    }

    #[test]
    fn two_hundred_percent_text_behaves_like_half_the_window() {
        let big = at(1440.0, 2.0);
        assert_eq!(big.shelf, at(720.0, 1.0).shelf);
        assert_eq!(big.shelf, ShelfMode::Spine);
        // The spine and the chrome scale with the text.
        assert!((big.shelf_width - 84.0).abs() < 0.01);
        assert!((big.titlebar - 100.0).abs() < 0.01);
        // 85 % text on 900 px is 1059 effective: a shelf.
        assert_eq!(at(900.0, 0.85).shelf, ShelfMode::Shelf);
    }

    #[test]
    fn pins_need_a_pin_and_nineteen_hundred_effective_px() {
        // 1700 px is a vast window, but not wide enough for a third column.
        let frame = at(1700.0, 1.0);
        assert_eq!(frame.room, Room::Vast);
        assert!(!frame.pins);
        assert!(at(1900.0, 1.0).pins);
        // 200 % text on 2560 px is 1280 effective: no pins.
        assert!(!at(2560.0, 2.0).pins);
        // Nothing pinned, no column, however wide.
        let empty = Frame::resolve(FrameInput {
            width: 2560.0,
            scale: 1.0,
            shelf_open: true,
            zen: false,
            shelf_width: SHELF,
            pinned: false,
        });
        assert!(!empty.pins);
    }

    #[test]
    fn zen_and_the_shelf_toggle() {
        let zen = Frame::resolve(FrameInput {
            width: 2560.0,
            scale: 1.0,
            shelf_open: true,
            zen: true,
            shelf_width: SHELF,
            pinned: true,
        });
        assert_eq!((zen.shelf, zen.pins), (ShelfMode::Hidden, false));
        let closed = Frame::resolve(FrameInput {
            width: 1440.0,
            scale: 1.0,
            shelf_open: false,
            zen: false,
            shelf_width: SHELF,
            pinned: false,
        });
        assert_eq!(closed.shelf, ShelfMode::Spine);
    }

    #[test]
    fn the_splitter_is_clamped_and_scaled() {
        let frame = Frame::resolve(FrameInput {
            width: 1440.0,
            scale: 1.25,
            shelf_open: true,
            zen: false,
            shelf_width: 900.0,
            pinned: false,
        });
        assert!((frame.shelf_width - SHELF_MAX * 1.25).abs() < 0.01);
    }
}
