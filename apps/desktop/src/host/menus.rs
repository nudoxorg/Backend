//! Native application menus and their typed GPUI actions.

use gpui::{App, AppContext as _, KeyBinding, Menu, MenuItem, OsAction, SystemMenuType, actions};
use crate::navigation::{Intent, SettingsPage};
use crate::runtime::UiEntityGraph;

actions!(
    nudox_menu,
    [
        About,
        Quit,
        Hide,
        HideOthers,
        ShowAll,
        CloseWindow,
        MinimizeWindow,
        ZoomWindow,
    ]
);

/// Installs the application menu and native window lifecycle actions.
pub(crate) fn install(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("cmd-q", Quit, None),
        KeyBinding::new("cmd-h", Hide, None),
        KeyBinding::new("cmd-alt-h", HideOthers, None),
        KeyBinding::new("cmd-w", CloseWindow, None),
        KeyBinding::new("cmd-m", MinimizeWindow, None),
    ]);
    cx.on_action(|_: &Quit, cx| cx.quit());
    cx.on_action(|_: &Hide, cx| cx.hide());
    cx.on_action(|_: &HideOthers, cx| cx.hide_other_apps());
    cx.on_action(|_: &ShowAll, cx| cx.unhide_other_apps());
    cx.on_action(|_: &CloseWindow, cx| {
        if let Some(window) = cx.active_window() {
            let _ = window.update(cx, |_, window, _| window.remove_window());
        }
    });
    cx.on_action(|_: &MinimizeWindow, cx| {
        if let Some(window) = cx.active_window() {
            let _ = window.update(cx, |_, window, _| window.minimize_window());
        }
    });
    cx.on_action(|_: &ZoomWindow, cx| {
        if let Some(window) = cx.active_window() {
            let _ = window.update(cx, |_, window, _| window.zoom_window());
        }
    });

    cx.set_menus([
        Menu::new("Nudox").items([
            MenuItem::action("About Nudox", About),
            MenuItem::action("Settings…", crate::shell::OpenSettingsAction),
            MenuItem::separator(),
            MenuItem::os_submenu("Services", SystemMenuType::Services),
            MenuItem::separator(),
            MenuItem::action("Hide Nudox", Hide),
            MenuItem::action("Hide Others", HideOthers),
            MenuItem::action("Show All", ShowAll),
            MenuItem::separator(),
            MenuItem::action("Quit Nudox", Quit),
        ]),
        Menu::new("File").items([
            MenuItem::action("Add a Folder…", crate::shell::AddFolderAction),
            MenuItem::separator(),
            MenuItem::action("Close Window", CloseWindow),
        ]),
        Menu::new("Edit").items([
            MenuItem::os_action("Undo", gpui_component::input::Undo, OsAction::Undo),
            MenuItem::os_action("Redo", gpui_component::input::Redo, OsAction::Redo),
            MenuItem::separator(),
            MenuItem::os_action("Cut", gpui_component::input::Cut, OsAction::Cut),
            MenuItem::os_action("Copy", gpui_component::input::Copy, OsAction::Copy),
            MenuItem::os_action("Paste", gpui_component::input::Paste, OsAction::Paste),
            MenuItem::separator(),
            MenuItem::os_action(
                "Select All",
                gpui_component::input::SelectAll,
                OsAction::SelectAll,
            ),
        ]),
        Menu::new("Window").items([
            MenuItem::action("Minimize", MinimizeWindow),
            MenuItem::action("Zoom", ZoomWindow),
            MenuItem::separator(),
            MenuItem::action("Close Window", CloseWindow),
        ]),
    ]);

    cx.on_window_closed(|cx, _| {
        if cx.windows().is_empty() {
            cx.quit();
        }
    })
    .detach();
}

/// App-menu commands are local application actions. They must validate and
/// dispatch before a shell focus handle has entered the rendered dispatch
/// tree, and after owner failure has replaced the Reader's content. The
/// shell's handlers still own focused-window shortcuts; these App handlers
/// are the menu/focus-independent fallback over the same typed intents.
pub(crate) fn install_local_actions(graph: &UiEntityGraph, cx: &mut App) {
    let about = graph.root.downgrade();
    cx.on_action(move |_: &About, cx| {
        if let Some(root) = about.upgrade() {
            root.update(cx, |root, cx| root.queue(Intent::OpenSettings(SettingsPage::About), cx));
        }
    });
    let settings = graph.root.downgrade();
    cx.on_action(move |_: &crate::shell::OpenSettingsAction, cx| {
        if let Some(root) = settings.upgrade() {
            root.update(cx, |root, cx| root.queue(Intent::OpenSettings(SettingsPage::Appearance), cx));
        }
    });
    let add = graph.root.downgrade();
    cx.on_action(move |_: &crate::shell::AddFolderAction, cx| {
        if let Some(root) = add.upgrade() {
            root.update(cx, |root, cx| root.queue(Intent::OpenAddProject, cx));
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::ServiceMode;
    use crate::navigation::Overlay;
    use crate::runtime::owner::{OwnerFault, OwnerGate, OwnerState};
    use crate::runtime::reads::ReadPool;
    use crate::shell::tests::{Fixture, Rig, RootOnly, native_bounds_id, rig_with_engine_gate};
    use gpui::{AppContext as _, TestAppContext};
    use std::sync::Arc;

    fn menu_available(rig: &mut Rig) -> (bool, bool, bool) {
        rig.cx.cx.update(|cx| (
            cx.is_action_available(&About),
            cx.is_action_available(&crate::shell::OpenSettingsAction),
            cx.is_action_available(&crate::shell::AddFolderAction),
        ))
    }

    #[gpui::test]
    fn cold_and_recovered_home_keep_local_native_menu_actions_available(cx: &mut TestAppContext) {
        let gate = OwnerGate::starting();
        let mut rig = rig_with_engine_gate(cx, None, 1440.0, 900.0,
            ReadPool::start(2, |_| Fixture).expect("reader"), RootOnly, Some(gate.clone()));
        rig.cx.cx.update(|cx| install_local_actions(&rig.graph, cx));
        // macOS validates against App::is_action_available before any field
        // click. The app menu must not depend on Shell's focus node having
        // entered the latest rendered dispatch tree.
        rig.cx.update(|window, _| window.blur());
        assert_eq!(menu_available(&mut rig), (true, true, true));
        gate.publish(OwnerState::Failed(OwnerFault::Host(Arc::from("owner unavailable"))));
        rig.settle();
        rig.cx.update(|window, _| window.blur());
        assert_eq!(menu_available(&mut rig), (true, true, true));

        rig.cx.cx.update(|cx| cx.dispatch_action(&About));
        rig.settle();
        assert_eq!(rig.graph.store.read_with(rig.cx, |store, _| store.snapshot().overlay()),
            Some(Overlay::Settings(SettingsPage::About)));
        assert!(rig.said().iter().any(|line| line == "About Nudox"));
        rig.keys("escape");

        rig.cx.cx.update(|cx| cx.dispatch_action(&crate::shell::OpenSettingsAction));
        rig.settle();
        assert_eq!(rig.graph.store.read_with(rig.cx, |store, _| store.snapshot().overlay()),
            Some(Overlay::Settings(SettingsPage::Appearance)));
        rig.keys("escape");

        rig.cx.cx.update(|cx| cx.dispatch_action(&crate::shell::AddFolderAction));
        rig.settle();
        assert_eq!(rig.graph.store.read_with(rig.cx, |store, _| store.snapshot().overlay()),
            Some(Overlay::AddProject));
        rig.keys("escape");

        let retry = native_bounds_id(&mut rig, "status-retry", "Button", "Try again", true)
            .expect("failed owner's native retry");
        rig.cx.simulate_click(retry.center(), gpui::Modifiers::none());
        rig.draw();
        assert!(matches!(gate.state(), OwnerState::Starting));
        let root = rig.graph.store.read_with(rig.cx, |store, _| store.snapshot().key());
        gate.publish(OwnerState::Ready { key: root, mode: ServiceMode::Attached });
        rig.settle();
        rig.cx.update(|window, _| window.blur());
        assert_eq!(menu_available(&mut rig), (true, true, true), "recovered Home still validates local App commands before a click");
    }
}
