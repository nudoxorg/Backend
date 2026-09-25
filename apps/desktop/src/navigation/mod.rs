//! Typed navigation and UI interaction boundary.

pub mod action;
pub mod history;
pub mod intent;
pub mod journey_specs;
pub mod reducer;
pub mod route;
mod workspace_reducer;

pub use action::{
    AccessibilityRole, ActionId, ActionNode, ActionSpec, CommandPaletteState, KeyChord,
    SemanticFamily, SemanticState, VoiceChannel,
};
pub use history::{MAX_ROUTE_HISTORY, RouteHistory};
pub use intent::{Effect, EngineCommand, FolderPickerOutcome, Intent, Reduction, RequestId};
pub use reducer::reduce;
pub use route::{
    Coordinate, CoordinateError, OrbitRoute, Overlay, PackageLane, PackageRoute, ReleaseId, Route,
    RouteDepth, RouteKey, Selection, SettingsPage, SymbolRoute, View,
};
