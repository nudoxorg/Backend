//! Control glyphs the UI icon set does not carry: plus, cross and minus,
//! drawn as flat cut bars (square ends, no rounding) so they sit in the
//! same stroke weight as the icons beside them.

use crate::paint::geom::{Fill, Poly, pt};
use gpui::{Bounds, Hsla, IntoElement, Pixels, Styled, canvas};

/// A control glyph.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub enum Glyph {
    /// `+`: add.
    Plus,
    /// `×`: remove, close.
    Cross,
    /// `−`: collapse, fewer.
    Minus,
}

/// A bar from `a` to `b` (in a 24-unit box mapped onto `bounds`), `w` wide.
fn bar(bounds: Bounds<Pixels>, a: (f32, f32), b: (f32, f32), w: f32) -> Poly {
    let k = f32::from(bounds.size.width) / 24.0;
    let (ox, oy) = (f32::from(bounds.origin.x), f32::from(bounds.origin.y));
    let (ax, ay) = (ox + a.0 * k, oy + a.1 * k);
    let (bx, by) = (ox + b.0 * k, oy + b.1 * k);
    let (dx, dy) = (bx - ax, by - ay);
    let len = (dx * dx + dy * dy).sqrt().max(1e-3);
    let (nx, ny) = (-dy / len * w * 0.5, dx / len * w * 0.5);
    // Clockwise on screen.
    Poly::new([
        pt(ax - nx, ay - ny),
        pt(bx - nx, by - ny),
        pt(bx + nx, by + ny),
        pt(ax + nx, ay + ny),
    ])
}

/// `glyph` in `color`, `size` px square, stroked like a 16 px icon.
#[must_use]
pub fn glyph(glyph: Glyph, size: Pixels, color: Hsla) -> impl IntoElement + Styled {
    canvas(
        |_, _, _| {},
        move |bounds, (), window, _| {
            let w = f32::from(size) * 1.6 / 16.0 * 1.15;
            let mut fill = Fill::new();
            match glyph {
                Glyph::Plus => {
                    fill.poly(&bar(bounds, (12.0, 5.0), (12.0, 19.0), w));
                    fill.poly(&bar(bounds, (5.0, 12.0), (12.0 - w * 0.5 * 24.0 / f32::from(size), 12.0), w));
                    fill.poly(&bar(bounds, (12.0 + w * 0.5 * 24.0 / f32::from(size), 12.0), (19.0, 12.0), w));
                }
                Glyph::Cross => {
                    fill.poly(&bar(bounds, (6.0, 6.0), (18.0, 18.0), w));
                    fill.poly(&bar(bounds, (18.0, 6.0), (12.8, 11.2), w));
                    fill.poly(&bar(bounds, (11.2, 12.8), (6.0, 18.0), w));
                }
                Glyph::Minus => fill.poly(&bar(bounds, (5.0, 12.0), (19.0, 12.0), w)),
            }
            fill.paint(window, color);
        },
    )
    .flex_none()
    .size(size)
}
