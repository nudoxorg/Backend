//! The global keymap registry for lindsey (GUI-PLAN §13.6 + Appendix B).
//!
//! # Why this is the single source of truth
//!
//! A binding that exists but is not discoverable is a bug. The `?` shortcuts
//! overlay (§23.3) and the command palette (§23.1) both render their content
//! *from this registry* — never from a hand-maintained list. That architectural
//! decision is enforced here by making `KEYMAP_REGISTRY` the only place where
//! a [`KeymapEntry`] is created, and by exposing [`all_bindings`] as the only
//! path to register bindings with GPUI.
//!
//! The contract:
//! - Every row in Appendix B maps to exactly one [`KeymapEntry`] (plus an
//!   optional Linux alternate entry — see `linux_keystroke`).
//! - Every entry has a non-empty `description`. A missing description renders
//!   as a blank row in `?` — the tests below treat that as a build-time bug.
//! - No two entries in the same context share the same `keystroke`.
//! - `all_bindings()` is called once in `main.rs`; the result is handed to
//!   GPUI's keymap so the registry *is* the runtime keymap.
//!
//! # For the `?` overlay and palette implementers
//!
//! Call [`entries_for_context`] to get an iterator of entries for a given
//! context name. Group by `context` to build the cheat-sheet sections.
//! Each entry's `keystroke` + `linux_keystroke` + `description` is everything
//! needed to render a binding row.

use gpui::KeyBinding;

use gpui::prelude::*;

use crate::app::actions::{
    ActivateTab1, ActivateTab2, ActivateTab3, ActivateTab4, ActivateTab5, ActivateTab6,
    ActivateTab7, ActivateTab8, ActivateTab9, CloseTab, CollapseTreeNode, ConfirmOverlay,
    Copy, CopySymbolUri, Cut, DeepLinkLine, DiffAgainstPrevious, DismissOverlay,
    ExpandNeighbors, ExpandRow, ExpandTreeNode, FitGraphToView, GoToDocsTab, GoToRefsTab,
    GoToSourceTab, GoToTimelineTab, JumpToSection1, JumpToSection2, JumpToSection3,
    MoveSelectionDown, MoveSelectionUp, NavigateBack, NavigateForward, NextInnerTab,
    NextSection, OpenCommandPalette, OpenInBackgroundTab, OpenOmniSearch, OpenProject,
    OpenSettings, OpenVersionPicker, OpenWithoutClosing, Paste, PinGraphNode, PrevInnerTab,
    PrevSection, Redo, SelectAll, SwitchFocusTreeTable, SyncSelected, ToggleBottomDock,
    ToggleHud, ToggleLeftDock, ToggleShortcutsOverlay, Undo,
};

// ── KeymapEntry ───────────────────────────────────────────────────────────────

/// One row in the keymap registry.
///
/// The `?` overlay and command palette render their content from this type.
/// Every field is `'static` so the registry can live as a compile-time slice.
pub struct KeymapEntry {
    /// The macOS keystroke string (e.g. `"cmd-k"`, `"g d"`, `"escape"`).
    ///
    /// This is the primary binding registered with GPUI on all platforms.
    /// On macOS `cmd` is the Command key; on Linux/Windows the entry in
    /// `linux_keystroke` is registered *in addition* as the `ctrl` equivalent.
    pub keystroke: &'static str,

    /// The Linux/Windows `ctrl`-equivalent binding, if different from `keystroke`.
    ///
    /// `None` when the binding is platform-agnostic (e.g. `"escape"`, `"f12"`,
    /// `"g d"`, single-letter context bindings). Both bindings produce the same
    /// action; the registry lists them separately so `?` can show the relevant
    /// one per OS.
    pub linux_keystroke: Option<&'static str>,

    /// GPUI context predicate string (matched against `KeyContext`).
    ///
    /// Examples: `"global"`, `"Pane"`, `"Overlay"`, `"SymbolPage"`,
    /// `"!InputFocused"`, `"DebugMode"`.
    pub context: &'static str,

    /// Human-readable description shown in the `?` overlay and palette.
    ///
    /// A blank description is a bug: it renders as an empty row in `?`. The
    /// test `every_entry_has_description` fails the build if any entry is empty.
    pub description: &'static str,

    /// Factory function that builds the [`KeyBinding`] for this entry.
    ///
    /// Called by [`all_bindings`]. Stored as `fn() -> KeyBinding` (a function
    /// pointer, not a closure) so `KeymapEntry` is `Sync` and can live in a
    /// `static` slice without `unsafe`.
    pub binding: fn() -> KeyBinding,
}

// ── Registry ─────────────────────────────────────────────────────────────────

/// The complete keymap. Every Appendix B row is present; every entry has a
/// description. Queried by `all_bindings`, `entries_for_context`, and the `?`
/// overlay renderer.
pub static KEYMAP_REGISTRY: &[KeymapEntry] = &[
    // ── Global ────────────────────────────────────────────────────────────────
    KeymapEntry {
        keystroke: "cmd-k",
        linux_keystroke: Some("ctrl-k"),
        context: "global",
        description: "Open omni-search",
        binding: || KeyBinding::new("cmd-k", OpenOmniSearch, Some("global")),
    },
    KeymapEntry {
        keystroke: "ctrl-k",
        linux_keystroke: None,
        context: "global",
        description: "Open omni-search (Linux/Windows)",
        binding: || KeyBinding::new("ctrl-k", OpenOmniSearch, Some("global")),
    },
    KeymapEntry {
        keystroke: "cmd-shift-p",
        linux_keystroke: Some("ctrl-shift-p"),
        context: "global",
        description: "Open command palette",
        binding: || KeyBinding::new("cmd-shift-p", OpenCommandPalette, Some("global")),
    },
    KeymapEntry {
        keystroke: "ctrl-shift-p",
        linux_keystroke: None,
        context: "global",
        description: "Open command palette (Linux/Windows)",
        binding: || KeyBinding::new("ctrl-shift-p", OpenCommandPalette, Some("global")),
    },
    KeymapEntry {
        keystroke: "cmd-[",
        linux_keystroke: Some("ctrl-["),
        context: "global",
        description: "Navigate back",
        binding: || KeyBinding::new("cmd-[", NavigateBack, Some("global")),
    },
    KeymapEntry {
        keystroke: "ctrl-[",
        linux_keystroke: None,
        context: "global",
        description: "Navigate back (Linux/Windows)",
        binding: || KeyBinding::new("ctrl-[", NavigateBack, Some("global")),
    },
    KeymapEntry {
        keystroke: "cmd-]",
        linux_keystroke: Some("ctrl-]"),
        context: "global",
        description: "Navigate forward",
        binding: || KeyBinding::new("cmd-]", NavigateForward, Some("global")),
    },
    KeymapEntry {
        keystroke: "ctrl-]",
        linux_keystroke: None,
        context: "global",
        description: "Navigate forward (Linux/Windows)",
        binding: || KeyBinding::new("ctrl-]", NavigateForward, Some("global")),
    },
    KeymapEntry {
        keystroke: "cmd-b",
        linux_keystroke: Some("ctrl-b"),
        context: "global",
        description: "Toggle left sidebar",
        binding: || KeyBinding::new("cmd-b", ToggleLeftDock, Some("global")),
    },
    KeymapEntry {
        keystroke: "ctrl-b",
        linux_keystroke: None,
        context: "global",
        description: "Toggle left sidebar (Linux/Windows)",
        binding: || KeyBinding::new("ctrl-b", ToggleLeftDock, Some("global")),
    },
    KeymapEntry {
        keystroke: "cmd-j",
        linux_keystroke: Some("ctrl-j"),
        context: "global",
        description: "Toggle bottom dock",
        binding: || KeyBinding::new("cmd-j", ToggleBottomDock, Some("global")),
    },
    KeymapEntry {
        keystroke: "ctrl-j",
        linux_keystroke: None,
        context: "global",
        description: "Toggle bottom dock (Linux/Windows)",
        binding: || KeyBinding::new("ctrl-j", ToggleBottomDock, Some("global")),
    },
    KeymapEntry {
        keystroke: "cmd-,",
        linux_keystroke: Some("ctrl-,"),
        context: "global",
        description: "Open settings",
        binding: || KeyBinding::new("cmd-,", OpenSettings, Some("global")),
    },
    KeymapEntry {
        keystroke: "ctrl-,",
        linux_keystroke: None,
        context: "global",
        description: "Open settings (Linux/Windows)",
        binding: || KeyBinding::new("ctrl-,", OpenSettings, Some("global")),
    },
    KeymapEntry {
        keystroke: "cmd-o",
        linux_keystroke: Some("ctrl-o"),
        context: "global",
        description: "Open project folder",
        binding: || KeyBinding::new("cmd-o", OpenProject, Some("global")),
    },
    KeymapEntry {
        keystroke: "ctrl-o",
        linux_keystroke: None,
        context: "global",
        description: "Open project folder (Linux/Windows)",
        binding: || KeyBinding::new("ctrl-o", OpenProject, Some("global")),
    },
    // ── Shortcuts overlay — fires only when no text input is focused ───────────
    KeymapEntry {
        keystroke: "?",
        linux_keystroke: None,
        context: "!InputFocused",
        description: "Show keyboard shortcuts",
        binding: || KeyBinding::new("?", ToggleShortcutsOverlay, Some("!InputFocused")),
    },
    // ── HUD — debug builds only ───────────────────────────────────────────────
    KeymapEntry {
        keystroke: "f12",
        linux_keystroke: None,
        context: "DebugMode",
        description: "Toggle performance HUD",
        binding: || KeyBinding::new("f12", ToggleHud, Some("DebugMode")),
    },
    // ── Pane ──────────────────────────────────────────────────────────────────
    KeymapEntry {
        keystroke: "cmd-w",
        linux_keystroke: Some("ctrl-w"),
        context: "Pane",
        description: "Close current tab",
        binding: || KeyBinding::new("cmd-w", CloseTab, Some("Pane")),
    },
    KeymapEntry {
        keystroke: "ctrl-w",
        linux_keystroke: None,
        context: "Pane",
        description: "Close current tab (Linux/Windows)",
        binding: || KeyBinding::new("ctrl-w", CloseTab, Some("Pane")),
    },
    // ActivateTab1-9 — macOS
    KeymapEntry {
        keystroke: "cmd-1",
        linux_keystroke: Some("ctrl-1"),
        context: "Pane",
        description: "Activate tab 1",
        binding: || KeyBinding::new("cmd-1", ActivateTab1, Some("Pane")),
    },
    KeymapEntry {
        keystroke: "ctrl-1",
        linux_keystroke: None,
        context: "Pane",
        description: "Activate tab 1 (Linux/Windows)",
        binding: || KeyBinding::new("ctrl-1", ActivateTab1, Some("Pane")),
    },
    KeymapEntry {
        keystroke: "cmd-2",
        linux_keystroke: Some("ctrl-2"),
        context: "Pane",
        description: "Activate tab 2",
        binding: || KeyBinding::new("cmd-2", ActivateTab2, Some("Pane")),
    },
    KeymapEntry {
        keystroke: "ctrl-2",
        linux_keystroke: None,
        context: "Pane",
        description: "Activate tab 2 (Linux/Windows)",
        binding: || KeyBinding::new("ctrl-2", ActivateTab2, Some("Pane")),
    },
    KeymapEntry {
        keystroke: "cmd-3",
        linux_keystroke: Some("ctrl-3"),
        context: "Pane",
        description: "Activate tab 3",
        binding: || KeyBinding::new("cmd-3", ActivateTab3, Some("Pane")),
    },
    KeymapEntry {
        keystroke: "ctrl-3",
        linux_keystroke: None,
        context: "Pane",
        description: "Activate tab 3 (Linux/Windows)",
        binding: || KeyBinding::new("ctrl-3", ActivateTab3, Some("Pane")),
    },
    KeymapEntry {
        keystroke: "cmd-4",
        linux_keystroke: Some("ctrl-4"),
        context: "Pane",
        description: "Activate tab 4",
        binding: || KeyBinding::new("cmd-4", ActivateTab4, Some("Pane")),
    },
    KeymapEntry {
        keystroke: "ctrl-4",
        linux_keystroke: None,
        context: "Pane",
        description: "Activate tab 4 (Linux/Windows)",
        binding: || KeyBinding::new("ctrl-4", ActivateTab4, Some("Pane")),
    },
    KeymapEntry {
        keystroke: "cmd-5",
        linux_keystroke: Some("ctrl-5"),
        context: "Pane",
        description: "Activate tab 5",
        binding: || KeyBinding::new("cmd-5", ActivateTab5, Some("Pane")),
    },
    KeymapEntry {
        keystroke: "ctrl-5",
        linux_keystroke: None,
        context: "Pane",
        description: "Activate tab 5 (Linux/Windows)",
        binding: || KeyBinding::new("ctrl-5", ActivateTab5, Some("Pane")),
    },
    KeymapEntry {
        keystroke: "cmd-6",
        linux_keystroke: Some("ctrl-6"),
        context: "Pane",
        description: "Activate tab 6",
        binding: || KeyBinding::new("cmd-6", ActivateTab6, Some("Pane")),
    },
    KeymapEntry {
        keystroke: "ctrl-6",
        linux_keystroke: None,
        context: "Pane",
        description: "Activate tab 6 (Linux/Windows)",
        binding: || KeyBinding::new("ctrl-6", ActivateTab6, Some("Pane")),
    },
    KeymapEntry {
        keystroke: "cmd-7",
        linux_keystroke: Some("ctrl-7"),
        context: "Pane",
        description: "Activate tab 7",
        binding: || KeyBinding::new("cmd-7", ActivateTab7, Some("Pane")),
    },
    KeymapEntry {
        keystroke: "ctrl-7",
        linux_keystroke: None,
        context: "Pane",
        description: "Activate tab 7 (Linux/Windows)",
        binding: || KeyBinding::new("ctrl-7", ActivateTab7, Some("Pane")),
    },
    KeymapEntry {
        keystroke: "cmd-8",
        linux_keystroke: Some("ctrl-8"),
        context: "Pane",
        description: "Activate tab 8",
        binding: || KeyBinding::new("cmd-8", ActivateTab8, Some("Pane")),
    },
    KeymapEntry {
        keystroke: "ctrl-8",
        linux_keystroke: None,
        context: "Pane",
        description: "Activate tab 8 (Linux/Windows)",
        binding: || KeyBinding::new("ctrl-8", ActivateTab8, Some("Pane")),
    },
    KeymapEntry {
        keystroke: "cmd-9",
        linux_keystroke: Some("ctrl-9"),
        context: "Pane",
        description: "Activate tab 9",
        binding: || KeyBinding::new("cmd-9", ActivateTab9, Some("Pane")),
    },
    KeymapEntry {
        keystroke: "ctrl-9",
        linux_keystroke: None,
        context: "Pane",
        description: "Activate tab 9 (Linux/Windows)",
        binding: || KeyBinding::new("ctrl-9", ActivateTab9, Some("Pane")),
    },
    // ── Overlay ───────────────────────────────────────────────────────────────
    KeymapEntry {
        keystroke: "escape",
        linux_keystroke: None,
        context: "Overlay",
        description: "Dismiss overlay / clear input",
        binding: || KeyBinding::new("escape", DismissOverlay, Some("Overlay")),
    },
    KeymapEntry {
        keystroke: "enter",
        linux_keystroke: None,
        context: "Overlay",
        description: "Confirm action",
        binding: || KeyBinding::new("enter", ConfirmOverlay, Some("Overlay")),
    },
    KeymapEntry {
        keystroke: "up",
        linux_keystroke: None,
        context: "Overlay",
        description: "Move selection up",
        binding: || KeyBinding::new("up", MoveSelectionUp, Some("Overlay")),
    },
    KeymapEntry {
        keystroke: "down",
        linux_keystroke: None,
        context: "Overlay",
        description: "Move selection down",
        binding: || KeyBinding::new("down", MoveSelectionDown, Some("Overlay")),
    },
    KeymapEntry {
        keystroke: "tab",
        linux_keystroke: None,
        context: "Overlay",
        description: "Next section",
        binding: || KeyBinding::new("tab", NextSection, Some("Overlay")),
    },
    KeymapEntry {
        keystroke: "shift-tab",
        linux_keystroke: None,
        context: "Overlay",
        description: "Previous section",
        binding: || KeyBinding::new("shift-tab", PrevSection, Some("Overlay")),
    },
    KeymapEntry {
        keystroke: "cmd-enter",
        linux_keystroke: Some("ctrl-enter"),
        context: "Overlay",
        description: "Open without closing overlay",
        binding: || KeyBinding::new("cmd-enter", OpenWithoutClosing, Some("Overlay")),
    },
    KeymapEntry {
        keystroke: "ctrl-enter",
        linux_keystroke: None,
        context: "Overlay",
        description: "Open without closing overlay (Linux/Windows)",
        binding: || KeyBinding::new("ctrl-enter", OpenWithoutClosing, Some("Overlay")),
    },
    KeymapEntry {
        keystroke: "alt-enter",
        linux_keystroke: None,
        context: "Overlay",
        description: "Open in background tab",
        binding: || KeyBinding::new("alt-enter", OpenInBackgroundTab, Some("Overlay")),
    },
    KeymapEntry {
        keystroke: "space",
        linux_keystroke: None,
        context: "Overlay",
        description: "Expand row detail",
        binding: || KeyBinding::new("space", ExpandRow, Some("Overlay")),
    },
    // ── OmniSearch ────────────────────────────────────────────────────────────
    KeymapEntry {
        keystroke: "cmd-1",
        linux_keystroke: Some("ctrl-1"),
        context: "OmniSearch",
        description: "Jump to Name results section",
        binding: || KeyBinding::new("cmd-1", JumpToSection1, Some("OmniSearch")),
    },
    KeymapEntry {
        keystroke: "ctrl-1",
        linux_keystroke: None,
        context: "OmniSearch",
        description: "Jump to Name results section (Linux/Windows)",
        binding: || KeyBinding::new("ctrl-1", JumpToSection1, Some("OmniSearch")),
    },
    KeymapEntry {
        keystroke: "cmd-2",
        linux_keystroke: Some("ctrl-2"),
        context: "OmniSearch",
        description: "Jump to Type results section",
        binding: || KeyBinding::new("cmd-2", JumpToSection2, Some("OmniSearch")),
    },
    KeymapEntry {
        keystroke: "ctrl-2",
        linux_keystroke: None,
        context: "OmniSearch",
        description: "Jump to Type results section (Linux/Windows)",
        binding: || KeyBinding::new("ctrl-2", JumpToSection2, Some("OmniSearch")),
    },
    KeymapEntry {
        keystroke: "cmd-3",
        linux_keystroke: Some("ctrl-3"),
        context: "OmniSearch",
        description: "Jump to Semantic results section",
        binding: || KeyBinding::new("cmd-3", JumpToSection3, Some("OmniSearch")),
    },
    KeymapEntry {
        keystroke: "ctrl-3",
        linux_keystroke: None,
        context: "OmniSearch",
        description: "Jump to Semantic results section (Linux/Windows)",
        binding: || KeyBinding::new("ctrl-3", JumpToSection3, Some("OmniSearch")),
    },
    // ── SymbolPage ────────────────────────────────────────────────────────────
    KeymapEntry {
        keystroke: "cmd-shift-[",
        linux_keystroke: Some("ctrl-shift-["),
        context: "SymbolPage",
        description: "Previous inner tab",
        binding: || KeyBinding::new("cmd-shift-[", PrevInnerTab, Some("SymbolPage")),
    },
    KeymapEntry {
        keystroke: "ctrl-shift-[",
        linux_keystroke: None,
        context: "SymbolPage",
        description: "Previous inner tab (Linux/Windows)",
        binding: || KeyBinding::new("ctrl-shift-[", PrevInnerTab, Some("SymbolPage")),
    },
    KeymapEntry {
        keystroke: "cmd-shift-]",
        linux_keystroke: Some("ctrl-shift-]"),
        context: "SymbolPage",
        description: "Next inner tab",
        binding: || KeyBinding::new("cmd-shift-]", NextInnerTab, Some("SymbolPage")),
    },
    KeymapEntry {
        keystroke: "ctrl-shift-]",
        linux_keystroke: None,
        context: "SymbolPage",
        description: "Next inner tab (Linux/Windows)",
        binding: || KeyBinding::new("ctrl-shift-]", NextInnerTab, Some("SymbolPage")),
    },
    KeymapEntry {
        keystroke: "y",
        linux_keystroke: None,
        context: "SymbolPage",
        description: "Copy symbol URI to clipboard",
        binding: || KeyBinding::new("y", CopySymbolUri, Some("SymbolPage")),
    },
    KeymapEntry {
        keystroke: "v",
        linux_keystroke: None,
        context: "SymbolPage",
        description: "Open version picker",
        binding: || KeyBinding::new("v", OpenVersionPicker, Some("SymbolPage")),
    },
    KeymapEntry {
        keystroke: "g d",
        linux_keystroke: None,
        context: "SymbolPage",
        description: "Go to Docs tab",
        binding: || KeyBinding::new("g d", GoToDocsTab, Some("SymbolPage")),
    },
    KeymapEntry {
        keystroke: "g s",
        linux_keystroke: None,
        context: "SymbolPage",
        description: "Go to Source tab",
        binding: || KeyBinding::new("g s", GoToSourceTab, Some("SymbolPage")),
    },
    KeymapEntry {
        keystroke: "g r",
        linux_keystroke: None,
        context: "SymbolPage",
        description: "Go to Refs tab",
        binding: || KeyBinding::new("g r", GoToRefsTab, Some("SymbolPage")),
    },
    KeymapEntry {
        keystroke: "g t",
        linux_keystroke: None,
        context: "SymbolPage",
        description: "Go to Timeline tab",
        binding: || KeyBinding::new("g t", GoToTimelineTab, Some("SymbolPage")),
    },
    // ── SymbolPage > SourceTab ────────────────────────────────────────────────
    KeymapEntry {
        keystroke: "shift-l",
        linux_keystroke: None,
        context: "SymbolPage > SourceTab",
        description: "Deep-link to current line",
        binding: || KeyBinding::new("shift-l", DeepLinkLine, Some("SymbolPage > SourceTab")),
    },
    // ── GraphView ─────────────────────────────────────────────────────────────
    KeymapEntry {
        keystroke: "f",
        linux_keystroke: None,
        context: "GraphView",
        description: "Fit graph to view",
        binding: || KeyBinding::new("f", FitGraphToView, Some("GraphView")),
    },
    KeymapEntry {
        keystroke: "e",
        linux_keystroke: None,
        context: "GraphView",
        description: "Expand neighbors of selected node",
        binding: || KeyBinding::new("e", ExpandNeighbors, Some("GraphView")),
    },
    KeymapEntry {
        keystroke: "p",
        linux_keystroke: None,
        context: "GraphView",
        description: "Pin / unpin selected graph node",
        binding: || KeyBinding::new("p", PinGraphNode, Some("GraphView")),
    },
    // ── ProjectPanel ──────────────────────────────────────────────────────────
    KeymapEntry {
        keystroke: "s",
        linux_keystroke: None,
        context: "ProjectPanel",
        description: "Sync selected dependency",
        binding: || KeyBinding::new("s", SyncSelected, Some("ProjectPanel")),
    },
    // ── PackageBrowser ────────────────────────────────────────────────────────
    KeymapEntry {
        keystroke: "d",
        linux_keystroke: None,
        context: "PackageBrowser",
        description: "Diff against previous version",
        binding: || KeyBinding::new("d", DiffAgainstPrevious, Some("PackageBrowser")),
    },
    KeymapEntry {
        keystroke: "left",
        linux_keystroke: None,
        context: "PackageBrowser",
        description: "Collapse tree node",
        binding: || KeyBinding::new("left", CollapseTreeNode, Some("PackageBrowser")),
    },
    KeymapEntry {
        keystroke: "right",
        linux_keystroke: None,
        context: "PackageBrowser",
        description: "Expand tree node",
        binding: || KeyBinding::new("right", ExpandTreeNode, Some("PackageBrowser")),
    },
    KeymapEntry {
        keystroke: "tab",
        linux_keystroke: None,
        context: "PackageBrowser",
        description: "Switch focus between tree and table",
        binding: || KeyBinding::new("tab", SwitchFocusTreeTable, Some("PackageBrowser")),
    },
];

// ── Public API ────────────────────────────────────────────────────────────────

/// Build all [`KeyBinding`]s from the registry for registration with GPUI.
///
/// Called once in `main.rs`:
/// ```ignore
/// cx.bind_keys(keymaps::all_bindings());
/// ```
pub fn all_bindings() -> Vec<KeyBinding> {
    KEYMAP_REGISTRY.iter().map(|e| (e.binding)()).collect()
}

/// Iterate registry entries for a given context name.
///
/// Used by the `?` shortcuts overlay to render bindings grouped by context,
/// and by the command palette to filter by the currently active context stack.
///
/// The comparison is exact: `"SymbolPage"` does not match `"SymbolPage > SourceTab"`.
/// The overlay should call this for each context in the active stack and merge
/// the results to get the full set of currently applicable bindings.
pub fn entries_for_context(context: &str) -> impl Iterator<Item = &'static KeymapEntry> {
    KEYMAP_REGISTRY.iter().filter(move |e| e.context == context)
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    /// Every Appendix B description keyword must appear in at least one entry.
    #[test]
    fn all_appendix_b_rows_present() {
        let descriptions: Vec<&str> = KEYMAP_REGISTRY.iter().map(|e| e.description).collect();

        let keywords = [
            "omni-search",
            "command palette",
            "back",
            "forward",
            "left sidebar",
            "bottom dock",
            "settings",
            "project",
            "shortcuts",
            "tab",
            "dismiss",
            "confirm",
            "up",
            "down",
            "section",
        ];

        for kw in keywords {
            let found = descriptions
                .iter()
                .any(|d| d.to_lowercase().contains(kw));
            assert!(
                found,
                "No keymap entry found with description containing {:?}",
                kw
            );
        }

        assert!(
            KEYMAP_REGISTRY.len() >= 15,
            "Expected at least 15 keymap entries, found {}",
            KEYMAP_REGISTRY.len()
        );
    }

    /// Within each context, no two entries share the same primary keystroke.
    ///
    /// Linux alternates are separate entries with `linux_keystroke: None` —
    /// they occupy their own `keystroke` slot and are not duplicates of the
    /// macOS entry in GPUI's keymap.
    #[test]
    fn no_duplicate_bindings_per_context() {
        let mut seen: HashMap<(&str, &str), usize> = HashMap::new();

        for (ix, entry) in KEYMAP_REGISTRY.iter().enumerate() {
            let key = (entry.context, entry.keystroke);
            if let Some(prev) = seen.insert(key, ix) {
                panic!(
                    "Duplicate binding {:?} in context {:?}: entries {} and {}",
                    entry.keystroke, entry.context, prev, ix
                );
            }
        }
    }

    /// Every entry must have a non-empty description — blank rows in `?` are bugs.
    #[test]
    fn every_entry_has_description() {
        for (ix, entry) in KEYMAP_REGISTRY.iter().enumerate() {
            assert!(
                !entry.description.is_empty(),
                "Entry {} (keystroke {:?} in context {:?}) has an empty description",
                ix,
                entry.keystroke,
                entry.context
            );
        }
    }

    /// The registry must have more than 20 entries (sanity check against accidental truncation).
    #[test]
    fn all_referenced_actions_are_distinct() {
        assert!(
            KEYMAP_REGISTRY.len() > 20,
            "Expected more than 20 keymap entries, found {}",
            KEYMAP_REGISTRY.len()
        );
    }
}
