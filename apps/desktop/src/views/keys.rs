//! Keyboard chords, spelled once and rendered per platform.
//! A chord binds as `cmd-k` on macOS and `ctrl-k` elsewhere, and labels
//! itself as `⌘K` or `Ctrl+K` from the same value — so a hint can never
//! disagree with the key that actually fires.
//!
//! Every shortcut in the window is a constant here. Views draw the label
//! from the constant and the keymap binds from the constant, which makes
//! "the hint says ⌘⇧A but nothing happens on Windows" a category of defect
//! this module removes rather than one it asks a tester to find.

use crate::navigation::ActionId;
use gpui::KeyBinding;

/// One keyboard chord: an optional primary modifier, shift, alt, and a key.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct Chord {
    primary: bool,
    shift: bool,
    alt: bool,
    key: &'static str,
}

impl Chord {
    /// A chord on the platform's primary modifier: command or control.
    pub(crate) const fn primary(key: &'static str) -> Self {
        Self {
            primary: true,
            shift: false,
            alt: false,
            key,
        }
    }

    /// A chord with no modifier.
    pub(crate) const fn plain(key: &'static str) -> Self {
        Self {
            primary: false,
            shift: false,
            alt: false,
            key,
        }
    }

    /// Adds shift.
    pub(crate) const fn shift(mut self) -> Self {
        self.shift = true;
        self
    }

    /// Adds the alternate modifier.
    pub(crate) const fn alt(mut self) -> Self {
        self.alt = true;
        self
    }

    /// Returns the keymap spelling GPUI parses.
    pub(crate) fn binding(self) -> String {
        let mut out = String::with_capacity(20);
        if self.primary {
            out.push_str(if is_mac() { "cmd-" } else { "ctrl-" });
        }
        if self.shift {
            out.push_str("shift-");
        }
        if self.alt {
            out.push_str("alt-");
        }
        out.push_str(self.key);
        out
    }

    /// Returns the label a hint draws.
    pub(crate) fn label(self) -> String {
        if is_mac() {
            let mut out = String::with_capacity(6);
            if self.primary {
                out.push('⌘');
            }
            if self.alt {
                out.push('⌥');
            }
            if self.shift {
                out.push('⇧');
            }
            out.push_str(key_label(self.key, true));
            return out;
        }
        let mut parts: Vec<&str> = Vec::with_capacity(4);
        if self.primary {
            parts.push("Ctrl");
        }
        if self.alt {
            parts.push("Alt");
        }
        if self.shift {
            parts.push("Shift");
        }
        parts.push(key_label(self.key, false));
        parts.join("+")
    }
}

/// Returns whether this build runs on macOS.
pub(crate) const fn is_mac() -> bool {
    cfg!(target_os = "macos")
}

fn key_label(key: &'static str, mac: bool) -> &'static str {
    match (key, mac) {
        ("escape", true) => "esc",
        ("escape", false) => "Esc",
        ("enter", true) => "↩",
        ("enter", false) => "Enter",
        ("tab", true) => "⇥",
        ("tab", false) => "Tab",
        ("backspace", true) => "⌫",
        ("backspace", false) => "Backspace",
        ("up", _) => "↑",
        ("down", _) => "↓",
        ("left", _) => "←",
        ("right", _) => "→",
        ("pageup", true) => "⇞",
        ("pageup", false) => "PgUp",
        ("pagedown", true) => "⇟",
        ("pagedown", false) => "PgDn",
        ("home", true) => "↖",
        ("home", false) => "Home",
        ("end", true) => "↘",
        ("end", false) => "End",
        ("a", _) => "A",
        ("b", _) => "B",
        ("c", _) => "C",
        ("d", _) => "D",
        ("k", _) => "K",
        ("l", _) => "L",
        ("m", _) => "M",
        ("p", _) => "P",
        ("r", _) => "R",
        ("t", _) => "T",
        ("w", _) => "W",
        ("h", _) => "H",
        ("e", _) => "E",
        ("f", _) => "F",
        ("n", _) => "N",
        ("o", _) => "O",
        (other, _) => other,
    }
}

/// Focus the omnibar.
pub(crate) const FOCUS_OMNIBAR: Chord = Chord::primary("k");
/// Open the command palette.
pub(crate) const OPEN_PALETTE: Chord = Chord::primary("p").shift();
/// Add a project.
pub(crate) const ADD_PROJECT: Chord = Chord::primary("n");
/// Show or hide the library panel.
pub(crate) const TOGGLE_LIBRARY: Chord = Chord::primary("b");
/// Show or hide the context panel.
pub(crate) const TOGGLE_CONTEXT: Chord = Chord::primary("\\");
/// Open keyboard and CLI help.
pub(crate) const OPEN_HELP: Chord = Chord::primary("/").shift();
/// Toggle the surface appearance.
pub(crate) const TOGGLE_APPEARANCE: Chord = Chord::primary("a").shift();
/// Toggle the local/registry metadata policy.
pub(crate) const TOGGLE_PRIVACY: Chord = Chord::primary("y").shift();
/// Toggle reduced motion.
pub(crate) const TOGGLE_REDUCED_MOTION: Chord = Chord::primary("m").shift();
/// Open settings.
pub(crate) const OPEN_SETTINGS: Chord = Chord::primary(",");
/// Open the host platform disclosure.
pub(crate) const OPEN_PLATFORM_MENU: Chord = Chord::primary("p").alt();
/// Open the feature flags disclosure.
pub(crate) const OPEN_FEATURE_MENU: Chord = Chord::primary("f").alt();
/// Open the documentation disclosure.
pub(crate) const OPEN_DOCS_MENU: Chord = Chord::primary("d").alt();
/// Open the active language disclosure.
pub(crate) const OPEN_LANGUAGE_MENU: Chord = Chord::primary("l").alt();
/// Open the MCP/Agents setup page.
pub(crate) const OPEN_AGENTS_SETTINGS: Chord = Chord::primary("a").alt();
/// Walk history back.
pub(crate) const GO_BACK: Chord = Chord::primary("[");
/// Walk history forward.
pub(crate) const GO_FORWARD: Chord = Chord::primary("]");
/// Go home.
pub(crate) const GO_HOME: Chord = Chord::primary("h").shift();
/// Close the active tab.
pub(crate) const CLOSE_TAB: Chord = Chord::primary("w");
/// Activate the previous tab.
pub(crate) const PREVIOUS_TAB: Chord = Chord::primary("[").shift();
/// Activate the next tab.
pub(crate) const NEXT_TAB: Chord = Chord::primary("]").shift();
/// Copy the readable identity.
pub(crate) const COPY_IDENTITY: Chord = Chord::primary("c");
/// Copy the stable key.
pub(crate) const COPY_KEY: Chord = Chord::primary("c").shift();
/// Reset the interface size.
pub(crate) const RESET_INTERFACE: Chord = Chord::primary("0");
/// Grow the interface.
pub(crate) const GROW_INTERFACE: Chord = Chord::primary("=");
/// Shrink the interface.
pub(crate) const SHRINK_INTERFACE: Chord = Chord::primary("-");
/// Reload the page.
pub(crate) const RELOAD: Chord = Chord::primary("r");
/// Show the source of the page being read, over the page.
pub(crate) const OPEN_SOURCE: Chord = Chord::primary("e");
/// Find text in the captured source sheet.
pub(crate) const FIND_IN_SOURCE: Chord = Chord::primary("f");
/// Move to the previous source find match.
pub(crate) const PREVIOUS_SOURCE_MATCH: Chord = Chord::plain("enter").shift();
/// Open the source of the page being read in the external editor.
pub(crate) const OPEN_EDITOR: Chord = Chord::primary("e").shift();
/// Dismiss the topmost transient surface.
pub(crate) const DISMISS: Chord = Chord::plain("escape");
/// Accept the selected row.
pub(crate) const ACCEPT: Chord = Chord::plain("enter");
/// Complete the selected row into the field.
pub(crate) const COMPLETE: Chord = Chord::plain("tab");

/// Returns the chord that activates the tab at one ordinal, 1 through 9.
pub(crate) const fn tab_chord(ordinal: u8) -> Chord {
    Chord::primary(match ordinal {
        1 => "1",
        2 => "2",
        3 => "3",
        4 => "4",
        5 => "5",
        6 => "6",
        7 => "7",
        8 => "8",
        _ => "9",
    })
}
/// GPUI action payload used by the single root action listener.
///
/// A numeric payload lets the stable navigation registry remain the source of
/// truth while GPUI still receives a real typed action for every keyboard
/// binding. `no_json` is intentional: these bindings are product defaults,
/// not user supplied serialized actions.
#[derive(Clone, Debug, PartialEq, gpui::Action)]
#[action(no_json)]
pub(crate) struct ShellAction {
    /// Stable [`ActionId`] representation.
    pub(crate) id: u16,
}

/// Returns the product's default shell bindings.
pub(crate) fn bindings() -> Vec<KeyBinding> {
    [
        (ADD_PROJECT, ActionId::AddProject),
        (TOGGLE_LIBRARY, ActionId::ToggleShelf),
        (TOGGLE_CONTEXT, ActionId::ToggleContext),
        (OPEN_SETTINGS, ActionId::OpenSettings),
        (OPEN_PALETTE, ActionId::OpenCommandPalette),
        (GO_BACK, ActionId::Back),
        (GO_FORWARD, ActionId::Forward),
        (GO_HOME, ActionId::OpenHome),
        (OPEN_HELP, ActionId::OpenHelp),
        (TOGGLE_APPEARANCE, ActionId::ToggleAppearance),
        (TOGGLE_PRIVACY, ActionId::TogglePrivacy),
        (TOGGLE_REDUCED_MOTION, ActionId::ToggleReducedMotion),
    ]
    .into_iter()
    .map(|(chord, action)| {
        let binding = chord.binding();
        KeyBinding::new(&binding, ShellAction { id: action as u16 }, None)
    })
    .collect()
}
