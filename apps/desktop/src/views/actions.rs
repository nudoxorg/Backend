//! Every action this window can perform, and the keys that reach them.
//! The text field's editing keys are bound separately, under its own context.
//! Nothing is bound twice, so no keystroke has an ambiguous owner.
//!
//! Every key comes from [`super::keys`], so the binding that fires and the
//! hint a view draws are one value: `⌘K` on a Mac is `Ctrl+K` on Windows in
//! both places or in neither.
//!
//! The editable-text element ships a default binding set that claims Enter,
//! Tab, Escape, and the arrow keys. Those four are exactly the keys an omnibar
//! needs, so this module declines the default set and rebuilds it without
//! them: editing keys stay under the `EditableText` context, while navigation
//! and command keys belong to the window. The result is that typing into the
//! omnibar and driving it from the keyboard are the same activity.

use super::keys::{self, Chord};
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
        /// Open the browse page.
        GoHome,
        /// Close the active tab.
        CloseTab,
        /// Activate the tab before this one in tree order.
        PreviousTab,
        /// Activate the tab after this one in tree order.
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
        /// Show the source of the page being read, over the page.
        OpenSource,
        /// Open the source of the page being read in the external editor.
        OpenEditor,
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
        /// Select the next admitted graph node.
        GraphNext,
        /// Select the previous admitted graph node.
        GraphPrevious,
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

fn bind<A: gpui::Action>(chord: Chord, action: A, context: Option<&str>) -> KeyBinding {
    KeyBinding::new(&chord.binding(), action, context)
}

fn command_bindings() -> Vec<KeyBinding> {
    vec![
        bind(keys::FOCUS_OMNIBAR, FocusOmnibar, None),
        bind(keys::OPEN_PALETTE, OpenPalette, None),
        bind(keys::ADD_PROJECT, AddProject, None),
        bind(keys::TOGGLE_LIBRARY, ToggleLibrary, None),
        bind(keys::TOGGLE_CONTEXT, ToggleContext, None),
        bind(keys::OPEN_SETTINGS, OpenSettings, None),
        bind(keys::GO_BACK, GoBack, None),
        bind(keys::GO_FORWARD, GoForward, None),
        bind(keys::GO_HOME, GoHome, None),
        bind(keys::CLOSE_TAB, CloseTab, None),
        bind(keys::PREVIOUS_TAB, PreviousTab, None),
        bind(keys::NEXT_TAB, NextTab, None),
        bind(keys::COPY_IDENTITY, CopyIdentity, Some(WINDOW_CONTEXT)),
        bind(keys::COPY_KEY, CopyKey, Some(WINDOW_CONTEXT)),
        bind(keys::RESET_INTERFACE, ResetInterface, None),
        bind(keys::GROW_INTERFACE, GrowInterface, None),
        bind(keys::SHRINK_INTERFACE, ShrinkInterface, None),
        bind(keys::RELOAD, Reload, None),
        bind(keys::OPEN_SOURCE, OpenSource, None),
        bind(keys::OPEN_EDITOR, OpenEditor, None),
        bind(keys::TOGGLE_APPEARANCE, ToggleAppearance, None),
        bind(keys::TOGGLE_MOTION, ToggleMotion, None),
        bind(
            Chord::primary("down").alt(),
            GraphNext,
            Some(WINDOW_CONTEXT),
        ),
        bind(
            Chord::primary("up").alt(),
            GraphPrevious,
            Some(WINDOW_CONTEXT),
        ),
    ]
}

fn navigation_bindings() -> Vec<KeyBinding> {
    vec![
        bind(keys::DISMISS, Dismiss, None),
        bind(Chord::plain("up"), MoveUp, None),
        bind(Chord::plain("down"), MoveDown, None),
        bind(Chord::plain("pageup"), PageUp, None),
        bind(Chord::plain("pagedown"), PageDown, None),
        bind(Chord::plain("home"), SelectFirst, None),
        bind(Chord::plain("end"), SelectLast, None),
        bind(keys::ACCEPT, Accept, None),
        bind(keys::COMPLETE, Complete, None),
        bind(keys::tab_chord(1), Tab1, None),
        bind(keys::tab_chord(2), Tab2, None),
        bind(keys::tab_chord(3), Tab3, None),
        bind(keys::tab_chord(4), Tab4, None),
        bind(keys::tab_chord(5), Tab5, None),
        bind(keys::tab_chord(6), Tab6, None),
        bind(keys::tab_chord(7), Tab7, None),
        bind(keys::tab_chord(8), Tab8, None),
        bind(keys::tab_chord(9), Tab9, None),
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
        .with::<editing::SelectLeft>("shift-left")
        .with::<editing::SelectRight>("shift-right")
        .with::<editing::SelectAll>(&Chord::primary("a").binding())
        .with::<editing::Copy>(&Chord::primary("c").binding())
        .with::<editing::Cut>(&Chord::primary("x").binding())
        .with::<editing::Paste>(&Chord::primary("v").binding())
        .with::<editing::Undo>(&Chord::primary("z").binding())
        .with::<editing::Redo>(&Chord::primary("z").shift().binding());
    platform_editing(bindings)
}

#[cfg(target_os = "macos")]
fn platform_editing(bindings: ActionBindingCollection) -> ActionBindingCollection {
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
fn platform_editing(bindings: ActionBindingCollection) -> ActionBindingCollection {
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
