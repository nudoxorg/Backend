//! Empty-first-launch and project recovery surfaces.
//!
//! Onboarding is a normal route projection backed by the same immutable
//! workspace state as the shelf. The folder picker, service index, and
//! connection setup are emitted as typed intents so a restarted window and a
//! live window follow the same path.

use super::catalog::package_cards;
use super::primitives::heading;
use crate::model::{AppSnapshot, ProjectPhase};
use crate::navigation::{Intent, SettingsPage};
use crate::runtime::UiRootEntity;
use crate::theme::Theme;
use crate::theme::palette::Paint;
use crate::theme::tokens::{Space, TypeScale, space};
use crate::ui::{components, surface, text};
use gpui::prelude::FluentBuilder as _;
use gpui::{AnyElement, Context, IntoElement, ParentElement, Styled, div, px};
use gpui_component::progress::Progress;

/// Renders the home projection for both the empty shelf and an admitted shelf.
pub(super) fn home_page(
    root: &mut UiRootEntity,
    theme: &Theme,
    snapshot: &AppSnapshot,
    cx: &mut Context<UiRootEntity>,
) -> AnyElement {
    let first_launch = snapshot.workspace().projects.is_empty();
    let title = if first_launch {
        "Start with a local project"
    } else {
        "Your documentation shelf"
    };
    let cards = if first_launch {
        surface::sunken(theme)
            .p(px(18.0))
            .child(
                text::single_line(text::faint(theme))
                    .child("Your local index will appear here after the service finishes."),
            )
            .into_any_element()
    } else {
        package_cards(root, theme, snapshot, cx)
    };
    let actions = if first_launch {
        div()
            .flex()
            .flex_wrap()
            .gap(space(Space::Snug))
            .child(
                components::button_with_state(
                    theme,
                    "first-launch-get-started",
                    "Choose a folder…",
                    components::Weight::Primary,
                    false,
                    true,
                )
                .on_click(cx.listener(|this, _, _, cx| {
                    this.queue(Intent::OpenAddProject, cx);
                })),
            )
            .child(
                components::button_with_state(
                    theme,
                    "first-launch-agents",
                    "Connect Claude / MCP",
                    components::Weight::Regular,
                    false,
                    true,
                )
                .on_click(cx.listener(|this, _, _, cx| {
                    this.queue(Intent::OpenSettings(SettingsPage::Agents), cx);
                })),
            )
            .child(
                components::button_with_state(
                    theme,
                    "first-launch-help",
                    "How it works",
                    components::Weight::Quiet,
                    false,
                    true,
                )
                .on_click(cx.listener(|this, _, _, cx| {
                    this.queue(Intent::OpenHelp, cx);
                })),
            )
            .into_any_element()
    } else {
        div().into_any_element()
    };
    let mismatch_banner = workspace_mismatch(theme, snapshot, cx);
    let project_status = active_project_status(theme, snapshot, cx);
    surface::cut(theme, Paint::MintLine)
        .p(px(28.0))
        .flex()
        .flex_col()
        .gap(space(Space::Gutter))
        .child(heading(theme, title))
        .child(text::single_line(text::body(theme)).child(if first_launch {
            "Nudox keeps source and indexes on this machine. Choose a folder and the local index will build in the background."
        } else {
            "Local-first documentation, declarations, source, graph, and registry facts from the live index."
        }))
        .when_some(snapshot.workspace().path_error.clone(), |page, error| {
            page.child(
                surface::sunken(theme)
                    .p(px(12.0))
                    .child(
                        text::single_line(text::body(theme).text_color(theme.paint(Paint::Stopped)))
                            .child(error.to_string()),
                    ),
            )
        })
        .child(actions)
        .child(mismatch_banner)
        .when_some(project_status, |page, status| page.child(status))
        .child(cards)
        .into_any_element()
}

fn workspace_mismatch(
    theme: &Theme,
    snapshot: &AppSnapshot,
    cx: &mut Context<UiRootEntity>,
) -> AnyElement {
    let Some((active, host)) = snapshot
        .workspace()
        .active
        .as_ref()
        .zip(snapshot.workspace().host.as_ref())
        .filter(|(active, host)| active != host)
    else {
        return div().into_any_element();
    };
    let host_project = host.clone();
    let active_label = crate::ui::text::elide(&active.display_lossy(), 54);
    let host_label = crate::ui::text::elide(&host.display_lossy(), 54);
    surface::sunken(theme)
        .p(px(12.0))
        .flex()
        .flex_col()
        .gap(space(Space::Tight))
        .child(
            text::single_line(text::body(theme).text_color(theme.paint(Paint::Stopped))).child(
                "Shelf selection differs from the connected workspace.",
            ),
        )
        .child(text::body(theme).child(format!(
            "Selected: {active_label} · live service: {host_label}. Select the live row before reading results."
        )))
        .child(
            components::button_with_state(
                theme,
                "select-live-workspace",
                "Select live workspace",
                components::Weight::Quiet,
                false,
                true,
            )
            .on_click(cx.listener(move |this, _, _, cx| {
                this.queue(Intent::ActivateProject(host_project.clone()), cx);
            })),
        )
        .into_any_element()
}

fn active_project_status(
    theme: &Theme,
    snapshot: &AppSnapshot,
    cx: &mut Context<UiRootEntity>,
) -> Option<AnyElement> {
    let project = snapshot.workspace().active.as_ref().and_then(|active| {
        snapshot
            .workspace()
            .projects
            .iter()
            .find(|project| project.id == *active)
    })?;
    if project.phase == ProjectPhase::Ready {
        return None;
    }
    let path = project.id.clone();
    let path_label = text::elide(project.path.as_ref(), 64);
    let (headline, detail) = match project.phase {
        ProjectPhase::Indexing => (
            "Indexing this project",
            "The local service is reading the folder. This view becomes ready after the service confirms the index.",
        ),
        ProjectPhase::Cancelling => (
            "Finishing cancellation",
            "The local service is finishing the active ingest. This row will settle when the producer replies.",
        ),
        ProjectPhase::Cancelled => (
            "Indexing paused",
            "The last indexing request was cancelled. Retry when you are ready to continue.",
        ),
        ProjectPhase::Failed => (
            "This project needs attention",
            project
                .error
                .as_deref()
                .unwrap_or("The local service could not finish indexing this folder."),
        ),
        ProjectPhase::Missing => (
            "This folder is unavailable",
            "The saved path is no longer a directory. Choose the folder again to repair this project.",
        ),
        ProjectPhase::Ready => return None,
    };
    let mut card = surface::sunken(theme)
        .p(px(16.0))
        .flex()
        .flex_col()
        .gap(space(Space::Tight))
        .child(text::single_line(text::heading(theme, TypeScale::Interface)).child(headline))
        .child(text::body(theme).child(detail.to_owned()))
        .child(
            text::single_line(text::faint(theme))
                .child(format!("{} · {}", project.label, path_label)),
        );
    match project.phase {
        ProjectPhase::Indexing => {
            let cancel = path.clone();
            card = card
                .child(
                    Progress::new("active-index-progress")
                        .loading(true)
                        .color(theme.paint(Paint::Waiting))
                        .accessibility_label("Indexing local project"),
                )
                .child(
                    components::button_with_state(
                        theme,
                        "cancel-active-index",
                        "Cancel indexing",
                        components::Weight::Quiet,
                        false,
                        true,
                    )
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.queue(Intent::CancelIndex(cancel.clone()), cx);
                    })),
                );
        }
        ProjectPhase::Cancelling => {
            card = card.child(
                Progress::new("cancelling-index-progress")
                    .loading(true)
                    .color(theme.paint(Paint::Waiting))
                    .accessibility_label("Finishing project cancellation"),
            );
        }
        ProjectPhase::Cancelled | ProjectPhase::Failed => {
            let retry = path.clone();
            card = card.child(
                components::button_with_state(
                    theme,
                    "retry-active-index",
                    "Retry indexing",
                    components::Weight::Primary,
                    false,
                    true,
                )
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.queue(Intent::RetryIndex(retry.clone()), cx);
                })),
            );
        }
        ProjectPhase::Missing => {
            card = card.child(
                components::button_with_state(
                    theme,
                    "repair-missing-project",
                    "Choose the folder again",
                    components::Weight::Primary,
                    false,
                    true,
                )
                .on_click(cx.listener(|this, _, _, cx| {
                    this.queue(Intent::OpenAddProject, cx);
                })),
            );
        }
        ProjectPhase::Ready => {}
    }
    Some(card.into_any_element())
}
