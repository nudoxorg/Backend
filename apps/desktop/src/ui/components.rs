//! Typed adapters around GPUI CE components.
//!
//! GPUI CE owns interaction semantics: focus traversal, accessible roles,
//! disabled/loading behavior, managed tooltip placement, and popup lifetimes.
//! Nudox owns the visual language. These small builders are the seam between
//! the two: callers ask for a Nudox role and receive a real component with
//! the palette projected by [`crate::theme::sync_components`].
//!
//! The facade below keeps the existing `crate::ui::components` API stable
//! while giving each concern a real module boundary. Semantic value types are
//! dependency-free, the action tree owns traversal policy, per-window frames
//! own post-layout/native-focus evidence, and visual builders depend on those
//! lower-level seams.

mod action_frames;
mod action_tree;
mod semantic;
mod visual;

pub(crate) use action_frames::{ActionFrameToken, ActionFrames};
pub(crate) use action_tree::{ActionRegistrar, ActionTree};
pub(crate) use semantic::{
    ActionMetadata, ActionRelation, ActionRole, ActionState, SemanticBounds,
};
pub(crate) use visual::{
    Weight, button, button_with_state, button_with_state_and_accessible, card_button, icon_button,
    icon_button_with_state, input, input_with_state, list_item, list_item_with_state, measure,
    popover, popover_with_action, search_input, search_input_with_state, settings,
    settings_with_action, tooltip, tree, tree_with_action, vertical_scroll,
};

#[cfg(test)]
#[path = "components/tests.rs"]
mod tests;
