//! Layout lints on a painted frame: the probe's text boxes and targets plus
//! the pixels that were actually drawn.
//!
//! | Rule | Fails when |
//! |---|---|
//! | `clip` | a text box is narrower than its text (clip overflow) or than its widest word (wrap overflow) and draws no ellipsis, or shorter than one line |
//! | `overlap` | two texts of one [`crate::probe::region`] overlap by more than 1 px in both axes |
//! | `offscreen` | a focusable element is not entirely inside the viewport |
//! | `target` | a clickable element is smaller than 24 x 24 px |
//! | `contrast` | the ink painted in a text box against the ground painted around it is under 4.5:1 (3:1 for large text: 24 px, or 18.66 px at weight 700) |
//!
//! Contrast is measured from the frame's pixels: the ground is the box's
//! most common colour, the ink the pixel that contrasts with it most. Glyph
//! stems reach full coverage at 2x, so contrast is only measured at 2x; a 1x
//! capture reports it as not measured rather than guessing.

use super::json::Json;
use crate::probe::Ledger;
use backend_gui_harness::{Viewport, contrast_ratio};
use image::RgbaImage;
use std::collections::HashMap;

/// A lint rule.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum Rule {
    /// Clipped text.
    Clip,
    /// Overlapping sibling text.
    Overlap,
    /// A focusable off the viewport.
    Offscreen,
    /// A hit target under 24 px.
    Target,
    /// Contrast under the WCAG AA threshold.
    Contrast,
}

impl Rule {
    /// Every rule, in report order.
    pub const ALL: [Self; 5] = [
        Self::Clip,
        Self::Overlap,
        Self::Offscreen,
        Self::Target,
        Self::Contrast,
    ];

    /// A stable name.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Clip => "clip",
            Self::Overlap => "overlap",
            Self::Offscreen => "offscreen",
            Self::Target => "target",
            Self::Contrast => "contrast",
        }
    }
}

/// One lint failure.
#[derive(Clone, Debug, PartialEq)]
pub struct Lint {
    /// The rule.
    pub rule: Rule,
    /// The element (or `a + b` for overlaps).
    pub key: String,
    /// What, with operands.
    pub detail: String,
}

/// What the lints looked at.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Coverage {
    /// Text boxes checked.
    pub texts: usize,
    /// Targets checked.
    pub targets: usize,
    /// Text boxes whose contrast was measured.
    pub contrast: usize,
    /// Text boxes whose contrast could not be measured (1x, off-screen, empty).
    pub contrast_skipped: usize,
}

/// The lints of one frame.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Linted {
    /// Failures.
    pub lints: Vec<Lint>,
    /// Coverage.
    pub coverage: Coverage,
    /// The lowest measured contrast and where.
    pub lowest_contrast: Option<(String, f32)>,
}

fn intersection(a: &crate::probe::BoundsSample, b: &crate::probe::BoundsSample) -> (f32, f32) {
    let width = (a.x + a.width).min(b.x + b.width) - a.x.max(b.x);
    let height = (a.y + a.height).min(b.y + b.height) - a.y.max(b.y);
    (width, height)
}

/// Measures the contrast of the ink in a logical box of `image` (painted at
/// `scale`): the ground is the most common colour, the ink the pixel that
/// contrasts with it most. `None` when the box has no pixels on the image or
/// holds one colour only.
#[must_use]
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_precision_loss
)]
pub fn ink_contrast(
    image: &RgbaImage,
    scale: u8,
    x: f32,
    y: f32,
    width: f32,
    height: f32,
) -> Option<(f32, [u8; 3], [u8; 3])> {
    let s = f32::from(scale);
    let left = (x * s).floor().max(0.0) as u32;
    let top = (y * s).floor().max(0.0) as u32;
    let right = (((x + width) * s).ceil() as u32).min(image.width());
    let bottom = (((y + height) * s).ceil() as u32).min(image.height());
    if right <= left || bottom <= top {
        return None;
    }
    let mut counts: HashMap<[u8; 3], u32> = HashMap::new();
    for py in top..bottom {
        for px in left..right {
            let [r, g, b, _] = image.get_pixel(px, py).0;
            *counts.entry([r, g, b]).or_default() += 1;
        }
    }
    let (ground, _) = counts.iter().max_by_key(|(colour, count)| (**count, **colour))?;
    let ground = *ground;
    let (ink, ratio) = counts
        .keys()
        .map(|colour| (*colour, contrast_ratio(*colour, ground)))
        .max_by(|a, b| a.1.total_cmp(&b.1))?;
    (counts.len() > 1).then_some((ratio, ink, ground))
}

/// Lints one frame: `image` painted at `viewport` (its scale), with the
/// probe's `ledger` from the same draw.
#[must_use]
pub fn lint(image: &RgbaImage, ledger: &Ledger, viewport: Viewport) -> Linted {
    let mut out = Linted::default();
    #[allow(clippy::cast_precision_loss)]
    let (width, height) = (viewport.width as f32, viewport.height as f32);
    for text in &ledger.texts {
        out.coverage.texts += 1;
        if text.clipped_without_ellipsis() {
            out.lints.push(Lint {
                rule: Rule::Clip,
                key: text.key.clone(),
                detail: format!(
                    "`{}` needs {:.1} px ({:?}; widest word {:.1} px) in a {:.1} px box, no ellipsis",
                    text.content,
                    text.natural_width,
                    text.overflow,
                    text.min_width,
                    text.bounds.width
                ),
            });
        } else if text.clipped_vertically() {
            out.lints.push(Lint {
                rule: Rule::Clip,
                key: text.key.clone(),
                detail: format!(
                    "`{}` sits in a {:.1} px tall box; one line is {:.1} px",
                    text.content, text.bounds.height, text.line_height
                ),
            });
        }
        // Contrast from the pixels.
        let visible = text.bounds.x < width
            && text.bounds.y < height
            && text.bounds.x + text.bounds.width > 0.0
            && text.bounds.y + text.bounds.height > 0.0;
        let measured = (viewport.scale == 2 && visible)
            .then(|| {
                ink_contrast(
                    image,
                    viewport.scale,
                    text.bounds.x,
                    text.bounds.y,
                    text.bounds.width,
                    text.bounds.height,
                )
            })
            .flatten();
        match measured {
            Some((ratio, ink, ground)) => {
                out.coverage.contrast += 1;
                if out
                    .lowest_contrast
                    .as_ref()
                    .is_none_or(|(_, lowest)| ratio < *lowest)
                {
                    out.lowest_contrast = Some((text.key.clone(), ratio));
                }
                let large = text.size >= 24.0 || (text.size >= 18.66 && text.weight >= 700.0);
                let threshold = if large { 3.0 } else { 4.5 };
                if ratio < threshold {
                    out.lints.push(Lint {
                        rule: Rule::Contrast,
                        key: text.key.clone(),
                        detail: format!(
                            "`{}` {:.2}:1 (ink #{:02x}{:02x}{:02x} on #{:02x}{:02x}{:02x}); needs {threshold}:1",
                            text.content, ratio, ink[0], ink[1], ink[2], ground[0], ground[1], ground[2]
                        ),
                    });
                }
            }
            None => out.coverage.contrast_skipped += 1,
        }
    }
    // Overlap between texts of one region.
    for (index, a) in ledger.texts.iter().enumerate() {
        for b in &ledger.texts[index + 1..] {
            if a.region != b.region || a.key == b.key {
                continue;
            }
            let (w, h) = intersection(&a.bounds, &b.bounds);
            if w > 1.0 && h > 1.0 {
                out.lints.push(Lint {
                    rule: Rule::Overlap,
                    key: format!("{} + {}", a.key, b.key),
                    detail: format!(
                        "`{}` and `{}` overlap by {w:.1} x {h:.1} px",
                        a.content, b.content
                    ),
                });
            }
        }
    }
    for target in &ledger.targets {
        out.coverage.targets += 1;
        let b = &target.bounds;
        if target.state.focusable && !b.within(width + 0.5, height + 0.5) {
            out.lints.push(Lint {
                rule: Rule::Offscreen,
                key: target.key.clone(),
                detail: format!(
                    "focusable at ({:.1}, {:.1}) {:.1}x{:.1} is not inside the {width:.0}x{height:.0} viewport",
                    b.x, b.y, b.width, b.height
                ),
            });
        }
        if target.state.clickable && (b.width < 24.0 || b.height < 24.0) {
            out.lints.push(Lint {
                rule: Rule::Target,
                key: target.key.clone(),
                detail: format!(
                    "hit target {:.1}x{:.1} px is under 24x24",
                    b.width, b.height
                ),
            });
        }
    }
    out
}

/// A lint result as JSON.
#[must_use]
#[allow(clippy::cast_precision_loss)]
pub fn json(linted: &Linted) -> Json {
    Json::obj([
        (
            "coverage",
            Json::obj([
                ("texts", Json::num(linted.coverage.texts as f64)),
                ("targets", Json::num(linted.coverage.targets as f64)),
                ("contrast", Json::num(linted.coverage.contrast as f64)),
                (
                    "contrast_skipped",
                    Json::num(linted.coverage.contrast_skipped as f64),
                ),
            ]),
        ),
        (
            "lowest_contrast",
            linted
                .lowest_contrast
                .as_ref()
                .map_or(Json::Null, |(key, ratio)| {
                    Json::obj([
                        ("key", Json::str(key.clone())),
                        ("ratio", Json::num(f64::from(*ratio))),
                    ])
                }),
        ),
        (
            "lints",
            Json::Arr(
                linted
                    .lints
                    .iter()
                    .map(|lint| {
                        Json::obj([
                            ("rule", Json::str(lint.rule.name())),
                            ("key", Json::str(lint.key.clone())),
                            ("detail", Json::str(lint.detail.clone())),
                        ])
                    })
                    .collect(),
            ),
        ),
    ])
}

#[cfg(test)]
mod tests {
    use super::ink_contrast;
    use image::{Rgba, RgbaImage};

    #[test]
    fn contrast_comes_from_the_painted_ink_not_the_declared_colour() {
        // A 20x10 logical box at 2x: dark ground, a 2 px stroke of ink.
        let mut image = RgbaImage::from_pixel(40, 20, Rgba([10, 14, 24, 255]));
        for y in 4..16 {
            for x in 10..12 {
                image.put_pixel(x, y, Rgba([200, 210, 230, 255]));
            }
            // Antialiased fringe that must not win.
            image.put_pixel(12, y, Rgba([90, 95, 110, 255]));
        }
        let (ratio, ink, ground) = ink_contrast(&image, 2, 0.0, 0.0, 20.0, 10.0).expect("measured");
        assert_eq!(ground, [10, 14, 24]);
        assert_eq!(ink, [200, 210, 230]);
        assert!(ratio > 10.0, "{ratio}");
        // Faint ink on the same ground reads low.
        let mut faint = RgbaImage::from_pixel(40, 20, Rgba([10, 14, 24, 255]));
        for y in 4..16 {
            faint.put_pixel(10, y, Rgba([40, 46, 60, 255]));
        }
        let (ratio, ..) = ink_contrast(&faint, 2, 0.0, 0.0, 20.0, 10.0).expect("measured");
        assert!(ratio < 2.0, "{ratio}");
        // One colour: nothing painted, nothing measured.
        let blank = RgbaImage::from_pixel(40, 20, Rgba([10, 14, 24, 255]));
        assert!(ink_contrast(&blank, 2, 0.0, 0.0, 20.0, 10.0).is_none());
    }
}
