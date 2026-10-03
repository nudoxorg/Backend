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
use crate::navigation::Overlay;
use super::root::TransientFocusReturn;
use add::Form;
use facet::overlay::dialog::{self, Dialog};
use gpui::{App, AppContext as _, Entity, Global, IntoElement as _, Window, WindowId};
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
    restore: Option<TransientFocusReturn>,
}

#[derive(Default)]
struct PerWindow(HashMap<WindowId, Mounted>);

impl Global for PerWindow {}

/// Makes the dialog agree with the snapshot's overlay. Called by the shell
/// whenever the overlay changes.
pub(crate) fn sync(links: &Links, before: TransientFocusReturn, window: &mut Window, cx: &mut App) -> Option<TransientFocusReturn> {
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
            let fresh = cx.default_global::<PerWindow>().0.get(&id).is_none_or(|mounted| mounted.restore.is_none());
            if fresh { content.update(cx, |add, cx| add.opened(window, cx)); }
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
                if fresh { mounted.restore = Some(before); }
            }
            let input = content.read(cx).input().clone();
            input.update(cx, |input, cx| input.focus(window, cx));
        }
        (false, true) => {
            dialog::close(window, cx);
            let covered = links.snapshot(cx).session().overlay_is_covered(Overlay::AddProject);
            let (restore, content) = cx.default_global::<PerWindow>().0.get_mut(&id).map_or((None, None), |mounted| {
                mounted.ours = false;
                (if covered { None } else { mounted.restore.take() }, Some(mounted.content.clone()))
            });
            let landed = content.is_some_and(|content| content.read(cx).landed());
            // Covered forms retain their text and return receipt. A real
            // dismissal returns through the Shell's current-visit gate after
            // the underlay paints, for both Escape and pointer Cancel.
            if landed { window.blur(); } else if !covered { return restore; }
        }
        (false, false) => {
            if !links.snapshot(cx).session().overlay_is_covered(Overlay::AddProject) {
                if let Some(mounted) = cx.default_global::<PerWindow>().0.get_mut(&id) { mounted.restore = None; }
            }
        }
        (true, true) => {}
    }
    None
}

/// Focus only the currently mounted Add owner, never a retained covered form.
pub(crate) fn focus_current(window: &mut Window, cx: &mut App) -> bool {
    let id = window.window_handle().window_id();
    let Some(content) = cx.default_global::<PerWindow>().0.get(&id)
        .filter(|mounted| mounted.ours).map(|mounted| mounted.content.clone()) else { return false; };
    let input = content.read(cx).input().clone();
    input.update(cx, |input, cx| input.focus(window, cx));
    true
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
