//! Charts built out of the same boxes everything else is built out of.
//! One hue, one lightness, no gridlines, no legend, and the ends labelled.
//! A series is a slice of counts; nothing here knows what it is counting.
//!
//! The design decision this module embodies is that a chart in a reading
//! surface is a sentence, not an instrument panel. A reader looking at a
//! package wants one answer — is this thing used, and is that rising or
//! falling — and that answer is carried entirely by the silhouette of the
//! series. Gridlines, a second hue, a legend for one series, and an axis with
//! nine ticks all add ink without adding that answer, so none of them are
//! here: the ends of the range are labelled, the peak is stated in words
//! beside the chart, and everything between is shape.
//!
//! Every height is computed in integer arithmetic against the tallest column,
//! which is why the series is taken as `u64` counts and the box heights as
//! `u16` pixels: there is no floating-point scaling step to disagree with
//! itself between two frames, and a column can never round to zero and vanish.
//! A chart is drawn in one hue placed on the chromatic plane, so it reads at
//! the same strength on the Abyss ground and on Glacier.

use crate::theme::Theme;
use crate::theme::palette::Paint;
use crate::theme::ramp::Hue;
use crate::theme::tokens::{Space, TypeScale, hairline, space, type_size};
use gpui::{Div, ParentElement, Styled, div, px};

/// Smallest height a column is drawn at, so a quiet week is still visible.
const FLOOR: u16 = 1;

/// Returns a bare sparkline: one column per count, no axis and no labels.
///
/// This is the form a dense row or a card uses, where the chart has to fit in
/// the space a number would have taken. It states the shape and nothing else.
pub(crate) fn sparkline(theme: &Theme, hue: Hue, counts: &[u64], height: u16) -> Div {
    columns(theme, hue, counts, height).w_full()
}

/// Returns a labelled chart: the columns, a baseline, and the range ends.
///
/// The labels are the first and last step of the series, which is the only
/// axis a series of equal steps needs. The peak belongs in the sentence beside
/// the chart rather than on it, because a number floating over a column is
/// read as belonging to that column.
pub(crate) fn histogram(
    theme: &Theme,
    hue: Hue,
    counts: &[u64],
    height: u16,
    ends: (&str, &str),
) -> Div {
    div()
        .w_full()
        .flex()
        .flex_col()
        .gap(space(Space::Tight))
        .child(columns(theme, hue, counts, height).w_full())
        .child(div().w_full().h(hairline()).bg(theme.paint(Paint::Rule1)))
        .child(axis(theme, ends))
}

/// Returns the two end labels of a range, pushed to the edges.
fn axis(theme: &Theme, ends: (&str, &str)) -> Div {
    let (start, end) = ends;
    div()
        .w_full()
        .flex()
        .items_center()
        .justify_between()
        .child(super::text::faint(theme).child(start.to_owned()))
        .child(super::text::faint(theme).child(end.to_owned()))
}

/// Returns the columns of one series, scaled against its own tallest count.
fn columns(theme: &Theme, hue: Hue, counts: &[u64], height: u16) -> Div {
    let peak = counts.iter().copied().max().unwrap_or(0).max(1);
    let ink = theme.on_plane(hue);
    div()
        .flex()
        .items_end()
        .gap(px(1.0))
        .h(px(f32::from(height)))
        .children(counts.iter().map(|count| {
            div()
                .flex_1()
                .min_w(px(1.0))
                .h(px(f32::from(column(*count, peak, height))))
                .bg(ink)
        }))
}

/// Returns the pixel height one count occupies against the series peak.
///
/// A count of zero still draws one pixel: a week nobody downloaded the package
/// is a fact about that week, and a column that vanished would read as a gap
/// in the series rather than as a trough in it.
pub(crate) fn column(count: u64, peak: u64, height: u16) -> u16 {
    let tall = count.saturating_mul(u64::from(height)) / peak.max(1);
    u16::try_from(tall).unwrap_or(height).clamp(FLOOR, height)
}

/// Returns a one-hue meter stating a part of a whole.
///
/// The filled portion is laid out by flex growth rather than by a percentage
/// width, so the two parts always add up to the track exactly and a rounding
/// step can never leave a one-pixel seam at the join.
pub(crate) fn meter(theme: &Theme, hue: Hue, part: u64, whole: u64) -> Div {
    let percent = share(part, whole);
    div()
        .w_full()
        .h(px(6.0))
        .flex()
        .overflow_hidden()
        .bg(theme.plane_wash(hue, 0.14))
        .child(
            div()
                .flex_grow(f32::from(percent))
                .h_full()
                .bg(theme.on_plane(hue)),
        )
        .child(div().flex_grow(f32::from(100_u16.saturating_sub(percent))))
}

/// Returns the whole-percent share one part is of one whole.
pub(crate) fn share(part: u64, whole: u64) -> u16 {
    let percent = part.saturating_mul(100) / whole.max(1);
    u16::try_from(percent).unwrap_or(100).min(100)
}

/// Returns the faint tag a sampled chart carries, in the one place it belongs.
///
/// A chart drawn from sample values wears its label on the chart rather than
/// only in the section head, because a figure a reader screenshots leaves the
/// section head behind.
pub(crate) fn sample_tag(theme: &Theme) -> Div {
    div()
        .flex_none()
        .px(px(4.0))
        .py(px(1.0))
        .bg(theme.paint(Paint::Tint))
        .text_size(type_size(TypeScale::Micro))
        .text_color(theme.paint(Paint::Silver3))
        .child("sample")
}
