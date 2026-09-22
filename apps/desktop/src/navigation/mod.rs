//! Typed navigation and UI interaction boundary.

pub mod action;
pub mod focus;
pub mod history;
pub mod intent;
pub mod modal;
pub mod reducer;
pub mod route;
mod workspace_reducer;

pub use action::{
    AccessibilityRole, ActionId, ActionNode, ActionSpec, CommandPaletteState, KeyChord,
    SemanticFamily, SemanticState, VoiceChannel,
};
pub use focus::{ActionKey, EscapeResult, FocusId, FocusNode, FocusOrigin, FocusRoute, FocusTree};
pub use history::{RouteHistory, MAX_ROUTE_HISTORY};
pub use intent::{Effect, EngineCommand, Intent, Reduction, RequestId};
pub use modal::{ModalFrame, ModalId, ModalStack};
pub use reducer::reduce;
pub use route::{
    Coordinate, CoordinateError, OrbitRoute, Overlay, PackageLane, PackageRoute, PageRoute, Route,
    RouteDepth, RouteKey, Selection, SettingsPage, SourceRoute,
};
