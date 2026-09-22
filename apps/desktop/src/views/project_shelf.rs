//! Project shelf content.
//!
//! The shell owns placement and responsive width. This module owns the row
//! semantics: selecting a local project changes the served workspace, and all
//! lifecycle controls emit typed intents. It deliberately contains no width
//! constants so the canonical [`crate::core::ResponsiveLayout`] remains the
//! single source of responsive geometry.

use crate::model::{AppSnapshot, ProjectPhase};
use crate::navigation::{Intent, OrbitRoute, PackageLane, PackageRoute, Route};
use crate::runtime::UiRootEntity;
use crate::theme::Theme;
use crate::theme::palette::Paint;
use crate::theme::tokens::{Space, TypeScale, space, type_size};
use crate::ui::{components, surface, text};
use gpui::prelude::FluentBuilder as _;
use gpui::{AnyElement, Context, IntoElement, ParentElement, Styled, div, px};
use gpui_component::Selectable as _;

/// Builds shelf contents for either the expanded shelf or the narrow rail.
///
/// The caller supplies the responsive mode resolved by the shell. Keeping the
/// mode as a value makes component stress states deterministic and avoids a
/// second width calculation inside the row renderer.
pub(super) fn panel(
    theme: &Theme,
    snapshot: &AppSnapshot,
    expanded: bool,
    cx: &mut Context<UiRootEntity>,
) -> impl IntoElement {
    let rows = snapshot
        .shelf()
        .items
        .iter()
        .enumerate()
        .map(|(index, item)| row(theme, snapshot, expanded, index, item, cx))
        .collect::<Vec<_>>();
    let add = components::measure(
        theme,
        "add-project",
        components::button_with_state(
            theme,
            "add-project",
            "+ Add project",
            components::Weight::Primary,
            false,
            true,
        )
        .on_click(cx.listener(|this, _, _, cx| {
            this.queue(Intent::OpenFolderPicker, cx);
        })),
    );

    div()
        .flex_none()
        .min_h(px(0.0))
        .flex()
        .flex_col()
        .gap(space(Space::Base))
        .p(space(Space::Gutter))
        .border_r(px(1.0))
        .border_color(theme.paint(Paint::Rule2))
        .bg(theme.paint(Paint::Abyss1))
        .child(
            div()
                .text_size(type_size(TypeScale::Small))
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .text_color(theme.paint(Paint::Silver2))
                .child(if expanded { "SHELF" } else { "N" }),
        )
        .children(rows)
        .child(div().flex_1())
        .child(add)
}

fn row(
    theme: &Theme,
    snapshot: &AppSnapshot,
    expanded: bool,
    index: usize,
    item: &crate::model::ShelfItem,
    cx: &mut Context<UiRootEntity>,
) -> AnyElement {
    let identity = item.identity.clone();
    let route = match identity.clone() {
        crate::core::ResourceIdentity::Project(project) => {
            Route::Orbit(OrbitRoute::Project(project))
        }
        crate::core::ResourceIdentity::Local(_) => Route::Orbit(OrbitRoute::Home),
        crate::core::ResourceIdentity::Package(package) => Route::Package(PackageRoute {
            project: None,
            package,
            lane: PackageLane::Overview,
            selected: Some(item.object),
        }),
    };
    let local = match &identity {
        crate::core::ResourceIdentity::Local(project) => Some(project.clone()),
        _ => None,
    };
    let status = local
        .as_ref()
        .and_then(|project| {
            snapshot
                .workspace()
                .projects
                .iter()
                .find(|candidate| candidate.id == *project)
        })
        .map(status)
        .unwrap_or_else(|| "Registry".to_owned());
    let indexing = local.as_ref().is_some_and(|project| {
        snapshot.workspace().projects.iter().any(|candidate| {
            candidate.id == *project
                && matches!(
                    candidate.phase,
                    ProjectPhase::Indexing | ProjectPhase::Cancelling
                )
        })
    });
    let selected = snapshot.shelf().selected.as_ref() == Some(&identity);
    let activate = local.clone();
    let reveal = local.clone();
    let retry = local.clone();
    let cancel = local.clone();
    let remove = local;
    // Keep the full label in the CE button even in rail mode. The canonical
    // shell may visually elide it, but the control's accessible name remains
    // the project the user is selecting.
    let label = item.label.to_string();
    let shelf_id = format!("shelf-{index}");
    let mut row = div()
        .flex()
        .min_w(px(0.0))
        .items_center()
        .gap(space(Space::Tight))
        .child(components::measure(
            theme,
            shelf_id,
            components::button_with_state(
                theme,
                format!("shelf-{index}"),
                label,
                components::Weight::Quiet,
                false,
                true,
            )
            .flex_1()
            .min_w(px(0.0))
            .selected(selected)
            .on_click(cx.listener(move |this, _, _, cx| {
                if let Some(project) = activate.clone() {
                    this.queue(Intent::ActivateProject(project), cx);
                }
                this.queue(Intent::Navigate(route.clone()), cx);
            })),
        ));
    if expanded {
        row = row.child(
            text::single_line(text::faint(theme))
                .flex_1()
                .min_w(px(0.0))
                .child(status),
        );
        let reveal_button = components::measure(
            theme,
            format!("shelf-reveal-{index}"),
            components::button_with_state(
                theme,
                format!("shelf-reveal-{index}"),
                "Reveal",
                components::Weight::Quiet,
                reveal.is_none(),
                true,
            )
            .on_click(cx.listener(move |this, _, _, cx| {
                if let Some(project) = reveal.clone() {
                    this.queue(Intent::RevealProject(project), cx);
                }
            })),
        );
        row = row.child(reveal_button);
        let lifecycle_id = if indexing {
            format!("shelf-cancel-{index}")
        } else {
            format!("shelf-retry-{index}")
        };
        let lifecycle = if indexing {
            components::button_with_state(
                theme,
                lifecycle_id.clone(),
                "Cancel",
                components::Weight::Quiet,
                cancel.is_none(),
                true,
            )
            .on_click(cx.listener(move |this, _, _, cx| {
                if let Some(project) = cancel.clone() {
                    this.queue(Intent::CancelIndex(project), cx);
                }
            }))
        } else {
            components::button_with_state(
                theme,
                lifecycle_id.clone(),
                "Retry",
                components::Weight::Quiet,
                retry.is_none(),
                true,
            )
            .on_click(cx.listener(move |this, _, _, cx| {
                if let Some(project) = retry.clone() {
                    this.queue(Intent::RetryIndex(project), cx);
                }
            }))
        };
        row = row
            .child(components::measure(theme, lifecycle_id, lifecycle))
            .child(components::measure(
                theme,
                format!("shelf-remove-{index}"),
                components::button_with_state(
                    theme,
                    format!("shelf-remove-{index}"),
                    "Remove",
                    components::Weight::Quiet,
                    remove.is_none(),
                    true,
                )
                .on_click(cx.listener(move |this, _, _, cx| {
                    if let Some(project) = remove.clone() {
                        this.queue(Intent::RemoveProject(project), cx);
                    }
                })),
            ));
    }
    row.into_any_element()
}

fn status(project: &crate::model::WorkspaceProject) -> String {
    match project.phase {
        ProjectPhase::Indexing => project.progress.map_or_else(
            || "Indexing…".to_owned(),
            |value| format!("Indexing {value}%"),
        ),
        ProjectPhase::Cancelling => "Cancelling…".to_owned(),
        ProjectPhase::Ready => project.files_indexed.map_or_else(
            || "Ready".to_owned(),
            |files| format!("Ready · {files} files"),
        ),
        _ if project.error.is_some() => format!(
            "{} · {}",
            project.phase.label(),
            project.error.as_deref().unwrap_or_default()
        ),
        _ => project.phase.label().to_owned(),
    }
}
