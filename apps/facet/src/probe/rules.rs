//! The layout rules that read only the probe's ledger: which part of a text
//! is on screen, whether two texts sit on each other, whether a text is
//! stranded past a side of the window.
//!
//! The harness lint (`gallery::lint`, which adds the painted pixels) and the
//! shell's rig tests (`apps/desktop/src/shell/fit_tests.rs`) both call these,
//! so a rule is written once and both read the same frame the same way.

use super::{BoundsSample, ScrollSample, TextSample};

/// The part of a text box that is actually on screen: inside the `width` x
/// `height` window and inside the clip its ancestors set. `None` when none
/// of it is.
///
/// Native visibility affects pixel and overlap checks, never intrinsic
/// layout metrics (those stay on [`TextSample`]).
#[must_use]
pub fn visible_bounds(text: &TextSample, width: f32, height: f32) -> Option<BoundsSample> {
    // Missing renderer evidence is not a viewport-sized clip. Treating it as
    // one lets a geometric fixture or uninstrumented element prove visibility.
    let clip = text.paint_clip.as_ref()?;
    clipped_bounds(&text.bounds, clip, width, height)
}

/// Intersect a box with its actual renderer clip and the window viewport.
#[must_use]
pub fn clipped_bounds(
    bounds: &BoundsSample,
    clip: &BoundsSample,
    width: f32,
    height: f32,
) -> Option<BoundsSample> {
    let x = bounds.x.max(clip.x).max(0.0);
    let y = bounds.y.max(clip.y).max(0.0);
    let right = (bounds.x + bounds.width).min(clip.x + clip.width).min(width);
    let bottom = (bounds.y + bounds.height).min(clip.y + clip.height).min(height);
    (right > x && bottom > y).then(|| BoundsSample { key: bounds.key.clone(), x, y, width: right - x, height: bottom - y })
}

/// Whether the renderer left a substantial readable portion of this box.
/// A one-pixel sliver, or a thin edge strip, cannot satisfy visual coverage.
#[must_use]
pub fn meaningfully_visible(bounds: &BoundsSample, visible: &BoundsSample) -> bool {
    const MIN_AXIS_FRACTION: f32 = 0.8;
    bounds.width > 0.0
        && bounds.height > 0.0
        && visible.width / bounds.width >= MIN_AXIS_FRACTION
        && visible.height / bounds.height >= MIN_AXIS_FRACTION
}

/// Whether every point of a text's intrinsic box is inside its paint clip
/// and the window. Use this before an exact-string visibility assertion:
/// [`visible_bounds`] alone also reports partially clipped text.
#[must_use]
pub fn fully_visible(text: &TextSample, width: f32, height: f32) -> bool {
    let bounds = &text.bounds;
    if bounds.x < 0.0 || bounds.y < 0.0 || bounds.x + bounds.width > width || bounds.y + bounds.height > height {
        return false;
    }
    text.paint_clip.as_ref().is_some_and(|clip| {
        bounds.x >= clip.x
            && bounds.y >= clip.y
            && bounds.x + bounds.width <= clip.x + clip.width
            && bounds.y + bounds.height <= clip.y + clip.height
    })
}

/// The size of the intersection of two boxes (negative when they do not meet
/// on that axis).
#[must_use]
pub fn overlap(a: &BoundsSample, b: &BoundsSample) -> (f32, f32) {
    let width = (a.x + a.width).min(b.x + b.width) - a.x.max(b.x);
    let height = (a.y + a.height).min(b.y + b.height) - a.y.max(b.y);
    (width, height)
}

/// A side of the window.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Side {
    /// Past the left edge.
    Left,
    /// Past the right edge.
    Right,
}

impl Side {
    /// The side's word, for reports.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Left => "left",
            Self::Right => "right",
        }
    }
}

/// The side of the `width` px window a text lies WHOLLY past, when no scroll
/// container's content reaches it: words laid out where nothing can show
/// them (a strip 200 px beyond the right edge lints clean under the other
/// rules, because a text that is not on screen has no visible part to
/// measure). Only the horizontal axis: text below the fold is a page that
/// scrolls. Blank text and zero-width boxes are not text laid out anywhere.
#[must_use]
pub fn stranded(text: &TextSample, scrolls: &[ScrollSample], width: f32) -> Option<Side> {
    let bounds = &text.bounds;
    if text.content.trim().is_empty() || bounds.width <= 0.5 {
        return None;
    }
    let side = if bounds.x >= width - 0.5 {
        Side::Right
    } else if bounds.x + bounds.width <= 0.5 {
        Side::Left
    } else {
        return None;
    };
    (!scrolls
        .iter()
        .any(|scroll| scroll.reaches(bounds, &text.scroll_ancestors)))
    .then_some(side)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::probe::TextOverflow;

    fn bounds(x: f32, y: f32, width: f32, height: f32) -> BoundsSample {
        BoundsSample { key: "box".to_owned(), x, y, width, height }
    }

    fn text(content: &str, x: f32, y: f32, width: f32, height: f32) -> TextSample {
        TextSample {
            key: format!("text:{content}"),
            bounds: bounds(x, y, width, height),
            paint_clip: None,
            scroll_ancestors: Vec::new(),
            natural_width: width,
            overflow: TextOverflow::Wrap,
            content: content.to_owned(),
            min_width: width,
            line_height: height,
            size: 14.0,
            weight: 400.0,
            region: None,
        }
    }

    #[test]
    fn a_text_cut_by_the_window_shows_only_the_part_inside_it() {
        let cut = text("wide words", 300.0, 10.0, 100.0, 20.0);
        let mut cut = cut;
        cut.paint_clip = Some(bounds(0.0, 0.0, 360.0, 640.0));
        let seen = visible_bounds(&cut, 360.0, 640.0).expect("the left 60 px are on screen");
        assert_eq!((seen.x, seen.y, seen.width, seen.height), (300.0, 10.0, 60.0, 20.0));
        assert!(!fully_visible(&cut, 360.0, 640.0));
        let mut inside = text("inside", 10.0, 10.0, 100.0, 20.0);
        inside.paint_clip = Some(bounds(0.0, 0.0, 360.0, 640.0));
        assert!(fully_visible(&inside, 360.0, 640.0));
    }

    #[test]
    fn a_text_under_its_ancestors_clip_shows_only_what_the_clip_lets_through() {
        let mut clipped = text("under a clip", 0.0, 0.0, 200.0, 20.0);
        clipped.paint_clip = Some(bounds(0.0, 0.0, 50.0, 20.0));
        let seen = visible_bounds(&clipped, 360.0, 640.0).expect("50 px inside the clip");
        assert_eq!(seen.width, 50.0);
        clipped.paint_clip = Some(bounds(300.0, 0.0, 50.0, 20.0));
        assert!(visible_bounds(&clipped, 360.0, 640.0).is_none(), "the clip and the box do not meet");
    }

    #[test]
    fn a_text_wholly_past_a_side_is_stranded_and_a_text_cut_by_it_is_not() {
        let scrolls: [ScrollSample; 0] = [];
        // Wholly right of a 360 px window: nothing shows it, so `visible_bounds` has nothing to measure.
        let strip = text("as_integer", 400.0, 10.0, 90.0, 20.0);
        assert!(visible_bounds(&strip, 360.0, 640.0).is_none());
        assert_eq!(stranded(&strip, &scrolls, 360.0), Some(Side::Right));
        assert_eq!(stranded(&text("left", -120.0, 10.0, 90.0, 20.0), &scrolls, 360.0), Some(Side::Left));
        // Cut by the edge (part of it shows) is `clip`/`edge`, not stranded.
        assert_eq!(stranded(&text("cut", 300.0, 10.0, 90.0, 20.0), &scrolls, 360.0), None);
        // Below the fold is a page that scrolls; blank text and zero-width boxes are not text.
        assert_eq!(stranded(&text("below", 10.0, 2000.0, 90.0, 20.0), &scrolls, 360.0), None);
        assert_eq!(stranded(&text("   ", 400.0, 10.0, 90.0, 20.0), &scrolls, 360.0), None);
        assert_eq!(stranded(&text("empty", 400.0, 10.0, 0.0, 20.0), &scrolls, 360.0), None);
    }

    #[test]
    fn a_text_a_scroll_container_reaches_is_not_stranded() {
        let strip = text("as_integer", 400.0, 10.0, 90.0, 20.0);
        let scroller = ScrollSample {
            key: "strip".to_owned(),
            viewport: bounds(0.0, 0.0, 360.0, 40.0),
            content: bounds(0.0, 0.0, 900.0, 40.0),
            ancestors: Vec::new(),
        };
        let mut strip = strip;
        strip.scroll_ancestors.push("strip".to_owned());
        assert_eq!(stranded(&strip, &[scroller], 360.0), None);
    }

    #[test]
    fn scroll_reach_requires_the_measured_container_in_the_actual_ancestor_chain() {
        let mut sample = text("row", 10.0, 340.0, 80.0, 20.0);
        let scroller = ScrollSample {
            key: "owned-scroll".to_owned(),
            viewport: bounds(0.0, 0.0, 400.0, 300.0),
            content: bounds(0.0, 0.0, 400.0, 500.0),
            ancestors: Vec::new(),
        };
        assert!(!scroller.reaches(&sample.bounds, &sample.scroll_ancestors));
        sample.scroll_ancestors.push("owned-scroll".to_owned());
        assert!(scroller.reaches(&sample.bounds, &sample.scroll_ancestors));

        let unrelated = ScrollSample { key: "other-scroll".to_owned(), ..scroller.clone() };
        assert!(!unrelated.reaches(&sample.bounds, &sample.scroll_ancestors));

        let nested = ScrollSample { ancestors: vec!["outer-scroll".to_owned()], ..scroller };
        assert!(!nested.reaches(&sample.bounds, &sample.scroll_ancestors));
        sample.scroll_ancestors = vec!["outer-scroll".to_owned(), "owned-scroll".to_owned()];
        assert!(nested.reaches(&sample.bounds, &sample.scroll_ancestors));
    }

    #[test]
    fn two_boxes_meet_by_the_size_of_their_intersection() {
        assert_eq!(overlap(&bounds(0.0, 0.0, 100.0, 20.0), &bounds(60.0, 5.0, 100.0, 20.0)), (40.0, 15.0));
        let (w, _) = overlap(&bounds(0.0, 0.0, 10.0, 20.0), &bounds(60.0, 0.0, 10.0, 20.0));
        assert!(w < 0.0, "apart on x");
    }
}
