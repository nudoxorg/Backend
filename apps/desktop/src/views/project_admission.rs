//! Keyboard-first local project admission.
//!
//! The editable spelling exists only inside the CE input. Confirmation admits
//! it into [`LocalProjectId`] before a product intent is emitted, so reducers,
//! persistence, the shelf, and index scheduling share one lossless identity.

use crate::core::LocalProjectId;
use crate::navigation::Intent;
use crate::runtime::UiRootEntity;
use crate::theme::Theme;
use crate::theme::palette::Paint;
use crate::theme::tokens::{Space, TypeScale, space};
use crate::ui::{components, surface, text};
use gpui::prelude::FluentBuilder as _;
use gpui::{Entity, Focusable as _, ParentElement as _, Styled as _, div, px};
use gpui_component::dialog::{Cancel, Confirm, Dialog};
use gpui_component::input::InputState;

/// Builds the CE-owned add-project modal around one ephemeral edit buffer.
pub(super) fn dialog(
    dialog: Dialog,
    theme: &Theme,
    owner: Entity<UiRootEntity>,
    input: Entity<InputState>,
    width: f32,
) -> Dialog {
    let content_owner = owner.clone();
    let content_input = input.clone();
    let browse_owner = owner.clone();
    let confirm_input = input.clone();
    let confirm_owner = owner.clone();
    let close_owner = owner;
    let footer = div()
        .flex()
        .flex_wrap()
        .items_center()
        .justify_end()
        .gap(space(Space::Snug))
        .child(
            components::button(
                theme,
                "add-project-browse",
                "Browse…",
                components::Weight::Quiet,
            )
            .on_click(move |_, window, app| {
                browse_owner.update(app, |root, cx| {
                    root.queue(Intent::OpenFolderPicker, cx);
                });
                window.dispatch_action(Box::new(Cancel), app);
            }),
        )
        .child(
            components::button(
                theme,
                "add-project-cancel",
                "Cancel",
                components::Weight::Regular,
            )
            .on_click(|_, window, app| {
                window.dispatch_action(Box::new(Cancel), app);
            }),
        )
        .child(
            components::button(
                theme,
                "add-project-confirm",
                "Add project",
                components::Weight::Primary,
            )
            .on_click(|_, window, app| {
                window.dispatch_action(Box::new(Confirm { secondary: false }), app);
            }),
        );

    dialog
        .title("Add a local project")
        .w(px(width))
        .max_w(px(720.0))
        .content(move |content, window, app| {
            let theme = crate::theme::theme(app);
            let snapshot = content_owner.read(app).snapshot();
            let value = content_input.read(app).value();
            let focused = content_input
                .read(app)
                .focus_handle(app)
                .is_focused(window);
            content
                .gap(space(Space::Base))
                .child(
                    text::body(&theme).child(
                        "Choose a source folder. Nudox keeps its source and indexes on this machine, then reuses versioned deltas when files change.",
                    ),
                )
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .gap(space(Space::Tight))
                        .child(
                            text::text_at(&theme, TypeScale::Small, Paint::Silver2)
                                .child("Project folder"),
                        )
                        .child(
                            components::input_with_state(
                                &theme,
                                &content_input,
                                "add-project-path",
                                "Project folder path",
                                value,
                                focused,
                            )
                            .w_full(),
                        )
                        .child(
                            text::faint(&theme)
                                .child("Paste a path and press Enter, or use Browse for the native picker."),
                        ),
                )
                .when_some(snapshot.workspace().path_error.clone(), |content, error| {
                    content.child(
                        surface::sunken(&theme)
                            .p(space(Space::Base))
                            .border_color(theme.paint(Paint::Stopped))
                            .child(
                                text::text_at(&theme, TypeScale::Small, Paint::Stopped)
                                    .child(error.to_string()),
                            ),
                    )
                })
        })
        .footer(footer)
        .on_ok(move |_, _, app| {
            let value = confirm_input.read(app).value();
            match admit_typed_path(value.as_ref()) {
                Ok(project) => {
                    confirm_owner.update(app, |root, cx| {
                        root.queue(Intent::AddProject { project }, cx);
                    });
                    true
                }
                Err(message) => {
                    confirm_owner.update(app, |root, cx| {
                        root.queue(Intent::RejectProjectPath { message }, cx);
                    });
                    false
                }
            }
        })
        .on_close(move |_, _, app| {
            close_owner.update(app, |root, cx| {
                root.queue(Intent::DismissOverlay, cx);
            });
        })
}

fn admit_typed_path(value: &str) -> Result<LocalProjectId, std::sync::Arc<str>> {
    let value = value.trim();
    if value.is_empty() {
        return Err("Enter a project folder path.".into());
    }
    LocalProjectId::from_path(std::path::Path::new(value))
        .map_err(|_| "That folder path cannot be represented on this platform.".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn typed_admission_trims_presentation_without_losing_native_identity() {
        let admitted = admit_typed_path("  /tmp/nudox-project  ").expect("typed path");
        assert_eq!(
            admitted.path(),
            std::path::PathBuf::from("/tmp/nudox-project")
        );
    }

    #[test]
    fn blank_project_path_never_crosses_the_identity_boundary() {
        assert_eq!(
            admit_typed_path("  \n ").expect_err("blank path"),
            std::sync::Arc::<str>::from("Enter a project folder path.")
        );
    }
}
