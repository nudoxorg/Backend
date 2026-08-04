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

gpui::actions!(
    lindsey,
    [
        /// Open the omni-search overlay (§15). Primary entry point for all navigation.
        OpenOmniSearch,
        /// Open the command palette overlay (§23.1). Lists all actions + keybindings.
        OpenCommandPalette,
        /// Navigate back in the nav history (§12.8). Restores scroll position.
        NavigateBack,
        /// Navigate forward in the nav history (§12.8).
        NavigateForward,
        /// Open the settings page (§21) as a WorkspaceItem.
        OpenSettings,
        /// Open a project folder via the OS file picker (§14).
        OpenProject,
        /// Toggle the `?` shortcuts overlay (§23.3). Only fires when no input is focused.
        ToggleShortcutsOverlay,
        /// Toggle the performance HUD (§25.2). Debug builds only.
        ToggleHud,
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
        /// Deep-link to the currently highlighted line in the Source view (§16).
        DeepLinkLine,
    ]
);

// ── Graph view ────────────────────────────────────────────────────────────────

gpui::actions!(
    lindsey,
    [
        /// Fit the entire graph into the visible viewport with animated pan+zoom (§18.4).
        FitGraphToView,
        /// Expand the neighbors of the currently selected graph node (§18.4).
        ExpandNeighbors,
        /// Pin or unpin the selected graph node so force-layout ignores it (§18.4).
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
        /// Open a diff view comparing the current package against its previous version (§17/§22).
        DiffAgainstPrevious,
        /// Collapse the selected module-tree node in the package browser (§17).
        CollapseTreeNode,
        /// Expand the selected module-tree node in the package browser (§17).
        ExpandTreeNode,
        /// Switch keyboard focus between the module tree and the item table (§17).
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
