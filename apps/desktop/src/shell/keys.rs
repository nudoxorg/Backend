//! The key table: every shell command, its chord, and where it listens —
//! one row each. Moving a key is editing one row; the bindings, the key caps
//! that rise while ⌘ is held, and Settings › Keys all read this table.
//!
//! Plain letters listen only outside text inputs, so typing in Ask never
//! walks focus. Rows marked *provisional* are the owner's first placement.

use gpui::{KeyBinding, actions};

actions!(
    nudox,
    [
        /// The next focusable.
        FocusNext,
        /// The previous focusable.
        FocusPrev,
        /// Open what the focus stands on.
        Activate,
        /// Peek the focused item (again: pin it).
        Peek,
        /// Show the focused declaration's code.
        PeelSource,
        /// The graph: the declaration's, or the whole world's.
        Graph,
        /// A declaration's code ↔ its page.
        CodePage,
        /// Hint mode.
        HintMode,
        /// Ask.
        Ask,
        /// Back along the thread.
        Back,
        /// Forward along the thread.
        Forward,
        /// Surface one depth.
        Surface,
        /// Zen: one page, no shelf.
        Zen,
        /// The shelf.
        ToggleShelf,
        /// The next zone.
        NextZone,
        /// The previous zone.
        PrevZone,
        /// Close the topmost thing.
        Escape,
        /// Text one step larger.
        ZoomIn,
        /// Text one step smaller.
        ZoomOut,
        /// Text back to the system's size.
        ZoomReset,
        /// Orbit.
        DepthOrbit,
        /// The package.
        DepthPackage,
        /// The declaration's page.
        DepthPage,
        /// The declaration's code.
        DepthCode,
        /// Settings.
        OpenSettings,
        /// Hold what you are on (the hand).
        Hold,
        /// Open or close the hand.
        OpenHand,
        /// The hand's first card.
        HandCard1,
        /// The hand's second card.
        HandCard2,
        /// The hand's third card.
        HandCard3,
        /// The hand's fourth card.
        HandCard4,
        /// The hand's fifth card.
        HandCard5,
        /// Copy this place's `nudox://` address.
        CopyAddress,
        /// Tour this package in the graph.
        Tour,
        /// Add a folder to the shelf.
        AddFolder,
    ]
);

/// The key context of the shell root.
pub(crate) const CONTEXT: &str = "NudoxShell";

/// A command the table binds.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub(crate) enum Command {
    /// J / ↓.
    FocusNext,
    /// K / ↑.
    FocusPrev,
    /// ↵.
    Activate,
    /// Space.
    Peek,
    /// S.
    PeelSource,
    /// G.
    Graph,
    /// ⌘. .
    CodePage,
    /// F.
    HintMode,
    /// ⌘K.
    Ask,
    /// ⌘[.
    Back,
    /// ⌘].
    Forward,
    /// ⌘↑.
    Surface,
    /// ⌘⇧. .
    Zen,
    /// ⌘\.
    ToggleShelf,
    /// Tab.
    NextZone,
    /// ⇧Tab.
    PrevZone,
    /// Esc.
    Escape,
    /// ⌘+.
    ZoomIn,
    /// ⌘−.
    ZoomOut,
    /// ⌘0.
    ZoomReset,
    /// ⌃1.
    DepthOrbit,
    /// ⌃2.
    DepthPackage,
    /// ⌃3.
    DepthPage,
    /// ⌃4.
    DepthCode,
    /// ⌘,.
    OpenSettings,
    /// ⌘D.
    Hold,
    /// H.
    OpenHand,
    /// ⌘1.
    HandCard1,
    /// ⌘2.
    HandCard2,
    /// ⌘3.
    HandCard3,
    /// ⌘4.
    HandCard4,
    /// ⌘5.
    HandCard5,
    /// ⌘⇧C.
    CopyAddress,
    /// T.
    Tour,
    /// ⌘O.
    AddFolder,
}

/// Where a row listens.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Scope {
    /// Anywhere in the shell, including text inputs (⌘ chords, Esc).
    Shell,
    /// Outside text inputs (plain letters and arrows).
    Plain,
}

/// One row of the table.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Key {
    /// The command.
    pub command: Command,
    /// GPUI keystroke spelling (`secondary` is ⌘ on macOS, Ctrl elsewhere).
    pub chord: &'static str,
    /// The cap the key shows while ⌘ is held (and in Settings › Keys).
    pub cap: &'static str,
    /// Where it listens.
    pub scope: Scope,
    /// What it does, in the words Settings › Keys uses.
    pub says: &'static str,
}

const fn key(
    command: Command,
    chord: &'static str,
    cap: &'static str,
    scope: Scope,
    says: &'static str,
) -> Key {
    Key {
        command,
        chord,
        cap,
        scope,
        says,
    }
}

/// The table. The first row for a command is its primary chord (its cap).
pub(crate) const TABLE: &[Key] = &[
    key(
        Command::FocusNext,
        "j",
        "J",
        Scope::Plain,
        "walk focus down",
    ),
    key(
        Command::FocusNext,
        "down",
        "↓",
        Scope::Plain,
        "walk focus down",
    ),
    key(Command::FocusPrev, "k", "K", Scope::Plain, "walk focus up"),
    key(Command::FocusPrev, "up", "↑", Scope::Plain, "walk focus up"),
    key(
        Command::Activate,
        "enter",
        "↵",
        Scope::Plain,
        "open what the focus stands on",
    ),
    key(
        Command::Peek,
        "space",
        "Space",
        Scope::Plain,
        "peek; again to pin",
    ),
    key(
        Command::PeelSource,
        "s",
        "S",
        Scope::Plain,
        "the focused declaration's code",
    ),
    // provisional (owner, 2026-09-25)
    key(Command::Graph, "g", "G", Scope::Plain, "the graph"),
    // provisional (owner, 2026-09-25)
    key(
        Command::CodePage,
        "secondary-.",
        "⌘.",
        Scope::Shell,
        "code ↔ page",
    ),
    key(Command::HintMode, "f", "F", Scope::Plain, "hint mode"),
    key(Command::Ask, "secondary-k", "⌘K", Scope::Shell, "ask"),
    key(
        Command::Back,
        "secondary-[",
        "⌘[",
        Scope::Shell,
        "back along the thread",
    ),
    key(
        Command::Forward,
        "secondary-]",
        "⌘]",
        Scope::Shell,
        "forward along the thread",
    ),
    key(
        Command::Surface,
        "secondary-up",
        "⌘↑",
        Scope::Plain,
        "surface one depth",
    ),
    // provisional: ⌘. went to code ↔ page
    key(
        Command::Zen,
        "secondary-shift-.",
        "⌘⇧.",
        Scope::Shell,
        "zen: one page, no shelf",
    ),
    key(
        Command::ToggleShelf,
        "secondary-\\",
        "⌘\\",
        Scope::Shell,
        "the shelf",
    ),
    key(Command::NextZone, "tab", "Tab", Scope::Plain, "next zone"),
    key(
        Command::PrevZone,
        "shift-tab",
        "⇧Tab",
        Scope::Plain,
        "previous zone",
    ),
    key(
        Command::Escape,
        "escape",
        "Esc",
        Scope::Shell,
        "close the topmost thing; back to the pinned release",
    ),
    key(
        Command::ZoomIn,
        "secondary-=",
        "⌘+",
        Scope::Shell,
        "text larger",
    ),
    key(
        Command::ZoomIn,
        "secondary-+",
        "⌘+",
        Scope::Shell,
        "text larger",
    ),
    key(
        Command::ZoomOut,
        "secondary--",
        "⌘−",
        Scope::Shell,
        "text smaller",
    ),
    key(
        Command::ZoomReset,
        "secondary-0",
        "⌘0",
        Scope::Shell,
        "text at the system's size",
    ),
    // Depth moved to ⌃ when the hand took ⌘1–⌘5 (lead, 2026-09-27).
    key(Command::DepthOrbit, "ctrl-1", "⌃1", Scope::Shell, "Orbit"),
    key(
        Command::DepthPackage,
        "ctrl-2",
        "⌃2",
        Scope::Shell,
        "the package",
    ),
    key(Command::DepthPage, "ctrl-3", "⌃3", Scope::Shell, "the page"),
    key(Command::DepthCode, "ctrl-4", "⌃4", Scope::Shell, "the code"),
    key(
        Command::OpenSettings,
        "secondary-,",
        "⌘,",
        Scope::Shell,
        "settings",
    ),
    key(
        Command::Hold,
        "secondary-d",
        "⌘D",
        Scope::Shell,
        "hold it in the hand",
    ),
    key(Command::OpenHand, "h", "H", Scope::Plain, "the hand"),
    key(
        Command::HandCard1,
        "secondary-1",
        "⌘1",
        Scope::Shell,
        "the hand's first card",
    ),
    key(
        Command::HandCard2,
        "secondary-2",
        "⌘2",
        Scope::Shell,
        "the hand's second card",
    ),
    key(
        Command::HandCard3,
        "secondary-3",
        "⌘3",
        Scope::Shell,
        "the hand's third card",
    ),
    key(
        Command::HandCard4,
        "secondary-4",
        "⌘4",
        Scope::Shell,
        "the hand's fourth card",
    ),
    key(
        Command::HandCard5,
        "secondary-5",
        "⌘5",
        Scope::Shell,
        "the hand's fifth card",
    ),
    key(
        Command::CopyAddress,
        "secondary-shift-c",
        "⌘⇧C",
        Scope::Shell,
        "copy the address",
    ),
    key(
        Command::Tour,
        "t",
        "T",
        Scope::Plain,
        "tour this package in the graph",
    ),
    key(
        Command::AddFolder,
        "secondary-o",
        "⌘O",
        Scope::Shell,
        "add a folder",
    ),
];

/// The cap a command shows (its first row).
#[must_use]
pub(crate) fn cap(command: Command) -> &'static str {
    TABLE
        .iter()
        .find(|key| key.command == command)
        .map_or("", |key| key.cap)
}

fn binding_context(key: &Key) -> String {
    // Graph, menus and Compare each own a different part of the keyboard.
    let graph_owns = matches!(
        key.command,
        Command::FocusNext
            | Command::FocusPrev
            | Command::Activate
            | Command::Ask
            | Command::Escape
            | Command::Peek
            | Command::Tour
    );
    let menu_owns = key.scope == Scope::Plain || key.command == Command::Escape;
    let compare_owns = matches!(key.command, Command::FocusNext | Command::FocusPrev);
    let mut context = String::from(CONTEXT);
    if key.scope == Scope::Plain {
        context.push_str(" && !Input");
    }
    if graph_owns {
        context.push_str(" && !Graph");
    }
    if menu_owns {
        context.push_str(" && !Menu");
    }
    if compare_owns {
        context.push_str(" && !BrowseCompare");
    }
    context
}

fn binding(key: &Key) -> KeyBinding {
    let context_text = binding_context(key);
    let context = Some(context_text.as_str());
    let chord = key.chord;
    match key.command {
        Command::FocusNext => KeyBinding::new(chord, FocusNext, context),
        Command::FocusPrev => KeyBinding::new(chord, FocusPrev, context),
        Command::Activate => KeyBinding::new(chord, Activate, context),
        Command::Peek => KeyBinding::new(chord, Peek, context),
        Command::PeelSource => KeyBinding::new(chord, PeelSource, context),
        Command::Graph => KeyBinding::new(chord, Graph, context),
        Command::CodePage => KeyBinding::new(chord, CodePage, context),
        Command::HintMode => KeyBinding::new(chord, HintMode, context),
        Command::Ask => KeyBinding::new(chord, Ask, context),
        Command::Back => KeyBinding::new(chord, Back, context),
        Command::Forward => KeyBinding::new(chord, Forward, context),
        Command::Surface => KeyBinding::new(chord, Surface, context),
        Command::Zen => KeyBinding::new(chord, Zen, context),
        Command::ToggleShelf => KeyBinding::new(chord, ToggleShelf, context),
        Command::NextZone => KeyBinding::new(chord, NextZone, context),
        Command::PrevZone => KeyBinding::new(chord, PrevZone, context),
        Command::Escape => KeyBinding::new(chord, Escape, context),
        Command::ZoomIn => KeyBinding::new(chord, ZoomIn, context),
        Command::ZoomOut => KeyBinding::new(chord, ZoomOut, context),
        Command::ZoomReset => KeyBinding::new(chord, ZoomReset, context),
        Command::DepthOrbit => KeyBinding::new(chord, DepthOrbit, context),
        Command::DepthPackage => KeyBinding::new(chord, DepthPackage, context),
        Command::DepthPage => KeyBinding::new(chord, DepthPage, context),
        Command::DepthCode => KeyBinding::new(chord, DepthCode, context),
        Command::OpenSettings => KeyBinding::new(chord, OpenSettings, context),
        Command::Hold => KeyBinding::new(chord, Hold, context),
        Command::OpenHand => KeyBinding::new(chord, OpenHand, context),
        Command::HandCard1 => KeyBinding::new(chord, HandCard1, context),
        Command::HandCard2 => KeyBinding::new(chord, HandCard2, context),
        Command::HandCard3 => KeyBinding::new(chord, HandCard3, context),
        Command::HandCard4 => KeyBinding::new(chord, HandCard4, context),
        Command::HandCard5 => KeyBinding::new(chord, HandCard5, context),
        Command::CopyAddress => KeyBinding::new(chord, CopyAddress, context),
        Command::Tour => KeyBinding::new(chord, Tour, context),
        Command::AddFolder => KeyBinding::new(chord, AddFolder, context),
    }
}

/// Every binding in the table.
#[must_use]
pub fn bindings() -> Vec<KeyBinding> {
    TABLE.iter().map(binding).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn compare_keeps_its_own_row_keys_without_losing_shell_shortcuts() {
        for chord in ["j", "k", "down", "up"] {
            let key = TABLE
                .iter()
                .find(|key| key.chord == chord)
                .expect("row key is bound");
            assert!(
                binding_context(key).contains("!BrowseCompare"),
                "{chord} must reach the focused Compare page"
            );
        }
        let ask = TABLE
            .iter()
            .find(|key| key.command == Command::Ask)
            .expect("Ask is bound");
        assert!(
            !binding_context(ask).contains("BrowseCompare"),
            "Ask stays available over Compare"
        );
    }

    #[test]
    fn no_chord_is_bound_twice_and_every_command_has_a_cap() {
        let mut seen = HashSet::new();
        for key in TABLE {
            assert!(
                seen.insert((key.chord, key.scope == Scope::Plain)),
                "{} is bound twice",
                key.chord
            );
            assert!(!cap(key.command).is_empty(), "{:?} has no cap", key.command);
        }
        // The owner's zoom keys hold ⌘− and ⌘0: nothing else may.
        let zoom_out = TABLE
            .iter()
            .filter(|key| key.chord == "secondary--")
            .collect::<Vec<_>>();
        assert_eq!(zoom_out.len(), 1);
        assert_eq!(zoom_out[0].command, Command::ZoomOut);
        let reset = TABLE
            .iter()
            .filter(|key| key.chord == "secondary-0")
            .collect::<Vec<_>>();
        assert_eq!(reset.len(), 1);
        assert_eq!(reset[0].command, Command::ZoomReset);
        // Every binding parses.
        assert_eq!(bindings().len(), TABLE.len());
    }
}
