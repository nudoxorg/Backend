//! Layout lints on a painted frame: the probe's text boxes and targets plus
//! the pixels that were actually drawn.
//!
//! | Rule | Fails when |
//! |---|---|
//! | `clip` | a text box is narrower than its text (clip overflow) or than its widest word (wrap overflow) and draws no ellipsis, or shorter than one line |
//! | `overlap` | two texts of one [`crate::probe::region`] overlap by more than 1 px in both axes |
//! | `offscreen` | a focusable element is not entirely inside the viewport, or a text lies wholly past its left or right edge with no scroll container reaching it |
//! | `target` | a clickable element is smaller than 24 x 24 px |
//! | `contrast` | the ink painted in a text box against the ground painted around it is under 4.5:1 (3:1 for large text: 24 px, or 18.66 px at weight 700) |
//!
//! Text and targets under a modal veil (an open dialog's scrim, `probe::veil`) are not judged:
//! the page beneath a dialog is not readable, the dialog's own text is what the reader sees.
//!
//! Contrast is measured from the frame's pixels: the ground is the box's
//! most common colour, the ink the pixel that contrasts with it most. Glyph
//! stems reach full coverage at 2x, so contrast is only measured at 2x; a 1x
//! capture reports it as not measured rather than guessing.

use super::json::Json;
use crate::probe::Ledger;
pub use crate::probe::rules::{fully_visible, visible_bounds};
use crate::probe::rules::{overlap, stranded};
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
    /// Text rectangles fully hidden by the actual native paint mask/window.
    pub hidden_texts: usize,
    /// Texts under a modal veil (the page beneath an open dialog): not
    /// judged, the dialog's own text is.
    pub occluded_texts: usize,
    /// Targets under a modal veil: not judged.
    pub occluded_targets: usize,
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
    let (ground, _) = counts
        .iter()
        .max_by_key(|(colour, count)| (**count, **colour))?;
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
    let under_veil: Vec<bool> = ledger
        .texts
        .iter()
        .enumerate()
        .map(|(index, text)| {
            ledger
                .veils
                .iter()
                .any(|veil| veil.covers_text(index, &text.bounds))
        })
        .collect();
    for (index, text) in ledger.texts.iter().enumerate() {
        if under_veil[index] {
            out.coverage.occluded_texts += 1;
            continue;
        }
        out.coverage.texts += 1;
        if let Some(side) = stranded(text, &ledger.scrolls, width) {
            out.lints.push(Lint {
                rule: Rule::Offscreen,
                key: text.key.clone(),
                detail: format!(
                    "`{}` at ({:.1}, {:.1}) {:.1}x{:.1} lies wholly past the {} edge of the {width:.0} px viewport, \
                     and no scroll container's content reaches it",
                    text.content, text.bounds.x, text.bounds.y, text.bounds.width, text.bounds.height, side.name()
                ),
            });
        }
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
        let visible = visible_bounds(text, width, height);
        if visible.is_none() {
            out.coverage.hidden_texts += 1;
        }
        let measured = (viewport.scale == 2)
            .then(|| visible.as_ref())
            .flatten()
            .and_then(|bounds| {
                ink_contrast(
                    image,
                    viewport.scale,
                    bounds.x,
                    bounds.y,
                    bounds.width,
                    bounds.height,
                )
            });
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
        if under_veil[index] {
            continue;
        }
        for (offset, b) in ledger.texts[index + 1..].iter().enumerate() {
            if under_veil[index + 1 + offset] || a.region != b.region || a.key == b.key {
                continue;
            }
            let (Some(a_visible), Some(b_visible)) = (
                visible_bounds(a, width, height),
                visible_bounds(b, width, height),
            ) else {
                continue;
            };
            let (w, h) = overlap(&a_visible, &b_visible);
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
    for (index, target) in ledger.targets.iter().enumerate() {
        if ledger
            .veils
            .iter()
            .any(|veil| veil.covers_target(index, &target.bounds))
        {
            out.coverage.occluded_targets += 1;
            continue;
        }
        out.coverage.targets += 1;
        let b = &target.bounds;
        let reachable_by_scroll = ledger.scrolls.iter().any(|scroll| scroll.reaches(b));
        if target.state.focusable && !b.within(width + 0.5, height + 0.5) && !reachable_by_scroll {
            out.lints.push(Lint {
                rule: Rule::Offscreen,
                key: target.key.clone(),
                detail: format!(
                    "focusable at ({:.1}, {:.1}) {:.1}x{:.1} is not inside the {width:.0}x{height:.0} \
                     viewport, and no scroll container's content reaches it",
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
                (
                    "hidden_texts",
                    Json::num(linted.coverage.hidden_texts as f64),
                ),
                (
                    "occluded_texts",
                    Json::num(linted.coverage.occluded_texts as f64),
                ),
                (
                    "occluded_targets",
                    Json::num(linted.coverage.occluded_targets as f64),
                ),
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
    use super::{fully_visible, ink_contrast, lint};
    use crate::probe::{BoundsSample, Ledger, ScrollSample, Target, TargetSample};
    use backend_gui_harness::Viewport;
    use image::{Rgba, RgbaImage};

    fn blank(width: u32, height: u32) -> RgbaImage {
        RgbaImage::from_pixel(width, height, Rgba([10, 14, 24, 255]))
    }

    fn viewport() -> Viewport {
        Viewport {
            width: 400,
            height: 300,
            scale: 1,
        }
    }

    fn bounds(x: f32, y: f32, width: f32, height: f32) -> BoundsSample {
        BoundsSample {
            key: "b".to_owned(),
            x,
            y,
            width,
            height,
        }
    }

    fn focusable_at(key: &str, bounds_sample: BoundsSample) -> TargetSample {
        TargetSample {
            key: key.to_owned(),
            bounds: bounds_sample,
            state: Target {
                focusable: true,
                ..Target::default()
            },
        }
    }

    fn text_at(
        key: &str,
        rect: BoundsSample,
        clip: Option<BoundsSample>,
    ) -> crate::probe::TextSample {
        crate::probe::TextSample {
            key: key.to_owned(),
            bounds: rect,
            paint_clip: clip,
            natural_width: 20.0,
            overflow: crate::probe::TextOverflow::Clip,
            content: "label".to_owned(),
            min_width: 20.0,
            line_height: 10.0,
            size: 12.0,
            weight: 400.0,
            region: Some("graph".to_owned()),
        }
    }

    /// A dialog over a page: the page beneath is under the scrim and is not judged (its clipped
    /// text and its tiny target would otherwise fail lint on every dialog scene); the dialog's
    /// own text and target are.
    #[test]
    fn lint_judges_only_what_is_above_a_modal_veil() {
        use crate::probe::VeilSample;
        let mut under = text_at("page-clipped", bounds(20.0, 20.0, 30.0, 14.0), None);
        under.natural_width = 200.0;
        let mut above = text_at("dialog-clipped", bounds(120.0, 120.0, 30.0, 14.0), None);
        above.natural_width = 200.0;
        let target = |key: &str, rect: BoundsSample| TargetSample {
            key: key.to_owned(),
            bounds: rect,
            state: Target {
                clickable: true,
                ..Target::default()
            },
        };
        let ledger = Ledger {
            texts: vec![under, above],
            targets: vec![
                target("page-tiny", bounds(20.0, 60.0, 12.0, 12.0)),
                target("dialog-tiny", bounds(120.0, 160.0, 12.0, 12.0)),
            ],
            veils: vec![VeilSample {
                bounds: bounds(0.0, 0.0, 400.0, 300.0),
                texts: 1,
                targets: 1,
            }],
            ..Ledger::default()
        };
        let result = lint(&blank(400, 300), &ledger, viewport());
        assert_eq!(
            (
                result.coverage.occluded_texts,
                result.coverage.occluded_targets
            ),
            (1, 1)
        );
        assert_eq!(
            (result.coverage.texts, result.coverage.targets),
            (1, 1),
            "only the dialog is judged"
        );
        let keys: Vec<&str> = result.lints.iter().map(|lint| lint.key.as_str()).collect();
        assert!(
            !keys.iter().any(|key| key.starts_with("page-")),
            "the page under the scrim is not judged: {keys:?}"
        );
        assert!(
            keys.contains(&"dialog-clipped") && keys.contains(&"dialog-tiny"),
            "the dialog is: {keys:?}"
        );
    }

    #[test]
    fn exact_text_visibility_handles_fractional_edges_and_clips() {
        let intrinsic = bounds(10.1, 5.2, 30.2, 12.3);
        let fully_contained = text_at(
            "fractional",
            intrinsic.clone(),
            Some(bounds(0.1, 0.2, 40.3, 20.0)),
        );
        assert!(fully_visible(&fully_contained, 100.0, 100.0));

        let clipped = text_at("clipped", intrinsic, Some(bounds(0.1, 0.2, 40.0, 20.0)));
        assert!(!fully_visible(&clipped, 100.0, 100.0));
    }

    #[test]
    fn native_hidden_text_neither_overlaps_a_card_nor_samples_its_background() {
        let ledger = Ledger {
            texts: vec![
                text_at(
                    "hidden",
                    bounds(10.0, 100.0, 40.0, 20.0),
                    Some(bounds(0.0, 0.0, 80.0, 50.0)),
                ),
                text_at("card", bounds(10.0, 100.0, 40.0, 20.0), None),
            ],
            ..Ledger::default()
        };
        let result = lint(
            &blank(800, 600),
            &ledger,
            Viewport {
                scale: 2,
                ..viewport()
            },
        );
        assert_eq!(result.coverage.texts, 2);
        assert_eq!(result.coverage.hidden_texts, 1);
        assert!(
            !result.lints.iter().any(|lint| lint.key.contains("hidden")),
            "{:?}",
            result.lints
        );
    }

    #[test]
    fn native_partial_text_still_checks_visible_low_contrast_ink() {
        let ledger = Ledger {
            texts: vec![text_at(
                "partial",
                bounds(10.0, 10.0, 40.0, 20.0),
                Some(bounds(0.0, 0.0, 80.0, 20.0)),
            )],
            ..Ledger::default()
        };
        let mut image = blank(800, 600);
        image.put_pixel(24, 26, Rgba([25, 30, 40, 255]));
        image.put_pixel(24, 50, Rgba([255, 255, 255, 255])); // outside the actual clip
        let result = lint(
            &image,
            &ledger,
            Viewport {
                scale: 2,
                ..viewport()
            },
        );
        assert_eq!(result.coverage.hidden_texts, 0);
        assert_eq!(result.coverage.contrast, 1);
        assert!(
            result
                .lints
                .iter()
                .any(|lint| lint.rule == super::Rule::Contrast && lint.key == "partial"),
            "{:?}",
            result.lints
        );
    }

    #[test]
    fn native_visible_text_overlap_remains_a_failure() {
        let ledger = Ledger {
            texts: vec![
                text_at(
                    "a",
                    bounds(10.0, 10.0, 40.0, 20.0),
                    Some(bounds(0.0, 0.0, 35.0, 25.0)),
                ),
                text_at("b", bounds(20.0, 12.0, 40.0, 20.0), None),
            ],
            ..Ledger::default()
        };
        let result = lint(&blank(400, 300), &ledger, viewport());
        assert!(
            result
                .lints
                .iter()
                .any(|lint| lint.rule == super::Rule::Overlap && lint.key == "a + b"),
            "{:?}",
            result.lints
        );
    }

    /// A row 40 px below a 300 px viewport is offscreen when nothing
    /// declares a scroll container over it: the coordinator's "fixed row
    /// pushed below the window" canary. Guards against a regression that
    /// exempts everything (dropping the scroll check entirely would make
    /// this canary wrongly pass — see the mutation-proof command in
    /// CHECKPOINT-3).
    #[test]
    fn a_focusable_below_the_viewport_with_no_scroll_container_is_offscreen() {
        let ledger = Ledger {
            targets: vec![focusable_at("row", bounds(10.0, 340.0, 100.0, 24.0))],
            ..Ledger::default()
        };
        let linted = lint(&blank(400, 300), &ledger, viewport());
        assert!(
            linted
                .lints
                .iter()
                .any(|item| item.rule.name() == "offscreen"),
            "a fixed row below the window with no scroll container must still lint offscreen: {:?}",
            linted.lints
        );
    }

    /// The same row, but now a scroll container whose content extends down
    /// to cover it (viewport 400x300, content 400x500) declares it reachable:
    /// the coordinator's "long scrolling list" canary must pass.
    #[test]
    fn a_focusable_below_the_viewport_inside_a_scroll_containers_content_is_not_offscreen() {
        let ledger = Ledger {
            targets: vec![focusable_at("row", bounds(10.0, 340.0, 100.0, 24.0))],
            scrolls: vec![ScrollSample {
                key: "list".to_owned(),
                viewport: bounds(0.0, 0.0, 400.0, 300.0),
                content: bounds(0.0, 0.0, 400.0, 500.0),
            }],
            ..Ledger::default()
        };
        let linted = lint(&blank(400, 300), &ledger, viewport());
        assert!(
            !linted
                .lints
                .iter()
                .any(|item| item.rule.name() == "offscreen"),
            "a row inside a scroll container's content extent is reachable, not offscreen: {:?}",
            linted.lints
        );
    }

    /// A scroll container whose content does not actually exceed its
    /// viewport on either axis scrolls nothing, so it cannot make an
    /// otherwise-offscreen row reachable — the seam only exempts real
    /// overflow, not any nearby `ScrollSample`.
    #[test]
    fn a_scroll_sample_that_does_not_actually_overflow_exempts_nothing() {
        let ledger = Ledger {
            targets: vec![focusable_at("row", bounds(10.0, 340.0, 100.0, 24.0))],
            scrolls: vec![ScrollSample {
                key: "list".to_owned(),
                viewport: bounds(0.0, 0.0, 400.0, 300.0),
                content: bounds(0.0, 0.0, 400.0, 300.0),
            }],
            ..Ledger::default()
        };
        let linted = lint(&blank(400, 300), &ledger, viewport());
        assert!(
            linted
                .lints
                .iter()
                .any(|item| item.rule.name() == "offscreen"),
            "a container whose content matches its viewport does not scroll: {:?}",
            linted.lints
        );
    }

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

    /// A strip laid out past the right edge has no visible part, so the other
    /// rules have nothing to measure and lint it clean. It is an `offscreen`
    /// finding unless a scroll container reaches it.
    #[test]
    fn a_text_wholly_past_the_edge_is_an_offscreen_finding_unless_a_scroller_reaches_it() {
        use crate::probe::{ScrollSample, TextOverflow, TextSample};
        let strip = |x: f32| TextSample {
            key: "strip-word".to_owned(),
            bounds: bounds(x, 40.0, 90.0, 20.0),
            paint_clip: None,
            natural_width: 90.0,
            overflow: TextOverflow::Wrap,
            content: "as_integer".to_owned(),
            min_width: 90.0,
            line_height: 20.0,
            size: 14.0,
            weight: 400.0,
            region: None,
        };
        let offscreen = |ledger: &Ledger| {
            lint(&blank(400, 300), ledger, viewport())
                .lints
                .iter()
                .filter(|lint| lint.rule == super::Rule::Offscreen && lint.key == "strip-word")
                .count()
        };
        // Wholly right of the 400 px viewport: a finding, in words.
        let stranded = Ledger {
            texts: vec![strip(420.0)],
            ..Ledger::default()
        };
        let found = lint(&blank(400, 300), &stranded, viewport());
        let finding = found
            .lints
            .iter()
            .find(|lint| lint.key == "strip-word")
            .expect("a stranded text is a finding");
        assert!(
            finding.detail.contains("wholly past the right edge"),
            "{}",
            finding.detail
        );
        // Inside the window, or reached by a scroller, it is not.
        assert_eq!(
            offscreen(&Ledger {
                texts: vec![strip(100.0)],
                ..Ledger::default()
            }),
            0
        );
        let scroller = ScrollSample {
            key: "strip".to_owned(),
            viewport: bounds(0.0, 0.0, 400.0, 60.0),
            content: bounds(0.0, 0.0, 900.0, 60.0),
        };
        assert_eq!(
            offscreen(&Ledger {
                texts: vec![strip(420.0)],
                scrolls: vec![scroller],
                ..Ledger::default()
            }),
            0
        );
    }
}
