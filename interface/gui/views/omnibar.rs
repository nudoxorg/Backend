//! Defines the omnibar for `interface-gui`.
//! This module owns the one field and its mode, scope, and submission chrome.
//! Its narrow surface is a single bar the whole window addresses through one focus handle.

use gpui::{AnyElement, Context, InteractiveElement, IntoElement, Styled, div, prelude::*};

use crate::app::{Workspace, input::FieldTarget};
use crate::theme::tokens::Role;
use crate::ui::{self, hsla};

/// Draws the omnibar: mode chip, scope chip, the field, and the scope's dismissal.
pub(crate) fn bar(
    workspace: &Workspace,
    window: &mut gpui::Window,
    cx: &mut Context<Workspace>,
) -> AnyElement {
    let theme = *workspace.theme();
    let focused = workspace.omnibar_focus().is_focused(window);
    let mode = workspace.search().mode();
    let placeholder = if mode.is_commands() {
        "run a command"
    } else {
        "search · @package scope · > commands"
    };
    let scope_chip = mode.scope().map(|scope| {
        ui::button(&theme, &format!("in {scope}"), false, {
            cx.listener(|workspace, _: &gpui::ClickEvent, _, cx| {
                workspace.search_mut().clear_scope();
                cx.notify();
            })
        })
    });
    div()
        .id("omnibar")
        .key_context("Omnibar")
        .track_focus(workspace.omnibar_focus())
        .flex()
        .items_center()
        .gap(gpui::px(theme.pixels(crate::theme::Space::Tight.rems())))
        .px(gpui::px(theme.pixels(crate::theme::Space::Base.rems())))
        .py(gpui::px(theme.pixels(crate::theme::Space::Tight.rems())))
        .border_b_1()
        .border_color(hsla(theme.palette().divider()))
        .bg(hsla(theme.palette().panel()))
        .child(ui::with_role(div(), &theme, Role::Caption).child(
            if mode.is_commands() { ">" } else { "⌕" }.to_owned(),
        ))
        .children(scope_chip)
        .child(
            ui::field(&theme, Role::Ui, workspace.search().text(), placeholder, focused)
                .id("omnibar-field"),
        )
        .child(workspace.field_bridge(FieldTarget::Omnibar, workspace.omnibar_focus(), cx))
        .into_any_element()
}
