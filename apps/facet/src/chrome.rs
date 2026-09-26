//! Chrome: the frame every page sits in — the titlebar (the trail), the
//! shelf's pieces, the spine, the status bar and the pinned column's frame.
//!
//! Each piece is plain data in and a `RenderOnce` out, sized by the
//! [`Measure`](crate::Measure) of the region it gets; the shell (W-Shell)
//! owns the state and decides which regions exist at which width. Every
//! secondary element is optional: a piece shows what it is given and
//! nothing else.

#[cfg(feature = "gallery")]
pub(crate) mod gallery;
pub mod shelf;
pub mod titlebar;

pub use shelf::{
    Book, Row, RowTone, Shelf, ShelfData, ShelfRow, Spine, book, pins_frame, row, shelf, spine,
    status_bar, up_link,
};
pub use titlebar::{Bead, Here, Lights, Plan, TitleButton, Titlebar, TitlebarData, titlebar};

use gpui::Hsla;

/// `color` at `alpha` times its own opacity.
pub(crate) fn with_alpha(color: Hsla, alpha: f32) -> Hsla {
    let mut color = color;
    color.alpha *= alpha.clamp(0.0, 1.0);
    color
}
