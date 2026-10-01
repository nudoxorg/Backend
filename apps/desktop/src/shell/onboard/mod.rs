//! First run and the lifecycle around it: the add-a-folder dialog, the words
//! a project says while it indexes, and how it says a failure.
//!
//! The dialog is the float layer's own modal ([`facet::overlay::dialog`]),
//! opened while the snapshot's overlay is [`Overlay::AddProject`] and closed
//! when it is not. The snapshot decides; [`sync`] follows it, so ⌘O, the
//! Library's button, Esc and the reducer all reach the same state and no
//! second copy of "is it open" exists to disagree with it.

mod add;
pub(crate) mod commands;
pub(crate) mod failure;
pub(crate) mod library;
mod path;

use super::region::Links;
use crate::navigation::{Overlay, Route};
use add::Form;
use facet::overlay::dialog::{self, Dialog};
use gpui::{App, AppContext as _, Entity, FocusHandle, Global, IntoElement as _, Window, WindowId};
use std::collections::HashMap;
use std::rc::Rc;

/// One window's dialog: its content, and whether the dialog the float layer
/// holds is ours (another sheet may hold it, and is never closed by us).
struct Mounted {
    content: Entity<Form>,
    ours: bool,
    /// What held focus when the dialog opened: the dialog puts it back when
    /// it closes, and so does [`sync`], because the field inside it goes
    /// away holding focus otherwise (the shell's keys then reach nothing).
    restore: Option<(FocusHandle, Route)>,
}

#[derive(Default)]
struct PerWindow(HashMap<WindowId, Mounted>);

impl Global for PerWindow {}

/// Makes the dialog agree with the snapshot's overlay. Called by the shell
/// whenever the overlay changes.
pub(crate) fn sync(links: &Links, window: &mut Window, cx: &mut App) {
    let wants = links.snapshot(cx).overlay() == Some(Overlay::AddProject);
    let id = window.window_handle().window_id();
    let open = dialog::is_open(window, cx);
    let ours = cx.default_global::<PerWindow>().0.get(&id).is_some_and(|mounted| mounted.ours);
    match (wants, open && ours) {
        (true, false) => {
            let content = match cx.default_global::<PerWindow>().0.get(&id) {
                Some(mounted) => mounted.content.clone(),
                None => {
                    let content = cx.new(|cx| Form::new(links.clone(), window, cx));
                    cx.default_global::<PerWindow>().0.insert(id, Mounted { content: content.clone(), ours: false, restore: None });
                    content
                }
            };
            let before = window.focused(cx);
            let route = links.snapshot(cx).route().clone();
            content.update(cx, |add, cx| add.opened(window, cx));
            let shown = content.clone();
            dialog::open(
                Dialog {
                    title: "Add a folder".into(),
                    body: "".into(),
                    buttons: Vec::new(),
                    // A stray click must not throw away a path being typed:
                    // Esc, Cancel and a successful add close it, all through
                    // the overlay.
                    dismissible: false,
                    sheet: Some(Rc::new(move |measure, _, cx| {
                        shown.update(cx, |add, _| add.measured(measure));
                        shown.clone().into_any_element()
                    })),
                },
                window,
                cx,
            );
            if let Some(mounted) = cx.default_global::<PerWindow>().0.get_mut(&id) {
                mounted.ours = true;
                mounted.restore = before.map(|handle| (handle, route));
            }
            let input = content.read(cx).input().clone();
            input.update(cx, |input, cx| input.focus(window, cx));
        }
        (false, true) => {
            dialog::close(window, cx);
            let (restore, content) = cx.default_global::<PerWindow>().0.get_mut(&id).map_or((None, None), |mounted| {
                mounted.ours = false;
                (mounted.restore.take(), Some(mounted.content.clone()))
            });
            let landed = content.is_some_and(|content| content.read(cx).landed());
            // Back to what held it on Esc or Cancel, unless the place
            // changed under the dialog. An add lands a new project on the
            // Library: focus goes to no control (the keyboard ring does not
            // come back on "Add a folder", as if nothing had happened), and
            // the next key starts from the page's first target (D6).
            match restore {
                _ if landed => window.blur(),
                Some((handle, route)) if links.snapshot(cx).route() == &route => window.focus(&handle, cx),
                Some(_) => {}
                None => window.blur(),
            }
        }
        (true, true) | (false, false) => {}
    }
}

/// The dialog's text field, once it has been opened in this window.
#[cfg(test)]
pub(crate) fn field(window: &Window, cx: &mut App) -> Option<Entity<gpui_component::input::InputState>> {
    let id = window.window_handle().window_id();
    let content = cx.default_global::<PerWindow>().0.get(&id)?.content.clone();
    Some(content.read(cx).input().clone())
}

#[cfg(test)]
mod tests;
