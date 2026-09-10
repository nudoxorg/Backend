//! Defines the shell layout for `interface-gui`.
//! This module owns the one action surface and the composition of panels into a window.
//! Its narrow surface is the whole chrome: omnibar on top, library and context panels flanking the
//! reader, status bar beneath, and the palette floating over all of it when summoned.

mod library_panel;
mod omnibar;
mod outline_panel;
mod palette;
mod reader;
mod results;

use gpui::{
    AnyElement, Context, InteractiveElement, IntoElement, SharedString, Styled, div, prelude::*,
};

use crate::app::{Cancel, CancelCompile, CancelAdd, CloseTab, CycleAppearance, FocusOmnibar, GrowInterface, NavigateBack, NavigateForward, NextTab, PageDown, PageUp, PreviousTab, Refresh, SelectFirst, SelectLast, ShrinkInterface, StepDown, StepUp, Submit, SubmitAdd, ToggleLibraryPanel, ToggleMotion, ToggleOutlinePanel, Workspace};
use crate::theme::{Space, tokens::Role};
use crate::ui::{self, hsla};

/// Draws the whole window from the workspace's stores.
pub(crate) fn shell(
    workspace: &mut Workspace,
    window: &mut gpui::Window,
    cx: &mut Context<Workspace>,
) -> AnyElement {
    let theme = *workspace.theme();
    let palette_rows = workspace.registry_rows();
    let commands = workspace.search().mode().is_commands();
    let root = div()
        .id("shell")
        .key_context("Workspace")
        .track_focus(workspace.root_focus())
        .flex()
        .flex_col()
        .size_full()
        .bg(hsla(theme.palette().app_background()))
        .text_color(hsla(theme.palette().text()))
        .overflow_hidden()
        .on_action(cx.listener(|workspace, _: &FocusOmnibar, window, cx| {
            workspace.omnibar_focus().focus(window, cx);
            cx.notify();
        }))
        .on_action(cx.listener(|workspace, _: &Submit, window, cx| {
            if workspace.search().mode().is_commands() {
                let rows = workspace.registry_rows();
                let selected = workspace.command_cursor().min(rows.len().saturating_sub(1));
                if let Some(spec) = rows.get(selected) {
                    let id = spec.id;
                    workspace.execute_registry(id, window);
                }
            } else {
                workspace.submit();
            }
            cx.notify();
        }))
        .on_action(cx.listener(|workspace, _: &Cancel, _, cx| {
            if workspace.search().mode().is_commands() || workspace.search().mode().scope().is_none()
            {
                workspace.search_mut().clear();
            } else {
                workspace.search_mut().clear_scope();
            }
            cx.notify();
        }))
        .on_action(cx.listener(|workspace, _: &StepDown, _, cx| {
            if workspace.search().mode().is_commands() {
                workspace.step_registry(true);
            } else {
                workspace.search_mut().step(true);
            }
            cx.notify();
        }))
        .on_action(cx.listener(|workspace, _: &StepUp, _, cx| {
            if workspace.search().mode().is_commands() {
                workspace.step_registry(false);
            } else {
                workspace.search_mut().step(false);
            }
            cx.notify();
        }))
        .on_action(cx.listener(|workspace, _: &PageDown, _, cx| {
            workspace.search_mut().page(true);
            cx.notify();
        }))
        .on_action(cx.listener(|workspace, _: &PageUp, _, cx| {
            workspace.search_mut().page(false);
            cx.notify();
        }))
        .on_action(cx.listener(|workspace, _: &SelectFirst, _, cx| {
            workspace.search_mut().select_first();
            workspace.step_registry_first();
            cx.notify();
        }))
        .on_action(cx.listener(|workspace, _: &SelectLast, _, cx| {
            workspace.search_mut().select_last();
            workspace.step_registry_last();
            cx.notify();
        }))
        .on_action(cx.listener(|workspace, _: &SubmitAdd, _, cx| {
            workspace.submit_add();
            cx.notify();
        }))
        .on_action(cx.listener(|workspace, _: &CancelAdd, window, cx| {
            workspace.close_add_flow(window);
            cx.notify();
        }))
        .on_action(cx.listener(|workspace, _: &ToggleLibraryPanel, _, cx| {
            workspace.toggle_library_panel(cx);
        }))
        .on_action(cx.listener(|workspace, _: &ToggleOutlinePanel, _, cx| {
            workspace.toggle_outline_panel(cx);
        }))
        .on_action(cx.listener(|workspace, _: &NavigateBack, _, cx| {
            if let Some(key) = workspace.documents_mut().navigate(true) {
                workspace.fetch(&key);
            }
            cx.notify();
        }))
        .on_action(cx.listener(|workspace, _: &NavigateForward, _, cx| {
            if let Some(key) = workspace.documents_mut().navigate(false) {
                workspace.fetch(&key);
            }
            cx.notify();
        }))
        .on_action(cx.listener(|workspace, _: &CloseTab, _, cx| {
            let active = workspace.documents().active_index();
            if let Some(key) = workspace.documents_mut().close_tab(active) {
                workspace.fetch(&key);
            }
            cx.notify();
        }))
        .on_action(cx.listener(|workspace, _: &NextTab, _, cx| {
            workspace.documents_mut().cycle_tab(true);
            workspace.refetch_active();
            cx.notify();
        }))
        .on_action(cx.listener(|workspace, _: &PreviousTab, _, cx| {
            workspace.documents_mut().cycle_tab(false);
            workspace.refetch_active();
            cx.notify();
        }))
        .on_action(cx.listener(|workspace, _: &Refresh, _, cx| {
            workspace.refresh();
            cx.notify();
        }))
        .on_action(cx.listener(|workspace, _: &CycleAppearance, _, cx| {
            workspace.cycle_appearance(cx);
        }))
        .on_action(cx.listener(|workspace, _: &GrowInterface, _, cx| {
            workspace.step_interface(true, cx);
        }))
        .on_action(cx.listener(|workspace, _: &ShrinkInterface, _, cx| {
            workspace.step_interface(false, cx);
        }))
        .on_action(cx.listener(|workspace, _: &ToggleMotion, _, cx| {
            workspace.toggle_motion(cx);
        }))
        .on_action(cx.listener(|workspace, _: &CancelCompile, _, cx| {
            workspace.cancel_compile();
            cx.notify();
        }))
        .child(omnibar::bar(workspace, window, cx))
        .child(
            div()
                .id("body")
                .flex_1()
                .flex()
                .min_h_0()
                .child(library_panel::panel(workspace, window, cx))
                .child(reader::column(workspace, window, cx))
                .child(outline_panel::panel(workspace, window, cx)),
        )
        .child(status_bar(workspace, cx))
        .when(commands && !palette_rows.is_empty(), |root| {
            root.child(palette::overlay(workspace, cx))
        })
        .into_any_element();
    root
}

/// The status bar: capability glyphs, epoch, the active compile, and the reader's own faults.
fn status_bar(workspace: &Workspace, cx: &Context<Workspace>) -> AnyElement {
    let theme = *workspace.theme();
    let capabilities = [
        interface_library::Capability::Compiler,
        interface_library::Capability::Shelf,
        interface_library::Capability::Lexical,
        interface_library::Capability::Graph,
        interface_library::Capability::Vector,
        interface_library::Capability::Embedder,
    ];
    let mut left = div().id("status-left").flex().items_center().gap(gpui::px(
        theme.pixels(Space::Tight.rems()),
    ));
    for capability in capabilities {
        let glyph = workspace.store().capability_glyph(capability);
        let label = format!("{glyph} {}", capability.label());
        left = left.child(
            ui::with_role(div(), &theme, Role::Dense)
                .text_color(hsla(theme.palette().text_low()))
                .child(label),
        );
    }
    left = left.child(
        ui::with_role(div(), &theme, Role::Dense)
            .text_color(hsla(theme.palette().text_low()))
            .child(format!("epoch {}", workspace.store().epoch().0)),
    );
    if let Some(job) = workspace.store().active() {
        left = left.child(
            ui::button(&theme, &format!("{} {} ⨯", job.dots(), job.label()), false, {
                cx.listener(|workspace, _: &gpui::ClickEvent, _, cx| {
                    workspace.cancel_compile();
                    cx.notify();
                })
            })
            .into_any_element(),
        );
    }
    let mut right = div().id("status-right").flex().items_center().gap(gpui::px(
        theme.pixels(Space::Tight.rems()),
    ));
    let fault = workspace
        .startup_fault()
        .or_else(|| workspace.preferences_fault())
        .or_else(|| workspace.action_fault())
        .map(str::to_owned);
    if let Some(fault) = fault {
        right = right.child(
            ui::with_role(div(), &theme, Role::Dense)
                .text_color(hsla(theme::Status::Warn.color(theme.appearance())))
                .child(fault),
        );
    }
    if let Some(lines) = workspace.unreadable_pref_lines().first() {
        right = right.child(
            ui::with_role(div(), &theme, Role::Dense)
                .text_color(hsla(theme::Status::Warn.color(theme.appearance())))
                .child(format!("gui.prefs:{} ignored", lines.get())),
        );
    }
    right = right.child(ui::button(
        &theme,
        workspace.preferences().appearance.label(),
        false,
        cx.listener(|workspace, _: &gpui::ClickEvent, _, cx| workspace.cycle_appearance(cx)),
    ));
    right = right.child(
        ui::with_role(div(), &theme, Role::Dense)
            .text_color(hsla(theme.palette().text_low()))
            .child(format!("· {}", workspace.preferences().size.label())),
    );
    right = right.child(ui::button(
        &theme,
        "A+",
        false,
        cx.listener(|workspace, _: &gpui::ClickEvent, _, cx| workspace.step_interface(true, cx)),
    ));
    right = right.child(ui::button(
        &theme,
        "A−",
        false,
        cx.listener(|workspace, _: &gpui::ClickEvent, _, cx| workspace.step_interface(false, cx)),
    ));
    right = right.child(ui::button(
        &theme,
        SharedString::from(if workspace.preferences().motion.animates() {
            "motion"
        } else {
            "still"
        }),
        false,
        cx.listener(|workspace, _: &gpui::ClickEvent, _, cx| workspace.toggle_motion(cx)),
    ));
    div()
        .id("status")
        .flex()
        .items_center()
        .justify_between()
        .px(gpui::px(theme.pixels(Space::Tight.rems())))
        .py(gpui::px(theme.pixels(Space::Hair.rems())))
        .border_t_1()
        .border_color(hsla(theme.palette().divider()))
        .bg(hsla(theme.palette().panel()))
        .child(left)
        .child(right)
        .into_any_element()
}
