//! Every action this window can perform, and the keys that reach them.
//! The text field's editing keys are bound separately, under its own context.
//! Nothing is bound twice, so no keystroke has an ambiguous owner.
//!
//! The editable-text element ships a default binding set that claims Enter,
//! Tab, Escape, and the arrow keys. Those four are exactly the keys an omnibar
//! needs, so this module declines the default set and rebuilds it without
//! them: editing keys stay under the `EditableText` context, while navigation
//! and command keys belong to the window. The result is that typing into the
//! omnibar and driving it from the keyboard are the same activity.

use gpui::{ActionBindingCollection, KeyBinding, actions};
use gpui_elements::editable_text::actions as editing;

/// The key context the root of this window advertises.
pub(crate) const WINDOW_CONTEXT: &str = "Nudox";

/// The key context the editable text element advertises.
pub(crate) const FIELD_CONTEXT: &str = gpui_elements::editable_text::actions::DEFAULT_INPUT_CONTEXT;

actions!(
    nudox,
    [
        /// Move focus to the omnibar and select its contents.
        FocusOmnibar,
        /// Open the omnibar already in command-palette mode.
        OpenPalette,
        /// Expand the library panel's inline add-a-project flow.
        AddProject,
        /// Open or collapse the library panel.
        ToggleLibrary,
        /// Open or collapse the context panel.
        ToggleContext,
        /// Open or close the settings sheet.
        OpenSettings,
        /// Walk the active tab's history back one step.
        GoBack,
        /// Walk the active tab's history forward one step.
        GoForward,
        /// Close the active tab.
        CloseTab,
        /// Activate the tab to the left.
        PreviousTab,
        /// Activate the tab to the right.
        NextTab,
        /// Copy the active page's readable coordinate.
        CopyIdentity,
        /// Copy the active page's stable key.
        CopyKey,
        /// Restore the default reading size.
        ResetInterface,
        /// Increase the reading size by one notch.
        GrowInterface,
        /// Decrease the reading size by one notch.
        ShrinkInterface,
        /// Re-read whatever the reader is showing.
        Reload,
        /// Close the topmost transient surface.
        Dismiss,
        /// Move the omnibar selection up one row.
        MoveUp,
        /// Move the omnibar selection down one row.
        MoveDown,
        /// Move the omnibar selection up one page.
        PageUp,
        /// Move the omnibar selection down one page.
        PageDown,
        /// Move the omnibar selection to the first row.
        SelectFirst,
        /// Move the omnibar selection to the last row.
        SelectLast,
        /// Open the selected row, or run the selected command.
        Accept,
        /// Complete the selected row into the field without opening it.
        Complete,
        /// Switch between the Ink and Vellum appearances.
        ToggleAppearance,
        /// Suppress or restore motion.
        ToggleMotion,
        /// Activate the first tab.
        Tab1,
        /// Activate the second tab.
        Tab2,
        /// Activate the third tab.
        Tab3,
        /// Activate the fourth tab.
        Tab4,
        /// Activate the fifth tab.
        Tab5,
        /// Activate the sixth tab.
        Tab6,
        /// Activate the seventh tab.
        Tab7,
        /// Activate the eighth tab.
        Tab8,
        /// Activate the ninth tab.
        Tab9,
    ]
);

/// Returns every window binding, in the window's own key context.
pub(crate) fn window_bindings() -> Vec<KeyBinding> {
    let mut bindings = command_bindings();
    bindings.extend(navigation_bindings());
    bindings
}

fn command_bindings() -> Vec<KeyBinding> {
    vec![
        KeyBinding::new("cmd-k", FocusOmnibar, None),
        KeyBinding::new("cmd-l", OpenPalette, None),
        KeyBinding::new("cmd-shift-a", AddProject, None),
        KeyBinding::new("cmd-b", ToggleLibrary, None),
        KeyBinding::new("cmd-\\", ToggleContext, None),
        KeyBinding::new("cmd-,", OpenSettings, None),
        KeyBinding::new("cmd-[", GoBack, None),
        KeyBinding::new("cmd-]", GoForward, None),
        KeyBinding::new("cmd-w", CloseTab, None),
        KeyBinding::new("cmd-shift-[", PreviousTab, None),
        KeyBinding::new("cmd-shift-]", NextTab, None),
        KeyBinding::new("cmd-c", CopyIdentity, Some(WINDOW_CONTEXT)),
        KeyBinding::new("cmd-shift-c", CopyKey, Some(WINDOW_CONTEXT)),
        KeyBinding::new("cmd-0", ResetInterface, None),
        KeyBinding::new("cmd-=", GrowInterface, None),
        KeyBinding::new("cmd--", ShrinkInterface, None),
        KeyBinding::new("cmd-r", Reload, None),
        KeyBinding::new("cmd-shift-d", ToggleAppearance, None),
        KeyBinding::new("cmd-shift-m", ToggleMotion, None),
    ]
}

fn navigation_bindings() -> Vec<KeyBinding> {
    vec![
        KeyBinding::new("escape", Dismiss, None),
        KeyBinding::new("up", MoveUp, None),
        KeyBinding::new("down", MoveDown, None),
        KeyBinding::new("pageup", PageUp, None),
        KeyBinding::new("pagedown", PageDown, None),
        KeyBinding::new("home", SelectFirst, None),
        KeyBinding::new("end", SelectLast, None),
        KeyBinding::new("enter", Accept, None),
        KeyBinding::new("tab", Complete, None),
        KeyBinding::new("cmd-1", Tab1, None),
        KeyBinding::new("cmd-2", Tab2, None),
        KeyBinding::new("cmd-3", Tab3, None),
        KeyBinding::new("cmd-4", Tab4, None),
        KeyBinding::new("cmd-5", Tab5, None),
        KeyBinding::new("cmd-6", Tab6, None),
        KeyBinding::new("cmd-7", Tab7, None),
        KeyBinding::new("cmd-8", Tab8, None),
        KeyBinding::new("cmd-9", Tab9, None),
    ]
}

/// Returns the editing bindings, minus the five keys the omnibar needs.
///
/// The element's own `default_bindings` is deliberately not used: it claims
/// `enter`, `tab`, `escape`, `up`, and `down`, which are the keys that drive a
/// result list. Everything else about text editing is kept exactly as the
/// element expects it.
pub(crate) fn editing_bindings() -> ActionBindingCollection {
    let bindings = ActionBindingCollection::default()
        .with::<editing::DeleteLeft>("backspace")
        .with::<editing::DeleteRight>("delete")
        .with::<editing::NavLeft>("left")
        .with::<editing::NavRight>("right")
        .with::<editing::SelectAll>("cmd-a")
        .with::<editing::SelectLeft>("shift-left")
        .with::<editing::SelectRight>("shift-right")
        .with::<editing::Copy>("cmd-c")
        .with::<editing::Cut>("cmd-x")
        .with::<editing::Paste>("cmd-v")
        .with::<editing::Undo>("cmd-z")
        .with::<editing::Redo>("cmd-shift-z");
    macos_editing(bindings)
}

#[cfg(target_os = "macos")]
fn macos_editing(bindings: ActionBindingCollection) -> ActionBindingCollection {
    bindings
        .with::<editing::DeleteWordLeft>("alt-backspace")
        .with::<editing::DeleteWordRight>("alt-delete")
        .with::<editing::DeleteToLineStart>("cmd-backspace")
        .with::<editing::NavLineStart>("cmd-left")
        .with::<editing::NavLineEnd>("cmd-right")
        .with::<editing::NavWordLeft>("alt-left")
        .with::<editing::NavWordRight>("alt-right")
        .with::<editing::SelectWordLeft>("alt-shift-left")
        .with::<editing::SelectWordRight>("alt-shift-right")
}

#[cfg(not(target_os = "macos"))]
fn macos_editing(bindings: ActionBindingCollection) -> ActionBindingCollection {
    bindings
        .with::<editing::DeleteWordLeft>("ctrl-backspace")
        .with::<editing::DeleteWordRight>("ctrl-delete")
        .with::<editing::NavLineStart>("home")
        .with::<editing::NavLineEnd>("end")
        .with::<editing::NavWordLeft>("ctrl-left")
        .with::<editing::NavWordRight>("ctrl-right")
        .with::<editing::SelectWordLeft>("ctrl-shift-left")
        .with::<editing::SelectWordRight>("ctrl-shift-right")
}

/// Returns the tab index one of the nine tab actions selects.
pub(crate) const fn tab_index(ordinal: usize) -> usize {
    ordinal.saturating_sub(1)
}
