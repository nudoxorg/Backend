//! CE-owned command palette adapter.
//!
//! The shell owns the typed `Overlay` route and action-to-intent mapping. The
//! editable query, keyboard selection, filtering, focus, and list rendering
//! stay inside `gpui_component::command::CommandState`; this module only
//! projects the current snapshot into CE command entries.

use crate::model::AppSnapshot;
use crate::navigation::{ActionId, Intent};
use crate::runtime::UiRootEntity;
use crate::theme::palette::Paint;
use crate::theme::tokens::{space, Space};
use crate::theme::Theme;
use crate::ui::{components, text};
use gpui::prelude::FluentBuilder as _;
use gpui::{div, px, AnyElement, App, Context, ElementId, IntoElement, ParentElement, Styled};
use gpui_component::command::{Command, CommandGroup, CommandItem, CommandState};
use gpui_component::dialog::Dialog;
use gpui_component::IndexPath;

/// Builds the compact search trigger in the shell header.
pub(crate) fn header_trigger(
    theme: &Theme,
    root: &mut UiRootEntity,
    cx: &mut Context<UiRootEntity>,
) -> impl IntoElement {
    let _ = root;
    let shortcut = ActionId::OpenCommandPalette
        .spec()
        .shortcut
        .map(|value| value.as_str().replace("cmd-", "⌘").to_ascii_uppercase())
        .unwrap_or_else(|| "⌘K".to_owned());
    components::button_with_state_and_accessible(
        theme,
        "command-palette",
        format!("Search all docs…  {shortcut}"),
        "Search all documentation and workspace actions",
        components::Weight::Regular,
        false,
        true,
    )
    .on_click(cx.listener(|this, _, _, cx| {
        this.queue(Intent::OpenCommandPalette, cx);
    }))
}

/// Builds the CE dialog content for the command palette.
pub(crate) fn dialog(dialog: Dialog, owner: gpui::Entity<UiRootEntity>, width: f32) -> Dialog {
    let owner_for_content = owner.clone();
    dialog
        .title("Search workspace")
        .w(px(width))
        .max_w(px(920.0))
        .content(move |content, window, app| {
            let state = window.use_keyed_state("nudox-command-palette", app, |window, cx| {
                CommandState::new(window, cx)
            });
            let snapshot = owner_for_content.read(app).snapshot();
            let targets = targets(&snapshot);
            let target_rows = targets.clone();
            let target_owner = owner_for_content.clone();
            let groups = command_groups(&snapshot);
            let command = groups
                .into_iter()
                .fold(Command::new(&state), |command, group| command.group(group))
                .placeholder("Search packages, projects, or actions")
                .on_confirm(move |path: IndexPath, _, app| {
                    if let Some(target) = target_rows
                        .get(path.section)
                        .and_then(|rows| rows.get(path.row))
                    {
                        activate(target.clone(), &target_owner, app);
                    }
                })
                .on_cancel({
                    let owner = owner_for_content.clone();
                    move |_, app| {
                        owner.update(app, |root, cx| root.queue(Intent::DismissOverlay, cx));
                    }
                })
                .header({
                    move |_, _, app| {
                        let theme = crate::theme::theme(app);
                        palette_status(&theme, &snapshot)
                    }
                })
                .footer({
                    move |_, _, app| {
                        let theme = crate::theme::theme(app);
                        palette_footer(&theme)
                    }
                })
                .bordered(false)
                .max_h(px(520.0));
            content
                .id(ElementId::Name("search-palette-content".into()))
                .p_0()
                .child(command)
        })
        .on_close(move |_, _, app| {
            owner.update(app, |root, cx| root.queue(Intent::DismissOverlay, cx));
        })
}

#[derive(Clone)]
enum Target {
    Project(crate::core::LocalProjectId),
    Action(ActionId),
}

fn targets(snapshot: &AppSnapshot) -> Vec<Vec<Target>> {
    let projects = snapshot
        .workspace()
        .projects
        .iter()
        .filter_map(|project| {
            crate::core::LocalProjectId::new(project.path.as_ref())
                .ok()
                .map(Target::Project)
        })
        .collect::<Vec<_>>();
    let actions = ActionId::ALL
        .into_iter()
        .filter(|action| action.intent().is_some())
        .map(Target::Action)
        .collect::<Vec<_>>();
    vec![projects, actions]
}

fn command_groups(snapshot: &AppSnapshot) -> Vec<CommandGroup> {
    let projects = snapshot
        .workspace()
        .projects
        .iter()
        .filter_map(|project| {
            crate::core::LocalProjectId::new(project.path.as_ref())
                .ok()
                .map(|_| {
                    CommandItem::new()
                        .label(project.label.clone())
                        .keywords([project.path.clone()])
                        .disabled(project.phase == crate::model::ProjectPhase::Missing)
                })
        })
        .collect::<Vec<_>>();
    let actions = ActionId::ALL
        .into_iter()
        .filter_map(|action| {
            let spec = action.spec();
            action.intent().map(|_| {
                CommandItem::new()
                    .label(spec.label)
                    .keywords([action.as_str()])
            })
        })
        .collect::<Vec<_>>();
    vec![
        CommandGroup::new().label("Projects").items(projects),
        CommandGroup::new().label("Actions").items(actions),
    ]
}

fn activate(target: Target, owner: &gpui::Entity<UiRootEntity>, app: &mut App) {
    owner.update(app, |root, cx| match target {
        Target::Project(project) => root.queue(Intent::ActivateProject(project), cx),
        Target::Action(action) => root.queue(Intent::Action(action), cx),
    });
}

fn palette_status(theme: &Theme, snapshot: &AppSnapshot) -> AnyElement {
    let label = if snapshot.workspace().active.is_some() {
        "Workspace index"
    } else {
        "Choose a project to search"
    };
    div()
        .flex()
        .items_center()
        .gap(space(Space::Tight))
        .px(space(Space::Room))
        .py(space(Space::Snug))
        .border_b(px(1.0))
        .border_color(theme.paint(Paint::Rule1))
        .child(div().text_color(theme.paint(Paint::Mint)).child("●"))
        .child(text::single_line(text::faint(theme)).child(label))
        .into_any_element()
}

fn palette_footer(theme: &Theme) -> AnyElement {
    div()
        .flex()
        .items_center()
        .gap(space(Space::Gutter))
        .px(space(Space::Room))
        .py(space(Space::Snug))
        .border_t(px(1.0))
        .border_color(theme.paint(Paint::Rule1))
        .bg(theme.paint(Paint::Abyss1))
        .child(text::single_line(text::faint(theme)).child("↑↓ move"))
        .child(text::single_line(text::faint(theme)).child("↩ open"))
        .child(text::single_line(text::faint(theme)).child("esc close"))
        .into_any_element()
}
