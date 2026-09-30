//! Native application menus and their typed GPUI actions.

use gpui::{App, AppContext as _, KeyBinding, Menu, MenuItem, OsAction, SystemMenuType, actions};

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
        Menu::new("File").items([MenuItem::action("Close Window", CloseWindow)]),
        Menu::new("Edit").items([
            MenuItem::os_action("Cut", gpui_component::input::Cut, OsAction::Cut),
            MenuItem::os_action("Copy", gpui_component::input::Copy, OsAction::Copy),
            MenuItem::os_action("Paste", gpui_component::input::Paste, OsAction::Paste),
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
    cx.on_reopen(|cx| {
        cx.activate(true);
        let window = cx
            .active_window()
            .or_else(|| cx.windows().into_iter().next());
        if let Some(window) = window {
            let _ = window.update(cx, |_, window, _| window.activate_window());
        }
    });
}
