//! Defines the omnibar for `interface-gui`.
//! This module owns the one field and its mode, scope, and submission chrome.
//! Its narrow surface is a single bar the whole window addresses through one focus handle.

use gpui::{AnyElement, Context, InteractiveElement, IntoElement, Styled, div, prelude::*};

use crate::app::{FieldTarget, Workspace};
use crate::store::search::OmnibarMode;
use crate::theme::{Role, Space};
use crate::ui::{self, hsla};

/// The glyph shown while the field drives the command registry.
const COMMAND_GLYPH: &str = ">";

/// The glyph shown while the field drives a search, scoped or not.
const SEARCH_GLYPH: &str = "⌕";

/// The glyph shown while the field drives the registry index.
const INDEX_GLYPH: &str = "#";

/// The placeholder while the registry is the field's job.
const COMMAND_PLACEHOLDER: &str = "run a command";

/// The placeholder while a search is the field's job.
///
/// The `#index` prefix joins this line only when the store's `OmnibarMode` grows the mode that
/// detects it; the field never teaches a prefix the store cannot yet read.
const SEARCH_PLACEHOLDER: &str = "search · @package scope · > commands";

/// The placeholder while the registry index is the field's job.
const INDEX_PLACEHOLDER: &str = "search the registry index";

/// The mode glyph column's width in rems, so the field does not shift when the mode flips.
const MODE_GLYPH_REMS: f32 = 1.5;

/// Draws the omnibar: mode chip, scope chip, the field, the live-search hint, and the scope's
/// dismissal.
pub(crate) fn bar(
    workspace: &Workspace,
    window: &mut gpui::Window,
    cx: &mut Context<Workspace>,
) -> AnyElement {
    let theme = *workspace.theme();
    let focused = workspace.omnibar_focus().is_focused(window);
    let (glyph, placeholder) = match workspace.search().mode() {
        OmnibarMode::Commands { .. } => (COMMAND_GLYPH, COMMAND_PLACEHOLDER),
        OmnibarMode::Index { .. } => (INDEX_GLYPH, INDEX_PLACEHOLDER),
        OmnibarMode::Idle | OmnibarMode::Search { .. } => (SEARCH_GLYPH, SEARCH_PLACEHOLDER),
    };
    let scope_chip = workspace.search().mode().scope().map(|scope| {
        ui::button(
            &theme,
            &format!("in {scope}"),
            false,
            cx.listener(|workspace, _: &gpui::ClickEvent, _, cx| {
                workspace.search_mut().clear_scope();
                cx.notify();
            }),
        )
    });
    div()
        .id("omnibar")
        .key_context("Omnibar")
        .track_focus(workspace.omnibar_focus())
        .flex()
        .items_center()
        .gap(gpui::px(theme.pixels(Space::Tight.rems())))
        .px(gpui::px(theme.pixels(Space::Base.rems())))
        .py(gpui::px(theme.pixels(Space::Tight.rems())))
        .border_b_1()
        .border_color(hsla(theme.palette().divider()))
        .bg(hsla(theme.palette().panel()))
        .child(
            ui::with_role(
                div()
                    .w(gpui::px(theme.root_pixels() * MODE_GLYPH_REMS))
                    .flex()
                    .justify_center(),
                &theme,
                Role::Caption,
            )
            .text_color(hsla(theme.palette().text_low()))
            .child(glyph.to_owned()),
        )
        .children(scope_chip)
        .child(
            ui::field(
                &theme,
                Role::Ui,
                workspace.search().text(),
                placeholder,
                focused,
            )
            .id("omnibar-field"),
        )
        .children(owed_search(workspace).map(|hint| {
            ui::text_low(&theme, Role::Dense, &hint)
                .flex_shrink(0.0)
                .into_any_element()
        }))
        .child(workspace.field_bridge(FieldTarget::Omnibar, workspace.omnibar_focus(), cx))
        .into_any_element()
}

/// The query the field still owes a search for, when the last terminal does not echo it.
///
/// The terminal echoes the text of the request it answered, so a query the echo does not carry
/// means the debounce is armed or a request is in flight; the bar says so in one dense line. The
/// comparison runs against the mode's query rather than the raw field text, which still carries
/// the scope chip a landed search legitimately lacks.
fn owed_search(workspace: &Workspace) -> Option<String> {
    let OmnibarMode::Search { query, .. } = workspace.search().mode() else {
        return None;
    };
    if query.is_empty() {
        return None;
    }
    let terminal = workspace.search().terminal()?;
    (terminal.request.text.as_str() != query.as_ref()).then(|| format!("searching {query}…"))
}
