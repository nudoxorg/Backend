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
    ActivateTab7, ActivateTab8, ActivateTab9, CloseTab, CollapseTreeNode, ConfirmOverlay, Copy,
    CopySymbolUri, Cut, CycleTheme, CycleThemeBack, DismissOverlay, DismissWindow,
    DiffAgainstPrevious, ExpandNeighbors, ExpandRow, ExpandTreeNode, FilterAuto, FilterName,
    FilterSemantic, FilterType, FitGraphToView, GoToDocsTab, GoToRefsTab, GoToSourceTab, HideApp,
    JumpToSection1,
    JumpToSection2, JumpToSection3, MoveSelectionDown, MoveSelectionUp, NextSection, OpenAccount,
    OpenCommandPalette, OpenInBackgroundTab, OpenOmniSearch, OpenProject, OpenSettings,
    OpenVersionPicker, OpenWithoutClosing, Paste, PinGraphNode, PrevSection, Quit, Redo, SelectAll,
    ShowWindow, SwitchFocusTreeTable, SyncSelected, ToggleBottomDock, ToggleLeftDock,
    ToggleShortcutsOverlay, Undo,
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
    /// `"!InputFocused"`.
    ///
    /// The string must name a context some element actually declares with
    /// `key_context`, or the entry is decoration: GPUI matches the predicate
    /// against the focused element's ancestor context stack, so a context that
    /// is never pushed can never match. Two entries have been removed from this
    /// registry for exactly that reason (`"DebugMode"`,
    /// `"SymbolPage > SourceTab"`) — see the comments at their old positions.
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
    // ── Application lifecycle ─────────────────────────────────────────────────
    //
    // These five rows are unlike every other entry in this registry in one
    // respect that matters: their actions are handled on the *App*
    // (`app::lifecycle::wire`), not on an element. That is what lets the same
    // actions still fire from the menu bar once the window is dismissed and no
    // dispatch tree exists — see `app::menus`.
    //
    // `cmd-Q` in particular is not a nicety. lindsey installed no menu bar and
    // gpui binds no default quit, so before this row existed the only way to
    // stop the process was to kill it — which skips `on_app_quit` and therefore
    // skips the MCP host's drain of in-flight agent requests.
    //
    // **Non-macOS caveat, stated rather than hidden.** `QuitMode::Default`
    // resolves to `LastWindowClosed` off macOS, so "close the window and keep
    // running" is a macOS behaviour; the two rows that describe it say so. They
    // deliberately have no `ctrl-` alternate for that reason — a Linux reader
    // should not be taught a key whose description is false there.
    KeymapEntry {
        keystroke: "cmd-q",
        linux_keystroke: Some("ctrl-q"),
        context: "global",
        description: "Quit lindsey",
        binding: || KeyBinding::new("cmd-q", Quit, Some("global")),
    },
    KeymapEntry {
        keystroke: "ctrl-q",
        linux_keystroke: None,
        context: "global",
        description: "Quit lindsey (Linux/Windows)",
        binding: || KeyBinding::new("ctrl-q", Quit, Some("global")),
    },
    KeymapEntry {
        keystroke: "cmd-h",
        linux_keystroke: None,
        context: "global",
        description: "Hide lindsey, keeping the window (macOS)",
        binding: || KeyBinding::new("cmd-h", HideApp, Some("global")),
    },
    // Not `cmd-W`: that is `CloseTab` in the `Pane` context, and the app menu
    // gives this action a key equivalent that AppKit consumes *before* GPUI
    // dispatches. Claiming `cmd-W` here would delete tab closing outright.
    KeymapEntry {
        keystroke: "cmd-shift-w",
        linux_keystroke: None,
        context: "global",
        description: "Close the window; lindsey keeps hosting MCP (macOS)",
        binding: || KeyBinding::new("cmd-shift-w", DismissWindow, Some("global")),
    },
    KeymapEntry {
        keystroke: "cmd-0",
        linux_keystroke: None,
        context: "global",
        description: "Show the lindsey window",
        binding: || KeyBinding::new("cmd-0", ShowWindow, Some("global")),
    },
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
        keystroke: "cmd-shift-a",
        linux_keystroke: Some("ctrl-shift-a"),
        context: "global",
        description: "Open the account panel (sign in / sign out)",
        binding: || KeyBinding::new("cmd-shift-a", OpenAccount, Some("global")),
    },
    KeymapEntry {
        keystroke: "ctrl-shift-a",
        linux_keystroke: None,
        context: "global",
        description: "Open the account panel (Linux/Windows)",
        binding: || KeyBinding::new("ctrl-shift-a", OpenAccount, Some("global")),
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
    // `NavigateBack` / `NavigateForward` (`cmd-[` / `cmd-]`, plus their `ctrl-`
    // alternates) were removed here and in `app::actions`. §12.8's
    // `stores::nav::NavHistory` is a finished, tested stack that nothing in the
    // app constructs, pushes to, or subscribes to: `Shell` holds no history,
    // `SymbolStore::open` records no entry, and the `Navigated` event has no
    // listener. There is therefore no sequence for "back" to walk, and the only
    // available implementations were a no-op or an invented one (e.g. "activate
    // the previously active tab") wearing back/forward's name. Rebind these in
    // the same change that wires `NavHistory` into `Shell` — the registry is
    // what `?` and the palette teach from, so a row here is a promise.
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
    // ── Theme ─────────────────────────────────────────────────────────────
    //
    // `cmd-shift-T` rather than a chord. Cycling is something the reader does
    // repeatedly while judging a page against a palette, and a chord turns
    // "try the next one" into a two-beat operation you stop doing after three
    // presses. T is the only letter it could be, and nothing in this app has
    // a "reopen closed tab" for it to collide with.
    KeymapEntry {
        keystroke: "cmd-shift-t",
        linux_keystroke: Some("ctrl-shift-t"),
        context: "global",
        description: "Next theme",
        binding: || KeyBinding::new("cmd-shift-t", CycleTheme, Some("global")),
    },
    KeymapEntry {
        keystroke: "ctrl-shift-t",
        linux_keystroke: None,
        context: "global",
        description: "Next theme (Linux/Windows)",
        binding: || KeyBinding::new("ctrl-shift-t", CycleTheme, Some("global")),
    },
    KeymapEntry {
        keystroke: "cmd-alt-shift-t",
        linux_keystroke: Some("ctrl-alt-shift-t"),
        context: "global",
        description: "Previous theme",
        binding: || KeyBinding::new("cmd-alt-shift-t", CycleThemeBack, Some("global")),
    },
    KeymapEntry {
        keystroke: "ctrl-alt-shift-t",
        linux_keystroke: None,
        context: "global",
        description: "Previous theme (Linux/Windows)",
        binding: || KeyBinding::new("ctrl-alt-shift-t", CycleThemeBack, Some("global")),
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
    // `ToggleHud` (`f12`) was removed here and in `app::actions`. `crate::perf`
    // is a one-line module skeleton — there is no performance HUD to toggle
    // (GUI-PLAN §26, "filled by its milestone"). The binding was additionally
    // scoped to a `"DebugMode"` key context that no element in the tree has
    // ever declared (`grep -rn 'key_context' src/`), so it could not have
    // dispatched even once a handler existed.
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
    // ── OmniSearch — text editing ─────────────────────────────────────────────
    KeymapEntry {
        keystroke: "cmd-a",
        linux_keystroke: Some("ctrl-a"),
        context: "OmniSearch",
        description: "Select all text in search input",
        binding: || KeyBinding::new("cmd-a", SelectAll, Some("OmniSearch")),
    },
    KeymapEntry {
        keystroke: "ctrl-a",
        linux_keystroke: None,
        context: "OmniSearch",
        description: "Select all text in search input (Linux/Windows)",
        binding: || KeyBinding::new("ctrl-a", SelectAll, Some("OmniSearch")),
    },
    KeymapEntry {
        keystroke: "cmd-c",
        linux_keystroke: Some("ctrl-c"),
        context: "OmniSearch",
        description: "Copy selected text (or all) to clipboard",
        binding: || KeyBinding::new("cmd-c", Copy, Some("OmniSearch")),
    },
    KeymapEntry {
        keystroke: "ctrl-c",
        linux_keystroke: None,
        context: "OmniSearch",
        description: "Copy selected text to clipboard (Linux/Windows)",
        binding: || KeyBinding::new("ctrl-c", Copy, Some("OmniSearch")),
    },
    KeymapEntry {
        keystroke: "cmd-x",
        linux_keystroke: Some("ctrl-x"),
        context: "OmniSearch",
        description: "Cut selected text to clipboard",
        binding: || KeyBinding::new("cmd-x", Cut, Some("OmniSearch")),
    },
    KeymapEntry {
        keystroke: "ctrl-x",
        linux_keystroke: None,
        context: "OmniSearch",
        description: "Cut selected text to clipboard (Linux/Windows)",
        binding: || KeyBinding::new("ctrl-x", Cut, Some("OmniSearch")),
    },
    KeymapEntry {
        keystroke: "cmd-v",
        linux_keystroke: Some("ctrl-v"),
        context: "OmniSearch",
        description: "Paste from clipboard into search input",
        binding: || KeyBinding::new("cmd-v", Paste, Some("OmniSearch")),
    },
    KeymapEntry {
        keystroke: "ctrl-v",
        linux_keystroke: None,
        context: "OmniSearch",
        description: "Paste from clipboard (Linux/Windows)",
        binding: || KeyBinding::new("ctrl-v", Paste, Some("OmniSearch")),
    },
    KeymapEntry {
        keystroke: "cmd-z",
        linux_keystroke: Some("ctrl-z"),
        context: "OmniSearch",
        description: "Undo last text edit in search input",
        binding: || KeyBinding::new("cmd-z", Undo, Some("OmniSearch")),
    },
    KeymapEntry {
        keystroke: "ctrl-z",
        linux_keystroke: None,
        context: "OmniSearch",
        description: "Undo last text edit (Linux/Windows)",
        binding: || KeyBinding::new("ctrl-z", Undo, Some("OmniSearch")),
    },
    KeymapEntry {
        keystroke: "cmd-shift-z",
        linux_keystroke: Some("ctrl-shift-z"),
        context: "OmniSearch",
        description: "Redo last undone edit in search input",
        binding: || KeyBinding::new("cmd-shift-z", Redo, Some("OmniSearch")),
    },
    KeymapEntry {
        keystroke: "ctrl-shift-z",
        linux_keystroke: None,
        context: "OmniSearch",
        description: "Redo last undone edit (Linux/Windows)",
        binding: || KeyBinding::new("ctrl-shift-z", Redo, Some("OmniSearch")),
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
    // Mode chips. `alt-`, not `cmd-`, because `cmd-1..3` above already *jump*
    // between sections and these *filter* to one — two similar-sounding verbs
    // on the same digits would be a trap.
    KeymapEntry {
        keystroke: "alt-0",
        linux_keystroke: None,
        context: "OmniSearch",
        description: "Search mode: Auto (all sections)",
        binding: || KeyBinding::new("alt-0", FilterAuto, Some("OmniSearch")),
    },
    KeymapEntry {
        keystroke: "alt-1",
        linux_keystroke: None,
        context: "OmniSearch",
        description: "Search mode: Name only",
        binding: || KeyBinding::new("alt-1", FilterName, Some("OmniSearch")),
    },
    KeymapEntry {
        keystroke: "alt-2",
        linux_keystroke: None,
        context: "OmniSearch",
        description: "Search mode: Type only",
        binding: || KeyBinding::new("alt-2", FilterType, Some("OmniSearch")),
    },
    KeymapEntry {
        keystroke: "alt-3",
        linux_keystroke: None,
        context: "OmniSearch",
        description: "Search mode: Semantic only",
        binding: || KeyBinding::new("alt-3", FilterSemantic, Some("OmniSearch")),
    },
    // ── SymbolPage ────────────────────────────────────────────────────────────
    //
    // L16/GUI-PLAN §16: the symbol page renders Implementations / References /
    // Source as collapsible sections below one continuous document, not as an
    // inner tab strip. `PrevInnerTab` / `NextInnerTab` / `GoToTimelineTab` were
    // bound here for a tab strip that was never built this way — the version
    // strip they'd have jumped to has no expand/collapse state and is already
    // permanently visible below the header, so there was nothing for
    // "go to timeline" to do that opening the page doesn't already do, and
    // "next/previous tab" has no referent at all. Removed rather than kept as
    // handlers that discard their argument (see `views::symbol_page` for the
    // one place `SymbolPage` still had exactly that shape before this fix).
    //
    // `y` / `v` / `g d` / `g s` / `g r` survive because each maps onto a real,
    // observable state change: clipboard + confirmation, the version picker
    // popover, and the three named sections respectively.
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
        description: "Scroll to Documentation",
        binding: || KeyBinding::new("g d", GoToDocsTab, Some("SymbolPage")),
    },
    KeymapEntry {
        keystroke: "g s",
        linux_keystroke: None,
        context: "SymbolPage",
        description: "Expand Source section",
        binding: || KeyBinding::new("g s", GoToSourceTab, Some("SymbolPage")),
    },
    KeymapEntry {
        keystroke: "g r",
        linux_keystroke: None,
        context: "SymbolPage",
        description: "Expand References section",
        binding: || KeyBinding::new("g r", GoToRefsTab, Some("SymbolPage")),
    },
    // `DeepLinkLine` (`shift-l`) was bound to context `"SymbolPage > SourceTab"`
    // — a context that has never once existed at runtime (`grep -rn
    // 'key_context' src/` finds only `"SymbolPage"`, never a nested
    // `"SourceTab"`). It is a source-view line-permalink feature that was
    // designed for the old inner-tab source view and never built for the
    // section-based one; there is no `SourceTab` to scope it to and no source
    // line selection state anywhere in `SymbolPage` for it to act on (the
    // Source section renders a static path + byte range, not a line list —
    // see `SymbolPage::render_source_body`). Removed rather than invented: a
    // real per-line deep link needs a line-addressable source view first,
    // which does not exist yet.
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
        description: "Expand graph neighbors",
        binding: || KeyBinding::new("e", ExpandNeighbors, Some("GraphView")),
    },
    KeymapEntry {
        keystroke: "p",
        linux_keystroke: None,
        context: "GraphView",
        description: "Pin graph node",
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
        description: "Collapse package tree node",
        binding: || KeyBinding::new("left", CollapseTreeNode, Some("PackageBrowser")),
    },
    KeymapEntry {
        keystroke: "right",
        linux_keystroke: None,
        context: "PackageBrowser",
        description: "Expand package tree node",
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
    ///
    /// "back" and "forward" were dropped from this list when the
    /// `NavigateBack` / `NavigateForward` bindings were removed (see the
    /// comment where they used to live). They are not silently gone: the
    /// removal is pinned in the opposite direction by
    /// [`unimplemented_features_stay_unbound`], so re-adding a row for either
    /// one without the store wiring behind it fails the build.
    #[test]
    fn all_appendix_b_rows_present() {
        let descriptions: Vec<&str> = KEYMAP_REGISTRY.iter().map(|e| e.description).collect();

        let keywords = [
            "omni-search",
            "command palette",
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
            let found = descriptions.iter().any(|d| d.to_lowercase().contains(kw));
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

    /// Features with no implementation behind them must have no binding.
    ///
    /// This is the guard that makes the L15 removals a decision rather than an
    /// omission. A row in this registry is a promise to the reader twice over:
    /// `?` teaches it and the command palette runs it, both directly from here.
    /// Adding one back for nav history or the perf HUD before
    /// `stores::nav::NavHistory` is wired into `Shell` / before `crate::perf`
    /// exists puts an inert key in front of the user with a description that
    /// says it works.
    #[test]
    fn unimplemented_features_stay_unbound() {
        for entry in KEYMAP_REGISTRY.iter() {
            let d = entry.description.to_lowercase();
            assert!(
                !d.contains("navigate back") && !d.contains("navigate forward"),
                "binding {:?} promises nav history, but nothing constructs, \
                 pushes to, or subscribes to `stores::nav::NavHistory` — wire \
                 it into `Shell` in the same change that adds this row",
                entry.keystroke
            );
            assert!(
                !d.contains("hud"),
                "binding {:?} promises the performance HUD, but `crate::perf` \
                 is still an empty module skeleton (GUI-PLAN §26)",
                entry.keystroke
            );
        }
    }

    /// Every entry's context must be one an element in the tree really declares.
    ///
    /// GPUI matches a binding's predicate against the *focused* element's
    /// ancestor context stack. A context string nothing ever pushes therefore
    /// makes the binding unreachable while it still renders in `?` as though it
    /// worked — the exact failure `"DebugMode"` (`f12`) and
    /// `"SymbolPage > SourceTab"` (`shift-l`) both had.
    #[test]
    fn every_context_is_declared_somewhere_in_the_view_tree() {
        // Every `key_context(..)` token that exists in `src/` today
        // (`grep -rn 'key_context' src/`), plus `global`, which `Shell`'s root
        // declares alongside `Workspace`.
        const DECLARED: &[&str] = &[
            "global",
            "Workspace",
            "Pane",
            "SymbolPage",
            "Overlay",
            "OmniSearch",
            "Shortcuts",
            "CommandPalette",
            "ProjectPanel",
            "GraphView",
            "PackageBrowser",
        ];

        for entry in KEYMAP_REGISTRY.iter() {
            // `!Foo` is a negation: it matches whenever `Foo` is *absent*, so
            // it needs no declaring element to be reachable.
            if entry.context.starts_with('!') {
                continue;
            }
            assert!(
                DECLARED.contains(&entry.context),
                "binding {:?} is scoped to context {:?}, which no element \
                 declares with `key_context` — it can never dispatch. Either \
                 declare the context on the view that owns the action, or \
                 remove the binding.",
                entry.keystroke,
                entry.context
            );
        }
    }

    /// Graph and package actions are discoverable only because their views
    /// declare the matching focused contexts.
    #[test]
    fn navigable_view_bindings_are_discoverable() {
        for context in ["GraphView", "PackageBrowser"] {
            assert!(
                entries_for_context(context).next().is_some(),
                "{context} has a real view but no discoverable bindings"
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
