//! All GPUI actions for the lindsey application.
//!
//! # Contract
//!
//! Every user-visible operation in lindsey is an action. Actions are the glue
//! between the keymap, the command palette, and the handlers that mutate store
//! state. They are declared here — ONE place — so that:
//!
//! - The keymap in `keymaps.rs` can reference them by value without circular
//!   dependencies.
//! - The command palette can enumerate them without scanning arbitrary modules.
//! - The `?` shortcuts overlay can render every binding because the registry
//!   (in `keymaps.rs`) knows both the action type and the human description.
//!
//! # Grouping
//!
//! Actions are grouped by the UI domain that owns their handler. The grouping
//! is cosmetic; GPUI's `actions!` macro registers each name globally.
//!
//! # Naming convention
//!
//! Verb-first, PascalCase: `OpenOmniSearch`, `ToggleLeftDock`, `NavigateBack`.
//! The GPUI action name string becomes `lindsey::OpenOmniSearch` etc.

// ── Navigation (global) ───────────────────────────────────────────────────────
//
// `NavigateBack` / `NavigateForward` (§12.8) and `ToggleHud` (§25.2) used to be
// declared here and bound in `keymaps` to `cmd-[` / `cmd-]` / `f12`. All three
// were removed rather than left declared, because an action that exists is an
// action the command palette will list and the `?` sheet will teach — and none
// of the three had anywhere to go:
//
// * Back/forward: `stores::nav::NavHistory` is a complete, tested stack that
//   nothing constructs, pushes to, or subscribes to. `Shell` owns no history,
//   `SymbolStore::open` records no entry, and `Navigated` has no listener, so
//   there is no sequence for "back" to move through. Restore the bindings in
//   the same commit that wires `NavHistory` into `Shell` — not before.
// * HUD: `crate::perf` is a one-line module skeleton (GUI-PLAN §26, "filled by
//   its milestone"). There is no HUD to toggle. Its binding was additionally
//   scoped to a `"DebugMode"` key context that no element in the tree has ever
//   declared, so it could not have fired even with a handler.
//
// A bound-and-inert key is worse than an unbound one: it teaches the reader the
// app is broken rather than that the feature is absent.

gpui::actions!(
    lindsey,
    [
        /// Open the omni-search overlay (§15). Primary entry point for all navigation.
        OpenOmniSearch,
        /// Open the command palette overlay (§23.1). Lists all actions + keybindings.
        OpenCommandPalette,
        /// Open the settings page (§21) as a WorkspaceItem.
        OpenSettings,
        /// Open the account overlay: sign in, or see the account you are signed
        /// in as (`docs/auth.md`).
        ///
        /// One action for both, because they are one surface. A separate
        /// `SignIn`/`ViewAccount` pair would need the caller to know which
        /// state the account is in before it could pick — and the whole point
        /// of `AccountPresentation` is that nobody outside `app::account` has
        /// to.
        OpenAccount,
        /// Open a project folder via the OS file picker (§14).
        OpenProject,
        /// Toggle the `?` shortcuts overlay (§23.3). Only fires when no input is focused.
        ToggleShortcutsOverlay,
        /// Advance to the next bundled theme and apply it immediately.
        ///
        /// # Why a cycle and not a picker
        ///
        /// A picker is the right affordance once there are twenty themes. With
        /// four, a picker costs a modal, a list, a selection model and a
        /// dismissal path to do what one key does — and it puts a scrim over
        /// the thing you are trying to judge. The whole point of switching a
        /// theme is to *look at the application in it*, which a modal prevents.
        /// The status bar names the live theme, so the cycle is not blind.
        CycleTheme,
        /// Go back one theme in the cycle.
        ///
        /// Present because a cycle without a reverse makes "I liked the last
        /// one" cost three more presses, and because a four-element ring is
        /// exactly the size where that is annoying rather than trivial.
        CycleThemeBack,
    ]
);

// ── Application lifecycle (macOS background residency) ────────────────────────
//
// These four are the only actions in this file whose handlers are registered on
// the *App* (`cx.on_action`) rather than on an element, and that is not a style
// choice. GPUI dispatches a keystroke down the focused element's ancestor chain,
// so with the window dismissed there is no chain and no element — every other
// action here is unreachable. `App::dispatch_action` falls through to the global
// listeners when `active_window()` is `None` (`gpui/src/app.rs:2230-2240`), which
// is what lets a menu item still work when the whole UI is gone.
//
// They are therefore reached from the menu bar first and the keymap second,
// which is the reverse of every other action in lindsey.

gpui::actions!(
    lindsey,
    [
        /// Rebuild and activate the window after it was dismissed (dock click,
        /// or Window ▸ Show lindsey). Idempotent — see `app::lifecycle`.
        ShowWindow,
        /// Tear the window down while leaving the engine and the hosted MCP
        /// endpoint running. Distinct from `CloseTab`, which closes a document.
        DismissWindow,
        /// Hide the application, keeping the window intact (`cmd-H`).
        HideApp,
        /// Quit for real, running the `on_app_quit` drain of in-flight agent
        /// requests first. Nothing bound this before: lindsey installed no menu
        /// bar, and gpui binds no default `cmd-Q`, so the app could only be
        /// killed from outside.
        Quit,
        /// Put the hosted MCP endpoint on the clipboard.
        ///
        /// Reachable from the app menu, so a reader whose window is dismissed
        /// can still hand the address to an agent without summoning the UI.
        CopyMcpEndpoint,
    ]
);

// ── Shell / dock ──────────────────────────────────────────────────────────────

gpui::actions!(
    lindsey,
    [
        /// Toggle the left sidebar dock (§13.1). Animates via `dock.slide` spring.
        ToggleLeftDock,
        /// Toggle the bottom dock (jobs + logs, §13.1). Same spring as left dock.
        ToggleBottomDock,
        /// Close the currently active tab (§13.4).
        CloseTab,
        /// Activate tab by index 1 (§13.4). Equivalent to the first tab in the strip.
        ActivateTab1,
        /// Activate tab by index 2.
        ActivateTab2,
        /// Activate tab by index 3.
        ActivateTab3,
        /// Activate tab by index 4.
        ActivateTab4,
        /// Activate tab by index 5.
        ActivateTab5,
        /// Activate tab by index 6.
        ActivateTab6,
        /// Activate tab by index 7.
        ActivateTab7,
        /// Activate tab by index 8.
        ActivateTab8,
        /// Activate tab by index 9.
        ActivateTab9,
    ]
);

// ── Overlay control ───────────────────────────────────────────────────────────

gpui::actions!(
    lindsey,
    [
        /// Pop the top overlay or clear the focused input (§13.5). Escape always cancels.
        DismissOverlay,
        /// Confirm / commit the focused overlay action (e.g. open selected search hit).
        ConfirmOverlay,
        /// Open the selected search hit in a background tab without closing the overlay.
        OpenInBackgroundTab,
        /// Open the selected search hit without closing the overlay.
        OpenWithoutClosing,
    ]
);

// ── Search / list navigation ──────────────────────────────────────────────────

gpui::actions!(
    lindsey,
    [
        /// Move the selection cursor up in the active list (omni-search, refs, deps).
        MoveSelectionUp,
        /// Move the selection cursor down in the active list.
        MoveSelectionDown,
        /// Jump to the next section in the search results (omni-search §15).
        NextSection,
        /// Jump to the previous section in the search results.
        PrevSection,
        /// Jump directly to the Name results section in omni-search (§15).
        JumpToSection1,
        /// Jump directly to the Type results section in omni-search.
        JumpToSection2,
        /// Jump directly to the Semantic results section in omni-search.
        JumpToSection3,
        /// Expand a row's inline detail panel (project deps, refs — §14 / §16).
        ExpandRow,
    ]
);

// ── Search mode chips ─────────────────────────────────────────────────────────
//
// The four chips on the omni-search input row (`Auto` / `Name` / `Type` /
// `Semantic`) were mouse-only, and — until `SearchMode::shows` — they did
// nothing at all when clicked: `SearchQuery` carries no mode field, so the
// engine could not be told which plane the reader wanted, and the store dropped
// the mode on the floor. Now that selecting one visibly scopes the results,
// there is something for a keystroke to do, and a chip a keyboard user cannot
// reach is a chip half the users do not have.

gpui::actions!(
    lindsey,
    [
        /// Route the query by shape — all three result sections shown (§15).
        FilterAuto,
        /// Show only lexical name matches.
        FilterName,
        /// Show only type-signature matches.
        FilterType,
        /// Show only semantic matches.
        FilterSemantic,
    ]
);

// ── Symbol page inner tabs ────────────────────────────────────────────────────

gpui::actions!(
    lindsey,
    [
        /// Switch to the previous inner tab in the symbol page (Docs/Source/Refs/…, §16).
        PrevInnerTab,
        /// Switch to the next inner tab in the symbol page.
        NextInnerTab,
        /// Go directly to the Docs inner tab (§16).
        GoToDocsTab,
        /// Go directly to the Source inner tab.
        GoToSourceTab,
        /// Go directly to the Refs inner tab.
        GoToRefsTab,
        /// Go directly to the Timeline inner tab.
        GoToTimelineTab,
        /// Copy the stable symbol URI for the current symbol to the clipboard (§16).
        CopySymbolUri,
        /// Open the version picker popover for the current symbol (§16).
        OpenVersionPicker,
    ]
);
//
// `DeepLinkLine` was declared here and bound (in a key context that never
// existed at runtime — see `app::keymaps`) to permalink the highlighted source
// line. `SymbolPage`'s Source section renders a path and a *byte* range, not a
// line list, and holds no per-line selection state, so there was no
// "highlighted line" for the action to address. It is removed rather than
// stubbed; a real line permalink needs a line-addressable source view first.

// ── Graph view ────────────────────────────────────────────────────────────────

gpui::actions!(
    lindsey,
    [
        /// Fit the graph's visible nodes into the viewport.
        FitGraphToView,
        /// Expand the selected graph node's neighbors.
        ExpandNeighbors,
        /// Pin or unpin the selected graph node.
        PinGraphNode,
    ]
);

// ── Project panel ─────────────────────────────────────────────────────────────

gpui::actions!(
    lindsey,
    [
        /// Trigger a sync for the selected dependency in the project panel (§14).
        SyncSelected,
    ]
);

// ── Package browser ───────────────────────────────────────────────────────────

gpui::actions!(
    lindsey,
    [
        /// Show the difference between the selected package and its predecessor.
        DiffAgainstPrevious,
        /// Collapse the selected package-tree node.
        CollapseTreeNode,
        /// Expand the selected package-tree node.
        ExpandTreeNode,
        /// Move keyboard focus between the package tree and item table.
        SwitchFocusTreeTable,
    ]
);

// ── Text editing (inputs / editors inside overlays) ───────────────────────────

gpui::actions!(
    lindsey,
    [
        /// Cut the current selection to the clipboard.
        Cut,
        /// Copy the current selection to the clipboard.
        Copy,
        /// Paste from the clipboard at the current cursor position.
        Paste,
        /// Select all content in the focused input or editor.
        SelectAll,
        /// Undo the last edit in the focused input or editor.
        Undo,
        /// Redo the last undone edit in the focused input or editor.
        Redo,
    ]
);
