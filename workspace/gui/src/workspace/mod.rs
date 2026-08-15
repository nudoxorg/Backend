//! `workspace` — the lindsey application shell (GUI-PLAN §13, §26).
//!
//! # Module topology
//!
//! ```text
//! workspace/
//! ├── shell.rs        §13.1 — the `Shell` entity: DockArea chrome, docks, overlays
//! ├── dock.rs         §13.1 — dock geometry + banner state (window-surviving)
//! ├── panels.rs       §13.1 — the dock `Panel` implementations (placeholders, Jobs, centre)
//! ├── item.rs         §13.4 — WorkspaceItem trait
//! ├── pane.rs         §13.4 — tab strip, ItemSlot, one-drop teardown
//! ├── status_bar.rs   §13.3 — bottom status bar
//! ├── overlays.rs     §13.5 — overlay stack
//! └── toasts.rs       §13.7 — toast stack
//! ```

pub mod dock;
pub mod item;
pub mod overlays;
pub mod pane;
pub mod panels;
pub mod shell;
pub mod status_bar;
pub mod toasts;
