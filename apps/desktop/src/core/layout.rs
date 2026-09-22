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
    HorizontalOverflow, PanelMode, RegionBounds, RegionId, RegionPresentation, RegionSlot,
    SafeContentBounds, SheetKind, ShellRegions,
};
pub use resolver::{CollapseStage, ResponsiveLayout, TitlebarDensity, WidthClass, resolve};
pub use transition::{LayoutTransitionKind, TransitionPlan, transition_plan};
