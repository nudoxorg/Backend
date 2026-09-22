//! CE-owned workspace settings sheet.
//!
//! The route reducer still owns which settings page is requested and the
//! workspace snapshot remains the only source of values. GPUI CE owns the
//! sheet, settings navigation/search, reset affordances, input focus, list
//! semantics, and dismissal. The small page bodies below are only Nudox
//! projections and typed intent adapters.

use crate::model::{AppSnapshot, ProjectPhase};
use crate::navigation::{Intent, SettingsPage};
use crate::runtime::UiRootEntity;
use crate::theme::Theme;
use crate::theme::palette::Paint;
use crate::theme::tokens::{Space, TypeScale, space};
use crate::ui::{components, surface, text};
use gpui::prelude::FluentBuilder as _;
use gpui::{AnyElement, AppContext as _, Entity, IntoElement, ParentElement, Styled, div, px};
use gpui_component::Sizable as _;
use gpui_component::progress::Progress;
use gpui_component::setting::{SelectIndex, SettingGroup, SettingItem, SettingPage};
use gpui_component::sheet::Sheet;

/// Builds a CE sheet containing the settings adapter.
///
/// Root owns the scrim, focus trap, animation, selection scope, resize handle,
/// and close restoration. `Settings` owns page navigation and its own search
/// input; page content reads the current root entity each time CE renders it,
/// so toggles stay live without a second store.
pub(super) fn sheet(
    sheet: Sheet,
    theme: &Theme,
    owner: Entity<UiRootEntity>,
    initial_page: SettingsPage,
    width: f32,
) -> Sheet {
    let pages = pages(owner.clone());
    let selected_index = SettingsPage::ALL
        .into_iter()
        .position(|candidate| candidate == initial_page)
        .unwrap_or(0);
    let settings_theme = theme.with_action_parent("settings-dialog");
    let settings = components::settings_with_action(
        &settings_theme,
        "workspace-settings",
        "Workspace settings",
        pages,
    )
    .sidebar_width(px(208.0))
    .sidebar_size_range(px(168.0)..px(280.0))
    .default_selected_index(SelectIndex {
        page_ix: selected_index,
        group_ix: None,
    })
    .with_size(gpui_component::Size::Large)
    .size_full();
    let close_owner = owner.clone();
    sheet
        .title(text::heading(theme, TypeScale::Section).child("Settings"))
        .size(px(width))
        .resizable(true)
        .child(settings)
        .on_close(move |_, _, app| {
            close_owner.update(app, |root, cx| root.queue(Intent::DismissOverlay, cx));
        })
}

fn pages(owner: Entity<UiRootEntity>) -> Vec<SettingPage> {
    SettingsPage::ALL
        .into_iter()
        .map(|page| {
            let page_owner = owner.clone();
            SettingPage::new(page_label(page))
                .description(page_description(page))
                .resettable(false)
                .default_open(true)
                .group(
                    SettingGroup::new().title("Nudox").item(
                        SettingItem::render(move |_, _, app| {
                            let snapshot = page_owner.read(app).snapshot();
                            let theme =
                                crate::theme::theme(app).with_action_parent("settings-dialog");
                            page_body(&theme, &snapshot, page, page_owner.clone())
                        })
                        .keywords([page_label(page), page_description(page)]),
                    ),
                )
        })
        .collect()
}

fn page_body(
    theme: &Theme,
    snapshot: &AppSnapshot,
    page: SettingsPage,
    owner: Entity<UiRootEntity>,
) -> AnyElement {
    match page {
        SettingsPage::Appearance => appearance_page(theme, snapshot, owner),
        SettingsPage::Editor => simple_page(
            theme,
            "Editor integrations",
            "Open source files with the configured editor while the local service keeps source identities versioned.",
        ),
        SettingsPage::Agents | SettingsPage::Connections => {
            connections_page(theme, snapshot, owner)
        }
        SettingsPage::Privacy => privacy_page(theme, snapshot, owner),
        SettingsPage::Diagnostics => diagnostics_page(theme, snapshot),
        SettingsPage::Index => index_page(theme, snapshot, owner),
        SettingsPage::Registry => registry_page(theme, snapshot, owner),
        SettingsPage::Legend => simple_page(
            theme,
            "Design language",
            "Mint marks identity and action, periwinkle marks focus, amber marks waiting work, and coral marks a recoverable failure.",
        ),
        SettingsPage::Help => help_page(theme),
    }
}

fn appearance_page(
    theme: &Theme,
    snapshot: &AppSnapshot,
    owner: Entity<UiRootEntity>,
) -> AnyElement {
    let appearance = match snapshot.settings().appearance {
        crate::model::AppearancePreference::Abyss => "Abyss",
        crate::model::AppearancePreference::Glacier => "Glacier",
    };
    let scale = format!("{}%", snapshot.settings().text_scale.percent());
    section(
        theme,
        "Appearance",
        "Durable preferences apply to every window and restart.",
    )
    .child(setting_row(
        theme,
        "Palette",
        appearance,
        Some(queue_button(
            theme,
            owner.clone(),
            "settings-appearance",
            "Switch palette",
            components::Weight::Regular,
            Intent::ToggleAppearance,
        )),
    ))
    .child(setting_row(
        theme,
        "Text scale",
        scale,
        Some(
            div()
                .flex()
                .gap(space(Space::Tight))
                .child(queue_button(
                    theme,
                    owner.clone(),
                    "text-scale-down",
                    "Smaller",
                    components::Weight::Quiet,
                    Intent::SetTextScale { up: false },
                ))
                .child(queue_button(
                    theme,
                    owner.clone(),
                    "text-scale-up",
                    "Larger",
                    components::Weight::Regular,
                    Intent::SetTextScale { up: true },
                )),
        ),
    ))
    .child(setting_row(
        theme,
        "Motion",
        if snapshot.settings().reduced_motion {
            "Reduced motion"
        } else {
            "Full motion"
        },
        Some(queue_button(
            theme,
            owner.clone(),
            "toggle-motion",
            if snapshot.settings().reduced_motion {
                "Enable motion"
            } else {
                "Reduce motion"
            },
            components::Weight::Regular,
            Intent::ToggleReducedMotion,
        )),
    ))
    .child(setting_row(
        theme,
        "Shelf",
        if snapshot.settings().shelf_open {
            "Expanded"
        } else {
            "Collapsed to rail"
        },
        Some(queue_button(
            theme,
            owner,
            "toggle-shelf",
            if snapshot.settings().shelf_open {
                "Collapse shelf"
            } else {
                "Expand shelf"
            },
            components::Weight::Regular,
            Intent::ToggleShelf,
        )),
    ))
    .into_any_element()
}

fn connections_page(
    theme: &Theme,
    snapshot: &AppSnapshot,
    owner: Entity<UiRootEntity>,
) -> AnyElement {
    let workspace = active_workspace(snapshot);
    let command = mcp_command(workspace);
    let config = mcp_config(workspace);
    let workspace_label = workspace
        .map(crate::core::LocalProjectId::display_lossy)
        .unwrap_or_else(|| "Add a project first".to_owned());
    let can_copy = workspace
        .and_then(|project| project.service_coordinate().ok())
        .is_some();
    let connection = match snapshot.settings().connection {
        crate::model::ConnectionStatus::Unknown => "Not tested",
        crate::model::ConnectionStatus::Testing => "Testing local service…",
        crate::model::ConnectionStatus::Connected => "Connected",
        crate::model::ConnectionStatus::Disconnected => "Disconnected",
    };
    section(
        theme,
        "Claude + MCP",
        "The generated setup stays scoped to the selected local workspace and never relies on environment variables.",
    )
    .child(setting_row(
        theme,
        "Workspace",
        workspace_label,
        Some(
            div()
                .flex()
                .gap(space(Space::Tight))
                .child(clipboard_button(
                    theme,
                    "copy-mcp-command",
                    "Copy command",
                    command,
                    components::Weight::Primary,
                    !can_copy,
                ))
                .child(clipboard_button(
                    theme,
                    "copy-mcp-config",
                    "Copy config",
                    config,
                    components::Weight::Quiet,
                    !can_copy,
            )),
        ),
    ))
    .child(setting_row(
        theme,
        "Configured checkout",
        workspace
            .map(crate::core::LocalProjectId::display_lossy)
            .unwrap_or_else(|| "No project selected".to_owned()),
        None::<gpui::Div>,
    ))
    .child(mcp_checkout_mismatch(theme, snapshot, owner.clone()))
    .child(setting_row(
        theme,
        "Connection",
        connection,
        Some(queue_button(
            theme,
            owner.clone(),
            "test-connection",
            "Test connection",
            components::Weight::Regular,
            Intent::TestConnection,
        )),
    ))
    .child(setting_row(
        theme,
        "Local daemon",
        match snapshot.settings().service_mode {
            crate::model::ServiceMode::Embedded => "Embedded in this Nudox process",
            crate::model::ServiceMode::Attached => "Attached to an existing local daemon",
        },
        None::<gpui::Div>,
    ))
    .child(setting_row(
        theme,
        "CLI help",
        "nudox mcp --help · nudox project --help",
        Some(clipboard_button(
            theme,
            "copy-cli-help",
            "Copy help command",
            "nudox mcp --help && nudox project --help".to_owned(),
            components::Weight::Quiet,
            false,
        )),
    ))
    .into_any_element()
}

fn mcp_checkout_mismatch(
    theme: &Theme,
    snapshot: &AppSnapshot,
    owner: Entity<UiRootEntity>,
) -> AnyElement {
    let Some(selected) = snapshot.workspace().active.as_ref() else {
        return div().into_any_element();
    };
    let Some(served) = snapshot.workspace().host.as_ref() else {
        return div().into_any_element();
    };
    if selected == served {
        return div().into_any_element();
    }
    let served_for_action = served.clone();
    surface::sunken(theme)
        .p(px(12.0))
        .flex()
        .flex_col()
        .gap(space(Space::Tight))
        .child(
            text::single_line(text::body(theme).text_color(theme.paint(Paint::Stopped)))
                .child("Configured checkout differs from the live service."),
        )
        .child(text::body(theme).child(format!(
            "Claude will be configured for {}, while the daemon currently serves {}. Switch the served workspace before querying.",
            selected.display_lossy(),
            served.display_lossy(),
        )))
        .child(queue_button(
            theme,
            owner,
            "select-served-checkout",
            "Use served checkout",
            components::Weight::Quiet,
            Intent::ActivateProject(served_for_action),
        ))
        .into_any_element()
}

fn privacy_page(theme: &Theme, snapshot: &AppSnapshot, owner: Entity<UiRootEntity>) -> AnyElement {
    section(
        theme,
        "Privacy and data policy",
        "Source files and local indexes stay on this machine. Registry metadata can be enabled separately.",
    )
    .child(setting_row(
        theme,
        "Policy",
        match snapshot.settings().privacy {
            crate::model::PrivacyPreference::LocalOnly => "Local only",
            crate::model::PrivacyPreference::RegistryMetadata => "Registry metadata allowed",
        },
        Some(queue_button(
            theme,
            owner,
            "toggle-privacy",
            "Change policy",
            components::Weight::Regular,
            Intent::TogglePrivacy,
        )),
    ))
    .into_any_element()
}

fn diagnostics_page(theme: &Theme, snapshot: &AppSnapshot) -> AnyElement {
    let host = snapshot
        .workspace()
        .host
        .as_ref()
        .map(crate::core::LocalProjectId::display_lossy)
        .unwrap_or_else(|| "No local service host".to_owned());
    let active = snapshot
        .workspace()
        .active
        .as_ref()
        .map(crate::core::LocalProjectId::display_lossy)
        .unwrap_or_else(|| "No project selected".to_owned());
    let authority = format!(
        "cursor {} · observation {}",
        snapshot.key().generation(),
        snapshot.key().observation()
    );
    section(
        theme,
        "Diagnostics",
        "These values are read from the current typed runtime and persisted shelf.",
    )
    .child(setting_row(
        theme,
        "Served workspace",
        host,
        None::<gpui::Div>,
    ))
    .child(setting_row(
        theme,
        "Selected workspace",
        active,
        None::<gpui::Div>,
    ))
    .child(setting_row(
        theme,
        "Rebind",
        if snapshot.workspace().requires_rebind() {
            "Required before querying"
        } else {
            "Current"
        },
        None::<gpui::Div>,
    ))
    .child(setting_row(
        theme,
        "Indexed authority",
        authority,
        None::<gpui::Div>,
    ))
    .into_any_element()
}

fn index_page(theme: &Theme, snapshot: &AppSnapshot, owner: Entity<UiRootEntity>) -> AnyElement {
    let mut content = section(
        theme,
        "Projects and index",
        "Choose a folder to add another local project. Selecting a shelf row changes the actual served workspace before queries are admitted.",
    )
    .child(queue_button(
        theme,
        owner.clone(),
        "choose-project",
        "Choose local project…",
        components::Weight::Primary,
        Intent::OpenFolderPicker,
    ));
    if snapshot.workspace().projects.is_empty() {
        content = content.child(
            surface::sunken(theme).p(space(Space::Room)).child(
                text::dim(theme)
                    .child("The shelf is empty. Choose a folder to create the first workspace."),
            ),
        );
    } else {
        let rows = snapshot
            .workspace()
            .projects
            .iter()
            .enumerate()
            .map(|(index, project)| {
                let selected = snapshot.workspace().active.as_ref() == Some(&project.id);
                let project_id = project.id.clone();
                let owner = owner.clone();
                components::list_item_with_state(
                    theme,
                    format!("settings-project-{index}"),
                    project.label.clone(),
                    project.phase != ProjectPhase::Missing,
                    true,
                    selected,
                )
                .on_click(move |_, _, app| {
                    owner.update(app, |root, cx| {
                        root.queue(Intent::ActivateProject(project_id.clone()), cx)
                    });
                })
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .gap(space(Space::Tight))
                        .child(text::label(theme).child(project.label.clone()))
                        .child(text::faint(theme).child(project.path.clone())),
                )
                .into_any_element()
            });
        content = content.child(surface::panel(theme).p(space(Space::Tight)).children(rows));
    }
    let active = snapshot
        .workspace()
        .active
        .as_deref()
        .unwrap_or("No active workspace");
    content
        .child(setting_row(
            theme,
            "Active workspace",
            active,
            None::<gpui::Div>,
        ))
        .child(index_status(theme, snapshot))
        .into_any_element()
}

fn registry_page(theme: &Theme, snapshot: &AppSnapshot, owner: Entity<UiRootEntity>) -> AnyElement {
    section(
        theme,
        "Registry and cache",
        "Advisories and immutable cache retention are explicit preferences over producer-owned registry facts.",
    )
    .child(setting_row(
        theme,
        "Advisories",
        if snapshot.settings().advisories {
            "Enabled"
        } else {
            "Disabled"
        },
        Some(queue_button(
            theme,
            owner.clone(),
            "toggle-advisories",
            "Toggle advisories",
            components::Weight::Regular,
            Intent::ToggleAdvisories,
        )),
    ))
    .child(setting_row(
        theme,
        "Immutable cache",
        if snapshot.settings().cache_enabled {
            format!("Enabled · {} days", snapshot.settings().cache_days)
        } else {
            "Disabled".to_owned()
        },
        Some(
            div()
                .flex()
                .gap(space(Space::Tight))
                .child(queue_button(
                    theme,
                    owner.clone(),
                    "toggle-cache",
                    "Toggle cache",
                    components::Weight::Quiet,
                    Intent::ToggleCache,
                ))
                .child(queue_button(
                    theme,
                    owner.clone(),
                    "cache-days-down",
                    "Shorter",
                    components::Weight::Quiet,
                    Intent::SetCacheDays { up: false },
                ))
                .child(queue_button(
                    theme,
                    owner,
                    "cache-days-up",
                    "Longer",
                    components::Weight::Quiet,
                    Intent::SetCacheDays { up: true },
                )),
        ),
    ))
    .into_any_element()
}

fn help_page(theme: &Theme) -> AnyElement {
    section(
        theme,
        "Keyboard and CLI help",
        "Every control remains reachable through native CE focus traversal, including the collapsed shelf and settings search.",
    )
    .children(
        [
            ("⌘N", "Add project"),
            ("⌘,", "Open settings"),
            ("⌘⇧P", "Open command palette"),
            ("⌘⇧M", "Toggle reduced motion"),
            ("Esc", "Dismiss the active CE surface"),
        ]
        .into_iter()
        .map(|(key, label)| setting_row(theme, key, label, None::<gpui::Div>)),
    )
    .into_any_element()
}

fn simple_page(theme: &Theme, title: &'static str, body: &'static str) -> AnyElement {
    section(theme, title, body).into_any_element()
}

fn section(theme: &Theme, title: &str, description: &str) -> gpui::Div {
    surface::cut(theme, Paint::MintLine)
        .p(space(Space::Room))
        .flex()
        .flex_col()
        .gap(space(Space::Room))
        .child(text::heading(theme, TypeScale::Section).child(title.to_owned()))
        .child(text::body(theme).child(description.to_owned()))
}

fn setting_row(
    theme: &Theme,
    label: &str,
    value: impl Into<gpui::SharedString>,
    control: Option<impl IntoElement>,
) -> gpui::Div {
    let mut row = surface::panel(theme)
        .p(px(14.0))
        .flex()
        .items_start()
        .justify_between()
        .gap(space(Space::Room))
        .child(
            div()
                .flex()
                .flex_col()
                .gap(space(Space::Tight))
                .flex_1()
                .min_w(px(0.0))
                .child(text::label(theme).child(label.to_owned()))
                .child(text::dim(theme).child(value.into())),
        );
    if let Some(control) = control {
        row = row.child(div().flex_none().child(control));
    }
    row
}

fn index_status(theme: &Theme, snapshot: &AppSnapshot) -> impl IntoElement {
    let status = snapshot
        .workspace()
        .active
        .as_ref()
        .and_then(|active| {
            snapshot
                .workspace()
                .projects
                .iter()
                .find(|p| p.id == *active)
        })
        .map_or("No project selected".to_owned(), |project| {
            project
                .error
                .as_deref()
                .map_or_else(|| project.phase.label().to_owned(), ToOwned::to_owned)
        });
    surface::sunken(theme)
        .p(space(Space::Room))
        .flex()
        .flex_col()
        .gap(space(Space::Tight))
        .child(text::label(theme).child("Index status"))
        .child(text::dim(theme).child(status))
        .when(
            snapshot
                .workspace()
                .active
                .as_ref()
                .and_then(|active| {
                    snapshot
                        .workspace()
                        .projects
                        .iter()
                        .find(|p| p.id == *active)
                })
                .is_some_and(|project| {
                    matches!(
                        project.phase,
                        ProjectPhase::Indexing | ProjectPhase::Cancelling
                    )
                }),
            |this| {
                this.child(
                    Progress::new("settings-index-progress")
                        .loading(true)
                        .color(theme.paint(Paint::Waiting))
                        .accessibility_label("Indexing local project"),
                )
            },
        )
}

fn queue_button(
    theme: &Theme,
    owner: Entity<UiRootEntity>,
    id: impl Into<gpui::SharedString>,
    label: impl Into<gpui::SharedString>,
    weight: components::Weight,
    intent: Intent,
) -> gpui::Div {
    let id = id.into();
    let button = components::button_with_state(theme, id.clone(), label, weight, false, true)
        .on_click(move |_, _, app| {
            owner.update(app, |root, cx| root.queue(intent.clone(), cx));
        });
    components::measure(theme, id, button)
}

fn clipboard_button(
    theme: &Theme,
    id: &'static str,
    label: &'static str,
    value: String,
    weight: components::Weight,
    disabled: bool,
) -> gpui::Div {
    let button = components::button_with_state(theme, id, label, weight, disabled, true).on_click(
        move |_, _, app| {
            app.write_to_clipboard(gpui::ClipboardItem::new_string(value.clone()));
        },
    );
    components::measure(theme, id, button)
}

fn active_workspace(snapshot: &AppSnapshot) -> Option<&crate::core::LocalProjectId> {
    let active = snapshot.workspace().active.as_ref()?;
    snapshot
        .workspace()
        .projects
        .iter()
        .find(|project| project.id == *active && project.phase != ProjectPhase::Missing)
        .map(|project| &project.id)
}

fn mcp_command(project: Option<&crate::core::LocalProjectId>) -> String {
    let Some(project) = project else {
        return "Add a project before copying the Claude command.".to_owned();
    };
    let Ok(path) = project.service_coordinate() else {
        return "The selected folder cannot be represented by the CLI on this platform.".to_owned();
    };
    format!("nudox mcp --workspace {}", shell_quote(path))
}

fn mcp_config(project: Option<&crate::core::LocalProjectId>) -> String {
    let Some(project) = project else {
        return "Add a project before copying the Claude config.".to_owned();
    };
    let Ok(path) = project.service_coordinate() else {
        return "The selected folder cannot be represented by the CLI on this platform.".to_owned();
    };
    format!(
        "{{\n  \"mcpServers\": {{\n    \"nudox\": {{\n      \"command\": \"nudox\",\n      \"args\": [\"mcp\", \"--workspace\", {}]\n    }}\n  }}\n}}",
        json_quote(path)
    )
}

fn json_quote(value: &str) -> String {
    let mut quoted = String::with_capacity(value.len().saturating_add(2));
    quoted.push('"');
    for character in value.chars() {
        match character {
            '"' => quoted.push_str("\\\""),
            '\\' => quoted.push_str("\\\\"),
            '\n' => quoted.push_str("\\n"),
            '\r' => quoted.push_str("\\r"),
            '\t' => quoted.push_str("\\t"),
            character if character.is_control() => {
                use std::fmt::Write as _;
                let _ = write!(quoted, "\\u{:04x}", character as u32);
            }
            character => quoted.push(character),
        }
    }
    quoted.push('"');
    quoted
}

#[cfg(not(target_os = "windows"))]
fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

#[cfg(target_os = "windows")]
fn shell_quote(value: &str) -> String {
    format!("\"{}\"", value.replace('"', "\\\""))
}

fn page_label(page: SettingsPage) -> &'static str {
    match page {
        SettingsPage::Appearance => "Appearance",
        SettingsPage::Editor => "Editor",
        SettingsPage::Agents => "Agents",
        SettingsPage::Connections => "Connections",
        SettingsPage::Privacy => "Privacy",
        SettingsPage::Diagnostics => "Diagnostics",
        SettingsPage::Index => "Projects & index",
        SettingsPage::Registry => "Registry",
        SettingsPage::Legend => "Legend",
        SettingsPage::Help => "Help",
    }
}

fn page_description(page: SettingsPage) -> &'static str {
    match page {
        SettingsPage::Appearance => "Theme, scale, motion, and shelf layout",
        SettingsPage::Editor => "Local editor integration",
        SettingsPage::Agents => "Claude and MCP setup",
        SettingsPage::Connections => "Connection and daemon status",
        SettingsPage::Privacy => "Local versus registry metadata policy",
        SettingsPage::Diagnostics => "Runtime and workspace authority",
        SettingsPage::Index => "Projects, folders, and index lifecycle",
        SettingsPage::Registry => "Advisories and immutable cache",
        SettingsPage::Legend => "Nudox visual language",
        SettingsPage::Help => "Keyboard shortcuts and CLI commands",
    }
}
