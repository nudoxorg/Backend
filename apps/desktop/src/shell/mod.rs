//! The desktop shell: one window root composed of region entities.
//!
//! ```text
//! ┌ titlebar ─ shelf toggle · altimeter · bead thread + here capsule · trail · inbox ┐
//! ├ shelf / kspine ┬ reader: folio (≤ 1080) · gutter 34 · margin 250 ┬ pins (vast) ┤
//! └ status ─ mono address ·······························  ⌥ x-ray · hold ⌘ for keys ┘
//!   + the float layer (Ask, peeks, hints), always the root's last child
//! ```
//!
//! Three rules hold everywhere:
//!
//! 1. **Regions re-render only for their own slice.** Each region is its own
//!    `Entity`, embedded as a *cached* view ([`region::measured`]); it
//!    subscribes to the [`DataStore`](crate::runtime::store::DataStore) and
//!    calls `notify` only when its [`Watch`](crate::runtime::store::Watch)
//!    says a watched page key or snapshot branch moved. A hover wave in the
//!    reader never re-renders the titlebar or the shelf.
//! 2. **Every region sizes from its own measured width.** The cached view is
//!    laid out first; its bounds are handed to the region *before* it renders
//!    in the same frame, so a region's [`Measure`](facet::Measure) is the
//!    width it actually gets, never the window's — and never a frame late.
//! 3. **Nothing blocks.** Page data arrives through the store's read pool and
//!    its wake task; the shell only reads what already landed and asks for
//!    the rest (`ensure`, `prefetch`), so an idle window requests no frame.

pub(crate) mod acquire;
mod ask;
pub(crate) mod bodies;
mod facet_sync;
mod focus;
mod frame;
mod hand;
mod hints;
mod jump;
mod keys;
mod markdown;
pub(crate) mod kit;
mod onboard;
mod peeks;
mod pins;
mod reader;
mod region;
mod reveal;
mod root;
mod shelf;
mod side;
mod status;
#[cfg(test)]
mod symbol_links;
mod system;
mod text_fit;
mod titlebar;

#[cfg(test)]
mod anatomy_tests;
#[cfg(test)]
mod comb_tests;
#[cfg(test)]
mod fit_tests;
#[cfg(test)]
mod fluid_tests;
#[cfg(test)]
mod graph_tests;
#[cfg(test)]
mod hand_tests;
#[cfg(test)]
mod jump_tests;
#[cfg(test)]
mod motion_tests;
#[cfg(test)]
mod orbit_tests;
#[cfg(test)]
mod shelf_tests;
#[cfg(test)]
pub(crate) mod tests;

pub use frame::{Frame, FrameInput, ShelfMode};
pub(crate) use keys::OpenSettings as OpenSettingsAction;
pub use keys::bindings as key_bindings;
pub(crate) use keys::{Command as KeyCommand, TABLE as KEY_TABLE};
pub use reader::Way;
pub use root::{RenderCounts, Shell, open_shell};
