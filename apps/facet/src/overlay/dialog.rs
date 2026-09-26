//! The dialog: a scrim, one cut plate, a title, a sentence or two, and the
//! buttons. Drawn by the float layer over everything else.
//!
//! - **Modal.** The scrim occludes the window; a press on it closes a
//!   dismissible dialog. Focus moves into the dialog on open, Tab and ⇧Tab
//!   cycle inside it, Esc closes a dismissible one, ↵ presses the primary
//!   button, and focus returns where it was when the dialog closes.
//! - **Reversible.** Entrance and exit are one presence driven from the
//!   moment it was last retargeted: closing mid-entrance reverses from where
//!   it is; opening again mid-exit reverses back. The dialog stays drawn
//!   until its exit settles.

use super::float::{self, Presence};
use super::text::prose;
use crate::controls::button;
use crate::measure::{Measure, Set};
use crate::motion;
use crate::paint::{Chamfer, cut};
use crate::theme::ActiveFacet;
use crate::tokens::ty;
use gpui::{
    AnyElement, App, FocusHandle, Global, InteractiveElement, IntoElement,
    KeyDownEvent, MouseButton, ParentElement, SharedString, Styled, Window, WindowId, div, px,
};
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::time::Duration;

const ENTER: Duration = Duration::from_millis(380);
const EXIT: Duration = Duration::from_millis(200);

type Action = Rc<dyn Fn(&mut Window, &mut App)>;

/// One button.
#[derive(Clone)]
pub struct DialogButton {
    /// Its label.
    pub label: SharedString,
    /// The one action that advances (mint; ↵ presses it).
    pub primary: bool,
    /// Destructive (coral).
    pub danger: bool,
    /// What it does (the dialog closes after).
    pub on_press: Action,
}

impl DialogButton {
    /// A button reading `label`.
    pub fn new(label: impl Into<SharedString>, on_press: impl Fn(&mut Window, &mut App) + 'static) -> Self {
        Self {
            label: label.into(),
            primary: false,
            danger: false,
            on_press: Rc::new(on_press),
        }
    }

    /// The primary action.
    #[must_use]
    pub const fn primary(mut self) -> Self {
        self.primary = true;
        self
    }

    /// Destructive.
    #[must_use]
    pub const fn danger(mut self) -> Self {
        self.danger = true;
        self
    }
}

/// A dialog.
#[derive(Clone)]
pub struct Dialog {
    /// The question or statement.
    pub title: SharedString,
    /// A sentence or two (prose markup).
    pub body: SharedString,
    /// The buttons, in reading order (the primary last).
    pub buttons: Vec<DialogButton>,
    /// Esc and a press on the scrim close it.
    pub dismissible: bool,
}

struct Open {
    dialog: Dialog,
    presence: Presence,
    closing: bool,
    focus: FocusHandle,
    restore: Option<FocusHandle>,
}

#[derive(Default)]
struct PerWindow(HashMap<WindowId, Rc<RefCell<Option<Open>>>>);

impl Global for PerWindow {}

fn slot(window: &Window, cx: &mut App) -> Rc<RefCell<Option<Open>>> {
    let id = window.window_handle().window_id();
    cx.default_global::<PerWindow>().0.entry(id).or_default().clone()
}

fn duration(full: Duration, cx: &App) -> Duration {
    if motion::reduced(cx) { Duration::ZERO } else { full }
}

/// Opens `dialog` (replacing any open one); focus moves into it.
pub fn open(dialog: Dialog, window: &mut Window, cx: &mut App) {
    let slot = slot(window, cx);
    let now = motion::now(cx);
    let enter = duration(ENTER, cx);
    let focus_handle = {
        let mut current = slot.borrow_mut();
        match current.as_mut() {
            Some(open) => {
                open.dialog = dialog;
                open.closing = false;
                open.presence.retarget(1.0, enter, now);
                open.focus.clone()
            }
            None => {
                let focus = cx.focus_handle();
                *current = Some(Open {
                    dialog,
                    presence: Presence::entering(now, enter),
                    closing: false,
                    focus: focus.clone(),
                    restore: window.focused(cx),
                });
                focus
            }
        }
    };
    window.focus(&focus_handle, cx);
    float::refresh(window, cx);
}

/// Closes the dialog (its exit plays; focus returns).
pub fn close(window: &mut Window, cx: &mut App) {
    let slot = slot(window, cx);
    let now = motion::now(cx);
    let exit = duration(EXIT, cx);
    let restore = {
        let mut current = slot.borrow_mut();
        let Some(open) = current.as_mut().filter(|open| !open.closing) else {
            return;
        };
        open.closing = true;
        open.presence.retarget(0.0, exit, now);
        (open.focus.contains_focused(window, cx) || open.focus.is_focused(window))
            .then(|| open.restore.clone())
    };
    match restore {
        Some(Some(handle)) => window.focus(&handle, cx),
        Some(None) => window.blur(),
        None => {}
    }
    float::refresh(window, cx);
}

/// Whether a dialog is open (not leaving).
#[must_use]
pub fn is_open(window: &Window, cx: &mut App) -> bool {
    slot(window, cx)
        .borrow()
        .as_ref()
        .is_some_and(|open| !open.closing)
}

fn press(button: &DialogButton, window: &mut Window, cx: &mut App) {
    (button.on_press)(window, cx);
    close(window, cx);
}

/// The scrim and the dialog as the layer draws them (absolutely filling the
/// layer's box). `None` when no dialog is open or leaving.
pub fn element(measure: &Measure, window: &mut Window, cx: &mut App) -> Option<AnyElement> {
    let slot = slot(window, cx);
    let now = motion::now(cx);
    let (dialog, t, live, focus) = {
        let mut current = slot.borrow_mut();
        let open = current.as_ref()?;
        let t = open.presence.value(now);
        let live = open.presence.live(now);
        if open.closing && !live && t <= 0.0 {
            *current = None;
            return None;
        }
        (open.dialog.clone(), t, live, open.focus.clone())
    };
    if live {
        motion::request_frame(window, cx);
    }
    let palette = cx.facet().palette();
    let scale = measure.scale();
    let width = px(440.0 * scale).min(measure.width() - px(32.0));
    let inner = measure.within(width - px(48.0 * scale));
    let count = dialog.buttons.len();
    let mut buttons = div().flex().justify_end().gap(px(8.0 * scale));
    for (index, spec) in dialog.buttons.iter().enumerate() {
        let spec = spec.clone();
        let mut control = button(("dialog-button", index), spec.label.clone(), &inner);
        if spec.primary {
            control = control.primary();
        }
        if spec.danger {
            control = control.danger();
        }
        buttons = buttons.child(control.on_click(move |window, cx| press(&spec, window, cx)));
    }
    let dismissible = dialog.dismissible;
    let primary = dialog.buttons.iter().find(|button| button.primary).cloned();
    let trap = focus.clone();
    let plate = cut()
        .chamfer(Chamfer::Lg)
        .fill(palette.plate)
        .floating()
        .w(width)
        .flex()
        .flex_col()
        .gap(px(14.0 * scale))
        .p(px(24.0 * scale))
        .child(
            div()
                .set(ty::DIALOG, &inner)
                .text_color(palette.ink0.hsla())
                .child(dialog.title.clone()),
        )
        .child(
            prose("dialog-body", dialog.body.clone(), ty::BODY, &inner)
                .color(palette.ink2)
                .code_color(palette.ink1),
        )
        .child(buttons);
    let scrim = div()
        .id("dialog-scrim")
        .absolute()
        .top_0()
        .left_0()
        .size_full()
        .flex()
        .items_center()
        .justify_center()
        .occlude()
        .track_focus(&focus)
        .on_key_down(move |event: &KeyDownEvent, window, cx| {
            let keystroke = &event.keystroke;
            match keystroke.key.as_str() {
                "escape" if dismissible => close(window, cx),
                "enter" => {
                    if let Some(primary) = &primary {
                        press(primary, window, cx);
                    }
                }
                "tab" => {
                    // The trap: Tab and ⇧Tab cycle through the buttons only.
                    if keystroke.modifiers.shift {
                        window.focus_prev(cx);
                        if !trap.contains_focused(window, cx) || trap.is_focused(window) {
                            trap.focus(window, cx);
                            for _ in 0..count {
                                window.focus_next(cx);
                            }
                        }
                    } else {
                        window.focus_next(cx);
                        if !trap.contains_focused(window, cx) || trap.is_focused(window) {
                            trap.focus(window, cx);
                            window.focus_next(cx);
                        }
                    }
                }
                _ => return,
            }
            cx.stop_propagation();
        })
        .on_mouse_down(MouseButton::Left, move |_, window, cx| {
            if dismissible {
                close(window, cx);
            }
            cx.stop_propagation();
        })
        .child(
            div()
                .absolute()
                .top_0()
                .left_0()
                .size_full()
                .bg(palette.veil.hsla())
                .opacity(t),
        )
        .child(
            // Composited once: the plate fades and grows as one surface.
            gpui::layer(
                div()
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .child(plate),
            )
            .origin(0.5, 0.5)
            .scale(0.97 + 0.03 * t)
            .translate(gpui::point(px(0.0), px((1.0 - t) * 10.0 * scale)))
            .opacity(t),
        );
    Some(scrim.into_any_element())
}

#[cfg(test)]
mod tests {
    use super::{Dialog, DialogButton, close, is_open, open};
    use crate::overlay::float;
    use gpui::{
        Context, FocusHandle, InteractiveElement, IntoElement, ParentElement, Render, Styled,
        TestAppContext, VisualTestContext, Window, div, px, size,
    };

    struct Page {
        trigger: FocusHandle,
    }

    impl Render for Page {
        fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            div()
                .size_full()
                .child(
                    div()
                        .id("trigger")
                        .track_focus(&self.trigger)
                        .child("Open"),
                )
                .child(float::layer(window, cx))
        }
    }

    /// One platform frame, so a just-opened dialog's key handler is mounted.
    fn frame(cx: &mut VisualTestContext) {
        cx.update(|window, cx| {
            window.simulate_next_frame(cx);
            window.refresh();
            window.draw(cx).clear(cx);
        });
    }

    fn page(cx: &mut TestAppContext) -> (FocusHandle, &mut VisualTestContext) {
        let (view, cx) = cx.add_window_view(|_, cx| Page {
            trigger: cx.focus_handle(),
        });
        cx.simulate_resize(size(px(900.0), px(700.0)));
        let trigger = view.read_with(cx, |page, _| page.trigger.clone());
        cx.update(|window, cx| window.focus(&trigger, cx));
        frame(cx);
        (trigger, cx)
    }

    fn a_dialog(dismissible: bool) -> Dialog {
        Dialog {
            title: "Remove serde from the shelf?".into(),
            body: "Its pages stay in the index.".into(),
            buttons: vec![DialogButton::new("Keep", |_, _| {}), DialogButton::new("Remove", |_, _| {}).primary()],
            dismissible,
        }
    }

    #[gpui::test]
    fn opening_moves_focus_in_and_closing_returns_it_to_the_trigger(cx: &mut TestAppContext) {
        let (trigger, cx) = page(cx);
        assert_eq!(
            cx.update(|window, cx| window.focused(cx)),
            Some(trigger.clone()),
            "the trigger should hold focus before the dialog opens"
        );
        cx.update(|window, cx| open(a_dialog(true), window, cx));
        frame(cx);
        assert!(
            cx.update(|window, cx| window.focused(cx)) != Some(trigger.clone()),
            "focus should have moved off the trigger into the dialog"
        );
        assert!(cx.update(|window, cx| is_open(window, cx)), "the dialog should report open");

        cx.update(|window, cx| close(window, cx));
        frame(cx);
        assert_eq!(
            cx.update(|window, cx| window.focused(cx)),
            Some(trigger),
            "focus must return to the trigger once the dialog closes"
        );
        assert!(!cx.update(|window, cx| is_open(window, cx)), "closing must flip is_open to false at once");
    }

    #[gpui::test]
    fn a_second_close_while_already_closing_is_a_no_op(cx: &mut TestAppContext) {
        let (trigger, cx) = page(cx);
        cx.update(|window, cx| open(a_dialog(true), window, cx));
        frame(cx);
        cx.update(|window, cx| close(window, cx));
        frame(cx);
        assert_eq!(cx.update(|window, cx| window.focused(cx)), Some(trigger.clone()));
        // The user moved focus elsewhere (simulated by blurring); a second,
        // stray close() must not touch it.
        cx.update(|window, _| window.blur());
        cx.update(|window, cx| close(window, cx));
        frame(cx);
        assert_eq!(
            cx.update(|window, cx| window.focused(cx)),
            None,
            "a no-op close must not refocus anything"
        );
    }

    #[gpui::test]
    fn replacing_an_open_dialog_keeps_the_original_restore_target(cx: &mut TestAppContext) {
        let (trigger, cx) = page(cx);
        cx.update(|window, cx| open(a_dialog(true), window, cx));
        frame(cx);
        // Open again while the first is still up (e.g. its content changes):
        // the restore target must stay the original trigger, not whatever
        // had focus at the moment of the second `open`.
        cx.update(|window, cx| open(a_dialog(false), window, cx));
        frame(cx);
        cx.update(|window, cx| close(window, cx));
        frame(cx);
        assert_eq!(
            cx.update(|window, cx| window.focused(cx)),
            Some(trigger),
            "replacing the dialog must not lose the original restore target"
        );
    }

    #[gpui::test]
    fn esc_closes_a_dismissible_dialog_and_restores_focus(cx: &mut TestAppContext) {
        let (trigger, cx) = page(cx);
        cx.update(|window, cx| open(a_dialog(true), window, cx));
        frame(cx);
        cx.simulate_keystrokes("escape");
        frame(cx);
        assert!(!cx.update(|window, cx| is_open(window, cx)), "Esc must close a dismissible dialog");
        assert_eq!(
            cx.update(|window, cx| window.focused(cx)),
            Some(trigger),
            "Esc-close must restore focus like any other close"
        );
    }

    #[gpui::test]
    fn esc_does_nothing_on_a_dialog_that_is_not_dismissible(cx: &mut TestAppContext) {
        let (_trigger, cx) = page(cx);
        cx.update(|window, cx| open(a_dialog(false), window, cx));
        frame(cx);
        cx.simulate_keystrokes("escape");
        frame(cx);
        assert!(cx.update(|window, cx| is_open(window, cx)), "a non-dismissible dialog must ignore Esc");
    }
}
