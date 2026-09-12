//! Defines the library panel for `interface-gui`.
//! This module owns the shelf rows, the inline add flow, and the active compile's journey.
//! Its narrow surface keeps every package failure inside its row.

use core::cell::Cell;

use gpui::{
    AnyElement, Context, InteractiveElement, IntoElement, ScrollStrategy, Styled,
    UniformListScrollHandle, div, prelude::*,
};

use crate::app::{FieldTarget, Workspace};
use crate::store::library::{AddDraft, RowStatus};
use crate::theme::{Role, Space, Status};
use crate::ui::{self, hsla};

/// The height of one shelf row.
const ROW_HEIGHT: f32 = 40.0;

/// How many shelf rows the list is laid out with before it grows into its flex space.
const SHELF_ROWS: f32 = 12.0;

/// The tallest the add flow reveals to, at full ease, in rems.
const ADD_FLOW_HEIGHT_REMS: f32 = 14.0;

/// The collapsed add flow's one button.
const ADD_BUTTON: &str = "+ add package";

/// The coordinate field's placeholder spelling.
const ADD_PLACEHOLDER: &str = "cargo:name@version";

/// The dense line under a draft the add flow would accept as typed.
const VALID_HINT: &str = "enter to compile";

/// The glyph on the active compile's cancel control.
const CANCEL_GLYPH: &str = "⨯";

thread_local! {
    /// The shelf selection the list last followed, so wheel scrolling is not undone every frame.
    static FOLLOWED: Cell<Option<usize>> = const { Cell::new(None) };
}

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
    let selected = workspace.shelf_cursor().min(rows.len().saturating_sub(1));
    let scroll = UniformListScrollHandle::new();
    let follow = FOLLOWED.with(|cell| {
        let same = cell.get() == Some(selected);
        cell.set(Some(selected));
        !same
    });
    if follow {
        scroll.scroll_to_item(selected, ScrollStrategy::Nearest);
    }
    let list = gpui::uniform_list(
        "shelf",
        rows.len(),
        cx.processor(
            move |workspace, range: core::ops::Range<usize>, _window, cx| -> Vec<AnyElement> {
                let theme = *workspace.theme();
                let rows = workspace.store().rows();
                range
                    .map(|index| {
                        let Some(row) = rows.get(index) else {
                            return div().into_any_element();
                        };
                        let selected = index == workspace.shelf_cursor();
                        shelf_row(&theme, row, selected, index, workspace, cx)
                    })
                    .collect::<Vec<_>>()
            },
        ),
    )
    .track_scroll(&scroll)
    .h(gpui::px(ROW_HEIGHT * SHELF_ROWS))
    .flex_grow(1.0);
    let job = workspace
        .store()
        .active()
        .map(|job| (job.dots(), job.label()));
    let head_tail = match job {
        Some((dots, label)) => div()
            .id("compile-job")
            .flex()
            .items_center()
            .gap(gpui::px(theme.pixels(Space::Hair.rems())))
            .child(
                ui::with_role(div(), &theme, Role::Mono)
                    .text_color(hsla(Status::Info.color(theme.appearance())))
                    .child(dots),
            )
            .child(ui::text_low(&theme, Role::Dense, label))
            .child(
                ui::button(&theme, CANCEL_GLYPH, false, {
                    cx.listener(|workspace, _: &gpui::ClickEvent, _, cx| {
                        workspace.cancel_compile();
                        cx.notify();
                    })
                })
                .into_any_element(),
            )
            .into_any_element(),
        None => ui::with_role(div(), &theme, Role::Dense)
            .text_color(hsla(theme.palette().text_low()))
            .child(format!("{}", rows.len()))
            .into_any_element(),
    };
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
                .child(head_tail),
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

/// One shelf row: ecosystem mark, name, version, status line, and tail glyph.
fn shelf_row(
    theme: &crate::theme::Theme,
    row: &crate::store::library::ShelfRow,
    selected: bool,
    index: usize,
    _workspace: &Workspace,
    cx: &mut Context<Workspace>,
) -> AnyElement {
    let mark = crate::theme::ecosystem_mark(row.coordinate.ecosystem);
    let ground = if selected {
        theme.palette().element_active()
    } else {
        theme.palette().panel()
    };
    let hue = match &row.status {
        RowStatus::Requested => theme.palette().text_inert(),
        RowStatus::Compiling { .. } => Status::Info.color(theme.appearance()),
        RowStatus::Ready { .. } => Status::Ok.color(theme.appearance()),
        RowStatus::Failed { .. } => Status::Danger.color(theme.appearance()),
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
            if let Some(row) = workspace.store().rows().get(index)
                && row.status.is_readable()
            {
                let coordinate = row.coordinate.clone();
                workspace.open_outline(&coordinate);
            }
            cx.notify();
        }))
        .child(
            ui::with_role(div(), theme, Role::Dense)
                .text_color(hsla(mark.color(theme.appearance())))
                .child(mark.glyph().to_owned()),
        )
        .child(
            ui::with_role(div(), theme, Role::Ui)
                .text_color(hsla(theme.palette().text()))
                .child(row.coordinate.name.as_str().to_owned()),
        )
        .child(ui::text_low(theme, Role::Dense, row.coordinate.version.as_str()))
        .child(ui::text_low(theme, Role::Dense, &row.census_line()))
        .child(
            ui::with_role(div(), theme, Role::Dense)
                .text_color(hsla(hue))
                .child(row.status.glyph().to_owned()),
        )
        .into_any_element()
}

/// The inline add flow at the foot of the panel, revealed at the motion set's ease.
fn add_flow(
    workspace: &Workspace,
    window: &mut gpui::Window,
    cx: &mut Context<Workspace>,
) -> AnyElement {
    let theme = *workspace.theme();
    let flow = &workspace.store().add;
    let focused = workspace.add_focus().is_focused(window);
    if !flow.open {
        return ui::button(&theme, ADD_BUTTON, false, {
            cx.listener(|workspace, _: &gpui::ClickEvent, window, cx| {
                workspace.open_add_flow("", window, cx);
                cx.notify();
            })
        })
        .into_any_element();
    }
    let reveal = workspace.motion().add_flow().eased();
    let mut chips = div()
        .id("ecosystems")
        .flex()
        .flex_wrap()
        .gap(gpui::px(theme.pixels(Space::Hair.rems())));
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
            ui::field(&theme, Role::Mono, &flow.text, ADD_PLACEHOLDER, focused)
                .id("add-field"),
        )
        .child(workspace.field_bridge(FieldTarget::AddCoordinate, workspace.add_focus(), cx));
    match &flow.draft {
        AddDraft::Empty => {}
        AddDraft::Valid { .. } => {
            column = column.child(ui::text_low(&theme, Role::Dense, VALID_HINT));
        }
        AddDraft::Invalid { message, .. } => {
            column = column.child(
                ui::with_role(div(), &theme, Role::Dense)
                    .text_color(hsla(Status::Warn.color(theme.appearance())))
                    .child(message.clone()),
            );
        }
    }
    if let Some(fault) = &flow.fault {
        column = column.child(ui::fault_block(&theme, fault));
    }
    column
        .max_h(gpui::px(theme.root_pixels() * ADD_FLOW_HEIGHT_REMS * reveal))
        .opacity(reveal)
        .overflow_hidden()
        .into_any_element()
}
