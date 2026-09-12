//! Defines the results sheet for `interface-gui`.
//! This module owns the kind chips, lane chips, hit rows, and the continuation affordance.
//! Its narrow surface guarantees zero rows never masquerade as no matches.

use compiler_ir_vocabulary::EntityKind;
use gpui::{AnyElement, Context, InteractiveElement, IntoElement, Styled, div, prelude::*};
use interface_search::Hit;

use crate::app::Workspace;
use crate::store::document::PageKey;
use crate::store::search::{OmnibarMode, PAGE_ROWS};
use crate::theme::{Radius, Role, Space, Status, kind_color, kind_ground};
use crate::ui::{self, hsla, kind_glyph, kind_word};

/// The height of one hit row.
const ROW_HEIGHT: f32 = 36.0;

/// The rows the sheet lays out at once, so the continuation line and paging agree.
const SHEET_ROWS: usize = PAGE_ROWS;

/// The continuation affordance under a page that cut rows off, naming the key that pages.
const MORE_ROWS_LINE: &str = "more rows · pagedown";

/// One page of rows as a height multiplier.
fn sheet_rows() -> f32 {
    u16::try_from(SHEET_ROWS).map_or(0.0, f32::from)
}

/// Draws the results sheet when a terminal exists and the omnibar is in search mode.
pub(crate) fn sheet(workspace: &Workspace, cx: &mut Context<Workspace>) -> AnyElement {
    let theme = *workspace.theme();
    let showing = matches!(workspace.search().mode(), OmnibarMode::Search { .. })
        && workspace.search().terminal().is_some();
    if !showing {
        return div().into_any_element();
    }
    let hits = workspace.search().hits();
    let lanes = workspace.search().lanes();
    let mut lane_chips = div()
        .id("lanes")
        .flex()
        .flex_wrap()
        .px(gpui::px(theme.pixels(Space::Base.rems())))
        .py(gpui::px(theme.pixels(Space::Hair.rems())))
        .gap(gpui::px(theme.pixels(Space::Hair.rems())));
    for chip in &lanes {
        lane_chips = lane_chips.child(ui::lane_chip(&theme, chip));
    }
    let list = gpui::uniform_list(
        "hits",
        hits.len(),
        cx.processor(
            move |workspace, range: core::ops::Range<usize>, _window, cx| -> Vec<AnyElement> {
                let theme = *workspace.theme();
                let hits = workspace.search().hits();
                let selected = workspace.search().selected();
                range
                    .map(|index| {
                        let Some(hit) = hits.get(index) else {
                            return div().into_any_element();
                        };
                        hit_row(&theme, hit, index == selected, index, workspace, cx)
                    })
                    .collect::<Vec<_>>()
            },
        ),
    )
    .h(gpui::px(ROW_HEIGHT * sheet_rows()));
    div()
        .id("results")
        .flex()
        .flex_col()
        .border_b_1()
        .border_color(hsla(theme.palette().divider()))
        .bg(hsla(theme.palette().panel()))
        .children(kind_chip_row(workspace, cx))
        .child(lane_chips)
        .child(list)
        .child(continuation_line(workspace, hits.len(), cx))
        .into_any_element()
}

/// The chip row naming every declaration kind on screen, one chip per kind with its count.
///
/// The row draws only the kinds the current result set actually served, never the whole
/// vocabulary, and hides entirely when no rows are on screen. A chip whose kind the next request
/// admits draws in the kind's own hue; a refused kind draws inert, and clicking either asks the
/// workspace to flip it, which re-arms the debounced search.
fn kind_chip_row(
    workspace: &Workspace,
    cx: &mut Context<Workspace>,
) -> Option<AnyElement> {
    let theme = *workspace.theme();
    let hits = workspace.search().hits();
    let admitted = workspace.search().kinds();
    let mut chips = Vec::new();
    for (slot, kind) in EntityKind::ALL.into_iter().enumerate() {
        let count = hits
            .iter()
            .filter(|hit| hit.symbol.kind == kind)
            .count();
        if count == 0 {
            continue;
        }
        chips.push(kind_chip(
            &theme,
            kind,
            slot,
            count,
            admitted.contains(kind),
            cx,
        ));
    }
    if chips.is_empty() {
        return None;
    }
    Some(
        div()
            .id("kinds")
            .flex()
            .flex_wrap()
            .px(gpui::px(theme.pixels(Space::Base.rems())))
            .py(gpui::px(theme.pixels(Space::Hair.rems())))
            .gap(gpui::px(theme.pixels(Space::Hair.rems())))
            .children(chips)
            .into_any_element(),
    )
}

/// One kind chip: glyph, word, and on-screen count, in the lane-chip shape.
fn kind_chip(
    theme: &crate::theme::Theme,
    kind: EntityKind,
    slot: usize,
    count: usize,
    admitted: bool,
    cx: &mut Context<Workspace>,
) -> AnyElement {
    let appearance = theme.appearance();
    let (ground, ink) = if admitted {
        (
            kind_ground(kind, appearance),
            kind_color(kind, appearance),
        )
    } else {
        (
            theme.palette().element(),
            theme.palette().text_inert(),
        )
    };
    ui::with_role(div(), theme, Role::Dense)
        .id(("kind-chip", slot))
        .flex()
        .px(gpui::px(theme.pixels(Space::Tight.rems())))
        .py(gpui::px(theme.pixels(Space::Hair.rems())))
        .rounded(gpui::px(theme.pixels(Radius::Chip.rems())))
        .bg(hsla(ground))
        .text_color(hsla(ink))
        .cursor_pointer()
        .hover(|style| style.text_color(hsla(theme.palette().text())))
        .on_click(cx.listener(move |workspace, _: &gpui::ClickEvent, _, cx| {
            workspace.toggle_search_kind(kind, cx);
            cx.notify();
        }))
        .child(format!("{} {} {}", kind_glyph(kind), kind_word(kind), count))
        .into_any_element()
}

/// The line beneath the rows: an affordance that fetches more, or the row count when complete.
///
/// The truncation word is a claim the sheet has to be able to keep, so when the terminal is cut
/// the line is a live control that asks for the next page; when the terminal is complete the line
/// falls back to the plain count. A landed continuation replaces the terminal, so the line
/// re-derives from the new truncation and never restates a page that no longer exists.
fn continuation_line(
    workspace: &Workspace,
    loaded: usize,
    cx: &mut Context<Workspace>,
) -> AnyElement {
    let theme = *workspace.theme();
    if workspace.search().is_truncated() {
        ui::with_role(div(), &theme, Role::Dense)
            .id("continue-rows")
            .px(gpui::px(theme.pixels(Space::Base.rems())))
            .py(gpui::px(theme.pixels(Space::Hair.rems())))
            .text_color(hsla(Status::Warn.color(theme.appearance())))
            .cursor_pointer()
            .hover(|style| style.text_color(hsla(theme.palette().text())))
            .on_click(cx.listener(|workspace, _: &gpui::ClickEvent, _, cx| {
                workspace.continue_search();
                cx.notify();
            }))
            .child(MORE_ROWS_LINE.to_owned())
            .into_any_element()
    } else {
        ui::with_role(div(), &theme, Role::Dense)
            .px(gpui::px(theme.pixels(Space::Base.rems())))
            .py(gpui::px(theme.pixels(Space::Hair.rems())))
            .text_color(hsla(theme.palette().text_low()))
            .child(format!("{loaded} rows"))
            .into_any_element()
    }
}

/// One hit row: kind glyph, navigable name, signature, summary, and the lane that produced it.
fn hit_row(
    theme: &crate::theme::Theme,
    hit: &Hit,
    selected: bool,
    index: usize,
    _workspace: &Workspace,
    cx: &mut Context<Workspace>,
) -> AnyElement {
    let key = PageKey::of(&hit.symbol.address);
    let name_key = key.clone();
    let ground = if selected {
        theme.palette().element_active()
    } else {
        theme.palette().panel()
    };
    div()
        .id(("hit", index))
        .h(gpui::px(ROW_HEIGHT))
        .flex()
        .items_center()
        .gap(gpui::px(theme.pixels(Space::Tight.rems())))
        .px(gpui::px(theme.pixels(Space::Base.rems())))
        .bg(hsla(ground))
        .cursor_pointer()
        .on_click(cx.listener(move |workspace, _: &gpui::ClickEvent, _window, cx| {
            let key = key.clone();
            workspace.open_page(&key);
            cx.notify();
        }))
        .child(ui::kind_glyph_chip(theme, hit.symbol.kind))
        .child(
            ui::nav_text(theme, hit.symbol.name.as_str())
                .id(("hit-name", index))
                .flex_shrink(0.0)
                .on_click(cx.listener(move |workspace, _: &gpui::ClickEvent, _window, cx| {
                    let key = name_key.clone();
                    workspace.open_page(&key);
                    cx.notify();
                })),
        )
        .children(hit.signature.as_ref().map(|signature| {
            ui::text_low(theme, Role::Mono, &signature.plain())
                .min_w_0()
                .truncate()
        }))
        .children(hit.summary.as_ref().map(|summary| {
            ui::text_low(theme, Role::Dense, summary.as_str())
                .flex_1()
                .min_w_0()
                .truncate()
        }))
        .child(
            ui::with_role(div(), theme, Role::Dense)
                .flex_shrink(0.0)
                .text_color(hsla(theme.palette().text_inert()))
                .child(hit.lane.label().to_owned()),
        )
        .into_any_element()
}
