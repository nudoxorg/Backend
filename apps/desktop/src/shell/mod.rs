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

mod ask;
mod bodies;
mod facet_sync;
mod focus;
mod frame;
mod hints;
mod keys;
mod kit;
mod peeks;
mod pins;
mod reader;
mod region;
mod reveal;
mod root;
mod shelf;
mod status;
mod system;
mod text_fit;
mod thread;
mod titlebar;

#[cfg(test)]
pub(crate) mod tests;

pub use frame::{Frame, FrameInput, ShelfMode};
pub use reader::Way;
pub use keys::bindings as key_bindings;
pub use root::{RenderCounts, Shell, open_shell};
