//! Defines the library panel for `interface-gui`.
//! This module owns the shelf rows, the inline add flow, and the active compile's journey.
//! Its narrow surface keeps every package failure inside its row.

use gpui::{AnyElement, Context, InteractiveElement, IntoElement, Styled, div, prelude::*};

use crate::app::Workspace;
use crate::store::library::RowStatus;
use crate::theme::{Space, tokens::Role};
use crate::ui::{self, hsla};

/// The height of one shelf row.
const ROW_HEIGHT: f32 = 40.0;

/// Draws the library panel at its animated width.
pub(crate) fn panel(
    workspace: &Workspace,
    window: &mut gpui::Window,
    cx: &mut Context<Workspace>,
) -> AnyElement {
    let theme = *workspace.theme();
    let width = workspace.motion().library_width();
    if width < 1.0 {
        return div().id("library").w(gpui::px(0.0)).into_any_element();
    }
    let rows = workspace.store().rows();
    let cursor = workspace.shelf_cursor();
    let list = gpui::uniform_list(
        "shelf",
        rows.len(),
        cx.processor(move |workspace, range, _window, _cx| {
            let theme = *workspace.theme();
            let rows = workspace.store().rows();
            range
                .map(|index| {
                    let Some(row) = rows.get(index) else {
                        return div().into_any_element();
                    };
                    let selected = index == workspace.shelf_cursor();
                    shelf_row(&theme, row, selected, index, workspace, cx)
                        .into_any_element()
                })
                .collect::<Vec<_>>()
        }),
    )
    .h(gpui::px(ROW_HEIGHT * 12.0))
    .flex_grow();
    div()
        .id("library")
        .flex()
        .flex_col()
        .w(gpui::px(width))
        .min_w_0()
        .border_r_1()
        .border_color(hsla(theme.palette().divider()))
        .bg(hsla(theme.palette().panel()))
        .child(
            div()
                .id("library-head")
                .flex()
                .items_center()
                .justify_between()
                .px(gpui::px(theme.pixels(Space::Tight.rems())))
                .py(gpui::px(theme.pixels(Space::Hair.rems())))
                .child(ui::section_label(&theme, "Library"))
                .child(
                    ui::with_role(div(), &theme, Role::Dense)
                        .text_color(hsla(theme.palette().text_low()))
                        .child(format!("{}", rows.len())),
                ),
        )
        .child(
            div()
                .id("shelf-fault")
                .children(workspace.store().shelf_fault().map(|fault| {
                    ui::fault_block(&theme, fault).into_any_element()
                })),
        )
        .when(rows.is_empty() && workspace.store().shelf_fault().is_none(), |panel| {
            panel.child(
                div()
                    .id("shelf-empty")
                    .px(gpui::px(theme.pixels(Space::Tight.rems())))
                    .child(ui::empty_shelf_hint(&theme)),
            )
        })
        .child(list)
        .child(add_flow(workspace, window, cx))
        .into_any_element()
}

/// One shelf row: ecosystem mark, name, status line, and tail glyph.
fn shelf_row(
    theme: &crate::theme::Theme,
    row: &crate::store::library::ShelfRow,
    selected: bool,
    index: usize,
    _workspace: &Workspace,
    cx: &mut Context<Workspace>,
) -> div
{
    let mark = crate::theme::ecosystem_mark(row.coordinate.ecosystem);
    let ground = if selected {
        theme.palette().element_active()
    } else {
        theme.palette().panel()
    };
    div()
        .id(("shelf-row", index))
        .h(gpui::px(ROW_HEIGHT))
        .flex()
        .items_center()
        .gap(gpui::px(theme.pixels(Space::Tight.rems())))
        .px(gpui::px(theme.pixels(Space::Tight.rems())))
        .bg(hsla(ground))
        .cursor_pointer()
        .on_click(cx.listener(move |workspace, _: &gpui::ClickEvent, _window, cx| {
            workspace.select_shelf_row(index);
            cx.notify();
        }))
        .child(
            ui::with_role(div(), &theme, Role::Dense)
                .text_color(hsla(mark.color(theme.appearance())))
                .child(mark.glyph().to_owned()),
        )
        .child(
            ui::with_role(div(), &theme, Role::Ui)
                .text_color(hsla(theme.palette().text()))
                .child(row.coordinate.name.as_str().to_owned()),
        )
        .child(
            ui::with_role(div(), &theme, Role::Dense)
                .text_color(hsla(theme.palette().text_low()))
                .child(row.census_line()),
        )
        .child(
            ui::with_role(div(), &theme, Role::Dense)
                .text_color(hsla(match &row.status {
                    RowStatus::Requested | RowStatus::Compiling { .. } => theme.palette().text_low(),
                    RowStatus::Ready { .. } => theme::Status::Ok.color(theme.appearance()),
                    RowStatus::Failed { .. } => theme::Status::Danger.color(theme.appearance()),
                }))
                .child(row.status.glyph().to_owned()),
        )
}

/// The inline add flow at the foot of the panel.
fn add_flow(
    workspace: &Workspace,
    window: &mut gpui::Window,
    cx: &mut Context<Workspace>,
) -> AnyElement {
    let theme = *workspace.theme();
    let flow = &workspace.store().add;
    let focused = workspace.add_focus().is_focused(window);
    if !flow.open {
        return ui::button(&theme, "+ add package", false, {
            cx.listener(|workspace, _: &gpui::ClickEvent, window, cx| {
                workspace.open_add_flow("", window);
                cx.notify();
            })
        })
        .into_any_element();
    }
    let mut chips = div().id("ecosystems").flex().flex_wrap().gap(gpui::px(
        theme.pixels(Space::Hair.rems()),
    ));
    for ecosystem in interface_identity::ALL_ECOSYSTEMS {
        let chosen = ecosystem == flow.ecosystem;
        chips = chips.child(
            ui::button(
                &theme,
                interface_identity::ecosystem_tag(ecosystem).as_str(),
                chosen,
                {
                    cx.listener(move |workspace, _: &gpui::ClickEvent, _, cx| {
                        workspace.store_mut().add.choose(ecosystem);
                        cx.notify();
                    })
                },
            )
            .into_any_element(),
        );
    }
    let mut column = div()
        .id("add-flow")
        .key_context("AddField")
        .track_focus(workspace.add_focus())
        .flex()
        .flex_col()
        .gap(gpui::px(theme.pixels(Space::Hair.rems())))
        .px(gpui::px(theme.pixels(Space::Tight.rems())))
        .py(gpui::px(theme.pixels(Space::Tight.rems())))
        .border_t_1()
        .border_color(hsla(theme.palette().divider()))
        .child(chips)
        .child(
            ui::field(&theme, Role::Mono, &flow.text, "cargo:name@version", focused)
                .id("add-field"),
        )
        .child(workspace.field_bridge(FieldTarget::AddCoordinate, workspace.add_focus(), cx));
    match &flow.draft {
        crate::store::library::AddDraft::Empty => {}
        crate::store::library::AddDraft::Valid { .. } => {
            column = column.child(ui::text_low(&theme, Role::Dense, "enter to compile"));
        }
        crate::store::library::AddDraft::Invalid { message, .. } => {
            column = column.child(
                ui::with_role(div(), &theme, Role::Dense)
                    .text_color(hsla(theme::Status::Warn.color(theme.appearance())))
                    .child(message.clone()),
            );
        }
    }
    if let Some(fault) = &flow.fault {
        column = column.child(ui::fault_block(&theme, fault));
    }
    column.into_any_element()
}
