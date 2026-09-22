//! Canonical responsive shell authority.
//!
//! The resolver is deliberately independent of routes, focus, viewport
//! anchors, and action frames. It consumes only measured window content
//! geometry, the user's text scale, and durable pane preferences. Views
//! receive the resulting value and project it into GPUI component primitives.

mod adapter;
mod input;
mod regions;
mod resolver;
pub mod test_support;
mod tokens;
mod transition;

pub use adapter::LayoutCache;
pub use input::{LayoutInput, LogicalPx, PanelPreferences, TextScale, WindowContentSize};
pub use regions::{
    HorizontalOverflow, OverlayPresentation, RegionBounds, RegionId, RegionPresentation,
    RegionSlot, SafeContentBounds, SheetKind, SheetPresentation, SheetSide, ShellRegions,
};
pub use resolver::{CollapseStage, PanelMode, ResponsiveLayout, WidthClass, resolve};
pub use transition::{LayoutTransitionKind, TransitionPlan, transition_plan};
