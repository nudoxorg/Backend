//! Defines the context panel for `interface-gui`.
//! This module owns the package outline tree and the hover card beneath it.
//! Its narrow surface keeps navigation state one tree deep, at the reader's right hand.

use gpui::{AnyElement, Context, Div, InteractiveElement, IntoElement, Styled, div, prelude::*};
use interface_documents::{Outline, OutlineNode};

use crate::app::{ContextSlot, Workspace};
use crate::store::document::{HoverCard, PageKey};
use crate::theme::{Role, Space, Theme};
use crate::ui::{self, hsla};

/// The height of one row the tree list draws.
///
/// Every row the list draws shares it — declarations and count lines alike — because the list is
/// uniform: it measures one row and lays out the rest on that measurement.
const ROW_HEIGHT: f32 = 28.0;

/// How many rows tall the tree viewport is before it scrolls inside the panel.
const VISIBLE_ROWS: f32 = 16.0;

/// The deepest outline depth drawn as a row.
///
/// Declarations deeper than this collapse into the count line under their nearest drawn ancestor,
/// so the panel can never grow into an unbounded scroll.
const MAX_DRAWN_DEPTH: u16 = 6;

/// The most declaration rows the tree draws before its tail collapses into one count line.
///
/// Picked from [`OutlineNode::size`] economics: the context panel is a right-hand companion, not
/// a second document, so past four hundred rows the remaining tree is summarized rather than
/// drawn, and the list scrolls inside its viewport instead of stretching the panel.
const ROW_BUDGET: usize = 400;

/// The reserved height of the hover-card body, so Pending → Ready cannot shift the layout.
const CARD_BODY_MIN_HEIGHT: f32 = 96.0;

/// The fixed height of the pending "reading" line.
const PENDING_LINE_HEIGHT: f32 = 20.0;

/// One drawn row of the context tree.
#[derive(Clone, Copy, Debug)]
enum TreeRow<'a> {
    /// A declaration drawn at or above the disclosure depth.
    Node {
        /// The declaration.
        node: &'a OutlineNode,
        /// Its depth in the tree.
        depth: usize,
    },
    /// The count line standing in for declarations that were not drawn.
    Withheld {
        /// How many declarations the line stands in for.
        count: usize,
        /// The depth the line hangs at.
        depth: usize,
    },
}

/// Draws the context panel at its animated width.
pub(crate) fn panel(
    workspace: &Workspace,
    _window: &mut gpui::Window,
    cx: &mut Context<Workspace>,
) -> AnyElement {
    let theme = *workspace.theme();
    let width = workspace.motion().outline_width();
    if width < 1.0 {
        return div().id("context").w(gpui::px(0.0)).into_any_element();
    }
    let mut column = div()
        .id("context")
        .flex()
        .flex_col()
        .min_h_0()
        .overflow_hidden()
        .w(gpui::px(width))
        .min_w_0()
        .border_l_1()
        .border_color(hsla(theme.palette().divider()))
        .bg(hsla(theme.palette().panel()))
        .child(
            div()
                .id("context-head")
                .px(gpui::px(theme.pixels(Space::Tight.rems())))
                .py(gpui::px(theme.pixels(Space::Hair.rems())))
                .child(ui::section_label(&theme, "Context")),
        );
    match workspace.context() {
        ContextSlot::Idle => {
            column = column.child(
                div()
                    .id("context-hint")
                    .px(gpui::px(theme.pixels(Space::Tight.rems())))
                    .child(ui::text_low(
                        &theme,
                        Role::Body,
                        "select a package to see its tree",
                    )),
            );
        }
        ContextSlot::Loading { package } => {
            column = column.child(
                div()
                    .id("context-hint")
                    .px(gpui::px(theme.pixels(Space::Tight.rems())))
                    .child(ui::text_low(
                        &theme,
                        Role::Body,
                        &format!("reading {}", package.name.as_str()),
                    )),
            );
        }
        ContextSlot::Ready { outline } => {
            let rows = disclosure_rows(outline);
            let list = gpui::uniform_list(
                "outline",
                rows.len(),
                cx.processor(
                    move |workspace, range: core::ops::Range<usize>, _window, cx| -> Vec<AnyElement> {
                        let theme = *workspace.theme();
                        let ContextSlot::Ready { outline } = workspace.context() else {
                            return Vec::new();
                        };
                        let rows = disclosure_rows(outline);
                        range
                            .map(|index| {
                                let Some(row) = rows.get(index).copied() else {
                                    return div().into_any_element();
                                };
                                tree_row(&theme, row, index, cx)
                            })
                            .collect::<Vec<_>>()
                    },
                ),
            )
            .h(gpui::px(ROW_HEIGHT * VISIBLE_ROWS))
            .flex_grow(1.0);
            column = column.child(list);
        }
        ContextSlot::Failed { fault } => {
            column = column.child(
                div()
                    .id("context-fault")
                    .px(gpui::px(theme.pixels(Space::Tight.rems())))
                    .child(ui::fault_block(&theme, fault)),
            );
        }
    }
    if let Some(card) = workspace
        .hovered()
        .and_then(|key| workspace.documents().card(key))
    {
        column = column.child(hover_card(&theme, card));
    }
    column.into_any_element()
}

/// Flattens the containment tree into drawn rows under the depth and row-budget discipline.
///
/// A node draws when it is no deeper than [`MAX_DRAWN_DEPTH`] and the [`ROW_BUDGET`] still has
/// room; its withheld descendants, if any, become the count line beneath it. Once the budget is
/// spent, everything still unvisited collapses into one final count line and the walk stops.
fn disclosure_rows(outline: &Outline) -> Vec<TreeRow<'_>> {
    let total = outline.roots.iter().map(OutlineNode::size).sum::<usize>();
    let mut rows = Vec::new();
    let mut drawn = 0usize;
    let mut accounted = 0usize;
    for (node, depth) in outline.walk() {
        if depth > usize::from(MAX_DRAWN_DEPTH) {
            continue;
        }
        if drawn >= ROW_BUDGET {
            rows.push(TreeRow::Withheld {
                count: total - accounted,
                depth,
            });
            return rows;
        }
        drawn += 1;
        accounted += 1;
        rows.push(TreeRow::Node { node, depth });
        if depth == usize::from(MAX_DRAWN_DEPTH) {
            let count = node.children.iter().map(OutlineNode::size).sum::<usize>();
            if count > 0 {
                accounted += count;
                rows.push(TreeRow::Withheld {
                    count,
                    depth: depth.saturating_add(1),
                });
            }
        }
    }
    rows
}

/// Draws one tree row: a declaration, or the count line standing in for withheld ones.
fn tree_row(
    theme: &Theme,
    row: TreeRow<'_>,
    index: usize,
    cx: &mut Context<Workspace>,
) -> AnyElement {
    match row {
        TreeRow::Node { node, depth } => outline_row(theme, node, depth, index, cx),
        TreeRow::Withheld { count, depth } => withheld_line(theme, count, depth, index),
    }
}

/// One declaration row: kind glyph chip and name, indented by its depth.
///
/// Clicking opens the declaration as a page; hovering asks for its card, on enter only, the way
/// the store's at-most-once law expects.
fn outline_row(
    theme: &Theme,
    node: &OutlineNode,
    depth: usize,
    index: usize,
    cx: &mut Context<Workspace>,
) -> AnyElement {
    let key = PageKey::of(&node.symbol.address);
    let indent = u16::try_from(depth).map_or(0.0, f32::from) * theme.pixels(Space::Hair.rems());
    div()
        .id(("outline-row", index))
        .h(gpui::px(ROW_HEIGHT))
        .flex()
        .items_center()
        .gap(gpui::px(theme.pixels(Space::Hair.rems())))
        .pl(gpui::px(indent))
        .pr(gpui::px(theme.pixels(Space::Hair.rems())))
        .cursor_pointer()
        .on_click({
            let key = key.clone();
            cx.listener(move |workspace, _: &gpui::ClickEvent, _window, cx| {
                workspace.open_page(&key);
                cx.notify();
            })
        })
        .on_hover(cx.listener(move |workspace, hovered: &bool, _window, cx| {
            if *hovered {
                workspace.open_card(&key);
            } else {
                workspace.forget_hover(&key);
            }
            cx.notify();
        }))
        .child(ui::kind_glyph_chip(theme, node.symbol.kind))
        .child(ui::nav_text(theme, node.symbol.name.as_str()))
        .into_any_element()
}

/// The count line standing in for declarations the panel withheld.
fn withheld_line(theme: &Theme, count: usize, depth: usize, index: usize) -> AnyElement {
    let indent = u16::try_from(depth).map_or(0.0, f32::from) * theme.pixels(Space::Hair.rems());
    div()
        .id(("outline-count", index))
        .h(gpui::px(ROW_HEIGHT))
        .flex()
        .items_center()
        .pl(gpui::px(indent))
        .child(ui::inert_text(
            theme,
            Role::Dense,
            &format!("{count} declarations below"),
        ))
        .into_any_element()
}

/// The hover-card surface: what the panel knows about the declaration under the pointer.
///
/// The body keeps its reserved height through Pending → Ready, so nothing beneath the card
/// moves while the engine answers.
fn hover_card(theme: &Theme, card: &HoverCard) -> AnyElement {
    div()
        .id("hover-card")
        .flex()
        .flex_col()
        .border_t_1()
        .border_color(hsla(theme.palette().divider()))
        .px(gpui::px(theme.pixels(Space::Tight.rems())))
        .py(gpui::px(theme.pixels(Space::Tight.rems())))
        .child(ui::section_label(theme, "Under the pointer"))
        .child(
            div()
                .id("hover-card-body")
                .flex()
                .flex_col()
                .pt(gpui::px(theme.pixels(Space::Hair.rems())))
                .gap(gpui::px(theme.pixels(Space::Hair.rems())))
                .min_h(gpui::px(CARD_BODY_MIN_HEIGHT))
                .children(match card {
                    HoverCard::Pending => Some(pending_line(theme).into_any_element()),
                    HoverCard::Ready {
                        signature,
                        summary,
                        kind,
                    } => Some(
                        ready_body(theme, signature, *kind, summary.as_ref()).into_any_element(),
                    ),
                    HoverCard::Missing { fault } => {
                        Some(ui::fault_block(theme, fault).into_any_element())
                    }
                }),
        )
        .into_any_element()
}

/// The reserved-height line drawn while a card is on its way.
fn pending_line(theme: &Theme) -> AnyElement {
    div()
        .id("card-pending")
        .h(gpui::px(PENDING_LINE_HEIGHT))
        .flex()
        .items_center()
        .child(ui::inert_text(theme, Role::Dense, "reading"))
        .into_any_element()
}

/// The answered card: kind chip and word, the signature specimen, and the first doc line.
fn ready_body(
    theme: &Theme,
    signature: &interface_documents::Signature,
    kind: compiler_ir_vocabulary::EntityKind,
    summary: Option<&interface_documents::Text>,
) -> gpui::Stateful<Div> {
    div()
        .id("card-ready")
        .flex()
        .flex_col()
        .gap(gpui::px(theme.pixels(Space::Hair.rems())))
        .child(
            div()
                .id("card-kind")
                .flex()
                .items_center()
                .gap(gpui::px(theme.pixels(Space::Hair.rems())))
                .child(ui::kind_glyph_chip(theme, kind))
                .child(ui::inert_text(theme, Role::Dense, ui::kind_word(kind))),
        )
        .child(ui::text(theme, Role::Mono, &signature.plain()))
        .children(summary.map(|summary| ui::text_low(theme, Role::Body, summary.as_str())))
}
