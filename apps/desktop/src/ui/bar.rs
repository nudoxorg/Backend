//! The language bar: one project's mix as a proportion, and its progress.
//! Rested, it is three pixels of colour; under the pointer it grows and names
//! every language with its logo and count. While a project indexes, the same
//! bar is the progress indicator, so a reader never learns two instruments.
//!
//! The counts are never written beside the bar. A proportion is a length, and
//! a reader who wants the numbers hovers — which is also where the logos live,
//! because a logo is a fine thing to see once and a poor thing to see on
//! every row.

use crate::presentation::project::Standing;
use crate::theme::Theme;
use crate::theme::language::{hue as language_hue, label as language_label};
use crate::theme::palette::Paint;
use crate::theme::tokens::{Space, TypeScale, space, type_size};
use crate::ui::icon::{self, Logo};
use backend_present::{Language, LanguageCount};
use gpui::AppContext as _;
use gpui::prelude::FluentBuilder as _;
use gpui::{
    Animation, AnimationExt as _, AnyElement, AnyView, App, Div, ElementId, FontWeight,
    InteractiveElement, IntoElement, ParentElement, SharedString, StatefulInteractiveElement,
    Styled, Window, div, px,
};
use std::time::Duration;

/// Height of the bar at rest.
const REST: f32 = 3.0;

/// Height of the bar under the pointer.
const GROWN: f32 = 7.0;

/// How long one indexing sweep takes.
const SWEEP: Duration = Duration::from_millis(1400);

/// What the bar is reporting besides the mix.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Motion {
    /// The project is readable; the bar is still.
    Still,
    /// Rows are arriving; a sweep travels the bar.
    Indexing,
    /// Accepted with no rows yet; the bar breathes.
    Waiting,
}

impl Motion {
    /// Returns the motion one shelf standing implies.
    pub(crate) const fn of(standing: Standing) -> Self {
        match standing {
            Standing::Readable | Standing::Empty | Standing::Failed => Self::Still,
            Standing::Indexing => Self::Indexing,
            Standing::Requested => Self::Waiting,
        }
    }
}

/// Returns the bar for one project, hover-grown and explained on hover.
///
/// `id` must be unique in its list; it keys the hover group and the sweep.
pub(crate) fn language_bar(
    theme: &Theme,
    id: impl Into<SharedString>,
    counts: &[LanguageCount],
    total: u64,
    motion: Motion,
) -> impl IntoElement {
    let id: SharedString = id.into();
    let group = SharedString::from(format!("bar-{id}"));
    let reduced = theme.reduced_motion();
    let tip = tooltip_rows(counts, total);
    let segments = segments(theme, counts, total, motion);
    div()
        .id(ElementId::Name(id.clone()))
        .group(group.clone())
        .w_full()
        .py(px(2.0))
        .cursor_default()
        .child(
            div()
                .w_full()
                .h(px(REST))
                .group_hover(group, |style| style.h(px(GROWN)))
                .rounded_full()
                .overflow_hidden()
                .relative()
                .bg(theme.paint(Paint::Hover))
                .child(
                    div()
                        .absolute()
                        .inset_0()
                        .flex()
                        .gap(px(1.0))
                        .children(segments),
                )
                .when(motion != Motion::Still, |bar| {
                    bar.child(sweep(theme, &id, motion, reduced))
                }),
        )
        .tooltip_show_delay(Duration::from_millis(200))
        .tooltip(move |_window, cx| mix_tip(tip.clone(), cx))
}

fn segments(theme: &Theme, counts: &[LanguageCount], total: u64, motion: Motion) -> Vec<Div> {
    let total = total.max(1);
    if counts.is_empty() {
        let (hue, alpha) = (crate::theme::language::hue(Language::Unknown), 0.35);
        return vec![
            div()
                .h_full()
                .flex_1()
                .rounded_full()
                .bg(theme.plane_wash(hue, if motion == Motion::Still { 0.0 } else { alpha })),
        ];
    }
    counts
        .iter()
        .map(|count| {
            div()
                .h_full()
                .rounded_full()
                .bg(theme.on_plane(language_hue(count.language())))
                .flex_basis(px(0.0))
                .flex_grow(ratio(count.declarations().get(), total))
                .flex_shrink(1.0)
        })
        .collect()
}

/// Returns the travelling highlight drawn over an indexing bar.
fn sweep(theme: &Theme, id: &SharedString, motion: Motion, reduced: bool) -> AnyElement {
    let mut wash = theme.paint(Paint::TextStrong);
    wash.alpha = 0.55;
    let breathing = motion == Motion::Waiting;
    let band = div()
        .absolute()
        .top_0()
        .bottom_0()
        .w(gpui::relative(0.28))
        .rounded_full()
        .bg(wash);
    if reduced {
        return band
            .left(gpui::relative(if breathing { 0.36 } else { 0.0 }))
            .opacity(if breathing { 0.45 } else { 0.0 })
            .into_any_element();
    }
    band.with_animation(
        ElementId::Name(SharedString::from(format!("sweep-{id}"))),
        Animation::new(SWEEP)
            .repeat()
            .with_easing(gpui::ease_in_out),
        move |band, delta| {
            if breathing {
                band.left(gpui::relative(0.36))
                    .opacity((0.5 - delta).abs().mul_add(-1.2, 0.8).clamp(0.15, 0.8))
            } else {
                band.left(gpui::relative(delta.mul_add(1.28, -0.28)))
            }
        },
    )
    .into_any_element()
}

/// One row of the hover tooltip: logo, name, share, count.
#[derive(Clone, Debug)]
struct MixRow {
    language: Language,
    count: u64,
    share: u32,
}

fn tooltip_rows(counts: &[LanguageCount], total: u64) -> Vec<MixRow> {
    let total = total.max(1);
    counts
        .iter()
        .map(|count| MixRow {
            language: count.language(),
            count: count.declarations().get(),
            share: percent(count.declarations().get(), total),
        })
        .collect()
}

fn mix_tip(rows: Vec<MixRow>, cx: &mut App) -> AnyView {
    let theme = crate::theme::theme(cx);
    cx.new(|_| MixTip { theme, rows }).into()
}

struct MixTip {
    theme: Theme,
    rows: Vec<MixRow>,
}

impl gpui::Render for MixTip {
    fn render(&mut self, _window: &mut Window, _cx: &mut gpui::Context<Self>) -> impl IntoElement {
        let theme = &self.theme;
        let total: u64 = self.rows.iter().map(|row| row.count).sum();
        super::surface::raised(theme)
            .px(space(Space::Base))
            .py(space(Space::Snug))
            .flex()
            .flex_col()
            .gap(px(3.0))
            .when(self.rows.is_empty(), |tip| {
                tip.child(
                    div()
                        .text_size(type_size(TypeScale::Tiny))
                        .text_color(theme.paint(Paint::TextDim))
                        .child("No declarations published yet."),
                )
            })
            .children(self.rows.iter().map(|row| mix_row(theme, row)))
            .when(!self.rows.is_empty(), |tip| {
                tip.child(
                    div()
                        .pt(px(2.0))
                        .text_size(type_size(TypeScale::Micro))
                        .text_color(theme.paint(Paint::TextFaint))
                        .child(format!("{total} declarations")),
                )
            })
    }
}

fn mix_row(theme: &Theme, row: &MixRow) -> Div {
    let ink = theme.on_plane(language_hue(row.language));
    div()
        .flex()
        .items_center()
        .gap(space(Space::Snug))
        .child(
            div()
                .flex_none()
                .w(px(14.0))
                .h(px(14.0))
                .flex()
                .items_center()
                .justify_center()
                .when_some(Logo::of(row.language), |slot, logo| {
                    slot.child(icon::logo(logo, 13.0, ink))
                }),
        )
        .child(
            div()
                .w(px(88.0))
                .text_size(type_size(TypeScale::Tiny))
                .font_weight(FontWeight::MEDIUM)
                .text_color(theme.paint(Paint::TextStrong))
                .child(language_label(row.language)),
        )
        .child(
            div()
                .w(px(34.0))
                .text_size(type_size(TypeScale::Tiny))
                .text_color(ink)
                .child(format!("{}%", row.share)),
        )
        .child(
            div()
                .text_size(type_size(TypeScale::Tiny))
                .font_family(theme.specimen())
                .text_color(theme.paint(Paint::TextDim))
                .child(row.count.to_string()),
        )
}

fn ratio(part: u64, total: u64) -> f32 {
    let part = u32::try_from(part).unwrap_or(u32::MAX);
    let total = u32::try_from(total).unwrap_or(u32::MAX).max(1);
    #[expect(
        clippy::cast_precision_loss,
        reason = "a declaration count above sixteen million cannot change a bar's width"
    )]
    let share = part as f32 / total as f32;
    share.clamp(0.0, 1.0)
}

fn percent(part: u64, total: u64) -> u32 {
    let scaled = part.saturating_mul(100) / total.max(1);
    u32::try_from(scaled).unwrap_or(100).min(100)
}
