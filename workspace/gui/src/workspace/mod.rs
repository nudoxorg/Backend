//! `workspace` — the lindsey application shell (GUI-PLAN §13, §26).
//!
//! # Module topology
//!
//! ```text
//! workspace/
//! ├── shell.rs        §13.1 — DockArea chrome, docks, banner surface
//! ├── item.rs         §13.4 — WorkspaceItem trait
//! ├── pane.rs         §13.4 — tab strip, ItemSlot, one-drop teardown
//! ├── status_bar.rs   §13.3 — bottom status bar
//! ├── overlays.rs     §13.5 — overlay stack (owned by another agent)
//! └── toasts.rs       §13.7 — toast stack (owned by another agent)
//! ```
//!
//! The two modules declared below but not authored here (`overlays`, `toasts`)
//! are owned by a sibling agent.  They are declared here so the module tree
//! compiles end-to-end even while those files are being written.

pub mod item;
pub mod overlays;
pub mod pane;
pub mod shell;
pub mod status_bar;
pub mod toasts;
