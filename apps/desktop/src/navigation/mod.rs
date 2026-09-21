//! Typed navigation and UI interaction boundary.

pub mod action;
pub mod focus;
pub mod history;
pub mod intent;
pub mod modal;
pub mod reducer;
pub mod route;

pub use action::{
    AccessibilityRole, ActionId, ActionNode, ActionSpec, CommandPaletteState, KeyChord,
    SemanticFamily, SemanticState, VoiceChannel,
};
pub use focus::{FocusId, FocusNode, FocusRoute, FocusTree};
pub use history::{MAX_ROUTE_HISTORY, RouteHistory};
pub use intent::{Effect, EngineCommand, Intent, Reduction, RequestId};
pub use modal::{ModalFrame, ModalId, ModalStack};
pub use reducer::reduce;
pub use route::{
    Coordinate, CoordinateError, OrbitRoute, Overlay, PackageLane, PackageRoute, PageRoute, Route,
    RouteDepth, RouteKey, Selection, SettingsPage, SourceRoute,
};
