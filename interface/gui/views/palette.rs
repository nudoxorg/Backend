//! Defines the command palette for `interface-gui`.
//! This module owns the overlay that renders the shared command registry over the window.
//! Its narrow surface is one filtered list whose rows are the same twelve rows every surface shows.

use gpui::{AnyElement, Context, InteractiveElement, IntoElement, Styled, div, prelude::*};
use interface_library::{CommandId, CommandSpec, Mutation};

use crate::app::Workspace;
use crate::theme::{Radius, Role, Space, Status};
use crate::ui::{self, hsla};

/// The height of one palette row.
const ROW_HEIGHT: f32 = 44.0;

/// The palette card's width in rems, wide enough for every description at every size.
const CARD_WIDTH_REMS: f32 = 40.0;

/// How far the palette card sits from the top of the window, in pixels.
const CARD_TOP_PXS: f32 = 96.0;

/// The width of the selection nib bar, in pixels.
const NIB_WIDTH_PXS: f32 = 3.0;

/// The domain glyph column's width in rems, so titles align across both domains.
const DOMAIN_GLYPH_REMS: f32 = 1.5;

/// The glyph marking a registry row whose command serves the shelf this window holds.
const SHELF_GLYPH: &str = "⌂";

/// The glyph marking a registry row whose command reads the shared registry index.
const INDEX_GLYPH: &str = "≣";

/// The word the mutation mark draws beside a registry row whose command writes the shelf.
const WRITE_MARK: &str = "write";

/// Draws the palette overlay: scrim, floating panel, and the filtered registry.
pub(crate) fn overlay(workspace: &Workspace, cx: &mut Context<Workspace>) -> AnyElement {
    let theme = *workspace.theme();
    let rows = workspace.registry_rows();
    let selected = workspace.command_cursor().min(rows.len().saturating_sub(1));
    let reveal = workspace.motion().palette().eased();
    let mut list = div().id("palette-rows").relative().flex().flex_col();
    for (index, spec) in rows.iter().enumerate() {
        let chosen = index == selected;
        let ground = if chosen {
            theme.palette().element_active()
        } else {
            theme.palette().panel()
        };
        list = list.child(
            div()
                .id(("palette-row", index))
                .h(gpui::px(ROW_HEIGHT))
                .flex()
                .items_center()
                .gap(gpui::px(theme.pixels(Space::Tight.rems())))
                .px(gpui::px(theme.pixels(Space::Base.rems())))
                .bg(hsla(ground))
                .cursor_pointer()
                .on_click(cx.listener(move |workspace, _: &gpui::ClickEvent, window, cx| {
                    let id = workspace.registry_rows().get(index).map(|spec| spec.id);
                    if let Some(id) = id {
                        workspace.execute_registry(id, window, cx);
                    }
                    cx.notify();
                }))
                .child(domain_column(&theme, spec))
                .child(
                    ui::with_role(div(), &theme, Role::Ui)
                        .flex_shrink(0.0)
                        .text_color(hsla(theme.palette().text()))
                        .child(spec.title.to_owned()),
                )
                .child(
                    ui::with_role(div().flex_1().min_w_0(), &theme, Role::Dense)
                        .text_color(hsla(theme.palette().text_low()))
                        .child(spec.description.to_owned()),
                )
                .children(mutation_mark(&theme, spec.mutation)),
        );
    }
    list = list.child(
        div()
            .id("palette-nib")
            .absolute()
            .left_0()
            .top(gpui::px(nib_top(selected)))
            .w(gpui::px(NIB_WIDTH_PXS))
            .h(gpui::px(ROW_HEIGHT))
            .bg(hsla(theme.palette().accent())),
    );
    div()
        .id("palette")
        .absolute()
        .size_full()
        .top_0()
        .left_0()
        .bg(hsla(theme.palette().scrim()))
        .flex()
        .justify_center()
        .child(
            div()
                .id("palette-card")
                .mt(gpui::px(CARD_TOP_PXS))
                .w(gpui::px(theme.root_pixels() * CARD_WIDTH_REMS))
                .flex()
                .flex_col()
                .rounded(gpui::px(theme.pixels(Radius::Floating.rems())))
                .bg(hsla(theme.palette().panel()))
                .border_1()
                .border_color(hsla(theme.palette().border()))
                .opacity(reveal)
                .child(list),
        )
        .into_any_element()
}

/// The fixed-width column that carries one row's domain glyph, so titles align across domains.
fn domain_column(theme: &crate::theme::Theme, spec: &CommandSpec) -> AnyElement {
    ui::with_role(
        div()
            .w(gpui::px(theme.root_pixels() * DOMAIN_GLYPH_REMS))
            .flex()
            .justify_center(),
        theme,
        Role::Dense,
    )
    .text_color(hsla(theme.palette().text_low()))
    .child(domain_glyph(spec))
    .into_any_element()
}

/// Which domain's glyph one registry row draws.
///
/// The three index commands read the shared registry index rather than the shelf; every other row
/// serves the shelf this window holds.
const fn domain_glyph(spec: &CommandSpec) -> &'static str {
    match spec.id {
        CommandId::IndexSearch | CommandId::PackageVersions | CommandId::PackageProfile => {
            INDEX_GLYPH
        }
        CommandId::Packages
        | CommandId::Add
        | CommandId::Remove
        | CommandId::Show
        | CommandId::Outline
        | CommandId::Resolve
        | CommandId::Search
        | CommandId::Graph
        | CommandId::Health => SHELF_GLYPH,
    }
}

/// Places the selection nib beside the selected row.
///
/// The shell owns the nib's spring and never drives it, so the value rests at zero; the nib
/// derives its offset from the palette cursor and the fixed row height instead, which stays
/// visually exact without the spring.
fn nib_top(selected: usize) -> f32 {
    u16::try_from(selected).map_or(0.0, |offset| f32::from(offset) * ROW_HEIGHT)
}

/// The mark drawn at the tail of every registry row whose command writes the shelf.
fn mutation_mark(theme: &crate::theme::Theme, mutation: Mutation) -> Option<AnyElement> {
    match mutation {
        Mutation::Read => None,
        Mutation::Write => Some(
            ui::with_role(div(), theme, Role::Dense)
                .px(gpui::px(theme.pixels(Space::Tight.rems())))
                .py(gpui::px(theme.pixels(Space::Hair.rems())))
                .rounded(gpui::px(theme.pixels(Radius::Chip.rems())))
                .bg(hsla(Status::Warn.ground(theme.appearance())))
                .text_color(hsla(Status::Warn.color(theme.appearance())))
                .child(WRITE_MARK.to_owned())
                .into_any_element(),
        ),
    }
}
